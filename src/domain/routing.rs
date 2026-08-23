use super::size::{SizeTier, parse_size, round_to_alignment, size_tier, validate_size};

pub const STANDARD_MODEL: &str = "gpt-image-2";
pub const QUALITY_MODEL: &str = "gpt-image-2-openai";
pub const SUPPORTED_IMAGE_MODELS: [&str; 2] = [STANDARD_MODEL, QUALITY_MODEL];

pub fn is_grok_model(model: Option<&str>) -> bool {
    model.is_some_and(|value| value.trim().to_ascii_lowercase().starts_with("grok-"))
}

pub fn model_error(requested: Option<&str>, default_model: &str) -> Option<String> {
    let model = requested.unwrap_or(default_model);
    if SUPPORTED_IMAGE_MODELS.contains(&model) {
        return None;
    }
    let supported = SUPPORTED_IMAGE_MODELS.join(" / ");
    if is_grok_model(Some(model)) {
        return Some(format!(
            "Grok 生图渠道暂时关闭，待服务器支持后再启用；当前仅支持 {supported}。"
        ));
    }
    Some(format!(
        "不支持 model={}；当前仅支持 {supported}。Grok 生图渠道暂时关闭，待服务器支持后再启用。",
        python_string_repr(model)
    ))
}

pub fn resolve_model(
    requested: Option<&str>,
    default_model: &str,
    size: &str,
) -> (String, Vec<String>) {
    let mut model = requested.unwrap_or(default_model).to_owned();
    let tier = size_tier(size);
    let mut notes = Vec::new();
    if is_large_tier(tier) && !is_quality_model(&model) && !is_grok_model(Some(&model)) {
        notes.push(format!(
            "size={size} ({}) 已自动切到高质量线路 {QUALITY_MODEL}",
            tier_label(tier)
        ));
        model = QUALITY_MODEL.to_owned();
    }
    (model, notes)
}

pub fn infer_size_from_prompt(prompt: &str) -> Option<(String, String)> {
    let normalized = prompt.to_lowercase();
    if let Some((width, height)) = find_explicit_pixels(&normalized) {
        let aligned_width = round_to_alignment(width);
        let aligned_height = round_to_alignment(height);
        let inferred = format!("{aligned_width}x{aligned_height}");
        if validate_size(Some(&inferred), false).1.is_some() {
            return None;
        }
        let reason = if aligned_width != width || aligned_height != height {
            format!(
                "prompt 含像素 {width}x{height}，对齐 16 倍数为 {aligned_width}x{aligned_height}"
            )
        } else {
            format!("prompt 含明确像素 {width}x{height}")
        };
        return Some((inferred, reason));
    }

    let vertical_keywords = [
        "9:16",
        "竖屏",
        "竖版",
        "vertical",
        "portrait",
        "phone wallpaper",
        "tiktok",
        "reels",
        "stories",
        "手机壁纸",
    ];
    let horizontal_keywords = [
        "16:9",
        "横屏",
        "横版",
        "landscape",
        "widescreen",
        "desktop wallpaper",
        "wallpaper",
        "壁纸",
        "banner",
        "封面",
        "cover",
    ];
    let square_keywords = [
        "正方形",
        "square",
        "avatar",
        "头像",
        "icon",
        "logo",
        "profile pic",
        "头像图",
        "图标",
    ];
    let poster_keywords = ["poster", "海报", "2:3", "movie poster"];
    let photo_keywords = ["3:2", "photograph", "照片"];
    let is_vertical = contains_any(&normalized, &vertical_keywords);
    let is_horizontal = contains_any(&normalized, &horizontal_keywords);
    let is_square = contains_any(&normalized, &square_keywords);
    let is_poster = contains_any(&normalized, &poster_keywords);
    let is_photo = contains_any(&normalized, &photo_keywords);

    if contains_word(&normalized, "4k")
        || normalized.contains("uhd")
        || normalized.contains("ultra hd")
        || normalized.contains("ultra-hd")
        || normalized.contains("ultrahd")
        || normalized.contains("超高清")
    {
        return if is_vertical {
            Some(("2160x3840".into(), "prompt 含 4K 关键字 + 竖屏".into()))
        } else {
            Some(("3840x2160".into(), "prompt 含 4K 关键字（默认横屏）".into()))
        };
    }
    if contains_word(&normalized, "2k")
        || normalized.contains("1080p")
        || normalized.contains("full hd")
        || normalized.contains("full-hd")
        || normalized.contains("fullhd")
        || contains_word(&normalized, "fhd")
    {
        return if is_vertical {
            Some((
                "1152x2048".into(),
                "prompt 含 2K/1080p 关键字 + 竖屏".into(),
            ))
        } else {
            Some((
                "2048x1152".into(),
                "prompt 含 2K/1080p 关键字（默认横屏）".into(),
            ))
        };
    }
    if normalized.contains("720p") || contains_word(&normalized, "hd") {
        return if is_vertical {
            Some(("720x1280".into(), "prompt 含 720p 关键字 + 竖屏".into()))
        } else {
            Some(("1280x720".into(), "prompt 含 720p 关键字".into()))
        };
    }
    if is_square {
        return Some(("1024x1024".into(), "prompt 含正方形/logo/头像关键字".into()));
    }
    if is_poster {
        return Some(("1024x1536".into(), "prompt 含海报/2:3 关键字".into()));
    }
    if is_photo {
        return Some(("1536x1024".into(), "prompt 含照片/3:2 关键字".into()));
    }
    if is_vertical {
        return Some(("1024x1536".into(), "prompt 含竖屏关键字（1K 默认）".into()));
    }
    if is_horizontal {
        return Some(("1536x1024".into(), "prompt 含横屏关键字（1K 默认）".into()));
    }
    None
}

