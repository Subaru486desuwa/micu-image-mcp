//! Explicitly gated, one-call image acceptance against the current clean release.
//! Configuration and credentials stay in memory; the relay caps upstream sends at two.
use clap::Parser;
use micu_devtools::{
    Result,
    engineering::{provenance, write_report},
    live::{Relay, image_origin_allowed},
    stdio::Session,
};
use micu_image_mcp::{
    config::{ENV_KEYS, EnvironmentSnapshot},
    domain::routing::{FLARE_MODEL, LEGACY_FLARE_MODEL},
    installer::codex::parse_config_launch,
};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

#[derive(Parser)]
struct Options {
    #[arg(long)]
    live: bool,
    #[arg(long)]
    config: PathBuf,
    #[arg(long)]
    binary: PathBuf,
    #[arg(long)]
    output: PathBuf,
}

fn authorize(live: bool, gate: Option<&str>) -> Result<()> {
    if !live || gate != Some("1") {
        return Err("requires --live and MICU_RUN_LIVE_TESTS=1; no credentials read".into());
    }
    Ok(())
}

fn configured_environment(
    text: &str,
    mut environment: EnvironmentSnapshot,
) -> Result<EnvironmentSnapshot> {
    let launch = parse_config_launch(text)
        .map_err(|_| "configuration cannot be parsed")?
        .ok_or("configured micu-image launch is missing")?;
    for (name, value) in launch.env() {
        if ENV_KEYS.contains(&name.as_str()) {
            environment.insert(name.clone(), value.clone());
        }
    }
    Ok(environment)
}

fn generate_once(call: impl FnOnce(Value) -> Result<Value>) -> Result<Value> {
    call(json!({
        "prompt":"A single cobalt blue ceramic coffee cup on a plain white background, centered product photograph, soft light, no text, no logo, no watermark.",
        "size":"1024x1024", "n":1, "session_id":"new", "basename":"blue_ceramic_cup"
    }))
}

fn run(binary: &Path, output: &Path, config: &Path, report: &mut Value) -> Result<()> {
    report["phase"] = json!("credentials");
    let text = fs::read_to_string(config).map_err(|_| "configuration unavailable")?;
    let mut environment = configured_environment(&text, EnvironmentSnapshot::capture(ENV_KEYS)?)?;
    drop(text);
    let origin = environment
        .get("MICU_BASEURL")
        .unwrap_or("https://www.micuapi.ai")
        .trim_end_matches('/')
        .to_owned();
    if !image_origin_allowed(&origin) {
        return Err("configured origin is not approved for live image validation".into());
    }
    environment.load_platform_secrets(); // Product policy: authentication UI is forbidden.
    report["credential_status"] = serde_json::to_value(environment.credential_status())?;
    let key = SecretString::from(
        environment
            .get("MICU_API_KEY")
            .filter(|key| !key.trim().is_empty())
            .ok_or("no noninteractively available configured credential")?
            .trim()
            .to_owned(),
    );
    let relay = Relay::new(&origin, key.expose_secret(), true, 2)?;
    report["phase"] = json!("startup");
    let mut env = vec![
        ("MICU_BASEURL", relay.http.url.as_str()),
        ("MICU_API_KEY", "relay-local-placeholder"),
        (
            "MICU_SAVE_DIR",
            output.to_str().ok_or("invalid output path")?,
        ),
        (
            "MICU_SAVE_DIR_ROOT",
            output.to_str().ok_or("invalid output path")?,
        ),
    ];
    for name in ["MICU_TRUSTED_DOWNLOAD_HOSTS", "MICU_ALLOW_FAKE_IP_DOWNLOAD"] {
        if let Some(value) = environment.get(name) {
            env.push((name, value));
        }
    }
    let mut session = Session::new(binary, output, &env)?;
    session.set_timeout(Duration::from_secs(400));
    let initialized = session.initialize()?;
    let tools = session.request("tools/list", json!({}))?;
    let info = session.call("server_info", json!({}))?;
    if initialized["result"]["serverInfo"]["version"] != env!("CARGO_PKG_VERSION")
        || info["version"] != env!("CARGO_PKG_VERSION")
        || info["default_model"] != FLARE_MODEL
        || info["model_catalog"]["source"] != "manual"
        || tools["result"]["tools"].as_array().map(Vec::len) != Some(5)
    {
        return Err("candidate identity/default model did not match; no image call sent".into());
    }
    report["initialized_version"] = info["version"].clone();
    report["default_model"] = info["default_model"].clone();
    report["tool_count"] = json!(5);
    report["phase"] = json!("generation");
    report["logical_image_calls"] = json!(1);
    let began = Instant::now();
    let result = generate_once(|arguments| session.call("image_generate", arguments));
    drop(session);
    let requests = relay.finish()?;
    report["upstream_post_attempts"] = json!(requests.len());
    report["transport_sends"] = json!(
        requests
            .iter()
            .filter(|request| request["send_started"] == true)
            .count()
    );
    report["requests"] = json!(requests);
    report["generation_seconds"] = json!(began.elapsed().as_secs_f64());
    let result = result?;
    report["used_fallback"] = result["used_fallback"].clone();
    let actual = result["model"]
        .as_str()
        .filter(|model| [FLARE_MODEL, LEGACY_FLARE_MODEL].contains(model))
        .ok_or("unexpected reported model")?;
    report["actual_model"] = json!(actual);
    if result["ok"] != true {
        return Err("single image call failed; no additional paid call allowed".into());
    }
    report["phase"] = json!("artifact_validation");
    let saved = result["saved"]
        .as_array()
        .filter(|saved| saved.len() == 1)
        .ok_or("expected one saved image")?;
    let path =
        PathBuf::from(saved[0]["path"].as_str().ok_or("missing saved path")?).canonicalize()?;
    if !path.starts_with(output) {
        return Err("saved image escaped the isolated output root".into());
    }
    let reader = image::ImageReader::open(&path)?.with_guessed_format()?;
    let format = reader.format().ok_or("image format unavailable")?;
    let dimensions = reader.into_dimensions()?;
    report["saved"] = json!({"path":path,"format":format!("{format:?}"),"bytes":path.metadata()?.len(),"width":dimensions.0,"height":dimensions.1});
    report["session_revision"] = result["session"]["revision"].clone();
    if dimensions != (1024, 1024)
        || result["session"]["revision"] != 1
        || result["requested_n"] != 1
    {
        return Err("actual image/session did not meet the one-image contract".into());
    }
    report["phase"] = json!("complete");
    Ok(())
}

