//! Состав потока: удаление, вставка, смена типа, прогноз
//! последнего seq и фиксация (`agr:stream-ops`, `agr:view-input-seq`,
//! `agr:seq-forecast`, `agr:commit-seq-check`). Работают на копии эталона во
//! временном каталоге: фиксация пишет рядом с файлом.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tui_patch::data::{fetch, File};
use tui_patch::decode;
use tui_patch::desk::{Desk, Focus, Key, PromptKind};
use tui_patch::edit::commit::{self, Target};
use tui_patch::edit::{stream, Edits, Insert};
use tui_patch::io::{self, Context, Msg, Open, Reader};

fn root() -> PathBuf {
    std::env::var_os("CARGO_MANIFEST_DIR")
        .map(Into::into)
        .unwrap_or_else(|| env!("CARGO_MANIFEST_DIR").into())
}

/// Копия эталона `name` в своём каталоге теста.
fn copy(name: &str, test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mutator-stream-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(format!("{name}.dat"));
    std::fs::copy(root().join(format!("tests/data/{name}.dat")), &p).unwrap();
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

/// Копия сообщения `seq` файла как вставка.
fn copy_of(path: &Path, seq: i64, occupied: bool) -> Insert {
    let f = File::open(path).unwrap();
    let m = all(path).into_iter().find(|m| m.seq == seq).unwrap();
    let st = f.scheme.message(m.msgid).unwrap();
    Insert {
        name: st.name.clone(),
        body: stream::body_of(&decode::decode(&f.scheme, st, &m.data), &m.data),
        occupied,
        data: Vec::new(),
    }
}

/// Записать `e` в новый файл рядом; прогноз — по правкам, ответ — записанное.
fn commit_and_forecast(path: &Path, e: &Edits, out: &str) -> (Option<i64>, Vec<Msg>) {
    let ctx = Context::new();
    let info = File::open(path).unwrap().info;
    let last = info.seq_last.unwrap();
    let map = e.seq_map();
    let alive = fetch::alive_before(
        &ctx,
        &File::open(path).unwrap().raw_sources(),
        info.seq_begin.unwrap(),
        last,
        |s| map.deleted(s),
    )
    .unwrap();
    let forecast = map.last(last, alive);
    let patch = e.commit_patch();
    let target = path.with_file_name(out);
    let done = commit::run_all(
        &[commit::Plan::Single {
            src: path.to_path_buf(),
            target: Target::NewFile(target.clone()),
        }],
        patch.as_ref(),
        &AtomicBool::new(false),
        None,
        |_| {},
    )
    .unwrap_or_else(|x| panic!("{x}\n{}", e.yaml()))
    .pop()
    .unwrap();
    let got = all(&target);
    assert_eq!(
        done.last,
        got.last().map(|m| m.seq),
        "итог записи — настоящий последний"
    );
    assert_eq!(done.written, got.len());
    (forecast, got)
}

/// `agr:seq-forecast` `agr:commit-seq-check` `agr:stream-ops` — прогноз по
/// правкам совпадает с настоящим последним seq после записи, и каждое
/// сообщение ложится на предсказанный seq: сторожит и прогноз, и перенумерацию.
#[test]
fn forecast_matches_real_last_seq_after_commit() {
    io::quiet();
    for name in ["plain", "sparse"] {
        let path = copy(name, &format!("forecast-{name}"));
        let src = all(&path);
        let seqs: Vec<i64> = src.iter().map(|m| m.seq).collect();
        let s = |i: usize| seqs[i];
        let next = |i: usize| seqs.get(i + 1).copied();
        let f = File::open(&path).unwrap();
        let name_of = |i: usize| f.scheme.message(src[i].msgid).unwrap().name.clone();

        let mut mixes: Vec<(&str, Edits)> = Vec::new();
        // Удалить три подряд, вставить три — последний seq прежний.
        let mut e = Edits::default();
        for i in 10..13 {
            e.delete(s(i), &name_of(i), true, next(i));
        }
        for _ in 0..3 {
            e.insert(s(50), copy_of(&path, s(50), true)).unwrap();
        }
        mixes.push(("remove3+insert3", e));
        // Дыра диапазоном, вставка перед первым, смена поля.
        let mut e = Edits::default();
        for i in 20..25 {
            e.delete(s(i), &name_of(i), false, next(i));
        }
        e.insert(s(0), copy_of(&path, s(2), true)).unwrap();
        e.set(s(30), &name_of(30), "header.order_id", "1000", "777");
        mixes.push(("clear5+insert-first", e));
        // Удалён хвост — последним становится предыдущий живой.
        let mut e = Edits::default();
        let n = seqs.len();
        for i in n - 3..n {
            e.delete(s(i), &name_of(i), true, next(i));
        }
        mixes.push(("remove-tail", e));
        // Дыра и вставка на одном seq сдвигают хвост.
        let mut e = Edits::default();
        e.delete(s(40), &name_of(40), false, next(40));
        e.insert(s(40), copy_of(&path, s(42), true)).unwrap();
        mixes.push(("clear+insert-same-seq", e));
        if name == "sparse" {
            // Вставка в дыру файла никого не сдвигает.
            let mut e = Edits::default();
            e.insert(s(3) + 1, copy_of(&path, s(3), false)).unwrap();
            mixes.push(("insert-into-gap", e));
        }

        for (k, (what, e)) in mixes.iter().enumerate() {
            let (forecast, got) = commit_and_forecast(&path, e, &format!("out{k}.dat"));
            assert_eq!(
                forecast,
                got.last().map(|m| m.seq),
                "{name}/{what}: прогноз последнего seq\n{}",
                e.yaml()
            );
            let map = e.seq_map();
            for m in &src {
                let Some(o) = map.out(m.seq) else { continue };
                let w = got
                    .iter()
                    .find(|x| x.seq == o)
                    .unwrap_or_else(|| panic!("{name}/{what}: seq {} → {o} не записан", m.seq));
                assert_eq!(w.msgid, m.msgid, "{name}/{what}: seq {} → {o}", m.seq);
            }
            for (at, v) in e.all_inserts() {
                for k in 0..v.len() {
                    let o = map.out_insert(at, k);
                    assert!(
                        got.iter().any(|x| x.seq == o),
                        "{name}/{what}: вставка {k} перед {at} → {o} не записана"
                    );
                }
            }
            let removed = src.iter().filter(|m| map.deleted(m.seq)).count();
            let inserted: usize = e.all_inserts().map(|(_, v)| v.len()).sum();
            assert_eq!(got.len(), src.len() - removed + inserted, "{name}/{what}");
        }
    }
}

/// `agr:commit-seq-check` — дубль seq от transform останавливает запись с
/// местом; исходник цел, результата нет.
#[test]
fn duplicate_seq_from_transform_stops_commit_and_keeps_source() {
    io::quiet();
    if !lua() {
        return;
    }
    let path = copy("plain", "dup");
    let before = std::fs::read(&path).unwrap();
    let target = path.with_file_name("dup.out.dat");
    let code = "function tll_on_data(seq, name, data)\n  if seq == 5 then seq = 4 end\n  tll_callback(seq, name, data)\nend";
    let r = commit::run_all(
        &[commit::Plan::Single {
            src: path.clone(),
            target: Target::NewFile(target.clone()),
        }],
        None,
        &AtomicBool::new(false),
        Some((code, Some(io::LuaMode::Transform))),
        |_| {},
    );
    let e = r.expect_err("дубль seq должен остановить запись");
    assert!(e.contains("seq 4 после 4"), "{e}");
    assert!(!target.exists(), "результата нет");
    assert_eq!(std::fs::read(&path).unwrap(), before, "исходник цел");
}

fn open(path: &Path) -> Desk {
    let mut d = Desk::open(File::open(path).unwrap(), Arc::new(|| {}));
    d.height = 20;
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

fn select(d: &mut Desk, seq: i64) {
    d.show_seq(seq);
    settle(d);
    let i = d
        .rows
        .iter()
        .position(|r| r.seq == seq && r.ins.is_none())
        .unwrap();
    d.select(i);
}

fn tag(d: &Desk, seq: i64) -> String {
    let r = d
        .rows
        .iter()
        .find(|r| r.seq == seq && r.ins.is_none())
        .unwrap();
    r.tag.clone().unwrap_or_else(|| r.seq.to_string())
}

/// `agr:view-input-seq` `agr:stream-ops` — удалённое со сдвигом остаётся
/// строкой с пометкой, строки дальше показывают seq после записи; `u`
/// возвращает удалённое.
#[test]
fn deleted_row_stays_marked_and_u_restores_it() {
    io::quiet();
    let path = copy("plain", "delete");
    let mut d = open(&path);
    select(&mut d, 5);
    key(&mut d, Key::Char('d'));
    assert_eq!(d.prompt.as_ref().map(|p| p.kind), Some(PromptKind::Delete));
    key(&mut d, Key::Enter);
    assert!(d.edits.deleted(5), "{}", d.status);
    assert_eq!(tag(&d, 5), "×5");
    assert_eq!(tag(&d, 6), "6→5");
    assert_eq!(tag(&d, 4), "4");
    select(&mut d, 5);
    key(&mut d, Key::Char('u'));
    assert!(d.edits.is_empty(), "{}", d.status);
    assert_eq!(tag(&d, 6), "6");
}

/// `agr:stream-ops` — диапазон от метки `v` удаляется дырой одной командой.
#[test]
fn range_delete_by_mark_becomes_one_command() {
    io::quiet();
    let path = copy("plain", "range");
    let mut d = open(&path);
    select(&mut d, 10);
    key(&mut d, Key::Char('v'));
    select(&mut d, 14);
    key(&mut d, Key::Char('d'));
    key(&mut d, Key::Down);
    key(&mut d, Key::Enter);
    assert_eq!(
        d.edits.deleted_seqs(),
        vec![10, 11, 12, 13, 14],
        "{}",
        d.status
    );
    assert_eq!(d.edits.yaml(), "- cmd: Clear\n  from: 10\n  till: 15\n");
    assert_eq!(tag(&d, 15), "15", "дыра seq не сдвигает");
}

/// `agr:view-input-seq` `agr:edit-checked` — копия встаёт строкой перед
/// текущим, её поле правится в детали; после записи она на своём seq.
#[test]
fn inserted_copy_appears_before_current_and_its_field_is_editable() {
    io::quiet();
    let path = copy("plain", "insert");
    let mut d = open(&path);
    select(&mut d, 6);
    key(&mut d, Key::Char('o'));
    assert_eq!(d.edits.inserts(6).len(), 1, "{}", d.status);
    let i = d
        .rows
        .iter()
        .position(|r| r.ins == Some(0))
        .expect("строка вставки");
    assert_eq!(d.rows[i].seq, 6);
    assert_eq!(d.rows[i + 1].seq, 6);
    assert_eq!(d.rows[i].tag.as_deref(), Some("+6"));
    assert_eq!(tag(&d, 6), "6→7");
    assert!(!d.rows[i].data.is_empty(), "тело вставки собрано");

    d.select(i);
    key(&mut d, Key::Enter);
    d.focus = Focus::Detail;
    let det = d.detail.as_mut().expect("деталь вставки");
    assert_eq!(det.ins, Some(0));
    det.cursor = det
        .lines()
        .iter()
        .position(|(_, n)| n.path == "header.order_id")
        .unwrap();
    key(&mut d, Key::Char('e'));
    let p = d.prompt.as_ref().expect("строка ввода");
    for _ in 0..p.text.chars().count() {
        d.key(Key::Backspace);
    }
    for c in "4242".chars() {
        d.key(Key::Char(c));
    }
    key(&mut d, Key::Enter);
    assert_eq!(
        d.edits.inserts(6)[0]
            .body
            .get("header.order_id")
            .map(String::as_str),
        Some("4242"),
        "{}",
        d.status
    );

    key(&mut d, Key::Char('w'));
    key(&mut d, Key::Enter);
    let out = d
        .reopen
        .take()
        .unwrap_or_else(|| panic!("{:?}", d.commit.as_ref().and_then(|c| c.error.clone())));
    let got = all(&out);
    let f = File::open(&out).unwrap();
    let m = got.iter().find(|m| m.seq == 6).unwrap();
    let st = f.scheme.message(m.msgid).unwrap();
    assert_eq!(
        decode::field(&f.scheme, st, &m.data, "header.order_id").as_deref(),
        Some("4242")
    );
    assert_eq!(got.len(), all(&path).len() + 1);
}

/// `agr:edit-checked` — копия сообщения с байтами вне ASCII ложится точно:
/// тело вставки по полям совпадает с исходником.
#[test]
fn copy_with_non_ascii_bytes_is_exact() {
    io::quiet();
    let path = copy("plain", "inexact");
    let src = all(&path).into_iter().find(|m| m.seq == 1).unwrap();
    let mut d = open(&path);
    select(&mut d, 1);
    key(&mut d, Key::Char('o'));
    assert_eq!(d.edits.count(), 1, "{}", d.status);
    let f = File::open(&path).unwrap();
    let st = f.scheme.message(src.msgid).unwrap();
    let copy = &d.edits.inserts(1)[0].data;
    assert!(decode::diff(&f.scheme, st, &src.data, copy).is_empty());
}

/// `agr:edit-checked` `agr:stream-ops` — смена типа переносит одноимённые поля
/// и видна в виде, пока не зафиксирована.
#[test]
fn retype_carries_same_named_fields() {
    io::quiet();
    let path = copy("plain", "retype");
    let mut d = open(&path);
    select(&mut d, 1);
    let ts = {
        let r = d.selected().unwrap();
        let f = &d.file;
        let st = f.scheme.message(r.msgid).unwrap();
        decode::field(&f.scheme, st, &r.data, "header.ts")
    };
    key(&mut d, Key::Char('T'));
    let p = d.prompt.as_mut().expect("выбор типа");
    p.sel = p.items.iter().position(|i| i.label == "Cancel").unwrap();
    key(&mut d, Key::Enter);
    let r = d.rows.iter().find(|r| r.seq == 1).unwrap();
    assert_eq!(r.name, "Cancel", "{}", d.status);
    let st = d.file.scheme.message(r.msgid).unwrap();
    assert_eq!(decode::field(&d.file.scheme, st, &r.data, "header.ts"), ts);
    assert!(d.edits.yaml().contains("cmd: Update"));
}

/// `agr:seq-forecast` `agr:tui-same-data` — первая строка диалога фиксации
/// говорит, сменится ли последний seq: удалили одно и вставили одно — нет.
#[test]
fn commit_dialog_first_line_says_whether_last_seq_changes() {
    use tui_patch::ui::tui::frame;
    io::quiet();
    let path = copy("plain", "headline");
    let mut d = open(&path);
    select(&mut d, 3);
    key(&mut d, Key::Char('d'));
    key(&mut d, Key::Enter);
    key(&mut d, Key::Char('w'));
    let c = d.commit.as_ref().expect("диалог");
    assert_eq!(c.headline(), "последний seq 399 → 398 — сменится");
    key(&mut d, Key::Esc);
    select(&mut d, 10);
    key(&mut d, Key::Char('o'));
    key(&mut d, Key::Char('w'));
    let c = d.commit.as_ref().expect("диалог");
    assert_eq!(
        c.headline(),
        "последний seq 399 — не меняется",
        "{}",
        d.status
    );
    let g = frame(&mut d, 160, 40);
    let rows: Vec<String> = (0..g.rows()).map(|r| g.text(r)).collect();
    assert!(
        rows[2].contains("последний seq 399 — не меняется"),
        "{rows:#?}"
    );
}

/// Сообщения файла по seq: (msgid, тело).
fn by_seq(path: &Path) -> std::collections::BTreeMap<i64, (i32, Vec<u8>)> {
    all(path)
        .into_iter()
        .map(|m| (m.seq, (m.msgid, m.data)))
        .collect()
}

fn commit_new(d: &mut Desk) -> PathBuf {
    key(d, Key::Char('w'));
    key(d, Key::Enter);
    d.reopen
        .take()
        .unwrap_or_else(|| panic!("{:?}", d.commit.as_ref().and_then(|c| c.error.clone())))
}

/// `agr:stream-ops` — `v` и `T` меняют тип у диапазона одним телом: одна
/// команда `Update` с `from`/`till`, после записи тип сменён ровно у
/// выбранных, тела одинаковые, seq и последний seq прежние.
#[test]
fn range_retype_by_mark_writes_one_update() {
    io::quiet();
    let path = copy("plain", "range-retype");
    let ping = {
        let f = File::open(&path).unwrap();
        let id = f
            .scheme
            .messages()
            .find(|m| m.name == "Ping")
            .unwrap()
            .msgid;
        id
    };
    let mut d = open(&path);
    select(&mut d, 10);
    key(&mut d, Key::Char('v'));
    select(&mut d, 13);
    key(&mut d, Key::Char('T'));
    let p = d.prompt.as_mut().expect("выбор типа");
    p.sel = p.items.iter().position(|i| i.label == "Ping").unwrap();
    key(&mut d, Key::Enter);
    assert_eq!(
        d.edits.yaml(),
        "- cmd: Update\n  from: 10\n  till: 14\n  name: 'Ping'\n  fields: {}\n",
        "{}",
        d.status
    );
    assert!(d
        .rows
        .iter()
        .filter(|r| (10..14).contains(&r.seq))
        .all(|r| r.name == "Ping"));
    let out = commit_new(&mut d);
    let (a, b) = (by_seq(&path), by_seq(&out));
    assert_eq!(
        a.keys().collect::<Vec<_>>(),
        b.keys().collect::<Vec<_>>(),
        "seq прежние"
    );
    for (seq, (msgid, data)) in &a {
        if (10..14).contains(seq) {
            assert_eq!(b[seq].0, ping, "seq {seq} стал Ping");
            assert_eq!(b[seq].1, b[&10].1, "seq {seq}: тело общее");
        } else {
            assert_eq!(&b[seq], &(*msgid, data.clone()), "seq {seq} не тронут");
        }
    }
}

/// `agr:stream-ops` — в фильтре диапазон меняет тип только у найденного:
/// Order на экране стали Ping, Cancel между ними не тронуты.
#[test]
fn filtered_range_retype_touches_only_found() {
    io::quiet();
    if !lua() {
        return;
    }
    let path = copy("plain", "filter-retype");
    let mut d = open(&path);
    d.set_filter(tui_patch::lua::Filter {
        messages: vec!["Order".into()],
        expr: String::new(),
    });
    let t = Instant::now();
    while !d.matches.as_ref().is_some_and(|m| m.is_done()) {
        d.wait(Duration::from_millis(100));
        assert!(t.elapsed() < Duration::from_secs(30), "{}", d.status);
    }
    settle(&mut d);
    select(&mut d, 10);
    key(&mut d, Key::Char('v'));
    select(&mut d, 21);
    key(&mut d, Key::Char('T'));
    let p = d.prompt.as_mut().expect("выбор типа");
    p.sel = p.items.iter().position(|i| i.label == "Ping").unwrap();
    key(&mut d, Key::Enter);
    d.set_filter(tui_patch::lua::Filter::default());
    settle(&mut d);
    let out = commit_new(&mut d);
    let (a, b) = (by_seq(&path), by_seq(&out));
    let f = File::open(&out).unwrap();
    let name = |id: i32| f.scheme.message(id).unwrap().name.clone();
    for seq in 10..=21 {
        let want = if name(a[&seq].0) == "Order" {
            "Ping".to_string()
        } else {
            name(a[&seq].0)
        };
        assert_eq!(name(b[&seq].0), want, "seq {seq}");
        if want != "Ping" {
            assert_eq!(a[&seq], b[&seq], "seq {seq} не тронут");
        }
    }
    assert_eq!(name(b[&22].0), name(a[&22].0), "за диапазоном не тронуто");
}
