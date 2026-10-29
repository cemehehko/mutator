//! Вид браузера: свой [`Desk`] на вкладку, в своём потоке, и его кадры
//! (`agr:serve-view-desk`, `agr:serve-view-frames`).
//!
//! Браузер — ещё один вид того же просмотрщика, как терминал: клавиши и
//! щелчки ([`Input`]) идут в [`Desk::key`] и [`crate::ui::tui::apply`], кадр —
//! та же сетка [`crate::ui::tui::frame`], что у `--tui`. Своего просмотра здесь
//! нет: только перевод ввода и сетки в JSON ([`encode`]). Кадр уходит в
//! `watch` — подписчику нужен последний, а не каждый; по сети идёт разница
//! строк ([`Frame::diff`]), как терминал пишет только изменившиеся клетки.
//!
//! На ввод кадр отвечает сразу, а сам по себе — не чаще [`FRAME`]: у живого
//! линка шапка меняется на каждом сообщении, и полный экран десятки раз в
//! секунду забивал туннель и браузер так, что клавиши тонули в очереди.
//!
//! Вид только смотрит ([`Desk::view_only`]); F2–F4 (вид, агент, Lua-редактор)
//! и выход `q` браузеру не нужны и до вида не доходят. Сессия файла не
//! читается и не пишется: у сервера своих сессий нет.
//!
//! Вид живёт, пока на его кадры подписаны; без подписчиков [`IDLE`] —
//! закрывается ([`Views::reap`]).
//!
//! Наружу: [`Views`], [`View`], [`Input`], [`Frame`], [`encode`], [`IDLE`], [`FRAME`].

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::watch;

use crate::data::File;
use crate::desk::{Desk, Key};
use crate::ui::tui::{self, grid::Grid, Act};

/// Сколько вид живёт без подписчиков кадров.
pub const IDLE: Duration = Duration::from_secs(30);
/// Как часто вид смотрит на события воркера без ввода — как терминал.
const TICK: Duration = Duration::from_millis(50);
/// Кадр без ввода — не чаще.
pub const FRAME: Duration = Duration::from_millis(100);

/// Ввод от браузера.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Input {
    /// Клавиша по имени: `Up`, `Enter`, `F1`…
    Key(String),
    /// Печатный знак.
    Char(char),
    /// Ctrl+буква.
    Ctrl(char),
    /// Щелчок по клетке: строка, столбец. По границе детали — начало
    /// перетаскивания её.
    Click(usize, usize),
    /// Мышь с нажатой кнопкой на клетке: тянет границу детали, если начали с неё.
    Drag(usize, usize),
    /// Кнопку отпустили.
    Release,
    /// Колесо: строк вниз (минус — вверх).
    Wheel(isize),
    /// Размер сетки: столбцов, строк.
    Size(usize, usize),
    /// Воркер вида что-то принёс — перерисовать; браузеру недоступно.
    #[serde(skip)]
    Wake,
}

impl Input {
    /// Клавиша вида; `None` — браузеру её не дают (F2–F4) или имя чужое.
    fn key(&self) -> Option<Key> {
        Some(match self {
            Input::Char(c) => Key::Char(*c),
            Input::Ctrl(c) => Key::Ctrl(*c),
            Input::Key(k) => match k.as_str() {
                "Enter" => Key::Enter,
                "Esc" => Key::Esc,
                "Tab" => Key::Tab,
                "BackTab" => Key::BackTab,
                "Backspace" => Key::Backspace,
                "Delete" => Key::Delete,
                "Up" => Key::Up,
                "Down" => Key::Down,
                "Left" => Key::Left,
                "Right" => Key::Right,
                "PageUp" => Key::PageUp,
                "PageDown" => Key::PageDown,
                "Home" => Key::Home,
                "End" => Key::End,
                // F1 — справка; терминал и окно шлют её непечатаемым знаком.
                "F1" => Key::Char('\u{F1}'),
                _ => return None,
            },
            _ => return None,
        })
    }
}

/// Кадр: размер и строки — каждая уже JSON-массивом отрезков одного цвета
/// `[текст, цвет, подложка|null]`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Frame {
    pub cols: usize,
    pub rows: usize,
    pub lines: Vec<String>,
    /// Строка границы детали — браузер ставит над ней курсор «тянуть».
    pub split: Option<usize>,
}

