use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
};

use image::{GenericImageView, ImageFormat, ImageReader, Limits};

pub const MAX_DECODED_IMAGE_PIXELS: u64 = 16 * 1024 * 1024;
pub const MAX_DECODED_IMAGE_EDGE: u32 = 8_192;
pub const MAX_DECODE_ALLOC_BYTES: u64 = 96 * 1024 * 1024;
pub const MAX_GIF_FRAMES: u32 = 1_024;
pub const MAX_GIF_COMPOSITED_PIXELS: u64 = 1 << 30;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodedImageInfo {
    pub format: ImageFormat,
    pub format_id: &'static str,
    pub frame_count: Option<u32>,
    pub mime: &'static str,
    pub extension: &'static str,
    pub dimensions: (u32, u32),
    pub png_color_type: Option<u8>,
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
    let (format_id, mime, extension) = match format {
        ImageFormat::Png => ("png", "image/png", "png"),
        ImageFormat::Jpeg => ("jpeg", "image/jpeg", "jpg"),
        ImageFormat::WebP => ("webp", "image/webp", "webp"),
        ImageFormat::Gif => ("gif", "image/gif", "gif"),
        _ => {
            return Err(format!(
                "{label} 不是受支持的图片格式（PNG/JPEG/WebP/GIF magic 不匹配）"
            ));
        }
    };
    let frame_count = (format == ImageFormat::Gif)
        .then(|| count_gif_frames(file, dimensions, label))
        .transpose()?;
    Ok(DecodedImageInfo {
        format,
        format_id,
        frame_count,
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

/// Lets input validation refuse GIF, which it never accepts, before decoding any frame.
pub fn has_gif_header(file: &File) -> bool {
    read_header(file).is_ok_and(|header| is_gif_header(&header))
}

fn is_gif_header(header: &[u8]) -> bool {
    header.starts_with(b"GIF87a") || header.starts_with(b"GIF89a")
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
    if png || jpeg || webp || is_gif_header(header) {
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

/// `decode()` stops at the first GIF frame, so walk the rest under a pixel budget.
/// Frames must fit the canvas, which makes canvas pixels an upper bound per frame.
fn count_gif_frames(file: &File, dimensions: (u32, u32), label: &str) -> Result<u32, String> {
    let corrupt = || format!("{label} 无法完整解码（文件可能截断、损坏或格式标记错误）");
    let mut clone = file.try_clone().map_err(|_| corrupt())?;
    clone.seek(SeekFrom::Start(0)).map_err(|_| corrupt())?;
    let mut options = gif::DecodeOptions::new();
    options.check_frame_consistency(true);
    let mut decoder = options
        .read_info(BufReader::new(clone))
        .map_err(|_| corrupt())?;
    let mut frames = 0_u32;
    while decoder.read_next_frame().map_err(|_| corrupt())?.is_some() {
        frames += 1;
        if gif_budget_exceeded(frames, dimensions) {
            return Err(format!(
                "{label} 的 GIF 超过 {MAX_GIF_FRAMES} 帧或 {MAX_GIF_COMPOSITED_PIXELS} 合成像素上限，已拒绝（防解压炸弹）"
            ));
        }
    }
    Ok(frames)
}

fn gif_budget_exceeded(frames: u32, (width, height): (u32, u32)) -> bool {
    let composited = u64::from(frames)
        .saturating_mul(u64::from(width))
        .saturating_mul(u64::from(height));
    frames > MAX_GIF_FRAMES || composited > MAX_GIF_COMPOSITED_PIXELS
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

pub(crate) fn format_name(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "PNG",
        ImageFormat::Jpeg => "JPEG",
        ImageFormat::WebP => "WEBP",
        ImageFormat::Gif => "GIF",
        _ => "未知",
    }
}

#[cfg(test)]
mod tests {
    use std::{borrow::Cow, collections::BTreeMap, fs};

    use image::{
        Delay, Frame, ImageFormat, Rgb, RgbImage, Rgba, RgbaImage, codecs::gif::GifEncoder,
    };

    use crate::{
        config::{PathPolicy, test_paths},
        fs::input::{MAX_INPUT_FILE_BYTES, validate_input_image, validate_mask},
    };

    use super::*;

    fn config(root: &std::path::Path) -> PathPolicy {
        let paths = test_paths(
            root,
            BTreeMap::from([
                (
                    "MICU_SAVE_DIR_ROOT".into(),
                    root.join("out").to_string_lossy().into_owned(),
                ),
                (
                    "MICU_INPUT_ROOT".into(),
                    root.join("input").to_string_lossy().into_owned(),
                ),
            ]),
        );
        PathPolicy::new(&paths)
    }

    fn save_rgb(path: &std::path::Path, format: ImageFormat, width: u32, height: u32) {
        let image = RgbImage::from_pixel(width, height, Rgb([10, 20, 30]));
        image
            .save_with_format(path, format)
            .unwrap_or_else(|error| panic!("{error}"));
    }

    fn gif_bytes(frames: u32) -> Vec<u8> {
        let mut encoded = Vec::new();
        let mut encoder = GifEncoder::new(&mut encoded);
        for index in 0..frames {
            let pixel = Rgba([(index % 251) as u8, 90, 160, 255]);
            encoder
                .encode_frame(Frame::from_parts(
                    RgbaImage::from_pixel(16, 16, pixel),
                    0,
                    0,
                    Delay::from_numer_denom_ms(100, 1),
                ))
                .unwrap();
        }
        drop(encoder);
        encoded
    }

    /// One indexed frame per `(left, width)`, drawn on a 16×16 canvas.
    fn gif_with_frames(frames: &[(u16, u16)]) -> Vec<u8> {
        let mut encoded = Vec::new();
        let mut encoder = gif::Encoder::new(&mut encoded, 16, 16, &[0, 0, 0, 255, 255, 255])
            .unwrap_or_else(|error| panic!("{error}"));
        for &(left, width) in frames {
            let buffer = vec![0_u8; usize::from(width) * usize::from(width)];
            encoder
                .write_frame(&gif::Frame {
                    left,
                    width,
                    height: width,
                    buffer: Cow::Owned(buffer),
                    ..gif::Frame::default()
                })
                .unwrap_or_else(|error| panic!("{error}"));
        }
        drop(encoder);
        encoded
    }

    fn inspect_bytes(bytes: &[u8]) -> Result<DecodedImageInfo, String> {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sample.gif");
        fs::write(&path, bytes).unwrap();
        inspect_image_file(&File::open(path).unwrap(), bytes.len() as u64, "sample")
    }

    #[test]
    fn gif_inspection_reports_every_frame() {
        let info = inspect_bytes(&gif_bytes(3)).unwrap();
        assert_eq!(info.format_id, "gif");
        assert_eq!(info.extension, "gif");
        assert_eq!(info.frame_count, Some(3));
        assert_eq!(info.dimensions, (16, 16));
    }

    #[test]
    fn gif_truncated_after_the_first_frame_is_rejected() {
        let bytes = gif_bytes(3);
        let error = inspect_bytes(&bytes[..bytes.len() * 2 / 3]).unwrap_err();
        assert!(error.contains("无法完整解码"), "{error}");
    }

    #[test]
    fn gif_frame_budget_rejects_excess_frames() {
        let error = inspect_bytes(&gif_bytes(MAX_GIF_FRAMES + 1)).unwrap_err();
        assert!(error.contains("防解压炸弹"), "{error}");
    }

    #[test]
    fn gif_frames_larger_than_the_canvas_are_rejected() {
        for frames in [&[(0, 2_048)][..], &[(0, 16), (0, 2_048)]] {
            let error = inspect_bytes(&gif_with_frames(frames)).unwrap_err();
            assert!(error.contains("无法完整解码"), "{error}");
        }
    }

    #[test]
    fn gif_frames_extending_past_the_canvas_are_rejected() {
        let error = inspect_bytes(&gif_with_frames(&[(0, 16), (8, 16)])).unwrap_err();
        assert!(error.contains("无法完整解码"), "{error}");
    }

    #[test]
    fn gif_sub_rectangle_frames_inside_the_canvas_stay_valid() {
        let info = inspect_bytes(&gif_with_frames(&[(0, 16), (8, 8), (0, 4)])).unwrap();
        assert_eq!(info.frame_count, Some(3));
    }

    #[test]
    fn gif_composition_budget_counts_canvas_pixels() {
        assert!(!gif_budget_exceeded(1_024, (1_024, 1_024)));
        assert!(gif_budget_exceeded(1_025, (16, 16)));
        assert!(gif_budget_exceeded(65, (4_096, 4_096)));
    }

    #[test]
    fn gif_input_is_rejected_from_its_header_before_any_frame_is_decoded() {
        let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
        let input = temp.path().join("input");
        fs::create_dir_all(&input).unwrap_or_else(|error| panic!("{error}"));
        // Everything after the logical screen descriptor is undecodable on purpose:
        // reaching the decoder would report a corrupt file instead of the format.
        let mut bytes = gif_bytes(1);
        bytes.truncate(16);
        bytes.resize(64, 0xff);
        let path = input.join("animation.gif");
        fs::write(&path, bytes).unwrap_or_else(|error| panic!("{error}"));
        let error =
            validate_input_image(&config(temp.path()), &path.to_string_lossy(), "image_path")
                .err()
                .unwrap_or_default();
        assert!(error.contains("格式 GIF 不受上游支持"), "{error}");
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
