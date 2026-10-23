//! Индекс файла: номер строки ↔ seq без хранения всех seq (`agr:sparse-index`).
//!
//! Строим в фоне, параллельно по кускам ([`super::chunks`]). Каждый кусок
//! отдаёт число сообщений, seq каждого [`STEP`]-го от своего начала и счётчики
//! типов; склейка даёт отсортированные контрольные точки (строка, seq) с шагом не
//! больше [`STEP`]. Память — O(сообщений / STEP): гигабайтный файл — десятки КБ.
//!
//! Наружу: [`Index`], [`build`], [`STEP`].

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use super::chunks::{self, Chunk};
use crate::tll::{Context, Open, Reader};

/// Шаг контрольных точек: дойти от точки до любой строки — не больше стольких чтений.
pub const STEP: usize = 1024;

/// Готовый индекс.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Index {
    /// (номер строки, seq) — по возрастанию, первая — (0, первый seq).
    pub points: Vec<(usize, i64)>,
    /// Всего сообщений.
    pub total: usize,
    /// Сообщений каждого msgid.
    pub hist: HashMap<i32, usize>,
}

impl Index {
    /// Ближайшая точка не дальше строки `row`: (строка точки, её seq).
    pub fn point_for_row(&self, row: usize) -> Option<(usize, i64)> {
        let i = self.points.partition_point(|&(r, _)| r <= row);
        i.checked_sub(1).map(|i| self.points[i])
    }

    /// Ближайшая точка не дальше seq.
    pub fn point_for_seq(&self, seq: i64) -> Option<(usize, i64)> {
        let i = self.points.partition_point(|&(_, s)| s <= seq);
        i.checked_sub(1).map(|i| self.points[i])
    }
}

struct Part {
    count: usize,
    points: Vec<(usize, i64)>,
    hist: HashMap<i32, usize>,
}

fn part(ctx: &Context, path: &Path, (a, b): Chunk, stop: &Arc<AtomicBool>) -> Result<Part, String> {
    let mut p = Part {
        count: 0,
        points: Vec::new(),
        hist: HashMap::new(),
    };
    let r = Reader::open(ctx, &Open::file(path).at(a).meta())?.with_stop(stop.clone());
    for m in r {
        if m.seq >= b {
            break;
        }
        if p.count.is_multiple_of(STEP) {
            p.points.push((p.count, m.seq));
        }
        *p.hist.entry(m.msgid).or_default() += 1;
        p.count += 1;
    }
    Ok(p)
}

/// Построить индекс файла с seq `[begin, last]`. `progress` — доля готового.
pub fn build(
    path: &Path,
    begin: i64,
    last: i64,
    stop: &Arc<AtomicBool>,
    mut progress: impl FnMut(f32),
) -> Result<Index, String> {
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let list = chunks::even(begin, last, chunks::count(bytes, 8));
    let mut idx = Index::default();
    let mut err = None;
    let n = list.len().max(1);
    chunks::run(
        &list,
        bytes,
        stop,
        |ctx, c, halt| part(ctx, path, c, halt),
        |i, p| match p {
            Ok(p) => {
                let base = idx.total;
                idx.points
                    .extend(p.points.iter().map(|&(r, s)| (base + r, s)));
                idx.total += p.count;
                for (k, v) in p.hist {
                    *idx.hist.entry(k).or_default() += v;
                }
                progress((i + 1) as f32 / n as f32);
                true
            }
            Err(e) => {
                err = Some(e);
                false
            }
        },
    );
    if let Some(e) = err {
        return Err(e);
    }
    if stop.load(std::sync::atomic::Ordering::Relaxed) {
        return Err("отменено".into());
    }
    Ok(idx)
}
