//! Оболочка окна без экрана: заглушка, перетаскивание файла, смена файла
//! (`agr:open-without-file`). Кадры egui — над эталонными файлами.
//!
//! Сборка без GUI не тянет eframe, поэтому файл подключается только при фиче "gui".

#![cfg(feature = "gui")]

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, Once};

use egui;
use tui_patch::recent::Recent;
use tui_patch::ui::Shell;
use tui_patch::{session, tll};

/// Корень крейта во время запуска: сборка могла идти в другом каталоге.
fn root() -> PathBuf {
    std::env::var_os("CARGO_MANIFEST_DIR")
        .map(Into::into)
        .unwrap_or_else(|| env!("CARGO_MANIFEST_DIR").into())
}

fn fixture(name: &str) -> PathBuf {
    root().join("tests/data").join(name)
}

/// Свой каталог настроек на весь файл тестов: сессии и список последних не
/// должны попасть в настоящие настройки пользователя. Каталог один на процесс
/// (переменная окружения общая), поэтому тесты идут по одному: [`Win`] держит
/// замок и начинает с чистого каталога.
fn config() -> PathBuf {
    static ONCE: Once = Once::new();
    let dir = std::env::temp_dir().join(format!("mutator-shell-test-{}", std::process::id()));
    ONCE.call_once(|| {
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("MUTATOR_CONFIG_DIR", &dir);
        tll::quiet();
    });
    dir
}

/// Замок на каталог настроек; упавший тест замок не отравляет.
fn lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

struct Win {
    ctx: egui::Context,
    shell: Shell,
    t: f64,
    _lock: MutexGuard<'static, ()>,
}

impl Win {
    fn new(shell: Shell) -> Win {
        let guard = lock();
        let dir = config();
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Win {
            ctx: egui::Context::default(),
            shell,
            t: 0.0,
            _lock: guard,
        }
    }

    fn empty() -> Win {
        Win::new(Shell::new(None, Recent::default()))
    }

    /// Кадр с событиями ввода; `raw` — остальное, что приносит система.
    fn frame_with(&mut self, raw: impl FnOnce(&mut egui::RawInput)) -> Vec<String> {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1200.0, 800.0),
            )),
            time: Some(self.t),
            ..Default::default()
        };
        self.t += 1.0 / 60.0;
        raw(&mut input);
        let shell = &mut self.shell;
        let out = self.ctx.run_ui(input, |ui| shell.draw(ui));
        let mut texts = Vec::new();
        for s in &out.shapes {
            collect(&s.shape, &mut texts);
        }
        texts
    }

    fn frame(&mut self) -> Vec<String> {
        self.frame_with(|_| {})
    }

    fn drop_file(&mut self, path: &Path) -> Vec<String> {
        let p = path.to_path_buf();
        self.frame_with(|i| {
            i.dropped_files.push(egui::DroppedFile {
                path: Some(p),
                ..Default::default()
            })
        })
    }

    /// Путь открытого файла.
    fn open_path(&self) -> Option<PathBuf> {
        self.shell.app().map(|a| a.desk.file.path.clone())
    }
}

fn collect(shape: &egui::Shape, out: &mut Vec<String>) {
    match shape {
        egui::Shape::Text(t) => out.push(t.galley.text().to_string()),
        egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, out)),
        _ => {}
    }
}

fn has(texts: &[String], needle: &str) -> bool {
    texts.iter().any(|t| t.contains(needle))
}

/// `agr:open-without-file` — окно без файла показывает заглушку с подсказкой,
/// а не пустую таблицу.
#[test]
fn window_without_file_shows_placeholder() {
    let mut w = Win::empty();
    let texts = w.frame();
    assert!(w.shell.app().is_none());
    assert!(has(&texts, "Перетащите файл датника"), "{texts:?}");
}

/// `agr:open-without-file` — файл, брошенный в окно-заглушку, открывается и
/// попадает первым в список последних, а тот переживает перезапуск.
#[test]
fn dropped_file_opens_over_placeholder_and_is_remembered() {
    let mut w = Win::empty();
    let lz4 = fixture("lz4.dat");
    let texts = w.drop_file(&lz4);
    assert_eq!(w.open_path(), Some(lz4.clone()));
    assert!(!has(&texts, "Перетащите файл датника"), "заглушка ушла");
    assert_eq!(w.shell.recent().files.first(), std::fs::canonicalize(&lz4).ok().as_ref());
    assert_eq!(
        Recent::load_from(&config()).files.first(),
        w.shell.recent().files.first(),
        "список записан на диск"
    );
}

/// `agr:open-without-file` — брошенный файл заменяет открытый: у нового своя
/// сессия, а сессия прежнего записана на диск.
#[test]
fn dropped_file_replaces_open_one_and_keeps_its_session() {
    let mut w = Win::empty();
    let (lz4, plain) = (fixture("lz4.dat"), fixture("plain.dat"));
    w.drop_file(&lz4);
    w.drop_file(&plain);
    assert_eq!(w.open_path(), Some(plain.clone()));
    assert!(
        session::load(&lz4).is_some(),
        "сессия прежнего файла записана при смене"
    );
    assert_eq!(w.shell.recent().files.len(), 2);
    assert_eq!(
        w.shell.recent().files.first(),
        std::fs::canonicalize(&plain).ok().as_ref(),
        "новый файл — первым"
    );
}

/// `agr:open-without-file` — файл, которого нет, не ломает окно: прежний
/// остаётся, причина — в строке состояния; у заглушки — в подсказке.
#[test]
fn unreadable_drop_keeps_current_state_and_says_why() {
    let mut w = Win::empty();
    let texts = {
        w.drop_file(&fixture("нет-такого.dat"));
        w.frame()
    };
    assert!(w.shell.app().is_none());
    assert!(w.shell.note().is_some());
    assert!(has(&texts, "нет-такого.dat"), "{texts:?}");

    let lz4 = fixture("lz4.dat");
    w.drop_file(&lz4);
    w.drop_file(&fixture("тоже-нет.dat"));
    assert_eq!(w.open_path(), Some(lz4), "прежний файл на месте");
    let status = &w.shell.app().unwrap().desk.status;
    assert!(status.contains("тоже-нет.dat"), "{status}");
}

/// `agr:open-without-file` — тот же файл ещё раз не пересобирает окно: вид,
/// фильтр и место остаются.
#[test]
fn dropping_the_open_file_again_does_nothing() {
    let mut w = Win::empty();
    let lz4 = fixture("lz4.dat");
    w.drop_file(&lz4);
    let first = w.shell.recent().files.clone();
    assert!(!w.shell.app().is_none());
    w.drop_file(&lz4);
    assert_eq!(w.shell.recent().files, first);
    assert!(w.shell.app().unwrap().desk.status.is_empty());
}

/// `agr:open-without-file` — пока файл над окном, видна подсказка, что будет.
#[test]
fn hovering_a_file_shows_a_drop_hint() {
    let mut w = Win::empty();
    let p = fixture("lz4.dat");
    let texts = w.frame_with(|i| {
        i.hovered_files.push(egui::HoveredFile {
            path: Some(p),
            ..Default::default()
        })
    });
    assert!(has(&texts, "Отпустите, чтобы открыть lz4.dat"), "{texts:?}");
    assert!(!has(&w.frame(), "Отпустите"), "ушёл файл — ушла подсказка");
}
