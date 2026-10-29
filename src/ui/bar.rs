//! Панель окна: фильтр (Lua как у tll-read), типы, колонки и пресеты, поиск в
//! обе стороны, переход. В окне строки ввода — поля egui; клавиши `f`, `/`,
//! `g` только ставят в них фокус. Выбор типов и колонок — окно поверх таблицы
//! над тем же [`Prompt`](crate::desk::Prompt), что и в TUI. Есть правки —
//! кнопка «Зафиксировать».
//!
//! Наружу: [`Field`], [`Bar`], [`picker`].

use egui;

use super::theme::*;
use crate::desk::{Desk, Key, PromptKind};
use crate::lua::Filter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Field {
    Filter,
    Search,
    Goto,
}

pub(super) struct Bar {
    filter: String,
    search: String,
    goto: String,
    pub(super) focus: Option<Field>,
}

impl Bar {
    pub(super) fn new(d: &Desk) -> Bar {
        Bar {
            filter: d.filter.expr.clone(),
            search: String::new(),
            goto: String::new(),
            focus: None,
        }
    }

    fn field(&mut self, ui: &mut egui::Ui, which: Field, width: f32, hint: &str) -> bool {
        let text = match which {
            Field::Filter => &mut self.filter,
            Field::Search => &mut self.search,
            Field::Goto => &mut self.goto,
        };
        let r = ui.add(
            egui::TextEdit::singleline(text)
                .desired_width(width)
                .hint_text(hint)
                .font(egui::TextStyle::Monospace),
        );
        if self.focus == Some(which) {
            r.request_focus();
            self.focus = None;
        }
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            ui.memory_mut(|m| m.surrender_focus(r.id));
        }
        enter
    }

    /// Нарисовать панель; `true` — переключиться на вид TUI.
    pub(super) fn show(&mut self, ui: &mut egui::Ui, d: &mut Desk) -> bool {
        let mut tui = false;
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(super::tui::title(d))
                    .monospace()
                    .color(TEXT),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                tui = ui
                    .button("TUI (F2)")
                    .on_hover_text("тот же экран сеткой знаков")
                    .clicked();
                if !d.lua_code.is_empty() {
                    let label = if d.lua_enabled {
                        "Lua: вкл"
                    } else {
                        "Lua: выкл"
                    };
                    if ui
                        .button(egui::RichText::new(label).color(if d.lua_enabled {
                            OK
                        } else {
                            DIM
                        }))
                        .on_hover_text(
                            "включить/выключить transform-код, не стирая его (правка — F4)",
                        )
                        .clicked()
                    {
                        d.toggle_transform();
                    }
                }
                if !d.edits.is_empty()
                    && ui
                        .button(
                            egui::RichText::new(format!("Зафиксировать ({})", d.edits.count()))
                                .color(EDITED),
                        )
                        .on_hover_text("w — записать правки в новый файл")
                        .clicked()
                {
                    d.open_commit();
                }
                if d.index.is_none() {
                    ui.add(
                        egui::ProgressBar::new(d.index_progress)
                            .desired_width(120.0)
                            .text("индекс"),
                    );
                }
                if let Some(m) = &d.matches {
                    if !m.is_done() {
                        ui.add(
                            egui::ProgressBar::new(m.progress())
                                .desired_width(120.0)
                                .text("фильтр"),
                        );
                    }
                }
            });
        });
        ui.horizontal(|ui| {
            ui.label("Фильтр");
            let w = (ui.available_width() * 0.4).max(240.0);
            if self.field(
                ui,
                Field::Filter,
                w,
                "Lua: name == 'NewOrder' and data.header.status == 'OK'",
            ) {
                d.set_filter(Filter {
                    messages: d.filter.messages.clone(),
                    expr: self.filter.trim().to_string(),
                });
            }
            if !d.filter.expr.is_empty()
                && ui
                    .small_button("✕")
                    .on_hover_text("снять выражение")
                    .clicked()
            {
                self.filter.clear();
                d.set_filter(Filter {
                    messages: d.filter.messages.clone(),
                    expr: String::new(),
                });
            }
            let types = if d.filter.messages.is_empty() {
                "Типы: все".to_string()
            } else {
                format!("Типы: {}", d.filter.messages.len())
            };
            if ui.button(types).clicked() {
                d.key(Key::Char('t'));
            }
            ui.separator();
            if ui.button("Колонки").clicked() {
                d.key(Key::Char('c'));
            }
            let current = d
                .presets
                .get(d.preset)
                .map(|p| p.name.clone())
                .unwrap_or_default();
            let mut pick = None;
            egui::ComboBox::from_id_salt("preset")
                .selected_text(current)
                .show_ui(ui, |ui| {
                    for (i, p) in d.presets.iter().enumerate() {
                        if ui.selectable_label(i == d.preset, &p.name).clicked() {
                            pick = Some(i);
                        }
                    }
                });
            if let Some(i) = pick {
                d.use_preset(i);
            }
            ui.separator();
            ui.label("Поиск");
            let enter = self.field(ui, Field::Search, 220.0, "Lua-выражение");
            let back = ui.small_button("◀").on_hover_text("назад (?, N)").clicked();
            let fwd = ui
                .small_button("▶")
                .on_hover_text("вперёд (/, n)")
                .clicked()
                || enter;
            if (back || fwd) && !self.search.trim().is_empty() {
                d.search(Filter::expr(self.search.trim()), back);
            }
            ui.separator();
            ui.label("Перейти");
            if self.field(ui, Field::Goto, 90.0, "seq, +N, -N") {
                d.goto(self.goto.trim());
            }
        });
        tui
    }
}

/// Окно выбора типов или колонок.
pub(super) fn picker(ui: &mut egui::Ui, d: &mut Desk) {
    let Some(p) = &d.prompt else { return };
    if !matches!(p.kind, PromptKind::Types | PromptKind::Columns) {
        return;
    }
    let kind = p.kind;
    let mut toggled = None;
    let mut close = false;
    egui::Window::new(p.title())
        .collapsible(false)
        .default_size([420.0, 520.0])
        .show(ui.ctx(), |ui| {
            let p = d.prompt.as_mut().unwrap();
            ui.add(
                egui::TextEdit::singleline(&mut p.text)
                    .hint_text("найти")
                    .desired_width(f32::INFINITY),
            );
            egui::ScrollArea::vertical()
                .max_height(440.0)
                .show(ui, |ui| {
                    for (i, item) in p.visible() {
                        let mut on = item.on;
                        ui.horizontal(|ui| {
                            if ui.checkbox(&mut on, &item.label).changed() {
                                toggled = Some(i);
                            }
                            ui.label(egui::RichText::new(&item.hint).color(DIM));
                        });
                    }
                });
            ui.horizontal(|ui| {
                close = ui
                    .button(if kind == PromptKind::Types {
                        "Применить"
                    } else {
                        "Закрыть"
                    })
                    .clicked();
                if kind == PromptKind::Types && ui.button("Все").clicked() {
                    for it in &mut p.items {
                        it.on = false;
                    }
                    close = true;
                }
            });
        });
    if let Some(i) = toggled {
        let p = d.prompt.as_mut().unwrap();
        p.items[i].on = !p.items[i].on;
        if kind == PromptKind::Columns {
            let path = p.items[i].label.clone();
            d.toggle_column(&path);
        }
    }
    if close {
        d.key(Key::Esc);
    }
}
