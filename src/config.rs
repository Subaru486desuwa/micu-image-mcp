use std::{collections::BTreeMap, env::VarError, path::PathBuf, time::Duration};

use secrecy::SecretString;
use thiserror::Error;
use url::Url;

use crate::validation::routing::STANDARD_MODEL;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResponseFormat {
    Auto,
    Url,
    B64Json,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub base_url: Url,
    pub api_key: SecretString,
    pub default_model: String,
    pub save_dir: PathBuf,
    pub save_root: PathBuf,
    pub input_root: Option<PathBuf>,
    pub use_shell_proxy: bool,
    pub response_format: ResponseFormat,
    pub response_formats_to_try: Vec<&'static str>,
    pub trusted_download_hosts: Vec<String>,
    pub allow_fake_ip_download: bool,
    pub lock_file: PathBuf,
    pub grok_base_url: String,
    pub grok_api_key: SecretString,
    pub xai_model: String,
    pub grok_size_mode: String,
    pub api_request_timeout: Duration,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("环境变量 {name} 不是有效 UTF-8")]
    InvalidUnicode { name: &'static str },
    #[error("MICU_BASEURL 无法解析: {value:?} ({detail})")]
    InvalidBaseUrl { value: String, detail: String },
    #[error("MICU_BASEURL 仅允许 https，或本机 localhost HTTP；收到 {0}")]
    UnsafeBaseUrl(String),
    #[error("无法确定用户 home 目录")]
    HomeUnavailable,
    #[error("无法解析当前工作目录: {0}")]
    CurrentDirectory(String),
}

impl Config {
    pub fn load() -> Result<Self, ConfigError> {
        const KEYS: &[&str] = &[
            "HOME",
            "USERPROFILE",
            "MICU_API_KEY",
            "MICU_BASEURL",
            "MICU_MODEL",
            "MICU_SAVE_DIR",
            "MICU_SAVE_DIR_ROOT",
            "MICU_INPUT_ROOT",
            "MICU_USE_SHELL_PROXY",
            "MICU_RESPONSE_FORMAT",
            "MICU_TRUSTED_DOWNLOAD_HOSTS",
            "MICU_ALLOW_FAKE_IP_DOWNLOAD",
            "MICU_GROK_BASEURL",
            "MICU_GROK_API_KEY",
            "XAI_API_KEY",
            "GROK_API_KEY",
            "XAI_MODEL",
            "GROK_MODEL",
            "MICU_GROK_SIZE_MODE",
            "MICU_CONTRACT_TESTING",
            "MICU_TEST_API_TIMEOUT_MS",
        ];
        let mut environment = BTreeMap::new();
        for &name in KEYS {
            match std::env::var(name) {
                Ok(value) => {
                    environment.insert(name.to_owned(), value);
                }
                Err(VarError::NotPresent) => {}
                Err(VarError::NotUnicode(_)) => {
                    return Err(ConfigError::InvalidUnicode { name });
                }
            }
        }
        Self::from_map(&environment)
    }

    pub fn from_map(environment: &BTreeMap<String, String>) -> Result<Self, ConfigError> {
        let home = environment
            .get("HOME")
            .or_else(|| environment.get("USERPROFILE"))
            .map(PathBuf::from)
            .or_else(dirs::home_dir)
            .ok_or(ConfigError::HomeUnavailable)?;
        let base_url_raw = environment
            .get("MICU_BASEURL")
            .map(String::as_str)
            .unwrap_or("https://www.micuapi.ai");
        let base_url = Url::parse(base_url_raw).map_err(|error| ConfigError::InvalidBaseUrl {
            value: base_url_raw.to_owned(),
            detail: error.to_string(),
        })?;
        if !is_safe_base_url(&base_url) {
            return Err(ConfigError::UnsafeBaseUrl(base_url_raw.to_owned()));
        }

        let default_root = home.join("Pictures").join("micu-out");
        let save_root = match environment.get("MICU_SAVE_DIR_ROOT") {
            Some(path) => absolute_path(path, &home)?,
            None => default_root,
        };
        let save_dir = match environment.get("MICU_SAVE_DIR") {
            Some(path) => absolute_path(path, &home)?,
            None => save_root.clone(),
        };
        let input_root = environment
            .get("MICU_INPUT_ROOT")
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(|value| absolute_path(value, &home))
            .transpose()?;

        let response_format = match environment
            .get("MICU_RESPONSE_FORMAT")
            .map(|value| value.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("url") => ResponseFormat::Url,
            Some("b64_json") => ResponseFormat::B64Json,
            _ => ResponseFormat::Auto,
        };
        let response_formats_to_try = match response_format {
            ResponseFormat::Auto => vec!["url", "b64_json"],
            ResponseFormat::Url => vec!["url"],
            ResponseFormat::B64Json => vec!["b64_json"],
        };
        let trusted_raw = environment
            .get("MICU_TRUSTED_DOWNLOAD_HOSTS")
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .unwrap_or("oss.filenest.top");
        let mut trusted_download_hosts = trusted_raw
            .split(',')
            .map(|host| host.trim().trim_end_matches('.').to_ascii_lowercase())
            .filter(|host| !host.is_empty())
            .collect::<Vec<_>>();
        trusted_download_hosts.sort();
        trusted_download_hosts.dedup();

        let grok_base_url = environment
            .get("MICU_GROK_BASEURL")
            .cloned()
            .unwrap_or_else(|| base_url_raw.to_owned());
        let grok_key = first_non_empty(
            environment,
            &["MICU_GROK_API_KEY", "XAI_API_KEY", "GROK_API_KEY"],
        );
        let xai_model = first_non_empty(environment, &["XAI_MODEL", "GROK_MODEL"])
            .unwrap_or_else(|| "grok-imagine-image-lite".to_owned());
        let grok_size_mode = environment
            .get("MICU_GROK_SIZE_MODE")
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "contain".to_owned());
        let api_request_timeout = if environment
            .get("MICU_CONTRACT_TESTING")
            .is_some_and(|value| value.trim() == "1")
        {
            environment
                .get("MICU_TEST_API_TIMEOUT_MS")
                .and_then(|value| value.trim().parse::<u64>().ok())
                .map(|milliseconds| Duration::from_millis(milliseconds.max(10)))
                .unwrap_or(Duration::from_secs(600))
        } else {
            Duration::from_secs(600)
        };

