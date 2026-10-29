//! Список линков resolve-сервера (`agr:resolve-browse`): что экспортировали
//! сервисы стенда — для выбора линка вместо файла.
//!
//! Протокол — дело бэкенда ([`Driver::resolve`], у TLL —
//! [`crate::tll::resolve`]): здесь только его события ([`BrowseEvent`]) и
//! [`Browser`] — поток, который держит подписку и собирает из событий
//! [`Snapshot`]: записи по ключу (сервис, канал), состояние и ошибку. Обрыв —
//! переподключение через [`RETRY`]; список с сервера приходит заново, поэтому
//! на Active прежний сбрасывается. Вид будит `notify`.
//!
//! [`Entry`] — экспорт, разобранный в колонки: `proto` и адрес — из `init.*`
//! (url клиента, как его собирает `tll-resolve-browse`), схема — хеш или url,
//! число её сообщений — если тело пришло в `scheme.<hash>`. Бэкенд, у которого
//! сервер шлёт url клиента целиком ([`Export::url`]), отдаёт его как есть:
//! колонки разбираются из самого url, схема — его `schema`/`scheme` или
//! `scheme` из `config`.
//!
//! Наружу: [`Browser`], [`Snapshot`], [`Entry`], [`Export`], [`BrowseEvent`],
//! [`DriverBrowse`], [`RETRY`].

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::link::LinkState;
use super::{drivers, Backend, Context, Driver};

/// Экспорт канала, как его прислал сервер.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Export {
    pub service: String,
    pub channel: String,
    pub tags: Vec<String>,
    /// Машина сервиса.
    pub host: String,
    /// Пары `config`: `init.*` — параметры клиента, `scheme.<hash>` — тела схем.
    pub config: Vec<(String, String)>,
    /// Сообщений в схеме, если её тело пришло.
    pub messages: Option<usize>,
    /// Url клиента целиком, если сервер прислал его готовым; иначе url
    /// собирается из `init.*`.
    pub url: Option<String>,
}

impl Export {
    /// Параметр клиента без `init.`: `tll.proto`, `tll.host`, `scheme`… У
    /// экспорта без префикса (так шлют тесты libtll) — ключ как есть.
    pub fn param(&self, key: &str) -> Option<&str> {
        let init = format!("init.{key}");
        let prefixed = self.config.iter().any(|(k, _)| k.starts_with("init."));
        self.config
            .iter()
            .find(|(k, _)| if prefixed { *k == init } else { k == key })
            .map(|(_, v)| v.as_str())
    }

    /// Параметры клиента по порядку, кроме тел схем; адрес парой
    /// `….tll.host.host` и `….tll.host.port` (так шлёт процессор) — одним
    /// `….tll.host=HOST:PORT`.
    fn params(&self) -> Vec<(String, String)> {
        let prefixed = self.config.iter().any(|(k, _)| k.starts_with("init."));
        let all: Vec<(&str, &str)> = self
            .config
            .iter()
            .filter_map(|(k, v)| {
                let k = if prefixed {
                    k.strip_prefix("init.")?
                } else {
                    k.as_str()
                };
                (!k.starts_with("scheme.")).then_some((k, v.as_str()))
            })
            .collect();
        let get = |key: &str| all.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
        let mut out = Vec::new();
        for (k, v) in &all {
            if let Some(base) = k.strip_suffix(".host.host") {
                match get(&format!("{base}.host.port")) {
                    Some(port) => out.push((format!("{base}.host"), format!("{v}:{port}"))),
                    None => out.push((format!("{base}.host"), v.to_string())),
                }
            } else if let Some(base) = k.strip_suffix(".host.port") {
                if get(&format!("{base}.host.host")).is_none() {
                    out.push((format!("{base}.host"), format!(":{v}")));
                }
            } else {
                out.push((k.to_string(), v.to_string()));
            }
        }
        out
    }
}

