//! Данные файла для интерфейса: окно строк, индекс, фильтр, поиск — в фоне.
//!
//! Интерфейс и воркер говорят только каналами: [`Cmd`] туда, [`Ev`] обратно,
//! кадр не ждёт диска (`agr:ui-no-wait`). Воркер читает окна строк сам, а долгое
//! (индекс, фильтр, поиск) отдаёт потокам с флагом отмены: новый фильтр или поиск
//! отменяет прежний. Запросы окна несут поколение — устаревшие пропускаются,
//! так что перетаскивание полосы прокрутки не копит очередь.
//!
//! Файлы: `chunks.rs` — параллельный проход по кускам seq, `index.rs` —
//! разреженный индекс строк, `fetch.rs` — чтение окна, `scan.rs` — фильтр и поиск.
//!
//! Lua-transform встаёт в чтение тел окна: [`File::source_transform`] — тот же
//! файл с правками, но с transform под ними; тела едут в вид уже прогоными —
//! как на фиксации, — а рядом с ними [`Mutations`] (что изменилось, значение
//! в файле) и [`Ev::TransformError`] (`agr:transform-shadow`). Диф мутатора
//! считается второй читкой: те же seq читаются сырыми из файла и прогоняются
//! через [`Transformer`] (пара `lua+direct`). Фильтр и поиск работают по тому
//! же правленному потоку (transform, поверх — правки): типы отбираются в Rust по `msgid`,
//! выражение `f`/`t` — пост-эвалом через [`Transformer::filter`]
//! (`agr:filter-post-eval`). Проверка правки — по тому же потоку, что читает
//! вид (правка ложится поверх мутации, как на фиксации); масс-скан — по сырому
//! потоку: найденный seq — файловый. Фиксация — отдельно, всеми правками с
//! составом потока поверх transform (ручная правка поверх мутации скрипта).
//!
//! Наружу: [`File`], [`Part`], [`Worker`], [`Cmd`], [`Ev`], [`View`], [`At`],
//! [`Edge`], [`Index`], [`Matches`], [`Mutations`], [`fetch`], [`filter`],
//! [`search`].
//!
//! Ещё: [`STEP`], [`Place`], [`shadow`] — тот же диф мутатора зовёт `--dump`;
//! [`SEQ_PATH`] — «поле» seq в карте [`Mutations`] (исходный seq, если Lua его
//! поменял); [`raw_window`] — сырое окно для [`shadow`].

mod chunks;
pub mod fetch;
mod index;
mod scan;

pub use index::{Index, STEP};
pub use scan::Matches;
pub use scan::{filter, search};

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use crate::decode;
use crate::edit::{commit, Patch};
use crate::io::link::Link;
use crate::io::{Context, Info, LuaMode, Msg, Open, Reader, Transformer};
use crate::lua::Filter;
use crate::scheme::Scheme;

/// seq → пути полей, изменившиеся после мутатора: путь → значение в файле, до
/// мутатора (`agr:transform-shadow`). Пустая карта у seq — тело менялось, но
/// полей не нашли (новое тело не разобралось).
pub type Mutations = BTreeMap<i64, BTreeMap<String, String>>;

/// Часть открытого файла: путь и свои границы seq (`agr:glue-parts`).
#[derive(Debug, Clone)]
pub struct Part {
    pub path: PathBuf,
    pub info: Info,
}

impl Part {
    /// Первый seq; пустой части — 0.
    pub fn begin(&self) -> i64 {
        self.info.seq_begin.unwrap_or(0)
    }
    /// Последний seq; пустой части — −1: чтению она не отвечает.
    pub fn last(&self) -> i64 {
        self.info.seq_last.unwrap_or(-1)
    }
    fn empty(&self) -> bool {
        self.info.seq_last.is_none()
    }
}

/// Открытый файл — или склейка (`agr:glue-parts`): упорядоченные части с
/// восходящими непересекающимися диапазонами seq (инвариант тот же, что у карты
/// rotate+), общие границы и схема последней части (как rotate).
#[derive(Debug, Clone)]
pub struct File {
    /// Части по возрастанию seq; одиночный файл — одна. Пустые (молодой
    /// `current.dat`) остаются — чтению они инертны.
    pub parts: Vec<Part>,
    /// Имя для человека: путь файла или строка спеки — сессия, титул, агент.
    pub name: String,
    /// Общее: begin первой непустой, last последней, формат — последней части.
    pub info: Info,
    pub scheme: Arc<Scheme>,
    /// Схема данных строкой url (`yamls+gz://…`) — мутатору для имён полей.
    pub scheme_url: Option<String>,
    /// Правки, через которые файл показывается; `None` — как есть.
    pub patch: Option<Patch>,
    /// Lua-код (фильтр или transform).
    pub lua: Option<String>,
    /// Режим Lua-канала; `None` при `lua` — по умолчанию Filter.
    pub lua_mode: Option<LuaMode>,
    /// Линк, который пишет этот файл-спул: вид — только просмотр, хвост
    /// растёт сам (`agr:link-spool`).
    pub link: Option<Arc<Link>>,
}

