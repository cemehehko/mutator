//! Деталь в окне: дерево полей выбранного сообщения и hex с подсветкой байт
//! текущего поля. Строка дерева — путь в стиле tll-patch (`agr:patch-path`):
//! Красные значения — правленые и изменённые мутатором
//! (`agr:transform-shadow`); в подсказке — значение из файла. Двойной щелчок
//! по полю — правка ([`edit_line`] — строка ввода значения).
//!
//! В окне правки флагов/enum значок и подпись — один виджет (галочка или
//! радиокнопка со своей подписью), поэтому они стоят в одной строке.
//!
//! Наружу: [`show`], [`edit_line`].

use egui;
use egui::text::{LayoutJob, TextFormat};

use super::theme::*;
use crate::desk::{Desk, EditKind, Focus, Key, PromptKind};

pub(super) fn show(ui: &mut egui::Ui, d: &mut Desk) {
    let Some(det) = &d.detail else {
        ui.label("нет выбранной строки");
        return;
    };
    let Some(row) = d.rows.iter().find(|r| r.seq == det.seq && r.ins == det.ins) else {
        return;
    };
    let font = egui::FontId::monospace(13.0);
    let lines = det.lines();
    let cursor = det.cursor;
    let cur = det.current().cloned();
    let data = row.data.clone();
    // Правки этого сообщения: путь → значение в файле.
    let edited: std::collections::HashMap<String, String> = d
        .edits
        .message(d.orig_seq(det.seq))
        .filter(|_| det.ins.is_none())
        .map(|m| {
            m.fields
                .iter()
                .map(|(p, e)| (p.clone(), e.old.clone()))
                .collect()
        })
        .unwrap_or_default();
    // Изменённое мутатором: путь → значение в файле, до него.
    let mutated: std::collections::BTreeMap<String, String> =
        d.mutations.get(&det.seq).cloned().unwrap_or_default();
    let seq_in_file = d.seq_in_file(row.seq);
    let mut clicked = None;
    let mut edit = false;
    ui.horizontal(|ui| {
        ui.colored_label(NAME, egui::RichText::new(&row.name).strong());
        // Состав потока: вставка, удалено, seq после записи (`agr:view-input-seq`).
        let seq = match (&row.tag, row.ins) {
            (Some(t), Some(_)) => format!("вставка перед {} → seq {}", row.seq, &t[1..]),
            (Some(t), None) => format!("seq {t}"),
            (None, _) => format!("seq {}", row.seq),
        };
        ui.label(egui::RichText::new(seq).color(DIM));
        // Мутатор поменял seq: рядом, красным, тот, что в файле.
        if let Some(orig) = seq_in_file {
            ui.label(egui::RichText::new(format!("(в файле {orig})")).color(EDITED));
        }
        ui.label(egui::RichText::new(format!("{} байт", row.data.len())).color(DIM));
        if let Some(n) = &cur {
            ui.separator();
            ui.label(egui::RichText::new(&n.path).monospace().color(ACCENT));
            if d.show_types {
                ui.label(
                    egui::RichText::new(format!("{}  @{}+{}", n.ty, n.offset, n.size)).color(DIM),
                );
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.checkbox(&mut d.show_hex, "hex").on_hover_text("x");
            ui.checkbox(&mut d.show_types, "типы")
                .on_hover_text("i — тип, смещение и размер поля");
        });
    });
    let row_h = ui.ctx().fonts_mut(|f| f.row_height(&font)) + 2.0;
    let focus = d.focus == Focus::Detail;
    let width = ui.available_width();
    let hex_w = 16usize;
    let cw = ui.ctx().fonts_mut(|f| f.glyph_width(&font, '0'));
    let hex_px = if d.show_hex {
        (6 + hex_w * 4) as f32 * cw + 24.0
    } else {
        0.0
    };
    let tree_px = (width - hex_px).max(200.0);
    // Значение выбранного поля, не влезшее в строку дерева, — целиком, с переносом.
    if let Some(n) = &cur {
        let (depth, _) = lines[cursor];
        let room = room_chars(tree_px, cw, depth, n, d.show_types);
        if n.text().chars().count() > room {
            egui::ScrollArea::vertical()
                .id_salt("value")
                .max_height(row_h * 4.0)
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(n.text()).monospace().color(TEXT))
                            .wrap(),
                    );
                });
            ui.separator();
        }
    }
    let (show_hex, show_types) = (d.show_hex, d.show_types);
    ui.horizontal_top(|ui| {
        let size = egui::vec2(tree_px, ui.available_height());
        ui.allocate_ui_with_layout(size, egui::Layout::top_down(egui::Align::Min), |ui| {
            let mut area = egui::ScrollArea::vertical()
                .id_salt("tree")
                .auto_shrink([false, false]);
            if d.detail_follow {
                area = area.vertical_scroll_offset(
                    (cursor as f32 * row_h - ui.available_height() / 2.0).max(0.0),
                );
            }
            area.show_rows(ui, row_h, lines.len(), |ui, range| {
                for k in range {
                    let (depth, n) = lines[k];
                    let mut job = LayoutJob::default();
                    let fmt = |c| TextFormat {
                        font_id: font.clone(),
                        color: c,
                        ..Default::default()
                    };
                    job.append(&" ".repeat(depth * 2), 0.0, fmt(DIM));
                    job.append(
                        &n.name,
                        0.0,
                        fmt(if n.error.is_some() { ERROR } else { NAME }),
                    );
                    let v = n.text();
                    if !v.is_empty() {
                        let room = room_chars(tree_px, cw, depth, n, show_types);
                        job.append(": ", 0.0, fmt(DIM));
                        job.append(
                            &crate::ui::cols::fit(&v, room.min(v.chars().count()), false),
                            0.0,
                            fmt(
                                if edited.contains_key(&n.path) || mutated.contains_key(&n.path) {
                                    EDITED
                                } else {
                                    TEXT
                                },
                            ),
                        );
                    }
                    if show_types {
                        job.append(
                            &format!("   {}  @{}+{}", n.ty, n.offset, n.size),
                            0.0,
                            fmt(DIM),
                        );
                    }
                    let sel = k == cursor;
                    let (rect, resp) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), row_h),
                        egui::Sense::click(),
                    );
                    if sel {
                        ui.painter().rect_filled(
                            rect,
                            0.0,
                            if focus { SELECT } else { SELECT_DIM },
                        );
                    }
                    let galley = ui.fonts_mut(|f| f.layout_job(job));
                    ui.painter()
                        .galley(rect.min + egui::vec2(4.0, 1.0), galley, TEXT);
                    let resp = resp.on_hover_ui(|ui| {
                        ui.set_max_width(600.0);
                        ui.label(egui::RichText::new(&n.path).monospace().color(ACCENT));
                        if !v.is_empty() {
                            ui.add(egui::Label::new(egui::RichText::new(&v).monospace()).wrap());
                        }
                        if let Some(old) = edited.get(&n.path) {
                            ui.label(
                                egui::RichText::new(format!("в файле: {old}  (u — вернуть)"))
                                    .monospace()
                                    .color(DIM),
                            );
                        }
                        if let Some(old) = mutated.get(&n.path) {
                            let t = if old.is_empty() {
                                "в файле: поле не было".to_string()
                            } else {
                                format!("в файле: {old}")
                            };
                            ui.label(egui::RichText::new(t).monospace().color(DIM));
                        }
                    });
                    if resp.clicked() || resp.double_clicked() {
                        clicked = Some(k);
                        edit |= resp.double_clicked();
                    }
                }
            });
        });
        if !show_hex {
            return;
        }
        ui.separator();
        let (from, to) = cur
            .as_ref()
            .map(|n| (n.offset, n.offset + n.size))
            .unwrap_or((0, 0));
        egui::ScrollArea::vertical()
            .id_salt("hex")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut job = LayoutJob::default();
                let fmt = |c, bg| TextFormat {
                    font_id: font.clone(),
                    color: c,
                    background: bg,
                    ..Default::default()
                };
                for off in (0..data.len()).step_by(hex_w) {
                    job.append(
                        &format!("{off:04x}  "),
                        0.0,
                        fmt(DIM, egui::Color32::TRANSPARENT),
                    );
                    for j in 0..hex_w {
                        let p = off + j;
                        match data.get(p) {
                            Some(b) => {
                                let on = p >= from && p < to;
                                job.append(
                                    &format!("{b:02x}"),
                                    0.0,
                                    fmt(
                                        if on { TEXT } else { DIM },
                                        if on {
                                            HILITE
                                        } else {
                                            egui::Color32::TRANSPARENT
                                        },
                                    ),
                                );
                                job.append(" ", 0.0, fmt(DIM, egui::Color32::TRANSPARENT));
                            }
                            None => job.append("   ", 0.0, fmt(DIM, egui::Color32::TRANSPARENT)),
                        }
                    }
                    job.append(" ", 0.0, fmt(DIM, egui::Color32::TRANSPARENT));
                    for j in 0..hex_w {
                        let p = off + j;
                        let Some(b) = data.get(p) else { break };
                        let on = p >= from && p < to;
                        let c = if b.is_ascii_graphic() {
                            *b as char
                        } else {
                            '·'
                        };
                        job.append(
                            &c.to_string(),
                            0.0,
                            fmt(
                                if on { TEXT } else { DIM },
                                if on {
                                    HILITE
                                } else {
                                    egui::Color32::TRANSPARENT
                                },
                            ),
                        );
                    }
                    job.append("\n", 0.0, fmt(DIM, egui::Color32::TRANSPARENT));
                }
                ui.label(job);
            });
    });
    d.detail_follow = false;
    if let Some(k) = clicked {
        if let Some(det) = &mut d.detail {
            det.cursor = k;
        }
        d.focus = Focus::Detail;
        if edit {
            d.edit_field();
        }
    }
}

