//! Экран редактора Lua-кода (transform-режим) — Gui и TUI.
//!
//! Gui: многострочный [`egui::TextEdit`] с подсветкой синтаксиса, номера строк
//! слева. Tui: сетка [`Grid`] с раскрашенными токенами, редактирование
//! Desk-клавишами.
//!
//! Наружу: [`gui`], [`tui`], [`tui_key`], [`apply_code`], [`gutter`],
//! [`xy_cursor`].

use egui;

use crate::desk::{Desk, Key};
use crate::lua_highlight;
use crate::ui::theme::*;
use crate::ui::tui::grid::Grid;

/// Экран редактора в Gui: TextEdit с подсветкой + номера строк + подсказка.
/// Код применяется при любом уходе с экрана ([`apply_code`]) — кнопки нет.
///
/// `agr:gui-lua-text` — панель номеров строк берёт ровно свою ширину
/// (`allocate_exact_size` по ширине галлеи), а не `max_rect`: иначе поле ввода
/// схлопывается и код не виден.
pub fn gui(ui: &mut egui::Ui, desk: &mut Desk) {
    let title = if desk.lua_code.is_empty() {
        "Lua transform (F4) — новый код"
    } else {
        "Lua transform (F4) — правка кода"
    };

    egui::Panel::top("lua-editor-title").show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(title).monospace().color(ACCENT));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("Сбросить")
                    .on_hover_text("вернуть стандартный код")
                    .clicked()
                {
                    desk.lua_reset_confirm = true;
                }
                ui.label(
                    egui::RichText::new("Esc/F4 — применить и назад · сброс — в дефолт")
                        .small()
                        .color(DIM),
                );
            });
        });
    });

    // Подтверждение сброса в дефолт.
    if desk.lua_reset_confirm {
        egui::Modal::new(egui::Id::new("lua-reset-confirm")).show(ui.ctx(), |ui| {
            ui.set_width(360.0);
            ui.heading("Сбросить код в дефолт?");
            ui.label("Текущий код Lua будет заменён стандартным и не сохранён.");
            ui.horizontal(|ui| {
                if ui.button("Сбросить").clicked() {
                    desk.reset_lua_to_default();
                }
                if ui.button("Отмена").clicked() {
                    desk.lua_reset_confirm = false;
                }
            });
        });
    }

    let row_h = ui.fonts_mut(|f| f.row_height(&egui::TextStyle::Monospace.resolve(ui.style())));

    egui::CentralPanel::default().show(ui, |ui| {
        ui.horizontal_top(|ui| {
            // Номера строк слева — узкая колонка по ширине самого длинного номера.
            let lines = desk.lua_code.lines().count().max(1);
            let line_w = format!(" {} ", lines).len();
            let gutter_text: String = (0..lines)
                .map(|i| format!(" {:>line_w$} ", i + 1))
                .collect::<Vec<_>>()
                .join("\n");

            let font =
                egui::FontId::monospace(ui.style().text_styles[&egui::TextStyle::Monospace].size);
            let gutter_galley = ui.painter().layout_no_wrap(gutter_text, font.clone(), DIM);
            let gutter_w = gutter_galley.size().x + 4.0;
            let (gutter_rect, _) = ui.allocate_exact_size(
                egui::vec2(gutter_w, ui.available_height()),
                egui::Sense::hover(),
            );
            ui.painter().galley(gutter_rect.min, gutter_galley, DIM);

            // Основное поле ввода с подсветкой — занимает оставшуюся ширину.
            let mut layouter = |ui: &egui::Ui,
                                text: &dyn egui::widgets::text_edit::TextBuffer,
                                _wrap_width: f32| {
                let job = lua_highlight::highlight_lua(ui, text.as_str());
                ui.fonts_mut(|f| f.layout_job(job))
            };
            ui.add(
                egui::TextEdit::multiline(&mut desk.lua_code)
                    .code_editor()
                    .desired_width(ui.available_width())
                    .desired_rows(ui.available_height() as usize / row_h as usize)
                    .font(egui::TextStyle::Monospace)
                    .layouter(&mut layouter),
            );
        });
    });

    egui::Panel::bottom("lua-editor-help").show(ui, |ui| {
        ui.label(
            egui::RichText::new("Esc/F4 — применить и вернуться к таблице")
                .small()
                .color(DIM),
        );
    });
}