impl File {
    /// Открыть по спеке: путь, склейка `a+b+c` или ротация `-N` (`agr:glue-spec`).
    pub fn open_spec(spec: &str) -> Result<File, String> {
        let paths = crate::spec::expand(spec)?;
        let ctx = Context::new();
        let mut parts = Vec::with_capacity(paths.len());
        let mut scheme = Scheme::default();
        let mut scheme_url = None;
        for p in &paths {
            let r =
                Reader::open(&ctx, &Open::file(p)).map_err(|e| format!("{}: {e}", p.display()))?;
            parts.push(Part {
                path: p.clone(),
                info: r.info(),
            });
            // Схема — последней части, у которой она есть (как rotate).
            if let Some(u) = r.scheme_url() {
                scheme_url = Some(u);
                scheme = r.scheme().unwrap_or_default();
            }
        }
        let filled: Vec<&Part> = parts.iter().filter(|p| !p.empty()).collect();
        for w in filled.windows(2) {
            if w[0].last() >= w[1].begin() {
                return Err(format!(
                    "склейка не по возрастанию seq: {} кончается {}, а {} начинается с {}",
                    w[0].path.display(),
                    w[0].last(),
                    w[1].path.display(),
                    w[1].begin()
                ));
            }
        }
        let mut info = parts.last().map(|p| p.info.clone()).unwrap_or_default();
        info.seq_begin = filled.first().map(|p| p.begin());
        info.seq_last = filled.last().map(|p| p.last());
        Ok(File {
            parts,
            name: spec.to_string(),
            info,
            scheme: Arc::new(scheme),
            scheme_url,
            patch: None,
            lua: None,
            lua_mode: None,
            link: None,
        })
    }

    /// Открыть один файл: путь — буквальная спека.
    pub fn open(path: &Path) -> Result<File, String> {
        Self::open_spec(&path.to_string_lossy())
    }

    /// Спул линка, когда он готов ([`Link::ready`]): одна часть — спул, имя —
    /// url линка.
    pub fn open_link(link: Link) -> Result<File, String> {
        let mut f = File::open(link.spool())?;
        f.name = link.url().to_string();
        f.link = Some(Arc::new(link));
        Ok(f)
    }

    /// Ключ сессии (`agr:session-by-name`): у файла — его имя, у линка — url:
    /// спул каждый раз новый, а вид — того же линка.
    pub fn session_key(&self) -> PathBuf {
        match &self.link {
            Some(l) => PathBuf::from(format!("{}.link", crate::io::link::sanitize(l.url()))),
            None => PathBuf::from(&self.name),
        }
    }

    /// Что открывать для чтения: файл с правками, без Lua — вид показывает файл
    /// как есть (`agr:transform-shadow`).
    pub fn sources(&self) -> Vec<fetch::Src> {
        self.parts
            .iter()
            .map(|p| fetch::Src {
                open: Open::file(&p.path)
                    .backend(self.info.backend)
                    .patch(self.patch.clone()),
                begin: p.begin(),
                last: p.last(),
            })
            .collect()
    }

    /// Что открывать для чтения тел в виде: тот же файл с правками, но с
    /// transform под ними — тела приходят окончательными, как уйдут в файл на
    /// фиксации (`agr:transform-shadow`): ручная правка ложится поверх мутации
    /// скрипта.
    pub fn source_transform(&self) -> Vec<fetch::Src> {
        self.parts
            .iter()
            .map(|p| {
                let mut o = Open::file(&p.path).backend(self.info.backend);
                if self.transforming() {
                    o.lua = self.lua.clone();
                    o.lua_mode = self.lua_mode;
                }
                o = o.patch(self.patch.clone());
                fetch::Src {
                    open: o,
                    begin: p.begin(),
                    last: p.last(),
                }
            })
            .collect()
    }

    /// Части как есть, без правок и мутатора: диф мутатора и подсчёт строк.
    pub fn raw_sources(&self) -> Vec<fetch::Src> {
        self.parts
            .iter()
            .map(|p| fetch::Src {
                open: Open::file(&p.path).backend(self.info.backend),
                begin: p.begin(),
                last: p.last(),
            })
            .collect()
    }

    /// Включён ли transform: есть код и режим Transform.
    pub fn transforming(&self) -> bool {
        self.lua.is_some() && self.lua_mode == Some(LuaMode::Transform)
    }

    pub fn begin(&self) -> i64 {
        self.info.seq_begin.unwrap_or(0)
    }
    pub fn last(&self) -> i64 {
        self.info.seq_last.unwrap_or(-1)
    }
}

/// Какие строки показывает таблица.
#[derive(Debug, Clone)]
pub enum View {
    /// Все сообщения файла.
    All,
    /// Найденное фильтром.
    Filtered(Arc<Matches>),
}

/// Куда встать окном.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum At {
    /// Первая строка с seq ≥ этого.
    Seq(i64),
    /// Строка с этим номером (нужен индекс или фильтр).
    Row(usize),
    /// Последняя страница.
    End,
    /// Строка с этим seq (или ближайшая за ним) и столько строк до неё: окно
    /// с контекстом выше — для возврата к выбранной строке (`agr:anchor`).
    Around(i64, usize),
}

/// Край окна, от которого дочитывать.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edge {
    pub seq: i64,
    pub row: Option<usize>,
}

