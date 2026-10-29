//! Список линков resolve сеткой знаков ([`frame`]) — для терминала, вида TUI
//! в окне и браузера (`agr:resolve-browse`); щелчок — [`apply`]. Пока
//! выбранный линк не стал Active — его состояние на весь экран ([`waiting`],
//! `agr:link-state`).
//!
//! Экран сверху вниз: заголовок с сервером и его состоянием, строка фильтра
//! (или адреса сервера), шапка с ▲▼ у колонки сортировки — щелчок сортирует,
//! строки с полосой прокрутки, выбранный линк и url клиента, подсказка клавиш.
//!
//! Форма «Открыть линк» ([`draw_form`], `agr:link-open`) — рамкой поверх
//! списка (Ctrl+O) и поверх вида линка (`o`).
//!
//! Наружу: [`frame`], [`apply`], [`waiting`], [`draw_form`], [`layout`] (его же
//! берёт окно), `shown` — строка ввода с курсором, общая с формой отправки.

use super::grid::Grid;
use super::Act;
use crate::browse::{Action, Browse, Col, FormField, Line, OpenForm};
use crate::io::link::{LinkState, Status};
use crate::ui::cols::fit;
use crate::ui::theme::*;

/// Строк над таблицей и под ней: заголовок, фильтр, шапка; выбранный, подсказка.
const CHROME: usize = 5;

/// Ширины колонок: по содержимому видимых строк в пределах `max`; что не
/// влезло — не показываем, остаток — последней.
pub fn layout(b: &Browse, avail: usize) -> Vec<(Col, usize)> {
    let vis: Vec<_> = b.rows().iter().skip(b.top).take(b.height.max(1)).collect();
    let mut out = Vec::new();
    let mut used = 0;
    for c in Col::ALL {
        let max = match c {
            Col::Service | Col::Channel => 28,
            Col::Proto | Col::Addr => 24,
            Col::Host | Col::Tags | Col::Scheme => 18,
            Col::Params => usize::MAX,
        };
        let w = vis
            .iter()
            .map(|e| c.cell(e).chars().count())
            .chain([c.title().chars().count() + 1])
            .max()
            .unwrap_or(0)
            .min(max);
        let gap = usize::from(!out.is_empty());
        if used + gap + w > avail {
            let left = avail.saturating_sub(used + gap);
            if left >= 6 {
                out.push((c, left));
            }
            break;
        }
        used += gap + w;
        out.push((c, w));
    }
    out
}

/// Цвет состояния сервера или линка.
fn state_color(s: LinkState) -> egui::Color32 {
    match s {
        LinkState::Active => OK,
        LinkState::Error => ERROR,
        LinkState::Closed | LinkState::Closing => DIM,
        LinkState::Opening => NAME,
    }
}

