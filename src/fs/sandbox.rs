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

use crate::config::{AppPaths, PathPolicy, create_directory_within};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutputDirectory {
    pub relative: PathBuf,
    pub absolute: PathBuf,
}

#[derive(Clone)]
pub struct OutputSandbox {
    root: Arc<Dir>,
    root_path: Arc<PathBuf>,
    policy: PathPolicy,
}

impl OutputSandbox {
    pub fn new(paths: &AppPaths) -> Result<Self, String> {
        let root_path = paths.save_root.clone();
        let root = Dir::open_ambient_dir(&root_path, ambient_authority())
            .map_err(|error| format!("无法打开 save root {}: {error}", root_path.display()))?;
        Ok(Self {
            root: Arc::new(root),
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
            self.root
                .open_dir(&canonical_relative)
                .map_err(|_| self.save_dir_error(raw_for_error.as_deref().unwrap_or_default()))?;
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
            let mut options = OpenOptions::new();
            options.read(true).write(true).create_new(true);
            match self.root.open_with(&relative, &options) {
                Ok(file) => {
                    return Ok((
                        TempLease {
                            cleanup: Arc::new(TempCleanup {
                                root: self.root.clone(),
                                relative,
                                removed: AtomicBool::new(false),
                            }),
                        },
                        file.into_std(),
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
            match self
                .root
                .hard_link(lease.relative(), self.root.as_ref(), &candidate)
            {
                Ok(()) => {
                    committed = Some(candidate);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
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
        if self
            .cleanup
            .root
            .remove_file(&self.cleanup.relative)
            .is_ok()
        {
            self.cleanup.removed.store(true, Ordering::Release);
        }
    }
}

struct TempCleanup {
    root: Arc<Dir>,
    relative: PathBuf,
    removed: AtomicBool,
}

impl Drop for TempCleanup {
    fn drop(&mut self) {
        if !self.removed.load(Ordering::Acquire) && self.root.remove_file(&self.relative).is_ok() {
            self.removed.store(true, Ordering::Release);
        }
    }
}

fn python_string_repr(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('\'', "\\'");
    format!("'{escaped}'")
}
