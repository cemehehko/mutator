//! Настоящий терминал: `mutator FILE --tui`. Кадр — та же сетка [`super::frame`],
//! вывод — только изменившиеся клетки (`out.rs`).
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
//! Наружу: [`run`].

mod keys;
mod out;

use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{self, Event, KeyEventKind, MouseButton, MouseEventKind};
use crossterm::{cursor, execute, terminal};

use crate::agent as agent_;
use crate::data::File;
use crate::desk::{Desk, Key};
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
    let log = quiet_stderr();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
    }));
    let mut desk = Desk::open(file, Arc::new(|| {}));
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
    let result = {
        let _raw = Raw::enter().map_err(|e| format!("терминал: {e}"))?;
        drive(&mut desk, cmd, events)
    };
    save(&desk);
    if let Some(p) = log {
        println!("Журнал: {}", p.display());
    }
    result
}

/// Сессия терминала — то же, что у окна, кроме вида и геометрии окна: их
/// оставляем от записанной окном.
fn save(d: &Desk) {
    let key = std::path::Path::new(&d.file.name);
    let mut s = d.session();
    if let Some(old) = session::load(key) {
        (s.look, s.window) = (old.look, old.window);
    }
    let _ = session::save(key, &s);
}

fn drive(
    d: &mut Desk,
    cmd: agent_::CmdTx,
    events: std::sync::mpsc::Receiver<agent_::Ev>,
) -> Result<(), String> {
    let mut screen = out::Screen::new(out::Screen::truecolor_from_env());
    let mut stdout = std::io::stdout();
    let io = |e: std::io::Error| format!("терминал: {e}");
    let mut edits = d.edits.clone();
    let mut last_saved = d.session();
    let mut checked = std::time::Instant::now();
    let mut agent = crate::ui::agent::State::new();
    let mut agent_look = false;
    let mut lua_look = false;
    loop {
        d.poll();
        agent.poll(&events);
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
            let f = crate::ui::agent::frame(&mut agent, cols, rows);
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
            let _ = cmd.send(agent_::Cmd::Shutdown);
            return Ok(());
        }
        if !event::poll(TICK).map_err(io)? {
            continue;
        }
        loop {
            match event::read().map_err(io)? {
                Event::Key(k) if k.kind != KeyEventKind::Release => {
                    if keys::is_quit(&k) {
                        let _ = cmd.send(agent_::Cmd::Shutdown);
                        return Ok(());
                    }
                    let Some(key) = keys::map(&k) else {
                        continue;
                    };
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
                        crate::ui::agent::key(&mut agent, &cmd, key);
                    } else {
                        d.key(key);
                    }
                }
                Event::Mouse(m) => match m.kind {
                    MouseEventKind::Down(MouseButton::Left) => match &frame {
                        Frame::Agent(g) => {
                            if let Some(a) = g.act_at(m.row as usize, m.column as usize) {
                                crate::ui::agent::apply(&mut agent, &cmd, a);
                            }
                        }
                        Frame::Desk(g) => {
                            if let Some(a) = g.act_at(m.row as usize, m.column as usize) {
                                super::apply(d, a);
                            }
                        }
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
