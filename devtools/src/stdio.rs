use crate::Result;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const FRAME_LIMIT: u64 = 1024 * 1024;
const STDERR_LIMIT: usize = 1024 * 1024;

/// Never inherit credentials, user configuration, proxies or live-test switches.
pub fn isolated_command(binary: &Path, root: &Path) -> Command {
    let mut command = Command::new(binary);
    command.env_clear();
    for name in [
        "PATH",
        "SystemRoot",
        "WINDIR",
        "PATHEXT",
        "TMP",
        "TEMP",
        "TMPDIR",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .current_dir(root)
        .env("HOME", root)
        .env("USERPROFILE", root)
        .env("LOCALAPPDATA", root.join("data"))
        .env("APPDATA", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("MICU_SAVE_DIR", root)
        .env("MICU_SAVE_DIR_ROOT", root)
        .env("MICU_API_KEY", "")
        .env("MICU_KEYCHAIN_SERVICE", "micu-offline-test-no-entry")
        .env("MICU_KEYCHAIN_ACCOUNT", "no-entry")
        .env("MICU_BASEURL", "http://127.0.0.1:9")
        .env("MICU_USE_SHELL_PROXY", "0")
        .env("MICU_RUN_LIVE_TESTS", "0")
        .env("MICU_UPDATE_CHECK", "0");
    command
}

pub fn latest_meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name": "micu-rust-contract", "version": "1"},
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}

pub fn tool_value(response: &Value) -> Result<Value> {
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .ok_or("MCP response has no text content")?;
    Ok(serde_json::from_str(text)?)
}

pub struct Session {
    child: Child,
    receiver: mpsc::Receiver<Result<Value>>,
    pending: HashMap<u64, Value>,
    next_id: u64,
    timeout: Duration,
    stderr: Arc<Mutex<Vec<u8>>>,
    readers: Vec<JoinHandle<()>>,
    pub frames: usize,
}

impl Session {
    pub fn new(binary: &Path, root: &Path, env: &[(&str, &str)]) -> Result<Self> {
        let mut command = isolated_command(binary, root);
        command.envs(env.iter().copied());
        Self::spawn(command)
    }

    pub fn spawn(mut command: Command) -> Result<Self> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = child.stdout.take().ok_or("child stdout missing")?;
        let stderr_pipe = child.stderr.take().ok_or("child stderr missing")?;
        let (sender, receiver) = mpsc::channel();
        let stdout_thread = thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                match Read::take(&mut reader, FRAME_LIMIT + 1).read_until(b'\n', &mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        let frame: Result<Value> = (|| {
                            if line.len() as u64 > FRAME_LIMIT || !line.ends_with(b"\n") {
                                return Err("oversized or incomplete MCP stdout frame".into());
                            }
                            let value: Value = serde_json::from_slice(&line)
                                .map_err(|_| "stdout is not a JSON-RPC frame")?;
                            if value["jsonrpc"] != "2.0" || !value.is_object() {
                                return Err("stdout is not JSON-RPC 2.0".into());
                            }
                            Ok(value)
                        })();
                        let failed = frame.is_err();
                        if sender.send(frame).is_err() || failed {
                            break;
                        }
                    }
                    Err(_) => {
                        let _ignored = sender.send(Err("reading MCP stdout failed".into()));
                        break;
                    }
                }
            }
        });
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let captured = stderr.clone();
        let stderr_thread = thread::spawn(move || {
            let mut reader = stderr_pipe;
            let mut bytes = [0; 8192];
            while let Ok(length) = reader.read(&mut bytes) {
                if length == 0 {
                    break;
                }
                if let Ok(mut destination) = captured.lock() {
                    let remaining = STDERR_LIMIT.saturating_sub(destination.len());
                    destination.extend_from_slice(&bytes[..length.min(remaining)]);
                }
            }
        });
        Ok(Self {
            child,
            receiver,
            pending: HashMap::new(),
            next_id: 1,
            timeout: Duration::from_secs(30),
            stderr,
            readers: vec![stdout_thread, stderr_thread],
            frames: 0,
        })
    }

    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn stderr_bytes(&self) -> Vec<u8> {
        self.stderr
            .lock()
            .map(|bytes| bytes.clone())
            .unwrap_or_default()
    }

    pub fn send(&mut self, method: &str, params: Value) -> Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        self.write(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        Ok(id)
    }

    pub fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.write(json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    fn write(&mut self, value: Value) -> Result<()> {
        let stdin = self.child.stdin.as_mut().ok_or("child stdin missing")?;
        serde_json::to_writer(&mut *stdin, &value)?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;
        Ok(())
    }

    pub fn initialize(&mut self) -> Result<Value> {
        let response = self.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05", "capabilities": {},
                "clientInfo": {"name": "micu-rust-contract", "version": "1"}
            }),
        )?;
        self.notify("notifications/initialized", json!({}))?;
        Ok(response)
    }

    pub fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.send(method, params)?;
        self.wait(id)
    }

    pub fn wait(&mut self, id: u64) -> Result<Value> {
        if let Some(value) = self.pending.remove(&id) {
            return Ok(value);
        }
        let deadline = Instant::now() + self.timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let message = self
                .receiver
                .recv_timeout(remaining)
                .map_err(|_| "MCP exited or timed out before the requested response")??;
            self.frames += 1;
            if let Some(received) = message["id"].as_u64() {
                if received == id {
                    return Ok(message);
                }
                if received >= self.next_id || self.pending.len() >= 1024 {
                    return Err("unsolicited or excessive MCP responses".into());
                }
                self.pending.insert(received, message);
            } else if !message["method"].is_string() {
                return Err("MCP frame is neither a response nor a notification".into());
            }
        }
    }

    pub fn call(&mut self, tool: &str, arguments: Value) -> Result<Value> {
        tool_value(&self.request("tools/call", json!({"name": tool, "arguments": arguments}))?)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let _ignored = self.child.kill();
        let _ignored = self.child.wait();
        for reader in self.readers.drain(..) {
            let _ignored = reader.join();
        }
    }
}