fn probe(options: &Options) -> Result<Value> {
    authorize(
        options.live,
        std::env::var("MICU_RUN_LIVE_TESTS").ok().as_deref(),
    )?;
    let root = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()?;
    if !root.status.success() {
        return Err("run from the candidate worktree".into());
    }
    let root = PathBuf::from(String::from_utf8(root.stdout)?.trim()).canonicalize()?;
    let binary = options.binary.canonicalize()?;
    let expected = root
        .join("target/release")
        .join(format!("micu-image-mcp{}", std::env::consts::EXE_SUFFIX))
        .canonicalize()?;
    if binary != expected {
        return Err("use this worktree's release binary".into());
    }
    let identity = provenance(&binary);
    if identity["working_tree_dirty"] != false {
        return Err("candidate must be clean and committed before live validation".into());
    }
    let parent = options.output.parent().ok_or("output parent missing")?;
    fs::create_dir_all(parent)?;
    fs::create_dir(&options.output)?;
    let output = options.output.canonicalize()?;
    if !output.starts_with(root.join("target").canonicalize()?) {
        return Err("live output must be ignored under this worktree's target directory".into());
    }
    let began = Instant::now();
    let mut report = json!({"status":"in_progress","version":env!("CARGO_PKG_VERSION"),"provenance":identity,"upstream_post_budget":2,"upstream_post_attempts":0,"logical_image_calls":0,"visual_review":"pending","phase":"preflight"});
    write_report(&output.join("report.json"), &report)?;
    let result = run(&binary, &output, &options.config, &mut report);
    report["status"] = json!(if result.is_ok() { "passed" } else { "failed" });
    report["total_seconds"] = json!(began.elapsed().as_secs_f64());
    write_report(&output.join("report.json"), &report)?;
    result?;
    Ok(report)
}

fn main() {
    let options = Options::parse();
    match probe(&options) {
        Ok(report) => {
            let _result = writeln!(
                std::io::stdout(),
                "single image acceptance completed; post_attempts={}; report={}",
                report["upstream_post_attempts"],
                options.output.join("report.json").display()
            );
        }
        Err(_) => {
            let _result = writeln!(
                std::io::stderr(),
                "single image acceptance stopped; no additional image call was made; inspect the sanitized report if created"
            );
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    #[test]
    fn both_live_guards_are_required_before_credentials() {
        for (flag, gate) in [
            (false, None),
            (true, None),
            (false, Some("1")),
            (true, Some("0")),
        ] {
            assert!(authorize(flag, gate).is_err());
        }
        assert!(authorize(true, Some("1")).is_ok());
    }
    #[test]
    fn configured_environment_uses_existing_parser_and_preserves_no_key_output() {
        let inherited = EnvironmentSnapshot::from_map(BTreeMap::from([(
            "MICU_API_KEY".into(),
            "inherited-fixture".into(),
        )]));
        let parsed=configured_environment("[mcp_servers.micu-image]\ncommand='old-binary'\n[mcp_servers.micu-image.env]\nMICU_API_KEY='configured-fixture'\nMICU_BASEURL='https://www.micuapi.ai'",inherited).unwrap();
        assert_eq!(parsed.get("MICU_API_KEY"), Some("configured-fixture"));
        assert!(!format!("{parsed:?}").contains("configured-fixture"));
        assert!(image_origin_allowed(parsed.get("MICU_BASEURL").unwrap()));
        assert!(!image_origin_allowed("https://untrusted.invalid"));
    }
    #[test]
    fn generation_action_is_exactly_one_fixed_request() -> Result<()> {
        let mut count = 0;
        generate_once(|args| {
            count += 1;
            assert_eq!(args["n"], 1);
            assert_eq!(args["size"], "1024x1024");
            assert_eq!(args["session_id"], "new");
            assert!(args.get("model").is_none());
            assert!(args.get("quality").is_none());
            Ok(json!({"ok":true}))
        })?;
        assert_eq!(count, 1);
        Ok(())
    }
}
