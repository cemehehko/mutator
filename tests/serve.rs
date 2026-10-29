//! Сервер снаружи: живой роутер на свободном порту, запросы — по HTTP, события
//! — из SSE, как их видит тестировщик со своим скриптом.
//!
//! Линк — `lua+file://` на копии эталона, как в `tests/link.rs`; без tll-lua
//! проверка линка пропускается.

use std::path::PathBuf;
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{json, Value};
use tui_patch::io::{self, Context};
use tui_patch::serve::ops::{self, Links};
use tui_patch::serve::rest::{check_bind, guard, router, Server};

fn plain() -> String {
    format!("{}/tests/data/plain.dat", env!("CARGO_MANIFEST_DIR"))
}

/// Поднять сервер на свободном порту; адрес — `http://127.0.0.1:PORT`.
async fn serve() -> String {
    io::quiet();
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}", l.local_addr().unwrap());
    let app = router(Server::default());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    addr
}

/// Клиент мимо прокси окружения: сервер — на локальном порту.
fn http() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

async fn get(url: String) -> reqwest::Response {
    http().get(url).send().await.unwrap()
}

async fn post(addr: &str, op: &str, body: Value) -> reqwest::Response {
    http()
        .post(format!("{addr}/api/v1/ops/{op}"))
        .json(&body)
        .send()
        .await
        .unwrap()
}

