//! Настройки подключения к модели: адрес OpenAI-совместимого API, ключ,
//! модель, прокси; проверка подключения и список моделей.
//!
//! В TUI окно правится прямо в нём ([`key_tui`]): адрес, ключ, модель, прокси;
//! в окне egui — форма [`window`].
//!
//! Наружу: [`box_tui`], [`key_tui`], [`FIELDS`], [`window`].

use egui;

use super::{Act, State};
use crate::agent::Settings;
use crate::agent::{Cmd, CmdTx};
use crate::desk::Key;
use crate::ui::theme::{ACCENT, DIM, ERROR, OK, PANEL, TEXT};
use crate::ui::tui::grid::Grid;

/// Поля, которые правятся в TUI, по порядку обхода Tab.
pub(super) const FIELDS: [&str; 4] = ["адрес ", "ключ  ", "модель", "прокси"];

/// Значение поля черновика.
fn field_mut(d: &mut Settings, i: usize) -> &mut String {
    match i {
        0 => &mut d.base_url,
        1 => &mut d.api_key,
        2 => &mut d.model,
        _ => &mut d.proxy,
    }
}

/// Ключ на экране: хвост из четырёх знаков, остальное скрыто.
fn masked(key: &str) -> String {
    let n = key.chars().count();
    if n == 0 {
        return String::new();
    }
    let tail: String = key.chars().skip(n.saturating_sub(4)).collect();
    format!("{}{tail}", "•".repeat(n.saturating_sub(4).min(12)))
}

/// Клавиша в окне настроек TUI: Tab/↑/↓ — поле, знаки и забой — правка,
/// Ctrl+U — очистить поле, Ctrl+T — проверить подключение, Ctrl+L — список
/// моделей, Enter — сохранить, Esc — отмена. Окно модальное: всегда `true`,
/// кроме Ctrl+A (прерывание хода работает и под окном).
pub(super) fn key_tui(state: &mut State, cmd: &CmdTx, k: Key) -> bool {
    let n = FIELDS.len();
    let f = state.settings_field.min(n - 1);
    match k {
        Key::Ctrl('a') => return false,
        Key::Esc => state.settings_draft = None,
        Key::Enter => {
            if let Some(d) = state.settings_draft.take() {
                let _ = cmd.send(Cmd::SaveSettings { settings: d });
            }
        }
        Key::Tab | Key::Down => state.settings_field = (f + 1) % n,
        Key::BackTab | Key::Up => state.settings_field = (f + n - 1) % n,
        Key::Ctrl('t') => {
            if let Some(d) = &state.settings_draft {
                state.checked = None;
                let _ = cmd.send(Cmd::CheckSettings {
                    settings: d.clone(),
                });
            }
        }
        Key::Ctrl('l') => {
            if let Some(d) = &state.settings_draft {
                state.models = None;
                let _ = cmd.send(Cmd::ListModels {
                    settings: d.clone(),
                });
            }
        }
        Key::Ctrl('u') => {
            if let Some(d) = state.settings_draft.as_mut() {
                field_mut(d, f).clear();
            }
        }
        Key::Backspace => {
            if let Some(d) = state.settings_draft.as_mut() {
                field_mut(d, f).pop();
            }
        }
        Key::Char(c) => {
            if let Some(d) = state.settings_draft.as_mut() {
                field_mut(d, f).push(c);
            }
        }
        _ => {}
    }
    true
}

