//! Оболочка окна без экрана: список линков без файла, перетаскивание файла,
//! смена файла (`agr:open-without-file`, `agr:resolve-browse`). Кадры egui —
//! над эталонными файлами.
//!
//! Сборка без GUI не тянет eframe, поэтому файл подключается только при фиче "gui".

#![cfg(feature = "gui")]

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, Once};

use tui_patch::recent::Recent;
use tui_patch::ui::Shell;
use tui_patch::{io, session};

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
        io::quiet();
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
        self.shell
            .app()
            .map(|a| PathBuf::from(a.desk.file.name.clone()))
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

/// `agr:open-without-file` `agr:resolve-browse` — окно без файла показывает
/// список линков resolve; сервер не знаем — просит его адрес.
#[test]
fn window_without_file_shows_resolve_list() {
    let mut w = Win::empty();
    std::env::remove_var("MUTATOR_RESOLVE");
    let texts = w.frame();
    assert!(w.shell.app().is_none());
    assert!(w.shell.browsing());
    assert!(has(&texts, "Линки resolve"), "{texts:?}");
    assert!(has(&texts, "введите адрес resolve-сервера"), "{texts:?}");
    assert!(has(&texts, "Ctrl+O"), "{texts:?}");
    assert!(
        !texts.iter().any(|t| t.contains("⌘O")),
        "⌘O забирает меню «Открыть…»: {texts:?}"
    );
}

/// `agr:resolve-browse` — F5 показывает список поверх открытого файла, Esc
/// возвращает к нему же.
#[test]
fn f5_shows_list_over_open_file_and_esc_returns() {
    let mut w = Win::empty();
    std::env::remove_var("MUTATOR_RESOLVE");
    let lz4 = fixture("lz4.dat");
    w.drop_file(&lz4);
    assert!(!w.shell.browsing());
    let key = |k| egui::Event::Key {
        key: k,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Default::default(),
    };
    let texts = w.frame_with(|i| i.events.push(key(egui::Key::F5)));
    assert!(w.shell.browsing(), "F5 — список");
    assert!(has(&texts, "Линки resolve"), "{texts:?}");
    assert!(w.shell.browse().is_some_and(|b| b.back));
    w.frame_with(|i| i.events.push(key(egui::Key::Escape)));
    w.frame();
    assert!(!w.shell.browsing(), "Esc — назад к виду");
    assert_eq!(w.open_path(), Some(lz4), "вид тот же");
}

/// `agr:resolve-browse` — ошибка открытия из списка поверх файла видна в
/// списке: строки вида на экране нет.
#[test]
fn open_error_from_the_list_is_shown_on_the_list() {
    let mut w = Win::empty();
    std::env::remove_var("MUTATOR_RESOLVE");
    let lz4 = fixture("lz4.dat");
    w.drop_file(&lz4);
    let ctx = w.ctx.clone();
    w.shell.show_browse(&ctx);
    w.shell.open_link(&ctx, "not-a-link", "", None, false, None);
    let note = w
        .shell
        .browse()
        .and_then(|b| b.note.clone())
        .unwrap_or_default();
    assert!(note.contains("not-a-link"), "{note}");
    let status = w.shell.app().unwrap().desk.status.clone();
    assert!(
        !status.contains("not-a-link"),
        "ошибка ушла в строку вида: {status}"
    );
    assert!(has(&w.frame(), "not-a-link"), "строка списка");
}

/// `agr:link-state` — Esc на ожидании линка бросает его и остаётся в списке,
/// а не возвращает к файлу.
#[test]
fn esc_on_the_waiting_link_stays_on_the_list() {
    let mut w = Win::empty();
    std::env::remove_var("MUTATOR_RESOLVE");
    let lz4 = fixture("lz4.dat");
    w.drop_file(&lz4);
    let ctx = w.ctx.clone();
    w.shell.show_browse(&ctx);
    w.shell.open_link(
        &ctx,
        "resolve://nosuch/channel",
        "mode=online",
        None,
        false,
        None,
    );
    let texts = w.frame();
    assert!(has(&texts, "Esc — бросить"), "{texts:?}");
    assert!(!w.shell.browsing(), "кадр ожидания");
    let key = |k| egui::Event::Key {
        key: k,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Default::default(),
    };
    w.frame_with(|i| i.events.push(key(egui::Key::Escape)));
    assert!(w.shell.browsing(), "Esc остался в списке");
    assert_eq!(w.open_path(), Some(lz4), "файл под списком тот же");
    let texts = w.frame();
    assert!(has(&texts, "Линки resolve"), "{texts:?}");
    assert!(!has(&texts, "Esc — бросить"), "{texts:?}");
}

/// `agr:open-without-file` — файл, брошенный в окно-заглушку, открывается и
/// попадает первым в список последних, а тот переживает перезапуск.
#[test]
fn dropped_file_opens_over_placeholder_and_is_remembered() {
    let mut w = Win::empty();
    let lz4 = fixture("lz4.dat");
    let texts = w.drop_file(&lz4);
    assert_eq!(w.open_path(), Some(lz4.clone()));
    assert!(!w.shell.browsing(), "список ушёл");
    let _ = texts;
    assert_eq!(
        w.shell.recent().files.first(),
        std::fs::canonicalize(&lz4).ok().as_ref()
    );
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

/// `agr:link-open` — `o` у линка в окне открывает форму и в обычном виде, и
/// в виде TUI (F2).
#[test]
fn o_opens_link_form_in_window_and_f2() {
    let mut w = Win::empty();
    if !tui_patch::io::Context::new().ensure_lua(io::Backend::TLL) {
        return;
    }
    let src = fixture("plain.dat");
    let url = format!(
        "lua+file://{};autoclose=no;lua.code=function tll_on_data(seq, name, data) \
         tll_callback(seq, name, data) end",
        src.display()
    );
    let ctx = w.ctx.clone();
    w.shell.open_path(&ctx, Path::new(&url));
    for _ in 0..500 {
        w.frame();
        if w.shell.app().is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(w.shell.app().is_some(), "линк открылся");
    let key = |k| egui::Event::Key {
        key: k,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Default::default(),
    };
    for look_f2 in [false, true] {
        if look_f2 {
            w.frame_with(|i| i.events.push(key(egui::Key::F2)));
            w.frame();
        }
        let texts = w.frame_with(|i| i.events.push(egui::Event::Text("o".into())));
        let texts = [texts, w.frame()].concat();
        let app = w.shell.app().unwrap();
        assert!(app.desk.link_form.is_some(), "F2={look_f2}: форма открыта");
        // Сетка F2 рисует по знаку на клетку — текст склеиваем.
        let all: String = texts.concat();
        assert!(
            all.contains("Открытьлинк") || all.contains("Открыть линк"),
            "F2={look_f2}: форма видна"
        );
        w.frame_with(|i| i.events.push(key(egui::Key::Escape)));
        assert!(w.shell.app().unwrap().desk.link_form.is_none());
    }
}
