//! Сервер для тестировщиков: датник и линк снаружи — скриптом, LLM-клиентом
//! или браузером, без окна и терминала. Операции — читающие инструменты
//! агента, одни на все транспорты (`agr:serve-same-tools`); писать сервер не
//! умеет (`agr:serve-no-write`). Транспорт — фича сборки (`agr:serve-features`).
//!
//! Файлы: `ops.rs` — операции без транспорта: отбор инструментов, аргумент
//! `file`, открытые линки; `rest.rs` — REST и SSE на axum; `mcp.rs` — MCP
//! по HTTP и stdio (фича `mcp`); `wui.rs` — веб-UI на `/`, `view.rs` — вид
//! браузера: `Desk` на сервере и его кадры (фича `wui`).
//!
//! Наружу: [`ops`], [`rest`], `mcp`, `wui`, `view`.

#[cfg(feature = "mcp")]
pub mod mcp;
pub mod ops;
pub mod rest;
#[cfg(feature = "wui")]
pub mod view;
#[cfg(feature = "wui")]
pub mod wui;