/// Команды воркеру.
#[derive(Debug)]
pub enum Cmd {
    /// Новое окно из `n` строк с `at`.
    Window {
        gen: u64,
        view: View,
        at: At,
        n: usize,
    },
    /// Дочитать `n` строк после (`after`) или перед краем окна.
    Extend {
        gen: u64,
        view: View,
        after: bool,
        edge: Edge,
        n: usize,
    },
    /// Запустить фильтр; пустой — остановить прежний.
    Filter { id: u64, filter: Filter },
    /// Найти ближайшее совпадение от seq `from`.
    Search {
        id: u64,
        filter: Filter,
        from: i64,
        back: bool,
    },
    /// Остановить поиск.
    StopSearch,
    /// Спул линка подрос: перечитать границы, дописать индекс и фильтр по
    /// хвосту — курсор и окно вид не трогает (`agr:link-tail`).
    Tail,
    /// Читать дальше через эти правки (`None` — без правок). Окна и фильтр
    /// после этого вид запрашивает заново.
    Patch(Option<Patch>),
    /// Установить Lua-код и режим; `None` для `lua` — убрать.
    Lua {
        lua: Option<String>,
        lua_mode: Option<LuaMode>,
    },
    /// Прочитать сообщение `seq` через кандидат-правки `patch`: примется ли правка.
    Check { id: u64, patch: Patch, seq: i64 },
    /// Записать файл через правки `patch` — план за планом, в фоне, с прогрессом.
    Commit {
        patch: Option<Patch>,
        plans: Vec<commit::Plan>,
        /// Код Lua-transform — применить его же при записи, а не только чтении.
        lua_code: Option<String>,
        lua_mode: Option<LuaMode>,
    },
    /// Seq файла в `[from, to]` — для удаления диапазона (`agr:stream-ops`).
    Seqs { id: u64, from: i64, to: i64 },
    /// Последний неудалённый seq не дальше `last` — для прогноза, когда хвост
    /// удалён (`agr:seq-forecast`).
    TailAlive { id: u64, last: i64, dead: Vec<i64> },
    /// Просканировать список seq: вернуть те, у которых поле `field_path` есть в схеме.
    /// `seeds` — кандидаты (seq из Matches); `None` — сканировать диапазон.
    MassScan {
        id: u64,
        seeds: Option<Vec<i64>>,
        begin: i64,
        last: i64,
        field_path: String,
        scheme: Arc<Scheme>,
    },
}

/// Как лечь строкам окна.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    Replace,
    Before,
    After,
}

/// События воркера.
#[derive(Debug)]
pub enum Ev {
    Window {
        gen: u64,
        place: Place,
        rows: Vec<Msg>,
        /// Номер первой строки, если известен (индекс готов или вид — фильтр).
        first_row: Option<usize>,
        /// Перед окном строк нет.
        at_start: bool,
        /// После окна строк нет (у растущего фильтра — пока нет).
        at_end: bool,
        /// seq → пути полей, которые мутатор изменил (значение после него).
        mutated: Mutations,
        /// Мутатор сломался: текст ошибки; вид остался сырым.
        transform_error: Option<String>,
    },
    /// Трансформер не собрался или сломался при открытии окна: текст.
    TransformError(String),
    IndexProgress(f32),
    Index(Arc<Index>),
    /// Ответ на [`Cmd::Tail`]: новые границы спула и индекс с хвостом (если
    /// индекс уже был); найденное фильтром в хвосте придёт [`Ev::FilterTick`].
    Tail {
        info: Info,
        index: Option<Arc<Index>>,
    },
    /// Фильтр запущен: его результат растёт в `matches`.
    Filter {
        id: u64,
        matches: Arc<Matches>,
    },
    /// Фильтр нашёл ещё (или закончил).
    FilterTick {
        id: u64,
    },
    Found {
        id: u64,
        seq: Option<i64>,
        error: Option<String>,
    },
    /// Ответ на [`Cmd::Check`]: сообщение с правкой или почему её не записать.
    Checked {
        id: u64,
        result: Result<Msg, String>,
    },
    CommitProgress(f32),
    /// Фиксация закончилась: итоги по планам или ошибка (исходники целы).
    Committed(Result<Vec<commit::Committed>, String>),
    /// Ответ на [`Cmd::Seqs`]: (seq, msgid) и первый seq после диапазона.
    Seqs {
        id: u64,
        result: Result<fetch::FileSeqs, String>,
    },
    /// Ответ на [`Cmd::TailAlive`].
    TailAlive {
        id: u64,
        result: Result<Option<i64>, String>,
    },
    /// Результат MassScan: пары (seq, имя_типа) для сообщений с нужным полем.
    MassScan {
        id: u64,
        pairs: Vec<(i64, String)>,
    },
    Error(String),
}

/// Воркер: поток чтения окон и фоновые задачи.
pub struct Worker {
    tx: Sender<Cmd>,
    rx: Receiver<Ev>,
}

type Notify = Arc<dyn Fn() + Send + Sync>;

impl Worker {
    /// Запустить воркер файла; `notify` будит интерфейс после каждого события.
    pub fn start(file: File, notify: Notify) -> Worker {
        let (tx, crx) = mpsc::channel();
        let (etx, rx) = mpsc::channel();
        let index: Arc<Mutex<Option<Arc<Index>>>> = Arc::default();
        spawn_index(&file, &etx, &notify, &index);
        std::thread::Builder::new()
            .name("data".into())
            .spawn(move || run(file, crx, etx, notify, index))
            .expect("поток data");
        Worker { tx, rx }
    }

    pub fn send(&self, c: Cmd) {
        let _ = self.tx.send(c);
    }

    /// Все накопившиеся события.
    pub fn poll(&self) -> Vec<Ev> {
        self.rx.try_iter().collect()
    }

    /// Дождаться события (тесты и `--tui` без ввода).
    pub fn wait(&self, timeout: std::time::Duration) -> Option<Ev> {
        self.rx.recv_timeout(timeout).ok()
    }
}

