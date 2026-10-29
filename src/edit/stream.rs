//! Состав потока: удаление, смена типа и вставка сообщений поверх файла —
//! в смысле команд tll-patch `Remove`, `Clear`, `Update`, `Insert`
//! (`agr:stream-ops`); пишет их `encode.rs`.
//!
//! Всё адресуется входным seq — seq файла. На seq одна операция [`Op`];
//! вставки ([`Insert`]) — отдельно, перед своим seq, в дыру — одна. Тело
//! вставки и сменённого типа — [`Body`]: плоские пути детали (`header.status`,
//! `trailer.extras[0].key`) → текст значения; из разобранного сообщения его
//! собирает [`body_of`], в yaml для показа вкладывает [`body_yaml`].
//!
//! Перенумерацию вид не видит (`agr:view-input-seq`): seq после записи и
//! последний seq файла считает [`SeqMap`] по самим командам, без прохода —
//! как запись: вставка в живой seq сдвигает всё после неё на +1, `Remove` —
//! на −1, `Clear` и вставка в дыру — не сдвигают (`agr:seq-forecast`).
//!
//! Наружу: [`Op`], [`Insert`], [`Body`], [`body_of`], [`body_yaml`], [`SeqMap`],
//! [`Forecast`]; для `encode.rs` — [`tree`] (тело деревом), [`Y`] (его узел) и
//! [`BYTES`] (метка байтового значения).

use std::collections::BTreeMap;

use crate::decode::{Node, Value};

/// Тело сообщения для `Update` и `Insert`: путь поля → текст значения.
pub type Body = BTreeMap<String, String>;

/// Операция над сообщением файла.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// Удалить; `renumber` — seq дальше −1 (`Remove`), иначе дыра (`Clear`).
    /// `next` — следующий seq файла, если известен: подряд идущие удаления
    /// одного рода сливаются в одну команду-диапазон.
    Delete {
        name: String,
        renumber: bool,
        next: Option<i64>,
    },
    /// Сменить тип: тело целиком из `body` (`Update`), правки полей
    /// сообщения вливаются в него. `from` — тип в файле; `next` — как у
    /// удаления: подряд идущие смены на один тип с одним телом — одна
    /// команда-диапазон.
    Retype {
        from: String,
        name: String,
        body: Body,
        next: Option<i64>,
    },
}

/// Новое сообщение перед seq.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Insert {
    pub name: String,
    pub body: Body,
    /// Seq, перед которым вставка, есть в файле: тогда seq дальше +1, иначе
    /// вставка ложится в дыру и никого не сдвигает.
    pub occupied: bool,
    /// Тело, собранное по `body` при проверке, — для строки вставки в виде.
    pub data: Vec<u8>,
}

/// Метка байтового значения в [`Body`]: дальше — hex, в yaml — `"\xNN"`.
pub(crate) const BYTES: char = '\u{1}';

/// Тело разобранного сообщения `data`: все скаляры, включая элементы
/// списков, — в виде, который разберёт [`super::encode`]: биты — числом,
/// байты — hex за меткой (хвостовые нули — по умолчанию), так что копия
/// сообщения совпадает с исходником байт в байт.
pub fn body_of(root: &Node, data: &[u8]) -> Body {
    root.walk()
        .into_iter()
        .filter(|(_, n)| n.value.is_some() && n.error.is_none())
        .map(|(_, n)| {
            let raw = data.get(n.offset..n.offset + n.size).unwrap_or(&[]);
            let v = match &n.value {
                Some(Value::Bits(_)) => {
                    let mut b = [0u8; 8];
                    let k = raw.len().min(8);
                    b[..k].copy_from_slice(&raw[..k]);
                    u64::from_le_bytes(b).to_string()
                }
                Some(Value::Bytes(b)) => {
                    let end = b.iter().rposition(|&x| x != 0).map_or(0, |i| i + 1);
                    let hex: String = b[..end].iter().map(|x| format!("{x:02x}")).collect();
                    format!("{BYTES}{hex}")
                }
                _ => n.text(),
            };
            (n.path.clone(), v)
        })
        .collect()
}

