use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use super::InstallError;

pub fn install_binary(source: &Path, destination: &Path) -> Result<PathBuf, InstallError> {
    if !source.is_file() {
        return Err(InstallError::BinarySource("source 不存在或不是文件".into()));
    }
    if same_file(source, destination)? {
        make_executable(destination)?;
        return Ok(destination.to_path_buf());
    }
    if destination.is_file() && files_equal(source, destination)? {
        make_executable(destination)?;
        return Ok(destination.to_path_buf());
    }

    let parent = destination
        .parent()
        .ok_or_else(|| InstallError::BinaryDirectory("目标没有 parent".into()))?;
    fs::create_dir_all(parent).map_err(|error| InstallError::BinaryDirectory(error.to_string()))?;
    make_private_directory(parent)?;
    let mut temp = tempfile::Builder::new()
        .prefix(".micu-binary-")
        .tempfile_in(parent)
        .map_err(|error| InstallError::BinaryCopy(error.to_string()))?;
    let mut input =
        File::open(source).map_err(|error| InstallError::BinarySource(error.to_string()))?;
    std::io::copy(&mut input, temp.as_file_mut())
        .map_err(|error| InstallError::BinaryCopy(error.to_string()))?;
    temp.as_file_mut()
        .flush()
        .map_err(|error| InstallError::BinaryCopy(error.to_string()))?;
    temp.as_file_mut()
        .sync_all()
        .map_err(|error| InstallError::BinaryCopy(error.to_string()))?;
    make_executable(temp.path())?;
    temp.persist(destination)
        .map_err(|error| InstallError::BinaryReplace(error.error.to_string()))?;
    sync_parent(parent)?;
    Ok(destination.to_path_buf())
}

fn same_file(left: &Path, right: &Path) -> Result<bool, InstallError> {
    if left == right {
        return Ok(true);
    }
    if !right.exists() {
        return Ok(false);
    }
    let left =
        fs::canonicalize(left).map_err(|error| InstallError::BinarySource(error.to_string()))?;
    let right =
        fs::canonicalize(right).map_err(|error| InstallError::BinarySource(error.to_string()))?;
    Ok(left == right)
}

fn files_equal(left: &Path, right: &Path) -> Result<bool, InstallError> {
    let left_metadata =
        fs::metadata(left).map_err(|error| InstallError::BinarySource(error.to_string()))?;
    let right_metadata =
        fs::metadata(right).map_err(|error| InstallError::BinarySource(error.to_string()))?;
    if left_metadata.len() != right_metadata.len() {
        return Ok(false);
    }
    let mut left_file =
        File::open(left).map_err(|error| InstallError::BinarySource(error.to_string()))?;
    let mut right_file =
        File::open(right).map_err(|error| InstallError::BinarySource(error.to_string()))?;
    left_file
        .seek(SeekFrom::Start(0))
        .map_err(|error| InstallError::BinarySource(error.to_string()))?;
    right_file
        .seek(SeekFrom::Start(0))
        .map_err(|error| InstallError::BinarySource(error.to_string()))?;
    let mut left_buffer = [0_u8; 16 * 1024];
    let mut right_buffer = [0_u8; 16 * 1024];
    loop {
        let left_read = left_file
            .read(&mut left_buffer)
            .map_err(|error| InstallError::BinarySource(error.to_string()))?;
        let right_read = right_file
            .read(&mut right_buffer)
            .map_err(|error| InstallError::BinarySource(error.to_string()))?;
        if left_read != right_read || left_buffer[..left_read] != right_buffer[..right_read] {
            return Ok(false);
        }
        if left_read == 0 {
            return Ok(true);
        }
    }
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), InstallError> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)
        .map_err(|error| InstallError::BinaryCopy(error.to_string()))?
        .permissions();
    permissions.set_mode(permissions.mode() | 0o700);
    fs::set_permissions(path, permissions)
        .map_err(|error| InstallError::BinaryCopy(error.to_string()))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<(), InstallError> {
    Ok(())
}

#[cfg(unix)]
fn make_private_directory(path: &Path) -> Result<(), InstallError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| InstallError::BinaryDirectory(error.to_string()))
}

#[cfg(not(unix))]
fn make_private_directory(_path: &Path) -> Result<(), InstallError> {
    Ok(())
}

#[cfg(unix)]
fn sync_parent(parent: &Path) -> Result<(), InstallError> {
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| InstallError::BinaryReplace(error.to_string()))
}

#[cfg(not(unix))]
fn sync_parent(_parent: &Path) -> Result<(), InstallError> {
    Ok(())
}
