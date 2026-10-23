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
//! `patch+file`, но с transform поверх файла; тела едут в вид уже прогоными —
//! как на фиксации, — а рядом с ними [`Mutations`] (что изменилось, значение
//! в файле) и [`Ev::TransformError`] (`agr:transform-shadow`). Диф мутатора
//! считается второй читкой: те же seq читаются сырыми из файла и прогоняются
//! через [`Transformer`] (пара `lua+direct`). Фильтр и поиск работают по тому
//! же правленному потоку `patch+lua+file`: типы отбираются в Rust по `msgid`,
//! выражение `f`/`t` — пост-эвалом через [`Transformer::filter`]
//! (`agr:filter-post-eval`). Проверка правки — по тому же потоку, что читает
//! вид (правка ложится поверх мутации, как на фиксации); масс-скан — по сырому
//! потоку: найденный seq — файловый. Фиксация — отдельно и по-прежнему через
//! полный `patch+[lua+]file://` (ручная правка поверх мутации скрипта).
//!
//! Наружу: [`File`], [`Worker`], [`Cmd`], [`Ev`], [`View`], [`At`], [`Edge`],
//! [`Index`], [`Matches`], [`Mutations`], [`fetch`], [`filter`], [`search`].
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
use crate::edit::commit::{self, Target};
use crate::io::{Context, Info, LuaMode, Msg, Open, Reader, Transformer};
use crate::lua::Filter;
use crate::scheme::Scheme;

/// seq → пути полей, изменившиеся после мутатора: путь → значение в файле, до
/// мутатора (`agr:transform-shadow`). Пустая карта у seq — тело менялось, но
/// полей не нашли (новое тело не разобралось).
pub type Mutations = BTreeMap<i64, BTreeMap<String, String>>;

/// Открытый файл: путь, границы seq, схема.
#[derive(Debug, Clone)]
pub struct File {
    pub path: PathBuf,
    pub info: Info,
    pub scheme: Arc<Scheme>,
    /// Схема данных строкой url (`yamls+gz://…`) — мутатору для имён полей.
    pub scheme_url: Option<String>,
    /// yaml правок, через который файл показывается; `None` — как есть.
    pub patch: Option<PathBuf>,
    /// Lua-код (фильтр или transform).
    pub lua: Option<String>,
    /// Режим Lua-канала; `None` при `lua` — по умолчанию Filter.
    pub lua_mode: Option<LuaMode>,
}

impl File {
    pub fn open(path: &Path) -> Result<File, String> {
        let ctx = Context::new();
        let r = Reader::open(&ctx, &Open::file(path))?;
        Ok(File {
            path: path.to_path_buf(),
            info: r.info(),
            scheme: Arc::new(r.scheme().unwrap_or_default()),
            scheme_url: r.scheme_url(),
            patch: None,
            lua: None,
            lua_mode: None,
        })
    }

    /// Что открывать для чтения: файл с правками, без Lua — вид показывает файл
    /// как есть (`agr:transform-shadow`).
    pub fn source(&self) -> Open {
        Open::file(&self.path)
            .backend(self.info.backend)
            .patch(self.patch.clone())
    }

