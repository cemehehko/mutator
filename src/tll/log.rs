//! Ошибки libtll — в текст для экрана: свой обработчик журнала запоминает
//! последнюю ошибку потока и отдаёт всё прежнему обработчику.
//!
//! Нужно, чтобы ошибка Lua-фильтра (`AND` вместо `and`, сравнение с `nil` у
//! типа без поля) была видна, а не превращалась в пустой или обрезанный
//! результат.
//!
//! Наружу: [`install`], [`take`], [`explain`].

use std::cell::RefCell;
use std::os::raw::{c_char, c_int, c_longlong, c_void};
use std::sync::Once;

use super::sys::*;

static ONCE: Once = Once::new();
static mut PREV: *const tll_logger_impl_t = std::ptr::null();
static mut IMPL: tll_logger_impl_t = tll_logger_impl_t {
    log: Some(log),
    log_new: Some(log_new),
    log_free: Some(log_free),
    configure: Some(configure),
    release: None,
    user: std::ptr::null_mut(),
};

thread_local! {
    static LAST: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn prev() -> Option<&'static tll_logger_impl_t> {
    // SAFETY: PREV пишется один раз в `install` до регистрации обработчика.
    unsafe { (*std::ptr::addr_of!(PREV)).as_ref() }
}

unsafe extern "C" fn log(
    ts: c_longlong,
    cat: *const c_char,
    level: c_int,
    data: *const c_char,
    size: usize,
    obj: *mut c_void,
) -> c_int {
    if level >= TLL_LOGGER_ERROR && !data.is_null() {
        let text = String::from_utf8_lossy(std::slice::from_raw_parts(data.cast::<u8>(), size))
            .into_owned();
        LAST.with(|l| {
            let mut l = l.borrow_mut();
            match l.as_mut() {
                Some(t) => {
                    t.push('\n');
                    t.push_str(&text);
                }
                None => *l = Some(text),
            }
        });
    }
    match prev().and_then(|p| p.log) {
        Some(f) => f(ts, cat, level, data, size, obj),
        None => 0,
    }
}

unsafe extern "C" fn log_new(_: *mut tll_logger_impl_t, cat: *const c_char) -> *mut c_void {
    match prev().and_then(|p| p.log_new.map(|f| (p, f))) {
        Some((p, f)) => f(p as *const _ as *mut _, cat),
        None => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn log_free(_: *mut tll_logger_impl_t, cat: *const c_char, obj: *mut c_void) {
    if let Some((p, f)) = prev().and_then(|p| p.log_free.map(|f| (p, f))) {
        f(p as *const _ as *mut _, cat, obj)
    }
}

unsafe extern "C" fn configure(_: *mut tll_logger_impl_t, cfg: *const tll_config_t) -> c_int {
    match prev().and_then(|p| p.configure.map(|f| (p, f))) {
        Some((p, f)) => f(p as *const _ as *mut _, cfg),
        None => 0,
    }
}

/// Поставить обработчик (один раз на процесс).
pub fn install() {
    // Без libtll перехватывать нечего; зовут после `Context::try_new`.
    if api().is_err() {
        return;
    }
    ONCE.call_once(|| unsafe {
        PREV = tll_logger_impl_get();
        tll_logger_register(std::ptr::addr_of_mut!(IMPL));
    });
}

/// Ошибки libtll этого потока с прошлого `take`, построчно; забираются.
pub fn take() -> Option<String> {
    LAST.with(|l| l.borrow_mut().take())
}

/// Ошибка Lua человеческим языком: без служебного префикса и с подсказкой.
pub fn explain(raw: &str) -> String {
    // Строка с позицией в коде Lua — самая точная; иначе первая.
    let first = raw
        .lines()
        .find(|l| l.contains("[string"))
        .or_else(|| raw.lines().next())
        .unwrap_or(raw);
    let msg = match first.find("]:") {
        Some(i) => first[i + 2..]
            .split_once(": ")
            .map(|(_, m)| m)
            .unwrap_or(&first[i + 2..]),
        None => first,
    }
    .trim()
    .trim_end_matches(':');
    let on = raw
        .lines()
        .find_map(|l| l.trim().strip_prefix("on message: "))
        .and_then(|l| {
            let name = l.split("name: ").nth(1)?.split(',').next()?;
            let seq = l.split("seq: ").nth(1)?.split(',').next()?;
            Some(format!(" ({name}, seq {seq})"))
        });
    let hint = if msg.contains("with nil") || msg.contains("a nil value") {
        " — поля нет у этого типа: добавь `data.x ~= nil and …` или выбери типы"
    } else if msg.contains("near") || msg.contains("expected") {
        " — синтаксис Lua: and, or, not, ==, ~=, строки в кавычках"
    } else {
        ""
    };
    format!("{msg}{}{hint}", on.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Текст ошибки Lua из журнала — коротко, с сообщением и подсказкой.
    #[test]
    fn lua_errors_read_as_hints() {
        let raw = "Lua function tll_filter failed: [string \"function tll_on_open(cfg)...\"]:6: attempt to compare number with nil\n  on message: type: Data, msgid: 10120, name: BlockInitMsg, seq: 0, size: 195";
        let e = explain(raw);
        assert!(
            e.starts_with("attempt to compare number with nil (BlockInitMsg, seq 0) — поля нет"),
            "{e}"
        );
        assert!(explain("x: [string \"f\"]:6: syntax error near 'AND'").contains("синтаксис Lua"));
        assert!(
            explain("[string \"f\"]:7: attempt to index a nil value (field 'x')")
                .contains("поля нет")
        );
        assert_eq!(explain("Failed to open file"), "Failed to open file");
        assert_eq!(explain("[string \"f\"]:3: oops"), "oops");
    }

    /// Обработчик журнала копит ошибки потока и отдаёт их один раз; прочие
    /// уровни не копятся, а журнал идёт и прежнему обработчику.
    #[test]
    fn handler_collects_errors_per_thread() {
        install();
        take();
        let cat = c"mutator.test";
        unsafe {
            let obj = log_new(std::ptr::null_mut(), cat.as_ptr());
            let e1 = b"first";
            let e2 = b"second";
            let info = b"info";
            log(
                0,
                cat.as_ptr(),
                TLL_LOGGER_ERROR,
                e1.as_ptr().cast(),
                e1.len(),
                obj,
            );
            log(0, cat.as_ptr(), 2, info.as_ptr().cast(), info.len(), obj);
            log(
                0,
                cat.as_ptr(),
                TLL_LOGGER_CRITICAL,
                e2.as_ptr().cast(),
                e2.len(),
                obj,
            );
            log_free(std::ptr::null_mut(), cat.as_ptr(), obj);
        }
        assert_eq!(take().as_deref(), Some("first\nsecond"));
        assert_eq!(take(), None);
        std::thread::spawn(|| assert_eq!(take(), None))
            .join()
            .unwrap();
    }
}
