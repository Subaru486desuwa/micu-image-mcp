use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use cap_std::{ambient_authority, fs::Dir};
use thiserror::Error;

use super::EnvironmentSnapshot;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathSource {
    pub home: PathBuf,
    pub startup_cwd: PathBuf,
    pub executable: PathBuf,
    pub data_local: PathBuf,
}

impl PathSource {
    pub fn new(
        home: PathBuf,
        startup_cwd: PathBuf,
        executable: PathBuf,
        data_local: PathBuf,
    ) -> Self {
        Self {
            home,
            startup_cwd,
            executable,
            data_local,
        }
    }

    pub fn capture() -> Result<Self, PathError> {
        let home = dirs::home_dir().ok_or(PathError::HomeUnavailable)?;
        let startup_cwd = std::env::current_dir()
            .map_err(|error| PathError::CurrentDirectory(error.to_string()))?;
        let executable = std::env::current_exe()
            .map_err(|error| PathError::CurrentExecutable(error.to_string()))?;
        let data_local = dirs::data_local_dir().unwrap_or_else(|| home.join(".local/share"));
        Ok(Self::new(home, startup_cwd, executable, data_local))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppPaths {
    pub home: PathBuf,
    pub startup_cwd: PathBuf,
    pub executable: PathBuf,
    pub install_binary: PathBuf,
    pub save_root: PathBuf,
    pub default_save_dir: PathBuf,
    pub input_root: Option<PathBuf>,
    pub cache_dir: PathBuf,
    pub lock_file: PathBuf,
    pub codex_config: PathBuf,
    pub claude_config: PathBuf,
}

#[derive(Clone, Debug)]
pub struct PathPolicy {
    home: PathBuf,
    startup_cwd: PathBuf,
    save_root: PathBuf,
    default_save_dir: PathBuf,
    input_root: Option<PathBuf>,
}

#[derive(Debug, Error)]
pub enum PathError {
    #[error("无法确定用户 home 目录")]
    HomeUnavailable,
    #[error("无法解析 server 启动工作目录: {0}")]
    CurrentDirectory(String),
    #[error("无法定位当前 Rust binary: {0}")]
    CurrentExecutable(String),
    #[error("当前 Rust binary 不存在或不是文件")]
    ExecutableMissing,
    #[error("不支持 user-home 路径 {raw:?}；只允许 ~、~/... 或 ~\\...")]
    UnsupportedTilde { raw: String },
    #[error("{context} 包含无法解析的父目录 ..")]
    ParentTraversal { context: &'static str },
    #[error("无法创建 {context}: {detail}")]
    CreateDirectory {
        context: &'static str,
        detail: String,
    },
    #[error("无法规范化 {context}: {detail}")]
    Canonicalize {
        context: &'static str,
        detail: String,
    },
    #[error("{context} 必须位于安全根目录内")]
    OutsideRoot { context: &'static str },
    #[error("MICU_INPUT_ROOT 必须存在且是目录: {0}")]
    InputRoot(String),
}

impl AppPaths {
    pub fn resolve(
        environment: &EnvironmentSnapshot,
        source: PathSource,
    ) -> Result<Self, PathError> {
        if !source.executable.is_file() {
            return Err(PathError::ExecutableMissing);
        }
        let home = fs::canonicalize(&source.home).map_err(|error| PathError::Canonicalize {
            context: "home",
            detail: error.to_string(),
        })?;
        let startup_cwd =
            fs::canonicalize(&source.startup_cwd).map_err(|error| PathError::Canonicalize {
                context: "startup cwd",
                detail: error.to_string(),
            })?;
        let executable =
            fs::canonicalize(&source.executable).map_err(|error| PathError::Canonicalize {
                context: "current executable",
                detail: error.to_string(),
            })?;
        let data_local = if source.data_local.exists() {
            fs::canonicalize(&source.data_local).map_err(|error| PathError::Canonicalize {
                context: "data-local directory",
                detail: error.to_string(),
            })?
        } else {
            normalize_absolute(&source.data_local, "data-local directory")?
        };

        let raw_save_root = environment.get("MICU_SAVE_DIR_ROOT");
        let configured_root = match raw_save_root {
            Some(raw) => resolve_root(raw, &home, "MICU_SAVE_DIR_ROOT")?,
            None => home.join("Pictures/micu-out"),
        };
        fs::create_dir_all(&configured_root).map_err(|error| PathError::CreateDirectory {
            context: "MICU_SAVE_DIR_ROOT",
            detail: error.to_string(),
        })?;
        let save_root =
            fs::canonicalize(&configured_root).map_err(|error| PathError::Canonicalize {
                context: "MICU_SAVE_DIR_ROOT",
                detail: error.to_string(),
            })?;

        let default_save_dir = match environment.get("MICU_SAVE_DIR") {
            Some(raw) => resolve_under_root(raw, &home, &save_root, "MICU_SAVE_DIR")?,
            None => save_root.clone(),
        };
        let default_save_dir =
            create_directory_within(&save_root, &default_save_dir, "MICU_SAVE_DIR")?;

        let input_root = environment
            .get("MICU_INPUT_ROOT")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|raw| {
                let requested = resolve_root(raw, &home, "MICU_INPUT_ROOT")?;
                let canonical = fs::canonicalize(&requested)
                    .map_err(|error| PathError::InputRoot(error.to_string()))?;
                if !canonical.is_dir() {
                    return Err(PathError::InputRoot("不是目录".into()));
                }
                Ok(canonical)
            })
            .transpose()?;

        let cache_dir = home.join(".cache/micu-image");
        let binary_name = if cfg!(windows) {
            "micu-image-mcp.exe"
        } else {
            "micu-image-mcp"
        };
        Ok(Self {
            home: home.clone(),
            startup_cwd,
            executable,
            install_binary: data_local.join("micu-image-mcp/bin").join(binary_name),
            save_root: save_root.clone(),
            default_save_dir,
            input_root,
            lock_file: cache_dir.join("bigsize.lock"),
            cache_dir,
            codex_config: home.join(".codex/config.toml"),
            claude_config: home.join(".claude.json"),
        })
    }
}

impl PathPolicy {
    pub fn new(paths: &AppPaths) -> Self {
        Self {
            home: paths.home.clone(),
            startup_cwd: paths.startup_cwd.clone(),
            save_root: paths.save_root.clone(),
            default_save_dir: paths.default_save_dir.clone(),
            input_root: paths.input_root.clone(),
        }
    }

    pub fn resolve_save_dir(&self, raw: Option<&str>) -> Result<PathBuf, PathError> {
        let candidate = match raw {
            Some(value) => resolve_under_root(value, &self.home, &self.save_root, "save_dir")?,
            None => self.default_save_dir.clone(),
        };
        ensure_within(&self.save_root, &candidate, "save_dir")?;
        Ok(candidate)
    }

    pub fn resolve_input_path(&self, raw: &str) -> Result<PathBuf, PathError> {
        let candidate = if let Some(root) = &self.input_root {
            let candidate = resolve_under_root(raw, &self.home, root, "input path")?;
            ensure_within(root, &candidate, "input path")?;
            candidate
        } else {
            resolve_relative(raw, &self.home, &self.startup_cwd, "input path")?
        };
        Ok(candidate)
    }

    pub fn input_root(&self) -> Option<&Path> {
        self.input_root.as_deref()
    }

    pub fn save_root(&self) -> &Path {
        &self.save_root
    }
}

pub(crate) fn create_directory_within(
    root: &Path,
    candidate: &Path,
    context: &'static str,
) -> Result<PathBuf, PathError> {
    ensure_within(root, candidate, context)?;
    let relative = candidate
        .strip_prefix(root)
        .map_err(|_| PathError::OutsideRoot { context })?;
    let capability = Dir::open_ambient_dir(root, ambient_authority()).map_err(|error| {
        PathError::Canonicalize {
            context,
            detail: error.to_string(),
        }
    })?;
    if !relative.as_os_str().is_empty() {
        capability
            .create_dir_all(relative)
            .map_err(|error| PathError::CreateDirectory {
                context,
                detail: error.to_string(),
            })?;
    }
    let canonical = fs::canonicalize(candidate).map_err(|error| PathError::Canonicalize {
        context,
        detail: error.to_string(),
    })?;
    ensure_within(root, &canonical, context)?;
    Ok(canonical)
}

fn resolve_root(raw: &str, home: &Path, context: &'static str) -> Result<PathBuf, PathError> {
    resolve_relative(raw, home, home, context)
}

fn resolve_under_root(
    raw: &str,
    home: &Path,
    root: &Path,
    context: &'static str,
) -> Result<PathBuf, PathError> {
    let candidate = resolve_relative(raw, home, root, context)?;
    anchor_within_root(root, &candidate, context)
}

fn anchor_within_root(
    root: &Path,
    candidate: &Path,
    context: &'static str,
) -> Result<PathBuf, PathError> {
    if fs::symlink_metadata(candidate).is_ok() {
        let canonical = fs::canonicalize(candidate).map_err(|error| PathError::Canonicalize {
            context,
            detail: error.to_string(),
        })?;
        ensure_within(root, &canonical, context)?;
        return Ok(canonical);
    }

    let mut ancestor = candidate;
    let mut missing = Vec::new();
    loop {
        if fs::symlink_metadata(ancestor).is_ok() {
            break;
        }
        let name = ancestor
            .file_name()
            .ok_or(PathError::OutsideRoot { context })?;
        missing.push(name.to_os_string());
        ancestor = ancestor
            .parent()
            .ok_or(PathError::OutsideRoot { context })?;
    }
    let mut anchored = fs::canonicalize(ancestor).map_err(|error| PathError::Canonicalize {
        context,
        detail: error.to_string(),
    })?;
    ensure_within(root, &anchored, context)?;
    for component in missing.into_iter().rev() {
        anchored.push(component);
    }
    ensure_within(root, &anchored, context)?;
    Ok(anchored)
}

fn resolve_relative(
    raw: &str,
    home: &Path,
    relative_base: &Path,
    context: &'static str,
) -> Result<PathBuf, PathError> {
    let expanded = if raw == "~" {
        home.to_path_buf()
    } else if let Some(remainder) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        home.join(remainder)
    } else if raw.starts_with('~') {
        return Err(PathError::UnsupportedTilde {
            raw: raw.to_owned(),
        });
    } else {
        let path = PathBuf::from(raw);
        if path.is_absolute() {
            path
        } else {
            relative_base.join(path)
        }
    };
    normalize_absolute(&expanded, context)
}

fn normalize_absolute(path: &Path, context: &'static str) -> Result<PathBuf, PathError> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(PathError::ParentTraversal { context });
                }
            }
            Component::Normal(value) => normalized.push(value),
        }
    }
    Ok(normalized)
}

