//! Список диалогов: создание, выбор, удаление. В окне — панель слева на
//! широком экране, иначе строка в шапке; в TUI — строка шапки.
//!
//! Наружу: [`DialogAct`], [`apply`], [`row`], [`settings_row`], [`panels`].

use egui;

use super::{Act, State};
use crate::agent::{Cmd, CmdTx};
use crate::ui::theme::*;
use crate::ui::tui::grid::Grid;

/// Ширина экрана, с которой список диалогов уходит в панель слева.
const WIDE: f32 = 1000.0;

/// Действие щелчка по списку диалогов.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum DialogAct {
    /// Переключить активный диалог (номер в [`State::dialogs`]).
    Pick(usize),
    /// Закрыть диалог (номер в [`State::dialogs`]).
    Close(usize),
    /// Завести новый диалог.
    New,
}

/// Применить действие списка: команды уходят воркеру.
pub(super) fn apply(state: &State, cmd: &CmdTx, a: DialogAct) {
    match a {
        DialogAct::Pick(i) => {
            if let Some(id) = state.dialogs.get(i).map(|d| d.id.clone()) {
                let _ = cmd.send(Cmd::PickDialog { id });
            }
        }
        DialogAct::Close(i) => {
            if let Some(id) = state.dialogs.get(i).map(|d| d.id.clone()) {
                let _ = cmd.send(Cmd::CloseDialog { id });
            }
        }
        DialogAct::New => {
            let _ = cmd.send(Cmd::NewDialog);
        }
    }
}

/// Строка диалогов в TUI: активный в скобках, щелчок выбирает.
pub(super) fn row(g: &mut Grid<Act>, state: &State, cols: usize) {
    if state.dialogs.is_empty() {
        g.put(0, 1, "Нет диалогов — задайте вопрос", DIM);
        return;
    }
    let mut x = 1;
    for (i, d) in state.dialogs.iter().enumerate() {
        if d.closed {
            continue;
        }
        let active = state.active.as_deref() == Some(d.id.as_str());
        let title = if active {
            format!("[{}]", d.title)
        } else {
            d.title.clone()
        };
        let n = title.chars().count();
        if x + n + 2 >= cols {
            break;
        }
        let fg = if active { ACCENT } else { TEXT };
        if active {
            g.shade(0, x, n + 2, SELECT);
        }
        x = g.put(0, x + 1, &title, fg);
        x = g.put(0, x, " ", TEXT);
        g.link(0, x - n - 1, n + 1, Act::Pick(i));
    }
    // Правый край: контекст.
    if let Some(c) = state.context {
        let label = format!(
            "{} {}",
            c.0,
            if c.1 {
                "токенов"
            } else {
                "≈ токенов"
            }
        );
        g.put_right(0, cols.saturating_sub(1), &label, DIM);
    }
}

/// Строка настроек в TUI: ссылка и подсказка, справа — ошибка.
pub(super) fn settings_row(g: &mut Grid<Act>, state: &State, cols: usize) {
    g.put(1, 1, "настройки", ACCENT);
    g.link(1, 1, 8, Act::Settings);
    let hint = "отправить — Enter";
    let hint_at = cols.saturating_sub(hint.chars().count() + 1);
    g.put(1, hint_at, hint, DIM);
    let e = state.error.as_deref().unwrap_or("");
    if !e.is_empty() {
        let shown = crate::ui::cols::fit(e, cols.saturating_sub(12), false);
        g.put(1, 11, &shown, ERROR);
    }
}

/// Диалоги и шапка в egui: панель диалогов слева на широком экране, иначе
/// строка в шапке. Панели создаёт на верхнем уровне — вкладывать их в egui
/// нельзя, поэтому зовёт её [`super::show`], а не шапка.
pub(super) fn panels(ui: &mut egui::Ui, state: &mut State, cmd: &CmdTx) {
    let wide = ui.ctx().input(|i| i.viewport_rect().width()) >= WIDE;
    if wide {
        egui::Panel::left("agent-dialogs")
            .resizable(true)
            .default_size(220.0)
            .show(ui, |ui| list(ui, state, cmd));
    }
    egui::Panel::top("agent-bar").show(ui, |ui| {
        ui.horizontal(|ui| {
            if !wide && ui.button("+ Диалог").clicked() {
                apply(state, cmd, DialogAct::New);
            }
            if let Some(c) = state.context {
                ui.label(
                    egui::RichText::new(format!(
                        "контекст: {} {}",
                        c.0,
                        if c.1 {
                            "токенов"
                        } else {
                            "≈ токенов"
                        }
                    ))
                    .color(DIM),
                );
            }
            if ui
                .button("Настройки")
                .on_hover_text("адрес API, ключ, модель")
                .clicked()
            {
                state.settings_draft = Some(state.settings.clone());
            }
        });
        if !wide {
            ui.horizontal_wrapped(|ui| {
                titles(ui, state, cmd);
            });
        }
    });
}

/// Список диалогов панелью слева: новый, выбор, удаление.
fn list(ui: &mut egui::Ui, state: &State, cmd: &CmdTx) {
    ui.vertical(|ui| {
        if ui.button("+ Новый диалог").clicked() {
            let _ = cmd.send(Cmd::NewDialog);
        }
        ui.separator();
        titles(ui, state, cmd);
    });
}

/// Строки диалогов: выбор щелчком, удаление крестиком.
fn titles(ui: &mut egui::Ui, state: &State, cmd: &CmdTx) {
    for (i, d) in state.dialogs.iter().enumerate() {
        if d.closed {
            continue;
        }
        let is_active = state.active.as_deref() == Some(d.id.as_str());
        ui.horizontal(|ui| {
            if ui.selectable_label(is_active, &d.title).clicked() && !is_active {
                apply(state, cmd, DialogAct::Pick(i));
            }
            if ui
                .small_button("×")
                .on_hover_text("закрыть диалог")
                .clicked()
            {
                apply(state, cmd, DialogAct::Close(i));
            }
        });
    }
}
