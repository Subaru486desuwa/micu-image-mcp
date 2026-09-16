use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::{BufRead, IsTerminal, Write},
    path::{Path, PathBuf},
};

use secrecy::ExposeSecret;
use url::Url;

use crate::config::{
    AppPaths, Config, ENV_KEYS, EnvironmentSnapshot, PathSource, is_safe_base_url,
};
use crate::credentials::{
    DEFAULT_CREDENTIAL_ACCOUNT, DEFAULT_CREDENTIAL_SERVICE, load_api_key, store_api_key,
    validate_api_key,
};

pub mod atomic;
pub mod binary;
pub mod claude;
mod client_config;
pub mod codex;
mod error;

pub use client_config::ClientLaunchSpec;
pub use error::InstallError;

const DEFAULT_BASE_URL: &str = "https://www.micuapi.ai";

#[derive(Clone, Debug)]
pub struct InstallOptions {
    pub no_codex: bool,
    pub no_claude: bool,
    pub yes: bool,
    pub base_url: Option<String>,
    pub save_dir: Option<PathBuf>,
    pub binary_path: Option<PathBuf>,
    pub dev: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct ResetOptions {
    pub no_codex: bool,
    pub no_claude: bool,
    pub yes: bool,
}

pub fn install(options: InstallOptions) -> Result<(), InstallError> {
    let mut environment = EnvironmentSnapshot::capture(ENV_KEYS)?;
    if let Some(save_dir) = &options.save_dir {
        let value = path_text(save_dir, "--save-dir")?;
        environment.insert("MICU_SAVE_DIR_ROOT", value.clone());
        environment.insert("MICU_SAVE_DIR", value);
    } else if environment.get("MICU_SAVE_DIR_ROOT").is_none()
        && let Some(save_dir) = environment.get("MICU_SAVE_DIR").map(str::to_owned)
    {
        environment.insert("MICU_SAVE_DIR_ROOT", save_dir);
    }
    let base_url = validated_base_url(
        options
            .base_url
            .as_deref()
            .or_else(|| environment.get("MICU_BASEURL"))
            .unwrap_or(DEFAULT_BASE_URL),
    )?;
    environment.insert("MICU_BASEURL", base_url.clone());

    let paths = AppPaths::resolve(&environment, PathSource::capture()?)?;
    configure_api_key(&mut environment, &paths, &options)?;
    let selected_source = options.binary_path.as_deref().unwrap_or(&paths.executable);
    let command = if options.dev {
        selected_source
            .canonicalize()
            .map_err(|error| InstallError::BinarySource(error.to_string()))?
    } else {
        binary::install_binary(selected_source, &paths.install_binary)?
    };
    let launch = ClientLaunchSpec::new(
        command.clone(),
        Vec::<OsString>::new(),
        install_environment(&environment, &paths, &base_url)?,
    );

    write_status(format!(
        "将安装 Rust binary={}，save_dir={}，API key 不写入客户端配置",
        path_text(&command, "installed binary")?,
        path_text(&paths.default_save_dir, "save dir")?,
    ))?;
    if !options.yes && !confirm("继续写入 Claude/Codex 配置？")? {
        return Err(InstallError::Cancelled);
    }
    if !options.no_claude {
        let report = claude::write_config_file(&paths.claude_config, &launch)?;
        write_status(format!(
            "已更新 {}{}",
            path_text(&report.path, "Claude config")?,
            backup_suffix(report.backup.as_deref())?
        ))?;
    }
    if !options.no_codex {
        let report = codex::write_config_file(&paths.codex_config, &launch)?;
        write_status(format!(
            "已更新 {}{}",
            path_text(&report.path, "Codex config")?,
            backup_suffix(report.backup.as_deref())?
        ))?;
    }
    Ok(())
}

pub fn reset(options: ResetOptions) -> Result<(), InstallError> {
    if !options.yes && !confirm("仅移除 micu-image 配置并保留其他 MCP server，继续？")?
    {
        return Err(InstallError::Cancelled);
    }
    let environment = EnvironmentSnapshot::capture(ENV_KEYS)?;
    let paths = AppPaths::resolve(&environment, PathSource::capture()?)?;
    if !options.no_claude && claude::reset_config_file(&paths.claude_config)?.is_some() {
        write_status("已从 Claude JSON 移除 mcpServers.micu-image".into())?;
    }
    if !options.no_codex && codex::reset_config_file(&paths.codex_config)?.is_some() {
        write_status("已从 Codex TOML 移除 mcp_servers.micu-image".into())?;
    }
    Ok(())
}

pub fn doctor() -> Result<(), InstallError> {
    let mut environment = EnvironmentSnapshot::capture(ENV_KEYS)?;
    environment.load_platform_secrets();
    let paths = AppPaths::resolve(&environment, PathSource::capture()?)?;
    let config = Config::from_env(&environment)?;
    let mut configured = 0_usize;

    if paths.codex_config.is_file() {
        let text = fs::read_to_string(&paths.codex_config)
            .map_err(|error| InstallError::Doctor(format!("读取 Codex config 失败: {error}")))?;
        if let Some(launch) = codex::parse_config_launch(&text)? {
            verify_launch("Codex", &launch, &paths)?;
            configured += 1;
        }
    }
    if paths.claude_config.is_file() {
        let text = fs::read_to_string(&paths.claude_config)
            .map_err(|error| InstallError::Doctor(format!("读取 Claude config 失败: {error}")))?;
        if let Some(launch) = claude::parse_config_launch(&text)? {
            verify_launch("Claude", &launch, &paths)?;
            configured += 1;
        }
    }
    if configured == 0 {
        return Err(InstallError::Doctor(
            "Codex/Claude 均未配置 micu-image".into(),
        ));
    }
    if !paths.save_root.is_dir() || !paths.default_save_dir.is_dir() {
        return Err(InstallError::Doctor(
            "save root/default save dir 不可用".into(),
        ));
    }
    write_status(format!(
        "doctor: OK version={} clients={} base_url={} api_key_configured={} save_root={} lock_file={}",
        env!("CARGO_PKG_VERSION"),
        configured,
        config.base_url,
        !config.api_key.expose_secret().trim().is_empty(),
        path_text(&paths.save_root, "save root")?,
        path_text(&paths.lock_file, "lock file")?,
    ))
}

fn install_environment(
    source: &EnvironmentSnapshot,
    paths: &AppPaths,
    base_url: &str,
) -> Result<BTreeMap<String, String>, InstallError> {
    let mut environment = BTreeMap::from([
        (
            "MICU_SAVE_DIR".into(),
            path_text(&paths.default_save_dir, "MICU_SAVE_DIR")?,
        ),
        (
            "MICU_SAVE_DIR_ROOT".into(),
            path_text(&paths.save_root, "MICU_SAVE_DIR_ROOT")?,
        ),
    ]);
    if base_url != DEFAULT_BASE_URL {
        environment.insert("MICU_BASEURL".into(), base_url.into());
    }
    if let Some(input_root) = &paths.input_root {
        environment.insert(
            "MICU_INPUT_ROOT".into(),
            path_text(input_root, "MICU_INPUT_ROOT")?,
        );
    }
    for name in [
        "MICU_MODEL",
        "MICU_USE_SHELL_PROXY",
        "MICU_RESPONSE_FORMAT",
        "MICU_TRUSTED_DOWNLOAD_HOSTS",
        "MICU_ALLOW_FAKE_IP_DOWNLOAD",
        "MICU_KEYCHAIN_ACCOUNT",
        "MICU_KEYCHAIN_SERVICE",
    ] {
        if let Some(value) = source
            .get(name)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            environment.insert(name.into(), value.into());
        }
    }
    Ok(environment)
}

