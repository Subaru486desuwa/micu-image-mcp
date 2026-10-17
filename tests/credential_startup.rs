use micu_devtools::{
    Result,
    stdio::{Session, isolated_command, tool_value},
};
use micu_image_mcp::{
    app::AppState,
    credentials::{CredentialInteraction, CredentialReader, CredentialStoreError},
    mcp_server::MicuServer,
};
use rmcp::ServiceExt;
use serde_json::json;

struct DeniedStore(std::sync::atomic::AtomicUsize);
impl CredentialReader for DeniedStore {
    fn read(
        &self,
        service: &str,
        account: &str,
        interaction: CredentialInteraction,
    ) -> std::result::Result<Option<String>, CredentialStoreError> {
        assert_eq!(
            interaction,
            CredentialInteraction::Forbidden,
            "background startup attempted authentication UI"
        );
        assert_eq!(
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
            0,
            "runtime retried credential lookup"
        );
        assert_eq!((service, account), ("custom-service", "custom-account"));
        match std::env::var("MICU_FIXTURE_STATE").as_deref() {
            Ok("available") => Ok(Some("sk-synthetic-existing-key-12345".into())),
            Ok("missing") => Ok(None),
            Ok("unavailable") => Err(CredentialStoreError::Unavailable("unavailable".into())),
            Ok("environment") => panic!("environment key must bypass system store"),
            _ => Err(CredentialStoreError::AccessRequired),
        }
    }
}

// A subprocess runs the actual application construction and MCP transport with only
// the operating-system credential boundary substituted. No product test env switch.
fn credential_fixture_server() -> Result<()> {
    if std::env::var("MICU_FIXTURE_SERVER").as_deref() != Ok("1") {
        return Ok(());
    }
    let runtime = tokio::runtime::Runtime::new()?;
    if std::env::var("MICU_FIXTURE_DOCTOR").as_deref() == Ok("1") {
        return micu_image_mcp::installer::doctor_with_credentials(&DeniedStore(
            std::sync::atomic::AtomicUsize::new(0),
        ))
        .map_err(Into::into);
    }
    runtime.block_on(async {
        let state =
            AppState::load_with_credentials(&DeniedStore(std::sync::atomic::AtomicUsize::new(0)))?;
        let server = MicuServer::new(state.tool_engine()?)?;
        server
            .serve((tokio::io::stdin(), tokio::io::stdout()))
            .await?
            .waiting()
            .await?;
        Ok(())
    })
}

fn repeated_background_startup_never_requests_credential_ui_and_reports_recovery() -> Result<()> {
    let root = tempfile::tempdir()?;
    for status in [
        "access_required",
        "access_required",
        "access_required",
        "available",
        "missing",
        "unavailable",
        "environment",
    ] {
        let mut command = isolated_command(&std::env::current_exe()?, root.path());
        command
            .env("MICU_FIXTURE_SERVER", "1")
            .env("MICU_FIXTURE_STATE", status)
            .env("MICU_KEYCHAIN_SERVICE", "custom-service")
            .env("MICU_KEYCHAIN_ACCOUNT", "custom-account");
        if status == "environment" {
            command.env("MICU_API_KEY", "sk-synthetic-environment-key-12345");
        }
        let mut server = Session::spawn(command)?;
        server.set_timeout(std::time::Duration::from_secs(3));
        if let Err(error) = server.initialize() {
            return Err(format!(
                "{error}: {}",
                String::from_utf8_lossy(&server.stderr_bytes())
            )
            .into());
        }
        let info = tool_value(
            &server.request("tools/call", json!({"name":"server_info","arguments":{}}))?,
        )?;
        let configured = matches!(status, "available" | "environment");
        assert_eq!(info["api_key_configured"], configured);
        assert_eq!(
            info["credentials"]["status"],
            if status == "available" {
                "system_store"
            } else {
                status
            }
        );
        assert!(!info.to_string().contains("sk-synthetic"));
        // Multiple informational requests do not re-query or retry the store.
        server.request("tools/call", json!({"name":"server_info","arguments":{}}))?;
        if configured {
            continue;
        }
        assert!(
            info["credentials"]["recovery"]
                .as_str()
                .unwrap()
                .contains("credentials authorize")
        );
        let generation = server.request("tools/call", json!({"name":"image_generate","arguments":{"prompt":"fixture", "model":"gpt-image-2.5-flare-openai"}}))?;
        assert!(generation.to_string().contains("credentials authorize"));
    }
    Ok(())
}

fn explicit_authorization_checks_background_access_and_never_retries_denial() -> Result<()> {
    use micu_image_mcp::credentials::authorize_credentials;
    use std::cell::RefCell;
    struct Store {
        calls: RefCell<Vec<CredentialInteraction>>,
        fail: Option<CredentialInteraction>,
    }
    impl CredentialReader for Store {
        fn read(
            &self,
            _: &str,
            _: &str,
            mode: CredentialInteraction,
        ) -> std::result::Result<Option<String>, CredentialStoreError> {
            self.calls.borrow_mut().push(mode);
            if self.fail == Some(mode) {
                Err(CredentialStoreError::AccessRequired)
            } else {
                Ok(Some("synthetic-secret".into()))
            }
        }
    }
    for fail in [
        Some(CredentialInteraction::Allowed),
        Some(CredentialInteraction::Forbidden),
        None,
    ] {
        let reader = Store {
            calls: RefCell::new(vec![]),
            fail,
        };
        assert_eq!(
            authorize_credentials(&reader, "s", "a").is_err(),
            fail.is_some()
        );
        let expected = if fail == Some(CredentialInteraction::Allowed) {
            vec![CredentialInteraction::Allowed]
        } else {
            vec![
                CredentialInteraction::Allowed,
                CredentialInteraction::Forbidden,
            ]
        };
        assert_eq!(*reader.calls.borrow(), expected);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let root = tempfile::tempdir()?;
        let result = isolated_command(
            std::path::Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")),
            root.path(),
        )
        .args(["credentials", "authorize"])
        .output()?;
        assert!(
            !result.status.success(),
            "missing store must not report authorization success"
        );
        assert!(!root.path().join(".codex/config.toml").exists());
        assert!(!root.path().join(".claude.json").exists());
    }
    Ok(())
}

fn doctor_reports_credential_failure_without_requesting_ui() -> Result<()> {
    let root = tempfile::tempdir()?;
    // No clients are installed: doctor's normal non-credential failure still
    // proves the system read was attempted with the background policy.
    let mut command = isolated_command(&std::env::current_exe()?, root.path());
    let result = command
        .env("MICU_FIXTURE_SERVER", "1")
        .env("MICU_FIXTURE_DOCTOR", "1")
        .env("MICU_KEYCHAIN_SERVICE", "custom-service")
        .env("MICU_KEYCHAIN_ACCOUNT", "custom-account")
        .output()?;
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("Codex/Claude"));
    assert!(!String::from_utf8_lossy(&result.stderr).contains("background startup attempted"));
    Ok(())
}

fn main() -> Result<()> {
    if std::env::var("MICU_FIXTURE_SERVER").as_deref() == Ok("1") {
        credential_fixture_server()
    } else {
        repeated_background_startup_never_requests_credential_ui_and_reports_recovery()?;
        doctor_reports_credential_failure_without_requesting_ui()?;
        explicit_authorization_checks_background_access_and_never_retries_denial()
    }
}
