use micu_devtools::{Result, stdio::isolated_command};
use serde_json::{Value, json};
use std::{fs, path::Path};

#[test]
fn invalid_key_is_rejected_before_installer_writes_configuration() -> Result<()> {
    let root = tempfile::tempdir()?;
    let secret = "not-a-valid-fixture-secret";
    let output = isolated_command(Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")), root.path())
        .env("MICU_API_KEY", secret)
        .args(["install", "--yes"])
        .output()?;
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains(secret));
    assert!(!root.path().join(".claude.json").exists());
    assert!(!root.path().join(".codex/config.toml").exists());
    Ok(())
}

#[test]
fn actual_install_doctor_and_reset_preserve_other_configuration_and_back_up_files() -> Result<()> {
    let root = tempfile::tempdir()?;
    let binary = Path::new(env!("CARGO_BIN_EXE_micu-image-mcp"));
    fs::create_dir(root.path().join(".codex"))?;
    fs::write(
        root.path().join(".claude.json"),
        json!({"theme":"dark","mcpServers":{"other":{"command":"other"}}}).to_string(),
    )?;
    fs::write(
        root.path().join(".codex/config.toml"),
        "model = 'gpt-test'\n[mcp_servers.other]\ncommand = 'other'\n",
    )?;
    let install = isolated_command(binary, root.path())
        .args(["install", "--yes"])
        .output()?;
    assert!(install.status.success(), "installer failed");
    let claude: Value = serde_json::from_slice(&fs::read(root.path().join(".claude.json"))?)?;
    assert_eq!(claude["theme"], "dark");
    assert_eq!(claude["mcpServers"]["other"]["command"], "other");
    let command = Path::new(
        claude["mcpServers"]["micu-image"]["command"]
            .as_str()
            .unwrap(),
    );
    assert!(command.is_file());
    assert!(!command.starts_with(Path::new(env!("CARGO_MANIFEST_DIR")).join("target")));
    assert_eq!(claude["mcpServers"]["micu-image"]["args"], json!([]));
    assert!(
        claude["mcpServers"]["micu-image"]["env"]
            .get("MICU_API_KEY")
            .is_none()
    );
    assert!(
        isolated_command(binary, root.path())
            .arg("doctor")
            .output()?
            .status
            .success()
    );
    assert!(
        isolated_command(binary, root.path())
            .args(["reset", "--yes"])
            .output()?
            .status
            .success()
    );
    let claude: Value = serde_json::from_slice(&fs::read(root.path().join(".claude.json"))?)?;
    assert!(claude["mcpServers"].get("micu-image").is_none());
    assert_eq!(claude["mcpServers"]["other"]["command"], "other");
    let codex = fs::read_to_string(root.path().join(".codex/config.toml"))?;
    assert!(codex.contains("[mcp_servers.other]"));
    assert!(!codex.contains("mcp_servers.micu-image"));
    assert!(
        fs::read_dir(root.path())?
            .filter_map(std::result::Result::ok)
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with(".claude.json.bak."))
    );
    Ok(())
}

#[test]
fn dev_install_is_the_explicit_opt_out_from_a_stable_binary() -> Result<()> {
    let root = tempfile::tempdir()?;
    let binary = Path::new(env!("CARGO_BIN_EXE_micu-image-mcp"));
    let output = isolated_command(binary, root.path())
        .args(["install", "--yes", "--no-claude", "--dev", "--binary-path"])
        .arg(binary)
        .output()?;
    assert!(output.status.success());
    let codex: toml_edit::DocumentMut =
        fs::read_to_string(root.path().join(".codex/config.toml"))?.parse()?;
    assert_eq!(
        Path::new(
            codex["mcp_servers"]["micu-image"]["command"]
                .as_str()
                .unwrap()
        ),
        binary
    );
    Ok(())
}