/// Кадр списка в `cols`×`rows` знаков; задаёт высоту таблицы [`Browse::height`].
pub fn frame(b: &mut Browse, cols: usize, rows: usize) -> Grid<Act> {
    let mut g: Grid<Act> = Grid::new(cols);
    let rows = rows.max(CHROME + 1);
    let table = rows - CHROME;
    b.height = table.max(1);

    // Заголовок: что это и чей сервер.
    g.shade(0, 0, cols, PANEL);
    g.put(0, 0, "mutator — линки resolve", TEXT);
    if b.browser().is_some() {
        let s = b.snapshot();
        let t = format!("● {} {}", s.addr, s.state.name());
        g.put_right(0, cols.saturating_sub(1), &t, state_color(s.state));
    }

    // Фильтр или адрес сервера.
    match &b.addr {
        Some(a) => {
            let x = g.put(1, 0, "Сервер HOST:PORT: ", ACCENT);
            let x = g.put(1, x, a, TEXT);
            g.set(1, x, '▏', ACCENT);
        }
        None => {
            let x = g.put(1, 0, "Фильтр: ", DIM);
            let x = g.put(1, x, &b.filter, TEXT);
            g.set(1, x, '▏', ACCENT);
            if b.filter.is_empty() {
                g.put(1, x + 2, "сервис, канал или сервис/канал", DIM);
            }
        }
    }

    // Шапка.
    let width = cols.saturating_sub(1);
    let layout = layout(b, width);
    g.shade(2, 0, cols, PANEL);
    let mut x = 0;
    for (i, (c, w)) in layout.iter().enumerate() {
        let mut t = c.title().to_string();
        if *c == b.sort {
            t.push(if b.asc { '▲' } else { '▼' });
        }
        let fg = if *c == b.sort { ACCENT } else { DIM };
        g.put(2, x, &fit(&t, *w, false), fg);
        let ix = Col::ALL.iter().position(|k| k == c).unwrap_or(i);
        g.link(2, x, *w, Act::Sort(ix));
        x += w + 1;
    }

    // Строки.
    let sel = b.sel_row();
    let h = b.height;
    for i in 0..h {
        let y = 3 + i;
        let idx = b.top + i;
        let Some(e) = b.rows().get(idx) else { continue };
        if Some(idx) == sel {
            g.shade(y, 0, width, SELECT);
        }
        let mut x = 0;
        for (c, w) in &layout {
            let fg = match c {
                Col::Service => NAME,
                Col::Channel => TEXT,
                Col::Proto => ACCENT,
                _ => DIM,
            };
            g.put(y, x, &fit(&c.cell(e), *w, false), fg);
            x += w + 1;
        }
        g.link(y, 0, width, Act::Row(idx));
    }
    if b.rows().is_empty() {
        let t = match (b.browser().is_some(), b.total()) {
            (false, _) => "введите адрес resolve-сервера",
            (true, 0) if b.state() == LinkState::Active => "сервер ничего не экспортирует",
            (true, 0) => "ждём список от сервера…",
            _ => "под фильтр ничего не подходит",
        };
        g.put(4, 2, t, DIM);
    }
    scrollbar(&mut g, b, width, 3, h);

    // Выбранный линк и подсказка.
    let y = 3 + h;
    g.shade(y, 0, cols, PANEL);
    if let Some(e) = b.selected() {
        let x = g.put(y, 0, &e.link(), TEXT);
        g.put(y, x + 2, &e.url, DIM);
    }
    let y = y + 1;
    g.put_right(
        y,
        cols,
        "Enter открыть · ^O url · ^S служебные · Tab/^R сортировка · ^A сервер · Esc",
        DIM,
    );
    // Состояние важнее подсказки: поверх неё, если не влезли обе.
    let fg = if b.note.is_some() {
        ERROR
    } else {
        state_color(b.state())
    };
    g.put(y, 0, &b.status(), fg);
    if let Some(f) = &b.form {
        draw_form(&mut g, f, cols, rows);
    }
    g.reach_rows(rows);
    g
}

/// Строка формы шириной `w`: текст с курсором `▏`, окно сдвинуто так, чтобы
/// курсор был виден.
pub(super) fn shown(l: &Line, w: usize, active: bool) -> String {
    let mut chars: Vec<char> = l.text.chars().collect();
    if active {
        chars.insert(l.cursor.min(chars.len()), '▏');
    }
    let at = if active { l.cursor } else { 0 };
    let start = (at + 1).saturating_sub(w);
    chars.iter().skip(start).take(w).collect()
}

/// Форма «Открыть линк» рамкой посреди экрана.
pub fn draw_form(g: &mut Grid<Act>, f: &OpenForm, cols: usize, rows: usize) {
    let w = cols.saturating_sub(4).clamp(20, 100);
    let h = 7;
    let x = cols.saturating_sub(w) / 2;
    let y = (rows / 3).min(rows.saturating_sub(h));
    for r in 0..h {
        g.shade(y + r, x, w, PANEL);
        for k in 0..w {
            g.set(y + r, x + k, ' ', TEXT);
        }
    }
    g.put(y, x + 1, "Открыть линк", ACCENT);
    let field = |g: &mut Grid<Act>, row: usize, name: &str, l: &Line, active: bool| {
        let fg = if active { ACCENT } else { DIM };
        g.put(row, x + 1, name, fg);
        let at = x + 8;
        let room = (x + w).saturating_sub(at + 1);
        if active {
            g.shade(row, at, room, SELECT);
        }
        g.put(row, at, &shown(l, room, active), TEXT);
    };
    field(g, y + 2, "url:", &f.url, f.field == FormField::Url);
    field(g, y + 3, "open:", &f.open, f.field == FormField::Open);
    g.put(
        y + 4,
        x + 8,
        &fit(
            "↑↓: online · initial · seq;seq=N · block;block=N · история",
            w.saturating_sub(9),
            false,
        ),
        DIM,
    );
    g.put(
        y + 6,
        x + 1,
        &fit(
            "Tab — поле · Enter — открыть · Esc — назад · ^U — стереть",
            w.saturating_sub(2),
            false,
        ),
        DIM,
    );
}

fn scrollbar(g: &mut Grid<Act>, b: &Browse, x: usize, y: usize, h: usize) {
    let n = b.rows().len();
    if h < 2 || n <= h {
        return;
    }
    for i in 0..h {
        g.set(y + i, x, '│', SELECT_DIM);
        g.link(y + i, x, 1, Act::Scroll(i as f32 / (h - 1) as f32));
    }
    let start = (b.top * h / n).min(h - 1);
    let len = (h * h).div_ceil(n).clamp(1, h - start);
    for i in start..start + len {
        g.set(y + i, x, '█', DIM);
    }
}

