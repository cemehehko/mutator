//! Параллельный проход по кускам seq-диапазона (`agr:chunks`).
//!
//! file-канал ищет seq бисекцией блоков, поэтому кусок `[a, b)` открывается сразу
//! с `seq=a` — и N потоков читают файл одновременно, каждый в своём контексте
//! libtll. Потоки берут куски по порядку, а результаты отдаются вызывающему строго
//! по возрастанию номера: так индекс и фильтр растут непрерывным префиксом, а
//! поиск находит ближайшее совпадение, не дожидаясь остальных кусков.
//!
//! Наружу: [`Chunk`], [`even`], [`growing`], [`BYTES_PER_THREAD`], [`threads`],
//! [`count`], [`run`], [`run_state`].

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::time::Duration;

use crate::tll::Context;

/// Кусок seq: `[from, to)`.
pub type Chunk = (i64, i64);

/// Равные куски `[begin, end]` — примерно `n` штук.
pub fn even(begin: i64, end: i64, n: usize) -> Vec<Chunk> {
    if end < begin {
        return Vec::new();
    }
    let span = (end - begin) as i128 + 1;
    let n = (n.max(1) as i128).min(span);
    (0..n)
        .map(|i| {
            let a = begin as i128 + span * i / n;
            let b = begin as i128 + span * (i + 1) / n;
            (a as i64, b as i64)
        })
        .collect()
}

/// Куски, растущие от `from` геометрически — поиск сначала смотрит рядом.
/// `back` — от `from` к `limit` вниз (куски идут по убыванию seq).
pub fn growing(from: i64, limit: i64, first: i64, max: i64, back: bool) -> Vec<Chunk> {
    let mut out = Vec::new();
    let mut size = first.max(1);
    let mut at = from;
    loop {
        if back {
            if at <= limit {
                break;
            }
            let a = (at - size).max(limit);
            out.push((a, at));
            at = a;
        } else {
            if at > limit {
                break;
            }
            let b = at.saturating_add(size).min(limit.saturating_add(1));
            out.push((at, b));
            at = b;
        }
        size = (size * 2).min(max.max(1));
    }
    out
}

/// Байт файла на поток: меньше — потоки не окупают открытие каналов.
pub const BYTES_PER_THREAD: u64 = 64 << 20;

/// Сколько потоков читать файл `bytes`: ядра (или `TUI_PATCH_THREADS`), не
/// больше кусков и не больше, чем окупится по размеру.
pub fn threads(chunks: usize, bytes: u64) -> usize {
    let by_size = (1 + bytes / BYTES_PER_THREAD) as usize;
    let cores = std::env::var("TUI_PATCH_THREADS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
                .min(by_size)
        });
    cores.min(chunks).max(1)
}

/// Сколько кусков резать: по нескольку на поток, чтобы префикс рос ровно.
pub fn count(bytes: u64, per_thread: usize) -> usize {
    threads(usize::MAX, bytes) * per_thread
}

/// Пройти куски параллельно. `work` читает кусок в контексте своего потока и
/// отдаёт читателю флаг `halt`; `ready` получает результаты по порядку и
/// возвращает `false`, чтобы остановить остальные. `stop` — внешняя отмена: проход
/// её только читает, иначе вызывающий не отличил бы отмену от «нашли, хватит».
pub fn run<R: Send>(
    chunks: &[Chunk],
    bytes: u64,
    stop: &Arc<AtomicBool>,
    work: impl Fn(&Context, Chunk, &Arc<AtomicBool>) -> R + Sync,
    ready: impl FnMut(usize, R) -> bool,
) {
    run_state(
        chunks,
        bytes,
        stop,
        |_| (),
        |ctx, (), c, halt| work(ctx, c, halt),
        ready,
    )
}

