//! Разметка ответа модели: строки в блоках, общая для окна и TUI.
//!
//! Модель отвечает Markdown-подобным текстом; и окно, и TUI рисуют его одним
//! и тем же разбором, чтобы ответ выглядел одинаково в обоих видах
//! (`agr:markup-shared`). Разметка нарочно узкая: заголовки до третьего уровня,
//! списки, таблицы и выделение внутри строки. Чего нет — то и не размечается:
//! ссылки остаются текстом.
//!
//! [`parse`] режет текст на [`Block`] — заголовок, абзац, пункт списка,
//! таблицу или блок кода; [`spans`] режет строку на [`Span`] — обычный текст,
//! жирный и код. Разбор — чистые функции без состояния: их проверяют тесты,
//! а рисуют виды по-своему.
//!
//! Наружу: [`Block`], [`Span`], [`parse`], [`spans`].

/// Блок ответа: то, что вид рисует отдельной строкой или группой строк.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// Заголовок уровня 1–3.
    Heading { level: u8, text: String },
    /// Абзац: строки подряд, склеенные пробелом.
    Paragraph(String),
    /// Пункт маркированного списка; `depth` — вложенность с нуля.
    Bullet { depth: usize, text: String },
    /// Пункт нумерованного списка; `number` — как он написан (`1.`).
    Numbered {
        depth: usize,
        number: String,
        text: String,
    },
    /// Таблица: шапка и строки; ячейки — текстом, спаны внутри не размечаются.
    Table {
        header: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    /// Блок кода: язык из ```-строки и строки тела как есть.
    Code { lang: String, lines: Vec<String> },
}

/// Кусок строки: обычный текст, жирный или код.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Span {
    Text(String),
    Bold(String),
    Code(String),
}

/// Разобрать текст ответа в блоки.
pub fn parse(text: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut para: Vec<String> = Vec::new();
    let mut code: Option<(String, Vec<String>)> = None;
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;

    let flush = |para: &mut Vec<String>, out: &mut Vec<Block>| {
        if !para.is_empty() {
            out.push(Block::Paragraph(para.join(" ")));
            para.clear();
        }
    };

    while i < lines.len() {
        let line = lines[i];
        i += 1;
        if let Some((lang, lines)) = &mut code {
            if line.trim_start().starts_with("```") {
                out.push(Block::Code {
                    lang: std::mem::take(lang),
                    lines: std::mem::take(lines),
                });
                code = None;
            } else {
                lines.push(line.to_string());
            }
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            flush(&mut para, &mut out);
            let lang = trimmed.strip_prefix("```").unwrap_or_default().trim();
            code = Some((lang.to_string(), Vec::new()));
            continue;
        }
        if trimmed.is_empty() {
            flush(&mut para, &mut out);
            continue;
        }
        if let Some((level, text)) = heading(trimmed) {
            flush(&mut para, &mut out);
            out.push(Block::Heading { level, text });
            continue;
        }
        if let Some(block) = list_item(line) {
            flush(&mut para, &mut out);
            out.push(block);
            continue;
        }
        if is_row(trimmed) {
            // Таблица: строки с `|` подряд, вторая — разделитель `|---|`.
            let start = i - 1;
            let mut j = i;
            while j < lines.len() && is_row(lines[j].trim_start()) {
                j += 1;
            }
            let run: Vec<&str> = lines[start..j].to_vec();
            if run.len() >= 2 && separator(run[1]) {
                flush(&mut para, &mut out);
                out.push(Block::Table {
                    header: cells(run[0]),
                    rows: run[2..].iter().map(|l| cells(l)).collect(),
                });
                i = j;
                continue;
            }
        }
        para.push(trimmed.to_string());
    }
    match code {
        // Незакрытый блок кода — до конца текста: лучше код, чем абзац с ```.
        Some((lang, lines)) => out.push(Block::Code { lang, lines }),
        None => flush(&mut para, &mut out),
    }
    out
}

/// Строка-кандидат в таблицу: обрамлена `|` (пробелы по краям игнорируются).
fn is_row(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('|') && t.ends_with('|') && t.len() > 1
}

