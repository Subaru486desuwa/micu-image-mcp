use std::{collections::BTreeMap, fs, sync::Arc};

use bytes::Bytes;
use futures_util::stream;
use image::{ImageFormat, Rgb, RgbImage};
use micu_image_mcp::{
    config::{AppPaths, EnvironmentSnapshot, PathSource},
    fs::output_store::OutputStore,
};

fn path_text(path: &std::path::Path) -> String {
    path.to_str()
        .unwrap_or_else(|| panic!("test path must be Unicode"))
        .to_owned()
}

fn fixture() -> (tempfile::TempDir, AppPaths, OutputStore, Vec<u8>) {
    let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let data = temp.path().join("data");
    let root = temp.path().join("safe");
    let executable = temp
        .path()
        .join(if cfg!(windows) { "micu.exe" } else { "micu" });
    for directory in [&home, &cwd, &data] {
        fs::create_dir_all(directory).unwrap_or_else(|error| panic!("{error}"));
    }
    fs::write(&executable, b"binary").unwrap_or_else(|error| panic!("{error}"));
    let environment = EnvironmentSnapshot::from_map(BTreeMap::from([
        ("MICU_SAVE_DIR_ROOT".into(), path_text(&root)),
        ("MICU_SAVE_DIR".into(), path_text(&root)),
    ]));
    let paths = AppPaths::resolve(&environment, PathSource::new(home, cwd, executable, data))
        .unwrap_or_else(|error| panic!("{error}"));
    let store = OutputStore::new(&paths).unwrap_or_else(|error| panic!("{error}"));

    let image_path = temp.path().join("valid.png");
    RgbImage::from_pixel(32, 24, Rgb([1, 2, 3]))
        .save_with_format(&image_path, ImageFormat::Png)
        .unwrap_or_else(|error| panic!("{error}"));
    let image = fs::read(image_path).unwrap_or_else(|error| panic!("{error}"));
    (temp, paths, store, image)
}

#[test]
fn rejected_traversal_and_root_prefix_confusion_have_no_outside_side_effect() {
    let (temp, paths, store, _) = fixture();
    let outside = temp.path().join("safe2");
    for raw in [
        "../escape",
        "../../escape",
        &path_text(&outside),
        &path_text(&outside.join("deep/new")),
    ] {
        assert!(store.resolve_save_dir(Some(raw)).is_err(), "{raw}");
    }
    assert!(!outside.exists());
    assert_eq!(
        fs::read_dir(&paths.save_root)
            .unwrap_or_else(|error| panic!("{error}"))
            .count(),
        0
    );
}

#[test]
fn unicode_nonexistent_deep_directory_is_created_inside_root() {
    let (_temp, paths, store, _) = fixture();
    let location = store
        .resolve_save_dir(Some("不存在/深层/米醋 图像"))
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(location.absolute.is_dir());
    assert!(location.absolute.starts_with(&paths.save_root));
}

#[test]
fn concurrent_creation_of_the_same_directory_is_safe() {
    let (_temp, _paths, store, _) = fixture();
    let store = Arc::new(store);
    let threads = (0..8)
        .map(|_| {
            let store = store.clone();
            std::thread::spawn(move || store.resolve_save_dir(Some("shared/deep")))
        })
        .collect::<Vec<_>>();
    let resolved = threads
        .into_iter()
        .map(|thread| {
            thread
                .join()
                .unwrap_or_else(|_| panic!("thread panicked"))
                .unwrap_or_else(|error| panic!("{error}"))
        })
        .collect::<Vec<_>>();
    assert!(resolved.iter().all(|item| item == &resolved[0]));
    assert!(resolved[0].absolute.is_dir());
}

