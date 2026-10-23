//! Ввод-вывод датников за одним фасадом: остальной код знает только этот модуль
//! (`agr:io-facade`), а формат файла — дело бэкенда (`agr:backend-choice`).
//!
//! Бэкенд — реализация [`Driver`]: узнаёт свой файл по первым байтам, заводит
//! контекст на поток ([`DriverContext`]), из него — чтение, запись и Lua-пару, и
//! знает свой Lua-диалект (код фильтра, шаблон transform) и, по желанию, свою
//! печать сообщения для `--dump`. Встроенный бэкенд — TLL ([`crate::tll`], фича
//! `tll`); другие подключаются извне: [`register`] до запуска [`crate::app::main`].
//!
//! [`Backend`] — имя бэкенда, им помечены [`Open`] и [`Info`]. Выбор: `--backend`
//! ([`force`]) перекрывает всё; иначе — [`Open::backend`] (бэкенд уже открытого
//! файла); иначе — первый бэкенд, узнавший первые байты файла. Бэкенда нет в
//! сборке или файл никто не узнал — понятная ошибка.
//!
//! [`Context`] держит контексты бэкендов лениво: создать его дёшево, а контекст
//! библиотеки бэкенда появляется при первом файле своего формата. Поэтому код,
//! который заводит контекст на поток (`data/`, `dump`, фиксация, агент), не знает
//! и не должен знать, чей файл.
//!
//! Файлы: `dynlib.rs` — загрузка библиотек бэкендов и таблицы их функций.
//!
//! Наружу: [`Backend`], [`Driver`], [`DriverContext`], [`DriverReader`],
//! [`DriverWriter`], [`DriverTransformer`], [`register`], [`driver`], [`force`],
//! [`resolve`], [`Context`], [`Pooled`], [`Open`], [`Reader`], [`Writer`],
//! [`Transformer`], [`Msg`], [`Info`], [`LuaMode`], [`dump_line`], [`quiet`],
//! [`take_error`].

pub mod dynlib;

use std::cell::RefCell;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock, RwLock};

use crate::scheme::Scheme;

/// Имя бэкенда: `tll` и те, что зарегистрированы извне.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Backend(&'static str);

impl Backend {
    /// Встроенный бэкенд TLL (есть в сборке с фичей `tll`).
    pub const TLL: Backend = Backend("tll");

    /// Имя внешнего бэкенда — то же, что отдаёт его [`Driver::name`].
    pub const fn named(name: &'static str) -> Backend {
        Backend(name)
    }

    /// Короткое имя — для опций, сессий и сообщений.
    pub fn name(self) -> &'static str {
        self.0
    }
}

impl std::str::FromStr for Backend {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        driver_named(s)
            .map(|d| Backend(d.name()))
            .ok_or_else(|| format!("неизвестный бэкенд {s}: в этой сборке — {}", names()))
    }
}

/// Бэкенд: формат датника и всё, что с ним делается.
pub trait Driver: Send + Sync {
    /// Имя для `--backend` и [`Backend`].
    fn name(&self) -> &'static str;
    /// Его ли файл — по первым байтам (до 64).
    fn detect(&self, head: &[u8]) -> bool;
    /// Контекст на поток; ошибка — нет библиотеки бэкенда.
    fn context(&self) -> Result<Box<dyn DriverContext>, String>;
    /// Код Lua-фильтра: отбор типов `messages` и выражение `expr`.
    fn filter_code(&self, messages: &[String], expr: &str) -> String;
    /// Заготовка кода transform для редактора.
    fn transform_template(&self) -> String;
    /// Сообщение строкой для `--dump`, если у бэкенда своя печать; `None` —
    /// общий YAML.
    fn dump_line(&self, _scheme: Option<&Scheme>, _msg: &Msg) -> Option<Result<String, String>> {
        None
    }
    /// Приглушить журнал библиотеки до ошибок.
    fn quiet(&self) {}
    /// Ошибки журнала этого потока с прошлого раза.
    fn take_error(&self) -> Option<String> {
        None
    }
}

