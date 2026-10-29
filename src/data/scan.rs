//! Фильтр и поиск по всему файлу, параллельно по кускам.
//!
//! Фильтр копит seq совпадений в [`Matches`] — общий для скана, воркера окна и
//! интерфейса; список растёт непрерывным префиксом файла, так что таблица
//! показывает найденное сразу (`agr:filter-streams`). Поиск идёт кусками,
//! растущими от текущей строки, и отдаёт ближайшее совпадение в нужную сторону.
//!
//! Порядок — полный конвейер (`agr:filter-post-eval`): кусок читается через
//! [`File::source_transform`] (`patch+lua+file` — transform F4 поверх ручных
//! правок), типы отбираются в Rust по `msgid` (без Lua, сохраняет
//! `meta`-оптимизацию), а выражение `f`/`t` прогоняется пост-эвалом через пару
//! `lua+direct` ([`Transformer::filter`]). Так фильтр и поиск видят данные
//! после transform и правок, а не сырой файл.
//!
//! Склейка (`agr:glue-parts`): куски режутся по частям (по байтам каждой) и
//! идут в порядке частей — seq совпадений остаются по возрастанию, а поиск
//! переходит через границы частей: сперва растущие окна в своей части, потом
//! куски соседних.
//!
//! [`Transformer`] создаётся один раз на worker-поток (не на кусок) — см.
//! [`super::chunks::run_state`].
//!
//! Наружу: [`Matches`], [`filter`], [`search`].

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use super::chunks::{self, Chunk};
use super::fetch::Src;
use super::File;
use crate::io::{Context, Reader, Transformer};
use crate::lua::Filter;

/// Результат фильтра — растёт, пока идёт скан.
#[derive(Debug, Default)]
pub struct Matches {
    /// seq совпадений по возрастанию.
    pub seqs: RwLock<Vec<i64>>,
    /// Скан закончен (успешно, с ошибкой или отменён).
    pub done: AtomicBool,
    /// Готовая доля, в тысячных.
    pub progress: AtomicU32,
    pub error: Mutex<Option<String>>,
}

impl Matches {
    pub fn len(&self) -> usize {
        self.seqs.read().unwrap().len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }
    pub fn progress(&self) -> f32 {
        self.progress.load(Ordering::Relaxed) as f32 / 1000.0
    }
    /// seq строк `[from, from+n)`.
    pub fn slice(&self, from: usize, n: usize) -> Vec<i64> {
        let s = self.seqs.read().unwrap();
        s[from.min(s.len())..(from + n).min(s.len())].to_vec()
    }
    /// Номер строки первого совпадения с seq ≥ `seq`.
    pub fn row_of(&self, seq: i64) -> usize {
        self.seqs.read().unwrap().partition_point(|&s| s < seq)
    }
}

/// msgid'ы типов из `filter.messages`, отобранные по схеме файла; `None` — все
/// типы. Имя, которого нет в схеме, не попадает в набор: фильтр по
/// несуществующему типу не находит ничего.
fn wanted_msgids(f: &File, filter: &Filter) -> Option<HashSet<i32>> {
    if filter.messages.is_empty() {
        return None;
    }
    let mut set = HashSet::new();
    for name in &filter.messages {
        if let Some(st) = f.scheme.messages().find(|s| &s.name == name) {
            set.insert(st.msgid);
        }
    }
    Some(set)
}

/// Состояние worker-потока: выражение фильтра как пара `lua+direct`. `None` —
/// выражения нет, отбираются только типы в Rust. Создаётся один раз на поток.
struct Eval {
    tf: Option<Transformer>,
}

/// Завод [`Eval`]: код выражения и схема файла (для имён в Lua).
struct EvalFactory {
    backend: crate::io::Backend,
    code: Option<String>,
    scheme_url: Option<String>,
}

impl EvalFactory {
    fn build(&self, ctx: &Context) -> Result<Eval, String> {
        let tf = match (&self.code, &self.scheme_url) {
            (Some(code), Some(url)) => Some(Transformer::filter(ctx, self.backend, code, url)?),
            _ => None,
        };
        Ok(Eval { tf })
    }
}

/// Прочитать кусок `[a, b)` части `src` через transform+правки и отдать seq
/// совпадений: типы — в Rust, выражение — пост-эвалом через [`Eval`].
fn chunk(
    ctx: &Context,
    f: &File,
    filter: &Filter,
    src: &Src,
    eval: &mut Eval,
    (a, b): Chunk,
    stop: &Arc<AtomicBool>,
) -> Result<Vec<i64>, String> {
    let wanted = wanted_msgids(f, filter);

    // Тела нужны только выражению; при отборе одних типов читаем meta.
    let mut open = src.open.clone().at(a);
    if eval.tf.is_none() {
        open = open.meta();
    }
    let mut r = Reader::open(ctx, &open)?.with_stop(stop.clone());

    let mut out = Vec::new();
    for m in r.by_ref() {
        if m.seq >= b {
            break;
        }
        if wanted.as_ref().is_some_and(|w| !w.contains(&m.msgid)) {
            continue;
        }
        match eval.tf.as_mut() {
            Some(t) => {
                if t.transform(&m)?.is_some() {
                    out.push(m.seq);
                }
            }
            None => out.push(m.seq),
        }
    }
    // Ошибка выражения на сообщении обрывает кусок — это ошибка фильтра,
    // а не «найдено меньше».
    match r.error() {
        Some(e) => Err(e),
        None => Ok(out),
    }
}

