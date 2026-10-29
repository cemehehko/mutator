//! Окно: обычный вид egui или TUI-сетка в окне (F2). Оба вида — над одним
//! [`Desk`] (`agr:tui-same-data`); окно перерисовывается по событию воркера, а
//! не по таймеру (`agr:repaint-on-event`).
//!
//! Слои как в black-d: детали (`table.rs` — таблица, `detail.rs` — дерево и
//! hex, `bar.rs` — панель фильтра/поиска/колонок, `commit.rs` — диалог
//! фиксации правок, `mass.rs` — подтверждение массовой правки,
//! `lua_editor.rs` — редактор Lua-кода transform-режима, `agent/` — состояние
//! экрана агента, `help.rs` — справка F1, `shell.rs` — оболочка окна: список
//! линков без файла, смена файла и перетаскивание, `browse.rs` — список линков
//! resolve в окне, `send.rs` — форма отправки в линк, `menu.rs` — родное меню macOS и
//! диалог выбора файла), подсистема `tui/`
//! (сетка,
//! кадр, терминал), а этот модуль только собирает страницу. Общие правила
//! раскладки колонок — `cols.rs`, цвета — `theme.rs`.
//!
//! Высота панели детали — доля высоты окна (`agr:detail-share`), одна с TUI и
//! сессией: панель окна следует за долей, пока границу не тянут, а тянет
//! пользователь — доля берётся из фактической высоты.
//!
//! Сессия файла ([`crate::session`]) возвращается при открытии и пишется раз
//! в секунду, если поменялась, и при выходе. После фиксации правок окно
//! открывает записанный файл ([`crate::desk::Desk::reopen`]), а сессия
//! прежнего файла пишется уже без правок.
//!
//! Линк окно открывает [`run_link`]: заглушка с его состоянием, пока спула
//! нет (`agr:link-state`).
//!
//! Наружу: [`run`], [`run_link`], [`Look`], [`App`], [`Shell`], [`cols`], [`theme`], [`tui`].

pub mod agent;
mod bar;
#[cfg(feature = "gui")]
mod browse;
pub mod cols;
mod commit;
mod detail;
mod help;
pub mod lua_editor;
mod mass;
#[cfg(feature = "gui")]
mod menu;
#[cfg(feature = "gui")]
mod send;
#[cfg(feature = "gui")]
mod shell;
mod table;
pub mod theme;
pub mod tui;

#[cfg(feature = "gui")]
pub use shell::Shell;

use std::sync::Arc;

use egui;

use crate::agent as agent_;
use crate::data::File;
use crate::desk::{Desk, Key};
use crate::session::{self, Session};
use theme::*;

/// Вид окна.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    Gui,
    Tui,
    Agent,
    /// Редактор Lua-кода для transform-режима (F4).
    LuaEditor,
}

/// Окно просмотрщика.
pub struct App {
    pub desk: Desk,
    pub look: Look,
    bar: bar::Bar,
    wheel: f32,
    shot: Option<Shot>,
    /// Tab, забранный у egui до кадра ([`App::filter_input`]).
    tabs: Vec<Key>,
    /// Контекст последнего кадра — для [`eframe::App::on_exit`].
    ctx: Option<egui::Context>,
    /// Последняя записанная сессия и когда проверяли.
    saved: Session,
    checked: std::time::Instant,
    /// Команды воркеру агента.
    cmd: agent_::CmdTx,
    /// События агента — вид опрашивает их каждый кадр.
    events: std::sync::mpsc::Receiver<agent_::Ev>,
    /// Состояние экрана агента ([`agent::State`]).
    pub agent: agent::State,
}

/// Снимок окна для проверки без человека: `MUTATOR_SHOT=файл.ppm`, клавиши
/// перед снимком — `MUTATOR_SHOT_KEYS` через запятую (`End,Enter,F2`, иначе —
/// текст по знакам). Окно дожидается индекса и тишины, снимает себя и закрывается.
struct Shot {
    path: std::path::PathBuf,
    keys: Vec<String>,
    frames: u32,
    asked: bool,
}

