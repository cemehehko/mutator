//! libtll: чтение датника file-каналом, по желанию — через Lua.
//!
//! [`Context`] — контекст каналов; у каждого потока свой: libtll не обещает
//! потокобезопасность одного контекста. [`Reader`] открывает `file://` с нужного
//! seq и отдаёт сообщения по одному. Поверх file-канала встают префиксы:
//! `lua+` — transform ([`LuaMode::Transform`], зовёт `tll_on_data`, меняет тела);
//! ручные правки накладывает фасад поверх этого потока (`agr:patch-overlay`),
//! поэтому ручная правка ложится поверх мутации скрипта. [`LuaMode::Filter`] (`lua.preset=filter`,
//! `tll_filter`) остался только для `--dump` (печать как `tll-read -f`):
//! экранный фильтр и поиск больше не ходят префиксом — они пост-эвал через
//! [`Transformer::filter`] (`agr:filter-post-eval`). Цикл опроса свой: обходим
//! дерево каналов и зовём `process` у тех, кому он нужен (`DCAPS_PROCESS`) — так
//! работает и префикс `lua+` с дочерним файлом.
//!
//! Файлы: `log.rs` — ошибки журнала libtll в текст, `sys.rs` — сырые объявления C API, `convert.rs` — перенос
//! `tll_scheme_t` в [`crate::scheme::Scheme`], `write.rs` — запись нового файла,
//! `transform.rs` — прогон тел через Lua без чтения (`lua+direct`), `driver.rs` —
//! TLL как бэкенд фасада (`Driver`), `link.rs` — линк: любой канал по url
//! (`resolve://`, `stream+`), его состояние и сообщения, `resolve.rs` —
//! протокол resolve-сервера: список экспортов.
//!
//! Бэкенд TLL фасада [`crate::io`]: общие типы ([`Open`], [`Msg`], [`Info`],
//! [`LuaMode`]) объявлены там, здесь — только их чтение и запись libtll.
//!
//! Наружу: [`Context`], [`Reader`], [`Writer`], [`Transformer`], [`driver`], [`quiet`],
//! [`load_scheme`], [`log`], `lifecycle` (внутри модуля).

mod convert;
pub mod driver;
pub mod link;
pub mod log;
pub mod resolve;
pub mod sys;
mod transform;
mod write;

pub use transform::Transformer;

pub use write::Writer;

use std::collections::VecDeque;
use std::ffi::{CStr, CString};
use std::os::raw::{c_int, c_void};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

pub use crate::io::{Info, LuaMode, Msg, Open};
use crate::scheme::Scheme;
use sys::*;

/// Загрузить схему по url libtll (`yamls://…`, `yaml://файл`) — для тестов и
/// схем не из файла данных.
pub fn load_scheme(url: &str) -> Option<Scheme> {
    let _g = lifecycle();
    unsafe {
        let p = tll_scheme_load(url.as_ptr().cast(), url.len() as c_int);
        if p.is_null() {
            return None;
        }
        let s = convert::scheme(&*p);
        tll_scheme_unref(p);
        Some(s)
    }
}

/// Приглушить журнал libtll до ошибок: info-строки на каждое открытие канала
/// мешают и выводу `--dump`, и терминалу.
pub fn quiet() {
    // Нет libtll — нечего приглушать: TLL-файлы откроются с ошибкой загрузки.
    if sys::api().is_err() {
        return;
    }
    let name = b"tll";
    unsafe {
        tll_logger_set(
            name.as_ptr().cast(),
            name.len() as c_int,
            TLL_LOGGER_ERROR,
            1,
        )
    };
}

/// Контекст каналов libtll. Один на поток.
pub struct Context {
    ptr: *mut tll_channel_context_t,
}

// Контекст можно передать в другой поток целиком, но не делить.
unsafe impl Send for Context {}

impl Context {
    /// Контекст или текст ошибки — для фасада [`crate::io`].
    pub fn try_new() -> Result<Self, String> {
        sys::api()?;
        let _g = lifecycle();
        let ptr = unsafe { tll_channel_context_new(std::ptr::null_mut()) };
        if ptr.is_null() {
            return Err("не создался контекст libtll".into());
        }
        Ok(Context { ptr })
    }

