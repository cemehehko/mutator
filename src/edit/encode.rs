//! Запись правок в тела сообщений по схеме (`agr:edit-encode`): текст значения
//! → байты поля. Обратная сторона [`crate::decode`]: принимается тот же текст,
//! что показывает деталь (enum — имя или число, bits — `a|b=2` или число, fixed
//! и decimal128 — как `str(Decimal)`, время — ISO, длительность — число с
//! суффиксом), так что `decode(encode(s)) == s`.
//!
//! Работает над нейтральной моделью [`Scheme`] и [`Msg`] — одинаково для любого
//! бэкенда. Семантика — команд tll-patch:
//! - [`apply`] — `Field`: тип тела сверяется с именем в правке, переполнение и
//!   мусор — ошибка, у записанного поля ставится бит карты присутствия,
//!   offset-строка дописывается в хвост тела и указатель переводится на неё
//!   (старые байты остаются, тело валидно для читателей). Путь идёт через
//!   вложенные сообщения и выбранный вариант union; списки не правятся;
//! - [`compose`] — тело `Update` и `Insert` с нуля: незаданное — нули, списки
//!   и offset-строки — в хвост по порядку, union — по имени варианта;
//! - [`view`] — что видит вид: правки полей и смена типа, без перенумерации
//!   (`agr:view-input-seq`);
//! - [`Stream`] — фиксация: ещё вставки, удаления и сдвиг seq, как канал
//!   tll-patch (`agr:stream-ops`).
//!
//! Наружу: [`apply`], [`compose`], [`view`], [`Stream`].

use std::collections::BTreeMap;

use crate::io::Msg;
use crate::scheme::{Field, Int, Kind, PtrVer, Scheme, Struct, Sub, TimeRes};

use super::stream::{tree, BYTES, Y};
use super::{Body, Edits, MsgEdit, Op, Span};

/// Тело `m` с правками `e`: ошибка — первая правка, которую нельзя записать.
pub fn apply(scheme: &Scheme, m: &Msg, e: &MsgEdit) -> Result<Msg, String> {
    let st = scheme
        .message(m.msgid)
        .ok_or_else(|| format!("seq {}: msgid {} нет в схеме", m.seq, m.msgid))?;
    if st.name != e.name {
        return Err(format!(
            "seq {}: сообщение {}, а правка для {}",
            m.seq, st.name, e.name
        ));
    }
    let mut data = m.data.clone();
    for (path, f) in &e.fields {
        set(scheme, st, &mut data, path, &f.new)
            .map_err(|x| format!("seq {} {path}: {x}", m.seq))?;
    }
    Ok(Msg {
        seq: m.seq,
        msgid: m.msgid,
        data,
    })
}

/// Тело сообщения `name` с нуля по `body`: (msgid, байты).
pub fn compose(scheme: &Scheme, name: &str, body: &Body) -> Result<(i32, Vec<u8>), String> {
    let st = scheme
        .structs
        .iter()
        .find(|s| s.msgid != 0 && s.name == name)
        .ok_or_else(|| format!("сообщения {name} нет в схеме"))?;
    let mut data = vec![0; st.size];
    fill_struct(scheme, st, &mut data, 0, &tree(body))?;
    Ok((st.msgid, data))
}

/// Сообщение, каким его показывает вид: смена типа — тело с нуля (правки полей
/// поверх), правки полей — в тело; удаления и вставки вид рисует сам.
pub fn view(scheme: &Scheme, edits: &Edits, m: Msg) -> Result<Msg, String> {
    match edits.op(m.seq) {
        Some(Op::Retype { name, body, .. }) => retyped(scheme, edits, m.seq, name, body),
        _ => match edits.message(m.seq) {
            Some(e) => apply(scheme, &m, e),
            None => Ok(m),
        },
    }
}

/// Тело смены типа на `seq`: `body` и поверх — правки полей сообщения.
fn retyped(
    scheme: &Scheme,
    edits: &Edits,
    seq: i64,
    name: &str,
    body: &Body,
) -> Result<Msg, String> {
    let mut b = body.clone();
    if let Some(e) = edits.message(seq) {
        for (path, f) in &e.fields {
            b.insert(path.clone(), f.new.clone());
        }
    }
    let (msgid, data) =
        compose(scheme, name, &b).map_err(|x| format!("seq {seq} → {name}: {x}"))?;
    Ok(Msg { seq, msgid, data })
}

