//! MCP поверх [`super::ops`]: тот же список и тот же вызов, что у REST, —
//! только в протоколе MCP (rmcp). `tools/list` — [`ops::list`], `tools/call` —
//! [`ops::call`] в `spawn_blocking`; долгие сканы MCP ждёт целиком — ответ
//! на вызов и есть итог (`agr:serve-mcp`). Отказ операции — результат с
//! `isError` и текстом причины: клиент MCP показывает его модели, а
//! протокольную ошибку — нет.
//!
//! Два транспорта: streamable HTTP на `/mcp` того же сервера — только с
//! bearer-токеном ([`mount`]: без токена `/mcp` нет вовсе), и stdio
//! ([`stdio`]): stdout — только протокол, журнал — в stderr.
//!
//! Наружу: [`Mcp`], [`mount`], [`stdio`].

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::Router;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
    ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde_json::Value;

use super::ops;
use super::rest::Server;

/// Обработчик MCP на состоянии сервера — тех же линках, что у REST.
#[derive(Clone)]
pub struct Mcp {
    server: Server,
}

impl Mcp {
    pub fn new(server: Server) -> Mcp {
        Mcp { server }
    }

    /// Операции списком инструментов MCP: только чтение.
    fn tools(&self) -> Vec<Tool> {
        ops::list(self.server.links())
            .into_iter()
            .filter_map(|o| {
                let schema = o["parameters"].as_object()?.clone();
                let tool = Tool::new(
                    o["name"].as_str()?.to_string(),
                    o["description"].as_str().unwrap_or_default().to_string(),
                    schema,
                );
                Some(tool.annotate(ToolAnnotations::new().read_only(true).destructive(false)))
            })
            .collect()
    }
}

impl ServerHandler for Mcp {
    fn get_info(&self) -> ServerConfig {
        let mut info = ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "Read-only access to TLL data files and live links: the same tools the \
                 mutator agent uses. Pass `file` — a path or a link url — unless the \
                 server was started with a default one.",
            );
        info.server_info = Implementation::new("mutator", env!("CARGO_PKG_VERSION"));
        info
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let args = Value::Object(request.arguments.unwrap_or_default());
        let op = request.name.to_string();
        let server = self.server.clone();
        let out = tokio::task::spawn_blocking(move || ops::call(server.links(), &op, &args))
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(match out {
            Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
            Err(e) => CallToolResult::error(vec![ContentBlock::text(e.to_string())]),
        }
        .into())
    }
}

/// Смонтировать `/mcp` (streamable HTTP) за bearer-токеном; без токена —
/// роутер как был.
pub fn mount(router: Router, server: Server, token: Option<String>) -> Router {
    let Some(token) = token else {
        return router;
    };
    // Хост не сверяем: доступ держит токен, а слушать могут и не loopback.
    let config = StreamableHttpServerConfig::default().disable_allowed_hosts();
    // Сессии MCP держат свои потоки SSE: остановка сервера их гасит
    // (`agr:serve-stop`).
    tokio::spawn({
        let (cancel, server) = (config.cancellation_token.clone(), server.clone());
        async move {
            server.stopped().await;
            cancel.cancel();
        }
    });
    let mcp = Mcp::new(server);
    let service: StreamableHttpService<Mcp, LocalSessionManager> =
        StreamableHttpService::new(move || Ok(mcp.clone()), Default::default(), config);
    let guarded = Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn_with_state(Arc::new(token), bearer));
    router.merge(guarded)
}

async fn bearer(State(token): State<Arc<String>>, req: Request, next: Next) -> Response {
    let ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| t == token.as_str());
    if ok {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            "нужен токен",
        )
            .into_response()
    }
}

/// MCP по stdio до конца ввода: stdout — только протокол.
pub fn stdio(server: Server) -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        // Руками в терминале молчание похоже на зависание: сказать, чего ждём.
        eprintln!("mutator: MCP по stdio — жду клиента (JSON-RPC в stdin), Ctrl+D — выход");
        let running = Mcp::new(server)
            .serve(rmcp::transport::stdio())
            .await
            .map_err(|e| e.to_string())?;
        running.waiting().await.map_err(|e| e.to_string())?;
        Ok(())
    })
}
