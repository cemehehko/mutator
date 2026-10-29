//! Правки полей: что поменяно, yaml для tll-patch и сводка для диалога фиксации.
//!
//! Правка — новое значение поля сообщения по пути tll-patch (`header.status`,
//! `agr:patch-path`). Все правки одного сообщения — одна команда `Field`: на
//! один seq tll-patch берёт только одну. Значение пишется текстом, как его
//! показывает деталь и как его разбирает tll-patch (enum — имя, bits — `a|b`,
//! Decimal и время — строкой). Вернуть полю старое значение — снять правку.
//!
//! Файл не меняется: вид читает его через tll-patch с yaml из [`Store`]
//! (`agr:patch-overlay`); каждая версия правок — новый файл, чтобы чтения в
//! полёте дочитали свою. Правки переживают перезапуск в сессии файла
//! ([`Saved`], `agr:edits-in-session`). Записать новый файл — `commit.rs`.
//!
//! Кроме полей — состав потока: удаление, смена типа и вставка
//! (`agr:stream-ops`, `stream.rs`). Вид читает yaml только с `Field` и
//! `Update` ([`Store::write`]), фиксация — полный, с перенумерацией
//! ([`Edits::yaml`], `agr:view-input-seq`).
//!
//! Файлы: `commit.rs` — фиксация: прогон файла через tll-patch в новый файл,
//! свободные имена, бэкап; `stream.rs` — операции над составом потока и
//! прогноз seq после записи.
//!
//! Наружу: [`Edits`], [`MsgEdit`], [`FieldEdit`], [`Summary`], [`Saved`],
//! [`SavedOp`], [`Store`], [`commit`], [`stream`]; из `stream.rs` — [`Op`],
//! [`Insert`], [`Body`], [`SeqMap`], [`Forecast`].

pub mod commit;
pub mod stream;

pub use stream::{Body, Forecast, Insert, Op, SeqMap};

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Правка одного поля: значение в файле и новое.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldEdit {
    pub old: String,
    pub new: String,
}

/// Правки одного сообщения.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MsgEdit {
    /// Тип сообщения: tll-patch сверяет его с телом.
    pub name: String,
    /// Путь поля → правка.
    pub fields: BTreeMap<String, FieldEdit>,
}

/// Все правки файла по входному seq.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Edits {
    msgs: BTreeMap<i64, MsgEdit>,
    /// Удаление или смена типа — одна на seq (`agr:stream-ops`).
    ops: BTreeMap<i64, Op>,
    /// Вставки перед seq, по порядку.
    inserts: BTreeMap<i64, Vec<Insert>>,
}

/// Правка в сессии файла.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    pub seq: i64,
    pub name: String,
    pub path: String,
    pub old: String,
    pub new: String,
}

/// Удаление, смена типа или вставка в сессии файла.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedOp {
    pub seq: i64,
    /// `remove`, `clear`, `retype`, `insert`.
    pub kind: String,
    pub name: String,
    /// Для `retype` — тип в файле.
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub body: Body,
    /// Для удаления — следующий seq файла; для вставки — в живой ли seq.
    #[serde(default)]
    pub next: Option<i64>,
    #[serde(default)]
    pub occupied: bool,
    /// Для вставки — тело от tll-patch, hex.
    #[serde(default)]
    pub data: String,
}

/// Сводка для диалога фиксации: строка на поле и итоги.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub lines: Vec<String>,
    /// (что, сколько): сообщений, полей, по типам, диапазон seq.
    pub totals: Vec<(String, String)>,
}

impl Edits {
    pub fn is_empty(&self) -> bool {
        self.msgs.is_empty() && self.ops.is_empty() && self.inserts.is_empty()
    }

    /// Сколько всего правок: полей, удалений, смен типа и вставок.
    pub fn count(&self) -> usize {
        self.fields() + self.ops.len() + self.inserts.values().map(Vec::len).sum::<usize>()
    }

    /// Операция над сообщением, если есть.
    pub fn op(&self, seq: i64) -> Option<&Op> {
        self.ops.get(&seq)
    }

    /// Сообщение удалено.
    pub fn deleted(&self, seq: i64) -> bool {
        matches!(self.ops.get(&seq), Some(Op::Delete { .. }))
    }

