use thiserror::Error;

use crate::config::{ConfigError, EnvironmentError, PathError};

#[derive(Debug, Error)]
pub enum InstallError {
    #[error(transparent)]
    Environment(#[from] EnvironmentError),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Paths(#[from] PathError),
    #[error("Codex TOML 无法解析: {0}")]
    TomlParse(String),
    #[error("Codex TOML 结构无效: {0}")]
    InvalidCodex(String),
    #[error("{context} 不是合法 Unicode，无法无损写入客户端配置")]
    NonUnicodePath { context: &'static str },
    #[error("Codex TOML 写后 round-trip 校验失败: {0}")]
    CodexRoundTrip(String),
    #[error("Claude JSON 无法解析: {0}")]
    JsonParse(String),
    #[error("Claude JSON 结构无效: {0}")]
    InvalidClaude(String),
    #[error("Claude JSON 写后 round-trip 校验失败: {0}")]
    ClaudeRoundTrip(String),
    #[error("安装 binary source 无效: {0}")]
    BinarySource(String),
    #[error("创建稳定 binary 安装目录失败: {0}")]
    BinaryDirectory(String),
    #[error("复制稳定 binary 失败: {0}")]
    BinaryCopy(String),
    #[error("原子替换稳定 binary 失败: {0}")]
    BinaryReplace(String),
    #[error("配置文件 {action} 失败: {detail}")]
    ConfigIo {
        action: &'static str,
        detail: String,
    },
    #[error("配置文件原子替换失败: {detail}; target={target:?}; backup={backup:?}")]
    ConfigReplace {
        detail: String,
        target: std::path::PathBuf,
        backup: Option<std::path::PathBuf>,
    },
    #[error("配置临时文件校验失败: {detail}; target={target:?}; temp={temp:?}; backup={backup:?}")]
    VerificationFailed {
        detail: String,
        target: std::path::PathBuf,
        temp: std::path::PathBuf,
        backup: Option<std::path::PathBuf>,
    },
    #[error("用户取消")]
    Cancelled,
    #[error("installer 状态输出失败: {0}")]
    StatusIo(String),
    #[error("doctor 失败: {0}")]
    Doctor(String),
}
