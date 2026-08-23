use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};

use crate::config::{AppPaths, PathPolicy, create_directory_within, is_windows_unc_root};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutputDirectory {
    pub relative: PathBuf,
    pub absolute: PathBuf,
}

#[derive(Clone)]
pub struct OutputSandbox {
    backend: SandboxBackend,
    root_path: Arc<PathBuf>,
    policy: PathPolicy,
}

#[derive(Clone)]
enum SandboxBackend {
    Capability(Arc<Dir>),
    #[cfg(windows)]
    WindowsUnc,
}

impl OutputSandbox {
    pub fn new(paths: &AppPaths) -> Result<Self, String> {
        let root_path = paths.save_root.clone();
        let backend = if is_windows_unc_root(&root_path) {
            #[cfg(windows)]
            {
                SandboxBackend::WindowsUnc
            }
            #[cfg(not(windows))]
            {
                unreachable!("UNC backend is Windows-only")
            }
        } else {
            SandboxBackend::Capability(Arc::new(
                Dir::open_ambient_dir(&root_path, ambient_authority()).map_err(|error| {
                    format!("无法打开 save root {}: {error}", root_path.display())
                })?,
            ))
        };
        Ok(Self {
            backend,
            root_path: Arc::new(root_path),
            policy: PathPolicy::new(paths),
        })
    }

    pub fn resolve(&self, save_dir: Option<&str>) -> Result<OutputDirectory, String> {
        let raw_for_error = save_dir.map(str::to_owned);
        let requested = self
            .policy
            .resolve_save_dir(save_dir)
            .map_err(|_| self.save_dir_error(raw_for_error.as_deref().unwrap_or_default()))?;
        let canonical = create_directory_within(self.root_path.as_ref(), &requested, "save_dir")
            .map_err(|_| self.save_dir_error(raw_for_error.as_deref().unwrap_or_default()))?;
        let canonical_relative = canonical
            .strip_prefix(self.root_path.as_ref())
            .map_err(|_| self.save_dir_error(raw_for_error.as_deref().unwrap_or_default()))?
            .to_path_buf();
        if !canonical_relative.as_os_str().is_empty() {
            match &self.backend {
                SandboxBackend::Capability(root) => {
                    root.open_dir(&canonical_relative).map_err(|_| {
                        self.save_dir_error(raw_for_error.as_deref().unwrap_or_default())
                    })?;
                }
                #[cfg(windows)]
                SandboxBackend::WindowsUnc => {
                    std::fs::read_dir(&canonical).map_err(|_| {
                        self.save_dir_error(raw_for_error.as_deref().unwrap_or_default())
                    })?;
                }
            }
        }
        Ok(OutputDirectory {
            relative: canonical_relative,
            absolute: canonical,
        })
    }

