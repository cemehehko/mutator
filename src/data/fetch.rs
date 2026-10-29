//! Чтение окна строк: всегда несколько сотен сообщений, сколько бы ни весил файл.
//!
//! Вперёд — открыть канал с `seq=` (бисекция блоков в libtll) и прочитать N.
//! Назад — окно seq перед якорем, растущее вчетверо, пока не наберётся N
//! (seq бывают с дырами). По номеру строки — от ближайшей точки индекса. Найденное
//! фильтром — по списку seq, с переоткрытием на больших пропусках. Что
//! открывать — шаблоны [`Src`]: путь части и, если есть, правки поверх.
//!
//! Склейка (`agr:glue-parts`): файл — это упорядоченные части с восходящими
//! непересекающимися диапазонами seq; окно, не набравшее N, продолжается с
//! начала следующей части, назад — части проходятся справа налево. Пустая часть
//! (молодой `current.dat`) инертна: её `last` меньше любого seq. Одиночный
//! файл — срез из одной части, поведение прежнее.
//!
//! Для состава потока (`agr:stream-ops`, `agr:seq-forecast`) — сырые seq
//! файла: [`seqs_in`] — все seq диапазона и следующий за ним, [`alive_before`]
//! — последний неудалённый seq, чтением назад теми же растущими окнами.
//!
//! Наружу: [`Src`], [`from_seq`], [`before_seq`], [`from_row`], [`row_of`],
//! [`by_seqs`], [`seqs_in`], [`alive_before`], [`FileSeqs`].

use std::collections::VecDeque;

use super::index::Index;
use crate::io::{Context, Msg, Open, Reader};

/// Пропуск seq, после которого дешевле переоткрыть канал, чем читать подряд.
const REOPEN_GAP: i64 = 256;

/// Шаблон чтения одной части: канал (путь, правки, Lua) и границы seq части.
#[derive(Debug, Clone)]
pub struct Src {
    pub open: Open,
    pub begin: i64,
    pub last: i64,
}

impl Src {
    /// Первая часть, которая может держать seq (last ≥ seq); `None` — за концом.
    pub fn part_of(src: &[Src], seq: i64) -> Option<usize> {
        src.iter().position(|s| s.last >= seq)
    }
}

/// N сообщений начиная с первого seq ≥ `seq`.
pub fn from_seq(ctx: &Context, src: &[Src], seq: i64, n: usize) -> Result<Vec<Msg>, String> {
    let i = Src::part_of(src, seq).unwrap_or(src.len());
    read_forward(ctx, src, i, seq, 0, n)
}

/// N сообщений с номера строки `row` по индексу.
pub fn from_row(
    ctx: &Context,
    src: &[Src],
    idx: &Index,
    row: usize,
    n: usize,
) -> Result<Vec<Msg>, String> {
    let Some((prow, pseq)) = idx.point_for_row(row) else {
        return Ok(Vec::new());
    };
    let i = Src::part_of(src, pseq).unwrap_or(src.len());
    read_forward(ctx, src, i, pseq, row - prow, n)
}

/// Вперёд от части `i` с seq `seq` (пропустив `skip` сообщений) — до N или конца
/// склейки: не набралось в части — продолжить с начала следующей.
fn read_forward(
    ctx: &Context,
    src: &[Src],
    i: usize,
    seq: i64,
    mut skip: usize,
    n: usize,
) -> Result<Vec<Msg>, String> {
    let mut out = Vec::with_capacity(n);
    for s in &src[i.min(src.len())..] {
        if out.len() == n {
            break;
        }
        let mut r = Reader::open(ctx, &s.open.clone().at(seq))?;
        while out.len() < n {
            let Some(m) = r.next() else { break };
            if skip > 0 {
                skip -= 1;
            } else {
                out.push(m);
            }
        }
        if let Some(e) = r.error() {
            return Err(e);
        }
    }
    Ok(out)
}

/// До N сообщений строго перед `seq`, по возрастанию: части справа налево,
/// внутри части — окно, растущее вчетверо, пока не наберётся (seq бывают с дырами).
pub fn before_seq(
    ctx: &Context,
    src: &[Src],
    seq: i64,
    n: usize,
    begin: i64,
) -> Result<Vec<Msg>, String> {
    let mut out: Vec<Msg> = Vec::with_capacity(n);
    let mut upper = seq;
    for s in src.iter().rev() {
        if out.len() == n || upper <= begin {
            break;
        }
        // Верх диапазона в этой части: messages < upper; части ниже её начала
        // не смотрим — продолжаем следующей, более старой.
        let part_upper = upper.min(s.last.saturating_add(1));
        if part_upper <= s.begin {
            continue;
        }
        let got = before_in_part(ctx, s, part_upper, n - out.len(), begin.max(s.begin))?;
        out.splice(0..0, got);
        upper = s.begin;
    }
    Ok(out)
}