pub fn size_note(requested: &str, actual: Option<(u32, u32)>) -> Option<String> {
    let (requested_width, requested_height) = parse_size(requested)?;
    let (actual_width, actual_height) = actual?;
    if (requested_width, requested_height) == (actual_width, actual_height) {
        return None;
    }
    let requested_megapixels =
        f64::from(requested_width) * f64::from(requested_height) / 1_000_000.0;
    let actual_megapixels = f64::from(actual_width) * f64::from(actual_height) / 1_000_000.0;
    Some(format!(
        "⚠ 实际 {actual_width}×{actual_height} ({actual_megapixels:.2}MP) ≠ 请求 {requested_width}×{requested_height} ({requested_megapixels:.2}MP)；米醋后端可能重映射自定义尺寸。需要精确像素时请使用 gpt-image-2-openai，并核对 saved.actual_size。"
    ))
}

pub fn is_quality_model(model: &str) -> bool {
    model == QUALITY_MODEL
}

pub fn is_large_tier(tier: SizeTier) -> bool {
    matches!(tier, SizeTier::TwoK | SizeTier::FourK)
}

fn tier_label(tier: SizeTier) -> &'static str {
    match tier {
        SizeTier::Unknown => "unknown",
        SizeTier::Small => "small",
        SizeTier::OneK => "1k",
        SizeTier::TwoK => "2k",
        SizeTier::FourK => "4k",
    }
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn is_word_character(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

fn contains_word(haystack: &str, needle: &str) -> bool {
    haystack.match_indices(needle).any(|(index, _)| {
        let before = haystack[..index].chars().next_back();
        let after = haystack[index + needle.len()..].chars().next();
        !before.is_some_and(is_word_character) && !after.is_some_and(is_word_character)
    })
}

fn find_explicit_pixels(prompt: &str) -> Option<(u32, u32)> {
    let bytes = prompt.as_bytes();
    for start in 0..bytes.len() {
        if !bytes[start].is_ascii_digit() {
            continue;
        }
        let mut cursor = start;
        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() && cursor - start < 4 {
            cursor += 1;
        }
        if cursor - start < 3 {
            continue;
        }
        let first_end = cursor;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor < bytes.len() && matches!(bytes[cursor], b'x' | b'X') {
            cursor += 1;
        } else if bytes.get(cursor..cursor.saturating_add(2)) == Some("×".as_bytes()) {
            cursor += 2;
        } else {
            continue;
        }
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let second_start = cursor;
        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() && cursor - second_start < 4 {
            cursor += 1;
        }
        if cursor - second_start < 3 {
            continue;
        }
        let width = prompt[start..first_end].parse().ok()?;
        let height = prompt[second_start..cursor].parse().ok()?;
        return Some((width, height));
    }
    None
}

fn python_string_repr(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('\'', "\\'");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_contract_is_exact_and_preserves_grok_public_error() {
        assert_eq!(model_error(Some(STANDARD_MODEL), STANDARD_MODEL), None);
        assert_eq!(model_error(Some(QUALITY_MODEL), STANDARD_MODEL), None);
        assert!(
            model_error(Some(" gpt-image-2 "), STANDARD_MODEL)
                .is_some_and(|text| text.contains("不支持 model=' gpt-image-2 '"))
        );
        assert!(
            model_error(Some("GROK-imagine-image"), STANDARD_MODEL)
                .is_some_and(|text| text.contains("Grok 生图渠道暂时关闭"))
        );
    }

    #[test]
    fn high_resolution_routes_to_quality_with_public_note() {
        assert_eq!(
            resolve_model(None, STANDARD_MODEL, "1024x1024"),
            (STANDARD_MODEL.into(), vec![])
        );
        let (model, notes) = resolve_model(Some(STANDARD_MODEL), STANDARD_MODEL, "2048x1152");
        assert_eq!(model, QUALITY_MODEL);
        assert_eq!(
            notes,
            vec!["size=2048x1152 (2k) 已自动切到高质量线路 gpt-image-2-openai"]
        );
    }

    #[test]
    fn prompt_inference_preserves_priority_and_python_rounding() {
        assert_eq!(
            infer_size_from_prompt("画一张 1920x1080 的图").map(|item| item.0),
            Some("1920x1088".into())
        );
        assert_eq!(
            infer_size_from_prompt("4K vertical 海报").map(|item| item.0),
            Some("2160x3840".into())
        );
        assert_eq!(
            infer_size_from_prompt("FullHD vertical phone wallpaper").map(|item| item.0),
            Some("1152x2048".into())
        );
        assert_eq!(
            infer_size_from_prompt("a minimalist logo").map(|item| item.0),
            Some("1024x1024".into())
        );
        assert_eq!(infer_size_from_prompt("4K landscape 1024x512 layout"), None);
        assert_eq!(infer_size_from_prompt("a red apple"), None);
    }

    #[test]
    fn size_mismatch_note_is_stable() {
        assert_eq!(size_note("1024x1024", Some((1024, 1024))), None);
        let note = size_note("3840x2160", Some((2048, 1152)));
        assert!(note.is_some_and(
            |text| text.contains("实际 2048×1152") && text.contains("≠ 请求 3840×2160")
        ));
    }
}
