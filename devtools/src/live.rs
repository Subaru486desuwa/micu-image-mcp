use crate::{
    Result,
    clients::{credentials, gated, redact},
    engineering::{provenance, write_report},
    mock::{HttpFixture, body, header, png, respond},
    stdio::Session,
};
use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub struct Relay {
    pub http: HttpFixture,
    records: Arc<Mutex<Vec<Value>>>,
}
impl Relay {
    pub fn new(origin: &str, key: &str, images: bool, limit: usize) -> Result<Self> {
        let allowed = if images {
            &["https://www.micuapi.ai", "https://sub.micuapi.ai"][..]
        } else {
            &[
                "https://sub.micuapi.ai",
                "https://api.anthropic.com",
                "https://api.deepseek.com",
            ][..]
        };
        if !allowed.contains(&origin) || key.trim().is_empty() {
            return Err("unapproved provider origin or empty credential".into());
        }
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(180))
            .build()?;
        let origin = origin.to_owned();
        let key = key.to_owned();
        let records = Arc::new(Mutex::new(Vec::new()));
        let observed = records.clone();
        let http = HttpFixture::start(move |mut request| {
            let path = request
                .url()
                .split('?')
                .next()
                .unwrap_or_default()
                .to_owned();
            let allowed = if images {
                matches!(path.as_str(), "/v1/images/generations" | "/v1/images/edits")
            } else {
                matches!(
                    path.as_str(),
                    "/v1/messages"
                        | "/v1/messages/count_tokens"
                        | "/v1/chat/completions"
                        | "/chat/completions"
                )
            };
            if request.method().as_str() != "POST" || !allowed {
                return respond(request, 404, vec![], "application/json", &[]);
            }
            let bytes = body(&mut request)?;
            if bytes.len() > 8 * 1024 * 1024 {
                return respond(request, 413, vec![], "application/json", &[]);
            }
            let index = {
                let mut events = observed.lock().map_err(|_| "relay lock poisoned")?;
                if events.len() >= limit {
                    return respond(
                        request,
                        429,
                        b"request budget exhausted".to_vec(),
                        "text/plain",
                        &[],
                    );
                }
                let index = events.len();
                events.push(json!({"sequence":index+1,"path":path,"request_bytes":bytes.len()}));
                index
            };
            let mut target = origin.clone();
            let mut path = path;
            if origin == "https://api.deepseek.com" && path.contains("messages") {
                target.push_str("/anthropic");
            }
            if origin == "https://api.deepseek.com" && path == "/chat/completions" {
                path = "/v1/chat/completions".into();
            }
            let began = Instant::now();
            let result = (|| -> Result<(u16, String, Vec<u8>)> {
                let mut authorization =
                    reqwest::header::HeaderValue::from_str(&format!("Bearer {key}"))
                        .map_err(|_| "invalid relay credential")?;
                authorization.set_sensitive(true);
                let mut api_key = reqwest::header::HeaderValue::from_str(&key)
                    .map_err(|_| "invalid relay credential")?;
                api_key.set_sensitive(true);
                let mut builder = client
                    .post(format!("{target}{path}"))
                    .header("Authorization", authorization)
                    .header("x-api-key", api_key)
                    .header("Content-Type", header(&request, "Content-Type"))
                    .header("Accept-Encoding", "identity");
                for name in ["anthropic-version", "anthropic-beta", "User-Agent", "x-app"] {
                    let value = header(&request, name);
                    if !value.is_empty() {
                        builder = builder.header(name, value);
                    }
                }
                let response = builder
                    .body(bytes)
                    .send()
                    .map_err(|_| "upstream transport failed")?;
                let status = response.status().as_u16();
                let mime = response
                    .headers()
                    .get("Content-Type")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or("application/json")
                    .to_owned();
                let mut bytes = Vec::new();
                response
                    .take(25 * 1024 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|_| "upstream read failed")?;
                if bytes.len() > 25 * 1024 * 1024 {
                    return Err("upstream response exceeds limit".into());
                }
                // Never forward echoed credentials into a native client's files or stdout.
                let text = String::from_utf8(bytes)
                    .map_err(|_| "upstream returned non-text response")?
                    .replace(&key, "<REDACTED>");
                Ok((status, mime, text.into_bytes()))
            })();
            let (status, mime, bytes) = result.unwrap_or_else(|_| {
                (
                    502,
                    "application/json".into(),
                    b"{\"error\":\"upstream failed; details omitted\"}".to_vec(),
                )
            });
            let mut events = observed.lock().map_err(|_| "relay lock poisoned")?;
            events[index]["status"] = json!(status);
            events[index]["wall_seconds"] = json!(began.elapsed().as_secs_f64());
            events[index]["response_bytes"] = json!(bytes.len());
            drop(events);
            respond(request, status, bytes, &mime, &[])
        })?;
        Ok(Self { http, records })
    }
    pub fn records(&self) -> Vec<Value> {
        self.records
            .lock()
            .map(|values| values.clone())
            .unwrap_or_default()
    }
}