impl Shot {
    fn from_env() -> Option<Shot> {
        let path = std::env::var_os("MUTATOR_SHOT")?.into();
        let keys = std::env::var("MUTATOR_SHOT_KEYS")
            .map(|k| {
                k.split(',')
                    .map(|s| s.to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        Some(Shot {
            path,
            keys,
            frames: 0,
            asked: false,
        })
    }
}

fn save_ppm(path: &std::path::Path, img: &egui::ColorImage) -> std::io::Result<()> {
    let mut out = format!("P6\n{} {}\n255\n", img.size[0], img.size[1]).into_bytes();
    for p in &img.pixels {
        out.extend_from_slice(&[p.r(), p.g(), p.b()]);
    }
    std::fs::write(path, out)
}

/// Открыть окно. `file` — файл, с которым открыться; без него окно стартует
/// заглушкой (`agr:open-without-file`). `session` — сохранённая сессия файла
/// (или `None`), `setup` — параметры командной строки поверх неё.
#[cfg(feature = "gui")]
pub fn run(
    file: Option<File>,
    session: Option<Session>,
    setup: impl FnOnce(&mut Desk) + 'static,
) -> Result<(), String> {
    let title = match &file {
        Some(f) => format!("mutator — {}", f.name),
        None => "mutator".to_string(),
    };
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1400.0, 900.0])
        .with_title(&title);
    if let Some([x, y, w, h]) = session.as_ref().and_then(|s| s.window) {
        viewport = viewport
            .with_position([x, y])
            .with_inner_size([w.max(400.0), h.max(300.0)]);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        &title,
        options,
        Box::new(move |cc| {
            style(&cc.egui_ctx);
            let mut recent = crate::recent::Recent::load();
            let app = file.map(|f| {
                recent.push(std::path::Path::new(&f.name));
                let _ = recent.save();
                App::from_file(&cc.egui_ctx, f, session, setup)
            });
            let menu = menu::Menu::new(&cc.egui_ctx, &recent);
            Ok(Box::new(shell::Shell::new(app, recent).with_menu(menu)))
        }),
    )
    .map_err(|e| e.to_string())
}

/// Открыть окно на линке `url` с параметрами `open`: заглушка с его
/// состоянием, пока он не стал Active, потом — вид на спуле (`agr:link-state`).
#[cfg(feature = "gui")]
pub fn run_link(
    url: String,
    open: String,
    keep: Option<std::path::PathBuf>,
    fresh: bool,
    setup: impl FnOnce(&mut Desk) + 'static,
) -> Result<(), String> {
    let title = format!("mutator — {url}");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1400.0, 900.0])
            .with_title(&title),
        ..Default::default()
    };
    eframe::run_native(
        &title,
        options,
        Box::new(move |cc| {
            style(&cc.egui_ctx);
            let recent = crate::recent::Recent::load();
            let menu = menu::Menu::new(&cc.egui_ctx, &recent);
            let mut shell = shell::Shell::new(None, recent).with_menu(menu);
            shell.open_link(
                &cc.egui_ctx,
                &url,
                &open,
                keep,
                fresh,
                Some(Box::new(setup)),
            );
            Ok(Box::new(shell))
        }),
    )
    .map_err(|e| e.to_string())
}

#[cfg(not(feature = "gui"))]
pub fn run(
    _file: Option<File>,
    _session: Option<Session>,
    _setup: impl FnOnce(&mut Desk) + 'static,
) -> Result<(), String> {
    Err("сборка без GUI: используйте --tui или --dump".to_string())
}

fn style(ctx: &egui::Context) {
    // Только тёмная тема: иначе egui берёт системную, а цвета таблицы — тёмные.
    ctx.set_theme(egui::ThemePreference::Dark);
    // Стрелка «→» есть только в моноширинном Hack: пропорциональный Ubuntu
    // рисовал бы её квадратом. Hack запасным в пропорциональную семью — знак
    // берётся из него, а не пропадает.
    let mut fonts = egui::FontDefinitions::default();
    if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        family.push("Hack".to_owned());
    }
    ctx.set_fonts(fonts);
    ctx.style_mut_of(egui::Theme::Dark, |s| {
        s.visuals.panel_fill = BG;
        s.visuals.window_fill = PANEL;
        s.visuals.extreme_bg_color = PANEL;
        s.visuals.selection.bg_fill = SELECT;
    });
}