    /// Вставки перед seq.
    pub fn inserts(&self, seq: i64) -> &[Insert] {
        self.inserts.get(&seq).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Все вставки: seq → список.
    pub fn all_inserts(&self) -> impl Iterator<Item = (i64, &[Insert])> {
        self.inserts.iter().map(|(s, v)| (*s, v.as_slice()))
    }

    /// Есть ли что-то с seq в `[from, to]`: правка поля, операция или вставка —
    /// части склейки с таким диапазоном есть что писать (`agr:glue-commit`).
    pub fn touches(&self, from: i64, to: i64) -> bool {
        self.msgs.range(from..=to).next().is_some()
            || self.ops.range(from..=to).next().is_some()
            || self.inserts.range(from..=to).next().is_some()
    }

    /// Карта seq после записи (`agr:seq-forecast`).
    pub fn seq_map(&self) -> SeqMap {
        SeqMap::new(&self.ops, &self.inserts)
    }

    /// Удалить сообщение: со сдвигом seq дальше (`renumber`) или дырой.
    /// Правки полей и смена типа этого сообщения снимаются — удалённое не
    /// правится. `next` — следующий seq файла, для слияния в диапазон.
    pub fn delete(&mut self, seq: i64, name: &str, renumber: bool, next: Option<i64>) {
        self.msgs.remove(&seq);
        let name = match self.ops.remove(&seq) {
            Some(Op::Retype { from, .. }) => from,
            _ => name.to_string(),
        };
        self.ops.insert(
            seq,
            Op::Delete {
                name,
                renumber,
                next,
            },
        );
    }

    /// Снять удаление или смену типа; `true` — было что снимать.
    pub fn restore(&mut self, seq: i64) -> bool {
        let had = self.ops.remove(&seq).is_some();
        if had {
            self.msgs.remove(&seq);
        }
        had
    }

    /// Сменить тип сообщения на `name` с телом `body`; вернуть исходный тип —
    /// снять смену. Прежние правки полей уже в `body` (его собирают с вида).
    pub fn retype(&mut self, seq: i64, from: &str, name: &str, body: Body) -> Result<(), String> {
        let from = match self.ops.get(&seq) {
            Some(Op::Delete { .. }) => return Err(format!("seq {seq} удалён")),
            Some(Op::Retype { from, .. }) => from.clone(),
            None => from.to_string(),
        };
        self.msgs.remove(&seq);
        if from == name {
            self.ops.remove(&seq);
        } else {
            self.ops.insert(
                seq,
                Op::Retype {
                    from,
                    name: name.to_string(),
                    body,
                    next: None,
                },
            );
        }
        Ok(())
    }

    /// Сменить тип у многих сообщений одним телом: `(seq, тип в файле,
    /// следующий seq файла)`. Удалённые и уже этого типа не трогаются, правки
    /// их полей снимаются; ответ — сколько сменено.
    pub fn retype_many(
        &mut self,
        msgs: &[(i64, String, Option<i64>)],
        name: &str,
        body: &Body,
    ) -> usize {
        let mut n = 0;
        for (seq, from, next) in msgs {
            let from = match self.ops.get(seq) {
                Some(Op::Delete { .. }) => continue,
                Some(Op::Retype { from, .. }) => from.clone(),
                None => from.clone(),
            };
            if from == name {
                continue;
            }
            self.msgs.remove(seq);
            self.ops.insert(
                *seq,
                Op::Retype {
                    from,
                    name: name.to_string(),
                    body: body.clone(),
                    next: *next,
                },
            );
            n += 1;
        }
        n
    }

    /// Вставить сообщение перед seq; ответ — его номер среди вставок seq.
    /// В дыру — одна вставка: вторая легла бы на тот же seq.
    pub fn insert(&mut self, seq: i64, ins: Insert) -> Result<usize, String> {
        if !ins.occupied && !self.inserts(seq).is_empty() {
            return Err(format!("в дыру перед seq {seq} — одна вставка"));
        }
        let v = self.inserts.entry(seq).or_default();
        v.push(ins);
        Ok(v.len() - 1)
    }

    /// Убрать `k`-ю вставку перед seq; `true` — была.
    pub fn drop_insert(&mut self, seq: i64, k: usize) -> bool {
        let Some(v) = self.inserts.get_mut(&seq) else {
            return false;
        };
        if k >= v.len() {
            return false;
        }
        v.remove(k);
        if v.is_empty() {
            self.inserts.remove(&seq);
        }
        true
    }

    /// Тело `k`-й вставки перед seq, собранное tll-patch.
    pub fn set_insert_data(&mut self, seq: i64, k: usize, data: Vec<u8>) {
        if let Some(i) = self.inserts.get_mut(&seq).and_then(|v| v.get_mut(k)) {
            i.data = data;
        }
    }

    /// Seq удалённых сообщений.
    pub fn deleted_seqs(&self) -> Vec<i64> {
        self.ops
            .iter()
            .filter(|(_, o)| matches!(o, Op::Delete { .. }))
            .map(|(s, _)| *s)
            .collect()
    }

    /// Поставить полю `k`-й вставки перед seq значение.
    pub fn set_insert(&mut self, seq: i64, k: usize, path: &str, value: &str) -> bool {
        match self.inserts.get_mut(&seq).and_then(|v| v.get_mut(k)) {
            Some(i) => {
                i.body.insert(path.to_string(), value.to_string());
                true
            }
            None => false,
        }
    }

    /// Сколько сообщений правится.
    pub fn messages(&self) -> usize {
        self.msgs.len()
    }

    /// Сколько полей правится.
    pub fn fields(&self) -> usize {
        self.msgs.values().map(|m| m.fields.len()).sum()
    }

    pub fn iter(&self) -> impl Iterator<Item = (i64, &MsgEdit)> {
        self.msgs.iter().map(|(s, m)| (*s, m))
    }

    /// Правка поля, если есть.
    pub fn get(&self, seq: i64, path: &str) -> Option<&FieldEdit> {
        self.msgs.get(&seq)?.fields.get(path)
    }

    /// Правки сообщения, если есть.
    pub fn message(&self, seq: i64) -> Option<&MsgEdit> {
        self.msgs.get(&seq)
    }

    /// Поставить полю значение `new`; `current` — что поле показывает сейчас.
    /// Старым остаётся значение из файла (первой правки); вернуть его — снять
    /// правку.
    pub fn set(&mut self, seq: i64, name: &str, path: &str, current: &str, new: &str) {
        if self.deleted(seq) {
            // Удалённое не правится (`agr:stream-ops`): вызывающий говорит об
            // этом сам.
            return;
        }
        let old = self
            .get(seq, path)
            .map(|e| e.old.clone())
            .unwrap_or_else(|| current.to_string());
        if old == new {
            self.unset(seq, path);
            return;
        }
        let m = self.msgs.entry(seq).or_insert_with(|| MsgEdit {
            name: name.to_string(),
            fields: BTreeMap::new(),
        });
        m.fields.insert(
            path.to_string(),
            FieldEdit {
                old,
                new: new.to_string(),
            },
        );
    }

    /// Снять правку поля; `true` — она была.
    pub fn unset(&mut self, seq: i64, path: &str) -> bool {
        let Some(m) = self.msgs.get_mut(&seq) else {
            return false;
        };
        let had = m.fields.remove(path).is_some();
        if m.fields.is_empty() {
            self.msgs.remove(&seq);
        }
        had
    }

    /// Полный yaml команд tll-patch — для фиксации: `Field` на сообщение,
    /// `Update` на смену типа, `Remove`/`Clear` (подряд идущие — одним
    /// диапазоном `from`/`till`), `Insert`. Значения — строками в одинарных
    /// кавычках (tll-config сам приводит их к типу поля).
    pub fn yaml(&self) -> String {
        self.commands(true)
    }

    /// yaml для вида: только `Field` и `Update` — они не перенумеровывают, и
    /// чтение с середины файла их не путает (`agr:view-input-seq`).
    pub fn view_yaml(&self) -> String {
        self.commands(false)
    }

    /// Одна команда `Insert` — проверить вставку до принятия.
    pub fn insert_yaml(seq: i64, ins: &Insert) -> String {
        format!(
            "- cmd: Insert\n  seq: {seq}\n  name: {}\n{}",
            quote(&ins.name),
            fields_yaml(&ins.body)
        )
    }

    fn commands(&self, full: bool) -> String {
        let mut out = String::new();
        let mut seqs: std::collections::BTreeSet<i64> = self.msgs.keys().copied().collect();
        seqs.extend(self.ops.keys().copied());
        if full {
            seqs.extend(self.inserts.keys().copied());
        }
        // Незакрытый диапазон: (что, from, последний, следующий seq файла).
        let mut run: Option<(Run, i64, i64, Option<i64>)> = None;
        let flush = |out: &mut String, run: &mut Option<(Run, i64, i64, Option<i64>)>| {
            let Some((what, from, last, _)) = run.take() else {
                return;
            };
            let cmd = match &what {
                Run::Delete(true) => "Remove",
                Run::Delete(false) => "Clear",
                Run::Retype(..) => "Update",
            };
            if from == last {
                out.push_str(&format!("- cmd: {cmd}\n  seq: {from}\n"));
            } else {
                out.push_str(&format!(
                    "- cmd: {cmd}\n  from: {from}\n  till: {}\n",
                    last + 1
                ));
            }
            if let Run::Retype(name, body) = &what {
                out.push_str(&format!("  name: {}\n{}", quote(name), fields_yaml(body)));
            }
        };
        // Продолжить диапазон или начать новый.
        let extend = |out: &mut String,
                      run: &mut Option<(Run, i64, i64, Option<i64>)>,
                      what: Run,
                      seq: i64,
                      next: Option<i64>| {
            match run {
                Some((w, _, last, nx)) if *w == what && (seq == *last + 1 || *nx == Some(seq)) => {
                    *last = seq;
                    *nx = next;
                }
                _ => {
                    flush(out, run);
                    *run = Some((what, seq, seq, next));
                }
            }
        };
        for seq in seqs {
            if full {
                if let Some(v) = self.inserts.get(&seq) {
                    // Вставка перед seq рвёт диапазон: она между удалёнными.
                    flush(&mut out, &mut run);
                    for ins in v {
                        out.push_str(&Self::insert_yaml(seq, ins));
                    }
                }
            }
            match self.ops.get(&seq) {
                Some(Op::Delete { renumber, next, .. }) => {
                    if full {
                        extend(&mut out, &mut run, Run::Delete(*renumber), seq, *next);
                    }
                }
                Some(Op::Retype {
                    name, body, next, ..
                }) => match self.msgs.get(&seq) {
                    // Правки полей — своё тело: отдельной командой.
                    Some(m) => {
                        flush(&mut out, &mut run);
                        let mut b = body.clone();
                        for (path, e) in &m.fields {
                            b.insert(path.clone(), e.new.clone());
                        }
                        out.push_str(&format!(
                            "- cmd: Update\n  seq: {seq}\n  name: {}\n{}",
                            quote(name),
                            fields_yaml(&b)
                        ));
                    }
                    None => extend(
                        &mut out,
                        &mut run,
                        Run::Retype(name.clone(), body.clone()),
                        seq,
                        *next,
                    ),
                },
                None => {
                    let Some(m) = self.msgs.get(&seq) else {
                        continue;
                    };
                    flush(&mut out, &mut run);
                    out.push_str(&format!(
                        "- cmd: Field\n  seq: {seq}\n  name: {}\n  fields:\n",
                        quote(&m.name)
                    ));
                    for (path, e) in &m.fields {
                        out.push_str(&format!("    {path}: {}\n", quote(&e.new)));
                    }
                }
            }
        }
        flush(&mut out, &mut run);
        out
    }

    /// Сводка: «seq тип путь: было → стало» и итоги.
    pub fn summary(&self) -> Summary {
        let mut lines = Vec::new();
        let mut by_type: BTreeMap<&str, usize> = BTreeMap::new();
        let map = self.seq_map();
        let after = |seq: i64| match map.out(seq) {
            Some(o) if o != seq => format!(" (станет {o})"),
            _ => String::new(),
        };
        for (seq, op) in &self.ops {
            lines.push(match op {
                Op::Delete {
                    name,
                    renumber: true,
                    ..
                } => format!("{seq} {name}: удалено, seq дальше −1"),
                Op::Delete { name, .. } => format!("{seq} {name}: удалено, дыра"),
                Op::Retype { from, name, .. } => {
                    format!("{seq} {from} → {name}: тип сменён{}", after(*seq))
                }
            });
        }
        for (seq, v) in &self.inserts {
            for (k, i) in v.iter().enumerate() {
                lines.push(format!(
                    "{} {}: вставка перед {seq}",
                    map.out_insert(*seq, k),
                    i.name
                ));
            }
        }
        for (seq, m) in &self.msgs {
            *by_type.entry(&m.name).or_default() += 1;
            for (path, e) in &m.fields {
                lines.push(format!("{seq} {} {path}: {} → {}", m.name, e.old, e.new));
            }
        }
        let mut totals = vec![
            ("сообщений".to_string(), self.messages().to_string()),
            ("полей".to_string(), self.fields().to_string()),
        ];
        let (mut removed, mut cleared, mut retyped) = (0, 0, 0);
        for op in self.ops.values() {
            match op {
                Op::Delete { renumber: true, .. } => removed += 1,
                Op::Delete { .. } => cleared += 1,
                Op::Retype { .. } => retyped += 1,
            }
        }
        let inserted: usize = self.inserts.values().map(Vec::len).sum();
        for (k, n) in [
            ("удалено со сдвигом", removed),
            ("удалено дырой", cleared),
            ("сменён тип", retyped),
            ("вставлено", inserted),
        ] {
            if n > 0 {
                totals.push((k.to_string(), n.to_string()));
            }
        }
        if let (Some(a), Some(b)) = (self.msgs.keys().next(), self.msgs.keys().next_back()) {
            totals.push((
                "seq".to_string(),
                if a == b {
                    a.to_string()
                } else {
                    format!("{a}…{b}")
                },
            ));
        }
        for (t, n) in by_type {
            totals.push((t.to_string(), n.to_string()));
        }
        Summary { lines, totals }
    }

    /// Для сессии.
    pub fn saved(&self) -> Vec<Saved> {
        self.msgs
            .iter()
            .flat_map(|(seq, m)| {
                m.fields.iter().map(move |(path, e)| Saved {
                    seq: *seq,
                    name: m.name.clone(),
                    path: path.clone(),
                    old: e.old.clone(),
                    new: e.new.clone(),
                })
            })
            .collect()
    }

    /// Состав потока для сессии.
    pub fn saved_ops(&self) -> Vec<SavedOp> {
        let mut out = Vec::new();
        for (seq, op) in &self.ops {
            out.push(match op {
                Op::Delete {
                    name,
                    renumber,
                    next,
                } => SavedOp {
                    seq: *seq,
                    kind: if *renumber { "remove" } else { "clear" }.into(),
                    name: name.clone(),
                    from: String::new(),
                    body: Body::new(),
                    next: *next,
                    occupied: false,
                    data: String::new(),
                },
                Op::Retype {
                    from,
                    name,
                    body,
                    next,
                } => SavedOp {
                    seq: *seq,
                    kind: "retype".into(),
                    name: name.clone(),
                    from: from.clone(),
                    body: body.clone(),
                    next: *next,
                    occupied: false,
                    data: String::new(),
                },
            });
        }
        for (seq, v) in &self.inserts {
            for i in v {
                out.push(SavedOp {
                    seq: *seq,
                    kind: "insert".into(),
                    name: i.name.clone(),
                    from: String::new(),
                    body: i.body.clone(),
                    next: None,
                    occupied: i.occupied,
                    data: i.data.iter().map(|b| format!("{b:02x}")).collect(),
                });
            }
        }
        out
    }

    /// Из сессии: правки полей и состав потока (старые сессии — без него).
    pub fn from_saved(s: &[Saved]) -> Edits {
        Edits::from_session(s, &[])
    }

    /// Из сессии целиком: сначала состав потока, потом поля — правки полей
    /// сменённого типа ложатся поверх его тела.
    pub fn from_session(s: &[Saved], ops: &[SavedOp]) -> Edits {
        let mut e = Edits::default();
        for o in ops {
            match o.kind.as_str() {
                "remove" | "clear" => e.delete(o.seq, &o.name, o.kind == "remove", o.next),
                "retype" => {
                    e.retype_many(&[(o.seq, o.from.clone(), o.next)], &o.name, &o.body);
                }
                "insert" => {
                    let _ = e.insert(
                        o.seq,
                        Insert {
                            name: o.name.clone(),
                            body: o.body.clone(),
                            occupied: o.occupied,
                            data: unhex(&o.data),
                        },
                    );
                }
                _ => {}
            }
        }
        for x in s {
            e.set(x.seq, &x.name, &x.path, &x.old, &x.new);
        }
        e
    }
}

/// Что сливается в команду-диапазон: удаление одного рода или смена на один
/// тип с одним телом (`agr:stream-ops`).
#[derive(PartialEq)]
enum Run {
    Delete(bool),
    Retype(String, Body),
}

/// `fields:` команды: пустое тело — `{}`, иначе вложенный yaml.
fn fields_yaml(body: &Body) -> String {
    if body.is_empty() {
        "  fields: {}\n".into()
    } else {
        format!("  fields:\n{}", stream::body_yaml(body, 4))
    }
}

/// Байты из hex сессии; битое — пусто.
fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .filter_map(|i| u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok())
        .collect()
}