/// Щелчок по кадру списка: строка — выбрать (выбранная — открыть), шапка —
/// сортировка, полоса — прокрутка.
pub fn apply(b: &mut Browse, a: Act) -> Option<Action> {
    match a {
        Act::Row(i) if b.sel_row() == Some(i) => b.key(crate::desk::Key::Enter),
        Act::Row(i) => {
            b.select(i);
            None
        }
        Act::Sort(i) => {
            b.sort_by(Col::ALL[i.min(Col::ALL.len() - 1)]);
            None
        }
        Act::Scroll(f) => {
            let max = b.rows().len().saturating_sub(b.height.max(1));
            b.top = (f * max as f32).round() as usize;
            None
        }
        Act::Key(k) => b.key(k),
        _ => None,
    }
}

/// Линк, выбранный из списка, ещё не Active: url, состояние, ошибка.
pub fn waiting(s: &Status, cols: usize, rows: usize) -> Grid<Act> {
    let mut g: Grid<Act> = Grid::new(cols);
    let rows = rows.max(6);
    g.shade(0, 0, cols, PANEL);
    g.put(0, 0, "mutator — открываю линк", TEXT);
    let mid = rows / 3;
    let center = |t: &str| cols.saturating_sub(t.chars().count()) / 2;
    g.put(mid, center(&s.url), &s.url, TEXT);
    let line = format!("● {} · {}", s.state.name(), s.open);
    g.put(mid + 2, center(&line), &line, state_color(s.state));
    if let Some(e) = &s.error {
        for (i, l) in e.lines().take(rows.saturating_sub(mid + 5)).enumerate() {
            g.put(mid + 4 + i, center(l).max(1), l, ERROR);
        }
    }
    g.put(rows - 1, 0, "Esc — к списку", DIM);
    g.reach_rows(rows);
    g
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browse::tests::sample;

    /// `agr:resolve-browse` — кадр списка: шапка с ▲ у колонки сортировки,
    /// строки по порядку, «N из M»; щелчок по шапке сортирует, по выбранной
    /// строке — открывает.
    #[test]
    fn list_frame_shows_and_sorts() {
        let mut b = sample();
        let g = frame(&mut b, 120, 16);
        assert_eq!(g.rows(), 16);
        let head = g.text(2);
        assert!(head.starts_with("сервис▲"), "{head}");
        assert!(head.contains("proto"), "{head}");
        assert!(g.text(3).starts_with("gateway"), "{}", g.text(3));
        assert!(g.text(15).contains("4 из 4"), "{}", g.text(15));
        assert!(g.text(14).starts_with("resolve://gateway/orders"));

        let col = head
            .find("proto")
            .map(|b| head[..b].chars().count())
            .unwrap();
        let a = g.act_at(2, col).expect("шапка — ссылка");
        assert_eq!(apply(&mut b, a), None);
        assert_eq!(b.sort, Col::Proto);
        let g = frame(&mut b, 120, 16);
        assert!(g.text(2).contains("proto▲"), "{}", g.text(2));
        assert!(g.text(3).starts_with("gateway quotes"), "{}", g.text(3));

        let row = g.act_at(4, 0).unwrap();
        assert_eq!(apply(&mut b, row), None, "первый щелчок — выбрать");
        assert_eq!(
            apply(&mut b, row),
            Some(Action::Open(
                "resolve://md/feed".into(),
                "mode=online".into()
            ))
        );
    }

    /// `agr:link-open` — форма Ctrl+O рамкой поверх списка: url, параметры,
    /// курсор в активном поле.
    #[test]
    fn form_is_drawn_over_the_list() {
        let mut b = sample();
        b.key(crate::desk::Key::Ctrl('o'));
        let g = frame(&mut b, 100, 24);
        let all: String = (0..g.rows()).map(|r| g.text(r) + "\n").collect();
        assert!(all.contains("Открыть линк"), "{all}");
        assert!(all.contains("url:   resolve://gateway/orders"), "{all}");
        assert!(all.contains("open:  mode=online▏"), "{all}");
    }

    /// `agr:link-state` — ожидание линка: url, состояние и ошибка видны.
    #[test]
    fn waiting_frame_shows_link_state() {
        let s = Status {
            url: "resolve://a/b".into(),
            open: "mode=online".into(),
            state: LinkState::Error,
            error: Some("нет сервиса".into()),
            ..Default::default()
        };
        let g = waiting(&s, 60, 12);
        let all: String = (0..g.rows()).map(|r| g.text(r) + "\n").collect();
        assert!(all.contains("resolve://a/b"), "{all}");
        assert!(all.contains("Error · mode=online"), "{all}");
        assert!(all.contains("нет сервиса"), "{all}");
    }
}
