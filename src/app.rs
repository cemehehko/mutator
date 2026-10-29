//! Запуск mutator: разбор аргументов и выбор режима — окно, терминал (`--tui`)
//! или печать (`--dump`, `--stat`, `--dump-code`, `--dump-seq`).
//!
//! Вместо файла — линк (`resolve://svc/ch`, `stream+…`): [`io::link::Link`]
//! пишет его в спул, вид открывается на спуле, когда линк стал Active; до того
//! терминал печатает смены состояния, окно — показывает их в заглушке
//! (`agr:link-state`). `--open` — параметры открытия, `--keep` — оставить спул.
//!
//! В библиотеке, а не в бинарнике: сборка с внешним бэкендом — свой бинарник,
//! который зовёт [`crate::io::register`] и затем [`main`].
//!
//! Наружу: [`main`].

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use crate::data::File;
use crate::desk::Desk;
use crate::dump::{self, Dump, Painter};
use crate::io;
use crate::lua::Filter;
use crate::range::Range;
use crate::ui;

const USAGE: &str = "\
mutator [FILE|LINK] [параметры] — просмотр датника или линка

  FILE                  без файла окно открывает последний или спрашивает файл;
                        для --tui, --dump и --stat файл обязателен;
                        несколько файлов одним видом: a.dat+b.dat — склейка в
                        указанном порядке, r.current.dat-3 — файл и ещё 3 более
                        старых той же ротации (номера в именах, current — последний)
  LINK                  канал вместо файла: resolve://SERVICE/CHANNEL, stream+…,
                        с префиксами (lua+…); только просмотр
  --url LINK            то же, что LINK, — явно (url канала целиком)
  --resolve HOST:PORT   resolve-сервер: для resolve:// без resolve.request и для
                        схем по хешу (scheme=sha256://…); или MUTATOR_RESOLVE
  --open PARAMS         параметры открытия линка: mode=online (по умолчанию),
                        mode=initial, mode=seq;seq=N, mode=block;block=N
  --keep DIR            спул линка оставить датником в DIR (по умолчанию —
                        временный, удаляется при выходе)

  --gui                 окно, не отсоединяясь от терминала (как --foreground)
  --tui                 терминал вместо окна (в сборке без GUI — всегда терминал)
  -s, --seq RANGE       диапазон как у tll-read: SEQ | BEGIN:END | +N | -N
  -f, --filter EXPR     Lua-выражение как у tll-read -f (seq, name, data, msgid, addr, time)
  --message NAME        только этот тип (можно несколько раз)
  --dump                напечатать сообщения YAML и выйти (сессия: фильтр, правки
                        и transform применяются, изменённые поля помечены)
  --with-size           в --dump печатать размер
  --once, --first       в --dump напечатать только первое сообщение выборки
  --color WHEN          цвет --dump: auto, always, never (по умолчанию auto)
  --stat                свести файл: границы seq, сколько сообщений, каких типов
  --dump-code           напечатать код Lua-фильтра и выйти
  --dump-seq            напечатать первый и последний seq и выйти
  --foreground, -F      окно не отсоединять от терминала (по умолчанию из терминала
                        окно уходит в фон, и приглашение возвращается сразу)
  --fresh               не читать сессию файла (настройки, фильтр, место, правки)
  --backend NAME        чем читать файл (бэкенд сборки; по умолчанию — по самому файлу)

Окружение:
  MUTATOR_LUA    — путь к модулю tll-lua (…/build/tll-lua)
  MUTATOR_LIBTLL — путь к libtll, если она не в системных каталогах";

#[derive(Default)]
struct Args {
    file: Option<PathBuf>,
    link: Option<String>,
    resolve: Option<String>,
    open: Option<String>,
    keep: Option<PathBuf>,
    tui: bool,
    range: Option<String>,
    filter: Filter,
    dump: bool,
    with_size: bool,
    once: bool,
    color: Color,
    stat: bool,
    dump_code: bool,
    dump_seq: bool,
    fresh: bool,
    foreground: bool,
    gui: bool,
    backend: Option<io::Backend>,
}

/// Краска `--dump`: auto — только если вывод в терминал.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
enum Color {
    #[default]
    Auto,
    Always,
    Never,
}

fn parse() -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(x) = it.next() {
        let mut val = |name: &str| it.next().ok_or(format!("{name}: нужно значение"));
        match x.as_str() {
            "-h" | "--help" => return Err(String::new()),
            "--tui" | "-tui" => a.tui = true,
            "-s" | "--seq" => a.range = Some(val(&x)?),
            "-f" | "--filter" => a.filter.expr = val(&x)?,
            "--message" => a.filter.messages.push(val(&x)?),
            "--dump" => a.dump = true,
            "--with-size" => a.with_size = true,
            "--once" | "--first" => a.once = true,
            "--color" => {
                a.color = match val(&x)?.as_str() {
                    "auto" => Color::Auto,
                    "always" => Color::Always,
                    "never" => Color::Never,
                    v => return Err(format!("--color: {v} (auto, always, never)")),
                }
            }
            "--stat" => a.stat = true,
            "--dump-code" => a.dump_code = true,
            "--dump-seq" => a.dump_seq = true,
            "--fresh" => a.fresh = true,
            "--foreground" | "-F" => a.foreground = true,
            "--gui" => a.gui = true,
            "--url" => a.link = Some(val(&x)?),
            "--resolve" => a.resolve = Some(val(&x)?),
            "--open" => a.open = Some(val(&x)?),
            "--keep" => a.keep = Some(PathBuf::from(val(&x)?)),
            "--backend" => {
                a.backend = Some(val(&x)?.parse().map_err(|e| format!("--backend: {e}"))?)
            }
            s if s.starts_with('-') => return Err(format!("неизвестный параметр {s}")),
            s if io::link::is_link(s) => a.link = Some(s.to_string()),
            s => a.file = Some(PathBuf::from(s.strip_prefix("file://").unwrap_or(s))),
        }
    }
    check(&a)?;
    Ok(a)
}

