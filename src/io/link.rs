//! Линк — не файл, а канал (`resolve://svc/ch`, `stream+…`, `lua+…`): поток
//! приходит по сети, и открыть его заново «с любого seq» дорого или нельзя
//! вовсе. Поэтому линк читается один раз и пишется в спул — обычный датник,
//! а всё остальное (окно строк, индекс, фильтр, поиск, деталь) читает спул как
//! растущий файл (`agr:link-spool`).
//!
//! [`Link::start`] заводит поток записи: он открывает url бэкендом
//! ([`DriverLink`]), на первом Active — когда известна схема (у `resolve://` она
//! приходит с сервера) — создаёт спул и дальше дописывает в него каждое
//! сообщение. Состояние канала, ошибка журнала, есть ли у линка история и
//! сколько записано — в [`Status`] (`agr:link-state`); вид будит `notify`.
//!
//! Спул по умолчанию временный: `$TMPDIR/mutator-<pid>/`, удаляется вместе с
//! [`Link`] и при выходе; каталоги процессов, которых уже нет, чистит
//! следующий запуск ([`cleanup`], [`cleanup_stale`]). С `--keep DIR` спул
//! остаётся датником в `DIR` под именем из url ([`sanitize`]).
//!
//! Наружу: [`Link`], [`Spool`], [`Status`], [`LinkState`], [`LinkEvent`], [`DriverLink`],
//! [`is_link`], [`default_open`].

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::{drivers, Backend, Context, Info, Msg, Writer};

/// Параметры открытия по умолчанию — у бэкенда, по url: онлайн, история — по
/// запросу (`--open`, `o`); у канала без истории параметров нет вовсе — свой
/// `mode` есть, например, у `lua+`, и чужой он отвергнет (`agr:link-open`).
pub fn default_open(url: &str) -> String {
    drivers()
        .into_iter()
        .find(|d| d.link(url))
        .map(|d| d.link_open(url))
        .unwrap_or_default()
}

/// Состояние канала линка — как у libtll.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LinkState {
    #[default]
    Opening,
    Active,
    Error,
    Closing,
    Closed,
}

impl LinkState {
    pub fn name(self) -> &'static str {
        match self {
            LinkState::Opening => "Opening",
            LinkState::Active => "Active",
            LinkState::Error => "Error",
            LinkState::Closing => "Closing",
            LinkState::Closed => "Closed",
        }
    }
}

/// Что отдаёт канал линка бэкенда.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkEvent {
    State(LinkState),
    Data(Msg),
    /// История догружена, дальше онлайн; seq — последний на сервере.
    Online(i64),
    /// Ошибка журнала бэкенда — текст для человека.
    Error(String),
}

/// Канал линка бэкенда.
pub trait DriverLink: Send {
    /// Обработать канал не дольше `timeout`; накопившиеся события.
    fn poll(&mut self, timeout: Duration) -> Vec<LinkEvent>;
    /// Схема данных строкой url — для спула; есть после Active.
    fn scheme_url(&self) -> Option<String>;
    /// Умеет ли канал отдавать историю (stream-клиент); известно после Active.
    fn history(&self) -> bool;
}

/// Линк ли это: url с протоколом, который узнал бэкенд, а не `file://` и не путь.
pub fn is_link(s: &str) -> bool {
    drivers().iter().any(|d| d.link(s))
}

/// Состояние линка для вида.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub url: String,
    /// Параметры открытия (`mode=…`).
    pub open: String,
    pub state: LinkState,
    /// Последняя ошибка: канала, журнала или записи спула.
    pub error: Option<String>,
    /// Есть ли история; `None` — ещё не Active.
    pub history: Option<bool>,
    /// История догружена, идёт онлайн.
    pub online: bool,
    /// Записано сообщений и последний seq.
    pub count: u64,
    pub last: Option<i64>,
    /// Спул готов: путь датника.
    pub spool: Option<PathBuf>,
    /// Спул останется после выхода.
    pub kept: bool,
}

impl Status {
    /// Строка для статуса: `resolve://a/b · Active · онлайн · 1234`.
    pub fn line(&self) -> String {
        let mut s = format!("{} · {}", self.url, self.state.name());
        match (self.history, self.online) {
            (_, true) => s.push_str(" · онлайн"),
            (Some(true), false) => s.push_str(" · история"),
            _ => {}
        }
        s.push_str(&format!(" · {}", self.count));
        if let Some(e) = &self.error {
            s.push_str(" · ");
            s.push_str(e.lines().next().unwrap_or(""));
        }
        s
    }
}

/// Спул: датник линка. Временный удаляется с последним владельцем — линком
/// или его переподключением ([`Link::resume`]); с `--keep` остаётся.
#[derive(Debug)]
pub struct Spool {
    pub path: PathBuf,
    pub kept: bool,
}

