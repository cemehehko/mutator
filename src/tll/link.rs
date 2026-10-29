//! Линк TLL: любой канал по строке url — `resolve://svc/ch`, `stream+…`,
//! префиксы (`lua+…`) — как его создала бы libtll сама. Разбор url, резолв и
//! префиксы — дело libtll: здесь только канал, опрос и перевод его сообщений
//! в [`LinkEvent`].
//!
//! История есть у stream-клиента: в его control-схеме — `Online`, которым он
//! сообщает, что история догружена и дальше онлайн (`agr:link-state`). У
//! `resolve://` протокол известен только после резолва, поэтому схемы смотрим
//! на Active.
//!
//! Схему по хешу (`scheme=sha256://…` в url, скопированном из resolve) libtll
//! берёт только из кэша контекста, куда её кладёт `resolve://`. Для прямого
//! url её загружает [`preload`]: спрашивает все экспорты у resolve-сервера
//! (`--resolve HOST:PORT`, `MUTATOR_RESOLVE`; протокол — [`super::resolve`]) и
//! кладёт в кэш тело с нужным хешем. Тот же сервер подставляется в
//! `resolve://` без `resolve.request` ([`with_resolve`]).
//!
//! Наружу: [`Link`], [`is_link`], [`default_open`], [`preload`], [`with_resolve`].

use std::collections::VecDeque;
use std::os::raw::{c_int, c_void};
use std::time::{Duration, Instant};

use super::sys::*;
use super::{convert, lifecycle, log, next_name, process_tree, resolve, Context};
use crate::io::link::{DriverLink, LinkEvent, LinkState};
use crate::io::resolve::BrowseEvent;
use crate::io::Msg;

/// Url канала, а не файла: есть протокол, и он не `file`.
pub fn is_link(s: &str) -> bool {
    match s.split_once("://") {
        Some((proto, _)) => {
            !proto.is_empty()
                && proto != "file"
                && proto
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '_')
        }
        None => false,
    }
}

/// Параметры открытия по умолчанию: stream-клиенту `mode` обязателен —
/// онлайн, история по запросу; `resolve://` чаще всего ведёт к stream. Прочим
/// — ничего: свой `mode` есть у `lua+`, и `online` он отвергнет.
pub fn default_open(url: &str) -> &'static str {
    let proto = url.split_once("://").map_or("", |(p, _)| p);
    if proto.split('+').any(|p| p == "stream" || p == "resolve") {
        "mode=online"
    } else {
        ""
    }
}

/// Url с нашими `name` и `dump`, если их там нет: url, скопированный из
/// конфига сервиса, часто несёт свой `name`, а повтор ключа libtll отвергает.
fn with_defaults(url: &str, n: u64) -> String {
    let has = |k: &str| {
        url.split(';')
            .skip(1)
            .any(|kv| kv.split_once('=').is_some_and(|(key, _)| key.trim() == k))
    };
    let mut full = url.to_string();
    if !has("name") {
        full += &format!(";name=ml{n}");
    }
    if !has("dump") {
        full += ";dump=no";
    }
    full
}

#[derive(Default)]
struct Pump {
    events: VecDeque<LinkEvent>,
    /// msgid `Online` в control-схеме; ищется на Active.
    online: Option<i32>,
    ch: usize,
}

/// Открытый канал линка.
pub struct Link {
    ctx: *mut tll_channel_context_t,
    ch: *mut tll_channel_t,
    pump: Box<Pump>,
}

unsafe impl Send for Link {}

const TLL_MESSAGE_CONTROL: c_short = 1;
const TLL_MESSAGE_MASK_CONTROL: c_uint = 1 << 1;

use std::os::raw::{c_short, c_uint};

fn state(id: c_int) -> LinkState {
    match id {
        1 => LinkState::Opening,
        2 => LinkState::Active,
        3 => LinkState::Closing,
        4 => LinkState::Error,
        _ => LinkState::Closed,
    }
}

