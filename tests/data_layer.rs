//! Слой данных на эталонных файлах: индекс, окна, фильтр и поиск сверяются с
//! прямым чтением всего файла. Фильтр и поиск — при наличии tll-lua
//! (`TUI_PATCH_LUA`), иначе пропускаются с сообщением.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use tui_patch::data::{fetch, At, Cmd, Edge, Ev, File, Place, View, Worker};
use tui_patch::lua::Filter;
use tui_patch::tll::{self, Context, LuaMode, Msg, Open, Reader};

/// Корень крейта во время запуска: сборка могла идти в другом каталоге.
fn root() -> std::path::PathBuf {
    std::env::var_os("CARGO_MANIFEST_DIR")
        .map(Into::into)
        .unwrap_or_else(|| env!("CARGO_MANIFEST_DIR").into())
}

fn fixture(name: &str) -> PathBuf {
    root().join("tests/data").join(format!("{name}.dat"))
}

fn all(path: &Path) -> Vec<Msg> {
    let ctx = Context::new();
    let mut r = Reader::open(&ctx, &Open::file(path)).unwrap();
    std::iter::from_fn(|| r.next()).collect()
}

fn lua() -> bool {
    tll::quiet();
    let ok = Context::new().ensure_lua();
    if !ok {
        eprintln!("нет tll-lua (TUI_PATCH_LUA) — проверки фильтра пропущены");
    }
    ok
}

fn seqs(v: &[Msg]) -> Vec<i64> {
    v.iter().map(|m| m.seq).collect()
}

fn worker(path: &Path) -> (Worker, Arc<tui_patch::data::Index>) {
    let w = Worker::start(File::open(path).unwrap(), Arc::new(|| {}));
    loop {
        match w.wait(Duration::from_secs(20)).expect("индекс не пришёл") {
            Ev::Index(i) => return (w, i),
            Ev::Error(e) => panic!("{e}"),
            _ => {}
        }
    }
}

fn window(w: &Worker) -> (Place, Vec<i64>, Option<usize>, bool, bool) {
    loop {
        match w.wait(Duration::from_secs(20)).expect("окно не пришло") {
            Ev::Window {
                place,
                rows,
                first_row,
                at_start,
                at_end,
                ..
            } => return (place, seqs(&rows), first_row, at_start, at_end),
            Ev::Error(e) => panic!("{e}"),
            _ => {}
        }
    }
}

/// Индекс строится параллельно в нескольких контекстах libtll и знает число
/// строк и типы; окно по номеру строки и номер строки по seq совпадают с прямым
/// чтением — и на плотных, и на разреженных seq.
/// `agr:sparse-index` `agr:tll-lifecycle-lock`
#[test]
fn index_and_windows_match_full_read() {
    tll::quiet();
    for name in ["plain", "lz4", "sparse"] {
        let path = fixture(name);
        let msgs = all(&path);
        let (w, idx) = worker(&path);
        assert_eq!(idx.total, msgs.len(), "{name}");
        assert_eq!(idx.hist.values().sum::<usize>(), msgs.len());
        let ctx = Context::new();
        for row in [0, 1, 37, msgs.len() / 2, msgs.len() - 1] {
            let got = fetch::from_row(&ctx, &Open::file(&path), &idx, row, 5).unwrap();
            assert_eq!(
                seqs(&got),
                seqs(&msgs[row..(row + 5).min(msgs.len())]),
                "{name} row {row}"
            );
            assert_eq!(
                fetch::row_of(&ctx, &Open::file(&path), &idx, msgs[row].seq).unwrap(),
                row
            );
        }

        // Окно с середины, дочитывание назад и вперёд, конец файла.
        let mid = msgs.len() / 2;
        w.send(Cmd::Window {
            gen: 1,
            view: View::All,
            at: At::Seq(msgs[mid].seq),
            n: 10,
        });
        let (_, rows, first, _, _) = window(&w);
        assert_eq!(rows, seqs(&msgs[mid..mid + 10]));
        assert_eq!(first, Some(mid));
        w.send(Cmd::Extend {
            gen: 1,
            view: View::All,
            after: false,
            edge: Edge {
                seq: rows[0],
                row: first,
            },
            n: 7,
        });
        let (place, before, bfirst, _, _) = window(&w);
        assert_eq!(
            (place, before, bfirst),
            (Place::Before, seqs(&msgs[mid - 7..mid]), Some(mid - 7))
        );
        w.send(Cmd::Window {
            gen: 2,
            view: View::All,
            at: At::End,
            n: 4,
        });
        let (_, tail, tfirst, _, at_end) = window(&w);
        assert_eq!(tail, seqs(&msgs[msgs.len() - 4..]));
        assert_eq!(tfirst, Some(msgs.len() - 4));
        assert!(at_end);
        w.send(Cmd::Window {
            gen: 3,
            view: View::All,
            at: At::Seq(i64::MIN),
            n: 3,
        });
        let (_, head, _, at_start, _) = window(&w);
        assert_eq!(head, seqs(&msgs[..3]));
        assert!(at_start);
        // Назад от начала — пусто и упор.
        assert!(
            fetch::before_seq(&ctx, &Open::file(&path), msgs[0].seq, 5, msgs[0].seq)
                .unwrap()
                .is_empty()
        );
    }
}