/// Значение для yaml: текст — в одинарных кавычках, байты — в двойных с
/// `\xNN`.
fn scalar(v: &str) -> String {
    let Some(hex) = v.strip_prefix(BYTES) else {
        return super::quote(v);
    };
    let mut out = String::from("\"");
    for i in (0..hex.len()).step_by(2) {
        let Ok(b) = u8::from_str_radix(&hex[i..(i + 2).min(hex.len())], 16) else {
            continue;
        };
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x20..=0x7e => out.push(b as char),
            _ => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out.push('"');
    out
}

/// Узел тела деревом: поле, список или значение.
pub(crate) enum Y {
    Map(BTreeMap<String, Y>),
    Seq(BTreeMap<usize, Y>),
    Leaf(String),
}

/// Шаг пути: имя поля или номер элемента.
enum Step {
    Name(String),
    Index(usize),
}

fn steps(path: &str) -> Vec<Step> {
    let mut out = Vec::new();
    for part in path.split('.') {
        let mut rest = part;
        if let Some(i) = rest.find('[') {
            out.push(Step::Name(rest[..i].to_string()));
            rest = &rest[i..];
            while let Some(r) = rest.strip_prefix('[') {
                let Some(end) = r.find(']') else { break };
                if let Ok(n) = r[..end].parse() {
                    out.push(Step::Index(n));
                }
                rest = &r[end + 1..];
            }
        } else {
            out.push(Step::Name(rest.to_string()));
        }
    }
    out
}

fn put(node: &mut Y, steps: &[Step], value: &str) {
    let Some((first, rest)) = steps.split_first() else {
        *node = Y::Leaf(value.to_string());
        return;
    };
    let fresh = || match rest.first() {
        Some(Step::Index(_)) => Y::Seq(BTreeMap::new()),
        Some(Step::Name(_)) => Y::Map(BTreeMap::new()),
        None => Y::Leaf(String::new()),
    };
    match first {
        Step::Name(k) => {
            if !matches!(node, Y::Map(_)) {
                *node = Y::Map(BTreeMap::new());
            }
            let Y::Map(m) = node else { unreachable!() };
            put(m.entry(k.clone()).or_insert_with(fresh), rest, value);
        }
        Step::Index(i) => {
            if !matches!(node, Y::Seq(_)) {
                *node = Y::Seq(BTreeMap::new());
            }
            let Y::Seq(s) = node else { unreachable!() };
            put(s.entry(*i).or_insert_with(fresh), rest, value);
        }
    }
}

fn emit(out: &mut String, node: &Y, indent: usize) {
    let pad = " ".repeat(indent);
    match node {
        Y::Map(m) => {
            for (k, v) in m {
                match v {
                    Y::Leaf(s) => out.push_str(&format!("{pad}{k}: {}\n", scalar(s))),
                    Y::Map(c) if c.is_empty() => out.push_str(&format!("{pad}{k}: {{}}\n")),
                    Y::Seq(c) if c.is_empty() => out.push_str(&format!("{pad}{k}: []\n")),
                    _ => {
                        out.push_str(&format!("{pad}{k}:\n"));
                        emit(out, v, indent + 2);
                    }
                }
            }
        }
        Y::Seq(s) => {
            for v in s.values() {
                match v {
                    Y::Leaf(x) => out.push_str(&format!("{pad}- {}\n", scalar(x))),
                    _ => {
                        // Первая строка элемента — за дефисом, остальные — под ней.
                        let mut inner = String::new();
                        emit(&mut inner, v, indent + 2);
                        let inner = inner.replacen(&format!("{pad}  "), &format!("{pad}- "), 1);
                        out.push_str(&inner);
                    }
                }
            }
        }
        Y::Leaf(s) => out.push_str(&format!("{pad}{}\n", scalar(s))),
    }
}

/// Тело деревом: пути вкладываются в словари, `[i]` — в списки по порядку
/// номеров (пропуски номеров сжимаются, как у списка yaml).
pub(crate) fn tree(body: &Body) -> BTreeMap<String, Y> {
    let mut root = Y::Map(BTreeMap::new());
    for (path, value) in body {
        put(&mut root, &steps(path), value);
    }
    match root {
        Y::Map(m) => m,
        _ => BTreeMap::new(),
    }
}

/// Тело в yaml-поля команды tll-patch с отступом `indent` — для показа правок
/// и переноса в tll-patch.
pub fn body_yaml(body: &Body, indent: usize) -> String {
    let mut out = String::new();
    emit(&mut out, &Y::Map(tree(body)), indent);
    out
}

/// Сдвиг seq после записи по командам (`agr:seq-forecast`): точки, где он
/// меняется, и вставки.
#[derive(Debug, Clone, Default)]
pub struct SeqMap {
    /// Удаления со сдвигом (`Remove`).
    removed: Vec<i64>,
    /// Удаления без сдвига (`Clear`).
    cleared: Vec<i64>,
    /// seq → (сколько вставок, в живой ли seq).
    inserts: BTreeMap<i64, (usize, bool)>,
}

impl SeqMap {
    pub fn new(ops: &BTreeMap<i64, Op>, inserts: &BTreeMap<i64, Vec<Insert>>) -> SeqMap {
        let mut m = SeqMap::default();
        for (seq, op) in ops {
            if let Op::Delete { renumber, .. } = op {
                if *renumber {
                    m.removed.push(*seq);
                } else {
                    m.cleared.push(*seq);
                }
            }
        }
        for (seq, v) in inserts {
            if !v.is_empty() {
                m.inserts
                    .insert(*seq, (v.len(), v.iter().any(|i| i.occupied)));
            }
        }
        m
    }

    /// Сдвиг seq перед `seq` от всех команд левее: вставки в живые seq минус
    /// удаления со сдвигом. Для части склейки — сдвиг, который запись вносит
    /// сама: канал части об этих командах не знает (`agr:glue-commit`).
    pub fn before(&self, seq: i64) -> i64 {
        let up: usize = self
            .inserts
            .range(..seq)
            .filter(|(_, (_, occ))| *occ)
            .map(|(_, (n, _))| n)
            .sum();
        let down = self.removed.partition_point(|&r| r < seq);
        up as i64 - down as i64
    }

    /// Удалено ли сообщение.
    pub fn deleted(&self, seq: i64) -> bool {
        self.removed.binary_search(&seq).is_ok() || self.cleared.binary_search(&seq).is_ok()
    }

    /// Seq сообщения файла после записи; удалённое — `None`.
    pub fn out(&self, seq: i64) -> Option<i64> {
        if self.deleted(seq) {
            return None;
        }
        let here = match self.inserts.get(&seq) {
            Some((n, true)) => *n as i64,
            _ => 0,
        };
        Some(seq + self.before(seq) + here)
    }

    /// Seq `k`-й вставки перед `seq` после записи.
    pub fn out_insert(&self, seq: i64, k: usize) -> i64 {
        let occupied = self.inserts.get(&seq).is_some_and(|(_, o)| *o);
        seq + self.before(seq) + if occupied { k as i64 } else { 0 }
    }

    /// Последний seq после записи: `last` — последний seq файла, `alive` —
    /// последний неудалённый seq файла (если `last` удалён, его ищет
    /// читатель). Вставки за последним seq не бывает (`agr:stream-ops`).
    pub fn last(&self, last: i64, alive: Option<i64>) -> Option<i64> {
        if let Some(s) = self.out(last) {
            return Some(s);
        }
        let mut best = alive.and_then(|a| self.out(a));
        for (&seq, &(n, _)) in self.inserts.range(..=last) {
            if alive.is_none_or(|a| seq > a) {
                let s = self.out_insert(seq, n.saturating_sub(1));
                best = Some(best.map_or(s, |b: i64| b.max(s)));
            }
        }
        best
    }
}

/// Прогноз для первой строки фиксации: последний seq до и после записи.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forecast {
    pub before: i64,
    /// `None` — после записи сообщений не останется.
    pub after: Option<i64>,
    /// Включён transform: прогноз его не учитывает.
    pub transform: bool,
}

