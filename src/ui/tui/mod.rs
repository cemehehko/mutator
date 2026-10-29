//! TUI: весь экран просмотрщика одной сеткой знаков — и в окне (вид «TUI»), и
//! в настоящем терминале ([`term`]). Рисует [`frame`] из того же [`Desk`], что и
//! обычный вид (`agr:tui-same-data`); раскладка задаёт [`Desk::height`].
//!
//! Экран сверху вниз: заголовок файла, шапка таблицы, строки с полосой
//! прокрутки, деталь (дерево полей и hex), строка ввода, строка состояния.
//! Списки выбора (типы, колонки) — рамкой поверх таблицы, диалог фиксации
//! правок и справка F1 — рамкой на весь экран. Красные значения — правленые и
//! изменённые мутатором (`agr:transform-shadow`).
//!
//! Файлы: `grid.rs` — сетка, `term/` — терминал, `browse.rs` — список линков
//! resolve и ожидание выбранного линка.
//!
//! Наружу: [`frame`], [`Act`], [`grid`], [`term`], [`browse`], [`position`].
//!
//! У линка в заголовке справа — его состояние ([`link_line`], `agr:link-state`):
//! видно всегда, цветом по состоянию; тот же значок — в строке состояния окна.
//!
//! Ещё: [`group`], [`title`], [`apply`], [`link_line`], [`split_at`].

pub mod browse;
pub mod grid;
pub mod term;

use egui::Color32;

use crate::decode::Node;
use crate::desk::{CommitMode, Desk, EditKind, Focus, Key, PromptKind};
use crate::ui::cols::{self, ColKind};
use crate::ui::help;
use crate::ui::theme::*;
use grid::Grid;

/// Действие по щелчку в сетке.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Act {
    /// Выбрать строку окна (номер в [`Desk::rows`]).
    Row(usize),
    /// Перейти по доле полосы прокрутки.
    Scroll(f32),
    /// Строка дерева детали.
    Field(usize),
    /// Пункт списка выбора (номер среди видимых).
    Pick(usize),
    Key(Key),
    /// Колонка списка resolve: сортировать по ней ([`browse`]).
    Sort(usize),
    /// Граница таблицы и детали: её тянут мышью ([`split_at`]), щелчок — ничего.
    Split,
}

/// Строка «12 345 678» — разряды пробелом.
pub fn group(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

/// Положение выбора: seq из файла и номер строки таблицы — подписаны, чтобы
/// номер строки не путали с seq.
pub fn position(d: &Desk) -> String {
    let seq = d
        .selected()
        .map(|r| format!("seq {}", r.seq))
        .unwrap_or_default();
    match (d.sel_row(), d.total()) {
        (Some(r), Some((t, grow))) => format!(
            "{seq} · строка {} из {}{}",
            group(r + 1),
            group(t),
            if grow { "+" } else { "" }
        ),
        _ => seq,
    }
}

/// Состояние линка строкой и цветом (`agr:link-state`); не линк — `None`.
pub fn link_line(d: &Desk) -> Option<(String, Color32)> {
    use crate::io::link::LinkState;
    let s = d.link_status()?;
    let fg = match s.state {
        LinkState::Active => OK,
        LinkState::Error => ERROR,
        LinkState::Closed => DIM,
        LinkState::Opening | LinkState::Closing => NAME,
    };
    let mut t = format!("● {}", s.state.name());
    match (s.history, s.online) {
        (_, true) => t += " · онлайн",
        (Some(true), false) => t += " · история",
        _ => {}
    }
    t += &format!(" · {} · {}", s.open, group(s.count as usize));
    if let Some(e) = &s.error {
        t += &format!(" · {}", e.lines().next().unwrap_or(""));
    }
    if s.kept {
        if let Some(p) = &s.spool {
            t += &format!(" · спул {}", p.display());
        }
    }
    Some((t, fg))
}

/// Заголовок: файл, границы, сколько строк, фильтр, индекс.
pub fn title(d: &Desk) -> String {
    let name = match &d.file.link {
        Some(l) => l.url().to_string(),
        None => std::path::Path::new(&d.file.name)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| d.file.name.clone()),
    };
    let i = &d.file.info;
    let mut s = format!(" {name}");
    if let (Some(b), Some(l)) = (i.seq_begin, i.seq_last) {
        s += &format!("  seq {b}…{l}");
    }
    match (&d.matches, &d.index) {
        (Some(m), _) => {
            s += &format!("  найдено {}", group(m.len()));
            if !m.is_done() {
                s += &format!(" ({:.0}%)", m.progress() * 100.0);
            }
        }
        (None, Some(idx)) => s += &format!("  {} сообщений", group(idx.total)),
        (None, None) => s += &format!("  индекс {:.0}%", d.index_progress * 100.0),
    }
    if let Some(c) = &i.compression {
        if c != "none" {
            s += &format!("  {c}");
        }
    }
    if !d.filter.is_empty() {
        s += &format!("  │ {}", d.filter.summary());
    }
    if !d.lua_code.is_empty() {
        let state = if d.lua_enabled { "вкл" } else { "выкл" };
        s += &format!("  │ Lua transform {state} (F4 правка)");
    }
    if !d.edits.is_empty() {
        s += &format!("  │ правок {} (w)", d.edits.count());
    }
    s
}

