//! Разбор тела сообщения по схеме: дерево [`Node`] для детали и одиночные поля для
//! колонок.
//!
//! Правила те же, что у python-tll (а значит у `tll-read`): карта присутствия
//! (`pmap`) прячет неустановленные поля, offset-ptr трёх версий, строка — до нуля
//! (`byteN`) или длиной минус завершающий ноль (`string`), enum вне схемы — числом.
//! Сломанное поле не роняет сообщение: узел с [`Node::error`] (`agr:bad-message`).
//! У каждого узла — путь в стиле tll-patch (`header.status`, `agr:patch-path`),
//! смещение от начала тела и размер — для hex с подсветкой.
//!
//! Тела сравниваются целиком: [`diff`] — пути скалярных полей с разными
//! значениями у двух тел одного сообщения (`agr:transform-shadow`).
//!
//! Файлы: `value.rs` — скаляры и их текст.
//!
//! Наружу: [`Node`], [`decode`], [`field`], [`diff`], [`Value`].
//!
//! Ещё: [`decimal`], [`time_point`].

mod value;

pub use value::{decimal, time_point, Value};

use std::collections::BTreeMap;

use crate::scheme::{Field, Int, Kind, PtrVer, Scheme, Struct, Sub};

/// Узел дерева сообщения.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    /// Имя поля или `[i]` для элемента списка.
    pub name: String,
    /// Путь от корня: `header.status`, `trailer.extras[0].key`.
    pub path: String,
    /// Тип для детали: `int64 fixed8`, `*Leg`.
    pub ty: String,
    /// Смещение от начала тела и размер (для списков — заголовок указателя).
    pub offset: usize,
    pub size: usize,
    /// Скаляр; у составных — `None`.
    pub value: Option<Value>,
    pub children: Vec<Node>,
    /// Что пошло не так при разборе этого узла.
    pub error: Option<String>,
}

impl Node {
    /// Текст значения для дерева: скаляр, `[n]` у списков, ошибка.
    pub fn text(&self) -> String {
        if let Some(e) = &self.error {
            return format!("<{e}>");
        }
        match &self.value {
            Some(v) => v.text(),
            None if self.ty.starts_with('*') || self.ty.ends_with(']') => {
                format!("[{}]", self.children.len())
            }
            None => String::new(),
        }
    }

    /// Обход в глубину: (глубина, узел).
    pub fn walk(&self) -> Vec<(usize, &Node)> {
        let mut out = Vec::new();
        fn rec<'a>(n: &'a Node, d: usize, out: &mut Vec<(usize, &'a Node)>) {
            for c in &n.children {
                out.push((d, c));
                rec(c, d + 1, out);
            }
        }
        rec(self, 0, &mut out);
        out
    }
}

/// Разобрать всё сообщение. Корень — безымянный узел с полями-детьми.
pub fn decode(scheme: &Scheme, msg: &Struct, data: &[u8]) -> Node {
    let mut root = Node {
        name: msg.name.clone(),
        path: String::new(),
        ty: msg.name.clone(),
        offset: 0,
        size: data.len(),
        value: None,
        children: Vec::new(),
        error: None,
    };
    if data.len() < msg.size {
        root.error = Some(format!(
            "тело {} байт короче сообщения {}",
            data.len(),
            msg.size
        ));
    }
    root.children = structure(scheme, msg, data, 0, "");
    root
}

/// Одно поле по пути (`header.status`) — для колонки. `None`, если в этом
/// сообщении такого поля нет или оно не установлено (`agr:missing-field`).
pub fn field(scheme: &Scheme, msg: &Struct, data: &[u8], path: &str) -> Option<String> {
    let (off, f) = scheme.field_path(msg, path)?;
    if !present_on_path(scheme, msg, data, path) {
        return None;
    }
    let n = node(scheme, f, data, off, path.to_string(), f.name.clone());
    if n.error.is_some() {
        return Some(n.text());
    }
    Some(match &n.value {
        Some(v) => v.text(),
        None => n.text(),
    })
}

