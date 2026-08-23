use std::sync::Arc;

use thiserror::Error;

use crate::{
    config::{
        AppPaths, Config, ConfigError, ENV_KEYS, EnvironmentError, EnvironmentSnapshot, PathError,
        PathSource,
    },
    tools::ToolEngine,
};

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub paths: Arc<AppPaths>,
}

#[derive(Debug, Error)]
pub enum AppError {
    #[error(transparent)]
    Environment(#[from] EnvironmentError),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Paths(#[from] PathError),
    #[error("初始化 MCP modules 失败: {0}")]
    Modules(String),
}

impl AppState {
    pub fn load() -> Result<Self, AppError> {
        let mut environment = EnvironmentSnapshot::capture(ENV_KEYS)?;
        environment.load_platform_secrets();
        let source = PathSource::capture()?;
        let config = Arc::new(Config::from_env(&environment)?);
        let paths = Arc::new(AppPaths::resolve(&environment, source)?);
        Ok(Self { config, paths })
    }

    pub fn tool_engine(&self) -> Result<ToolEngine, AppError> {
        ToolEngine::production(self.config.clone(), self.paths.clone()).map_err(AppError::Modules)
    }
}