/// Сколько строк занимает перенесённое значение выбранного поля.
const WRAP: usize = 6;

const HELP: &str = "↑↓ PgUp/PgDn Home/End  Enter деталь  x hex  i типы полей  f фильтр  t типы  / ? поиск  n/N  g переход  c колонки  p пресет  e правка  E transform вкл/выкл  F4 код Lua  w зафиксировать  { } высота детали  q выход";

/// Нарисовать экран `cols`×`rows`; заодно задать высоты таблицы и детали.
/// Строка ввода внизу экрана: у всех подсказок, кроме списков выбора поверх
/// таблицы.
fn line_prompt(d: &Desk) -> bool {
    d.prompt.as_ref().is_some_and(|p| {
        !matches!(p.kind, PromptKind::Types | PromptKind::Columns)
            || matches!(
                p.kind,
                PromptKind::Edit {
                    kind: EditKind::Enum | EditKind::Bits,
                    ..
                }
            )
    })
}

/// Строк на таблицу и деталь вместе: экран без заголовка, шапки, состояния и
/// строки ввода.
fn avail_rows(d: &Desk, rows: usize) -> usize {
    let fixed = 3 + line_prompt(d) as usize;
    rows.max(6).saturating_sub(fixed).max(3)
}

/// Граница детали перетащена на строку экрана `row` (экран `rows` строк):
/// доля детали — от этой строки до низа, как у окна, тянущего панель
/// (`agr:detail-share`).
pub fn split_at(d: &mut Desk, rows: usize, row: usize) {
    let avail = avail_rows(d, rows);
    let table = row.saturating_sub(2).clamp(1, avail);
    d.detail_ratio = ((avail - table) as f32 / avail as f32).clamp(0.05, 0.8);
    d.status = format!("высота детали: {:.0}%", d.detail_ratio * 100.0);
}

