//! 米醋 gpt-image-2 MCP server — Rust port.
//!
//! 把 `python server.py` 的全套功能（4 tool + size 路由 + 双层锁 + 沙箱 + 重试）
//! 用 rmcp 1.x + tokio + reqwest 重写。语义与 Python 主线对齐。

use anyhow::Result;
use rmcp::{ServiceExt, transport::stdio};
use tracing_subscriber::EnvFilter;

mod config;
mod http;
mod lock;
mod routing;
mod sandbox;
mod tools;

use tools::MicuImageServer;

#[tokio::main]
async fn main() -> Result<()> {
    // stderr 走 tracing；stdout 是 MCP JSON-RPC 通道，绝对不能混入日志。
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    tracing::info!("micu-image-mcp (rust) starting");

    let server = MicuImageServer::new()?;
    let service = server.serve(stdio()).await.inspect_err(|e| {
        tracing::error!(?e, "stdio service init failed");
    })?;

    let reason = service.waiting().await?;
    tracing::info!(?reason, "service exited");
    Ok(())
}