    pub(crate) fn create_temp(
        &self,
        location: &OutputDirectory,
    ) -> Result<(TempLease, std::fs::File), String> {
        static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);
        let epoch_nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        for _ in 0..1_000 {
            let counter = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let filename = format!(".micu-{}-{epoch_nanos}-{counter}.tmp", std::process::id());
            let relative = location.relative.join(filename);
            let opened = match &self.backend {
                SandboxBackend::Capability(root) => {
                    let mut options = OpenOptions::new();
                    options.read(true).write(true).create_new(true);
                    root.open_with(&relative, &options)
                        .map(cap_std::fs::File::into_std)
                }
                #[cfg(windows)]
                SandboxBackend::WindowsUnc => {
                    if let Err(error) = create_directory_within(
                        self.root_path.as_ref(),
                        &location.absolute,
                        "save_dir",
                    ) {
                        Err(std::io::Error::other(error.to_string()))
                    } else {
                        std::fs::OpenOptions::new()
                            .read(true)
                            .write(true)
                            .create_new(true)
                            .open(self.root_path.join(&relative))
                    }
                }
            };
            match opened {
                Ok(file) => {
                    return Ok((
                        TempLease {
                            cleanup: Arc::new(TempCleanup {
                                backend: self.backend.clone(),
                                #[cfg(windows)]
                                root_path: self.root_path.clone(),
                                relative,
                                removed: AtomicBool::new(false),
                            }),
                        },
                        file,
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(format!("无法创建输出临时文件: {error}")),
            }
        }
        Err("无法创建唯一输出临时文件".into())
    }

    pub(crate) fn commit(
        &self,
        lease: &mut TempLease,
        location: &OutputDirectory,
        basename: &str,
        extension: &str,
    ) -> Result<PathBuf, String> {
        let mut committed: Option<PathBuf> = None;
        for index in 1..=1_000 {
            let filename = if index == 1 {
                format!("{basename}.{extension}")
            } else {
                format!("{basename}_{index}.{extension}")
            };
            let candidate = location.relative.join(filename);
            let committed_result = match &self.backend {
                SandboxBackend::Capability(root) => {
                    root.hard_link(lease.relative(), root.as_ref(), &candidate)
                }
                #[cfg(windows)]
                SandboxBackend::WindowsUnc => {
                    if let Err(error) = create_directory_within(
                        self.root_path.as_ref(),
                        &location.absolute,
                        "save_dir",
                    ) {
                        Err(std::io::Error::other(error.to_string()))
                    } else {
                        std::fs::rename(
                            self.root_path.join(lease.relative()),
                            self.root_path.join(&candidate),
                        )
                    }
                }
            };
            match committed_result {
                Ok(()) => {
                    if self.uses_rename_commit() {
                        lease.mark_committed();
                    }
                    committed = Some(candidate);
                    break;
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::AlreadyExists
                        || self.root_path.join(&candidate).exists() =>
                {
                    continue;
                }
                Err(error) => return Err(format!("输出文件原子提交失败: {error}")),
            }
        }
        let relative = committed.ok_or_else(|| format!("basename 冲突过多：{basename}"))?;
        lease.remove_now();
        Ok(self.root_path.join(relative))
    }

    fn save_dir_error(&self, raw: &str) -> String {
        format!(
            "save_dir 必须在安全根目录 {} 之下；收到 {}。留空让 MCP 用默认目录，或先把 MICU_SAVE_DIR_ROOT 改到你想要的位置。",
            self.root_path.display(),
            python_string_repr(raw)
        )
    }

    fn uses_rename_commit(&self) -> bool {
        #[cfg(windows)]
        {
            matches!(&self.backend, SandboxBackend::WindowsUnc)
        }
        #[cfg(not(windows))]
        {
            false
        }
    }
}

#[derive(Clone)]
pub(crate) struct TempLease {
    cleanup: Arc<TempCleanup>,
}

impl TempLease {
    fn relative(&self) -> &Path {
        &self.cleanup.relative
    }

    fn remove_now(&mut self) {
        if self.cleanup.remove().is_ok() {
            self.cleanup.removed.store(true, Ordering::Release);
        }
    }

    fn mark_committed(&mut self) {
        self.cleanup.removed.store(true, Ordering::Release);
    }
}

struct TempCleanup {
    backend: SandboxBackend,
    #[cfg(windows)]
    root_path: Arc<PathBuf>,
    relative: PathBuf,
    removed: AtomicBool,
}

impl TempCleanup {
    fn remove(&self) -> std::io::Result<()> {
        match &self.backend {
            SandboxBackend::Capability(root) => root.remove_file(&self.relative),
            #[cfg(windows)]
            SandboxBackend::WindowsUnc => std::fs::remove_file(self.root_path.join(&self.relative)),
        }
    }
}

impl Drop for TempCleanup {
    fn drop(&mut self) {
        if !self.removed.load(Ordering::Acquire) && self.remove().is_ok() {
            self.removed.store(true, Ordering::Release);
        }
    }
}

fn python_string_repr(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('\'', "\\'");
    format!("'{escaped}'")
}
