//! Разбор совпадает с python-tll (а значит с tll-read) на эталонах
//! `tests/data/*.dat` + `*.json` (их пишет `scripts/golden.py`).
//!
//! `MUTATOR_GOLDEN=файл.dat:эталон.json` — сверить ещё и живой файл (эталон —
//! `scripts/golden.py файл.dat эталон.json`).

use std::collections::HashMap;
use std::path::Path;

use serde_json::{json, Map, Value as J};
use tui_patch::decode::{self, Node, Value};
use tui_patch::dump::{self, Dump};
use tui_patch::io::{Context, Open, Reader};
use tui_patch::lua::Filter;
use tui_patch::range::Range;

/// Корень крейта во время запуска: сборка могла идти в другом каталоге.
fn root() -> std::path::PathBuf {
    std::env::var_os("CARGO_MANIFEST_DIR")
        .map(Into::into)
        .unwrap_or_else(|| env!("CARGO_MANIFEST_DIR").into())
}

fn to_json(n: &Node) -> J {
    if let Some(Value::Bits(b)) = &n.value {
        return J::Object(
            b.iter()
                .map(|(k, v, _)| (k.clone(), json!(v.to_string())))
                .collect(),
        );
    }
    if let Some(v) = &n.value {
        return json!(v.text());
    }
    if let Some(e) = &n.error {
        panic!("ошибка разбора {}: {e}", n.path);
    }
    if n.ty.starts_with('*') || n.ty.ends_with(']') {
        return J::Array(n.children.iter().map(to_json).collect());
    }
    let mut m = Map::new();
    for c in &n.children {
        m.insert(c.name.clone(), to_json(c));
    }
    J::Object(m)
}

fn check(dat: &Path, expected: &Path) -> usize {
    tui_patch::io::quiet();
    let want: Vec<J> = serde_json::from_str(&std::fs::read_to_string(expected).unwrap()).unwrap();
    let want: HashMap<i64, &J> = want
        .iter()
        .map(|w| (w["seq"].as_i64().unwrap(), w))
        .collect();
    let ctx = Context::new();
    let r = Reader::open(&ctx, &Open::file(dat)).unwrap();
    let scheme = r.scheme().expect("схема в файле");
    let mut seen = 0;
    for m in r {
        let Some(w) = want.get(&m.seq) else { continue };
        let st = scheme.message(m.msgid).expect("msgid в схеме");
        assert_eq!(w["name"], json!(st.name), "seq {}", m.seq);
        let got = to_json(&decode::decode(&scheme, st, &m.data));
        assert_eq!(got, w["data"], "seq {} {}", m.seq, st.name);
        seen += 1;
    }
    assert_eq!(seen, want.len(), "не все эталонные сообщения прочитаны");
    seen
}

/// `agr:same-as-tll-read` — каждое сообщение эталонов разбирается в то же дерево, что у python-tll:
/// все типы полей, pmap, union, три версии offset-ptr, LZ4 и разреженные seq.
#[test]
fn decode_matches_python_tll_on_fixtures() {
    let dir = root().join("tests/data");
    for name in ["plain", "lz4", "sparse"] {
        let n = check(
            &dir.join(format!("{name}.dat")),
            &dir.join(format!("{name}.json")),
        );
        assert!(n > 100, "{name}: мало сообщений {n}");
    }
}