/// Пути скалярных полей, значения которых разошлись: путь → значение `before`
/// — в файле, до мутатора (`agr:transform-shadow`). Пропавшие в `before`
/// поля — с пустой строкой. Списки сравниваются поэлементно
/// (`extras[0].key`); тела одного сообщения.
pub fn diff(
    scheme: &Scheme,
    msg: &Struct,
    before: &[u8],
    after: &[u8],
) -> BTreeMap<String, String> {
    let mut b = BTreeMap::new();
    let mut a = BTreeMap::new();
    scalars(&decode(scheme, msg, before), &mut b);
    scalars(&decode(scheme, msg, after), &mut a);
    let mut out = BTreeMap::new();
    for (p, t) in &b {
        if a.get(p) != Some(t) {
            out.insert(p.clone(), t.clone());
        }
    }
    for p in a.keys() {
        if !b.contains_key(p) {
            out.insert(p.clone(), String::new());
        }
    }
    out
}

/// Листья дерева: путь → текст значения или ошибки разбора.
fn scalars(n: &Node, out: &mut BTreeMap<String, String>) {
    if n.children.is_empty() {
        out.insert(n.path.clone(), n.text());
        return;
    }
    for c in &n.children {
        scalars(c, out);
    }
}

fn present_on_path(scheme: &Scheme, msg: &Struct, data: &[u8], path: &str) -> bool {
    let mut st = msg;
    let mut base = 0;
    for p in path.split('.') {
        let Some(f) = st.fields.iter().find(|f| f.name == p) else {
            return false;
        };
        if !present(st, f, data, base) {
            return false;
        }
        if let Kind::Message(m) = f.kind {
            base += f.offset;
            st = &scheme.structs[m];
        }
    }
    true
}

/// Поле установлено по карте присутствия (или карты нет).
fn present(st: &Struct, f: &Field, data: &[u8], base: usize) -> bool {
    let Some(pmap) = &st.pmap else { return true };
    if &f.name == pmap {
        return false;
    }
    if f.index < 0 {
        return true;
    }
    let Some(pf) = st.fields.iter().find(|x| &x.name == pmap) else {
        return true;
    };
    let i = f.index as usize;
    data.get(base + pf.offset + i / 8)
        .is_some_and(|b| b & (1 << (i % 8)) != 0)
}

fn structure(scheme: &Scheme, st: &Struct, data: &[u8], base: usize, prefix: &str) -> Vec<Node> {
    st.fields
        .iter()
        .filter(|f| present(st, f, data, base))
        .map(|f| {
            let path = if prefix.is_empty() {
                f.name.clone()
            } else {
                format!("{prefix}.{}", f.name)
            };
            node(scheme, f, data, base + f.offset, path, f.name.clone())
        })
        .collect()
}

fn get(data: &[u8], off: usize, n: usize) -> Result<&[u8], String> {
    data.get(off..off + n)
        .ok_or_else(|| format!("за концом тела: {off}+{n} > {}", data.len()))
}

fn uint(b: &[u8]) -> u64 {
    let mut x = [0u8; 8];
    x[..b.len()].copy_from_slice(b);
    u64::from_le_bytes(x)
}

fn int(data: &[u8], off: usize, t: Int) -> Result<i128, String> {
    Ok(match t {
        Int::I8 => get(data, off, 1)?[0] as i8 as i128,
        Int::I16 => i16::from_le_bytes(get(data, off, 2)?.try_into().unwrap()) as i128,
        Int::I32 => i32::from_le_bytes(get(data, off, 4)?.try_into().unwrap()) as i128,
        Int::I64 => i64::from_le_bytes(get(data, off, 8)?.try_into().unwrap()) as i128,
        Int::U8 => get(data, off, 1)?[0] as i128,
        Int::U16 => uint(get(data, off, 2)?) as i128,
        Int::U32 => uint(get(data, off, 4)?) as i128,
        Int::U64 => uint(get(data, off, 8)?) as i128,
    })
}