/// Контекст бэкенда на поток.
pub trait DriverContext: Send {
    /// Подгрузить Lua-линк; `false` — модуля нет.
    fn ensure_lua(&self) -> bool;
    /// Подгрузить линк правок; `false` — модуля нет.
    fn ensure_patch(&self) -> bool;
    fn reader(&self, o: &Open) -> Result<Box<dyn DriverReader>, String>;
    fn writer(
        &self,
        path: &Path,
        scheme: &str,
        like: &Info,
        overwrite: bool,
    ) -> Result<Box<dyn DriverWriter>, String>;
    /// Lua-пара: `filter` — фильтр (значения как в консоли), иначе transform.
    fn transformer(
        &self,
        code: &str,
        scheme_url: &str,
        filter: bool,
    ) -> Result<Box<dyn DriverTransformer>, String>;
}

/// Чтение файла бэкендом.
pub trait DriverReader: Send {
    fn next(&mut self) -> Option<Msg>;
    fn set_stop(&mut self, stop: Arc<AtomicBool>);
    fn error(&self) -> Option<String>;
    fn scheme(&self) -> Option<Scheme>;
    fn scheme_url(&self) -> Option<String>;
    fn info(&self) -> Info;
}

/// Запись файла бэкендом.
pub trait DriverWriter: Send {
    fn post(&mut self, m: &Msg) -> Result<(), String>;
}

/// Lua-пара бэкенда.
pub trait DriverTransformer: Send {
    fn transform(&mut self, m: &Msg) -> Result<Option<Msg>, String>;
}

static REGISTRY: RwLock<Vec<&'static dyn Driver>> = RwLock::new(Vec::new());
static BUILTIN: OnceLock<()> = OnceLock::new();

/// Подключить бэкенд (`agr:backend-plugin`). Звать до [`crate::app::main`];
/// повтор имени игнорируется.
pub fn register(d: &'static dyn Driver) {
    builtin();
    let mut r = REGISTRY.write().unwrap_or_else(|e| e.into_inner());
    if !r.iter().any(|x| x.name() == d.name()) {
        r.push(d);
    }
}

/// Встроенные бэкенды сборки — в реестр один раз.
fn builtin() {
    BUILTIN.get_or_init(|| {
        #[cfg(feature = "tll")]
        REGISTRY
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .push(&crate::tll::driver::Tll);
    });
}

fn drivers() -> Vec<&'static dyn Driver> {
    builtin();
    REGISTRY.read().unwrap_or_else(|e| e.into_inner()).clone()
}

fn driver_named(name: &str) -> Option<&'static dyn Driver> {
    drivers().into_iter().find(|d| d.name() == name)
}

fn names() -> String {
    let n: Vec<_> = drivers().iter().map(|d| d.name()).collect();
    if n.is_empty() {
        "ни одного".into()
    } else {
        n.join(", ")
    }
}

/// Реализация бэкенда `b`; нет в сборке — ошибка с перечнем имеющихся.
pub fn driver(b: Backend) -> Result<&'static dyn Driver, String> {
    driver_named(b.name())
        .ok_or_else(|| format!("бэкенда {} нет в этой сборке (есть: {})", b.name(), names()))
}

static FORCED: OnceLock<Backend> = OnceLock::new();

/// `--backend`: все файлы процесса читаются этим бэкендом. Звать один раз в
/// начале `main`; повторный вызов игнорируется.
pub fn force(b: Backend) {
    let _ = FORCED.set(b);
}

/// Бэкенд для файла: `--backend`, иначе подсказка вызывающего, иначе первый
/// бэкенд, узнавший начало файла. Нет в сборке или никто не узнал — ошибка.
pub fn resolve(path: &Path, hint: Option<Backend>) -> Result<Backend, String> {
    if let Some(b) = FORCED.get().copied().or(hint) {
        return driver(b).map(|_| b);
    }
    let mut head = Vec::with_capacity(64);
    std::fs::File::open(path)
        .and_then(|f| f.take(64).read_to_end(&mut head))
        .map_err(|e| format!("не открылся файл {}: {e}", path.display()))?;
    drivers()
        .into_iter()
        .find(|d| d.detect(&head))
        .map(|d| Backend(d.name()))
        .ok_or_else(|| {
            format!(
                "не понять формат {}: укажите --backend (в этой сборке — {})",
                path.display(),
                names()
            )
        })
}

/// Режим Lua-канала.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LuaMode {
    /// Только отбор сообщений. Остался для `--dump` (печать как консольный
    /// фильтр); экранный фильтр и поиск им не пользуются.
    Filter,
    /// Получает каждое сообщение, может менять и слать дальше.
    Transform,
}

