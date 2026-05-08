//! 输入校验：path 沙箱 / basename 字符集 / size 解析与限制 / image magic bytes / mask 校验。
//!
//! 与 Python `_resolve_save_dir` / `_safe_basename` / `_validate_size` /
//! `_validate_image_path` / `_validate_mask_against_image` 一一对应。

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use regex::Regex;

use crate::config::{
    DEFAULT_SAVE_DIR, MAX_INPUT_FILE_BYTES, MAX_SIZE_EDGE, MIN_SIZE_EDGE, SAVE_ROOT,
};

/// 校验 size = "WxH"，返回 (W, H) 或错误信息。
pub fn validate_size(size: &str) -> Result<(u32, u32), String> {
    let s = size.trim().to_lowercase();
    let re = Regex::new(r"^(\d+)x(\d+)$").unwrap();
    let caps = re
        .captures(&s)
        .ok_or_else(|| format!("size 格式错误：必须是 'WxH'（如 '1024x1024'），收到 {size:?}"))?;
    let w: u32 = caps[1]
        .parse()
        .map_err(|_| format!("size W 不是合法整数：{size}"))?;
    let h: u32 = caps[2]
        .parse()
        .map_err(|_| format!("size H 不是合法整数：{size}"))?;
    if w < MIN_SIZE_EDGE || h < MIN_SIZE_EDGE {
        return Err(format!("size 边长太小（最小 {MIN_SIZE_EDGE}），收到 {size}"));
    }
    if w > MAX_SIZE_EDGE || h > MAX_SIZE_EDGE {
        return Err(format!("size 边长太大（最大 {MAX_SIZE_EDGE}），收到 {size}"));
    }
    if w % 8 != 0 || h % 8 != 0 {
        return Err(format!(
            "size W/H 必须是 8 的整数倍（米醋实测约束），收到 {size}"
        ));
    }
    Ok((w, h))
}

/// basename 仅允许 `[A-Za-z0-9_.-]`，禁含 `/` 和 `..`。返回净化后的字符串。
pub fn safe_basename(name: &str) -> Option<String> {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        return None;
    }
    if name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')) {
        Some(name.to_string())
    } else {
        None
    }
}

/// 解析 save_dir，限制在 SAVE_ROOT 之下；None 走 DEFAULT_SAVE_DIR。
pub fn resolve_save_dir(save_dir: Option<&str>) -> Result<PathBuf, String> {
    let root = SAVE_ROOT.clone();
    if let Err(e) = std::fs::create_dir_all(&root) {
        return Err(format!("无法创建 save root {}: {e}", root.display()));
    }
    let target = match save_dir {
        None => DEFAULT_SAVE_DIR.clone(),
        Some(s) => {
            let expanded = shellexpand_tilde(s);
            PathBuf::from(expanded)
        }
    };
    let resolved = target
        .canonicalize()
        .or_else(|_| {
            std::fs::create_dir_all(&target)
                .map_err(|e| format!("创建目录失败 {}: {e}", target.display()))?;
            target
                .canonicalize()
                .map_err(|e| format!("规范化路径失败 {}: {e}", target.display()))
        })?;
    let root_canon = root
        .canonicalize()
        .map_err(|e| format!("规范化 root 失败：{e}"))?;
    if !resolved.starts_with(&root_canon) {
        return Err(format!(
            "save_dir 必须在安全根目录 {} 之下；收到 {save_dir:?}。\
             留空让 MCP 用默认目录，或先把 MICU_SAVE_DIR_ROOT 改到你想要的位置。",
            root_canon.display()
        ));
    }
    Ok(resolved)
}

/// 读输入图 bytes，magic 校验（PNG/JPEG/WebP/GIF），大小限制。返回 (raw_bytes, mime_str)。
pub fn validate_image_path(path: &str, field: &str) -> Result<(Vec<u8>, &'static str), String> {
    let p = Path::new(path);
    if !p.exists() {
        return Err(format!("{field} 不存在：{path}"));
    }
    if !p.is_file() {
        return Err(format!("{field} 不是普通文件：{path}"));
    }
    let meta = std::fs::metadata(p).map_err(|e| format!("{field} stat 失败：{e}"))?;
    if meta.len() as usize > MAX_INPUT_FILE_BYTES {
        return Err(format!(
            "{field} 过大 {:.1}MB，单图上限 {}MB",
            meta.len() as f64 / 1024.0 / 1024.0,
            MAX_INPUT_FILE_BYTES / 1024 / 1024
        ));
    }
    let bytes = std::fs::read(p).map_err(|e| format!("{field} 读失败：{e}"))?;
    let mime = detect_image_mime(&bytes)
        .ok_or_else(|| format!("{field} 不是 PNG/JPEG/WebP/GIF，magic bytes 校验失败"))?;
    Ok((bytes, mime))
}

fn detect_image_mime(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if b.starts_with(b"\xff\xd8\xff") {
        return Some("image/jpeg");
    }
    if b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    None
}

/// 从 PNG/JPEG header 读 (W, H)；失败返回 None。
pub fn detect_actual_size(bytes: &[u8]) -> Option<(u32, u32)> {
    image::load_from_memory(bytes)
        .ok()
        .map(|img| (img.width(), img.height()))
}

fn shellexpand_tilde(p: &str) -> String {
    if let Some(stripped) = p.strip_prefix('~') {
        if let Some(home) = dirs::home_dir() {
            return format!("{}{}", home.display(), stripped);
        }
    }
    p.to_string()
}

/// 通用 anyhow 包装：把 String 错误转 anyhow::Error。
pub fn map_str_err<T>(r: Result<T, String>) -> Result<T> {
    r.map_err(|s| anyhow!(s))
}