impl App {
    pub fn new(
        desk: Desk,
        cmd: agent_::CmdTx,
        events: std::sync::mpsc::Receiver<agent_::Ev>,
    ) -> App {
        let bar = bar::Bar::new(&desk);
        App {
            desk,
            look: Look::Gui,
            bar,
            wheel: 0.0,
            shot: Shot::from_env(),
            tabs: Vec::new(),
            ctx: None,
            saved: Session::default(),
            checked: std::time::Instant::now(),
            cmd,
            events,
            agent: agent::State::new(),
        }
    }

    /// Приложение для файла: свой `Desk` с сессией (`restore`), параметрами
    /// командной строки (`setup`) и своим агентом — его файл и диалоги
    /// привязаны к имени файла при старте, поэтому при смене файла приложение
    /// строится заново, а не переезжает.
    #[cfg(feature = "gui")]
    pub fn from_file(
        ctx: &egui::Context,
        file: File,
        session: Option<Session>,
        setup: impl FnOnce(&mut Desk),
    ) -> App {
        let repaint = ctx.clone();
        let mut desk = Desk::open(file, Arc::new(move || repaint.request_repaint()));
        if let Some(s) = &session {
            desk.restore(s);
        }
        setup(&mut desk);
        let (cmd, events) = agent_::start(
            desk.file.clone(),
            desk.act_tx(),
            desk.file_gen(),
            agent_::LuaView {
                code: desk.lua_code.clone(),
                enabled: desk.lua_enabled,
            },
        );
        let mut app = App::new(desk, cmd, events);
        if let Some(s) = &session {
            app.look = if s.look.as_deref() == Some("tui") {
                Look::Tui
            } else {
                Look::Gui
            };
        }
        app.saved = app.snapshot(ctx);
        app
    }

    /// Окно закрывает этот файл: агенту — отбой, сессия — на диск.
    #[cfg(feature = "gui")]
    fn finish(&self, ctx: &egui::Context) {
        let _ = self.cmd.send(agent_::Cmd::Shutdown);
        if self.shot.is_none() {
            let _ = session::save(&self.desk.file.session_key(), &self.snapshot(ctx));
        }
    }

    /// Клавиши окна в [`Key`]: всё, пока ни одно поле ввода не в фокусе.
    fn keys(&mut self, ui: &egui::Ui) -> Vec<Key> {
        let typing = ui.ctx().memory(|m| m.focused().is_some());
        let mut out = std::mem::take(&mut self.tabs);

        ui.input(|i| {
            for e in &i.events {
                match e {
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => {
                        if *key == egui::Key::F1 {
                            out.push(Key::Char('\u{F1}'));
                            continue;
                        }
                        if *key == egui::Key::F2 {
                            out.push(Key::Char('\u{F2}'));
                            continue;
                        }
                        if *key == egui::Key::F3 {
                            out.push(Key::Char('\u{F3}'));
                            continue;
                        }
                        if *key == egui::Key::F4 {
                            out.push(Key::Char('\u{F4}'));
                            continue;
                        }
                        if typing {
                            continue;
                        }
                        let k = match key {
                            // ⌘U — стереть поле формы `o`.
                            egui::Key::U if modifiers.command || modifiers.ctrl => Key::Ctrl('u'),
                            egui::Key::ArrowUp => Key::Up,
                            egui::Key::ArrowDown => Key::Down,
                            egui::Key::ArrowLeft => Key::Left,
                            egui::Key::ArrowRight => Key::Right,
                            egui::Key::PageUp => Key::PageUp,
                            egui::Key::PageDown => Key::PageDown,
                            egui::Key::Home => Key::Home,
                            egui::Key::End => Key::End,
                            egui::Key::Enter => Key::Enter,
                            egui::Key::Escape => Key::Esc,
                            egui::Key::Tab if modifiers.shift => Key::BackTab,
                            egui::Key::Tab => Key::Tab,
                            egui::Key::Backspace => Key::Backspace,
                            egui::Key::Delete => Key::Delete,
                            egui::Key::Insert => Key::Insert,
                            _ => continue,
                        };
                        out.push(k);
                    }
                    egui::Event::Text(t) if !typing => out.extend(t.chars().map(Key::Char)),
                    _ => {}
                }
            }
        });
        out
    }