fn configure_api_key(
    environment: &mut EnvironmentSnapshot,
    paths: &AppPaths,
    options: &InstallOptions,
) -> Result<(), InstallError> {
    let service = environment
        .get("MICU_KEYCHAIN_SERVICE")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_CREDENTIAL_SERVICE)
        .to_owned();
    let account = environment
        .get("MICU_KEYCHAIN_ACCOUNT")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_CREDENTIAL_ACCOUNT)
        .to_owned();

    if let Some(api_key) = environment
        .get("MICU_API_KEY")
        .filter(|value| !value.trim().is_empty())
    {
        validate_api_key(api_key)?;
        match store_api_key(&service, &account, api_key) {
            Ok(()) => {
                environment.insert("MICU_KEYCHAIN_SERVICE", service);
                environment.insert("MICU_KEYCHAIN_ACCOUNT", account);
                write_status(
                    "检测到 MICU_API_KEY 环境变量，格式校验通过并已同步到系统安全凭据；不会写入客户端配置"
                        .into(),
                )?;
            }
            Err(error) => write_status(format!(
                "检测到 MICU_API_KEY 环境变量且格式校验通过，但系统安全凭据写入失败（{error}）；将继续依赖进程环境"
            ))?,
        }
        return Ok(());
    }

    match load_api_key(&service, &account) {
        Ok(Some(api_key)) => match validate_api_key(&api_key) {
            Ok(_) => {
                environment.insert("MICU_KEYCHAIN_SERVICE", service);
                environment.insert("MICU_KEYCHAIN_ACCOUNT", account);
                write_status("已找到系统安全凭据中的 API key，格式校验通过，将直接复用".into())?;
                return Ok(());
            }
            Err(error) if std::io::stdin().is_terminal() => {
                write_status(format!(
                    "系统安全凭据中的 API key 格式无效（{error}）；请重新输入以覆盖旧凭据"
                ))?;
            }
            Err(error) => return Err(error.into()),
        },
        Ok(None) => {}
        Err(error) => {
            write_status(format!(
                "未能读取系统安全凭据（{error}）；若当前终端可交互，将继续进行首次配置"
            ))?;
        }
    }

    if let Some(api_key) = legacy_client_api_key(paths, options)? {
        match validate_api_key(&api_key) {
            Ok(_) => {
                store_api_key(&service, &account, &api_key)?;
                environment.insert("MICU_KEYCHAIN_SERVICE", service);
                environment.insert("MICU_KEYCHAIN_ACCOUNT", account);
                write_status(
                    "已从旧客户端 MCP 配置迁移 API key 到系统安全凭据；客户端配置中的明文 key 将被移除"
                        .into(),
                )?;
                return Ok(());
            }
            Err(error) if std::io::stdin().is_terminal() => {
                write_status(format!(
                    "旧客户端配置中的 API key 格式无效（{error}）；请重新输入以替换旧凭据"
                ))?;
            }
            Err(error) => return Err(error.into()),
        }
    }

    if !std::io::stdin().is_terminal() {
        write_status(
            "未检测到 API key，且当前为非交互输入；安装继续，但首次生图前需设置 MICU_API_KEY 或重新交互运行 installer"
                .into(),
        )?;
        return Ok(());
    }

    for attempt in 1..=3 {
        let api_key = rpassword::prompt_password("Micu API Key（输入隐藏，格式 sk-...）: ")
            .map_err(|error| InstallError::CredentialInput(error.to_string()))?;
        let api_key = api_key.trim();
        match validate_api_key(api_key) {
            Ok(_) => {
                store_api_key(&service, &account, api_key)?;
                environment.insert("MICU_KEYCHAIN_SERVICE", service);
                environment.insert("MICU_KEYCHAIN_ACCOUNT", account);
                write_status("API key 格式校验通过，已写入系统安全凭据存储".into())?;
                return Ok(());
            }
            Err(error) if attempt < 3 => {
                write_status(format!("API key 格式无效：{error}；请重新输入"))?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    unreachable!("three credential attempts always return")
}

fn legacy_client_api_key(
    paths: &AppPaths,
    options: &InstallOptions,
) -> Result<Option<String>, InstallError> {
    let mut found = Vec::new();
    if !options.no_codex && paths.codex_config.is_file() {
        let text =
            fs::read_to_string(&paths.codex_config).map_err(|error| InstallError::ConfigIo {
                action: "读取 Codex 旧配置以迁移 API key",
                detail: error.to_string(),
            })?;
        if let Some(launch) = codex::parse_config_launch(&text)?
            && let Some(api_key) = launch
                .env()
                .get("MICU_API_KEY")
                .filter(|value| !value.trim().is_empty())
        {
            found.push(api_key.clone());
        }
    }
    if !options.no_claude && paths.claude_config.is_file() {
        let text =
            fs::read_to_string(&paths.claude_config).map_err(|error| InstallError::ConfigIo {
                action: "读取 Claude 旧配置以迁移 API key",
                detail: error.to_string(),
            })?;
        if let Some(launch) = claude::parse_config_launch(&text)?
            && let Some(api_key) = launch
                .env()
                .get("MICU_API_KEY")
                .filter(|value| !value.trim().is_empty())
        {
            found.push(api_key.clone());
        }
    }
    found.dedup();
    match found.len() {
        0 => Ok(None),
        1 => Ok(found.pop()),
        _ => Err(InstallError::LegacyCredentialConflict),
    }
}

fn verify_launch(
    client: &str,
    launch: &ClientLaunchSpec,
    paths: &AppPaths,
) -> Result<(), InstallError> {
    if !launch.args().is_empty() {
        return Err(InstallError::Doctor(format!(
            "{client} micu-image args 必须为空"
        )));
    }
    let command = launch.command();
    if !command.is_file() {
        return Err(InstallError::Doctor(format!(
            "{client} binary 不存在: {}",
            path_text(command, "configured binary")?
        )));
    }
    if !is_executable(command)? {
        return Err(InstallError::Doctor(format!(
            "{client} binary 不可执行: {}",
            path_text(command, "configured binary")?
        )));
    }
    let canonical = command
        .canonicalize()
        .map_err(|error| InstallError::Doctor(format!("{client} binary 无法解析: {error}")))?;
    let stable = paths.install_binary.canonicalize().ok();
    let current = paths.executable.canonicalize().ok();
    if Some(canonical.clone()) != stable && Some(canonical.clone()) != current {
        return Err(InstallError::Doctor(format!(
            "{client} command 不是稳定安装 binary 或当前 --dev binary"
        )));
    }
    let output = std::process::Command::new(&canonical)
        .arg("version")
        .output()
        .map_err(|error| InstallError::Doctor(format!("{client} version 检查失败: {error}")))?;
    if !output.status.success() {
        return Err(InstallError::Doctor(format!(
            "{client} binary version 命令失败"
        )));
    }
    let version = String::from_utf8(output.stdout)
        .map_err(|_| InstallError::Doctor(format!("{client} version 输出不是 UTF-8")))?;
    if version.trim() != env!("CARGO_PKG_VERSION") {
        return Err(InstallError::Doctor(format!(
            "{client} binary version={}，期望 {}",
            version.trim(),
            env!("CARGO_PKG_VERSION")
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn is_executable(path: &Path) -> Result<bool, InstallError> {
    use std::os::unix::fs::PermissionsExt;

    fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
        .map_err(|error| InstallError::Doctor(format!("检查 binary 权限失败: {error}")))
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> Result<bool, InstallError> {
    Ok(path.is_file())
}

fn validated_base_url(raw: &str) -> Result<String, InstallError> {
    let url = Url::parse(raw).map_err(|error| {
        InstallError::Config(crate::config::ConfigError::InvalidBaseUrl {
            value: raw.to_owned(),
            detail: error.to_string(),
        })
    })?;
    if !is_safe_base_url(&url) {
        return Err(InstallError::Config(
            crate::config::ConfigError::UnsafeBaseUrl(raw.to_owned()),
        ));
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

fn path_text(path: &Path, context: &'static str) -> Result<String, InstallError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or(InstallError::NonUnicodePath { context })
}

fn backup_suffix(path: Option<&Path>) -> Result<String, InstallError> {
    path.map(|backup| path_text(backup, "backup path").map(|text| format!("，备份={text}")))
        .transpose()
        .map(Option::unwrap_or_default)
}

fn confirm(prompt: &str) -> Result<bool, InstallError> {
    let mut stderr = std::io::stderr().lock();
    write!(stderr, "{prompt} [y/N]: ")
        .map_err(|error| InstallError::StatusIo(error.to_string()))?;
    stderr
        .flush()
        .map_err(|error| InstallError::StatusIo(error.to_string()))?;
    let mut input = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut input)
        .map_err(|error| InstallError::StatusIo(error.to_string()))?;
    Ok(matches!(
        input.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn write_status(message: String) -> Result<(), InstallError> {
    writeln!(std::io::stderr().lock(), "{message}")
        .map_err(|error| InstallError::StatusIo(error.to_string()))
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs};

    use crate::config::test_paths;

    use super::{InstallError, InstallOptions, legacy_client_api_key};

    fn options() -> InstallOptions {
        InstallOptions {
            no_codex: false,
            no_claude: false,
            yes: true,
            base_url: None,
            save_dir: None,
            binary_path: None,
            dev: false,
        }
    }

    #[test]
    fn legacy_client_api_key_is_recovered_without_exposing_it() {
        let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
        let paths = test_paths(temp.path(), BTreeMap::new());
        let key = "sk-12345678901234567";
        fs::create_dir_all(
            paths
                .codex_config
                .parent()
                .unwrap_or_else(|| panic!("missing codex parent")),
        )
        .unwrap_or_else(|error| panic!("{error}"));
        fs::write(
            &paths.codex_config,
            format!(
                "[mcp_servers.micu-image]\ncommand = 'old'\nargs = []\n\n[mcp_servers.micu-image.env]\nMICU_API_KEY = '{key}'\n"
            ),
        )
        .unwrap_or_else(|error| panic!("{error}"));
        fs::write(
            &paths.claude_config,
            format!(
                r#"{{"mcpServers":{{"micu-image":{{"command":"old","args":[],"env":{{"MICU_API_KEY":"{key}"}}}}}}}}"#
            ),
        )
        .unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(
            legacy_client_api_key(&paths, &options()).unwrap_or_else(|error| panic!("{error}")),
            Some(key.into())
        );
    }

    #[test]
    fn conflicting_legacy_client_api_keys_are_never_guessed() {
        let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
        let paths = test_paths(temp.path(), BTreeMap::new());
        fs::create_dir_all(
            paths
                .codex_config
                .parent()
                .unwrap_or_else(|| panic!("missing codex parent")),
        )
        .unwrap_or_else(|error| panic!("{error}"));
        fs::write(
            &paths.codex_config,
            "[mcp_servers.micu-image]\ncommand = 'old'\nargs = []\n\n[mcp_servers.micu-image.env]\nMICU_API_KEY = 'sk-11111111111111111'\n",
        )
        .unwrap_or_else(|error| panic!("{error}"));
        fs::write(
            &paths.claude_config,
            r#"{"mcpServers":{"micu-image":{"command":"old","args":[],"env":{"MICU_API_KEY":"sk-22222222222222222"}}}}"#,
        )
        .unwrap_or_else(|error| panic!("{error}"));

        assert!(matches!(
            legacy_client_api_key(&paths, &options()),
            Err(InstallError::LegacyCredentialConflict)
        ));
    }
}
