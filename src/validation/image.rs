use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use cap_std::{ambient_authority, fs::Dir};
use image::{GenericImageView, ImageFormat, ImageReader, Limits};

use crate::config::Config;

pub const MAX_INPUT_FILE_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_TOTAL_INPUT_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_DECODED_IMAGE_PIXELS: u64 = 16 * 1024 * 1024;
pub const MAX_DECODED_IMAGE_EDGE: u32 = 8_192;
pub const MAX_DECODE_ALLOC_BYTES: u64 = 96 * 1024 * 1024;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodedImageInfo {
    pub format: ImageFormat,
    pub mime: &'static str,
    pub extension: &'static str,
    pub dimensions: (u32, u32),
    pub png_color_type: Option<u8>,
}

pub fn validate_input_image(
    config: &Config,
    path: &str,
    label: &str,
) -> Result<ValidatedImage, String> {
    let requested = expand_input_path(path)?;
    let (resolved_path, file) = open_input_file(config, &requested, path, label)?;
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
        .map(|name| name.to_string_lossy().into_owned())
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

pub fn inspect_image_file(
    file: &File,
    size_bytes: u64,
    label: &str,
) -> Result<DecodedImageInfo, String> {
    let header = read_header(file).map_err(|error| format!("{label} 读取失败: {error}"))?;
    validate_magic(&header, size_bytes, label)?;
    if let Some((width, height)) = png_dimensions(&header) {
        validate_decode_dimensions(width, height, label)?;
    }
    let (format, dimensions) = decode_image(file, label)?;
    let (mime, extension) = match format {
        ImageFormat::Png => ("image/png", "png"),
        ImageFormat::Jpeg => ("image/jpeg", "jpg"),
        ImageFormat::WebP => ("image/webp", "webp"),
        ImageFormat::Gif => ("image/gif", "gif"),
        _ => {
            return Err(format!(
                "{label} 不是受支持的图片格式（PNG/JPEG/WebP/GIF magic 不匹配）"
            ));
        }
    };
    Ok(DecodedImageInfo {
        format,
        mime,
        extension,
        dimensions,
        png_color_type: if format == ImageFormat::Png {
            header.get(25).copied()
        } else {
            None
        },
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

fn expand_input_path(raw: &str) -> Result<PathBuf, String> {
    let path = if raw == "~" {
        dirs::home_dir().ok_or_else(|| "无法确定用户 home 目录".to_owned())?
    } else if let Some(remainder) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        dirs::home_dir()
            .ok_or_else(|| "无法确定用户 home 目录".to_owned())?
            .join(remainder)
    } else {
        PathBuf::from(raw)
    };
    if path.is_absolute() {
        Ok(path)
    } else {
        std::env::current_dir()
            .map(|current| current.join(path))
            .map_err(|error| format!("无法解析相对输入路径: {error}"))
    }
}

fn open_input_file(
    config: &Config,
    requested: &Path,
    raw: &str,
    label: &str,
) -> Result<(PathBuf, File), String> {
    if let Some(root) = &config.input_root {
        let canonical_root = std::fs::canonicalize(root)
            .map_err(|error| format!("无法打开 MICU_INPUT_ROOT={}: {error}", root.display()))?;
        let canonical_target = std::fs::canonicalize(requested)
            .map_err(|_| format!("{label} 不存在: {}", requested.display()))?;
        let relative = canonical_target
            .strip_prefix(&canonical_root)
            .map_err(|_| {
                format!(
                    "{label} 必须在 MICU_INPUT_ROOT={} 之下（已启用输入路径白名单）；收到 {}",
                    canonical_root.display(),
                    python_string_repr(raw)
                )
            })?;
        let root_dir =
            Dir::open_ambient_dir(&canonical_root, ambient_authority()).map_err(|error| {
                format!(
                    "无法打开 MICU_INPUT_ROOT={}: {error}",
                    canonical_root.display()
                )
            })?;
        let file = root_dir.open(relative).map_err(|_| {
            format!(
                "{label} 必须在 MICU_INPUT_ROOT={} 之下（已启用输入路径白名单）；收到 {}",
                canonical_root.display(),
                python_string_repr(raw)
            )
        })?;
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

fn read_header(file: &File) -> std::io::Result<Vec<u8>> {
    let mut clone = file.try_clone()?;
    clone.seek(SeekFrom::Start(0))?;
    let mut header = vec![0_u8; 32];
    let read = clone.read(&mut header)?;
    header.truncate(read);
    Ok(header)
}

fn validate_magic(header: &[u8], size_bytes: u64, label: &str) -> Result<(), String> {
    if size_bytes < 16 || header.len() < 16 {
        return Err(format!("{label} 太小（{size_bytes} 字节），不像合法图片"));
    }
    let png = header.starts_with(b"\x89PNG\r\n\x1a\n");
    let jpeg = header.starts_with(b"\xff\xd8\xff");
    let webp = header.starts_with(b"RIFF") && header.get(8..12) == Some(b"WEBP");
    let gif = header.starts_with(b"GIF87a") || header.starts_with(b"GIF89a");
    if png || jpeg || webp || gif {
        Ok(())
    } else {
        Err(format!(
            "{label} 不是受支持的图片格式（PNG/JPEG/WebP/GIF magic 不匹配）"
        ))
    }
}

fn png_dimensions(header: &[u8]) -> Option<(u32, u32)> {
    if header.len() >= 24
        && header.starts_with(b"\x89PNG\r\n\x1a\n")
        && header.get(12..16) == Some(b"IHDR")
    {
        let width = u32::from_be_bytes(header.get(16..20)?.try_into().ok()?);
        let height = u32::from_be_bytes(header.get(20..24)?.try_into().ok()?);
        return Some((width, height));
    }
    None
}

fn decoder_limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DECODED_IMAGE_EDGE);
    limits.max_image_height = Some(MAX_DECODED_IMAGE_EDGE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC_BYTES);
    limits
}

fn reader_for(file: &File) -> Result<ImageReader<BufReader<File>>, String> {
    let mut clone = file
        .try_clone()
        .map_err(|error| format!("无法复制图片 file handle: {error}"))?;
    clone
        .seek(SeekFrom::Start(0))
        .map_err(|error| format!("无法重置图片 file handle: {error}"))?;
    let mut reader = ImageReader::new(BufReader::new(clone))
        .with_guessed_format()
        .map_err(|error| format!("无法识别图片格式: {error}"))?;
    reader.limits(decoder_limits());
    Ok(reader)
}

fn decode_image(file: &File, label: &str) -> Result<(ImageFormat, (u32, u32)), String> {
    let dimensions_reader = reader_for(file)
        .map_err(|_| format!("{label} 无法完整解码（文件可能截断、损坏或格式标记错误）"))?;
    let format = dimensions_reader
        .format()
        .ok_or_else(|| format!("{label} 无法完整解码（文件可能截断、损坏或格式标记错误）"))?;
    let dimensions = dimensions_reader
        .into_dimensions()
        .map_err(|_| format!("{label} 无法完整解码（文件可能截断、损坏或格式标记错误）"))?;
    validate_decode_dimensions(dimensions.0, dimensions.1, label)?;

    let decoded = reader_for(file)
        .and_then(|reader| {
            reader
                .decode()
                .map_err(|error| format!("图片 decoder 错误: {error}"))
        })
        .map_err(|_| format!("{label} 无法完整解码（文件可能截断、损坏或格式标记错误）"))?;
    if decoded.dimensions() != dimensions {
        return Err(format!(
            "{label} 无法完整解码（文件尺寸在解码期间发生变化）"
        ));
    }
    Ok((format, dimensions))
}

fn validate_decode_dimensions(width: u32, height: u32, label: &str) -> Result<(), String> {
    if width > MAX_DECODED_IMAGE_EDGE || height > MAX_DECODED_IMAGE_EDGE {
        return Err(format!(
            "{label} 尺寸 {width}x{height} 的边长超过解码上限 {MAX_DECODED_IMAGE_EDGE}，已拒绝（防解压炸弹）"
        ));
    }
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if pixels > MAX_DECODED_IMAGE_PIXELS {
        return Err(format!(
            "{label} 尺寸 {width}x{height} 的总像素超过解码上限 {MAX_DECODED_IMAGE_PIXELS}，已拒绝（防解压炸弹）"
        ));
    }
    Ok(())
}

fn format_name(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "PNG",
        ImageFormat::Jpeg => "JPEG",
        ImageFormat::WebP => "WEBP",
        ImageFormat::Gif => "GIF",
        _ => "未知",
    }
}

fn python_string_repr(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('\'', "\\'");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs};

    use image::{ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};

    use super::*;

    fn config(root: &std::path::Path) -> Config {
        Config::from_map(&BTreeMap::from([
            (
                "HOME".into(),
                root.join("home").to_string_lossy().into_owned(),
            ),
            (
                "MICU_SAVE_DIR_ROOT".into(),
                root.join("out").to_string_lossy().into_owned(),
            ),
            (
                "MICU_INPUT_ROOT".into(),
                root.join("input").to_string_lossy().into_owned(),
            ),
        ]))
        .unwrap_or_else(|error| panic!("{error}"))
    }

    fn save_rgb(path: &std::path::Path, format: ImageFormat, width: u32, height: u32) {
        let image = RgbImage::from_pixel(width, height, Rgb([10, 20, 30]));
        image
            .save_with_format(path, format)
            .unwrap_or_else(|error| panic!("{error}"));
    }

    #[test]
    fn valid_images_follow_magic_not_extension_and_are_fully_decoded() {
        let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
        let input = temp.path().join("input");
        fs::create_dir_all(&input).unwrap_or_else(|error| panic!("{error}"));
        let path = input.join("actually-jpeg.png");
        save_rgb(&path, ImageFormat::Jpeg, 64, 48);
        let validated =
            validate_input_image(&config(temp.path()), &path.to_string_lossy(), "image_path")
                .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(validated.format, ImageFormat::Jpeg);
        assert_eq!(validated.mime, "image/jpeg");
        assert_eq!(validated.dimensions, (64, 48));
    }

    #[test]
    fn truncated_spoofed_and_oversized_inputs_are_rejected_before_upload() {
        let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
        let input = temp.path().join("input");
        fs::create_dir_all(&input).unwrap_or_else(|error| panic!("{error}"));
        let valid = input.join("valid.png");
        save_rgb(&valid, ImageFormat::Png, 64, 48);
        let mut bytes = fs::read(&valid).unwrap_or_else(|error| panic!("{error}"));
        bytes.truncate(40);
        let truncated = input.join("truncated.png");
        fs::write(&truncated, bytes).unwrap_or_else(|error| panic!("{error}"));
        assert!(
            validate_input_image(
                &config(temp.path()),
                &truncated.to_string_lossy(),
                "image_path"
            )
            .is_err_and(|error| error.contains("完整解码"))
        );

        let spoofed = input.join("spoofed.png");
        fs::write(&spoofed, b"not an image but longer than sixteen bytes")
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(
            validate_input_image(
                &config(temp.path()),
                &spoofed.to_string_lossy(),
                "image_path"
            )
            .is_err_and(|error| error.contains("magic 不匹配"))
        );

        let oversized = input.join("oversized.png");
        let file = File::create(&oversized).unwrap_or_else(|error| panic!("{error}"));
        file.set_len(MAX_INPUT_FILE_BYTES + 1)
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(
            validate_input_image(
                &config(temp.path()),
                &oversized.to_string_lossy(),
                "image_path"
            )
            .is_err_and(|error| error.contains("超过单文件上限"))
        );
    }

    #[test]
    fn decoder_limits_reject_declared_bombs_before_allocating() {
        let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
        let input = temp.path().join("input");
        fs::create_dir_all(&input).unwrap_or_else(|error| panic!("{error}"));
        let path = input.join("bomb.png");
        save_rgb(&path, ImageFormat::Png, 32, 24);
        let mut bytes = fs::read(&path).unwrap_or_else(|error| panic!("{error}"));
        bytes[16..20].copy_from_slice(&100_000_u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&100_000_u32.to_be_bytes());
        fs::write(&path, bytes).unwrap_or_else(|error| panic!("{error}"));
        assert!(
            validate_input_image(&config(temp.path()), &path.to_string_lossy(), "image_path")
                .is_err_and(|error| error.contains("防解压炸弹"))
        );
    }

    #[test]
    fn mask_requires_png_matching_dimensions_and_alpha() {
        let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
        let input = temp.path().join("input");
        fs::create_dir_all(&input).unwrap_or_else(|error| panic!("{error}"));
        let source_path = input.join("source.png");
        let rgb_mask_path = input.join("rgb-mask.png");
        let rgba_mask_path = input.join("rgba-mask.png");
        save_rgb(&source_path, ImageFormat::Png, 64, 48);
        save_rgb(&rgb_mask_path, ImageFormat::Png, 64, 48);
        RgbaImage::from_pixel(64, 48, Rgba([0, 0, 0, 0]))
            .save_with_format(&rgba_mask_path, ImageFormat::Png)
            .unwrap_or_else(|error| panic!("{error}"));
        let cfg = config(temp.path());
        let source = validate_input_image(&cfg, &source_path.to_string_lossy(), "image_path")
            .unwrap_or_else(|error| panic!("{error}"));
        let rgb_mask = validate_input_image(&cfg, &rgb_mask_path.to_string_lossy(), "mask_path")
            .unwrap_or_else(|error| panic!("{error}"));
        let rgba_mask = validate_input_image(&cfg, &rgba_mask_path.to_string_lossy(), "mask_path")
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(validate_mask(&rgb_mask, &source).is_err_and(|error| error.contains("alpha")));
        assert_eq!(validate_mask(&rgba_mask, &source), Ok(()));
    }

    #[cfg(unix)]
    #[test]
    fn input_root_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
        let input = temp.path().join("input");
        fs::create_dir_all(&input).unwrap_or_else(|error| panic!("{error}"));
        let outside = temp.path().join("outside.png");
        save_rgb(&outside, ImageFormat::Png, 64, 48);
        let link = input.join("escape.png");
        symlink(&outside, &link).unwrap_or_else(|error| panic!("{error}"));
        assert!(
            validate_input_image(&config(temp.path()), &link.to_string_lossy(), "image_path")
                .is_err_and(|error| error.contains("MICU_INPUT_ROOT"))
        );
    }
}
