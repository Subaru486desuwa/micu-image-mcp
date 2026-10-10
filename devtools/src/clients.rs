use crate::{
    Result,
    engineering::{provenance, write_report},
    mock::{HttpFixture, body, respond},
    stdio::isolated_command,
};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read},
    path::Path,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

pub const TOOLS: [&str; 5] = [
    "image_generate",
    "image_edit",
    "image_batch_edit",
    "image_multi_reference",
    "server_info",
];

pub fn run_client(mut command: Command, timeout: Duration) -> Result<(bool, f64)> {
    let (success, seconds, _) = invoke(&mut command, timeout)?;
    Ok((success, seconds))
}

pub fn invoke(command: &mut Command, timeout: Duration) -> Result<(bool, f64, Option<String>)> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let output = child.stdout.take().ok_or("client stdout missing")?;
    let reader = thread::spawn(move || {
        let mut reader = BufReader::new(output);
        let mut session = None;
        loop {
            let mut bytes = Vec::new();
            let length = Read::take(&mut reader, 1024 * 1024 + 1)
                .read_until(b'\n', &mut bytes)
                .unwrap_or(0);
            if length == 0 {
                break;
            }
            if length > 1024 * 1024 {
                continue;
            }
            if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                let id = value["sessionID"]
                    .as_str()
                    .or_else(|| value["part"]["sessionID"].as_str());
                if let Some(id) = id.filter(|id| {
                    id.len() < 100
                        && id.starts_with("ses")
                        && id.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
                }) {
                    session = Some(id.to_owned());
                }
            }
        }
        session
    });
    let began = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            // Clean up MCP children a client may leave behind, including after successful exit.
            terminate_tree(child.id());
            return Ok((
                status.success(),
                began.elapsed().as_secs_f64(),
                reader.join().map_err(|_| "client reader failed")?,
            ));
        }
        if began.elapsed() >= timeout {
            terminate_tree(child.id());
            let _ignored = child.kill();
            let _ignored = child.wait();
            let _ignored = reader.join();
            return Err("native client timed out; output omitted".into());
        }
        thread::sleep(Duration::from_millis(20));
    }
}
fn terminate_tree(pid: u32) {
    if cfg!(unix) {
        let _ignored = Command::new("kill")
            .args(["-KILL", "--", &format!("-{pid}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    if cfg!(windows) {
        let _ignored = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

pub fn mcp_config(
    executable: &Path,
    binary: &Path,
    audit: &Path,
    env: &[(&str, &str)],
    omit: bool,
    catalog: Option<&Path>,
) -> Value {
    let mut args = vec![
        "audit-proxy".to_owned(),
        "--binary".into(),
        binary.display().to_string(),
        "--audit".into(),
        audit.display().to_string(),
    ];
    if omit {
        args.push("--omit-cache".into());
    }
    if let Some(catalog) = catalog {
        args.extend(["--catalog".into(), catalog.display().to_string()]);
    }
    json!({"mcpServers":{"micu":{"command":executable,"args":args,"env":env.iter().map(|(name,value)|(name.to_string(),json!(value))).collect::<serde_json::Map<String,Value>>()}}})
}

pub fn catalog_probe(cli: &Path, binary: &Path, output: &Path) -> Result<Value> {
    if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir(output)?;
    let identity = client_identity(cli)?;
    let names: Vec<_> = TOOLS
        .iter()
        .map(|name| format!("mcp__micu__{name}"))
        .collect();
    let mut cases = Vec::new();
    for omit in [true, false] {
        let root = tempfile::tempdir()?;
        let audit = root.path().join("rpc.jsonl");
        let records = Arc::new(Mutex::new(Vec::<Value>::new()));
        let observed = records.clone();
        let fixture = HttpFixture::start(move |mut request| {
            let value: Value = serde_json::from_slice(&body(&mut request)?)?;
            let tools: Vec<_> = value["tools"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|item| item["name"].as_str())
                .map(str::to_owned)
                .collect();
            let results: Vec<_> = value["messages"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|message| message["content"].as_array().into_iter().flatten())
                .filter(|item| item["type"] == "tool_result")
                .collect();
            let info_ok = results.iter().any(|item| {
                item["is_error"] != true && item.to_string().contains("available_models")
            });
            observed
                .lock()
                .map_err(|_| "model fixture lock poisoned")?
                .push(json!({"tools":tools,"info_ok":info_ok}));
            let call =
                tools.iter().any(|name| name == "mcp__micu__server_info") && results.is_empty();
            let content = if call {
                json!({"type":"tool_use","id":"toolu_local_info","name":"mcp__micu__server_info","input":{}})
            } else {
                json!({"type":"text","text":"LOCAL_FIXTURE_DONE"})
            };
            let reason = if call { "tool_use" } else { "end_turn" };
            let message = json!({"id":"msg_fixture","type":"message","role":"assistant","model":value["model"],"content":[content],"stop_reason":reason,"stop_sequence":null,"usage":{"input_tokens":1,"output_tokens":1}});
            if value["stream"] != true {
                return respond(
                    request,
                    200,
                    serde_json::to_vec(&message)?,
                    "application/json",
                    &[],
                );
            }
            let mut start = content.clone();
            start[if call { "input" } else { "text" }] = if call { json!({}) } else { json!("") };
            let delta = if call {
                json!({"type":"input_json_delta","partial_json":"{}"})
            } else {
                json!({"type":"text_delta","text":"LOCAL_FIXTURE_DONE"})
            };
            let mut initial = message.clone();
            initial["content"] = json!([]);
            initial["stop_reason"] = Value::Null;
            let events = [
                (
                    "message_start",
                    json!({"type":"message_start","message":initial}),
                ),
                (
                    "content_block_start",
                    json!({"type":"content_block_start","index":0,"content_block":start}),
                ),
                (
                    "content_block_delta",
                    json!({"type":"content_block_delta","index":0,"delta":delta}),
                ),
                (
                    "content_block_stop",
                    json!({"type":"content_block_stop","index":0}),
                ),
                (
                    "message_delta",
                    json!({"type":"message_delta","delta":{"stop_reason":reason,"stop_sequence":null},"usage":{"output_tokens":1}}),
                ),
                ("message_stop", json!({"type":"message_stop"})),
            ];
            let raw = events
                .iter()
                .map(|(name, value)| format!("event: {name}\ndata: {value}\n\n"))
                .collect::<String>();
            respond(request, 200, raw.into_bytes(), "text/event-stream", &[])
        })?;
        let config = root.path().join("mcp.json");
        let root_text = root.path().to_str().ok_or("invalid fixture path")?;
        let env = [
            ("MICU_API_KEY", "offline-client-key"),
            ("MICU_BASEURL", "http://127.0.0.1:9"),
            ("MICU_SAVE_DIR", root_text),
            ("MICU_SAVE_DIR_ROOT", root_text),
            ("MICU_UPDATE_CHECK", "0"),
        ];
        write_report(
            &config,
            &mcp_config(&std::env::current_exe()?, binary, &audit, &env, omit, None),
        )?;
        let mut command = isolated_command(cli, root.path());
        command
            .env("ANTHROPIC_API_KEY", "sk-local-fixture-only")
            .env("ANTHROPIC_BASE_URL", &fixture.url)
            .env("CLAUDE_CONFIG_DIR", root.path().join("claude"))
            .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
            .env("DISABLE_AUTOUPDATER", "1")
            .env("ENABLE_TOOL_SEARCH", "false")
            .args([
                "-p",
                "Use server_info once then reply OK.",
                "--bare",
                "--model",
                "claude-sonnet-4-6",
                "--output-format",
                "json",
                "--strict-mcp-config",
                "--mcp-config",
            ])
            .arg(&config)
            .args([
                "--setting-sources",
                "",
                "--tools",
                &names.join(","),
                "--allowedTools",
                &names.join(","),
                "--permission-mode",
                "dontAsk",
            ]);
        let argv = command
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let (success, seconds) = run_client(command, Duration::from_secs(45))?;
        let rpc = read_audit(&audit)?;
        let modern = rpc.iter().any(|item| item["method"] == "server/discover");
        let observed = records.lock().map_err(|_| "model fixture lock poisoned")?;
        let registered: std::collections::BTreeSet<_> = observed
            .iter()
            .flat_map(|item| item["tools"].as_array().into_iter().flatten())
            .filter_map(Value::as_str)
            .filter(|name| name.starts_with("mcp__micu__"))
            .collect();
        let info = observed.iter().any(|item| item["info_ok"] == true);
        let expected: std::collections::BTreeSet<_> = names.iter().map(String::as_str).collect();
        let calls = rpc
            .iter()
            .filter(|item| item["method"] == "tools/call")
            .count();
        if omit && modern {
            if !registered.is_empty() || info {
                return Err(
                    "negative catalog control did not reproduce registration failure".into(),
                );
            }
        } else if !success || registered != expected || !info || calls != 1 {
            return Err("native client did not register and call the five-tool catalog".into());
        }
        if !omit
            && rpc
                .iter()
                .filter(|item| item["method"] == "tools/list")
                .count()
                != 1
        {
            return Err("fixed catalog was fetched more than once".into());
        }
        cases.push(json!({"omit_cache":omit,"argv":argv,"client_success":success,"elapsed_seconds":seconds,"modern_protocol":modern,"registered_tools":registered,"server_info_success":info,"rpc":rpc}));
    }
    let report =
        json!({"provenance":provenance(binary),"client":identity,"paid_requests":0,"cases":cases});
    write_report(&output.join("report.json"), &report)?;
    Ok(report)
}

pub fn read_audit(path: &Path) -> Result<Vec<Value>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = fs::read(path)?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err("audit exceeds limit".into());
    }
    bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| Ok(serde_json::from_slice(line)?))
        .collect()
}

pub fn credentials() -> Result<Value> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 64 * 1024 {
        return Err("credential input exceeds limit".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "invalid credential JSON on stdin".into())
}

pub fn gated(name: &str) -> Result<()> {
    if std::env::var(name).as_deref() != Ok("1") {
        return Err(format!("requires explicit {name}=1").into());
    }
    Ok(())
}

pub fn redact(mut value: Value, secrets: &[&str]) -> Value {
    fn visit(value: &mut Value, secrets: &[&str]) {
        match value {
            Value::String(text) => {
                for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
                    *text = text.replace(secret, "<REDACTED>");
                }
            }
            Value::Array(values) => {
                for item in values {
                    visit(item, secrets);
                }
            }
            Value::Object(object) => {
                for item in object.values_mut() {
                    visit(item, secrets);
                }
            }
            _ => {}
        }
    }
    visit(&mut value, secrets);
    value
}

fn client_identity(cli: &Path) -> Result<Value> {
    let root = tempfile::tempdir()?;
    let mut command = isolated_command(cli, root.path());
    let mut child = command
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or("client version stdout unavailable")?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ignored = stdout.take(4097).read_to_end(&mut bytes);
        bytes
    });
    let began = Instant::now();
    let success = loop {
        if let Some(status) = child.try_wait()? {
            break status.success();
        }
        if began.elapsed() > Duration::from_secs(5) {
            let _ignored = child.kill();
            let _ignored = child.wait();
            break false;
        }
        thread::sleep(Duration::from_millis(20));
    };
    let bytes = reader.join().map_err(|_| "client version reader failed")?;
    if !success || bytes.len() > 4096 {
        return Err("could not obtain a bounded native client version".into());
    }
    Ok(
        json!({"executable":provenance(cli),"version":String::from_utf8(bytes).map_err(|_|"invalid native client version")?.trim()}),
    )
}
