//! Вид браузера снаружи: открыть файл, читать кадры из SSE, слать клавиши —
//! как страница `/`. Экран — тот же, что у `--tui`.

use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{json, Value};
use tui_patch::io;
use tui_patch::serve::rest::{router, Server};
use tui_patch::serve::wui::mount;

fn plain() -> String {
    format!("{}/tests/data/plain.dat", env!("CARGO_MANIFEST_DIR"))
}

fn http() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

async fn serve() -> String {
    io::quiet();
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}", l.local_addr().unwrap());
    let server = Server::default();
    let app = mount(router(server.clone()), server);
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    addr
}

/// Текст экрана строками.
fn text(f: &Value) -> Vec<String> {
    f["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            l.as_array()
                .unwrap()
                .iter()
                .map(|r| r[0].as_str().unwrap())
                .collect()
        })
        .collect()
}

type Body =
    std::pin::Pin<Box<dyn futures_util::Stream<Item = reqwest::Result<axum::body::Bytes>> + Send>>;

/// Кадры вида, собранные в экран, как их собирает страница: полный кадр
/// заменяет всё, разница — свои строки.
struct Frames {
    body: Body,
    buf: String,
    screen: Value,
}

impl Frames {
    async fn open(addr: &str, id: &str) -> Frames {
        let r = http()
            .get(format!("{addr}/api/v1/views/{id}/frames"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.headers()["content-type"], "text/event-stream");
        Frames {
            body: Box::pin(r.bytes_stream()),
            buf: String::new(),
            screen: json!({"cols": 0, "rows": 0, "lines": []}),
        }
    }

    fn apply(&mut self, f: Value) {
        self.screen["cols"] = f["cols"].clone();
        self.screen["rows"] = f["rows"].clone();
        self.screen["split"] = f["split"].clone();
        if f["full"] == true {
            self.screen["lines"] = f["lines"].clone();
            return;
        }
        let lines = self.screen["lines"].as_array_mut().unwrap();
        for (i, l) in f["lines"].as_object().unwrap() {
            lines[i.parse::<usize>().unwrap()] = l.clone();
        }
    }

    /// Экран, когда `ok` скажет «да» (или 20 с).
    /// Экран, когда `ok` скажет «да», или `None` за `wait`.
    async fn try_until(&mut self, wait: Duration, ok: impl Fn(&Value) -> bool) -> Option<Value> {
        let read = async {
            while let Some(chunk) = self.body.next().await {
                self.buf.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
                while let Some(end) = self.buf.find("\n\n") {
                    let frame: String = self.buf.drain(..end + 2).collect();
                    for line in frame.lines() {
                        if let Some(d) = line.strip_prefix("data: ") {
                            self.apply(serde_json::from_str(d).unwrap());
                            if ok(&self.screen) {
                                return true;
                            }
                        }
                    }
                }
            }
            false
        };
        let found = tokio::time::timeout(wait, read).await.unwrap_or(false);
        found.then(|| self.screen.clone())
    }

    /// Экран, когда `ok` скажет «да» (или 20 с).
    async fn until(&mut self, what: &str, ok: impl Fn(&Value) -> bool) -> Value {
        match self.try_until(Duration::from_secs(20), ok).await {
            Some(f) => f,
            None => panic!("{what}: не дождались; экран {:#?}", text(&self.screen)),
        }
    }
}

async fn input(addr: &str, id: &str, v: Value) {
    let r = http()
        .post(format!("{addr}/api/v1/views/{id}/input"))
        .json(&v)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204, "{v}");
}

