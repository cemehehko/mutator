//! Последние открытые файлы: окно без аргумента открывает последний, меню
//! «Файл» показывает список (`agr:open-without-file`).
//!
//! Лежат в `recent.toml` рядом с сессиями ([`crate::session::config_dir`]).
//! Свежий — первым, без повторов, не больше [`LIMIT`]; путь абсолютный, чтобы
//! запуск из другого каталога находил тот же файл. Удалённый с диска файл из
//! списка не стирается, а просто пропускается в [`Recent::existing`] и
//! [`Recent::last_existing`]: съёмный диск вернётся — файл снова в меню.
//!
//! Наружу: [`Recent`], [`LIMIT`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Сколько файлов помним.
pub const LIMIT: usize = 10;

/// Список последних файлов, свежий первым.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Recent {
    pub files: Vec<PathBuf>,
}

/// Абсолютный путь файла; не нашёлся на диске — как есть.
fn absolute(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

impl Recent {
    /// Прочитать из каталога настроек; нет или не читается — пусто.
    pub fn load_from(dir: &Path) -> Recent {
        std::fs::read_to_string(dir.join("recent.toml"))
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Записать в каталог настроек (через временный файл).
    pub fn save_to(&self, dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let text = toml::to_string(self).map_err(|e| e.to_string())?;
        let path = dir.join("recent.toml");
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
    }

    /// Прочитать из общего каталога настроек.
    pub fn load() -> Recent {
        crate::session::config_dir()
            .map(|d| Recent::load_from(&d))
            .unwrap_or_default()
    }

    /// Записать в общий каталог настроек.
    pub fn save(&self) -> Result<(), String> {
        let dir = crate::session::config_dir().ok_or("нет каталога настроек")?;
        self.save_to(&dir)
    }

    /// Файл открыт: он первый, повтор убирается, хвост за [`LIMIT`] отрезается.
    pub fn push(&mut self, path: &Path) {
        let path = absolute(path);
        self.files.retain(|p| p != &path);
        self.files.insert(0, path);
        self.files.truncate(LIMIT);
    }

    /// Очистить список.
    pub fn clear(&mut self) {
        self.files.clear();
    }

    /// Файлы, которые ещё есть на диске, свежий первым. Спека склейки жива,
    /// пока разворачивается: `a.dat+b.dat`, `r.current.dat-3`.
    pub fn existing(&self) -> Vec<&PathBuf> {
        self.files
            .iter()
            .filter(|p| p.is_file() || crate::spec::expand(&p.to_string_lossy()).is_ok())
            .collect()
    }

    /// Последний открытый файл, если он ещё есть на диске.
    pub fn last_existing(&self) -> Option<PathBuf> {
        self.existing().first().map(|p| (*p).clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Каталог для теста: свой, чтобы не делить `MUTATOR_CONFIG_DIR` с другими.
    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mutator-recent-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn file(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, b"x").unwrap();
        p
    }

    /// `agr:open-without-file` — свежий файл первым, повторное открытие не
    /// плодит дублей, а поднимает файл наверх.
    #[test]
    fn push_puts_newest_first_without_duplicates() {
        let d = scratch("push");
        let (a, b) = (file(&d, "a.dat"), file(&d, "b.dat"));
        let mut r = Recent::default();
        r.push(&a);
        r.push(&b);
        r.push(&a);
        assert_eq!(r.files, vec![absolute(&a), absolute(&b)]);
    }

    /// `agr:open-without-file` — помним не больше [`LIMIT`] файлов, самые
    /// старые отпадают.
    #[test]
    fn push_keeps_only_the_limit() {
        let d = scratch("limit");
        let mut r = Recent::default();
        let all: Vec<_> = (0..LIMIT + 3)
            .map(|i| file(&d, &format!("f{i}.dat")))
            .collect();
        for p in &all {
            r.push(p);
        }
        assert_eq!(r.files.len(), LIMIT);
        assert_eq!(r.files[0], absolute(all.last().unwrap()));
        assert!(!r.files.contains(&absolute(&all[0])), "самый старый отпал");
    }

    /// `agr:open-without-file` — удалённый файл в меню и в «последнем» не
    /// участвует, но из списка не стирается.
    #[test]
    fn missing_files_are_skipped_but_kept() {
        let d = scratch("missing");
        let (a, b) = (file(&d, "a.dat"), file(&d, "b.dat"));
        let mut r = Recent::default();
        r.push(&a);
        r.push(&b);
        std::fs::remove_file(&b).unwrap();
        assert_eq!(r.last_existing(), Some(absolute(&a)));
        assert_eq!(r.existing().len(), 1);
        assert_eq!(r.files.len(), 2, "в списке файл остался");
        std::fs::remove_file(&a).unwrap();
        assert_eq!(r.last_existing(), None);
    }

    /// `agr:open-without-file` — список переживает перезапуск; нет файла или
    /// он испорчен — пустой список, а не ошибка.
    #[test]
    fn roundtrips_through_disk_and_tolerates_garbage() {
        let d = scratch("disk");
        let mut r = Recent::default();
        r.push(&file(&d, "a.dat"));
        r.save_to(&d).unwrap();
        assert_eq!(Recent::load_from(&d), r);
        std::fs::write(d.join("recent.toml"), "это не toml {").unwrap();
        assert_eq!(Recent::load_from(&d), Recent::default());
        assert_eq!(Recent::load_from(&d.join("нет")), Recent::default());
    }

    /// `agr:open-without-file` — «Очистить список» опустошает его.
    #[test]
    fn clear_empties_the_list() {
        let d = scratch("clear");
        let mut r = Recent::default();
        r.push(&file(&d, "a.dat"));
        r.clear();
        assert!(r.files.is_empty());
        assert_eq!(r.last_existing(), None);
    }
}
