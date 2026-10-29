//! Диалог фиксации правок в окне: слева — сводка и итоги, справа — те же
//! правки yaml-командами tll-patch, снизу — новый файл или перезапись с
//! бэкапом и имя. Над тем же [`crate::desk::Commit`], что и наложение в TUI.
//!
//! Наружу: [`show`].

use egui;

use super::theme::*;
use crate::desk::{CommitMode, Desk, Key};

pub(super) fn show(ui: &mut egui::Ui, d: &mut Desk) {
    let Some(c) = &mut d.commit else { return };
    let mut start = false;
    let mut close = false;
    let running = c.running;
    egui::Modal::new(egui::Id::new("commit")).show(ui.ctx(), |ui| {
        ui.set_width((ui.ctx().content_rect().width() * 0.8).min(1200.0));
        ui.heading(format!("Зафиксировать правки — {}", d.file.name));
        // Первая строка — прогноз последнего seq или ошибка записи
        // (`agr:seq-forecast`, `agr:commit-seq-check`).
        let head_fg = match (&c.error, &c.forecast) {
            (Some(_), _) => ERROR,
            (None, Some(f)) if f.changes() => EDITED,
            _ => TEXT,
        };
        ui.label(egui::RichText::new(c.headline()).strong().color(head_fg));
        ui.separator();
        let h = (ui.ctx().content_rect().height() * 0.55).max(200.0);
        ui.columns(2, |cols| {
            let ui = &mut cols[0];
            egui::Grid::new("totals").num_columns(2).show(ui, |ui| {
                for (k, v) in &c.summary.totals {
                    ui.label(egui::RichText::new(k).color(DIM));
                    ui.label(egui::RichText::new(v).monospace().color(TEXT));
                    ui.end_row();
                }
            });
            ui.separator();
            egui::ScrollArea::vertical()
                .id_salt("summary")
                .max_height(h)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for l in &c.summary.lines {
                        ui.label(egui::RichText::new(l).monospace().color(EDITED));
                    }
                });
            let ui = &mut cols[1];
            ui.label(egui::RichText::new("правки yaml").color(DIM));
            egui::ScrollArea::both()
                .id_salt("yaml")
                .max_height(h)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(&c.yaml).monospace().color(TEXT))
                            .extend(),
                    );
                });
        });
        ui.separator();
        ui.add_enabled_ui(running.is_none(), |ui| {
            ui.horizontal(|ui| {
                ui.radio_value(&mut c.mode, CommitMode::NewFile, "Новый файл");
                ui.radio_value(
                    &mut c.mode,
                    CommitMode::Overwrite,
                    "Перезаписать, исходник — в бэкап",
                );
            });
            ui.horizontal(|ui| {
                ui.label(match c.mode {
                    CommitMode::NewFile => "Имя нового файла",
                    CommitMode::Overwrite => "Имя бэкапа",
                });
                let r = ui.add(
                    egui::TextEdit::singleline(c.name_mut())
                        .desired_width(f32::INFINITY)
                        .font(egui::TextStyle::Monospace),
                );
                if r.changed() {
                    c.error = None;
                }
                start |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            });
        });

        if let Some(p) = running {
            ui.add(egui::ProgressBar::new(p).text("пишу…"));
        }
        ui.horizontal(|ui| {
            ui.add_enabled_ui(running.is_none(), |ui| {
                start |= ui.button("Зафиксировать").clicked();
                close |= ui.button("Отмена").clicked();
            });
        });
    });
    if start {
        d.start_commit();
    } else if close {
        d.key(Key::Esc);
    }
}
