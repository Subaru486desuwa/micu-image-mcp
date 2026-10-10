use crate::{
    Result,
    clients::{TOOLS, credentials, gated, invoke, mcp_config, read_audit, redact},
    engineering::{provenance, write_report},
    live::Relay,
    mock::MockMicu,
    stdio::isolated_command,
};
use serde_json::{Value, json};
use std::{fs, path::Path, time::Duration};

pub fn validate(calls: &[Value]) -> Result<Value> {
    let images: Vec<_> = calls
        .iter()
        .filter(|call| matches!(call["tool"].as_str(), Some("image_generate" | "image_edit")))
        .collect();
    if images.len() != 4 || images.iter().any(|call| call["result"]["ok"] != true) {
        return Err("expected four successful image tool calls".into());
    }
    let sessions: Vec<_> = images
        .iter()
        .map(|call| &call["result"]["session"])
        .collect();
    let ids: Vec<_> = sessions.iter().map(|value| &value["id"]).collect();
    if !ids[0].is_string()
        || !ids[2].is_string()
        || ids[0] != ids[1]
        || ids[0] != ids[3]
        || ids[0] == ids[2]
    {
        return Err("native harness session routing was not A,A,B,A".into());
    }
    for (index, (revision, parent, history)) in [
        (1, Value::Null, 1),
        (2, json!(1), 2),
        (1, Value::Null, 1),
        (3, json!(2), 3),
    ]
    .into_iter()
    .enumerate()
    {
        if sessions[index]["revision"] != revision
            || sessions[index]["parent_revision"] != parent
            || sessions[index]["history_length"] != history
        {
            return Err("native harness selected the wrong revision chain".into());
        }
    }
    for (edit, previous) in [(1, 0), (3, 1)] {
        let saved = &images[previous]["result"]["saved"];
        let path = if saved.is_array() {
            &saved[0]["path"]
        } else {
            &saved["path"]
        };
        if !path.is_string() || sessions[edit]["input_path"] != *path {
            return Err("native edit did not use the selected previous output".into());
        }
    }
    if images[0]["tool"] != "image_generate" || images[2]["tool"] != "image_generate" {
        return Err("new tasks must generate from text".into());
    }
    let context = sessions[2]["context"].to_string().to_lowercase();
    if [
        "小猫",
        "小狗",
        "滑翔伞",
        "降落伞",
        "摩托",
        "cat",
        "dog",
        "motorbike",
        "motorcycle",
        "parachute",
        "paraglider",
    ]
    .iter()
    .any(|word| context.contains(word))
    {
        return Err("new task inherited another task's constraints".into());
    }
    Ok(
        json!({"ok":true,"routing":["A","A","B","A"],"revisions":[1,2,1,3],"successful_image_calls":4,"new_task_context_isolated":true}),
    )
}

pub struct Options<'a> {
    pub cli: &'a Path,
    pub kind: &'a str,
    pub binary: &'a Path,
    pub model: &'a str,
    pub output: &'a Path,
    pub timeout: u64,
    pub live_images: bool,
    pub catalog: Option<&'a Path>,
}