/// Как [`run`], но у каждого потока своё состояние `S`, живущее вместе с его
/// контекстом: `init` зовётся один раз на поток до кусков, `work` получает
/// `&mut S` (не деля его между потоками). Состояние дропается до того, как
/// контекст вернётся в пул — так `Transformer` на поток можно переиспользовать
/// между кусками, не создавая `lua+direct` заново (`agr:filter-post-eval`).
pub fn run_state<R: Send, S>(
    chunks: &[Chunk],
    bytes: u64,
    stop: &Arc<AtomicBool>,
    init: impl Fn(&Context) -> S + Sync,
    work: impl Fn(&Context, &mut S, Chunk, &Arc<AtomicBool>) -> R + Sync,
    mut ready: impl FnMut(usize, R) -> bool,
) {
    let halt = Arc::new(AtomicBool::new(false));
    let next = AtomicUsize::new(0);
    let (tx, rx) = mpsc::channel::<(usize, R)>();
    std::thread::scope(|s| {
        for _ in 0..threads(chunks.len(), bytes) {
            let tx = tx.clone();
            // `init` живёт у потока по ссылке: Sync даёт общий доступ, а для
            // spawn нужен Send — по значению тип `impl Fn + Sync` не уезжает.
            let (next, work, halt, init) = (&next, &work, halt.clone(), &init);
            s.spawn(move || {
                let ctx = Context::pooled();
                let mut state = init(&ctx);
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= chunks.len() || halt.load(Ordering::Relaxed) {
                        break;
                    }
                    let r = work(&ctx, &mut state, chunks[i], &halt);
                    if tx.send((i, r)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);
        let mut pending = BTreeMap::new();
        let mut want = 0;
        let mut go = true;
        loop {
            let got = match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(x) => Some(x),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            };
            if stop.load(Ordering::Relaxed) {
                halt.store(true, Ordering::Relaxed);
                go = false;
            }
            let Some((i, r)) = got else { continue };
            if !go {
                continue;
            }
            pending.insert(i, r);
            while let Some(r) = pending.remove(&want) {
                if !ready(want, r) {
                    halt.store(true, Ordering::Relaxed);
                    go = false;
                    break;
                }
                want += 1;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Куски покрывают диапазон без дыр и наложений, в обе стороны.
    #[test]
    fn chunks_cover_range_without_gaps() {
        let c = even(5, 104, 7);
        assert_eq!(c.first().unwrap().0, 5);
        assert_eq!(c.last().unwrap().1, 105);
        assert!(c.windows(2).all(|w| w[0].1 == w[1].0));
        assert_eq!(even(3, 3, 10), vec![(3, 4)]);
        assert!(even(5, 4, 3).is_empty());

        let f = growing(10, 100, 4, 16, false);
        assert_eq!(f[0], (10, 14));
        assert_eq!(f.last().unwrap().1, 101);
        assert!(f.windows(2).all(|w| w[0].1 == w[1].0));
        let b = growing(100, 0, 4, 16, true);
        assert_eq!(b[0], (96, 100));
        assert_eq!(b.last().unwrap().0, 0);
        assert!(b.windows(2).all(|w| w[0].0 == w[1].1));
    }

    /// `agr:chunks` — результаты приходят строго по порядку кусков, `false`
    /// останавливает проход.
    #[test]
    fn results_arrive_in_chunk_order() {
        let chunks = even(0, 999, 50);
        let stop = Arc::new(AtomicBool::new(false));
        let mut got = Vec::new();
        run(
            &chunks,
            u64::MAX,
            &stop,
            |_, (a, _), _| a,
            |i, a| {
                got.push((i, a));
                i < 20
            },
        );
        assert_eq!(got.len(), 21);
        assert!(got
            .iter()
            .enumerate()
            .all(|(n, (i, a))| *i == n && *a == chunks[n].0));
    }

    /// `agr:filter-post-eval` — состояние (`Transformer`) строится один раз на
    /// поток-воркер, а не на кусок: `init` зовут `threads(...)` раз, по куску
    /// работает `work`.
    #[test]
    fn state_is_built_once_per_thread() {
        let chunks = even(0, 999, 64);
        let stop = Arc::new(AtomicBool::new(false));
        let inits = Arc::new(AtomicUsize::new(0));
        let works = Arc::new(AtomicUsize::new(0));
        run_state(
            &chunks,
            u64::MAX, // байтов с запасом — потоков больше одного
            &stop,
            {
                let inits = inits.clone();
                move |_| inits.fetch_add(1, Ordering::Relaxed)
            },
            {
                let works = works.clone();
                move |_, _, _, _| {
                    works.fetch_add(1, Ordering::Relaxed);
                    ()
                }
            },
            |_, _| true,
        );
        assert_eq!(
            inits.load(Ordering::Relaxed),
            threads(chunks.len(), u64::MAX),
            "init — один раз на поток"
        );
        assert_eq!(
            works.load(Ordering::Relaxed),
            chunks.len(),
            "work — по разу на кусок"
        );
    }
}
