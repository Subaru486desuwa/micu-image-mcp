use std::{collections::BTreeMap, fs};

#[cfg(windows)]
use std::path::PathBuf;

use micu_image_mcp::config::{AppPaths, EnvironmentSnapshot, PathPolicy, PathSource};

fn path_text(path: &std::path::Path) -> String {
    path.to_str()
        .unwrap_or_else(|| panic!("test temp path must be Unicode"))
        .to_owned()
}

fn source(temp: &tempfile::TempDir) -> PathSource {
    let home = temp.path().join("home");
    let startup_cwd = temp.path().join("startup-cwd");
    let data_local = temp.path().join("data-local");
    let executable = temp.path().join(if cfg!(windows) {
        "micu-image-mcp.exe"
    } else {
        "micu-image-mcp"
    });
    fs::create_dir_all(&home).unwrap_or_else(|error| panic!("{error}"));
    fs::create_dir_all(&startup_cwd).unwrap_or_else(|error| panic!("{error}"));
    fs::create_dir_all(&data_local).unwrap_or_else(|error| panic!("{error}"));
    fs::write(&executable, b"binary").unwrap_or_else(|error| panic!("{error}"));
    PathSource::new(home, startup_cwd, executable, data_local)
}

fn environment(entries: impl IntoIterator<Item = (&'static str, String)>) -> EnvironmentSnapshot {
    EnvironmentSnapshot::from_map(
        entries
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect::<BTreeMap<_, _>>(),
    )
}

#[test]
fn app_paths_resolve_once_from_injected_sources_without_ambient_home_or_cwd() {
    let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
    let source = source(&temp);
    let paths = AppPaths::resolve(&environment([]), source.clone())
        .unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(
        paths.home,
        fs::canonicalize(&source.home).unwrap_or_else(|error| panic!("{error}"))
    );
    assert_eq!(
        paths.startup_cwd,
        fs::canonicalize(&source.startup_cwd).unwrap_or_else(|error| panic!("{error}"))
    );
    assert_eq!(
        paths.executable,
        fs::canonicalize(&source.executable).unwrap_or_else(|error| panic!("{error}"))
    );
    assert_eq!(
        paths.save_root,
        fs::canonicalize(source.home.join("Pictures/micu-out"))
            .unwrap_or_else(|error| panic!("{error}"))
    );
    assert_eq!(paths.default_save_dir, paths.save_root);
    assert_eq!(paths.input_root, None);
    assert_eq!(paths.cache_dir, paths.home.join(".cache/micu-image"));
    assert_eq!(paths.lock_file, paths.cache_dir.join("bigsize.lock"));
    assert_eq!(paths.codex_config, paths.home.join(".codex/config.toml"));
    assert_eq!(paths.claude_config, paths.home.join(".claude.json"));
    assert_eq!(
        paths.install_binary,
        fs::canonicalize(&source.data_local)
            .unwrap_or_else(|error| panic!("{error}"))
            .join("micu-image-mcp/bin")
            .join(if cfg!(windows) {
                "micu-image-mcp.exe"
            } else {
                "micu-image-mcp"
            })
    );
    assert!(paths.save_root.is_dir());
}

#[test]
fn save_root_is_home_based_and_save_dir_is_root_based_never_cwd_based() {
    let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
    let source = source(&temp);
    let paths = AppPaths::resolve(
        &environment([
            ("MICU_SAVE_DIR_ROOT", "relative-root".into()),
            ("MICU_SAVE_DIR", "nested/output".into()),
        ]),
        source.clone(),
    )
    .unwrap_or_else(|error| panic!("{error}"));

    let canonical_root = fs::canonicalize(source.home.join("relative-root"))
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(paths.save_root, canonical_root);
    assert_eq!(
        paths.default_save_dir,
        fs::canonicalize(source.home.join("relative-root/nested/output"))
            .unwrap_or_else(|error| panic!("{error}"))
    );
    assert!(!paths.save_root.starts_with(&source.startup_cwd));
    assert!(paths.default_save_dir.is_dir());
}

#[test]
fn save_policy_resolves_relative_paths_under_root_and_rejects_escape_without_side_effect() {
    let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
    let source = source(&temp);
    let paths =
        AppPaths::resolve(&environment([]), source).unwrap_or_else(|error| panic!("{error}"));
    let policy = PathPolicy::new(&paths);

    assert_eq!(
        policy
            .resolve_save_dir(Some("nested/中文"))
            .unwrap_or_else(|error| panic!("{error}")),
        paths.save_root.join("nested/中文")
    );
    assert_eq!(
        policy
            .resolve_save_dir(Some(&path_text(&paths.save_root.join("absolute"))))
            .unwrap_or_else(|error| panic!("{error}")),
        paths.save_root.join("absolute")
    );

    let outside = temp.path().join("escape-created-by-bug");
    let traversal = format!(
        "../../{}",
        outside.file_name().unwrap_or_default().to_string_lossy()
    );
    assert!(policy.resolve_save_dir(Some(&traversal)).is_err());
    assert!(
        !outside.exists(),
        "rejected save path must have no side effect"
    );
    assert!(
        policy
            .resolve_save_dir(Some(&path_text(&temp.path().join("outside"))))
            .is_err()
    );
}

#[test]
fn input_root_controls_relative_and_absolute_inputs() {
    let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
    let source = source(&temp);
    let input_root = source.home.join("inputs");
    fs::create_dir_all(&input_root).unwrap_or_else(|error| panic!("{error}"));
    let paths = AppPaths::resolve(&environment([("MICU_INPUT_ROOT", "inputs".into())]), source)
        .unwrap_or_else(|error| panic!("{error}"));
    let policy = PathPolicy::new(&paths);
    let input_root = fs::canonicalize(input_root).unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(
        policy
            .resolve_input_path("relative.png")
            .unwrap_or_else(|error| panic!("{error}")),
        input_root.join("relative.png")
    );
    assert_eq!(
        policy
            .resolve_input_path(&path_text(&input_root.join("absolute.png")))
            .unwrap_or_else(|error| panic!("{error}")),
        input_root.join("absolute.png")
    );
    assert!(
        policy
            .resolve_input_path(&path_text(&temp.path().join("outside.png")))
            .is_err()
    );
}

#[test]
fn input_without_root_uses_captured_startup_cwd() {
    let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
    let source = source(&temp);
    let paths = AppPaths::resolve(&environment([]), source.clone())
        .unwrap_or_else(|error| panic!("{error}"));
    let policy = PathPolicy::new(&paths);
    assert_eq!(
        policy
            .resolve_input_path("relative.png")
            .unwrap_or_else(|error| panic!("{error}")),
        fs::canonicalize(source.startup_cwd)
            .unwrap_or_else(|error| panic!("{error}"))
            .join("relative.png")
    );
}

#[test]
fn tilde_expansion_is_exact_and_rejects_named_user_forms() {
    let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
    let source = source(&temp);
    let paths = AppPaths::resolve(
        &environment([("MICU_SAVE_DIR_ROOT", "~/safe".into())]),
        source.clone(),
    )
    .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        paths.save_root,
        fs::canonicalize(source.home.join("safe")).unwrap_or_else(|error| panic!("{error}"))
    );

    let error = AppPaths::resolve(
        &environment([("MICU_SAVE_DIR_ROOT", "~someone/output".into())]),
        source,
    )
    .expect_err("~someone must not be appended to current home");
    assert!(error.to_string().contains("~someone"));
}

#[cfg(windows)]
#[test]
fn windows_root_checks_are_component_aware_and_case_insensitive() {
    let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
    let source = source(&temp);
    let root = source.home.join("Safe");
    let paths = AppPaths::resolve(
        &environment([("MICU_SAVE_DIR_ROOT", path_text(&root))]),
        source,
    )
    .unwrap_or_else(|error| panic!("{error}"));
    let policy = PathPolicy::new(&paths);
    let different_case = PathBuf::from(path_text(&root).to_ascii_lowercase()).join("child");
    assert!(
        policy
            .resolve_save_dir(Some(&path_text(&different_case)))
            .is_ok()
    );
    let confused = root.with_file_name("Safe2").join("child");
    assert!(
        policy
            .resolve_save_dir(Some(&path_text(&confused)))
            .is_err()
    );
}
