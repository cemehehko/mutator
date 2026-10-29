//! REST и SSE поверх [`super::ops`]: axum-роутер ([`router`]) и запуск
//! ([`run`]). Здесь только перевод: тело запроса — аргументы операции, ответ
//! операции — JSON, отказ — ProblemDetails (RFC 9457).
//!
//! | Метод | Путь | Что |
//! |---|---|---|
//! | GET | `/healthz` | жив ли сервер |
//! | GET | `/api/v1/info` | версия и файл по умолчанию |
//! | GET | `/api/v1/ops` | операции и схемы аргументов |
//! | POST | `/api/v1/ops/{op}` | вызов: быстрая — `200 {result}`, долгая — `202 {op_id}` |
//! | GET | `/api/v1/runs/{op_id}` | запуск: состояние и итог |
//! | GET | `/api/v1/runs/{op_id}/events` | SSE одного запуска |
//! | GET | `/api/v1/events` | SSE всех событий |
//! | GET, POST, DELETE | `/api/v1/links` | открытые линки: список, открыть `{url, open?}`, закрыть `?url=` |
//!
//! Долгая операция — запуск с номером: события `op_started`, найденное
//! поиском — `op_item` по мере скана, потом `op_finished` с итогом или
//! `op_failed` (`agr:serve-events`); последние
//! [`HISTORY`] запусков отвечают и после конца. Все вызовы — в
//! `spawn_blocking`: скан не держит ни `/healthz`, ни SSE.
//!
//! События — `broadcast` на [`BUFFER`]; отставший клиент получает `resync` и
//! читает дальше. Состояние открытых линков сервер сверяет каждые
//! [`LINK_TICK`] и шлёт событие `link` на каждую перемену (`agr:serve-link-handle`).
//!
//! Доступ: без токена — только loopback ([`check_bind`]); с токеном он
//! закрывает весь роутер ([`guard`], `agr:serve-token`).
//!
//! По SIGINT или SIGTERM сервер закрывает потоки SSE и ждёт вызовы в полёте
//! не дольше [`GRACE`]; повторный Ctrl+C — выход сразу (`agr:serve-stop`).
//!
//! Наружу: [`router`], [`run`], [`serve_on`], [`Server`], [`Event`], [`HISTORY`], [`BUFFER`],
//! [`LINK_TICK`], [`GRACE`], [`guard`], [`check_bind`].

use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::stream::{self, Stream};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::broadcast;

use super::ops::{self, LinkInfo, Links};

/// Сколько запусков помнить после конца.
pub const HISTORY: usize = 50;
/// Буфер событий: отставший дальше получает `resync`.
pub const BUFFER: usize = 1024;
/// Как часто сверять состояние линков.
pub const LINK_TICK: Duration = Duration::from_millis(500);

/// Событие SSE.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    OpStarted {
        op_id: String,
        op: String,
        file: String,
    },
    /// Найденное поиском — по мере скана, до итога.
    OpItem {
        op_id: String,
        op: String,
        text: String,
    },
    OpFinished {
        op_id: String,
        op: String,
        result: String,
        duration_ms: u64,
    },
    OpFailed {
        op_id: String,
        op: String,
        error: String,
    },
    Link(LinkInfo),
    Resync {
        reason: String,
    },
}

impl Event {
    /// Имя события SSE: `op` — запуски, `link`, `resync`.
    fn name(&self) -> &'static str {
        match self {
            Event::Link(_) => "link",
            Event::Resync { .. } => "resync",
            _ => "op",
        }
    }

    fn op_id(&self) -> Option<&str> {
        match self {
            Event::OpStarted { op_id, .. }
            | Event::OpItem { op_id, .. }
            | Event::OpFinished { op_id, .. }
            | Event::OpFailed { op_id, .. } => Some(op_id),
            _ => None,
        }
    }
}

/// Запуск долгой операции.
#[derive(Debug, Clone, Serialize)]
struct Run {
    op_id: String,
    op: String,
    file: String,
    /// `running`, `finished`, `failed`.
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// Состояние сервера: линки, события, запуски.
#[derive(Clone)]
pub struct Server {
    inner: Arc<Inner>,
}

struct Inner {
    links: Links,
    /// Сервер останавливается: потоки SSE закрываются сами
    /// (`agr:serve-stop`).
    stop: tokio::sync::watch::Sender<bool>,
    events: broadcast::Sender<Event>,
    runs: Mutex<VecDeque<Run>>,
    next: AtomicU64,
}

impl Default for Server {
    fn default() -> Self {
        Server::new(Links::default())
    }
}

impl Server {
    /// Сервер на этих линках (с файлом по умолчанию и `--keep`).
    pub fn new(links: Links) -> Self {
        Server {
            inner: Arc::new(Inner {
                links,
                stop: tokio::sync::watch::channel(false).0,
                events: broadcast::channel(BUFFER).0,
                runs: Mutex::default(),
                next: AtomicU64::new(1),
            }),
        }
    }