/// Строка YAML в одинарных кавычках: внутри удваивается только `'`.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Версии yaml правок во временном каталоге процесса; каталог убирается вместе
/// со [`Store`].
#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
    n: u64,
}

impl Default for Store {
    fn default() -> Self {
        Self::new()
    }
}

impl Store {
    pub fn new() -> Store {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let k = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Store {
            dir: std::env::temp_dir().join(format!("tui-patch-{}-{k}", std::process::id())),
            n: 0,
        }
    }

    /// Записать новую версию yaml для вида ([`Edits::view_yaml`]); `None` —
    /// читать файл как есть.
    pub fn write(&mut self, e: &Edits) -> Result<Option<PathBuf>, String> {
        self.write_text(&e.view_yaml())
    }

    /// Записать полный yaml для фиксации ([`Edits::yaml`]).
    pub fn write_full(&mut self, e: &Edits) -> Result<Option<PathBuf>, String> {
        self.write_text(&e.yaml())
    }

    /// Записать yaml как есть; пустой — `None`.
    pub fn write_text(&mut self, yaml: &str) -> Result<Option<PathBuf>, String> {
        if yaml.is_empty() {
            return Ok(None);
        }
        std::fs::create_dir_all(&self.dir).map_err(|x| format!("{}: {x}", self.dir.display()))?;
        self.n += 1;
        let p = self.dir.join(format!("patch-{}.yaml", self.n));
        std::fs::write(&p, yaml).map_err(|x| format!("{}: {x}", p.display()))?;
        Ok(Some(p))
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_of_one_message_make_one_command() {
        let mut e = Edits::default();
        e.set(5, "Order", "price", "100", "12.5");
        e.set(5, "Order", "header.order_id", "1000", "777");
        e.set(7, "Cancel", "header.status", "OK", "Fail");
        assert_eq!(
            e.yaml(),
            "- cmd: Field\n  seq: 5\n  name: 'Order'\n  fields:\n    header.order_id: '777'\n    price: '12.5'\n\
             - cmd: Field\n  seq: 7\n  name: 'Cancel'\n  fields:\n    header.status: 'Fail'\n"
        );
    }

    #[test]
    fn quotes_are_doubled_in_yaml() {
        let mut e = Edits::default();
        e.set(1, "Order", "comment", "", "it's: #1");
        assert!(e.yaml().contains("comment: 'it''s: #1'\n"));
    }

    #[test]
    fn returning_the_old_value_drops_the_edit() {
        let mut e = Edits::default();
        e.set(5, "Order", "price", "100", "12.5");
        // Деталь теперь показывает 12.5, старым остаётся значение из файла.
        e.set(5, "Order", "price", "12.5", "13");
        assert_eq!(e.get(5, "price").unwrap().old, "100");
        e.set(5, "Order", "price", "13", "100");
        assert!(e.is_empty());
    }

    #[test]
    fn unset_removes_empty_message() {
        let mut e = Edits::default();
        e.set(5, "Order", "price", "100", "1");
        assert!(e.unset(5, "price"));
        assert!(!e.unset(5, "price"));
        assert!(e.message(5).is_none());
    }

    #[test]
    fn summary_counts_messages_fields_and_types() {
        let mut e = Edits::default();
        e.set(5, "Order", "price", "100", "1");
        e.set(5, "Order", "symbol", "SBER", "GAZP");
        e.set(9, "Order", "price", "7", "8");
        e.set(12, "Cancel", "header.status", "OK", "Fail");
        let s = e.summary();
        assert_eq!(s.lines.len(), 4);
        assert_eq!(s.lines[0], "5 Order price: 100 → 1");
        let t: BTreeMap<_, _> = s.totals.into_iter().collect();
        assert_eq!(t["сообщений"], "3");
        assert_eq!(t["полей"], "4");
        assert_eq!(t["seq"], "5…12");
        assert_eq!(t["Order"], "2");
        assert_eq!(t["Cancel"], "1");
    }

    #[test]
    fn saved_edits_come_back_the_same() {
        let mut e = Edits::default();
        e.set(5, "Order", "price", "100", "1");
        e.set(12, "Cancel", "header.status", "OK", "Fail");
        assert_eq!(Edits::from_saved(&e.saved()), e);
    }

    fn removed(e: &mut Edits, seqs: &[i64], renumber: bool) {
        for &s in seqs {
            e.delete(s, "Order", renumber, None);
        }
    }

    /// `agr:stream-ops` — подряд идущие удаления одного рода — одна команда
    /// `from`/`till`, одиночное — `seq`.
    #[test]
    fn consecutive_removals_make_one_range_command() {
        let mut e = Edits::default();
        removed(&mut e, &[3, 4, 5, 9], true);
        assert_eq!(
            e.yaml(),
            "- cmd: Remove\n  from: 3\n  till: 6\n- cmd: Remove\n  seq: 9\n"
        );
        // По следующему seq файла: 10 → 20 → 30 в разреженном файле — подряд.
        let mut e = Edits::default();
        e.delete(10, "Order", false, Some(20));
        e.delete(20, "Order", false, Some(30));
        e.delete(30, "Order", false, Some(40));
        assert_eq!(e.yaml(), "- cmd: Clear\n  from: 10\n  till: 31\n");
    }

    /// `agr:stream-ops` — удаление со сдвигом и дыра в один диапазон не
    /// сливаются, вставка между удалёнными рвёт диапазон.
    #[test]
    fn remove_and_clear_never_merge_into_one_range() {
        let mut e = Edits::default();
        removed(&mut e, &[3, 4], true);
        removed(&mut e, &[5, 6], false);
        e.insert(
            4,
            Insert {
                name: "Order".into(),
                body: Body::new(),
                occupied: true,
                data: Vec::new(),
            },
        )
        .unwrap();
        let y = e.yaml();
        assert_eq!(y.matches("cmd: Remove").count(), 2, "{y}");
        assert!(y.contains("- cmd: Clear\n  from: 5\n  till: 7\n"), "{y}");
        assert!(
            y.find("cmd: Insert").unwrap() < y.find("seq: 4").unwrap(),
            "{y}"
        );
    }

    /// `agr:stream-ops` — поле удалённого сообщения не правится, его правки
    /// снимаются удалением.
    #[test]
    fn field_edit_on_removed_message_is_refused() {
        let mut e = Edits::default();
        e.set(5, "Order", "price", "100", "1");
        e.delete(5, "Order", true, None);
        assert!(e.get(5, "price").is_none());
        e.set(5, "Order", "price", "100", "2");
        assert!(e.get(5, "price").is_none());
        assert!(e.restore(5));
        assert!(e.is_empty());
    }

    /// `agr:stream-ops` `agr:view-input-seq` — смена типа и правки её полей —
    /// одна команда `Update`; вид её видит, удаления и вставки — нет.
    #[test]
    fn retype_absorbs_field_edits_into_one_update() {
        let mut e = Edits::default();
        let mut body = Body::new();
        body.insert("header.id".into(), "7".into());
        e.retype(5, "Order", "Cancel", body).unwrap();
        e.set(5, "Cancel", "reason", "0", "3");
        removed(&mut e, &[8], true);
        let y = e.yaml();
        assert_eq!(y.matches("- cmd:").count(), 2, "{y}");
        assert!(
            y.starts_with("- cmd: Update\n  seq: 5\n  name: 'Cancel'\n  fields:\n    header:\n      id: '7'\n    reason: '3'\n"),
            "{y}"
        );
        let v = e.view_yaml();
        assert!(v.contains("cmd: Update") && !v.contains("Remove"), "{v}");
    }

    /// `agr:stream-ops` — подряд идущие смены на один тип одним телом — одна
    /// команда `Update` с `from`/`till`; правка поля, другой тип или пропуск в
    /// файле рвут диапазон, уже нужного типа — не трогаются.
    #[test]
    fn consecutive_retypes_make_one_range_update() {
        let mut e = Edits::default();
        let msgs: Vec<(i64, String, Option<i64>)> = (10..15)
            .map(|s| {
                (
                    s,
                    if s == 12 { "Ping" } else { "Order" }.to_string(),
                    Some(s + 1),
                )
            })
            .collect();
        assert_eq!(e.retype_many(&msgs, "Ping", &Body::new()), 4);
        assert_eq!(
            e.yaml(),
            "- cmd: Update\n  from: 10\n  till: 12\n  name: 'Ping'\n  fields: {}\n\
             - cmd: Update\n  from: 13\n  till: 15\n  name: 'Ping'\n  fields: {}\n"
        );
        // Правка поля сменённого — своё тело, отдельная команда.
        e.set(14, "Ping", "header.ts", "0", "1");
        let y = e.yaml();
        assert!(
            y.contains("  seq: 13\n  name: 'Ping'\n  fields: {}\n"),
            "{y}"
        );
        assert!(
            y.contains("  seq: 14\n  name: 'Ping'\n  fields:\n    header:\n"),
            "{y}"
        );
        // Разреженный файл: соседи по `next`.
        let mut e = Edits::default();
        let msgs = vec![
            (5, "Order".to_string(), Some(15)),
            (15, "Order".to_string(), Some(25)),
        ];
        e.retype_many(&msgs, "Ping", &Body::new());
        assert!(
            e.yaml()
                .starts_with("- cmd: Update\n  from: 5\n  till: 16\n"),
            "{}",
            e.yaml()
        );
        assert!(e.view_yaml().contains("from: 5"), "вид видит смену типа");
    }

    /// `agr:stream-ops` — вернуть исходный тип — снять смену.
    #[test]
    fn retype_back_to_file_type_drops_the_edit() {
        let mut e = Edits::default();
        e.retype(5, "Order", "Cancel", Body::new()).unwrap();
        e.retype(5, "Cancel", "Order", Body::new()).unwrap();
        assert!(e.is_empty());
    }

    /// `agr:stream-ops` — в дыру одна вставка: вторая легла бы на тот же seq.
    #[test]
    fn second_insert_into_one_gap_is_refused() {
        let mut e = Edits::default();
        let gap = Insert {
            name: "Order".into(),
            body: Body::new(),
            occupied: false,
            data: Vec::new(),
        };
        assert_eq!(e.insert(15, gap.clone()), Ok(0));
        assert!(e.insert(15, gap).is_err());
        assert_eq!(e.inserts(15).len(), 1);
    }

    /// `agr:edits-in-session` — состав потока переживает перезапуск, старая
    /// сессия без него читается.
    #[test]
    fn saved_ops_come_back_the_same() {
        let mut e = Edits::default();
        e.delete(3, "Order", true, Some(4));
        e.delete(9, "Order", false, None);
        let mut body = Body::new();
        body.insert("header.id".into(), "7".into());
        e.retype(5, "Order", "Cancel", body.clone()).unwrap();
        e.set(5, "Cancel", "reason", "0", "3");
        e.insert(
            7,
            Insert {
                name: "Order".into(),
                body,
                occupied: true,
                data: vec![1, 2, 0xab],
            },
        )
        .unwrap();
        e.set(12, "Order", "price", "1", "2");
        assert_eq!(Edits::from_session(&e.saved(), &e.saved_ops()), e);
        let old: crate::session::Session = toml::from_str(
            "file = 'x'\n[[edits]]\nseq = 1\nname = 'O'\npath = 'p'\nold = '1'\nnew = '2'\n",
        )
        .unwrap();
        assert!(old.stream.is_empty());
        assert_eq!(Edits::from_session(&old.edits, &old.stream).fields(), 1);
    }

    #[test]
    fn store_writes_a_new_file_per_version() {
        let mut s = Store::new();
        let mut e = Edits::default();
        assert_eq!(s.write(&e).unwrap(), None);
        e.set(5, "Order", "price", "100", "1");
        let a = s.write(&e).unwrap().unwrap();
        let b = s.write(&e).unwrap().unwrap();
        assert_ne!(a, b);
        assert_eq!(std::fs::read_to_string(&b).unwrap(), e.yaml());
        let dir = a.parent().unwrap().to_path_buf();
        drop(s);
        assert!(!dir.exists());
    }
}