    fn gui(&mut self, ui: &mut egui::Ui, keys: Vec<Key>) {
        for k in keys {
            // В окне строки ввода — свои поля egui: клавиши их только фокусируют.
            // Формы `o` и `s` — как строка ввода: буквы — им, а не панели.
            match (
                k,
                self.desk.prompt.is_some()
                    || self.desk.link_form.is_some()
                    || self.desk.send_form.is_some(),
            ) {
                (Key::Char('f'), false) => self.bar.focus = Some(bar::Field::Filter),
                (Key::Char('/'), false) | (Key::Char('?'), false) => {
                    self.bar.focus = Some(bar::Field::Search)
                }
                (Key::Char('g'), false) => self.bar.focus = Some(bar::Field::Goto),
                _ => self.desk.key(k),
            }
        }
        egui::Panel::top("bar").show(ui, |ui| {
            if self.bar.show(ui, &mut self.desk) {
                self.look = Look::Tui;
            }
        });
        egui::Panel::bottom("status").show(ui, |ui| status(ui, &self.desk));
        // `agr:detail-share`: высота детали — доля окна, а не то, что egui
        // запомнил в прошлый раз. Панель помнит свою высоту (`PanelState`) и,
        // раз помнит, `default_size` уже не смотрит: пустая деталь («нет
        // выбранной строки») запомнилась бы как минимум, и высота из сессии или
        // от `{`/`}` до панели бы не дошла. Поэтому пока границу не тянут —
        // запомненное сбрасываем и высоту задаёт `detail_ratio`; тянут —
        // наоборот, доля берётся из фактической высоты панели.
        let ctx = ui.ctx();
        let resize = egui::Id::new("detail").with("__resize");
        let dragging = ctx.is_being_dragged(resize) || ctx.drag_stopped_id() == Some(resize);
        if !dragging && egui::PanelState::load(ctx, egui::Id::new("detail")).is_some() {
            ctx.data_mut(|d| d.remove::<egui::PanelState>(egui::Id::new("detail")));
        }
        if self.desk.detail_open {
            let total = ui.available_height();
            let size = total * self.desk.detail_ratio;
            let r = egui::Panel::bottom("detail")
                .resizable(true)
                .default_size(size)
                .show(ui, |ui| {
                    // Панель меряется по содержимому: без этого пустая деталь
                    // схлопнула бы её до минимума, и egui запомнил бы эту
                    // высоту вместо заданной.
                    ui.take_available_space();
                    detail::show(ui, &mut self.desk);
                });
            if dragging && total > 0.0 {
                self.desk.detail_ratio = (r.response.rect.height() / total).clamp(0.05, 0.8);
            }
        }
        egui::CentralPanel::default()
            .show(ui, |ui| table::show(ui, &mut self.desk, &mut self.wheel));
        bar::picker(ui, &mut self.desk);
        detail::edit_line(ui, &mut self.desk);
        commit::show(ui, &mut self.desk);
        #[cfg(feature = "gui")]
        if let Some(f) = &self.desk.link_form {
            browse::form(ui, f);
        }
        #[cfg(feature = "gui")]
        if let Some(f) = &self.desk.send_form {
            send::show(ui, f);
        }
        mass::show(ui, &mut self.desk);
    }

    fn tui(&mut self, ui: &mut egui::Ui, keys: Vec<Key>) {
        for k in keys {
            self.desk.key(k);
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(BG))
            .show(ui, |ui| {
                let m = tui::grid::Metrics::new(ui.ctx(), 13.0);
                let size = ui.available_size();
                let cols = m.cols(size.x);
                let rows = ((size.y / m.cell.y).floor() as usize).max(6);
                let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                self.wheel += scroll / m.cell.y;
                let lines = self.wheel.trunc();
                if lines != 0.0 {
                    self.wheel -= lines;
                    if self.desk.help.open {
                        // Колесо над справкой листает справку (вниз — вперёд).
                        let h = &mut self.desk.help;
                        h.keyboard = true;
                        h.scroll = (h.scroll as isize - lines as isize).max(0) as usize;
                    } else {
                        self.desk.scroll(-lines as isize);
                    }
                }
                let grid = tui::frame(&mut self.desk, cols, rows);
                if let Some(a) = tui::grid::show(ui, &grid, &m, true) {
                    tui::apply(&mut self.desk, a);
                }
            });
    }

    /// Экран агента: диалог с моделью поверх того же `Desk`.
    fn agent_screen(&mut self, ui: &mut egui::Ui, _keys: Vec<Key>) {
        let cmd = self.cmd.clone();
        let state = &mut self.agent;
        agent::show(ui, state, &cmd);
    }

