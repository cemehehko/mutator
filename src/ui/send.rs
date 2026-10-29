//! Форма «Отправить в линк» в окне (`agr:link-post`): модальное окно поверх
//! вида — тип, seq и все поля типа таблицей с прокруткой. Ввод — клавишами
//! через `Desk` ([`crate::send::SendForm::key`]), здесь только показ.
//!
//! Наружу: [`show`].

use egui;

use super::theme::*;
use crate::browse::Line;
use crate::send::SendForm;

/// Поле текстом: курсор `▏` — в активном.
fn line_text(l: &Line, active: bool) -> String {
    let mut chars: Vec<char> = l.text.chars().collect();
    if active {
        chars.insert(l.cursor.min(chars.len()), '▏');
    }
    chars.into_iter().collect()
}

/// Значение строкой: активное — на подложке выбора.
fn value(ui: &mut egui::Ui, l: &Line, active: bool) {
    let t = egui::RichText::new(line_text(l, active))
        .monospace()
        .color(TEXT);
    let t = if active {
        t.background_color(SELECT)
    } else {
        t
    };
    ui.add(egui::Label::new(t).wrap());
}

/// Форма модальным окном.
pub(super) fn show(ui: &egui::Ui, f: &SendForm) {
    egui::Modal::new(egui::Id::new("send-link")).show(ui.ctx(), |ui| {
        let rect = ui.ctx().content_rect();
        ui.set_width((rect.width() * 0.7).clamp(480.0, 1100.0));
        ui.heading("Отправить в линк");
        egui::Grid::new("send-head")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                let fg = |a: bool| if a { ACCENT } else { DIM };
                ui.label(egui::RichText::new("тип").color(fg(f.sel == 0)));
                let t = egui::RichText::new(format!("◂ {} ▸", f.name()))
                    .monospace()
                    .color(TEXT);
                let t = if f.sel == 0 { t.background_color(SELECT) } else { t };
                ui.label(t);
                ui.end_row();
                ui.label(egui::RichText::new("seq").color(fg(f.sel == 1)));
                value(ui, &f.seq, f.sel == 1);
                ui.end_row();
            });
        ui.separator();
        egui::ScrollArea::vertical()
            .max_height(rect.height() * 0.6)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                egui::Grid::new("send-fields")
                    .num_columns(3)
                    .striped(true)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        for (i, p) in f.fields.iter().enumerate() {
                            let active = f.sel == i + 2;
                            let path = egui::RichText::new(&p.path)
                                .monospace()
                                .color(if active { ACCENT } else { NAME });
                            let r = ui.label(path);
                            if active {
                                r.scroll_to_me(None);
                            }
                            ui.label(egui::RichText::new(&p.ty).monospace().color(DIM));
                            value(ui, &p.value, active);
                            ui.end_row();
                        }
                    });
            });
        ui.separator();
        match &f.error {
            Some(e) => ui.label(egui::RichText::new(e).color(ERROR)),
            None => ui.label(
                egui::RichText::new(
                    "↑↓ — поле · ←→ на типе — другой тип · Enter — отправить · Esc — назад · ⌘U — стереть поле",
                )
                .color(DIM),
            ),
        };
    });
}
