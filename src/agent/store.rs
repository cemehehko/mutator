//!
//! Данные режима агента: настройки подключения к модели и диалоги. Лежат рядом
//! с сессиями просмотрщика, в `~/.config/tui-patch/agent`: [`Settings`] —
//! `settings.json`, диалоги — дневник `dialogs-<владелец>.jsonl`. Владелец —
//! имя файла датника ([`owner_of`]), как сессия привязана к имени, а не к пути
//! (`agr:session-by-name`): копия датника со стенда открывается с теми же
//! диалогами.
//!
//! Диалог ([`Dialog`]) — заголовок и лента [`Msg`].
//! Контекст модели у каждого диалога свой: переключили диалог — сменился и он.
//!
//! Дневник — строка меты, дальше по строке на сообщение (`agr:agent-jsonl`):
//! переписка дописывается в конец, обрыв портит только недописанную строку.
//! Цельная перезапись (временный файл и переименование) — когда поменялись
//! метаданные. Прежний цельный `.json` ещё читается. Ключ можно задать и
//! переменной `TUI_PATCH_API_KEY` — она сильнее файла.
//!
//! Наружу: [`Settings`], [`Dialog`], [`Msg`], [`CallReq`],
//! [`Store`], [`UNTITLED`], [`owner_of`], [`load_settings`],
//! [`save_settings`], [`load_store`], [`save_store`], [`append_message`],
//! [`new_dialog`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Имя свежего диалога, пока его не переименовали по первому вопросу.
pub const UNTITLED: &str = "Новый диалог";

/// Настройки подключения: адрес OpenAI-совместимого API, ключ, модель, прокси.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// Прокси: `host:port` или `http://host:port`; пусто — без прокси.
    pub proxy: String,
    pub proxy_login: String,
    pub proxy_password: String,
    /// Отладка: ход агента пишется в журнал и виден в ленте.
    pub debug: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            // Подключение по умолчанию (provod); ключ — в настройках агента или в TUI_PATCH_API_KEY.
            // Модель — недорогая, со стримингом и вызовом инструментов.
            base_url: "https://api.provod.ai/v1".into(),
            api_key: String::new(),
            model: "gemini-2.5-flash-lite".into(),
            proxy: String::new(),
            proxy_login: String::new(),
            proxy_password: String::new(),
            debug: false,
        }
    }
}

/// Вызов инструмента, как его попросила модель: лежит в [`Msg::Assistant`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CallReq {
    pub id: String,
    pub name: String,
    /// Аргументы — сырая строка JSON из потока: не парсить и не пересобирать.
    pub arguments: String,
}

/// Сообщение диалога — та же форма, что уходит модели (`agr:agent-tool-pairing`).
/// Старый плоский формат `{"role","content"}` читается один в один: все новые
/// поля — с `#[serde(default)]`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Msg {
    /// Служебный текст; в диалоге не хранится, промпт строится заново на ход.
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        #[serde(default)]
        content: String,
        #[serde(default)]
        tool_calls: Vec<CallReq>,
    },
    Tool {
        call_id: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        content: String,
        /// В лог и UI; на провод не идёт — в OpenAI-протоколе такого поля нет,
        /// отказ и так виден модели текстом (`ERROR: …`).
        #[serde(default, skip_serializing)]
        is_error: bool,
    },
}

impl Msg {
    /// Роль строки — для ленты (`user`/`assistant`/`tool`/`system`).
    pub fn role(&self) -> &'static str {
        match self {
            Msg::System { .. } => "system",
            Msg::User { .. } => "user",
            Msg::Assistant { .. } => "assistant",
            Msg::Tool { .. } => "tool",
        }
    }

    /// Текст сообщения; у ассистента — только текст, вызовы отдельно.
    pub fn content(&self) -> &str {
        match self {
            Msg::System { content } | Msg::User { content } | Msg::Assistant { content, .. } => {
                content
            }
            Msg::Tool { content, .. } => content,
        }
    }
}

/// Один диалог: заголовок и лента сообщений.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Dialog {
    pub id: String,
    pub title: String,
    pub messages: Vec<Msg>,
    /// Диалог закрыт: скрыт из списка, но не стёрт.
    #[serde(default)]
    pub closed: bool,
}

/// Все диалоги владельца и активный из них.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Store {
    pub dialogs: Vec<Dialog>,
    pub active: Option<String>,
}

impl Store {
    /// Активный диалог.
    pub fn active(&self) -> Option<&Dialog> {
        self.active.as_deref().and_then(|id| self.by_id(id))
    }

