//! Веб-UI (фича `wui`): одна страница `web/index.html`, вшитая в бинарь как
//! есть, без сборки — deb собирается без Node (`agr:serve-wui`). Две вкладки:
//! «Просмотр» — вид [`super::view`] на сервере, страница только рисует его
//! кадры и отдаёт ввод; «API» — формы по схемам `GET /api/v1/ops`, вызовы
//! REST и одно подключение SSE.
//!
//! Виды:
//!
//! | Метод | Путь | Что |
//! |---|---|---|
//! | POST | `/api/v1/views` | открыть вид `{file?}` — `{id, file}`; без файла — список линков resolve |
//! | POST | `/api/v1/views/{id}/input` | ввод, один или массивом: `{key}`, `{char}`, `{ctrl}`, `{click: [r, c]}`, `{wheel}`, `{size: [cols, rows]}` |
//! | GET | `/api/v1/views/{id}/frames` | SSE кадров (`frame`) |
//! | DELETE | `/api/v1/views/{id}` | закрыть |
//!
//! Наружу: [`mount`], [`PAGE`].

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use futures_util::stream::{self, Stream};
use serde::Deserialize;
use serde_json::{json, Value};

use super::rest::Server;
use super::view::{Frame, Input, Start, Views, IDLE};
use crate::io::link::Link;

/// Страница веб-UI.
pub const PAGE: &str = include_str!("../../web/index.html");

#[derive(Clone)]
struct Ui {
    server: Server,
    views: Arc<Views>,
}

/// Страница на `/` и виды под `/api/v1/views`; брошенные виды закрываются сами.
pub fn mount(router: Router, server: Server) -> Router {
    let begin = server.clone();
    let opener = Arc::new(move |url: &str, open: &str| {
        let keep = begin.links().spool_dir().map(|p| p.to_path_buf());
        // Не ждём Active здесь: вид рисует ожидание и по Esc бросает линк.
        Link::start(url, open, None, keep.as_deref(), Arc::new(|| {})).map(Start::Link)
    });
    let adopt_at = server.clone();
    let adopt = Arc::new(move |link: Link| adopt_at.links().adopt(link));
    let release_at = server.clone();
    let release = Arc::new(move |url: &str| {
        release_at.links().close(url);
    });
    let share_at = server.clone();
    let share = Arc::new(
        move |addr: &str, browser: Arc<crate::io::resolve::Browser>| {
            share_at.adopt_resolve(browser);
            let mut recent = crate::recent::Recent::load();
            recent.resolve = Some(addr.to_string());
            let _ = recent.save();
        },
    );
    let views = Arc::new(Views::bind(
        server.resolve_slot(),
        opener,
        adopt,
        release,
        share,
    ));
    tokio::spawn({
        let views = views.clone();
        async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                views.reap(IDLE);
            }
        }
    });
    let ui = Router::new()
        .route("/", get(|| async { Html(PAGE) }))
        .route("/api/v1/views", post(open))
        .route("/api/v1/views/{id}", delete(close))
        .route("/api/v1/views/{id}/input", post(input))
        .route("/api/v1/views/{id}/frames", get(frames))
        .with_state(Ui { server, views });
    router.merge(ui)
}

fn problem(status: StatusCode, detail: String) -> Response {
    let body = json!({
        "type": "about:blank",
        "title": status.canonical_reason().unwrap_or(""),
        "status": status.as_u16(),
        "detail": detail,
    });
    (
        status,
        [("content-type", "application/problem+json")],
        body.to_string(),
    )
        .into_response()
}

#[derive(Deserialize, Default)]
struct Open {
    file: Option<String>,
}

async fn open(State(ui): State<Ui>, body: Option<Json<Open>>) -> Response {
    let name = body
        .and_then(|Json(o)| o.file)
        .filter(|f| !f.is_empty())
        .or_else(|| ui.server.links().default_file().map(str::to_string));
    // Без файла — список линков resolve (`agr:resolve-browse`).
    let Some(name) = name else {
        return Json(json!({ "id": ui.views.open_browse(), "file": null })).into_response();
    };
    let server = ui.server.clone();
    let n = name.clone();
    match tokio::task::spawn_blocking(move || server.links().file(&n)).await {
        Ok(Ok(file)) => Json(json!({ "id": ui.views.open(file), "file": name })).into_response(),
        Ok(Err(e)) => problem(StatusCode::UNPROCESSABLE_ENTITY, e),
        Err(e) => problem(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn close(State(ui): State<Ui>, Path(id): Path<String>) -> Response {
    if ui.views.close(&id) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        problem(StatusCode::NOT_FOUND, format!("вида {id} нет"))
    }
}

/// Ввод — один или массивом по порядку: браузер копит нажатия, пока
/// предыдущая пачка в пути, и порядок клавиш не теряется.
async fn input(State(ui): State<Ui>, Path(id): Path<String>, Json(v): Json<Value>) -> Response {
    let Some(view) = ui.views.get(&id) else {
        return problem(StatusCode::NOT_FOUND, format!("вида {id} нет"));
    };
    let all = match v {
        Value::Array(a) => a,
        v => vec![v],
    };
    for i in all {
        match serde_json::from_value::<Input>(i) {
            Ok(i) => {
                if !view.send(i) {
                    return problem(StatusCode::GONE, format!("вид {id} закрыт"));
                }
            }
            Err(e) => return problem(StatusCode::BAD_REQUEST, e.to_string()),
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn frames(State(ui): State<Ui>, Path(id): Path<String>) -> Response {
    let Some(view) = ui.views.get(&id) else {
        return problem(StatusCode::NOT_FOUND, format!("вида {id} нет"));
    };
    let rx = view.frames();
    // Подписка держит вид живым. Первый кадр — целиком, дальше — только
    // изменившиеся строки (`agr:serve-view-frames`).
    let s: std::pin::Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>> =
        Box::pin(stream::unfold(
            (rx, None::<Arc<Frame>>, ui.server.clone()),
            |(mut rx, prev, srv)| async move {
                loop {
                    let f = rx.borrow_and_update().clone();
                    if let Some(data) = f.diff(prev.as_deref()) {
                        let e = Event::default().event("frame").data(data);
                        return Some((Ok(e), (rx, Some(f), srv)));
                    }
                    // Сервер останавливается — поток кончается (`agr:serve-stop`).
                    tokio::select! {
                        r = rx.changed() => r.ok()?,
                        _ = srv.stopped() => return None,
                    }
                }
            },
        ));
    Sse::new(s)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Страница говорит с сервером только через его API: вид, схемы, вызовы,
    /// линки, события — и ни одного внешнего адреса.
    /// `agr:serve-wui`
    #[test]
    fn the_page_only_talks_to_this_servers_api() {
        for path in ["/ops", "/info", "/links", "/runs/", "/events", "/views"] {
            assert!(PAGE.contains(&format!("api(\"{path}")), "нет {path}");
        }
        assert_eq!(
            PAGE.matches("new EventSource(api(\"/events\"))").count(),
            1,
            "одно SSE событий"
        );
        assert!(
            !PAGE.contains("http://") && !PAGE.contains("https://"),
            "внешний адрес"
        );
    }
}