/// Сообщения по списку seq: подряд, с пропусками и с несуществующими seq.
#[test]
fn by_seqs_returns_exactly_requested() {
    tll::quiet();
    let path = fixture("sparse");
    let msgs = all(&path);
    let ctx = Context::new();
    let want: Vec<i64> = msgs
        .iter()
        .map(|m| m.seq)
        .filter(|s| s % 3 == 0 || *s > 3000)
        .collect();
    let mut ask = want.clone();
    ask.insert(1, want[1] - 1); // такого seq нет: шаг 10
    ask.sort();
    let got = fetch::by_seqs(&ctx, &Open::file(&path), &ask).unwrap();
    assert_eq!(seqs(&got), want);
}

/// `agr:filter-streams` — фильтр по кускам находит ровно то же, что Lua-фильтр
/// одним проходом, а поиск — ближайшее совпадение в обе стороны.
#[test]
fn filter_and_search_match_single_pass() {
    if !lua() {
        return;
    }
    let path = fixture("lz4");
    let filter = Filter {
        messages: vec!["Order".into(), "Cancel".into()],
        expr: "data.header.status == 'Fail'".into(),
    };
    let ctx = Context::new();
    let mut r = Reader::open(&ctx, &Open::file(&path).filter(filter.code())).unwrap();
    let want: Vec<i64> = std::iter::from_fn(|| r.next()).map(|m| m.seq).collect();
    assert!(want.len() > 100);

    let (w, _) = worker(&path);
    w.send(Cmd::Filter {
        id: 7,
        filter: filter.clone(),
    });
    let m = loop {
        if let Some(Ev::Filter { id: 7, matches }) = w.wait(Duration::from_secs(20)) {
            break matches;
        }
    };
    while !m.is_done() {
        w.wait(Duration::from_secs(20));
    }
    assert_eq!(*m.seqs.read().unwrap(), want);

    // Окно отфильтрованного вида — по номеру строки.
    w.send(Cmd::Window {
        gen: 10,
        view: View::Filtered(m.clone()),
        at: At::Row(50),
        n: 6,
    });
    let (_, rows, first, _, _) = window(&w);
    assert_eq!((rows, first), (want[50..56].to_vec(), Some(50)));

    // Поиск: ближайшее после и до seq из середины.
    let stop = Arc::new(AtomicBool::new(false));
    let file = File::open(&path).unwrap();
    let from = want[want.len() / 2];
    let begin = file.info.seq_begin.unwrap();
    let last = file.info.seq_last.unwrap();
    for (back, expect) in [
        (false, want[want.len() / 2 + 1]),
        (true, want[want.len() / 2 - 1]),
    ] {
        w.send(Cmd::Search {
            id: 3,
            filter: filter.clone(),
            from,
            back,
        });
        let got = loop {
            if let Some(Ev::Found { id: 3, seq, error }) = w.wait(Duration::from_secs(20)) {
                assert_eq!(error, None);
                break seq;
            }
        };
        assert_eq!(got, Some(expect), "back={back}");
    }
    let none = tui_patch::lua::Filter::expr("false");
    w.send(Cmd::Search {
        id: 4,
        filter: none,
        from: begin,
        back: false,
    });
    loop {
        if let Some(Ev::Found { id: 4, seq, .. }) = w.wait(Duration::from_secs(20)) {
            assert_eq!(seq, None);
            break;
        }
    }
    let _ = (stop, last);
}

