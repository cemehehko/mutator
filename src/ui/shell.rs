//! Оболочка окна: файл можно открыть, не перезапуская программу
//! (`agr:open-without-file`).
//!
//! `Shell` стоит над [`App`](super::App): держит `Option<App>`, список
//! последних файлов и родное меню. Окно без файла — это `Shell` без `App`:
//! заглушка с подсказкой, а `Desk` не знает про «файла нет» (иначе нечего было
//! бы показывать в таблице, агенту и сессии).
//!
//! Файл открывают три пути, и все ведут в [`Shell::open_path`]: меню «Файл»
//! (диалог или «Последние»), перетаскивание файла в окно (в любом состоянии —
//! и над заглушкой, и поверх открытого файла) и диалог первого кадра, если при
//! запуске без аргумента последнего файла не нашлось. Закрыли диалог ничего не
//! выбрав — остаётся заглушка.
//!
//! Смена файла строит `App` заново ([`App::from_file`](super::App::from_file)):
//! агент и его диалоги привязаны к имени файла при старте. Сессия прежнего
//! файла пишется на диск, агенту прежнего — отбой; у нового — своя сессия
//! (`agr:session-by-name`). Идёт фиксация правок — файл не меняем: она ещё
//! пишет, и закрыть её окном было бы потерей. Не открылся новый — остаётся
//! прежний, причина — в подсказке заглушки или в строке состояния.
//!
//! Наружу: [`Shell`].

use std::path::Path;

use egui;

use super::menu::{self, Action, Menu};
use super::{theme::*, App};
use crate::data::File;
use crate::recent::Recent;
use crate::session;

/// Окно: приложение с файлом или заглушка, последние файлы, меню.
pub struct Shell {
    app: Option<App>,
    recent: Recent,
    menu: Menu,
    /// Почему не открылся файл — показывает заглушка.
    note: Option<String>,
    /// Диалог первого кадра уже спрашивали (или файл был с самого начала).
    asked: bool,
}

impl Shell {
    /// Оболочка без родного меню; [`Shell::with_menu`] его подключает.
    pub fn new(app: Option<App>, recent: Recent) -> Shell {
        Shell {
            asked: app.is_some(),
            app,
            recent,
            menu: Menu::none(),
            note: None,
        }
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

    /// Открыть файл вместо текущего (или вместо заглушки). `true` — открыли.
    pub fn open_path(&mut self, ctx: &egui::Context, path: &Path) -> bool {
        if let Some(app) = &self.app {
            if app.desk.file.path == path {
                return false;
            }
            if app.desk.commit.as_ref().is_some_and(|c| c.running.is_some()) {
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
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
            "mutator — {}",
            path.display()
        )));
        self.recent.push(path);
        self.save_recent();
        self.menu.sync(&self.recent);
        true
    }

    /// Сообщение: заглушке — в подсказку, окну с файлом — в строку состояния.
    fn say(&mut self, text: String) {
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

    /// Заглушка: окно без файла.
    fn placeholder(&self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() / 3.0);
                ui.label(
                    egui::RichText::new("Перетащите файл датника в окно")
                        .size(22.0)
                        .color(TEXT),
                );
                ui.label(
                    egui::RichText::new("или выберите Файл → Открыть… (⌘O)")
                        .size(16.0)
                        .color(DIM),
                );
                if let Some(n) = &self.note {
                    ui.add_space(16.0);
                    ui.colored_label(ERROR, n);
                }
                let known = self.recent.existing();
                if !known.is_empty() {
                    ui.add_space(16.0);
                    ui.label(egui::RichText::new("Последние файлы — в меню Файл").color(DIM));
                }
            });
        });
    }

    /// Кадр окна без eframe — его зовут и тесты.
    pub fn draw(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.take_dropped(&ctx);
        match &mut self.app {
            Some(app) => app.draw(ui),
            None => self.placeholder(ui),
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
        // Запуск без файла и без последнего: диалог первого кадра.
        if !self.asked {
            self.asked = true;
            self.ask_file(ctx);
        }
        if let Some(app) = &mut self.app {
            app.logic(ctx, frame);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        if let Some(app) = &mut self.app {
            app.raw_input_hook(ctx, raw);
        }
    }
}
