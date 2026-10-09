use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};

struct Session {
    child: Child,
    reader: BufReader<std::process::ChildStdout>,
}

impl Session {
    fn new(root: &Path, enabled: bool) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_micu-image-mcp"))
            .env("HOME", root)
            .env("USERPROFILE", root)
            .env("LOCALAPPDATA", root)
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("MICU_UPDATE_CACHE_DIR", root.join("cache"))
            .env("MICU_API_KEY", "offline-fixture")
            .env("MICU_SAVE_DIR_ROOT", root)
            .env("MICU_SAVE_DIR", root)
            .env("MICU_UPDATE_CHECK", if enabled { "1" } else { "0" })
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let reader = BufReader::new(child.stdout.take().unwrap());
        Self { child, reader }
    }

    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        writeln!(
            self.child.stdin.as_mut().unwrap(),
            "{}",
            json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params})
        )
        .unwrap();
        let mut line = String::new();
        self.reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    fn initialize(&mut self) -> Value {
        let result = self.request(
            1,
            "initialize",
            json!({
                "protocolVersion":"2024-11-05", "capabilities":{},
                "clientInfo":{"name":"offline-update-test", "version":"1"}
            }),
        );
        writeln!(
            self.child.stdin.as_mut().unwrap(),
            "{}",
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .unwrap();
        result
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ignored = self.child.kill();
        let _ignored = self.child.wait();
    }
}

fn cache_file(root: &Path) -> PathBuf {
    root.join("cache/micu-image-mcp/update.json")
}

#[test]
fn server_info_delivers_one_upgrade_hint_and_keeps_the_five_tools() {
    let temp = tempfile::tempdir().unwrap();
    let cache = cache_file(temp.path());
    fs::create_dir_all(cache.parent().unwrap()).unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    fs::write(
        &cache,
        json!({"checked_at":now,"latest":"0.99.0"}).to_string(),
    )
    .unwrap();
    let mut server = Session::new(temp.path(), true);
    let init = server.initialize();
    assert!(
        init["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("server_info")
    );
    let tools = server.request(2, "tools/list", json!({}));
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 5);
    let first = server.request(
        3,
        "tools/call",
        json!({"name":"server_info","arguments":{}}),
    );
    let update = &first["result"]["structuredContent"]["update"];
    assert_eq!(update["status"], "available");
    assert!(update["hint"].as_str().unwrap().contains("0.99.0"));
    let text: Value =
        serde_json::from_str(first["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text["update"], *update);
    let second = server.request(
        4,
        "tools/call",
        json!({"name":"server_info","arguments":{}}),
    );
    assert_eq!(
        second["result"]["structuredContent"]["update"]["status"],
        "available"
    );
    assert!(
        second["result"]["structuredContent"]["update"]
            .get("hint")
            .is_none()
    );
}

#[test]
fn disabled_checks_return_immediately_without_a_cache_or_hint() {
    let temp = tempfile::tempdir().unwrap();
    let mut server = Session::new(temp.path(), false);
    server.initialize();
    let info = server.request(
        2,
        "tools/call",
        json!({"name":"server_info","arguments":{}}),
    );
    assert_eq!(
        info["result"]["structuredContent"]["update"]["status"],
        "disabled"
    );
    assert!(
        info["result"]["structuredContent"]["update"]
            .get("hint")
            .is_none()
    );
    assert!(!cache_file(temp.path()).exists());
}

#[test]
fn an_image_response_carries_the_hint_if_server_info_has_not_delivered_it() {
    let temp = tempfile::tempdir().unwrap();
    let cache = cache_file(temp.path());
    fs::create_dir_all(cache.parent().unwrap()).unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    fs::write(
        cache,
        json!({"checked_at":now,"latest":"0.99.0"}).to_string(),
    )
    .unwrap();
    let mut server = Session::new(temp.path(), true);
    server.initialize();
    // Missing required prompt fails before any image I/O or provider request.
    let failed = server.request(
        2,
        "tools/call",
        json!({"name":"image_generate","arguments":{}}),
    );
    assert_eq!(failed["result"]["isError"], true);
    assert!(
        failed["result"]["content"][1]["text"]
            .as_str()
            .unwrap()
            .contains("0.99.0")
    );
    let info = server.request(
        3,
        "tools/call",
        json!({"name":"server_info","arguments":{}}),
    );
    assert_eq!(
        info["result"]["structuredContent"]["update"]["status"],
        "available"
    );
    assert!(
        info["result"]["structuredContent"]["update"]
            .get("hint")
            .is_none()
    );
}

#[cfg(unix)]
#[test]
fn bootstrap_script_updates_each_posix_platform_and_preserves_user_files() {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let mocks = root.join("commands");
    fs::create_dir(&mocks).unwrap();
    let candidate = if let Some(path) = std::env::var_os("MICU_UPDATE_FIXTURE_BINARY") {
        fs::read(path).unwrap()
    } else {
        format!("#!/bin/sh\nprintf '{}\\n'\n", env!("CARGO_PKG_VERSION")).into_bytes()
    };
    fs::write(root.join("candidate"), &candidate).unwrap();
    let digest: String = Sha256::digest(&candidate)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    fs::write(
        mocks.join("uname"),
        "#!/bin/sh\ncase \"$1\" in -s) echo \"$FIXTURE_OS\";; -m) echo \"$FIXTURE_ARCH\";; esac\n",
    )
    .unwrap();
    fs::write(mocks.join("curl"), r#"#!/bin/sh
out=""
while [ "$#" -gt 0 ]; do
    case "$1" in -o) out="$2"; shift 2;; *) url="$1"; shift;; esac
done
case "$url" in
    */releases/latest) printf 'https://github.com/Subaru486desuwa/micu-image-mcp/releases/tag/v%s' "$FIXTURE_VERSION";;
    */v"$FIXTURE_VERSION"/SHA256SUMS) cp "$FIXTURE_ROOT/sums" "$out";;
    */v"$FIXTURE_VERSION"/"$FIXTURE_ASSET") cp "$FIXTURE_ROOT/candidate" "$out";;
    *) exit 99;;
