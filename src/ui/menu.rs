//! Родное меню macOS и диалог выбора файла (`agr:open-without-file`).
//!
//! Меню — системная строка вверху экрана (`muda`): меню приложения (скрыть,
//! выйти) и «Файл» с пунктами «Открыть…» (⌘O), «Линки resolve…» (F5),
//! «Последние файлы» и «Очистить список». Edit-меню нет намеренно: его ускорители перехватили бы ⌘C/⌘V,
//! которые окно получает как обычные клавиши. Меню создаётся в создании
//! приложения eframe — на главном потоке и после старта `NSApplication`.
//! Щелчки приходят обработчиком `muda` из системы: он кладёт действие в канал
//! и будит окно, а [`Menu::poll`] забирает их в кадре.
//!
//! Подменю последних пересобирается [`Menu::sync`] при каждом изменении
//! списка. Пункт называет файл и каталог, а какой путь за пунктом, меню помнит
//! само: [`Action::Recent`] несёт путь, а не номер.
//!
//! На других системах (и без `muda`) меню нет: те же имена, но [`Menu::poll`]
//! пуст, а [`pick_file`] ничего не показывает. Остаётся перетаскивание файла
//! в окно.
//!
//! Наружу: [`Menu`], [`Action`], [`pick_file`].

use std::path::PathBuf;

/// Что выбрали в меню.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// «Открыть…» — показать диалог выбора файла.
    Open,
    /// «Линки resolve…» — список линков (F5).
    Browse,
    /// Файл из «Последних».
    Recent(PathBuf),
    /// «Очистить список».
    Clear,
}

/// Идентификатор пункта «Открыть…».
#[cfg(target_os = "macos")]
const OPEN: &str = "open";
/// Идентификатор пункта «Линки resolve…».
#[cfg(target_os = "macos")]
const BROWSE: &str = "browse";
/// Идентификатор пункта «Очистить список».
#[cfg(target_os = "macos")]
const CLEAR: &str = "clear";
/// Приставка идентификаторов файлов: `recent:<номер в списке>`.
#[cfg(target_os = "macos")]
const RECENT: &str = "recent:";

/// Подпись пункта файла: имя и, в скобках, каталог.
fn label(path: &std::path::Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    match path.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(dir) => format!("{name}  ({})", dir.display()),
        None => name,
    }
}

/// Диалог выбора датника; `None` — закрыли ничего не выбрав (или диалога нет).
#[cfg(target_os = "macos")]
pub fn pick_file() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Открыть датник")
        .add_filter("Датники TLL", &["dat"])
        .add_filter("Все файлы", &["*"])
        .pick_file()
}

#[cfg(not(target_os = "macos"))]
pub fn pick_file() -> Option<PathBuf> {
    None
}

#[cfg(target_os = "macos")]
pub use mac::Menu;

#[cfg(target_os = "macos")]
mod mac {
    use std::path::PathBuf;
    use std::sync::mpsc::{channel, Receiver};

    use muda::accelerator::{Accelerator, Code, Modifiers};
    use muda::{Menu as Native, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};

    use super::{label, Action, BROWSE, CLEAR, OPEN, RECENT};
    use crate::recent::Recent;

    /// Системное меню окна.
    pub struct Menu {
        /// Держим: меню живёт, пока жив этот владелец. `None` — меню без
        /// системной строки (тесты).
        _native: Option<Native>,
        recent: Option<Submenu>,
        /// Что за пунктами подменю «Последние» — путь по порядку.
        paths: Vec<PathBuf>,
        rx: Receiver<String>,
    }

    impl Menu {
        pub fn new(ctx: &egui::Context, recent: &Recent) -> Menu {
            let native = Native::new();
            let app = Submenu::new("mutator", true);
            let _ = app.append_items(&[
                &PredefinedMenuItem::about(None, None),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::services(None),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::hide(None),
                &PredefinedMenuItem::hide_others(None),
                &PredefinedMenuItem::show_all(None),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::quit(None),
            ]);
            let file = Submenu::new("Файл", true);
            let open = MenuItem::with_id(
                OPEN,
                "Открыть…",
                true,
                Some(Accelerator::new(Modifiers::META, Code::KeyO)),
            );
            let browse = MenuItem::with_id(
                BROWSE,
                "Линки resolve…",
                true,
                Some(Accelerator::new(Modifiers::empty(), Code::F5)),
            );
            let list = Submenu::new("Последние файлы", true);
            let clear = MenuItem::with_id(CLEAR, "Очистить список", true, None);
            let _ = file.append_items(&[
                &open,
                &browse,
                &list,
                &PredefinedMenuItem::separator(),
                &clear,
            ]);
            let _ = native.append_items(&[&app, &file]);
            native.init_for_nsapp();

            let (tx, rx) = channel();
            let wake = ctx.clone();
            MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
                let _ = tx.send(e.id.0.clone());
                wake.request_repaint();
            }));
            let mut m = Menu {
                _native: Some(native),
                recent: Some(list),
                paths: Vec::new(),
                rx,
            };
            m.sync(recent);
            m
        }

        /// Без системного меню: ничего не рисует и не присылает.
        pub fn none() -> Menu {
            let (_tx, rx) = channel();
            Menu {
                _native: None,
                recent: None,
                paths: Vec::new(),
                rx,
            }
        }

        /// Пересобрать подменю «Последние файлы» по списку.
        pub fn sync(&mut self, recent: &Recent) {
            self.paths = recent.existing().into_iter().cloned().collect();
            let Some(list) = &self.recent else { return };
            while list.remove_at(0).is_some() {}
            if self.paths.is_empty() {
                let _ = list.append(&MenuItem::new("Нет последних файлов", false, None));
                return;
            }
            for (i, p) in self.paths.iter().enumerate() {
                let _ = list.append(&MenuItem::with_id(
                    format!("{RECENT}{i}"),
                    label(p),
                    true,
                    None,
                ));
            }
        }

        /// Действия, выбранные в меню с прошлого кадра.
        pub fn poll(&mut self) -> Vec<Action> {
            let mut out = Vec::new();
            while let Ok(id) = self.rx.try_recv() {
                match id.as_str() {
                    OPEN => out.push(Action::Open),
                    BROWSE => out.push(Action::Browse),
                    CLEAR => out.push(Action::Clear),
                    _ => {
                        let path = id
                            .strip_prefix(RECENT)
                            .and_then(|n| n.parse::<usize>().ok())
                            .and_then(|n| self.paths.get(n));
                        if let Some(p) = path {
                            out.push(Action::Recent(p.clone()));
                        }
                    }
                }
            }
            out
        }
    }
}

/// Без родного меню: пустая заглушка с теми же именами.
#[cfg(not(target_os = "macos"))]
pub struct Menu;

#[cfg(not(target_os = "macos"))]
impl Menu {
    pub fn new(_ctx: &egui::Context, _recent: &crate::recent::Recent) -> Menu {
        Menu
    }

    pub fn none() -> Menu {
        Menu
    }

    pub fn sync(&mut self, _recent: &crate::recent::Recent) {}

    pub fn poll(&mut self) -> Vec<Action> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Подпись пункта: имя файла и каталог в скобках.
    #[test]
    fn label_shows_name_and_directory() {
        assert_eq!(
            label(std::path::Path::new("/data/mu0/out.dat")),
            "out.dat  (/data/mu0)"
        );
        assert_eq!(label(std::path::Path::new("out.dat")), "out.dat");
    }
}
