//! Склейка файлов (`agr:glue-spec`, `agr:glue-parts`, `agr:glue-commit`):
//! спека `a+b` и ротация `-N` открывают несколько датников одним видом, окна и
//! индекс идут по всем частям, фиксация пишет каждую часть отдельно — со
//! сдвигом перенумерации через границы и именами по фактическим границам.
//! Работает на собранной Writer-ом ротации во временном каталоге.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tui_patch::data::{fetch, File};
use tui_patch::decode;
use tui_patch::desk::{Desk, Focus, Key, PromptKind};
use tui_patch::edit::{commit, Edits, Span};
use tui_patch::io::{self, Context, Msg, Open, Reader, Writer};
use tui_patch::spec;

fn root() -> PathBuf {
    std::env::var_os("CARGO_MANIFEST_DIR")
        .map(Into::into)
        .unwrap_or_else(|| env!("CARGO_MANIFEST_DIR").into())
}

/// Каталог теста: свой, чистый.
fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("mutator-glue-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Собрать в `dir` ротацию из эталона: `r.0.dat` (seq 0–99), `r.100.dat`
/// (100–199), `r.current.dat` (200–249) — имена как у filename-key=first.
/// Ответ — сообщения, из которых собрана (в порядке следования).
fn rotation(dir: &Path) -> Vec<Msg> {
    let src = File::open(&root().join("tests/data/plain.dat")).unwrap();
    let scheme = src.scheme_url.clone().expect("схема эталона");
    let like = src.info.clone();
    let ctx = Context::new();
    let mut r = Reader::open(&ctx, &Open::file(root().join("tests/data/plain.dat"))).unwrap();
    let msgs: Vec<Msg> = r.by_ref().take(250).collect();
    let mut at = 0usize;
    for (name, n) in [
        ("r.0.dat", 100usize),
        ("r.100.dat", 100),
        ("r.current.dat", 50),
    ] {
        let mut w = Writer::create(&ctx, &dir.join(name), &scheme, &like)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        for m in &msgs[at..at + n] {
            w.post(m).expect("post");
        }
        drop(w);
        at += n;
    }
    msgs
}

/// Спека склейки всех частей ротации в `dir`.
fn glue_spec(dir: &Path) -> String {
    [
        dir.join("r.0.dat"),
        dir.join("r.100.dat"),
        dir.join("r.current.dat"),
    ]
    .iter()
    .map(|p| p.display().to_string())
    .collect::<Vec<_>>()
    .join("+")
}

fn open_spec(spec: &str, height: usize) -> Desk {
    io::quiet();
    let mut d = Desk::open(File::open_spec(spec).unwrap(), Arc::new(|| {}));
    d.height = height;
    let t = Instant::now();
    while d.index.is_none() {
        d.wait(Duration::from_millis(100));
        assert!(t.elapsed() < Duration::from_secs(30), "индекс не пришёл");
    }
    settle(&mut d);
    d
}

/// Дождаться, пока не останется запросов в полёте.
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

/// Дождаться конца фиксации: диалог закрылся (или остался с ошибкой).
fn settle_commit(d: &mut Desk) {
    let t = Instant::now();
    while d.commit.is_some() {
        let why = d.commit.as_ref().and_then(|c| c.error.clone());
        d.wait(Duration::from_millis(100));
        assert!(
            t.elapsed() < Duration::from_secs(60),
            "фиксация не кончилась: {why:?}"
        );
    }
    settle(d);
}

/// Все seq файла по порядку.
fn seqs_of(path: &Path) -> Vec<i64> {
    let ctx = Context::new();
    let mut r = Reader::open(&ctx, &Open::file(path)).unwrap();
    std::iter::from_fn(|| r.next()).map(|m| m.seq).collect()
}

/// Значение поля `path` сообщения `seq` файла.
fn field(path: &Path, seq: i64, name: &str) -> Option<String> {
    let f = File::open(path).unwrap();
    let m = Reader::open(&Context::new(), &Open::file(path))
        .unwrap()
        .find(|m| m.seq == seq)?;
    decode::field(&f.scheme, f.scheme.message(m.msgid)?, &m.data, name)
}

/// Первый seq сообщения типа `msgid` в `[from, to)`.
fn typed(msgs: &[Msg], msgid: i32, from: i64, to: i64) -> i64 {
    msgs.iter()
        .find(|m| m.seq >= from && m.seq < to && m.msgid == msgid)
        .unwrap_or_else(|| panic!("нет msgid {msgid} в [{from},{to})"))
        .seq
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

/// `agr:glue-spec` `agr:glue-parts` — «current-2» разворачивается в три файла
/// по возрастанию; вид открывает их одним потоком: строки сквозные, окна через
/// границы частей вперёд и назад, номер строки — по seq в средней части.
#[test]
fn glue_reads_rotation_as_one_stream() {
    io::quiet();
    let d0 = dir("read");
    rotation(&d0);

    let anchor = d0.join("r.current.dat-2").display().to_string();
    let files = spec::expand(&anchor).unwrap();
    let names: Vec<_> = files
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["r.0.dat", "r.100.dat", "r.current.dat"]);

    let spec = glue_spec(&d0);
    let d = open_spec(&spec, 10);
    assert_eq!(d.index.as_ref().unwrap().total, 250, "строки сквозные");
    assert_eq!((d.file.begin(), d.file.last()), (0, 249));

    let ctx = Context::new();
    let srcs = File::open_spec(&spec).unwrap().raw_sources();
    let range = |v: &[Msg]| v.iter().map(|m| m.seq).collect::<Vec<_>>();
    // Окно вперёд через границу первой и второй части.
    assert_eq!(
        range(&fetch::from_seq(&ctx, &srcs, 95, 10).unwrap()),
        (95..105).collect::<Vec<_>>()
    );
    // Через границу второй и третьей.
    assert_eq!(
        range(&fetch::from_seq(&ctx, &srcs, 197, 6).unwrap()),
        (197..203).collect::<Vec<_>>()
    );
    // Назад через границу.
    assert_eq!(
        range(&fetch::before_seq(&ctx, &srcs, 102, 5, 0).unwrap()),
        (97..102).collect::<Vec<_>>()
    );
    // Конец склейки — хвост последней части.
    assert_eq!(
        range(&fetch::before_seq(&ctx, &srcs, 250, 4, 0).unwrap()),
        (246..250).collect::<Vec<_>>()
    );
    // Строка по seq в средней части.
    assert_eq!(
        fetch::row_of(&ctx, &srcs, d.index.as_ref().unwrap(), 150).unwrap(),
        150
    );
}

/// `agr:glue-parts` — пересечение диапазонов склейки — ошибка открытия: файлы
/// обязаны идти по возрастанию без наложений, как карта rotate+.
#[test]
fn overlapping_glue_is_refused() {
    io::quiet();
    let d0 = dir("overlap");
    rotation(&d0);
    let spec = format!(
        "{}+{}",
        d0.join("r.100.dat").display(),
        d0.join("r.0.dat").display()
    );
    let e = File::open_spec(&spec).unwrap_err();
    assert!(e.contains("не по возрастанию"), "{e}");
}

/// `agr:glue-commit` — фиксация склейки пишет каждую часть с правками своим
/// файлом-тегом (число в имени — новая граница), чистую часть не трогает, один
/// и тот же patch раскладывается по частям сам.
#[test]
fn commit_writes_each_edited_part_with_tag_names() {
    io::quiet();
    let d0 = dir("commit");
    let msgs = rotation(&d0);
    let spec = glue_spec(&d0);
    let mut d = open_spec(&spec, 10);
    let mid = typed(&msgs, 10, 200, 250);
    set(&mut d, 5, "header.order_id", "777");
    set(&mut d, mid, "header.order_id", "778");

    d.open_commit();
    let c = d.commit.as_ref().expect("диалог");
    assert_eq!(c.new_name, "patched", "склейке имя — тег");
    d.start_commit();
    settle_commit(&mut d);
    assert!(d.commit.is_none(), "диалог закрылся");
    assert!(d.reopen.is_none(), "склейку заново не открываем");

    let (out1, out3) = (d0.join("r.0.patched.dat"), d0.join("r.current.patched.dat"));
    assert!(out1.is_file(), "первая часть записана: {}", d.status);
    assert!(out3.is_file(), "третья часть записана");
    assert!(
        !d0.join("r.100.patched.dat").exists(),
        "чистая часть не пишется"
    );
    assert_eq!(seqs_of(&out1), (0..100).collect::<Vec<_>>());
    assert_eq!(seqs_of(&out3), (200..250).collect::<Vec<_>>());
    assert_eq!(
        field(&out1, 5, "header.order_id").as_deref(),
        Some("777"),
        "правка легла в свою часть"
    );
    assert_eq!(field(&out3, mid, "header.order_id").as_deref(), Some("778"));
    assert!(
        field(&out1, mid, "header.order_id").is_none(),
        "чужой seq не переехал"
    );
}

/// Планы фиксации склейки — как их собирает диалог: части с правками, с
/// transform и сдвинутые правками ранних частей.
fn plans(file: &File, e: &Edits, transform: bool, tag: &str, overwrite: bool) -> Vec<commit::Plan> {
    let map = e.seq_map();
    let mut out = Vec::new();
    for (i, p) in file.parts.iter().enumerate() {
        let shift = map.before(p.begin());
        let span = Span {
            from: (i > 0).then(|| p.begin()),
            till: file.parts.get(i + 1).map(|n| n.begin()),
        };
        let last = span.till.map_or(i64::MAX, |t| t - 1);
        if !transform && !e.touches(p.begin(), last) && shift == 0 {
            continue;
        }
        if overwrite && commit::growing(&p.path) {
            panic!("растущая часть в перезаписи");
        }
        out.push(commit::Plan::Part {
            src: p.path.clone(),
            info: p.info.clone(),
            overwrite,
            tag: tag.into(),
            shift,
            span,
        });
    }
    out
}

/// `agr:glue-commit` — перенумерация переезжает через части: удаление со
/// сдвигом в первой части сдвигает начала поздних, имена выходов несут новые
/// границы (`r.99.patched.dat`), семья выходов дизъюнктна и восходит —
/// склеивается обратно одним видом.
#[test]
fn renumber_crosses_parts_and_outputs_stay_glueable() {
    io::quiet();
    let d0 = dir("renum");
    let msgs = rotation(&d0);
    let fixed = typed(&msgs, 10, 200, 250);
    let mut e = Edits::default();
    e.delete(50, "Order", true, Some(51));
    e.set(fixed, "Order", "header.order_id", "x", "777");
    let patch = e.commit_patch();
    let file = File::open_spec(&glue_spec(&d0)).unwrap();
    let ps = plans(&file, &e, false, "patched", false);
    assert_eq!(ps.len(), 3, "средняя часть пишется со сдвигом");
    let done = commit::run_all(&ps, patch.as_ref(), &AtomicBool::new(false), None, |_| {}).unwrap();
    assert_eq!(done.len(), 3);

    let (out1, out2, out3) = (
        d0.join("r.0.patched.dat"),
        d0.join("r.99.patched.dat"),
        d0.join("r.current.patched.dat"),
    );
    assert_eq!(
        seqs_of(&out1),
        (0..50)
            .chain(51..100)
            .map(|s| if s > 50 { s - 1 } else { s })
            .collect::<Vec<_>>(),
        "удаление со сдвигом внутри первой части"
    );
    assert_eq!(
        seqs_of(&out2),
        (99..199).collect::<Vec<_>>(),
        "средняя сдвинута на −1"
    );
    assert_eq!(seqs_of(&out3), (199..249).collect::<Vec<_>>());
    // Семья склеивается обратно одним видом.
    let glued = [out1, out2.clone(), out3]
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join("+");
    let g = File::open_spec(&glued).unwrap();
    assert_eq!((g.begin(), g.last()), (0, 248));
    // Правленное поле доехало в сдвинутую часть.
    assert_eq!(
        field(
            &d0.join("r.current.patched.dat"),
            fixed - 1,
            "header.order_id"
        )
        .as_deref(),
        Some("777")
    );
}

/// `agr:glue-commit` — вставка пишется той частью, перед чьим seq стоит, и
/// один раз: ранние части не дописывают её себе в хвост.
#[test]
fn insert_lands_once_in_its_own_part() {
    use tui_patch::edit::Insert;
    io::quiet();
    let d0 = dir("insert");
    rotation(&d0);
    let mut e = Edits::default();
    let ping = |n: &str| Insert {
        name: "Ping".into(),
        body: [("n".to_string(), n.to_string())].into(),
        occupied: true,
        data: Vec::new(),
    };
    e.insert(150, ping("150")).unwrap();
    e.insert(100, ping("100")).unwrap();
    let patch = e.commit_patch();
    let file = File::open_spec(&glue_spec(&d0)).unwrap();
    let ps = plans(&file, &e, false, "patched", false);
    let done = commit::run_all(&ps, patch.as_ref(), &AtomicBool::new(false), None, |_| {}).unwrap();
    assert_eq!(done.len(), 2, "первая часть без правок не пишется");
    let out2 = d0.join("r.100.patched.dat");
    let pings: Vec<_> = Reader::open(&Context::new(), &Open::file(&out2))
        .unwrap()
        .filter(|m| m.msgid == 30)
        .map(|m| m.seq)
        .collect();
    // Пинги файла — 104, 109, …; вставки — 100 и 151 (после первой +1).
    assert!(pings.contains(&100) && pings.contains(&151), "{pings:?}");
    assert_eq!(seqs_of(&out2).len(), 102, "две вставки, по одной");
    assert_eq!(
        seqs_of(&d0.join("r.current.patched.dat")).first(),
        Some(&202)
    );
}

/// `agr:glue-commit` — опустевшая часть файла-результата не оставляет: пустой
/// нумерованный файл ломал бы rotate+. Новым файлом — выхода просто нет,
/// перезаписью — исходник в бэкапе; поздние части сдвигаются на удалённое.
#[test]
fn emptied_part_writes_no_file() {
    io::quiet();
    let d0 = dir("empty-new");
    rotation(&d0);
    let mut e = Edits::default();
    for s in 100..200 {
        e.delete(s, "Order", true, Some(s + 1));
    }
    let patch = e.commit_patch();
    let file = File::open_spec(&glue_spec(&d0)).unwrap();
    let ps = plans(&file, &e, false, "patched", false);
    assert_eq!(ps.len(), 2, "первая чистая и не сдвинутая — не пишется");
    let done = commit::run_all(&ps, patch.as_ref(), &AtomicBool::new(false), None, |_| {}).unwrap();
    assert_eq!(
        done.iter().filter(|d| d.empty).count(),
        1,
        "средняя опустела"
    );
    let mut outs: Vec<_> = std::fs::read_dir(&d0)
        .unwrap()
        .flatten()
        .map(|x| x.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("patched"))
        .collect();
    outs.sort();
    assert_eq!(outs, vec!["r.current.patched.dat"], "выход — только хвост");
    assert_eq!(
        seqs_of(&d0.join("r.current.patched.dat")),
        (100..150).collect::<Vec<_>>(),
        "хвост сдвинут на −100"
    );
    assert!(
        d0.join("r.100.dat").is_file(),
        "исходник средней части на месте"
    );

    // Перезапись: пара без растущей части, удаляем всю вторую.
    let d1 = dir("empty-ow");
    rotation(&d1);
    let mut e = Edits::default();
    for s in 100..200 {
        e.delete(s, "Order", true, Some(s + 1));
    }
    let patch = e.commit_patch();
    let spec = format!(
        "{}+{}",
        d1.join("r.0.dat").display(),
        d1.join("r.100.dat").display()
    );
    let file = File::open_spec(&spec).unwrap();
    let ps = plans(&file, &e, false, "orig", true);
    assert_eq!(ps.len(), 1, "только опустевшая часть");
    let done = commit::run_all(&ps, patch.as_ref(), &AtomicBool::new(false), None, |_| {}).unwrap();
    assert!(done[0].empty);
    assert!(d1.join("r.100.orig.dat").is_file(), "исходник — в бэкапе");
    assert!(!d1.join("r.100.dat").exists(), "пустого результата нет");
    assert!(
        d1.join("r.0.dat").is_file(),
        "чистая первая часть не тронута"
    );
}

/// `agr:glue-commit` — перезапись ложит результат под пересчитанным именем
/// ротации (`r.99.dat`), исходники — в невидимые бэкапы, набор остаётся
/// правильно нумерованным: `-N` разворачивается и по новым именам.
#[test]
fn overwrite_renames_parts_to_recalculated_names() {
    io::quiet();
    let d0 = dir("ow");
    rotation(&d0);
    let mut e = Edits::default();
    e.delete(50, "Order", true, Some(51));
    let patch = e.commit_patch();
    let spec = format!(
        "{}+{}",
        d0.join("r.0.dat").display(),
        d0.join("r.100.dat").display()
    );
    let file = File::open_spec(&spec).unwrap();
    let ps = plans(&file, &e, false, "orig", true);
    assert_eq!(ps.len(), 2, "вторая часть пишется со сдвигом");
    commit::run_all(&ps, patch.as_ref(), &AtomicBool::new(false), None, |_| {}).unwrap();

    // Результаты — под пересчитанными именами, исходники — в бэкапах.
    assert_eq!(
        seqs_of(&d0.join("r.0.dat")),
        (0..50)
            .chain(51..100)
            .map(|s| if s > 50 { s - 1 } else { s })
            .collect::<Vec<_>>()
    );
    assert_eq!(seqs_of(&d0.join("r.99.dat")), (99..199).collect::<Vec<_>>());
    assert_eq!(
        seqs_of(&d0.join("r.0.orig.dat")),
        (0..100).collect::<Vec<_>>()
    );
    assert_eq!(
        seqs_of(&d0.join("r.100.orig.dat")),
        (100..200).collect::<Vec<_>>()
    );

    // Новый набор читается спекой `-N` по новым номерам.
    let again = spec::expand(&d0.join("r.99.dat-1").display().to_string()).unwrap();
    let names: Vec<_> = again
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["r.0.dat", "r.99.dat"]);
}