fn emit(tx: &Sender<Ev>, notify: &Notify, ev: Ev) {
    let _ = tx.send(ev);
    notify();
}

/// Окно + [`Ev::TransformError`]: ошибку показываем только при смене текста —
/// после первой она повторяется на каждое окно.
fn emit_window(tx: &Sender<Ev>, notify: &Notify, ev: Reply, gen: u64, tx_err: &mut Option<String>) {
    let (place, rows, first_row, at_start, at_end, mutated, terr) = ev;
    if let Some(e) = &terr {
        if tx_err.as_deref() != Some(e.as_str()) {
            emit(tx, notify, Ev::TransformError(e.clone()));
        }
    }
    *tx_err = terr;
    emit(
        tx,
        notify,
        Ev::Window {
            gen,
            place,
            rows,
            first_row,
            at_start,
            at_end,
            mutated,
            transform_error: tx_err.clone(),
        },
    );
}

/// Ошибка трансформера: [`Ev::TransformError`] при смене текста, `None` —
/// тихий сброс (transform выключили или починили).
fn report_tx_err(tx: &Sender<Ev>, notify: &Notify, cur: &mut Option<String>, e: Option<String>) {
    if cur.as_deref() == e.as_deref() {
        return;
    }
    if let Some(e) = &e {
        emit(tx, notify, Ev::TransformError(e.clone()));
    }
    *cur = e;
}

fn spawn_index(
    file: &File,
    tx: &Sender<Ev>,
    notify: &Notify,
    slot: &Arc<Mutex<Option<Arc<Index>>>>,
) {
    let parts: Vec<(PathBuf, i64, i64)> = file
        .parts
        .iter()
        .map(|p| (p.path.clone(), p.begin(), p.last()))
        .collect();
    let (tx, notify, slot) = (tx.clone(), notify.clone(), slot.clone());
    std::thread::spawn(move || {
        let stop = Arc::new(AtomicBool::new(false));
        let mut last = 0.0;
        let list: Vec<(&Path, i64, i64)> = parts
            .iter()
            .map(|(p, b, l)| (p.as_path(), *b, *l))
            .collect();
        let r = index::build(&list, &stop, |p| {
            if p - last >= 0.02 || p >= 1.0 {
                last = p;
                emit(&tx, &notify, Ev::IndexProgress(p));
            }
        });
        match r {
            Ok(i) => {
                let i = Arc::new(i);
                *slot.lock().unwrap() = Some(i.clone());
                emit(&tx, &notify, Ev::Index(i));
            }
            Err(e) => emit(&tx, &notify, Ev::Error(format!("индекс: {e}"))),
        }
    });
}

struct Job {
    stop: Arc<AtomicBool>,
}

