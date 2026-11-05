use super::size::{SizeTier, parse_size, round_to_alignment, size_tier, validate_size};

pub const STANDARD_MODEL: &str = "gpt-image-2";
pub const QUALITY_MODEL: &str = "gpt-image-2-openai";
pub const FLARE_MODEL: &str = "gpt-image-2.5-flare";
pub const LEGACY_FLARE_MODEL: &str = "gpt-image-2.5-flare-openai";
pub const SUNBURST_MODEL: &str = "gpt-image-2.5-sunburst";
pub const LEGACY_SUNBURST_MODEL: &str = "gpt-image-2.5-sunburst-openai";
pub const FLARE_2_6_MODEL: &str = "gpt-image-2.6-flare";
pub const SUNBURST_2_6_MODEL: &str = "gpt-image-2.6-sunburst";
pub const SUPPORTED_IMAGE_MODELS: [&str; 6] = [
    STANDARD_MODEL,
    QUALITY_MODEL,
    FLARE_MODEL,
    SUNBURST_MODEL,
    FLARE_2_6_MODEL,
    SUNBURST_2_6_MODEL,
];

pub fn canonical_model(model: &str) -> &str {
    match model {
        LEGACY_FLARE_MODEL => FLARE_MODEL,
        LEGACY_SUNBURST_MODEL => SUNBURST_MODEL,
        _ => model,
    }
}

/// Only renamed aliases of the same model. The standard and historical quality
/// routes are distinct, not a suffix compatibility pair.
pub fn suffix_fallback(model: &str) -> Option<&'static str> {
    match model {
        FLARE_MODEL => Some(LEGACY_FLARE_MODEL),
        SUNBURST_MODEL => Some(LEGACY_SUNBURST_MODEL),
        _ => None,
    }
}

const BASE_QUALITY_VALUES: [&str; 4] = ["auto", "high", "low", "medium"];
const EXTENDED_QUALITY_VALUES: [&str; 6] = ["auto", "high", "low", "max", "medium", "xhigh"];
const GIF_FORMAT: &str = "gif";
const OUTPUT_FORMAT_VALUES: [&str; 3] = ["jpeg", "png", "webp"];
const ANIMATED_OUTPUT_FORMAT_VALUES: [&str; 4] = ["gif", "jpeg", "png", "webp"];
const STANDARD_PARALLEL_LIMIT: usize = 5;

pub fn is_supported_model(model: &str) -> bool {
    SUPPORTED_IMAGE_MODELS.contains(&canonical_model(model))
}

/// Accepted `quality` values, sorted as the public server_info contract lists them.
pub fn quality_values(model: &str) -> &'static [&'static str] {
    match canonical_model(model) {
        FLARE_MODEL | SUNBURST_MODEL | FLARE_2_6_MODEL | SUNBURST_2_6_MODEL => {
            &EXTENDED_QUALITY_VALUES
        }
        _ => &BASE_QUALITY_VALUES,
    }
}

/// Accepted `output_format` values, sorted as the public server_info contract lists them.
pub fn output_format_values(model: &str) -> &'static [&'static str] {
    match canonical_model(model) {
        FLARE_2_6_MODEL | SUNBURST_2_6_MODEL => &ANIMATED_OUTPUT_FORMAT_VALUES,
        _ => &OUTPUT_FORMAT_VALUES,
    }
}

/// Upstream retry scheduling chosen by the route; the HTTP executor only applies it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryOptions {
    pub enabled: bool,
    pub big_size: bool,
}

/// How one supported model runs at one validated size: the effective model,
/// quality choices, retry schedule, parallelism and response-format negotiation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelRoute {
    model: String,
    tier: SizeTier,
}

