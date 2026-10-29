//! Форма «Отправить в линк» сеткой знаков ([`draw`], `agr:link-post`) — рамкой
//! поверх вида линка (`s`): в терминале, виде TUI окна и браузере. Сверху тип
//! (←→ меняют) и seq, ниже — все поля типа: путь, тип, значение; выбранное
//! поле — с курсором. Внизу — отказ кодировщика или канала, а нет его —
//! подсказка клавиш.
//!
//! Наружу: [`draw`].

use super::browse::shown;
use super::grid::Grid;
use super::Act;
use crate::send::SendForm;
use crate::ui::cols::fit;
use crate::ui::theme::*;

/// Форма рамкой посреди экрана; окно полей прокручивается к выбранному.
pub fn draw(g: &mut Grid<Act>, f: &mut SendForm, cols: usize, rows: usize) {
    let w = cols.saturating_sub(4).clamp(30, 120);
    let h = rows.saturating_sub(2).clamp(8, f.rows() + 6);
    let x = cols.saturating_sub(w) / 2;
    let y = rows.saturating_sub(h) / 2;
    for r in 0..h {
        g.shade(y + r, x, w, PANEL);
        for k in 0..w {
            g.set(y + r, x + k, ' ', TEXT);
        }
    }
    g.put(y, x + 1, "Отправить в линк", ACCENT);
    let label = 8;
    let at = x + 1 + label;
    let room = (x + w).saturating_sub(at + 1);
    let head = |g: &mut Grid<Act>, row: usize, name: &str, active: bool| {
        g.put(row, x + 1, name, if active { ACCENT } else { DIM });
        if active {
            g.shade(row, at, room, SELECT);
        }
    };
    head(g, y + 2, "тип:", f.sel == 0);
    g.put(
        y + 2,
        at,
        &fit(&format!("◂ {} ▸", f.name()), room, false),
        TEXT,
    );
    head(g, y + 3, "seq:", f.sel == 1);
    g.put(y + 3, at, &shown(&f.seq, room, f.sel == 1), TEXT);

    let body = h.saturating_sub(7);
    f.scroll_to(body);
    let path_w = f
        .fields
        .iter()
        .map(|p| p.path.chars().count())
        .max()
        .unwrap_or(0)
        .min(w / 2);
    let ty_w = 14.min(w / 6);
    for (i, field) in f.fields.iter().enumerate().skip(f.top).take(body) {
        let row = y + 5 + i - f.top;
        let active = f.sel == i + 2;
        let fg = if active { ACCENT } else { NAME };
        g.put(row, x + 1, &fit(&field.path, path_w, false), fg);
        let tx = x + 2 + path_w;
        g.put(row, tx, &fit(&field.ty, ty_w, false), DIM);
        let vx = tx + ty_w + 1;
        let vroom = (x + w).saturating_sub(vx + 1);
        if active {
            g.shade(row, vx, vroom, SELECT);
        }
        g.put(row, vx, &shown(&field.value, vroom, active), TEXT);
    }
    let (text, fg) = match &f.error {
        Some(e) => (e.lines().next().unwrap_or("").to_string(), ERROR),
        None => (
            format!(
                "↑↓ поле ({} из {}) · ←→ тип · Enter — отправить · Esc — назад · ^U — стереть",
                f.sel.saturating_sub(1).min(f.fields.len()),
                f.fields.len()
            ),
            DIM,
        ),
    };
    g.put(
        y + h - 1,
        x + 1,
        &fit(&text, w.saturating_sub(2), false),
        fg,
    );
}