    /// Активный диалог для правки.
    pub fn active_mut(&mut self) -> Option<&mut Dialog> {
        let id = self.active.clone()?;
        self.by_id_mut(&id)
    }

    /// Диалог по id.
    pub fn by_id(&self, id: &str) -> Option<&Dialog> {
        self.dialogs.iter().find(|d| d.id == id)
    }

    /// Диалог по id для правки.
    pub fn by_id_mut(&mut self, id: &str) -> Option<&mut Dialog> {
        self.dialogs.iter_mut().find(|d| d.id == id)
    }
}

/// Владелец диалогов — имя файла датника: знаки вне `[A-Za-z0-9._-]` — `_`.
pub fn owner_of(data: &Path) -> String {
    let name = data
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Каталог агента: `~/.config/tui-patch/agent`.
fn dir() -> Option<PathBuf> {
    Some(crate::session::config_dir()?.join("agent"))
}

fn settings_path() -> Option<PathBuf> {
    Some(dir()?.join("settings.json"))
}

fn dialogs_path(owner: &str) -> Option<PathBuf> {
    Some(dir()?.join(format!("dialogs-{owner}.jsonl")))
}

/// Прежний цельный формат; читается, пока не перезаписан дневником.
fn dialogs_path_old(owner: &str) -> Option<PathBuf> {
    Some(dir()?.join(format!("dialogs-{owner}.json")))
}

/// Настройки; нет файла или он не читается — умолчания. Ключ из окружения
/// (`TUI_PATCH_API_KEY`) сильнее файла: конфигом можно делиться, секретом нет.
pub fn load_settings() -> Settings {
    let mut s: Settings = settings_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    if let Ok(key) = std::env::var("TUI_PATCH_API_KEY") {
        if !key.is_empty() {
            s.api_key = key;
        }
    }
    s
}

/// Записать настройки; файл с ключом читают только хозяин (0600).
pub fn save_settings(s: &Settings) -> Result<(), String> {
    let path = settings_path().ok_or("нет каталога настроек")?;
    write_json(&path, s)?;
    restrict(&path);
    Ok(())
}

/// Отдать файл только хозяину; не Unix или ошибка chmod — не помеха.
fn restrict(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Диалоги владельца из дневника JSONL; нет файла — прежний цельный `.json`
/// или пусто. Оборванная при записи строка (обрыв, крэш) просто не читается
/// (`agr:agent-jsonl`).
pub fn load_store(owner: &str) -> Store {
    let Some(path) = dialogs_path(owner) else {
        return Store::default();
    };
    if let Ok(text) = std::fs::read_to_string(&path) {
        return parse_jsonl(&text);
    }
    if let Some(old) = dialogs_path_old(owner) {
        if let Ok(text) = std::fs::read_to_string(old) {
            return serde_json::from_str(&text).unwrap_or_default();
        }
    }
    Store::default()
}

/// Метаданные дневника: первая строка файла.
#[derive(Serialize, Deserialize)]
struct StoreMeta {
    active: Option<String>,
    dialogs: Vec<DialogMeta>,
}

#[derive(Serialize, Deserialize)]
struct DialogMeta {
    id: String,
    title: String,
    closed: bool,
}

/// Разобрать дневник: строка меты, дальше по строке на сообщение; строки,
/// не читаемые как JSON, пропускаются — это хвост недописанной записи.
fn parse_jsonl(text: &str) -> Store {
    let mut store = Store::default();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(meta) = v.get("meta") {
            if let Ok(m) = serde_json::from_value::<StoreMeta>(meta.clone()) {
                store.active = m.active;
                store.dialogs = m
                    .dialogs
                    .into_iter()
                    .map(|d| Dialog {
                        id: d.id,
                        title: d.title,
                        messages: Vec::new(),
                        closed: d.closed,
                    })
                    .collect();
            }
        } else if let (Some(d), Some(msg)) =
            (v.get("d").and_then(|x| x.as_str()), v.get("msg").cloned())
        {
            // Новый формат: сообщение целиком, в форме провода.
            if let Ok(m) = serde_json::from_value::<Msg>(msg) {
                if let Some(d) = store.dialogs.iter_mut().find(|x| x.id == d) {
                    d.messages.push(m);
                }
            }
        } else if let (Some(d), Some(role), Some(text)) = (
            v.get("d").and_then(|x| x.as_str()),
            v.get("role").and_then(|x| x.as_str()),
            v.get("text").and_then(|x| x.as_str()),
        ) {
            // Дневник первых дней: только роль и текст, без вызовов.
            if let Some(d) = store.dialogs.iter_mut().find(|x| x.id == d) {
                d.messages.push(match role {
                    "assistant" => Msg::Assistant {
                        content: text.into(),
                        tool_calls: Vec::new(),
                    },
                    _ => Msg::User {
                        content: text.into(),
                    },
                });
            }
        }
    }
    store
}

/// Записать диалоги целиком: метаданные (список, имена, режимы) поменялись.
/// Сообщения при одной лишь переписке так не пишут — их дописывает
/// [`append_message`].
pub fn save_store(owner: &str, store: &Store) -> Result<(), String> {
    let path = dialogs_path(owner).ok_or("нет каталога настроек")?;
    let meta = StoreMeta {
        active: store.active.clone(),
        dialogs: store
            .dialogs
            .iter()
            .map(|d| DialogMeta {
                id: d.id.clone(),
                title: d.title.clone(),
                closed: d.closed,
            })
            .collect(),
    };
    let mut text =
        serde_json::to_string(&serde_json::json!({ "meta": meta })).map_err(|e| e.to_string())?;
    text.push('\n');
    for d in &store.dialogs {
        for m in &d.messages {
            text.push_str(&message_line(&d.id, m));
            text.push('\n');
        }
    }
    write_text(&path, &text)
}

/// Строка дневника: сообщение с номером диалога.
fn message_line(dialog_id: &str, m: &Msg) -> String {
    serde_json::json!({ "d": dialog_id, "msg": m }).to_string()
}

/// Дописать сообщение в дневник: append без перезаписи, обрыв портит только
/// недописанную строку (`agr:agent-jsonl`).
pub fn append_message(owner: &str, store: &Store, dialog_id: &str, m: &Msg) -> Result<(), String> {
    let path = dialogs_path(owner).ok_or("нет каталога настроек")?;
    if !path.exists() {
        // Дневника ещё нет: сначала метаданные, потом строка сообщения.
        return save_store(owner, store).and_then(|()| {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .map_err(|e| e.to_string())?;
            writeln!(f, "{}", message_line(dialog_id, m)).map_err(|e| e.to_string())
        });
    }
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    writeln!(f, "{}", message_line(dialog_id, m)).map_err(|e| e.to_string())
}

/// Завести диалог и сделать его активным; возвращает его id.
pub fn new_dialog(store: &mut Store) -> String {
    let id = next_id();
    store.dialogs.push(Dialog {
        id: id.clone(),
        title: UNTITLED.into(),
        messages: Vec::new(),
        closed: false,
    });
    store.active = Some(id.clone());
    id
}

/// Запись через временный файл: обрыв не портит прежнюю версию.
fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    write_text(path, &text)
}