impl Job {
    fn new() -> Job {
        Job {
            stop: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn run(
    file: File,
    rx: Receiver<Cmd>,
    tx: Sender<Ev>,
    notify: Notify,
    index: Arc<Mutex<Option<Arc<Index>>>>,
) {
    let mut file = file;
    let ctx = Context::new();
    let mut filter_job: Option<Job> = None;
    let mut search_job: Option<Job> = None;
    let mut commit_job: Option<Job> = None;
    let mut xform: Option<(Transformer, String)> = None;
    let mut tx_err: Option<String> = None;
    // Текущий фильтр — для хвоста растущего спула (`agr:link-tail`).
    let mut tail_filter: Option<TailFilter> = None;
    while let Ok(first) = rx.recv() {
        let mut batch = vec![first];
        batch.extend(rx.try_iter());
        let newest = batch
            .iter()
            .filter_map(|c| match c {
                Cmd::Window { gen, .. } => Some(*gen),
                _ => None,
            })
            .max();
        for c in batch {
            match c {
                Cmd::Window { gen, .. } | Cmd::Extend { gen, .. }
                    if newest.is_some_and(|n| gen < n) => {}
                Cmd::Window { gen, view, at, n } => {
                    let idx = index.lock().unwrap().clone();
                    let mutx = match transformer(&ctx, &file, &mut xform) {
                        Ok(t) => t,
                        Err(e) => {
                            report_tx_err(&tx, &notify, &mut tx_err, Some(e));
                            continue;
                        }
                    };
                    let read = Read {
                        ctx: &ctx,
                        f: &file,
                        tx: mutx,
                    };
                    match window(read, &view, idx.as_deref(), at, n) {
                        Ok(ev) => emit_window(&tx, &notify, ev, gen, &mut tx_err),
                        Err(e) => emit(&tx, &notify, Ev::Error(e)),
                    }
                }
                Cmd::Extend {
                    gen,
                    view,
                    after,
                    edge,
                    n,
                } => {
                    let idx = index.lock().unwrap().clone();
                    let mutx = match transformer(&ctx, &file, &mut xform) {
                        Ok(t) => t,
                        Err(e) => {
                            report_tx_err(&tx, &notify, &mut tx_err, Some(e));
                            continue;
                        }
                    };
                    let read = Read {
                        ctx: &ctx,
                        f: &file,
                        tx: mutx,
                    };
                    match extend(read, &view, idx.as_deref(), after, edge, n) {
                        Ok(ev) => emit_window(&tx, &notify, ev, gen, &mut tx_err),
                        Err(e) => emit(&tx, &notify, Ev::Error(e)),
                    }
                }
                Cmd::Filter { id, filter } => {
                    filter_job = None;
                    tail_filter = None;
                    if filter.is_empty() {
                        continue;
                    }
                    let job = Job::new();
                    let m = Arc::new(Matches::default());
                    emit(
                        &tx,
                        &notify,
                        Ev::Filter {
                            id,
                            matches: m.clone(),
                        },
                    );
                    tail_filter = Some(TailFilter {
                        id,
                        filter: filter.clone(),
                        matches: m.clone(),
                        scanned: file.last(),
                        busy: Arc::new(AtomicBool::new(false)),
                        stop: job.stop.clone(),
                    });
                    let (file, tx, notify, stop) =
                        (file.clone(), tx.clone(), notify.clone(), job.stop.clone());
                    std::thread::spawn(move || {
                        scan::filter(&file, &filter, &stop, &m, || {
                            emit(&tx, &notify, Ev::FilterTick { id })
                        });
                    });
                    filter_job = Some(job);
                }
                Cmd::Search {
                    id,
                    filter,
                    from,
                    back,
                } => {
                    let job = Job::new();
                    let (file, tx, notify, stop) =
                        (file.clone(), tx.clone(), notify.clone(), job.stop.clone());
                    std::thread::spawn(move || {
                        let r = scan::search(&file, &filter, from, back, &stop);
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                        let (seq, error) = match r {
                            Ok(s) => (s, None),
                            Err(e) => (None, Some(e)),
                        };
                        emit(&tx, &notify, Ev::Found { id, seq, error });
                    });
                    search_job = Some(job);
                }
                Cmd::StopSearch => search_job = None,
                Cmd::Tail => {
                    // Растёт последняя часть: у спула линка она одна.
                    let Some(part) = file.parts.last() else {
                        continue;
                    };
                    let Ok(info) = Reader::open(&ctx, &Open::file(&part.path)).map(|r| r.info())
                    else {
                        continue;
                    };
                    let grew = info.seq_last > file.info.seq_last;
                    if grew {
                        if file.parts.len() == 1 {
                            file.info = info.clone();
                        } else {
                            file.info.seq_last = info.seq_last;
                        }
                        if let Some(p) = file.parts.last_mut() {
                            p.info = info;
                        }
                    }
                    // Хвост фильтра — и без роста: прежний мог ещё идти, когда
                    // пришёл прошлый `Tail`.
                    if let Some(t) = tail_filter.as_mut() {
                        t.scan(&file, &tx, &notify);
                    }
                    let idx = index.lock().unwrap().clone();
                    if grew || idx.as_ref().is_some_and(|i| i.last < file.info.seq_last) {
                        let idx = tail_index(&file, &index);
                        emit(
                            &tx,
                            &notify,
                            Ev::Tail {
                                info: file.info.clone(),
                                index: idx,
                            },
                        );
                    }
                }
                Cmd::Patch(p) => file.patch = p,
                Cmd::Lua { lua, lua_mode } => {
                    file.lua = lua;
                    file.lua_mode = lua_mode;
                    report_tx_err(&tx, &notify, &mut tx_err, None);
                }
                Cmd::Check { id, patch, seq } => {
                    let srcs = file.source_transform();
                    let result = match fetch::Src::part_of(&srcs, seq) {
                        Some(j) => check(&ctx, &srcs[j].open.clone().patch(Some(patch)), seq),
                        None => Err(format!("seq {seq} за концом файла")),
                    };
                    emit(&tx, &notify, Ev::Checked { id, result });
                }
                Cmd::Commit {
                    patch,
                    plans,
                    lua_code,
                    lua_mode,
                } => {
                    let job = Job::new();
                    let (plans, tx, notify, stop) =
                        (plans.clone(), tx.clone(), notify.clone(), job.stop.clone());
                    let code = lua_code.clone();
                    std::thread::spawn(move || {
                        let r = commit::run_all(
                            &plans,
                            patch.as_ref(),
                            &stop,
                            code.as_deref().map(|c| (c, lua_mode)),
                            |p| emit(&tx, &notify, Ev::CommitProgress(p)),
                        );
                        emit(&tx, &notify, Ev::Committed(r));
                    });
                    commit_job = Some(job);
                }
                Cmd::Seqs { id, from, to } => {
                    let result = fetch::seqs_in(&ctx, &file.raw_sources(), from, to);
                    emit(&tx, &notify, Ev::Seqs { id, result });
                }
                Cmd::TailAlive { id, last, dead } => {
                    let dead: std::collections::BTreeSet<i64> = dead.into_iter().collect();
                    let result =
                        fetch::alive_before(&ctx, &file.raw_sources(), file.begin(), last, |s| {
                            dead.contains(&s)
                        });
                    emit(&tx, &notify, Ev::TailAlive { id, result });
                }
                Cmd::MassScan {
                    id,
                    seeds,
                    begin,
                    last,
                    field_path,
                    scheme,
                } => {
                    let srcs = file.sources();
                    let tx = tx.clone();
                    let notify = notify.clone();
                    std::thread::spawn(move || {
                        let ctx = Context::new();
                        let mut pairs = Vec::new();
                        if let Some(seeds) = seeds {
                            for &seed in &seeds {
                                let m = fetch::Src::part_of(&srcs, seed)
                                    .and_then(|j| {
                                        Reader::open(&ctx, &srcs[j].open.clone().at(seed)).ok()
                                    })
                                    .and_then(|mut r| r.next());
                                if let Some(m) = m {
                                    let msg = scheme.message(m.msgid);
                                    if msg.is_some_and(|st| {
                                        scheme.field_path(st, &field_path).is_some()
                                    }) {
                                        let name = msg
                                            .map(|s| s.name.clone())
                                            .unwrap_or_else(|| format!("#{}", m.msgid));
                                        pairs.push((seed, name));
                                    }
                                }
                            }
                        } else {
                            'parts: for s in &srcs {
                                if s.last < begin {
                                    continue;
                                }
                                let Ok(mut r) = Reader::open(
                                    &ctx,
                                    &s.open.clone().at(begin.max(s.begin)).meta(),
                                ) else {
                                    continue;
                                };
                                for m in r.by_ref() {
                                    if m.seq > last {
                                        break 'parts;
                                    }
                                    let msg = scheme.message(m.msgid);
                                    if msg.is_some_and(|st| {
                                        scheme.field_path(st, &field_path).is_some()
                                    }) {
                                        let name = msg
                                            .map(|s| s.name.clone())
                                            .unwrap_or_else(|| format!("#{}", m.msgid));
                                        pairs.push((m.seq, name));
                                    }
                                }
                            }
                        }
                        emit(&tx, &notify, Ev::MassScan { id, pairs });
                    });
                }
            }
        }
    }
    drop(filter_job);
    drop(search_job);
    drop(commit_job);
}

/// Индекс с хвостом спула за его прежним концом; индекса ещё нет — `None`:
/// вид попросит хвост снова, когда индекс придёт (`agr:link-tail`).
fn tail_index(file: &File, slot: &Mutex<Option<Arc<Index>>>) -> Option<Arc<Index>> {
    let cur = slot.lock().unwrap().clone()?;
    let from = cur.last.map_or(file.begin(), |l| l + 1);
    if from > file.last() {
        return Some(cur);
    }
    let stop = Arc::new(AtomicBool::new(false));
    let part = file.parts.last()?;
    let tail = index::build(&[(part.path.as_path(), from, file.last())], &stop, |_| {}).ok()?;
    let mut next = (*cur).clone();
    next.append(tail);
    let next = Arc::new(next);
    *slot.lock().unwrap() = Some(next.clone());
    Some(next)
}

/// Фильтр растущего спула: до какого seq отсканирован и идёт ли досканирование
/// хвоста. Хвост сканируется в свой [`Matches`] и дописывается в общий целиком
/// — префикс найденного растёт ровно, как у основного скана.
struct TailFilter {
    id: u64,
    filter: Filter,
    matches: Arc<Matches>,
    scanned: i64,
    busy: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl TailFilter {
    /// Досканировать `(scanned, file.last()]`, если основной скан закончен
    /// без ошибки и прежний хвост уже дописан.
    fn scan(&mut self, file: &File, tx: &Sender<Ev>, notify: &Notify) {
        let m = &self.matches;
        if !m.is_done() || m.error.lock().unwrap().is_some() || self.scanned >= file.last() {
            return;
        }
        if self.busy.swap(true, Ordering::AcqRel) {
            return;
        }
        let (from, to) = (self.scanned + 1, file.last());
        self.scanned = to;
        let (file, filter, m, busy, stop, tx, notify, id) = (
            file.clone(),
            self.filter.clone(),
            m.clone(),
            self.busy.clone(),
            self.stop.clone(),
            tx.clone(),
            notify.clone(),
            self.id,
        );
        std::thread::spawn(move || {
            let tail = Matches::default();
            scan::filter_range(&file, &filter, Some((from, to)), &stop, &tail, || {});
            if !stop.load(Ordering::Relaxed) {
                match tail.error.lock().unwrap().take() {
                    Some(e) => *m.error.lock().unwrap() = Some(e),
                    None => m
                        .seqs
                        .write()
                        .unwrap()
                        .extend(tail.seqs.into_inner().unwrap()),
                }
                emit(&tx, &notify, Ev::FilterTick { id });
            }
            busy.store(false, Ordering::Release);
        });
    }
}

/// Сообщение `seq` через правки `src`: правка, которую не записать, обрывает
/// чтение — её текст и есть ответ (`agr:edit-checked`).
fn check(ctx: &Context, src: &Open, seq: i64) -> Result<Msg, String> {
    let mut r = Reader::open(ctx, &src.clone().at(seq))?;
    let m = r.next();
    if let Some(e) = r.error() {
        return Err(e);
    }
    match m {
        Some(m) if m.seq == seq => Ok(m),
        _ => Err(r
            .error()
            .unwrap_or_else(|| format!("seq {seq} не прочитался"))),
    }
}

/// Готовое окно — всё для [`Ev::Window`], кроме gen.
type Reply = (
    Place,
    Vec<Msg>,
    Option<usize>,
    bool,
    bool,
    Mutations,
    Option<String>,
);

fn reply(
    place: Place,
    rows: Vec<Msg>,
    first_row: Option<usize>,
    at_start: bool,
    at_end: bool,
    mutated: Mutations,
    transform_error: Option<String>,
) -> Reply {
    (
        place,
        rows,
        first_row,
        at_start,
        at_end,
        mutated,
        transform_error,
    )
}

/// Трансформер под текущий код и схему: создать/пересоздать при смене, сбросить
/// при выключенном transform. Ошибка сборки — `Err`, вид остаётся сырым.
fn transformer<'a>(
    ctx: &Context,
    f: &File,
    cur: &'a mut Option<(Transformer, String)>,
) -> Result<Option<&'a mut Transformer>, String> {
    let Some(code) = f.transforming().then(|| f.lua.clone().unwrap()) else {
        *cur = None;
        return Ok(None);
    };
    let key = format!("{code}\u{0}{}", f.scheme_url.as_deref().unwrap_or_default());
    if cur.as_ref().is_some_and(|(_, k)| k == &key) {
        return Ok(Some(&mut cur.as_mut().unwrap().0));
    }
    let url = f
        .scheme_url
        .clone()
        .ok_or("схема файла не читается — мутатору не с чем работать")?;
    *cur = Some((Transformer::new(ctx, f.info.backend, &code, &url)?, key));
    Ok(Some(&mut cur.as_mut().unwrap().0))
}

/// Карта того, что мутатор изменил в строках окна: seq → путь → значение в
/// файле до мутатора (`agr:transform-shadow`). Тела в виде уже прогоные — их
/// читает канал чтения (transform, поверх — правки); здесь то же окно читается
/// сырым из файла (`raw`) и прогоняется мутатором второй читкой, чтобы
/// вычислить диф. Строки окна (`rows`) сопоставляются с прогоном по порядку и
/// по seq после мутатора: Lua мог поменять seq сообщения, тогда исходный seq
/// лежит в карте под путём [`SEQ_PATH`] и строка помечена, даже если тело то же.
/// Выброшенное мутатором сообщение в вид не попадает — в карте его нет.
pub(crate) fn shadow(
    f: &File,
    tx: &mut Transformer,
    raw: &[Msg],
    rows: &[Msg],
) -> Result<Mutations, String> {
    let mut passed = Vec::with_capacity(raw.len());
    for m in raw {
        if let Some(after) = tx.transform(m)? {
            passed.push((m, after));
        }
    }
    let mut out = Mutations::new();
    let mut next = 0;
    for row in rows {
        let Some(k) = passed[next.min(passed.len())..]
            .iter()
            .position(|(_, after)| after.seq == row.seq)
            .map(|k| k + next)
        else {
            continue;
        };
        next = k + 1;
        let (before, after) = &passed[k];
        let mut changed = if after.data == before.data {
            BTreeMap::new()
        } else {
            match f.scheme.message(before.msgid) {
                Some(st) => decode::diff(&f.scheme, st, &before.data, &after.data),
                None => BTreeMap::new(),
            }
        };
        if after.seq != before.seq {
            changed.insert(SEQ_PATH.to_string(), before.seq.to_string());
        } else if after.data == before.data {
            continue;
        }
        out.insert(row.seq, changed);
    }
    Ok(out)
}

/// Путь «поля» seq в карте [`Mutations`]: значение — seq в файле до мутатора.
pub const SEQ_PATH: &str = "seq";

/// Окно как читает его вид, но сырым из файла — для [`shadow`]: те же позиция
/// и размер, без мутатора и правок.
pub(crate) fn raw_window(f: &File, load: LoadFn<'_>) -> Result<Vec<Msg>, String> {
    load(&f.raw_sources())
}

/// Аргументы чтения окна: контекст libtll, файл и мутатор (если включён).
struct Read<'a> {
    ctx: &'a Context,
    f: &'a File,
    tx: Option<&'a mut Transformer>,
}