    /// Что открывать для чтения тел в виде: тот же `patch+file`, но с transform
    /// поверх файла — тела приходят окончательными, как уйдут в файл на
    /// фиксации (`agr:transform-shadow`). Порядок префиксов — `patch+lua+file`:
    /// ручная правка ложится поверх мутации скрипта.
    pub fn source_transform(&self) -> Open {
        let mut o = Open::file(&self.path).backend(self.info.backend);
        if self.transforming() {
            o.lua = self.lua.clone();
            o.lua_mode = self.lua_mode;
        }
        o.patch(self.patch.clone())
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
    /// Читать дальше через этот yaml правок (`None` — без правок). Окна и
    /// фильтр после этого вид запрашивает заново.
    Patch(Option<PathBuf>),
    /// Установить Lua-код и режим; `None` для `lua` — убрать.
    Lua {
        lua: Option<String>,
        lua_mode: Option<LuaMode>,
    },
    /// Прочитать сообщение `seq` через кандидат-yaml `patch`: примется ли правка.
    Check { id: u64, patch: PathBuf, seq: i64 },
    /// Записать файл через правки `patch` в `target` — в фоне, с прогрессом.
    Commit {
        patch: Option<PathBuf>,
        target: Target,
        /// Код Lua-transform — применить его же при записи, а не только чтении.
        lua_code: Option<String>,
        lua_mode: Option<LuaMode>,
    },
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
    /// Ответ на [`Cmd::Check`]: сообщение с правкой или ошибка tll-patch.
    Checked {
        id: u64,
        result: Result<Msg, String>,
    },
    CommitProgress(f32),
    /// Фиксация закончилась: путь результата или ошибка (исходник цел).
    Committed(Result<PathBuf, String>),
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
    let (file, tx, notify, slot) = (file.clone(), tx.clone(), notify.clone(), slot.clone());
    std::thread::spawn(move || {
        let stop = Arc::new(AtomicBool::new(false));
        let mut last = 0.0;
        let r = index::build(&file.path, file.begin(), file.last(), &stop, |p| {
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
                    let (file, tx, notify, stop) =
                        (file.clone(), tx.clone(), notify.clone(), job.stop.clone());
                    std::thread::spawn(move || {
                        scan::filter(&file, &filter, file.begin(), file.last(), &stop, &m, || {
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
                        let r = scan::search(
                            &file,
                            &filter,
                            from,
                            back,
                            file.begin(),
                            file.last(),
                            &stop,
                        );
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
                Cmd::Patch(p) => file.patch = p,
                Cmd::Lua { lua, lua_mode } => {
                    file.lua = lua;
                    file.lua_mode = lua_mode;
                    report_tx_err(&tx, &notify, &mut tx_err, None);
                }
                Cmd::Check { id, patch, seq } => {
                    let src = file.source_transform().patch(Some(patch));
                    let result = check(&ctx, &src, seq);
                    emit(&tx, &notify, Ev::Checked { id, result });
                }
                Cmd::Commit {
                    patch,
                    target,
                    lua_code,
                    lua_mode,
                } => {
                    let job = Job::new();
                    let (src, tx, notify, stop) = (
                        file.path.clone(),
                        tx.clone(),
                        notify.clone(),
                        job.stop.clone(),
                    );
                    let code = lua_code.clone();
                    let mode = lua_mode;
                    std::thread::spawn(move || {
                        let r = commit::run(
                            &src,
                            patch.as_deref(),
                            &target,
                            &stop,
                            code.as_deref(),
                            mode,
                            |p| emit(&tx, &notify, Ev::CommitProgress(p)),
                        );
                        emit(&tx, &notify, Ev::Committed(r));
                    });
                    commit_job = Some(job);
                }
                Cmd::MassScan {
                    id,
                    seeds,
                    begin,
                    last,
                    field_path,
                    scheme,
                } => {
                    let src = file.source();
                    let tx = tx.clone();
                    let notify = notify.clone();
                    std::thread::spawn(move || {
                        let ctx = Context::new();
                        let mut pairs = Vec::new();
                        if let Some(seeds) = seeds {
                            for &seed in &seeds {
                                if let Some(m) = Reader::open(&ctx, &src.clone().at(seed))
                                    .ok()
                                    .and_then(|mut r| r.next())
                                {
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
                        } else if let Ok(r) = Reader::open(&ctx, &src.clone().at(begin).meta()) {
                            for m in r {
                                if m.seq > last {
                                    break;
                                }
                                let msg = scheme.message(m.msgid);
                                if msg
                                    .is_some_and(|st| scheme.field_path(st, &field_path).is_some())
                                {
                                    let name = msg
                                        .map(|s| s.name.clone())
                                        .unwrap_or_else(|| format!("#{}", m.msgid));
                                    pairs.push((m.seq, name));
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

/// Сообщение `seq` через правки `src`: ошибка tll-patch обрывает канал — её
/// текст и есть ответ (`agr:edit-checked`).
fn check(ctx: &Context, src: &Open, seq: i64) -> Result<Msg, String> {
    let mut r = Reader::open(ctx, &src.clone().at(seq))?;
    let m = r.next();
    if let Some(e) = r.error() {
        return Err(e);
    }
    // Старый tll-patch на неверном значении только пишет в журнал и отдаёт
    // сообщение без правки: ошибка журнала — тоже отказ.
    if let Some(e) = crate::io::take_error() {
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
/// читает канал чтения через `patch+lua+file`; здесь то же окно читается
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
pub(crate) fn raw_window(
    f: &File,
    load: &dyn Fn(&Open) -> Result<Vec<Msg>, String>,
) -> Result<Vec<Msg>, String> {
    load(&Open::file(&f.path).backend(f.info.backend))
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
            let load = |src: &Open| match at {
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
            let (rows, fallback_err) = read_with_fallback(f, &load)?;
            let first_row = match (idx, rows.first()) {
                (Some(i), Some(m)) => Some(fetch::row_of(
                    ctx,
                    &Open::file(&f.path).backend(f.info.backend),
                    i,
                    m.seq,
                )?),
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
            let load = |src: &Open| fetch::by_seqs(ctx, src, &m.slice(row, n));
            let (rows, fallback_err) = read_with_fallback(f, &load)?;
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

/// Прочитать окно через transform-канал (`patch+lua+file`); если transform
/// сломался на чтении (битый код, ошибка исполнения) — откатиться к сырому
/// чтению (`patch+file`) и вернуть текст ошибки, чтобы скрипт не ронял вид
/// (`agr:transform-shadow`). При выключенном transform — сразу сырое чтение.
fn read_with_fallback(
    f: &File,
    load: impl Fn(&Open) -> Result<Vec<Msg>, String>,
) -> Result<(Vec<Msg>, Option<String>), String> {
    if !f.transforming() {
        return Ok((load(&f.source())?, None));
    }
    match load(&f.source_transform()) {
        Ok(rows) => Ok((rows, None)),
        Err(e) => match load(&f.source()) {
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
    load: &dyn Fn(&Open) -> Result<Vec<Msg>, String>,
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
    load: &dyn Fn(&Open) -> Result<Vec<Msg>, String>,
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
                let load = |src: &Open| fetch::from_seq(ctx, src, edge.seq + 1, n);
                let (rows, fallback_err) = read_with_fallback(f, &load)?;
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
                let load = |src: &Open| fetch::before_seq(ctx, src, edge.seq, n, f.begin());
                let (rows, fallback_err) = read_with_fallback(f, &load)?;
                let first = edge.row.and_then(|r| r.checked_sub(rows.len()));
                let first = first.or_else(|| {
                    idx.and_then(|i| {
                        rows.first().and_then(|m| {
                            fetch::row_of(
                                ctx,
                                &Open::file(&f.path).backend(f.info.backend),
                                i,
                                m.seq,
                            )
                            .ok()
                        })
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
                let load = |src: &Open| fetch::by_seqs(ctx, src, &m.slice(from, n));
                let (rows, fallback_err) = read_with_fallback(f, &load)?;
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
                let load = |src: &Open| fetch::by_seqs(ctx, src, &m.slice(from, row - from));
                let (rows, fallback_err) = read_with_fallback(f, &load)?;
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
