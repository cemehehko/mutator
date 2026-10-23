//! Точка входа: разбор аргументов и выбор режима — окно, терминал (`--tui`) или
//! печать (`--dump`, `--stat`, `--dump-code`, `--dump-seq`).

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use tui_patch::data::File;
use tui_patch::desk::Desk;
use tui_patch::dump::{self, Dump, Painter};
use tui_patch::lua::Filter;
use tui_patch::range::Range;
use tui_patch::tll::{self, Context, Open, Reader};
use tui_patch::ui;

const USAGE: &str = "\
mutator [FILE] [параметры] — просмотр датника TLL

  FILE                  без файла окно открывает последний или спрашивает файл;
                        для --tui, --dump и --stat файл обязателен

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

Окружение:
  MUTATOR_LUA   — путь к модулю tll-lua (…/build/tll-lua)
  MUTATOR_PATCH — путь к модулю tll-patch (…/build/src/tll-patch)";

#[derive(Default)]
struct Args {
    file: Option<PathBuf>,
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
            s if s.starts_with('-') => return Err(format!("неизвестный параметр {s}")),
            s => a.file = Some(PathBuf::from(s.strip_prefix("file://").unwrap_or(s))),
        }
    }
    check(&a)?;
    Ok(a)
}

/// `--gui` — окно: с терминалом и печатью он не сочетается, а в сборке без GUI
/// окна нет.
fn check(a: &Args) -> Result<(), String> {
    if !a.gui {
        return Ok(());
    }
    if !cfg!(feature = "gui") {
        return Err("--gui: сборка без GUI, окна нет (терминал — по умолчанию, --dump — печать)".into());
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

fn main() -> ExitCode {
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
    let file = match a.file.clone() {
        Some(f) => Some(f),
        // Окно без аргумента: последний открытый файл; если его нет — заглушка
        // с диалогом выбора (`agr:open-without-file`).
        None if windowed(&a) => tui_patch::recent::Recent::load().last_existing(),
        None => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    // Окно из терминала уходит в фон: консоль не нужна (`agr:gui-detach`).
    if file.as_ref().is_none_or(|f| f.exists()) && detach(&a) {
        return ExitCode::SUCCESS;
    }
    tll::quiet();
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

fn run(a: Args, file: Option<PathBuf>) -> Result<(), String> {
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
        let info = Reader::open(&Context::new(), &Open::file(&file))?.info();
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
        tui_patch::session::load(&file)
    };
    if a.dump {
        // Дамп показывает то же, что вид: правки и transform сессии применяются.
        let edits = session
            .as_ref()
            .map(|s| tui_patch::edit::Edits::from_saved(&s.edits))
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
    let file = File::open(&file)?;
    let filter = a.filter.clone();
    let start = a.range.clone();
    let setup = move |d: &mut Desk| {
        if !filter.is_empty() {
            d.set_filter(filter);
        }
        if let Some(s) = start {
            let begin = s.split(':').next().unwrap_or("").to_string();
            if !begin.is_empty() {
                d.goto(&begin);
            }
        }
    };
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
        assert!(!should_detach(false, false, false, true), "терминал и печать");
        assert!(!should_detach(true, true, false, true), "--foreground");
        assert!(!should_detach(true, false, true, true), "снимок окна");
        assert!(!should_detach(true, false, false, false), "запуск не из терминала");
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