impl Frame {
    /// JSON для подписчика, который видел `prev`: только изменившиеся строки
    /// (`{"lines": {"3": …}}`); без `prev` или с другим размером — весь кадр
    /// (`"full": true`). `None` — показывать нечего: кадр тот же.
    pub fn diff(&self, prev: Option<&Frame>) -> Option<String> {
        // Вид ещё не нарисовал первый кадр.
        if self.cols == 0 {
            return None;
        }
        let same_size = prev.is_some_and(|p| p.cols == self.cols && p.rows == self.rows);
        if !same_size {
            return Some(format!(
                r#"{{"cols":{},"rows":{},"split":{},"full":true,"lines":[{}]}}"#,
                self.cols,
                self.rows,
                json!(self.split),
                self.lines.join(",")
            ));
        }
        let prev = prev?;
        let changed: Vec<String> = self
            .lines
            .iter()
            .enumerate()
            .filter(|(i, l)| prev.lines.get(*i) != Some(*l))
            .map(|(i, l)| format!(r#""{i}":{l}"#))
            .collect();
        (!changed.is_empty() || prev.split != self.split).then(|| {
            format!(
                r#"{{"cols":{},"rows":{},"split":{},"lines":{{{}}}}}"#,
                self.cols,
                self.rows,
                json!(self.split),
                changed.join(",")
            )
        })
    }
}

/// Сетка — кадр.
pub fn encode(g: &Grid<Act>) -> Frame {
    let hex = |c: egui::Color32| format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b());
    let run =
        |text: &str, fg, bg: egui::Color32| json!([text, hex(fg), (bg.a() > 0).then(|| hex(bg))]);
    let lines = (0..g.rows())
        .map(|r| {
            let mut runs: Vec<Value> = Vec::new();
            let mut text = String::new();
            let mut style: Option<(egui::Color32, egui::Color32)> = None;
            for c in 0..g.cols() {
                let Some(cell) = g.cell(r, c) else { continue };
                let s = (cell.fg, cell.bg);
                if let Some((fg, bg)) = style.filter(|x| *x != s) {
                    runs.push(run(&text, fg, bg));
                    text.clear();
                }
                style = Some(s);
                text.push(cell.ch);
            }
            if let Some((fg, bg)) = style {
                runs.push(run(&text, fg, bg));
            }
            Value::Array(runs).to_string()
        })
        .collect();
    let split = (0..g.rows()).find(|&r| g.act_at(r, 0).is_some_and(|a| matches!(a, Act::Split)));
    Frame {
        cols: g.cols(),
        rows: g.rows(),
        lines,
        split,
    }
}

/// Открытый вид: вход в его поток и последний кадр.
pub struct View {
    input: Mutex<mpsc::Sender<Input>>,
    frames: Arc<watch::Sender<Arc<Frame>>>,
    stop: Arc<AtomicBool>,
    /// С какого момента у вида нет подписчиков; `None` — есть.
    idle_since: Mutex<Option<Instant>>,
}

impl View {
    /// Передать ввод виду; `false` — вид уже закрыт.
    pub fn send(&self, i: Input) -> bool {
        self.input.lock().unwrap().send(i).is_ok()
    }

    /// Подписка на кадры: сразу — последний.
    pub fn frames(&self) -> watch::Receiver<Arc<Frame>> {
        self.frames.subscribe()
    }
}

impl Drop for View {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Открытые виды по id.
#[derive(Default)]
pub struct Views {
    open: Mutex<BTreeMap<String, Arc<View>>>,
    next: AtomicU64,
}

impl Views {
    /// Открыть вид на файле: свой поток с [`Desk`]; id — для ввода и кадров.
    pub fn open(&self, file: File) -> String {
        let id = format!("v{}", self.next.fetch_add(1, Ordering::Relaxed) + 1);
        let (tx, rx) = mpsc::channel();
        let frames = Arc::new(watch::channel(Arc::new(Frame::default())).0);
        let stop = Arc::new(AtomicBool::new(false));
        let wake = tx.clone();
        let (out, halt) = (frames.clone(), stop.clone());
        std::thread::spawn(move || drive(file, rx, wake, out, halt));
        let view = View {
            input: Mutex::new(tx),
            frames,
            stop,
            idle_since: Mutex::new(Some(Instant::now())),
        };
        self.open.lock().unwrap().insert(id.clone(), Arc::new(view));
        id
    }

    pub fn get(&self, id: &str) -> Option<Arc<View>> {
        self.open.lock().unwrap().get(id).cloned()
    }

    /// Закрыть вид; `false` — не был открыт.
    pub fn close(&self, id: &str) -> bool {
        self.open.lock().unwrap().remove(id).is_some()
    }

    /// Закрыть виды, у которых нет подписчиков дольше `idle`.
    pub fn reap(&self, idle: Duration) {
        let now = Instant::now();
        self.open.lock().unwrap().retain(|_, v| {
            let mut since = v.idle_since.lock().unwrap();
            if v.frames.receiver_count() > 0 {
                *since = None;
                return true;
            }
            let t = *since.get_or_insert(now);
            now.duration_since(t) < idle
        });
    }

