use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::{Map as JsonMap, Value as JsonValue, json};
use toml_edit::{Array, DocumentMut, Item, value};
use url::Url;

use crate::config::{Config, is_safe_base_url};

const DEFAULT_BASE_URL: &str = "https://www.micuapi.ai";

#[derive(Clone, Debug)]
pub struct InstallOptions {
    pub no_codex: bool,
    pub no_claude: bool,
    pub yes: bool,
    pub base_url: Option<String>,
    pub save_dir: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug)]
pub struct ResetOptions {
    pub no_codex: bool,
    pub no_claude: bool,
    pub yes: bool,
}

pub fn install(options: InstallOptions) -> Result<(), String> {
    let home = dirs::home_dir().ok_or_else(|| "无法确定用户 home 目录".to_owned())?;
    let base_url = validated_base_url(
        options
            .base_url
            .or_else(|| std::env::var("MICU_BASEURL").ok())
            .as_deref()
            .unwrap_or(DEFAULT_BASE_URL),
    )?;
    let api_key = match std::env::var("MICU_API_KEY")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
    {
        Some(key) => key,
        None if options.yes => {
            return Err("--yes 模式需要环境变量 MICU_API_KEY".into());
        }
        None => rpassword::prompt_password("米醋 Image2 API key: ")
            .map_err(|error| format!("读取 API key 失败: {error}"))?
            .trim()
            .to_owned(),
    };
    if api_key.is_empty() {
        return Err("API key 不能为空".into());
    }
    let save_dir = options
        .save_dir
        .or_else(|| std::env::var_os("MICU_SAVE_DIR").map(PathBuf::from))
        .unwrap_or_else(|| home.join("Pictures").join("micu-out"));
    let save_dir = absolute_path(&save_dir)?;
    fs::create_dir_all(&save_dir)
        .map_err(|error| format!("创建输出目录 {} 失败: {error}", save_dir.display()))?;
    let binary = std::env::current_exe()
        .map_err(|error| format!("无法定位当前 binary: {error}"))?
        .canonicalize()
        .map_err(|error| format!("无法解析当前 binary: {error}"))?;
    let env = install_environment(&api_key, &base_url, &save_dir);

    write_status(format!(
        "将安装 Rust binary={}，API key={}，save_dir={}",
        binary.display(),
        mask_key(&api_key),
        save_dir.display()
    ))?;
    if !options.yes && !confirm("继续写入 Claude/Codex 配置？")? {
        return Err("用户取消".into());
    }
    if !options.no_claude {
        let path = home.join(".claude.json");
        install_claude_file(&path, &binary, &env)?;
        write_status(format!("已更新 {}", path.display()))?;
    }
    if !options.no_codex {
        let path = home.join(".codex").join("config.toml");
        install_codex_file(&path, &binary, &env)?;
        write_status(format!("已更新 {}", path.display()))?;
    }
    Ok(())
}

pub fn reset(options: ResetOptions) -> Result<(), String> {
    if !options.yes && !confirm("仅移除 micu-image 配置并保留其他 MCP server，继续？")?
    {
        return Err("用户取消".into());
    }
    let home = dirs::home_dir().ok_or_else(|| "无法确定用户 home 目录".to_owned())?;
    if !options.no_claude {
        let path = home.join(".claude.json");
        if reset_claude_file(&path)? {
            write_status(format!(
                "已从 {} 移除 mcpServers.micu-image",
                path.display()
            ))?;
        }
    }
    if !options.no_codex {
        let path = home.join(".codex").join("config.toml");
        if reset_codex_file(&path)? {
            write_status(format!(
                "已从 {} 移除 [mcp_servers.micu-image]",
                path.display()
            ))?;
        }
    }
    Ok(())
}

