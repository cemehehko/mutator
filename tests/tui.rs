//! Кадр TUI: что видно на экране и куда ведут щелчки — на эталонном файле.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tui_patch::data::File;
use tui_patch::desk::{Desk, Key};
use tui_patch::io;
use tui_patch::ui::tui::{apply, frame, grid::Grid, Act};

/// Корень крейта во время запуска: сборка могла идти в другом каталоге.
fn root() -> std::path::PathBuf {
    std::env::var_os("CARGO_MANIFEST_DIR")
        .map(Into::into)
        .unwrap_or_else(|| env!("CARGO_MANIFEST_DIR").into())
}

fn open() -> Desk {
    io::quiet();
    let path = root().join("tests/data/lz4.dat");
    let mut d = Desk::open(File::open(&path).unwrap(), Arc::new(|| {}));
    settle(&mut d);
    d
}

fn settle(d: &mut Desk) {
    let t = Instant::now();
    while d.index.is_none() || !d.idle() {
        d.wait(Duration::from_millis(50));
        assert!(t.elapsed() < Duration::from_secs(20));
    }
}

fn text(g: &Grid<Act>) -> Vec<String> {
    (0..g.rows()).map(|r| g.text(r)).collect()
}

/// Экран: заголовок с файлом и числом сообщений, шапка колонок, строки с
/// seq и именем; таблица занимает всё, что осталось.
#[test]
fn screen_shows_title_header_and_rows() {
    let mut d = open();
    frame(&mut d, 120, 30);
    settle(&mut d);
    let g = frame(&mut d, 120, 30);
    let t = text(&g);
    assert_eq!(g.rows(), 30);
    assert!(
        t[0].contains("lz4.dat") && t[0].contains("1 500 сообщений"),
        "{}",
        t[0]
    );
    assert!(
        t[1].contains("seq") && t[1].contains("name") && t[1].contains("status"),
        "{}",
        t[1]
    );
    assert!(t[2].contains("100") && t[2].contains("Order"), "{}", t[2]);
    assert_eq!(d.height, 27);
    assert!(t[29].contains("seq 100 · строка 1 из 1 500"), "{}", t[29]);
}

/// `agr:tui-same-data` — щелчок в сетке меняет тот же [`Desk`], что и окно:
/// щелчок по строке выбирает её, повторный — открывает деталь; в детали —
/// поля, hex и типы — по `x` и `i`, щелчок по полю ставит на него курсор.
#[test]
fn clicks_select_and_open_detail() {
    let mut d = open();
    let g = frame(&mut d, 160, 40);
    let a = g.act_at(5, 10).expect("строка — ссылка");
    assert_eq!(a, Act::Row(3));
    apply(&mut d, a);
    assert_eq!(d.sel, 3);
    apply(&mut d, a);
    assert!(d.detail_open);
    let g = frame(&mut d, 160, 40);
    let t = text(&g);
    let head = t
        .iter()
        .position(|l| l.starts_with("─ Cancel  seq 103"))
        .expect("шапка детали");
    assert!(t[head + 1].contains("header"));
    assert!(
        t[head + 2].contains("ts: 2026-09-22T13:41:27.003002Z"),
        "{}",
        t[head + 2]
    );
    // По умолчанию ни hex, ни типов полей: только имена и значения.
    assert!(
        !t[head + 1].contains("0000"),
        "hex выключен: {}",
        t[head + 1]
    );
    assert!(
        !t[head + 2].contains("time_point"),
        "типы выключены: {}",
        t[head + 2]
    );
    d.key(tui_patch::desk::Key::Char('x'));
    d.key(tui_patch::desk::Key::Char('i'));
    let t = text(&frame(&mut d, 160, 40));
    assert!(
        t[head + 1].contains("0000"),
        "x — hex рядом: {}",
        t[head + 1]
    );
    assert!(
        t[head + 2].contains("int64 time_point us @0+8"),
        "i — типы: {}",
        t[head + 2]
    );
    assert!(d.session().show_hex && d.session().show_types, "в сессию");
    let f = g.act_at(head + 3, 2).expect("поле — ссылка");
    apply(&mut d, f);
    assert_eq!(
        d.detail.as_ref().unwrap().current().unwrap().path,
        "header.status"
    );
}

/// Список типов рисуется рамкой поверх таблицы, набор текста его сужает.
#[test]
fn picker_overlays_table() {
    let mut d = open();
    d.key(Key::Char('t'));
    for c in "anc".chars() {
        d.key(Key::Char(c));
    }
    let g = frame(&mut d, 100, 30);
    let t = text(&g).join("\n");
    assert!(t.contains("типы сообщений"));
    assert!(t.contains("[ ] Cancel"), "{t}");
    assert!(!t.contains("[ ] Order"));
    d.key(Key::Char(' '));
    assert!(d
        .prompt
        .as_ref()
        .unwrap()
        .items
        .iter()
        .any(|i| i.label == "Cancel" && i.on));
}

/// Длинное значение поля в детали: у невыбранного — обрезано с «…», у
/// выбранного — перенесено на следующие строки, соседние поля сдвинуты.
#[test]
fn long_values_wrap_on_selected_field() {
    let mut d = open();
    d.key(tui_patch::desk::Key::Enter);
    let t = text(&frame(&mut d, 24, 40));
    let ts = t
        .iter()
        .position(|l| l.trim_start().starts_with("ts:"))
        .expect("ts");
    assert!(t[ts].ends_with('…'), "обрезано: {}", t[ts]);
    assert!(t[ts + 1].trim_start().starts_with("status:"));
    d.key(tui_patch::desk::Key::Tab);
    d.key(tui_patch::desk::Key::Down);
    let t = text(&frame(&mut d, 24, 40));
    let ts = t
        .iter()
        .position(|l| l.trim_start().starts_with("ts:"))
        .expect("ts");
    let joined = format!("{}{}", t[ts].split(": ").nth(1).unwrap(), t[ts + 1].trim());
    assert_eq!(joined, "2026-09-22T13:41:27Z", "перенесено целиком");
    assert!(
        t[ts + 2].trim_start().starts_with("status:"),
        "дальше сдвинуто: {}",
        t[ts + 2]
    );
}

/// `agr:transform-shadow` — мутатор поменял seq: он красный в таблице, а в шапке
/// детали рядом показан исходный seq файла.
#[test]
fn changed_seq_is_red_and_detail_shows_original() {
    if !io::Context::new().ensure_lua(tui_patch::io::Backend::TLL) {
        eprintln!("нет tll-lua (TUI_PATCH_LUA) — проверка пропущена");
        return;
    }
    let mut d = open();
    d.set_transform(
        "function tll_on_data(seq, name, data)\n    tll_callback(seq + 1000, name, data)\nend"
            .into(),
    );
    settle(&mut d);
    d.key(Key::Enter);
    settle(&mut d);
    let g = frame(&mut d, 120, 30);
    let t = text(&g);
    let row = d.rows.first().expect("строка").seq;
    let orig = d.seq_in_file(row).expect("исходный seq известен");
    // Первая строка таблицы: её seq — красным (колонка seq начинается с 4-й клетки).
    let line = t
        .iter()
        .position(|l| l.contains(&row.to_string()))
        .expect("строка с новым seq");
    let col = t[line].find(&row.to_string()).unwrap();
    let col = t[line][..col].chars().count();
    assert_eq!(g.fg(line, col), Some(tui_patch::ui::theme::EDITED));
    assert!(
        t.iter().any(|l| l.contains(&format!("(в файле {orig})"))),
        "в шапке детали — исходный seq: {t:#?}"
    );
}
