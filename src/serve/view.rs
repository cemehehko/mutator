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
//! Вид без файла — список линков resolve ([`Views::open_browse`],
//! `agr:resolve-browse`): та же сетка [`crate::ui::tui::browse`], что у
//! терминала. Enter открывает выбранный линк в том же виде ([`Opener`],
//! [`Start`]): пока он не Active, кадр — ожидание, Esc бросает его и остаётся
//! в списке. Уход на другой канал закрывает прежний в карте линков. F5 из
//! вида — снова список, Esc — назад.
//!
//! Наружу: [`Views`], [`View`], [`Input`], [`Frame`], [`Opener`], [`Start`],
//! [`encode`], [`IDLE`], [`FRAME`].

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::watch;

use crate::browse::{Action, Browse};
use crate::data::File;
use crate::desk::{Desk, Key};
use crate::io::link::Link;
use crate::io::resolve::Browser;
use crate::ui::tui::{self, grid::Grid, Act};

/// Как вид открывает линк, выбранный в списке или формой `o`: url и
/// параметры открытия → файл сразу или линк, который вид ждёт сам.
pub type Opener = Arc<dyn Fn(&str, &str) -> Result<Start, String> + Send + Sync>;

/// Чем кончилось начало открытия. [`Start::Link`] вид ждёт в своём цикле и
/// по Esc бросает; [`Start::File`] — уже готовый файл.
pub enum Start {
    /// Спул ещё не готов: вид рисует ожидание и бросает линк по Esc.
    Link(Link),
    /// Файл уже есть (тест или открытие без ожидания).
    File(File),
}

/// Убрать url из карты линков, когда вид с него ушёл.
type Release = Arc<dyn Fn(&str) + Send + Sync>;
/// Спул готов — файл и место в карте линков.
type Adopt = Arc<dyn Fn(Link) -> Result<File, String> + Send + Sync>;
/// Ctrl+A поднял или сменил список: отдать его REST и запомнить адрес.
type Share = Arc<dyn Fn(&str, Arc<Browser>) + Send + Sync>;

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
                // F1 — справка, F5 — список линков; терминал и окно шлют их
                // непечатаемыми знаками.
                "F1" => Key::Char('\u{F1}'),
                "F5" => Key::Char('\u{F5}'),
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
pub struct Views {
    open: Mutex<BTreeMap<String, Arc<View>>>,
    next: AtomicU64,
    /// Сервер списка линков, общий с REST: Ctrl+A меняет адрес на месте.
    slot: Arc<Mutex<Option<Arc<Browser>>>>,
    opener: Option<Opener>,
    adopt: Option<Adopt>,
    release: Option<Release>,
    share: Option<Share>,
}

impl Default for Views {
    fn default() -> Self {
        Views {
            open: Mutex::default(),
            next: AtomicU64::new(0),
            slot: Arc::new(Mutex::new(None)),
            opener: None,
            adopt: None,
            release: None,
            share: None,
        }
    }
}

impl Views {
    /// Виды со списком линков сервера `browser`; выбранный линк открывает `opener`.
    pub fn new(browser: Option<Arc<Browser>>, opener: Opener) -> Views {
        Views {
            slot: Arc::new(Mutex::new(browser)),
            opener: Some(opener),
            ..Default::default()
        }
    }

    /// Вкладка делит список с REST и закрывает прежний линк, уйдя на другой.
    pub(crate) fn bind(
        slot: Arc<Mutex<Option<Arc<Browser>>>>,
        opener: Opener,
        adopt: Adopt,
        release: Release,
        share: Share,
    ) -> Views {
        Views {
            slot,
            opener: Some(opener),
            adopt: Some(adopt),
            release: Some(release),
            share: Some(share),
            ..Default::default()
        }
    }

    /// Открыть вид на файле: свой поток с [`Desk`]; id — для ввода и кадров.
    pub fn open(&self, file: File) -> String {
        self.spawn(Some(file))
    }

    /// Открыть вид со списком линков resolve.
    pub fn open_browse(&self) -> String {
        self.spawn(None)
    }