    /// Экран редактора Lua: GUI-редактор с подсветкой синтаксиса.
    fn lua_editor_screen(&mut self, ui: &mut egui::Ui, keys: Vec<Key>) {
        // Esc — применить код и назад.
        if keys.contains(&Key::Esc) {
            lua_editor::apply_code(&mut self.desk);
            self.look = Look::Gui;
            return;
        }
        lua_editor::gui(ui, &mut self.desk);
    }
}

fn status(ui: &mut egui::Ui, d: &Desk) {
    ui.horizontal(|ui| {
        if let Some((t, fg)) = tui::link_line(d) {
            ui.label(egui::RichText::new(t).monospace().color(fg));
        }
        ui.label(egui::RichText::new(tui::position(d)).monospace());
        if d.searching {
            ui.spinner();
        }
        if !d.status.is_empty() {
            ui.colored_label(if d.status.starts_with("не ") || d.status.contains(':') { ERROR } else { OK }, &d.status);
        } else {
            ui.label(egui::RichText::new("Enter — деталь · x hex · i типы полей · f фильтр · t типы · / ? поиск · n/N · g переход · c колонки · p пресет · e правка поля · E transform вкл/выкл · F4 код Lua · w зафиксировать · F2 — TUI").color(DIM));
        }
    });
}

impl App {
    fn shoot(&mut self, ui: &egui::Ui, keys: &mut Vec<Key>) {
        let Some(shot) = &mut self.shot else { return };
        let ctx = ui.ctx().clone();
        let image = ui.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = image {
            let _ = save_ppm(&shot.path, &img);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
        let busy = !self.desk.idle()
            || self.desk.index.is_none()
            || self.desk.matches.as_ref().is_some_and(|m| !m.is_done());
        if busy || shot.asked {
            return;
        }
        shot.frames += 1;
        if shot.frames < 5 {
            return;
        }
        if !shot.keys.is_empty() {
            let k = shot.keys.remove(0);
            shot.frames = 0;
            let named = match k.as_str() {
                "End" => Some(Key::End),
                "Home" => Some(Key::Home),
                "Enter" => Some(Key::Enter),
                "Down" => Some(Key::Down),
                "Up" => Some(Key::Up),
                "Tab" => Some(Key::Tab),
                "Esc" => Some(Key::Esc),
                "PageDown" => Some(Key::PageDown),
                "F2" => Some(Key::Char('\u{F2}')),
                "F3" => Some(Key::Char('\u{F3}')),
                _ => None,
            };
            match named {
                Some(k) => keys.push(k),
                None => keys.extend(k.chars().map(Key::Char)),
            }
            return;
        }
        shot.asked = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
    }
}

impl App {
    /// Сессия окна: [`Desk::session`] плюс вид, прямоугольник окна и высота детали.
    pub fn snapshot(&self, ctx: &egui::Context) -> Session {
        let mut s = self.desk.session();
        s.look = Some(if self.look == Look::Tui { "tui" } else { "gui" }.into());
        s.window = ctx.input(|i| {
            let v = i.viewport();
            match (v.outer_rect, v.inner_rect) {
                (Some(o), Some(r)) => Some([o.min.x, o.min.y, r.width(), r.height()]),
                _ => None,
            }
        });
        s
    }

    /// Правки записаны в новый файл: сессия прежнего — без правок, вид —
    /// на новый файл.
    fn reopen(&mut self, ctx: &egui::Context) {
        let Some(p) = self.desk.reopen.take() else {
            return;
        };
        if self.shot.is_none() {
            let _ = session::save(&self.desk.file.session_key(), &self.snapshot(ctx));
        }
        match self.desk.reopen(&p) {
            Ok(()) => {
                let title = format!("mutator — {}", p.display());
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
                self.bar = bar::Bar::new(&self.desk);
                self.saved = Session::default();
            }
            Err(e) => self.desk.status = format!("{}: {e}", p.display()),
        }
    }

