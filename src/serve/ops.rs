//! Операции сервера — читающие инструменты агента, без вида и без транспорта:
//! список со схемами ([`list`]) и вызов ([`call`]) поверх [`crate::agent`]
//! (`agr:serve-same-tools`). Своих операций здесь нет — только отбор и
//! аргумент `file`, которого у агента нет: агент читает открытый в виде файл,
//! а серверу его называют в каждом вызове — путь или url линка.
//!
//! Отбор — белый список [`OPS`]: правки, transform и движение вида наружу не
//! выходят, по имени — [`Error::Forbidden`] (`agr:serve-no-write`). Долгие сканы
//! помечены в [`LONG`]: транспорт зовёт их запуском (`agr:serve-events`).
//!
//! Линк держится открытым в [`Links`] по url: первый вызов поднимает спул,
//! следующие читают его как файл, свежий на момент вызова
//! (`agr:serve-link-handle`); не ставший Active за [`OPEN_WAIT`] — отказ с
//! его состоянием. Файл или линк из командной строки — файл по
//! умолчанию: вызов без `file` читает его, линк открыт с его `--open` заранее.
//!
//! Всё здесь блокирует (диск, libtll): транспорт зовёт из `spawn_blocking`.
//!
//! Наружу: [`OPS`], [`LONG`], [`list`], [`call`], [`call_each`], [`is_long`], [`Error`],
//! [`Links`], [`LinkInfo`], [`OPEN_WAIT`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};

use crate::agent::{build_tools, execute, search_each, ToolCtx};
use crate::data::File;
use crate::io::link::{self, Link};

/// Инструменты агента, которые видны снаружи: только чтение файла.
pub const OPS: &[&str] = &[
    "file_overview",
    "message_types",
    "schema_fields",
    "column_presets",
    "read_messages",
    "message_detail",
    "make_filter",
    "search_messages",
    "count_messages",
    "post_message",
];

/// Сколько ждать, пока линк станет Active.
pub const OPEN_WAIT: Duration = Duration::from_secs(20);

/// Долгие — сканят файл целиком или до найденного.
pub const LONG: &[&str] = &["search_messages", "count_messages"];

/// Долгая ли операция.
pub fn is_long(op: &str) -> bool {
    LONG.contains(&op)
}

/// Отказ операции; транспорт переводит его в свой ответ.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Такой операции нет вовсе.
    Unknown(String),
    /// Операция агента есть, но меняет файл или вид — снаружи её нет.
    Forbidden(String),
    /// Операция отказала: нет файла, плохие аргументы, ошибка чтения.
    Failed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unknown(op) => write!(f, "операции {op} нет"),
            Error::Forbidden(op) => write!(f, "{op}: сервер только читает"),
            Error::Failed(e) => f.write_str(e),
        }
    }
}

/// Список операций: имя, описание, долгая ли и JSON-схема аргументов — схема
/// инструмента агента плюс `file`, обязательный, если файла по умолчанию нет.
pub fn list(links: &Links) -> Vec<Value> {
    build_tools()
        .into_iter()
        .filter_map(|t| {
            let f = &t["function"];
            let name = f["name"].as_str()?;
            if !OPS.contains(&name) {
                return None;
            }
            let mut params = f["parameters"].clone();
            params["properties"]["file"] = json!({
                "type": "string",
                "description": "Data file path or link url (resolve://…, stream+…, lua+…)"
            });
            if links.default.is_none() {
                let mut required = params["required"].as_array().cloned().unwrap_or_default();
                required.insert(0, json!("file"));
                params["required"] = Value::Array(required);
            }
            Some(json!({
                "name": name,
                "description": f["description"],
                "long": is_long(name),
                "parameters": params,
            }))
        })
        .collect()
}

/// Вызвать операцию `op` с аргументами `args` (с `file`): ответ инструмента
/// агента — текст, тот же, что видит модель.
pub fn call(links: &Links, op: &str, args: &Value) -> Result<String, Error> {
    call_each(links, op, args, &mut |_| {})
}

/// То же, но найденное поиском `each` видит по мере скана — сервер шлёт его
/// событием `op_item`, не дожидаясь конца файла (`agr:serve-events`).
pub fn call_each(
    links: &Links,
    op: &str,
    args: &Value,
    each: &mut dyn FnMut(&str),
) -> Result<String, Error> {
    if !OPS.contains(&op) {
        let known = build_tools().iter().any(|t| t["function"]["name"] == op);
        return Err(if known {
            Error::Forbidden(op.into())
        } else {
            Error::Unknown(op.into())
        });
    }
    let name = args
        .get("file")
        .and_then(|f| f.as_str())
        .or(links.default.as_deref())
        .ok_or_else(|| Error::Failed("нужен file: путь или url линка".into()))?;
    let file = links.file(name).map_err(Error::Failed)?;
    let mut ctx = ToolCtx::detached(file);
    if op == "search_messages" {
        return search_each(&mut ctx, args, each).map_err(Error::Failed);
    }
    execute(&mut ctx, op, &args.to_string()).map_err(Error::Failed)
}