/// До N сообщений одной части строго перед `seq`, по возрастанию.
fn before_in_part(
    ctx: &Context,
    s: &Src,
    seq: i64,
    n: usize,
    begin: i64,
) -> Result<Vec<Msg>, String> {
    let mut w = (n as i64 * 2).max(64);
    loop {
        let lo = seq.saturating_sub(w).max(begin);
        let mut r = Reader::open(ctx, &s.open.clone().at(lo))?;
        let mut ring = VecDeque::with_capacity(n);
        for m in r.by_ref() {
            if m.seq >= seq {
                break;
            }
            if ring.len() == n {
                ring.pop_front();
            }
            ring.push_back(m);
        }
        if let Some(e) = r.error() {
            return Err(e);
        }
        if ring.len() == n || lo == begin {
            return Ok(ring.into());
        }
        w = w.saturating_mul(4);
    }
}

/// Номер строки первого сообщения с seq ≥ `seq`: от точки индекса, через части.
pub fn row_of(ctx: &Context, src: &[Src], idx: &Index, seq: i64) -> Result<usize, String> {
    let Some((prow, pseq)) = idx.point_for_seq(seq) else {
        return Ok(0);
    };
    let mut row = prow;
    for s in src {
        let start = pseq.max(s.begin);
        if s.last < start {
            continue;
        }
        let r = Reader::open(ctx, &s.open.clone().at(start).meta())?;
        for m in r {
            if m.seq >= seq {
                return Ok(row);
            }
            row += 1;
        }
    }
    Ok(row)
}

/// Сообщения с ровно этими seq (по возрастанию) — окно отфильтрованного вида.
pub fn by_seqs(ctx: &Context, src: &[Src], seqs: &[i64]) -> Result<Vec<Msg>, String> {
    let mut out = Vec::with_capacity(seqs.len());
    // Часть открытого канала и сам канал: переоткрытие — при смене части или
    // большом пропуске внутри неё.
    let mut r: Option<(usize, Reader)> = None;
    // Прочитанное, но ещё не нужное: оно может оказаться следующим искомым.
    let mut held: Option<Msg> = None;
    // seq последнего прочитанного: по нему решаем, читать подряд или переоткрыть.
    let mut pos = i64::MIN;
    for &want in seqs {
        let in_held = held.as_ref().is_some_and(|m| m.seq >= want);
        let reopen = match (&r, Src::part_of(src, want)) {
            (None, _) => true,
            (Some((i, _)), Some(j)) => {
                !in_held && (*i != j || want.saturating_sub(pos) > REOPEN_GAP)
            }
            (Some(_), None) => return Ok(out),
        };
        if reopen {
            if let Some((_, rd)) = r.as_mut() {
                if let Some(e) = rd.error() {
                    return Err(e);
                }
            }
            let j = Src::part_of(src, want).unwrap();
            r = Some((j, Reader::open(ctx, &src[j].open.clone().at(want))?));
            held = None;
        }
        let Some((_, rd)) = r.as_mut() else {
            return Ok(out);
        };
        loop {
            let m = match held.take() {
                Some(m) => m,
                None => match rd.next() {
                    Some(m) => {
                        pos = m.seq;
                        m
                    }
                    None => return Ok(out),
                },
            };
            if m.seq == want {
                out.push(m);
                break;
            }
            if m.seq > want {
                held = Some(m);
                break;
            }
        }
    }
    if let Some((_, rd)) = r.as_mut() {
        if let Some(e) = rd.error() {
            return Err(e);
        }
    }
    Ok(out)
}

/// Seq и msgid сообщений файла и первый seq после них.
pub type FileSeqs = (Vec<(i64, i32)>, Option<i64>);

/// Seq и msgid сообщений файла в `[from, to]` и первый seq после `to`.
pub fn seqs_in(ctx: &Context, src: &[Src], from: i64, to: i64) -> Result<FileSeqs, String> {
    let mut out = Vec::new();
    let mut next = None;
    for s in src {
        if s.last < from || s.begin > to {
            continue;
        }
        let mut r = Reader::open(ctx, &s.open.clone().at(from.max(s.begin)).meta())?;
        for m in r.by_ref() {
            if m.seq > to {
                next = Some(m.seq);
                break;
            }
            out.push((m.seq, m.msgid));
        }
        if let Some(e) = r.error() {
            return Err(e);
        }
        if next.is_some() {
            break;
        }
    }
    Ok((out, next))
}

/// Последний seq файла не дальше `last`, который не `dead`; `None` — все
/// удалены.
pub fn alive_before(
    ctx: &Context,
    src: &[Src],
    begin: i64,
    last: i64,
    dead: impl Fn(i64) -> bool,
) -> Result<Option<i64>, String> {
    if !dead(last) {
        return Ok(Some(last));
    }
    let meta: Vec<Src> = src
        .iter()
        .map(|s| Src {
            open: s.open.clone().meta(),
            begin: s.begin,
            last: s.last,
        })
        .collect();
    let mut at = last;
    loop {
        let w = before_seq(ctx, &meta, at, 256, begin)?;
        if let Some(m) = w.iter().rev().find(|m| !dead(m.seq)) {
            return Ok(Some(m.seq));
        }
        match w.first() {
            Some(m) if m.seq > begin => at = m.seq,
            _ => return Ok(None),
        }
    }
}