/// Что открыть.
#[derive(Debug, Clone, Default)]
pub struct Open {
    pub path: PathBuf,
    /// Бэкенд, если известен заранее; `None` — по файлу ([`resolve`]).
    pub backend: Option<Backend>,
    /// Первое сообщение — с seq не меньше этого; `None` — с начала файла.
    pub seq: Option<i64>,
    /// Код Lua для префикса `lua+` (transform — [`LuaMode::Transform`], либо
    /// фильтр для `--dump`).
    pub lua: Option<String>,
    /// Режим Lua-канала; `None` при `lua` — Filter (как у `--dump`).
    pub lua_mode: Option<LuaMode>,
    /// Не копировать тела: индексу хватает seq и msgid.
    pub meta_only: bool,
    /// yaml правок — читать через префикс `patch+`.
    pub patch: Option<PathBuf>,
}

impl Open {
    pub fn file(path: impl AsRef<Path>) -> Self {
        Open {
            path: path.as_ref().to_path_buf(),
            ..Default::default()
        }
    }
    pub fn backend(mut self, b: Backend) -> Self {
        self.backend = Some(b);
        self
    }
    pub fn at(mut self, seq: i64) -> Self {
        self.seq = Some(seq);
        self
    }
    pub fn filter(mut self, code: Option<String>) -> Self {
        self.lua = code;
        self
    }
    pub fn meta(mut self) -> Self {
        self.meta_only = true;
        self
    }
    pub fn patch(mut self, yaml: Option<PathBuf>) -> Self {
        self.patch = yaml;
        self
    }
}

/// Сообщение из файла: тело скопировано, канал его не держит.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Msg {
    pub seq: i64,
    pub msgid: i32,
    pub data: Vec<u8>,
}

/// Сведения о файле.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Info {
    /// Чем файл прочитан — им же пишется новый.
    pub backend: Backend,
    /// Первый и последний seq; `None` — файл пуст.
    pub seq_begin: Option<i64>,
    pub seq_last: Option<i64>,
    pub block: Option<String>,
    pub compression: Option<String>,
    /// Версия формата файла — новый файл пишется той же.
    pub version: Option<String>,
}

/// Контексты бэкендов на поток; каждый создаётся при первой надобности.
#[derive(Default)]
pub struct Context {
    ctxs: RefCell<Vec<(Backend, Box<dyn DriverContext>)>>,
}

impl Context {
    pub fn new() -> Self {
        Self::default()
    }

    /// Сделать `f` с контекстом бэкенда `b`, заведя его при первой надобности.
    fn with<T>(
        &self,
        b: Backend,
        f: impl FnOnce(&dyn DriverContext) -> Result<T, String>,
    ) -> Result<T, String> {
        if !self.ctxs.borrow().iter().any(|(x, _)| *x == b) {
            let c = driver(b)?.context()?;
            self.ctxs.borrow_mut().push((b, c));
        }
        let ctxs = self.ctxs.borrow();
        let (_, c) = ctxs.iter().find(|(x, _)| *x == b).unwrap();
        f(c.as_ref())
    }

    /// Подгрузить Lua-линк бэкенда; `false` — модуля нет.
    pub fn ensure_lua(&self, b: Backend) -> bool {
        self.with(b, |c| Ok(c.ensure_lua())).unwrap_or(false)
    }

    /// Подгрузить линк правок бэкенда; `false` — модуля нет.
    pub fn ensure_patch(&self, b: Backend) -> bool {
        self.with(b, |c| Ok(c.ensure_patch())).unwrap_or(false)
    }
}

/// Контекст из общего пула: схема файла, разобранная при первом открытии,
/// кэшируется в контексте бэкенда, и повторные сканы её не разбирают заново.
pub struct Pooled(Option<Context>);

static POOL: std::sync::Mutex<Vec<Context>> = std::sync::Mutex::new(Vec::new());

impl Context {
    pub fn pooled() -> Pooled {
        let c = POOL.lock().unwrap_or_else(|e| e.into_inner()).pop();
        Pooled(Some(c.unwrap_or_default()))
    }
}

impl std::ops::Deref for Pooled {
    type Target = Context;
    fn deref(&self) -> &Context {
        self.0.as_ref().unwrap()
    }
}

