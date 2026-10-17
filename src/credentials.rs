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
    #[error("系统凭据已锁定、被拒绝或需要用户授权；后台不会弹出认证窗口")]
    AccessRequired,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialInteraction {
    Forbidden,
    Allowed,
}

/// Boundary to the system credential service; macOS background callers forbid UI.
pub trait CredentialReader {
    fn read(
        &self,
        service: &str,
        account: &str,
        interaction: CredentialInteraction,
    ) -> Result<Option<String>, CredentialStoreError>;
}

pub struct SystemCredentials;

impl CredentialReader for SystemCredentials {
    fn read(
        &self,
        service: &str,
        account: &str,
        interaction: CredentialInteraction,
    ) -> Result<Option<String>, CredentialStoreError> {
        #[cfg(target_os = "macos")]
        {
            macos::read(None, service, account, interaction)
        }
        #[cfg(not(target_os = "macos"))]
        {
            // Preserve the existing Windows/Linux store selection and unlock behavior.
            let _ = interaction;
            let entry = keyring::Entry::new(service, account)
                .map_err(|_| CredentialStoreError::Unavailable("无法连接系统凭据服务".into()))?;
            match entry.get_password() {
                Ok(secret) => Ok(Some(secret)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(_) => Err(CredentialStoreError::Read("系统凭据无法读取".into())),
            }
        }
    }
}

/// Normal runtime lookup. On macOS this fails closed if UI cannot be disabled.
pub fn load_api_key(service: &str, account: &str) -> Result<Option<String>, CredentialStoreError> {
    SystemCredentials.read(service, account, CredentialInteraction::Forbidden)
}

/// Only for explicit, user-initiated installation or authorization.
pub fn load_api_key_interactive(
    service: &str,
    account: &str,
) -> Result<Option<String>, CredentialStoreError> {
    SystemCredentials.read(service, account, CredentialInteraction::Allowed)
}

pub const CREDENTIAL_RECOVERY: &str = "在终端运行客户端配置所指向的同一个 micu-image-mcp 可执行文件的 credentials authorize 命令，完成后重连 MCP；首次配置或换 Key 请运行 credentials set。自定义凭据项须沿用 MICU_KEYCHAIN_SERVICE / MICU_KEYCHAIN_ACCOUNT。";

/// Authorization is successful only when the same entry is subsequently usable
/// without UI. Never write the key, change ACLs or retry a cancelled prompt.
pub fn authorize_credentials(
    reader: &dyn CredentialReader,
    service: &str,
    account: &str,
) -> Result<(), CredentialStoreError> {
    let secret = reader
        .read(service, account, CredentialInteraction::Allowed)?
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| CredentialStoreError::Read("未找到凭据；请运行 credentials set".into()))?;
    match reader.read(service, account, CredentialInteraction::Forbidden)? {
        Some(verified) if verified == secret => Ok(()),
        _ => Err(CredentialStoreError::Read(
            "凭据在授权后不可用；请检查目标凭据项后重试".into(),
        )),
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use security_framework::os::macos::{
        keychain::{SecKeychain, SecPreferencesDomain},
        passwords::find_generic_password,
    };

    // The OS UI switch is process-wide. All credential reads/writes in this
    // module serialize; guards are never held across async suspension points.
    pub(super) static ACCESS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn error(error: security_framework::base::Error) -> CredentialStoreError {
        match error.code() {
            -25308 | -25293 | -128 => CredentialStoreError::AccessRequired,
            code => CredentialStoreError::Read(format!("macOS 凭据读取失败 (OSStatus {code})")),
        }
    }

    pub(super) fn read(
        keychain: Option<&SecKeychain>,
        service: &str,
        account: &str,
        interaction: CredentialInteraction,
    ) -> Result<Option<String>, CredentialStoreError> {
        let _access = ACCESS
            .lock()
            .map_err(|_| CredentialStoreError::Unavailable("凭据访问锁不可用".into()))?;
        // Preserve an already-disabled OS setting: the library guard enables UI
        // on Drop, so create it only when UI was previously enabled.
        let _ui_guard = if interaction == CredentialInteraction::Forbidden
            && SecKeychain::user_interaction_allowed().map_err(error)?
        {
            Some(SecKeychain::disable_user_interaction().map_err(error)?)
        } else {
            None
        };
        let default;
        let keychain = match keychain {
            Some(value) => value,
            None => {
                // Exactly the User domain used by apple-native-keyring-store.
                default =
                    SecKeychain::default_for_domain(SecPreferencesDomain::User).map_err(error)?;
                &default
            }
        };
        match find_generic_password(Some(std::slice::from_ref(keychain)), service, account) {
            Ok((bytes, _)) => String::from_utf8(bytes.to_owned()).map(Some).map_err(|_| {
                CredentialStoreError::Read("凭据不是有效 UTF-8；请运行 credentials set".into())
            }),
            Err(value) if value.code() == -25300 => Ok(None),
            Err(value) => Err(error(value)),
        }
    }
}

/// Explicit keychain adapter for isolated native acceptance; it shares the exact
/// UI policy and lookup implementation used for the user's default keychain.
#[cfg(target_os = "macos")]
pub struct MacKeychainCredentials(pub security_framework::os::macos::keychain::SecKeychain);

#[cfg(target_os = "macos")]
impl CredentialReader for MacKeychainCredentials {
    fn read(
        &self,
        service: &str,
        account: &str,
        interaction: CredentialInteraction,
    ) -> Result<Option<String>, CredentialStoreError> {
        macos::read(Some(&self.0), service, account, interaction)
    }
}

pub fn store_api_key(
    service: &str,
    account: &str,
    api_key: &str,
) -> Result<(), CredentialStoreError> {
    #[cfg(target_os = "macos")]
    let _access = macos::ACCESS
        .lock()
        .map_err(|_| CredentialStoreError::Unavailable("凭据访问锁不可用".into()))?;
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