/// Событие подписки бэкенда.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowseEvent {
    State(LinkState),
    Export(Export),
    DropChannel {
        service: String,
        channel: String,
    },
    DropService {
        service: String,
    },
    /// Ошибка журнала бэкенда — текст для человека.
    Error(String),
}

/// Подписка бэкенда на экспорты.
pub trait DriverBrowse: Send {
    /// Обработать канал не дольше `timeout`; накопившиеся события.
    fn poll(&mut self, timeout: Duration) -> Vec<BrowseEvent>;
}

/// Строка списка: экспорт в колонках.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entry {
    pub service: String,
    pub channel: String,
    pub tags: Vec<String>,
    pub host: String,
    /// `init.tll.proto`: `stream+pub+tcp`, `pub+tcp`…
    pub proto: String,
    /// `init.tll.host`: адрес канала.
    pub addr: String,
    /// `init.scheme`: `sha256:ab12cd34` (хеш — коротко) или url схемы.
    pub scheme: String,
    pub messages: Option<usize>,
    /// Прочие параметры клиента: `mode=client; …`.
    pub params: String,
    /// Url клиента целиком, как его печатает `tll-resolve-browse`.
    pub url: String,
}

impl Entry {
    pub fn new(e: &Export) -> Entry {
        if let Some(url) = &e.url {
            return Entry::direct(e, url);
        }
        let params = e.params();
        let get = |key: &str| {
            params
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        let scheme = match get("scheme") {
            s if s.starts_with("sha256://") => {
                let h = &s["sha256://".len()..];
                format!("sha256:{}", h.get(..8).unwrap_or(h))
            }
            s => s,
        };
        let rest: Vec<String> = params
            .iter()
            .filter(|(k, _)| !matches!(k.as_str(), "tll.proto" | "tll.host" | "scheme"))
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        let mut url = format!("{}://{}", get("tll.proto"), get("tll.host"));
        for (k, v) in &params {
            if !matches!(k.as_str(), "tll.proto" | "tll.host") {
                url.push_str(&format!(";{k}={v}"));
            }
        }
        Entry {
            service: e.service.clone(),
            channel: e.channel.clone(),
            tags: e.tags.clone(),
            host: e.host.clone(),
            proto: get("tll.proto"),
            addr: get("tll.host"),
            scheme,
            messages: e.messages,
            params: rest.join("; "),
            url,
        }
    }

    /// Строка по готовому url `proto://адрес;k=v;…`: схема — параметр
    /// `schema`/`scheme`, а нет его — `scheme` из `config` (хеш).
    fn direct(e: &Export, url: &str) -> Entry {
        let (proto, rest) = url.split_once("://").unwrap_or(("", url));
        let mut parts = rest.split(';').filter(|s| !s.is_empty());
        let addr = parts.next().unwrap_or_default().to_string();
        let mut scheme = String::new();
        let mut rest = Vec::new();
        for p in parts {
            match p.split_once('=') {
                Some(("schema" | "scheme", v)) => scheme = v.to_string(),
                _ => rest.push(p.to_string()),
            }
        }
        if scheme.is_empty() {
            scheme = e.param("scheme").unwrap_or_default().to_string();
        }
        if let Some(h) = scheme.strip_prefix("sha256://") {
            scheme = format!("sha256:{}", h.get(..8).unwrap_or(h));
        }
        Entry {
            service: e.service.clone(),
            channel: e.channel.clone(),
            tags: e.tags.clone(),
            host: e.host.clone(),
            proto: proto.to_string(),
            addr,
            scheme,
            messages: e.messages,
            params: rest.join("; "),
            url: url.to_string(),
        }
    }

    /// Что открыть: `resolve://SERVICE/CHANNEL` — через тот же сервер.
    pub fn link(&self) -> String {
        format!("resolve://{}/{}", self.service, self.channel)
    }

    /// Ключ строки: сервис и канал.
    pub fn key(&self) -> (String, String) {
        (self.service.clone(), self.channel.clone())
    }
}

/// Что известно о сервере сейчас.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    /// `HOST:PORT` сервера.
    pub addr: String,
    pub state: LinkState,
    pub error: Option<String>,
    pub entries: BTreeMap<(String, String), Entry>,
}

