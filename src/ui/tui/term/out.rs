//! Вывод сетки в терминал ([`Screen`]): пишутся только клетки, изменившиеся
//! с прошлого кадра, — иначе экран мигал бы на каждом кадре.
//!
//! Цвета — 24-битные, если терминал их объявил (`COLORTERM=truecolor` или
//! `24bit`), иначе ближайшие из палитры 256 ([`ansi256`]): Terminal.app на Mac
//! truecolor не объявляет. Перенесено из black-d (`src/ui/tui/term/out.rs`).

use std::io::Write;

use crossterm::style::{Color, Print, SetBackgroundColor, SetForegroundColor};
use crossterm::{cursor, queue};
use egui;

use crate::ui::tui::grid::{Cell, Grid};

/// Что уже стоит на экране терминала.
pub(super) struct Screen {
    /// Клетки прошлого кадра по строкам; пусто — экран ещё не рисовали.
    shown: Vec<Vec<Cell>>,
    truecolor: bool,
}

impl Screen {
    pub(super) fn new(truecolor: bool) -> Self {
        Self {
            shown: Vec::new(),
            truecolor,
        }
    }

    /// Умеет ли терминал 24-битный цвет — по `COLORTERM`.
    pub(super) fn truecolor_from_env() -> bool {
        std::env::var("COLORTERM")
            .map(|v| matches!(v.as_str(), "truecolor" | "24bit"))
            .unwrap_or(false)
    }

    /// Забыть, что на экране: после смены размера терминал рисуем заново.
    pub(super) fn forget(&mut self) {
        self.shown.clear();
    }

    /// Нарисовать кадр `grid`, дописав в `out` только изменившиеся клетки.
    pub(super) fn draw<A: Clone>(
        &mut self,
        out: &mut impl Write,
        grid: &Grid<A>,
    ) -> std::io::Result<()> {
        let mut fg = None;
        let mut bg = None;
        // Где сейчас курсор терминала: соседние клетки пишутся без перехода.
        let mut at: Option<(usize, usize)> = None;
        let mut next = Vec::with_capacity(grid.rows());
        for row in 0..grid.rows() {
            let mut line = Vec::with_capacity(grid.cols());
            for col in 0..grid.cols() {
                let Some(cell) = grid.cell(row, col) else {
                    continue;
                };
                line.push(cell);
                let old = self.shown.get(row).and_then(|l| l.get(col));
                if old == Some(&cell) {
                    continue;
                }
                if at != Some((row, col)) {
                    queue!(out, cursor::MoveTo(col as u16, row as u16))?;
                }
                let want_fg = self.color(cell.fg);
                if fg != Some(want_fg) {
                    queue!(out, SetForegroundColor(want_fg))?;
                    fg = Some(want_fg);
                }
                let want_bg = if cell.bg == egui::Color32::TRANSPARENT {
                    Color::Reset
                } else {
                    self.color(cell.bg)
                };
                if bg != Some(want_bg) {
                    queue!(out, SetBackgroundColor(want_bg))?;
                    bg = Some(want_bg);
                }
                queue!(out, Print(cell.ch))?;
                at = Some((row, col + 1));
            }
            next.push(line);
        }
        if fg.is_some() || bg.is_some() {
            queue!(
                out,
                SetForegroundColor(Color::Reset),
                SetBackgroundColor(Color::Reset)
            )?;
        }
        out.flush()?;
        self.shown = next;
        Ok(())
    }

    fn color(&self, c: egui::Color32) -> Color {
        if self.truecolor {
            Color::Rgb {
                r: c.r(),
                g: c.g(),
                b: c.b(),
            }
        } else {
            Color::AnsiValue(ansi256(c))
        }
    }
}

/// Ближайший цвет палитры 256: куб 6×6×6 или шкала серого — что ближе.
pub(super) fn ansi256(c: egui::Color32) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let level = |v: u8| {
        LEVELS
            .iter()
            .enumerate()
            .min_by_key(|(_, l)| (i32::from(**l) - i32::from(v)).abs())
            .map(|(i, _)| i as u8)
            .unwrap_or(0)
    };
    let (r, g, b) = (level(c.r()), level(c.g()), level(c.b()));
    let cube = 16 + 36 * r + 6 * g + b;
    let cube_rgb = [LEVELS[r as usize], LEVELS[g as usize], LEVELS[b as usize]];
    let mean = (u16::from(c.r()) + u16::from(c.g()) + u16::from(c.b())) / 3;
    let gray_step = ((mean.saturating_sub(8)) / 10).min(23) as u8;
    let gray = 8 + 10 * gray_step;
    let dist = |rgb: [u8; 3]| {
        [c.r(), c.g(), c.b()]
            .iter()
            .zip(rgb)
            .map(|(a, b)| (i32::from(*a) - i32::from(b)).pow(2))
            .sum::<i32>()
    };
    if dist([gray, gray, gray]) < dist(cube_rgb) {
        232 + gray_step
    } else {
        cube
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::{OK as GREEN, TEXT as TUI_TEXT};

    fn grid(text: &str) -> Grid<()> {
        let mut g = Grid::new(4);
        let row = g.line();
        g.put(row, 0, text, TUI_TEXT);
        g
    }

    /// Первый кадр пишет все клетки, следующий — только изменившуюся.
    #[test]
    fn second_frame_writes_only_what_changed() {
        let mut screen = Screen::new(true);
        let mut first = Vec::new();
        screen.draw(&mut first, &grid("abcd")).expect("пишется");
        let first = String::from_utf8_lossy(&first).to_string();
        assert!(first.contains("abcd"), "{first:?}");

        let mut second = Vec::new();
        screen.draw(&mut second, &grid("abXd")).expect("пишется");
        let second = String::from_utf8_lossy(&second).to_string();
        assert!(second.contains('X'));
        assert!(!second.contains('a') && !second.contains('d'), "{second:?}");

        let mut third = Vec::new();
        screen.draw(&mut third, &grid("abXd")).expect("пишется");
        assert!(third.is_empty(), "без изменений — ни байта");
    }

    /// Без truecolor цвета ложатся в палитру 256, а серое — в шкалу серого.
    #[test]
    fn colors_fall_back_to_256() {
        assert_eq!(ansi256(egui::Color32::from_rgb(255, 0, 0)), 196);
        assert_eq!(ansi256(egui::Color32::BLACK), 16);
        assert!((232..=255).contains(&ansi256(egui::Color32::from_rgb(128, 128, 128))));
        // crossterm 0.29 слушается NO_COLOR: снимаем, чтобы увидеть разметку.
        crossterm::style::Colored::set_ansi_color_disabled(false);
        let mut out = Vec::new();
        let mut g = Grid::<()>::new(1);
        let row = g.line();
        g.put(row, 0, "x", GREEN);
        Screen::new(false).draw(&mut out, &g).expect("пишется");
        let text = String::from_utf8_lossy(&out).to_string();
        assert!(text.contains("38;5;"), "цвет из палитры 256: {text:?}");
    }
}
