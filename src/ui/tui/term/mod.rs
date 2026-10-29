//! Настоящий терминал: `mutator FILE --tui`. Кадр — та же сетка [`super::frame`],
//! вывод — только изменившиеся клетки (`out.rs`).
//!
//! Без файла — список линков resolve ([`run_browse`], [`super::browse`]):
//! Enter ждёт выбранный линк на экране его состояния и открывает вид на спуле;
//! F5 из вида — снова список, Esc в нём — назад к виду (`agr:resolve-browse`).
//!
//! Журнал libtll и паники — в файл во временном каталоге: stderr лёг бы поверх
//! экрана (`agr:term-quiet`). Сырой режим, альтернативный экран и мышь
//! снимаются при любом выходе и при панике (`agr:term-restore`).
//!
//! Сессия пишется при выходе и при каждой смене правок — они живут в ней
//! (`agr:edits-in-session`). После фиксации правок открывается записанный
//! файл ([`crate::desk::Desk::reopen`]).
//!
//! Файлы: `out.rs` — вывод сетки, `keys.rs` — клавиши crossterm в [`crate::desk::Key`].
//!
//! Наружу: [`run`], [`run_browse`].

mod keys;
mod out;

use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{self, Event, KeyEventKind, MouseButton, MouseEventKind};
use crossterm::{cursor, execute, terminal};

use crate::agent as agent_;
use crate::browse::{Action, Browse};
use crate::data::File;
use crate::desk::{Desk, Key};
use crate::io::link::Link;
use crate::io::resolve::Browser;
use crate::session::{self, Session};
use crate::ui::lua_editor::tui_key;

/// Как часто проверять события воркера без ввода.
const TICK: Duration = Duration::from_millis(50);

/// Вернуть терминал в прежний вид.
fn restore() {
    let mut out = std::io::stdout();
    let _ = execute!(
        out,
        event::DisableMouseCapture,
        terminal::LeaveAlternateScreen,
        cursor::Show
    );
    let _ = terminal::disable_raw_mode();
}

/// Сырой режим, пока жив: `Drop` возвращает терминал и при `?`, и при панике.
struct Raw;

impl Raw {
    fn enter() -> std::io::Result<Raw> {
        terminal::enable_raw_mode()?;
        let raw = Raw;
        execute!(
            std::io::stdout(),
            terminal::EnterAlternateScreen,
            event::EnableMouseCapture,
            cursor::Hide
        )?;
        Ok(raw)
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        restore();
    }
}

extern "C" {
    fn dup2(from: i32, to: i32) -> i32;
}

/// stderr — в файл журнала: туда пишут libtll и паники.
fn quiet_stderr() -> Option<std::path::PathBuf> {
    use std::os::fd::AsRawFd;
    let path = std::env::temp_dir().join("mutator.log");
    let f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;
    // SAFETY: dup2 на открытый файл; старый stderr больше не нужен.
    (unsafe { dup2(f.as_raw_fd(), 2) } >= 0).then_some(path)
}

/// Запустить терминальный просмотр файла.
pub fn run(
    file: File,
    session: Option<Session>,
    setup: impl FnOnce(&mut Desk),
) -> Result<(), String> {
    let mut desk = Desk::open(file, Arc::new(|| {}));
    if let Some(s) = &session {
        desk.restore(s);
    }
    setup(&mut desk);
    screens(Some(Opened::new(desk)))
}

/// Запустить терминал со списком линков resolve (`agr:resolve-browse`).
pub fn run_browse() -> Result<(), String> {
    screens(None)
}

/// Открытый вид и его агент.
struct Opened {
    desk: Desk,
    cmd: agent_::CmdTx,
    events: std::sync::mpsc::Receiver<agent_::Ev>,
    agent: crate::ui::agent::State,
}

impl Opened {
    fn new(desk: Desk) -> Opened {
        let (cmd, events) = agent_::start(
            desk.file.clone(),
            desk.act_tx(),
            desk.file_gen(),
            agent_::LuaView {
                code: desk.lua_code.clone(),
                enabled: desk.lua_enabled,
            },
        );
        Opened {
            desk,
            cmd,
            events,
            agent: crate::ui::agent::State::new(),
        }
    }