fn window(r: Read, view: &View, idx: Option<&Index>, at: At, n: usize) -> Result<Reply, String> {
    let Read { ctx, f, tx } = r;
    let (begin, last) = (f.begin(), f.last());
    match view {
        View::All => {
            let load = |src: &[fetch::Src]| match at {
                // Seq за концом файла новый libtll не открывает — встаём на последний.
                At::Seq(s) => fetch::from_seq(ctx, src, s.min(last), n),
                At::Row(r) => match idx {
                    Some(i) => fetch::from_row(ctx, src, i, r, n),
                    None => Err("номер строки — после индекса".into()),
                },
                At::End => fetch::before_seq(ctx, src, last + 1, n, begin),
                // Контекст выше строки и дальше вперёд: окно целиком по обе стороны.
                At::Around(s, before) => {
                    let s = s.min(last);
                    let mut rows = fetch::before_seq(ctx, src, s, before, begin)?;
                    rows.extend(fetch::from_seq(ctx, src, s, n)?);
                    Ok(rows)
                }
            };
            let (rows, fallback_err) = read_with_fallback(f, load)?;
            let first_row = match (idx, rows.first()) {
                (Some(i), Some(m)) => Some(fetch::row_of(ctx, &f.raw_sources(), i, m.seq)?),
                _ => None,
            };
            let at_start = rows.first().is_none_or(|m| m.seq <= begin);
            let at_end = rows.len() < n || rows.last().is_some_and(|m| m.seq >= last);
            let (mutated, terr) = shadow_after(fallback_err, f, tx, &rows, &load);
            Ok(reply(
                Place::Replace,
                rows,
                first_row,
                at_start,
                at_end,
                mutated,
                terr,
            ))
        }
        View::Filtered(m) => {
            let total = m.len();
            let row = match at {
                At::Seq(s) => m.row_of(s),
                At::Row(r) => r.min(total),
                At::End => total.saturating_sub(n),
                At::Around(s, before) => m.row_of(s).saturating_sub(before),
            };
            let load = |src: &[fetch::Src]| fetch::by_seqs(ctx, src, &m.slice(row, n));
            let (rows, fallback_err) = read_with_fallback(f, load)?;
            let at_end = row + rows.len() >= total && m.is_done();
            let (mutated, terr) = shadow_after(fallback_err, f, tx, &rows, &load);
            Ok(reply(
                Place::Replace,
                rows,
                Some(row),
                row == 0,
                at_end,
                mutated,
                terr,
            ))
        }
    }
}

