pub fn safe_basename(name: Option<&str>) -> Option<String> {
    let value = name?;
    if value.trim().is_empty()
        || value.len() > 100
        || value.starts_with('.')
        || value.contains("..")
        || value.contains('/')
        || value.contains('\\')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return None;
    }
    Some(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basename_contract_accepts_only_the_frozen_ascii_surface() {
        assert_eq!(safe_basename(Some("a-b_c.d")), Some("a-b_c.d".into()));
        assert_eq!(safe_basename(Some(&"a".repeat(100))), Some("a".repeat(100)));
        for rejected in [
            "",
            "   ",
            "a/b",
            "a\\b",
            "../etc/passwd",
            "..hidden",
            ".hidden",
            "file name",
            "中文",
        ] {
            assert_eq!(safe_basename(Some(rejected)), None, "{rejected:?}");
        }
        assert_eq!(safe_basename(Some(&"a".repeat(101))), None);
        assert_eq!(safe_basename(None), None);
    }
}
