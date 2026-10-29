//! Правка полей: вид, проверка значения, фильтр, фиксация,
//! сессия. Работают на копии эталона во временном каталоге: фиксация пишет
//! рядом с файлом.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tui_patch::data::{fetch, File};
use tui_patch::decode;
use tui_patch::desk::{CommitMode, Desk, Focus, Key, PromptKind};
use tui_patch::edit::Edits;
use tui_patch::io::{self, Context, Msg, Open, Reader};
use tui_patch::lua::Filter;

fn root() -> PathBuf {
    std::env::var_os("CARGO_MANIFEST_DIR")
        .map(Into::into)
        .unwrap_or_else(|| env!("CARGO_MANIFEST_DIR").into())
}

/// Копия `plain.dat` в своём каталоге теста.
fn copy(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mutator-edit-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("plain.dat");
    std::fs::copy(root().join("tests/data/plain.dat"), &p).unwrap();
    p
}

fn lua() -> bool {
    Context::new().ensure_lua(tui_patch::io::Backend::TLL)
}

fn all(path: &Path) -> Vec<Msg> {
    let ctx = Context::new();
    let r = Reader::open(&ctx, &Open::file(path)).unwrap();
    r.collect()
}

fn open(path: &Path) -> Desk {
    let mut d = Desk::open(File::open(path).unwrap(), Arc::new(|| {}));
    d.height = 10;
    let t = Instant::now();
    while d.index.is_none() {
        d.wait(Duration::from_millis(100));
        assert!(t.elapsed() < Duration::from_secs(30), "индекс не пришёл");
    }
    settle(&mut d);
    d
}

fn settle(d: &mut Desk) {
    let t = Instant::now();
    loop {
        d.poll();
        if d.idle() {
            return;
        }
        let got = d.wait(Duration::from_millis(100));
        if !got && d.idle() {
            return;
        }
        assert!(
            t.elapsed() < Duration::from_secs(30),
            "не успокоилось: {}",
            d.status
        );
    }
}

fn key(d: &mut Desk, k: Key) {
    d.key(k);
    settle(d);
}

/// Выбрать сообщение `seq`, открыть деталь и встать на поле `path`.
fn at_field(d: &mut Desk, seq: i64, path: &str) {
    d.show_seq(seq);
    settle(d);
    if !d.detail_open {
        key(d, Key::Enter);
    }
    d.focus = Focus::Detail;
    let det = d.detail.as_mut().expect("деталь");
    assert_eq!(det.seq, seq);
    det.cursor = det
        .lines()
        .iter()
        .position(|(_, n)| n.path == path)
        .unwrap_or_else(|| panic!("нет поля {path}"));
}

/// Поставить полю значение с клавиш: `e`, стереть, набрать, Enter.
fn set(d: &mut Desk, seq: i64, path: &str, value: &str) {
    at_field(d, seq, path);
    key(d, Key::Char('e'));
    let p = d.prompt.as_ref().expect("строка ввода");
    assert!(matches!(p.kind, PromptKind::Edit { seq: s, .. } if s == seq));
    for _ in 0..p.text.chars().count() {
        d.key(Key::Backspace);
    }
    for c in value.chars() {
        d.key(Key::Char(c));
    }
    key(d, Key::Enter);
}

fn shown(d: &Desk, seq: i64, path: &str) -> Option<String> {
    let r = d.rows.iter().find(|r| r.seq == seq)?;
    let st = d.file.scheme.message(r.msgid)?;
    decode::field(&d.file.scheme, st, &r.data, path)
}

fn field(path: &Path, seq: i64, name: &str) -> Option<String> {
    let f = File::open(path).unwrap();
    let m = all(path).into_iter().find(|m| m.seq == seq)?;
    decode::field(&f.scheme, f.scheme.message(m.msgid)?, &m.data, name)
}

/// `agr:patch-overlay` `agr:edit-checked` — принятая правка видна в таблице и
/// детали и подсвечена; файл на диске не меняется.
#[test]
fn edit_shows_through_patch_and_file_stays() {
    io::quiet();
    let path = copy("overlay");
    let before = std::fs::read(&path).unwrap();
    let mut d = open(&path);
    set(&mut d, 1, "header.order_id", "777");
    assert!(d.prompt.is_none(), "{}", d.status);
    assert!(d.edited(1, "header.order_id"));
    assert!(d.edited_row(1) && !d.edited_row(0));
    assert_eq!(shown(&d, 1, "header.order_id").as_deref(), Some("777"));
    let det = d.detail.as_ref().unwrap();
    assert_eq!(
        det.current().unwrap().text(),
        "777",
        "курсор остался на поле"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before, "файл не тронут");

    // Вернуть старое значение — правки нет.
    set(&mut d, 1, "header.order_id", "1000");
    assert!(d.edits.is_empty());
    assert!(d.file.patch.is_none());
}