/// Загрузчик окна: шаблоны частей → сообщения (вид, transform, сырой файл).
type LoadFn<'a> = &'a dyn Fn(&[fetch::Src]) -> Result<Vec<Msg>, String>;

/// Прочитать окно через transform-канал (с правками поверх); если transform
/// сломался на чтении (битый код, ошибка исполнения) — откатиться к чтению
/// файла с правками без transform и вернуть текст ошибки, чтобы скрипт не ронял вид
/// (`agr:transform-shadow`). При выключенном transform — сразу сырое чтение.
fn read_with_fallback(
    f: &File,
    load: impl Fn(&[fetch::Src]) -> Result<Vec<Msg>, String>,
) -> Result<(Vec<Msg>, Option<String>), String> {
    if !f.transforming() {
        return Ok((load(&f.sources())?, None));
    }
    match load(&f.source_transform()) {
        Ok(rows) => Ok((rows, None)),
        Err(e) => match load(&f.sources()) {
            Ok(rows) => Ok((rows, Some(e))),
            Err(raw) => Err(raw),
        },
    }
}

/// Подсветка после чтения: при откате (`fallback_err`) строки сырые, карты нет;
/// иначе — диф мутатора второй читкой.
fn shadow_after(
    fallback_err: Option<String>,
    f: &File,
    tx: Option<&mut Transformer>,
    rows: &[Msg],
    load: LoadFn<'_>,
) -> (Mutations, Option<String>) {
    match fallback_err {
        Some(e) => (Mutations::new(), Some(e)),
        None => shadow_or_empty(f, tx, rows, load),
    }
}

