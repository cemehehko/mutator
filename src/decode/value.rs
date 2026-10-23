//! Скалярные значения и их текст — ровно как у `tll-read` (python-tll):
//! fixed и decimal128 печатаются по правилам `str(Decimal)`, время — ISO с
//! долями 3/6/9 знаков по самому значению, длительность — число с суффиксом.
//!
//! Наружу: [`Value`], [`double`], [`bytes`], [`decimal`], [`decimal128`],
//! [`time_point`].

use crate::scheme::TimeRes;

/// Скаляр поля.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i128),
    Double(f64),
    /// Десятичное: знак, коэффициент, порядок (fixed и decimal128).
    Dec {
        neg: bool,
        coeff: u128,
        exp: i32,
    },
    /// Особые значения decimal128: `Infinity`, `-Infinity`, `NaN`, `sNaN`.
    DecSpecial(&'static str),
    /// Значение перечисления; `name` — `None`, если числа нет в схеме.
    Enum {
        name: Option<String>,
        raw: i64,
    },
    /// Момент времени в наносекундах от эпохи.
    Time(i128),
    Duration {
        value: i128,
        res: TimeRes,
    },
    /// Все битовые поля: имя, значение, ширина; в тексте — только ненулевые.
    Bits(Vec<(String, u64, u32)>),
    Str(String),
    Bytes(Vec<u8>),
}

impl Value {
    /// Текст для ячейки и детали.
    pub fn text(&self) -> String {
        match self {
            Value::Int(v) => v.to_string(),
            Value::Double(v) => double(*v),
            Value::Dec { neg, coeff, exp } => decimal(*neg, *coeff, *exp),
            Value::DecSpecial(s) => s.to_string(),
            Value::Enum { name: Some(n), .. } => n.clone(),
            Value::Enum { name: None, raw } => raw.to_string(),
            Value::Time(ns) => time_point(*ns),
            Value::Duration { value, res } => format!("{value}{}", res.suffix()),
            Value::Bits(b) => {
                let parts: Vec<_> = b
                    .iter()
                    .filter(|(_, v, _)| *v != 0)
                    .map(|(n, v, w)| {
                        if *w == 1 {
                            n.clone()
                        } else {
                            format!("{n}={v}")
                        }
                    })
                    .collect();
                parts.join("|")
            }
            Value::Str(s) => s.clone(),
            Value::Bytes(b) => bytes(b),
        }
    }
}

/// `float` в Python: целое значение — с `.0`.
pub fn double(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e16 {
        format!("{v:.1}")
    } else {
        format!("{v}")
    }
}

/// Байты как в tll-read: ASCII — строкой, иначе hex.
pub fn bytes(b: &[u8]) -> String {
    if b.iter().all(|&x| x < 0x80) {
        b.iter()
            .map(|&c| {
                if c == 0 {
                    "\\0".to_string()
                } else {
                    (c as char).to_string()
                }
            })
            .collect()
    } else {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }
}

/// `str(Decimal)` из Python: обычная запись, пока порядок не положителен и
/// число не слишком мелкое, иначе — научная.
pub fn decimal(neg: bool, coeff: u128, exp: i32) -> String {
    let digits = coeff.to_string();
    let n = digits.len() as i64;
    let exp = exp as i64;
    let left = exp + n;
    let dot = if exp <= 0 && left > -6 { left } else { 1 };
    let (int, frac) = if dot <= 0 {
        (
            "0".to_string(),
            format!(".{}{}", "0".repeat((-dot) as usize), digits),
        )
    } else if dot >= n {
        (
            format!("{}{}", digits, "0".repeat((dot - n) as usize)),
            String::new(),
        )
    } else {
        (
            digits[..dot as usize].to_string(),
            format!(".{}", &digits[dot as usize..]),
        )
    };
    let e = if left == dot {
        String::new()
    } else {
        format!("E{:+}", left - dot)
    };
    format!("{}{int}{frac}{e}", if neg { "-" } else { "" })
}