/// `agr:edit-checked` — значение, которое поле не вместит, не принимается:
/// правок нет, строка ввода возвращается с тем же текстом.
#[test]
fn bad_values_are_refused() {
    io::quiet();
    let path = copy("refused");
    let mut d = open(&path);
    for (f, v) in [
        ("header.order_id", "abc"),
        ("symbol", "SYMBOL_LONGER_THAN_17"),
        ("side", "Up"),
    ] {
        set(&mut d, 1, f, v);
        assert!(d.edits.is_empty(), "{f}={v} принято");
        assert!(d.status.contains("не принята"), "{}", d.status);
        let p = d.prompt.take().expect("ввод вернулся");
        assert_eq!(p.text, v);
    }
}

/// Правки на соседних seq не мешают окну, открытому с последнего из них.
#[test]
fn window_from_seq_sees_edits_before_it() {
    io::quiet();
    let path = copy("seek");
    let mut e = Edits::default();
    e.set(0, "Order", "header.order_id", "1000", "100");
    e.set(1, "Order", "header.order_id", "1000", "101");
    e.set(5, "Order", "header.order_id", "1002", "105");
    let patch = e.patch();
    let f0 = File::open(&path).unwrap();
    let srcs: Vec<_> = f0
        .raw_sources()
        .into_iter()
        .map(|mut s| {
            s.open = s.open.patch(patch.clone());
            s
        })
        .collect();
    let ctx = Context::new();
    let m = fetch::from_seq(&ctx, &srcs, 5, 1).unwrap();
    let f = File::open(&path).unwrap();
    let st = f.scheme.message(m[0].msgid).unwrap();
    assert_eq!(
        decode::field(&f.scheme, st, &m[0].data, "header.order_id").as_deref(),
        Some("105")
    );
}

/// Фильтр идёт после transform и ручных правок (`agr:filter-post-eval`): ручная
/// правка видна фильтру, он находит правленое значение, а не файловое.
#[test]
fn filter_sees_edited_values() {
    io::quiet();
    if !lua() {
        return;
    }
    let path = copy("filter");
    let mut d = open(&path);
    set(&mut d, 6, "header.order_id", "4242");
    // seq 6 в файле имеет order_id=1003, правка меняет его на 4242 — фильтр
    // должен видеть 4242 (правка ложится до фильтра).
    d.set_filter(Filter {
        messages: vec!["Order".into()],
        expr: "data.header.order_id == 4242".into(),
    });
    settle(&mut d);
    let t = Instant::now();
    while !d.matches.as_ref().is_some_and(|m| m.is_done()) {
        d.wait(Duration::from_millis(100));
        assert!(t.elapsed() < Duration::from_secs(30));
    }
    settle(&mut d);
    let seqs: Vec<i64> = d.rows.iter().map(|r| r.seq).collect();
    assert_eq!(seqs, vec![6], "{}", d.status);
}

/// `agr:commit-keeps-original` — новый файл: в нём правка, остальные
/// сообщения побайтно как в исходнике, исходник не тронут; потом вид
/// открывает новый файл без правок.
#[test]
fn commit_to_new_file() {
    io::quiet();
    let path = copy("newfile");
    let before = std::fs::read(&path).unwrap();
    let mut d = open(&path);
    set(&mut d, 1, "header.order_id", "777");
    set(&mut d, 5, "comment", "новый комментарий");
    key(&mut d, Key::Char('w'));
    let c = d.commit.as_ref().expect("диалог");
    assert_eq!(c.mode, CommitMode::NewFile);
    assert!(c.yaml.contains("seq: 5") && c.summary.lines.len() == 2);
    let want = path.with_file_name("plain.patched.dat");
    assert_eq!(c.new_name, want.display().to_string());
    key(&mut d, Key::Enter);
    let out = d
        .reopen
        .take()
        .unwrap_or_else(|| panic!("{:?}", d.commit.as_ref().and_then(|c| c.error.clone())));
    assert_eq!(out, want);
    assert!(d.edits.is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), before, "исходник не тронут");

    let (a, b) = (all(&path), all(&out));
    assert_eq!(a.len(), b.len());
    for (x, y) in a.iter().zip(&b) {
        assert_eq!((x.seq, x.msgid), (y.seq, y.msgid));
        if x.seq != 1 && x.seq != 5 {
            assert_eq!(x.data, y.data, "seq {} без правки", x.seq);
        }
    }
    assert_eq!(field(&out, 1, "header.order_id").as_deref(), Some("777"));
    assert_eq!(
        field(&out, 5, "comment").as_deref(),
        Some("новый комментарий")
    );

    d.reopen(&out).unwrap();
    settle(&mut d);
    assert_eq!(d.file.name, out.display().to_string());
    assert!(d.file.patch.is_none() && d.edits.is_empty());
    assert_eq!(
        shown(&d, 5, "comment").as_deref(),
        Some("новый комментарий")
    );
}