    /// Загрузить модуль каналов (`.../tll-lua` — путь без `lib` и расширения).
    pub fn load(&self, module: &str) -> Result<(), String> {
        let m = CString::new(module).map_err(|e| e.to_string())?;
        let _g = lifecycle();
        let r = unsafe {
            tll_channel_module_load_cfg(self.ptr, m.as_ptr(), c"".as_ptr(), std::ptr::null())
        };
        if r != 0 {
            return Err(format!("не загрузился модуль {module}"));
        }
        Ok(())
    }

    /// Подгрузить tll-lua, если `lua+` ещё нет: путь — `MUTATOR_LUA`
    /// (`.../build/tll-lua`), иначе системный `tll-lua`.
    pub fn ensure_lua(&self) -> bool {
        if self.has_impl("lua+") {
            return true;
        }
        let module = std::env::var("MUTATOR_LUA").unwrap_or_else(|_| "tll-lua".into());
        self.load(&module).is_ok() && self.has_impl("lua+")
    }

    /// Есть ли реализация канала (`lua+`).
    pub fn has_impl(&self, name: &str) -> bool {
        let n = CString::new(name).unwrap();
        !unsafe { tll_channel_impl_get(self.ptr, n.as_ptr()) }.is_null()
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        let _g = lifecycle();
        unsafe { tll_channel_context_free(self.ptr) };
    }
}

#[derive(Default)]
struct Pump {
    queue: VecDeque<Msg>,
    done: bool,
    /// Канал ушёл в Error: данные кончились не сами.
    failed: bool,
    meta_only: bool,
}

/// Открытый канал чтения.
pub struct Reader {
    ctx: *mut tll_channel_context_t,
    ch: *mut tll_channel_t,
    pump: Box<Pump>,
    stop: Option<Arc<AtomicBool>>,
}

unsafe impl Send for Reader {}

static NAMES: AtomicU64 = AtomicU64::new(0);

/// Имя канала, свободное в любом контексте.
fn next_name() -> u64 {
    NAMES.fetch_add(1, Ordering::Relaxed)
}

/// libtll считает ссылки логгеров неатомарно (`refbase_t<tll_logger_obj_t, 0,
/// false>`): создание и удаление каналов и контекстов из разных потоков портит
/// память. Поэтому жизненный цикл — под этим замком, а чтение идёт параллельно
/// (`agr:tll-lifecycle-lock`).
static LIFECYCLE: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) fn lifecycle() -> std::sync::MutexGuard<'static, ()> {
    LIFECYCLE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Сколько пустых проходов подряд считать концом данных: файл без `autoclose`
/// (растущий `current.dat`) иначе крутил бы цикл вечно.
const IDLE_PASSES: usize = 3;

unsafe extern "C" fn on_msg(
    _c: *const tll_channel_t,
    msg: *const tll_msg_t,
    user: *mut c_void,
) -> c_int {
    let pump = &mut *(user as *mut Pump);
    let m = &*msg;
    match m.type_ {
        TLL_MESSAGE_DATA => {
            let data = if pump.meta_only || m.size == 0 || m.data.is_null() {
                Vec::new()
            } else {
                std::slice::from_raw_parts(m.data as *const u8, m.size).to_vec()
            };
            pump.queue.push_back(Msg {
                seq: m.seq,
                msgid: m.msgid,
                data,
            });
        }
        TLL_MESSAGE_STATE => {
            if matches!(
                m.msgid,
                TLL_STATE_CLOSED | TLL_STATE_ERROR | TLL_STATE_DESTROY
            ) {
                pump.done = true;
            }
            if m.msgid == TLL_STATE_ERROR {
                pump.failed = true;
            }
        }
        _ => {}
    }
    0
}

/// Начальные параметры канала: адрес файла, имя и общие флаги.
fn base_kv(path: &str, n: u64) -> Vec<(&'static str, String)> {
    vec![
        ("tll.host", path.to_string()),
        ("name", format!("tp{n}")),
        ("dump", "no".into()),
        ("autoclose", "yes".into()),
    ]
}

/// Параметры Lua-префикса для кода и режима: `preset=filter` только в режиме
/// `Filter` (или когда режим не задан); `Transform` зовёт `tll_on_data` без
/// пресета, чтобы можно было менять сообщения.
fn lua_kv(code: &str, mode: Option<LuaMode>) -> Vec<(&'static str, String)> {
    let mut kv = vec![("lua.code", code.to_string())];
    if mode != Some(LuaMode::Transform) {
        kv.push(("lua.preset", "filter".to_string()));
    }
    kv
}

