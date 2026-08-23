pub const MAX_N: i64 = 10;
pub const MIN_SIZE_EDGE: u32 = 256;
pub const MAX_SIZE_EDGE: u32 = 3_840;
pub const SIZE_ALIGNMENT: u32 = 16;
pub const MIN_IMAGE_PIXELS: u64 = 655_360;
pub const MAX_IMAGE_PIXELS: u64 = 8_294_400;
pub const MAX_IMAGE_ASPECT_RATIO: f64 = 3.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SizeTier {
    Unknown,
    Small,
    OneK,
    TwoK,
    FourK,
}

pub fn parse_size(size: &str) -> Option<(u32, u32)> {
    let normalized = size.trim().to_ascii_lowercase();
    let (width, height) = normalized.split_once('x')?;
    if width.is_empty()
        || height.is_empty()
        || width.contains('x')
        || height.contains('x')
        || !width.bytes().all(|byte| byte.is_ascii_digit())
        || !height.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    Some((width.parse().ok()?, height.parse().ok()?))
}

pub fn size_tier(size: &str) -> SizeTier {
    let Some((width, height)) = parse_size(size) else {
        return SizeTier::Unknown;
    };
    match width.max(height) {
        0..=1_023 => SizeTier::Small,
        1_024..=1_599 => SizeTier::OneK,
        1_600..=2_999 => SizeTier::TwoK,
        _ => SizeTier::FourK,
    }
}

pub fn validate_size(size: Option<&str>, allow_none: bool) -> (Option<String>, Option<String>) {
    let Some(raw) = size else {
        return if allow_none {
            (None, None)
        } else {
            (
                None,
                Some("size 不能为 None（此 tool 必须传明确 size）".to_owned()),
            )
        };
    };
    let Some((width, height)) = parse_size(raw) else {
        return (
            None,
            Some(format!(
                "size 格式错误：必须是 'WxH'（如 '1024x1024'），收到 {}",
                python_string_repr(raw)
            )),
        );
    };
    if width == 0 || height == 0 {
        return (None, Some(format!("size W/H 必须为正数，收到 {raw}")));
    }
    if width < MIN_SIZE_EDGE || height < MIN_SIZE_EDGE {
        return (
            None,
            Some(format!("size 边长太小（最小 {MIN_SIZE_EDGE}），收到 {raw}")),
        );
    }
    if width > MAX_SIZE_EDGE || height > MAX_SIZE_EDGE {
        return (
            None,
            Some(format!("size 边长太大（最大 {MAX_SIZE_EDGE}），收到 {raw}")),
        );
    }
    if width % SIZE_ALIGNMENT != 0 || height % SIZE_ALIGNMENT != 0 {
        return (
            None,
            Some(format!(
                "size W/H 必须是 {SIZE_ALIGNMENT} 的倍数，收到 {raw}"
            )),
        );
    }
    let ratio = f64::from(width.max(height)) / f64::from(width.min(height));
    if ratio > MAX_IMAGE_ASPECT_RATIO {
        return (
            None,
            Some(format!(
                "size 长宽比不能超过 {}:1，收到 {raw}",
                MAX_IMAGE_ASPECT_RATIO as u32
            )),
        );
    }
    let pixels = u64::from(width) * u64::from(height);
    if pixels < MIN_IMAGE_PIXELS {
        return (
            None,
            Some(format!(
                "size 总像素太少（最小 {}），收到 {raw}",
                format_with_commas(MIN_IMAGE_PIXELS)
            )),
        );
    }
    if pixels > MAX_IMAGE_PIXELS {
        return (
            None,
            Some(format!(
                "size 总像素太多（最大 {}），收到 {raw}",
                format_with_commas(MAX_IMAGE_PIXELS)
            )),
        );
    }
    (Some(format!("{width}x{height}")), None)
}

pub fn validate_n(n: &serde_json::Value) -> Option<String> {
    let Some(value) = n.as_i64() else {
        return Some(format!("n 必须是整数，收到 {}", json_type_name(n)));
    };
    if value < 1 {
        return Some(format!("n 必须 ≥ 1，收到 {value}"));
    }
    if value > MAX_N {
        return Some(format!(
            "n 必须 ≤ {MAX_N}，收到 {value}（防止意外 burn quota）"
        ));
    }
    None
}

