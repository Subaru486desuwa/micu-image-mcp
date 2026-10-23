use std::{fs, process::Command};

#[test]
fn release_check_validates_tag_version_and_notes_without_an_interpreter() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("src")).unwrap();
    fs::create_dir_all(root.path().join("docs/releases")).unwrap();
    fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname = 'micu-image-mcp'\nversion = '0.4.1'\n",
    )
    .unwrap();
    fs::write(root.path().join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(root.path().join("docs/releases/v0.4.1.md"), "Release\n").unwrap();
    let run = |tag: &str| {
        Command::new(env!("CARGO_BIN_EXE_micu-devtools"))
            .args(["release-check", "--root"])
            .arg(root.path())
            .args(["--tag", tag])
            .output()
            .unwrap()
    };
    assert!(run("v0.4.1").status.success());
    assert!(!run("v0.4.2").status.success());
    assert!(!run("v0.4.1+build").status.success());
    fs::remove_file(root.path().join("docs/releases/v0.4.1.md")).unwrap();
    assert!(!run("v0.4.1").status.success());
}

#[test]
fn matrix_plan_is_offline_and_paid_modes_require_explicit_switches() {
    let binary = env!("CARGO_BIN_EXE_micu-devtools");
    let plan = Command::new(binary)
        .arg("quality-matrix")
        .env_remove("MICU_RUN_LIVE_TESTS")
        .output()
        .unwrap();
    assert!(plan.status.success());
    let value: serde_json::Value = serde_json::from_slice(&plan.stdout).unwrap();
    assert_eq!(value["cases"].as_array().unwrap().len(), 48);
    assert_eq!(value["paid_requests"], 0);
    assert_eq!(value["cases"][0]["model"], "gpt-image-2.5-flare");
    for model in ["gpt-image-2.5-flare", "gpt-image-2.5-flare-openai"] {
        let plan = Command::new(binary)
            .args(["quality-matrix", "--models", model])
            .env_remove("MICU_RUN_LIVE_TESTS")
            .output()
            .unwrap();
        assert!(plan.status.success());
        let value: serde_json::Value = serde_json::from_slice(&plan.stdout).unwrap();
        assert_eq!(value["paid_requests"], 0);
        let cases = value["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 14);
        assert!(
            cases
                .iter()
                .all(|case| case["model"] == "gpt-image-2.5-flare")
        );
    }
    let live = Command::new(binary)
        .args(["quality-matrix", "--live"])
        .env_remove("MICU_RUN_LIVE_TESTS")
        .output()
        .unwrap();
    assert!(!live.status.success());
    assert!(String::from_utf8_lossy(&live.stderr).contains("MICU_RUN_LIVE_TESTS"));
    let native = Command::new(binary)
        .args([
            "native-harness",
            "--cli",
            "missing",
            "--model",
            "fixture",
            "--output",
            "unused",
        ])
        .env_remove("MICU_RUN_HARNESS_TESTS")
        .output()
        .unwrap();
    assert!(!native.status.success());
    assert!(String::from_utf8_lossy(&native.stderr).contains("MICU_RUN_HARNESS_TESTS"));
    assert!(
        micu_devtools::live::Relay::new("https://untrusted.invalid", "secret", true, 1).is_err()
    );
}

#[test]
fn source_audit_rejects_interpreter_files_and_embedded_workflows() {
    let root = tempfile::tempdir().unwrap();
    assert!(
        Command::new("git")
            .arg("init")
            .arg(root.path())
            .output()
            .unwrap()
            .status
            .success()
    );
    fs::write(root.path().join("main.rs"), "fn main() {}\n").unwrap();
    assert!(micu_devtools::repository::source_audit(root.path()).is_ok());
    fs::write(root.path().join("hidden.py"), "pass\n").unwrap();
    assert!(micu_devtools::repository::source_audit(root.path()).is_err());
    fs::remove_file(root.path().join("hidden.py")).unwrap();
    for (name, contents) in [
        ("run.sh", "#!/bin/sh\npython legacy_runner.py\n"),
        ("probe", "#!/usr/bin/env python3.13\n"),
        ("run.ps1", "& 'C:/Python313/python.exe' legacy_runner.py\n"),
    ] {
        let path = root.path().join(name);
        fs::write(&path, contents).unwrap();
        assert!(
            micu_devtools::repository::source_audit(root.path()).is_err(),
            "{name}"
        );
        fs::remove_file(path).unwrap();
    }
    fs::create_dir_all(root.path().join(".github/workflows")).unwrap();
    fs::write(
        root.path().join(".github/workflows/test.yml"),
        "shell: python\n",
    )
    .unwrap();
    assert!(micu_devtools::repository::source_audit(root.path()).is_err());
}

#[test]
fn reports_redact_nested_credentials_and_never_keep_arbitrary_tool_errors() {
    use serde_json::json;
    let secret = "fixture-private-value";
    let report = micu_devtools::clients::redact(
        json!({"nested":[{"error":format!("credential: {secret}")}]}),
        &[secret],
    );
    assert!(!report.to_string().contains(secret));
    let summary = micu_devtools::audit::tool_summary(
        &json!({"ok":false,"error":secret,"prompt":secret,"session":{"context":{"private":secret,"nested":{"prompt":secret}}}}),
    );
    assert!(!summary.to_string().contains(secret));
    assert_eq!(
        micu_devtools::audit::tool_summary(
            &json!({"session":{"context":{"location":"vacation by sea"}}})
        )["session"]["context_contains_previous_subject"],
        false
    );
}

#[test]
fn rust_audit_proxy_records_a_real_restartable_four_turn_session_without_prompts()
-> micu_devtools::Result<()> {
    use micu_devtools::{
        mock::MockMicu,
        stdio::{Session, isolated_command},
    };
    use serde_json::json;
    use std::path::PathBuf;
    let binary = std::env::var_os("MICU_TEST_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../target/debug")
                .join(format!("micu-image-mcp{}", std::env::consts::EXE_SUFFIX))
        })
        .canonicalize()?;
    let root = tempfile::tempdir()?;
    let api = MockMicu::exact_images()?;
    let audit = root.path().join("audit.jsonl");
    let mut ids: Vec<serde_json::Value> = Vec::new();
    for (index, tool) in [
        "image_generate",
        "image_edit",
        "image_generate",
        "image_edit",
    ]
    .iter()
    .enumerate()
    {
        let mut command = isolated_command(
            std::path::Path::new(env!("CARGO_BIN_EXE_micu-devtools")),
            root.path(),
        );
        command
            .envs(api.env())
            .args(["audit-proxy", "--binary"])
            .arg(&binary)
            .arg("--audit")
            .arg(&audit);
        let mut server = Session::spawn(command)?;
        server.initialize()?;
        let result=server.call(tool,json!({"prompt":"private-prompt-must-not-be-in-audit","session_id":if index==1 || index==3 {ids[0].clone()} else {json!("new")},"size":"1024x1024","quality":"low"}))?;
        assert_eq!(result["ok"], true);
        ids.push(result["session"]["id"].clone());
    }
    let events = micu_devtools::clients::read_audit(&audit)?;
    assert_eq!(micu_devtools::harness::validate(&events)?["ok"], true);
    let raw = fs::read_to_string(&audit)?;
    assert!(!raw.contains("private-prompt-must-not-be-in-audit"));
    assert!(!raw.contains(micu_devtools::mock::FIXTURE_KEY));
    Ok(())
}