/// `--gui` — окно: с терминалом и печатью он не сочетается, а в сборке без GUI
/// окна нет.
fn check(a: &Args) -> Result<(), String> {
    if a.link.is_some() {
        if a.stat || a.dump_seq {
            return Err(
                "--stat и --dump-seq — для файла; линк откройте с --keep и сведите спул".into(),
            );
        }
        if a.range.is_some() {
            return Err("-s — для файла; у линка начало задаёт --open \"mode=seq;seq=N\"".into());
        }
    } else if a.open.is_some() || a.keep.is_some() || a.resolve.is_some() {
        return Err("--open, --keep и --resolve — только для линка".into());
    }
    if !a.gui {
        return Ok(());
    }
    if !cfg!(feature = "gui") {
        return Err(
            "--gui: сборка без GUI, окна нет (терминал — по умолчанию, --dump — печать)".into(),
        );
    }
    match (a.tui, a.dump, a.stat, a.dump_seq) {
        (false, false, false, false) => Ok(()),
        _ => Err("--gui не сочетается с --tui, --dump, --stat и --dump-seq".into()),
    }
}

/// Окно остаётся в терминале: просили `--foreground` или `--gui` (`agr:gui-detach`).
fn stays_in_terminal(a: &Args) -> bool {
    a.foreground || a.gui
}

