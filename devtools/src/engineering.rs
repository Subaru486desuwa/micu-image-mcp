use crate::{
    Result,
    mock::{FIXTURE_KEY, MockMicu},
    stdio::{Session, tool_value},
};
use serde_json::{Value, json};
use std::{fs, path::Path, thread, time::Instant};

pub fn provenance(binary: &Path) -> Value {
    use sha2::Digest;
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    json!({"binary":binary,"os":std::env::consts::OS,"arch":std::env::consts::ARCH,
        "binary_sha256":fs::read(binary).ok().map(|bytes|sha2::Sha256::digest(bytes).iter().map(|byte|format!("{byte:02x}")).collect::<String>()),
        "rustc":std::process::Command::new("rustc").arg("--version").output().ok().map(|output|String::from_utf8_lossy(&output.stdout).trim().to_owned()),
        "profile":if binary.components().any(|part|part.as_os_str()=="release") {"release"} else {"unspecified"},
        "commit":git(&["rev-parse","HEAD"]),"working_tree_dirty":git(&["status","--porcelain"]).map(|status|!status.is_empty())})
}

pub fn write_report(path: &Path, report: &Value) -> Result<()> {
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    serde_json::to_writer_pretty(&mut file, report)?;
    std::io::Write::write_all(&mut file, b"\n")?;
    Ok(())
}

pub fn smoke(binary: &Path) -> Result<Value> {
    let root = tempfile::tempdir()?;
    let api = MockMicu::new()?;
    let mut server = Session::new(binary, root.path(), &api.env())?;
    server.initialize()?;
    let tools = server.request("tools/list", json!({}))?;
    if tools["result"]["tools"].as_array().map(Vec::len) != Some(5) {
        return Err("expected five MCP tools".into());
    }
    let info = server.call("server_info", json!({}))?;
    let first = server.call(
        "image_generate",
        json!({"prompt":"offline smoke","session_id":"new"}),
    )?;
    if first["ok"] != true {
        return Err("offline generation failed".into());
    }
    let id = first["session"]["id"].clone();
    drop(server);
    let mut restarted = Session::new(binary, root.path(), &api.env())?;
    restarted.initialize()?;
    let edited = restarted.call(
        "image_edit",
        json!({"prompt":"keep subject, change background","session_id":id}),
    )?;
    if edited["session"]["revision"] != 2 {
        return Err("offline session restart failed".into());
    }
    Ok(
        json!({"provenance":provenance(binary),"tools":5,"available_models":info["available_models"],"session_revisions":2,"paid_requests":0,"passed":true}),
    )
}