impl Reader {
    pub fn open(ctx: &Context, o: &Open) -> Result<Reader, String> {
        log::install();
        log::take();
        let path = o.path.to_str().ok_or("путь не в UTF-8")?;
        let n = next_name();
        let mut kv = base_kv(path, n);
        let mut proto = String::from("file");
        if let Some(code) = &o.lua {
            if !ctx.ensure_lua() {
                return Err("Lua недоступен: нет модуля tll-lua (MUTATOR_LUA)".into());
            }
            kv.extend(lua_kv(code, o.lua_mode));
            proto = format!("lua+{proto}");
        }
        kv.push(("tll.proto", proto.clone()));
        let _g = lifecycle();
        let ch = unsafe { new_channel(ctx, &kv) };
        if ch.is_null() {
            return Err(format!("не создался канал {proto}://{path}"));
        }
        let mut r = Reader {
            ctx: unsafe { tll_channel_context_ref(ctx.ptr) },
            ch,
            pump: Box::default(),
            stop: None,
        };
        r.pump.meta_only = o.meta_only;
        let user = (&mut *r.pump) as *mut Pump as *mut c_void;
        unsafe {
            tll_channel_callback_add(
                ch,
                on_msg,
                user,
                TLL_MESSAGE_MASK_DATA | TLL_MESSAGE_MASK_STATE,
            );
        }
        let params = o.seq.map(|s| format!("seq={s}")).unwrap_or_default();
        let rc = unsafe { tll_channel_open(ch, params.as_ptr().cast(), params.len()) };
        let state = unsafe { tll_channel_state(ch) };
        // Замок снимается до `r`: его `Drop` берёт тот же замок, и ранний
        // возврат с ошибкой иначе заблокировал бы поток сам на себе.
        drop(_g);
        if rc != 0 || state == TLL_STATE_ERROR {
            return Err(match log::take() {
                Some(e) if o.lua.is_some() => log::explain(&e),
                Some(e) => e,
                None => format!("не открылся файл {path}"),
            });
        }
        Ok(r)
    }

    /// Флаг отмены: взведён — [`Reader::next`] сразу отдаёт `None`. Нужен
    /// сканам с Lua-фильтром: без совпадений `next` иначе дочитал бы файл.
    pub fn with_stop(mut self, stop: Arc<AtomicBool>) -> Self {
        self.stop = Some(stop);
        self
    }

    /// Канал оборвался ошибкой (у Lua-фильтра — ошибкой выражения): текст.
    pub fn error(&self) -> Option<String> {
        self.pump.failed.then(|| {
            log::take()
                .map(|e| log::explain(&e))
                .unwrap_or_else(|| "канал оборвался ошибкой".into())
        })
    }

    fn stopped(&self) -> bool {
        self.stop
            .as_ref()
            .is_some_and(|s| s.load(Ordering::Relaxed))
    }

    /// Схема данных канала.
    pub fn scheme(&self) -> Option<Scheme> {
        let s = unsafe { tll_channel_scheme(self.ch, TLL_MESSAGE_DATA as c_int) };
        if s.is_null() {
            return None;
        }
        Some(unsafe { convert::scheme(&*s) })
    }

    /// Схема данных строкой url (`yamls+gz://…`) — для канала записи.
    pub fn scheme_url(&self) -> Option<String> {
        unsafe {
            let s = tll_channel_scheme(self.ch, TLL_MESSAGE_DATA as c_int);
            if s.is_null() {
                return None;
            }
            let p = tll_scheme_dump(s, c"yamls+gz".as_ptr());
            if p.is_null() {
                return None;
            }
            let r = CStr::from_ptr(p).to_string_lossy().into_owned();
            free(p.cast());
            Some(r)
        }
    }

    /// Первый/последний seq и параметры файла — у самого file-канала (у
    /// префикса `lua+` — у его ребёнка).
    pub fn info(&self) -> Info {
        let file = unsafe { file_channel(self.ch) };
        let get = |k: &str| unsafe { config_get(file, k) };
        Info {
            backend: crate::io::Backend::TLL,
            seq_begin: get("info.seq-begin").and_then(|s| s.parse().ok()),
            seq_last: get("info.seq").and_then(|s| s.parse().ok()),
            block: get("info.block"),
            compression: get("info.compression"),
            version: get("info.version"),
        }
    }
}

