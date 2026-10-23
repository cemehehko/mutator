//! Подсветка синтаксиса Lua: токенизатор по строкам и сборка в
//! [`egui::text::LayoutJob`] для [`egui::TextEdit`]. Без зависимостей,
//! достаточно для редактора transform-кода.
//!
//! Наружу: [`highlight_lua`], [`highlight_lines`].

use crate::ui::theme;
use egui;

use egui::text::{LayoutJob, TextFormat};

/// Ключевые слова Lua.
const KEYWORDS: &[&str] = &[
    "function", "end", "if", "then", "elseif", "else", "return", "local", "for", "while", "do",
    "repeat", "until", "break", "in", "nil", "true", "false", "and", "or", "not", "goto",
];

/// Встроенные функции tll-lua.
const TLL_BUILTINS: &[&str] = &[
    "tll_child_post",
    "tll_on_data",
    "tll_on_open",
    "tll_on_close",
    "tll_filter",
    "tll_on_active",
    "tll_on_control",
    "tll_on_post",
    "tll_on_post_control",
    "tll_callback",
    "tll_self",
    "tll_self_child",
    "tll_self_scheme",
    "tll_child_scheme",
    "tll_msg_copy",
    "tll_msg_deepcopy",
    "tll_msg_pmap_check",
    "tll_time_point",
    "tll_logger",
];

/// Цвета подсветки.
struct Colors {
    kw: egui::Color32,
    builtin: egui::Color32,
    string: egui::Color32,
    comment: egui::Color32,
    number: egui::Color32,
    text: egui::Color32,
}

fn colors() -> Colors {
    Colors {
        kw: theme::ACCENT,
        builtin: egui::Color32::from_rgb(0xe0, 0xaa, 0x60),
        string: egui::Color32::from_rgb(0x98, 0xc3, 0x79),
        comment: theme::DIM,
        number: egui::Color32::from_rgb(0xe5, 0xc0, 0x7b),
        text: theme::TEXT,
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Токенизировать одну строку Lua в сегменты (текст, цвет).
fn tokenize_line(line: &str, col: &Colors) -> Vec<(String, egui::Color32)> {
    let mut out = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        // Строка: "..." или '...'
        if c == '"' || c == '\'' {
            let mut s = String::new();
            s.push(c);
            i += 1;
            while i < n {
                let ch = chars[i];
                s.push(ch);
                if ch == '\\' && i + 1 < n {
                    i += 1;
                    s.push(chars[i]);
                } else if ch == c {
                    break;
                }
                i += 1;
            }
            i += 1;
            out.push((s, col.string));
            continue;
        }
        // Комментарий: --...  или --[[...]]
        if c == '-' && i + 1 < n && chars[i + 1] == '-' {
            let rest: String = chars[i..].iter().collect();
            out.push((rest, col.comment));
            break;
        }
        // Число: 0x..., 0, 123, 1.5, .5
        if c.is_ascii_digit() || (c == '.' && i + 1 < n && chars[i + 1].is_ascii_digit()) {
            let start = i;
            if c == '0' && i + 1 < n && (chars[i + 1] == 'x' || chars[i + 1] == 'X') {
                i += 2;
                while i < n && chars[i].is_ascii_hexdigit() {
                    i += 1;
                }
            } else {
                if c == '.' {
                    i += 1;
                }
                while i < n && chars[i].is_ascii_digit() {
                    i += 1;
                }
                if i < n && chars[i] == '.' {
                    i += 1;
                    while i < n && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                if i < n && (chars[i] == 'e' || chars[i] == 'E') {
                    i += 1;
                    if i < n && (chars[i] == '+' || chars[i] == '-') {
                        i += 1;
                    }
                    while i < n && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                }
            }
            let num: String = chars[start..i].iter().collect();
            out.push((num, col.number));
            continue;
        }
        // Идентификатор или ключевое слово
        if is_ident_start(c) {
            let start = i;
            while i < n && is_ident(chars[i]) {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let color = if TLL_BUILTINS.contains(&word.as_str()) {
                col.builtin
            } else if KEYWORDS.contains(&word.as_str()) {
                col.kw
            } else {
                col.text
            };
            out.push((word, color));
            continue;
        }
        // Прочие символы
        out.push((c.to_string(), col.text));
        i += 1;
    }
    out
}

/// Собрать [`LayoutJob`] для многострочного текста Lua с подсветкой.
pub fn highlight_lua(ui: &egui::Ui, text: &str) -> LayoutJob {
    let col = colors();
    let mono = egui::TextStyle::Monospace.resolve(ui.style());
    let mut job = LayoutJob::default();
    for line in text.lines() {
        for (t, color) in tokenize_line(line, &col) {
            job.append(
                &t,
                0.0,
                TextFormat {
                    font_id: mono.clone(),
                    color,
                    ..Default::default()
                },
            );
        }
        job.append(
            "\n",
            0.0,
            TextFormat {
                font_id: mono.clone(),
                color: col.text,
                ..Default::default()
            },
        );
    }
    job
}

/// Собрать раскрашенные строки для TUI: `Vec<(текст, цвет_egui)>` для каждой
/// строки.
pub fn highlight_lines(text: &str) -> Vec<Vec<(String, egui::Color32)>> {
    let col = colors();
    text.lines().map(|line| tokenize_line(line, &col)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ключевое слово — синий, строка — зелёный, комментарий — серый.
    #[test]
    fn keywords_blue_strings_green_comments_gray() {
        let col = colors();
        let toks = tokenize_line("function foo()", &col);
        assert_eq!(toks[0].1, col.kw, "function — ключевое слово");
        assert_eq!(toks[1].1, col.text, "пробел");
        assert_eq!(toks[2].1, col.text, "foo — идентификатор");

        let toks = tokenize_line("\"hello\"", &col);
        assert_eq!(toks[0].1, col.string, "строка");
        assert_eq!(toks[0].0, "\"hello\"");

        let toks = tokenize_line("-- comment", &col);
        assert_eq!(toks[0].1, col.comment, "комментарий");
        assert_eq!(toks[0].0, "-- comment");
    }

    /// tll_child_post и другие встроенные — оранжевый.
    #[test]
    fn tll_builtins_are_orange() {
        let col = colors();
        let toks = tokenize_line("tll_child_post(data)", &col);
        assert_eq!(toks[0].1, col.builtin, "tll_child_post — встроенный");
        assert!(toks[0].0 == "tll_child_post");
    }

    /// Числа — жёлтый.
    #[test]
    fn numbers_are_yellow() {
        let col = colors();
        let toks = tokenize_line("42", &col);
        assert_eq!(toks[0].1, col.number);
        assert_eq!(toks[0].0, "42");

        let toks = tokenize_line("0xff", &col);
        assert_eq!(toks[0].1, col.number);
        assert_eq!(toks[0].0, "0xff");
    }

    /// highlight_lines разбивает текст по строкам.
    #[test]
    fn highlight_lines_splits_by_line() {
        let lines = highlight_lines("local x\nreturn x");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].first().map(|s| s.0.as_str()), Some("local"));
        assert_eq!(lines[1].first().map(|s| s.0.as_str()), Some("return"));
    }
}
