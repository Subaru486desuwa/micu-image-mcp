use thiserror::Error;

pub const DEFAULT_CREDENTIAL_SERVICE: &str = "micu-image-mcp";
pub const DEFAULT_CREDENTIAL_ACCOUNT: &str = "image2-api-key";
pub const MIN_API_KEY_LEN: usize = 20;
pub const MAX_API_KEY_LEN: usize = 512;

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ApiKeyFormatError {
    #[error("API key 必须以 sk- 开头")]
    Prefix,
    #[error("API key 长度必须在 {MIN_API_KEY_LEN}..={MAX_API_KEY_LEN} 个 ASCII 字符之间")]
    Length,
    #[error("API key 只能包含 ASCII 字母、数字、'-' 和 '_'")]
    Charset,
}

#[derive(Debug, Error)]
pub enum CredentialStoreError {
    #[error("系统安全凭据存储不可用: {0}")]
    Unavailable(String),
    #[error("读取系统安全凭据失败: {0}")]
    Read(String),
    #[error("写入系统安全凭据失败: {0}")]
    Write(String),
}

pub fn validate_api_key(raw: &str) -> Result<&str, ApiKeyFormatError> {
    if !raw.starts_with("sk-") {
        return Err(ApiKeyFormatError::Prefix);
    }
    if !(MIN_API_KEY_LEN..=MAX_API_KEY_LEN).contains(&raw.len()) {
        return Err(ApiKeyFormatError::Length);
    }
    if !raw
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(ApiKeyFormatError::Charset);
    }
    Ok(raw)
}

pub fn load_api_key(service: &str, account: &str) -> Result<Option<String>, CredentialStoreError> {
    let entry = keyring::Entry::new(service, account)
        .map_err(|error| CredentialStoreError::Unavailable(error.to_string()))?;
    match entry.get_password() {
        Ok(secret) => Ok(Some(secret)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(CredentialStoreError::Read(error.to_string())),
    }
}

pub fn store_api_key(
    service: &str,
    account: &str,
    api_key: &str,
) -> Result<(), CredentialStoreError> {
    validate_api_key(api_key).map_err(|error| CredentialStoreError::Write(error.to_string()))?;
    let entry = keyring::Entry::new(service, account)
        .map_err(|error| CredentialStoreError::Unavailable(error.to_string()))?;
    entry
        .set_password(api_key)
        .map_err(|error| CredentialStoreError::Write(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::{ApiKeyFormatError, MAX_API_KEY_LEN, MIN_API_KEY_LEN, validate_api_key};

    #[test]
    fn api_key_format_requires_sk_prefix_length_and_safe_ascii_charset() {
        let minimum = format!("sk-{}", "a".repeat(MIN_API_KEY_LEN - 3));
        assert_eq!(validate_api_key(&minimum), Ok(minimum.as_str()));

        let maximum = format!("sk-{}", "Z".repeat(MAX_API_KEY_LEN - 3));
        assert_eq!(validate_api_key(&maximum), Ok(maximum.as_str()));

        assert_eq!(
            validate_api_key("pk-12345678901234567"),
            Err(ApiKeyFormatError::Prefix)
        );
        assert_eq!(
            validate_api_key("sk-too-short"),
            Err(ApiKeyFormatError::Length)
        );
        assert_eq!(
            validate_api_key("sk-1234567890123456!"),
            Err(ApiKeyFormatError::Charset)
        );
        assert_eq!(
            validate_api_key("sk-1234567890123456 中"),
            Err(ApiKeyFormatError::Charset)
        );
        assert_eq!(
            validate_api_key(" sk-12345678901234567"),
            Err(ApiKeyFormatError::Prefix)
        );
        assert_eq!(
            validate_api_key("sk-12345678901234567 "),
            Err(ApiKeyFormatError::Charset)
        );
    }
}