/// Окно ввода нового значения поля — над тем же [`crate::desk::Prompt`], что
/// и строка ввода TUI.
pub(super) fn edit_line(ui: &mut egui::Ui, d: &mut Desk) {
    let Some(p) = &mut d.prompt else { return };
    let is_mass = matches!(p.kind, PromptKind::MassEdit { .. });
    if !matches!(
        p.kind,
        PromptKind::Edit { .. } | PromptKind::MassEdit { .. }
    ) {
        return;
    }
    let is_enum = matches!(
        p.kind,
        PromptKind::Edit {
            kind: EditKind::Enum,
            ..
        } | PromptKind::MassEdit {
            kind: EditKind::Enum,
            ..
        }
    );
    let is_bits = matches!(
        p.kind,
        PromptKind::Edit {
            kind: EditKind::Bits,
            ..
        } | PromptKind::MassEdit {
            kind: EditKind::Bits,
            ..
        }
    );
    let has_items = !p.items.is_empty();
    let title = if is_mass {
        format!("Массовая правка {}", p.path)
    } else {
        format!("Правка {}", p.path)
    };
    let mut submit = false;
    let mut cancel = false;
    let hint = if is_enum {
        "имя из списка или десятичное число"
    } else if is_bits {
        "hex-число или флаги из списка"
    } else {
        "значение проверяется записью по схеме; enum — имя, bits — a|b"
    };
    let mut width: f32 = 480.0;
    if has_items {
        width = width.max(640.0);
    }
    let screen = ui.ctx().content_rect().height();
    let row_h = 22.0;
    // Список — по числу пунктов, но не выше экрана (заголовок, поле, кнопки ~120px).
    let list_h = if has_items {
        let need = p.items.len() as f32 * row_h + 8.0;
        need.min((screen * 0.7).max(160.0)).max(120.0)
    } else {
        0.0
    };
    let current = p.items.iter().position(|it| it.on).unwrap_or(0);
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ui.ctx(), |ui| {
            ui.set_width(width);
            if has_items {
                ui.columns(2, |cols| {
                    let ui = &mut cols[0];
                    let label = if is_enum {
                        "Значения"
                    } else {
                        "Флаги"
                    };
                    ui.label(egui::RichText::new(label).color(DIM));
                    let mut area = egui::ScrollArea::vertical()
                        .max_height(list_h)
                        .auto_shrink([false, true]);
                    if is_enum {
                        area = area.vertical_scroll_offset(
                            (current as f32 * row_h - list_h / 2.0).max(0.0),
                        );
                    }
                    area.show(ui, |ui| {
                        let vis: Vec<(usize, bool, String)> = p
                            .items
                            .iter()
                            .enumerate()
                            .map(|(i, it)| {
                                (i, it.on, {
                                    if let Some(v) = it.value {
                                        if is_bits {
                                            format!("{} ({:#x})", it.label, v)
                                        } else {
                                            format!("{} ({})", it.label, v)
                                        }
                                    } else {
                                        it.label.clone()
                                    }
                                })
                            })
                            .collect();
                        for (idx, on, label) in &vis {
                            let mut on = *on;
                            // Подпись — в самой галочке: отдельным `label`
                            // она встала бы следующей строкой.
                            let fg = if p.items[*idx].on { ACCENT } else { TEXT };
                            let text = egui::RichText::new(label).color(fg).monospace();
                            let r = if is_enum {
                                ui.radio(on, text)
                            } else {
                                ui.checkbox(&mut on, text)
                            };
                            if r.clicked() {
                                p.items[*idx].on = !p.items[*idx].on;
                                if is_enum {
                                    // Один выбор: снять все, поставить этот.
                                    for (j, item) in p.items.iter_mut().enumerate() {
                                        item.on = j == *idx;
                                    }
                                    if let Some(v) = p.items[*idx].value {
                                        // Enum: всегда десятичное число
                                        p.text = v.to_string();
                                        p.cursor = p.text.chars().count();
                                    }
                                } else {
                                    // Биты: пересчитать raw по маскам
                                    let raw: u64 = p
                                        .items
                                        .iter()
                                        .filter(|it| it.on)
                                        .filter_map(|it| it.value)
                                        .fold(0u64, |acc, m| acc | (m as u64));
                                    p.text = format!("{raw:#x}");
                                    p.cursor = p.text.chars().count();
                                }
                            }
                        }
                        if vis.is_empty() && !p.text.is_empty() {
                            ui.label(egui::RichText::new("нет совпадений").color(DIM));
                        }
                    });

                    // Справа: поле ввода числа.
                    let ui = &mut cols[1];
                    let value_label = if is_bits { "Hex" } else { "Значение" };
                    ui.label(
                        egui::RichText::new(if is_enum { "Код" } else { value_label }).color(DIM),
                    );
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut p.text)
                            .desired_width(180.0)
                            .font(egui::TextStyle::Monospace),
                    );
                    // Enter и Esc считаются, только когда поле их само получило.
                    submit = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    cancel = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape));
                    if !r.has_focus() && !submit && !cancel {
                        r.request_focus();
                    }
                });
            } else {
                let r = ui.add(
                    egui::TextEdit::singleline(&mut p.text)
                        .desired_width(480.0)
                        .font(egui::TextStyle::Monospace),
                );
                // Enter и Esc считаются, только когда поле их само получило.
                submit = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                cancel = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape));
                if !r.has_focus() && !submit && !cancel {
                    r.request_focus();
                }
            }
            ui.horizontal(|ui| {
                submit |= ui.button("Принять (Enter)").clicked();
                cancel |= ui.button("Отмена (Esc)").clicked();
            });
            ui.label(egui::RichText::new(hint).color(DIM));
        });
    if submit || cancel {
        if let Some(p) = &mut d.prompt {
            p.cursor = p.text.chars().count();
        }
        d.key(if submit { Key::Enter } else { Key::Esc });
    }
}

/// Сколько знаков значения влезает в строку дерева шириной `px`: без отступа,
/// имени, «: », типа (если показан) и полей.
fn room_chars(px: f32, cw: f32, depth: usize, n: &crate::decode::Node, types: bool) -> usize {
    let meta = if types {
        format!("   {}  @{}+{}", n.ty, n.offset, n.size)
            .chars()
            .count()
    } else {
        0
    };
    let used = depth * 2 + n.name.chars().count() + 2 + 2 + meta;
    ((px / cw) as usize).saturating_sub(used).max(8)
}