/// Окно настроек в TUI: поля правятся прямо в нём (см. [`key_tui`]).
pub(super) fn box_tui(g: &mut Grid<Act>, state: &State, cols: usize, rows: usize) {
    let draft = state.settings_draft.as_ref().unwrap();
    let focus = state.settings_field.min(FIELDS.len() - 1);
    let values = [
        draft.base_url.clone(),
        masked(&draft.api_key),
        draft.model.clone(),
        draft.proxy.clone(),
    ];
    // Ответ проверки и список моделей — ниже полей, сколько влезет.
    let mut extra: Vec<(String, egui::Color32)> = Vec::new();
    match &state.checked {
        Some(Ok(s)) => {
            extra.push((format!("подключено: {s}"), OK));
            if state.tools_ok == Some(false) {
                extra.push((
                    "⚠ tools не поддержаны: агент не вызовет инструменты".into(),
                    ERROR,
                ));
            }
        }
        Some(Err(e)) => extra.push((e.clone(), ERROR)),
        None => {}
    }
    match &state.models {
        Some(Ok(ms)) => {
            let room = rows.saturating_sub(FIELDS.len() + extra.len() + 8).max(1);
            for m in ms.iter().take(room) {
                extra.push((format!("  {m}"), TEXT));
            }
            if ms.len() > room {
                extra.push((format!("  … ещё {}", ms.len() - room), DIM));
            }
        }
        Some(Err(e)) => extra.push((e.clone(), ERROR)),
        None => {}
    }
    let h = (FIELDS.len() + extra.len() + 4).min(rows);
    let w = (cols * 3 / 4).clamp(40, 100).min(cols);
    let x = (cols - w) / 2;
    let y = (rows - h) / 2;
    for r in 0..h {
        g.shade(y + r, x, w, PANEL);
    }
    g.put(y, x + 1, "Настройки подключения", ACCENT);
    for (i, label) in FIELDS.iter().enumerate() {
        let row = y + 1 + i;
        let mark = if i == focus { "▸" } else { " " };
        let head = format!("{mark} {label}: ");
        let hx = g.put(row, x + 1, &head, if i == focus { ACCENT } else { DIM });
        let room = (x + w).saturating_sub(hx + 1);
        let v = &values[i];
        // Правка идёт в конец значения: если не влезло — показываем хвост.
        let skip = v.chars().count().saturating_sub(room.saturating_sub(1));
        let shown: String = v.chars().skip(skip).collect();
        let vx = g.put(row, hx, &shown, TEXT);
        if i == focus {
            g.shade(row, vx, 1, ACCENT);
        }
    }
    for (i, (l, c)) in extra.iter().enumerate() {
        let row = y + 2 + FIELDS.len() + i;
        if row + 2 > y + h {
            break;
        }
        g.put(
            row,
            x + 1,
            &crate::ui::cols::fit(l, w.saturating_sub(2), false),
            *c,
        );
    }
    g.put(
        y + h - 1,
        x + 1,
        &crate::ui::cols::fit(
            "Tab — поле · Enter — сохранить · Ctrl+T — проверить · Ctrl+L — модели · Esc — отмена",
            w.saturating_sub(2),
            false,
        ),
        DIM,
    );
    g.link(y + h - 1, x, w, Act::Back);
}

/// Форма в egui: поля, проверка, список моделей, сохранение.
pub(super) fn window(ui: &mut egui::Ui, state: &mut State, cmd: &CmdTx) {
    if state.settings_draft.is_none() {
        return;
    }
    let mut save = false;
    let mut close = false;
    let mut check = false;
    let mut list = false;
    {
        let draft = state.settings_draft.as_mut().unwrap();
        egui::Window::new("Настройки подключения")
            .collapsible(false)
            .show(ui.ctx(), |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut draft.base_url)
                        .hint_text("адрес OpenAI-совместимого API")
                        .desired_width(f32::INFINITY),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut draft.api_key)
                        .hint_text("ключ API")
                        .password(true)
                        .desired_width(f32::INFINITY),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut draft.model)
                        .hint_text("модель")
                        .desired_width(f32::INFINITY),
                );
                ui.horizontal(|ui| {
                    ui.label("Прокси");
                    ui.add(
                        egui::TextEdit::singleline(&mut draft.proxy)
                            .hint_text("host:port, пусто — напрямую")
                            .desired_width(200.0),
                    );
                });
                ui.checkbox(&mut draft.debug, "Показывать трассировку хода");
                ui.horizontal(|ui| {
                    check = ui.button("Проверить").clicked();
                    list = ui.button("Модели").clicked();
                    save = ui.button("Сохранить").clicked();
                    close = ui.button("Отмена").clicked();
                });
                if let Some(r) = &state.checked {
                    match r {
                        Ok(s) => {
                            ui.colored_label(OK, format!("подключено: {s}"));
                            if state.tools_ok == Some(false) {
                                ui.colored_label(
                                    ERROR,
                                    "⚠ tools не поддержаны: агент не сможет вызывать инструменты",
                                );
                            }
                        }
                        Err(e) => {
                            ui.colored_label(ERROR, e);
                        }
                    };
                }
                if let Some(r) = &state.models {
                    match r {
                        Ok(models) => {
                            for m in models {
                                ui.label(egui::RichText::new(m).monospace().color(TEXT));
                            }
                        }
                        Err(e) => {
                            ui.colored_label(ERROR, e);
                        }
                    };
                }
            });
        if check {
            let _ = cmd.send(Cmd::CheckSettings {
                settings: draft.clone(),
            });
        }
        if list {
            let _ = cmd.send(Cmd::ListModels {
                settings: draft.clone(),
            });
        }
        if save {
            let _ = cmd.send(Cmd::SaveSettings {
                settings: draft.clone(),
            });
        }
    }
    if save || close {
        state.settings_draft = None;
    }
}