/// Кадры SSE как JSON: `data:` каждого события, пока не наберётся `n` или не
/// выйдет время.
async fn frames(addr: &str, path: &str, n: usize, until: impl Fn(&Value) -> bool) -> Vec<Value> {
    let r = get(format!("{addr}{path}")).await;
    assert_eq!(r.headers()["content-type"], "text/event-stream");
    let mut body = r.bytes_stream();
    let mut buf = String::new();
    let mut out = Vec::new();
    let read = async {
        while let Some(chunk) = body.next().await {
            buf.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
            while let Some(end) = buf.find("\n\n") {
                let frame: String = buf.drain(..end + 2).collect();
                for line in frame.lines() {
                    if let Some(d) = line.strip_prefix("data: ") {
                        let v: Value = serde_json::from_str(d).unwrap();
                        let stop = until(&v);
                        out.push(v);
                        if stop || out.len() >= n {
                            return;
                        }
                    }
                }
            }
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(20), read).await;
    out
}

/// Вызов по HTTP отвечает тем же текстом, что операция без транспорта, а
/// список операций — те же схемы.
/// `agr:serve-same-tools`
#[tokio::test(flavor = "multi_thread")]
async fn http_answers_the_same_text_as_the_op_itself() {
    let addr = serve().await;
    assert_eq!(
        get(format!("{addr}/healthz")).await.text().await.unwrap(),
        "ok"
    );
    let info: Value = get(format!("{addr}/api/v1/info"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(info["version"], env!("CARGO_PKG_VERSION"));
    let listed: Value = get(format!("{addr}/api/v1/ops"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(listed, json!(ops::list(&Links::default())));

    let args = json!({"file": plain(), "from": 5, "count": 2});
    let r = post(&addr, "message_types", args.clone()).await;
    assert_eq!(r.status(), 200);
    let got: Value = r.json().await.unwrap();
    let want = ops::call(&Links::default(), "message_types", &args).unwrap();
    assert_eq!(got["result"], want);
}

/// Правка снаружи — 403 ProblemDetails, неизвестная операция — 404, нет
/// файла — 422 с причиной.
/// `agr:serve-no-write`
#[tokio::test(flavor = "multi_thread")]
async fn writes_are_refused_with_problem_details() {
    let addr = serve().await;
    let r = post(&addr, "edit_messages", json!({"file": plain()})).await;
    assert_eq!(r.status(), 403);
    assert_eq!(r.headers()["content-type"], "application/problem+json");
    let p: Value = r.json().await.unwrap();
    assert_eq!(p["status"], 403);
    assert!(
        p["detail"].as_str().unwrap().contains("только читает"),
        "{p}"
    );

    assert_eq!(post(&addr, "no_such", json!({})).await.status(), 404);
    let r = post(&addr, "file_overview", json!({"file": "/нет/такого.dat"})).await;
    assert_eq!(r.status(), 422);
}

/// Долгая операция — `202 {op_id}`, в SSE — начало и итог; пока скан идёт,
/// `/healthz` отвечает; итог виден и после конца: в `/runs/{id}` и первым
/// кадром SSE запуска.
/// `agr:serve-events`
#[tokio::test(flavor = "multi_thread")]
async fn a_long_op_reports_start_and_result_in_sse() {
    let addr = serve().await;
    let events = tokio::spawn({
        let addr = addr.clone();
        async move { frames(&addr, "/api/v1/events", 2, |v| v["type"] == "op_finished").await }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let r = post(
        &addr,
        "count_messages",
        json!({"file": plain(), "expr": "seq > 0"}),
    )
    .await;
    assert_eq!(r.status(), 202);
    let id = r.json::<Value>().await.unwrap()["op_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(get(format!("{addr}/healthz")).await.status(), 200);

    let got = events.await.unwrap();
    assert_eq!(got[0]["type"], "op_started", "{got:?}");
    assert_eq!(got[0]["op_id"], id.as_str());
    let done = got.last().unwrap();
    assert_eq!(done["type"], "op_finished", "{got:?}");
    let want = ops::call(
        &Links::default(),
        "count_messages",
        &json!({"file": plain(), "expr": "seq > 0"}),
    )
    .unwrap();
    assert_eq!(done["result"], want);

    let run: Value = get(format!("{addr}/api/v1/runs/{id}"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(run["state"], "finished");
    let late = frames(&addr, &format!("/api/v1/runs/{id}/events"), 1, |_| true).await;
    assert_eq!(late[0]["type"], "op_finished");
    assert_eq!(late[0]["result"], want);
}

/// Поиск отдаёт найденное по мере скана: каждое — событием `op_item` до
/// итога, с полями по просьбе; итог — все найденные вместе.
/// `agr:serve-events`
#[tokio::test(flavor = "multi_thread")]
async fn search_streams_each_match_before_the_result() {
    let addr = serve().await;
    let events = tokio::spawn({
        let addr = addr.clone();
        async move { frames(&addr, "/api/v1/events", 10, |v| v["type"] == "op_finished").await }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let args = json!({"file": plain(), "expr": "seq % 100 == 7", "count": 50, "bodies": true});
    assert_eq!(post(&addr, "search_messages", args).await.status(), 202);
    let got = events.await.unwrap();
    let kinds: Vec<_> = got.iter().map(|v| v["type"].as_str().unwrap()).collect();
    assert_eq!(
        kinds,
        [
            "op_started",
            "op_item",
            "op_item",
            "op_item",
            "op_item",
            "op_finished"
        ],
        "{got:?}"
    );
    assert!(
        got[1]["text"]
            .as_str()
            .unwrap()
            .starts_with("- seq: 7\n  name: "),
        "{}",
        got[1]["text"]
    );
    let items: String = got[1..5]
        .iter()
        .map(|v| v["text"].as_str().unwrap())
        .collect();
    assert_eq!(got[5]["result"], items.as_str());
}

/// Линк держится открытым по url: первый вызов поднимает спул, список
/// линков и событие `link` показывают его состояние, `DELETE` закрывает.
/// `agr:serve-link-handle`
#[tokio::test(flavor = "multi_thread")]
async fn a_link_stays_open_and_reports_its_state() {
    io::quiet();
    if !Context::new().ensure_lua(io::Backend::TLL) {
        eprintln!("нет tll-lua — проверка линка пропущена");
        return;
    }
    let dir: PathBuf = std::env::temp_dir().join(format!("mutator-serve-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.dat");
    std::fs::copy(plain(), &src).unwrap();
    let url = format!(
        "lua+file://{};autoclose=no;lua.code=function tll_on_data(seq, name, data) \
         tll_callback(seq, name, data) end",
        src.display()
    );
    let addr = serve().await;
    let events = tokio::spawn({
        let addr = addr.clone();
        async move { frames(&addr, "/api/v1/events", 50, |v| v["type"] == "link").await }
    });
    let r = post(&addr, "file_overview", json!({"file": url})).await;
    assert_eq!(r.status(), 200);
    let text = r.json::<Value>().await.unwrap()["result"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(text.contains("live link"), "{text}");

    let links: Value = get(format!("{addr}/api/v1/links"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(links[0]["url"], url.as_str());
    assert_eq!(links[0]["state"], "Active");
    let got = events.await.unwrap();
    assert!(
        got.iter()
            .any(|v| v["type"] == "link" && v["url"] == url.as_str()),
        "{got:?}"
    );

    let del = http()
        .delete(format!("{addr}/api/v1/links"))
        .query(&[("url", &url)])
        .send()
        .await
        .unwrap();
    assert_eq!(del.status(), 204);
    let links: Value = get(format!("{addr}/api/v1/links"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(links, json!([]));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Ctrl+C останавливает сервер, даже когда браузер держит SSE открытым:
/// поток событий кончается, процесс выходит за секунды, а не висит.
/// `agr:serve-stop`
#[tokio::test(flavor = "multi_thread")]
async fn ctrl_c_stops_the_server_with_sse_open() {
    use std::io::{BufRead, BufReader};
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_mutator"))
        .args(["--serve", &format!("127.0.0.1:{port}"), &plain()])
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut err = BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    while !line.contains("слушаю") {
        line.clear();
        assert!(err.read_line(&mut line).unwrap() > 0, "сервер не поднялся");
    }
    let addr = format!("http://127.0.0.1:{port}");
    let r = get(format!("{addr}/api/v1/events")).await;
    let mut body = r.bytes_stream();
    let sse = tokio::spawn(async move { while body.next().await.is_some() {} });

    let t = std::time::Instant::now();
    let pid = child.id().to_string();
    std::process::Command::new("kill")
        .args(["-INT", &pid])
        .status()
        .unwrap();
    let status = tokio::task::spawn_blocking(move || child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(
        t.elapsed() < Duration::from_secs(5),
        "выход за {:?}",
        t.elapsed()
    );
    assert!(status.success(), "{status}");
    tokio::time::timeout(Duration::from_secs(2), sse)
        .await
        .expect("поток SSE закрыт")
        .unwrap();
}

/// Линк, не ставший Active вовремя, — отказ с его состоянием, а не вечное
/// ожидание; в списке открытых его нет.
/// `agr:serve-link-handle`
#[test]
fn a_link_that_does_not_open_in_time_is_refused() {
    io::quiet();
    if !Context::new().ensure_lua(io::Backend::TLL) {
        eprintln!("нет tll-lua — проверка линка пропущена");
        return;
    }
    let url = format!(
        "lua+file://{};autoclose=no;lua.code=function tll_on_data(seq, name, data) \
         tll_callback(seq, name, data) end",
        plain()
    );
    let links = Links::default();
    let e = links.open_within(&url, None, Duration::ZERO).unwrap_err();
    assert!(e.contains("не открылся за 0 с"), "{e}");
    assert!(links.list().is_empty());
    links
        .open_within(&url, None, Duration::from_secs(20))
        .expect("вовремя — открыт");
}

/// Долгая операция без `file` на сервере с файлом по умолчанию: в
/// `op_started` — тот файл, что реально читается.
/// `agr:serve-events`
#[tokio::test(flavor = "multi_thread")]
async fn op_started_names_the_default_file() {
    io::quiet();
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}", l.local_addr().unwrap());
    let app = router(Server::new(Links::new(Some(plain()), None)));
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    let events = tokio::spawn({
        let addr = addr.clone();
        async move { frames(&addr, "/api/v1/events", 2, |v| v["type"] == "op_started").await }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        post(&addr, "count_messages", json!({"expr": "seq > 0"}))
            .await
            .status(),
        202
    );
    let got = events.await.unwrap();
    assert_eq!(got[0]["type"], "op_started", "{got:?}");
    assert_eq!(got[0]["file"], plain().as_str());
}

/// Одновременные первые обращения к одному линку поднимают один канал:
/// оба читают один спул, в карте — одна запись.
/// `agr:serve-link-handle`
#[test]
fn concurrent_first_calls_open_one_link() {
    io::quiet();
    if !Context::new().ensure_lua(io::Backend::TLL) {
        eprintln!("нет tll-lua — проверка линка пропущена");
        return;
    }
    let url = format!(
        "lua+file://{};autoclose=no;lua.code=function tll_on_data(seq, name, data) \
         tll_callback(seq, name, data) end",
        plain()
    );
    let links = std::sync::Arc::new(Links::default());
    let spools: Vec<_> = (0..4)
        .map(|_| {
            let (links, url) = (links.clone(), url.clone());
            std::thread::spawn(move || links.file(&url).unwrap().parts[0].path.clone())
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|t| t.join().unwrap())
        .collect();
    assert!(spools.windows(2).all(|w| w[0] == w[1]), "{spools:?}");
    assert_eq!(links.list().len(), 1);
}

/// Без токена — только loopback: наружу файлы и Lua без входа не отдаём.
/// `agr:serve-token`
#[test]
fn without_a_token_the_server_listens_only_on_loopback() {
    assert!(check_bind("127.0.0.1:0", false).is_ok());
    assert!(check_bind("localhost:0", false).is_ok());
    let e = check_bind("0.0.0.0:0", false).unwrap_err();
    assert!(e.contains("токен"), "{e}");
    assert!(check_bind("0.0.0.0:0", true).is_ok(), "с токеном — можно");
}

/// Токен закрывает весь сервер: без него — 401, с Bearer — ответ; вход по
/// `?token=` ставит cookie, дальше пускает cookie; `/healthz` открыт.
/// `agr:serve-token`
#[tokio::test(flavor = "multi_thread")]
async fn a_token_guards_the_whole_server() {
    io::quiet();
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}", l.local_addr().unwrap());
    let app = guard(router(Server::default()), Some("s3cret".into()));
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    let ops = format!("{addr}/api/v1/ops");
    assert_eq!(get(ops.clone()).await.status(), 401);
    assert_eq!(get(format!("{addr}/api/v1/events")).await.status(), 401);
    assert_eq!(get(format!("{addr}/healthz")).await.status(), 200);
    let wrong = http().get(&ops).bearer_auth("nope").send().await.unwrap();
    assert_eq!(wrong.status(), 401);
    let bearer = http().get(&ops).bearer_auth("s3cret").send().await.unwrap();
    assert_eq!(bearer.status(), 200);

    let entry = get(format!("{ops}?token=s3cret")).await;
    assert_eq!(entry.status(), 200);
    let cookie = entry.headers()["set-cookie"].to_str().unwrap().to_string();
    assert!(
        cookie.starts_with("mutator_token=s3cret;") && cookie.contains("HttpOnly"),
        "{cookie}"
    );
    let pair = cookie.split(';').next().unwrap();
    let by_cookie = http()
        .get(&ops)
        .header("cookie", pair)
        .send()
        .await
        .unwrap();
    assert_eq!(by_cookie.status(), 200);
}

/// `agr:link-post` — снаружи `post_message` шлёт сообщение в линк, если
/// канал его принимает: tcp-клиент к серверу — сообщение у сервера с
/// заданным полем; у файла — 422 с причиной.
#[tokio::test(flavor = "multi_thread")]
async fn post_message_sends_into_a_link() {
    let addr = serve().await;
    let f = tui_patch::data::File::open(std::path::Path::new(&plain())).unwrap();
    let scheme = f.scheme_url.clone().unwrap();
    let port = 41000 + (std::process::id() % 10000) as u16;
    let server = tui_patch::io::link::Link::start(
        &format!("tcp://127.0.0.1:{port};mode=server;scheme={scheme}"),
        "",
        None,
        None,
        std::sync::Arc::new(|| {}),
    )
    .unwrap();
    server.ready(|_| {}).expect("сервер");
    let client = format!("tcp://127.0.0.1:{port};mode=client;scheme={scheme}");
    let r = post(
        &addr,
        "post_message",
        json!({"file": client, "message": "Order", "seq": 5,
               "fields": {"header.order_id": "901", "header.status": "Wait"}}),
    )
    .await;
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert!(v["result"].as_str().unwrap().starts_with("sent"), "{v}");
    let t = std::time::Instant::now();
    while server.status().count < 1 {
        assert!(
            t.elapsed() < Duration::from_secs(10),
            "{}",
            server.status().line()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let mut rd =
        tui_patch::io::Reader::open(&Context::new(), &tui_patch::io::Open::file(server.spool()))
            .unwrap();
    let m = rd.next().unwrap();
    let st = f.scheme.message(m.msgid).unwrap();
    let field = |p: &str| tui_patch::decode::field(&f.scheme, st, &m.data, p);
    assert_eq!(
        (m.seq, field("header.order_id").as_deref()),
        (5, Some("901"))
    );
    assert_eq!(field("header.status").as_deref(), Some("Wait"));

    let r = post(
        &addr,
        "post_message",
        json!({"file": plain(), "message": "Order"}),
    )
    .await;
    assert_eq!(r.status(), 422);
    let p: Value = r.json().await.unwrap();
    assert!(
        p["detail"].as_str().unwrap().contains("not a live link"),
        "{p}"
    );
}