pub fn stress(binary: &Path, processes: usize, rounds: usize) -> Result<Value> {
    if !(1..=32).contains(&processes) || !(1..=100).contains(&rounds) {
        return Err("processes must be 1..32 and rounds 1..100".into());
    }
    let mut phases = Vec::new();
    for (name, model, size, images) in [
        ("standard-1k", "gpt-image-2", "1024x1024", 5),
        (
            "quality-cross-process-lock",
            "gpt-image-2-openai",
            "2048x2048",
            1,
        ),
    ] {
        let root = tempfile::tempdir()?;
        let api = MockMicu::new()?;
        let began = Instant::now();
        let mut saved = 0;
        let mut frames = 0;
        for _ in 0..rounds {
            let outcomes = thread::scope(|scope| {
                let jobs: Vec<_> = (0..processes)
                    .map(|index| {
                        let root = root.path();
                        let api = &api;
                        scope.spawn(move || -> Result<(usize, usize)> {
                            let destination = root.join(format!("process-{index}"));
                            fs::create_dir_all(&destination)?;
                            let mut env = api.env();
                            env.push((
                                "MICU_SAVE_DIR",
                                destination.to_str().ok_or("invalid fixture path")?,
                            ));
                            let mut server = Session::new(binary, root, &env)?;
                            server.initialize()?;
                            let prompt = if images == 5 {
                                "[scenario:concurrency_five] stress"
                            } else {
                                "[scenario:concurrency_probe] stress"
                            };
                            let result = server.call(
                                "image_generate",
                                json!({"prompt":prompt,"model":model,"size":size,"n":images}),
                            )?;
                            if result["ok"] != true
                                || result["saved"].as_array().map(Vec::len) != Some(images)
                            {
                                return Err("stress generation failed".into());
                            }
                            if String::from_utf8_lossy(&server.stderr_bytes()).contains(FIXTURE_KEY)
                            {
                                return Err("stderr leaked fixture credential".into());
                            }
                            Ok((images, server.frames))
                        })
                    })
                    .collect();
                jobs.into_iter()
                    .map(|job| {
                        job.join()
                            .map_err(|_| "stress worker panicked".into())
                            .and_then(|result| result)
                    })
                    .collect::<Result<Vec<_>>>()
            })?;
            for (count, rpc) in outcomes {
                saved += count;
                frames += rpc;
            }
        }
        let expected = processes * rounds * images;
        if images == 1 && !root.path().join(".cache/micu-image/bigsize.lock").is_file() {
            return Err(
                "high-resolution requests did not use the default cross-process lock path".into(),
            );
        }
        let metrics = api.metrics();
        let captured = api.requests().len();
        if saved != expected
            || captured != expected
            || (images == 1 && metrics.max_active != 1)
            || (images == 5 && metrics.max_active < 5)
        {
            return Err(format!("stress invariant failed in {name}: saved={saved}/{expected}, requests={captured}, max_active={}",metrics.max_active).into());
        }
        phases.push(json!({"name":name,"processes":processes,"rounds":rounds,"expected_requests":expected,"captured_requests":captured,
            "saved_images":saved,"stdout_frames":frames,"max_active":metrics.max_active,"wall_seconds":began.elapsed().as_secs_f64()}));
    }
    Ok(json!({"provenance":provenance(binary),"paid_requests":0,"passed":true,"phases":phases}))
}

pub fn session_concurrency(binary: &Path, repeats: usize) -> Result<Value> {
    if !(1..=100).contains(&repeats) {
        return Err("repeat must be 1..100".into());
    }
    let mut samples = Vec::new();
    for workers in [1, 2, 4] {
        for _ in 0..repeats {
            let root = tempfile::tempdir()?;
            let api = MockMicu::new()?;
            let worker_text = workers.to_string();
            let mut env = api.env();
            env.push(("TOKIO_WORKER_THREADS", &worker_text));
            let mut server = Session::new(binary, root.path(), &env)?;
            server.initialize()?;
            let args = json!({"prompt":"[scenario:concurrency_probe] portrait","session_id":"new"});
            let started = Instant::now();
            let mut ids = Vec::new();
            for _ in 0..4 {
                let result = server.call("image_generate", args.clone())?;
                ids.push(result["session"]["id"].clone());
            }
            let sequential = started.elapsed().as_secs_f64() * 1000.0;
            let started = Instant::now();
            let requests: Vec<_> = (0..4)
                .map(|_| {
                    server.send(
                        "tools/call",
                        json!({"name":"image_generate","arguments":args}),
                    )
                })
                .collect::<Result<_>>()?;
            for id in requests {
                let result = tool_value(&server.wait(id)?)?;
                if result["ok"] != true {
                    return Err("parallel session failed".into());
                }
                ids.push(result["session"]["id"].clone());
            }
            let parallel = started.elapsed().as_secs_f64() * 1000.0;
            let unique: std::collections::HashSet<_> =
                ids.iter().filter_map(Value::as_str).collect();
            if unique.len() != 8 {
                return Err("independent sessions collided".into());
            }
            let params =
                json!({"name":"image_edit","arguments":{"prompt":"continue","session_id":ids[4]}});
            let a = server.send("tools/call", params.clone())?;
            let b = server.send("tools/call", params)?;
            let a = tool_value(&server.wait(a)?)?;
            let b = tool_value(&server.wait(b)?)?;
            if (a["ok"] == true) == (b["ok"] == true) || api.requests().len() != 9 {
                return Err("same-session overlap did not yield one successful edit".into());
            }
            samples.push(json!({"workers":workers,"sequential_four_ms":sequential,"parallel_four_ms":parallel,"independent_sessions":8,"max_active":api.metrics().max_active,"same_session_overlap":"one success, one busy, one request"}));
        }
    }
    Ok(
        json!({"provenance":provenance(binary),"injected_api_delay_ms":200,"paid_requests":0,"samples":samples}),
    )
}
