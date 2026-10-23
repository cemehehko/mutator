//! Сетка символов TUI ([`Grid`] из клеток [`Cell`]) и её отрисовка
//! ([`show`]) клетками размера [`Metrics`].
//!
//! Знак стоит в своей клетке: позиция — номер столбца, умноженный на ширину
//! клетки, а не сумма ширин глифов. Поэтому глиф из запасного шрифта (рубль,
//! кириллица в Hack, стрелка) не сдвигает столбцы следующих строк. Рамки
//! `─│┌┐└┘├┤┬┴┼` и блоки `▁…█`, `▏…▉`, `░` рисуются фигурами по клетке:
//! линии смыкаются без зазоров, и в шрифте их может не быть вовсе.
//!
//! Нажимаемое — ссылки: отрезок строки с действием. Тесты находят ссылку по
//! её тексту ([`links_id`]), не угадывая геометрию сетки.
//!
//! Перенесено из black-d (`src/ui/tui/grid.rs`).
//!
//! `agr:tui-grid`
//!
//! Наружу: [`Grid`], [`Cell`], [`Metrics`], [`show`].

use egui;

use crate::ui::theme::{SELECT as TUI_CURSOR, TEXT as TUI_TEXT};

/// Клетка: знак, его цвет и подложка (прозрачная — без подложки).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cell {
    pub(crate) ch: char,
    pub(crate) fg: egui::Color32,
    pub(crate) bg: egui::Color32,
}

const BLANK: Cell = Cell {
    ch: ' ',
    fg: TUI_TEXT,
    bg: egui::Color32::TRANSPARENT,
};

/// Отрезок строки, по нажатию на который случается действие `act`.
struct Link<A> {
    row: usize,
    col: usize,
    len: usize,
    act: A,
}

/// Сетка шириной в `cols` клеток; строки добавляются по мере письма.
pub struct Grid<A> {
    cols: usize,
    lines: Vec<Vec<Cell>>,
    links: Vec<Link<A>>,
}