/// Разделитель шапки: все ячейки — `---`, `:--` или `--:`.
fn separator(line: &str) -> bool {
    let cs = cells(line);
    !cs.is_empty()
        && cs.iter().all(|c| {
            let b = c.strip_prefix(':').unwrap_or(c);
            let b = b.strip_suffix(':').unwrap_or(b);
            !b.is_empty() && b.chars().all(|ch| ch == '-')
        })
}

/// Ячейки строки таблицы: содержимое между `|`.
fn cells(line: &str) -> Vec<String> {
    let inner = line
        .strip_prefix('|')
        .and_then(|x| x.strip_suffix('|'))
        .unwrap_or(line);
    inner.split('|').map(|c| c.trim().to_string()).collect()
}

/// Заголовок `#`, `##` или `###`; четыре решётки и больше — уже не заголовок.
fn heading(line: &str) -> Option<(u8, String)> {
    let level = line.chars().take_while(|&c| c == '#').count();
    if !(1..=3).contains(&level) {
        return None;
    }
    let rest = &line[level..];
    let text = rest.strip_prefix(' ')?.trim();
    (!text.is_empty()).then(|| (level as u8, text.to_string()))
}

/// Пункт списка: `- `, `* `, `+ ` или `N. `; отступ задаёт вложенность.
fn list_item(line: &str) -> Option<Block> {
    let indent = line.len() - line.trim_start().len();
    let depth = indent / 2;
    let body = line.trim_start();
    if let Some(text) = body
        .strip_prefix("- ")
        .or_else(|| body.strip_prefix("* "))
        .or_else(|| body.strip_prefix("+ "))
    {
        return Some(Block::Bullet {
            depth,
            text: text.trim().to_string(),
        });
    }
    let digits = body.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 {
        if let Some(rest) = body[digits..].strip_prefix(". ") {
            return Some(Block::Numbered {
                depth,
                number: format!("{}.", &body[..digits]),
                text: rest.trim().to_string(),
            });
        }
    }
    None
}