/// Правки потоком для фиксации — как канал tll-patch: перед сообщением —
/// вставки до его seq (вставка в живой seq сдвигает seq дальше на +1), `Remove`
/// выкидывает со сдвигом на −1, `Clear` — без сдвига, смена типа и правки полей
/// — на месте. Вставки берутся только из [`Span`] — у части склейки свои.
pub struct Stream {
    edits: std::sync::Arc<Edits>,
    /// Вставки по порядку: (seq, номер среди вставок seq).
    inserts: Vec<(i64, usize)>,
    next: usize,
    delta: i64,
}

impl Stream {
    pub fn new(edits: std::sync::Arc<Edits>, span: Span) -> Stream {
        let inserts = edits
            .all_inserts()
            .filter(|(s, _)| span.from.is_none_or(|f| *s >= f) && span.till.is_none_or(|t| *s < t))
            .flat_map(|(s, v)| (0..v.len()).map(move |k| (s, k)))
            .collect();
        Stream {
            edits,
            inserts,
            next: 0,
            delta: 0,
        }
    }

    /// Сообщение файла `m` → что уходит в запись, по порядку.
    pub fn push(&mut self, scheme: &Scheme, m: Msg, out: &mut Vec<Msg>) -> Result<(), String> {
        self.flush(scheme, Some(m.seq), out)?;
        let seq = m.seq + self.delta;
        match self.edits.op(m.seq) {
            Some(Op::Delete { renumber, .. }) => {
                if *renumber {
                    self.delta -= 1;
                }
            }
            Some(Op::Retype { name, body, .. }) => {
                let mut r = retyped(scheme, &self.edits, m.seq, name, body)?;
                r.seq = seq;
                out.push(r);
            }
            None => {
                let mut r = match self.edits.message(m.seq) {
                    Some(e) => apply(scheme, &m, e)?,
                    None => m,
                };
                r.seq = seq;
                out.push(r);
            }
        }
        Ok(())
    }

    /// Конец файла: оставшиеся вставки — за последним сообщением.
    pub fn finish(&mut self, scheme: &Scheme, out: &mut Vec<Msg>) -> Result<(), String> {
        self.flush(scheme, None, out)
    }

    /// Вставки с seq не больше `upto` (`None` — все); вставка в seq
    /// пришедшего сообщения сдвигает его и всё дальше.
    fn flush(
        &mut self,
        scheme: &Scheme,
        upto: Option<i64>,
        out: &mut Vec<Msg>,
    ) -> Result<(), String> {
        while let Some(&(s, k)) = self.inserts.get(self.next) {
            if upto.is_some_and(|u| s > u) {
                break;
            }
            self.next += 1;
            let ins = &self.edits.inserts(s)[k];
            let (msgid, data) = compose(scheme, &ins.name, &ins.body)
                .map_err(|x| format!("вставка перед {s}: {x}"))?;
            out.push(Msg {
                seq: s + self.delta,
                msgid,
                data,
            });
            if upto == Some(s) {
                self.delta += 1;
            }
        }
        Ok(())
    }
}

/// Записать `text` в поле `path` тела `data` сообщения `st`.
fn set(
    scheme: &Scheme,
    st: &Struct,
    data: &mut Vec<u8>,
    path: &str,
    text: &str,
) -> Result<(), String> {
    let (f, off, pmap) = locate(scheme, st, data, path)?;
    write(f, data, off, text)?;
    if let Some((pm, bit)) = pmap {
        set_bit(data, pm, bit)?;
    }
    Ok(())
}

/// Записать скаляр `text` в поле `f` по смещению `off`.
fn write(f: &Field, data: &mut Vec<u8>, off: usize, text: &str) -> Result<(), String> {
    // Строки — как есть, остальное — без пробелов по краям.
    let raw = text;
    let text = text.trim();
    match &f.kind {
        Kind::Int(t, sub) => {
            let v = int_value(*t, sub, text)?;
            put(data, off, &v.to_le_bytes()[..t.size()])?;
        }
        Kind::Double(sub) => {
            let v = match sub {
                Sub::TimePoint(r) => time_ns(text)? as f64 / r.nanos() as f64,
                _ => text
                    .parse::<f64>()
                    .map_err(|_| format!("«{text}» — не число"))?,
            };
            put(data, off, &v.to_le_bytes())?;
        }
        Kind::Decimal128 => put(data, off, &decimal128(text)?.to_le_bytes())?,
        Kind::Bytes { string } => {
            let b = match raw.strip_prefix(BYTES) {
                Some(hex) => unhex(hex)?,
                None if *string => raw.as_bytes().to_vec(),
                None => raw_bytes(text, f.size)?,
            };
            if b.len() > f.size {
                return Err(format!("{} байт не влезают в {}", b.len(), f.size));
            }
            let mut v = b;
            v.resize(f.size, 0);
            put(data, off, &v)?;
        }
        Kind::Pointer {
            ver, string: true, ..
        } => {
            // Как tll-patch: строка с нулём — в хвост тела, указатель — на неё.
            let at = data.len();
            if at < off {
                return Err("за концом тела".into());
            }
            let head = pointer(*ver, at - off, raw.len() + 1, 1)?;
            put(data, off, &head)?;
            data.extend_from_slice(raw.as_bytes());
            data.push(0);
        }
        _ => return Err("не скаляр — правится только значение".into()),
    }
    Ok(())
}