/// Разбор decimal128 (BID, little-endian) в [`Value`].
pub fn decimal128(b: &[u8; 16]) -> Value {
    let v = u128::from_le_bytes(*b);
    let neg = v >> 127 == 1;
    let top = (v >> 122) & 0x1f;
    if top == 0x1e {
        return Value::DecSpecial(if neg { "-Infinity" } else { "Infinity" });
    }
    if top == 0x1f {
        return Value::DecSpecial(if (v >> 121) & 1 == 1 { "sNaN" } else { "NaN" });
    }
    let (exp, coeff) = if (v >> 125) & 3 == 3 {
        // Коэффициент вне канонического диапазона — по стандарту ноль.
        (((v >> 111) & 0x3fff) as i32, 0)
    } else {
        (((v >> 113) & 0x3fff) as i32, v & ((1u128 << 113) - 1))
    };
    let coeff = if coeff >= 10u128.pow(34) { 0 } else { coeff };
    Value::Dec {
        neg,
        coeff,
        exp: exp - 6176,
    }
}

/// Момент времени как `TimePoint.__str__` python-tll.
pub fn time_point(ns: i128) -> String {
    let secs = ns.div_euclid(1_000_000_000);
    let frac = ns.rem_euclid(1_000_000_000);
    let days = secs.div_euclid(86_400) as i64;
    let sod = secs.rem_euclid(86_400) as i64;
    let (y, m, d) = civil(days);
    let base = format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}",
        sod / 3600,
        sod / 60 % 60,
        sod % 60
    );
    if frac == 0 {
        format!("{base}Z")
    } else if frac % 1_000_000 == 0 {
        format!("{base}.{:03}Z", frac / 1_000_000)
    } else if frac % 1_000 == 0 {
        format!("{base}.{:06}Z", frac / 1_000)
    } else {
        format!("{base}.{frac:09}Z")
    }
}

/// Дни от эпохи → (год, месяц, день); алгоритм Хиннанта.
fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Десятичные печатаются как `str(Decimal)` в Python — так их видит tll-read.
    #[test]
    fn decimals_print_like_python_decimal() {
        assert_eq!(decimal(false, 27150000000, -8), "271.50000000");
        assert_eq!(decimal(false, 0, -8), "0E-8");
        assert_eq!(decimal(false, 0, -6176), "0E-6176");
        assert_eq!(decimal(false, 300000000, -2), "3000000.00");
        assert_eq!(decimal(true, 5, -3), "-0.005");
        assert_eq!(decimal(false, 5, -7), "5E-7");
        assert_eq!(decimal(false, 12, 2), "1.2E+3");
        assert_eq!(decimal(false, 0, 0), "0");
        assert_eq!(decimal(false, 1, -6), "0.000001");
    }

    /// decimal128 читается из BID: порядок со смещением 6176, ноль — `0E-6176`.
    #[test]
    fn decimal128_reads_bid() {
        assert_eq!(decimal128(&[0; 16]).text(), "0E-6176");
        let v: u128 = (6174u128 << 113) | 300000000;
        assert_eq!(decimal128(&v.to_le_bytes()).text(), "3000000.00");
        let inf: u128 = 0x1e << 122;
        assert_eq!(decimal128(&inf.to_le_bytes()).text(), "Infinity");
    }

    /// Время — ISO в UTC с долями по значению, как `TimePoint.__str__`.
    #[test]
    fn time_points_print_like_tll_read() {
        assert_eq!(
            time_point(1_790_084_487_562_819_000),
            "2026-09-22T13:41:27.562819Z"
        );
        assert_eq!(
            time_point(1_790_084_487_562_000_000),
            "2026-09-22T13:41:27.562Z"
        );
        assert_eq!(
            time_point(1_790_084_487_000_000_000),
            "2026-09-22T13:41:27Z"
        );
        assert_eq!(time_point(1), "1970-01-01T00:00:00.000000001Z");
        assert_eq!(time_point(0), "1970-01-01T00:00:00Z");
    }
}
