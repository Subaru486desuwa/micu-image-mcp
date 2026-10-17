//! Native acceptance with synthetic secrets in one temporary keychain only.
//! Run from the candidate revision; never points at or locks the login keychain.
#![forbid(unsafe_code)]

#[cfg(any(target_os = "macos", test))]
mod checkout {
    use std::{path::Path, process::Command};

    pub fn verify(root: &Path, revision: &str) -> Result<(), &'static str> {
        let source_root = root
            .canonicalize()
            .map_err(|_| "cannot confirm source checkout")?;
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(&source_root)
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_INDEX_FILE")
                .args(args)
                .output()
                .map_err(|_| "cannot confirm source checkout")
        };
        let top = git(&["rev-parse", "--show-toplevel"])?;
        if !top.status.success()
            || Path::new(String::from_utf8_lossy(&top.stdout).trim())
                .canonicalize()
                .ok()
                .as_ref()
                != Some(&source_root)
        {
            return Err("cannot confirm source checkout");
        }
        let actual = git(&["rev-parse", "--verify", "HEAD^{commit}"])?;
        if !actual.status.success() || String::from_utf8_lossy(&actual.stdout).trim() != revision {
            return Err("revision does not match the current source checkout");
        }
        let state = git(&[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ])?;
        if !state.status.success() {
            return Err("cannot confirm source checkout cleanliness");
        }
        if !state.stdout.is_empty() {
            return Err("source checkout must be committed and clean before native acceptance");
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn git(root: &Path, args: &[&str]) -> String {
            let output = Command::new("git")
                .arg("-C")
                .arg(root)
                .args([
                    "-c",
                    "core.hooksPath=/dev/null",
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "user.name=Native Test",
                    "-c",
                    "user.email=native-test@example.invalid",
                ])
                .args(args)
                .output()
                .expect("fixture git invocation");
            assert!(output.status.success(), "fixture git failed");
            String::from_utf8(output.stdout).expect("fixture git output")
        }

        #[test]
        fn acceptance_requires_clean_committed_source_but_allows_ignored_build_artifacts() {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path();
            assert!(verify(root, "HEAD").is_err());
            git(root, &["init", "--quiet"]);
            assert!(verify(root, "HEAD").is_err());
            std::fs::write(root.join("source.rs"), "committed source").unwrap();
            std::fs::write(root.join(".gitignore"), "/target/\n").unwrap();
            git(root, &["add", "source.rs", ".gitignore"]);
            git(
                root,
                &["commit", "--quiet", "-m", "native acceptance fixture"],
            );
            let revision = git(root, &["rev-parse", "HEAD"]).trim().to_owned();
            assert!(verify(root, &revision).is_ok());
            assert!(
                verify(root, "HEAD").is_err(),
                "require the complete resolved revision"
            );
            std::fs::create_dir(root.join("target")).unwrap();
            std::fs::write(root.join("target/report.json"), "ignored artifact").unwrap();
            assert!(verify(root, &revision).is_ok());
            std::fs::write(root.join("untracked.rs"), "new source").unwrap();
            assert!(
                verify(root, &revision).is_err(),
                "untracked source must not inherit an old revision"
            );
            std::fs::remove_file(root.join("untracked.rs")).unwrap();
            std::fs::write(root.join("source.rs"), "modified source").unwrap();
            assert!(verify(root, &revision).is_err());
            git(root, &["add", "source.rs"]);
            assert!(verify(root, &revision).is_err());
            assert!(
                verify(root, "HEAD").is_err(),
                "require the complete resolved revision"
            );
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    Err("macos_keychain_probe requires a native Mac; Linux is not a UI acceptance result".into())
}

#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    native::run()
}

#[cfg(target_os = "macos")]
mod native {
    use clap::Parser;
    use micu_devtools::{
        Result,
        stdio::{Session, isolated_command, tool_value},
    };
    use micu_image_mcp::{
        app::AppState,
        credentials::{MacKeychainCredentials, authorize_credentials},
        mcp_server::MicuServer,
    };
    use rmcp::ServiceExt;
    use security_framework::os::macos::keychain::{CreateOptions, SecKeychain};
    use serde_json::json;
    use std::{
        io::Write,
        path::{Path, PathBuf},
        process::Command,
        time::Duration,
    };

    const ACCOUNT: &str = "image2-api-key";
    const KEY: &str = "sk-native-synthetic-key-not-valid-12345";
    const PASSWORD: &str = "synthetic-keychain-test-password-only";
    const ALLOWED: &str = micu_image_mcp::credentials::DEFAULT_CREDENTIAL_SERVICE;
    const DENIED: &str = "micu-native-denied-fixture";

    #[derive(Parser)]
    struct Options {
        /// Exact source revision from which this example was built.
        #[arg(long)]
        revision: Option<String>,
        /// Enable two expected, user-initiated dialogs: first Deny, then Always Allow.
        #[arg(long)]
        interactive: bool,
        #[arg(long, hide = true)]
        keychain: Option<PathBuf>,
        #[arg(long, hide = true)]
        authorize_fixture: bool,
    }