    /// Записать сессию, если она поменялась (`agr:session-by-name`).
    fn persist(&mut self, ctx: &egui::Context) {
        if self.shot.is_some() {
            return;
        }
        let s = self.snapshot(ctx);
        if s != self.saved {
            if let Err(e) = session::save(&self.desk.file.session_key(), &s) {
                self.desk.status = format!("сессия: {e}");
            }
            self.saved = s;
        }
    }
}

#[cfg(feature = "gui")]
impl eframe::App for App {
    fn on_exit(&mut self) {
        match self.ctx.clone() {
            Some(ctx) => self.finish(&ctx),
            None => {
                let _ = self.cmd.send(agent_::Cmd::Shutdown);
            }
        }
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.ctx = Some(ctx.clone());
        self.desk.poll();
        let _ = self.cmd.send(agent_::Cmd::UpdateSnap {
            snap: self.desk.make_snap(),
        });
        self.reopen(ctx);
        if self.checked.elapsed() >= std::time::Duration::from_secs(1) {
            self.checked = std::time::Instant::now();
            self.persist(ctx);
        }
        if self.desk.quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        // Пока идут фоновые задачи, прогресс в заголовке должен двигаться.
        if !self.desk.idle()
            || self.desk.index.is_none()
            || self.desk.matches.as_ref().is_some_and(|m| !m.is_done())
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        self.filter_input(ctx, raw);
    }
}

impl App {
    /// Забрать Tab у egui, пока ни одно поле не в фокусе: egui обходит им поля
    /// ещё до кода кадра, а у нас Tab — фокус таблица/деталь.
    pub fn filter_input(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        if ctx.memory(|m| m.focused().is_some()) {
            return;
        }
        raw.events.retain(|e| match e {
            egui::Event::Key {
                key: egui::Key::Tab,
                pressed,
                modifiers,
                ..
            } => {
                if *pressed {
                    self.tabs.push(if modifiers.shift {
                        Key::BackTab
                    } else {
                        Key::Tab
                    });
                }
                false
            }
            _ => true,
        });
    }

    /// Кадр окна без eframe — его зовут и тесты.
    pub fn draw(&mut self, ui: &mut egui::Ui) {
        self.desk.poll();
        let _ = self.cmd.send(agent_::Cmd::UpdateSnap {
            snap: self.desk.make_snap(),
        });
        self.agent.poll(&self.events);
        let mut keys = self.keys(ui);
        self.shoot(ui, &mut keys);
        if keys.contains(&Key::Char('\u{F2}')) {
            self.look = if self.look == Look::Gui {
                Look::Tui
            } else {
                Look::Gui
            };
            keys.retain(|k| *k != Key::Char('\u{F2}'));
        }
        // F3 — экран агента: переключает с таблицы на диалог и обратно.
        if keys.contains(&Key::Char('\u{F3}')) {
            // Уходя с редактора Lua, применяем код.
            if self.look == Look::LuaEditor {
                lua_editor::apply_code(&mut self.desk);
            }
            self.look = if self.look == Look::Agent {
                Look::Gui
            } else {
                Look::Agent
            };
            keys.retain(|k| *k != Key::Char('\u{F3}'));
        }
        // F4 — редактор Lua transform: переключает с таблицы на редактор и
        // обратно; уход с редактора применяет код.
        if keys.contains(&Key::Char('\u{F4}')) {
            if self.look == Look::LuaEditor {
                lua_editor::apply_code(&mut self.desk);
                self.look = Look::Gui;
            } else {
                // Если transform-кода ещё нет — заготовка из шаблона.
                if self.desk.lua_code.is_empty() {
                    self.desk.lua_code =
                        crate::lua::transform_template_for(self.desk.file.info.backend);
                }
                self.look = Look::LuaEditor;
            }
            keys.retain(|k| *k != Key::Char('\u{F4}'));
        }
        // F1 — справка: поверх любого вида, глотает клавиши, пока открыта.
        if keys.contains(&Key::Char('\u{F1}')) {
            keys.retain(|k| *k != Key::Char('\u{F1}'));
            self.desk.help.open = !self.desk.help.open;
        }
        match self.look {
            Look::Gui => self.gui(ui, keys),
            Look::Tui => self.tui(ui, keys),
            Look::Agent => self.agent_screen(ui, keys),
            Look::LuaEditor => self.lua_editor_screen(ui, keys),
        }
        // В TUI-виде справку рисует сетка рамкой; модалка — над остальными.
        if self.desk.help.open && self.look != Look::Tui {
            help::show(ui, &mut self.desk);
        }
    }
}
