//! Size → tier / model 路由 helpers.
//!
//! 与 Python `_max_edge` / `_size_tier` / `_resolve_model` / `_bypass_edits` /
//! `_reject_4k_with_reference` 一一对应。

use crate::config::{DEFAULT_MODEL_ENV, HIGH_RES_EDGE, PRO_MODEL};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Small,
    OneK,
    TwoK,
    FourK,
    Unknown,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Small => "small",
            Tier::OneK => "1k",
            Tier::TwoK => "2k",
            Tier::FourK => "4k",
            Tier::Unknown => "unknown",
        }
    }
}

pub fn max_edge(w: u32, h: u32) -> u32 {
    w.max(h)
}

pub fn size_tier(w: u32, h: u32) -> Tier {
    let e = max_edge(w, h);
    if e == 0 {
        Tier::Unknown
    } else if e < 1024 {
        Tier::Small
    } else if e < HIGH_RES_EDGE {
        Tier::OneK
    } else if e < 3000 {
        Tier::TwoK
    } else {
        Tier::FourK
    }
}

/// 返回 (effective_model, notes)：≥2K 强制 pro。
pub fn resolve_model(requested: Option<&str>, w: u32, h: u32) -> (String, Vec<String>) {
    let mut notes = vec![];
    let tier = size_tier(w, h);
    let req = requested
        .map(|s| s.to_string())
        .unwrap_or_else(|| DEFAULT_MODEL_ENV.clone());
    if matches!(tier, Tier::TwoK | Tier::FourK) && !req.to_lowercase().contains("pro") {
        notes.push(format!(
            "size={w}x{h} ({}) 仅 pro 支持，已自动切到 {PRO_MODEL}",
            tier.as_str()
        ));
        return (PRO_MODEL.to_string(), notes);
    }
    (req, notes)
}

pub fn bypass_edits(model: &str, w: u32, h: u32) -> bool {
    model.to_lowercase().contains("pro") && max_edge(w, h) >= HIGH_RES_EDGE
}

/// ≥4K image_edit / image_multi_reference 在米醋 origin 稳定 > 120s 撞 CF 524；入口直接拒。
pub fn reject_4k_with_reference(w: u32, h: u32, tool: &str) -> Option<String> {
    if size_tier(w, h) != Tier::FourK {
        return None;
    }
    Some(format!(
        "size={w}x{h} (4K) 在 {tool} 已禁用：origin 处理 4K + 参考图稳定 > 120s，\
         撞 Cloudflare Proxy Read Timeout 物理上限。请改用 2K：\
         横屏 \"2048x1152\" / 竖屏 \"1152x2048\" / 方形 \"2048x2048\"。\
         若必须 4K，可两步法：先 1K/2K 出综合图 → 再用 image_generate(size=\"3840x2160\") \
         描述同场景升 4K（人物 ID 不保证一致）。"
    ))
}