impl<A: Clone> Grid<A> {
    pub(crate) fn new(cols: usize) -> Self {
        Self {
            cols: cols.max(1),
            lines: Vec::new(),
            links: Vec::new(),
        }
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn rows(&self) -> usize {
        self.lines.len()
    }

    /// Новая пустая строка снизу; возвращает её номер.
    pub(crate) fn line(&mut self) -> usize {
        self.lines.push(vec![BLANK; self.cols]);
        self.lines.len() - 1
    }

    /// Довести сетку ровно до `n` строк снизу пустыми — экран терминала целиком.
    pub(crate) fn reach_rows(&mut self, n: usize) {
        if n > 0 {
            self.reach(n - 1);
        }
    }

    /// Пустые строки до `rows` включительно — чтобы писать в строку по номеру.
    fn reach(&mut self, row: usize) {
        while self.lines.len() <= row {
            self.line();
        }
    }

    /// Один знак в клетку; за краем — ничего.
    pub(crate) fn set(&mut self, row: usize, col: usize, ch: char, fg: egui::Color32) {
        if col >= self.cols {
            return;
        }
        self.reach(row);
        let cell = &mut self.lines[row][col];
        cell.ch = ch;
        cell.fg = fg;
    }

    /// Текст с клетки `col`: знак на клетку, что за краем — обрезано.
    /// Возвращает столбец сразу за текстом.
    pub(crate) fn put(&mut self, row: usize, col: usize, text: &str, fg: egui::Color32) -> usize {
        let mut at = col;
        for ch in text.chars().filter(|c| !c.is_control()) {
            self.set(row, at, ch, fg);
            at += 1;
        }
        at.min(self.cols)
    }

    /// Текст, прижатый правым краем к столбцу `end` (не включая его).
    /// Возвращает столбец, с которого текст начался.
    pub(crate) fn put_right(
        &mut self,
        row: usize,
        end: usize,
        text: &str,
        fg: egui::Color32,
    ) -> usize {
        let len = text.chars().count();
        let start = end.saturating_sub(len);
        let skip = len.saturating_sub(end);
        let shown: String = text.chars().skip(skip).collect();
        self.put(row, start, &shown, fg);
        start
    }

    /// Подложка `len` клеток с `col`.
    pub(crate) fn shade(&mut self, row: usize, col: usize, len: usize, bg: egui::Color32) {
        self.reach(row);
        let end = (col + len).min(self.cols);
        for cell in self.lines[row].iter_mut().take(end).skip(col) {
            cell.bg = bg;
        }
    }

    /// Перекрасить знаки `len` клеток с `col` — инверсия выбранного пункта.
    #[cfg(test)]
    pub(crate) fn ink(&mut self, row: usize, col: usize, len: usize, fg: egui::Color32) {
        self.reach(row);
        let end = (col + len).min(self.cols);
        for cell in self.lines[row].iter_mut().take(end).skip(col) {
            cell.fg = fg;
        }
    }

    /// Сделать `len` клеток с `col` ссылкой на действие `act`.
    pub(crate) fn link(&mut self, row: usize, col: usize, len: usize, act: A) {
        let len = len.min(self.cols.saturating_sub(col));
        if len > 0 {
            self.reach(row);
            self.links.push(Link { row, col, len, act });
        }
    }

    /// Строка текстом, без хвостовых пробелов: так её видят тесты.
    pub fn text(&self, row: usize) -> String {
        self.lines
            .get(row)
            .map(|line| line.iter().map(|c| c.ch).collect::<String>())
            .unwrap_or_default()
            .trim_end()
            .to_string()
    }

    /// Цвет знака клетки — тестам: подсветка правок.
    pub fn fg(&self, row: usize, col: usize) -> Option<egui::Color32> {
        self.lines.get(row)?.get(col).map(|c| c.fg)
    }

    /// Подложка клетки — тестам: где стоит курсор.
    pub fn bg(&self, row: usize, col: usize) -> Option<egui::Color32> {
        self.lines.get(row)?.get(col).map(|c| c.bg)
    }

    /// Действие ссылки под клеткой.
    pub fn act_at(&self, row: usize, col: usize) -> Option<A> {
        self.link_at(row, col).map(|l| l.act.clone())
    }

    /// Клетка `(row, col)`; за пределами сетки — `None`.
    pub(crate) fn cell(&self, row: usize, col: usize) -> Option<Cell> {
        self.lines.get(row).and_then(|line| line.get(col)).copied()
    }

    /// Последняя положенная ссылка под клеткой: позже положенная — сверху.
    fn link_at(&self, row: usize, col: usize) -> Option<&Link<A>> {
        self.links
            .iter()
            .rev()
            .find(|l| l.row == row && col >= l.col && col < l.col + l.len)
    }

    /// Текст ссылки — по нему её находят тесты.
    fn link_text(&self, link: &Link<A>) -> String {
        self.lines[link.row][link.col..link.col + link.len]
            .iter()
            .map(|c| c.ch)
            .collect::<String>()
            .trim()
            .to_string()
    }
}

/// Шрифт и размер клетки сетки.
#[derive(Clone, Debug)]
pub struct Metrics {
    pub(crate) font: egui::FontId,
    pub(crate) cell: egui::Vec2,
}

impl Metrics {
    /// Клетка моноширинного шрифта кегля `size`: ширина нуля и высота строки.
    pub(crate) fn new(ctx: &egui::Context, size: f32) -> Self {
        let font = egui::FontId::monospace(size);
        let (width, height) = ctx.fonts_mut(|f| (f.glyph_width(&font, '0'), f.row_height(&font)));
        let width = if width > 0.5 { width } else { size * 0.6 };
        Self {
            font,
            cell: egui::vec2(width, height.ceil().max(size)),
        }
    }