/// `agr:glue-commit` — ключ имени части виден по номеру (названа последним seq —
/// новое имя по новому последнему), растущая часть перезапись запрещает,
/// флаг отмены останавливает проход.
#[test]
fn part_naming_key_and_guards() {
    io::quiet();
    let d0 = dir("key-last");
    let msgs = rotation(&d0);
    // Средняя часть названа последним seq (filename-key=last), а не первым.
    std::fs::rename(d0.join("r.100.dat"), d0.join("r.199.dat")).unwrap();
    let mut e = Edits::default();
    e.delete(50, "Order", true, Some(51));
    let patch = e.commit_patch();
    let spec = format!(
        "{}+{}",
        d0.join("r.0.dat").display(),
        d0.join("r.199.dat").display()
    );
    let file = File::open_spec(&spec).unwrap();
    let ps = plans(&file, &e, false, "patched", false);
    commit::run_all(&ps, patch.as_ref(), &AtomicBool::new(false), None, |_| {}).unwrap();
    // Сдвиг −1: диапазон 99..198, ключ last → имя по новому последнему.
    assert_eq!(
        seqs_of(&d0.join("r.198.patched.dat")),
        (99..199).collect::<Vec<_>>(),
        "имя по ключу last"
    );

    // Растущая часть не перезаписывается даже планом напрямую.
    let mut e2 = Edits::default();
    e2.set(
        typed(&msgs, 10, 200, 250),
        "Order",
        "header.order_id",
        "x",
        "777",
    );
    let patch2 = e2.commit_patch();
    let stop = AtomicBool::new(false);
    let cur = d0.join("r.current.dat");
    let err = commit::run_all(
        &[commit::Plan::Part {
            src: cur.clone(),
            info: file.parts[0].info.clone(),
            overwrite: true,
            tag: "orig".into(),
            shift: 0,
            span: Default::default(),
        }],
        patch2.as_ref(),
        &stop,
        None,
        |_| {},
    )
    .unwrap_err();
    assert!(err.contains("растущий"), "{err}");

    // Отменённый проход не пишет ничего.
    let stop2 = AtomicBool::new(true);
    let err = commit::run_all(
        &[commit::Plan::Part {
            src: cur,
            info: file.parts[0].info.clone(),
            overwrite: false,
            tag: "patched".into(),
            shift: 0,
            span: Default::default(),
        }],
        patch2.as_ref(),
        &stop2,
        None,
        |_| {},
    )
    .unwrap_err();
    assert_eq!(err, "отменено");
    assert!(!d0.join("r.current.patched.dat").exists());
}