    fn spawn(&self, file: Option<File>) -> String {
        let id = format!("v{}", self.next.fetch_add(1, Ordering::Relaxed) + 1);
        let (tx, rx) = mpsc::channel();
        let frames = Arc::new(watch::channel(Arc::new(Frame::default())).0);
        let stop = Arc::new(AtomicBool::new(false));
        let wake = tx.clone();
        let (out, halt) = (frames.clone(), stop.clone());
        let lists = Lists {
            slot: self.slot.clone(),
            opener: self.opener.clone(),
            adopt: self.adopt.clone(),
            release: self.release.clone(),
            share: self.share.clone(),
        };
        std::thread::spawn(move || drive(file, lists, rx, wake, out, halt));
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

/// Чем вид открывает список и линки из него.
struct Lists {
    slot: Arc<Mutex<Option<Arc<Browser>>>>,
    opener: Option<Opener>,
    adopt: Option<Adopt>,
    release: Option<Release>,
    share: Option<Share>,
}

/// Список из общего слота — свежий адрес, а не копия на старте вида.
fn fresh(lists: &Lists, notify: &Arc<dyn Fn() + Send + Sync>) -> Browse {
    let browser = lists.slot.lock().unwrap_or_else(|e| e.into_inner()).clone();
    Browse::new(browser, notify.clone())
}

/// Url, который вид держит в карте линков: смена канала и конец вида его закрывают.
struct Held {
    url: Option<String>,
    release: Option<Release>,
}

impl Held {
    fn switch(&mut self, url: &str) {
        if self.url.as_deref().is_some_and(|p| p != url) {
            if let Some(r) = &self.release {
                r(self.url.as_deref().unwrap());
            }
        }
        self.url = Some(url.to_string());
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        if let (Some(url), Some(r)) = (self.url.take(), self.release.take()) {
            r(&url);
        }
    }
}

/// Вид браузера на файле: только смотрит.
fn desk_of(file: File, notify: &Arc<dyn Fn() + Send + Sync>) -> Desk {
    let mut d = Desk::open(file, notify.clone());
    d.view_only = true;
    d
}

/// Поток вида: ввод → [`Desk`] или список, изменившийся кадр → `out`.
fn drive(
    file: Option<File>,
    lists: Lists,
    rx: mpsc::Receiver<Input>,
    wake: mpsc::Sender<Input>,
    out: Arc<watch::Sender<Arc<Frame>>>,
    stop: Arc<AtomicBool>,
) {
    // Воркер будит вид пустым вводом.
    let notify: Arc<dyn Fn() + Send + Sync> = {
        let wake = Mutex::new(wake);
        Arc::new(move || {
            let _ = wake.lock().unwrap().send(Input::Wake);
        })
    };
    let (mut cols, mut rows) = (120usize, 40usize);
    let mut desk = file.map(|f| desk_of(f, &notify));
    let mut list: Option<Browse> = None;
    let mut listing = desk.is_none();
    // Линк ещё не Active: Esc бросает его, клавиши до вида не доходят.
    let mut pending: Option<Link> = None;
    let mut held = Held {
        url: None,
        release: lists.release.clone(),
    };
    let mut grid: Option<Grid<Act>> = None;
    let mut last = Frame::default();
    let mut drawn = Instant::now() - FRAME;
    let mut splitting = false;
    let publish = |g: &Grid<Act>, last: &mut Frame| {
        let frame = encode(g);
        if frame != *last {
            out.send_replace(Arc::new(frame.clone()));
            *last = frame;
        }
    };
    let complain = |desk: &mut Option<Desk>,
                    list: &mut Option<Browse>,
                    listing: bool,
                    url: &str,
                    err: &str| {
        let msg = format!("{url}: {err}");
        match desk.as_mut().filter(|_| !listing) {
            Some(d) => d.status = msg,
            None => {
                list.get_or_insert_with(|| fresh(&lists, &notify)).note = Some(msg);
            }
        }
    };
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
        for i in &inputs {
            if let Input::Size(c, r) = i {
                (cols, rows) = ((*c).clamp(20, 400), (*r).clamp(6, 200));
            }
        }
        // Пока линк открывается, Esc бросает его. Остальной ввод не копится:
        // после успеха он попал бы уже в новый вид.
        if pending.is_some() {
            if inputs.iter().any(|i| i.key() == Some(Key::Esc)) {
                pending = None;
            }
        } else {
            let mut want: Option<(String, String)> = None;
            for i in inputs {
                if matches!(i, Input::Size(..)) {
                    continue;
                }
                if listing || desk.is_none() {
                    let b = list.get_or_insert_with(|| fresh(&lists, &notify));
                    b.back = desk.is_some();
                    let action = match i {
                        Input::Click(r, c) => grid
                            .as_ref()
                            .and_then(|g| g.act_at(r, c))
                            .and_then(|a| tui::browse::apply(b, a)),
                        Input::Wheel(n) => {
                            b.scroll(n);
                            None
                        }
                        i => i.key().and_then(|k| b.key(k)),
                    };
                    match action {
                        Some(Action::Open(url, open)) => want = Some((url, open)),
                        Some(Action::Back) => listing = false,
                        Some(Action::Connect(a)) => {
                            if let Some(share) = &lists.share {
                                if let Some(br) = b.browser().cloned() {
                                    share(&a, br);
                                }
                            }
                        }
                        Some(Action::Quit) | None => {}
                    }
                    continue;
                }
                let Some(d) = desk.as_mut() else { continue };
                match i {
                    Input::Wake | Input::Size(..) => {}
                    Input::Click(r, c) => match grid.as_ref().and_then(|g| g.act_at(r, c)) {
                        Some(Act::Split) => splitting = true,
                        Some(a) => tui::apply(d, a),
                        None => {}
                    },
                    Input::Drag(r, _) if splitting => tui::split_at(d, rows, r),
                    Input::Drag(..) => {}
                    Input::Release => splitting = false,
                    Input::Wheel(n) if d.help.open => {
                        d.help.scroll = d.help.scroll.saturating_add_signed(n);
                        d.help.keyboard = true;
                    }
                    Input::Wheel(n) => d.scroll(n),
                    i => match i.key() {
                        Some(Key::Char('\u{F5}')) => listing = true,
                        Some(k) => d.key(k),
                        None => {}
                    },
                }
                if let Some(l) = d.open_link.take() {
                    want = Some(l);
                }
            }
            if let Some((url, open)) = want {
                match lists.opener.as_ref().map(|f| f(&url, &open)) {
                    Some(Ok(Start::File(file))) => {
                        held.switch(&url);
                        desk = Some(desk_of(file, &notify));
                        listing = false;
                    }
                    Some(Ok(Start::Link(link))) => pending = Some(link),
                    Some(Err(e)) => complain(&mut desk, &mut list, listing, &url, &e),
                    None => complain(
                        &mut desk,
                        &mut list,
                        listing,
                        &url,
                        "этот сервер линки не открывает",
                    ),
                }
            }
        }
        let ready = pending.as_ref().is_some_and(|l| l.status().spool.is_some());
        if ready {
            let link = pending.take().unwrap();
            let url = link.url().to_string();
            let opened = match &lists.adopt {
                Some(f) => f(link),
                None => File::open_link(link),
            };
            match opened {
                Ok(file) => {
                    held.switch(&url);
                    desk = Some(desk_of(file, &notify));
                    listing = false;
                }
                Err(e) => complain(&mut desk, &mut list, listing, &url, &e),
            }
        } else if let Some(st) = pending.as_ref().map(|l| l.status()) {
            publish(&tui::browse::waiting(&st, cols, rows), &mut last);
            continue;
        }
        if let Some(d) = desk.as_mut() {
            d.poll();
            // Выходить из вида браузеру некуда: вкладку закрывают сами.
            d.quit = false;
        }
        if let Some(b) = list.as_mut() {
            b.poll();
        }
        if !asked && drawn.elapsed() < FRAME {
            continue;
        }
        drawn = Instant::now();
        let g = match desk.as_mut() {
            Some(d) if !listing => tui::frame(d, cols, rows),
            _ => {
                let b = list.get_or_insert_with(|| fresh(&lists, &notify));
                b.back = desk.is_some();
                tui::browse::frame(b, cols, rows)
            }
        };
        publish(&g, &mut last);
        grid = Some(g);
    }
}

#[cfg(all(test, feature = "tll"))]
mod tests {
    use super::*;
    use crate::io::link::LinkState;

