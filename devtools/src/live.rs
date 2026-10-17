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
                let mut event = json!({"sequence":index+1,"path":path,"request_bytes":bytes.len()});
                if images {
                    let response_format_present =
                        if header(&request, "Content-Type").starts_with("application/json") {
                            serde_json::from_slice::<Value>(&bytes)
                                .ok()
                                .is_some_and(|value| value.get("response_format").is_some())
                        } else {
                            let field = b"name=\"response_format\"";
                            bytes.windows(field.len()).any(|window| window == field)
                        };
                    event["response_format_present"] = json!(response_format_present);
                }
                events.push(event);
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
        let model = match model.as_str() {
            "gpt-image-2.5-flare" => "gpt-image-2.5-flare-openai",
            "gpt-image-2.5-sunburst" => "gpt-image-2.5-sunburst-openai",
            model => model,
        };
        if ![
            "gpt-image-2.5-flare-openai",
            "gpt-image-2.5-sunburst-openai",
            "gpt-image-2",
            "gpt-image-2-openai",
        ]
        .contains(&model)
        {
            return Err("unknown matrix model".into());
        }
        for size in sizes {
            if ![
                "1024x1024",
                "2048x1152",
                "1152x2048",
                "2048x2048",
                "3840x2160",
                "2160x3840",
            ]
            .contains(&size.as_str())
            {
                return Err(
                    "matrix size must be one of the supported 1K/2K/4K acceptance sizes".into(),
                );
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
    let available = match image_models(origin, key) {
        Ok(models) => models,
        Err(error) => {
            write_report(
                &output.join("report.json"),
                &json!({"passed":false,"phase":"model_preflight","paid_requests":0}),
            )?;
            return Err(error);
        }
    };
    let cases: Vec<_> = cases
        .into_iter()
        .filter(|case| {
            case["model"]
                .as_str()
                .is_some_and(|name| available.contains(name))
        })
        .collect();
    if cases.is_empty() {
        return Err("selected image models are not available; no image requests sent".into());
    }
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
        if authentication_failed(&relay.records()) {
            break;
        }
    }
    let report = redact(
        json!({"provenance":provenance(binary),"plan_only":false,"authentication_failed":authentication_failed(&relay.records()),"cases":reports,"upstream_requests":relay.records()}),
        &[key],
    );
    write_report(&output.join("report.json"), &report)?;
    if authentication_failed(&relay.records()) {
        return Err("image authentication failed; remaining matrix stopped".into());
    }
    Ok(report)
}

pub fn authentication_failed(records: &[Value]) -> bool {
    records
        .iter()
        .any(|event| matches!(event["status"].as_u64(), Some(401 | 403)))
}

fn image_models(origin: &str, key: &str) -> Result<std::collections::HashSet<String>> {
    if !["https://www.micuapi.ai", "https://sub.micuapi.ai"].contains(&origin)
        || key.trim().is_empty()
    {
        return Err("unapproved image origin or empty credential".into());
    }
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()?;
    let mut authorization = reqwest::header::HeaderValue::from_str(&format!("Bearer {key}"))
        .map_err(|_| "invalid image credential")?;
    authorization.set_sensitive(true);
    let response = client
        .get(format!("{origin}/v1/models"))
        .header("Authorization", authorization)
        .send()
        .map_err(|_| "model preflight transport failed")?;
    if !response.status().is_success() {
        return Err("image credential/model preflight failed; no image requests sent".into());
    }
    let mut bytes = Vec::new();
    response
        .take(5 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "model preflight read failed")?;
    if bytes.len() > 5 * 1024 * 1024 {
        return Err("model preflight exceeds limit".into());
    }
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| "model preflight returned invalid JSON")?;
    Ok(value["data"]
        .as_array()
        .ok_or("model preflight catalog missing")?
        .iter()
        .filter_map(|item| item["id"].as_str())
        .map(str::to_owned)
        .collect())
}