    /// Вид закрывается: сессия — на диск, агенту — отбой.
    fn close(self) {
        save(&self.desk);
        let _ = self.cmd.send(agent_::Cmd::Shutdown);
    }
}

/// Чем кончился экран.
enum Exit {
    Quit,
    /// F5 из вида — к списку линков.
    Browse,
    /// Esc в списке — назад к виду.
    Back,
    /// Из списка открыт линк.
    Open(Box<File>),
    /// Форма `o` вида назвала другой линк: url и параметры.
    Link(String, String),
}

/// Экраны терминала: вид файла или линка и список линков resolve, между
/// ними — F5 и Esc; выбранный в списке линк заменяет вид.
fn screens(mut current: Option<Opened>) -> Result<(), String> {
    let log = quiet_stderr();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
    }));
    let mut recent = crate::recent::Recent::load();
    let mut list: Option<Browse> = None;
    // Линк, который ждём на экране списка: из формы `o` вида.
    let mut queued: Option<(String, String)> = None;
    let result = (|| {
        let _raw = Raw::enter().map_err(|e| format!("терминал: {e}"))?;
        let mut screen = out::Screen::new(out::Screen::truecolor_from_env());
        loop {
            let exit = match &mut current {
                Some(o) if list.as_ref().is_none_or(|b| !b.back) => drive(o, &mut screen)?,
                _ => {
                    let b = list.get_or_insert_with(|| browse_for(&recent));
                    b.back = current.is_some();
                    pick(b, &mut recent, &mut screen, queued.take())?
                }
            };
            screen.forget();
            match exit {
                Exit::Quit => return Ok(()),
                Exit::Browse => {
                    let b = list.get_or_insert_with(|| browse_for(&recent));
                    b.back = true;
                }
                Exit::Link(url, open) => {
                    let b = list.get_or_insert_with(|| browse_for(&recent));
                    b.back = true;
                    queued = Some((url, open));
                }
                Exit::Back => {
                    if let Some(b) = &mut list {
                        b.back = false;
                    }
                }
                Exit::Open(file) => {
                    if let Some(o) = current.take() {
                        o.close();
                    }
                    let session = session::load(&file.session_key());
                    let mut desk = Desk::open(*file, Arc::new(|| {}));
                    if let Some(s) = &session {
                        desk.restore(s);
                    }
                    current = Some(Opened::new(desk));
                    if let Some(b) = &mut list {
                        b.back = false;
                    }
                }
            }
        }
    })();
    if let Some(o) = current {
        o.close();
    }
    if let Some(p) = log {
        println!("Журнал: {}", p.display());
    }
    result
}

/// Список линков сервера из `--resolve`, окружения или `recent.toml`.
fn browse_for(recent: &crate::recent::Recent) -> Browse {
    let browser = recent
        .resolve_addr()
        .map(|a| Arc::new(Browser::start(&a, Arc::new(|| {}))));
    Browse::new(browser, Arc::new(|| {}))
}