pub fn doctor() -> Result<(), String> {
    let config = Config::load().map_err(|error| error.to_string())?;
    write_status(format!(
        "binary={} version={} base_url={} api_key={} save_root={}",
        std::env::current_exe()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "<unknown>".into()),
        env!("CARGO_PKG_VERSION"),
        config.base_url,
        mask_key(secrecy::ExposeSecret::expose_secret(&config.api_key)),
        config.save_root.display()
    ))?;
    fs::create_dir_all(&config.save_root)
        .map_err(|error| format!("save root 不可写 {}: {error}", config.save_root.display()))?;
    write_status("doctor: OK".into())
}

fn install_environment(api_key: &str, base_url: &str, save_dir: &Path) -> BTreeMap<String, String> {
    let mut env = BTreeMap::from([
        ("MICU_API_KEY".into(), api_key.into()),
        (
            "MICU_SAVE_DIR".into(),
            save_dir.to_string_lossy().into_owned(),
        ),
        (
            "MICU_SAVE_DIR_ROOT".into(),
            save_dir.to_string_lossy().into_owned(),
        ),
    ]);
    if base_url != DEFAULT_BASE_URL {
        env.insert("MICU_BASEURL".into(), base_url.into());
    }
    env
}

fn install_claude_file(
    path: &Path,
    binary: &Path,
    env: &BTreeMap<String, String>,
) -> Result<(), String> {
    let current = if path.exists() {
        let text = fs::read_to_string(path)
            .map_err(|error| format!("读取 {} 失败: {error}", path.display()))?;
        serde_json::from_str(&text)
            .map_err(|error| format!("{} 不是合法 JSON: {error}", path.display()))?
    } else {
        json!({})
    };
    let updated = install_claude_value(current, binary, env)?;
    backup(path)?;
    let bytes = serde_json::to_vec_pretty(&updated)
        .map_err(|error| format!("Claude JSON 序列化失败: {error}"))?;
    atomic_write_secure(path, &bytes)
}

fn reset_claude_file(path: &Path) -> Result<bool, String> {
    if !path.exists() {
        return Ok(false);
    }
    let text = fs::read_to_string(path)
        .map_err(|error| format!("读取 {} 失败: {error}", path.display()))?;
    let current: JsonValue = serde_json::from_str(&text)
        .map_err(|error| format!("{} 不是合法 JSON: {error}", path.display()))?;
    let (updated, changed) = reset_claude_value(current)?;
    if !changed {
        return Ok(false);
    }
    backup(path)?;
    let bytes = serde_json::to_vec_pretty(&updated)
        .map_err(|error| format!("Claude JSON 序列化失败: {error}"))?;
    atomic_write_secure(path, &bytes)?;
    Ok(true)
}

fn install_codex_file(
    path: &Path,
    binary: &Path,
    env: &BTreeMap<String, String>,
) -> Result<(), String> {
    let text = if path.exists() {
        fs::read_to_string(path)
            .map_err(|error| format!("读取 {} 失败: {error}", path.display()))?
    } else {
        String::new()
    };
    let mut document = text
        .parse::<DocumentMut>()
        .map_err(|error| format!("{} 不是合法 TOML: {error}", path.display()))?;
    install_codex_document(&mut document, binary, env)?;
    backup(path)?;
    atomic_write_secure(path, document.to_string().as_bytes())
}

fn reset_codex_file(path: &Path) -> Result<bool, String> {
    if !path.exists() {
        return Ok(false);
    }
    let text = fs::read_to_string(path)
        .map_err(|error| format!("读取 {} 失败: {error}", path.display()))?;
    let mut document = text
        .parse::<DocumentMut>()
        .map_err(|error| format!("{} 不是合法 TOML: {error}", path.display()))?;
    if !reset_codex_document(&mut document) {
        return Ok(false);
    }
    backup(path)?;
    atomic_write_secure(path, document.to_string().as_bytes())?;
    Ok(true)
}

