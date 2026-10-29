//! Поведение [`Desk`] на эталонных файлах — без вида, только клавиши и события.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tui_patch::data::File;
use tui_patch::desk::{Desk, Focus, Key, PromptKind};
use tui_patch::io::{self, Context, LuaMode, Open, Reader};

/// Корень крейта во время запуска: сборка могла идти в другом каталоге.
fn root() -> std::path::PathBuf {
    std::env::var_os("CARGO_MANIFEST_DIR")
        .map(Into::into)
        .unwrap_or_else(|| env!("CARGO_MANIFEST_DIR").into())
}

fn fixture(name: &str) -> PathBuf {
    root().join("tests/data").join(format!("{name}.dat"))
}

fn all_seqs(path: &Path) -> Vec<i64> {
    let ctx = Context::new();
    let mut r = Reader::open(&ctx, &Open::file(path)).unwrap();
    std::iter::from_fn(|| r.next()).map(|m| m.seq).collect()
}

fn open(name: &str, height: usize) -> Desk {
    io::quiet();
    let mut d = Desk::open(File::open(&fixture(name)).unwrap(), Arc::new(|| {}));
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

fn typed(d: &mut Desk, s: &str) {
    for c in s.chars() {
        d.key(Key::Char(c));
    }
}

fn sel(d: &Desk) -> i64 {
    d.selected().expect("выбрана строка").seq
}

fn lua() -> bool {
    io::quiet();
    let ok = Context::new().ensure_lua(tui_patch::io::Backend::TLL);
    if !ok {
        eprintln!("нет tll-lua (TUI_PATCH_LUA) — проверки фильтра пропущены");
    }
    ok
}

/// `agr:anchor` — стрелками вниз проходится весь файл ровно по одному разу,
/// окно дочитывается и обрезается, номера строк совпадают с позицией; вверх —
/// обратно до начала.
#[test]
fn arrows_walk_every_message_once() {
    for name in ["lz4", "sparse"] {
        let want = all_seqs(&fixture(name));
        let mut d = open(name, 7);
        assert!(d.index.is_some(), "индекс готов");
        let mut got = vec![sel(&d)];
        for i in 1..want.len() {
            key(&mut d, Key::Down);
            got.push(sel(&d));
            assert_eq!(d.sel_row(), Some(i), "{name}: номер строки");
            assert!(
                d.sel >= d.top && d.sel < d.top + d.height,
                "выбор на экране"
            );
            assert!(
                d.rows.len() <= 7 * 8 + 64 * 4 + 200,
                "окно не растёт без меры"
            );
        }
        assert_eq!(got, want, "{name}");
        key(&mut d, Key::Down);
        assert_eq!(
            sel(&d),
            *want.last().unwrap(),
            "дальше последнего не уходит"
        );
        for _ in 0..want.len() {
            d.key(Key::Up);
        }
        settle(&mut d);
        for _ in 0..want.len() {
            key(&mut d, Key::Up);
        }
        assert_eq!(sel(&d), want[0]);
    }
}

/// Home/End, страницы и переход по `g`: seq, `+N` и `-N`.
#[test]
fn jumps_land_where_asked() {
    let want = all_seqs(&fixture("sparse"));
    let mut d = open("sparse", 10);
    key(&mut d, Key::End);
    assert_eq!(sel(&d), *want.last().unwrap());
    key(&mut d, Key::Home);
    assert_eq!(sel(&d), want[0]);
    key(&mut d, Key::PageDown);
    assert_eq!(sel(&d), want[10]);

    key(&mut d, Key::Char('g'));
    typed(&mut d, "2005");
    key(&mut d, Key::Enter);
    assert_eq!(sel(&d), 2005);
    key(&mut d, Key::Char('g'));
    typed(&mut d, "2007");
    key(&mut d, Key::Enter);
    assert_eq!(sel(&d), 2015, "нет такого seq — следующий");
    key(&mut d, Key::Char('g'));
    typed(&mut d, "+123");
    key(&mut d, Key::Enter);
    assert_eq!((sel(&d), d.sel_row()), (want[123], Some(123)));
    key(&mut d, Key::Char('g'));
    typed(&mut d, "-30");
    key(&mut d, Key::Enter);
    assert_eq!(sel(&d), want.last().unwrap() - 30);
    assert!(d.prompt.is_none());
}

/// `agr:patch-path` — деталь разбирает выбранное, курсор держит путь поля
/// между сообщениями одного типа, `a` добавляет поле по пути колонкой.
#[test]
fn detail_follows_selection() {
    let mut d = open("lz4", 10);
    key(&mut d, Key::Enter);
    assert!(d.detail_open);
    assert_eq!(d.detail.as_ref().unwrap().seq, sel(&d));
    key(&mut d, Key::Tab);
    assert_eq!(d.focus, Focus::Detail);
    while d.detail.as_ref().unwrap().current().unwrap().path != "ratio" {
        key(&mut d, Key::Down);
    }
    let path = "ratio".to_string();
    assert!(!d.columns.contains(&path));
    key(&mut d, Key::Char('a'));
    assert!(d.columns.contains(&path));
    let col = d.columns.iter().position(|c| *c == path).unwrap();
    assert_eq!(d.selected().unwrap().cells[col].as_deref(), Some("0.5"));

    key(&mut d, Key::Tab);
    key(&mut d, Key::Down);
    let det = d.detail.as_ref().unwrap();
    assert_eq!(det.seq, sel(&d));
    assert_eq!(det.current().unwrap().path, path, "путь тот же");
    key(&mut d, Key::Esc);
    assert!(!d.detail_open);
}

/// Колонки: у типа без поля — пусто (`agr:missing-field`), пресеты по `p`.
#[test]
fn columns_show_fields_or_nothing() {
    let mut d = open("lz4", 10);
    assert_eq!(
        d.columns[..3],
        ["header.ts", "header.status", "header.order_id"]
    );
    let sym = d
        .columns
        .iter()
        .position(|c| c == "symbol")
        .expect("symbol в пресете");
    let ping = d.rows.iter().find(|r| r.name == "Ping").unwrap();
    assert_eq!(ping.cells[sym], None);
    let order = d.rows.iter().find(|r| r.name == "Order").unwrap();
    assert_eq!(order.cells[sym].as_deref(), Some("SBER"));
    key(&mut d, Key::Char('p'));
    assert!(d.columns.is_empty(), "пресет «—»");
    assert!(d.rows.iter().all(|r| r.cells.is_empty()));
    key(&mut d, Key::Char('c'));
    let p = d.prompt.as_ref().unwrap();
    assert_eq!(p.kind, PromptKind::Columns);
    typed(&mut d, "u64");
    key(&mut d, Key::Char(' '));
    key(&mut d, Key::Enter);
    assert_eq!(d.columns, ["u64"]);
}

/// `agr:size-column` — колонка `#data` показывает размер тела в байтах:
/// включается пикером `c` как обычная колонка, в пресет по умолчанию не входит,
/// значение — длина тела, прижато вправо.
#[test]
fn data_size_column_shows_message_size() {
    use tui_patch::ui::cols::{self, ColKind};
    let mut d = open("lz4", 10);
    assert!(
        !d.columns.iter().any(|c| c == tui_patch::columns::DATA_LEN),
        "в пресет по умолчанию не входит"
    );
    key(&mut d, Key::Char('c'));
    typed(&mut d, tui_patch::columns::DATA_LEN);
    key(&mut d, Key::Char(' '));
    key(&mut d, Key::Enter);
    assert!(d.columns.iter().any(|c| c == tui_patch::columns::DATA_LEN));

    let row = d.selected().unwrap();
    assert_eq!(
        cols::cell(row, ColKind::DataLen),
        row.data.len().to_string(),
        "значение — длина тела"
    );
    let (layout, _) = cols::layout(&d, 500);
    let col = layout
        .iter()
        .find(|c| c.kind == ColKind::DataLen)
        .expect("колонка #data в раскладке");
    assert_eq!(col.title, "#data");
    assert!(col.right, "размер прижат вправо");
}

/// `agr:size-column`, `agr:picker-keys` — клавиши пикера колонок: End — конец
/// списка полей, Home — начало, Enter только закрывает окно (видимость
/// переключает пробел).
#[test]
fn columns_picker_end_home_and_enter_close() {
    let mut d = open("lz4", 10);
    key(&mut d, Key::Char('c'));
    assert_eq!(d.prompt.as_ref().unwrap().kind, PromptKind::Columns);

    let n = d.prompt.as_ref().unwrap().visible().len();
    assert!(n > 1, "полей в пикере больше одного");
    key(&mut d, Key::End);
    assert_eq!(d.prompt.as_ref().unwrap().sel, n - 1, "End — конец списка");
    key(&mut d, Key::Home);
    assert_eq!(d.prompt.as_ref().unwrap().sel, 0, "Home — начало списка");

    let before = d.columns.clone();
    key(&mut d, Key::Enter);
    assert!(d.prompt.is_none(), "Enter закрывает пикер");
    assert_eq!(d.columns, before, "Enter ничего не переключает");

    key(&mut d, Key::Char('c'));
    key(&mut d, Key::End);
    key(&mut d, Key::Char(' '));
    key(&mut d, Key::Enter);
    assert_eq!(
        d.columns.last().map(String::as_str),
        Some(tui_patch::columns::DATA_LEN),
        "пробел включил #data у конца списка"
    );
}

/// `agr:picker-keys` — окно типов: пробел переключает отметку и не закрывает
/// окно, Enter закрывает и применяет отмеченное — как Esc; Home/End — края.
#[test]
fn types_picker_enter_closes_and_applies() {
    let mut d = open("lz4", 10);
    key(&mut d, Key::Char('t'));
    assert_eq!(d.prompt.as_ref().unwrap().kind, PromptKind::Types);

    let n = d.prompt.as_ref().unwrap().visible().len();
    assert!(n > 1, "типов в списке больше одного");
    key(&mut d, Key::End);
    assert_eq!(d.prompt.as_ref().unwrap().sel, n - 1, "End — конец списка");
    key(&mut d, Key::Home);
    assert_eq!(d.prompt.as_ref().unwrap().sel, 0, "Home — начало списка");

    let first = d.prompt.as_ref().unwrap().items[0].label.clone();
    key(&mut d, Key::Char(' '));
    assert!(d.prompt.is_some(), "пробел не закрывает окно");
    assert!(
        d.prompt.as_ref().unwrap().items[0].on,
        "пробел переключил отметку"
    );
    key(&mut d, Key::Enter);
    assert!(d.prompt.is_none(), "Enter закрывает окно типов");
    assert_eq!(
        d.filter.messages,
        vec![first],
        "Enter применил отмеченный тип"
    );
}

/// Фильтр и поиск: через строку ввода, внутри фильтра, в обе стороны.
#[test]
fn filter_and_search_from_keys() {
    if !lua() {
        return;
    }
    let mut d = open("lz4", 8);
    key(&mut d, Key::Char('f'));
    typed(
        &mut d,
        "data.header ~= nil and data.header.status == 'Wait'",
    );
    key(&mut d, Key::Enter);
    let m = d.matches.clone().expect("фильтр");
    while !m.is_done() {
        settle(&mut d);
    }
    settle(&mut d);
    let want = m.seqs.read().unwrap().clone();
    let file = File::open(&fixture("lz4")).unwrap();
    let ctx = Context::new();
    let mut r = Reader::open(&ctx, &Open::file(fixture("lz4"))).unwrap();
    let direct: Vec<i64> = std::iter::from_fn(|| r.next())
        .filter(|m| {
            let st = file.scheme.message(m.msgid).unwrap();
            tui_patch::decode::field(&file.scheme, st, &m.data, "header.status").as_deref()
                == Some("Wait")
        })
        .map(|m| m.seq)
        .collect();
    assert_eq!(want, direct);
    assert_eq!(d.total(), Some((want.len(), false)));
    let mut got = vec![sel(&d)];
    for _ in 1..want.len() {
        key(&mut d, Key::Down);
        got.push(sel(&d));
    }
    assert_eq!(got, want);

    key(&mut d, Key::Home);
    key(&mut d, Key::Char('/'));
    typed(&mut d, "name == 'Cancel'");
    key(&mut d, Key::Enter);
    let first_cancel = sel(&d);
    assert_eq!(d.selected().unwrap().name, "Cancel");
    key(&mut d, Key::Char('n'));
    assert!(sel(&d) > first_cancel);
    assert_eq!(d.selected().unwrap().name, "Cancel");
    key(&mut d, Key::Char('N'));
    assert_eq!(sel(&d), first_cancel);
    key(&mut d, Key::Char('?'));
    typed(&mut d, "name == 'Nope'");
    key(&mut d, Key::Enter);
    assert_eq!((d.status.as_str(), sel(&d)), ("не найдено", first_cancel));

    // Снять фильтр — снова все строки, выбор на месте.
    key(&mut d, Key::Char('F'));
    assert!(d.matches.is_none());
    assert_eq!(sel(&d), first_cancel);
}

/// Ошибка в выражении фильтра — текстом в строке состояния и пустой таблицей,
/// а не молча неполным результатом: `AND` вместо `and`, сравнение с `nil`.
#[test]
fn filter_errors_are_shown() {
    if !lua() {
        return;
    }
    let mut d = open("lz4", 8);
    for (expr, want) in [
        ("data.n AND true", "синтаксис Lua"),
        ("data.n > 5", "поля нет"),
    ] {
        key(&mut d, Key::Char('f'));
        typed(&mut d, expr);
        key(&mut d, Key::Enter);
        let m = d.matches.clone().unwrap();
        while !m.is_done() {
            settle(&mut d);
        }
        settle(&mut d);
        assert!(d.status.contains(want), "{expr}: {}", d.status);
        assert_eq!(m.len(), 0, "{expr}: неполный результат не показан");
        key(&mut d, Key::Char('F'));
    }
}

/// `agr:session-by-name` — сессия возвращает фильтр, поиск, колонки, выбор и
/// деталь; через TOML без потерь; ключ — имя файла без каталога.
#[test]
fn session_restores_view() {
    if !lua() {
        return;
    }
    let mut d = open("lz4", 8);
    key(&mut d, Key::Char('p'));
    d.toggle_column("u64");
    key(&mut d, Key::Char('/'));
    typed(&mut d, "name == 'Ping'");
    key(&mut d, Key::Enter);
    key(&mut d, Key::Char('g'));
    typed(&mut d, "900");
    key(&mut d, Key::Enter);
    key(&mut d, Key::Enter);
    key(&mut d, Key::Tab);
    key(&mut d, Key::Down);
    key(&mut d, Key::Char('f'));
    typed(&mut d, "name == 'Order' or name == 'Cancel'");
    key(&mut d, Key::Enter);
    let m = d.matches.clone().unwrap();
    while !m.is_done() {
        settle(&mut d);
    }
    settle(&mut d);
    assert_eq!(sel(&d), 900, "выбор пережил фильтр");
    let s = d.session();
    let text = toml::to_string(&s).unwrap();
    let back: tui_patch::session::Session = toml::from_str(&text).unwrap();
    assert_eq!(back, s);
    assert_eq!(s.seq, Some(900));
    assert_eq!(s.columns.as_deref(), Some(&["u64".to_string()][..]));

    let mut e = open("lz4", 8);
    e.restore(&back);
    settle(&mut e);
    let m = e.matches.clone().expect("фильтр вернулся");
    while !m.is_done() {
        settle(&mut e);
    }
    settle(&mut e);
    assert_eq!(e.filter.expr, "name == 'Order' or name == 'Cancel'");
    assert_eq!(sel(&e), 900, "выбор вернулся");
    assert_eq!(e.columns, ["u64"]);
    assert!(e.detail_open);
    assert_eq!(
        e.detail.as_ref().unwrap().current().unwrap().path,
        s.detail_path.clone().unwrap()
    );
    key(&mut e, Key::Char('n'));
    // Поиск вернулся: `n` ищет Ping внутри фильтра Order/Cancel — и не находит,
    // а без сессии было бы «поиска ещё не было».
    assert_eq!(e.status, "не найдено");
}

/// Одно имя файла в разных каталогах — одна сессия; лишние знаки имени — `_`.
#[test]
fn session_file_is_named_by_data_file() {
    let a = tui_patch::session::path_for(Path::new("/a/b/out-mu0.0.dat")).unwrap();
    let b = tui_patch::session::path_for(Path::new("/other/out-mu0.0.dat")).unwrap();
    assert_eq!(a, b);
    assert!(a.ends_with("sessions/out-mu0.0.dat.toml"));
    let c = tui_patch::session::path_for(Path::new("x/a b:c.dat")).unwrap();
    assert!(c.ends_with("sessions/a_b_c.dat.toml"));
}

/// Колесо прокрутки уводит экран от выбора: обрезка лишнего у верхнего края
/// не уходит в минус по выбору, а выбранный остаётся настоящей строкой окна.
#[test]
fn trim_keeps_selection_when_view_scrolled_away() {
    let mut d = open("lz4", 30);
    d.sel = 0;
    d.top = 0;
    for _ in 0..30 {
        d.scroll(2000);
        settle(&mut d);
        assert!(d.sel < d.rows.len(), "выбор в окне");
        if d.at_end {
            break;
        }
    }
    assert!(d.at_end, "доскроллили до конца файла");
    assert!(d.selected().is_some(), "выбор — настоящая строка окна");
}

/// `set_transform` ставит режим Transform и сохраняет код в сессию;
/// `toggle_transform` выключает/включает код, не стирая его;
/// `clear_transform` возвращает обычный режим и стирает код.
#[test]
fn transform_is_set_toggled_and_cleared() {
    if !lua() {
        return;
    }
    let mut d = open("lz4", 8);
    assert_eq!(d.lua_mode, LuaMode::Filter, "по умолчанию — filter");
    assert!(d.lua_code.is_empty());

    d.set_transform(
        "function tll_on_data(seq, name, data)\n  tll_callback(seq, name, data)\nend".into(),
    );
    settle(&mut d);
    assert_eq!(
        d.lua_mode,
        LuaMode::Transform,
        "режим сменился на Transform"
    );
    assert!(d.lua_code.contains("tll_on_data"), "код запомнился");
    assert!(d.lua_enabled, "transform включён после set_transform");

    // Код переживает снимок сессии.
    let s = d.session();
    assert_eq!(
        s.lua_transform.as_deref(),
        Some("function tll_on_data(seq, name, data)\n  tll_callback(seq, name, data)\nend")
    );

    // Тумблер выключает код, но не стирает его.
    d.toggle_transform();
    settle(&mut d);
    assert!(!d.lua_enabled, "transform выключен");
    assert_eq!(d.lua_mode, LuaMode::Filter, "режим вернулся к filter");
    assert!(d.lua_code.contains("tll_on_data"), "код не стёрт тумблером");
    assert_eq!(d.session().lua_transform_enabled, Some(false));

    // Тумблер включает обратно.
    d.toggle_transform();
    settle(&mut d);
    assert!(d.lua_enabled, "transform включён обратно");
    assert_eq!(d.lua_mode, LuaMode::Transform);

    d.clear_transform();
    settle(&mut d);
    assert_eq!(d.lua_mode, LuaMode::Filter, "вернулись к filter");
    assert!(d.lua_code.is_empty(), "код стёрт");
    assert_eq!(d.session().lua_transform, None);
}

/// `agr:filter-post-eval` — фильтр и поиск видят transform-изменённые значения:
/// совпадение есть только по прогоным телам, без мутатора то же выражение пусто.
#[test]
fn filter_and_search_see_transformed_values() {
    if !lua() {
        return;
    }
    let mut d = open("sparse", 8);
    d.set_transform(
        "\
function tll_on_data(seq, name, data)
    if name ~= 'Ping' then tll_callback(seq, name, data) return end
    copy = tll_msg_copy(data)
    copy.n = 99999
    tll_callback(seq, name, copy)
end"
        .into(),
    );
    settle(&mut d);

    // Фильтр по прогоным телам: в файле n у Ping не больше 399.
    key(&mut d, Key::Char('f'));
    typed(&mut d, "data.n == 99999");
    key(&mut d, Key::Enter);
    let m = d.matches.clone().expect("фильтр");
    while !m.is_done() {
        settle(&mut d);
    }
    let seqs = m.seqs.read().unwrap().clone();
    assert_eq!(seqs.len(), 80, "все Ping прогоны до 99999");
    assert_eq!(seqs[0], 45, "первый Ping");

    // Снять фильтр, поиском по тому же выражению — первое совпадение, Ping.
    key(&mut d, Key::Char('F'));
    key(&mut d, Key::Home);
    key(&mut d, Key::Char('/'));
    typed(&mut d, "data.n == 99999");
    key(&mut d, Key::Enter);
    assert_eq!(sel(&d), 45, "поиск встал на первый прогоный Ping");
    assert_eq!(d.selected().unwrap().name, "Ping");
}

/// На старте transform из сессии применяется до первого фильтра: иначе первый
/// скан гоняет по сырому файлу и по прогоным значениям находит пусто.
#[test]
fn session_startup_applies_transform_before_filter() {
    if !lua() {
        return;
    }
    let mut d = open("sparse", 8);
    d.set_transform(
        "\
function tll_on_data(seq, name, data)
    if name ~= 'Ping' then tll_callback(seq, name, data) return end
    copy = tll_msg_copy(data)
    copy.n = 99999
    tll_callback(seq, name, copy)
end"
        .into(),
    );
    settle(&mut d);
    key(&mut d, Key::Char('f'));
    typed(&mut d, "data.n == 99999");
    key(&mut d, Key::Enter);
    let m = d.matches.clone().expect("фильтр");
    while !m.is_done() {
        settle(&mut d);
    }
    assert_eq!(
        m.seqs.read().unwrap().len(),
        80,
        "до переоткрытия — совпадения"
    );

    let s = d.session();
    let mut e = open("sparse", 8);
    e.restore(&s);
    let t = Instant::now();
    while e.matches.is_none() {
        e.wait(Duration::from_millis(100));
        assert!(t.elapsed() < Duration::from_secs(30), "фильтр не пришёл");
    }
    let m = e.matches.clone().expect("фильтр из сессии запустился");
    while !m.is_done() {
        settle(&mut e);
    }
    settle(&mut e);
    assert_eq!(
        m.seqs.read().unwrap().len(),
        80,
        "первый скан видел transform"
    );
    assert!(
        !e.rows.is_empty(),
        "строки видны сразу, без Enter на фильтре"
    );
}

/// Клавиши TUI-редактора Lua правят по курсору: навигация, вставка и удаление
/// знака у курсора, а не только в конец текста.
#[test]
fn lua_editor_tui_cursor_edits_in_place() {
    let mut d = open("lz4", 8);
    d.lua_code = "ac".into();
    d.lua_cursor = 1; // между 'a' и 'c'

    // Вставить 'b' на позицию курсора.
    assert!(tui_patch::ui::lua_editor::tui_key(&mut d, Key::Char('b')));
    assert_eq!(d.lua_code, "abc");
    assert_eq!(d.lua_cursor, 2, "курсор переехал за вставленный знак");

    // Назад на один знак.
    assert!(tui_patch::ui::lua_editor::tui_key(&mut d, Key::Left));
    assert_eq!(d.lua_cursor, 1);

    // Backspace у курсора стирает знак перед ним ('a' → курсор стал 1).
    assert!(tui_patch::ui::lua_editor::tui_key(&mut d, Key::Backspace));
    assert_eq!(d.lua_code, "bc");
    assert_eq!(d.lua_cursor, 0);

    // End — в конец строки, Home — в начало.
    assert!(tui_patch::ui::lua_editor::tui_key(&mut d, Key::End));
    assert_eq!(d.lua_cursor, 2);
    assert!(tui_patch::ui::lua_editor::tui_key(&mut d, Key::Home));
    assert_eq!(d.lua_cursor, 0);

    // Down на новой строке держит курсор в конце кода.
    d.lua_code = "abc\nde".into();
    d.lua_cursor = 0;
    assert!(tui_patch::ui::lua_editor::tui_key(&mut d, Key::Down));
    assert_eq!(d.lua_cursor, 4, "встал на начало второй строки");
    assert!(tui_patch::ui::lua_editor::tui_key(&mut d, Key::Right));
    assert_eq!(d.lua_cursor, 5);
}

/// Сброс кода через Ctrl-R ждёт подтверждения и возвращает шаблон по умолчанию.
#[test]
fn lua_editor_reset_requires_confirmation() {
    let mut d = open("lz4", 8);
    d.lua_code = "local x = 1".into();

    // Ctrl-R просит подтверждения, кода ещё не трогает.
    assert!(tui_patch::ui::lua_editor::tui_key(&mut d, Key::Ctrl('r')));
    assert!(d.lua_reset_confirm);
    assert_eq!(d.lua_code, "local x = 1");

    // Отмена оставляет код.
    assert!(tui_patch::ui::lua_editor::tui_key(&mut d, Key::Char('n')));
    assert!(!d.lua_reset_confirm);
    assert_eq!(d.lua_code, "local x = 1");

    // Подтверждение сбрасывает в шаблон по умолчанию.
    assert!(tui_patch::ui::lua_editor::tui_key(&mut d, Key::Ctrl('r')));
    assert!(tui_patch::ui::lua_editor::tui_key(&mut d, Key::Enter));
    assert!(!d.lua_reset_confirm);
    assert!(d.lua_code.contains("function tll_on_data"));
}

fn mutator() -> String {
    "function tll_on_data(seq, name, data)\n    tll_callback(seq, name, { u8 = 1 })\nend".into()
}

fn first_raw(name: &str) -> (i64, Vec<u8>) {
    let ctx = Context::new();
    let mut r = Reader::open(&ctx, &Open::file(fixture(name))).unwrap();
    let m = r.next().unwrap();
    (m.seq, m.data)
}

/// `agr:transform-shadow` — при мутаторе вид показывает прогоные тела (как на
/// фиксации), изменённые строки и поля помечены значениями из файла.
#[test]
fn transform_view_shows_mutated_bodies_marked_against_file() {
    if !lua() {
        return;
    }
    let mut d = open("lz4", 8);
    let (seq0, body0) = first_raw("lz4");
    d.set_transform(mutator());
    settle(&mut d);
    let row = d
        .rows
        .iter()
        .find(|r| r.seq == seq0)
        .expect("первая строка");
    assert_ne!(row.data, body0, "тело в виде прогоное");
    assert!(d.mutated_row(seq0), "строка помечена");
    assert_eq!(d.mutated(seq0, "u8"), Some("255"), "в карте — файл");
    // Мутатор перекодирует тело из таблицы: символ обнулился, в карте — файл.
    assert_eq!(d.mutated(seq0, "symbol"), Some("SBER"));
}

/// `agr:transform-shadow` — мутатор поменял seq: в виде новый seq, в карте под
/// путём `seq` — исходный; строка помечена, хотя тело не менялось.
#[test]
fn transform_seq_change_is_marked_with_original_seq() {
    if !lua() {
        return;
    }
    let mut d = open("lz4", 8);
    let (seq0, body0) = first_raw("lz4");
    d.set_transform(
        "function tll_on_data(seq, name, data)\n    tll_callback(seq + 1000, name, data)\nend"
            .into(),
    );
    settle(&mut d);
    let shifted = seq0 + 1000;
    let row = d
        .rows
        .iter()
        .find(|r| r.seq == shifted)
        .expect("в виде — новый seq");
    assert_eq!(row.data, body0, "тело то же");
    assert!(d.mutated_row(shifted), "строка помечена из-за одного seq");
    assert_eq!(d.seq_in_file(shifted), Some(seq0), "в файле — исходный seq");
    assert_eq!(d.orig_seq(shifted), seq0);
    // Без мутатора пометок нет.
    d.clear_transform();
    settle(&mut d);
    assert!(d.mutations.is_empty());
    assert_eq!(d.seq_in_file(shifted), None);
}

/// `agr:transform-shadow` — дочитанные строки тоже получают метки, trim их
/// не теряет для живого окна.
#[test]
fn extend_merges_mutation_marks_for_new_rows() {
    if !lua() {
        return;
    }
    let mut d = open("lz4", 5);
    d.set_transform(mutator());
    settle(&mut d);
    for _ in 0..8 {
        key(&mut d, Key::Down);
    }
    assert!(d.mutated_row(sel(&d)), "текущая строка помечена");
    assert!(
        d.rows.iter().all(|r| d.mutated_row(r.seq)),
        "всё окно, включая дочитанное, помечено"
    );
}

/// `agr:transform-shadow` — снятый transform убирает все метки.
#[test]
fn clear_transform_clears_mutation_marks() {
    if !lua() {
        return;
    }
    let mut d = open("lz4", 8);
    let (seq0, body0) = first_raw("lz4");
    d.set_transform(mutator());
    settle(&mut d);
    assert!(!d.mutations.is_empty());
    d.clear_transform();
    settle(&mut d);
    assert!(d.mutations.is_empty(), "метки сняты");
    assert!(!d.mutated_row(seq0));
    let row = d.rows.iter().find(|r| r.seq == seq0).expect("строка");
    assert_eq!(row.data, body0, "тело вернулось к файлу");
}

/// `agr:transform-shadow` — переключение мутатора перечитывает деталь: дерево
/// полей строится из нового тела, хотя seq не менялся.
#[test]
fn detail_rebuilds_when_transform_toggles() {
    if !lua() {
        return;
    }
    let mut d = open("lz4", 8);
    key(&mut d, Key::Enter); // деталь по первой строке
    let u8_text = |d: &Desk, seq: i64| {
        let det = d.detail.as_ref().expect("деталь");
        assert_eq!(det.seq, seq);
        det.lines()
            .into_iter()
            .find(|(_, n)| n.path == "u8")
            .expect("поле u8")
            .1
            .text()
    };
    let seq = sel(&d);
    assert_eq!(u8_text(&d, seq), "255", "до мутатора — тело файла");
    d.set_transform(mutator());
    settle(&mut d);
    assert_eq!(u8_text(&d, seq), "1", "после включения — прогоное тело");
    d.clear_transform();
    settle(&mut d);
    assert_eq!(u8_text(&d, seq), "255", "после снятия — тело файла");
}

fn key_raw(d: &mut Desk, k: Key) {
    d.key(k);
}

/// Справка F1 открывается и закрывается, глотает клавиши, пока открыта.
#[test]
fn help_opens_closes_and_swallows_keys() {
    let mut d = open("lz4", 8);
    let seq = sel(&d);
    key_raw(&mut d, Key::Char('\u{F1}'));
    assert!(d.help.open, "F1 открыл справку");
    key_raw(&mut d, Key::Down);
    assert_eq!(sel(&d), seq, "стрелка не двигает выбор под справкой");
    key_raw(&mut d, Key::Esc);
    assert!(!d.help.open, "Esc закрыл");
    key_raw(&mut d, Key::Down);
    assert_ne!(sel(&d), seq, "клавиши снова работают");
    key_raw(&mut d, Key::Char('\u{F1}'));
    key_raw(&mut d, Key::Char('\u{F1}'));
    assert!(!d.help.open, "повторный F1 — тумблер");
}

/// Скролл справки: рендер ограничивает снизу, Home возвращает в начало,
/// позиция переживает закрытие и переоткрытие.
#[test]
fn help_scrolls_to_bottom_and_back() {
    let mut d = open("lz4", 8);
    key_raw(&mut d, Key::Char('\u{F1}'));
    for _ in 0..3 {
        key_raw(&mut d, Key::PageDown);
    }
    assert!(d.help.scroll > 0, "PgDn листает");

    key_raw(&mut d, Key::End);
    // Кадр TUI ограничивает скролл числом строк справки.
    let _ = tui_patch::ui::tui::frame(&mut d, 120, 40);
    let at = d.help.scroll;
    assert!(at > 0 && at < usize::MAX, "End — у конца списка: {at}");

    key_raw(&mut d, Key::Home);
    assert_eq!(d.help.scroll, 0, "Home — в начало");

    key_raw(&mut d, Key::PageDown);
    let was = d.help.scroll;
    key_raw(&mut d, Key::Esc);
    key_raw(&mut d, Key::Char('\u{F1}'));
    assert!(d.help.open, "переоткрылась");
    assert_eq!(d.help.scroll, was, "позиция пережила переоткрытие");
}

/// Enter в TUI-редакторе — перенос строки, а не применение; Esc — применить
/// код и вернуться к таблице.
#[test]
fn lua_editor_enter_newline_esc_applies() {
    let mut d = open("lz4", 8);
    d.lua_code = "ac".into();
    d.lua_cursor = 1;
    assert!(tui_patch::ui::lua_editor::tui_key(&mut d, Key::Enter));
    assert_eq!(d.lua_code, "a\nc", "Enter — перенос строки");
    assert_eq!(d.lua_mode, LuaMode::Filter, "Enter код не применяет");

    assert!(!tui_patch::ui::lua_editor::tui_key(&mut d, Key::Esc));
    assert_eq!(d.lua_mode, LuaMode::Transform, "Esc применил код");
    assert!(d.lua_enabled, "transform включён");
}

/// Применение transform не сбрасывает отбор по типам сообщений.
#[test]
fn set_transform_keeps_type_filter() {
    let mut d = open("lz4", 8);
    d.filter.messages = vec!["Order".into()];
    d.set_transform(
        "function tll_on_data(seq, name, data)\n    tll_callback(seq, name, data)\nend".into(),
    );
    settle(&mut d);
    assert_eq!(
        d.filter.messages,
        vec!["Order".to_string()],
        "типы пережили включение transform"
    );
    d.clear_transform();
    settle(&mut d);
    assert_eq!(
        d.filter.messages,
        vec!["Order".to_string()],
        "типы пережили снятие transform"
    );
}

/// Ctrl+A / Ctrl+E — начало и конец строки, как в readline.
#[test]
fn lua_editor_ctrl_a_e_moves_to_line_edges() {
    let mut d = open("lz4", 8);
    d.lua_code = "ab\ncd".into();
    d.lua_cursor = 4; // 'd' на второй строке

    let e = tui_patch::ui::lua_editor::tui_key(&mut d, Key::Ctrl('e'));
    assert!(e);
    assert_eq!(d.lua_cursor, 5, "Ctrl+E — конец строки (после 'd')");

    assert!(tui_patch::ui::lua_editor::tui_key(&mut d, Key::Ctrl('a')));
    assert_eq!(d.lua_cursor, 3, "Ctrl+A — начало строки ('c')");

    assert!(tui_patch::ui::lua_editor::tui_key(&mut d, Key::Ctrl('a')));
    assert_eq!(d.lua_cursor, 3, "уже в начале строки — на месте");

    // На первой строке Ctrl+A — в начало кода.
    assert!(tui_patch::ui::lua_editor::tui_key(&mut d, Key::Up));
    assert!(tui_patch::ui::lua_editor::tui_key(&mut d, Key::Ctrl('a')));
    assert_eq!(d.lua_cursor, 0, "Ctrl+A на первой строке — в начало кода");
}

/// Последний seq эталона.
fn last_seq(name: &str) -> i64 {
    *all_seqs(&fixture(name)).last().expect("файл не пуст")
}

/// `agr:anchor` — возврат к последнему сообщению (сессия, переход `g`): оно
/// внизу экрана, а выше — предыдущие, без прокрутки; под ним пустоты нет.
#[test]
fn returning_to_the_last_message_puts_it_at_the_bottom() {
    let last = last_seq("lz4");
    let mut d = open("lz4", 8);
    d.restore(&tui_patch::session::Session {
        seq: Some(last),
        ..Default::default()
    });
    settle(&mut d);
    assert_eq!(sel(&d), last, "выбрано последнее");
    assert_eq!(d.top + d.height, d.rows.len(), "под последней пусто нет");
    assert!(d.top > 0, "выше неё есть строки, а не одна она");
    assert_eq!(d.sel, d.rows.len() - 1);

    // То же по `g`: и на последний seq, и за конец файла.
    for target in [last, last + 1000] {
        let mut d = open("lz4", 8);
        d.goto(&target.to_string());
        settle(&mut d);
        assert_eq!(sel(&d), last, "g {target}");
        assert_eq!(d.top + d.height, d.rows.len(), "g {target}: внизу экрана");
    }
}

/// `agr:anchor` — возврат к сообщению в середине файла: треть экрана контекста
/// выше выбранной, как у фильтра, а не выбранная на самом верху.
#[test]
fn returning_to_a_middle_message_keeps_a_third_of_the_screen_above() {
    let seqs = all_seqs(&fixture("lz4"));
    let mid = seqs[seqs.len() / 2];
    let mut d = open("lz4", 9);
    d.goto(&mid.to_string());
    settle(&mut d);
    assert_eq!(sel(&d), mid);
    assert_eq!(d.sel - d.top, d.height / 3, "треть экрана выше выбранной");
    assert!(d.top > 0, "выше выбранной есть строки в окне");
}

/// `agr:anchor` — окно дочитано до конца: растянули окно — последняя остаётся
/// внизу, а не уезжает вверх с пустотой под собой; строк меньше экрана — виден
/// весь хвост с первой строки окна.
#[test]
fn growing_the_screen_keeps_the_last_row_at_the_bottom() {
    let last = last_seq("lz4");
    let mut d = open("lz4", 8);
    d.goto(&last.to_string());
    settle(&mut d);
    d.set_height(20);
    assert_eq!(d.top + 20, d.rows.len(), "под последней пусто нет");
    d.set_height(10_000);
    assert_eq!(d.top, 0, "строк меньше экрана — виден весь хвост");
}

/// `agr:anchor` — PgDn до конца файла: последняя строка внизу экрана, а не
/// на самом верху с пустотой под собой; дальше PgDn ничего не двигает.
#[test]
fn page_down_to_the_end_keeps_the_last_row_at_the_bottom() {
    let last = last_seq("lz4");
    let mut d = open("lz4", 8);
    for _ in 0..400 {
        key(&mut d, Key::PageDown);
        // Пока дочитывается — пустоту под последней строкой оставлять нельзя.
        if d.at_end {
            assert!(
                d.top + d.height <= d.rows.len(),
                "PgDn: пустота под последней"
            );
        }
    }
    settle(&mut d);
    assert_eq!(sel(&d), last, "дошли до последней");
    assert_eq!(d.top + d.height, d.rows.len(), "последняя — у нижнего края");
    let before = (d.top, d.sel);
    key(&mut d, Key::PageDown);
    assert_eq!((d.top, d.sel), before, "ещё PgDn — всё на месте");
}

/// Прогнать `restore` сессии с `seq` и `transform`/фильтром и дождаться тишины.
fn restored(session: tui_patch::session::Session) -> Desk {
    let mut d = open("lz4", 8);
    d.restore(&session);
    settle(&mut d);
    if let Some(m) = d.matches.clone() {
        while !m.is_done() {
            settle(&mut d);
        }
    }
    settle(&mut d);
    d
}

/// seq Order из середины эталона: он есть и при Lua, оставляющем только Order.
fn middle_order() -> i64 {
    let mut d = open("lz4", 8);
    key(&mut d, Key::Char('f'));
    typed(&mut d, "name == 'Order'");
    key(&mut d, Key::Enter);
    let m = d.matches.clone().unwrap();
    while !m.is_done() {
        settle(&mut d);
    }
    let rows = m.len();
    d.goto(&format!("+{}", rows / 2));
    settle(&mut d);
    sel(&d)
}

/// `agr:session-by-name` — Lua-transform, выбрасывающий строки, не теряет
/// положение при перезапуске: выбрана та же строка, что в сессии.
#[test]
fn restore_with_dropping_transform_keeps_the_selected_row() {
    if !lua() {
        return;
    }
    let want = middle_order();
    let s = tui_patch::session::Session {
        seq: Some(want),
        lua_transform: Some(
            "function tll_on_data(seq, name, data)\n    if name == 'Order' then tll_callback(seq, name, data) end\nend"
                .into(),
        ),
        ..Default::default()
    };
    let d = restored(s);
    assert!(
        d.rows.iter().all(|r| r.name == "Order"),
        "transform выбросил остальные"
    );
    assert_eq!(sel(&d), want, "выбранная строка вернулась");
}

/// То же для transform, ничего не выбрасывающего.
#[test]
fn restore_with_passthrough_transform_keeps_the_selected_row() {
    if !lua() {
        return;
    }
    let want = middle_order();
    let s = tui_patch::session::Session {
        seq: Some(want),
        lua_transform: Some(
            "function tll_on_data(seq, name, data)\n    tll_callback(seq, name, data)\nend".into(),
        ),
        ..Default::default()
    };
    assert_eq!(sel(&restored(s)), want);
}

/// Фильтр-выражение и transform вместе.
#[test]
fn restore_with_filter_and_transform_keeps_the_selected_row() {
    if !lua() {
        return;
    }
    let want = middle_order();
    let s = tui_patch::session::Session {
        seq: Some(want),
        expr: "name == 'Order'".into(),
        lua_transform: Some(
            "function tll_on_data(seq, name, data)\n    tll_callback(seq, name, data)\nend".into(),
        ),
        ..Default::default()
    };
    assert_eq!(sel(&restored(s)), want);
}

/// `agr:reload` — `r` перечитывает растущий файл на том же месте: курсор
/// остаётся на прежнем seq, окно не прыгает в хвост, а в статусе — сколько
/// дописалось. `↓` на последней строке — то же.
#[test]
fn reload_keeps_position_and_reports_new_rows() {
    use tui_patch::io::{Info, Msg, Writer};

    let dir = std::env::temp_dir().join(format!("mutator-reload-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("growing.current.dat");
    std::fs::copy(fixture("plain"), &path).unwrap();

    let mut d = Desk::open(File::open(&path).unwrap(), Arc::new(|| {}));
    d.height = 8;
    let t = Instant::now();
    while d.index.is_none() {
        d.wait(Duration::from_millis(100));
        assert!(t.elapsed() < Duration::from_secs(30), "индекс не пришёл");
    }
    settle(&mut d);

    // Встаём на последний seq на момент открытия.
    let before = *all_seqs(&path).last().unwrap();
    key(&mut d, Key::End);
    assert_eq!(sel(&d), before, "стартовый выбор — последний seq");

    // Дописываем два сообщения — хвост вырастает на две строки.
    let f = File::open(&path).unwrap();
    let scheme = f.scheme_url.clone().expect("схема");
    let info: Info = f.info.clone();
    let ctx = Context::new();
    for seq in [before + 1, before + 2] {
        let mut w = Writer::open(&ctx, &path, &scheme, &info, false).expect("дописать");
        w.post(&Msg {
            seq,
            msgid: 30,
            data: vec![0; 4],
        })
        .expect("post");
        drop(w);
    }

    // ↓ на последней строке — перечитать, но курсор остаётся на прежнем seq.
    key(&mut d, Key::Down);
    assert_eq!(sel(&d), before, "курсор не сдвинулся с прежнего seq");
    assert!(
        d.status.contains("+2 строк"),
        "статус сообщает число новых строк: {}",
        d.status
    );

    // Новый хвост достижим обычным шагом вниз.
    key(&mut d, Key::Down);
    assert_eq!(
        sel(&d),
        before + 1,
        "шаг вниз — на первую дописанную строку"
    );

    // `r` из середины файла тоже сохраняет место.
    key(&mut d, Key::Up);
    assert_eq!(sel(&d), before, "вернулись на прежний seq");
    let later = before + 3;
    let mut w = Writer::open(&ctx, &path, &scheme, &info, false).expect("дописать");
    w.post(&Msg {
        seq: later,
        msgid: 30,
        data: vec![0; 4],
    })
    .expect("post");
    drop(w);
    key(&mut d, Key::Char('r'));
    assert_eq!(sel(&d), before, "после r курсор всё ещё на прежнем seq");
    assert!(
        d.status.contains("+1 строка"),
        "статус сообщает одну новую строку: {}",
        d.status
    );

    // `End` с последней строки: перечитать и встать на новую последнюю, а не на
    // прежний seq. Хвост дописываем больше размера окна, чтобы достройка нового
    // индекса не увела окно с конца (`agr:reload`).
    let cur_last = *all_seqs(&path).last().unwrap();
    key(&mut d, Key::End);
    assert_eq!(
        sel(&d),
        cur_last,
        "End — уже на последнем seq на момент открытия"
    );
    // Файл вырастает больше размера окна: `End` перечитывает его и обязан
    // встать на настоящий последний seq, а не на прежний.
    let grow = 400;
    let mut w = Writer::open(&ctx, &path, &scheme, &info, false).expect("дописать");
    for i in 1..=grow {
        w.post(&Msg {
            seq: cur_last + i,
            msgid: 30,
            data: vec![0; 4],
        })
        .expect("post");
    }
    drop(w);
    let new_last = *all_seqs(&path).last().unwrap();
    key(&mut d, Key::End);
    // Дождаться, пока перестроится индекс нового файла (`open_with` поднял
    // воркер заново) и вид успокоится.
    let t = Instant::now();
    while d.index.is_none() || !d.idle() {
        d.wait(Duration::from_millis(100));
        d.poll();
        assert!(
            t.elapsed() < Duration::from_secs(30),
            "индекс не пришёл: {}",
            d.status
        );
    }
    assert_eq!(
        sel(&d),
        new_last,
        "End с последней строки перечитывает и встаёт на новую последнюю"
    );
    assert!(
        d.sel >= d.top && d.sel < d.top + d.height,
        "выбранная строка на экране"
    );
}

/// `agr:reload` — `End` держится снапшота файла на момент переоткрытия: файл
/// дописали, пока окно читалось, — выбор на конце снапшота, не в середине и не
/// на свежем хвосте; следующий `End` догоняет конец, на спокойном — «не изменился».
#[test]
fn end_during_growth_lands_on_snapshot_end() {
    use tui_patch::io::{Info, Msg, Writer};

    let dir = std::env::temp_dir().join(format!("mutator-end-race-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("growing.current.dat");
    std::fs::copy(fixture("plain"), &path).unwrap();

    let mut d = Desk::open(File::open(&path).unwrap(), Arc::new(|| {}));
    d.height = 8;
    let t = Instant::now();
    while d.index.is_none() {
        d.wait(Duration::from_millis(100));
        assert!(t.elapsed() < Duration::from_secs(30), "индекс не пришёл");
    }
    settle(&mut d);

    let ctx = Context::new();
    let f = File::open(&path).unwrap();
    let scheme = f.scheme_url.clone().expect("схема");
    let info: Info = f.info.clone();
    let post = |seq: i64| {
        let mut w = Writer::open(&ctx, &path, &scheme, &info, false).expect("дописать");
        w.post(&Msg {
            seq,
            msgid: 30,
            data: vec![0; 4],
        })
        .expect("post");
    };

    // Стартуем с конца файла.
    key(&mut d, Key::End);
    let before = *all_seqs(&path).last().unwrap();
    assert_eq!(sel(&d), before, "стартовый выбор — последний seq");

    // Одна новая строка, и `End` уходит в перечитывание: снапшот `reload_to`
    // берёт синхронно…
    post(before + 1);
    d.key(Key::End);
    // …а пока новый воркер читает окно, файл дописывают ещё дважды.
    post(before + 2);
    post(before + 3);

    // Выбор — на конце снапшота переоткрытия, не в середине и не на новом хвосте.
    let t = Instant::now();
    while d.index.is_none() || !d.idle() {
        d.wait(Duration::from_millis(100));
        d.poll();
        assert!(
            t.elapsed() < Duration::from_secs(30),
            "не успокоилось: {}",
            d.status
        );
    }
    assert_eq!(
        sel(&d),
        before + 1,
        "End на конце снапшота переоткрытия, а не середина и не новый хвост"
    );
    assert!(
        d.sel >= d.top && d.sel < d.top + d.height,
        "выбранная строка на экране"
    );
    assert!(
        d.status.contains("+1 строка"),
        "статус — хвост на момент снапшота: {}",
        d.status
    );

    // Следующий `End` догоняет настоящий конец.
    key(&mut d, Key::End);
    assert_eq!(sel(&d), before + 3, "повторный End догнал новый хвост");
    assert!(d.status.contains("+2 строк"), "статус: {}", d.status);

    // Файл больше не растёт: `End` на месте ничего не перечитывает.
    key(&mut d, Key::End);
    assert_eq!(
        sel(&d),
        before + 3,
        "End на спокойном конце не двигает выбор"
    );
    assert!(
        d.status.contains("файл не изменился"),
        "статус: {}",
        d.status
    );
}

/// `agr:reload` — файл обновляет только `End` с последней строки (и `r`, `↓`,
/// `PgDn` там же): из середины `End` — навигация внутри открытого снимка,
/// без перечитывания — каким бы ни вырос файл на диске.
#[test]
fn end_from_middle_stays_within_the_open_snapshot() {
    use tui_patch::io::{Info, Msg, Writer};

    let dir = std::env::temp_dir().join(format!("mutator-end-middle-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("growing.current.dat");
    std::fs::copy(fixture("plain"), &path).unwrap();

    let mut d = Desk::open(File::open(&path).unwrap(), Arc::new(|| {}));
    d.height = 8;
    let t = Instant::now();
    while d.index.is_none() {
        d.wait(Duration::from_millis(100));
        assert!(t.elapsed() < Duration::from_secs(30), "индекс не пришёл");
    }
    settle(&mut d);

    let ctx = Context::new();
    let f = File::open(&path).unwrap();
    let scheme = f.scheme_url.clone().expect("схема");
    let info: Info = f.info.clone();
    let old_last = *all_seqs(&path).last().unwrap();
    assert_ne!(sel(&d), old_last, "курсор не на последней строке файла");

    // Пока курсор в середине, файл вырастает на 50 строк.
    let mut w = Writer::open(&ctx, &path, &scheme, &info, false).expect("дописать");
    for i in 1..=50 {
        w.post(&Msg {
            seq: old_last + i,
            msgid: 30,
            data: vec![0; 4],
        })
        .expect("post");
    }
    drop(w);

    // `End` из середины — в конец открытого снимка, файл не перечитывается.
    let gen = d.file_gen().load(std::sync::atomic::Ordering::SeqCst);
    key(&mut d, Key::End);
    assert_eq!(
        sel(&d),
        old_last,
        "End из середины — в конец снимка, а не выросшего файла"
    );
    assert_eq!(
        d.file_gen().load(std::sync::atomic::Ordering::SeqCst),
        gen,
        "перечитывания не было"
    );
}
