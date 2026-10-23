//! Окно подтверждения пачки правок: одна сводка, одно решение, файл агент сам
//! не пишет (`agr:agent-no-write`, `agr:agent-batch`).
//!
//! Наружу: [`box_tui`], [`window`].

use egui;

use super::{Act, State};
use crate::agent::Cmd;
use crate::agent::CmdTx;
use crate::ui::theme::{ACCENT, DIM, PANEL, SELECT, TEXT};
use crate::ui::tui::grid::Grid;

/// Итоги пачки одной строкой: «сообщений: 2 · полей: 4».
fn totals(c: &super::Confirm) -> String {
    c.summary
        .totals
        .iter()
        .take(2)
        .map(|(k, v)| format!("{k}: {v}"))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Окно в TUI: итог, начало сводки, Enter/Esc (и y/n).
pub(super) fn box_tui(g: &mut Grid<Act>, state: &State, cols: usize, rows: usize) {
    let c = state.confirm.as_ref().unwrap();
    let shown = c.summary.lines.len().min(6);
    let more = c.summary.lines.len() - shown;
    // Заголовок, итог, сводка (плюс строка «и ещё»), подсказка, отступ.
    let h = 3 + shown + usize::from(more > 0) + 1;
    let w = (cols * 2 / 3).clamp(30, 78).min(cols);
    let x = (cols - w) / 2;
    let y = (rows - h) / 2;
    for r in 0..h {
        g.shade(y + r, x, w, PANEL);
    }
    g.put(y, x + 1, "Подтвердить правки", ACCENT);
    g.put(y + 1, x + 1, &totals(c), TEXT);
    for (i, line) in c.summary.lines.iter().take(shown).enumerate() {
        g.put(
            y + 2 + i,
            x + 1,
            &crate::ui::cols::fit(line, w.saturating_sub(2), false),
            DIM,
        );
    }
    if more > 0 {
        g.put(y + 2 + shown, x + 1, &format!("…и ещё {more}"), DIM);
    }
    let hint = y + h - 2;
    g.shade(hint, x, w, SELECT);
    g.put(
        hint,
        x + 1,
        "Enter/y — подтвердить  Esc/n — отклонить",
        TEXT,
    );
    g.put(y + h - 1, x + 1, "  ", TEXT);
    g.link(hint, x, w, Act::Confirm);
}

/// Окно в egui: итог, вся сводка, кнопки «Подтвердить» и «Отклонить».
pub(super) fn window(ui: &mut egui::Ui, state: &mut State, cmd: &CmdTx) {
    let Some(c) = state.confirm.clone() else {
        return;
    };
    let mut ok = false;
    let mut no = false;
    egui::Window::new("Подтвердить правки")
        .collapsible(false)
        .show(ui.ctx(), |ui| {
            ui.label(egui::RichText::new(&c.tool).color(ACCENT));
            ui.label(egui::RichText::new(totals(&c)).color(TEXT));
            egui::ScrollArea::vertical()
                .max_height(240.0)
                .show(ui, |ui| {
                    for line in &c.summary.lines {
                        ui.label(egui::RichText::new(line).monospace().color(TEXT));
                    }
                });
            ui.horizontal(|ui| {
                ok = ui.button("Подтвердить").clicked();
                no = ui.button("Отклонить").clicked();
            });
        });
    if ok {
        let _ = cmd.send(Cmd::Confirm);
    } else if no {
        let _ = cmd.send(Cmd::Deny);
    }
    if ok || no {
        state.confirm = None;
    }
}