    /// Сколько видов открыто.
    pub fn len(&self) -> usize {
        self.open.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Поток вида: ввод → [`Desk`], изменившийся кадр → `out`.
fn drive(
    file: File,
    rx: mpsc::Receiver<Input>,
    wake: mpsc::Sender<Input>,
    out: Arc<watch::Sender<Arc<Frame>>>,
    stop: Arc<AtomicBool>,
) {
    // Воркер будит вид пустым вводом.
    let notify = {
        let wake = Mutex::new(wake);
        Arc::new(move || {
            let _ = wake.lock().unwrap().send(Input::Wake);
        })
    };
    let (mut cols, mut rows) = (120usize, 40usize);
    let mut d = Desk::open(file, notify);
    d.view_only = true;
    let mut grid: Option<Grid<Act>> = None;
    let mut last = Frame::default();
    let mut drawn = Instant::now() - FRAME;
    let mut splitting = false;
    while !stop.load(Ordering::Relaxed) {
        let mut inputs = Vec::new();
        match rx.recv_timeout(TICK) {
            Ok(i) => inputs.push(i),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        inputs.extend(rx.try_iter());
        // Ввод человека — кадр сразу; воркер и таймер — не чаще [`FRAME`].
        let asked = inputs.iter().any(|i| *i != Input::Wake);
        for i in inputs {
            match i {
                Input::Size(c, r) => (cols, rows) = (c.clamp(20, 400), r.clamp(6, 200)),
                Input::Wake => {}
                Input::Click(r, c) => match grid.as_ref().and_then(|g| g.act_at(r, c)) {
                    Some(Act::Split) => splitting = true,
                    Some(a) => tui::apply(&mut d, a),
                    None => {}
                },
                Input::Drag(r, _) if splitting => tui::split_at(&mut d, rows, r),
                Input::Drag(..) => {}
                Input::Release => splitting = false,
                Input::Wheel(n) if d.help.open => {
                    d.help.scroll = d.help.scroll.saturating_add_signed(n);
                    d.help.keyboard = true;
                }
                Input::Wheel(n) => d.scroll(n),
                i => {
                    if let Some(k) = i.key() {
                        d.key(k);
                    }
                }
            }
        }
        d.poll();
        // Выходить из вида браузеру некуда: вкладку закрывают сами.
        d.quit = false;
        if !asked && drawn.elapsed() < FRAME {
            continue;
        }
        drawn = Instant::now();
        let g = tui::frame(&mut d, cols, rows);
        let frame = encode(&g);
        if frame != last {
            out.send_replace(Arc::new(frame.clone()));
            last = frame;
        }
        grid = Some(g);
    }
}

#[cfg(all(test, feature = "tll"))]
mod tests {
    use super::*;

    /// Клавиши браузера — клавиши вида; F2–F4 до вида не доходят.
    /// `agr:serve-view-desk`
    #[test]
    fn browser_keys_map_to_desk_keys() {
        assert_eq!(Input::Key("Down".into()).key(), Some(Key::Down));
        assert_eq!(Input::Key("F1".into()).key(), Some(Key::Char('\u{F1}')));
        assert_eq!(Input::Char('f').key(), Some(Key::Char('f')));
        assert_eq!(Input::Ctrl('x').key(), Some(Key::Ctrl('x')));
        for k in ["F2", "F3", "F4", "Nope"] {
            assert_eq!(Input::Key(k.into()).key(), None, "{k}");
        }
        let i: Input = serde_json::from_str(r#"{"click":[3,10]}"#).unwrap();
        assert_eq!(i, Input::Click(3, 10));
    }

    /// Вид без подписчиков кадров закрывается; с подписчиком — живёт.
    /// `agr:serve-view-desk`
    #[test]
    fn a_view_nobody_watches_is_closed() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/plain.dat");
        let views = Views::default();
        let watched = views.open(File::open(&path).unwrap());
        let _rx = views.get(&watched).unwrap().frames();
        let left = views.open(File::open(&path).unwrap());
        views.reap(Duration::ZERO);
        assert!(views.get(&watched).is_some(), "смотрят — живёт");
        assert!(views.get(&left).is_none(), "никто не смотрит — закрыт");
        assert_eq!(views.len(), 1);
    }

    /// Кадр — сетка отрезками одного цвета: текст строки собирается обратно;
    /// подписчику после первого кадра идут только изменившиеся строки.
    /// `agr:serve-view-frames`
    #[test]
    fn a_frame_keeps_the_grid_text_and_sends_only_changes() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/plain.dat");
        let mut d = Desk::open(File::open(&path).unwrap(), Arc::new(|| {}));
        for _ in 0..200 {
            d.poll();
            if d.idle() && !d.rows.is_empty() {
                break;
            }
            d.wait(Duration::from_millis(20));
        }
        let g = tui::frame(&mut d, 80, 20);
        let first = encode(&g);
        let full: Value = serde_json::from_str(&first.diff(None).unwrap()).unwrap();
        assert_eq!(
            (full["cols"].clone(), full["full"].clone()),
            (json!(80), json!(true))
        );
        for r in 0..g.rows() {
            let text: String = full["lines"][r]
                .as_array()
                .unwrap()
                .iter()
                .map(|run| run[0].as_str().unwrap().to_string())
                .collect();
            assert_eq!(text.trim_end(), g.text(r), "строка {r}");
        }
        assert_eq!(first.diff(Some(&first)), None, "тот же кадр — ничего");

        d.key(Key::Down);
        for _ in 0..50 {
            d.poll();
            d.wait(Duration::from_millis(10));
        }
        let next = encode(&tui::frame(&mut d, 80, 20));
        let patch: Value = serde_json::from_str(&next.diff(Some(&first)).unwrap()).unwrap();
        let changed = patch["lines"].as_object().unwrap();
        assert!(patch.get("full").is_none());
        assert!(
            !changed.is_empty() && changed.len() < g.rows(),
            "{changed:?}"
        );
    }
}
