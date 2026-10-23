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
//! Файлы: `commit.rs` — фиксация: прогон файла через tll-patch в новый файл,
//! свободные имена, бэкап.
//!
//! Наружу: [`Edits`], [`MsgEdit`], [`FieldEdit`], [`Summary`], [`Saved`],
//! [`Store`], [`commit`].

pub mod commit;

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

/// Все правки файла по seq.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Edits {
    msgs: BTreeMap<i64, MsgEdit>,
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

/// Сводка для диалога фиксации: строка на поле и итоги.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub lines: Vec<String>,
    /// (что, сколько): сообщений, полей, по типам, диапазон seq.
    pub totals: Vec<(String, String)>,
}

impl Edits {
    pub fn is_empty(&self) -> bool {
        self.msgs.is_empty()
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

    /// yaml команд tll-patch: по `Field` на сообщение, значения — строками в
    /// одинарных кавычках (tll-config сам приводит их к типу поля).
    pub fn yaml(&self) -> String {
        let mut out = String::new();
        for (seq, m) in &self.msgs {
            out.push_str(&format!(
                "- cmd: Field\n  seq: {seq}\n  name: {}\n  fields:\n",
                quote(&m.name)
            ));
            for (path, e) in &m.fields {
                out.push_str(&format!("    {path}: {}\n", quote(&e.new)));
            }
        }
        out
    }

    /// Сводка: «seq тип путь: было → стало» и итоги.
    pub fn summary(&self) -> Summary {
        let mut lines = Vec::new();
        let mut by_type: BTreeMap<&str, usize> = BTreeMap::new();
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

    /// Из сессии.
    pub fn from_saved(s: &[Saved]) -> Edits {
        let mut e = Edits::default();
        for x in s {
            e.set(x.seq, &x.name, &x.path, &x.old, &x.new);
        }
        e
    }
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

    /// Записать новую версию; `None` — правок нет, читать файл как есть.
    pub fn write(&mut self, e: &Edits) -> Result<Option<PathBuf>, String> {
        if e.is_empty() {
            return Ok(None);
        }
        std::fs::create_dir_all(&self.dir).map_err(|x| format!("{}: {x}", self.dir.display()))?;
        self.n += 1;
        let p = self.dir.join(format!("patch-{}.yaml", self.n));
        std::fs::write(&p, e.yaml()).map_err(|x| format!("{}: {x}", p.display()))?;
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