/// Поля структуры `st` по `base` из дерева тела; у каждого заданного — бит
/// карты присутствия.
fn fill_struct(
    scheme: &Scheme,
    st: &Struct,
    data: &mut Vec<u8>,
    base: usize,
    fields: &BTreeMap<String, Y>,
) -> Result<(), String> {
    for (name, v) in fields {
        let f = st
            .fields
            .iter()
            .find(|f| &f.name == name)
            .ok_or_else(|| format!("нет поля {name} в {}", st.name))?;
        if let Some((pm, bit)) = pmap_bit(st, f, base) {
            set_bit(data, pm, bit)?;
        }
        fill(scheme, f, data, base + f.offset, v).map_err(|x| format!("{name}: {x}"))?;
    }
    Ok(())
}

/// Поле `f` по `off` из узла дерева тела.
fn fill(scheme: &Scheme, f: &Field, data: &mut Vec<u8>, off: usize, v: &Y) -> Result<(), String> {
    match (&f.kind, v) {
        (Kind::Message(i), Y::Map(m)) => fill_struct(scheme, &scheme.structs[*i], data, off, m),
        (
            Kind::Array {
                count,
                elem,
                capacity,
            },
            Y::Seq(items),
        ) => {
            if items.len() > *capacity {
                return Err(format!("{} элементов, а вмещает {capacity}", items.len()));
            }
            put(
                data,
                off + count.offset,
                &(items.len() as u64).to_le_bytes()[..count.size],
            )?;
            for (i, x) in items.values().enumerate() {
                fill(scheme, elem, data, off + elem.offset + i * elem.size, x)
                    .map_err(|e| format!("[{i}]: {e}"))?;
            }
            Ok(())
        }
        (
            Kind::Pointer {
                elem,
                ver,
                string: false,
            },
            Y::Seq(items),
        ) => {
            // Как alloc_pointer libtll: элементы — в хвост тела, большой шаг
            // элемента — словом перед ними.
            let at = data.len();
            let big = *ver == PtrVer::Default && elem.size >= 0xff;
            let head = pointer(
                *ver,
                at - off + if big { 4 } else { 0 },
                items.len(),
                elem.size,
            )?;
            put(data, off, &head)?;
            if big {
                data.extend_from_slice(&(elem.size as u32).to_le_bytes());
            }
            let start = data.len();
            data.resize(start + items.len() * elem.size, 0);
            for (i, x) in items.values().enumerate() {
                fill(scheme, elem, data, start + i * elem.size, x)
                    .map_err(|e| format!("[{i}]: {e}"))?;
            }
            Ok(())
        }
        (Kind::Union { tag, variants }, Y::Map(m)) => {
            let mut it = m.iter();
            let Some((name, x)) = it.next() else {
                return Ok(());
            };
            if it.next().is_some() {
                return Err("у union задан не один вариант".into());
            }
            let (i, var) = variants
                .iter()
                .enumerate()
                .find(|(_, v)| &v.name == name)
                .ok_or_else(|| format!("нет варианта {name} в union"))?;
            put(
                data,
                off + tag.offset,
                &(i as u64).to_le_bytes()[..tag.size],
            )?;
            fill(scheme, var, data, off + var.offset, x).map_err(|e| format!("{name}: {e}"))
        }
        (_, Y::Leaf(text)) => write(f, data, off, text),
        _ => Err("значение не той формы: список, вложенное или скаляр".into()),
    }
}

fn set_bit(data: &mut [u8], pm: usize, bit: usize) -> Result<(), String> {
    let b = data
        .get_mut(pm + bit / 8)
        .ok_or("карта присутствия за концом тела")?;
    *b |= 1 << (bit % 8);
    Ok(())
}

/// Бит карты присутствия: смещение карты в теле и номер бита.
type PmapBit = Option<(usize, usize)>;