#[test]
fn live_stress_guards_and_authentication_stop_distinguish_quality_rejections() {
    use serde_json::json;
    let run = |highres: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_micu-devtools"));
        command.args(["live-stress", "--output", "unused"]);
        if highres {
            command.arg("--highres");
        }
        command
            .env_remove("MICU_RUN_LIVE_TESTS")
            .env_remove("MICU_RUN_LIVE_STRESS")
            .env_remove("MICU_RUN_LIVE_HIGHRES_STRESS")
            .output()
            .unwrap()
    };
    assert!(!run(false).status.success());
    assert!(!run(true).status.success());
    assert!(micu_devtools::live::authentication_failed(&[
        json!({"status":401})
    ]));
    assert!(micu_devtools::live::authentication_failed(&[
        json!({"status":403})
    ]));
    assert!(!micu_devtools::live::authentication_failed(&[
        json!({"status":400})
    ]));
}

#[test]
fn concurrency_fixture_releases_late_requests_after_observing_five_overlapping_requests()
-> micu_devtools::Result<()> {
    use std::io::{Read, Write};
    let api = micu_devtools::mock::MockMicu::new()?;
    let send = || -> micu_devtools::Result<()> {
        let address = api.http.url.strip_prefix("http://").unwrap();
        let mut stream = std::net::TcpStream::connect(address)?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
        let body = r#"{"prompt":"[scenario:concurrency_five] fixture"}"#;
        write!(
            stream,
            "POST /v1/images/generations HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            micu_devtools::mock::FIXTURE_KEY,
            body.len()
        )?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response)?;
        assert!(response.starts_with(b"HTTP/1.1 200"));
        Ok(())
    };
    std::thread::scope(|scope| {
        let jobs: Vec<_> = (0..5).map(|_| scope.spawn(send)).collect();
        for job in jobs {
            job.join().expect("fixture request worker")?;
        }
        Ok::<_, micu_devtools::Error>(())
    })?;
    let began = std::time::Instant::now();
    send()?;
    assert!(began.elapsed() < std::time::Duration::from_secs(1));
    assert!(api.metrics().max_active >= 5);
    assert_eq!(api.requests().len(), 6);
    Ok(())
}