/// `agr:stream-ops` `agr:edit-encode` — состав потока даёт тот же поток, что
/// настоящий tll-patch: `tests/data/stream.json` пишет `scripts/golden.py`
/// (удаление со сдвигом и дырой, смена типа, вставки в живой seq и в дыру,
/// тела со списками всех offset-ptr, правки полей после сдвига).
#[test]
fn stream_matches_tll_patch() {
    use tui_patch::edit::{Body, Edits, Insert};
    tui_patch::io::quiet();
    let dir = root().join("tests/data");
    let cases: Vec<J> =
        serde_json::from_str(&std::fs::read_to_string(dir.join("stream.json")).unwrap()).unwrap();
    let body = |v: &J| -> Body {
        v.as_object()
            .unwrap()
            .iter()
            .map(|(k, x)| (k.clone(), x.as_str().unwrap().to_string()))
            .collect()
    };
    for c in &cases {
        let mut e = Edits::default();
        for o in c["ops"].as_array().unwrap() {
            let seq = o["seq"].as_i64().unwrap();
            let s = |k: &str| o[k].as_str().unwrap_or_default().to_string();
            match o["op"].as_str().unwrap() {
                "remove" => e.delete(seq, &s("from"), true, None),
                "clear" => e.delete(seq, &s("from"), false, None),
                "retype" => e
                    .retype(seq, &s("from"), &s("name"), body(&o["body"]))
                    .unwrap(),
                "insert" => {
                    let ins = Insert {
                        name: s("name"),
                        body: body(&o["body"]),
                        occupied: o["occupied"].as_bool().unwrap(),
                        data: Vec::new(),
                    };
                    e.insert(seq, ins).unwrap();
                }
                "field" => {
                    for (path, v) in body(&o["fields"]) {
                        e.set(seq, &s("name"), &path, "\u{0}", &v);
                    }
                }
                op => panic!("op {op}"),
            }
        }
        let ctx = Context::new();
        let src = Open::file(dir.join(c["file"].as_str().unwrap())).patch(e.commit_patch());
        let mut r = Reader::open(&ctx, &src).unwrap();
        let scheme = r.scheme().expect("схема в файле");
        let got: Vec<_> = r.by_ref().collect();
        assert_eq!(r.error(), None);
        let want = c["out"].as_array().unwrap();
        assert_eq!(got.len(), want.len(), "{}", c["file"]);
        for (m, w) in got.iter().zip(want) {
            let st = scheme.message(m.msgid).unwrap();
            assert_eq!(
                (m.seq, st.name.as_str()),
                (w["seq"].as_i64().unwrap(), w["name"].as_str().unwrap())
            );
            if !w["data"].is_null() {
                let d = to_json(&decode::decode(&scheme, st, &m.data));
                assert_eq!(d, w["data"], "{} seq {}", c["file"], m.seq);
            }
        }
    }
}

/// `agr:edit-encode` — свои правки дают то же тело, что настоящий tll-patch:
/// `tests/data/patch.json` пишет `scripts/golden.py` прогоном тех же правок
/// через `patch+file://` (все виды скаляров, offset-строка, вариант union,
/// поле за pmap).
#[test]
fn edits_match_tll_patch() {
    tui_patch::io::quiet();
    let dir = root().join("tests/data");
    let want: Vec<J> =
        serde_json::from_str(&std::fs::read_to_string(dir.join("patch.json")).unwrap()).unwrap();
    let mut edits = tui_patch::edit::Edits::default();
    for w in &want {
        let seq = w["seq"].as_i64().unwrap();
        for (path, v) in w["edits"].as_object().unwrap() {
            // «Было» — заведомо не новое: иначе `set` сочтёт правку снятой.
            let new = v.as_str().unwrap();
            edits.set(seq, w["name"].as_str().unwrap(), path, "\u{0}", new);
        }
    }
    let ctx = Context::new();
    let src = Open::file(dir.join("plain.dat")).patch(edits.patch());
    let r = Reader::open(&ctx, &src).unwrap();
    let scheme = r.scheme().expect("схема в файле");
    let got: HashMap<i64, J> = r
        .filter(|m| edits.message(m.seq).is_some())
        .map(|m| {
            let st = scheme.message(m.msgid).unwrap();
            (m.seq, to_json(&decode::decode(&scheme, st, &m.data)))
        })
        .collect();
    for w in &want {
        let seq = w["seq"].as_i64().unwrap();
        assert_eq!(got.get(&seq), Some(&w["data"]), "seq {seq}");
    }
}

/// Живой файл со стенда — по переменной окружения.
#[test]
fn decode_matches_python_tll_on_live_file() {
    let Ok(spec) = std::env::var("MUTATOR_GOLDEN") else {
        return;
    };
    let (dat, json) = spec.split_once(':').expect("файл.dat:эталон.json");
    let n = check(Path::new(dat), Path::new(json));
    eprintln!("сверено {n} сообщений");
}