/// Состояние открытого линка — для списка и событий.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LinkInfo {
    pub url: String,
    pub open: String,
    pub state: &'static str,
    pub online: bool,
    pub count: u64,
    pub last: Option<i64>,
    pub error: Option<String>,
    pub line: String,
}

/// Открытые линки по url. Спул живёт, пока линк держит кто-то: карта или
/// вид, читающий его.
#[derive(Default)]
pub struct Links {
    open: Mutex<BTreeMap<String, Arc<Link>>>,
    /// Замок открытия на url: одновременные первые обращения — по очереди.
    gates: Mutex<BTreeMap<String, Arc<Mutex<()>>>>,
    /// Файл или линк вызова без `file`.
    default: Option<String>,
    /// Каталог, где спулы остаются (`--keep`).
    keep: Option<PathBuf>,
}

impl Links {
    /// Линки с файлом по умолчанию и каталогом спулов; `default` — линк, —
    /// откройте его [`Links::open`] до первого вызова, чтобы задать параметры.
    pub fn new(default: Option<String>, keep: Option<PathBuf>) -> Links {
        Links {
            default,
            keep,
            ..Links::default()
        }
    }

    /// Файл для чтения: путь (`file://` снимается) или url линка — линк
    /// открывается при первом обращении с параметрами по умолчанию.
    pub fn file(&self, name: &str) -> Result<File, String> {
        if !link::is_link(name) {
            return File::open(Path::new(name.strip_prefix("file://").unwrap_or(name)));
        }
        // Первое обращение к url открывает линк, одновременные ждут его же:
        // открытие одного url — по очереди, второй канал не поднимается.
        let gate = self.gate(name);
        let _one = gate.lock().unwrap();
        let held = self.open.lock().unwrap().get(name).cloned();
        let l = match held {
            Some(l) => l,
            None => self.open_within(name, None, OPEN_WAIT)?,
        };
        // Спул растёт: границы seq — на момент вызова, линк — тот же.
        let mut f = File::open(l.spool())?;
        f.name = name.to_string();
        f.link = Some(l);
        Ok(f)
    }

    /// Файл или линк по умолчанию — из командной строки.
    pub fn default_file(&self) -> Option<&str> {
        self.default.as_deref()
    }

    /// Каталог, где спулы остаются (`--keep`).
    pub fn spool_dir(&self) -> Option<&Path> {
        self.keep.as_deref()
    }

    /// Положить уже поднятый линк в карту и открыть его спул как файл.
    /// Прежний с тем же url уходит из карты (`agr:serve-link-handle`).
    pub fn adopt(&self, link: Link) -> Result<File, String> {
        let url = link.url().to_string();
        let gate = self.gate(&url);
        let _one = gate.lock().unwrap();
        let l = Arc::new(link);
        let mut f = File::open(l.spool())?;
        f.name = url.clone();
        f.link = Some(l.clone());
        self.open.lock().unwrap().insert(url, l);
        Ok(f)
    }

    /// Замок открытия этого url.
    fn gate(&self, url: &str) -> Arc<Mutex<()>> {
        self.gates
            .lock()
            .unwrap()
            .entry(url.to_string())
            .or_default()
            .clone()
    }

    /// Открыть линк (`open` — параметры, `None` — как у бэкенда по умолчанию)
    /// и дождаться спула не дольше [`OPEN_WAIT`]. Уже открытый с тем же url
    /// уходит из карты; его канал и спул живут, пока его держат виды.
    pub fn open(&self, url: &str, open: Option<&str>) -> Result<Arc<Link>, String> {
        let gate = self.gate(url);
        let _one = gate.lock().unwrap();
        self.open_within(url, open, OPEN_WAIT)
    }