/// Вид — тот же экран, что у TUI: заголовок файла, строки сообщений; клавиши
/// двигают выбор; правка — отказ «только просмотр»; закрытый вид — 404.
/// `agr:serve-view-desk`
#[tokio::test(flavor = "multi_thread")]
async fn a_browser_view_is_the_tui_desk_and_only_looks() {
    let addr = serve().await;
    let r = http()
        .post(format!("{addr}/api/v1/views"))
        .json(&json!({"file": plain()}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let id = r.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut frames = Frames::open(&addr, &id).await;
    let has = |f: &Value, s: &str| text(f).iter().any(|l| l.contains(s));
    frames
        .until("заголовок и строки", |f| {
            has(f, "plain.dat") && has(f, "строка 1 из")
        })
        .await;

    input(&addr, &id, json!([{"key": "Down"}, {"key": "Down"}])).await;
    frames
        .until("третья строка", |f| has(f, "строка 3 из"))
        .await;

    // Деталь и правка поля в ней — отказ: вид только смотрит.
    input(
        &addr,
        &id,
        json!([{"key": "Enter"}, {"key": "Tab"}, {"char": "e"}]),
    )
    .await;
    frames
        .until("отказ в правке", |f| has(f, "только просмотр"))
        .await;

    let del = http()
        .delete(format!("{addr}/api/v1/views/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(del.status(), 204);
    let r = http()
        .post(format!("{addr}/api/v1/views/{id}/input"))
        .json(&json!({"key": "Down"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
}

/// Размер сетки задаёт браузер: кадр перестраивается под него.
/// `agr:serve-view-frames`
#[tokio::test(flavor = "multi_thread")]
async fn the_frame_follows_the_browser_size() {
    let addr = serve().await;
    let r = http()
        .post(format!("{addr}/api/v1/views"))
        .json(&json!({"file": plain()}))
        .send()
        .await
        .unwrap();
    let id = r.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut frames = Frames::open(&addr, &id).await;
    input(&addr, &id, json!({"size": [90, 25]})).await;
    let f = frames
        .until("90×25", |f| f["cols"] == 90 && f["rows"] == 25)
        .await;
    assert_eq!(f["lines"].as_array().unwrap().len(), 25);
    input(&addr, &id, json!({"size": [60, 12]})).await;
    frames
        .until("60×12", |f| f["cols"] == 60 && f["rows"] == 12)
        .await;
}

/// Границу детали тянут мышью: нажали на ней, повели, отпустили — граница
/// там, где отпустили.
/// `agr:detail-share`
#[tokio::test(flavor = "multi_thread")]
async fn the_detail_border_follows_the_mouse() {
    let addr = serve().await;
    let r = http()
        .post(format!("{addr}/api/v1/views"))
        .json(&json!({"file": plain()}))
        .send()
        .await
        .unwrap();
    let id = r.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut frames = Frames::open(&addr, &id).await;
    input(&addr, &id, json!([{"size": [80, 40]}, {"key": "Enter"}])).await;
    let f = frames
        .until("деталь открыта", |f| f["split"].is_u64())
        .await;
    // Граница могла сдвинуться в кадре, что ещё в пути (строка состояния):
    // тогда тянем заново от её нового места — как сделал бы человек.
    let mut target = None;
    for _ in 0..3 {
        let at = frames.screen["split"]
            .as_u64()
            .unwrap_or(f["split"].as_u64().unwrap());
        input(
            &addr,
            &id,
            json!([{"click": [at, 5]}, {"drag": [at - 4, 5]}, {"drag": [at - 8, 5]}, "release"]),
        )
        .await;
        if frames
            .try_until(Duration::from_secs(3), |f| f["split"] == at - 8)
            .await
            .is_some()
        {
            target = Some(at - 8);
            break;
        }
    }
    let at8 = target.expect("граница не тянется");
    // Отпустили — дальше мышь границу не двигает; ↓ — чтобы кадр точно пришёл.
    input(&addr, &id, json!([{"drag": [at8 + 8, 5]}, {"key": "Down"}])).await;
    let has = |f: &Value, s: &str| text(f).iter().any(|l| l.contains(s));
    let f = frames.until("деталь seq 1", |f| has(f, "seq 1 ")).await;
    assert_eq!(f["split"], at8);
}