impl Drop for Pooled {
    fn drop(&mut self) {
        if let Some(c) = self.0.take() {
            POOL.lock().unwrap_or_else(|e| e.into_inner()).push(c);
        }
    }
}

/// Открытый канал чтения.
pub struct Reader(Box<dyn DriverReader>);

impl Reader {
    pub fn open(ctx: &Context, o: &Open) -> Result<Reader, String> {
        let b = resolve(&o.path, o.backend)?;
        ctx.with(b, |c| c.reader(o)).map(Reader)
    }

    /// Флаг отмены: взведён — [`Reader::next`] сразу отдаёт `None`.
    pub fn with_stop(mut self, stop: Arc<AtomicBool>) -> Self {
        self.0.set_stop(stop);
        self
    }

    /// Канал оборвался ошибкой (у Lua-фильтра — ошибкой выражения): текст.
    pub fn error(&self) -> Option<String> {
        self.0.error()
    }

    /// Схема данных канала.
    pub fn scheme(&self) -> Option<Scheme> {
        self.0.scheme()
    }

    /// Схема данных строкой url — для канала записи и Lua-пары.
    pub fn scheme_url(&self) -> Option<String> {
        self.0.scheme_url()
    }

    /// Первый/последний seq и параметры файла.
    pub fn info(&self) -> Info {
        self.0.info()
    }
}

impl Iterator for Reader {
    type Item = Msg;
    fn next(&mut self) -> Option<Msg> {
        self.0.next()
    }
}

/// Открытый канал записи файла.
pub struct Writer(Box<dyn DriverWriter>);

impl Writer {
    /// Открыть файл `path` на запись со схемой `scheme` ([`Reader::scheme_url`])
    /// и параметрами исходника `like` — тем же бэкендом, что исходник.
    /// `overwrite` — писать заново, иначе дописывать к хвосту.
    pub fn open(
        ctx: &Context,
        path: &Path,
        scheme: &str,
        like: &Info,
        overwrite: bool,
    ) -> Result<Writer, String> {
        ctx.with(like.backend, |c| c.writer(path, scheme, like, overwrite))
            .map(Writer)
    }

    /// Создать файл `path` (существующий перезаписывается).
    pub fn create(ctx: &Context, path: &Path, scheme: &str, like: &Info) -> Result<Writer, String> {
        Writer::open(ctx, path, scheme, like, true)
    }

    /// Дописать сообщение.
    pub fn post(&mut self, m: &Msg) -> Result<(), String> {
        self.0.post(m)
    }
}

/// Прогон тел через Lua без чтения файла (`agr:transform-shadow`,
/// `agr:filter-post-eval`).
pub struct Transformer(Box<dyn DriverTransformer>);

impl Transformer {
    /// Transform: обработчик сообщения меняет тела; fixed/decimal128 — объектами.
    pub fn new(ctx: &Context, b: Backend, code: &str, scheme_url: &str) -> Result<Self, String> {
        ctx.with(b, |c| c.transformer(code, scheme_url, false))
            .map(Transformer)
    }

    /// Фильтр: выражение видит те же значения, что консольный фильтр бэкенда.
    pub fn filter(ctx: &Context, b: Backend, code: &str, scheme_url: &str) -> Result<Self, String> {
        ctx.with(b, |c| c.transformer(code, scheme_url, true))
            .map(Transformer)
    }

    /// Прогнать тело: `None` — сообщение выброшено, `Err` — ошибка канала.
    pub fn transform(&mut self, m: &Msg) -> Result<Option<Msg>, String> {
        self.0.transform(m)
    }
}

/// Сообщение строкой для `--dump`, если у бэкенда `b` своя печать.
pub fn dump_line(b: Backend, scheme: Option<&Scheme>, m: &Msg) -> Option<Result<String, String>> {
    driver(b).ok()?.dump_line(scheme, m)
}

/// Приглушить журналы бэкендов сборки до ошибок.
pub fn quiet() {
    for d in drivers() {
        d.quiet();
    }
}