unsafe extern "C" fn on_msg(
    _c: *const tll_channel_t,
    msg: *const tll_msg_t,
    user: *mut c_void,
) -> c_int {
    let pump = &mut *(user as *mut Pump);
    let m = &*msg;
    match m.type_ {
        TLL_MESSAGE_DATA => {
            let data = if m.size == 0 || m.data.is_null() {
                Vec::new()
            } else {
                std::slice::from_raw_parts(m.data as *const u8, m.size).to_vec()
            };
            pump.events.push_back(LinkEvent::Data(Msg {
                seq: m.seq,
                msgid: m.msgid,
                data,
            }));
        }
        TLL_MESSAGE_STATE => {
            let st = state(m.msgid);
            if st == LinkState::Active {
                pump.online = online_id(pump.ch as *mut tll_channel_t);
            }
            pump.events.push_back(LinkEvent::State(st));
        }
        TLL_MESSAGE_CONTROL if Some(m.msgid) == pump.online => {
            pump.events.push_back(LinkEvent::Online(m.seq));
        }
        _ => {}
    }
    0
}

/// msgid `Online` в control-схеме канала: есть — это stream-клиент с историей.
unsafe fn online_id(ch: *mut tll_channel_t) -> Option<i32> {
    let s = tll_channel_scheme(ch, TLL_MESSAGE_CONTROL as c_int);
    if s.is_null() {
        return None;
    }
    let s = convert::scheme(&*s);
    let id = s.messages().find(|m| m.name == "Online").map(|m| m.msgid);
    id
}