/// Открыть канал с seq за концом файла — ошибка или пустое чтение, но не
/// зависание: замок жизненного цикла снят до освобождения канала
/// (`agr:tll-lifecycle-lock`).
#[test]
fn open_past_end_does_not_hang() {
    tll::quiet();
    let path = fixture("plain");
    let ctx = Context::new();
    for _ in 0..3 {
        if let Ok(r) = Reader::open(&ctx, &Open::file(&path).at(1_000_000)) {
            assert_eq!(r.count(), 0);
        }
    }
    let ctx2 = Context::new();
    assert!(
        Reader::open(&ctx2, &Open::file(&path)).is_ok(),
        "замок свободен"
    );
}

/// Ошибка Lua видна у читателя: синтаксис — при открытии, сравнение с `nil`
/// — после чтения, с подсказкой и сообщением, на котором упало.
#[test]
fn reader_reports_lua_errors() {
    if !lua() {
        return;
    }
    let path = fixture("lz4");
    let ctx = Context::new();
    let bad = Filter::expr("data.n AND true");
    let e = Reader::open(&ctx, &Open::file(&path).filter(bad.code()))
        .err()
        .expect("синтаксис");
    assert!(
        e.contains("near 'AND'") && e.contains("синтаксис Lua"),
        "{e}"
    );
    let nil = Filter::expr("data.n > 5");
    let mut r = Reader::open(&ctx, &Open::file(&path).filter(nil.code())).unwrap();
    assert_eq!(r.by_ref().count(), 0);
    let e = r.error().expect("ошибка после чтения");
    assert!(
        e.contains("with nil") && e.contains("Order, seq 100"),
        "{e}"
    );
    let mut ok = Reader::open(&ctx, &Open::file(&path)).unwrap();
    assert!(ok.by_ref().count() > 0);
    assert_eq!(ok.error(), None);
}

/// Дефолтовый transform-шаблон проходит через libtll на реальном файле: ни
/// одно сообщение не падает, ошибки канала нет. Раньше шаблон звал
/// `tll_child_post(data)` с одним аргументом — Lua-синтаксис валиден, но
/// `encode_stack` падает «Too small number of arguments: 1 < min 3», а в
/// следующей правке `tll_child_post` пытался писать в дочерний файл-канал —
/// строковая проверка ни то, ни другое не ловила, нужен живой прогон.
#[test]
fn default_transform_template_runs_through_reader() {
    if !lua() {
        return;
    }
    let path = fixture("lz4");
    let ctx = Context::new();
    let mut open = Open::file(&path).filter(Some(tui_patch::lua::transform_template()));
    open.lua_mode = Some(LuaMode::Transform);
    let mut r = Reader::open(&ctx, &open).unwrap();
    let n = r.by_ref().count();
    assert!(n > 0, "шаблон должен пропустить все сообщения файла");
    assert_eq!(
        r.error(),
        None,
        "трансформация не должна падать на сообщениях"
    );
}

/// Код мутатора для тестов: тело перекодируется из таблицы — все поля в ноль,
/// кроме `u8` (у таблицы-тела недостающие поля остаются нулями).
fn mutator() -> String {
    "function tll_on_data(seq, name, data)\n    tll_callback(seq, name, { u8 = 1 })\nend".into()
}

fn passthrough() -> String {
    "function tll_on_data(seq, name, data)\n    tll_callback(seq, name, data)\nend".into()
}

fn dropper() -> String {
    "function tll_on_data(seq, name, data)\nend".into()
}

