use crate::Result;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::Instant,
};

pub fn tool_summary(value: &Value) -> Value {
    // A whitelist prevents arbitrary error messages, prompt text and credentials entering reports.
    let session = &value["session"];
    let saved = |item: &Value| json!({"path":item["path"],"size_bytes":item["size_bytes"]});
    json!({"ok":value["ok"],"session":{"id":session["id"],"revision":session["revision"],"parent_revision":session["parent_revision"],
        "history_length":session["history_length"],"input_path":session["input_path"],"context":session["context"]},
        "saved":if let Some(items)=value["saved"].as_array() {json!(items.iter().map(saved).collect::<Vec<_>>())} else {saved(&value["saved"])},"available_models":value["available_models"]})
}

pub fn relay(binary: &Path, audit: &Path, omit_cache: bool, catalog: Option<&Path>) -> Result<()> {
    let override_tools = catalog
        .map(|path| -> Result<Value> {
            Ok(serde_json::from_slice::<Value>(&fs::read(path)?)?["result"]["tools"].clone())
        })
        .transpose()?;
    if let Some(parent) = audit.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut options = fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let log = Arc::new(Mutex::new(options.open(audit)?));
    let mut child = Command::new(binary)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut input = child.stdin.take().ok_or("child stdin missing")?;
    let output = child.stdout.take().ok_or("child stdout missing")?;
    let pending = Arc::new(Mutex::new(HashMap::<String, (Instant, Value)>::new()));
    let sent = pending.clone();
    thread::spawn(move || {
        let mut reader = BufReader::new(std::io::stdin());
        loop {
            let mut bytes = Vec::new();
            if Read::take(&mut reader, 1024 * 1024 + 1)
                .read_until(b'\n', &mut bytes)
                .ok()
                .filter(|length| *length > 0 && *length <= 1024 * 1024)
                .is_none()
            {
                break;
            }
            let Ok(frame) = serde_json::from_slice::<Value>(&bytes) else {
                break;
            };
            if !frame["id"].is_null() && frame["method"].is_string() {
                let Ok(mut pending) = sent.lock() else {
                    break;
                };
                if pending.len() >= 1024 {
                    break;
                }
                pending.insert(frame["id"].to_string(), (Instant::now(), frame));
            }
            if input
                .write_all(&bytes)
                .and_then(|()| input.flush())
                .is_err()
            {
                break;
            }
        }
    });
    let outcome = (|| -> Result<()> {
        let mut reader = BufReader::new(output);
        let stdout = std::io::stdout();
        let mut stdout = stdout.lock();
        loop {
            let mut bytes = Vec::new();
            let length = Read::take(&mut reader, 1024 * 1024 + 1).read_until(b'\n', &mut bytes)?;
            if length == 0 {
                break;
            }
            if length > 1024 * 1024 {
                return Err("oversized MCP audit frame".into());
            }
            let mut frame: Value =
                serde_json::from_slice(&bytes).map_err(|_| "invalid JSON-RPC audit frame")?;
            let request = pending
                .lock()
                .map_err(|_| "audit lock poisoned")?
                .remove(&frame["id"].to_string());
            if let Some((started, request)) = request {
                let method = request["method"].as_str().ok_or("missing audit method")?;
                if omit_cache
                    && matches!(
                        method,
                        "tools/list"
                            | "prompts/list"
                            | "resources/list"
                            | "resources/templates/list"
                    )
                    && let Some(result) = frame["result"].as_object_mut()
                {
                    result.remove("ttlMs");
                    result.remove("cacheScope");
                }
                if method == "tools/list"
                    && let Some(tools) = &override_tools
                {
                    let actual = frame["result"]["tools"]
                        .as_array()
                        .ok_or("tool catalog missing")?;
                    let overrides = tools.as_array().ok_or("invalid catalog override")?;
                    if actual.len() != overrides.len()
                        || actual.iter().any(|tool| {
                            !overrides.iter().any(|item| {
                                item["name"] == tool["name"]
                                    && item["inputSchema"] == tool["inputSchema"]
                            })
                        })
                    {
                        return Err(
                            "catalog override changed tool identities or argument schemas".into(),
                        );
                    }
                    frame["result"]["tools"] = tools.clone();
                }
                let mut event = json!({"kind":"response","method":method,"elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
                    "rpc_error":frame.get("error").is_some(),"result_keys":frame["result"].as_object().map(|object|object.keys().collect::<Vec<_>>())});
                if method == "tools/list" {
                    event["tool_count"] = json!(frame["result"]["tools"].as_array().map(Vec::len));
                    event["ttlMs"] = frame["result"]["ttlMs"].clone();
                    event["cacheScope"] = frame["result"]["cacheScope"].clone();
                }
                if method == "tools/call" {
                    event["tool"] = request["params"]["name"].clone();
                    let args = &request["params"]["arguments"];
                    event["arguments"] = json!({"session_id":args["session_id"],"parent_revision":args["parent_revision"],"image_path":args["image_path"]});
                    let value = crate::stdio::tool_value(&frame).unwrap_or(Value::Null);
                    event["result"] = tool_summary(&value);
                }
                let secret = std::env::var("MICU_API_KEY").unwrap_or_default();
                let event = crate::clients::redact(event, &[&secret]);
                let mut log = log.lock().map_err(|_| "audit log lock poisoned")?;
                serde_json::to_writer(&mut *log, &event)?;
                log.write_all(b"\n")?;
                log.flush()?;
            }
            serde_json::to_writer(&mut stdout, &frame)?;
            stdout.write_all(b"\n")?;
            stdout.flush()?;
        }
        Ok(())
    })();
    let _ignored = child.kill();
    let _ignored = child.wait();
    outcome
}
