//! Public release checks and binary-only updates. This client never loads Micu credentials.
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use reqwest::Client;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::installer::binary::install_binary;

const REPO: &str = "Subaru486desuwa/micu-image-mcp";
const LATEST_API: &str =
    "https://api.github.com/repos/Subaru486desuwa/micu-image-mcp/releases/latest";
const CACHE_SECONDS: u64 = 24 * 60 * 60;
const METADATA_LIMIT: usize = 1024 * 1024;
const BINARY_LIMIT: usize = 128 * 1024 * 1024;
type UpdateResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone, Deserialize, Serialize)]
struct Cache {
    checked_at: u64,
    latest: Option<String>,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
}

#[derive(Clone)]
pub struct UpdateChecker {
    cache_file: PathBuf,
    executable: PathBuf,
    enabled: bool,
    cache: Arc<Mutex<Option<Cache>>>,
    running: Arc<AtomicBool>,
    reminded: Arc<AtomicBool>,
}

impl UpdateChecker {
    pub fn new(cache_file: PathBuf, executable: PathBuf, enabled: bool) -> Self {
        let cache = read_cache(&cache_file);
        Self {
            cache_file,
            executable,
            enabled,
            cache: Arc::new(Mutex::new(cache)),
            running: Arc::new(AtomicBool::new(false)),
            reminded: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn system() -> UpdateResult<Self> {
        let cache_root = std::env::var_os("MICU_UPDATE_CACHE_DIR")
            .map(PathBuf::from)
            .or_else(dirs::cache_dir)
            .ok_or("无法确定更新缓存目录")?;
        let enabled = !std::env::var("MICU_UPDATE_CHECK").is_ok_and(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off" | "no"
            )
        });
        Ok(Self::new(
            cache_root.join("micu-image-mcp/update.json"),
            std::env::current_exe()?,
            enabled,
        ))
    }

    fn fresh_cache(&self) -> Option<Cache> {
        self.cache.lock().ok()?.clone().filter(|cache| {
            timestamp()
                .checked_sub(cache.checked_at)
                .is_some_and(|age| age < CACHE_SECONDS)
        })
    }

    pub fn needs_check(&self) -> bool {
        self.enabled && self.fresh_cache().is_none()
    }

    /// Return immediately; only a private public-GitHub client does network I/O.
    pub fn start(&self) {
        self.start_request(LATEST_API.to_owned());
    }

    fn start_request(&self, url: String) {
        if !self.needs_check() || self.running.swap(true, Ordering::AcqRel) {
            return;
        }
        let checker = self.clone();
        tokio::spawn(async move {
            let _ignored = checker.check_request(true, &url).await;
            checker.running.store(false, Ordering::Release);
        });
    }

    pub async fn check(&self, force: bool) -> UpdateResult<Value> {
        self.check_request(force, LATEST_API).await
    }

    async fn check_request(&self, force: bool, url: &str) -> UpdateResult<Value> {
        // Explicit --force also permits a one-off check when automatic checks are disabled.
        if !force && !self.needs_check() {
            return Ok(self.info());
        }
        let result = async {
            let client = public_client(Duration::from_secs(3))?;
            fetch_latest(&client, url).await
        }
        .await;
        let cache = Cache {
            checked_at: timestamp(),
            latest: result.as_ref().ok().cloned(),
        };
        if let Ok(mut stored) = self.cache.lock() {
            *stored = Some(cache.clone());
        }
        // A read-only cache directory must not stop MCP usage or an explicit check.
        let _ignored = write_cache(&self.cache_file, &cache);
        result?;
        let mut explicit = self.clone();
        explicit.enabled = true;
        Ok(explicit.info())
    }

    pub fn info(&self) -> Value {
        let cache = self.fresh_cache();
        let latest = cache.as_ref().and_then(|cache| cache.latest.as_deref());
        let available = latest.is_some_and(|v| is_newer_stable(env!("CARGO_PKG_VERSION"), v));
        let status = if !self.enabled {
            "disabled"
        } else if cache.is_none() {
            "checking"
        } else if latest.is_none() {
            "unavailable"
        } else if available {
            "available"
        } else {
            "up_to_date"
        };
        let supported = platform_asset().is_some();
        json!({
            "status": status,
            "current_version": env!("CARGO_PKG_VERSION"),
            "latest_version": latest,
            "checked_at": cache.as_ref().map(|cache| cache.checked_at),
            "cache_seconds": CACHE_SECONDS,
            "release_url": latest.map(|v| format!("https://github.com/{REPO}/releases/tag/v{v}")),
            "update_command": if available && supported { Some(quote_command(&self.executable, cfg!(windows))) } else { None },
            "binary_update_supported": supported,
            "restart_required": available,
            "disable_hint": "设置 MCP 进程环境变量 MICU_UPDATE_CHECK=0 可关闭自动检查",
        })
    }