/// Весь запуск: аргументы процесса, выбор режима, код выхода.
pub fn main() -> ExitCode {
    let a = match parse() {
        Ok(a) => a,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("{e}\n");
            }
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    if a.dump_code {
        println!("{}", a.filter.code().unwrap_or_default());
        return ExitCode::SUCCESS;
    }
    if let Some(r) = &a.resolve {
        // Линк читает адрес в своём потоке — из окружения (`tll::link::preload`).
        std::env::set_var("MUTATOR_RESOLVE", r);
    }
    // Спулы линков, брошенные упавшими процессами, и свои — при выходе.
    io::link::cleanup_stale();
    let _spools = Spools;
    let file = match a.file.clone() {
        Some(f) => Some(f),
        None if a.link.is_some() => None,
        // Окно без аргумента: последний открытый файл; если его нет — заглушка
        // с диалогом выбора (`agr:open-without-file`).
        None if windowed(&a) => crate::recent::Recent::load().last_existing(),
        None => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    // Окно из терминала уходит в фон: консоль не нужна (`agr:gui-detach`).
    // Спека жива, пока разворачивается — существование проверяет разбор.
    if file
        .as_ref()
        .is_none_or(|f| crate::spec::expand(&f.to_string_lossy()).is_ok())
        && detach(&a)
    {
        return ExitCode::SUCCESS;
    }
    if let Some(b) = a.backend {
        io::force(b);
    }
    io::quiet();
    match run(a, file) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mutator: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Запуск без файла допустим только для окна: ни терминал, ни печать файла не
/// просят (GUI-сборка; без GUI окна нет).
fn windowed(a: &Args) -> bool {
    cfg!(feature = "gui") && !(a.tui || a.dump || a.stat || a.dump_seq)
}

/// Нужно ли окну уйти из терминала в фон: окно (не `--tui`/`--dump`/`--stat`),
/// запущено из терминала и не просили остаться. Из `.app`, Finder или скрипта
/// терминала на вводе нет — отсоединять нечего. Снимок окна для проверок
/// (`MUTATOR_SHOT`) тоже ждёт на месте. `agr:gui-detach`
fn should_detach(windowed: bool, foreground: bool, shot: bool, tty: bool) -> bool {
    windowed && !foreground && !shot && tty
}

/// Отсоединить окно: тот же исполняемый файл с теми же аргументами, но в
/// своей группе процессов и без терминала на вводе-выводе — закрытие
/// терминала и Ctrl+C его не трогают, приглашение возвращается сразу.
/// `true` — потомок запущен и этому процессу пора выйти; `false` — остаёмся
/// в терминале (отсоединять не нужно или не получилось).
#[cfg(all(feature = "gui", unix))]
fn detach(a: &Args) -> bool {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    let tty = std::io::stdin().is_terminal() || std::io::stdout().is_terminal();
    let shot = std::env::var_os("MUTATOR_SHOT").is_some();
    if !should_detach(windowed(a), stays_in_terminal(a), shot, tty) {
        return false;
    }
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    std::process::Command::new(exe)
        .args(std::env::args_os().skip(1))
        .arg("--foreground")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .is_ok()
}

#[cfg(not(all(feature = "gui", unix)))]
fn detach(_a: &Args) -> bool {
    false
}

/// Временные спулы этого процесса удаляются при любом выходе из [`main`],
/// и при панике тоже (`agr:link-spool`).
struct Spools;

impl Drop for Spools {
    fn drop(&mut self) {
        io::link::cleanup();
    }
}

fn run(a: Args, file: Option<PathBuf>) -> Result<(), String> {
    if let Some(url) = a.link.clone() {
        return run_link(a, url);
    }
    let Some(file) = file else {
        #[cfg(feature = "gui")]
        {
            return ui::run(None, None, |_| {});
        }
        #[cfg(not(feature = "gui"))]
        {
            return Err("нужен файл".to_string());
        }
    };
    if a.dump_seq {
        let info = File::open_spec(&file.to_string_lossy())?.info;
        let s = |v: Option<i64>| v.map(|x| x.to_string()).unwrap_or_default();
        println!(
            "First seq: {}\nLast seq: {}",
            s(info.seq_begin),
            s(info.seq_last)
        );
        return Ok(());
    }
    if a.stat {
        let stdout = std::io::stdout();
        return dump::stat(&file, &mut stdout.lock());
    }
    let session = if a.fresh {
        None
    } else {
        crate::session::load(&file)
    };
    if a.dump {
        // Дамп показывает то же, что вид: правки и transform сессии применяются.
        let edits = session
            .as_ref()
            .map(|s| crate::edit::Edits::from_session(&s.edits, &s.stream))
            .unwrap_or_default();
        let transform = session.as_ref().and_then(|s| {
            let on = s.lua_transform_enabled.unwrap_or(true);
            s.lua_transform
                .clone()
                .filter(|c| on && !c.trim().is_empty())
        });
        let range = match &a.range {
            Some(r) => Range::parse(r)?,
            None => Range::all(),
        };
        // Как в окне: фильтр из командной строки заменяет фильтр сессии.
        let mut filter = a.filter.clone();
        if filter.is_empty() {
            if let Some(s) = session.as_ref() {
                filter = Filter {
                    messages: s.messages.clone(),
                    expr: s.expr.clone(),
                };
            }
        }
        let d = Dump {
            path: file,
            range,
            filter,
            with_size: a.with_size,
            once: a.once,
            edits,
            transform,
            backend: Default::default(),
        };
        let stdout = std::io::stdout();
        let color = match a.color {
            Color::Always => true,
            Color::Never => false,
            Color::Auto => stdout.is_terminal(),
        };
        let mut lock = stdout.lock();
        return if color {
            dump::run(&d, &mut Painter::new(&mut lock))
        } else {
            dump::run(&d, &mut lock)
        };
    }
    let file = File::open_spec(&file.to_string_lossy())?;
    let setup = setup(&a);
    // Без GUI терминал — единственный интерфейс, `--tui` не нужен.
    #[cfg(not(feature = "gui"))]
    {
        ui::tui::term::run(file, session, setup)
    }
    #[cfg(feature = "gui")]
    {
        if a.tui {
            ui::tui::term::run(file, session, setup)
        } else {
            ui::run(Some(file), session, setup)
        }
    }
}

/// Что вид делает сразу после открытия: фильтр и место из командной строки.
fn setup(a: &Args) -> impl FnOnce(&mut Desk) + 'static {
    let filter = a.filter.clone();
    let start = a.range.clone();
    move |d: &mut Desk| {
        if !filter.is_empty() {
            d.set_filter(filter);
        }
        if let Some(s) = start {
            let begin = s.split(':').next().unwrap_or("").to_string();
            if !begin.is_empty() {
                d.goto(&begin);
            }
        }
    }
}

/// Линк: окно ждёт его в заглушке, терминал и печать — здесь, печатая смены
/// состояния, пока спул не готов (`agr:link-state`).
fn run_link(a: Args, url: String) -> Result<(), String> {
    let open = a
        .open
        .clone()
        .unwrap_or_else(|| io::link::default_open(&url));
    #[cfg(feature = "gui")]
    if !a.tui && !a.dump {
        let setup = setup(&a);
        return ui::run_link(url, open, a.keep.clone(), a.fresh, setup);
    }
    let link = io::link::Link::start(&url, &open, a.backend, a.keep.as_deref(), Arc::new(|| {}))?;
    link.ready(|s| eprintln!("mutator: {}", s.line()))?;
    if a.dump {
        return dump_link(&a, &link);
    }
    let file = File::open_link(link)?;
    let session = if a.fresh {
        None
    } else {
        crate::session::load(&file.session_key())
    };
    let setup = setup(&a);
    ui::tui::term::run(file, session, setup)
}

/// `--dump` линка: печатать спул по мере прихода, пока линк жив; `--once` —
/// первое сообщение и выход.
fn dump_link(a: &Args, link: &io::link::Link) -> Result<(), String> {
    use crate::range::Border;
    let stdout = std::io::stdout();
    let color = match a.color {
        Color::Always => true,
        Color::Never => false,
        Color::Auto => stdout.is_terminal(),
    };
    let mut printed: Option<i64> = None;
    loop {
        let s = link.status();
        if let Some(last) = s.last.filter(|&l| printed.is_none_or(|p| l > p)) {
            let d = Dump {
                path: link.spool().to_path_buf(),
                range: Range {
                    begin: printed.map(|p| Border::Seq(p + 1)),
                    end: Some(Border::Seq(last)),
                    single: false,
                },
                filter: a.filter.clone(),
                with_size: a.with_size,
                once: a.once,
                edits: Default::default(),
                transform: None,
                backend: Default::default(),
            };
            let mut lock = stdout.lock();
            if color {
                dump::run(&d, &mut Painter::new(&mut lock))?;
            } else {
                dump::run(&d, &mut lock)?;
            }
            printed = Some(last);
            if a.once {
                return Ok(());
            }
        }
        if matches!(
            s.state,
            io::link::LinkState::Error | io::link::LinkState::Closed
        ) && s.last == printed
        {
            eprintln!("mutator: {}", s.line());
            return match s.state {
                io::link::LinkState::Error => Err(s.line()),
                _ => Ok(()),
            };
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::{check, should_detach, stays_in_terminal, Args};

    /// `agr:gui-detach` — окно из терминала отсоединяется.
    #[test]
    fn window_started_from_a_terminal_detaches() {
        assert!(should_detach(true, false, false, true));
    }

    /// `agr:gui-detach` — отсоединять нечего или нельзя: не окно, просили
    /// остаться, снимок для проверок, терминала на вводе нет (`.app`, скрипт).
    #[test]
    fn it_stays_attached_when_detaching_makes_no_sense() {
        assert!(
            !should_detach(false, false, false, true),
            "терминал и печать"
        );
        assert!(!should_detach(true, true, false, true), "--foreground");
        assert!(!should_detach(true, false, true, true), "снимок окна");
        assert!(
            !should_detach(true, false, false, false),
            "запуск не из терминала"
        );
    }

    /// `agr:gui-detach` — `--gui` оставляет окно в терминале, как `--foreground`.
    #[test]
    fn gui_flag_keeps_the_window_in_the_terminal() {
        let gui = Args {
            gui: true,
            ..Args::default()
        };
        assert!(stays_in_terminal(&gui));
        assert!(stays_in_terminal(&Args {
            foreground: true,
            ..Args::default()
        }));
        assert!(!stays_in_terminal(&Args::default()));
    }

    /// `--gui` не сочетается с терминалом и печатью.
    #[test]
    fn gui_flag_conflicts_with_terminal_and_dump_modes() {
        let with = |f: fn(&mut Args)| {
            let mut a = Args {
                gui: true,
                ..Args::default()
            };
            f(&mut a);
            check(&a)
        };
        assert!(with(|a| a.tui = true).is_err());
        assert!(with(|a| a.dump = true).is_err());
        assert!(with(|a| a.stat = true).is_err());
        assert_eq!(check(&Args::default()), Ok(()));
        // Само по себе — окно: в сборке с GUI можно, без GUI — понятная ошибка.
        assert_eq!(with(|_| {}).is_ok(), cfg!(feature = "gui"));
    }
}