pub fn frame(d: &mut Desk, cols: usize, rows: usize) -> Grid<Act> {
    let mut g: Grid<Act> = Grid::new(cols);
    let rows = rows.max(6);
    let line_prompt = line_prompt(d);
    let avail = avail_rows(d, rows);
    let detail = if d.detail_open {
        let n = (avail as f32 * d.detail_ratio).round() as usize;
        n.clamp(2, avail.saturating_sub(1))
    } else {
        0
    };
    let table = avail.saturating_sub(detail).max(1);
    d.set_height(table);
    d.detail_height = detail.saturating_sub(1);

    // Заголовок.
    g.shade(0, 0, cols, PANEL);
    g.put(0, 0, &title(d), TEXT);
    if let Some((t, fg)) = link_line(d) {
        // Состояние линка — поверх хвоста заголовка: оно важнее.
        let w = t.chars().count() + 2;
        let x = cols.saturating_sub(w);
        for k in x..cols {
            g.set(0, k, ' ', TEXT);
        }
        g.put(0, x + 1, &t, fg);
    }

    // Таблица: шапка и строки, справа полоса прокрутки.
    let width = cols.saturating_sub(1);
    let (layout, more) = cols::layout(d, width);
    let mut x = 0;
    g.shade(1, 0, cols, PANEL);
    for c in &layout {
        g.put(1, x, &cols::fit(&c.title, c.width, c.right), DIM);
        x += c.width + 1;
    }
    if more || d.hscroll > 0 {
        let mark = match (d.hscroll > 0, more) {
            (true, true) => "◂▸",
            (true, false) => "◂ ",
            _ => " ▸",
        };
        g.put_right(1, width, mark, ACCENT);
    }
    for i in 0..table {
        let y = 2 + i;
        let idx = d.top + i;
        let Some(r) = d.rows.get(idx) else { continue };
        let selected = idx == d.sel;
        if selected {
            g.shade(
                y,
                0,
                width,
                if d.focus == Focus::Table {
                    SELECT
                } else {
                    SELECT_DIM
                },
            );
        }
        // Полос через строку в TUI нет: в терминале они рябят; в окне — есть.
        let mut x = 0;
        for c in &layout {
            let text = cols::cell(r, c.kind);
            let fg = match c.kind {
                _ if d.deleted_row(r) => DIM,
                ColKind::Seq if r.ins.is_some() || d.edited_row(r.seq) || d.mutated_row(r.seq) => {
                    EDITED
                }
                ColKind::Seq => DIM,
                ColKind::Name => NAME,
                ColKind::DataLen if d.edited_row(r.seq) || d.mutated_row(r.seq) => EDITED,
                ColKind::DataLen => TEXT,
                ColKind::Field(i)
                    if d.edited(r.seq, &d.columns[i])
                        || d.mutated(r.seq, &d.columns[i]).is_some() =>
                {
                    EDITED
                }
                ColKind::Field(_) => TEXT,
            };
            g.put(y, x, &cols::fit(&text, c.width, c.right), fg);
            x += c.width + 1;
        }
        g.link(y, 0, width, Act::Row(idx));
    }
    if d.rows.is_empty() {
        let msg = if d.idle() {
            "нет сообщений"
        } else {
            "читаю…"
        };
        g.put(3, 2, msg, DIM);
    }
    scrollbar(&mut g, d, cols.saturating_sub(1), 2, table);

    // Деталь.
    let mut y = 2 + table;
    if detail > 0 {
        draw_detail(&mut g, d, y, cols, detail);
        g.link(y, 0, cols, Act::Split);
        y += detail;
    }

    // Строка ввода.
    if line_prompt {
        let p = d.prompt.as_ref().unwrap();
        let head = match p.kind {
            PromptKind::Edit { .. } => format!("{} {}: ", p.title(), p.path),
            _ => format!("{}: ", p.title()),
        };
        let x = g.put(y, 0, &head, ACCENT);
        g.put(y, x, &p.text, TEXT);
        let cx = x + p.cursor;
        g.shade(y, cx, 1, ACCENT);
        y += 1;
    }

    // Состояние.
    let (text, fg) = if !d.status.is_empty() {
        (
            d.status.clone(),
            if d.status.starts_with("не ") || d.status.contains(':') {
                ERROR
            } else {
                OK
            },
        )
    } else {
        let pos = format!("{}  ", position(d));
        (format!("{pos}{HELP}"), DIM)
    };
    g.put(y, 0, &text, fg);
    g.reach_rows(rows);

    if let Some(p) = &d.prompt {
        if matches!(p.kind, PromptKind::Types | PromptKind::Columns)
            || matches!(
                p.kind,
                PromptKind::Edit {
                    kind: EditKind::Enum | EditKind::Bits,
                    ..
                }
            )
        {
            draw_picker(&mut g, d, cols, 1, table + 1);
        }
    }
    if d.commit.is_some() {
        draw_commit(&mut g, d, cols, rows);
    }
    if d.help.open {
        draw_help(&mut g, d, cols, rows);
    }
    if let Some(f) = &d.link_form {
        browse::draw_form(&mut g, f, cols, rows);
    }
    g
}