/// Экран редактора в TUI: сетка, раскрашенная синтаксисом, с курсором.
pub fn tui(desk: &mut Desk, cols: usize, rows: usize) -> Grid<usize> {
    let mut g: Grid<usize> = Grid::new(cols);
    let lua_code = &desk.lua_code;

    // Заголовок.
    let title = if lua_code.is_empty() {
        " Lua transform — новый код (Esc — применить и назад)"
    } else {
        " Lua transform — правка (Esc — применить и назад)"
    };
    g.shade(0, 0, cols, PANEL);
    g.put(0, 0, &crate::ui::cols::fit(title, cols, false), ACCENT);

    // Строка статуса снизу.
    g.shade(rows - 1, 0, cols, PANEL);
    if desk.lua_reset_confirm {
        g.put(
            rows - 1,
            0,
            " Сбросить код в дефолт? y/Enter — да · n/Esc — нет",
            ERROR,
        );
    } else {
        g.put(
            rows - 1,
            0,
            " Esc — применить и назад · ← → ↑ ↓ Home End · Ctrl-R — сброс в дефолт",
            DIM,
        );
    }

    // Тело: раскрашенный код, с прокруткой по курсору.
    let highlighted = lua_highlight::highlight_lines(lua_code);
    let body_rows = rows.saturating_sub(2);
    desk.lua_page = body_rows.max(1);

    // Курсор держим в границах кода и в кадре.
    desk.lua_cursor = desk.lua_cursor.min(lua_code.chars().count());
    let (crow, ccol) = cursor_xy(lua_code, desk.lua_cursor);
    if crow < desk.lua_scroll {
        desk.lua_scroll = crow;
    } else if crow >= desk.lua_scroll + body_rows {
        desk.lua_scroll = crow + 1 - body_rows;
    }
    let scroll = desk.lua_scroll.min(highlighted.len().saturating_sub(1));

    // Номера строк слева: ширина по самому большому номеру файла.
    let gutter = gutter(highlighted.len());
    for (i, tokens) in highlighted.iter().skip(scroll).take(body_rows).enumerate() {
        let n = i + scroll + 1;
        g.put(i + 1, 0, &format!("{:>w$}", n, w = gutter - 1), DIM);
        let mut x = gutter;
        for (text, color) in tokens {
            x = g.put(i + 1, x, text, *color);
            if x >= cols {
                break;
            }
        }
        // Щелчок по строке ставит курсор в столбец клика.
        g.link(i + 1, 0, cols, i + scroll);
    }
    // Курсор: подложка под знаком, где он стоит (инверсия).
    let cursor_visible_row = crow.saturating_sub(scroll);
    if cursor_visible_row < body_rows {
        g.shade(cursor_visible_row + 1, ccol + gutter, 1, ACCENT);
    }
    g.reach_rows(rows);
    g
}

/// Ширина поля номеров строк: номер + пробел (курсор и щелчок сдвигаются на неё).
pub(crate) fn gutter(total_lines: usize) -> usize {
    total_lines.max(1).to_string().len() + 1
}