/// Куски всех частей по порядку: `(кусок, часть)`; байты — для числа потоков.
fn plan_all(f: &File, per_thread: usize) -> (Vec<Src>, Vec<(Chunk, usize)>, u64) {
    let srcs = f.source_transform();
    let mut list = Vec::new();
    let mut bytes_all = 0u64;
    for (i, s) in srcs.iter().enumerate() {
        let bytes = std::fs::metadata(&s.open.path).map(|m| m.len()).unwrap_or(0);
        bytes_all += bytes;
        list.extend(
            chunks::even(s.begin, s.last, chunks::count(bytes, per_thread))
                .into_iter()
                .map(|c| (c, i)),
        );
    }
    (srcs, list, bytes_all)
}

/// Отфильтровать файл в `m`; `tick` — после каждого готового куска.
pub fn filter(
    f: &File,
    filter: &Filter,
    stop: &Arc<AtomicBool>,
    m: &Matches,
    mut tick: impl FnMut(),
) {
    let (srcs, list, bytes) = plan_all(f, 16);
    let ranges: Vec<Chunk> = list.iter().map(|&(c, _)| c).collect();
    let n = ranges.len().max(1);
    let factory = EvalFactory {
        backend: f.info.backend,
        code: filter.expr_code_for(f.info.backend),
        scheme_url: f.scheme_url.clone(),
    };
    chunks::run_state(
        &ranges,
        bytes,
        stop,
        |ctx| factory.build(ctx).ok(),
        |ctx, eval, i, c, halt| {
            // Трансформер не собрался — ошибка выражения; кусок отдаёт ошибку.
            let Some(eval) = eval else {
                return Err(factory
                    .build(ctx)
                    .err()
                    .unwrap_or_else(|| "Lua недоступен: нет модуля tll-lua (MUTATOR_LUA)".into()));
            };
            chunk(ctx, f, filter, &srcs[list[i].1], eval, c, halt)
        },
        |i, r| {
            let ok = match r {
                Ok(seqs) => {
                    m.seqs.write().unwrap().extend(seqs);
                    m.progress
                        .store(((i + 1) * 1000 / n) as u32, Ordering::Relaxed);
                    true
                }
                Err(e) => {
                    // Найденное до ошибки — неполное: показывать его как результат нельзя.
                    m.seqs.write().unwrap().clear();
                    *m.error.lock().unwrap() = Some(e);
                    false
                }
            };
            tick();
            ok
        },
    );
    m.done.store(true, Ordering::Release);
    tick();
}

/// Ближайшее совпадение после `from` (или до него — `back`).
pub fn search(
    f: &File,
    filter: &Filter,
    from: i64,
    back: bool,
    stop: &Arc<AtomicBool>,
) -> Result<Option<i64>, String> {
    let srcs = f.source_transform();
    // Растущие окна от `from` в своей части, потом куски соседних — по порядку
    // удаления от `from`, чтобы первое совпадение было ближайшим.
    let mut list: Vec<(Chunk, usize)> = Vec::new();
    let mut bytes_all = 0u64;
    // Часть с `from`; за концом всего — последняя (назад искать можно оттуда).
    let here = match srcs.iter().position(|s| s.last >= from) {
        Some(i) => i,
        None => srcs.len().saturating_sub(1),
    };
    let order: Vec<usize> = if back {
        (0..=here).rev().collect()
    } else {
        (here..srcs.len()).collect()
    };
    for (k, &i) in order.iter().enumerate() {
        let s = &srcs[i];
        let bytes = std::fs::metadata(&s.open.path).map(|m| m.len()).unwrap_or(0);
        bytes_all += bytes;
        let span = (s.last - s.begin).max(1);
        let max = (span / chunks::count(bytes, 4) as i64).max(4096);
        let ranges = if k == 0 {
            if back {
                chunks::growing(from, s.begin, 4096, max, true)
            } else {
                chunks::growing(from + 1, s.last, 4096, max, false)
            }
        } else {
            chunks::even(s.begin, s.last, chunks::count(bytes, 4))
        };
        list.extend(ranges.into_iter().map(|c| (c, i)));
    }
    let ranges: Vec<Chunk> = list.iter().map(|&(c, _)| c).collect();
    let factory = EvalFactory {
        backend: f.info.backend,
        code: filter.expr_code_for(f.info.backend),
        scheme_url: f.scheme_url.clone(),
    };
    let mut found = None;
    let mut err = None;
    chunks::run_state(
        &ranges,
        bytes_all,
        stop,
        |ctx| factory.build(ctx).ok(),
        |ctx, eval, i, c, halt| {
            let Some(eval) = eval else {
                return Err(factory
                    .build(ctx)
                    .err()
                    .unwrap_or_else(|| "Lua недоступен: нет модуля tll-lua (MUTATOR_LUA)".into()));
            };
            chunk(ctx, f, filter, &srcs[list[i].1], eval, c, halt)
        },
        |_, r| match r {
            Ok(seqs) => {
                let hit = if back { seqs.last() } else { seqs.first() };
                found = hit.copied();
                found.is_none()
            }
            Err(e) => {
                err = Some(e);
                false
            }
        },
    );
    match err {
        Some(e) => Err(e),
        None => Ok(found),
    }
}
