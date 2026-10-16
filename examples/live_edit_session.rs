//! Opt-in, three-request visual acceptance through the actual Rust MCP binary.
//! API keys enter through the environment or a hidden terminal prompt, never CLI args/files.
use std::{
    error::Error,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    time::Instant,
};

use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

struct Server {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    key: SecretString,
}

impl Server {
    fn start(binary: &Path, root: &Path, key: &SecretString) -> Result<Self> {
        let mut child = Command::new(binary)
            .env("MICU_API_KEY", key.expose_secret())
            .env("MICU_BASEURL", "https://www.micuapi.ai")
            .env("MICU_SAVE_DIR", root)
            .env("MICU_SAVE_DIR_ROOT", root)
            .env("MICU_INPUT_ROOT", root)
            .env("MICU_RESPONSE_FORMAT", "url")
            .env("MICU_USE_SHELL_PROXY", "0")
            .env_remove("MICU_MODEL")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let input = child.stdin.take().ok_or("MCP stdin unavailable")?;
        let output = BufReader::new(child.stdout.take().ok_or("MCP stdout unavailable")?);
        let mut server = Self {
            child,
            input,
            output,
            key: key.clone(),
        };
        server.request(
            1,
            "initialize",
            json!({
                "protocolVersion": "2024-11-05", "capabilities": {},
                "clientInfo": {"name": "rust-edit-session-acceptance", "version": "1"},
            }),
        )?;
        writeln!(
            server.input,
            "{}",
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
        )?;
        server.input.flush()?;
        Ok(server)
    }

    fn request(&mut self, id: u32, method: &str, params: Value) -> Result<Value> {
        writeln!(
            self.input,
            "{}",
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
        )?;
        self.input.flush()?;
        let mut line = String::new();
        // Cap a line before allocation grows without bound. Saved tool results are small JSON.
        std::io::Read::by_ref(&mut self.output)
            .take(1_048_577)
            .read_line(&mut line)?;
        if line.len() > 1_048_576 {
            return Err("MCP reply exceeds acceptance limit".into());
        }
        let response: Value = serde_json::from_str(&line)?;
        if response["id"] != id || response.get("error").is_some() {
            return Err("MCP returned a protocol error (raw payload omitted)".into());
        }
        Ok(response["result"].clone())
    }