/// Поле по пути: само поле, смещение в теле и его бит карты присутствия.
fn locate<'a>(
    scheme: &'a Scheme,
    msg: &'a Struct,
    data: &[u8],
    path: &str,
) -> Result<(&'a Field, usize, PmapBit), String> {
    if path.contains('[') {
        return Err("элементы списков не правятся".into());
    }
    let mut st = msg;
    let mut base = 0;
    let mut parts = path.split('.').peekable();
    while let Some(p) = parts.next() {
        let f = st
            .fields
            .iter()
            .find(|f| f.name == p)
            .ok_or_else(|| format!("нет поля {p} в {}", st.name))?;
        let pmap = pmap_bit(st, f, base);
        let mut f = f;
        let mut off = base + f.offset;
        // Через union — только в выбранный сейчас вариант.
        while let Kind::Union { tag, variants } = &f.kind {
            let Some(v) = parts.next() else {
                return Err("union — правится его вариант".into());
            };
            let i = get_uint(data, off + tag.offset, tag.size)? as usize;
            let cur = variants
                .get(i)
                .ok_or_else(|| format!("номер варианта union {i} вне схемы"))?;
            if cur.name != v {
                return Err(format!("в union выбран {}, а не {v}", cur.name));
            }
            off += cur.offset;
            f = cur;
        }
        if parts.peek().is_none() {
            return Ok((f, off, pmap));
        }
        match f.kind {
            Kind::Message(m) => {
                base = off;
                st = &scheme.structs[m];
            }
            _ => return Err(format!("{p} — не сообщение")),
        }
    }
    Err("пустой путь".into())
}

/// Бит карты присутствия поля `f` структуры `st`, лежащей по `base`.
fn pmap_bit(st: &Struct, f: &Field, base: usize) -> PmapBit {
    let name = st.pmap.as_ref()?;
    if f.index < 0 {
        return None;
    }
    let pf = st.fields.iter().find(|x| &x.name == name)?;
    Some((base + pf.offset, f.index as usize))
}

fn put(data: &mut [u8], off: usize, b: &[u8]) -> Result<(), String> {
    let n = data.len();
    data.get_mut(off..off + b.len())
        .ok_or_else(|| format!("за концом тела: {off}+{} > {n}", b.len()))?
        .copy_from_slice(b);
    Ok(())
}

fn get_uint(data: &[u8], off: usize, n: usize) -> Result<u64, String> {
    let b = data
        .get(off..off + n)
        .ok_or_else(|| format!("за концом тела: {off}+{n} > {}", data.len()))?;
    let mut x = [0u8; 8];
    x[..n.min(8)].copy_from_slice(&b[..n.min(8)]);
    Ok(u64::from_le_bytes(x))
}

/// Заголовок offset-ptr: `size` элементов по `entity` байт по смещению `rel`
/// от заголовка; шаг от 255 байт у Default — 0xff, само слово пишет вызывающий.
fn pointer(ver: PtrVer, rel: usize, size: usize, entity: usize) -> Result<Vec<u8>, String> {
    let big = || format!("список не помещается в offset-ptr ({size} по смещению {rel})");
    Ok(match ver {
        PtrVer::Default => {
            if rel > u32::MAX as usize || size >= 1 << 24 {
                return Err(big());
            }
            let mut h = (rel as u32).to_le_bytes().to_vec();
            h.extend_from_slice(&(size as u32).to_le_bytes()[..3]);
            h.push(entity.min(0xff) as u8);
            h
        }
        PtrVer::LegacyShort => {
            if rel > u16::MAX as usize || size > u16::MAX as usize {
                return Err(big());
            }
            [(rel as u16).to_le_bytes(), (size as u16).to_le_bytes()].concat()
        }
        PtrVer::LegacyLong => {
            if rel > u32::MAX as usize || size > u16::MAX as usize || entity > u16::MAX as usize {
                return Err(big());
            }
            let mut h = (rel as u32).to_le_bytes().to_vec();
            h.extend_from_slice(&(size as u16).to_le_bytes());
            h.extend_from_slice(&(entity as u16).to_le_bytes());
            h
        }
    })
}

