//! Оболочка окна: файл можно открыть, не перезапуская программу
//! (`agr:open-without-file`).
//!
//! `Shell` стоит над [`App`](super::App): держит `Option<App>`, список
//! последних файлов и родное меню. Окно без файла — это `Shell` без `App`:
//! список линков resolve ([`Browse`], `agr:resolve-browse`), а `Desk` не знает
//! про «файла нет» (иначе нечего было бы показывать в таблице, агенту и сессии).
//! F5 (или меню «Файл → Линки resolve…») показывает список и поверх открытого
//! вида; Esc — назад к виду. Выбранный линк открывается, как любой другой.
//!
//! Файл открывают два пути, и оба ведут в [`Shell::open_path`]: меню «Файл»
//! (диалог или «Последние») и перетаскивание файла в окно (в любом состоянии —
//! и над списком, и поверх открытого файла).
//!
//! Смена файла строит `App` заново ([`App::from_file`](super::App::from_file)):
//! агент и его диалоги привязаны к имени файла при старте. Сессия прежнего
//! файла пишется на диск, агенту прежнего — отбой; у нового — своя сессия
//! (`agr:session-by-name`). Идёт фиксация правок — файл не меняем: она ещё
//! пишет, и закрыть её окном было бы потерей. Не открылся новый — остаётся
//! прежний, причина — в подсказке заглушки или в строке состояния.
//!
//! Линк (`resolve://…`) окно ждёт, пока он не станет Active: до готового
//! спула заглушка показывает его состояние и ошибку (`agr:link-state`), потом
//! строится `App` на спуле. Линк из «Последних» открывается так же.
//!
//! Наружу: [`Shell`], [`Setup`].

use std::path::{Path, PathBuf};

use egui;

use super::menu::{self, Action, Menu};
use super::{theme::*, App};
use crate::browse::{self, Browse};
use crate::data::File;
use crate::desk::Desk;
use crate::io::link::{self, Link, LinkState};
use crate::io::resolve::Browser;
use crate::recent::Recent;
use crate::session;

/// Окно: приложение с файлом или заглушка, последние файлы, меню.
pub struct Shell {
    app: Option<App>,
    recent: Recent,
    menu: Menu,
    /// Почему не открылся файл — показывает список.
    note: Option<String>,
    /// Линк, который ещё не стал Active: заглушка показывает его состояние.
    pending: Option<Pending>,
    /// Список линков resolve: заводится при первом показе.
    browse: Option<Browse>,
    /// Список поверх открытого вида (F5); без вида он и так на экране.
    browsing: bool,
    /// Дробные строки колеса списка.
    wheel: f32,
}

/// Линк в ожидании спула.
struct Pending {
    link: Link,
    fresh: bool,
    setup: Option<Setup>,
}

/// Параметры командной строки поверх сессии — один раз, на готовом виде.
pub type Setup = Box<dyn FnOnce(&mut Desk)>;

impl Shell {
    /// Оболочка без родного меню; [`Shell::with_menu`] его подключает.
    pub fn new(app: Option<App>, recent: Recent) -> Shell {
        Shell {
            app,
            recent,
            menu: Menu::none(),
            note: None,
            pending: None,
            browse: None,
            browsing: false,
            wheel: 0.0,
        }
    }

    /// Открыть линк: окно на нём появится, когда спул будет готов; до того —
    /// заглушка с его состоянием. `fresh` — без сессии, `setup` — параметры
    /// командной строки поверх неё.
    pub fn open_link(
        &mut self,
        ctx: &egui::Context,
        url: &str,
        open: &str,
        keep: Option<PathBuf>,
        fresh: bool,
        setup: Option<Setup>,
    ) {
        let repaint = ctx.clone();
        let notify = std::sync::Arc::new(move || repaint.request_repaint());
        match Link::start(url, open, None, keep.as_deref(), notify) {
            Ok(link) => {
                self.pending = Some(Pending { link, fresh, setup });
                self.note = None;
                if let Some(b) = &mut self.browse {
                    b.note = None;
                }
            }
            Err(e) => self.say(format!("{url}: {e}")),
        }
    }