impl Forecast {
    /// Сменится ли последний seq.
    pub fn changes(&self) -> bool {
        self.after != Some(self.before)
    }

    /// Строка для диалога фиксации.
    pub fn line(&self) -> String {
        let mut s = match self.after {
            Some(a) if a == self.before => format!("последний seq {a} — не меняется"),
            Some(a) => format!("последний seq {} → {a} — сменится", self.before),
            None => format!("последний seq {} → сообщений не останется", self.before),
        };
        if self.transform {
            s.push_str(" (без учёта transform)");
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn del(renumber: bool) -> Op {
        Op::Delete {
            name: "Order".into(),
            renumber,
            next: None,
        }
    }

    fn ins(occupied: bool) -> Insert {
        Insert {
            name: "Order".into(),
            body: Body::new(),
            occupied,
            data: Vec::new(),
        }
    }

    /// `agr:stream-ops` — пути вкладываются в словари, `[i]` — в списки.
    #[test]
    fn body_yaml_nests_paths_and_lists() {
        let mut b = Body::new();
        b.insert("header.ts".into(), "1".into());
        b.insert("header.id".into(), "2".into());
        b.insert("market[0]".into(), "SPB".into());
        b.insert("market[1]".into(), "MOEX".into());
        b.insert("trailer.extras[0].key".into(), "10".into());
        b.insert("trailer.extras[0].value".into(), "100".into());
        b.insert("trailer.extras[1].key".into(), "20".into());
        assert_eq!(
            body_yaml(&b, 4),
            "    header:\n      id: '2'\n      ts: '1'\n    market:\n      - 'SPB'\n      - 'MOEX'\n\
             \x20   trailer:\n      extras:\n        - key: '10'\n          value: '100'\n        - key: '20'\n"
        );
    }

    /// `agr:seq-forecast` — удаление со сдвигом опускает seq дальше на 1, дыра
    /// — нет.
    #[test]
    fn removal_shifts_later_seqs_down() {
        let mut ops = BTreeMap::new();
        ops.insert(5, del(true));
        ops.insert(8, del(false));
        let m = SeqMap::new(&ops, &BTreeMap::new());
        assert_eq!(m.out(4), Some(4));
        assert_eq!(m.out(5), None);
        assert_eq!(m.out(6), Some(5));
        assert_eq!(m.out(8), None);
        assert_eq!(m.out(9), Some(8));
        assert_eq!(m.last(10, None), Some(9));
    }

    /// `agr:seq-forecast` — вставка перед живым seq сдвигает его и всё дальше
    /// на число вставок.
    #[test]
    fn insert_before_existing_shifts_later_seqs_up() {
        let mut ins_map = BTreeMap::new();
        ins_map.insert(5, vec![ins(true), ins(true)]);
        let m = SeqMap::new(&BTreeMap::new(), &ins_map);
        assert_eq!(m.out(4), Some(4));
        assert_eq!(m.out_insert(5, 0), 5);
        assert_eq!(m.out_insert(5, 1), 6);
        assert_eq!(m.out(5), Some(7));
        assert_eq!(m.out(9), Some(11));
    }

    /// `agr:seq-forecast` — вставка в дыру никого не сдвигает.
    #[test]
    fn insert_into_gap_keeps_later_seqs() {
        let mut ins_map = BTreeMap::new();
        ins_map.insert(15, vec![ins(false)]);
        let m = SeqMap::new(&BTreeMap::new(), &ins_map);
        assert_eq!(m.out_insert(15, 0), 15);
        assert_eq!(m.out(20), Some(20));
        assert_eq!(m.last(100, None), Some(100));
    }

    /// `agr:seq-forecast` — дыра и вставка на одном seq сдвигают хвост: для
    /// записи сообщение под `Clear` всё ещё входное.
    #[test]
    fn clear_and_insert_at_one_seq_shift_the_tail() {
        let mut ops = BTreeMap::new();
        ops.insert(5, del(false));
        let mut ins_map = BTreeMap::new();
        ins_map.insert(5, vec![ins(true)]);
        let m = SeqMap::new(&ops, &ins_map);
        assert_eq!(m.out_insert(5, 0), 5);
        assert_eq!(m.out(6), Some(7));
    }

    /// `agr:seq-forecast` — сколько удалено со сдвигом, столько вставлено —
    /// последний seq прежний.
    #[test]
    fn equal_removes_and_inserts_keep_last_seq() {
        let mut ops = BTreeMap::new();
        for s in 3..6 {
            ops.insert(s, del(true));
        }
        let mut ins_map = BTreeMap::new();
        ins_map.insert(7, vec![ins(true), ins(true), ins(true)]);
        let m = SeqMap::new(&ops, &ins_map);
        let f = Forecast {
            before: 10,
            after: m.last(10, None),
            transform: false,
        };
        assert!(!f.changes(), "{}", f.line());
        assert_eq!(f.line(), "последний seq 10 — не меняется");
    }

    /// `agr:seq-forecast` — удалён хвост: последним становится предыдущий
    /// живой или вставка за ним.
    #[test]
    fn removed_tail_takes_last_seq_from_previous_alive() {
        let mut ops = BTreeMap::new();
        ops.insert(9, del(true));
        ops.insert(10, del(true));
        let m = SeqMap::new(&ops, &BTreeMap::new());
        assert_eq!(m.last(10, Some(8)), Some(8));
        let mut ins_map = BTreeMap::new();
        ins_map.insert(10, vec![ins(true)]);
        let m = SeqMap::new(&ops, &ins_map);
        // 9 удалён (−1), вставка перед 10 ложится на 10 − 1 = 9.
        assert_eq!(m.last(10, Some(8)), Some(9));
        let f = Forecast {
            before: 10,
            after: m.last(10, Some(8)),
            transform: true,
        };
        assert_eq!(
            f.line(),
            "последний seq 10 → 9 — сменится (без учёта transform)"
        );
    }
}