/// `agr:same-as-tll-read` — `--dump` печатает тот же YAML, что сам tll-read
/// (`tests/data/*.yaml` пишет `scripts/golden.py` его выводом), байт в байт.
#[test]
fn dump_prints_the_same_yaml_as_tll_read() {
    tui_patch::io::quiet();
    let dir = root().join("tests/data");
    for name in ["plain", "lz4", "sparse"] {
        let d = Dump {
            path: dir.join(format!("{name}.dat")),
            range: Range::all(),
            filter: Filter::default(),
            with_size: false,
            ..Default::default()
        };
        let mut out = Vec::new();
        dump::run(&d, &mut out).unwrap();
        let want = std::fs::read(dir.join(format!("{name}.yaml"))).unwrap();
        if out != want {
            let (a, b) = (
                String::from_utf8_lossy(&out),
                String::from_utf8_lossy(&want),
            );
            let line = a.lines().zip(b.lines()).position(|(x, y)| x != y);
            panic!("{name}: расхождение в строке {line:?}");
        }
    }
}

/// Диапазон `-s` и размер — как у tll-read: одна точка, `+N` с начала и до seq.
#[test]
fn dump_range_forms() {
    tui_patch::io::quiet();
    let path = root().join("tests/data/sparse.dat");
    let run = |r: &str, size: bool| {
        let d = Dump {
            path: path.clone(),
            range: Range::parse(r).unwrap(),
            filter: Filter::default(),
            with_size: size,
            ..Default::default()
        };
        let mut out = Vec::new();
        dump::run(&d, &mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        s.lines()
            .filter_map(|l| l.strip_prefix("- seq: ").map(str::to_string))
            .collect::<Vec<_>>()
            .join(",")
            + if s.contains("  size: ") { " +size" } else { "" }
    };
    assert_eq!(run("15", false), "15");
    assert_eq!(run("+2:+3", false), "25,35,45");
    assert_eq!(run("3990:", true), "3995 +size");
    assert_eq!(run(":45", false), "5,15,25,35,45");
    assert_eq!(run("-10", false), "3985");
}

fn dump_out(range: &str, f: impl FnOnce(Dump) -> Dump) -> String {
    tui_patch::io::quiet();
    let d = f(Dump {
        path: root().join("tests/data/sparse.dat"),
        range: Range::parse(range).unwrap(),
        ..Default::default()
    });
    let mut out = Vec::new();
    dump::run(&d, &mut out).unwrap();
    String::from_utf8(out).unwrap()
}

/// `agr:dump-first` — `--once` печатает ровно первое сообщение выборки,
/// включая выборку с пропуском (`+2:` — третье сообщение).
#[test]
fn dump_once_prints_only_first_message() {
    let s = dump_out("+2:", |d| Dump { once: true, ..d });
    assert!(s.starts_with("- seq: 25\n"), "{s}");
    assert_eq!(s.lines().filter(|l| l.starts_with("- seq: ")).count(), 1);
    // Без once — вся выборка.
    let s = dump_out("+2:", |d| d);
    assert!(s.lines().filter(|l| l.starts_with("- seq: ")).count() > 1);
    // Одна точка и так одно сообщение — once ничего не меняет.
    let s = dump_out("15", |d| Dump { once: true, ..d });
    assert!(s.starts_with("- seq: 15\n  name: Order"), "{s}");
}

/// `agr:dump-color` — краска только через Painter, обычный вывод без ANSI.
#[test]
fn dump_color_only_through_painter() {
    let path = root().join("tests/data/sparse.dat");
    let d = Dump {
        path,
        range: Range::parse("15").unwrap(),
        ..Default::default()
    };
    let mut out = Vec::new();
    dump::run(&d, &mut out).unwrap();
    assert!(!String::from_utf8(out).unwrap().contains('\x1b'));
    let mut out = Vec::new();
    dump::run(&d, &mut dump::Painter::new(&mut out)).unwrap();
    let s = String::from_utf8(out).unwrap();
    assert!(s.contains("\x1b[94mseq\x1b[39;49;00m"), "{s:?}");
    assert!(s.contains("\x1b[1;36mOrder\x1b[39;49;00m"), "{s:?}");
}

/// `agr:dump-stat` — границы seq, всего сообщений и счётчики типов по убыванию.
#[test]
fn stat_counts_messages_by_type() {
    tui_patch::io::quiet();
    let path = root().join("tests/data/sparse.dat");
    let mut out = Vec::new();
    dump::stat(&path, &mut out).unwrap();
    let s = String::from_utf8(out).unwrap();
    assert!(s.starts_with("file: "), "{s}");
    assert!(s.contains("First seq: 5\n"), "{s}");
    assert!(s.contains("Last seq: 3995\n"), "{s}");
    let total: usize = s.lines().find(|l| l.starts_with("messages: ")).unwrap()[10..]
        .parse()
        .unwrap();
    assert!(total > 0);
    // Сумма счётчиков типов — общее число сообщений.
    let types: usize = s
        .lines()
        .skip_while(|l| !l.starts_with("types:"))
        .skip(1)
        .map(|l| {
            l.trim()
                .split(' ')
                .next()
                .unwrap()
                .parse::<usize>()
                .unwrap()
        })
        .sum();
    assert_eq!(types, total);
    // Сортировка по убыванию количества.
    let counts: Vec<usize> = s
        .lines()
        .skip_while(|l| !l.starts_with("types:"))
        .skip(1)
        .map(|l| l.trim().split(' ').next().unwrap().parse().unwrap())
        .collect();
    let mut sorted = counts.clone();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    assert_eq!(counts, sorted);
}

/// `agr:dump-mutations` — правки сессии применяются, изменённые поля помечены
/// комментарием со значением из файла.
#[test]
fn dump_applies_edits_and_marks_changed_fields() {
    let mut e = tui_patch::edit::Edits::default();
    // seq 5, Order: в файле price = 100.00000000.
    e.set(5, "Order", "price", "100.00000000", "777");
    let s = dump_out("5", |d| Dump {
        edits: e.clone(),
        ..d
    });
    // decimal печатается с масштабом, пометка — значение из файла.
    assert!(
        s.contains("price: '777.00000000'  # в файле: 100.00000000"),
        "{s}"
    );
    // Чужой seq не тронут.
    let s = dump_out("15", |d| Dump { edits: e, ..d });
    assert!(!s.contains("# в файле"), "{s}");
}

fn lua_ready() -> bool {
    let ok = tui_patch::io::Context::new().ensure_lua(tui_patch::io::Backend::TLL);
    if !ok {
        eprintln!("нет tll-lua (MUTATOR_LUA) — проверки transform пропущены");
    }
    ok
}

/// `agr:dump-mutations` — transform сессии: тела прогоные, изменённые поля
/// помечены значением из файла; фильтр при transform едет пост-эвалом
/// (`agr:filter-post-eval`) — типы и выражение работают поверх прогоных тел.
#[test]
fn dump_applies_transform_and_marks_changed_fields() {
    if !lua_ready() {
        return;
    }
    let code = "\
function tll_on_open(cfg) end
function tll_on_close() end
function tll_on_data(seq, name, data)
    -- reflection read-only: поле меняется в копии; у Order есть union и
    -- переполняющий u64 — обратная сборка таблицы для него не проходит,
    -- меняем только целочисленное поле Ping
    if name ~= 'Ping' then
        tll_callback(seq, name, data)
        return
    end
    copy = tll_msg_copy(data)
    copy.n = 99999
    tll_callback(seq, name, copy)
end
";
    let s = dump_out("45", |d| Dump {
        transform: Some(code.to_string()),
        ..d
    });
    assert!(s.contains("n: 99999  # в файле: 4"), "{s}");

    // Отбор типов — в Rust: только Ping, и тот прогоный.
    let s = dump_out(":", |d| Dump {
        filter: Filter {
            messages: vec!["Ping".into()],
            expr: String::new(),
        },
        transform: Some(code.to_string()),
        ..d
    });
    assert_eq!(s.lines().filter(|l| l.starts_with("- seq: ")).count(), 80);
    assert!(s.contains("n: 99999"), "{s}");

    // Выражение — пост-эвалом по прогоным телам: в файле n = 4, после мутатора 99.
    let s = dump_out(":", |d| Dump {
        filter: Filter::expr("data.n == 99999"),
        transform: Some(code.to_string()),
        ..d
    });
    assert_eq!(s.lines().filter(|l| l.starts_with("- seq: ")).count(), 80);
    assert!(s.contains("n: 99999"), "{s}");
    // Без transform то же выражение пусто.
    let s = dump_out(":", |d| Dump {
        filter: Filter::expr("data.n == 99999"),
        ..d
    });
    assert!(!s.contains("- seq: "), "{s}");
}
