//! Диалог подтверждения массовой правки: поле, значения, количество затронутых
//! сообщений и разбивка по типам. Над тем же [`crate::desk::MassEdit`], что и в TUI.
//!
//! Наружу: [`show`].

use egui;

use super::theme::*;
use crate::desk::Desk;

/// Окно подтверждения массовой правки.
pub(super) fn show(ui: &mut egui::Ui, d: &mut Desk) {
    let Some(me) = &d.mass_edit else { return };
    let scanning = d.scan_pending;
    let has = !me.pairs.is_empty();
    // Сводка по типам.
    let mut by_type: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for (_, name) in &me.pairs {
        *by_type.entry(name.as_str()).or_default() += 1;
    }
    let mut ok = false;
    let mut close = false;
    egui::Window::new("Массовая правка")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ui.ctx(), |ui| {
            ui.set_width(500.0);
            ui.label(
                egui::RichText::new(format!("Поле: {}", me.path))
                    .monospace()
                    .color(ACCENT),
            );
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&me.old_value).monospace().color(DIM));
                ui.label(egui::RichText::new("→").color(DIM));
                ui.label(egui::RichText::new(&me.new_value).monospace().color(EDITED));
            });
            ui.separator();
            if scanning {
                ui.spinner();
                ui.label("подсчитываю сообщения…");
            } else if has {
                ui.label(
                    egui::RichText::new(format!(
                        "Затронуто сообщений: {}  ({})",
                        me.count,
                        by_type.len()
                    ))
                    .color(TEXT),
                );
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(200.0)
                    .show(ui, |ui| {
                        for (ty, n) in &by_type {
                            ui.label(
                                egui::RichText::new(format!("  {ty}: {n}"))
                                    .monospace()
                                    .color(DIM),
                            );
                        }
                    });
            } else {
                ui.label("нет сообщений для правки");
            }
            ui.separator();
            ui.add_enabled_ui(!scanning && has, |ui| {
                ui.horizontal(|ui| {
                    ok = ui.button("Подтвердить (Enter)").clicked();
                    close = ui.button("Отмена (Esc)").clicked();
                });
            });
        });
    if ok {
        d.apply_mass_edit();
    } else if close {
        d.mass_edit = None;
        d.mass_pairs = None;
        d.status = "массовая правка отменена".into();
    }
}