pub fn native(options: Options<'_>) -> Result<Value> {
    gated("MICU_RUN_HARNESS_TESTS")?;
    if options.live_images {
        gated("MICU_RUN_LIVE_TESTS")?;
    }
    if !["claude", "opencode"].contains(&options.kind) || !(1..=700).contains(&options.timeout) {
        return Err("client must be claude/opencode; timeout must be 1..700".into());
    }
    let credential = credentials()?;
    let key = credential["api_key"]
        .as_str()
        .ok_or("api_key is required on stdin")?;
    let origin = credential["origin"]
        .as_str()
        .ok_or("model origin is required on stdin")?;
    let model = Relay::new(origin, key, false, 32)?;
    let images = MockMicu::exact_images()?;
    let image_relay = if options.live_images {
        Some(Relay::new(
            credential["image_origin"]
                .as_str()
                .unwrap_or("https://www.micuapi.ai"),
            credential["image_api_key"]
                .as_str()
                .ok_or("image_api_key is required on stdin")?,
            true,
            16,
        )?)
    } else {
        None
    };
    let root = tempfile::tempdir()?;
    if let Some(parent) = options
        .output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir(options.output)?;
    let image_dir = options.output.canonicalize()?.join("images");
    fs::create_dir_all(&image_dir)?;
    let root_text = image_dir.to_str().ok_or("invalid image root")?;
    let mut image_env = if let Some(relay) = &image_relay {
        vec![
            ("MICU_API_KEY", "relay-local-placeholder"),
            ("MICU_BASEURL", relay.http.url.as_str()),
        ]
    } else {
        images.env()
    };
    image_env.extend([
        ("MICU_SAVE_DIR", root_text),
        ("MICU_SAVE_DIR_ROOT", root_text),
        ("MICU_INPUT_ROOT", root_text),
        ("MICU_UPDATE_CHECK", "0"),
    ]);
    let audit = root.path().join("audit.jsonl");
    let config = root.path().join("mcp.json");
    let mcp = mcp_config(
        &std::env::current_exe()?,
        options.binary,
        &audit,
        &image_env,
        false,
        options.catalog,
    );
    write_report(&config, &mcp)?;
    let prompts = [
        "请生成一张小猫跳伞的图片。",
        "把它的降落伞换成滑翔伞，保留这只小猫和构图。",
        "现在再生成另一张独立图片：一位成年女性在沙滩海景中晒太阳。",
        "回到之前那张小猫图，把背景改成日落，保留滑翔伞。",
    ];
    let guidance = "使用可用的图片 MCP 工具实际完成请求，每张图只需一张，1024x1024，quality=low；保留被选图片与约束，新主题建立独立会话。只使用图片 MCP 工具，完成后一句中文回复，不展示内部会话ID。";
    let session_dir = tempfile::Builder::new().prefix("session-").tempdir()?;
    let suffix = session_dir
        .path()
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("session identifier unavailable")?;
    // Claude requires a UUID; derive it from cryptographically random tempfile naming and time.
    use sha2::Digest;
    let digest =
        sha2::Sha256::digest(format!("{suffix}{:?}", std::time::SystemTime::now()).as_bytes());
    let hex: String = digest
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let mut session = format!(
        "{}-{}-4{}-a{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[13..16],
        &hex[17..20],
        &hex[20..32]
    );
    let mut turns = Vec::new();
    for (index, prompt) in prompts.iter().enumerate() {
        let mut command = isolated_command(options.cli, root.path());
        command
            .env("ANTHROPIC_API_KEY", "sk-local-harness-placeholder")
            .env("ANTHROPIC_BASE_URL", &model.http.url)
            .env("DEEPSEEK_API_KEY", "sk-local-harness-placeholder")
            .env("CLAUDE_CONFIG_DIR", root.path().join("claude"))
            .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
            .env("ENABLE_TOOL_SEARCH", "false")
            .env("DISABLE_AUTOUPDATER", "1")
            .env("OPENCODE_DISABLE_AUTOUPDATE", "1");
        if options.kind == "claude" {
            let names = TOOLS
                .iter()
                .map(|name| format!("mcp__micu__{name}"))
                .collect::<Vec<_>>()
                .join(",");
            command
                .args([
                    "--bare",
                    "-p",
                    prompt,
                    "--output-format",
                    "json",
                    "--model",
                    options.model,
                    "--effort",
                    "low",
                    "--tools",
                    &names,
                    "--allowedTools",
                    "mcp__micu__*",
                    "--permission-mode",
                    "dontAsk",
                    "--strict-mcp-config",
                    "--mcp-config",
                ])
                .arg(&config)
                .args([
                    "--append-system-prompt",
                    guidance,
                    if index == 0 {
                        "--session-id"
                    } else {
                        "--resume"
                    },
                    &session,
                ]);
        } else {
            let local = &mcp["mcpServers"]["micu"];
            let mut cmd = vec![local["command"].clone()];
            cmd.extend(
                local["args"]
                    .as_array()
                    .ok_or("MCP args missing")?
                    .iter()
                    .cloned(),
            );
            let config = json!({"enabled_providers":["deepseek"],"provider":{"deepseek":{"options":{"baseURL":format!("{}/v1",model.http.url),"apiKey":"{env:DEEPSEEK_API_KEY}"},"models":{options.model:{"name":options.model,"tool_call":true,"reasoning":true,"limit":{"context":1000000,"output":8192}}}}},"tools":{"*":false,"micu_*":true},"permission":{"*":"deny","micu_*":"allow"},"mcp":{"micu":{"type":"local","command":cmd,"environment":local["env"],"enabled":true}},"agent":{"image":{"mode":"primary","prompt":guidance,"steps":8}}});
            command
                .env("OPENCODE_CONFIG_CONTENT", config.to_string())
                .args([
                    "run",
                    "--pure",
                    "--format",
                    "json",
                    "--model",
                    &format!("deepseek/{}", options.model),
                    "--agent",
                    "image",
                    prompt,
                ]);
            if index > 0 {
                command.args(["--session", &session]);
            }
        }
        let (success, seconds, id) = invoke(&mut command, Duration::from_secs(options.timeout))?;
        if options.kind == "opencode" && index == 0 {
            session = id.ok_or("OpenCode returned no session identifier")?;
        }
        turns.push(json!({"turn":index+1,"success":success,"wall_seconds":seconds}));
        if !success {
            break;
        }
    }
    let calls = read_audit(&audit)?;
    let validation = validate(&calls);
    let report = redact(
        json!({"provenance":provenance(options.binary),"client":options.cli,"model":options.model,"turns":turns,"validation":validation.as_ref().ok(),"passed":validation.is_ok(),"model_requests":model.records(),"paid_image_requests":image_relay.as_ref().map(|relay|relay.records().len()).unwrap_or(0),"mcp_calls":calls}),
        &[
            key,
            credential["image_api_key"].as_str().unwrap_or_default(),
        ],
    );
    fs::create_dir_all(options.output)?;
    write_report(&options.output.join("report.json"), &report)?;
    validation?;
    Ok(report)
}