    pub fn take_hint(&self) -> Option<String> {
        let info = self.info();
        if info["status"] != "available" || self.reminded.swap(true, Ordering::AcqRel) {
            return None;
        }
        let latest = info["latest_version"].as_str()?;
        let action = if let Some(command) = info["update_command"].as_str() {
            format!(
                "可在终端手动执行：{command}。更新只替换程序并保留配置、凭据、图片和会话；完成后重新连接 MCP"
            )
        } else {
            "此平台未发布预编译包，请按 README 的源码安装流程升级，再重新连接 MCP".into()
        };
        Some(format!(
            "建议更新 Micu Image MCP：v{} → v{latest}。{action}。当前版本仍可继续使用。",
            env!("CARGO_PKG_VERSION")
        ))
    }
}

fn timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn stable_version(raw: &str) -> Option<Version> {
    let version = Version::parse(raw.strip_prefix('v').unwrap_or(raw)).ok()?;
    (version.pre.is_empty() && version.build.is_empty()).then_some(version)
}

pub fn is_newer_stable(current: &str, candidate: &str) -> bool {
    match (Version::parse(current), stable_version(candidate)) {
        (Ok(current), Some(candidate)) => candidate > current,
        _ => false,
    }
}

fn read_cache(path: &Path) -> Option<Cache> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(4097)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > 4096 {
        return None;
    }
    let cache: Cache = serde_json::from_slice(&bytes).ok()?;
    if cache
        .latest
        .as_ref()
        .is_some_and(|v| stable_version(v).is_none())
    {
        return None;
    }
    Some(cache)
}

fn write_cache(path: &Path, cache: &Cache) -> UpdateResult<()> {
    let parent = path.parent().ok_or("更新缓存路径缺少目录")?;
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(&serde_json::to_vec(cache)?)?;
    temp.as_file_mut().sync_all()?;
    temp.persist(path)?;
    Ok(())
}

fn public_client(timeout: Duration) -> UpdateResult<Client> {
    Ok(Client::builder()
        .user_agent(concat!("micu-image-mcp/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(2))
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let allowed = attempt.url().scheme() == "https"
                && matches!(
                    attempt.url().host_str(),
                    Some(
                        "github.com"
                            | "api.github.com"
                            | "release-assets.githubusercontent.com"
                            | "objects.githubusercontent.com"
                    )
                );
            if allowed && attempt.previous().len() < 5 {
                attempt.follow()
            } else {
                attempt.error("更新下载重定向不受信任或次数过多")
            }
        }))
        .build()?)
}