/// Открыть файл сразу в transform-режиме, как его читает полный конвейер.
fn open_transform(path: &Path, code: String, seq: i64) -> Open {
    let mut o = Open::file(path).filter(Some(code));
    o.lua_mode = Some(LuaMode::Transform);
    o.at(seq)
}

/// Собрать окно, накопив попутные события; вернуть (строки, mutated, ошибка).
fn transformed_window(w: &Worker) -> (Vec<Msg>, tui_patch::data::Mutations, Option<String>) {
    let mut window = None;
    for _ in 0..10 {
        match w.wait(Duration::from_secs(20)).expect("окно не пришло") {
            Ev::TransformError(_) => {}
            Ev::Window {
                rows,
                mutated,
                transform_error,
                ..
            } => {
                window = Some((rows, mutated, transform_error));
                break;
            }
            Ev::Error(e) => panic!("{e}"),
            _ => {}
        }
    }
    window.expect("окно среди событий")
}

/// `agr:transform-shadow` — пара `lua+direct` прогоняет тело так же, как полный
/// конвейер `lua+file`: passthrough не меняет тело, мутатор даёт тот же байт.
#[test]
fn transform_via_direct_pair_matches_full_pipeline() {
    if !lua() {
        return;
    }
    let path = fixture("plain");
    let url = File::open(&path)
        .unwrap()
        .scheme_url
        .expect("схема файла в url");
    let ctx = Context::new();
    let raw = all(&path).remove(0);

    let out = tui_patch::tll::Transformer::new(&ctx, &passthrough(), &url)
        .unwrap()
        .transform(&raw)
        .unwrap()
        .expect("passthrough не выбрасывает сообщение");
    assert_eq!(out.seq, raw.seq);
    assert_eq!(out.data, raw.data);
    let mut r = Reader::open(&ctx, &open_transform(&path, passthrough(), raw.seq)).unwrap();
    assert_eq!(r.next().unwrap().data, raw.data);

    let mut tx = tui_patch::tll::Transformer::new(&ctx, &mutator(), &url).unwrap();
    let m = tx.transform(&raw).unwrap().expect("мутатор вернул тело");
    let mut r = Reader::open(&ctx, &open_transform(&path, mutator(), raw.seq)).unwrap();
    assert_eq!(
        m.data,
        r.next().unwrap().data,
        "мутатор совпадает с конвейером"
    );
}

/// `agr:transform-shadow` — мутатор без `tll_callback` выбрасывает сообщение.
#[test]
fn transform_reports_dropped_message_as_none() {
    if !lua() {
        return;
    }
    let path = fixture("plain");
    let url = File::open(&path).unwrap().scheme_url.unwrap();
    let ctx = Context::new();
    let raw = all(&path).remove(0);
    let mut tx = tui_patch::tll::Transformer::new(&ctx, &dropper(), &url).unwrap();
    assert!(tx.transform(&raw).unwrap().is_none());
}

/// `agr:transform-shadow` — битый код: синтаксис валит сборку пары, ошибка
/// исполнения — каждый прогон; канал не зацикливается.
#[test]
fn broken_lua_code_fails_transformer() {
    if !lua() {
        return;
    }
    let path = fixture("plain");
    let url = File::open(&path).unwrap().scheme_url.unwrap();
    let ctx = Context::new();
    let raw = all(&path).remove(0);
    assert!(tui_patch::tll::Transformer::new(
        &ctx,
        "function tll_on_data(seq, name, data) end (",
        &url
    )
    .is_err());
    let mut tx = tui_patch::tll::Transformer::new(
        &ctx,
        "function tll_on_data(seq, name, data) error('boom') end",
        &url,
    )
    .unwrap();
    assert!(tx.transform(&raw).is_err());
    assert!(tx.transform(&raw).is_err(), "ошибка повторяется");
}