/// `agr:commit-keeps-original` — перезапись: исходник уходит в бэкап под
/// свободным именем, занятое имя не затирается.
#[test]
fn commit_over_original_keeps_backup() {
    io::quiet();
    let path = copy("overwrite");
    let before = std::fs::read(&path).unwrap();
    let busy = path.with_file_name("plain.orig.dat");
    std::fs::write(&busy, "занято").unwrap();
    let mut d = open(&path);
    set(&mut d, 0, "price", "12.5");
    key(&mut d, Key::Char('w'));
    key(&mut d, Key::Tab);
    let c = d.commit.as_ref().unwrap();
    assert_eq!(c.mode, CommitMode::Overwrite);
    let backup = path.with_file_name("plain.orig-2.dat");
    assert_eq!(c.backup_name, backup.display().to_string());
    key(&mut d, Key::Enter);
    assert_eq!(d.reopen.take().as_deref(), Some(path.as_path()));
    assert_eq!(std::fs::read(&backup).unwrap(), before, "бэкап — исходник");
    assert_eq!(std::fs::read(&busy).unwrap(), "занято".as_bytes());
    assert_eq!(field(&path, 0, "price").as_deref(), Some("12.50000000"));
}

/// Занятое имя в диалоге — ошибка в диалоге, ничего не пишется.
#[test]
fn commit_refuses_taken_name() {
    io::quiet();
    let path = copy("taken");
    let mut d = open(&path);
    set(&mut d, 0, "price", "12.5");
    key(&mut d, Key::Char('w'));
    let c = d.commit.as_mut().unwrap();
    c.new_name = path.display().to_string();
    key(&mut d, Key::Enter);
    let c = d.commit.as_ref().unwrap();
    assert!(c.error.as_ref().is_some_and(|e| e.contains("уже есть")));
    assert!(c.running.is_none() && d.reopen.is_none());
    key(&mut d, Key::Esc);
    assert!(d.commit.is_none() && !d.edits.is_empty());
}

/// `agr:edits-in-session` — незафиксированные правки возвращаются с сессией и
/// сразу видны.
#[test]
fn edits_come_back_with_session() {
    io::quiet();
    let path = copy("session");
    let mut d = open(&path);
    set(&mut d, 1, "header.order_id", "777");
    let s = d.session();
    assert_eq!(s.edits.len(), 1);
    let text = toml::to_string(&s).unwrap();
    let s: tui_patch::session::Session = toml::from_str(&text).unwrap();
    drop(d);

    let mut d = open(&path);
    d.restore(&s);
    settle(&mut d);
    assert!(d.edited(1, "header.order_id"));
    assert_eq!(shown(&d, 1, "header.order_id").as_deref(), Some("777"));
}

/// `u` снимает правку поля под курсором.
#[test]
fn u_drops_field_edit() {
    io::quiet();
    let path = copy("unset");
    let mut d = open(&path);
    set(&mut d, 1, "header.order_id", "777");
    at_field(&mut d, 1, "header.order_id");
    key(&mut d, Key::Char('u'));
    assert!(d.edits.is_empty());
    assert_eq!(shown(&d, 1, "header.order_id").as_deref(), Some("1000"));
}

