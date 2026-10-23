//! Таблица окна: рисуется сама, моноширинно и только видимые строки — цена
//! кадра не зависит от размера файла. Колонки — [`super::cols::layout`] в
//! знаках, как в TUI. Справа полоса прокрутки по [`Desk::scroll_fraction`]:
//! пока нет индекса — по seq, потом по номеру строки. Красные: ячейки и seq
//! правленых сообщений, а также изменённых мутатором (`agr:transform-shadow`).
//!
//! Наружу: [`show`].

use egui;

use super::cols::{self, ColKind};
use super::theme::*;
use crate::desk::{Desk, Focus, Key};

const BAR: f32 = 12.0;

pub(super) fn show(ui: &mut egui::Ui, d: &mut Desk, wheel: &mut f32) {
    let font = egui::FontId::monospace(13.0);
    let (cw, ch) = ui
        .ctx()
        .fonts_mut(|f| (f.glyph_width(&font, '0'), f.row_height(&font)));
    let row_h = (ch + 4.0).round();
    let rect = ui.available_rect_before_wrap();
    ui.allocate_rect(rect, egui::Sense::hover());
    let body = egui::Rect::from_min_max(
        rect.min + egui::vec2(0.0, row_h),
        egui::pos2(rect.right() - BAR, rect.bottom()),
    );
    d.set_height((body.height() / row_h).floor() as usize);

    // Колесо: дробные строки копятся.
    if ui.rect_contains_pointer(rect) {
        *wheel += ui.input(|i| i.smooth_scroll_delta.y) / row_h;
        let lines = wheel.trunc();
        if lines != 0.0 {
            *wheel -= lines;
            d.scroll(-lines as isize);
        }
    }

    let avail = ((body.width() - 8.0) / cw).floor().max(10.0) as usize;
    let (layout, more) = cols::layout(d, avail);
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 0.0, BG);
    p.rect_filled(
        egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), row_h)),
        0.0,
        PANEL,
    );
    let text = |x: f32, y: f32, s: &str, c: egui::Color32| {
        p.text(
            egui::pos2(x, y + 2.0),
            egui::Align2::LEFT_TOP,
            s,
            font.clone(),
            c,
        );
    };
    let mut x = rect.left() + 4.0;
    for c in &layout {
        text(x, rect.top(), &cols::fit(&c.title, c.width, c.right), DIM);
        x += (c.width + 1) as f32 * cw;
    }
    if more || d.hscroll > 0 {
        let mark = if d.hscroll > 0 && more {
            "◂ ▸"
        } else if more {
            "▸"
        } else {
            "◂"
        };
        p.text(
            egui::pos2(body.right() - 4.0, rect.top() + 2.0),
            egui::Align2::RIGHT_TOP,
            mark,
            font.clone(),
            ACCENT,
        );
    }

    for i in 0..d.height {
        let idx = d.top + i;
        let Some(r) = d.rows.get(idx) else { break };
        let y = body.top() + i as f32 * row_h;
        let line =
            egui::Rect::from_min_size(egui::pos2(body.left(), y), egui::vec2(body.width(), row_h));
        if idx == d.sel {
            p.rect_filled(
                line,
                0.0,
                if d.focus == Focus::Table {
                    SELECT
                } else {
                    SELECT_DIM
                },
            );
        } else if (d.first_row.unwrap_or(0) + idx) % 2 == 1 {
            p.rect_filled(line, 0.0, STRIPE);
        }
        let mut x = body.left() + 4.0;
        for c in &layout {
            let fg = match c.kind {
                ColKind::Seq if d.edited_row(r.seq) || d.mutated_row(r.seq) => EDITED,
                ColKind::Seq => DIM,
                ColKind::Name => NAME,
                ColKind::Field(i)
                    if d.edited(r.seq, &d.columns[i])
                        || d.mutated(r.seq, &d.columns[i]).is_some() =>
                {
                    EDITED
                }
                ColKind::Field(_) => TEXT,
            };
            text(
                x,
                y,
                &cols::fit(&cols::cell(r, c.kind), c.width, c.right),
                fg,
            );
            x += (c.width + 1) as f32 * cw;
        }
    }
    if d.rows.is_empty() {
        let msg = if d.idle() {
            "нет сообщений"
        } else {
            "читаю…"
        };
        text(body.left() + 8.0, body.top() + 8.0, msg, DIM);
    }

    // Полоса прокрутки.
    let track = egui::Rect::from_min_max(egui::pos2(rect.right() - BAR, body.top()), rect.max);
    p.rect_filled(track, 0.0, PANEL);
    if let Some((at, size)) = d.scroll_fraction() {
        let h = (size * track.height()).max(16.0).min(track.height());
        let top = track.top() + at * (track.height() - h);
        p.rect_filled(
            egui::Rect::from_min_size(
                egui::pos2(track.left() + 2.0, top),
                egui::vec2(BAR - 4.0, h),
            ),
            4.0,
            DIM,
        );
    }

    // Полоса: щелчок или отпускание после перетаскивания — переход по доле.
    let bar = ui.interact(
        track,
        ui.id().with("scrollbar"),
        egui::Sense::click_and_drag(),
    );
    if bar.clicked() || bar.drag_stopped() {
        if let Some(pos) = bar
            .interact_pointer_pos()
            .or_else(|| ui.input(|i| i.pointer.latest_pos()))
        {
            d.scroll_to((pos.y - track.top()) / track.height());
        }
    }
    // Строки: щелчок — выбор, двойной — деталь.
    let rows = ui.interact(body, ui.id().with("rows"), egui::Sense::click());
    if rows.clicked() || rows.double_clicked() {
        if let Some(pos) = rows.interact_pointer_pos() {
            let i = ((pos.y - body.top()) / row_h) as usize + d.top;
            d.focus = Focus::Table;
            d.select(i);
            if rows.double_clicked() {
                d.key(Key::Enter);
            }
        }
    }
}