impl Drop for Spool {
    fn drop(&mut self) {
        if !self.kept {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Линк и его запись в спул.
pub struct Link {
    url: String,
    backend: Backend,
    keep: Option<PathBuf>,
    spool: Arc<Spool>,
    status: Arc<Mutex<Status>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

type Notify = Arc<dyn Fn() + Send + Sync>;

static SPOOLS: AtomicU64 = AtomicU64::new(0);

/// Сколько ждать канал за один проход: столько же — реакция на остановку.
const POLL: Duration = Duration::from_millis(50);
/// Будить вид не чаще: данные идут потоком.
const WAKE: Duration = Duration::from_millis(200);

impl Link {
    /// Открыть `url` с параметрами `open` бэкендом `backend` (`None` — первым,
    /// кто узнал url) и писать в новый спул; `keep` — каталог, где спул останется.
    pub fn start(
        url: &str,
        open: &str,
        backend: Option<Backend>,
        keep: Option<&Path>,
        notify: Notify,
    ) -> Result<Link, String> {
        let b = match backend {
            Some(b) => b,
            None => drivers()
                .into_iter()
                .find(|d| d.link(url))
                .map(|d| Backend(d.name()))
                .ok_or_else(|| format!("не линк: {url}"))?,
        };
        let spool = Arc::new(Spool {
            path: spool_path(url, keep)?,
            kept: keep.is_some(),
        });
        let status = Status {
            url: url.into(),
            open: open.into(),
            kept: keep.is_some(),
            ..Default::default()
        };
        Link::spawn(url, b, keep, spool, status, false, notify)
    }

    /// Переподключить линк в тот же спул, дописывая за последним сообщением:
    /// с историей — `mode=seq-data` с последнего seq, без неё — с теми же
    /// параметрами (`agr:link-open`). Ещё не было данных — как заново.
    pub fn resume(&self, notify: Notify) -> Result<Link, String> {
        let old = self.status();
        let open = match (old.history, old.last) {
            (Some(true), Some(l)) => format!("mode=seq-data;seq={l}"),
            _ => old.open.clone(),
        };
        let append = old.spool.is_some();
        let status = Status {
            url: old.url.clone(),
            open,
            kept: old.kept,
            count: old.count,
            last: old.last,
            history: old.history,
            ..Default::default()
        };
        Link::spawn(
            &self.url,
            self.backend,
            self.keep.as_deref(),
            self.spool.clone(),
            status,
            append,
            notify,
        )
    }

    /// Тот же линк с другими параметрами открытия — в новый спул.
    pub fn reopen(&self, open: &str, notify: Notify) -> Result<Link, String> {
        Link::start(
            &self.url,
            open,
            Some(self.backend),
            self.keep.as_deref(),
            notify,
        )
    }

    fn spawn(
        url: &str,
        b: Backend,
        keep: Option<&Path>,
        spool: Arc<Spool>,
        status: Status,
        append: bool,
        notify: Notify,
    ) -> Result<Link, String> {
        let open = status.open.clone();
        let status = Arc::new(Mutex::new(status));
        let stop = Arc::new(AtomicBool::new(false));
        let job = Job {
            backend: b,
            url: url.to_string(),
            open,
            spool: spool.path.clone(),
            append,
        };
        let (st, halt) = (status.clone(), stop.clone());
        let thread = std::thread::Builder::new()
            .name("link".into())
            .spawn(move || record(&job, &st, &halt, &notify))
            .map_err(|e| e.to_string())?;
        Ok(Link {
            url: url.into(),
            backend: b,
            keep: keep.map(Path::to_path_buf),
            spool,
            status,
            stop,
            thread: Some(thread),
        })
    }

    /// Url линка.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Путь спула (файл появится на первом Active).
    pub fn spool(&self) -> &Path {
        &self.spool.path
    }

    /// Текущее состояние.
    pub fn status(&self) -> Status {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Дождаться готового спула или ошибки до него; `each` видит каждую смену
    /// состояния (терминал печатает их, пока вида ещё нет).
    pub fn ready(&self, mut each: impl FnMut(&Status)) -> Result<PathBuf, String> {
        let mut seen = None;
        loop {
            let s = self.status();
            if seen.as_ref() != Some(&(s.state, s.error.clone())) {
                each(&s);
                seen = Some((s.state, s.error.clone()));
            }
            if let Some(p) = s.spool {
                return Ok(p);
            }
            if matches!(s.state, LinkState::Error | LinkState::Closed) {
                return Err(s.line());
            }
            std::thread::sleep(POLL);
        }
    }
}

impl std::fmt::Debug for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Link").field("url", &self.url).finish()
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Что открыть потоку записи.
struct Job {
    backend: Backend,
    url: String,
    open: String,
    spool: PathBuf,
    /// Дописывать в существующий спул (переподключение).
    append: bool,
}

/// Поток записи: канал линка → спул, состояние → [`Status`].
fn record(job: &Job, status: &Mutex<Status>, stop: &AtomicBool, notify: &Notify) {
    let set = |f: &mut dyn FnMut(&mut Status)| {
        f(&mut status.lock().unwrap_or_else(|e| e.into_inner()));
        notify();
    };
    let fail = |e: String| {
        set(&mut |s| {
            s.state = LinkState::Error;
            s.error = Some(e.clone());
        })
    };
    let ctx = Context::new();
    let mut link = match ctx.with(job.backend, |c| c.link(&job.url, &job.open)) {
        Ok(l) => l,
        Err(e) => return fail(e),
    };
    let mut writer: Option<Writer> = None;
    let mut woke = Instant::now();
    let mut dirty = false;
    while !stop.load(Ordering::Relaxed) {
        for ev in link.poll(POLL) {
            match ev {
                LinkEvent::State(st) => {
                    if st == LinkState::Active && writer.is_none() {
                        match open_spool(&ctx, job, link.scheme_url()) {
                            Ok(w) => {
                                writer = Some(w);
                                let h = link.history();
                                set(&mut |s| {
                                    s.spool = Some(job.spool.clone());
                                    s.history = Some(h);
                                });
                            }
                            Err(e) => return fail(e),
                        }
                    }
                    set(&mut |s| {
                        s.state = st;
                        if st == LinkState::Active {
                            s.error = None;
                        }
                    });
                    if matches!(st, LinkState::Closed | LinkState::Error) {
                        return;
                    }
                }
                LinkEvent::Online(_) => set(&mut |s| s.online = true),
                LinkEvent::Error(e) => set(&mut |s| s.error = Some(e.clone())),
                LinkEvent::Data(m) => {
                    let Some(w) = writer.as_mut() else { continue };
                    if let Err(e) = w.post(&m) {
                        return fail(format!("спул: {e}"));
                    }
                    let mut s = status.lock().unwrap_or_else(|e| e.into_inner());
                    s.count += 1;
                    s.last = Some(m.seq);
                    dirty = true;
                }
            }
        }
        if dirty && woke.elapsed() >= WAKE {
            notify();
            woke = Instant::now();
            dirty = false;
        }
    }
    if dirty {
        notify();
    }
}

/// Спул на запись: новый — со схемой линка, при переподключении — дописать.
fn open_spool(ctx: &Context, job: &Job, scheme: Option<String>) -> Result<Writer, String> {
    let scheme = scheme.ok_or("у линка нет схемы данных — спул не записать")?;
    let like = Info {
        backend: job.backend,
        ..Default::default()
    };
    Writer::open(ctx, &job.spool, &scheme, &like, !job.append)
}

/// Где лежит спул: `keep` — там, под именем из url и времени; иначе — во
/// временном каталоге процесса.
fn spool_path(url: &str, keep: Option<&Path>) -> Result<PathBuf, String> {
    let n = SPOOLS.fetch_add(1, Ordering::Relaxed);
    if let Some(dir) = keep {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        return Ok(dir.join(format!("{}-{secs}-{n}.dat", sanitize(url))));
    }
    let dir = temp_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir.join(format!("link-{n}.dat")))
}

/// Имя файла из url: буквы, цифры, `.`, `-`; прочее — `_`.
pub fn sanitize(url: &str) -> String {
    let s: String = url
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let s = s.trim_matches('_');
    s.chars().take(80).collect()
}

/// Временный каталог спулов этого процесса.
fn temp_dir() -> PathBuf {
    std::env::temp_dir().join(format!("mutator-{}", std::process::id()))
}

/// Удалить временные спулы этого процесса — при выходе и панике.
pub fn cleanup() {
    let _ = std::fs::remove_dir_all(temp_dir());
}

/// Удалить временные спулы процессов, которых уже нет: SIGKILL и падение не
/// оставляют мусор дольше следующего запуска.
pub fn cleanup_stale() {
    let Ok(dir) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for e in dir.flatten() {
        let name = e.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|n| n.strip_prefix("mutator-"))
            .and_then(|p| p.parse::<u32>().ok())
        else {
            continue;
        };
        if pid != std::process::id() && !alive(pid) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    // Сигнал 0 только проверяет процесс; EPERM — жив, но чужой.
    let ok = unsafe { kill(pid as i32, 0) } == 0;
    ok || std::io::Error::last_os_error().raw_os_error() == Some(1)
}

#[cfg(not(unix))]
fn alive(_pid: u32) -> bool {
    true
}