impl Snapshot {
    /// Применить событие подписки.
    pub fn apply(&mut self, ev: BrowseEvent) {
        match ev {
            BrowseEvent::State(st) => {
                if st == LinkState::Active {
                    // Сервер шлёт список заново: прежний мог устареть.
                    self.entries.clear();
                    self.error = None;
                }
                self.state = st;
            }
            BrowseEvent::Export(e) => {
                let en = Entry::new(&e);
                self.entries.insert(en.key(), en);
            }
            BrowseEvent::DropChannel { service, channel } => {
                self.entries.remove(&(service, channel));
            }
            BrowseEvent::DropService { service } => {
                self.entries.retain(|(s, _), _| *s != service);
            }
            BrowseEvent::Error(e) => self.error = Some(e),
        }
    }
}

type Notify = Arc<dyn Fn() + Send + Sync>;

/// Через сколько переподключаться после обрыва.
pub const RETRY: Duration = Duration::from_secs(2);
/// Сколько ждать канал за один проход: столько же — реакция на остановку.
const POLL: Duration = Duration::from_millis(50);
/// Будить вид не чаще: список приходит пачкой.
const WAKE: Duration = Duration::from_millis(100);

/// Подписка на resolve-сервер в своём потоке. Адрес меняется на месте
/// ([`Browser::retarget`]): окно, вкладка и REST держат один и тот же список.
pub struct Browser {
    snap: Arc<Mutex<Snapshot>>,
    stop: Mutex<Arc<AtomicBool>>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

/// Поток подписки на `addr`; остановка — выставленный флаг.
fn spawn_watch(
    addr: &str,
    snap: Arc<Mutex<Snapshot>>,
    notify: Notify,
) -> (Arc<AtomicBool>, Option<std::thread::JoinHandle<()>>) {
    let stop = Arc::new(AtomicBool::new(false));
    let (halt, a) = (stop.clone(), addr.to_string());
    let thread = std::thread::Builder::new()
        .name("resolve".into())
        .spawn(move || watch(&a, &snap, &halt, &notify))
        .ok();
    (stop, thread)
}

impl Browser {
    /// Подключиться к `addr` (`HOST:PORT`) первым бэкендом с resolve.
    pub fn start(addr: &str, notify: Notify) -> Browser {
        let snap = Arc::new(Mutex::new(Snapshot {
            addr: addr.to_string(),
            ..Default::default()
        }));
        let (stop, thread) = spawn_watch(addr, snap.clone(), notify);
        Browser {
            snap,
            stop: Mutex::new(stop),
            thread: Mutex::new(thread),
        }
    }

    /// Список без сервера — для тестов и видов без сети.
    pub fn fixed(snap: Snapshot) -> Browser {
        Browser {
            snap: Arc::new(Mutex::new(snap)),
            stop: Mutex::new(Arc::new(AtomicBool::new(true))),
            thread: Mutex::new(None),
        }
    }

    /// Подключиться к другому серверу, не меняя [`Browser`]: кто держит эту
    /// же ссылку (вкладка и `GET /api/v1/resolve`), видит новый адрес.
    pub fn retarget(&self, addr: &str, notify: Notify) {
        self.halt();
        *self.snap.lock().unwrap_or_else(|e| e.into_inner()) = Snapshot {
            addr: addr.to_string(),
            ..Default::default()
        };
        let (stop, thread) = spawn_watch(addr, self.snap.clone(), notify);
        *self.stop.lock().unwrap_or_else(|e| e.into_inner()) = stop;
        *self.thread.lock().unwrap_or_else(|e| e.into_inner()) = thread;
    }

    /// Остановить поток подписки и дождаться его.
    fn halt(&self) {
        self.stop
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = t.join();
        }
    }