        Ok(Self {
            base_url,
            api_key: environment
                .get("MICU_API_KEY")
                .cloned()
                .unwrap_or_default()
                .into(),
            default_model: environment
                .get("MICU_MODEL")
                .cloned()
                .unwrap_or_else(|| STANDARD_MODEL.to_owned()),
            save_dir,
            save_root,
            input_root,
            use_shell_proxy: env_truthy(environment.get("MICU_USE_SHELL_PROXY"), false),
            response_format,
            response_formats_to_try,
            trusted_download_hosts,
            allow_fake_ip_download: env_truthy(
                environment.get("MICU_ALLOW_FAKE_IP_DOWNLOAD"),
                true,
            ),
            lock_file: home.join(".cache").join("micu-image").join("bigsize.lock"),
            grok_base_url,
            grok_api_key: grok_key.unwrap_or_default().into(),
            xai_model,
            grok_size_mode,
            api_request_timeout,
        })
    }
}

pub fn is_safe_base_url(url: &Url) -> bool {
    if !url.username().is_empty() || url.password().is_some() || url.host_str().is_none() {
        return false;
    }
    if url.scheme() == "https" {
        return true;
    }
    if url.scheme() != "http" {
        return false;
    }
    let host = url
        .host_str()
        .unwrap_or_default()
        .trim_end_matches('.')
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    host == "localhost" || host == "127.0.0.1" || host == "::1" || host.ends_with(".localhost")
}

pub fn default_model() -> &'static str {
    STANDARD_MODEL
}

fn first_non_empty(environment: &BTreeMap<String, String>, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        environment
            .get(*name)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

fn env_truthy(value: Option<&String>, default: bool) -> bool {
    value.map_or(default, |raw| {
        matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes"
        )
    })
}

fn absolute_path(raw: &str, home: &std::path::Path) -> Result<PathBuf, ConfigError> {
    let expanded = if raw == "~" {
        home.to_path_buf()
    } else if let Some(remainder) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        home.join(remainder)
    } else {
        PathBuf::from(raw)
    };
    if expanded.is_absolute() {
        return Ok(expanded);
    }
    let current = std::env::current_dir()
        .map_err(|error| ConfigError::CurrentDirectory(error.to_string()))?;
    Ok(current.join(expanded))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use secrecy::ExposeSecret;

    use super::*;

    fn base_env() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("HOME".into(), "/tmp/micu-home".into()),
            ("MICU_API_KEY".into(), "sk-super-secret".into()),
            ("MICU_SAVE_DIR_ROOT".into(), "/tmp/micu-root".into()),
            ("MICU_SAVE_DIR".into(), "/tmp/micu-root/out".into()),
        ])
    }

    #[test]
    fn config_freezes_current_environment_and_redacts_secrets() {
        let config = Config::from_map(&base_env()).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(config.base_url.as_str(), "https://www.micuapi.ai/");
        assert_eq!(config.default_model, "gpt-image-2");
        assert_eq!(config.api_key.expose_secret(), "sk-super-secret");
        assert!(!format!("{config:?}").contains("sk-super-secret"));
        assert_eq!(config.response_formats_to_try, ["url", "b64_json"]);
    }

    #[test]
    fn only_https_or_loopback_http_base_urls_are_accepted() {
        for accepted in [
            "https://api.example.test",
            "http://localhost:8080",
            "http://127.0.0.1:8080",
            "http://[::1]:8080",
            "http://mock.localhost:8080",
        ] {
            let url = Url::parse(accepted).unwrap_or_else(|error| panic!("{error}"));
            assert!(is_safe_base_url(&url), "{accepted}");
        }
        for rejected in [
            "http://api.example.test",
            "ftp://example.test",
            "https://user:pass@example.test",
        ] {
            let url = Url::parse(rejected).unwrap_or_else(|error| panic!("{error}"));
            assert!(!is_safe_base_url(&url), "{rejected}");
        }
    }

    #[test]
    fn response_format_and_proxy_are_explicit_opt_ins() {
        let mut environment = base_env();
        environment.insert("MICU_RESPONSE_FORMAT".into(), "b64_json".into());
        environment.insert("MICU_USE_SHELL_PROXY".into(), "yes".into());
        environment.insert("MICU_ALLOW_FAKE_IP_DOWNLOAD".into(), "0".into());
        let config = Config::from_map(&environment).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(config.response_format, ResponseFormat::B64Json);
        assert_eq!(config.response_formats_to_try, ["b64_json"]);
        assert!(config.use_shell_proxy);
        assert!(!config.allow_fake_ip_download);
    }
}