/// Справка F1 рамкой на весь экран: листается, футер закрывает щелчком.
fn draw_help(g: &mut Grid<Act>, d: &mut Desk, cols: usize, rows: usize) {
    help::clamp_scroll(d, cols, rows);
    let h = &d.help;
    let (x, y, w, height, body) = help::layout(cols, rows);
    for r in 0..height {
        g.shade(y + r, x, w, PANEL);
        for k in 0..w {
            g.set(y + r, x + k, ' ', TEXT);
        }
    }
    g.put(y, x + 1, "Справка", ACCENT);
    g.put_right(y, x + w - 1, "F1/Esc — закрыть", DIM);
    let list = help::flat();
    for (i, (l, title)) in list.iter().skip(h.scroll).take(body).enumerate() {
        let fg = if *title { NAME } else { TEXT };
        g.put(
            y + 1 + i,
            x + 1,
            &cols::fit(l, w.saturating_sub(2), false),
            fg,
        );
    }
    let yy = y + height - 1;
    g.put_right(yy, x + w - 1, "↑↓ PgUp/PgDn листать  F1/Esc — закрыть", DIM);
    g.link(yy, x, w, Act::Key(Key::Esc));
}

fn scrollbar(g: &mut Grid<Act>, d: &Desk, x: usize, y: usize, h: usize) {
    if h < 2 {
        return;
    }
    for i in 0..h {
        g.set(y + i, x, '│', SELECT_DIM);
        g.link(y + i, x, 1, Act::Scroll(i as f32 / (h - 1) as f32));
    }
    if let Some((at, size)) = d.scroll_fraction() {
        let start = ((at * h as f32) as usize).min(h - 1);
        let len = ((size * h as f32).ceil() as usize).clamp(1, h - start);
        for i in start..start + len {
            g.set(y + i, x, '█', DIM);
        }
    }
}

