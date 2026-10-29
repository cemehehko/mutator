//! MCP снаружи: клиент rmcp против живого `/mcp` (HTTP с токеном) и против
//! бинаря с `--mcp-stdio` — как их видит LLM-клиент тестировщика.

use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use rmcp::ServiceExt;
use serde_json::{json, Value};
use tui_patch::io;
use tui_patch::serve::mcp::mount;
use tui_patch::serve::ops::{self, Links};
use tui_patch::serve::rest::{router, Server};

const TOKEN: &str = "t0ken";

fn plain() -> String {
    format!("{}/tests/data/plain.dat", env!("CARGO_MANIFEST_DIR"))
}

/// Сервер с `/mcp` (или без, `token: None`); адрес — `http://127.0.0.1:PORT`.
async fn serve(token: Option<&str>) -> String {
    io::quiet();
    // Прокси окружения мимо: сервер на локальном порту.
    std::env::set_var("NO_PROXY", "127.0.0.1,localhost");
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}", l.local_addr().unwrap());
    let server = Server::default();
    let app = mount(router(server.clone()), server, token.map(str::to_string));
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    addr
}

fn text(r: &CallToolResult) -> String {
    r.content
        .iter()
        .filter_map(|c| match c {
            ContentBlock::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect()
}

fn args(v: Value) -> serde_json::Map<String, Value> {
    v.as_object().unwrap().clone()
}

/// Инструменты MCP — операции сервера, ответ — тот же текст; правка —
/// `isError` с причиной.
/// `agr:serve-mcp`
#[tokio::test(flavor = "multi_thread")]
async fn mcp_tools_are_the_ops_and_answer_the_same_text() {
    let addr = serve(Some(TOKEN)).await;
    let config =
        StreamableHttpClientTransportConfig::with_uri(format!("{addr}/mcp")).auth_header(TOKEN);
    let client = ().serve(StreamableHttpClientTransport::from_config(config)).await.unwrap();

    let info = client.peer_info().unwrap();
    assert_eq!(
        info.server_info.as_ref().map(|s| s.name.as_str()),
        Some("mutator")
    );
    let tools = client.list_all_tools().await.unwrap();
    let mut names: Vec<_> = tools.iter().map(|t| t.name.to_string()).collect();
    names.sort();
    let mut want: Vec<_> = ops::OPS.iter().map(|s| s.to_string()).collect();
    want.sort();
    assert_eq!(names, want);
    assert!(tools
        .iter()
        .all(|t| t.annotations.as_ref().and_then(|a| a.read_only_hint) == Some(true)));

    let a = json!({"file": plain(), "from": 3, "count": 2});
    let got = client
        .call_tool(CallToolRequestParams::new("read_messages").with_arguments(args(a.clone())))
        .await
        .unwrap();
    assert_eq!(got.is_error, Some(false));
    assert_eq!(
        text(&got),
        ops::call(&Links::default(), "read_messages", &a).unwrap()
    );

    let refused = client
        .call_tool(
            CallToolRequestParams::new("edit_messages")
                .with_arguments(args(json!({"file": plain()}))),
        )
        .await
        .unwrap();
    assert_eq!(refused.is_error, Some(true));
    assert!(
        text(&refused).contains("только читает"),
        "{}",
        text(&refused)
    );
    client.cancel().await.unwrap();
}

/// `/mcp` — только с токеном: без токена сервера — 404, с чужим — 401.
/// `agr:serve-mcp`
#[tokio::test(flavor = "multi_thread")]
async fn mcp_needs_the_bearer_token() {
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
    let without = serve(None).await;
    let r = http
        .post(format!("{without}/mcp"))
        .json(&init)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    let with = serve(Some(TOKEN)).await;
    let r = http
        .post(format!("{with}/mcp"))
        .bearer_auth("чужой")
        .json(&init)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
}

/// `--mcp-stdio FILE`: stdout — только протокол (клиент поднимается), файл
/// из командной строки — по умолчанию.
/// `agr:serve-mcp`
#[tokio::test(flavor = "multi_thread")]
async fn stdio_serves_the_default_file() {
    io::quiet();
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_mutator"));
    cmd.arg("--mcp-stdio").arg(plain());
    let client = ().serve(TokioChildProcess::new(cmd).unwrap()).await.unwrap();
    let got = client
        .call_tool(CallToolRequestParams::new("message_types"))
        .await
        .unwrap();
    let want = ops::call(
        &Links::default(),
        "message_types",
        &json!({"file": plain()}),
    )
    .unwrap();
    assert_eq!(text(&got), want);
    client.cancel().await.unwrap();
}
