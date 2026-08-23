use std::{collections::BTreeMap, fs, path::Path};

use micu_image_mcp::installer::{
    ClientLaunchSpec, InstallError,
    atomic::replace_verified,
    binary::install_binary,
    claude::write_config_file as write_claude_config,
    codex::{parse_config_launch, write_config_file as write_codex_config},
};

#[test]
fn stable_binary_install_is_atomic_executable_and_idempotent() {
    let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
    let source = temp.path().join(if cfg!(windows) {
        "source.exe"
    } else {
        "source"
    });
    let destination = temp
        .path()
        .join("data-local/micu-image-mcp/bin")
        .join(if cfg!(windows) {
            "micu-image-mcp.exe"
        } else {
            "micu-image-mcp"
        });
    fs::write(&source, b"native-rust-binary-v1").unwrap_or_else(|error| panic!("{error}"));

    let first = install_binary(&source, &destination).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(first, destination);
    assert_eq!(
        fs::read(&destination).unwrap_or_else(|error| panic!("{error}")),
        b"native-rust-binary-v1"
    );
    assert!(destination.is_file());
    assert_executable(&destination);

    let second = install_binary(&source, &destination).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(second, destination);
    assert_eq!(
        fs::read(&destination).unwrap_or_else(|error| panic!("{error}")),
        b"native-rust-binary-v1"
    );

    fs::write(&source, b"native-rust-binary-v2").unwrap_or_else(|error| panic!("{error}"));
    let replaced = install_binary(&source, &destination).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(replaced, destination);
    assert_eq!(
        fs::read(&destination).unwrap_or_else(|error| panic!("{error}")),
        b"native-rust-binary-v2"
    );
    assert_executable(&destination);

    let leftovers = fs::read_dir(destination.parent().unwrap_or_else(|| Path::new(".")))
        .unwrap_or_else(|error| panic!("{error}"))
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".micu-binary-")
        })
        .count();
    assert_eq!(leftovers, 0);
}

#[test]
fn installing_an_already_stable_binary_is_a_noop() {
    let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
    let path = temp
        .path()
        .join(if cfg!(windows) { "micu.exe" } else { "micu" });
    fs::write(&path, b"same-file").unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        install_binary(&path, &path).unwrap_or_else(|error| panic!("{error}")),
        path
    );
}

#[test]
fn verified_client_config_writes_backup_reparse_and_preserve_original_on_failure() {
    let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o755))
            .unwrap_or_else(|error| panic!("{error}"));
    }
    let parent_permissions_before = fs::metadata(temp.path())
        .unwrap_or_else(|error| panic!("{error}"))
        .permissions();
    let codex = temp.path().join(".codex/config.toml");
    let claude = temp.path().join(".claude.json");
    fs::create_dir_all(codex.parent().unwrap_or_else(|| Path::new(".")))
        .unwrap_or_else(|error| panic!("{error}"));
    fs::write(
        &codex,
        "# keep\nmodel = 'gpt-test'\n\n[mcp_servers.other]\ncommand = 'other'\n",
    )
    .unwrap_or_else(|error| panic!("{error}"));
    fs::write(
        &claude,
        r#"{"theme":"dark","mcpServers":{"other":{"command":"other"}}}"#,
    )
    .unwrap_or_else(|error| panic!("{error}"));
    let launch = ClientLaunchSpec::new(
        Path::new("/stable/Micu MCP/micu-image-mcp").to_path_buf(),
        Vec::new(),
        BTreeMap::from([("MICU_SAVE_DIR".into(), "/tmp/图片".into())]),
    );

    let codex_report =
        write_codex_config(&codex, &launch).unwrap_or_else(|error| panic!("{error}"));
    let claude_report =
        write_claude_config(&claude, &launch).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        fs::metadata(temp.path())
            .unwrap_or_else(|error| panic!("{error}"))
            .permissions(),
        parent_permissions_before,
        "writing ~/.claude.json must not chmod the home directory"
    );
    assert!(
        codex_report
            .backup
            .as_ref()
            .is_some_and(|path| path.is_file())
    );
    assert!(
        claude_report
            .backup
            .as_ref()
            .is_some_and(|path| path.is_file())
    );
    let reparsed =
        parse_config_launch(&fs::read_to_string(&codex).unwrap_or_else(|error| panic!("{error}")))
            .unwrap_or_else(|error| panic!("{error}"))
            .unwrap_or_else(|| panic!("micu-image missing"));
    assert_eq!(reparsed.command(), launch.command());
    assert_eq!(reparsed.args(), launch.args());

    let original = fs::read(&codex).unwrap_or_else(|error| panic!("{error}"));
    let failed = replace_verified(&codex, b"not valid TOML", |_written| {
        Err(InstallError::CodexRoundTrip("forced mismatch".into()))
    })
    .expect_err("forced verification must fail");
    let failure = failed.to_string();
    assert!(failure.contains("target="));
    assert!(failure.contains("temp="));
    assert!(failure.contains("backup=None"));
    assert_eq!(
        fs::read(&codex).unwrap_or_else(|error| panic!("{error}")),
        original,
        "verification failure must not replace the original"
    );
    let leftovers = fs::read_dir(codex.parent().unwrap_or_else(|| Path::new(".")))
        .unwrap_or_else(|error| panic!("{error}"))
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".micu-config-")
        })
        .count();
    assert_eq!(leftovers, 0);
}

#[test]
fn invalid_existing_toml_is_never_overwritten_or_backed_up_as_success() {
    let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
    let path = temp.path().join("config.toml");
    let invalid = b"command = \"C:\\Python313\\python.exe\"\n";
    fs::write(&path, invalid).unwrap_or_else(|error| panic!("{error}"));
    let launch = ClientLaunchSpec::new(
        Path::new("/stable/micu-image-mcp").to_path_buf(),
        Vec::new(),
        BTreeMap::new(),
    );
    assert!(write_codex_config(&path, &launch).is_err());
    assert_eq!(
        fs::read(&path).unwrap_or_else(|error| panic!("{error}")),
        invalid
    );
    assert_eq!(
        fs::read_dir(temp.path())
            .unwrap_or_else(|error| panic!("{error}"))
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".bak."))
            .count(),
        0
    );
}

#[cfg(unix)]
fn assert_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    assert_ne!(
        fs::metadata(path)
            .unwrap_or_else(|error| panic!("{error}"))
            .permissions()
            .mode()
            & 0o111,
        0
    );
}

#[cfg(not(unix))]
fn assert_executable(_path: &Path) {}