fn draw_detail(g: &mut Grid<Act>, d: &Desk, y: usize, cols: usize, h: usize) {
    let Some(det) = &d.detail else {
        g.put(y, 0, "─ (нет строки)", DIM);
        return;
    };
    let Some(row) = d.rows.iter().find(|r| r.seq == det.seq && r.ins == det.ins) else {
        return;
    };
    // Состав потока: вставка, удалено, seq после записи (`agr:view-input-seq`).
    let head = match (&row.tag, row.ins) {
        (Some(t), Some(_)) => format!(
            "─ {}  вставка перед {} → seq {}",
            row.name,
            row.seq,
            &t[1..]
        ),
        (Some(t), None) => format!("─ {}  seq {t}", row.name),
        (None, _) => format!("─ {}  seq {}", row.name, row.seq),
    };
    let tail = format!("  {} байт ", row.data.len());
    for x in 0..cols {
        g.set(y, x, '─', DIM);
    }
    let mut hx = g.put(y, 0, &head, ACCENT);
    // Мутатор поменял seq: рядом, красным, тот, что в файле.
    if let Some(orig) = d.seq_in_file(row.seq) {
        hx = g.put(y, hx, &format!(" (в файле {orig})"), EDITED);
    }
    g.put(y, hx, &tail, ACCENT);
    let body = h - 1;
    let hex_w = if cols >= 140 { 16 } else { 8 };
    let hex_cols = if d.show_hex {
        6 + hex_w * 3 + hex_w + 1
    } else {
        0
    };
    let tree_w = cols.saturating_sub(hex_cols);
    let lines = det.lines();
    let focus = d.focus == Focus::Detail;
    // Строка поля: имя, значение, справа — тип. Длинное значение обрезано с «…»,
    // у выбранного поля — перенесено на следующие строки (до WRAP строк).
    let wrap_of = |k: usize| -> (usize, Vec<String>) {
        let (depth, n) = lines[k];
        let head = depth * 2 + n.name.chars().count() + 2;
        let room = tree_w.saturating_sub(head + 1).max(8);
        let text: Vec<char> = n.text().chars().collect();
        if text.len() <= room {
            return (head, vec![text.iter().collect()]);
        }
        if k != det.cursor {
            return (head, vec![cols::fit(&n.text(), room, false)]);
        }
        let mut parts: Vec<String> = text
            .chunks(room)
            .take(WRAP)
            .map(|c| c.iter().collect())
            .collect();
        if text.len() > room * WRAP {
            let last = parts.last_mut().unwrap();
            last.pop();
            last.push('…');
        }
        (head, parts)
    };
    // Выбранное с переносом должно влезть: при нужде начинаем ниже `top`.
    let extra = if det.cursor < lines.len() {
        wrap_of(det.cursor).1.len() - 1
    } else {
        0
    };
    let mut start = det.top;
    if det.cursor + 1 + extra > start + body {
        start = (det.cursor + 1 + extra).saturating_sub(body);
    }
    let mut yy = y + 1;
    let mut k = start;
    while yy < y + 1 + body && k < lines.len() {
        let (depth, n) = lines[k];
        let (_, parts) = wrap_of(k);
        let rows = parts.len().min(y + 1 + body - yy);
        if k == det.cursor {
            for r in 0..rows {
                g.shade(yy + r, 0, tree_w, if focus { SELECT } else { SELECT_DIM });
            }
        }
        let x = g.put(
            yy,
            depth * 2,
            &n.name,
            if n.error.is_some() { ERROR } else { NAME },
        );
        let mut x = x;
        if !parts[0].is_empty() {
            let fg = if (det.ins.is_none() && d.edited(det.seq, &n.path))
                || d.mutated(det.seq, &n.path).is_some()
            {
                EDITED
            } else {
                TEXT
            };
            let after = g.put(yy, x, ": ", DIM);
            x = g.put(yy, after, &parts[0], fg);
            for (r, p) in parts.iter().enumerate().take(rows).skip(1) {
                g.put(yy + r, after, p, fg);
            }
        }
        let meta = format!("{} @{}+{}", n.ty, n.offset, n.size);
        if d.show_types && x + meta.chars().count() + 2 < tree_w {
            g.put_right(yy, tree_w, &meta, DIM);
        }
        for r in 0..rows {
            g.link(yy + r, 0, tree_w, Act::Field(k));
        }
        yy += rows;
        k += 1;
    }
    // Hex выбранного: поле подсвечено, его начало — на экране.
    if !d.show_hex {
        return;
    }
    let cur = det.current();
    let (from, to) = cur
        .map(|n: &Node| (n.offset, n.offset + n.size))
        .unwrap_or((0, 0));
    let first = (from / hex_w).saturating_sub(body / 3);
    let x0 = tree_w + 1;
    for i in 0..body {
        let yy = y + 1 + i;
        g.set(yy, tree_w, '│', SELECT_DIM);
        let off = (first + i) * hex_w;
        if off >= row.data.len() {
            continue;
        }
        g.put(yy, x0, &format!("{off:04x}"), DIM);
        for j in 0..hex_w {
            let p = off + j;
            let Some(b) = row.data.get(p) else { break };
            let hx = x0 + 6 + j * 3;
            let ax = x0 + 6 + hex_w * 3 + j;
            let on = p >= from && p < to;
            if on {
                g.shade(yy, hx, 2, HILITE);
                g.shade(yy, ax, 1, HILITE);
            }
            g.put(yy, hx, &format!("{b:02x}"), if on { TEXT } else { DIM });
            let ch = if b.is_ascii_graphic() {
                *b as char
            } else {
                '·'
            };
            g.set(yy, ax, ch, if on { TEXT } else { DIM });
        }
    }
}