    /// Ждущий линк стал Active — окно переезжает на его спул.
    fn take_ready(&mut self, ctx: &egui::Context) {
        let ready = self
            .pending
            .as_ref()
            .is_some_and(|p| p.link.status().spool.is_some());
        if !ready {
            return;
        }
        let Pending { link, fresh, setup } = self.pending.take().unwrap();
        let url = link.url().to_string();
        let file = match File::open_link(link) {
            Ok(f) => f,
            Err(e) => {
                self.say(format!("{url}: {e}"));
                return;
            }
        };
        if let Some(old) = self.app.take() {
            old.finish(ctx);
        }
        let session = if fresh {
            None
        } else {
            session::load(&file.session_key())
        };
        let setup = setup.unwrap_or_else(|| Box::new(|_: &mut Desk| {}));
        self.app = Some(App::from_file(ctx, file, session, setup));
        self.browsing = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!("mutator — {url}")));
        self.recent.push(Path::new(&url));
        self.save_recent();
        self.menu.sync(&self.recent);
    }

    /// С родным меню: оно создаётся в создании приложения eframe.
    pub fn with_menu(mut self, menu: Menu) -> Shell {
        self.menu = menu;
        self
    }

    /// Приложение с открытым файлом, если оно есть.
    pub fn app(&self) -> Option<&App> {
        self.app.as_ref()
    }

    /// Последние открытые файлы.
    pub fn recent(&self) -> &Recent {
        &self.recent
    }

    /// Почему не открылся последний выбранный файл.
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    /// Виден ли список линков resolve.
    pub fn browsing(&self) -> bool {
        self.pending.is_none() && (self.browsing || self.app.is_none())
    }

    /// Список линков, если его уже заводили.
    pub fn browse(&self) -> Option<&Browse> {
        self.browse.as_ref()
    }

    /// Показать список линков (F5, меню); Esc вернёт к открытому виду.
    pub fn show_browse(&mut self, ctx: &egui::Context) {
        self.browsing = true;
        let back = self.app.is_some();
        self.ensure_browse(ctx).back = back;
    }

    /// Список на сервере из `--resolve`, окружения или `recent.toml`.
    fn ensure_browse(&mut self, ctx: &egui::Context) -> &mut Browse {
        let recent = &self.recent;
        self.browse.get_or_insert_with(|| {
            let repaint = ctx.clone();
            let notify: std::sync::Arc<dyn Fn() + Send + Sync> =
                std::sync::Arc::new(move || repaint.request_repaint());
            let browser = recent
                .resolve_addr()
                .map(|a| std::sync::Arc::new(Browser::start(&a, notify.clone())));
            Browse::new(browser, notify)
        })
    }

    /// Кадр списка и что в нём выбрали.
    fn browse_view(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let back = self.app.is_some();
        let note = self.note.clone();
        let mut wheel = self.wheel;
        let b = self.ensure_browse(&ctx);
        b.back = back;
        if note.is_some() && b.note.is_none() {
            b.note = note;
        }
        let action = super::browse::show(ui, b, &mut wheel);
        self.wheel = wheel;
        match action {
            Some(browse::Action::Open(url, open)) => {
                self.note = None;
                self.open_link(&ctx, &url, &open, None, false, None);
            }
            Some(browse::Action::Back) => self.browsing = false,
            Some(browse::Action::Connect(a)) => {
                self.recent.resolve = Some(a);
                self.save_recent();
            }
            Some(browse::Action::Quit) | None => {}
        }
    }

    /// Открыть файл вместо текущего (или вместо заглушки). `true` — открыли.
    pub fn open_path(&mut self, ctx: &egui::Context, path: &Path) -> bool {
        if let Some(url) = path.to_str().filter(|s| link::is_link(s)) {
            self.open_link(ctx, url, &link::default_open(url), None, false, None);
            return true;
        }
        if let Some(app) = &self.app {
            if app.desk.file.name == path.to_string_lossy() {
                return false;
            }
            if app
                .desk
                .commit
                .as_ref()
                .is_some_and(|c| c.running.is_some())
            {
                self.say("идёт фиксация правок — файл сменить нельзя".into());
                return false;
            }
        }
        let file = match File::open(path) {
            Ok(f) => f,
            Err(e) => {
                self.say(format!("{}: {e}", path.display()));
                return false;
            }
        };
        if let Some(old) = self.app.take() {
            old.finish(ctx);
        }
        let session = session::load(path);
        self.app = Some(App::from_file(ctx, file, session, |_| {}));
        self.note = None;
        self.browsing = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
            "mutator — {}",
            path.display()
        )));
        self.recent.push(path);
        self.save_recent();
        self.menu.sync(&self.recent);
        true
    }

    /// Сообщение: список на экране — в его строку, заглушке — в подсказку,
    /// окну с файлом — в строку состояния. Пока виден список, строки вида нет.
    fn say(&mut self, text: String) {
        if self.browsing {
            if let Some(b) = &mut self.browse {
                b.note = Some(text);
                return;
            }
        }
        match &mut self.app {
            Some(app) => app.desk.status = text,
            None => self.note = Some(text),
        }
    }

    fn save_recent(&mut self) {
        if let Err(e) = self.recent.save() {
            self.say(format!("последние файлы: {e}"));
        }
    }

    /// Выбранное в меню с прошлого кадра.
    fn menu_actions(&mut self, ctx: &egui::Context) {
        for a in self.menu.poll() {
            match a {
                Action::Open => self.ask_file(ctx),
                Action::Browse => self.show_browse(ctx),
                Action::Recent(p) => {
                    self.open_path(ctx, &p);
                }
                Action::Clear => {
                    self.recent.clear();
                    self.save_recent();
                    self.menu.sync(&self.recent);
                }
            }
        }
    }

    /// Диалог выбора файла; закрыли ничего не выбрав — всё как было.
    fn ask_file(&mut self, ctx: &egui::Context) {
        if let Some(p) = menu::pick_file() {
            self.open_path(ctx, &p);
        }
    }

    /// Перетащили файл — открыть первый из брошенных.
    fn take_dropped(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.iter().find_map(|f| f.path.clone()));
        if let Some(p) = dropped {
            self.open_path(ctx, &p);
        }
    }

    /// Над окном держат файл: затемнение и подсказка, что будет.
    fn drop_hint(&self, ctx: &egui::Context) {
        let Some(path) = ctx.input(|i| i.raw.hovered_files.iter().find_map(|f| f.path.clone()))
        else {
            return;
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("drop-hint"),
        ));
        let screen = ctx.content_rect();
        painter.rect_filled(screen, 0.0, egui::Color32::from_black_alpha(170));
        painter.text(
            screen.center(),
            egui::Align2::CENTER_CENTER,
            format!("Отпустите, чтобы открыть {name}"),
            egui::FontId::proportional(22.0),
            ACCENT,
        );
    }

    /// Заглушка линка, который ещё не стал Active: его состояние и ошибка;
    /// Esc — бросить его и вернуться.
    fn waiting(&self, ui: &mut egui::Ui, p: &Pending) {
        let s = p.link.status();
        let fg = match s.state {
            LinkState::Error => ERROR,
            LinkState::Closed => DIM,
            _ => NAME,
        };
        egui::CentralPanel::default().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() / 3.0);
                ui.label(
                    egui::RichText::new(&s.url)
                        .size(18.0)
                        .monospace()
                        .color(TEXT),
                );
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(format!("● {} · {}", s.state.name(), s.open))
                        .size(22.0)
                        .color(fg),
                );
                if let Some(e) = &s.error {
                    ui.add_space(12.0);
                    ui.colored_label(ERROR, e);
                }
                ui.add_space(16.0);
                ui.label(egui::RichText::new("Esc — бросить").color(DIM));
            });
        });
    }

    /// Кадр окна без eframe — его зовут и тесты.
    pub fn draw(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.take_dropped(&ctx);
        self.take_ready(&ctx);
        // Esc бросает ждущий линк и на этом кадр кончается: та же клавиша не
        // доходит до списка (там это «назад к файлу») и до детали в `Desk`.
        if self.pending.is_some() {
            let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
            if esc {
                self.pending = None;
                ctx.request_repaint();
            } else if let Some(p) = &self.pending {
                self.waiting(ui, p);
            }
            self.drop_hint(&ctx);
            return;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::F5)) && !self.browsing() {
            self.show_browse(&ctx);
        }
        if self.browsing() {
            self.browse_view(ui);
        } else if let Some(app) = &mut self.app {
            app.draw(ui);
            // Форма `o` вида назвала другой линк: открыть его, как из списка.
            if let Some((url, open)) = app.desk.open_link.take() {
                self.open_link(&ctx, &url, &open, None, false, None);
            }
        }
        self.drop_hint(&ctx);
    }
}

impl eframe::App for Shell {
    fn on_exit(&mut self) {
        if let Some(app) = &mut self.app {
            app.on_exit();
        }
    }

    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.menu_actions(ctx);
        if let Some(app) = self
            .app
            .as_mut()
            .filter(|_| self.pending.is_none() && !self.browsing)
        {
            app.logic(ctx, frame);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        if self.pending.is_some() {
            return;
        }
        if let Some(app) = &mut self.app {
            app.raw_input_hook(ctx, raw);
        }
    }
}
