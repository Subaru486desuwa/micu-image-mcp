use std::{
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

use micu_image_mcp::locks::BigRequestGate;

#[test]
fn lock_probe_child() {
    if std::env::var("MICU_LOCK_CHILD").ok().as_deref() != Some("1") {
        return;
    }
    let path = std::env::var_os("MICU_LOCK_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("MICU_LOCK_PATH is required"));
    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|error| panic!("{error}"));
    runtime.block_on(async move {
        let gate = BigRequestGate::new(path);
        let _guard = gate
            .acquire(&mut Vec::new())
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        tokio::time::sleep(Duration::from_millis(400)).await;
    });
}

#[test]
fn separate_processes_serialize_on_the_same_lock_file() {
    if std::env::var("MICU_LOCK_CHILD").ok().as_deref() == Some("1") {
        return;
    }
    let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
    let lock_path = temp.path().join("multiprocess.lock");
    let executable = std::env::current_exe().unwrap_or_else(|error| panic!("{error}"));
    let started = Instant::now();
    let mut first = Command::new(&executable)
        .args(["--exact", "lock_probe_child", "--nocapture"])
        .env("MICU_LOCK_CHILD", "1")
        .env("MICU_LOCK_PATH", &lock_path)
        .spawn()
        .unwrap_or_else(|error| panic!("{error}"));
    let mut second = Command::new(&executable)
        .args(["--exact", "lock_probe_child", "--nocapture"])
        .env("MICU_LOCK_CHILD", "1")
        .env("MICU_LOCK_PATH", &lock_path)
        .spawn()
        .unwrap_or_else(|error| panic!("{error}"));
    let first_status = first.wait().unwrap_or_else(|error| panic!("{error}"));
    let second_status = second.wait().unwrap_or_else(|error| panic!("{error}"));
    assert!(first_status.success() && second_status.success());
    assert!(
        started.elapsed() >= Duration::from_millis(750),
        "two 400ms critical sections overlapped: {:?}",
        started.elapsed()
    );
}
