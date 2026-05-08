//! HTTP 客户端与重试。
//!
//! - 全 server 共享一个 `reqwest::Client`（keepalive 连接池）。
//! - `call_with_retry`：1K = 4s/8s + jitter 双重试；≥2K = 60s 单次重试；网络层 status==0 额外免费重试 1 次。
//! - `big_size_lock=true` 时整个调用包在 (process Mutex) ⊃ (cross-process FileExt) 双层锁内。

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use once_cell::sync::Lazy;
use rand::Rng;
use reqwest::{Client, RequestBuilder};

use crate::config::USE_SHELL_PROXY;
use crate::lock::{BIG_SIZE_PROCESS_LOCK, acquire_async};

pub const RETRYABLE_STATUS: &[u16] = &[
    408, 429, 500, 502, 503, 504, 520, 521, 522, 523, 524, 525, 527,
];

pub static HTTP_CLIENT: Lazy<Client> = Lazy::new(|| {
    let mut b = Client::builder()
        .timeout(Duration::from_secs(180))
        .pool_idle_timeout(Duration::from_secs(60))
        .user_agent(concat!("micu-image-mcp-rust/", env!("CARGO_PKG_VERSION")));
    if !*USE_SHELL_PROXY {
        b = b.no_proxy();
    }
    b.build().expect("reqwest::Client build failed")
});

/// 单次发送：网络层异常 → status=0；正常返回 → (status, body_text)。
async fn attempt(req: RequestBuilder) -> (u16, String) {
    match req.send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let text = resp.text().await.unwrap_or_default();
            (status, text)
        }
        Err(e) => (0, format!("{}: {e}", std::any::type_name_of_val(&e))),
    }
}

fn is_retryable(status: u16) -> bool {
    status == 0 || RETRYABLE_STATUS.contains(&status)
}

fn is_success(status: u16) -> bool {
    (200..300).contains(&status)
}

/// `build_req`: 闭包，每次重试都重新构造请求（reqwest::RequestBuilder 不可 Clone）。
pub async fn call_with_retry<F>(
    build_req: F,
    retry_pro: bool,
    big_size_lock: bool,
) -> (u16, String)
where
    F: Fn() -> RequestBuilder + Send + Sync,
{
    let run = || async {
        let (mut status, mut text) = attempt(build_req()).await;

        if status == 0 {
            tokio::time::sleep(Duration::from_secs(2)).await;
            (status, text) = attempt(build_req()).await;
        }

        if big_size_lock {
            if !is_success(status) && retry_pro && is_retryable(status) {
                tokio::time::sleep(Duration::from_secs(60)).await;
                (status, text) = attempt(build_req()).await;
            }
        } else {
            if !is_success(status) && retry_pro && is_retryable(status) {
                let jitter = rand::thread_rng().gen_range(0.0..2.0);
                tokio::time::sleep(Duration::from_secs_f64(4.0 + jitter)).await;
                (status, text) = attempt(build_req()).await;
            }
            if !is_success(status) && retry_pro && is_retryable(status) {
                let jitter = rand::thread_rng().gen_range(0.0..2.0);
                tokio::time::sleep(Duration::from_secs_f64(8.0 + jitter)).await;
                (status, text) = attempt(build_req()).await;
            }
        }
        (status, text)
    };

    if big_size_lock {
        let proc_lock = Arc::clone(&BIG_SIZE_PROCESS_LOCK);
        let _proc_guard = proc_lock.lock_owned().await;
        let _file_guard = match acquire_async().await {
            Ok(g) => g,
            Err(e) => {
                tracing::error!(?e, "cross-process file lock acquire failed");
                return (0, format!("file lock acquire failed: {e}"));
            }
        };
        run().await
    } else {
        run().await
    }
}

pub fn baseurl() -> String {
    crate::config::BASEURL.clone()
}

/// 校验响应不超过 MAX_RESPONSE_BYTES（reqwest 已读为 String 后才能查 len，
/// 严格意义上这是事后校验；如需流式截断可改 stream API。当前 25MB 上限对响应体足够）。
pub fn check_response_size(text: &str) -> Result<(), String> {
    if text.len() > crate::config::MAX_RESPONSE_BYTES {
        return Err(format!(
            "远端响应 {:.1}MB 超过 {}MB 上限，已中断不落盘",
            text.len() as f64 / 1024.0 / 1024.0,
            crate::config::MAX_RESPONSE_BYTES / 1024 / 1024
        ));
    }
    Ok(())
}