    /// Клавиши браузера — клавиши вида; F2–F4 до вида не доходят.
    /// `agr:serve-view-desk`
    #[test]
    fn browser_keys_map_to_desk_keys() {
        assert_eq!(Input::Key("Down".into()).key(), Some(Key::Down));
        assert_eq!(Input::Key("F1".into()).key(), Some(Key::Char('\u{F1}')));
        assert_eq!(Input::Char('f').key(), Some(Key::Char('f')));
        assert_eq!(Input::Ctrl('x').key(), Some(Key::Ctrl('x')));
        assert_eq!(Input::Key("F5".into()).key(), Some(Key::Char('\u{F5}')));
        for k in ["F2", "F3", "F4", "Nope"] {
            assert_eq!(Input::Key(k.into()).key(), None, "{k}");
        }
        let i: Input = serde_json::from_str(r#"{"click":[3,10]}"#).unwrap();
        assert_eq!(i, Input::Click(3, 10));
    }

    /// Вид без файла — список линков: кадр списка, Enter открывает выбранный
    /// линк в том же виде, F5 — снова список, Esc — назад к виду.
    /// `agr:resolve-browse`
    #[test]
    fn a_view_without_file_lists_links_and_opens_one() {
        use crate::io::resolve::{BrowseEvent, Export, Snapshot};
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/plain.dat");
        let mut snap = Snapshot {
            addr: "h:1".into(),
            state: LinkState::Active,
            ..Default::default()
        };
        snap.apply(BrowseEvent::Export(Export {
            service: "gw".into(),
            channel: "orders".into(),
            ..Default::default()
        }));
        let asked = Arc::new(Mutex::new(Vec::new()));
        let seen = asked.clone();
        let views = Views::new(
            Some(Arc::new(Browser::fixed(snap))),
            Arc::new(move |url: &str, open: &str| {
                seen.lock().unwrap().push(format!("{url} {open}"));
                File::open(&path).map(Start::File)
            }),
        );
        let id = views.open_browse();
        let v = views.get(&id).unwrap();
        let mut rx = v.frames();
        let text = |rx: &mut watch::Receiver<Arc<Frame>>, needle: &str| {
            let until = Instant::now() + Duration::from_secs(10);
            loop {
                let f = rx.borrow_and_update().clone();
                let all = f.lines.join("\n");
                if all.contains(needle) {
                    return all;
                }
                assert!(Instant::now() < until, "нет «{needle}»: {all}");
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        v.send(Input::Size(100, 20));
        text(&mut rx, "линки resolve");
        text(&mut rx, "gw");
        v.send(Input::Key("Enter".into()));
        text(&mut rx, "plain.dat");
        assert_eq!(*asked.lock().unwrap(), ["resolve://gw/orders mode=online"]);
        v.send(Input::Key("F5".into()));
        text(&mut rx, "линки resolve");
        v.send(Input::Key("Esc".into()));
        text(&mut rx, "plain.dat");
    }

    fn wait_frame(rx: &mut watch::Receiver<Arc<Frame>>, needle: &str) -> String {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let all = rx.borrow_and_update().lines.join("\n");
            if all.contains(needle) {
                return all;
            }
            assert!(Instant::now() < until, "нет «{needle}»: {all}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// `agr:link-state` — пока линк открывается, Esc бросает его и возвращает
    /// список; клавиша не достаётся виду, которого ещё нет.
    #[test]
    fn esc_while_a_link_opens_returns_to_the_list() {
        use crate::io::resolve::{BrowseEvent, Export, Snapshot};
        let mut snap = Snapshot {
            addr: "h:1".into(),
            state: LinkState::Active,
            ..Default::default()
        };
        snap.apply(BrowseEvent::Export(Export {
            service: "gw".into(),
            channel: "orders".into(),
            ..Default::default()
        }));
        let views = Views::new(
            Some(Arc::new(Browser::fixed(snap))),
            Arc::new(|url: &str, open: &str| {
                Link::start(url, open, None, None, Arc::new(|| {})).map(Start::Link)
            }),
        );
        let v = views.get(&views.open_browse()).unwrap();
        let mut rx = v.frames();
        v.send(Input::Size(80, 16));
        wait_frame(&mut rx, "линки resolve");
        v.send(Input::Key("Enter".into()));
        wait_frame(&mut rx, "Esc — к списку");
        v.send(Input::Key("Esc".into()));
        let back = wait_frame(&mut rx, "линки resolve");
        assert!(
            !back.contains("открываю линк"),
            "Esc остался в списке: {back}"
        );
    }

    /// `agr:serve-link-handle` — вид, открыв другой канал, закрывает прежний.
    #[test]
    fn opening_another_channel_closes_the_previous() {
        use crate::io::resolve::{BrowseEvent, Export, Snapshot};
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/plain.dat");
        let mut snap = Snapshot {
            addr: "h:1".into(),
            state: LinkState::Active,
            ..Default::default()
        };
        for ch in ["orders", "ticks"] {
            snap.apply(BrowseEvent::Export(Export {
                service: "gw".into(),
                channel: ch.into(),
                ..Default::default()
            }));
        }
        let released = Arc::new(Mutex::new(Vec::new()));
        let rec = released.clone();
        let mut views = Views::new(
            Some(Arc::new(Browser::fixed(snap))),
            Arc::new(move |_url: &str, _open: &str| File::open(&path).map(Start::File)),
        );
        views.release = Some(Arc::new(move |url: &str| {
            rec.lock().unwrap().push(url.to_string());
        }));
        let v = views.get(&views.open_browse()).unwrap();
        let mut rx = v.frames();
        v.send(Input::Size(80, 16));
        wait_frame(&mut rx, "gw");
        v.send(Input::Key("Enter".into()));
        wait_frame(&mut rx, "plain.dat");
        assert!(
            released.lock().unwrap().is_empty(),
            "первый канал некого закрывать"
        );
        v.send(Input::Key("F5".into()));
        wait_frame(&mut rx, "линки resolve");
        v.send(Input::Key("Down".into()));
        v.send(Input::Key("Enter".into()));
        wait_frame(&mut rx, "plain.dat");
        assert_eq!(
            released.lock().unwrap().as_slice(),
            ["resolve://gw/orders".to_string()]
        );
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