    fn call(&mut self, id: u32, name: &str, arguments: Value) -> Result<Value> {
        let result = self.request(
            id,
            "tools/call",
            json!({"name": name, "arguments": arguments}),
        )?;
        if result["isError"] == true {
            return Err("MCP tool failed (raw payload omitted)".into());
        }
        let data = result["structuredContent"].clone();
        if data["ok"] != true {
            let detail = data["error"]
                .as_str()
                .or_else(|| data["errors"][0].as_str())
                .unwrap_or_default();
            let status = detail
                .split("HTTP ")
                .nth(1)
                .map(|text| {
                    text.chars()
                        .take_while(char::is_ascii_digit)
                        .collect::<String>()
                })
                .filter(|status| !status.is_empty())
                .unwrap_or_else(|| "unknown".into());
            let category = if detail.contains("model") || detail.contains("模型") {
                "model/API rejection"
            } else if detail.contains("quota") || detail.contains("余额") {
                "quota rejection"
            } else if detail.contains("key") || detail.contains("token") || detail.contains("令牌")
            {
                "credential/access rejection"
            } else {
                "request rejected"
            };
            let safe_detail = micu_image_mcp::http::response::sanitize_sensitive(
                detail,
                &[self.key.expose_secret()],
            )
            .split_whitespace()
            .map(|word| {
                if word.contains("https://") || word.contains("http://") {
                    "[URL]"
                } else {
                    word
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(240)
            .collect::<String>();
            return Err(format!(
                "{name} failed: HTTP {status}, {category}: {safe_detail}; no subsequent paid requests were sent"
            )
            .into());
        }
        Ok(data)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _kill = self.child.kill();
        let _wait = self.child.wait();
    }
}

fn stage(
    report: &mut Vec<Value>,
    label: &str,
    started: Instant,
    result: &Value,
    generated: bool,
) -> Result<PathBuf> {
    let saved = if generated {
        &result["saved"][0]
    } else {
        &result["saved"]
    };
    let path = PathBuf::from(saved["path"].as_str().ok_or("missing saved image path")?);
    let image = image::ImageReader::open(&path)?
        .with_guessed_format()?
        .into_dimensions()?;
    report.push(json!({
        "stage": label, "elapsed_seconds": started.elapsed().as_secs_f64(),
        "model": result["model"], "requested_size": result["size"],
        "saved": {"path": path, "actual_size": format!("{}x{}", image.0, image.1), "size_bytes": path.metadata()?.len()},
        "session": result.get("session"),
    }));
    std::fs::write(
        path.parent()
            .ok_or("output parent missing")?
            .join("live-report.json"),
        serde_json::to_vec_pretty(
            &json!({"status": "in_progress", "visual_review": "pending", "stages": report}),
        )?,
    )?;
    writeln!(
        std::io::stdout().lock(),
        "{label}: {}x{} saved to {}",
        image.0,
        image.1,
        path.display()
    )?;
    Ok(path)
}

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var("MICU_RUN_LIVE_TESTS").as_deref() != Ok("1") {
        return Err("Paid acceptance requires MICU_RUN_LIVE_TESTS=1".into());
    }
    let mut args = std::env::args_os().skip(1);
    let output = PathBuf::from(
        args.next()
            .ok_or("usage: live_edit_session OUTPUT_DIR [RUST_BINARY]")?,
    );
    std::fs::create_dir_all(&output)?;
    let output = output.canonicalize()?;
    let binary = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/debug/micu-image-mcp"))
        .canonicalize()?;
    let key: SecretString = match std::env::var("MICU_API_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
    {
        Some(key) => key.into(),
        None => rpassword::prompt_password("API key (hidden): ")?.into(),
    };
    if std::env::var("MICU_LIVE_CHECK_MODELS").as_deref() == Ok("1") {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let mut response = client
            .get("https://www.micuapi.ai/v1/models")
            .bearer_auth(key.expose_secret())
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await?;
        let status = response.status();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if bytes.len() + chunk.len() > 1_048_576 {
                return Err("model discovery response too large".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let data: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let has = |name| {
            data["data"]
                .as_array()
                .is_some_and(|models| models.iter().any(|model| model["id"] == name))
        };
        writeln!(
            std::io::stdout().lock(),
            "Access probe: HTTP {status}; flare={}, sunburst={}, image2={}, image2_openai={}",
            has("gpt-image-2.5-flare-openai"),
            has("gpt-image-2.5-sunburst"),
            has("gpt-image-2"),
            has("gpt-image-2-openai")
        )?;
        return Ok(());
    }
    let generate_model = std::env::var("MICU_LIVE_GENERATE_MODEL")
        .unwrap_or_else(|_| "gpt-image-2.5-flare-openai".into());
    let edit_model =
        std::env::var("MICU_LIVE_EDIT_MODEL").unwrap_or_else(|_| "gpt-image-2.5-sunburst".into());
    // Micu's Image 2.5 channel currently rejects higher qualities despite listing the models.
    let quality = std::env::var("MICU_LIVE_QUALITY").unwrap_or_else(|_| "low".into());
    let mut report = Vec::new();
    let mut server = Server::start(&binary, &output, &key)?;
    let started = Instant::now();
    let original = server.call(2, "image_generate", json!({
        "prompt": "一张专业生活方式人像照片：一位年轻成年东亚女性，黑色齐肩短发，黑色圆框眼镜，穿纯红色针织衫，左手端着无图案的白色陶瓷咖啡杯。半身正面构图，人物居中，自然微笑看向镜头。在明亮的室内窗边，背景是浅色墙面和一盆绿色植物。柔和自然光，清晰真实的面部细节。没有文字、标志或水印。",
        "size": "1024x1024", "model": generate_model, "quality": quality, "n": 1, "basename": "01_original",
        "session_id": "new",
        "context": {"identity": "同一人物的面容、黑色短发和黑色圆框眼镜", "clothing": "红色针织衫", "background": "明亮室内窗边，浅色墙面和绿色植物", "composition": "人物居中的半身正面构图，保持姿势和白色咖啡杯"},
    }))?;
    let original_path = stage(&mut report, "original", started, &original, true)?;
    let id = original["session"]["id"]
        .as_str()
        .ok_or("generation session ID missing")?
        .to_owned();
    if original["session"]["revision"] != 1 {
        return Err("first generation did not start session memory".into());
    }
    drop(server);
    let mut server = Server::start(&binary, &output, &key)?;
    let started = Instant::now();
    let beach = server.call(3, "image_edit", json!({
        "prompt": "只把室内背景替换为日落时的真实海边：清晰的海平线、海浪和温暖天空。保留同一人物的面容、黑色短发、黑色圆框眼镜、红色针织衫、白色咖啡杯、姿势和半身构图。自然融合日落光线。",
        "session_id": id, "model": edit_model, "quality": quality, "basename": "02_beach",
        "context": {"background": "日落海边，海浪与海平线"},
    }))?;
    let beach_path = stage(&mut report, "beach_edit", started, &beach, false)?;
    if beach["session"]["id"] != id
        || beach["session"]["revision"] != 2
        || beach["session"]["parent_revision"] != 1
        || beach["session"]["input_path"].as_str() != original_path.to_str()
    {
        return Err("first edit did not recover the generation session".into());
    }
    drop(server);
    // Restart the real MCP binary: the next request must recover its persistent context.
    let mut server = Server::start(&binary, &output, &key)?;
    let started = Instant::now();
    let blue = server.call(
        2,
        "image_generate",
        json!({
            "prompt": "把人物的针织衫改成纯蓝色，让面料纹理清晰自然。",
            "session_id": id, "context": {"clothing": "蓝色针织衫"}, "basename": "03_blue",
        }),
    )?;
    if blue["session"]["revision"] != 3
        || blue["session"]["parent_revision"] != 2
        || blue["session"]["history_length"] != 3
        || blue["session"]["input_path"].as_str() != beach_path.to_str()
        || blue["session"]["context"]["clothing"] != "蓝色针织衫"
        || blue["session"]["context"]["background"] != "日落海边，海浪与海平线"
    {
        return Err("continued edit did not preserve its parent/context contract".into());
    }
    stage(
        &mut report,
        "blue_generation_after_restart",
        started,
        &blue,
        true,
    )?;
    std::fs::write(
        output.join("live-report.json"),
        serde_json::to_vec_pretty(&json!({
            "status": "requests_completed", "visual_review": "pending", "paid_calls_requested": 3,
            "mcp_restart_after_generation": true,
            "mcp_restart_between_edits": true, "stages": report,
        }))?,
    )?;
    writeln!(
        std::io::stdout().lock(),
        "Three image requests completed; visual review required. Report: {}",
        output.join("live-report.json").display()
    )?;
    Ok(())
}
