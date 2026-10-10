use crate::{
    Result,
    engineering::provenance,
    mock::{MockMicu, png},
    stdio::Session,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

fn rss(pid: u32) -> Option<u64> {
    if cfg!(target_os = "linux") {
        fs::read_to_string(format!("/proc/{pid}/status"))
            .ok()?
            .lines()
            .find_map(|line| {
                line.strip_prefix("VmRSS:")?
                    .split_whitespace()
                    .next()?
                    .parse::<u64>()
                    .ok()
                    .map(|kb| kb * 1024)
            })
    } else if cfg!(target_os = "macos") {
        let output = Command::new("ps")
            .args(["-o", "rss=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        String::from_utf8(output.stdout)
            .ok()?
            .trim()
            .parse::<u64>()
            .ok()
            .map(|kb| kb * 1024)
    } else if cfg!(target_os = "windows") {
        let output = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!("(Get-Process -Id {pid}).WorkingSet64"),
            ])
            .output()
            .ok()?;
        String::from_utf8(output.stdout).ok()?.trim().parse().ok()
    } else {
        None
    }
}

pub fn runtime(binary: &Path, repeats: usize) -> Result<Value> {
    if !(1..=100).contains(&repeats) {
        return Err("repeat must be 1..100".into());
    }
    let mut samples = Vec::new();
    for _ in 0..repeats {
        for scenario in [
            "idle",
            "large_url",
            "large_b64",
            "near_json_cap",
            "multi_8mib",
        ] {
            let root = tempfile::tempdir()?;
            let api = MockMicu::new()?;
            let env = if scenario == "large_url" {
                api.proxy_env()
            } else {
                api.env()
            };
            let began = Instant::now();
            let mut server = Session::new(binary, root.path(), &env)?;
            server.initialize()?;
            let initialize_ms = began.elapsed().as_secs_f64() * 1000.0;
            let began = Instant::now();
            server.request("tools/list", json!({}))?;
            let tools_list_ms = began.elapsed().as_secs_f64() * 1000.0;
            let began = Instant::now();
            server.call("server_info", json!({}))?;
            let server_info_ms = began.elapsed().as_secs_f64() * 1000.0;
            let idle = rss(server.pid());
            let stop = Arc::new(AtomicBool::new(false));
            let memory = Arc::new(Mutex::new(Vec::new()));
            let sampler_stop = stop.clone();
            let captured = memory.clone();
            let pid = server.pid();
            let sampler = thread::spawn(move || {
                let start = Instant::now();
                while !sampler_stop.load(Ordering::Relaxed) {
                    if let Some(bytes) = rss(pid)
                        && let Ok(mut values) = captured.lock()
                    {
                        values.push(json!({"elapsed_ms":start.elapsed().as_secs_f64()*1000.0,"rss_bytes":bytes}));
                    }
                    thread::sleep(Duration::from_millis(if cfg!(target_os = "windows") {
                        100
                    } else {
                        5
                    }));
                }
            });
            let began = Instant::now();
            let result = (|| -> Result<Option<Value>> {
                if scenario == "idle" {
                    return Ok(None);
                }
                if scenario == "multi_8mib" {
                    let mut image = png(32, 24, 1)?;
                    image.resize(4 * 1024 * 1024 - 1024, 0);
                    let paths: Vec<_> = ["a.png", "b.png"]
                        .iter()
                        .map(|name| root.path().join(name))
                        .collect();
                    for path in &paths {
                        fs::write(path, &image)?;
                    }
                    return Ok(Some(server.call(
                        "image_multi_reference",
                        json!({"prompt":"multipart memory", "image_paths":paths}),
                    )?));
                }
                Ok(Some(server.call(
                    "image_generate",
                    json!({"prompt":format!("[scenario:{scenario}] memory benchmark")}),
                )?))
            })();
            stop.store(true, Ordering::Relaxed);
            sampler.join().map_err(|_| "memory sampler failed")?;
            let result = result?;
            if let Some(result) = &result
                && (result["ok"] == true) != (scenario != "near_json_cap")
            {
                return Err("benchmark scenario returned an unexpected outcome".into());
            }
            let memory = memory.lock().map_err(|_| "memory sampler lock poisoned")?;
            samples.push(json!({"scenario":scenario,"initialize_ms":initialize_ms,"tools_list_ms":tools_list_ms,"server_info_ms":server_info_ms,
                "operation_ms":began.elapsed().as_secs_f64()*1000.0,"idle_rss_bytes":idle,"final_rss_bytes":rss(server.pid()),
                "peak_rss_bytes":memory.iter().filter_map(|item|item["rss_bytes"].as_u64()).chain(idle).max(),"rss_samples":*memory,"ok":result.map(|result|result["ok"].clone())}));
        }
    }
    let root = tempfile::tempdir()?;
    let mut servers = Vec::new();
    let mut idle_processes = Vec::new();
    for _ in 0..4 {
        let mut server = Session::new(binary, root.path(), &[])?;
        server.initialize()?;
        idle_processes.push(json!({"pid":server.pid(),"rss_bytes":rss(server.pid())}));
        servers.push(server);
    }
    Ok(
        json!({"provenance":provenance(binary),"paid_requests":0,"samples":samples,"idle_processes":idle_processes,"performance_threshold":null}),
    )
}