fn draw_picker(g: &mut Grid<Act>, d: &Desk, cols: usize, y: usize, h: usize) {
    let p = d.prompt.as_ref().unwrap();
    let w = (cols * 2 / 3).clamp(30, 70).min(cols);
    let x = (cols - w) / 2;
    let h = h.clamp(5, 30);
    let inner = h - 3;
    for r in 0..h {
        g.shade(y + r, x, w, PANEL);
        for c in 0..w {
            g.set(y + r, x + c, ' ', TEXT);
        }
    }
    let hint = match p.kind {
        PromptKind::Types => "Пробел — отметить, Esc — применить",
        PromptKind::Columns => "Пробел — вкл/выкл, Esc/Enter — закрыть",
        PromptKind::Edit {
            kind: EditKind::Enum,
            ..
        } => "Enter — выбор из списка или числа",
        PromptKind::Edit {
            kind: EditKind::Bits,
            ..
        } => "Enter — hex или выбор флагов",
        _ => "Пробел — вкл/выкл, Esc — закрыть",
    };
    g.put(y, x + 1, p.title(), ACCENT);
    g.put_right(y, x + w - 1, hint, DIM);
    let q = g.put(y + 1, x + 1, "› ", ACCENT);
    g.put(y + 1, q, &p.text, TEXT);
    g.shade(y + 1, q + p.cursor, 1, ACCENT);
    let vis = p.visible();
    let top = if p.sel >= inner { p.sel + 1 - inner } else { 0 };
    for (k, (_, item)) in vis.iter().enumerate().skip(top).take(inner) {
        let yy = y + 2 + k - top;
        if k == p.sel {
            g.shade(yy, x, w, SELECT);
        }
        let is_enum = matches!(
            p.kind,
            PromptKind::Edit {
                kind: EditKind::Enum,
                ..
            }
        );
        let mark = match (is_enum, item.on) {
            (true, true) => "(•) ",
            (true, false) => "( ) ",
            (false, true) => "[x] ",
            (false, false) => "[ ] ",
        };
        let after = g.put(yy, x + 1, mark, if item.on { OK } else { DIM });
        g.put(yy, after, &item.label, TEXT);
        g.put_right(yy, x + w - 1, &item.hint, DIM);
        g.link(yy, x, w, Act::Pick(k));
    }
    if vis.is_empty() {
        g.put(y + 2, x + 2, "ничего не подходит", DIM);
    }
    let _: Color32 = TEXT;
}

/// Диалог фиксации: слева итоги и сводка, справа yaml, внизу куда писать.
fn draw_commit(g: &mut Grid<Act>, d: &Desk, cols: usize, rows: usize) {
    let c = d.commit.as_ref().unwrap();
    let (x, y) = (1.min(cols), 1.min(rows));
    let w = cols.saturating_sub(2).max(20);
    let h = rows.saturating_sub(2).max(8);
    for r in 0..h {
        g.shade(y + r, x, w, PANEL);
        for k in 0..w {
            g.set(y + r, x + k, ' ', TEXT);
        }
    }
    g.put(y, x + 1, "Зафиксировать правки", ACCENT);
    g.put_right(
        y,
        x + w - 1,
        "Tab — режим  Enter — записать  Esc — отмена  ↑↓ PgUp/PgDn — листать",
        DIM,
    );
    // Первая строка — прогноз последнего seq или ошибка записи
    // (`agr:seq-forecast`, `agr:commit-seq-check`).
    let head_fg = match (&c.error, &c.forecast) {
        (Some(_), _) => ERROR,
        (None, Some(f)) if f.changes() => EDITED,
        _ => TEXT,
    };
    g.put(
        y + 1,
        x + 1,
        &cols::fit(&c.headline(), w.saturating_sub(2), false),
        head_fg,
    );
    let y = y + 1;
    let left = w / 2;
    let body = h.saturating_sub(6);
    let mut list: Vec<(String, Color32)> = c
        .summary
        .totals
        .iter()
        .map(|(k, v)| (format!("{k}: {v}"), TEXT))
        .collect();
    list.push((String::new(), TEXT));
    list.extend(c.summary.lines.iter().map(|l| (l.clone(), EDITED)));
    for (i, (l, fg)) in list.iter().skip(c.scroll).take(body).enumerate() {
        g.put(
            y + 1 + i,
            x + 1,
            &cols::fit(l, left.saturating_sub(2), false),
            *fg,
        );
    }
    for i in 0..body {
        g.set(y + 1 + i, x + left, '│', SELECT_DIM);
    }
    let right = w.saturating_sub(left + 2);
    for (i, l) in c.yaml.lines().skip(c.scroll).take(body).enumerate() {
        g.put(y + 1 + i, x + left + 2, &cols::fit(l, right, false), TEXT);
    }
    let yy = y + 1 + body;
    let mark = |on: bool| if on { "(•) " } else { "( ) " };
    let mut xx = g.put(yy, x + 1, mark(c.mode == CommitMode::NewFile), ACCENT);
    xx = g.put(yy, xx, "новый файл    ", TEXT);
    xx = g.put(yy, xx, mark(c.mode == CommitMode::Overwrite), ACCENT);
    g.put(yy, xx, "перезаписать, исходник — в бэкап", TEXT);
    let label = match c.mode {
        CommitMode::NewFile => "имя нового файла: ",
        CommitMode::Overwrite => "имя бэкапа: ",
    };
    let q = g.put(yy + 1, x + 1, label, DIM);
    let end = g.put(yy + 1, q, c.name(), TEXT);
    if c.running.is_none() {
        g.shade(yy + 1, end, 1, ACCENT);
    }
    let (msg, fg) = match (&c.error, c.running) {
        (Some(_), _) => (String::new(), ERROR),
        (None, Some(p)) => (format!("пишу… {:.0}%", p * 100.0), OK),
        _ => (String::new(), DIM),
    };
    g.put(
        yy + 2,
        x + 1,
        &cols::fit(&msg, w.saturating_sub(2), false),
        fg,
    );
}