pub fn quality_matrix(models: &[String], sizes: &[String]) -> Result<Vec<Value>> {
    let mut cases = Vec::new();
    for model in models {
        if ![
            "gpt-image-2.5-flare",
            "gpt-image-2.5-sunburst",
            "gpt-image-2",
            "gpt-image-2-openai",
        ]
        .contains(&model.as_str())
        {
            return Err("unknown matrix model".into());
        }
        for size in sizes {
            if !["1024x1024", "2048x2048", "4096x4096"].contains(&size.as_str()) {
                return Err("matrix sizes must be 1024x1024, 2048x2048 or 4096x4096".into());
            }
            for tool in ["image_generate", "image_edit"] {
                for quality in [
                    None,
                    Some("auto"),
                    Some("low"),
                    Some("medium"),
                    Some("high"),
                    Some("xhigh"),
                    Some("max"),
                ] {
                    if !model.contains("2.5") && matches!(quality, Some("xhigh" | "max")) {
                        continue;
                    }
                    cases.push(json!({"index":cases.len()+1,"model":model,"size":size,"tool":tool,"quality":quality}));
                }
            }
        }
    }
    if cases.is_empty() {
        return Err("matrix must contain at least one case".into());
    }
    Ok(cases)
}

pub fn quality(
    binary: &Path,
    models: &[String],
    sizes: &[String],
    live: bool,
    limit: usize,
    output: Option<&Path>,
) -> Result<Value> {
    let cases = quality_matrix(models, sizes)?;
    if !live {
        return Ok(json!({"paid_requests":0,"plan_only":true,"cases":cases}));
    }
    gated("MICU_RUN_LIVE_TESTS")?;
    if !(1..=144).contains(&limit) {
        return Err("case limit must be 1..144".into());
    }
    let credential = credentials()?;
    let key = credential["api_key"]
        .as_str()
        .ok_or("api_key is required on stdin")?;
    let origin = credential["origin"]
        .as_str()
        .unwrap_or("https://www.micuapi.ai");
    let output = output.ok_or("live matrix requires --output directory")?;
    if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir(output)?;
    let output = output.canonicalize()?;
    let root = tempfile::tempdir()?;
    let source = root.path().join("source.png");
    fs::write(&source, png(32, 24, 1)?)?;
    let relay = Relay::new(origin, key, true, limit * 4)?;
    let output_text = output.to_str().ok_or("invalid output path")?;
    let env = [
        ("MICU_API_KEY", "relay-local-placeholder"),
        ("MICU_BASEURL", relay.http.url.as_str()),
        ("MICU_SAVE_DIR", output_text),
        ("MICU_SAVE_DIR_ROOT", output_text),
    ];
    let mut reports = Vec::new();
    for case in cases.into_iter().take(limit) {
        let mut server = Session::new(binary, root.path(), &env)?;
        server.set_timeout(Duration::from_secs(700));
        server.initialize()?;
        let mut args = json!({"prompt":"A centered cobalt blue ceramic cup on a red table, plain white background; no text or logos.","model":case["model"],"size":case["size"],"n":1,"basename":format!("quality_{}",case["index"])});
        if !case["quality"].is_null() {
            args["quality"] = case["quality"].clone();
        }
        if case["tool"] == "image_edit" {
            args["image_path"] = json!(source);
            args["prompt"] = json!("Change the cup to yellow; preserve the composition.");
        }
        let began = Instant::now();
        let result = server.call(case["tool"].as_str().ok_or("matrix tool missing")?, args)?;
        let mut artifacts = Vec::new();
        let saved = &result["saved"];
        let saved: Vec<_> = if let Some(items) = saved.as_array() {
            items.iter().collect()
        } else if saved.is_object() {
            vec![saved]
        } else {
            vec![]
        };
        for saved in saved {
            if let Some(path) = saved["path"].as_str() {
                let image = image::open(path)?;
                artifacts.push(json!({"path":path,"width":image.width(),"height":image.height()}));
            }
        }
        reports.push(json!({"case":case,"ok":result["ok"],"wall_seconds":began.elapsed().as_secs_f64(),"artifacts":artifacts}));
    }
    let report = redact(
        json!({"provenance":provenance(binary),"plan_only":false,"cases":reports,"upstream_requests":relay.records()}),
        &[key],
    );
    write_report(&output.join("report.json"), &report)?;
    Ok(report)
}