    /// Сколько клеток помещается в ширину `width`.
    pub(crate) fn cols(&self, width: f32) -> usize {
        ((width / self.cell.x).floor() as usize).max(1)
    }
}

/// Куда тесты кладут нарисованные ссылки этого кадра: текст и прямоугольник.
pub fn links_id() -> egui::Id {
    egui::Id::new("mutator-tui-links")
}

/// Ссылки этого кадра, накопленные со всех сеток.
fn remember_links(ctx: &egui::Context, found: Vec<(String, egui::Rect)>) {
    let frame = ctx.cumulative_frame_nr();
    ctx.data_mut(|d| {
        let slot = d.get_temp_mut_or_default::<(u64, Vec<(String, egui::Rect)>)>(links_id());
        if slot.0 != frame {
            *slot = (frame, Vec::new());
        }
        slot.1.extend(found);
    });
}

/// Рисует сетку; `live` — можно нажимать. Возвращает действие нажатой ссылки.
pub fn show<A: Clone>(ui: &mut egui::Ui, grid: &Grid<A>, m: &Metrics, live: bool) -> Option<A> {
    let size = egui::vec2(grid.cols as f32 * m.cell.x, grid.rows() as f32 * m.cell.y);
    let sense = if live {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    let cell_at = |pos: egui::Pos2| {
        let rel = pos - rect.min;
        if rel.x < 0.0 || rel.y < 0.0 {
            return None;
        }
        Some(((rel.y / m.cell.y) as usize, (rel.x / m.cell.x) as usize))
    };
    let cell_rect = |row: usize, col: usize, len: usize| {
        egui::Rect::from_min_size(
            rect.min + egui::vec2(col as f32 * m.cell.x, row as f32 * m.cell.y),
            egui::vec2(len as f32 * m.cell.x, m.cell.y),
        )
    };
    let hovered = live
        .then(|| response.hover_pos())
        .flatten()
        .and_then(cell_at)
        .and_then(|(row, col)| grid.link_at(row, col));
    if live {
        let found = grid
            .links
            .iter()
            .map(|l| (grid.link_text(l), cell_rect(l.row, l.col, l.len)))
            .collect();
        remember_links(ui.ctx(), found);
    }
    if hovered.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    let painter = ui.painter_at(rect);
    let clip = ui.clip_rect();
    // Рисуем только видимые строки: длинная лента в прокрутке не тратит кадр
    // на то, чего не видно.
    let first = (((clip.top() - rect.top()) / m.cell.y).floor().max(0.0)) as usize;
    let last =
        ((((clip.bottom() - rect.top()) / m.cell.y).ceil()).max(0.0) as usize).min(grid.rows());
    for row in first..last {
        for col in 0..grid.cols {
            let cell = grid.lines[row][col];
            let at = cell_rect(row, col, 1);
            if cell.bg != egui::Color32::TRANSPARENT {
                painter.rect_filled(at.expand2(egui::vec2(0.5, 0.0)), 0.0, cell.bg);
            }
        }
        if let Some(link) = hovered.filter(|l| l.row == row) {
            painter.rect_filled(cell_rect(row, link.col, link.len), 0.0, TUI_CURSOR);
        }
        for col in 0..grid.cols {
            let cell = grid.lines[row][col];
            if cell.ch != ' ' {
                glyph(&painter, cell_rect(row, col, 1), cell, &m.font);
            }
        }
    }

    let clicked = live && response.clicked();
    clicked
        .then(|| response.interact_pointer_pos())
        .flatten()
        .and_then(cell_at)
        .and_then(|(row, col)| grid.link_at(row, col))
        .map(|l| l.act.clone())
}

/// Плечи линии рамки из центра клетки: влево, вправо, вверх, вниз.
fn arms(ch: char) -> Option<[bool; 4]> {
    Some(match ch {
        '─' | '━' => [true, true, false, false],
        '│' => [false, false, true, true],
        '┌' => [false, true, false, true],
        '┐' => [true, false, false, true],
        '└' => [false, true, true, false],
        '┘' => [true, false, true, false],
        '├' => [false, true, true, true],
        '┤' => [true, false, true, true],
        '┬' => [true, true, false, true],
        '┴' => [true, true, true, false],
        '┼' => [true, true, true, true],
        _ => return None,
    })
}

/// Нижние восьмушки `▁…█`: сколько восьмых высоты закрашено.
fn lower_eighths(ch: char) -> Option<u8> {
    match ch as u32 {
        code @ 0x2581..=0x2588 => Some((code - 0x2580) as u8),
        _ => None,
    }
}

/// Левые восьмушки `▏…▉`: сколько восьмых ширины закрашено.
fn left_eighths(ch: char) -> Option<u8> {
    match ch as u32 {
        code @ 0x2589..=0x258F => Some((0x2590 - code) as u8),
        _ => None,
    }
}

/// Знак в клетке: рамка и блоки — фигурами, остальное — шрифтом по центру
/// клетки.
fn glyph(painter: &egui::Painter, at: egui::Rect, cell: Cell, font: &egui::FontId) {
    if let Some([left, right, up, down]) = arms(cell.ch) {
        let width = if cell.ch == '━' { 2.0 } else { 1.0 };
        let stroke = egui::Stroke::new(width, cell.fg);
        // Центр — на середине пикселя, чтобы линия в один пиксель не мылилась.
        let c = egui::pos2(at.center().x.floor() + 0.5, at.center().y.floor() + 0.5);
        if left {
            painter.line_segment([egui::pos2(at.left(), c.y), c], stroke);
        }
        if right {
            painter.line_segment([c, egui::pos2(at.right(), c.y)], stroke);
        }
        if up {
            painter.line_segment([egui::pos2(c.x, at.top()), c], stroke);
        }
        if down {
            painter.line_segment([c, egui::pos2(c.x, at.bottom())], stroke);
        }
        return;
    }
    if let Some(n) = lower_eighths(cell.ch) {
        let top = at.bottom() - at.height() * n as f32 / 8.0;
        let block = egui::Rect::from_min_max(egui::pos2(at.left(), top), at.max);
        painter.rect_filled(block.expand2(egui::vec2(0.5, 0.0)), 0.0, cell.fg);
        return;
    }
    if let Some(n) = left_eighths(cell.ch) {
        let right = at.left() + at.width() * n as f32 / 8.0;
        let block = egui::Rect::from_min_max(at.min, egui::pos2(right, at.bottom()));
        painter.rect_filled(block, 0.0, cell.fg);
        return;
    }
    if cell.ch == '░' {
        painter.rect_filled(at, 0.0, cell.fg.gamma_multiply(0.3));
        return;
    }
    painter.text(
        egui::pos2(at.center().x, at.top()),
        egui::Align2::CENTER_TOP,
        cell.ch,
        font.clone(),
        cell.fg,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::ACCENT as TUI_ACCENT;

    /// Кириллица, рубль и блок занимают по клетке: столбец после них тот же,
    /// что и после латиницы, — шрифт не двигает столбцы.
    ///
    /// `agr:tui-grid`
    #[test]
    fn every_sign_takes_one_cell() {
        let mut grid: Grid<()> = Grid::new(20);
        let a = grid.line();
        let b = grid.line();
        let after_cyr = grid.put(a, 0, "Сбер ₽█", TUI_TEXT);
        let after_lat = grid.put(b, 0, "SBER $#", TUI_TEXT);
        assert_eq!(after_cyr, after_lat);
        grid.put(a, after_cyr, "|", TUI_TEXT);
        grid.put(b, after_lat, "|", TUI_TEXT);
        assert_eq!(grid.cell(a, 7).map(|c| c.ch), Some('|'));
        assert_eq!(grid.cell(b, 7).map(|c| c.ch), Some('|'));
    }

    /// Что не влезло, обрезается у края; правое выравнивание держит край.
    #[test]
    fn text_is_clipped_at_the_edge_and_right_aligned_to_it() {
        let mut grid: Grid<()> = Grid::new(6);
        let row = grid.line();
        assert_eq!(grid.put(row, 3, "длинный", TUI_TEXT), 6);
        assert_eq!(grid.text(row), "   дли");
        let row = grid.line();
        assert_eq!(grid.put_right(row, 6, "12,5", TUI_TEXT), 2);
        assert_eq!(grid.text(row), "  12,5");
    }

    /// Позже положенная ссылка перекрывает раннюю: строка — ссылка на бумагу,
    /// а столбец в ней — своя.
    #[test]
    fn later_link_wins() {
        let mut grid = Grid::new(10);
        let row = grid.line();
        grid.link(row, 0, 10, 1);
        grid.link(row, 4, 2, 2);
        assert_eq!(grid.link_at(row, 1).map(|l| l.act), Some(1));
        assert_eq!(grid.link_at(row, 5).map(|l| l.act), Some(2));
        assert!(grid.link_at(row + 1, 5).is_none());
    }

    /// Блоки знают свою долю клетки, а прочие знаки — нет.
    #[test]
    fn blocks_know_their_share() {
        assert_eq!(lower_eighths('▁'), Some(1));
        assert_eq!(lower_eighths('█'), Some(8));
        assert_eq!(left_eighths('▏'), Some(1));
        assert_eq!(left_eighths('▉'), Some(7));
        assert_eq!(lower_eighths('a'), None);
        assert!(arms('┼').is_some() && arms('x').is_none());
        let mut grid: Grid<()> = Grid::new(3);
        let row = grid.line();
        grid.put(row, 0, "abc", TUI_TEXT);
        grid.ink(row, 1, 1, TUI_ACCENT);
        assert_eq!(grid.cell(row, 1).map(|c| c.fg), Some(TUI_ACCENT));
    }
}