/// Сообщения по одному; `None` — данные кончились, канал закрылся или скан отменён.
impl Iterator for Reader {
    type Item = Msg;

    fn next(&mut self) -> Option<Msg> {
        let mut idle = 0;
        while self.pump.queue.is_empty() && !self.pump.done {
            if self.stopped() {
                return None;
            }
            let before = self.pump.queue.len();
            let progressed = unsafe { process_tree(self.ch) };
            if !progressed && self.pump.queue.len() == before {
                idle += 1;
                if idle >= IDLE_PASSES {
                    break;
                }
            } else {
                idle = 0;
            }
        }
        self.pump.queue.pop_front()
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        let _g = lifecycle();
        unsafe {
            tll_channel_free(self.ch);
            tll_channel_context_free(self.ctx);
        }
    }
}

/// Канал по параметрам url (`tll.proto`, `tll.host`, …). Звать под [`lifecycle`].
unsafe fn new_channel(ctx: &Context, kv: &[(&str, String)]) -> *mut tll_channel_t {
    let cfg = tll_config_new();
    for (k, v) in kv {
        tll_config_set(
            cfg,
            k.as_ptr().cast(),
            k.len() as c_int,
            v.as_ptr().cast(),
            v.len() as c_int,
        );
    }
    let ch = tll_channel_new_url(ctx.ptr, cfg, std::ptr::null_mut(), std::ptr::null());
    tll_config_unref(cfg);
    ch
}

/// Самый глубокий канал дерева — это file под префиксами.
unsafe fn file_channel(mut c: *mut tll_channel_t) -> *mut tll_channel_t {
    loop {
        let l = tll_channel_children(c);
        if l.is_null() || (*l).channel.is_null() {
            return c;
        }
        c = (*l).channel;
    }
}

unsafe fn config_get(c: *mut tll_channel_t, key: &str) -> Option<String> {
    let cfg = tll_channel_config(c);
    if cfg.is_null() {
        return None;
    }
    let mut len: c_int = 0;
    let v = tll_config_get_copy(cfg, key.as_ptr().cast(), key.len() as c_int, &mut len);
    let r = if v.is_null() {
        None
    } else {
        Some(CStr::from_ptr(v).to_string_lossy().into_owned())
    };
    if !v.is_null() {
        tll_config_value_free(v);
    }
    tll_config_unref(cfg);
    r
}

/// Один проход опроса по дереву: `true`, если кто-то что-то сделал.
unsafe fn process_tree(c: *mut tll_channel_t) -> bool {
    let mut any = false;
    let internal = (*c).internal;
    if !internal.is_null() {
        let dcaps = (*internal).dcaps;
        if dcaps & (TLL_DCAPS_PROCESS | TLL_DCAPS_SUSPEND) == TLL_DCAPS_PROCESS {
            // EAGAIN — «данных нет», всё прочее — работа или ошибка, которая
            // придёт сообщением о состоянии.
            let r = tll_channel_process(c, 0, 0);
            any |= r != libc_eagain();
        }
    }
    let mut l = tll_channel_children(c);
    while !l.is_null() {
        if !(*l).channel.is_null() {
            any |= process_tree((*l).channel);
        }
        l = (*l).next;
    }
    any
}

#[cfg(target_os = "macos")]
fn libc_eagain() -> c_int {
    35
}
#[cfg(not(target_os = "macos"))]
fn libc_eagain() -> c_int {
    11
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Transform-режим не ставит `lua.preset=filter`: tll-lua зовёт `tll_on_data`
    /// (меняет сообщения), а не `tll_filter`.
    #[test]
    fn transform_mode_omits_filter_preset() {
        assert_eq!(
            lua_kv("x", Some(LuaMode::Transform)),
            vec![("lua.code", "x".to_string())],
            "Transform — без preset=filter"
        );
        assert_eq!(
            lua_kv("x", Some(LuaMode::Filter)),
            vec![
                ("lua.code", "x".to_string()),
                ("lua.preset", "filter".to_string()),
            ],
            "Filter — с preset=filter"
        );
        assert_eq!(
            lua_kv("x", None),
            vec![
                ("lua.code", "x".to_string()),
                ("lua.preset", "filter".to_string()),
            ],
            "режим не задан — по умолчанию filter"
        );
    }
}