/// (строка, столбец) курсора: строка — номер с нуля, столбец — знак в строке.
fn cursor_xy(code: &str, cursor: usize) -> (usize, usize) {
    let mut row = 0;
    let mut col = 0;
    for (i, ch) in code.chars().enumerate() {
        if i == cursor {
            break;
        }
        if ch == '\n' {
            row += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    (row, col)
}

/// Индекс знака в `code` по (строка, столбец): столбец прижимается к длине.
pub(crate) fn xy_cursor(code: &str, row: usize, col: usize) -> usize {
    let mut at = 0;
    for (r, line) in code.split('\n').enumerate() {
        if r == row {
            return at + line.chars().take(col).count();
        }
        at += line.chars().count() + 1; // + '\n'
    }
    code.chars().count()
}

/// Обработать клавишу в TUI-редакторе: Enter — перенос строки, Esc — применить
/// и вернуться к таблице, остальное — навигация и правка по курсору.
/// Возвращает `false`, когда пора вернуться к таблице.
/// Применить код из редактора: пустой — снять transform. Зовёт любой уход
/// с экрана F4 — переключение всегда принимает изменения.
pub(crate) fn apply_code(desk: &mut Desk) {
    if desk.lua_code.trim().is_empty() {
        desk.clear_transform();
    } else {
        let code = desk.lua_code.clone();
        desk.set_transform(code);
    }
}

pub fn tui_key(desk: &mut Desk, k: Key) -> bool {
    // Подтверждение сброса: y/Enter — сбросить, n/Esc — отмена.
    if desk.lua_reset_confirm {
        return match k {
            Key::Char('y') | Key::Enter => {
                desk.reset_lua_to_default();
                true
            }
            Key::Char('n') | Key::Esc => {
                desk.lua_reset_confirm = false;
                true
            }
            _ => true,
        };
    }
    match k {
        // Enter — перенос строки, как в обычном редакторе; применение — при
        // любом уходе с экрана.
        Key::Enter | Key::Char('\n') => {
            let mut chars: Vec<char> = desk.lua_code.chars().collect();
            let idx = desk.lua_cursor.min(chars.len());
            chars.insert(idx, '\n');
            desk.lua_code = chars.into_iter().collect();
            desk.lua_cursor = idx + 1;
            true
        }
        Key::Esc => {
            apply_code(desk);
            false // применить и вернуться к таблице
        }
        Key::Char('\u{F1}') => true,
        Key::Ctrl('r') => {
            desk.lua_reset_confirm = true;
            true
        }
        Key::Left => {
            if desk.lua_cursor > 0 {
                desk.lua_cursor -= 1;
            }
            true
        }
        Key::Right => {
            let n = desk.lua_code.chars().count();
            if desk.lua_cursor < n {
                desk.lua_cursor += 1;
            }
            true
        }
        Key::Up => {
            let (row, col) = cursor_xy(&desk.lua_code, desk.lua_cursor);
            if row > 0 {
                desk.lua_cursor = xy_cursor(&desk.lua_code, row - 1, col);
            }
            true
        }
        Key::Down => {
            let (row, col) = cursor_xy(&desk.lua_code, desk.lua_cursor);
            desk.lua_cursor = xy_cursor(&desk.lua_code, row + 1, col);
            true
        }
        Key::PageUp => {
            let (row, col) = cursor_xy(&desk.lua_code, desk.lua_cursor);
            let page = desk.lua_page.max(1);
            desk.lua_cursor = xy_cursor(&desk.lua_code, row.saturating_sub(page), col);
            true
        }
        Key::PageDown => {
            let (row, col) = cursor_xy(&desk.lua_code, desk.lua_cursor);
            let page = desk.lua_page.max(1);
            desk.lua_cursor = xy_cursor(&desk.lua_code, row + page, col);
            true
        }
        Key::Home | Key::Ctrl('a') => {
            let (row, _) = cursor_xy(&desk.lua_code, desk.lua_cursor);
            desk.lua_cursor = xy_cursor(&desk.lua_code, row, 0);
            true
        }
        Key::End | Key::Ctrl('e') => {
            let (row, _) = cursor_xy(&desk.lua_code, desk.lua_cursor);
            // Конец строки — перед её переводом строки.
            let line = desk.lua_code.split('\n').nth(row).unwrap_or("");
            desk.lua_cursor = xy_cursor(&desk.lua_code, row, line.chars().count());
            true
        }
        Key::Backspace => {
            if desk.lua_cursor > 0 {
                let mut chars: Vec<char> = desk.lua_code.chars().collect();
                let idx = desk.lua_cursor - 1;
                chars.remove(idx);
                desk.lua_code = chars.into_iter().collect();
                desk.lua_cursor = idx;
            }
            true
        }
        Key::Delete => {
            let chars: Vec<char> = desk.lua_code.chars().collect();
            let idx = desk.lua_cursor;
            if idx < chars.len() {
                let mut chars = chars;
                chars.remove(idx);
                desk.lua_code = chars.into_iter().collect();
            }
            true
        }
        Key::Char(c) if !c.is_control() => {
            let mut chars: Vec<char> = desk.lua_code.chars().collect();
            let idx = desk.lua_cursor.min(chars.len());
            chars.insert(idx, c);
            desk.lua_code = chars.into_iter().collect();
            desk.lua_cursor = idx + 1;
            true
        }
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Курсор ↔ (строка, столбец) — согласованы; столбец прижимается к длине
    /// строки при движении вверх/вниз.
    #[test]
    fn cursor_xy_roundtrip_and_clamps_column() {
        let code = "ab\ncde\nfg";
        // Конец первой строки — перед '\n'.
        assert_eq!(cursor_xy(code, 2), (0, 2));
        assert_eq!(cursor_xy(code, 3), (1, 0));
        assert_eq!(cursor_xy(code, 6), (1, 3));
        assert_eq!(cursor_xy(code, 7), (2, 0));
        assert_eq!(cursor_xy(code, 9), (2, 2));
        // xy_cursor возвращает индекс знака.
        assert_eq!(xy_cursor(code, 0, 2), 2);
        assert_eq!(xy_cursor(code, 1, 3), 6);
        assert_eq!(xy_cursor(code, 2, 2), 9);
        // Столбец больше длины строки — прижимается к концу строки.
        assert_eq!(xy_cursor(code, 1, 99), 6);
        // Строки нет — в конец кода.
        assert_eq!(xy_cursor(code, 5, 0), code.chars().count());
    }

    /// Перемещение вниз с большого столбца прижимает курсор к концу короче.
    #[test]
    fn down_from_long_line_clamps_to_short_line_end() {
        let code = "abcd\ne\n";
        // Курсор на 'd' (столбец 3 строки 0).
        let cur = xy_cursor(code, 0, 3);
        assert_eq!(cursor_xy(code, cur), (0, 3));
        // Вниз: строка 1 длиной 1 — курсор встаёт на её конец (столбец 1).
        let down = xy_cursor(code, 1, 3);
        assert_eq!(cursor_xy(code, down), (1, 1));
    }
}