async fn fetch_bytes(client: &Client, url: &str, limit: usize) -> UpdateResult<Vec<u8>> {
    let mut response = client.get(url).send().await?.error_for_status()?;
    if response
        .content_length()
        .is_some_and(|size| size > limit as u64)
    {
        return Err("更新响应超过大小限制".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err("更新响应超过大小限制".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn fetch_latest(client: &Client, url: &str) -> UpdateResult<String> {
    let release: Release =
        serde_json::from_slice(&fetch_bytes(client, url, METADATA_LIMIT).await?)?;
    if release.draft || release.prerelease {
        return Err("最新 Release 不是正式版".into());
    }
    stable_version(&release.tag_name)
        .map(|v| v.to_string())
        .ok_or_else(|| "Release 版本号无效".into())
}

fn platform_asset() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("micu-image-mcp-macos-arm64"),
        ("macos", "x86_64") => Some("micu-image-mcp-macos-x86_64"),
        ("linux", "x86_64") => Some("micu-image-mcp-linux-x86_64"),
        ("windows", "x86_64") => Some("micu-image-mcp-windows-x86_64.exe"),
        _ => None,
    }
}

fn quote_command(path: &Path, windows: bool) -> String {
    let path = path.to_string_lossy();
    if windows {
        format!("& '{}' update", path.replace('\'', "''"))
    } else {
        format!("'{}' update", path.replace('\'', "'\\''"))
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn backup_path(target: &Path) -> PathBuf {
    let mut name = target.as_os_str().to_os_string();
    name.push(".previous");
    PathBuf::from(name)
}

fn replace_verified_binary(
    bytes: &[u8],
    checksums: &str,
    asset: &str,
    target: &Path,
) -> UpdateResult<()> {
    let hashes: Vec<_> = checksums
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let hash = fields.next()?;
            (fields.next()?.trim_start_matches('*') == asset).then_some(hash)
        })
        .collect();
    let actual = sha256_hex(bytes);
    if hashes.len() != 1 || !actual.eq_ignore_ascii_case(hashes[0]) {
        return Err("SHA-256 校验失败；保留现有程序".into());
    }
    if !target.is_file() || fs::symlink_metadata(target)?.file_type().is_symlink() {
        return Err("更新目标必须是已安装的普通文件；请先安装或指定 --binary-path".into());
    }
    let _lock = UpdateLock::acquire(target)?;
    let parent = target.parent().ok_or("更新目标缺少目录")?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(bytes)?;
    staged.as_file_mut().sync_all()?;
    // Only the old binary is backed up; no credential or client configuration is read.
    install_binary(target, &backup_path(target))?;
    #[cfg(windows)]
    if fs::canonicalize(target)? == fs::canonicalize(std::env::current_exe()?)? {
        self_replace::self_replace(staged.path())?;
        return Ok(());
    }
    install_binary(staged.path(), target)?;
    Ok(())
}

struct UpdateLock(PathBuf);

impl UpdateLock {
    fn acquire(target: &Path) -> UpdateResult<Self> {
        let mut name = target.as_os_str().to_os_string();
        name.push(".update-lock");
        let path = PathBuf::from(name);
        fs::create_dir(&path).map_err(|_| "另一个更新正在运行；请等待完成后重试")?;
        Ok(Self(path))
    }
}

impl Drop for UpdateLock {
    fn drop(&mut self) {
        let _ignored = fs::remove_dir(&self.0);
    }
}

/// Updating is an explicit CLI action; MCP tool inputs cannot select an update URL or path.
pub async fn update(target: Option<PathBuf>) -> UpdateResult<String> {
    let asset = platform_asset().ok_or("此平台未发布预编译包；请按 README 的源码安装流程升级")?;
    let target = target.unwrap_or(std::env::current_exe()?);
    if !target.is_file() {
        return Err("更新目标不存在；请先安装".into());
    }
    let client = public_client(Duration::from_secs(120))?;
    let version = fetch_latest(&public_client(Duration::from_secs(3))?, LATEST_API).await?;
    if Version::parse(&version)? < Version::parse(env!("CARGO_PKG_VERSION"))? {
        return Err("最新正式版早于当前程序；保留当前版本，不自动降级".into());
    }
    if !is_newer_stable(env!("CARGO_PKG_VERSION"), &version)
        && fs::canonicalize(&target)? == fs::canonicalize(std::env::current_exe()?)?
    {
        return Ok(format!(
            "当前 v{} 无需升级（最新正式版 v{version}）。",
            env!("CARGO_PKG_VERSION")
        ));
    }
    // Pin both downloads to the same tag, even if latest changes mid-update.
    let base = format!("https://github.com/{REPO}/releases/download/v{version}");
    let sums = String::from_utf8(
        fetch_bytes(&client, &format!("{base}/SHA256SUMS"), METADATA_LIMIT).await?,
    )?;
    let bytes = fetch_bytes(&client, &format!("{base}/{asset}"), BINARY_LIMIT).await?;
    replace_verified_binary(&bytes, &sums, asset, &target)?;
    Ok(format!(
        "已更新为 v{version}；配置、凭据、图片和会话已保留。旧程序备份：{}。请重新连接 MCP，再用 server_info 确认版本。",
        backup_path(&target).display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_newer_stable_versions_are_recommended() {
        assert!(is_newer_stable("0.4.1", "v0.4.2"));
        assert!(is_newer_stable("0.4.1", "v0.10.0"));
        assert!(!is_newer_stable("0.4.1", "v0.4.1"));
        assert!(!is_newer_stable("0.4.1", "v0.3.9"));
        assert!(!is_newer_stable("0.4.1", "v0.5.0-beta.1"));
        assert!(!is_newer_stable("0.4.1", "../../bad"));
    }

    #[test]
    fn cache_expiry_clock_rollback_and_disabled_checks() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("update.json");
        let now = timestamp();
        write_cache(
            &path,
            &Cache {
                checked_at: now,
                latest: Some("0.99.0".into()),
            },
        )
        .unwrap();
        let checker = UpdateChecker::new(path.clone(), temp.path().join("micu tool"), true);
        assert!(!checker.needs_check());
        assert_eq!(checker.info()["status"], "available");
        assert!(checker.take_hint().is_some());
        assert!(checker.take_hint().is_none());
        let disabled = UpdateChecker::new(path.clone(), temp.path().join("micu"), false);
        assert!(!disabled.needs_check());
        assert_eq!(disabled.info()["status"], "disabled");
        for checked_at in [now.saturating_sub(CACHE_SECONDS + 1), now + 600] {
            write_cache(
                &path,
                &Cache {
                    checked_at,
                    latest: Some("0.99.0".into()),
                },
            )
            .unwrap();
            let checker = UpdateChecker::new(path.clone(), temp.path().join("micu"), true);
            assert!(checker.needs_check());
            assert_eq!(checker.info()["status"], "checking");
        }
        fs::write(&path, b"broken cache").unwrap();
        assert!(UpdateChecker::new(path, temp.path().join("micu"), true).needs_check());
    }

    #[test]
    fn failed_check_is_cached_without_blocking_use() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("update.json");
        write_cache(
            &path,
            &Cache {
                checked_at: timestamp(),
                latest: None,
            },
        )
        .unwrap();
        let checker = UpdateChecker::new(path, temp.path().join("micu"), true);
        assert!(!checker.needs_check());
        assert_eq!(checker.info()["status"], "unavailable");
        assert!(checker.take_hint().is_none());
    }

    #[test]
    fn checksum_failure_keeps_binary_config_and_sessions_unchanged() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("micu");
        let config = temp.path().join("client.json");
        let session = temp.path().join("session.json");
        fs::write(&target, b"old binary").unwrap();
        fs::write(&config, b"custom configuration").unwrap();
        fs::write(&session, b"image session").unwrap();
        let asset = "micu-image-mcp-linux-x86_64";
        let good = format!("{}  {asset}\n", sha256_hex(b"new binary"));
        assert!(replace_verified_binary(b"tampered binary", &good, asset, &target).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"old binary");
        replace_verified_binary(b"new binary", &good, asset, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new binary");
        assert_eq!(fs::read(backup_path(&target)).unwrap(), b"old binary");
        assert_eq!(fs::read(config).unwrap(), b"custom configuration");
        assert_eq!(fs::read(session).unwrap(), b"image session");
    }

    #[test]
    fn shell_commands_quote_spaces_and_metacharacters() {
        assert_eq!(
            quote_command(Path::new("/a b/it's $x"), false),
            "'/a b/it'\\''s $x' update"
        );
        assert_eq!(
            quote_command(Path::new("C:\\a b\\it's.exe"), true),
            "& 'C:\\a b\\it''s.exe' update"
        );
    }

    #[tokio::test]
    async fn slow_release_check_is_asynchronous_and_never_sends_authorization() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/latest", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = [0; 4096];
            let size = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..size]).to_ascii_lowercase();
            assert!(!request.contains("authorization:"));
            assert!(!request.contains("api-key"));
            std::thread::sleep(Duration::from_millis(150));
            let body = r#"{"tag_name":"v0.99.0","draft":false,"prerelease":false}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let temp = tempfile::tempdir().unwrap();
        let checker = UpdateChecker::new(
            temp.path().join("update.json"),
            temp.path().join("micu"),
            true,
        );
        checker.start_request(url);
        assert_eq!(checker.info()["status"], "checking");
        tokio::time::timeout(Duration::from_secs(5), async {
            while checker.info()["status"] == "checking" {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(checker.info()["status"], "available");
        server.join().unwrap();
    }

    #[tokio::test]
    async fn failed_request_and_oversized_response_leave_usage_available() {
        let temp = tempfile::tempdir().unwrap();
        let checker = UpdateChecker::new(
            temp.path().join("update.json"),
            temp.path().join("micu"),
            true,
        );
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/latest", listener.local_addr().unwrap());
        drop(listener);
        assert!(checker.check_request(true, &url).await.is_err());
        assert_eq!(checker.info()["status"], "unavailable");
        assert!(!checker.needs_check());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/latest", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let size = stream.read(&mut request).unwrap();
            assert!(size > 0);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                METADATA_LIMIT + 1
            )
            .unwrap();
        });
        assert!(checker.check_request(true, &url).await.is_err());
        assert_eq!(checker.info()["status"], "unavailable");
        server.join().unwrap();
    }

    #[test]
    fn concurrent_update_and_ambiguous_checksum_preserve_the_installed_binary() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("micu");
        fs::write(&target, b"old").unwrap();
        let sums = format!("{}  asset\n", sha256_hex(b"new"));
        let lock = UpdateLock::acquire(&target).unwrap();
        assert!(replace_verified_binary(b"new", &sums, "asset", &target).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"old");
        drop(lock);
        assert!(replace_verified_binary(b"new", &sums.repeat(2), "asset", &target).is_err());
        assert_eq!(fs::read(target).unwrap(), b"old");
    }

    #[test]
    fn running_binary_can_replace_itself_and_restart() {
        use std::process::{Command, Stdio};

        // Run the production replacement path inside a disposable executable,
        // including Windows' self-replace helper, without touching the test runner.
        if let Some(candidate) = std::env::var_os("MICU_SELF_REPLACE_CANDIDATE") {
            let bytes = fs::read(candidate).unwrap();
            let target = std::env::current_exe().unwrap();
            let sums = format!("{}  fixture\n", sha256_hex(&bytes));
            replace_verified_binary(&bytes, &sums, "fixture", &target).unwrap();
            return;
        }
        if let Some(expected) = std::env::var_os("MICU_SELF_REPLACE_EXPECTED") {
            assert_eq!(
                fs::read(std::env::current_exe().unwrap()).unwrap(),
                fs::read(expected).unwrap()
            );
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let original = std::env::current_exe().unwrap();
        let target = temp.path().join(if cfg!(windows) {
            "installed 程序's binary.exe"
        } else {
            "installed 程序's binary"
        });
        install_binary(&original, &target).unwrap();
        let old_bytes = fs::read(&target).unwrap();
        let native_fixture = std::env::var_os("MICU_UPDATE_FIXTURE_BINARY");
        let bytes = if let Some(path) = &native_fixture {
            fs::read(path).unwrap()
        } else if cfg!(windows) {
            // A PE overlay leaves the executable runnable and makes replacement
            // observable when this unit test runs without the CI release fixture.
            let mut bytes = old_bytes.clone();
            bytes.extend_from_slice(b"micu-self-replace-offline-fixture");
            bytes
        } else {
            format!("#!/bin/sh\nprintf '{}\\n'\n", env!("CARGO_PKG_VERSION")).into_bytes()
        };
        let candidate = temp.path().join("candidate");
        fs::write(&candidate, &bytes).unwrap();
        let test_name = "updates::tests::running_binary_can_replace_itself_and_restart";
        let mut child = Command::new(&target)
            .args(["--exact", test_name, "--nocapture"])
            .env("MICU_SELF_REPLACE_CANDIDATE", &candidate)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(45);
        while child.try_wait().unwrap().is_none() {
            if std::time::Instant::now() >= deadline {
                let _ignored = child.kill();
                panic!("self-replacement subprocess did not exit within 45 seconds");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let result = child.wait_with_output().unwrap();
        assert!(result.status.success(), "{:?}", result);
        assert_eq!(fs::read(&target).unwrap(), bytes);
        assert_eq!(fs::read(backup_path(&target)).unwrap(), old_bytes);
        let mut command = Command::new(&target);
        if native_fixture.is_some() || cfg!(unix) {
            command.arg("version");
        } else {
            command
                .args(["--exact", test_name, "--nocapture"])
                .env("MICU_SELF_REPLACE_EXPECTED", &candidate);
        }
        let restarted = command.output().unwrap();
        assert!(restarted.status.success(), "{:?}", restarted);
        if native_fixture.is_some() || cfg!(unix) {
            assert_eq!(
                String::from_utf8_lossy(&restarted.stdout).trim(),
                env!("CARGO_PKG_VERSION")
            );
        }
    }
}
