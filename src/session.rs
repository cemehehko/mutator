//! Сессия просмотра: всё, что пользователь настроил для файла, переживает
//! перезапуск (`agr:session-by-name`).
//!
//! Ключ — имя файла без каталога: `out-mu0.0.dat` со стенда и его копия на диске
//! открываются с одной сессией. Лежат в `~/.config/mutator/sessions/<имя>.toml`
//! (или в `$XDG_CONFIG_HOME`). Снимок делает [`crate::desk::Desk::session`],
//! возвращает [`crate::desk::Desk::restore`]; окно добавляет своё — вид, размер
//! и положение. Незафиксированные правки полей тоже здесь
//! (`agr:edits-in-session`): файл не менялся, и при следующем открытии они
//! накладываются снова.
//!
//! Наружу: [`Session`], [`config_dir`], [`path_for`], [`load`], [`save`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Сохраняемое состояние.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    /// Полный путь, с которым файл открывали последним, — для справки.
    pub file: String,
    /// Фильтр: типы и Lua-выражение.
    pub messages: Vec<String>,
    pub expr: String,
    /// Последний поиск и его сторона — для `n`/`N`.
    pub search: Option<String>,
    pub search_back: bool,
    pub history_filter: Vec<String>,
    pub history_search: Vec<String>,
    /// Колонки полей; `None` — как в первом пресете.
    pub columns: Option<Vec<String>>,
    pub preset: usize,
    pub hscroll: usize,
    /// Выбранное сообщение.
    pub seq: Option<i64>,
    pub detail_open: bool,
    /// Поле под курсором детали (путь).
    pub detail_path: Option<String>,
    pub focus_detail: bool,
    /// Hex и типы полей в детали.
    pub show_hex: bool,
    pub show_types: bool,
    /// Окно: вид («gui»/«tui»), прямоугольник (x, y, ширина, высота).
    pub look: Option<String>,
    pub window: Option<[f32; 4]>,
    /// Доля высоты под деталь (0..1, по умолчанию 0.3); одна для TUI и GUI.
    pub detail_ratio: Option<f32>,
    /// Код Lua для transform-режима (F4).
    pub lua_transform: Option<String>,
    /// Включён ли transform-код; `Some(false)` — код есть, но выключен тумблером.
    pub lua_transform_enabled: Option<bool>,
    /// Незафиксированные правки (`agr:edits-in-session`).
    pub edits: Vec<crate::edit::Saved>,
}

/// Каталог настроек: `$XDG_CONFIG_HOME/mutator` или `~/.config/mutator`.
/// В тестах [`crate::agent::Store`] подменяет его через `MUTATOR_CONFIG_DIR`,
/// чтобы не затирать настоящие настройки пользователя.
pub fn config_dir() -> Option<PathBuf> {
    if let Some(t) = std::env::var_os("MUTATOR_CONFIG_DIR") {
        if !t.is_empty() {
            return Some(PathBuf::from(t));
        }
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    let new = base.join("mutator");
    // Ребрендинг: старый каталог tui-patch переезжает целиком (сессии, агент,
    // пресеты), если новый ещё не завёлся.
    let old = base.join("tui-patch");
    if !new.exists() && old.exists() {
        let _ = std::fs::rename(&old, &new);
    }
    Some(new)
}

/// Файл сессии для данных `data`: по имени, знаки вне `[A-Za-z0-9._-]` — `_`.
pub fn path_for(data: &Path) -> Option<PathBuf> {
    let name = data.file_name()?.to_string_lossy();
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    Some(config_dir()?.join("sessions").join(format!("{safe}.toml")))
}

/// Сессия файла; нет или не читается — `None`.
pub fn load(data: &Path) -> Option<Session> {
    let text = std::fs::read_to_string(path_for(data)?).ok()?;
    toml::from_str(&text).ok()
}

/// Записать сессию файла (через временный файл — обрыв не портит прежнюю).
pub fn save(data: &Path, s: &Session) -> Result<(), String> {
    let path = path_for(data).ok_or("нет каталога настроек")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = toml::to_string(s).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}
