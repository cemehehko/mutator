//! Протокол resolve-сервера libtll (`yaml://tll/logic/resolve.yaml`): запрос
//! всех экспортов и разбор ответов. Сервер на `Request{service: "*", channel:
//! "*"}` шлёт `ExportChannel` на каждый экспорт и держит подписку: дальше —
//! новые `ExportChannel`, `DropChannel`, `DropService`; конца списка нет.
//!
//! Этим пользуются двое: [`super::link::preload`] берёт из `config` тела схем
//! по хешу, а [`Browse`] — список экспортов для выбора линка
//! (`agr:resolve-browse`).
//!
//! Наружу: [`Browse`], [`RESOLVE_SCHEME`], [`addr`], [`request`], [`parse`],
//! [`client_url`].

use std::collections::BTreeMap;
use std::time::Duration;

use super::link::Link;
use super::Context;
use crate::io::link::{DriverLink, LinkEvent, LinkState};
use crate::io::resolve::{BrowseEvent, DriverBrowse, Export};
use crate::io::Msg;
use crate::scheme::Scheme;

/// Схема протокола resolve — из поставки libtll.
pub const RESOLVE_SCHEME: &str = "yaml://tll/logic/resolve.yaml";

/// Адрес resolve-сервера: `--resolve` (его кладёт в окружение запуск) или
/// `MUTATOR_RESOLVE`.
pub fn addr() -> Option<String> {
    std::env::var("MUTATOR_RESOLVE")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// Url клиента к серверу `addr` (`HOST:PORT`).
pub fn client_url(addr: &str) -> String {
    format!("tcp://{addr};mode=client;scheme={RESOLVE_SCHEME}")
}

/// `Request` на все экспорты: сервис и канал — `*`.
pub fn request(s: &Scheme) -> Result<Msg, String> {
    let st = s
        .messages()
        .find(|m| m.name == "Request")
        .ok_or("в схеме resolve нет Request")?;
    let mut e = crate::edit::Edits::default();
    e.set(0, "Request", "service", "", "*");
    e.set(0, "Request", "channel", "", "*");
    let m = Msg {
        seq: 0,
        msgid: st.msgid,
        data: vec![0; st.size],
    };
    crate::edit::encode::view(s, &e, m)
}

/// Ответ сервера событием; чужое сообщение — `None`.
pub fn parse(s: &Scheme, m: &Msg) -> Option<BrowseEvent> {
    let st = s.message(m.msgid)?;
    let root = crate::decode::decode(s, st, &m.data);
    let text = |name: &str| {
        root.children
            .iter()
            .find(|n| n.name == name)
            .and_then(|n| n.value.as_ref())
            .map(|v| v.text())
            .unwrap_or_default()
    };
    let list = |name: &str| -> Vec<&crate::decode::Node> {
        root.children
            .iter()
            .find(|n| n.name == name)
            .map(|n| n.children.iter().collect())
            .unwrap_or_default()
    };
    match st.name.as_str() {
        "ExportChannel" => Some(BrowseEvent::Export(Export {
            service: text("service"),
            channel: text("channel"),
            tags: list("tags")
                .iter()
                .filter_map(|n| n.value.as_ref().map(|v| v.text()))
                .collect(),
            host: text("host"),
            config: list("config")
                .iter()
                .filter_map(|kv| {
                    let get = |name: &str| {
                        kv.children
                            .iter()
                            .find(|n| n.name == name)
                            .and_then(|n| n.value.as_ref())
                            .map(|v| v.text())
                    };
                    Some((get("key")?, get("value").unwrap_or_default()))
                })
                .collect(),
            messages: None,
        })),
        "DropChannel" => Some(BrowseEvent::DropChannel {
            service: text("service"),
            channel: text("channel"),
        }),
        "DropService" => Some(BrowseEvent::DropService {
            service: text("service"),
        }),
        _ => None,
    }
}

/// Подписка на экспорты resolve-сервера (`agr:resolve-browse`).
pub struct Browse {
    link: Link,
    scheme: Option<Scheme>,
    /// Число сообщений схемы по её хешу: тело приходит в `config` экспорта,
    /// разбирать его на каждом экспорте незачем.
    counts: BTreeMap<String, Option<usize>>,
}

impl Browse {
    /// Подключиться к серверу `addr`; запрос уходит на Active.
    pub fn open(ctx: &Context, addr: &str) -> Result<Browse, String> {
        Ok(Browse {
            link: Link::open(ctx, &client_url(addr), "")?,
            scheme: None,
            counts: BTreeMap::new(),
        })
    }

    /// Сколько сообщений в схеме экспорта: тело `scheme.<hash>` из `config`
    /// для `init.scheme=sha256://<hash>`.
    fn messages(&mut self, e: &Export) -> Option<usize> {
        let hash = e
            .param("scheme")
            .and_then(|s| s.strip_prefix("sha256://").map(str::to_string))?;
        if let Some(n) = self.counts.get(&hash) {
            return *n;
        }
        let body = e
            .config
            .iter()
            .find(|(k, _)| k.strip_prefix("scheme.") == Some(hash.as_str()))
            .map(|(_, v)| v.clone())?;
        let n = super::load_scheme(&body).map(|s| s.messages().count());
        self.counts.insert(hash, n);
        n
    }
}

impl DriverBrowse for Browse {
    fn poll(&mut self, timeout: Duration) -> Vec<BrowseEvent> {
        let mut out = Vec::new();
        for ev in self.link.poll(timeout) {
            match ev {
                LinkEvent::State(LinkState::Active) if self.scheme.is_none() => {
                    let asked = self
                        .link
                        .data_scheme()
                        .ok_or_else(|| "у resolve-сервера нет схемы".to_string())
                        .and_then(|s| {
                            self.link.post(&request(&s)?)?;
                            Ok(s)
                        });
                    match asked {
                        Ok(s) => {
                            self.scheme = Some(s);
                            out.push(BrowseEvent::State(LinkState::Active));
                        }
                        Err(e) => out.push(BrowseEvent::Error(e)),
                    }
                }
                LinkEvent::State(st) => out.push(BrowseEvent::State(st)),
                LinkEvent::Error(e) => out.push(BrowseEvent::Error(e)),
                LinkEvent::Data(m) => {
                    let Some(s) = &self.scheme else { continue };
                    match parse(s, &m) {
                        Some(BrowseEvent::Export(mut e)) => {
                            e.messages = self.messages(&e);
                            out.push(BrowseEvent::Export(e));
                        }
                        Some(ev) => out.push(ev),
                        None => {}
                    }
                }
                LinkEvent::Online(_) => {}
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::{encode, Body};

    /// Схема resolve — копия из поставки libtll: тест не зависит от путей поиска.
    fn scheme() -> Scheme {
        let text = include_str!("../../tests/data/resolve.yaml");
        super::super::load_scheme(&format!("yamls://{text}")).expect("схема resolve")
    }

    fn msg(s: &Scheme, name: &str, body: &[(&str, &str)]) -> Msg {
        let body: Body = body
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let (msgid, data) = encode::compose(s, name, &body).unwrap();
        Msg {
            seq: 0,
            msgid,
            data,
        }
    }

    /// `agr:resolve-browse` — `ExportChannel` разбирается целиком: теги,
    /// хост, пары `config`; `Drop*` — уход канала и сервиса; прочее — мимо.
    #[test]
    fn resolve_answers_are_parsed() {
        let s = scheme();
        let m = msg(
            &s,
            "ExportChannel",
            &[
                ("service", "gw"),
                ("channel", "orders"),
                ("tags[0]", "gateway"),
                ("tags[1]", "a"),
                ("host", "h1"),
                ("config[0].key", "init.tll.proto"),
                ("config[0].value", "stream+pub+tcp"),
                ("config[1].key", "init.tll.host"),
                ("config[1].value", "h1:5555"),
            ],
        );
        let Some(BrowseEvent::Export(e)) = parse(&s, &m) else {
            panic!("не экспорт")
        };
        assert_eq!((e.service.as_str(), e.channel.as_str()), ("gw", "orders"));
        assert_eq!(e.tags, ["gateway", "a"]);
        assert_eq!(e.host, "h1");
        assert_eq!(e.param("tll.proto"), Some("stream+pub+tcp"));
        assert_eq!(e.param("tll.host"), Some("h1:5555"));

        let d = msg(
            &s,
            "DropChannel",
            &[("service", "gw"), ("channel", "orders")],
        );
        assert_eq!(
            parse(&s, &d),
            Some(BrowseEvent::DropChannel {
                service: "gw".into(),
                channel: "orders".into()
            })
        );
        let d = msg(&s, "DropService", &[("service", "gw")]);
        assert_eq!(
            parse(&s, &d),
            Some(BrowseEvent::DropService {
                service: "gw".into()
            })
        );
        let r = request(&s).unwrap();
        assert_eq!(parse(&s, &r), None, "Request — не ответ");
    }
}