/// Экран списка: до выбора линка, возврата к виду или выхода. Выбранный линк
/// ждём здесь же, рисуя его состояние, пока спул не готов.
fn pick(
    b: &mut Browse,
    recent: &mut crate::recent::Recent,
    screen: &mut out::Screen,
    start: Option<(String, String)>,
) -> Result<Exit, String> {
    let mut stdout = std::io::stdout();
    let io = |e: std::io::Error| format!("терминал: {e}");
    let mut pending: Option<Link> = None;
    let mut action = start.map(|(url, open)| Action::Open(url, open));
    loop {
        if let Some(Action::Open(url, open)) = action.take() {
            match Link::start(&url, &open, None, None, Arc::new(|| {})) {
                Ok(l) => pending = Some(l),
                Err(e) => b.note = Some(format!("{url}: {e}")),
            }
            screen.forget();
        }
        let (cols, rows) = terminal::size().map_err(io)?;
        let (cols, rows) = (cols as usize, rows as usize);
        if let Some(l) = &pending {
            if l.status().spool.is_some() {
                let l = pending.take().unwrap();
                let url = l.url().to_string();
                match File::open_link(l) {
                    Ok(f) => return Ok(Exit::Open(Box::new(f))),
                    Err(e) => b.note = Some(format!("{url}: {e}")),
                }
                continue;
            }
            let g = super::browse::waiting(&l.status(), cols, rows);
            screen.draw(&mut stdout, &g).map_err(io)?;
        } else {
            b.poll();
            let g = super::browse::frame(b, cols, rows);
            screen.draw(&mut stdout, &g).map_err(io)?;
        }
        stdout.flush().map_err(io)?;
        if !event::poll(TICK).map_err(io)? {
            continue;
        }
        let got = match event::read().map_err(io)? {
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                if keys::is_quit(&k) {
                    return Ok(Exit::Quit);
                }
                let Some(key) = keys::map(&k) else { continue };
                if pending.is_some() {
                    if key == Key::Esc {
                        pending = None;
                        screen.forget();
                    }
                    continue;
                }
                b.key(key)
            }
            Event::Mouse(m) if pending.is_none() => match m.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    let (cols, rows) = terminal::size().map_err(io)?;
                    let g = super::browse::frame(b, cols as usize, rows as usize);
                    match g.act_at(m.row as usize, m.column as usize) {
                        Some(a) => super::browse::apply(b, a),
                        None => None,
                    }
                }
                MouseEventKind::ScrollDown => {
                    b.scroll(3);
                    None
                }
                MouseEventKind::ScrollUp => {
                    b.scroll(-3);
                    None
                }
                _ => None,
            },
            Event::Resize(_, _) => {
                screen.forget();
                None
            }
            _ => None,
        };
        match got {
            Some(a @ Action::Open(..)) => action = Some(a),
            Some(Action::Back) => return Ok(Exit::Back),
            Some(Action::Quit) => return Ok(Exit::Quit),
            Some(Action::Connect(a)) => {
                recent.resolve = Some(a);
                let _ = recent.save();
            }
            None => {}
        }
    }
}

/// Сессия терминала — то же, что у окна, кроме вида и геометрии окна: их
/// оставляем от записанной окном.
fn save(d: &Desk) {
    let mut s = d.session();
    if let Some(old) = session::load(&d.file.session_key()) {
        (s.look, s.window) = (old.look, old.window);
    }
    let _ = session::save(&d.file.session_key(), &s);
}