fn sub_value(raw: i128, sub: &Sub) -> Value {
    match sub {
        Sub::None => Value::Int(raw),
        Sub::Enum(e) => Value::Enum {
            name: e.lookup(raw as i64).map(str::to_string),
            raw: raw as i64,
        },
        Sub::Fixed(p) => Value::Dec {
            neg: raw < 0,
            coeff: raw.unsigned_abs(),
            exp: -(*p as i32),
        },
        Sub::TimePoint(r) => Value::Time(raw * r.nanos()),
        Sub::Duration(r) => Value::Duration {
            value: raw,
            res: *r,
        },
        Sub::Bits(b) => Value::Bits(
            b.fields
                .iter()
                .map(|(n, off, w)| {
                    let mask = if *w >= 64 { u64::MAX } else { (1u64 << w) - 1 };
                    (n.clone(), ((raw as u64) >> off) & mask, *w)
                })
                .collect(),
        ),
    }
}

/// Разобрать поле `f`, лежащее по смещению `off` от начала тела.
fn node(scheme: &Scheme, f: &Field, data: &[u8], off: usize, path: String, name: String) -> Node {
    let mut n = Node {
        name,
        path,
        ty: f.type_name(scheme),
        offset: off,
        size: f.size,
        value: None,
        children: Vec::new(),
        error: None,
    };
    if let Err(e) = fill(scheme, f, data, off, &mut n) {
        n.error = Some(e);
    }
    n
}

fn fill(scheme: &Scheme, f: &Field, data: &[u8], off: usize, n: &mut Node) -> Result<(), String> {
    match &f.kind {
        Kind::Int(t, sub) => n.value = Some(sub_value(int(data, off, *t)?, sub)),
        Kind::Double(sub) => {
            let v = f64::from_le_bytes(get(data, off, 8)?.try_into().unwrap());
            n.value = Some(match sub {
                Sub::TimePoint(r) => Value::Time((v * r.nanos() as f64) as i128),
                _ => Value::Double(v),
            });
        }
        Kind::Decimal128 => {
            n.value = Some(value::decimal128(get(data, off, 16)?.try_into().unwrap()))
        }
        Kind::Bytes { string } => {
            let b = get(data, off, f.size)?;
            n.value = Some(if *string {
                let l = b.iter().position(|&c| c == 0).unwrap_or(b.len());
                Value::Str(String::from_utf8_lossy(&b[..l]).into_owned())
            } else {
                Value::Bytes(b.to_vec())
            });
        }
        Kind::Message(m) => n.children = structure(scheme, &scheme.structs[*m], data, off, &n.path),
        Kind::Array {
            count,
            elem,
            capacity,
        } => {
            let c = int(data, off + count.offset, int_kind(count))?;
            if c < 0 || c as usize > *capacity {
                return Err(format!("счётчик массива {c} вне 0..{capacity}"));
            }
            for i in 0..c as usize {
                let eo = off + elem.offset + i * elem.size;
                n.children.push(node(
                    scheme,
                    elem,
                    data,
                    eo,
                    format!("{}[{i}]", n.path),
                    format!("[{i}]"),
                ));
            }
        }
        Kind::Pointer { elem, ver, string } => {
            let (ptr_off, size, entity) = optr(data, off, *ver, elem.size)?;
            let start = off + ptr_off;
            if *string {
                if size == 0 {
                    n.value = Some(Value::Str(String::new()));
                    return Ok(());
                }
                let b = get(data, start, size)?;
                n.value = Some(Value::Str(
                    String::from_utf8_lossy(&b[..size - 1]).into_owned(),
                ));
                return Ok(());
            }
            if size > 0 {
                get(data, start, entity * size)?;
            }
            for i in 0..size {
                let eo = start + i * entity;
                n.children.push(node(
                    scheme,
                    elem,
                    data,
                    eo,
                    format!("{}[{i}]", n.path),
                    format!("[{i}]"),
                ));
            }
        }
        Kind::Union { tag, variants } => {
            let i = int(data, off + tag.offset, int_kind(tag))?;
            let v = variants
                .get(i as usize)
                .ok_or_else(|| format!("номер варианта union {i} вне схемы"))?;
            let path = format!("{}.{}", n.path, v.name);
            n.children
                .push(node(scheme, v, data, off + v.offset, path, v.name.clone()));
        }
    }
    Ok(())
}