    /// Остановиться: открытые потоки SSE (события, кадры видов, MCP)
    /// закрываются, новых не будет.
    pub fn stop(&self) {
        self.inner.stop.send_replace(true);
    }

    /// Дождаться остановки ([`Server::stop`]) — потокам, что живут долго.
    pub async fn stopped(&self) {
        let mut rx = self.inner.stop.subscribe();
        let _ = rx.wait_for(|s| *s).await;
    }

    /// Линки сервера — общие для всех транспортов.
    pub fn links(&self) -> &Links {
        &self.inner.links
    }

    fn emit(&self, e: Event) {
        let _ = self.inner.events.send(e);
    }

    fn set_run(&self, run: Run) {
        let mut runs = self.inner.runs.lock().unwrap();
        match runs.iter_mut().find(|r| r.op_id == run.op_id) {
            Some(r) => *r = run,
            None => {
                runs.push_back(run);
                while runs.len() > HISTORY {
                    runs.pop_front();
                }
            }
        }
    }

    /// Следить за линками: событие на каждую перемену состояния.
    async fn watch_links(self) {
        let mut seen: Vec<LinkInfo> = Vec::new();
        loop {
            tokio::time::sleep(LINK_TICK).await;
            let now = self.inner.links.list();
            for l in &now {
                if !seen.contains(l) {
                    self.emit(Event::Link(l.clone()));
                }
            }
            seen = now;
        }
    }
}

/// Роутер REST и SSE на этом состоянии; линки он сверяет сам, пока жив рантайм.
pub fn router(server: Server) -> Router {
    tokio::spawn(server.clone().watch_links());
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/v1/info", get(info))
        .route(
            "/api/v1/ops",
            get(|State(s): State<Server>| async move { Json(ops::list(&s.inner.links)) }),
        )
        .route("/api/v1/ops/{op}", post(call))
        .route("/api/v1/runs/{op_id}", get(run_state))
        .route("/api/v1/runs/{op_id}/events", get(run_events))
        .route("/api/v1/events", get(all_events))
        .route(
            "/api/v1/links",
            get(links).post(open_link).delete(close_link),
        )
        .with_state(server)
}

/// Cookie, которым браузер носит токен после входа по `?token=`.
const TOKEN_COOKIE: &str = "mutator_token";

/// Можно ли слушать `addr`: без токена — только loopback; снаружи файлы и
/// Lua-выражения без входа не отдаём (`agr:serve-token`).
pub fn check_bind(addr: &str, token: bool) -> Result<(), String> {
    use std::net::ToSocketAddrs;
    if token {
        return Ok(());
    }
    let all = addr.to_socket_addrs().map_err(|e| format!("{addr}: {e}"))?;
    match all.into_iter().find(|a| !a.ip().is_loopback()) {
        Some(a) => Err(format!(
            "--serve {addr}: адрес {} доступен не только с этой машины — задайте токен \
             (--token или MUTATOR_TOKEN), он закроет весь сервер",
            a.ip()
        )),
        None => Ok(()),
    }
}

/// Закрыть весь роутер токеном (`None` — как есть): `Authorization: Bearer`,
/// cookie [`TOKEN_COOKIE`] или `?token=` — он же ставит cookie, так входит
/// страница и её EventSource. `/healthz` открыт (`agr:serve-token`).
pub fn guard(router: Router, token: Option<String>) -> Router {
    match token {
        Some(t) => router.layer(middleware::from_fn_with_state(Arc::new(t), auth)),
        None => router,
    }
}

/// Строки равны, время сравнения от места различия не зависит.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

async fn auth(State(token): State<Arc<String>>, req: Request, next: Next) -> Response {
    if req.uri().path() == "/healthz" {
        return next.run(req).await;
    }
    let ok = |t: Option<&str>| t.is_some_and(|t| same(t, &token));
    let h = req.headers();
    let bearer = h
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let cookie = h
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|c| {
            c.split(';').find_map(|kv| {
                kv.trim()
                    .strip_prefix(&format!("{TOKEN_COOKIE}="))
                    .map(str::to_string)
            })
        });
    if ok(bearer) || ok(cookie.as_deref()) {
        return next.run(req).await;
    }
    let query = req.uri().query().and_then(|q| {
        q.split('&')
            .find_map(|kv| kv.strip_prefix("token="))
            .map(str::to_string)
    });
    if ok(query.as_deref()) {
        let mut resp = next.run(req).await;
        let c = format!("{TOKEN_COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict");
        if let Ok(v) = HeaderValue::from_str(&c) {
            resp.headers_mut().append(header::SET_COOKIE, v);
        }
        return resp;
    }
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
        "нужен токен: заголовок Authorization: Bearer или вход по ссылке /?token=…",
    )
        .into_response()
}