pub fn stress(binary: &Path, highres: bool, output: &Path) -> Result<Value> {
    gated("MICU_RUN_LIVE_TESTS")?;
    gated("MICU_RUN_LIVE_STRESS")?;
    if highres {
        gated("MICU_RUN_LIVE_HIGHRES_STRESS")?;
    }
    let credential = credentials()?;
    let key = credential["api_key"]
        .as_str()
        .ok_or("api_key required on stdin")?;
    let origin = credential["origin"]
        .as_str()
        .unwrap_or("https://www.micuapi.ai");
    let models = image_models(origin, key)?;
    let model = if highres {
        "gpt-image-2-openai"
    } else {
        "gpt-image-2"
    };
    if !models.contains(model) {
        return Err("required stress image model is not available; no image requests sent".into());
    }
    if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir(output)?;
    let output = output.canonicalize()?;
    let root = tempfile::tempdir()?;
    let relay = Relay::new(origin, key, true, 20)?;
    let sizes = if highres {
        vec![
            "2048x1152",
            "1152x2048",
            "2048x2048",
            "3840x2160",
            "2160x3840",
        ]
    } else {
        vec!["1024x1024"]
    };
    let ready = std::sync::atomic::AtomicUsize::new(0);
    let deadline = Instant::now() + Duration::from_secs(120);
    let outcomes = std::thread::scope(|scope| {
        let jobs:Vec<_>=sizes.iter().enumerate().map(|(index,size)| {
            let output=&output;let root=root.path();let relay=&relay;let ready=&ready;let count=sizes.len();
            scope.spawn(move || -> Result<Value> {
                let destination=output.join(format!("process-{index}"));fs::create_dir(&destination)?;
                let env=[("MICU_API_KEY","relay-local-placeholder"),("MICU_BASEURL",relay.http.url.as_str()),("MICU_SAVE_DIR",destination.to_str().ok_or("invalid stress path")?),("MICU_SAVE_DIR_ROOT",output.to_str().ok_or("invalid stress root")?)];
                let mut server=Session::new(binary,root,&env)?;server.set_timeout(Duration::from_secs(if highres {3600} else {900}));
                server.initialize()?;let info=server.call("server_info",json!({}))?;
                ready.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
                while ready.load(std::sync::atomic::Ordering::SeqCst)<count {if Instant::now()>deadline {return Err("live stress process readiness timed out".into());}std::thread::sleep(Duration::from_millis(10));}
                let mut args=json!({"prompt":"A centered cobalt blue ceramic cup on a red table, plain white background, no text or logo.","model":"gpt-image-2","size":size,"n":if highres {3} else {5},"basename":format!("stress_{index}")});
                if highres {args["quality"]=json!("high");}
                let began=Instant::now();let result=server.call("image_generate",args)?;
                let saved=result["saved"].as_array().ok_or("stress saved list missing")?;
                let expected=if highres {1} else {5};
                let mut artifacts=Vec::new();let mut paths=std::collections::HashSet::new();
                for item in saved {
                    let path=Path::new(item["path"].as_str().ok_or("stress image path missing")?);let path=path.canonicalize()?;
                    if !path.starts_with(output) || !paths.insert(path.clone()) {return Err("stress output escaped root or collided".into());}
                    let image=image::open(&path)?;
                    if highres && format!("{}x{}",image.width(),image.height())!=*size {return Err("live high-resolution output did not honor pixels".into());}
                    artifacts.push(json!({"path":path,"width":image.width(),"height":image.height()}));
                }
                let valid=result["ok"]==true && result["requested_n"]==expected && saved.len()==expected && result["errors"].as_array().is_some_and(Vec::is_empty) && (!highres || (result["model"]=="gpt-image-2-openai" && result["size_honored"]==true && result["notes"].to_string().contains("强制 N=1")));
                if !valid {return Err("live stress result did not satisfy the current public contract".into());}
                Ok(json!({"size":size,"saved":artifacts,"server_version":info["version"],"cross_process_lock":info["retry_policy"]["concurrency_2k_4k"],"lock_waited":result["notes"].to_string().contains("等待跨进程"),"wall_seconds":began.elapsed().as_secs_f64(),"stdout_frames":server.frames,"passed":true}))
            })
        }).collect();
        jobs.into_iter()
            .map(|job| {
                job.join()
                    .map_err(|_| "live stress worker failed".into())
                    .and_then(|value| value)
            })
            .collect::<Vec<Result<Value>>>()
    });
    let passed = outcomes.iter().all(Result::is_ok);
    let samples: Vec<_> = outcomes
        .into_iter()
        .map(|value| {
            value.unwrap_or_else(
                |_| json!({"passed":false,"error":"stress validation failed; details omitted"}),
            )
        })
        .collect();
    let waits = samples
        .iter()
        .filter(|sample| sample["lock_waited"] == true)
        .count();
    let consistent = samples.iter().all(|sample| {
        sample["server_version"] == samples[0]["server_version"]
            && sample["cross_process_lock"] == samples[0]["cross_process_lock"]
    });
    let passed = passed && consistent && (!highres || waits >= 4);
    let report = json!({"provenance":provenance(binary),"highres":highres,"passed":passed,"lock_waiters":waits,"samples":samples,"upstream_requests":relay.records()});
    write_report(&output.join("report.json"), &report)?;
    if !passed {
        return Err("live stress failed; see sanitized report".into());
    }
    Ok(report)
}