/// Экран вида: до выхода или F5.
fn drive(o: &mut Opened, screen: &mut out::Screen) -> Result<Exit, String> {
    let Opened {
        desk: d,
        cmd,
        events,
        agent,
    } = o;
    let mut stdout = std::io::stdout();
    let io = |e: std::io::Error| format!("терминал: {e}");
    let mut edits = d.edits.clone();
    let mut last_saved = d.session();
    let mut checked = std::time::Instant::now();
    let mut agent_look = false;
    let mut lua_look = false;
    // Тянут границу детали: от нажатия на неё до отпускания.
    let mut splitting = false;
    loop {
        d.poll();
        agent.poll(events);
        if let Some(p) = d.reopen.take() {
            save(d);
            last_saved = d.session();
            if let Err(e) = d.reopen(&p) {
                d.status = format!("{}: {e}", p.display());
            }
            screen.forget();
        }
        if d.edits != edits {
            edits = d.edits.clone();
            save(d);
            last_saved = d.session();
        }
        if checked.elapsed() >= Duration::from_secs(1) {
            checked = std::time::Instant::now();
            let s = d.session();
            if s != last_saved {
                save(d);
                last_saved = s;
            }
        }
        let (cols, rows) = terminal::size().map_err(io)?;
        let (cols, rows) = (cols as usize, rows as usize);
        // F3 — экран агента: тот же приём, что в окне ([`crate::ui::App::draw`]).
        enum Frame {
            Desk(super::Grid<super::Act>),
            Agent(crate::ui::tui::Grid<crate::ui::agent::Act>),
            Lua(crate::ui::tui::Grid<usize>),
        }
        let frame = if agent_look {
            let f = crate::ui::agent::frame(agent, cols, rows);
            screen.draw(&mut stdout, &f).map_err(io)?;
            Frame::Agent(f)
        } else if lua_look {
            let f = crate::ui::lua_editor::tui(d, cols, rows);
            screen.draw(&mut stdout, &f).map_err(io)?;
            Frame::Lua(f)
        } else {
            let f = super::frame(d, cols, rows);
            screen.draw(&mut stdout, &f).map_err(io)?;
            Frame::Desk(f)
        };
        if d.quit {
            return Ok(Exit::Quit);
        }
        if let Some((url, open)) = d.open_link.take() {
            return Ok(Exit::Link(url, open));
        }
        if !event::poll(TICK).map_err(io)? {
            continue;
        }
        loop {
            match event::read().map_err(io)? {
                Event::Key(k) if k.kind != KeyEventKind::Release => {
                    if keys::is_quit(&k) {
                        return Ok(Exit::Quit);
                    }
                    let Some(key) = keys::map(&k) else {
                        continue;
                    };
                    if key == Key::Char('\u{F5}') {
                        // F5 — к списку линков; уходя с редактора Lua, применяем код.
                        if lua_look {
                            crate::ui::lua_editor::apply_code(d);
                        }
                        return Ok(Exit::Browse);
                    }
                    if key == crate::desk::Key::Char('\u{F3}') {
                        // Уходя с редактора Lua, применяем код.
                        if lua_look {
                            crate::ui::lua_editor::apply_code(d);
                        }
                        agent_look = !agent_look;
                        lua_look = false;
                    } else if key == crate::desk::Key::Char('\u{F4}') {
                        // F4 — редактор Lua transform: переключить; уход
                        // применяет код, при пустом коде — заготовка.
                        if lua_look {
                            crate::ui::lua_editor::apply_code(d);
                            lua_look = false;
                            agent_look = false;
                        } else {
                            if d.lua_code.is_empty() {
                                d.lua_code =
                                    crate::lua::transform_template_for(d.file.info.backend);
                            }
                            lua_look = true;
                            agent_look = false;
                        }
                    } else if lua_look {
                        if !crate::ui::lua_editor::tui_key(d, key) {
                            lua_look = false;
                        }
                    } else if agent_look {
                        crate::ui::agent::key(agent, cmd, key);
                    } else {
                        d.key(key);
                    }
                }
                Event::Mouse(m) => match m.kind {
                    MouseEventKind::Down(MouseButton::Left) => match &frame {
                        Frame::Agent(g) => {
                            if let Some(a) = g.act_at(m.row as usize, m.column as usize) {
                                crate::ui::agent::apply(agent, cmd, a);
                            }
                        }
                        Frame::Desk(g) => match g.act_at(m.row as usize, m.column as usize) {
                            Some(super::Act::Split) => splitting = true,
                            Some(a) => super::apply(d, a),
                            None => {}
                        },
                        Frame::Lua(g) => {
                            // Щелчок по коду — курсор в место тыка
                            // (слева — поле номеров строк).
                            if let Some(line) = g.act_at(m.row as usize, m.column as usize) {
                                let n = d.lua_code.split('\n').count();
                                let col = (m.column as usize)
                                    .saturating_sub(crate::ui::lua_editor::gutter(n));
                                d.lua_cursor =
                                    crate::ui::lua_editor::xy_cursor(&d.lua_code, line, col);
                                d.lua_reset_confirm = false;
                            }
                        }
                    },
                    MouseEventKind::Drag(MouseButton::Left) if splitting => {
                        super::split_at(d, rows, m.row as usize);
                    }
                    MouseEventKind::Up(MouseButton::Left) => splitting = false,
                    MouseEventKind::ScrollDown => {
                        if agent_look {
                            agent.scroll_by(-3);
                        } else if d.help.open {
                            d.help.scroll += 3;
                            d.help.keyboard = true;
                        } else if lua_look {
                            // В редакторе колесо ведёт курсор — вид следует за ним.
                            for _ in 0..3 {
                                tui_key(d, Key::Down);
                            }
                        } else {
                            d.scroll(3);
                        }
                    }
                    MouseEventKind::ScrollUp => {
                        if agent_look {
                            agent.scroll_by(3);
                        } else if d.help.open {
                            d.help.scroll = d.help.scroll.saturating_sub(3);
                            d.help.keyboard = true;
                        } else if lua_look {
                            for _ in 0..3 {
                                tui_key(d, Key::Up);
                            }
                        } else {
                            d.scroll(-3);
                        }
                    }
                    _ => {}
                },
                Event::Resize(_, _) => screen.forget(),
                _ => {}
            }
            if !event::poll(Duration::ZERO).map_err(io)? {
                break;
            }
        }
        stdout.flush().map_err(io)?;
    }
}
