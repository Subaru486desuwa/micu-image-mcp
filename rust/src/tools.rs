//! MCP tools 实现。所有 tool 与 Python `server.py` 同名同义。
//!
//! 当前阶段：
//!   - server_info / image_generate 完整实现
//!   - image_edit / image_batch_edit / image_multi_reference 暂返回"未实现"占位
//!     （供后续 commit 填充，见 task #18-#20）
//!
//! 路由策略 / 重试 / 双层锁全部跟 Python 主线对齐。

use std::path::PathBuf;

use anyhow::Result;
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use rmcp::{
    handler::server::wrapper::Parameters, schemars, tool, tool_router,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::{self, MAX_RESPONSE_BYTES};
use crate::http::{HTTP_CLIENT, baseurl, call_with_retry, check_response_size};
use crate::routing::{Tier, reject_4k_with_reference, resolve_model, size_tier};
use crate::sandbox::{resolve_save_dir, safe_basename, validate_image_path, validate_size};

#[derive(Debug, Clone)]
pub struct MicuImageServer;

impl MicuImageServer {
    pub fn new() -> Result<Self> {
        Ok(Self)
    }
}

// ---------------- Args structs ----------------

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GenerateArgs {
    #[schemars(description = "图像描述。1-2000 字符。中英文混合可。")]
    pub prompt: String,
    #[schemars(description = "WxH 字符串如 '1024x1024' / '3840x2160'。留 None 让 MCP 从 prompt 推断。")]
    pub size: Option<String>,
    #[schemars(description = "张数 1-10。≥2K 强制 N=1。")]
    pub n: Option<u32>,
    pub model: Option<String>,
    pub save_dir: Option<String>,
    pub basename: Option<String>,
    pub api_key: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EditArgs {
    pub prompt: String,
    pub image_path: String,
    pub mask_path: Option<String>,
    pub size: Option<String>,
    pub model: Option<String>,
    pub save_dir: Option<String>,
    pub basename: Option<String>,
    pub api_key: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct BatchEditArgs {
    pub prompt: String,
    pub image_paths: Vec<String>,
    pub size: Option<String>,
    pub model: Option<String>,
    pub save_dir: Option<String>,
    pub api_key: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MultiReferenceArgs {
    pub prompt: String,
    pub image_paths: Vec<String>,
    pub size: Option<String>,
    pub model: Option<String>,
    pub save_dir: Option<String>,
    pub basename: Option<String>,
    pub api_key: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EmptyArgs {}

// ---------------- Helpers ----------------

fn infer_size_from_prompt(prompt: &str) -> &'static str {
    let p = prompt.to_lowercase();
    if p.contains("4k") || p.contains("uhd") || p.contains("3840") {
        "3840x2160"
    } else if p.contains("9:16") || p.contains("竖屏") || p.contains("portrait") {
        "1024x1536"
    } else if p.contains("16:9") || p.contains("横屏") || p.contains("landscape") {
        "1536x1024"
    } else {
        "1024x1024"
    }
}

#[derive(Debug, Serialize)]
struct SavedFile {
    index: usize,
    path: String,
    size_bytes: u64,
    actual_size: String,
    actual_megapixels: f64,
}

fn save_image_b64(b64: &str, out_path: &PathBuf) -> Result<SavedFile, String> {
    let bytes = B64
        .decode(b64.trim())
        .map_err(|e| format!("base64 解码失败：{e}"))?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(format!(
            "图片字节数 {:.1}MB > 上限 {}MB",
            bytes.len() as f64 / 1024.0 / 1024.0,
            MAX_RESPONSE_BYTES / 1024 / 1024
        ));
    }
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败：{e}"))?;
    }
    std::fs::write(out_path, &bytes).map_err(|e| format!("落盘失败：{e}"))?;
    let (w, h) = crate::sandbox::detect_actual_size(&bytes).unwrap_or((0, 0));
    Ok(SavedFile {
        index: 1,
        path: out_path.display().to_string(),
        size_bytes: bytes.len() as u64,
        actual_size: format!("{w}x{h}"),
        actual_megapixels: (w as f64 * h as f64) / 1_000_000.0,
    })
}

fn default_basename(prefix: &str) -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{prefix}_{ts}")
}

fn json_value_to_string(v: Value) -> String {
    serde_json::to_string(&v).unwrap_or_else(|_| "{}".into())
}

fn json_error(msg: impl Into<String>) -> String {
    json_value_to_string(json!({"ok": false, "error": msg.into()}))
}

// ---------------- Tool router ----------------

#[tool_router(server_handler)]
impl MicuImageServer {
    /// 文生图。
    #[tool(
        description = "文生图。size 可达真 4K (3840x2160 / 2160x3840)。size=None 时根据 prompt 推断；推断不出来兜底 1024x1024。max edge >= 1600 自动锁 gpt-image-2-pro。≥2K 强制 N=1。返回 JSON {ok, model, size, saved[], errors, notes}。"
    )]
    pub async fn image_generate(
        &self,
        Parameters(args): Parameters<GenerateArgs>,
    ) -> String {
        let key = match config::api_key_or_err(args.api_key.as_deref()) {
            Ok(k) => k,
            Err(e) => return json_error(e),
        };

        let size_str = args
            .size
            .clone()
            .unwrap_or_else(|| infer_size_from_prompt(&args.prompt).to_string());
        let (w, h) = match validate_size(&size_str) {
            Ok(v) => v,
            Err(e) => return json_error(e),
        };
        let n = args.n.unwrap_or(1).clamp(1, 10);
        let tier = size_tier(w, h);

        let basename = match args.basename.as_deref() {
            None => default_basename("gen"),
            Some(b) => match safe_basename(b) {
                Some(s) => s,
                None => {
                    return json_error(format!("basename {b:?} 含非法字符或路径分量"));
                }
            },
        };

        let save_dir = match resolve_save_dir(args.save_dir.as_deref()) {
            Ok(p) => p,
            Err(e) => return json_error(e),
        };

        let (eff_model, mut notes) = resolve_model(args.model.as_deref(), w, h);

        if matches!(tier, Tier::TwoK | Tier::FourK) && n > 1 {
            notes.push(format!("≥2K 强制 N=1（origin pro 串行队列）；n={n} 已 clamp 为 1"));
        }
        let effective_n = if matches!(tier, Tier::TwoK | Tier::FourK) {
            1
        } else {
            n
        };

        let url = format!("{}/v1/images/generations", baseurl());
        let body = json!({
            "model": eff_model,
            "prompt": args.prompt,
            "n": effective_n,
            "size": size_str,
            "response_format": "b64_json",
        });

        let big_size_lock = matches!(tier, Tier::TwoK | Tier::FourK);
        let retry_pro = eff_model.to_lowercase().contains("pro") || big_size_lock;

        let key_clone = key.clone();
        let url_clone = url.clone();
        let body_clone = body.clone();
        let build_req = move || {
            HTTP_CLIENT
                .post(&url_clone)
                .bearer_auth(&key_clone)
                .json(&body_clone)
        };

        let (status, text) = call_with_retry(build_req, retry_pro, big_size_lock).await;

        if let Err(e) = check_response_size(&text) {
            return json_error(e);
        }
        if !(200..300).contains(&status) {
            return json_error(format!(
                "HTTP {status}: {}",
                text.chars().take(400).collect::<String>()
            ));
        }

        let parsed: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => return json_error(format!("响应不是合法 JSON: {e}")),
        };
        let data_arr = parsed
            .get("data")
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default();
        if data_arr.is_empty() {
            return json_error(format!(
                "响应 data 字段为空: {}",
                text.chars().take(400).collect::<String>()
            ));
        }

        let mut saved_files: Vec<Value> = vec![];
        let mut errors: Vec<String> = vec![];
        for (idx, item) in data_arr.iter().enumerate() {
            let b64 = item.get("b64_json").and_then(|v| v.as_str());
            if let Some(b) = b64 {
                let out = save_dir.join(format!("{basename}_{}.png", idx + 1));
                match save_image_b64(b, &out) {
                    Ok(mut f) => {
                        f.index = idx + 1;
                        saved_files.push(serde_json::to_value(&f).unwrap());
                    }
                    Err(e) => errors.push(format!("idx {}: {e}", idx + 1)),
                }
            } else {
                errors.push(format!("idx {}: 无 b64_json 字段", idx + 1));
            }
        }

        json_value_to_string(json!({
            "ok": !saved_files.is_empty(),
            "model": eff_model,
            "size": size_str,
            "requested_n": n,
            "saved": saved_files,
            "errors": errors,
            "notes": notes,
        }))
    }

    /// 单图编辑（image-to-image）。
    #[tool(
        description = "单图编辑 (image-to-image)。1K 走 /v1/images/edits multipart（支持 alpha mask）；2K 走 /v1/images/generations + reference_image data url；4K 入口直接拒（CF 524 物理上限）。max edge >= 1600 自动锁 gpt-image-2-pro。"
    )]
    pub async fn image_edit(
        &self,
        Parameters(args): Parameters<EditArgs>,
    ) -> String {
        let key = match config::api_key_or_err(args.api_key.as_deref()) {
            Ok(k) => k,
            Err(e) => return json_error(e),
        };

        if args.prompt.trim().is_empty() {
            return json_error("prompt 不能为空");
        }

        let size_str = args.size.clone().unwrap_or_else(|| "1024x1024".to_string());
        let (w, h) = match validate_size(&size_str) {
            Ok(v) => v,
            Err(e) => return json_error(e),
        };

        if let Some(rej) = reject_4k_with_reference(w, h, "image_edit") {
            return json_error(rej);
        }

        let basename = match args.basename.as_deref() {
            None => default_basename("edit"),
            Some(b) => match safe_basename(b) {
                Some(s) => s,
                None => return json_error(format!("basename {b:?} 含非法字符或路径分量")),
            },
        };

        let save_dir = match resolve_save_dir(args.save_dir.as_deref()) {
            Ok(p) => p,
            Err(e) => return json_error(e),
        };

        let (img_bytes, img_mime) = match validate_image_path(&args.image_path, "image_path") {
            Ok(v) => v,
            Err(e) => return json_error(e),
        };

        let mask_bytes = if let Some(mp) = args.mask_path.as_deref() {
            match validate_image_path(mp, "mask_path") {
                Ok((b, _)) => Some(b),
                Err(e) => return json_error(e),
            }
        } else {
            None
        };

        let (eff_model, mut notes) = resolve_model(args.model.as_deref(), w, h);
        let tier = size_tier(w, h);
        let big_size_lock = matches!(tier, Tier::TwoK);
        let retry_pro = eff_model.to_lowercase().contains("pro") || big_size_lock;

        if matches!(tier, Tier::TwoK) && mask_bytes.is_some() {
            notes.push(
                "≥2K 路径走 generations + reference_image，不支持 alpha mask；mask 已忽略。"
                    .to_string(),
            );
        }

        let url;
        let resp_text: String;
        let status: u16;

        if matches!(tier, Tier::OneK | Tier::Small) {
            // 1K 路径：multipart edits
            url = format!("{}/v1/images/edits", baseurl());
            let key_clone = key.clone();
            let url_clone = url.clone();
            let img_bytes_clone = img_bytes.clone();
            let img_mime_str = img_mime.to_string();
            let mask_clone = mask_bytes.clone();
            let prompt = args.prompt.clone();
            let model_clone = eff_model.clone();
            let size_clone = size_str.clone();

            let build_req = move || {
                let mut form = reqwest::multipart::Form::new()
                    .text("prompt", prompt.clone())
                    .text("model", model_clone.clone())
                    .text("n", "1")
                    .text("size", size_clone.clone())
                    .text("response_format", "b64_json");
                let img_part = reqwest::multipart::Part::bytes(img_bytes_clone.clone())
                    .file_name("image.png")
                    .mime_str(&img_mime_str)
                    .unwrap();
                form = form.part("image", img_part);
                if let Some(mb) = &mask_clone {
                    let mp = reqwest::multipart::Part::bytes(mb.clone())
                        .file_name("mask.png")
                        .mime_str("image/png")
                        .unwrap();
                    form = form.part("mask", mp);
                }
                HTTP_CLIENT
                    .post(&url_clone)
                    .bearer_auth(&key_clone)
                    .multipart(form)
            };
            let (s, t) = call_with_retry(build_req, retry_pro, false).await;
            status = s;
            resp_text = t;
        } else {
            // ≥2K 路径：generations + reference_image data url
            url = format!("{}/v1/images/generations", baseurl());
            let img_b64 = B64.encode(&img_bytes);
            let data_url = format!("data:{img_mime};base64,{img_b64}");
            let body = json!({
                "model": eff_model,
                "prompt": args.prompt,
                "n": 1,
                "size": size_str,
                "reference_image": data_url,
                "response_format": "b64_json",
            });
            let key_clone = key.clone();
            let url_clone = url.clone();
            let body_clone = body.clone();
            let build_req = move || {
                HTTP_CLIENT
                    .post(&url_clone)
                    .bearer_auth(&key_clone)
                    .json(&body_clone)
            };
            let (s, t) = call_with_retry(build_req, retry_pro, big_size_lock).await;
            status = s;
            resp_text = t;
        }

        if let Err(e) = check_response_size(&resp_text) {
            return json_error(e);
        }
        if !(200..300).contains(&status) {
            return json_error(format!(
                "HTTP {status}: {}",
                resp_text.chars().take(400).collect::<String>()
            ));
        }

        let parsed: Value = match serde_json::from_str(&resp_text) {
            Ok(v) => v,
            Err(e) => return json_error(format!("响应不是合法 JSON: {e}")),
        };
        let item = parsed
            .get("data")
            .and_then(|d| d.as_array())
            .and_then(|a| a.first())
            .cloned();
        let b64 = item
            .as_ref()
            .and_then(|i| i.get("b64_json"))
            .and_then(|v| v.as_str());

        let saved = if let Some(b) = b64 {
            let out = save_dir.join(format!("{basename}.png"));
            match save_image_b64(b, &out) {
                Ok(f) => Some(serde_json::to_value(&f).unwrap()),
                Err(e) => {
                    return json_error(format!("落盘失败：{e}"));
                }
            }
        } else {
            return json_error(format!(
                "响应无 b64_json: {}",
                resp_text.chars().take(400).collect::<String>()
            ));
        };

        json_value_to_string(json!({
            "ok": true,
            "model": eff_model,
            "size": size_str,
            "used_fallback": false,
            "saved": saved,
            "notes": notes,
        }))
    }

    /// 批量编辑：N 进 N 出，统一 prompt + size 应用到每张图。
    #[tool(
        description = "批量编辑 N 进 N 出，每张同指令独立处理。仅 1K 档（≥2K 拒）。1K non-pro 5 并发，1K pro 串行 + 1.5s gap。"
    )]
    pub async fn image_batch_edit(
        &self,
        Parameters(args): Parameters<BatchEditArgs>,
    ) -> String {
        if args.image_paths.is_empty() {
            return json_error("image_paths 不能为空");
        }
        if args.image_paths.len() > 50 {
            return json_error(format!(
                "image_paths 最多 50 张，当前 {} 张",
                args.image_paths.len()
            ));
        }
        let size_str = args.size.clone().unwrap_or_else(|| "1024x1024".to_string());
        let (w, h) = match validate_size(&size_str) {
            Ok(v) => v,
            Err(e) => return json_error(e),
        };
        let tier = size_tier(w, h);
        if matches!(tier, Tier::TwoK | Tier::FourK) {
            return json_error(format!(
                "size={size_str} ({}) 在 image_batch_edit 已禁用：≥2K 走 generations + \
                 reference_image，单张 50s+，N 张串行无法接受。请改 size 到 1K，\
                 或改用 image_edit 单图（自动 ≥2K 走 generations + reference_image）。",
                tier.as_str()
            ));
        }
        let (eff_model, _) = resolve_model(args.model.as_deref(), w, h);
        let is_pro = eff_model.to_lowercase().contains("pro");
        let concurrency = if is_pro { 1 } else { 5 };
        let gap_ms = if is_pro { 1500 } else { 0 };

        // 按 concurrency 分批跑 image_edit；每张内部自带 1K multipart 路径
        let mut results: Vec<Value> = Vec::with_capacity(args.image_paths.len());
        let mut succeeded = 0usize;
        let total = args.image_paths.len();

        let chunks: Vec<Vec<(usize, String)>> = args
            .image_paths
            .iter()
            .enumerate()
            .map(|(i, p)| (i, p.clone()))
            .collect::<Vec<_>>()
            .chunks(concurrency)
            .map(|c| c.to_vec())
            .collect();

        for chunk in chunks {
            let mut handles = vec![];
            for (idx, p) in chunk {
                let single = EditArgs {
                    prompt: args.prompt.clone(),
                    image_path: p.clone(),
                    mask_path: None,
                    size: Some(size_str.clone()),
                    model: args.model.clone(),
                    save_dir: args.save_dir.clone(),
                    basename: Some(default_basename(&format!("batch_{}", idx + 1))),
                    api_key: args.api_key.clone(),
                };
                let server = self.clone();
                let h = tokio::spawn(async move {
                    let resp = server.image_edit(Parameters(single)).await;
                    (idx, p, resp)
                });
                handles.push(h);
            }
            for h in handles {
                match h.await {
                    Ok((idx, p, resp)) => {
                        let parsed: Value =
                            serde_json::from_str(&resp).unwrap_or_else(|_| json!({"ok": false}));
                        let ok = parsed.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                        if ok {
                            succeeded += 1;
                        }
                        results.push(json!({
                            "input_index": idx + 1,
                            "input_path": p,
                            "ok": ok,
                            "result": parsed,
                        }));
                    }
                    Err(e) => results.push(json!({
                        "ok": false,
                        "error": format!("task panic: {e}"),
                    })),
                }
            }
            if gap_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(gap_ms)).await;
            }
        }

        json_value_to_string(json!({
            "ok": succeeded > 0,
            "total": total,
            "succeeded": succeeded,
            "failed": total - succeeded,
            "concurrency": concurrency,
            "results": results,
        }))
    }

    /// 多图融合参考 → 输出 1 张新图。
    #[tool(
        description = "多图融合参考 N 进 1 出（2-10 张）。主路径 /v1/images/generations + image_urls=[...]（米醋扩展字段，size 真实生效）。1K 稳定，2K 不稳（米醋 origin 间歇 500），4K 入口直接拒（CF 524）。"
    )]
    pub async fn image_multi_reference(
        &self,
        Parameters(args): Parameters<MultiReferenceArgs>,
    ) -> String {
        let key = match config::api_key_or_err(args.api_key.as_deref()) {
            Ok(k) => k,
            Err(e) => return json_error(e),
        };

        if args.prompt.trim().is_empty() {
            return json_error("prompt 不能为空");
        }
        if args.image_paths.len() < 2 {
            return json_error(format!(
                "至少需要 2 张参考图（收到 {}）。1 张请用 image_edit；0 张请用 image_generate。",
                args.image_paths.len()
            ));
        }
        if args.image_paths.len() > 10 {
            return json_error(format!(
                "参考图最多 10 张，当前 {} 张。请减少或分批。",
                args.image_paths.len()
            ));
        }

        let size_str = args.size.clone().unwrap_or_else(|| "1024x1024".to_string());
        let (w, h) = match validate_size(&size_str) {
            Ok(v) => v,
            Err(e) => return json_error(e),
        };
        if let Some(rej) = reject_4k_with_reference(w, h, "image_multi_reference") {
            return json_error(rej);
        }

        let basename = match args.basename.as_deref() {
            None => default_basename("multiref"),
            Some(b) => match safe_basename(b) {
                Some(s) => s,
                None => return json_error(format!("basename {b:?} 含非法字符或路径分量")),
            },
        };
        let save_dir = match resolve_save_dir(args.save_dir.as_deref()) {
            Ok(p) => p,
            Err(e) => return json_error(e),
        };

        let mut image_urls: Vec<String> = vec![];
        let mut total_bytes: usize = 0;
        for (i, p) in args.image_paths.iter().enumerate() {
            let (raw, mime) = match validate_image_path(p, &format!("image_paths[{i}]")) {
                Ok(v) => v,
                Err(e) => return json_error(e),
            };
            total_bytes += raw.len();
            if total_bytes > config::MAX_TOTAL_INPUT_BYTES {
                return json_error(format!(
                    "参考图累计 {:.1}MB 超过总量上限 {}MB（base64 后会膨胀 33%）",
                    total_bytes as f64 / 1024.0 / 1024.0,
                    config::MAX_TOTAL_INPUT_BYTES / 1024 / 1024
                ));
            }
            let b64 = B64.encode(&raw);
            image_urls.push(format!("data:{mime};base64,{b64}"));
        }

        let (eff_model, mut notes) = resolve_model(args.model.as_deref(), w, h);
        let tier = size_tier(w, h);
        let big_size_lock = matches!(tier, Tier::TwoK);
        let retry_pro = eff_model.to_lowercase().contains("pro") || big_size_lock;

        let inflated_mb = total_bytes as f64 * 1.33 / 1024.0 / 1024.0;
        if inflated_mb > 4.0 {
            notes.push(format!(
                "参考图体积估 {inflated_mb:.1}MB，部分 serverless 代理可能拒收"
            ));
        }

        let full_prompt = format!(
            "Reference images are provided. Synthesize their visual elements (style, palette, \
             composition, subjects) into ONE single new image per the instruction below. \
             Do NOT collage, tile, or montage the references side-by-side unless explicitly asked.\n\nInstruction:\n{}",
            args.prompt
        );

        let url = format!("{}/v1/images/generations", baseurl());
        let body = json!({
            "model": eff_model,
            "prompt": full_prompt,
            "n": 1,
            "size": size_str,
            "image_urls": image_urls,
            "response_format": "b64_json",
        });
        let key_clone = key.clone();
        let url_clone = url.clone();
        let body_clone = body.clone();
        let build_req = move || {
            HTTP_CLIENT
                .post(&url_clone)
                .bearer_auth(&key_clone)
                .json(&body_clone)
        };
        let (status, text) = call_with_retry(build_req, retry_pro, big_size_lock).await;

        if let Err(e) = check_response_size(&text) {
            return json_error(e);
        }
        if !(200..300).contains(&status) {
            return json_error(format!(
                "HTTP {status}: {}",
                text.chars().take(400).collect::<String>()
            ));
        }

        let parsed: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => return json_error(format!("响应不是合法 JSON: {e}")),
        };
        let b64 = parsed
            .get("data")
            .and_then(|d| d.as_array())
            .and_then(|a| a.first())
            .and_then(|i| i.get("b64_json"))
            .and_then(|v| v.as_str());

        let saved = if let Some(b) = b64 {
            let out = save_dir.join(format!("{basename}.png"));
            match save_image_b64(b, &out) {
                Ok(f) => serde_json::to_value(&f).unwrap(),
                Err(e) => return json_error(format!("落盘失败：{e}")),
            }
        } else {
            return json_error(format!(
                "响应无 b64_json: {}",
                text.chars().take(400).collect::<String>()
            ));
        };

        json_value_to_string(json!({
            "ok": true,
            "model": eff_model,
            "n_references": args.image_paths.len(),
            "size": size_str,
            "saved": saved,
            "notes": notes,
        }))
    }

    /// 诊断。
    #[tool(
        description = "诊断：返回当前 server 配置 / size 路由规则 / 能力矩阵 / 重试策略 / 安全约束。LLM 第一次用本 server 之前调一次。"
    )]
    pub async fn server_info(
        &self,
        Parameters(_): Parameters<EmptyArgs>,
    ) -> String {
        json_value_to_string(json!({
            "base_url": &*config::BASEURL,
            "default_model": &*config::DEFAULT_MODEL_ENV,
            "available_models": ["gpt-image-2", "gpt-image-2-pro"],
            "default_save_dir": config::DEFAULT_SAVE_DIR.display().to_string(),
            "save_root": config::SAVE_ROOT.display().to_string(),
            "api_key_configured": !config::API_KEY.is_empty(),
            "implementation": "rust",
            "size_rules": {
                "format": "WxH 字符串（如 '1024x1024'）",
                "alignment": "W 与 H 都必须是 8 的整数倍（米醋实测约束）",
                "edge_range": "W/H 必须在 [256, 4096]",
                "compress_below_2_25mp": "≤2.25MP 请求被代理放大或压缩到 ~1.57MP（福利档）",
                "exact_above_4mp": "≥4MP 请求严格按 size 1:1 输出",
                "auto_pro_threshold": "max edge ≥ 1600 → 自动锁 gpt-image-2-pro",
            },
            "safety_constraints": {
                "n_range": "image_generate 的 n ∈ [1, 10]",
                "save_dir_root": format!("强制落在 {}", config::SAVE_ROOT.display()),
                "basename_charset": "仅允许 [A-Za-z0-9_-.]",
                "input_size_limits": format!("单图 ≤{}MB；image_multi_reference 总和 ≤{}MB",
                    config::MAX_INPUT_FILE_BYTES / 1024 / 1024,
                    config::MAX_TOTAL_INPUT_BYTES / 1024 / 1024),
                "input_image_validation": "PNG/JPEG/WebP/GIF magic bytes 校验",
                "response_size_limit": format!("{}MB", config::MAX_RESPONSE_BYTES / 1024 / 1024),
                "base_url_locked": "base_url 锁在启动期 MICU_BASEURL env",
            },
            "capability_matrix": {
                "image_generate": {
                    "1k": "可用 30s，N>1 自动 5 并发",
                    "2k_pro": "可用 40-60s，N=1 强制",
                    "4k_pro": "可用 50-80s，N=1 强制",
                },
                "image_edit": {
                    "1k": "可用 ~10s，multipart edits + 可选 alpha mask",
                    "2k_pro": "可用 ~50s，generations + reference_image 字段（不支持 mask）",
                    "4k_pro": "已禁用：origin 处理 4K + 参考图 > 120s，撞 CF 524；入口直接拒",
                },
                "image_batch_edit": {
                    "1k_non_pro": "5 并发",
                    "1k_pro": "串行 + 1.5s gap",
                    ">=2k": "拒绝",
                },
                "image_multi_reference": {
                    "1k": "稳定，2-10 张参考图融合 → 1 张，~30-100s",
                    "2k_pro": "可用但 origin 间歇 500（米醋 image_urls + ≥2K 状态不稳定）",
                    "4k_pro": "已禁用：4K 多图融合稳定 > 120s，入口直接拒",
                },
            },
            "retry_policy": {
                "retryable_status": [0, 408, 429, 500, 502, 503, 504, 520, 521, 522, 523, 524, 525, 527],
                "schedule_1k": "失败 → 4s + jitter → 重试 → 8s + jitter → 重试；网络层异常额外免费重试 1 次",
                "schedule_2k_4k": "双层锁内：失败 → 60s → 重试",
                "concurrency_2k_4k": "进程内 tokio::sync::Mutex(1) + 跨进程 fs2 FileExt（POSIX flock / Windows LockFileEx），整机串行 ≥2K",
            },
        }))
    }
}

