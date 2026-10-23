//! Колонки таблицы: seq и name всегда, дальше — пути полей.
//!
//! Пресет — набор путей. Встроенный «orders» (`header.ts`, `header.status`,
//! `header.order_id`, `symbol`, `side`, `price`, `amt`) берёт только пути,
//! которые есть хоть в одном сообщении схемы. Свои — в
//! `~/.config/mutator/presets.toml`, с привязкой к `schemaClassName` схемы:
//!
//! ```toml
//! [[preset]]
//! name = "money"
//! scheme = "AlgoScheme"      # необязательно
//! columns = ["header.ts", "person_id", "account_code", "assets"]
//! ```
//!
//! Наружу: [`Preset`], [`presets`], [`all_paths`].
//!
//! Ещё: [`config_path`], [`load`].

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::scheme::Scheme;

/// Набор колонок.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Preset {
    pub name: String,
    /// `schemaClassName`, для которого пресет; нет — для любой схемы.
    #[serde(default)]
    pub scheme: Option<String>,
    pub columns: Vec<String>,
}

#[derive(Deserialize)]
struct PresetFile {
    #[serde(default)]
    preset: Vec<Preset>,
}

const ORDERS: &[&str] = &[
    "header.ts",
    "header.status",
    "header.order_id",
    "symbol",
    "side",
    "price",
    "amt",
];

/// Путь к файлу пресетов пользователя.
pub fn config_path() -> Option<PathBuf> {
    Some(crate::session::config_dir()?.join("presets.toml"))
}

/// Пресеты для схемы: свои (подходящие по `schemaClassName`) раньше встроенных;
/// пути, которых в схеме нет, отброшены, пустые пресеты — тоже. Последний — «—»
/// без полей.
pub fn presets(scheme: &Scheme, user: Option<&str>) -> Vec<Preset> {
    let known = all_paths(scheme);
    let has = |p: &String| known.iter().any(|(k, _)| k == p);
    let class = scheme.option("schemaClassName");
    let mut out = Vec::new();
    if let Some(text) = user {
        if let Ok(f) = toml::from_str::<PresetFile>(text) {
            for p in f.preset {
                if p.scheme.as_deref().is_some_and(|s| Some(s) != class) {
                    continue;
                }
                out.push(Preset {
                    columns: p.columns.into_iter().filter(has).collect(),
                    ..p
                });
            }
        }
    }
    out.push(Preset {
        name: "orders".into(),
        scheme: None,
        columns: ORDERS.iter().map(|s| s.to_string()).filter(has).collect(),
    });
    out.retain(|p| !p.columns.is_empty());
    out.push(Preset {
        name: "—".into(),
        scheme: None,
        columns: Vec::new(),
    });
    out
}

/// Пресеты с файлом пользователя.
pub fn load(scheme: &Scheme) -> Vec<Preset> {
    let text = config_path().and_then(|p| std::fs::read_to_string(p).ok());
    presets(scheme, text.as_deref())
}

/// Все скалярные пути схемы и в скольких типах сообщений каждый есть — по
/// убыванию частоты: общие поля заголовка наверху списка выбора.
pub fn all_paths(scheme: &Scheme) -> Vec<(String, usize)> {
    let mut count: BTreeMap<String, usize> = BTreeMap::new();
    for m in scheme.messages() {
        for p in scheme.scalar_paths(m) {
            *count.entry(p).or_default() += 1;
        }
    }
    let mut v: Vec<_> = count.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheme::{Field, Int, Kind, Struct, Sub};

    fn scheme() -> Scheme {
        let f = |n: &str| Field {
            name: n.into(),
            offset: 0,
            size: 8,
            kind: Kind::Int(Int::I64, Sub::None),
            options: vec![],
            index: -1,
        };
        let head = Struct {
            name: "H".into(),
            msgid: 0,
            size: 8,
            fields: vec![f("status"), f("order_id")],
            options: vec![],
            pmap: None,
        };
        let m = |n: &str, id, extra: &str| Struct {
            name: n.into(),
            msgid: id,
            size: 16,
            fields: vec![
                Field {
                    name: "header".into(),
                    offset: 0,
                    size: 8,
                    kind: Kind::Message(0),
                    options: vec![],
                    index: -1,
                },
                f(extra),
            ],
            options: vec![],
            pmap: None,
        };
        let mut s = Scheme {
            structs: vec![head, m("A", 1, "price"), m("B", 2, "qty")],
            options: vec![("schemaClassName".into(), "X".into())],
            ..Default::default()
        };
        s.by_id.insert(1, 1);
        s.by_id.insert(2, 2);
        s
    }

    /// Встроенный пресет берёт только существующие поля; свои — по имени схемы и
    /// раньше встроенных; общие поля в выборе — наверху.
    #[test]
    fn presets_keep_known_paths_and_match_scheme_class() {
        let s = scheme();
        let user = r#"
[[preset]]
name = "mine"
scheme = "X"
columns = ["qty", "nope"]
[[preset]]
name = "other"
scheme = "Y"
columns = ["qty"]
"#;
        let p = presets(&s, Some(user));
        let names: Vec<_> = p.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["mine", "orders", "—"]);
        assert_eq!(p[0].columns, ["qty"]);
        assert_eq!(p[1].columns, ["header.status", "header.order_id", "price"]);
        let all = all_paths(&s);
        assert_eq!(all[0], ("header.order_id".into(), 2));
        assert_eq!(all[1], ("header.status".into(), 2));
    }
}
