//! Чтение окна строк: всегда несколько сотен сообщений, сколько бы ни весил файл.
//!
//! Вперёд — открыть канал с `seq=` (бисекция блоков в libtll) и прочитать N.
//! Назад — окно seq перед якорем, растущее вчетверо, пока не наберётся N
//! (seq бывают с дырами). По номеру строки — от ближайшей точки индекса. Найденное
//! фильтром — по списку seq, с переоткрытием на больших пропусках. Что
//! открывать — шаблон [`Open`]: путь и, если есть правки, yaml tll-patch.
//!
//! Наружу: [`from_seq`], [`before_seq`], [`from_row`], [`row_of`], [`by_seqs`].

use std::collections::VecDeque;

use super::index::Index;
use crate::tll::{Context, Msg, Open, Reader};

/// Пропуск seq, после которого дешевле переоткрыть канал, чем читать подряд.
const REOPEN_GAP: i64 = 256;

/// N сообщений начиная с первого seq ≥ `seq`.
pub fn from_seq(ctx: &Context, src: &Open, seq: i64, n: usize) -> Result<Vec<Msg>, String> {
    let mut r = Reader::open(ctx, &src.clone().at(seq))?;
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        match r.next() {
            Some(m) => out.push(m),
            None => break,
        }
    }
    if let Some(e) = r.error() {
        return Err(e);
    }
    Ok(out)
}

/// До N сообщений строго перед `seq`, по возрастанию.
pub fn before_seq(
    ctx: &Context,
    src: &Open,
    seq: i64,
    n: usize,
    begin: i64,
) -> Result<Vec<Msg>, String> {
    if n == 0 || seq <= begin {
        return Ok(Vec::new());
    }
    let mut w = (n as i64 * 2).max(64);
    loop {
        let lo = seq.saturating_sub(w).max(begin);
        let mut r = Reader::open(ctx, &src.clone().at(lo))?;
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

/// N сообщений с номера строки `row` по индексу.
pub fn from_row(
    ctx: &Context,
    src: &Open,
    idx: &Index,
    row: usize,
    n: usize,
) -> Result<Vec<Msg>, String> {
    let Some((prow, pseq)) = idx.point_for_row(row) else {
        return Ok(Vec::new());
    };
    let mut r = Reader::open(ctx, &src.clone().at(pseq))?;
    let mut skip = row - prow;
    let mut out = Vec::with_capacity(n);
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
    Ok(out)
}

/// Номер строки первого сообщения с seq ≥ `seq`.
pub fn row_of(ctx: &Context, src: &Open, idx: &Index, seq: i64) -> Result<usize, String> {
    let Some((prow, pseq)) = idx.point_for_seq(seq) else {
        return Ok(0);
    };
    let r = Reader::open(ctx, &src.clone().at(pseq).meta())?;
    let mut row = prow;
    for m in r {
        if m.seq >= seq {
            break;
        }
        row += 1;
    }
    Ok(row)
}

/// Сообщения с ровно этими seq (по возрастанию) — окно отфильтрованного вида.
pub fn by_seqs(ctx: &Context, src: &Open, seqs: &[i64]) -> Result<Vec<Msg>, String> {
    let mut out = Vec::with_capacity(seqs.len());
    let mut r: Option<Reader> = None;
    // Прочитанное, но ещё не нужное: оно может оказаться следующим искомым.
    let mut held: Option<Msg> = None;
    // seq последнего прочитанного: по нему решаем, читать подряд или переоткрыть.
    let mut pos = i64::MIN;
    for &want in seqs {
        let in_held = held.as_ref().is_some_and(|m| m.seq >= want);
        if !in_held && (r.is_none() || want.saturating_sub(pos) > REOPEN_GAP) {
            if let Some(rd) = r.as_mut() {
                if let Some(e) = rd.error() {
                    return Err(e);
                }
            }
            r = Some(Reader::open(ctx, &src.clone().at(want))?);
            held = None;
        }
        let rd = r.as_mut().unwrap();
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
    if let Some(rd) = r.as_mut() {
        if let Some(e) = rd.error() {
            return Err(e);
        }
    }
    Ok(out)
}