#[cfg(unix)]
#[test]
fn symlink_and_dangling_symlink_escape_are_rejected_without_creating_target() {
    use std::os::unix::fs::symlink;

    let (temp, paths, store, _) = fixture();
    let outside = temp.path().join("outside");
    fs::create_dir_all(&outside).unwrap_or_else(|error| panic!("{error}"));
    let link = paths.save_root.join("escape");
    symlink(&outside, &link).unwrap_or_else(|error| panic!("{error}"));
    assert!(store.resolve_save_dir(Some(&path_text(&link))).is_err());
    assert_eq!(
        fs::read_dir(&outside)
            .unwrap_or_else(|error| panic!("{error}"))
            .count(),
        0
    );

    let missing_target = temp.path().join("must-not-be-created");
    let dangling = paths.save_root.join("dangling");
    symlink(&missing_target, &dangling).unwrap_or_else(|error| panic!("{error}"));
    assert!(store.resolve_save_dir(Some(&path_text(&dangling))).is_err());
    assert!(!missing_target.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn replacing_resolved_directory_with_symlink_cannot_redirect_final_image() {
    use std::os::unix::fs::symlink;

    let (temp, _paths, store, image) = fixture();
    let location = store
        .resolve_save_dir(Some("race-target"))
        .unwrap_or_else(|error| panic!("{error}"));
    let outside = temp.path().join("outside-race");
    fs::create_dir_all(&outside).unwrap_or_else(|error| panic!("{error}"));
    fs::remove_dir(&location.absolute).unwrap_or_else(|error| panic!("{error}"));
    symlink(&outside, &location.absolute).unwrap_or_else(|error| panic!("{error}"));

    let result = store
        .save_stream(
            stream::iter([Ok::<_, String>(Bytes::from(image))]),
            None,
            &location,
            "race",
            "fixture",
        )
        .await;
    assert!(result.is_err());
    assert_eq!(
        fs::read_dir(&outside)
            .unwrap_or_else(|error| panic!("{error}"))
            .count(),
        0
    );
}

#[cfg(windows)]
#[test]
fn windows_junction_and_unc_escape_are_rejected() {
    let (temp, paths, store, _) = fixture();
    let outside = temp.path().join("outside-junction");
    fs::create_dir_all(&outside).unwrap_or_else(|error| panic!("{error}"));
    let junction = paths.save_root.join("junction");
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&junction)
        .arg(&outside)
        .status()
        .unwrap_or_else(|error| panic!("unable to create junction: {error}"));
    assert!(
        status.success(),
        "Windows runner must support junction regression test"
    );
    assert!(store.resolve_save_dir(Some(&path_text(&junction))).is_err());
    assert!(
        store
            .resolve_save_dir(Some(r"\\server\share\micu image"))
            .is_err()
    );
}

#[cfg(windows)]
#[test]
fn windows_unc_share_can_be_the_actual_output_capability_root() {
    struct ShareGuard(String);

    impl Drop for ShareGuard {
        fn drop(&mut self) {
            let _ignored = std::process::Command::new("net")
                .args(["share", &self.0, "/delete", "/y"])
                .status();
        }
    }

    let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
    let local_root = temp.path().join("UNC root 米醋");
    fs::create_dir_all(&local_root).unwrap_or_else(|error| panic!("{error}"));
    let share_name = format!("MicuMcp{}", std::process::id());
    let assignment = format!("{share_name}={}", path_text(&local_root));
    let status = std::process::Command::new("net")
        .args(["share", &assignment, "/GRANT:Everyone,FULL"])
        .status()
        .unwrap_or_else(|error| panic!("unable to create test share: {error}"));
    assert!(
        status.success(),
        "Windows runner must allow a temporary local SMB share"
    );
    let _share = ShareGuard(share_name.clone());

    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let data = temp.path().join("data");
    let executable = temp.path().join("micu.exe");
    for directory in [&home, &cwd, &data] {
        fs::create_dir_all(directory).unwrap_or_else(|error| panic!("{error}"));
    }
    fs::write(&executable, b"binary").unwrap_or_else(|error| panic!("{error}"));
    let unc_root = format!(r"\\localhost\{share_name}");
    let environment = EnvironmentSnapshot::from_map(BTreeMap::from([
        ("MICU_SAVE_DIR_ROOT".into(), unc_root),
        ("MICU_SAVE_DIR".into(), "nested output".into()),
    ]));
    let paths = AppPaths::resolve(&environment, PathSource::new(home, cwd, executable, data))
        .unwrap_or_else(|error| panic!("{error}"));
    let store = OutputStore::new(&paths).unwrap_or_else(|error| panic!("{error}"));
    let output = store
        .resolve_save_dir(Some("deeper/图片"))
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(paths.save_root.is_dir());
    assert!(paths.default_save_dir.is_dir());
    assert!(output.absolute.is_dir());
}