/// Последняя ошибка журнала этого потока (любого бэкенда) — сырой текст.
pub fn take_error() -> Option<String> {
    let all: Vec<String> = drivers().iter().filter_map(|d| d.take_error()).collect();
    (!all.is_empty()).then(|| all.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `agr:backend-choice` — имя бэкенда сборки разбирается из опции, чужое — ошибка
    /// с перечнем имеющихся.
    #[test]
    fn backend_names_are_checked_against_the_build() {
        for d in drivers() {
            assert_eq!(d.name().parse::<Backend>().unwrap().name(), d.name());
        }
        let e = "xx".parse::<Backend>().unwrap_err();
        assert!(e.contains("в этой сборке"), "{e}");
        let e = driver(Backend("xx")).err().unwrap();
        assert!(e.contains("нет в этой сборке"), "{e}");
    }

    /// Подставной внешний бэкенд: файл с сигнатурой `FAKE`, два сообщения.
    struct Fake;
    struct FakeCtx;
    struct FakeReader(Vec<Msg>);

    impl Driver for Fake {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn detect(&self, head: &[u8]) -> bool {
            head.starts_with(b"FAKE")
        }
        fn context(&self) -> Result<Box<dyn DriverContext>, String> {
            Ok(Box::new(FakeCtx))
        }
        fn filter_code(&self, _: &[String], expr: &str) -> String {
            format!("fake({expr})")
        }
        fn transform_template(&self) -> String {
            "fake template".into()
        }
        fn dump_line(&self, _: Option<&Scheme>, m: &Msg) -> Option<Result<String, String>> {
            Some(Ok(format!("fake seq {}", m.seq)))
        }
    }

    impl DriverContext for FakeCtx {
        fn ensure_lua(&self) -> bool {
            true
        }
        fn ensure_patch(&self) -> bool {
            false
        }
        fn reader(&self, _: &Open) -> Result<Box<dyn DriverReader>, String> {
            let m = |seq| Msg {
                seq,
                msgid: 1,
                data: vec![],
            };
            Ok(Box::new(FakeReader(vec![m(2), m(1)])))
        }
        fn writer(
            &self,
            _: &Path,
            _: &str,
            _: &Info,
            _: bool,
        ) -> Result<Box<dyn DriverWriter>, String> {
            Err("только чтение".into())
        }
        fn transformer(
            &self,
            _: &str,
            _: &str,
            _: bool,
        ) -> Result<Box<dyn DriverTransformer>, String> {
            Err("без Lua".into())
        }
    }

    impl DriverReader for FakeReader {
        fn next(&mut self) -> Option<Msg> {
            self.0.pop()
        }
        fn set_stop(&mut self, _: Arc<AtomicBool>) {}
        fn error(&self) -> Option<String> {
            None
        }
        fn scheme(&self) -> Option<Scheme> {
            None
        }
        fn scheme_url(&self) -> Option<String> {
            None
        }
        fn info(&self) -> Info {
            Info {
                backend: Backend("fake"),
                ..Default::default()
            }
        }
    }

    /// `agr:backend-plugin` — внешний бэкенд после [`register`] узнаёт свой файл,
    /// читает его, даёт свой Lua-диалект и свою печать `--dump`.
    #[test]
    fn registered_backend_reads_its_files() {
        static FAKE: Fake = Fake;
        register(&FAKE);
        let dir = std::env::temp_dir().join(format!("mutator-fake-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x.dat");
        std::fs::write(&p, b"FAKE....").unwrap();
        let b = resolve(&p, None).unwrap();
        assert_eq!(b.name(), "fake");
        assert_eq!("fake".parse::<Backend>().unwrap(), b);
        let r = Reader::open(&Context::new(), &Open::file(&p)).unwrap();
        assert_eq!(r.info().backend, b);
        let seqs: Vec<i64> = r.map(|m| m.seq).collect();
        assert_eq!(seqs, [1, 2]);
        let f = crate::lua::Filter::expr("x");
        assert_eq!(f.expr_code_for(b).unwrap(), "fake(x)");
        assert_eq!(crate::lua::transform_template_for(b), "fake template");
        let m = Msg {
            seq: 7,
            msgid: 1,
            data: vec![],
        };
        assert_eq!(dump_line(b, None, &m).unwrap().unwrap(), "fake seq 7");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// `agr:backend-choice` — формат, который не узнал ни один бэкенд, просит
    /// `--backend`.
    #[test]
    fn unknown_format_asks_for_backend_option() {
        let dir = std::env::temp_dir().join(format!("mutator-detect-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("junk.dat");
        std::fs::write(&p, b"junk junk junk").unwrap();
        let e = resolve(&p, None).unwrap_err();
        assert!(e.contains("--backend"), "{e}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
