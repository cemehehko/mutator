//! Лента агента. Ответ модели разбирает общий [`crate::markup`]
//! (`agr:markup-shared`): для TUI лента собирается строками — их считает
//! [`tape`], чтобы [`super::frame`] знал высоту и умел прокрутку; для окна те
//! же блоки рисуются виджетами egui.
//!
//! Наружу: [`tape`], [`Line`], [`wrap_line`], [`message`].

use egui;
use egui::text::LayoutJob;

use super::State;
use crate::markup::{self, Block, Span};
use crate::ui::theme::{ACCENT, DIM, NAME, TEXT};

/// Одна строка ленты: куски текста с цветом.
pub(super) type Line = Vec<(String, egui::Color32)>;

/// Разбить строку ленты на строки сетки: то, что шире `cols`, переносится
/// по словам, слово шире строки — по знакам.
pub(super) fn wrap_line(line: &Line, cols: usize) -> Vec<Line> {
    if cols == 0 {
        return vec![line.clone()];
    }
    let mut rows: Vec<Line> = Vec::new();
    let mut row: Line = Vec::new();
    let mut width = 0;
    for (text, fg) in line {
        for word in words(text) {
            let mut word = word;
            if width + word.chars().count() > cols && width > 0 {
                trim_row(&mut row);
                rows.push(std::mem::take(&mut row));
                width = 0;
                word = word.trim_start().to_string();
            }
            // Слово шире строки режем по знакам.
            while word.chars().count() > cols {
                let head: String = word.chars().take(cols).collect();
                let rest: String = word.chars().skip(cols).collect();
                rows.push(vec![(head, *fg)]);
                word = rest;
            }
            row.push((word.clone(), *fg));
            width += word.chars().count();
            if width >= cols {
                trim_row(&mut row);
                rows.push(std::mem::take(&mut row));
                width = 0;
            }
        }
    }
    trim_row(&mut row);
    if !row.is_empty() {
        rows.push(row);
    }
    rows
}

/// Хвостовые пробелы переноса в строку сетки не идут.
fn trim_row(row: &mut Line) {
    while let Some((t, _)) = row.last_mut() {
        let trimmed = t.trim_end().to_string();
        if trimmed.is_empty() {
            row.pop();
        } else {
            *t = trimmed;
            break;
        }
    }
}