    /// [`Links::open`] со своим пределом ожидания: линк, не ставший Active
    /// за `wait`, — отказ с его состоянием, спул убирается
    /// (`agr:serve-link-handle`). Неверное имя сервиса у `resolve://` иначе
    /// висит в Opening без конца.
    pub fn open_within(
        &self,
        url: &str,
        open: Option<&str>,
        wait: Duration,
    ) -> Result<Arc<Link>, String> {
        let open = open
            .map(str::to_string)
            .unwrap_or_else(|| link::default_open(url));
        let l = Link::start(url, &open, None, self.keep.as_deref(), Arc::new(|| {}))?;
        let deadline = Instant::now() + wait;
        let mut seen = None;
        loop {
            let st = l.status();
            if seen.as_ref() != Some(&(st.state, st.error.clone())) {
                eprintln!("mutator: {}", st.line());
                seen = Some((st.state, st.error.clone()));
            }
            if st.spool.is_some() {
                break;
            }
            if matches!(st.state, link::LinkState::Error | link::LinkState::Closed) {
                return Err(st.line());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "линк не открылся за {} с — проверьте имя сервиса и --resolve: {}",
                    wait.as_secs(),
                    st.line()
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let l = Arc::new(l);
        self.open.lock().unwrap().insert(url.into(), l.clone());
        Ok(l)
    }

    /// Закрыть линк: спул удаляется с последним читателем. `false` — не был открыт.
    pub fn close(&self, url: &str) -> bool {
        self.open.lock().unwrap().remove(url).is_some()
    }

    /// Состояния открытых линков.
    pub fn list(&self) -> Vec<LinkInfo> {
        let open = self.open.lock().unwrap();
        open.values()
            .map(|l| {
                let s = l.status();
                LinkInfo {
                    line: s.line(),
                    url: s.url,
                    open: s.open,
                    state: s.state.name(),
                    online: s.online,
                    count: s.count,
                    last: s.last,
                    error: s.error,
                }
            })
            .collect()
    }
}

#[cfg(all(test, feature = "tll"))]
mod tests {
    use super::*;

    fn plain() -> String {
        format!("{}/tests/data/plain.dat", env!("CARGO_MANIFEST_DIR"))
    }

    /// Ответ сервера — ответ инструмента агента на том же файле.
    /// `agr:serve-same-tools`
    #[test]
    fn a_call_answers_exactly_what_the_agent_tool_does() {
        crate::io::quiet();
        let args = json!({"file": plain(), "from": 10, "count": 3});
        let got = call(&Links::default(), "read_messages", &args).unwrap();
        let mut ctx = ToolCtx::detached(File::open(Path::new(&plain())).unwrap());
        let want = execute(&mut ctx, "read_messages", &args.to_string()).unwrap();
        assert_eq!(got, want);
    }

    /// Каждая операция списка — инструмент агента, и у каждой обязателен `file`.
    /// `agr:serve-same-tools`
    #[test]
    fn every_listed_op_is_an_agent_tool_with_a_file_argument() {
        let ops = list(&Links::default());
        assert_eq!(ops.len(), OPS.len());
        for op in &ops {
            assert!(OPS.contains(&op["name"].as_str().unwrap()));
            assert_eq!(op["parameters"]["required"][0], "file");
            assert_eq!(op["long"], is_long(op["name"].as_str().unwrap()));
        }
    }

    /// Правки и движение вида снаружи не видны, по имени — отказ.
    /// `agr:serve-no-write`
    #[test]
    fn writing_and_view_tools_are_refused_by_name() {
        let names: Vec<_> = list(&Links::default())
            .iter()
            .map(|o| o["name"].clone())
            .collect();
        for op in [
            "edit_messages",
            "set_lua_transform",
            "apply_filter",
            "goto",
            "set_columns",
        ] {
            assert!(!names.contains(&json!(op)), "{op} в списке");
            let e = call(&Links::default(), op, &json!({"file": plain()})).unwrap_err();
            assert_eq!(e, Error::Forbidden(op.into()));
        }
        let e = call(&Links::default(), "no_such", &json!({})).unwrap_err();
        assert_eq!(e, Error::Unknown("no_such".into()));
    }

    /// Файл из командной строки — по умолчанию: вызов без `file` читает его,
    /// и `file` в схеме уже не обязателен.
    /// `agr:serve-link-handle`
    #[test]
    fn a_default_file_answers_calls_without_file() {
        crate::io::quiet();
        let links = Links::new(Some(plain()), None);
        let bare = call(&links, "message_types", &json!({})).unwrap();
        let named = call(
            &Links::default(),
            "message_types",
            &json!({"file": plain()}),
        )
        .unwrap();
        assert_eq!(bare, named);
        assert!(list(&links)
            .iter()
            .all(|o| o["parameters"]["required"][0] != "file"));
        let e = call(&Links::default(), "message_types", &json!({})).unwrap_err();
        assert!(matches!(e, Error::Failed(_)));
    }
}