#[test]
fn partial_http_request_lines_are_reported_instead_of_treated_as_unused_connections()
-> micu_devtools::Result<()> {
    use std::io::{Read, Write};
    let api = micu_devtools::mock::MockMicu::new()?;
    let mut stream = std::net::TcpStream::connect(api.http.url.trim_start_matches("http://"))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;
    stream.write_all(b"POST /v1/images/generations HTTP/1.1")?;
    stream.shutdown(std::net::Shutdown::Write)?;
    assert_eq!(stream.read_to_end(&mut Vec::new())?, 0);
    let began = std::time::Instant::now();
    while api.http.errors().is_empty() && began.elapsed() < std::time::Duration::from_secs(1) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(api.requests().is_empty());
    assert_eq!(api.http.errors().len(), 1);
    Ok(())
}

#[test]
fn bursty_persistent_http_clients_do_not_starve_fixture_connection_workers()
-> micu_devtools::Result<()> {
    let api = micu_devtools::mock::MockMicu::new()?;
    // Connection pools may open then discard a socket without sending an HTTP request.
    for _ in 0..3 {
        drop(std::net::TcpStream::connect(
            api.http.url.trim_start_matches("http://"),
        )?);
    }
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(5))
        .build()?;
    let barrier = std::sync::Barrier::new(20);
    let send = || -> micu_devtools::Result<()> {
        barrier.wait();
        let response = client
            .post(format!("{}/v1/images/generations", api.http.url))
            .bearer_auth(micu_devtools::mock::FIXTURE_KEY)
            .json(&serde_json::json!({"prompt":"[scenario:concurrency_five] fixture"}))
            .send()?;
        assert!(response.status().is_success());
        assert_eq!(response.headers()["connection"], "close");
        assert!(!response.bytes()?.is_empty());
        Ok(())
    };
    std::thread::scope(|scope| {
        let jobs: Vec<_> = (0..20).map(|_| scope.spawn(send)).collect();
        for job in jobs {
            job.join().expect("burst request worker")?;
        }
        Ok::<_, micu_devtools::Error>(())
    })?;
    assert!(api.metrics().max_active >= 5);
    assert_eq!(api.requests().len(), 20);
    assert!(api.http.errors().is_empty(), "{:?}", api.http.errors());
    Ok(())
}