impl ModelRoute {
    /// `size` must already pass `validate_size`, and callers reject unsupported
    /// models first so that model errors keep precedence over size errors.
    pub fn resolve(
        requested: Option<&str>,
        default_model: &str,
        size: &str,
    ) -> (Self, Vec<String>) {
        let requested_model = requested.unwrap_or(default_model);
        let mut model = canonical_model(requested_model).to_owned();
        let tier = size_tier(size);
        let mut notes = Vec::new();
        if model != requested_model {
            notes.push(format!("模型 ID {requested_model} 已更新为 {model}"));
        }
        if is_large_tier(tier) && model == STANDARD_MODEL {
            notes.push(format!(
                "size={size} ({}) 已自动切到高质量线路 {QUALITY_MODEL}",
                tier_label(tier)
            ));
            model = QUALITY_MODEL.to_owned();
        }
        (Self { model, tier }, notes)
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn retry(&self) -> RetryOptions {
        let big_size = is_large_tier(self.tier);
        RetryOptions {
            enabled: is_quality_model(&self.model) || big_size,
            big_size,
        }
    }

    /// Only standard ≤1K routes fan out; quality and ≥2K routes run one request at a time.
    pub fn parallel_limit(&self) -> usize {
        if matches!(self.tier, SizeTier::Small | SizeTier::OneK) && !is_quality_model(&self.model) {
            STANDARD_PARALLEL_LIMIT
        } else {
            1
        }
    }

    /// 2K/4K generations are limited to one image per call.
    pub fn cap_n(&self, requested: usize) -> (usize, Option<String>) {
        if is_large_tier(self.tier) && requested > 1 {
            let note = format!(
                "{} 强制 N=1，已忽略请求的 n={requested}",
                tier_label(self.tier).to_ascii_uppercase()
            );
            (1, Some(note))
        } else {
            (requested, None)
        }
    }

    pub fn validate_quality(&self, quality: Option<&str>) -> Result<Option<String>, String> {
        validate_choice("quality", quality, quality_values(&self.model))
    }

    pub fn validate_output_format(&self, format: Option<&str>) -> Result<Option<String>, String> {
        validate_choice("output_format", format, output_format_values(&self.model))
    }

    /// Only the two legacy routes negotiate response format. Modern Images
    /// routes select it upstream; repeating an unchanged payload can charge twice.
    pub fn response_formats<'a>(
        &self,
        configured: &'a [&'static str],
    ) -> impl Iterator<Item = Option<&'static str>> + 'a {
        let explicit = matches!(self.model.as_str(), STANDARD_MODEL | QUALITY_MODEL);
        configured
            .iter()
            .take(if explicit { configured.len() } else { 1 })
            .map(move |format| explicit.then_some(*format))
    }
}

fn validate_choice(
    field: &str,
    value: Option<&str>,
    allowed: &[&str],
) -> Result<Option<String>, String> {
    let Some(raw) = value else {
        return Ok(None);
    };
    let cleaned = raw.trim().to_ascii_lowercase();
    if cleaned.is_empty() {
        return Ok(None);
    }
    if allowed.contains(&cleaned.as_str()) {
        Ok(Some(cleaned))
    } else {
        Err(format!(
            "{field} 不支持 {}；可选 {}",
            python_string_repr(raw),
            allowed.join(" / ")
        ))
    }
}