/// TUI: правленое значение красное в детали и в seq; `w` — диалог со
/// сводкой и yaml (`agr:tui-same-data`).
#[test]
fn tui_marks_edits_and_draws_commit_dialog() {
    use tui_patch::ui::theme::EDITED;
    use tui_patch::ui::tui::frame;
    io::quiet();
    let path = copy("tui");
    let mut d = open(&path);
    set(&mut d, 1, "header.order_id", "777");
    let g = frame(&mut d, 160, 40);
    let rows: Vec<String> = (0..g.rows()).map(|r| g.text(r)).collect();
    let y = rows
        .iter()
        .position(|l| l.trim_start().starts_with("order_id: 777"))
        .unwrap_or_else(|| panic!("{rows:#?}"));
    let x = rows[y].find("777").unwrap();
    let x = rows[y][..x].chars().count();
    assert_eq!(g.fg(y, x), Some(EDITED));
    assert!(rows[0].contains("правок 1"), "{}", rows[0]);

    key(&mut d, Key::Char('w'));
    let g = frame(&mut d, 160, 40);
    let rows: Vec<String> = (0..g.rows()).map(|r| g.text(r)).collect();
    let all = rows.join("\n");
    assert!(all.contains("Зафиксировать правки"), "{all}");
    assert!(all.contains("1 Order header.order_id: 1000 → 777"), "{all}");
    assert!(
        all.contains("seq: 1") && all.contains("header.order_id: '777'"),
        "{all}"
    );
    assert!(all.contains("plain.patched.dat"), "{all}");
}

/// `agr:transform-shadow` — фиксация с активным мутатором: в файл уходят и
/// правки, и прогоные тела — правки поверх transform, ручная правка поверх
/// мутации скрипта.
#[test]
fn commit_applies_patches_and_transform() {
    io::quiet();
    if !lua() {
        return;
    }
    let path = copy("commitlua");
    let mut d = open(&path);
    set(&mut d, 1, "header.order_id", "777");
    // Мутатор перекодирует тело из таблицы: u8=1, order_id переносит из сырого
    // тела (правка ложится после скрипта, поэтому скрипт видит 1000, а не 777).
    d.set_transform(
        "function tll_on_data(seq, name, data)\n  if name == 'Order' then\n    tll_callback(seq, name, { u8 = 1, header = { order_id = data.header.order_id } })\n  else\n    tll_callback(seq, name, {})\n  end\nend"
            .into(),
    );
    settle(&mut d);
    key(&mut d, Key::Char('w'));
    let c = d.commit.as_ref().expect("диалог фиксации");
    assert!(c.summary.lines.len() == 1, "правка в сводке: {c:?}");
    key(&mut d, Key::Enter);
    let out = d
        .reopen
        .take()
        .unwrap_or_else(|| panic!("{:?}", d.commit.as_ref().and_then(|c| c.error.clone())));
    let (msgs, f) = (all(&out), File::open(&out).unwrap());
    assert!(!msgs.is_empty());
    let mut changed = 0;
    let src = all(&path);
    for (m, s0) in msgs.iter().zip(&src) {
        assert_eq!(m.seq, s0.seq);
        if m.data != s0.data {
            changed += 1;
        }
        // У Order мутатор ставит u8=1; у типов без u8 тело обнулено целиком.
        if f.scheme.message(m.msgid).map(|st| st.name.as_str()) == Some("Order") {
            assert_eq!(
                decode::field(&f.scheme, f.scheme.message(m.msgid).unwrap(), &m.data, "u8")
                    .as_deref(),
                Some("1"),
                "seq {}: тело прогоное",
                m.seq
            );
        }
    }
    assert_eq!(changed, msgs.len(), "мутатор применился ко всем сообщениям");
    assert_eq!(field(&out, 1, "header.order_id").as_deref(), Some("777"));
}

/// Порядок «transform, затем правки»: ручная правка того же поля, которое перезаписывает
/// скрипт, побеждает — в файл уходит ручное значение, а не скриптовое.
#[test]
fn manual_edit_wins_over_transform() {
    io::quiet();
    if !lua() {
        return;
    }
    let path = copy("manualwins");
    let mut d = open(&path);
    set(&mut d, 1, "header.order_id", "777");
    // Мутатор принудительно ставит order_id=9999 у Order; ручная правка 777
    // должна лечь поверх и выиграть.
    d.set_transform(
        "function tll_on_data(seq, name, data)\n  if name == 'Order' then\n    tll_callback(seq, name, { header = { order_id = 9999 } })\n  else\n    tll_callback(seq, name, data)\n  end\nend"
            .into(),
    );
    settle(&mut d);
    key(&mut d, Key::Char('w'));
    key(&mut d, Key::Enter);
    let out = d
        .reopen
        .take()
        .unwrap_or_else(|| panic!("{:?}", d.commit.as_ref().and_then(|c| c.error.clone())));
    assert_eq!(field(&out, 1, "header.order_id").as_deref(), Some("777"));
    // Неправленое сообщение несёт скриптовое значение.
    assert_eq!(field(&out, 0, "header.order_id").as_deref(), Some("9999"));
}