/// Целое поля типа `t` с подтипом `sub` из текста; диапазон проверяется.
fn int_value(t: Int, sub: &Sub, text: &str) -> Result<i128, String> {
    let v = match sub {
        Sub::None => integer(text)?,
        Sub::Enum(e) => match integer(text) {
            Ok(v) => v,
            Err(_) => {
                let mut v = 0i64;
                for n in text.split('|').map(str::trim) {
                    let x = e
                        .values
                        .iter()
                        .find(|(name, _)| name == n)
                        .map(|(_, x)| *x)
                        .ok_or_else(|| format!("«{n}» нет в перечислении {}", e.name))?;
                    if !e.flags && text.contains('|') {
                        return Err(format!("{} — не флаги, значение одно", e.name));
                    }
                    v |= x;
                }
                v as i128
            }
        },
        Sub::Bits(b) => match integer(text) {
            Ok(v) => v,
            Err(_) => {
                let mut v = 0u64;
                for part in text.split('|').map(str::trim).filter(|s| !s.is_empty()) {
                    // Число среди имён — как у tll-patch: сырые биты.
                    if let Ok(x) = integer(part) {
                        v |= u64::try_from(x).map_err(|_| format!("{x} — не биты"))?;
                        continue;
                    }
                    let (n, x) = match part.split_once('=') {
                        Some((n, x)) => (n.trim(), Some(integer(x.trim())?)),
                        None => (part, None),
                    };
                    let (_, off, w) = b
                        .fields
                        .iter()
                        .find(|(name, ..)| name == n)
                        .ok_or_else(|| format!("«{n}» нет в битах {}", b.name))?;
                    let max = if *w >= 64 { u64::MAX } else { (1u64 << w) - 1 };
                    let x = match x {
                        Some(x) if x >= 0 && x as u128 <= max as u128 => x as u64,
                        Some(x) => return Err(format!("{n}={x} не влезает в {w} бит")),
                        None if *w == 1 => 1,
                        None => return Err(format!("{n} шириной {w} бит — нужно {n}=число")),
                    };
                    v |= x << off;
                }
                v as i128
            }
        },
        Sub::Fixed(p) => {
            let (neg, coeff, exp) = dec(text)?;
            let m = scale(coeff, exp, -(*p as i32))
                .ok_or_else(|| format!("«{text}» — больше {p} знаков после точки"))?;
            if neg {
                -(m as i128)
            } else {
                m as i128
            }
        }
        Sub::TimePoint(r) => {
            let ns = match integer(text) {
                Ok(v) => return range(t, v),
                Err(_) => time_ns(text)?,
            };
            if ns % r.nanos() != 0 {
                return Err(format!("«{text}» точнее разрешения поля ({})", r.suffix()));
            }
            ns / r.nanos()
        }
        Sub::Duration(r) => duration(text, *r)?,
    };
    range(t, v)
}

fn range(t: Int, v: i128) -> Result<i128, String> {
    let bits = 8 * t.size() as u32;
    let (lo, hi) = if t.signed() {
        (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
    } else {
        (0, (1i128 << bits) - 1)
    };
    if v < lo || v > hi {
        return Err(format!("{v} вне {}: {lo}..{hi}", t.name()));
    }
    Ok(v)
}

/// Целое: десятичное или `0x…`, со знаком.
fn integer(text: &str) -> Result<i128, String> {
    let bad = || format!("«{text}» — не целое");
    let (neg, s) = match text.strip_prefix('-') {
        Some(s) => (true, s),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let v = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => i128::from_str_radix(h, 16).map_err(|_| bad())?,
        None if !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()) => {
            s.parse::<i128>().map_err(|_| bad())?
        }
        None => return Err(bad()),
    };
    Ok(if neg { -v } else { v })
}