pub fn output_format_note(requested: Option<&str>, actual: &str) -> Option<String> {
    let requested = requested?;
    (requested != actual).then(|| {
        let hint = if requested == GIF_FORMAT {
            "；动图需要在 prompt 中明确要求动画"
        } else {
            ""
        };
        format!("⚠ 请求 output_format={requested}，实际返回 {actual}；以 saved[].format 为准{hint}")
    })
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

/// A returned GIF gets a neutral note because the upstream service picks its size.
/// A static answer to a GIF request also skips the exact-pixel route, which has no GIF.
pub fn size_note(
    requested: &str,
    actual: Option<(u32, u32)>,
    requested_format: Option<&str>,
    actual_format: &str,
) -> Option<String> {
    let (requested_width, requested_height) = parse_size(requested)?;
    let (actual_width, actual_height) = actual?;
    if (requested_width, requested_height) == (actual_width, actual_height) {
        return None;
    }
    if actual_format == GIF_FORMAT {
        return Some(format!(
            "GIF 实际 {actual_width}×{actual_height}：尺寸由上游决定，通常约 414×414，不保证遵循 size；以 saved[].actual_size 为准。"
        ));
    }
    let requested_megapixels =
        f64::from(requested_width) * f64::from(requested_height) / 1_000_000.0;
    let actual_megapixels = f64::from(actual_width) * f64::from(actual_height) / 1_000_000.0;
    let guidance = if requested_format == Some(GIF_FORMAT) {
        "涉及 GIF 时尺寸和宽高比不保证遵循请求，请以 saved[].actual_size 为准。"
    } else {
        "米醋后端可能重映射自定义尺寸。需要精确像素时请使用 gpt-image-2-openai，并核对 saved.actual_size。"
    };
    Some(format!(
        "⚠ 实际 {actual_width}×{actual_height} ({actual_megapixels:.2}MP) ≠ 请求 {requested_width}×{requested_height} ({requested_megapixels:.2}MP)；{guidance}"
    ))
}

pub fn is_quality_model(model: &str) -> bool {
    matches!(
        canonical_model(model),
        QUALITY_MODEL | SUNBURST_MODEL | SUNBURST_2_6_MODEL
    )
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

    fn route(model: &str, size: &str) -> ModelRoute {
        ModelRoute::resolve(Some(model), STANDARD_MODEL, size).0
    }

    #[test]
    fn output_format_is_optional_and_gif_is_limited_to_image_2_6() {
        for model in [FLARE_2_6_MODEL, SUNBURST_2_6_MODEL] {
            let route = route(model, "1024x1024");
            assert_eq!(route.validate_output_format(None), Ok(None));
            assert_eq!(route.validate_output_format(Some("  ")), Ok(None));
            assert_eq!(
                route.validate_output_format(Some(" GIF ")),
                Ok(Some("gif".into()))
            );
        }
        for model in [
            STANDARD_MODEL,
            QUALITY_MODEL,
            FLARE_MODEL,
            LEGACY_SUNBURST_MODEL,
        ] {
            let route = route(model, "1024x1024");
            assert_eq!(
                route.validate_output_format(Some("webp")),
                Ok(Some("webp".into()))
            );
            assert!(
                route
                    .validate_output_format(Some("gif"))
                    .is_err_and(|error| error.contains("output_format 不支持 'gif'"))
            );
        }
        assert!(
            route(FLARE_2_6_MODEL, "1024x1024")
                .validate_output_format(Some("jpg"))
                .is_err()
        );
    }

    #[test]
    fn output_format_note_only_reports_a_mismatch() {
        assert_eq!(output_format_note(None, "png"), None);
        assert_eq!(output_format_note(Some("gif"), "gif"), None);
        assert!(
            output_format_note(Some("gif"), "png")
                .is_some_and(|note| note.contains("png") && note.contains("prompt"))
        );
        assert!(
            output_format_note(Some("webp"), "png").is_some_and(|note| !note.contains("prompt"))
        );
    }

    #[test]
    fn model_contract_is_exact() {
        for model in [
            STANDARD_MODEL,
            QUALITY_MODEL,
            FLARE_MODEL,
            LEGACY_FLARE_MODEL,
            SUNBURST_MODEL,
            LEGACY_SUNBURST_MODEL,
            FLARE_2_6_MODEL,
            SUNBURST_2_6_MODEL,
        ] {
            assert!(is_supported_model(model), "{model}");
        }
        for model in [
            " gpt-image-2 ",
            "GROK-imagine-image",
            "grok-imagine-image",
            "gpt-image-2.6-flare-openai",
            "gpt-image-2.6-sunburst-openai",
        ] {
            assert!(!is_supported_model(model), "{model}");
        }
    }

    #[test]
    fn route_table_covers_every_model_and_size_tier() {
        // (model, size, effective model, retry enabled, big size, parallel limit, formats tried)
        let table = [
            (
                STANDARD_MODEL,
                "1024x1024",
                STANDARD_MODEL,
                false,
                false,
                5,
                2,
            ),
            (STANDARD_MODEL, "2048x1152", QUALITY_MODEL, true, true, 1, 2),
            (STANDARD_MODEL, "3840x2160", QUALITY_MODEL, true, true, 1, 2),
            (QUALITY_MODEL, "1024x1024", QUALITY_MODEL, true, false, 1, 2),
            (QUALITY_MODEL, "2048x1152", QUALITY_MODEL, true, true, 1, 2),
            (
                FLARE_2_6_MODEL,
                "1024x1024",
                FLARE_2_6_MODEL,
                false,
                false,
                5,
                1,
            ),
            (
                FLARE_2_6_MODEL,
                "2048x1152",
                FLARE_2_6_MODEL,
                true,
                true,
                1,
                1,
            ),
            (
                FLARE_2_6_MODEL,
                "3840x2160",
                FLARE_2_6_MODEL,
                true,
                true,
                1,
                1,
            ),
            (
                SUNBURST_2_6_MODEL,
                "1024x1024",
                SUNBURST_2_6_MODEL,
                true,
                false,
                1,
                1,
            ),
            (
                SUNBURST_2_6_MODEL,
                "2048x1152",
                SUNBURST_2_6_MODEL,
                true,
                true,
                1,
                1,
            ),
            (
                SUNBURST_2_6_MODEL,
                "3840x2160",
                SUNBURST_2_6_MODEL,
                true,
                true,
                1,
                1,
            ),
            (FLARE_MODEL, "1024x1024", FLARE_MODEL, false, false, 5, 1),
            (FLARE_MODEL, "2048x1152", FLARE_MODEL, true, true, 1, 1),
            (FLARE_MODEL, "3840x2160", FLARE_MODEL, true, true, 1, 1),
            (
                LEGACY_FLARE_MODEL,
                "2048x1152",
                FLARE_MODEL,
                true,
                true,
                1,
                1,
            ),
            (
                SUNBURST_MODEL,
                "1024x1024",
                SUNBURST_MODEL,
                true,
                false,
                1,
                1,
            ),
            (
                SUNBURST_MODEL,
                "2048x1152",
                SUNBURST_MODEL,
                true,
                true,
                1,
                1,
            ),
            (
                LEGACY_SUNBURST_MODEL,
                "1024x1024",
                SUNBURST_MODEL,
                true,
                false,
                1,
                1,
            ),
        ];
        let configured = ["url", "b64_json"];
        for (model, size, effective, enabled, big_size, parallel, formats) in table {
            let route = route(model, size);
            let case = format!("{model} @ {size}");
            assert_eq!(route.model(), effective, "{case}");
            assert_eq!(route.retry(), RetryOptions { enabled, big_size }, "{case}");
            assert_eq!(route.parallel_limit(), parallel, "{case}");
            let tried = route.response_formats(&configured).collect::<Vec<_>>();
            assert_eq!(tried.len(), formats, "{case}");
            if formats == 1 {
                assert_eq!(
                    tried,
                    vec![None],
                    "{case}: modern routes omit response_format"
                );
            } else {
                assert_eq!(tried, vec![Some("url"), Some("b64_json")], "{case}");
            }
        }
    }

    #[test]
    fn large_tiers_cap_generation_count_with_public_note() {
        assert_eq!(route(FLARE_MODEL, "1024x1024").cap_n(4), (4, None));
        assert_eq!(
            route(FLARE_MODEL, "2048x1152").cap_n(4),
            (1, Some("2K 强制 N=1，已忽略请求的 n=4".into()))
        );
        assert_eq!(route(FLARE_MODEL, "3840x2160").cap_n(1), (1, None));
    }

    #[test]
    fn quality_values_depend_on_the_effective_model() {
        assert_eq!(
            route(FLARE_MODEL, "1024x1024").validate_quality(Some(" HIGH ")),
            Ok(Some("high".into()))
        );
        assert_eq!(
            route(SUNBURST_MODEL, "1024x1024").validate_quality(Some("MAX")),
            Ok(Some("max".into()))
        );
        assert_eq!(
            route(FLARE_MODEL, "1024x1024").validate_quality(Some("  ")),
            Ok(None)
        );
        assert_eq!(
            route(FLARE_MODEL, "1024x1024").validate_quality(None),
            Ok(None)
        );
        assert_eq!(
            route(FLARE_MODEL, "1024x1024").validate_quality(Some("ultra")),
            Err("quality 不支持 'ultra'；可选 auto / high / low / max / medium / xhigh".into())
        );
        assert_eq!(
            route(STANDARD_MODEL, "1024x1024").validate_quality(Some("max")),
            Err("quality 不支持 'max'；可选 auto / high / low / medium".into())
        );
        // 2K gpt-image-2 switches to the quality route before quality is checked.
        assert!(
            route(STANDARD_MODEL, "2048x1152")
                .validate_quality(Some("xhigh"))
                .is_err()
        );
    }

    #[test]
    fn suffix_candidates_are_same_model_aliases_not_standard_quality_routes() {
        assert_eq!(suffix_fallback(FLARE_MODEL), Some(LEGACY_FLARE_MODEL));
        assert_eq!(suffix_fallback(SUNBURST_MODEL), Some(LEGACY_SUNBURST_MODEL));
        for model in [
            STANDARD_MODEL,
            QUALITY_MODEL,
            LEGACY_FLARE_MODEL,
            LEGACY_SUNBURST_MODEL,
        ] {
            assert_eq!(suffix_fallback(model), None);
        }
        assert!(is_quality_model(LEGACY_SUNBURST_MODEL));
    }

    #[test]
    fn flare_alias_is_exact_and_preserves_size_and_scheduling_policy() {
        for size in ["1024x1024", "2048x1152", "3840x2160"] {
            let (route, notes) = ModelRoute::resolve(Some(FLARE_MODEL), STANDARD_MODEL, size);
            assert_eq!((route.model(), notes.len()), (FLARE_MODEL, 0));
            let (route, notes) =
                ModelRoute::resolve(Some(LEGACY_FLARE_MODEL), STANDARD_MODEL, size);
            assert_eq!(route.model(), FLARE_MODEL);
            assert!(notes[0].contains("已更新为 gpt-image-2.5-flare"));
            let (route, _) = ModelRoute::resolve(None, LEGACY_FLARE_MODEL, size);
            assert_eq!(route.model(), FLARE_MODEL);
        }
        assert!(!is_quality_model(FLARE_MODEL));
        for invalid in [
            "gpt-image-2.5-flare-openai-openai",
            " gpt-image-2.5-flare",
            "gpt-image-2.5-flare ",
            "GPT-IMAGE-2.5-FLARE-OPENAI",
        ] {
            assert!(!is_supported_model(invalid));
        }
    }

    #[test]
    fn high_resolution_routes_to_quality_with_public_note() {
        let (route, notes) = ModelRoute::resolve(None, STANDARD_MODEL, "1024x1024");
        assert_eq!((route.model(), notes.len()), (STANDARD_MODEL, 0));
        let (route, notes) = ModelRoute::resolve(Some(STANDARD_MODEL), STANDARD_MODEL, "2048x1152");
        assert_eq!(route.model(), QUALITY_MODEL);
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
        assert_eq!(
            size_note("1024x1024", Some((1024, 1024)), None, "png"),
            None
        );
        for requested_format in [None, Some("png"), Some("webp")] {
            let note = size_note("3840x2160", Some((2048, 1152)), requested_format, "png");
            assert!(note.is_some_and(|text| text.contains("实际 2048×1152")
                && text.contains("≠ 请求 3840×2160")
                && text.contains(QUALITY_MODEL)));
        }
    }

    #[test]
    fn gif_size_note_never_recommends_a_route_without_gif() {
        assert_eq!(
            size_note("1024x1024", Some((1024, 1024)), Some("gif"), "gif"),
            None
        );
        for (requested_format, actual_format) in [
            (Some("gif"), "gif"),
            (None, "gif"),
            (Some("png"), "gif"),
            (Some("gif"), "png"),
        ] {
            let note = size_note(
                "1024x1024",
                Some((410, 410)),
                requested_format,
                actual_format,
            )
            .unwrap_or_default();
            assert!(note.contains("实际 410×410"), "{note}");
            assert!(note.contains("GIF"), "{note}");
            assert!(!note.contains(QUALITY_MODEL), "{note}");
            assert_eq!(note.starts_with('⚠'), actual_format != "gif", "{note}");
        }
    }
}