esac
"#).unwrap();
    for name in ["curl", "uname"] {
        fs::set_permissions(mocks.join(name), fs::Permissions::from_mode(0o700)).unwrap();
    }
    let target = root.join("installed 程序's binary");
    fs::write(root.join("client-config"), b"custom output root").unwrap();
    fs::write(root.join("session"), b"prior image history").unwrap();
    let run = |os: &str, arch: &str, asset: &str| {
        Command::new("sh")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/update.sh"))
            .args(["--binary-path", target.to_str().unwrap()])
            .env("FIXTURE_ROOT", root)
            .env("FIXTURE_OS", os)
            .env("FIXTURE_ARCH", arch)
            .env("FIXTURE_ASSET", asset)
            .env("FIXTURE_VERSION", env!("CARGO_PKG_VERSION"))
            .env(
                "PATH",
                format!("{}:{}", mocks.display(), std::env::var("PATH").unwrap()),
            )
            .output()
            .unwrap()
    };
    for (os, arch, asset) in [
        ("Linux", "x86_64", "micu-image-mcp-linux-x86_64"),
        ("Darwin", "arm64", "micu-image-mcp-macos-arm64"),
        ("Darwin", "x86_64", "micu-image-mcp-macos-x86_64"),
    ] {
        fs::write(&target, b"old binary").unwrap();
        fs::write(root.join("candidate"), &candidate).unwrap();
        fs::write(root.join("sums"), format!("{digest}  {asset}\n")).unwrap();
        let good = run(os, arch, asset);
        assert!(
            good.status.success(),
            "{}",
            String::from_utf8_lossy(&good.stderr)
        );
        assert_eq!(fs::read(&target).unwrap(), candidate);
        assert_eq!(
            fs::read(root.join("installed 程序's binary.previous")).unwrap(),
            b"old binary"
        );
        assert_eq!(
            fs::read(root.join("client-config")).unwrap(),
            b"custom output root"
        );
        assert_eq!(
            fs::read(root.join("session")).unwrap(),
            b"prior image history"
        );
        fs::write(root.join("candidate"), b"corrupted download").unwrap();
        let bad = run(os, arch, asset);
        assert!(!bad.status.success());
        assert_eq!(fs::read(&target).unwrap(), candidate);
        assert!(!root.join("installed 程序's binary.update-lock").exists());
    }
}
