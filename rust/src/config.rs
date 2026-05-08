//! 启动期 env 配置：MICU_API_KEY / BASEURL / SAVE_DIR / SAVE_DIR_ROOT / MODEL / USE_SHELL_PROXY.
//!
//! 与 Python `server.py` 顶部的环境变量读取语义保持一致。

use std::env;
use std::path::PathBuf;

use once_cell::sync::Lazy;

pub const DEFAULT_BASEURL: &str = "https://www.micuapi.ai";
pub const DEFAULT_MODEL: &str = "gpt-image-2";
pub const PRO_MODEL: &str = "gpt-image-2-pro";

/// `max(W, H) >= 1600` → 自动锁 pro 模型；image_edit 此档走 generations + reference_image。
pub const HIGH_RES_EDGE: u32 = 1600;
/// size 边长上下限。
pub const MIN_SIZE_EDGE: u32 = 256;
pub const MAX_SIZE_EDGE: u32 = 4096;
/// 输入图与响应大小限制。
pub const MAX_INPUT_FILE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_TOTAL_INPUT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 25 * 1024 * 1024;

pub static BASEURL: Lazy<String> =
    Lazy::new(|| env::var("MICU_BASEURL").unwrap_or_else(|_| DEFAULT_BASEURL.to_string()));

pub static API_KEY: Lazy<String> = Lazy::new(|| env::var("MICU_API_KEY").unwrap_or_default());

pub static DEFAULT_MODEL_ENV: Lazy<String> =
    Lazy::new(|| env::var("MICU_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string()));

pub static SAVE_ROOT: Lazy<PathBuf> = Lazy::new(|| {
    let raw = env::var("MICU_SAVE_DIR_ROOT")
        .ok()
        .or_else(|| env::var("MICU_SAVE_DIR").ok())
        .unwrap_or_else(|| {
            dirs::home_dir()
                .map(|h| h.join("Pictures").join("micu-out"))
                .unwrap_or_else(|| PathBuf::from("./micu-out"))
                .display()
                .to_string()
        });
    PathBuf::from(shellexpand_tilde(&raw))
});

pub static DEFAULT_SAVE_DIR: Lazy<PathBuf> = Lazy::new(|| {
    env::var("MICU_SAVE_DIR")
        .map(|s| PathBuf::from(shellexpand_tilde(&s)))
        .unwrap_or_else(|_| SAVE_ROOT.clone())
});

pub static USE_SHELL_PROXY: Lazy<bool> = Lazy::new(|| {
    matches!(
        env::var("MICU_USE_SHELL_PROXY").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
});

fn shellexpand_tilde(p: &str) -> String {
    if let Some(stripped) = p.strip_prefix('~') {
        if let Some(home) = dirs::home_dir() {
            return format!("{}{}", home.display(), stripped);
        }
    }
    p.to_string()
}

pub fn api_key_or_err(override_key: Option<&str>) -> Result<String, String> {
    if let Some(k) = override_key.filter(|s| !s.is_empty()) {
        return Ok(k.to_string());
    }
    if API_KEY.is_empty() {
        return Err(
            "未配置 API key。请设置 MICU_API_KEY 环境变量，或在调用时传 api_key 参数。".to_string(),
        );
    }
    Ok(API_KEY.clone())
}