fn install_claude_value(
    mut current: JsonValue,
    binary: &Path,
    env: &BTreeMap<String, String>,
) -> Result<JsonValue, String> {
    let root = current
        .as_object_mut()
        .ok_or_else(|| "~/.claude.json 顶层必须是 object".to_owned())?;
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| JsonValue::Object(JsonMap::new()))
        .as_object_mut()
        .ok_or_else(|| "~/.claude.json 的 mcpServers 必须是 object".to_owned())?;
    servers.insert(
        "micu-image".into(),
        json!({
            "command": binary.to_string_lossy(),
            "args": [],
            "env": env,
        }),
    );
    Ok(current)
}

fn reset_claude_value(mut current: JsonValue) -> Result<(JsonValue, bool), String> {
    let root = current
        .as_object_mut()
        .ok_or_else(|| "~/.claude.json 顶层必须是 object".to_owned())?;
    let changed = root
        .get_mut("mcpServers")
        .and_then(JsonValue::as_object_mut)
        .and_then(|servers| servers.remove("micu-image"))
        .is_some();
    Ok((current, changed))
}

fn install_codex_document(
    document: &mut DocumentMut,
    binary: &Path,
    env: &BTreeMap<String, String>,
) -> Result<(), String> {
    if document.get("mcp_servers").is_none() {
        document["mcp_servers"] = Item::Table(toml_edit::Table::new());
    }
    let servers = document
        .get_mut("mcp_servers")
        .and_then(Item::as_table_mut)
        .ok_or_else(|| "Codex config 的 mcp_servers 必须是 table".to_owned())?;
    let mut server = toml_edit::Table::new();
    server["command"] = value(binary.to_string_lossy().into_owned());
    server["args"] = value(Array::new());
    let mut env_table = toml_edit::Table::new();
    for (name, value_text) in env {
        env_table[name] = value(value_text.clone());
    }
    server.insert("env", Item::Table(env_table));
    servers.insert("micu-image", Item::Table(server));
    Ok(())
}

fn reset_codex_document(document: &mut DocumentMut) -> bool {
    document
        .get_mut("mcp_servers")
        .and_then(Item::as_table_mut)
        .and_then(|servers| servers.remove("micu-image"))
        .is_some()
}

fn validated_base_url(raw: &str) -> Result<String, String> {
    let url = Url::parse(raw).map_err(|error| format!("base URL 无法解析: {error}"))?;
    if !is_safe_base_url(&url) {
        return Err("base URL 仅允许 https，或本地 localhost HTTP".into());
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    let expanded = if let Ok(remainder) = path.strip_prefix("~") {
        let home = dirs::home_dir().ok_or_else(|| "无法确定用户 home 目录".to_owned())?;
        if remainder.as_os_str().is_empty() {
            home
        } else {
            home.join(remainder)
        }
    } else {
        path.to_path_buf()
    };
    if expanded.is_absolute() {
        Ok(expanded)
    } else {
        std::env::current_dir()
            .map(|current| current.join(expanded))
            .map_err(|error| format!("无法解析路径: {error}"))
    }
}

fn backup(path: &Path) -> Result<Option<PathBuf>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let filename = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config".into());
    let backup = path.with_file_name(format!("{filename}.bak.{timestamp}"));
    fs::copy(path, &backup).map_err(|error| format!("备份 {} 失败: {error}", path.display()))?;
    chmod_600(&backup)?;
    Ok(Some(backup))
}

fn atomic_write_secure(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("配置路径没有 parent: {}", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("创建配置目录 {} 失败: {error}", parent.display()))?;
    chmod_700(parent)?;
    let mut temp = tempfile::Builder::new()
        .prefix(".micu-config-")
        .tempfile_in(parent)
        .map_err(|error| format!("创建配置临时文件失败: {error}"))?;
    temp.write_all(bytes)
        .map_err(|error| format!("写配置临时文件失败: {error}"))?;
    temp.as_file()
        .sync_all()
        .map_err(|error| format!("sync 配置临时文件失败: {error}"))?;
    chmod_600(temp.path())?;
    temp.persist(path)
        .map_err(|error| format!("原子替换配置失败: {}", error.error))?;
    chmod_600(path)
}