/// Сколько ждать вызовы в полёте после сигнала остановки.
pub const GRACE: Duration = Duration::from_secs(10);

/// Слушать `addr` до SIGINT или SIGTERM; `wrap` добавляет к роутеру другие
/// транспорты того же сервера (MCP, веб).
pub fn run(addr: &str, server: Server, wrap: impl FnOnce(Router) -> Router) -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let done = rt.block_on(async {
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| format!("{addr}: {e}"))?;
        eprintln!(
            "mutator: слушаю http://{}",
            listener.local_addr().map_err(|e| e.to_string())?
        );
        serve_on(listener, server, wrap, shutdown()).await
    });
    // Долгий скан в `spawn_blocking` выход не держит.
    rt.shutdown_timeout(Duration::from_secs(1));
    done
}

/// Обслуживать `listener` до `signal`: потом SSE закрываются, вызовы в
/// полёте ждём не дольше [`GRACE`], повторный Ctrl+C — выход сразу
/// (`agr:serve-stop`).
pub async fn serve_on(
    listener: tokio::net::TcpListener,
    server: Server,
    wrap: impl FnOnce(Router) -> Router,
    signal: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), String> {
    let stop = server.clone();
    let app = wrap(router(server.clone()));
    let serve = axum::serve(listener, app).with_graceful_shutdown(async move {
        signal.await;
        eprintln!("mutator: останавливаюсь…");
        stop.stop();
    });
    tokio::select! {
        r = serve => r.map_err(|e| e.to_string()),
        _ = async {
            server.stopped().await;
            tokio::select! {
                _ = tokio::time::sleep(GRACE) => eprintln!("mutator: вызовы не кончились за {} с — выхожу", GRACE.as_secs()),
                _ = tokio::signal::ctrl_c() => {}
            }
        } => Ok(()),
    }
}

async fn shutdown() {
    let int = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(t) => t,
                Err(_) => return int.await.unwrap_or(()),
            };
        tokio::select! {
            _ = int => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = int.await;
}

/// Отказ — ProblemDetails.
fn problem(status: StatusCode, detail: impl Into<String>) -> Response {
    let body = json!({
        "type": "about:blank",
        "title": status.canonical_reason().unwrap_or(""),
        "status": status.as_u16(),
        "detail": detail.into(),
    });
    (
        status,
        [(header::CONTENT_TYPE, "application/problem+json")],
        body.to_string(),
    )
        .into_response()
}

fn op_problem(e: &ops::Error) -> Response {
    let status = match e {
        ops::Error::Unknown(_) => StatusCode::NOT_FOUND,
        ops::Error::Forbidden(_) => StatusCode::FORBIDDEN,
        ops::Error::Failed(_) => StatusCode::UNPROCESSABLE_ENTITY,
    };
    problem(status, e.to_string())
}

async fn call(
    State(s): State<Server>,
    Path(op): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let args = body.map(|Json(v)| v).unwrap_or_else(|| json!({}));
    if !args.is_object() {
        return problem(StatusCode::BAD_REQUEST, "тело — объект аргументов");
    }
    if !ops::is_long(&op) {
        let srv = s.clone();
        return match tokio::task::spawn_blocking(move || ops::call(&srv.inner.links, &op, &args))
            .await
        {
            Ok(Ok(result)) => Json(json!({ "result": result })).into_response(),
            Ok(Err(e)) => op_problem(&e),
            Err(e) => problem(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        };
    }
    let op_id = format!("r{}", s.inner.next.fetch_add(1, Ordering::Relaxed));
    // В событие — то, что реально откроется: без `file` — файл по умолчанию.
    let file = args["file"]
        .as_str()
        .or(s.inner.links.default_file())
        .unwrap_or_default()
        .to_string();
    let run = Run {
        op_id: op_id.clone(),
        op: op.clone(),
        file: file.clone(),
        state: "running",
        result: None,
        error: None,
    };
    s.set_run(run.clone());
    s.emit(Event::OpStarted {
        op_id: op_id.clone(),
        op: op.clone(),
        file,
    });
    let reply = op_id.clone();
    let srv = s.clone();
    tokio::spawn(async move {
        let t = Instant::now();
        let links = srv.clone();
        let (o, a) = (op.clone(), args);
        let id = op_id.clone();
        let out = tokio::task::spawn_blocking(move || {
            let mut each = |text: &str| {
                links.emit(Event::OpItem {
                    op_id: id.clone(),
                    op: o.clone(),
                    text: text.to_string(),
                })
            };
            ops::call_each(&links.inner.links, &o, &a, &mut each)
        })
        .await
        .unwrap_or_else(|e| Err(ops::Error::Failed(e.to_string())));
        let (state, ev) = match out {
            Ok(result) => (
                Run {
                    state: "finished",
                    result: Some(result.clone()),
                    ..run
                },
                Event::OpFinished {
                    op_id,
                    op,
                    result,
                    duration_ms: t.elapsed().as_millis() as u64,
                },
            ),
            Err(e) => (
                Run {
                    state: "failed",
                    error: Some(e.to_string()),
                    ..run
                },
                Event::OpFailed {
                    op_id,
                    op,
                    error: e.to_string(),
                },
            ),
        };
        srv.set_run(state);
        srv.emit(ev);
    });
    (StatusCode::ACCEPTED, Json(json!({ "op_id": reply }))).into_response()
}

async fn info(State(s): State<Server>) -> Json<Value> {
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "default": s.inner.links.default_file(),
    }))
}