/// Прогнать окно через мутатор; ошибка мутатора не роняет окно — только текст
/// для [`Ev::TransformError`].
fn shadow_or_empty(
    f: &File,
    tx: Option<&mut Transformer>,
    rows: &[Msg],
    load: LoadFn<'_>,
) -> (Mutations, Option<String>) {
    match tx {
        Some(tx) => match raw_window(f, load).and_then(|raw| shadow(f, tx, &raw, rows)) {
            Ok(m) => (m, None),
            Err(e) => (Mutations::new(), Some(e)),
        },
        None => (Mutations::new(), None),
    }
}

fn extend(
    r: Read,
    view: &View,
    idx: Option<&Index>,
    after: bool,
    edge: Edge,
    n: usize,
) -> Result<Reply, String> {
    let Read { ctx, f, tx } = r;
    match view {
        View::All => {
            if after {
                if edge.seq >= f.last() {
                    return Ok(reply(
                        Place::After,
                        Vec::new(),
                        None,
                        false,
                        true,
                        Mutations::new(),
                        None,
                    ));
                }
                let load = |src: &[fetch::Src]| fetch::from_seq(ctx, src, edge.seq + 1, n);
                let (rows, fallback_err) = read_with_fallback(f, load)?;
                let at_end = rows.len() < n || rows.last().is_some_and(|m| m.seq >= f.last());
                let first = edge.row.map(|r| r + 1);
                let (mutated, terr) = shadow_after(fallback_err, f, tx, &rows, &load);
                Ok(reply(
                    Place::After,
                    rows,
                    first,
                    false,
                    at_end,
                    mutated,
                    terr,
                ))
            } else {
                let load = |src: &[fetch::Src]| fetch::before_seq(ctx, src, edge.seq, n, f.begin());
                let (rows, fallback_err) = read_with_fallback(f, load)?;
                let first = edge.row.and_then(|r| r.checked_sub(rows.len()));
                let first = first.or_else(|| {
                    idx.and_then(|i| {
                        rows.first()
                            .and_then(|m| fetch::row_of(ctx, &f.raw_sources(), i, m.seq).ok())
                    })
                });
                let at_start = rows.len() < n || rows.first().is_some_and(|m| m.seq <= f.begin());
                let (mutated, terr) = shadow_after(fallback_err, f, tx, &rows, &load);
                Ok(reply(
                    Place::Before,
                    rows,
                    first,
                    at_start,
                    false,
                    mutated,
                    terr,
                ))
            }
        }
        View::Filtered(m) => {
            let row = edge.row.unwrap_or_else(|| m.row_of(edge.seq));
            if after {
                let from = row + 1;
                let load = |src: &[fetch::Src]| fetch::by_seqs(ctx, src, &m.slice(from, n));
                let (rows, fallback_err) = read_with_fallback(f, load)?;
                let at_end = from + rows.len() >= m.len() && m.is_done();
                let (mutated, terr) = shadow_after(fallback_err, f, tx, &rows, &load);
                Ok(reply(
                    Place::After,
                    rows,
                    Some(from),
                    false,
                    at_end,
                    mutated,
                    terr,
                ))
            } else {
                let from = row.saturating_sub(n);
                let load =
                    |src: &[fetch::Src]| fetch::by_seqs(ctx, src, &m.slice(from, row - from));
                let (rows, fallback_err) = read_with_fallback(f, load)?;
                let (mutated, terr) = shadow_after(fallback_err, f, tx, &rows, &load);
                Ok(reply(
                    Place::Before,
                    rows,
                    Some(from),
                    from == 0,
                    false,
                    mutated,
                    terr,
                ))
            }
        }
    }
}