/// Десятичное как `str(Decimal)`: знак, коэффициент, порядок (`1.5`, `0E-8`,
/// `1.2E+3`).
fn dec(text: &str) -> Result<(bool, u128, i32), String> {
    let bad = || format!("«{text}» — не десятичное число");
    let (neg, s) = match text.strip_prefix('-') {
        Some(s) => (true, s),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let (mant, e) = match s.find(['e', 'E']) {
        Some(i) => (&s[..i], s[i + 1..].parse::<i32>().map_err(|_| bad())?),
        None => (s, 0),
    };
    let (int, frac) = mant.split_once('.').unwrap_or((mant, ""));
    if int.is_empty() && frac.is_empty()
        || !int.bytes().chain(frac.bytes()).all(|c| c.is_ascii_digit())
    {
        return Err(bad());
    }
    let digits = format!("{int}{frac}");
    let digits = digits.trim_start_matches('0');
    let coeff = if digits.is_empty() {
        0
    } else {
        digits.parse::<u128>().map_err(|_| bad())?
    };
    Ok((neg, coeff, e - frac.len() as i32))
}

/// `coeff·10^exp` в единицах `10^to`; `None` — теряются ненулевые знаки или
/// переполнение.
fn scale(coeff: u128, exp: i32, to: i32) -> Option<u128> {
    if exp >= to {
        coeff.checked_mul(10u128.checked_pow((exp - to) as u32)?)
    } else {
        let d = 10u128.checked_pow((to - exp) as u32).unwrap_or(u128::MAX);
        coeff.is_multiple_of(d).then(|| coeff / d)
    }
}

/// decimal128 (BID) из текста: число или `Infinity`, `-Infinity`, `NaN`, `sNaN`.
fn decimal128(text: &str) -> Result<u128, String> {
    match text {
        "Infinity" | "+Infinity" => return Ok(0x1e << 122),
        "-Infinity" => return Ok((1 << 127) | (0x1e << 122)),
        "NaN" => return Ok(0x1f << 122),
        "sNaN" => return Ok((0x1f << 122) | (1 << 121)),
        _ => {}
    }
    let (neg, mut coeff, mut exp) = dec(text)?;
    let max = 10u128.pow(34);
    while coeff >= max && coeff % 10 == 0 {
        coeff /= 10;
        exp += 1;
    }
    let biased = exp + 6176;
    if coeff >= max || !(0..=12287).contains(&biased) {
        return Err(format!("«{text}» не влезает в decimal128"));
    }
    Ok(((neg as u128) << 127) | ((biased as u128) << 113) | coeff)
}

/// Длительность в единицах `r`: число с суффиксом (`15ms`, `1.5s`) или без —
/// тогда в единицах поля.
fn duration(text: &str, r: TimeRes) -> Result<i128, String> {
    const ALL: [TimeRes; 7] = [
        TimeRes::Ns,
        TimeRes::Us,
        TimeRes::Ms,
        TimeRes::S,
        TimeRes::M,
        TimeRes::H,
        TimeRes::D,
    ];
    // Длинные суффиксы раньше: `ms` не должен прочитаться как `s`.
    let (num, unit) = ALL
        .iter()
        .filter(|u| u.suffix().len() == 2)
        .chain(ALL.iter().filter(|u| u.suffix().len() == 1))
        .find_map(|u| text.strip_suffix(u.suffix()).map(|n| (n, *u)))
        .unwrap_or((text, r));
    let (neg, coeff, exp) = dec(num.trim())?;
    // В наносекундах, потом в единицах поля — без потери знаков.
    let ns = scale(coeff, exp, 0)
        .and_then(|c| c.checked_mul(unit.nanos() as u128))
        .or_else(|| {
            let c = coeff.checked_mul(unit.nanos() as u128)?;
            scale(c, exp, 0)
        })
        .ok_or_else(|| format!("«{text}» точнее наносекунды"))?;
    if ns % r.nanos() as u128 != 0 {
        return Err(format!("«{text}» точнее разрешения поля ({})", r.suffix()));
    }
    let v = (ns / r.nanos() as u128) as i128;
    Ok(if neg { -v } else { v })
}

/// Момент времени ISO (`2026-09-22T13:41:27.562Z`, `Z` и доли — по желанию,
/// дата без времени — полночь) в наносекундах от эпохи.
fn time_ns(text: &str) -> Result<i128, String> {
    let bad = || format!("«{text}» — не время ГГГГ-ММ-ДДTЧЧ:ММ:СС[.доли][Z]");
    let s = text.strip_suffix('Z').unwrap_or(text);
    let (date, time) = s.split_once(['T', ' ']).unwrap_or((s, "00:00:00"));
    let num = |x: &str| x.parse::<i64>().map_err(|_| bad());
    let d: Vec<&str> = date.split('-').collect();
    if d.len() != 3 {
        return Err(bad());
    }
    let (y, m, day) = (num(d[0])?, num(d[1])?, num(d[2])?);
    let (hms, frac) = time.split_once('.').unwrap_or((time, ""));
    let t: Vec<&str> = hms.split(':').collect();
    if t.len() != 3 || !(1..=12).contains(&m) || !(1..=31).contains(&day) || frac.len() > 9 {
        return Err(bad());
    }
    let (h, mi, sec) = (num(t[0])?, num(t[1])?, num(t[2])?);
    if h > 23 || mi > 59 || sec > 60 || !frac.bytes().all(|c| c.is_ascii_digit()) {
        return Err(bad());
    }
    let ns_frac = if frac.is_empty() {
        0
    } else {
        num(frac)? * 10i64.pow(9 - frac.len() as u32)
    };
    let days = days_from_civil(y, m, day);
    let secs = days as i128 * 86_400 + (h * 3600 + mi * 60 + sec) as i128;
    Ok(secs * 1_000_000_000 + ns_frac as i128)
}

/// Дата → дни от эпохи; обратное [`crate::decode::value::civil`].
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Сырые байты, как их печатает [`crate::decode::value::bytes`]: ASCII с `\0`,
/// иначе hex во весь размер поля.
fn raw_bytes(text: &str, size: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if let Some(r) = rest.strip_prefix("\\0") {
            out.push(0);
            rest = r;
        } else if c.is_ascii() {
            out.push(c as u8);
            rest = &rest[1..];
        } else {
            return Err(format!("«{text}» — не ASCII и не hex"));
        }
    }
    if out.len() <= size {
        return Ok(out);
    }
    let hex = text.len() == 2 * size && text.bytes().all(|c| c.is_ascii_hexdigit());
    if !hex {
        return Err(format!("{} байт не влезают в {size}", out.len()));
    }
    (0..size)
        .map(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

/// Байты из hex за меткой [`BYTES`] (копия сообщения, `body_of`).
fn unhex(hex: &str) -> Result<Vec<u8>, String> {
    if !hex.len().is_multiple_of(2) || !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("«{hex}» — не hex"));
    }
    (0..hex.len() / 2)
        .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

#[cfg(all(test, feature = "tll"))]
mod tests {
    use super::*;
    use crate::decode::decode;
    use crate::edit::FieldEdit;
    use std::collections::BTreeMap;

    fn scheme() -> Scheme {
        crate::tll::load_scheme(
            r#"yamls://
- name: Head
  fields:
    - {name: node, type: int32}
    - {name: ts, type: int64, options.type: time_point, options.resolution: us}
    - {name: login, type: byte8, options.type: string}
- name: Order
  id: 10
  enums:
    Side: {type: int8, enum: {Buy: 1, Sell: 2}}
  bits:
    Flags: {type: uint8, bits: [{name: a, offset: 0}, {name: b, offset: 1}, {name: c, offset: 2, size: 2}]}
  fields:
    - {name: head, type: Head}
    - {name: side, type: Side}
    - {name: flags, type: Flags}
    - {name: price, type: int64, options.type: fixed8}
    - {name: qty, type: uint16}
    - {name: d, type: decimal128}
    - {name: px, type: double}
    - {name: ttl, type: int32, options.type: duration, options.resolution: ms}
    - {name: raw, type: byte4}
    - {name: note, type: string}
    - {name: u, type: union, union: [{name: i, type: int8}, {name: f, type: double}]}
- name: Other
  id: 11
  fields:
    - {name: x, type: int8}
"#,
        )
        .expect("схема")
    }

    fn order(s: &Scheme) -> Msg {
        let st = s.message(10).unwrap();
        Msg {
            seq: 5,
            msgid: 10,
            data: vec![0; st.size],
        }
    }

    fn edit(fields: &[(&str, &str)]) -> MsgEdit {
        MsgEdit {
            name: "Order".into(),
            fields: fields
                .iter()
                .map(|(p, v)| {
                    (
                        p.to_string(),
                        FieldEdit {
                            old: String::new(),
                            new: v.to_string(),
                        },
                    )
                })
                .collect::<BTreeMap<_, _>>(),
        }
    }

    fn text(s: &Scheme, m: &Msg, path: &str) -> String {
        let n = decode(s, s.message(m.msgid).unwrap(), &m.data);
        n.walk()
            .into_iter()
            .find(|(_, x)| x.path == path)
            .map(|(_, x)| x.text())
            .unwrap_or_else(|| panic!("нет {path}"))
    }

    /// `agr:edit-encode` — текст, который показывает деталь, записывается и
    /// читается обратно тем же текстом — для каждого вида поля.
    #[test]
    fn edited_value_reads_back_as_the_same_text() {
        let s = scheme();
        let cases = [
            ("head.node", "-403"),
            ("head.ts", "2000-01-02T03:04:05.123456Z"),
            ("head.login", "login123"),
            ("side", "Sell"),
            ("flags", "a|c=2"),
            ("price", "200.20000000"),
            ("price", "-1E-8"),
            ("price", "-1.50000000"),
            ("qty", "65535"),
            ("d", "1.2E+3"),
            ("d", "0E-8"),
            ("d", "-Infinity"),
            ("px", "2.5"),
            ("ttl", "1500ms"),
            ("raw", "ab\\0d"),
            ("note", "новая заметка"),
            ("u.i", "-7"),
        ];
        for (path, v) in cases {
            let m = apply(&s, &order(&s), &edit(&[(path, v)])).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(text(&s, &m, path), v, "{path}");
        }
    }

    /// `agr:edit-encode` — тот же смысл в другой записи: enum и bits числом,
    /// длительность в других единицах, время без `Z`, hex для сырых байт.
    #[test]
    fn other_spellings_land_on_the_same_value() {
        let s = scheme();
        let cases = [
            ("side", "2", "Sell"),
            ("flags", "0x9", "a|c=2"),
            ("flags", "a|8", "a|c=2"),
            ("ttl", "1.5s", "1500ms"),
            ("head.ts", "2000-01-02T03:04:05", "2000-01-02T03:04:05Z"),
            ("raw", "ff00ff00", "ff00ff00"),
            ("price", "1e1", "10.00000000"),
        ];
        for (path, v, want) in cases {
            let m = apply(&s, &order(&s), &edit(&[(path, v)])).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(text(&s, &m, path), want, "{path} = {v}");
        }
    }

    /// `agr:edit-encode` — значение, которое поле не вместит, и чужой тип тела —
    /// отказ с текстом, тело не меняется (как `bad_field` у tll-patch).
    #[test]
    fn values_the_field_cannot_hold_are_refused() {
        let s = scheme();
        let bad = [
            ("qty", "abc"),
            ("qty", "65536"),
            ("qty", "-1"),
            ("side", "Hold"),
            ("flags", "c"),
            ("price", "0.000000001"),
            ("head.login", "слишком длинный"),
            ("ttl", "1us"),
            ("u.f", "1.0"),
            ("nope", "1"),
            ("head", "1"),
        ];
        for (path, v) in bad {
            let r = apply(&s, &order(&s), &edit(&[(path, v)]));
            assert!(r.is_err(), "{path} = {v} принято");
        }
        let mut e = edit(&[("x", "1")]);
        e.name = "Other".into();
        let err = apply(&s, &order(&s), &e).unwrap_err();
        assert!(err.contains("Order") && err.contains("Other"), "{err}");
    }

    /// `agr:edit-encode` — offset-строка дописывается в хвост, остальные поля и
    /// прежняя длина тела не тронуты; повторная правка снова читается.
    #[test]
    fn offset_string_grows_the_body_and_keeps_other_fields() {
        let s = scheme();
        let m = apply(&s, &order(&s), &edit(&[("qty", "7"), ("note", "abc")])).unwrap();
        let size = s.message(10).unwrap().size;
        assert_eq!(m.data.len(), size + 4);
        assert_eq!(text(&s, &m, "qty"), "7");
        let m = apply(&s, &m, &edit(&[("note", "")])).unwrap();
        assert_eq!(text(&s, &m, "note"), "");
        assert_eq!(text(&s, &m, "qty"), "7");
    }

    /// `agr:edit-encode` `agr:stream-ops` — тело с нуля: заданное читается тем
    /// же текстом, незаданное — нули, у union — выбранный вариант.
    #[test]
    fn composed_body_reads_back() {
        let s = scheme();
        let body: Body = [
            ("head.node", "7"),
            ("side", "Buy"),
            ("note", "x"),
            ("u.f", "2.5"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let (msgid, data) = compose(&s, "Order", &body).unwrap();
        let m = Msg {
            seq: 1,
            msgid,
            data,
        };
        assert_eq!(msgid, 10);
        for (path, v) in [
            ("head.node", "7"),
            ("side", "Buy"),
            ("note", "x"),
            ("u.f", "2.5"),
            ("qty", "0"),
        ] {
            assert_eq!(text(&s, &m, path), v, "{path}");
        }
    }

    /// `agr:stream-ops` — тело, которое не собрать, — отказ с причиной.
    #[test]
    fn bad_bodies_are_refused() {
        let s = scheme();
        let bad = |pairs: &[(&str, &str)]| {
            let b: Body = pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            compose(&s, "Order", &b).unwrap_err()
        };
        assert!(bad(&[("u.i", "1"), ("u.f", "1")]).contains("не один вариант"));
        assert!(bad(&[("nope", "1")]).contains("nope"));
        assert!(bad(&[("qty", "x")]).contains("qty"));
        assert!(compose(&s, "Nope", &Body::new()).is_err());
    }

    /// `agr:edit-encode` — правка ложится только на своё сообщение потока.
    #[test]
    fn overlay_touches_only_edited_seq() {
        let s = scheme();
        let mut e = Edits::default();
        e.set(5, "Order", "qty", "0", "9");
        let m = view(&s, &e, order(&s)).unwrap();
        assert_eq!(text(&s, &m, "qty"), "9");
        let mut other = order(&s);
        other.seq = 6;
        assert_eq!(view(&s, &e, other.clone()).unwrap(), other);
    }
}