impl Link {
    /// Создать канал по `url` и открыть его с параметрами `open`.
    pub fn open(ctx: &Context, url: &str, open: &str) -> Result<Link, String> {
        log::install();
        log::take();
        if url.contains("lua+") && !ctx.ensure_lua() {
            return Err("Lua недоступен: нет модуля tll-lua (MUTATOR_LUA)".into());
        }
        let url = &with_resolve(url, resolve::addr().as_deref());
        preload(ctx, url)?;
        let full = with_defaults(url, next_name());
        let g = lifecycle();
        let ch = unsafe {
            tll_channel_new(
                ctx.ptr,
                full.as_ptr().cast(),
                full.len(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if ch.is_null() {
            drop(g);
            return Err(log::take()
                .map(|e| log::explain(&e))
                .unwrap_or_else(|| format!("не создался канал {url}")));
        }
        let mut l = Link {
            ctx: unsafe { tll_channel_context_ref(ctx.ptr) },
            ch,
            pump: Box::default(),
        };
        l.pump.ch = ch as usize;
        let user = (&mut *l.pump) as *mut Pump as *mut c_void;
        unsafe {
            tll_channel_callback_add(
                ch,
                on_msg,
                user,
                TLL_MESSAGE_MASK_DATA | TLL_MESSAGE_MASK_STATE | TLL_MESSAGE_MASK_CONTROL,
            );
        }
        let rc = unsafe { tll_channel_open(ch, open.as_ptr().cast(), open.len()) };
        drop(g);
        if rc != 0 {
            return Err(log::take()
                .map(|e| log::explain(&e))
                .unwrap_or_else(|| format!("не открылся канал {url}")));
        }
        Ok(l)
    }
}

impl Link {
    /// Схема данных канала.
    pub(super) fn data_scheme(&self) -> Option<crate::scheme::Scheme> {
        let s = unsafe { tll_channel_scheme(self.ch, TLL_MESSAGE_DATA as c_int) };
        (!s.is_null()).then(|| unsafe { convert::scheme(&*s) })
    }

    /// Послать сообщение в канал.
    pub(super) fn post(&self, m: &Msg) -> Result<(), String> {
        let msg = tll_msg_t {
            type_: TLL_MESSAGE_DATA,
            flags: 0,
            msgid: m.msgid,
            seq: m.seq,
            pad0: std::ptr::null(),
            data: m.data.as_ptr().cast(),
            size: m.data.len(),
            addr: 0,
            time: 0,
            reserved: 0,
        };
        match unsafe { tll_channel_post(self.ch, &msg, 0) } {
            0 => Ok(()),
            e => Err(format!("не отправилось: код {e}")),
        }
    }
}

/// `resolve://` без своего `resolve.request` — через сервер `addr` по tcp.
pub fn with_resolve(url: &str, addr: Option<&str>) -> String {
    let via = url
        .split_once("://")
        .is_some_and(|(p, _)| p.split('+').any(|p| p == "resolve"));
    match addr {
        Some(a) if via && !url.contains("resolve.request") => format!(
            "{url};resolve.request.tll.proto=tcp;resolve.request.tll.host={a};\
             resolve.request.mode=client;resolve.request.scheme={}",
            resolve::RESOLVE_SCHEME
        ),
        _ => url.to_string(),
    }
}

/// Хеши схем в url: `scheme=sha256://…` (и `…scheme=` вложенных каналов).
fn hashes(url: &str) -> Vec<String> {
    url.split(';')
        .filter_map(|kv| kv.split_once('='))
        .filter(|(k, _)| *k == "scheme" || k.ends_with(".scheme"))
        .filter_map(|(_, v)| v.strip_prefix("sha256://"))
        .map(|h| h.trim().to_string())
        .collect()
}

/// Загрузить в кэш контекста схемы по хешу из url: тела берутся у
/// resolve-сервера, как их берёт `resolve://` (`agr:link-open`).
pub fn preload(ctx: &Context, url: &str) -> Result<(), String> {
    // Контекст у линка свой и свежий: кэш пуст, проверять его нечего.
    let mut missing = hashes(url);
    if missing.is_empty() {
        return Ok(());
    }
    let Some(addr) = resolve::addr() else {
        return Err(format!(
            "схема sha256://{}… — по хешу, её знает resolve-сервер: укажите --resolve HOST:PORT",
            &missing[0][..12.min(missing[0].len())]
        ));
    };
    let mut l = Link::open(ctx, &resolve::client_url(&addr), "")?;
    let until = Instant::now() + Duration::from_secs(10);
    let mut asked = false;
    let mut scheme = None;
    while !missing.is_empty() && Instant::now() < until {
        for ev in l.poll(Duration::from_millis(50)) {
            match ev {
                LinkEvent::State(LinkState::Active) if !asked => {
                    let s = l.data_scheme().ok_or("у resolve-сервера нет схемы")?;
                    l.post(&resolve::request(&s)?)?;
                    scheme = Some(s);
                    asked = true;
                }
                LinkEvent::State(LinkState::Error | LinkState::Closed) => {
                    return Err(format!("resolve-сервер {addr} недоступен"));
                }
                LinkEvent::Error(e) if !asked => {
                    return Err(format!("resolve-сервер {addr}: {e}"));
                }
                LinkEvent::Data(m) => {
                    let Some(s) = &scheme else { continue };
                    let Some(BrowseEvent::Export(e)) = resolve::parse(s, &m) else {
                        continue;
                    };
                    for (key, body) in e
                        .config
                        .into_iter()
                        .filter(|(k, _)| k.starts_with("scheme."))
                    {
                        let Some(i) = missing.iter().position(|h| key.contains(h.as_str())) else {
                            continue;
                        };
                        let p = unsafe {
                            tll_channel_context_scheme_load(
                                ctx.ptr,
                                body.as_ptr().cast(),
                                body.len() as c_int,
                                1,
                            )
                        };
                        if !p.is_null() {
                            unsafe { tll_scheme_unref(p) };
                            missing.remove(i);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    match missing.first() {
        None => Ok(()),
        Some(h) => Err(format!("resolve-сервер {addr} не знает схему sha256://{h}")),
    }
}

impl DriverLink for Link {
    fn poll(&mut self, timeout: Duration) -> Vec<LinkEvent> {
        let until = Instant::now() + timeout;
        while self.pump.events.is_empty() && Instant::now() < until {
            let busy = unsafe { process_tree(self.ch) };
            if !busy && self.pump.events.is_empty() {
                // Данных нет: не крутить процессор, но и не спать долго —
                // онлайн должен приходить без задержки.
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        let mut out: Vec<LinkEvent> = self.pump.events.drain(..).collect();
        if let Some(e) = log::take() {
            out.push(LinkEvent::Error(log::explain(&e)));
        }
        out
    }

    fn scheme_url(&self) -> Option<String> {
        unsafe {
            let s = tll_channel_scheme(self.ch, TLL_MESSAGE_DATA as c_int);
            if s.is_null() {
                return None;
            }
            let p = tll_scheme_dump(s, c"yamls+gz".as_ptr());
            if p.is_null() {
                return None;
            }
            let r = std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned();
            free(p.cast());
            Some(r)
        }
    }

    fn history(&self) -> bool {
        self.pump.online.is_some()
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        let _g = lifecycle();
        unsafe {
            tll_channel_free(self.ch);
            tll_channel_context_free(self.ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_link;

    /// `agr:link-open` — линк — url канала; файл, путь и `file://` — нет.
    #[test]
    fn links_are_channel_urls_not_files() {
        assert!(is_link("resolve://gateway/output"));
        assert!(is_link("stream+pub+tcp://host:1;request=tcp://host:2"));
        assert!(is_link("lua+resolve://svc/ch"));
        assert!(!is_link("file://x.dat"));
        assert!(!is_link("x.dat"));
        assert!(!is_link("/tmp/a b/c.dat"));
        assert!(!is_link("://x"));
    }

    /// `agr:link-open` — url из конфига сервиса со своим `name` открывается:
    /// наши `name` и `dump` — только если их нет.
    #[test]
    fn own_name_in_url_is_kept() {
        use super::with_defaults;
        assert_eq!(with_defaults("tcp://h:1", 3), "tcp://h:1;name=ml3;dump=no");
        assert_eq!(
            with_defaults("tcp://h:1;name=x/y;dump=yes", 3),
            "tcp://h:1;name=x/y;dump=yes"
        );
        assert_eq!(
            with_defaults("tcp://h:1;rename=a", 3),
            "tcp://h:1;rename=a;name=ml3;dump=no"
        );
    }

    /// `agr:link-open` — `resolve://` без своего запроса идёт через
    /// `--resolve`; хеши схем находятся и во вложенных каналах.
    #[test]
    fn resolve_server_is_substituted_and_hashes_found() {
        use super::{hashes, with_resolve};
        let u = with_resolve("resolve://a/b", Some("h:5555"));
        assert!(u.contains("resolve.request.tll.host=h:5555"), "{u}");
        assert_eq!(
            with_resolve("resolve://a/b;resolve.request=x", Some("h:1")),
            "resolve://a/b;resolve.request=x"
        );
        assert_eq!(with_resolve("tcp://h:1", Some("h:1")), "tcp://h:1");
        assert_eq!(
            hashes("stream+pub+tcp://;scheme=sha256://ab;request.scheme=sha256://cd;x=sha256://ef"),
            ["ab", "cd"]
        );
    }

    /// `agr:link-open` — онлайн по умолчанию — только там, где `mode` ждут:
    /// stream и resolve; `lua+file` свой `mode` не путает с чужим.
    #[test]
    fn default_open_params_depend_on_the_protocol() {
        use super::default_open;
        assert_eq!(default_open("resolve://svc/ch"), "mode=online");
        assert_eq!(
            default_open("stream+pub+tcp://h:1;request=tcp://h:2"),
            "mode=online"
        );
        assert_eq!(default_open("lua+file://x.dat"), "");
        assert_eq!(default_open("pub+tcp://h:1"), "");
    }
}