/// Цельный текст через временный файл и переименование: обрыв не портит
/// прежнее. Временное имя своё у каждой записи: две записи одного файла не
/// спорят за общий `.tmp` (воркер и интерфейс пишут из разных потоков).
fn write_text(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension(format!("json.tmp.{}", next_id()));
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// Id диалога: время в наносекундах и счётчик — уникален в пределах процесса.
fn next_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}-{:x}", N.fetch_add(1, Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Once;

    /// Владелец тестов: фиксированное имя, чтобы тесты не спорили за файлы.
    fn owner() -> String {
        "тест-дневник".into()
    }

    /// Тесты одного каталога настроек идут по очереди: файл и переменная
    /// окружения общие.
    fn disk() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Тесты пишут во временный каталог, а не в `~/.config/tui-patch/agent`,
    /// чтобы не затирать настоящие настройки пользователя.
    fn sandbox() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            let dir = std::env::temp_dir().join(format!("tui-patch-test-{}", std::process::id()));
            std::fs::remove_dir_all(&dir).ok();
            std::env::set_var("MUTATOR_CONFIG_DIR", &dir);
        });
    }

    /// Дневник с прошлого прогона не должен путать счётчики.
    fn fresh_journal() {
        let _ = std::fs::remove_file(dialogs_path(&owner()).unwrap());
        let _ = std::fs::remove_file(dialogs_path_old(&owner()).unwrap());
    }

    /// Владелец — имя файла, чужие знаки заменены.
    #[test]
    fn owner_is_file_name_with_safe_chars() {
        assert_eq!(
            owner_of(Path::new("/tmp/mu0/out-mu0.0.dat")),
            "out-mu0.0.dat"
        );
        assert_eq!(owner_of(Path::new("/tmp/файл 1.dat")), "_____1.dat");
    }

    /// Настройки переживают запись и чтение.
    #[test]
    fn settings_round_trip() {
        let s = Settings {
            api_key: "k".into(),
            model: "m".into(),
            proxy: "http://p:1".into(),
            ..Default::default()
        };
        let text = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&text).unwrap();
        assert_eq!(back.api_key, "k");
        assert_eq!(back.model, "m");
        assert_eq!(back.proxy, "http://p:1");
        assert!(!back.debug);
    }

    /// Недостающие поля настроек берутся из умолчаний, а не роняют чтение.
    #[test]
    fn settings_tolerate_missing_fields() {
        let s: Settings = serde_json::from_str(r#"{"model":"m"}"#).unwrap();
        assert_eq!(s.model, "m");
        assert_eq!(s.base_url, Settings::default().base_url);
    }

    /// Записанные настройки читает только хозяин: файл с ключом — 0600.
    #[cfg(unix)]
    #[test]
    fn saved_settings_are_owner_only() {
        sandbox();
        let _disk = disk();
        let s = Settings {
            api_key: "секрет".into(),
            ..Default::default()
        };
        save_settings(&s).unwrap();
        let path = settings_path().unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    /// Ключ из окружения сильнее файла: пустая переменная файл не затирает.
    #[test]
    fn env_key_overrides_file() {
        sandbox();
        let _disk = disk();
        let s = Settings {
            api_key: "из-файла".into(),
            ..Default::default()
        };
        save_settings(&s).unwrap();
        std::env::set_var("TUI_PATCH_API_KEY", "из-среды");
        assert_eq!(load_settings().api_key, "из-среды");
        std::env::set_var("TUI_PATCH_API_KEY", "");
        assert_eq!(load_settings().api_key, "из-файла");
        std::env::remove_var("TUI_PATCH_API_KEY");
    }

    /// Новый диалог становится активным и попадает в список.
    #[test]
    fn new_dialog_becomes_active() {
        let mut store = Store::default();
        let id = new_dialog(&mut store);
        assert_eq!(store.active.as_deref(), Some(id.as_str()));
        assert_eq!(store.active().map(|d| d.title.as_str()), Some(UNTITLED));
    }

    /// Диалоги переживают запись и чтение вместе с лентой.
    #[test]
    fn store_round_trip() {
        let mut store = Store::default();
        let id = new_dialog(&mut store);
        store.by_id_mut(&id).unwrap().messages.push(Msg::User {
            content: "привет".into(),
        });
        let text = serde_json::to_string(&store).unwrap();
        let back: Store = serde_json::from_str(&text).unwrap();
        assert_eq!(back.active.as_deref(), Some(id.as_str()));
        assert_eq!(back.active().unwrap().messages.len(), 1);
        assert_eq!(back.active().unwrap().messages[0].content(), "привет");
    }

    /// Дневник JSONL переживает запись и чтение: мета плюс сообщения
    /// (`agr:agent-jsonl`).
    #[test]
    fn jsonl_round_trip() {
        let _disk = disk();
        fresh_journal();
        let mut store = Store::default();
        let id = new_dialog(&mut store);
        store.by_id_mut(&id).unwrap().messages.push(Msg::User {
            content: "первый\nс переносом".into(),
        });
        store.by_id_mut(&id).unwrap().messages.push(Msg::Assistant {
            content: "ответ".into(),
            tool_calls: Vec::new(),
        });
        save_store(&owner(), &store).unwrap();
        let back = load_store(&owner());
        assert_eq!(back.active.as_deref(), Some(id.as_str()));
        assert_eq!(back.active().unwrap().messages.len(), 2);
        assert_eq!(
            back.active().unwrap().messages[0].content(),
            "первый\nс переносом"
        );
    }

    /// Оборванная при записи строка дневника просто не читается — остальное
    /// цело (`agr:agent-jsonl`).
    #[test]
    fn broken_last_line_is_dropped() {
        let _disk = disk();
        fresh_journal();
        let mut store = Store::default();
        let id = new_dialog(&mut store);
        store.by_id_mut(&id).unwrap().messages.push(Msg::User {
            content: "целое".into(),
        });
        save_store(&owner(), &store).unwrap();
        let path = dialogs_path(&owner()).unwrap();
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        // Недописанная строка: без перевода строки и без конца JSON.
        write!(f, "{{\"d\":\"x\",\"role\":\"assistant\",\"te").unwrap();
        let back = load_store(&owner());
        assert_eq!(back.active().unwrap().messages.len(), 1);
        assert_eq!(back.active().unwrap().messages[0].content(), "целое");
    }

    /// Прежний цельный формат читается, пока не перезаписан дневником.
    #[test]
    fn old_json_store_still_loads() {
        let _disk = disk();
        let mut store = Store::default();
        let id = new_dialog(&mut store);
        store.by_id_mut(&id).unwrap().messages.push(Msg::User {
            content: "старый формат".into(),
        });
        let old = dialogs_path_old(&owner()).unwrap();
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::write(&old, serde_json::to_string(&store).unwrap()).unwrap();
        std::fs::remove_file(dialogs_path(&owner()).unwrap()).ok();
        let back = load_store(&owner());
        assert_eq!(
            back.active().unwrap().messages[0].content(),
            "старый формат"
        );
    }

    /// Дописанное сообщение попадает в дневник без полной перезаписи.
    #[test]
    fn appended_message_lands_in_the_journal() {
        let _disk = disk();
        fresh_journal();
        let mut store = Store::default();
        let id = new_dialog(&mut store);
        save_store(&owner(), &store).unwrap();
        let m = Msg::Assistant {
            content: "дописано".into(),
            tool_calls: Vec::new(),
        };
        append_message(&owner(), &store, &id, &m).unwrap();
        let back = load_store(&owner());
        assert_eq!(back.active().unwrap().messages.len(), 1);
        assert_eq!(back.active().unwrap().messages[0].content(), "дописано");
    }

    /// Старый плоский формат сообщения читается тегированным enum: новые поля
    /// — с умолчаниями, первый же прежний файл не должен уронить загрузку.
    #[test]
    fn old_flat_message_still_loads() {
        let text = serde_json::json!({
            "dialogs": [{
                "id": "d1",
                "title": "старый",
                "mode": "Ask",
                "messages": [
                    { "role": "user", "content": "вопрос" },
                    { "role": "assistant", "content": "ответ" },
                ],
            }],
            "active": "d1",
        })
        .to_string();
        let store: Store = serde_json::from_str(&text).unwrap();
        let d = store.active().unwrap();
        assert_eq!(d.messages.len(), 2);
        assert_eq!(d.messages[0].role(), "user");
        match &d.messages[1] {
            Msg::Assistant {
                content,
                tool_calls,
            } => {
                assert_eq!(content, "ответ");
                assert!(tool_calls.is_empty());
            }
            other => panic!("ожидался assistant, пришло {other:?}"),
        }
    }

    /// Сообщение с вызовом инструмента переживает дневник: форма — как на
    /// проводе (`agr:agent-tool-pairing`).
    #[test]
    fn assistant_tool_calls_survive_the_journal() {
        let _disk = disk();
        fresh_journal();
        let mut store = Store::default();
        let id = new_dialog(&mut store);
        store.by_id_mut(&id).unwrap().messages.push(Msg::Assistant {
            content: "смотрю".into(),
            tool_calls: vec![CallReq {
                id: "c1".into(),
                name: "read_messages".into(),
                arguments: r#"{"count":5}"#.into(),
            }],
        });
        store.by_id_mut(&id).unwrap().messages.push(Msg::Tool {
            call_id: "c1".into(),
            name: "read_messages".into(),
            content: "- seq: 1".into(),
            is_error: false,
        });
        save_store(&owner(), &store).unwrap();
        let back = load_store(&owner());
        let d = back.active().unwrap();
        assert_eq!(d.messages.len(), 2);
        match &d.messages[0] {
            Msg::Assistant { tool_calls, .. } => assert_eq!(tool_calls[0].id, "c1"),
            other => panic!("ожидался assistant, пришло {other:?}"),
        }
        match &d.messages[1] {
            Msg::Tool {
                call_id,
                is_error,
                content,
                ..
            } => {
                assert_eq!(call_id, "c1");
                assert!(!*is_error);
                assert_eq!(content, "- seq: 1");
            }
            other => panic!("ожидался tool, пришло {other:?}"),
        }
    }

    /// Подключение по умолчанию — провод, недорогая модель; поля не пустые,
    /// чтобы агент работал сразу.
    #[test]
    fn default_connection_is_provod_with_cheap_model() {
        let d = Settings::default();
        assert_eq!(d.base_url, "https://api.provod.ai/v1");
        assert_eq!(d.model, "gemini-2.5-flash-lite");
        assert!(d.api_key.is_empty(), "ключа в коде нет");
    }
}