/// Слова с прилипшим пробелом: перенос между ними, пробел в начале новой
/// строки не остаётся.
fn words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        cur.push(ch);
        if ch == ' ' {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Лента как строки: сообщения, стриминг и журнал хода.
pub(super) fn tape(state: &State) -> Vec<Line> {
    let mut out = Vec::new();
    // В ленте — вопросы и ответы; инструменты — итогом в строке состояния
    // (`agr:agent-loop`).
    for m in state.messages.iter().filter(|m| super::in_tape(m)) {
        let (who, color) = match m.role.as_str() {
            "user" => ("Вы", NAME),
            "assistant" => ("Модель", ACCENT),
            _ => ("Инструмент", DIM),
        };
        out.push(vec![(format!("{who}: "), color)]);
        blocks(&mut out, &m.text);
    }
    if !state.streaming.is_empty() {
        out.push(vec![("Модель: ".to_string(), ACCENT)]);
        blocks(&mut out, &state.streaming);
        // Мигающий курсор: текст ещё идёт (`agr:agent-progress`).
        if state.caret(std::time::Instant::now()) {
            if let Some(last) = out.last_mut() {
                last.push(("▍".to_string(), ACCENT));
            }
        }
    }
    if state.show_trace {
        for t in &state.trace {
            out.push(vec![(t.clone(), DIM)]);
        }
    }
    out
}

/// Блоки разметки как строки ленты.
fn blocks(out: &mut Vec<Line>, text: &str) {
    for b in markup::parse(text) {
        match b {
            Block::Heading { level, text } => {
                let mark = match level {
                    1 => "# ".to_string(),
                    2 => "## ".to_string(),
                    _ => "### ".to_string(),
                };
                let mut line = vec![(mark, DIM)];
                line.extend(spans_line(&text));
                out.push(line);
            }
            Block::Paragraph(text) => {
                out.push(spans_line(&text));
            }
            Block::Bullet { depth, text } => {
                let indent = " ".repeat(depth * 2);
                let mut line = vec![(format!("{indent}• "), DIM)];
                line.extend(spans_line(&text));
                out.push(line);
            }
            Block::Numbered {
                depth,
                number,
                text,
            } => {
                let indent = " ".repeat(depth * 2);
                let mut line = vec![(format!("{indent}{number} "), DIM)];
                line.extend(spans_line(&text));
                out.push(line);
            }
            Block::Code { lang, lines } => {
                for (i, l) in lines.iter().cloned().enumerate() {
                    if i == 0 && !lang.is_empty() {
                        out.push(vec![(format!("{lang} "), DIM), (l.clone(), TEXT)]);
                    } else {
                        out.push(vec![("  ".to_string(), TEXT), (l.clone(), TEXT)]);
                    }
                }
            }
            Block::Table { header, rows } => {
                // Колонки по ширине самой длинной ячейки; длинные строки
                // переносит wrap_line — выравнивание после переноса не держится.
                // Ширина — по видимому тексту: маркеры `**` и `` ` `` не рисуются.
                let mut w: Vec<usize> = header.iter().map(|c| c.chars().count()).collect();
                for r in &rows {
                    for (i, c) in r.iter().enumerate() {
                        if let Some(x) = w.get_mut(i) {
                            *x = (*x).max(visible(c));
                        }
                    }
                }
                let fit = |cell: &str, i: usize| {
                    let pad = w
                        .get(i)
                        .copied()
                        .unwrap_or(0)
                        .saturating_sub(cell.chars().count());
                    format!("{cell}{}", " ".repeat(pad))
                };
                out.push(
                    header
                        .iter()
                        .enumerate()
                        .map(|(i, c)| (format!("{}  ", fit(c, i)), NAME))
                        .collect(),
                );
                out.push(
                    w.iter()
                        .map(|x| (format!("{}  ", "-".repeat(*x)), DIM))
                        .collect(),
                );
                for r in rows {
                    let mut line = Line::new();
                    for (i, c) in r.iter().enumerate() {
                        // Ячейка — спанами, как в окне; добивка — по видимой ширине.
                        let pad = w.get(i).copied().unwrap_or(0).saturating_sub(visible(c));
                        let gap = format!("{}  ", " ".repeat(pad));
                        match spans_line(c).as_slice() {
                            [(t, fg)] if *fg == TEXT => line.push((format!("{t}{gap}"), TEXT)),
                            _ => {
                                line.extend(spans_line(c));
                                line.push((gap, TEXT));
                            }
                        }
                    }
                    out.push(line);
                }
            }
        }
    }
}

/// Сколько знаков ячейки видно после разметки.
fn visible(text: &str) -> usize {
    markup::spans(text)
        .iter()
        .map(|s| match s {
            Span::Text(t) | Span::Bold(t) | Span::Code(t) => t.chars().count(),
        })
        .sum()
}

/// Строка ответа: жирный и код — цветом.
fn spans_line(text: &str) -> Line {
    markup::spans(text)
        .into_iter()
        .map(|s| match s {
            Span::Text(t) => (t, TEXT),
            Span::Bold(t) => (t, NAME),
            Span::Code(t) => (t, ACCENT),
        })
        .collect()
}

/// Одно сообщение ленты в окне: роль и разобранный ответ.
pub(super) fn message(ui: &mut egui::Ui, m: &crate::agent::MsgView) {
    let (who, color) = match m.role.as_str() {
        "user" => ("Вы", NAME),
        "assistant" => ("Модель", ACCENT),
        _ => ("Инструмент", DIM),
    };
    ui.label(egui::RichText::new(who).small().color(color));
    // Индексы сообщения и блока — в id таблицы: у любых двух таблиц ленты
    // id не совпадает, иначе egui рисует их друг на друге.
    for (i, block) in markup::parse(&m.text).into_iter().enumerate() {
        block_ui(ui, &block, m.index, i);
    }
    ui.add_space(4.0);
}

fn block_ui(ui: &mut egui::Ui, b: &Block, msg: usize, n: usize) {
    use crate::ui::theme::PANEL;
    match b {
        Block::Heading { level, text } => {
            let size = match level {
                1 => 17.0,
                2 => 15.0,
                _ => 13.0,
            };
            ui.label(egui::RichText::new(text).size(size).strong().color(TEXT));
        }
        Block::Paragraph(text) => {
            ui.label(spans_job(ui, text));
        }
        Block::Bullet { depth, text } => {
            ui.horizontal(|ui| {
                ui.add_space(*depth as f32 * 16.0);
                ui.label(egui::RichText::new("•").color(DIM));
                ui.label(spans_job(ui, text));
            });
        }
        Block::Numbered {
            depth,
            number,
            text,
        } => {
            ui.horizontal(|ui| {
                ui.add_space(*depth as f32 * 16.0);
                ui.label(egui::RichText::new(number).color(DIM));
                ui.label(spans_job(ui, text));
            });
        }
        Block::Code { lang, lines } => {
            egui::Frame::group(ui.style()).fill(PANEL).show(ui, |ui| {
                ui.set_width(ui.available_width());
                if !lang.is_empty() {
                    ui.label(egui::RichText::new(lang).small().color(DIM));
                }
                for l in lines {
                    ui.label(egui::RichText::new(l).monospace().color(TEXT));
                }
            });
        }
        Block::Table { header, rows } => {
            egui::Grid::new(egui::Id::new("markup-table").with(msg).with(n))
                .striped(true)
                .num_columns(header.len().max(1))
                .show(ui, |ui| {
                    for c in header {
                        ui.label(egui::RichText::new(c).strong().color(TEXT));
                    }
                    ui.end_row();
                    for r in rows {
                        for c in r {
                            ui.label(spans_job(ui, c));
                        }
                        ui.end_row();
                    }
                });
        }
    }
}

/// Строка ответа в [`LayoutJob`]: жирный и код — цветом, код ещё моноширинным.
fn spans_job(ui: &egui::Ui, text: &str) -> LayoutJob {
    let body = egui::TextStyle::Body.resolve(ui.style());
    let mono = egui::TextStyle::Monospace.resolve(ui.style());
    let mut job = LayoutJob::default();
    for s in markup::spans(text) {
        let (t, color, font) = match s {
            Span::Text(t) => (t, TEXT, body.clone()),
            Span::Bold(t) => (t, NAME, body.clone()),
            Span::Code(t) => (t, ACCENT, mono.clone()),
        };
        job.append(
            &t,
            0.0,
            egui::TextFormat {
                font_id: font,
                color,
                ..Default::default()
            },
        );
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Лента: роль, разметка и журнал — по строке на строку ответа.
    #[test]
    fn tape_is_lines_of_role_markup_and_trace() {
        let mut s = State::new();
        s.messages.push(crate::agent::MsgView {
            role: "user".into(),
            text: "вопрос".into(),
            index: 0,
            name: String::new(),
            is_error: false,
        });
        s.messages.push(crate::agent::MsgView {
            role: "assistant".into(),
            text: "# Заголовок\n- пункт\n`код`".into(),
            index: 1,
            name: String::new(),
            is_error: false,
        });
        let tape = super::tape(&s);
        // Вопрос — роль плюс абзац.
        assert_eq!(tape[0][0].0, "Вы: ");
        assert_eq!(tape[1][0].0, "вопрос");
        // Ответ: заголовок, пункт списка, код.
        assert_eq!(tape[2][0].0, "Модель: ");
        assert_eq!(tape[3][0].0, "# ");
        assert_eq!(tape[3][1].0, "Заголовок");
        assert_eq!(tape[4][0].0, "• ");
        // `код` — цветом акцента.
        assert_eq!(tape[5][0].1, ACCENT);
        assert_eq!(tape[5][0].0, "код");
        assert_eq!(tape.len(), 6);

        s.trace.push_back("→ overview".into());
        let tape = super::tape(&s);
        assert_eq!(tape.len(), 6, "журнал скрыт без show_trace");
        s.show_trace = true;
        let tape = super::tape(&s);
        assert_eq!(tape.last().unwrap()[0].0, "→ overview");
    }

    /// `agr:agent-loop`
    /// В ленте — вопросы и ответы: результаты инструментов и ходы модели из
    /// одних вызовов не показываются.
    #[test]
    fn tools_and_call_only_turns_stay_out_of_the_tape() {
        let mut s = State::new();
        let msg = |role: &str, text: &str, name: &str| crate::agent::MsgView {
            role: role.into(),
            text: text.into(),
            index: 0,
            name: name.into(),
            is_error: false,
        };
        s.messages = vec![
            msg("user", "вопрос", ""),
            msg("assistant", "", ""),
            msg("tool", "a\nb", "schema_fields"),
            msg("assistant", "ответ", ""),
        ];
        let text: Vec<String> = super::tape(&s)
            .iter()
            .map(|l| l.iter().map(|(t, _)| t.as_str()).collect())
            .collect();
        assert_eq!(text, ["Вы: ", "вопрос", "Модель: ", "ответ"]);
    }

    /// `agr:markup-shared`
    /// Пункты списков, заголовки и ячейки таблиц размечаются как в окне:
    /// маркеры `**` и `` ` `` не видны, жирный и код — цветом.
    #[test]
    fn list_heading_and_cells_render_spans() {
        let mut s = State::new();
        s.streaming = "## Типы **`Money`**\n\n\
                       1. **`TransferFunds`** (msgid 160)\n\
                       \x20  - `amount : decimal128`\n\n\
                       | тип | поле |\n|---|---|\n| **`Position`** | `amount` |"
            .into();
        let tape = super::tape(&s);
        let text = |l: &Line| l.iter().map(|(t, _)| t.as_str()).collect::<String>();
        for l in &tape[1..] {
            assert!(
                !text(l).contains('*') && !text(l).contains('`'),
                "{:?}",
                text(l)
            );
        }
        let colored = |l: &Line, t: &str, c| l.iter().any(|(x, fg)| x == t && *fg == c);
        assert!(colored(&tape[1], "Money", NAME), "заголовок: {:?}", tape[1]);
        assert!(
            colored(&tape[2], "TransferFunds", NAME),
            "пункт: {:?}",
            tape[2]
        );
        assert!(
            colored(&tape[3], "amount : decimal128", ACCENT),
            "подпункт: {:?}",
            tape[3]
        );
        let row = tape.last().unwrap();
        assert!(colored(row, "Position", NAME) && colored(row, "amount", ACCENT));
        // Колонки ровные: ширина — по видимому тексту.
        assert_eq!(text(&tape[tape.len() - 3]), "тип       поле    ");
        assert_eq!(text(row), "Position  amount  ");
    }

    /// Стримящийся ответ виден в ленте последним.
    #[test]
    fn streaming_shows_as_partial_answer() {
        let mut s = State::new();
        s.streaming = "отвечаю".into();
        let tape = super::tape(&s);
        assert_eq!(tape[0][0].0, "Модель: ");
        assert_eq!(tape[1][0].0, "отвечаю");
    }

    /// Таблица — шапка, черта и строки, ячейки по ширине колонки.
    #[test]
    fn table_is_header_rule_and_padded_rows() {
        let mut s = State::new();
        s.streaming = "| a | b |\n|---|---|\n| 1 | два |".into();
        let tape = super::tape(&s);
        assert_eq!(
            tape[1].iter().map(|(t, _)| t.as_str()).collect::<String>(),
            "a  b    "
        );
        assert_eq!(tape[2][0].0, "-  ");
        assert_eq!(tape[2][1].0, "---  ");
        assert_eq!(tape[3][1].0, "два  ");
        assert_eq!(tape[3][1].1, TEXT);
        assert_eq!(tape.len(), 4);
    }

    /// Длинный абзац переносится по словам, а не обрезается.
    #[test]
    fn long_text_wraps_by_words() {
        let line: Line = vec![("раз два три четыре".into(), TEXT)];
        let rows = wrap_line(&line, 8);
        let joined: Vec<String> = rows
            .iter()
            .map(|r| r.iter().map(|(t, _)| t.as_str()).collect())
            .collect();
        assert_eq!(joined, vec!["раз два", "три", "четыре"]);
    }

    /// Маркер списка остаётся в начале своей строки переноса.
    #[test]
    fn bullet_keeps_prefix_after_wrap() {
        let line: Line = vec![("• ".into(), DIM), ("первый второй".into(), TEXT)];
        let rows = wrap_line(&line, 9);
        assert_eq!(rows[0][0].0, "• ");
        assert_eq!(rows[0][1].0, "первый");
        assert_eq!(rows[1][0].0, "второй");
    }

    /// Слово шире строки режется по знакам.
    #[test]
    fn long_word_splits_by_chars() {
        let line: Line = vec![("абвгдеёжз".into(), TEXT)];
        let rows = wrap_line(&line, 4);
        let joined: String = rows
            .iter()
            .flat_map(|r| r.iter().map(|(t, _)| t.as_str()))
            .collect();
        assert_eq!(joined, "абвгдеёжз");
        assert!(rows.iter().all(|r| row_len(r) <= 4));
    }

    /// Ширина строки в знаках.
    fn row_len(r: &Line) -> usize {
        r.iter().map(|(t, _)| t.chars().count()).sum()
    }
}