/// Выполнить действие щелчка.
pub fn apply(d: &mut Desk, a: Act) {
    match a {
        Act::Row(i) => {
            if d.sel == i && d.prompt.is_none() {
                d.key(Key::Enter);
            } else {
                d.select(i);
            }
        }
        Act::Scroll(f) => d.scroll_to(f),
        Act::Field(k) => {
            // Щелчок по выбранному полю — правка, как Enter.
            let again =
                d.focus == Focus::Detail && d.detail.as_ref().is_some_and(|t| t.cursor == k);
            if let Some(det) = &mut d.detail {
                det.cursor = k;
            }
            d.focus = Focus::Detail;
            if again && d.prompt.is_none() {
                d.edit_field();
            }
        }
        Act::Pick(k) => {
            if let Some(p) = &mut d.prompt {
                p.sel = k;
            }
            d.key(Key::Char(' '));
        }
        Act::Key(k) => d.key(k),
        // Границу тянут, а не щёлкают: см. [`split_at`]; сортировка — у списка.
        Act::Split | Act::Sort(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Границу детали тянут мышью: где отпустили — там и граница, доля — в
    /// пределах, что у `{`/`}`.
    /// `agr:detail-share`
    #[test]
    fn dragging_the_detail_border_sets_its_share() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/plain.dat");
        let Ok(file) = crate::data::File::open(&path) else {
            return;
        };
        let mut d = Desk::open(file, std::sync::Arc::new(|| {}));
        d.detail_open = true;
        let g = frame(&mut d, 80, 40);
        let border = (0..g.rows())
            .find(|&r| g.act_at(r, 5) == Some(Act::Split))
            .expect("граница");
        split_at(&mut d, 40, border - 10);
        assert!(
            d.detail_ratio > 0.3,
            "выше — деталь больше: {}",
            d.detail_ratio
        );
        let g = frame(&mut d, 80, 40);
        let moved = (0..g.rows())
            .find(|&r| g.act_at(r, 5) == Some(Act::Split))
            .unwrap();
        assert_eq!(moved, border - 10, "граница там, где отпустили");
        split_at(&mut d, 40, 0);
        assert_eq!(d.detail_ratio, 0.8);
        split_at(&mut d, 40, 100);
        assert_eq!(d.detail_ratio, 0.05);
    }

    /// Разряды числа отделяются пробелом.
    #[test]
    fn numbers_group_by_thousands() {
        assert_eq!(group(0), "0");
        assert_eq!(group(1234), "1 234");
        assert_eq!(group(17057150), "17 057 150");
    }
}