/// Разобрать строку на куски: `**жирный**` и `` `код` ``. Код внутри
/// жирного (`` **`Имя`** `` — модели пишут так часто) — жирный без кавычек:
/// маркеры вида не должны вылезать текстом.
pub fn spans(text: &str) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    let mut plain = String::new();
    let mut rest = text;
    let push = |out: &mut Vec<Span>, plain: &mut String| {
        if !plain.is_empty() {
            out.push(Span::Text(std::mem::take(plain)));
        }
    };
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("**") {
            if let Some(end) = after.find("**") {
                push(&mut out, &mut plain);
                let inner: String = spans(&after[..end])
                    .into_iter()
                    .map(|s| match s {
                        Span::Text(t) | Span::Bold(t) | Span::Code(t) => t,
                    })
                    .collect();
                out.push(Span::Bold(inner));
                rest = &after[end + 2..];
                continue;
            }
        }
        if let Some(after) = rest.strip_prefix('`') {
            if let Some(end) = after.find('`') {
                push(&mut out, &mut plain);
                out.push(Span::Code(after[..end].to_string()));
                rest = &after[end + 1..];
                continue;
            }
        }
        let c = rest.chars().next().unwrap();
        plain.push(c);
        rest = &rest[c.len_utf8()..];
    }
    push(&mut out, &mut plain);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Заголовки до третьего уровня размечаются, четвёртый — уже абзац.
    /// Общий разбор ответа модели для окна и TUI (`agr:markup-shared`).
    #[test]
    fn headings_stop_at_three() {
        let b = parse("# один\n## два\n### три\n#### четыре");
        assert_eq!(
            b,
            vec![
                Block::Heading {
                    level: 1,
                    text: "один".into()
                },
                Block::Heading {
                    level: 2,
                    text: "два".into()
                },
                Block::Heading {
                    level: 3,
                    text: "три".into()
                },
                Block::Paragraph("#### четыре".into()),
            ]
        );
    }

    /// Строки абзаца склеиваются, пустая строка его разделяет.
    #[test]
    fn paragraph_lines_join_and_split() {
        let b = parse("первая\nвторая\n\nтретий");
        assert_eq!(
            b,
            vec![
                Block::Paragraph("первая вторая".into()),
                Block::Paragraph("третий".into()),
            ]
        );
    }

    /// Списки: маркеры `-`/`*`/`+`, нумерация и вложенность по отступу.
    #[test]
    fn lists_and_nesting() {
        let b = parse("- раз\n  - вложено\n1. первый\n2. второй");
        assert_eq!(
            b,
            vec![
                Block::Bullet {
                    depth: 0,
                    text: "раз".into()
                },
                Block::Bullet {
                    depth: 1,
                    text: "вложено".into()
                },
                Block::Numbered {
                    depth: 0,
                    number: "1.".into(),
                    text: "первый".into()
                },
                Block::Numbered {
                    depth: 0,
                    number: "2.".into(),
                    text: "второй".into()
                },
            ]
        );
    }

    /// Блок кода берёт язык из ``` и не размечает внутри.
    #[test]
    fn code_block_keeps_lines() {
        let b = parse("до\n```lua\nreturn seq > 5\n```\nпосле");
        assert_eq!(
            b,
            vec![
                Block::Paragraph("до".into()),
                Block::Code {
                    lang: "lua".into(),
                    lines: vec!["return seq > 5".into()]
                },
                Block::Paragraph("после".into()),
            ]
        );
    }

    /// Незакрытый блок кода идёт до конца текста.
    #[test]
    fn unterminated_code_runs_to_the_end() {
        let b = parse("```\nстрока");
        assert_eq!(
            b,
            vec![Block::Code {
                lang: String::new(),
                lines: vec!["строка".into()]
            }]
        );
    }

    /// Пустой текст — ни одного блока.
    #[test]
    fn empty_text_has_no_blocks() {
        assert!(parse("").is_empty());
        assert!(parse("\n\n").is_empty());
    }

    /// Таблица: шапка, разделитель и строки; строки не склеиваются в абзац.
    #[test]
    fn table_parses_header_separator_and_rows() {
        let b =
            parse("до\n| Параметр | Значение |\n|---|---|\n| file | a.dat |\n| seq | 5 |\n\nпосле");
        assert_eq!(
            b,
            vec![
                Block::Paragraph("до".into()),
                Block::Table {
                    header: vec!["Параметр".into(), "Значение".into()],
                    rows: vec![
                        vec!["file".into(), "a.dat".into()],
                        vec!["seq".into(), "5".into()],
                    ]
                },
                Block::Paragraph("после".into()),
            ]
        );
    }

    /// Выравнивание в разделителе (`:--`, `--:`) — тоже таблица.
    #[test]
    fn table_separator_with_alignment() {
        let b = parse("| a | b |\n|:-:|--:|\n| 1 | 2 |");
        assert_eq!(
            b,
            vec![Block::Table {
                header: vec!["a".into(), "b".into()],
                rows: vec![vec!["1".into(), "2".into()]],
            }]
        );
    }

    /// Строки с `|` без разделителя — не таблица: остаются текстом абзаца.
    #[test]
    fn pipe_lines_without_separator_stay_paragraph() {
        let b = parse("| просто |\n| текст |");
        assert_eq!(b, vec![Block::Paragraph("| просто | | текст |".into())]);
    }

    /// Внутри строки размечаются жирный и код, остальное — текст.
    #[test]
    fn inline_spans() {
        assert_eq!(
            spans("до **жирный** и `код` после"),
            vec![
                Span::Text("до ".into()),
                Span::Bold("жирный".into()),
                Span::Text(" и ".into()),
                Span::Code("код".into()),
                Span::Text(" после".into()),
            ]
        );
    }

    /// `agr:markup-shared`
    /// Код внутри жирного — жирный без обратных кавычек.
    #[test]
    fn code_inside_bold_is_bold_without_backticks() {
        assert_eq!(
            spans("**`Money`** (msgid 170) и **`a` и `b`**"),
            vec![
                Span::Bold("Money".into()),
                Span::Text(" (msgid 170) и ".into()),
                Span::Bold("a и b".into()),
            ]
        );
    }

    /// Незакрытые маркеры остаются текстом, а не съедают строку.
    #[test]
    fn unclosed_markers_stay_text() {
        assert_eq!(spans("**нет конца"), vec![Span::Text("**нет конца".into())]);
        assert_eq!(spans("`нет конца"), vec![Span::Text("`нет конца".into())]);
        assert!(spans("").is_empty());
    }
}