    /// Текущий список и состояние.
    pub fn snapshot(&self) -> Snapshot {
        self.snap.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Адрес сервера.
    pub fn addr(&self) -> String {
        self.snap
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .addr
            .clone()
    }

    /// Поменять список — тесты видов: пришёл экспорт, ушёл канал.
    pub fn apply(&self, ev: BrowseEvent) {
        self.snap
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .apply(ev);
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.halt();
    }
}

/// Бэкенд, который знает протокол resolve.
fn backend() -> Option<Backend> {
    drivers()
        .into_iter()
        .find(|d| d.resolve())
        .map(|d: &'static dyn Driver| Backend(d.name()))
}

/// Поток подписки: подключиться, собирать события, после обрыва — заново.
fn watch(addr: &str, snap: &Mutex<Snapshot>, stop: &AtomicBool, notify: &Notify) {
    let apply = |ev: BrowseEvent| {
        snap.lock().unwrap_or_else(|e| e.into_inner()).apply(ev);
    };
    let Some(b) = backend() else {
        apply(BrowseEvent::State(LinkState::Error));
        apply(BrowseEvent::Error(
            "ни один бэкенд сборки не знает resolve".into(),
        ));
        notify();
        return;
    };
    while !stop.load(Ordering::Relaxed) {
        let ctx = Context::new();
        match ctx.with(b, |c| c.browse(addr)) {
            Ok(mut sub) => {
                let mut woke = Instant::now();
                let mut dirty = false;
                'live: while !stop.load(Ordering::Relaxed) {
                    for ev in sub.poll(POLL) {
                        let end =
                            matches!(ev, BrowseEvent::State(LinkState::Error | LinkState::Closed));
                        let state = matches!(ev, BrowseEvent::State(_) | BrowseEvent::Error(_));
                        apply(ev);
                        dirty = true;
                        if state {
                            notify();
                            woke = Instant::now();
                            dirty = false;
                        }
                        if end {
                            break 'live;
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
            Err(e) => {
                apply(BrowseEvent::State(LinkState::Error));
                apply(BrowseEvent::Error(e));
                notify();
            }
        }
        let until = Instant::now() + RETRY;
        while Instant::now() < until && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(POLL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn export(svc: &str, ch: &str, config: &[(&str, &str)]) -> Export {
        Export {
            service: svc.into(),
            channel: ch.into(),
            config: config
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..Default::default()
        }
    }

    /// `agr:resolve-browse` — колонки из `init.*`: proto, адрес, короткий хеш
    /// схемы, прочие параметры; url клиента — как у `tll-resolve-browse`;
    /// открывается `resolve://svc/ch`.
    #[test]
    fn export_becomes_columns_and_client_url() {
        let e = export(
            "gw",
            "orders/online",
            &[
                ("init.tll.proto", "stream+pub+tcp"),
                ("init.tll.host", "h1:5555"),
                ("init.mode", "client"),
                ("init.scheme", "sha256://0123456789abcdef"),
                ("scheme.0123456789abcdef", "yamls://[]"),
            ],
        );
        let en = Entry::new(&e);
        assert_eq!(en.proto, "stream+pub+tcp");
        assert_eq!(en.addr, "h1:5555");
        assert_eq!(en.scheme, "sha256:01234567");
        assert_eq!(en.params, "mode=client");
        assert_eq!(
            en.url,
            "stream+pub+tcp://h1:5555;mode=client;scheme=sha256://0123456789abcdef"
        );
        assert_eq!(en.link(), "resolve://gw/orders/online");
        // Адрес парой host/port — как шлёт процессор, и у вложенных тоже.
        let live = Entry::new(&export(
            "acc-mgr",
            "toRef",
            &[
                ("init.mode", "client"),
                ("init.request.tll.host.host", "stand-141"),
                ("init.request.tll.host.port", "20009"),
                ("init.request.tll.proto", "tcp"),
                ("init.tll.host.host", "stand-141"),
                ("init.tll.host.port", "10009"),
                ("init.tll.proto", "stream+pub+tcp"),
            ],
        ));
        assert_eq!(live.addr, "stand-141:10009");
        assert_eq!(
            live.params,
            "mode=client; request.tll.host=stand-141:20009; request.tll.proto=tcp"
        );
        assert_eq!(
            live.url,
            "stream+pub+tcp://stand-141:10009;mode=client;request.tll.host=stand-141:20009;\
             request.tll.proto=tcp"
        );
        // Без `init.` (так шлют тесты libtll) — ключи как есть.
        let bare = Entry::new(&export("s", "a", &[("tll.proto", "tcp")]));
        assert_eq!(bare.proto, "tcp");
    }

    /// `agr:resolve-browse` — экспорт с готовым url (так шлёт сервер другого
    /// бэкенда): колонки — из самого url, схема — его `schema`, а без неё —
    /// короткий хеш из `config`; url остаётся как пришёл.
    #[test]
    fn export_with_ready_url_keeps_it() {
        let mut e = export("aggr", "online", &[]);
        e.host = "node-05".into();
        e.url = Some("zmtp://node-05:6241;af=ipv4;schema=yaml://aggr/aggr.yaml;type=sub".into());
        let en = Entry::new(&e);
        assert_eq!(
            (en.proto.as_str(), en.addr.as_str(), en.scheme.as_str()),
            ("zmtp", "node-05:6241", "yaml://aggr/aggr.yaml")
        );
        assert_eq!(en.params, "af=ipv4; type=sub");
        assert_eq!(en.url, e.url.clone().unwrap());
        assert_eq!(en.link(), "resolve://aggr/online");

        let mut e = export("aggr", "merger", &[("scheme", "sha256://5dae596597f62fcb")]);
        e.url = Some("replay+tcp://node-05:9000;mode=client".into());
        let en = Entry::new(&e);
        assert_eq!(
            (en.proto.as_str(), en.scheme.as_str()),
            ("replay+tcp", "sha256:5dae5965")
        );
        assert_eq!(en.params, "mode=client");
    }

    /// `agr:resolve-browse` — подписка: Active сбрасывает старый список,
    /// экспорт кладёт строку по ключу (повтор — заменяет), уход канала и
    /// сервиса — убирает.
    #[test]
    fn snapshot_follows_exports_and_drops() {
        let mut s = Snapshot::default();
        s.apply(BrowseEvent::Export(export("old", "x", &[])));
        s.apply(BrowseEvent::State(LinkState::Active));
        assert!(s.entries.is_empty(), "Active — список заново");
        for (svc, ch) in [("a", "1"), ("a", "2"), ("b", "1")] {
            s.apply(BrowseEvent::Export(export(svc, ch, &[])));
        }
        s.apply(BrowseEvent::Export(export(
            "a",
            "1",
            &[("tll.proto", "tcp")],
        )));
        assert_eq!(s.entries.len(), 3);
        assert_eq!(s.entries[&("a".into(), "1".into())].proto, "tcp");
        s.apply(BrowseEvent::DropChannel {
            service: "a".into(),
            channel: "2".into(),
        });
        s.apply(BrowseEvent::DropService {
            service: "b".into(),
        });
        let keys: Vec<_> = s.entries.keys().cloned().collect();
        assert_eq!(keys, [("a".to_string(), "1".to_string())]);
    }

    /// `agr:resolve-browse` — нет сервера — Error с причиной, а не тишина.
    #[cfg(feature = "tll")]
    #[test]
    fn unreachable_server_is_an_error() {
        super::super::quiet();
        let b = Browser::start("127.0.0.1:1", Arc::new(|| {}));
        let until = Instant::now() + Duration::from_secs(10);
        while b.snapshot().state != LinkState::Error && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        let s = b.snapshot();
        assert_eq!(s.state, LinkState::Error, "{s:?}");
        assert_eq!(s.addr, "127.0.0.1:1");
    }
}
