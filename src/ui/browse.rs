//! Список линков resolve в окне (`agr:resolve-browse`): тот же [`Browse`], что
//! у TUI, нарисованный как таблица окна ([`super::table`]) — моноширинно и
//! только видимые строки, колонки — по ширине содержимого, как в сетке.
//! Шапка щёлкается (сортировка), строка — выбор, выбранная — открыть; колесо
//! листает. Клавиши — те же [`Key`], что у TUI: фильтр печатается прямо в список.
//!
//! Форма «Открыть линк» (Ctrl+O в списке, `o` в виде линка) — модальным
//! окном ([`form`], `agr:link-open`); клавиши ей дают [`Browse`] и `Desk`.
//!
//! Наружу: [`show`], [`form`].

use egui;

use super::cols::fit;
use super::theme::*;
use crate::browse::{Action, Browse, Col, FormField, Line, OpenForm};
use crate::desk::Key;
use crate::io::link::LinkState;

const BAR: f32 = 12.0;

/// Клавиши окна в [`Key`] списка.
fn keys(ui: &egui::Ui) -> Vec<Key> {
    let mut out = Vec::new();
    ui.input(|i| {
        for e in &i.events {
            match e {
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    let k = match key {
                        egui::Key::R if modifiers.command || modifiers.ctrl => Key::Ctrl('r'),
                        egui::Key::A if modifiers.command || modifiers.ctrl => Key::Ctrl('a'),
                        egui::Key::O if modifiers.command || modifiers.ctrl => Key::Ctrl('o'),
                        egui::Key::U if modifiers.command || modifiers.ctrl => Key::Ctrl('u'),
                        egui::Key::S if modifiers.command || modifiers.ctrl => Key::Ctrl('s'),
                        egui::Key::ArrowLeft => Key::Left,
                        egui::Key::ArrowRight => Key::Right,
                        egui::Key::Delete => Key::Delete,
                        egui::Key::ArrowUp => Key::Up,
                        egui::Key::ArrowDown => Key::Down,
                        egui::Key::PageUp => Key::PageUp,
                        egui::Key::PageDown => Key::PageDown,
                        egui::Key::Home => Key::Home,
                        egui::Key::End => Key::End,
                        egui::Key::Enter => Key::Enter,
                        egui::Key::Escape => Key::Esc,
                        egui::Key::Tab if modifiers.shift => Key::BackTab,
                        egui::Key::Tab => Key::Tab,
                        egui::Key::Backspace => Key::Backspace,
                        _ => continue,
                    };
                    out.push(k);
                }
                egui::Event::Text(t) if !i.modifiers.command && !i.modifiers.ctrl => {
                    out.extend(t.chars().map(Key::Char))
                }
                _ => {}
            }
        }
    });
    out
}

/// Цвет состояния сервера.
fn state_color(s: LinkState) -> egui::Color32 {
    match s {
        LinkState::Active => OK,
        LinkState::Error => ERROR,
        LinkState::Closed | LinkState::Closing => DIM,
        LinkState::Opening => NAME,
    }
}