fn int_kind(f: &Field) -> Int {
    match &f.kind {
        Kind::Int(t, _) => *t,
        _ => Int::U8,
    }
}

/// Заголовок offset-ptr: (смещение от заголовка, число элементов, шаг элемента).
fn optr(
    data: &[u8],
    off: usize,
    ver: PtrVer,
    elem: usize,
) -> Result<(usize, usize, usize), String> {
    match ver {
        PtrVer::Default => {
            let h = get(data, off, 8)?;
            let mut offset = uint(&h[0..4]) as usize;
            let size = uint(&h[4..7]) as usize;
            let mut entity = h[7] as usize;
            if entity == 0xff {
                entity = uint(get(data, off + offset, 4)?) as usize;
                offset += 4;
            }
            Ok((offset, size, entity))
        }
        PtrVer::LegacyShort => {
            let h = get(data, off, 4)?;
            Ok((uint(&h[0..2]) as usize, uint(&h[2..4]) as usize, elem))
        }
        PtrVer::LegacyLong => {
            let h = get(data, off, 8)?;
            Ok((
                uint(&h[0..4]) as usize,
                uint(&h[4..6]) as usize,
                uint(&h[6..8]) as usize,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scheme() -> Scheme {
        crate::tll::load_scheme(
            r#"yamls://
- name: M
  id: 1
  enums:
    E: {type: int8, enum: {A: 1}}
  fields:
    - {name: e, type: E}
    - {name: u, type: union, union: [{name: a, type: int8}]}
    - {name: s, type: string}
    - {name: x, type: int64}
"#,
        )
        .expect("схема")
    }

    /// `agr:transform-shadow` — [`diff`] находит ровно изменившиеся скалярные
    /// поля (по офсетам из разбора) со значениями до, одинаковые тела дают
    /// пустую карту.
    #[test]
    fn diff_reports_changed_scalar_paths() {
        let s = scheme();
        let m = s.message(1).unwrap();
        let before = vec![0u8; m.size];
        let n = decode(&s, m, &before);
        let off = |p: &str| {
            n.walk()
                .into_iter()
                .find(|(_, x)| x.path == p)
                .unwrap()
                .1
                .offset
        };
        let mut after = before.clone();
        after[off("x")] = 42;
        after[off("e")] = 7; // enum вне схемы — числом
        let d = diff(&s, m, &before, &after);
        assert_eq!(d.len(), 2, "изменились ровно e и x: {d:?}");
        assert_eq!(d.get("x").map(String::as_str), Some("0"));
        assert_eq!(d.get("e").map(String::as_str), Some("0"));
        assert!(diff(&s, m, &before, &before).is_empty(), "тела равны");
    }

    /// `agr:bad-message` — сломанное сообщение разбирается до конца: короткое
    /// тело, enum вне схемы, номер варианта union и список за концом — ошибки
    /// у своих узлов, а не паника и не пустая деталь.
    #[test]
    fn broken_message_decodes_with_errors_in_place() {
        let s = scheme();
        let m = s.message(1).unwrap();
        let mut data = vec![0u8; m.size];
        data[0] = 7; // enum: такого значения нет
        data[1] = 3; // union: такого варианта нет
        data[3..7].copy_from_slice(&1000u32.to_le_bytes()); // строка за концом
        data[7] = 5;
        let n = decode(&s, m, &data);
        let by = |p: &str| {
            n.walk()
                .into_iter()
                .find(|(_, x)| x.path == p)
                .unwrap()
                .1
                .clone()
        };
        assert_eq!(by("e").text(), "7");
        assert!(by("u").error.as_deref().unwrap().contains("union"));
        assert!(by("s").error.as_deref().unwrap().contains("за концом"));
        let short = decode(&s, m, &data[..5]);
        assert!(short.error.is_some());
        assert!(short
            .children
            .iter()
            .any(|c| c.name == "x" && c.error.is_some()));
        assert_eq!(
            field(&s, m, &data[..5], "x")
                .as_deref()
                .map(|t| t.starts_with('<')),
            Some(true)
        );
    }
}