/// `agr:transform-shadow` — при включённом transform окно приходит прогоным
/// (как на фиксации), различия едут отдельной картой: seq 0 помечен, в карте —
/// значение из файла.
#[test]
fn window_rows_show_mutated_bodies_with_marks() {
    if !lua() {
        return;
    }
    let path = fixture("plain");
    let (w, _idx) = worker(&path);
    w.send(Cmd::Lua {
        lua: Some(mutator()),
        lua_mode: Some(LuaMode::Transform),
    });
    w.send(Cmd::Window {
        gen: 1,
        view: View::All,
        at: At::Seq(0),
        n: 8,
    });
    let (rows, mutated, terr) = transformed_window(&w);
    assert_eq!(terr, None);
    for (r, expect) in rows.iter().zip(all(&path).iter()) {
        let marked = mutated.contains_key(&r.seq);
        assert_eq!(r.data != expect.data, marked, "тело прогоное у помеченных");
    }
    assert!(mutated.contains_key(&0), "первое сообщение помечено");
    assert_eq!(
        mutated[&0].get("u8").map(String::as_str),
        Some("255"),
        "в карте — значение из файла"
    );
}

/// `agr:transform-shadow` — passthrough ничего не помечает.
#[test]
fn passthrough_transform_marks_nothing() {
    if !lua() {
        return;
    }
    let path = fixture("plain");
    let (w, _idx) = worker(&path);
    w.send(Cmd::Lua {
        lua: Some(passthrough()),
        lua_mode: Some(LuaMode::Transform),
    });
    w.send(Cmd::Window {
        gen: 1,
        view: View::All,
        at: At::Seq(0),
        n: 8,
    });
    let (rows, mutated, terr) = transformed_window(&w);
    assert_eq!(terr, None);
    assert_eq!(rows.len(), 8, "окно сырое");
    assert!(
        mutated.is_empty(),
        "passthrough ничего не меняет: {mutated:?}"
    );
}

/// `agr:transform-shadow` — выброшенные мутатором сообщения не попадают в вид:
/// transform стоит в канале чтения, строки в окне — только те, что скрипт
/// вернул наверх (как уйдёт в файл при фиксации).
#[test]
fn mutator_dropped_messages_stay_out_of_view() {
    if !lua() {
        return;
    }
    let path = fixture("plain");
    let (w, _idx) = worker(&path);
    w.send(Cmd::Lua {
        lua: Some(dropper()),
        lua_mode: Some(LuaMode::Transform),
    });
    w.send(Cmd::Window {
        gen: 1,
        view: View::All,
        at: At::Seq(0),
        n: 8,
    });
    let (rows, mutated, terr) = transformed_window(&w);
    assert_eq!(terr, None);
    assert!(
        rows.is_empty(),
        "dropper не возвращает сообщения — вид пуст"
    );
    assert!(mutated.is_empty());
}

/// `agr:transform-shadow` — битый код не роняет окно: строки сырые (прогон
/// не удался), mutated пуст, ошибка видна и в [`Ev::TransformError`], и в окне.
#[test]
fn broken_transform_code_leaves_raw_window_and_reports_error() {
    if !lua() {
        return;
    }
    let path = fixture("plain");
    let (w, _idx) = worker(&path);
    w.send(Cmd::Lua {
        lua: Some("function tll_on_data(seq, name, data) error('boom') end".into()),
        lua_mode: Some(LuaMode::Transform),
    });
    w.send(Cmd::Window {
        gen: 1,
        view: View::All,
        at: At::Seq(0),
        n: 8,
    });
    let mut seen_error = false;
    let mut window = None;
    for _ in 0..10 {
        match w.wait(Duration::from_secs(20)).expect("событие") {
            Ev::TransformError(e) => {
                assert!(e.contains("boom"), "{e}");
                seen_error = true;
            }
            Ev::Window {
                rows,
                mutated,
                transform_error,
                ..
            } => {
                assert!(transform_error.is_some());
                assert_eq!(rows.len(), 8, "строки сырые");
                assert!(mutated.is_empty());
                window = Some(());
                break;
            }
            Ev::Error(e) => panic!("{e}"),
            _ => {}
        }
    }
    assert!(window.is_some(), "окно пришло");
    assert!(seen_error, "TransformError пришёл");
}