fn ensure_within(root: &Path, candidate: &Path, context: &'static str) -> Result<(), PathError> {
    if path_starts_with(candidate, root)? {
        Ok(())
    } else {
        Err(PathError::OutsideRoot { context })
    }
}

#[cfg(not(windows))]
fn path_starts_with(candidate: &Path, root: &Path) -> Result<bool, PathError> {
    Ok(candidate.starts_with(root))
}

#[cfg(test)]
pub(crate) fn test_paths(
    root: &Path,
    environment: std::collections::BTreeMap<String, String>,
) -> AppPaths {
    let home = root.join("home");
    let startup_cwd = root.join("cwd");
    let data_local = root.join("data-local");
    let executable = root.join(if cfg!(windows) {
        "micu-image-mcp.exe"
    } else {
        "micu-image-mcp"
    });
    fs::create_dir_all(&home).unwrap_or_else(|error| panic!("{error}"));
    fs::create_dir_all(&startup_cwd).unwrap_or_else(|error| panic!("{error}"));
    fs::create_dir_all(&data_local).unwrap_or_else(|error| panic!("{error}"));
    fs::write(&executable, b"test binary").unwrap_or_else(|error| panic!("{error}"));
    AppPaths::resolve(
        &EnvironmentSnapshot::from_map(environment),
        PathSource::new(home, startup_cwd, executable, data_local),
    )
    .unwrap_or_else(|error| panic!("{error}"))
}

#[cfg(windows)]
fn path_starts_with(candidate: &Path, root: &Path) -> Result<bool, PathError> {
    use std::path::Component;

    fn component_text(component: Component<'_>) -> Option<&str> {
        match component {
            Component::Prefix(prefix) => prefix.as_os_str().to_str(),
            Component::RootDir | Component::CurDir | Component::ParentDir => None,
            Component::Normal(value) => value.to_str(),
        }
    }

    let candidate_components = candidate.components().collect::<Vec<_>>();
    let root_components = root.components().collect::<Vec<_>>();
    if root_components.len() > candidate_components.len() {
        return Ok(false);
    }
    for (candidate_component, root_component) in candidate_components.iter().zip(&root_components) {
        if candidate_component == root_component {
            continue;
        }
        let Some(candidate_text) = component_text(*candidate_component) else {
            return Ok(false);
        };
        let Some(root_text) = component_text(*root_component) else {
            return Ok(false);
        };
        if !candidate_text.eq_ignore_ascii_case(root_text) {
            return Ok(false);
        }
    }
    Ok(true)
}
