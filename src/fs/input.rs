use std::{
    fs::File,
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use cap_std::{ambient_authority, fs::Dir};
use image::ImageFormat;

use crate::config::{AppPaths, PathError, PathPolicy};

use super::image::{format_name, inspect_image_file};

pub const MAX_INPUT_FILE_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_TOTAL_INPUT_BYTES: u64 = 8 * 1024 * 1024;

pub struct ValidatedImage {
    pub path: PathBuf,
    pub filename: String,
    pub file: Arc<tempfile::NamedTempFile>,
    pub size_bytes: u64,
    pub format: ImageFormat,
    pub mime: &'static str,
    pub dimensions: (u32, u32),
    pub png_color_type: Option<u8>,
}

#[derive(Clone)]
pub struct InputStore {
    policy: PathPolicy,
}

impl InputStore {
    pub fn new(paths: &AppPaths) -> Self {
        Self {
            policy: PathPolicy::new(paths),
        }
    }

    pub fn validate_image(&self, path: &str, label: &str) -> Result<ValidatedImage, String> {
        validate_input_image(&self.policy, path, label)
    }
}

pub fn validate_input_image(
    policy: &PathPolicy,
    path: &str,
    label: &str,
) -> Result<ValidatedImage, String> {
    let requested = policy.resolve_input_path(path).map_err(|error| {
        if matches!(error, PathError::OutsideRoot { .. }) && !Path::new(path).exists() {
            format!("{label} 不存在: {path}")
        } else if let Some(root) = policy.input_root() {
            format!(
                "{label} 必须在 MICU_INPUT_ROOT={} 之下（已启用输入路径白名单）；收到 {}",
                root.display(),
                python_string_repr(path)
            )
        } else {
            error.to_string()
        }
    })?;
    let (resolved_path, file) = open_input_file(policy, &requested, path, label)?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("{label} 无法 stat: {error}"))?;
    if !metadata.is_file() {
        return Err(format!("{label} 不存在: {}", requested.display()));
    }
    let size_bytes = metadata.len();
    if size_bytes > MAX_INPUT_FILE_BYTES {
        return Err(format!(
            "{label} 文件 {:.1}MB 超过单文件上限 {}MB；请先压缩",
            size_bytes as f64 / 1024.0 / 1024.0,
            MAX_INPUT_FILE_BYTES / 1024 / 1024
        ));
    }
    let mut snapshot = tempfile::NamedTempFile::new()
        .map_err(|error| format!("{label} 无法创建安全输入快照: {error}"))?;
    let mut source = file
        .try_clone()
        .map_err(|error| format!("{label} 无法复制输入 file handle: {error}"))?;
    source
        .seek(SeekFrom::Start(0))
        .map_err(|error| format!("{label} 无法重置输入 file handle: {error}"))?;
    std::io::copy(&mut source, snapshot.as_file_mut())
        .map_err(|error| format!("{label} 安全输入快照写入失败: {error}"))?;
    snapshot
        .as_file_mut()
        .flush()
        .map_err(|error| format!("{label} 安全输入快照 flush 失败: {error}"))?;
    let info = inspect_image_file(snapshot.as_file(), size_bytes, label)?;
    if !matches!(
        info.format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP
    ) {
        return Err(format!(
            "{label} 格式 {} 不受上游支持；请转换为 PNG、JPEG 或 WebP",
            format_name(info.format)
        ));
    }
    if info.dimensions.0 < 16 || info.dimensions.1 < 16 {
        return Err(format!(
            "{label} 尺寸 {}x{} 太小，不像正常图片",
            info.dimensions.0, info.dimensions.1
        ));
    }
    let filename = resolved_path
        .file_name()
        .map(|name| {
            name.to_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{label} 文件名不是合法 Unicode，无法无损上传"))
        })
        .transpose()?
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "image".to_owned());
    Ok(ValidatedImage {
        path: resolved_path,
        filename,
        file: Arc::new(snapshot),
        size_bytes,
        format: info.format,
        mime: info.mime,
        dimensions: info.dimensions,
        png_color_type: info.png_color_type,
    })
}

pub fn validate_mask(mask: &ValidatedImage, image: &ValidatedImage) -> Result<(), String> {
    if mask.format != ImageFormat::Png {
        return Err("mask_path 必须是 PNG（OpenAI 规范要求 alpha 通道）".into());
    }
    if mask.dimensions != image.dimensions {
        return Err(format!(
            "mask 尺寸 {}x{} 必须与原图 {}x{} 一致",
            mask.dimensions.0, mask.dimensions.1, image.dimensions.0, image.dimensions.1
        ));
    }
    if !matches!(mask.png_color_type, Some(4 | 6)) {
        let color_type = mask.png_color_type;
        let description = match color_type {
            Some(0) => "灰度".to_owned(),
            Some(2) => "RGB".to_owned(),
            Some(3) => "调色板".to_owned(),
            Some(value) => format!("未知 ({value})"),
            None => "未知 (None)".to_owned(),
        };
        let rendered = color_type.map_or_else(|| "None".to_owned(), |value| value.to_string());
        return Err(format!(
            "mask PNG color_type={rendered}（{description}），缺 alpha 通道；必须用 GA(4) 或 RGBA(6) 格式，alpha=0 标记编辑区"
        ));
    }
    Ok(())
}

fn open_input_file(
    policy: &PathPolicy,
    requested: &Path,
    raw: &str,
    label: &str,
) -> Result<(PathBuf, File), String> {
    if let Some(root) = policy.input_root() {
        let canonical_root = root.to_path_buf();
        let canonical_target = std::fs::canonicalize(requested)
            .map_err(|_| format!("{label} 不存在: {}", requested.display()))?;
        let relative = canonical_target
            .strip_prefix(&canonical_root)
            .map_err(|_| input_root_error(label, &canonical_root, raw))?;
        let root_dir =
            Dir::open_ambient_dir(&canonical_root, ambient_authority()).map_err(|error| {
                format!(
                    "无法打开 MICU_INPUT_ROOT={}: {error}",
                    canonical_root.display()
                )
            })?;
        let file = root_dir
            .open(relative)
            .map_err(|_| input_root_error(label, &canonical_root, raw))?;
        return Ok((canonical_target, file.into_std()));
    }

    let file = File::open(requested).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            format!("{label} 不存在: {}", requested.display())
        } else {
            format!("{label} 读取失败: {error}")
        }
    })?;
    Ok((requested.to_path_buf(), file))
}

fn input_root_error(label: &str, root: &Path, raw: &str) -> String {
    format!(
        "{label} 必须在 MICU_INPUT_ROOT={} 之下（已启用输入路径白名单）；收到 {}",
        root.display(),
        python_string_repr(raw)
    )
}

fn python_string_repr(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('\'', "\\'");
    format!("'{escaped}'")
}