pub fn validate_quality(quality: Option<&serde_json::Value>) -> (Option<String>, Option<String>) {
    let Some(value) = quality else {
        return (None, None);
    };
    if value.is_null() {
        return (None, None);
    }
    let Some(raw) = value.as_str() else {
        return (
            None,
            Some(format!(
                "quality 必须是字符串，收到 {}",
                json_type_name(value)
            )),
        );
    };
    let cleaned = raw.trim().to_ascii_lowercase();
    if cleaned.is_empty() {
        return (None, None);
    }
    if !matches!(cleaned.as_str(), "auto" | "low" | "medium" | "high") {
        return (
            None,
            Some(format!(
                "quality 不支持 {}；可选 auto / high / low / medium",
                python_string_repr(raw)
            )),
        );
    }
    (Some(cleaned), None)
}

pub fn round_to_alignment(value: u32) -> u32 {
    let quotient = value / SIZE_ALIGNMENT;
    let remainder = value % SIZE_ALIGNMENT;
    let rounded_quotient = if remainder < SIZE_ALIGNMENT / 2 {
        quotient
    } else if remainder > SIZE_ALIGNMENT / 2 || quotient % 2 == 1 {
        quotient.saturating_add(1)
    } else {
        quotient
    };
    SIZE_ALIGNMENT.max(rounded_quotient.saturating_mul(SIZE_ALIGNMENT))
}

fn json_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "NoneType",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(number) if number.is_i64() || number.is_u64() => "int",
        serde_json::Value::Number(_) => "float",
        serde_json::Value::String(_) => "str",
        serde_json::Value::Array(_) => "list",
        serde_json::Value::Object(_) => "dict",
    }
}

fn python_string_repr(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('\'', "\\'");
    format!("'{escaped}'")
}

fn format_with_commas(value: u64) -> String {
    let digits = value.to_string();
    let mut output = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, byte) in digits.bytes().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            output.push(',');
        }
        output.push(char::from(byte));
    }
    output
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn size_contract_accepts_and_normalizes_documented_values() {
        assert_eq!(parse_size("  2048X1152  "), Some((2048, 1152)));
        assert_eq!(
            validate_size(Some("  1024X1024 "), false),
            (Some("1024x1024".into()), None)
        );
        assert_eq!(size_tier("1599x1000"), SizeTier::OneK);
        assert_eq!(size_tier("1600x1000"), SizeTier::TwoK);
        assert_eq!(size_tier("3000x1000"), SizeTier::FourK);
    }

    #[test]
    fn size_contract_rejects_each_public_limit_with_chinese_reason() {
        for (size, marker) in [
            ("128x128", "太小"),
            ("4096x2160", "太大"),
            ("1920x1080", "16"),
            ("3840x1024", "长宽比"),
            ("3840x3840", "总像素太多"),
            ("512x512", "总像素太少"),
        ] {
            let (cleaned, error) = validate_size(Some(size), false);
            assert_eq!(cleaned, None);
            assert!(
                error.as_deref().is_some_and(|text| text.contains(marker)),
                "{size}: {error:?}"
            );
        }
    }

    #[test]
    fn n_quality_and_alignment_match_the_reference_literals() {
        assert_eq!(
            validate_n(&json!(true)),
            Some("n 必须是整数，收到 bool".into())
        );
        assert_eq!(
            validate_n(&json!(11)),
            Some("n 必须 ≤ 10，收到 11（防止意外 burn quota）".into())
        );
        assert_eq!(validate_n(&json!(5)), None);
        assert_eq!(
            validate_quality(Some(&json!(" HIGH "))),
            (Some("high".into()), None)
        );
        assert!(validate_quality(Some(&json!("ultra"))).1.is_some());
        assert_eq!(round_to_alignment(1080), 1088);
        assert_eq!(round_to_alignment(1000), 992);
    }
}
