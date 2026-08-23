use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use super::InstallError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WriteReport {
    pub path: PathBuf,
    pub backup: Option<PathBuf>,
}

pub fn replace_verified<F>(
    path: &Path,
    bytes: &[u8],
    verify: F,
) -> Result<WriteReport, InstallError>
where
    F: FnOnce(&str) -> Result<(), InstallError>,
{
    let parent = path.parent().ok_or_else(|| InstallError::ConfigIo {
        action: "定位 parent",
        detail: "配置路径没有 parent".into(),
    })?;
    fs::create_dir_all(parent).map_err(|error| InstallError::ConfigIo {
        action: "创建目录",
        detail: error.to_string(),
    })?;
    let mut temp = tempfile::Builder::new()
        .prefix(".micu-config-")
        .tempfile_in(parent)
        .map_err(|error| InstallError::ConfigIo {
            action: "创建临时文件",
            detail: error.to_string(),
        })?;
    temp.write_all(bytes)
        .map_err(|error| InstallError::ConfigIo {
            action: "写临时文件",
            detail: error.to_string(),
        })?;
    temp.as_file_mut()
        .flush()
        .map_err(|error| InstallError::ConfigIo {
            action: "flush 临时文件",
            detail: error.to_string(),
        })?;
    temp.as_file_mut()
        .sync_all()
        .map_err(|error| InstallError::ConfigIo {
            action: "sync 临时文件",
            detail: error.to_string(),
        })?;
    make_private_file(temp.path())?;
    let written = fs::read_to_string(temp.path()).map_err(|error| InstallError::ConfigIo {
        action: "重读临时文件",
        detail: error.to_string(),
    })?;
    if let Err(error) = verify(&written) {
        return Err(InstallError::VerificationFailed {
            detail: error.to_string(),
            target: path.to_path_buf(),
            temp: temp.path().to_path_buf(),
            backup: None,
        });
    }

    let backup = create_backup(path)?;
    temp.persist(path)
        .map_err(|error| InstallError::ConfigReplace {
            detail: error.error.to_string(),
            target: path.to_path_buf(),
            backup: backup.clone(),
        })?;
    make_private_file(path)?;
    sync_parent(parent)?;
    Ok(WriteReport {
        path: path.to_path_buf(),
        backup,
    })
}

fn create_backup(path: &Path) -> Result<Option<PathBuf>, InstallError> {
    if !path.exists() {
        return Ok(None);
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let filename = path
        .file_name()
        .ok_or_else(|| InstallError::ConfigIo {
            action: "生成备份名",
            detail: "配置路径没有文件名".into(),
        })?
        .to_str()
        .ok_or(InstallError::NonUnicodePath {
            context: "config filename",
        })?;
    for counter in 0_u16..1_000 {
        let backup = path.with_file_name(format!("{filename}.bak.{timestamp}.{counter}"));
        match copy_new(path, &backup) {
            Ok(()) => {
                make_private_file(&backup)?;
                return Ok(Some(backup));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(InstallError::ConfigIo {
                    action: "创建备份",
                    detail: error.to_string(),
                });
            }
        }
    }
    Err(InstallError::ConfigIo {
        action: "创建备份",
        detail: "备份名冲突过多".into(),
    })
}

fn copy_new(source: &Path, destination: &Path) -> std::io::Result<()> {
    let mut source = File::open(source)?;
    let mut destination = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    std::io::copy(&mut source, &mut destination)?;
    destination.flush()?;
    destination.sync_all()
}

#[cfg(unix)]
fn make_private_file(path: &Path) -> Result<(), InstallError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| {
        InstallError::ConfigIo {
            action: "设置 0600 权限",
            detail: error.to_string(),
        }
    })
}

#[cfg(not(unix))]
fn make_private_file(_path: &Path) -> Result<(), InstallError> {
    Ok(())
}

#[cfg(unix)]
fn sync_parent(parent: &Path) -> Result<(), InstallError> {
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| InstallError::ConfigIo {
            action: "sync 配置目录",
            detail: error.to_string(),
        })
}

#[cfg(not(unix))]
fn sync_parent(_parent: &Path) -> Result<(), InstallError> {
    Ok(())
}