    fn security(args: &[&str], keychain: &Path) -> Result<()> {
        let output = Command::new("/usr/bin/security")
            .args(args)
            .arg(keychain)
            .output()?;
        if !output.status.success() {
            return Err("temporary keychain operation failed (details suppressed)".into());
        }
        Ok(())
    }

    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = Command::new("/usr/bin/security")
                .arg("delete-keychain")
                .arg(&self.0)
                .output();
        }
    }

    fn child(root: &Path, keychain: &Path, service: &str) -> Result<Command> {
        let mut command = isolated_command(&std::env::current_exe()?, root);
        command
            .arg("--keychain")
            .arg(keychain)
            .env("MICU_KEYCHAIN_SERVICE", service)
            .env("MICU_KEYCHAIN_ACCOUNT", ACCOUNT);
        Ok(command)
    }

    fn probe(root: &Path, keychain: &Path, service: &str, status: &str) -> Result<()> {
        // Restart repeatedly: a process-local refusal cache alone cannot pass.
        for _ in 0..3 {
            let mut session = Session::spawn(child(root, keychain, service)?)?;
            session.set_timeout(Duration::from_secs(5));
            session.initialize()?;
            for _ in 0..2 {
                let info = tool_value(
                    &session.request("tools/call", json!({"name":"server_info","arguments":{}}))?,
                )?;
                assert_eq!(info["credentials"]["status"], status);
                assert_eq!(info["api_key_configured"], status == "system_store");
                assert!(!info.to_string().contains(KEY));
            }
        }
        Ok(())
    }

    pub fn run() -> Result<()> {
        let options = Options::parse();
        if let Some(path) = &options.keychain {
            let reader = MacKeychainCredentials(SecKeychain::open(path)?);
            if options.authorize_fixture {
                let service = std::env::var("MICU_KEYCHAIN_SERVICE")?;
                authorize_credentials(&reader, &service, ACCOUNT)?;
                return Ok(());
            }
            return tokio::runtime::Runtime::new()?.block_on(async {
                let state = AppState::load_with_credentials(&reader)?;
                MicuServer::new(state.tool_engine()?)?
                    .serve((tokio::io::stdin(), tokio::io::stdout()))
                    .await?
                    .waiting()
                    .await?;
                Ok(())
            });
        }
        let revision = options
            .revision
            .ok_or("--revision is required for native acceptance evidence")?;
        super::checkout::verify(Path::new(env!("CARGO_MANIFEST_DIR")), &revision)?;
        let root = tempfile::tempdir()?;
        let path = root.path().join("micu-native-acceptance.keychain-db");
        let mut keychain = CreateOptions::new().password(PASSWORD).create(&path)?;
        let _cleanup = Cleanup(path.clone());
        keychain.set_generic_password(ALLOWED, ACCOUNT, KEY.as_bytes())?;
        probe(root.path(), &path, ALLOWED, "system_store")?;
        security(&["lock-keychain"], &path)?;
        probe(root.path(), &path, ALLOWED, "access_required")?;
        keychain.unlock(Some(PASSWORD))?;
        // A foreground authorization of an existing unlocked key must leave it
        // readable without UI, and a fresh background process must also work.
        let status = child(root.path(), &path, ALLOWED)?
            .arg("--authorize-fixture")
            .status()?;
        assert!(status.success());
        probe(root.path(), &path, ALLOWED, "system_store")?;
        // This synthetic entry trusts only /usr/bin/false, never this executable.
        // security is used only to create/lock/delete fixtures, never read keys.
        security(
            &[
                "add-generic-password",
                "-s",
                DENIED,
                "-a",
                ACCOUNT,
                "-T",
                "/usr/bin/false",
                "-w",
                KEY,
            ],
            &path,
        )?;
        probe(root.path(), &path, DENIED, "access_required")?;
        if !options.interactive {
            writeln!(
                std::io::stdout(),
                "{}",
                json!({"revision":revision,"version":env!("CARGO_PKG_VERSION"),"host":std::env::consts::ARCH,"background_existing_locked_denied_restarts":"passed","interactive_authorization":"not_run: rerun with --interactive"})
            )?;
            return Err("interactive cancellation and ACL recovery remain unverified; rerun with --interactive".into());
        }
        writeln!(
            std::io::stderr(),
            "仅针对临时合成凭据：接下来的显式授权窗口请点 Deny。"
        )?;
        let denied = child(root.path(), &path, DENIED)?
            .arg("--authorize-fixture")
            .status()?;
        assert!(!denied.success(), "cancelled authorization must fail once");
        probe(root.path(), &path, DENIED, "access_required")?;
        writeln!(
            std::io::stderr(),
            "仅针对临时合成凭据：接下来的显式授权窗口请点 Always Allow；如需密码，使用文档中的合成测试密码，勿输入 login 密码。"
        )?;
        let allowed = child(root.path(), &path, DENIED)?
            .arg("--authorize-fixture")
            .status()?;
        assert!(
            allowed.success(),
            "authorization did not leave noninteractive access available"
        );
        probe(root.path(), &path, DENIED, "system_store")?;
        security(&["delete-keychain"], &path)?;
        writeln!(
            std::io::stdout(),
            "{}",
            json!({"revision":revision,"version":env!("CARGO_PKG_VERSION"),"host":std::env::consts::ARCH,"native_keychain_acceptance":"passed","cleanup":"passed"})
        )?;
        Ok(())
    }
}