#[cfg(unix)]
fn chmod_600(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("设置 {} 权限失败: {error}", path.display()))
}

#[cfg(not(unix))]
fn chmod_600(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn chmod_700(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("设置 {} 权限失败: {error}", path.display()))
}

#[cfg(not(unix))]
fn chmod_700(_path: &Path) -> Result<(), String> {
    Ok(())
}

fn mask_key(key: &str) -> String {
    let characters = key.chars().collect::<Vec<_>>();
    if characters.len() <= 8 {
        return "***".into();
    }
    let start = characters.iter().take(5).collect::<String>();
    let end = characters
        .iter()
        .skip(characters.len().saturating_sub(4))
        .collect::<String>();
    format!("{start}...{end}")
}

fn confirm(prompt: &str) -> Result<bool, String> {
    let mut stderr = std::io::stderr().lock();
    write!(stderr, "{prompt} [y/N]: ").map_err(|error| error.to_string())?;
    stderr.flush().map_err(|error| error.to_string())?;
    let mut input = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut input)
        .map_err(|error| format!("读取确认失败: {error}"))?;
    Ok(matches!(
        input.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn write_status(message: String) -> Result<(), String> {
    let mut stderr = std::io::stderr().lock();
    writeln!(stderr, "{message}").map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_merge_and_reset_preserve_unrelated_fields() {
        let current = json!({
            "theme": "dark",
            "mcpServers": {"other": {"command": "other"}}
        });
        let env = BTreeMap::from([("MICU_API_KEY".into(), "secret".into())]);
        let updated = install_claude_value(current, Path::new("/bin/micu"), &env)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(updated["theme"], "dark");
        assert_eq!(updated["mcpServers"]["other"]["command"], "other");
        assert_eq!(updated["mcpServers"]["micu-image"]["command"], "/bin/micu");
        let (reset, changed) =
            reset_claude_value(updated).unwrap_or_else(|error| panic!("{error}"));
        assert!(changed);
        assert_eq!(reset["mcpServers"]["other"]["command"], "other");
        assert!(reset["mcpServers"].get("micu-image").is_none());
    }

    #[test]
    fn codex_merge_and_reset_preserve_unrelated_sections() {
        let mut document = "model = 'gpt-test'\n\n[mcp_servers.other]\ncommand = 'other'\n"
            .parse::<DocumentMut>()
            .unwrap_or_else(|error| panic!("{error}"));
        let env = BTreeMap::from([
            ("MICU_API_KEY".into(), "secret".into()),
            ("MICU_SAVE_DIR".into(), "/tmp/out".into()),
        ]);
        install_codex_document(&mut document, Path::new("/bin/micu"), &env)
            .unwrap_or_else(|error| panic!("{error}"));
        let rendered = document.to_string();
        assert!(rendered.contains("model = 'gpt-test'"));
        assert!(rendered.contains("[mcp_servers.other]"));
        assert!(rendered.contains("[mcp_servers.micu-image]"), "{rendered}");
        assert!(
            rendered.contains("[mcp_servers.micu-image.env]"),
            "{rendered}"
        );
        assert!(reset_codex_document(&mut document));
        let reset = document.to_string();
        assert!(reset.contains("[mcp_servers.other]"));
        assert!(!reset.contains("mcp_servers.micu-image"));
    }

    #[test]
    fn base_url_validation_and_masking_never_echo_full_keys() {
        assert_eq!(
            validated_base_url("https://example.test/").as_deref(),
            Ok("https://example.test")
        );
        assert!(validated_base_url("http://example.test").is_err());
        assert_eq!(
            validated_base_url("http://localhost:8080/").as_deref(),
            Ok("http://localhost:8080")
        );
        let masked = mask_key("sk-1234567890abcdef");
        assert_eq!(masked, "sk-12...cdef");
        assert!(!masked.contains("1234567890"));
    }
}
