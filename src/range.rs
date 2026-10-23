//! Границы в стиле `tll-read -s BEGIN[:[END]]` — для `--dump` и перехода `g`.
//!
//! `SEQ` — номер сообщения, `+N` — N сообщений (для начала — пропустить N),
//! `-N` — последний seq минус N (только начало).
//!
//! Наружу: [`Border`], [`Range`].

/// Одна граница.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Border {
    Seq(i64),
    Count(u64),
    FromLast(u64),
}

impl Border {
    pub fn parse(s: &str) -> Result<Border, String> {
        let s = s.trim();
        let num = |t: &str| t.parse::<u64>().map_err(|_| format!("не число: {s}"));
        if let Some(t) = s.strip_prefix('+') {
            Ok(Border::Count(num(t)?))
        } else if let Some(t) = s.strip_prefix('-') {
            Ok(Border::FromLast(num(t)?))
        } else {
            s.parse::<i64>()
                .map(Border::Seq)
                .map_err(|_| format!("не число: {s}"))
        }
    }
}

/// Диапазон `BEGIN[:[END]]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub begin: Option<Border>,
    pub end: Option<Border>,
    /// Одна точка без двоеточия: только сообщение BEGIN.
    pub single: bool,
}

impl Default for Range {
    fn default() -> Self {
        Range::all()
    }
}

impl Range {
    pub fn all() -> Self {
        Range {
            begin: None,
            end: None,
            single: false,
        }
    }

    pub fn parse(s: &str) -> Result<Range, String> {
        match s.split_once(':') {
            None => Ok(Range {
                begin: Some(Border::parse(s)?),
                end: None,
                single: true,
            }),
            Some((b, e)) => {
                let begin = if b.is_empty() {
                    None
                } else {
                    Some(Border::parse(b)?)
                };
                let end = if e.is_empty() {
                    None
                } else {
                    Some(Border::parse(e)?)
                };
                if matches!(end, Some(Border::FromLast(_))) {
                    return Err("-N можно только в начале".into());
                }
                Ok(Range {
                    begin,
                    end,
                    single: false,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Разбор `-s` как в tll-read: SEQ, +N, -N и формы с двоеточием.
    #[test]
    fn ranges_parse_like_tll_read() {
        assert_eq!(
            Range::parse("10").unwrap(),
            Range {
                begin: Some(Border::Seq(10)),
                end: None,
                single: true
            }
        );
        assert_eq!(
            Range::parse("+1:").unwrap(),
            Range {
                begin: Some(Border::Count(1)),
                end: None,
                single: false
            }
        );
        assert_eq!(
            Range::parse(":+3").unwrap(),
            Range {
                begin: None,
                end: Some(Border::Count(3)),
                single: false
            }
        );
        assert_eq!(Range::parse("-0").unwrap().begin, Some(Border::FromLast(0)));
        assert!(Range::parse("1:-2").is_err());
        assert!(Range::parse("x").is_err());
    }
}