async fn run_state(State(s): State<Server>, Path(op_id): Path<String>) -> Response {
    let runs = s.inner.runs.lock().unwrap();
    match runs.iter().find(|r| r.op_id == op_id) {
        Some(r) => Json(r).into_response(),
        None => problem(StatusCode::NOT_FOUND, format!("запуска {op_id} нет")),
    }
}

async fn all_events(State(s): State<Server>) -> impl IntoResponse {
    sse(&s, None)
}

/// SSE одного запуска: сначала его состояние, если он уже кончился, —
/// подписавшийся после конца итог не теряет.
async fn run_events(State(s): State<Server>, Path(op_id): Path<String>) -> impl IntoResponse {
    sse(&s, Some(op_id))
}

fn sse(
    s: &Server,
    only: Option<String>,
) -> Sse<impl Stream<Item = Result<axum::response::sse::Event, Infallible>>> {
    let rx = s.inner.events.subscribe();
    // Кончившийся запуск: итог — первым событием.
    let done = only.as_ref().and_then(|id| {
        let runs = s.inner.runs.lock().unwrap();
        let r = runs.iter().find(|r| &r.op_id == id)?;
        match r.state {
            "finished" => Some(Event::OpFinished {
                op_id: r.op_id.clone(),
                op: r.op.clone(),
                result: r.result.clone().unwrap_or_default(),
                duration_ms: 0,
            }),
            "failed" => Some(Event::OpFailed {
                op_id: r.op_id.clone(),
                op: r.op.clone(),
                error: r.error.clone().unwrap_or_default(),
            }),
            _ => None,
        }
    });
    let head = stream::iter(done.map(frame).map(Ok));
    let srv = s.clone();
    let tail = stream::unfold((rx, only, srv), |(mut rx, only, srv)| async move {
        loop {
            let got = tokio::select! {
                got = rx.recv() => got,
                _ = srv.stopped() => return None,
            };
            let e = match got {
                Ok(e) => e,
                Err(broadcast::error::RecvError::Lagged(n)) => Event::Resync {
                    reason: format!("пропущено {n} событий"),
                },
                Err(broadcast::error::RecvError::Closed) => return None,
            };
            if let Some(id) = &only {
                if e.op_id().is_some_and(|x| x != id) || matches!(e, Event::Link(_)) {
                    continue;
                }
            }
            return Some((Ok(frame(e)), (rx, only, srv)));
        }
    });
    Sse::new(stream::StreamExt::chain(head, tail))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

fn frame(e: Event) -> axum::response::sse::Event {
    axum::response::sse::Event::default()
        .event(e.name())
        .data(serde_json::to_string(&e).unwrap_or_default())
}

async fn links(State(s): State<Server>) -> Json<Vec<LinkInfo>> {
    Json(s.inner.links.list())
}

#[derive(Deserialize)]
struct OpenLink {
    url: String,
    open: Option<String>,
}

async fn open_link(State(s): State<Server>, Json(req): Json<OpenLink>) -> Response {
    let srv = s.clone();
    let url = req.url.clone();
    let out = tokio::task::spawn_blocking(move || {
        srv.inner
            .links
            .open(&req.url, req.open.as_deref())
            .map(|_| ())
    })
    .await;
    match out {
        Ok(Ok(())) => {
            let info = s.inner.links.list().into_iter().find(|l| l.url == url);
            Json(json!(info)).into_response()
        }
        Ok(Err(e)) => problem(StatusCode::UNPROCESSABLE_ENTITY, e),
        Err(e) => problem(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Deserialize)]
struct CloseLink {
    url: String,
}

async fn close_link(State(s): State<Server>, Query(q): Query<CloseLink>) -> Response {
    if s.inner.links.close(&q.url) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        problem(StatusCode::NOT_FOUND, format!("линк {} не открыт", q.url))
    }
}