/// `agr:glue-commit` — правка, которую не записать, останавливает запись
/// части: ошибка, временный файл убран, выхода нет, исходник цел.
#[test]
fn unwritable_edit_stops_and_cleans() {
    io::quiet();
    let d0 = dir("broken");
    let msgs = rotation(&d0);
    let src = d0.join("r.0.dat");
    let before = std::fs::read(&src).unwrap();
    let info = File::open(&src).unwrap().parts[0].info.clone();
    let mut e = Edits::default();
    e.set(
        typed(&msgs, 10, 0, 100),
        "Order",
        "header.order_id",
        "x",
        "не число",
    );
    let patch = e.commit_patch();
    let err = commit::run_all(
        &[commit::Plan::Part {
            src: src.clone(),
            info,
            overwrite: false,
            tag: "patched".into(),
            shift: 0,
            span: Default::default(),
        }],
        patch.as_ref(),
        &AtomicBool::new(false),
        None,
        |_| {},
    )
    .unwrap_err();
    assert!(!err.is_empty(), "ошибка есть");
    assert!(!d0.join("r.0.patched.dat").exists(), "выхода нет");
    assert_eq!(std::fs::read(&src).unwrap(), before, "исходник цел");
    let tmp_left = std::fs::read_dir(&d0)
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().contains(".tmp"));
    assert!(!tmp_left, "временный файл убран");
}

/// `agr:glue-parts` — растущий хвост живёт в последней части: `r` перечитывает
/// спеку и считает дописанное.
#[test]
fn reload_rereads_glue_tail() {
    io::quiet();
    let d0 = dir("reload");
    rotation(&d0);
    let spec = d0.join("r.current.dat-1").display().to_string();
    let mut d = open_spec(&spec, 8);
    key(&mut d, Key::End);

    let cur = d0.join("r.current.dat");
    let f = File::open(&cur).unwrap();
    let (scheme, info) = (f.scheme_url.clone().unwrap(), f.info.clone());
    let ctx = Context::new();
    let mut w = Writer::open(&ctx, &cur, &scheme, &info, false).expect("дописать");
    w.post(&Msg {
        seq: 250,
        msgid: 10,
        data: vec![0; 4],
    })
    .expect("post");
    drop(w);

    key(&mut d, Key::Down);
    assert!(d.status.contains("+1 строк"), "{}", d.status);
}