/// Кадр списка; `wheel` — дробные строки колеса между кадрами.
pub(super) fn show(ui: &mut egui::Ui, b: &mut Browse, wheel: &mut f32) -> Option<Action> {
    b.poll();
    let mut action = None;
    for k in keys(ui) {
        if let Some(a) = b.key(k) {
            action = Some(a);
        }
    }
    let font = egui::FontId::monospace(13.0);
    let (cw, ch) = ui
        .ctx()
        .fonts_mut(|f| (f.glyph_width(&font, '0'), f.row_height(&font)));
    let row_h = (ch + 4.0).round();

    egui::Panel::top("browse-bar").show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Линки resolve").strong().color(TEXT));
            match &b.addr {
                Some(a) => {
                    ui.label(egui::RichText::new("сервер HOST:PORT:").color(ACCENT));
                    ui.label(egui::RichText::new(format!("{a}▏")).monospace().color(TEXT));
                }
                None => {
                    ui.label(egui::RichText::new("фильтр:").color(DIM));
                    ui.label(
                        egui::RichText::new(format!("{}▏", b.filter))
                            .monospace()
                            .color(TEXT),
                    );
                    if b.filter.is_empty() {
                        ui.label(
                            egui::RichText::new("печатайте: сервис, канал или сервис/канал")
                                .color(DIM),
                        );
                    }
                }
            }
        });
    });
    egui::Panel::bottom("browse-status").show(ui, |ui| {
        if let Some(e) = b.selected() {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(e.link()).monospace().color(TEXT));
                ui.label(egui::RichText::new(&e.url).monospace().color(DIM));
            });
        }
        ui.horizontal(|ui| {
            let fg = if b.note.is_some() {
                ERROR
            } else {
                state_color(b.state())
            };
            ui.label(egui::RichText::new(b.status()).color(fg));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // ⌘O забирает меню «Открыть…» — до списка доходит Ctrl+O.
                let hint = if b.back {
                    "Enter — открыть · Ctrl+O — url и параметры · ⌘S — служебные · Tab, ⌘R — сортировка · ⌘A — сервер · Esc — назад"
                } else {
                    "Enter — открыть · Ctrl+O — url и параметры · ⌘S — служебные · Tab, ⌘R — сортировка · ⌘A — сервер"
                };
                ui.label(egui::RichText::new(hint).color(DIM));
            });
        });
    });
    egui::CentralPanel::default().show(ui, |ui| {
        let rect = ui.available_rect_before_wrap();
        ui.allocate_rect(rect, egui::Sense::hover());
        let body = egui::Rect::from_min_max(
            rect.min + egui::vec2(0.0, row_h),
            egui::pos2(rect.right() - BAR, rect.bottom()),
        );
        b.height = ((body.height() / row_h).floor() as usize).max(1);
        if ui.rect_contains_pointer(rect) {
            *wheel += ui.input(|i| i.smooth_scroll_delta.y) / row_h;
            let lines = wheel.trunc();
            if lines != 0.0 {
                *wheel -= lines;
                b.scroll(-lines as isize);
            }
        }
        let avail = ((body.width() - 8.0) / cw).floor().max(10.0) as usize;
        let layout = crate::ui::tui::browse::layout(b, avail);
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

        // Шапка: щелчок — сортировка.
        let mut x = rect.left() + 4.0;
        for (c, w) in &layout {
            let mut t = c.title().to_string();
            if *c == b.sort {
                t.push(if b.asc { '▲' } else { '▼' });
            }
            let fg = if *c == b.sort { ACCENT } else { DIM };
            text(x, rect.top(), &fit(&t, *w, false), fg);
            let r = egui::Rect::from_min_size(
                egui::pos2(x, rect.top()),
                egui::vec2(*w as f32 * cw, row_h),
            );
            if ui
                .interact(
                    r,
                    ui.id().with(("browse-head", c.title())),
                    egui::Sense::click(),
                )
                .clicked()
            {
                b.sort_by(*c);
            }
            x += (*w as f32 + 1.0) * cw;
        }

        // Строки.
        let sel = b.sel_row();
        let rows: Vec<_> = b
            .rows()
            .iter()
            .skip(b.top)
            .take(b.height)
            .cloned()
            .collect();
        let mut clicked = None;
        for (i, e) in rows.iter().enumerate() {
            let idx = b.top + i;
            let y = body.top() + i as f32 * row_h;
            let r = egui::Rect::from_min_size(
                egui::pos2(rect.left(), y),
                egui::vec2(body.width(), row_h),
            );
            if Some(idx) == sel {
                p.rect_filled(r, 0.0, SELECT);
            } else if idx % 2 == 1 {
                p.rect_filled(r, 0.0, STRIPE);
            }
            let mut x = rect.left() + 4.0;
            for (c, w) in &layout {
                let fg = match c {
                    Col::Service => NAME,
                    Col::Channel => TEXT,
                    Col::Proto => ACCENT,
                    _ => DIM,
                };
                text(x, y, &fit(&c.cell(e), *w, false), fg);
                x += (*w as f32 + 1.0) * cw;
            }
            if ui
                .interact(r, ui.id().with(("browse-row", idx)), egui::Sense::click())
                .clicked()
            {
                clicked = Some(idx);
            }
        }
        if let Some(i) = clicked {
            if sel == Some(i) {
                action = b.key(Key::Enter).or(action.take());
            } else {
                b.select(i);
            }
        }
        if b.rows().is_empty() {
            let t = match (b.browser().is_some(), b.total()) {
                (false, _) => "введите адрес resolve-сервера: HOST:PORT и Enter",
                (true, 0) if b.state() == LinkState::Active => "сервер ничего не экспортирует",
                (true, 0) => "ждём список от сервера…",
                _ => "под фильтр ничего не подходит",
            };
            text(rect.left() + 12.0, body.top() + row_h, t, DIM);
        }
        if let Some(f) = &b.form {
            form(ui, f);
        }

        // Полоса прокрутки.
        let n = b.rows().len();
        if n > b.height {
            let track = egui::Rect::from_min_max(
                egui::pos2(body.right(), body.top()),
                egui::pos2(rect.right(), body.bottom()),
            );
            p.rect_filled(track, 0.0, SELECT_DIM);
            let h = track.height();
            let start = h * b.top as f32 / n as f32;
            let len = (h * b.height as f32 / n as f32).max(8.0);
            p.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(track.left() + 2.0, track.top() + start),
                    egui::vec2(BAR - 4.0, len.min(h - start)),
                ),
                3.0,
                DIM,
            );
        }
    });
    action
}

/// Поле формы текстом: курсор `▏` — в активном.
fn line_text(l: &Line, active: bool) -> String {
    let mut chars: Vec<char> = l.text.chars().collect();
    if active {
        chars.insert(l.cursor.min(chars.len()), '▏');
    }
    chars.into_iter().collect()
}

/// Форма «Открыть линк» модальным окном: url, параметры, подсказки. Ввод —
/// клавишами через [`Browse`] или `Desk`, здесь только показ.
pub(super) fn form(ui: &egui::Ui, f: &OpenForm) {
    egui::Modal::new(egui::Id::new("open-link")).show(ui.ctx(), |ui| {
        ui.set_width((ui.ctx().content_rect().width() * 0.6).clamp(420.0, 900.0));
        ui.heading("Открыть линк");
        egui::Grid::new("open-link-grid")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                for (name, l, field) in [
                    ("url", &f.url, FormField::Url),
                    ("open", &f.open, FormField::Open),
                ] {
                    let active = f.field == field;
                    let fg = if active { ACCENT } else { DIM };
                    ui.label(egui::RichText::new(name).color(fg));
                    let t = egui::RichText::new(line_text(l, active))
                        .monospace()
                        .color(TEXT);
                    let t = if active {
                        t.background_color(SELECT)
                    } else {
                        t
                    };
                    ui.add(egui::Label::new(t).wrap());
                    ui.end_row();
                }
            });
        ui.label(
            egui::RichText::new(
                "↑↓ в open: online · initial · seq;seq=N · block;block=N · история",
            )
            .color(DIM),
        );
        ui.label(
            egui::RichText::new("Tab — поле · Enter — открыть · Esc — назад · ⌘U — стереть поле")
                .color(DIM),
        );
    });
}
