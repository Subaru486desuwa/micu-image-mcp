use crate::Result;
use base64::Engine;
use image::{ImageBuffer, ImageFormat, Rgba};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::{Cursor, Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tiny_http::{Header, Request, Response, Server, StatusCode};

pub const FIXTURE_KEY: &str = "offline-contract-key";
pub const RESPONSE_LIMIT: usize = 25 * 1024 * 1024;

#[derive(Clone, Debug, Serialize)]
pub struct Capture {
    pub method: String,
    pub path: String,
    pub authorization_valid: bool,
    pub fields: Value,
    pub files: Vec<Upload>,
    pub scenario: String,
    pub attempt: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct Upload {
    pub name: String,
    pub filename: String,
    pub mime: String,
    pub size: usize,
    pub sha256: String,
    #[serde(skip)]
    pub bytes: Vec<u8>,
}

pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn split_bytes<'a>(bytes: &'a [u8], separator: &[u8]) -> Vec<&'a [u8]> {
    let mut parts = Vec::new();
    let mut start = 0;
    while let Some(relative) = bytes[start..]
        .windows(separator.len())
        .position(|window| window == separator)
    {
        let end = start + relative;
        parts.push(&bytes[start..end]);
        start = end + separator.len();
    }
    parts.push(&bytes[start..]);
    parts
}

fn multipart(content_type: &str, bytes: &[u8]) -> Result<(Value, Vec<Upload>)> {
    let boundary = content_type
        .split(';')
        .find_map(|part| part.trim().strip_prefix("boundary="))
        .ok_or("multipart boundary missing")?
        .trim_matches('"');
    if boundary.is_empty() {
        return Err("empty multipart boundary".into());
    }
    let separator = format!("--{boundary}");
    let mut fields = serde_json::Map::new();
    let mut files = Vec::new();
    for part in split_bytes(bytes, separator.as_bytes()).into_iter().skip(1) {
        if part.starts_with(b"--") {
            break;
        }
        let part = part
            .strip_prefix(b"\r\n")
            .ok_or("invalid multipart framing")?;
        let divider = part
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
            .ok_or("multipart headers missing")?;
        let headers = std::str::from_utf8(&part[..divider])?;
        let content = part[divider + 4..]
            .strip_suffix(b"\r\n")
            .ok_or("multipart terminator missing")?;
        let disposition = headers
            .lines()
            .find(|line| {
                line.to_ascii_lowercase()
                    .starts_with("content-disposition:")
            })
            .ok_or("multipart disposition missing")?;
        let parameter = |name: &str| {
            disposition
                .split(';')
                .find_map(|part| part.trim().strip_prefix(name))
                .map(|value| value.trim_matches('"').to_owned())
        };
        let name = parameter("name=").ok_or("multipart name missing")?;
        if let Some(filename) = parameter("filename=") {
            let mime = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-type:")
                        .map(|mime| mime.trim().to_owned())
                })
                .unwrap_or_default();
            files.push(Upload {
                name,
                filename,
                mime,
                size: content.len(),
                sha256: sha256(content),
                bytes: content.to_vec(),
            });
        } else {
            fields.insert(
                name,
                Value::String(std::str::from_utf8(content)?.to_owned()),
            );
        }
    }
    Ok((Value::Object(fields), files))
}

/// A loopback-only HTTP server with owned workers and deterministic shutdown.
pub struct HttpFixture {
    pub url: String,
    stopped: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    errors: Arc<Mutex<Vec<String>>>,
}

impl HttpFixture {
    pub fn start(handler: impl Fn(Request) -> Result<()> + Send + Sync + 'static) -> Result<Self> {
        let server = Server::http("127.0.0.1:0")?;
        let url = format!("http://{}", server.server_addr());
        let stopped = Arc::new(AtomicBool::new(false));
        let errors = Arc::new(Mutex::new(Vec::new()));
        let stop = stopped.clone();
        let failures = errors.clone();
        let handler = Arc::new(handler);
        let worker = thread::spawn(move || {
            let mut workers = Vec::new();
            while !stop.load(Ordering::Acquire) {
                match server.recv_timeout(Duration::from_millis(50)) {
                    Ok(Some(request)) => {
                        let handler = handler.clone();
                        let failures = failures.clone();
                        workers.push(thread::spawn(move || {
                            if handler(request).is_err()
                                && let Ok(mut errors) = failures.lock()
                            {
                                errors.push("HTTP fixture request failed".into());
                            }
                        }));
                    }
                    Ok(None) => {}
                    Err(_) => break,
                }
            }
            for worker in workers {
                let _ignored = worker.join();
            }
        });
        Ok(Self {
            url,
            stopped,
            worker: Some(worker),
            errors,
        })
    }

    pub fn errors(&self) -> Vec<String> {
        self.errors
            .lock()
            .map(|errors| errors.clone())
            .unwrap_or_default()
    }
}

impl Drop for HttpFixture {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ignored = worker.join();
        }
    }
}

pub fn header(request: &Request, name: &'static str) -> String {
    request
        .headers()
        .iter()
        .find(|header| header.field.equiv(name))
        .map(|header| header.value.as_str().to_owned())
        .unwrap_or_default()
}

pub fn body(request: &mut Request) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    request
        .as_reader()
        .take((RESPONSE_LIMIT + 1) as u64)
        .read_to_end(&mut body)?;
    if body.len() > RESPONSE_LIMIT {
        return Err("fixture request exceeds limit".into());
    }
    Ok(body)
}

pub fn respond(
    request: Request,
    status: u16,
    bytes: Vec<u8>,
    mime: &str,
    extra: &[(&str, &str)],
) -> Result<()> {
    let mut headers =
        vec![Header::from_bytes("Content-Type", mime).map_err(|_| "invalid fixture header")?];
    for &(name, value) in extra {
        headers.push(Header::from_bytes(name, value).map_err(|_| "invalid fixture header")?);
    }
    let length = bytes.len();
    request.respond(Response::new(
        StatusCode(status),
        headers,
        Cursor::new(bytes),
        Some(length),
        None,
    ))?;
    Ok(())
}

pub fn respond_json(
    request: Request,
    status: u16,
    value: Value,
    extra: &[(&str, &str)],
) -> Result<()> {
    respond(
        request,
        status,
        serde_json::to_vec(&value)?,
        "application/json",
        extra,
    )
}

pub fn png(width: u32, height: u32, seed: u8) -> Result<Vec<u8>> {
    let image = ImageBuffer::from_pixel(width, height, Rgba([seed, 90u8, 160u8, 255u8]));
    let mut encoded = Cursor::new(Vec::new());
    image.write_to(&mut encoded, ImageFormat::Png)?;
    Ok(encoded.into_inner())
}

#[derive(Default)]
struct State {
    requests: Vec<Capture>,
    attempts: HashMap<String, usize>,
    active: usize,
    max_active: usize,
    starts: Vec<Instant>,
}

#[derive(Clone, Serialize)]
pub struct Metrics {
    pub max_active: usize,
    pub gaps_seconds: Vec<f64>,
}

struct ActiveRequest(Arc<Mutex<State>>);
impl Drop for ActiveRequest {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.lock() {
            state.active = state.active.saturating_sub(1);
        }
    }
}

fn scenario(prompt: &str) -> &str {
    prompt
        .split_once("[scenario:")
        .and_then(|(_, tail)| tail.split_once(']'))
        .map(|(name, _)| name)
        .unwrap_or("b64")
}

fn request_path(request: &Request) -> String {
    reqwest::Url::parse(request.url())
        .map(|url| url.path().to_owned())
        .unwrap_or_else(|_| request.url().to_owned())
}

fn api_response(
    request: Request,
    fields: &Value,
    scenario: &str,
    attempt: usize,
    seed: u8,
    public_url: &str,
) -> Result<()> {
    if scenario == "concurrency_probe" {
        thread::sleep(Duration::from_millis(200));
    }
    if scenario == "api_timeout" {
        thread::sleep(Duration::from_millis(250));
    }
    if matches!(scenario, "disconnect_once" | "disconnect_body_once") && attempt == 1 {
        let mut writer = request.into_writer();
        if scenario == "disconnect_body_once" {
            writer.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n{",
            )?;
        }
        return Ok(());
    }
    let status = match scenario {
        "retry_400_too_many" => Some(400),
        "retry_408" => Some(408),
        "retry_429" | "retry_after_seconds" | "retry_after_http_date" => Some(429),
        "retry_500" => Some(500),
        _ => None,
    };
    if let Some(status) = status.filter(|_| attempt == 1) {
        let message = if status == 400 {
            "Too Many Requests"
        } else {
            "fixture retry"
        };
        let retry = if scenario == "retry_after_http_date" {
            "Thu, 01 Jan 1970 00:00:00 GMT"
        } else {
            "0"
        };
        return respond_json(
            request,
            status,
            json!({"error":{"message":message}}),
            &[("Retry-After", retry)],
        );
    }
    match scenario {
        "http_524" => {
            return respond_json(
                request,
                524,
                json!({"error":{"message":"fixture timeout"}}),
                &[],
            );
        }
        "secret_error" => {
            return respond_json(
                request,
                400,
                json!({"error":{"message":format!("Authorization: Bearer {FIXTURE_KEY}; image=iVBORw0KGgo{}", "A".repeat(200))}}),
                &[],
            );
        }
        "no_image" => return respond_json(request, 200, json!({"data":[]}), &[]),
        "invalid_json" => {
            return respond(
                request,
                200,
                b"{invalid JSON".to_vec(),
                "application/json",
                &[],
            );
        }
        "content_length_too_large" => {
            let mut writer = request.into_writer();
            write!(
                writer,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{{}}",
                RESPONSE_LIMIT + 1
            )?;
            return Ok(writer.flush()?);
        }
        "stream_too_large" => {
            return Ok(request.respond(Response::new(
                StatusCode(200),
                vec![],
                Cursor::new(vec![b'x'; RESPONSE_LIMIT + 1]),
                None,
                None,
            ))?);
        }
        "near_json_cap" => {
            return respond_json(
                request,
                200,
                json!({"data":[], "padding":"A".repeat(RESPONSE_LIMIT - 4096)}),
                &[],
            );
        }
        _ => {}
    }
    let (width, height) = if scenario == "exact_b64" {
        fields["size"]
            .as_str()
            .and_then(|size| size.split_once('x'))
            .and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)))
            .filter(|(w, h)| u64::from(*w) * u64::from(*h) <= 8_294_400)
            .ok_or("invalid fixture size")?
    } else {
        (32, 24)
    };
    let mut image = png(width, height, seed)?;
    if scenario == "large_b64" {
        image.resize(17 * 1024 * 1024, 0);
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(image);
    if scenario == "data_url" {
        return respond_json(
            request,
            200,
            json!({"data":[{"url":format!("data:image/png;base64,{encoded}")}]}),
            &[],
        );
    }
    if fields["response_format"] != "b64_json" {
        let suffix = match scenario {
            "url_success" => Some("asset.png"),
            "url_no_content_length" => Some("asset-no-length.png"),
            "large_url" => Some("large.png"),
            "url_truncated_then_b64" => Some("truncated.png"),
            "url_redirect_private_then_b64" => Some("redirect-private.png"),
            _ => None,
        };
        if let Some(suffix) = suffix {
            return respond_json(
                request,
                200,
                json!({"data":[{"url":format!("{public_url}/{suffix}")}]}),
                &[],
            );
        }
    }
    if matches!(scenario, "url_loopback" | "url_mapped_loopback")
        && fields["response_format"] != "b64_json"
    {
        let url = if scenario == "url_loopback" {
            "http://127.0.0.1:9/private.png"
        } else {
            "http://[::ffff:127.0.0.1]/private.png"
        };
        return respond_json(request, 200, json!({"data":[{"url":url}]}), &[]);
    }
    respond_json(request, 200, json!({"data":[{"b64_json":encoded}]}), &[])
}

pub struct MockMicu {
    pub http: HttpFixture,
    state: Arc<Mutex<State>>,
}

impl MockMicu {
    pub fn new() -> Result<Self> {
        Self::start(false)
    }

    pub fn exact_images() -> Result<Self> {
        Self::start(true)
    }

    fn start(exact_images: bool) -> Result<Self> {
        let state = Arc::new(Mutex::new(State::default()));
        let captured = state.clone();
        let public_url = Arc::new(Mutex::new(String::new()));
        let public = public_url.clone();
        let http = HttpFixture::start(move |mut request| {
            if request.method().as_str() == "GET" {
                let path = request_path(&request);
                let authorization_valid = header(&request, "Authorization").is_empty();
                captured
                    .lock()
                    .map_err(|_| "fixture lock poisoned")?
                    .requests
                    .push(Capture {
                        method: "GET".into(),
                        path: path.clone(),
                        authorization_valid,
                        fields: json!({}),
                        files: vec![],
                        scenario: "download".into(),
                        attempt: 1,
                    });
                if path == "/redirect-private.png" {
                    return respond(
                        request,
                        302,
                        vec![],
                        "image/png",
                        &[("Location", "http://127.0.0.1:9/private.png")],
                    );
                }
                let mut image = png(32, 24, 99)?;
                if path == "/truncated.png" {
                    image.truncate(24);
                }
                if path == "/large.png" {
                    image.resize(24 * 1024 * 1024, 0);
                }
                if path == "/asset-no-length.png" {
                    let response = Response::new(
                        StatusCode(200),
                        vec![
                            Header::from_bytes("Content-Type", "image/png")
                                .map_err(|_| "bad mime")?,
                        ],
                        Cursor::new(image),
                        None,
                        None,
                    );
                    return Ok(request.respond(response)?);
                }
                return respond(request, 200, image, "image/png", &[]);
            }
            let content_type = header(&request, "Content-Type");
            let bytes = body(&mut request)?;
            let (fields, files) = if content_type.starts_with("multipart/form-data") {
                multipart(&content_type, &bytes)?
            } else {
                (serde_json::from_slice(&bytes)?, Vec::new())
            };
            let authorization_valid =
                header(&request, "Authorization") == format!("Bearer {FIXTURE_KEY}");
            let scenario = scenario(fields["prompt"].as_str().unwrap_or_default()).to_owned();
            let (seed, attempt) = {
                let mut state = captured.lock().map_err(|_| "fixture lock poisoned")?;
                let attempt = state.attempts.entry(scenario.clone()).or_default();
                *attempt += 1;
                let attempt = *attempt;
                state.active += 1;
                state.max_active = state.max_active.max(state.active);
                state.starts.push(Instant::now());
                state.requests.push(Capture {
                    method: "POST".into(),
                    path: request_path(&request),
                    authorization_valid,
                    fields: fields.clone(),
                    files,
                    scenario: scenario.clone(),
                    attempt,
                });
                (state.requests.len() as u8, attempt)
            };
            let _active = ActiveRequest(captured.clone());
            if !authorization_valid {
                return respond_json(
                    request,
                    401,
                    json!({"error":{"message":"invalid fixture authorization"}}),
                    &[],
                );
            }
            let public_url = public.lock().map_err(|_| "fixture lock poisoned")?.clone();
            let response_scenario = if exact_images && scenario == "b64" {
                "exact_b64"
            } else {
                &scenario
            };
            api_response(
                request,
                &fields,
                response_scenario,
                attempt,
                seed,
                &public_url,
            )
        })?;
        let port = reqwest::Url::parse(&http.url)?
            .port()
            .ok_or("fixture port missing")?;
        *public_url.lock().map_err(|_| "fixture lock poisoned")? = format!("http://1.1.1.1:{port}");
        Ok(Self { http, state })
    }

    pub fn env(&self) -> Vec<(&str, &str)> {
        vec![
            ("MICU_API_KEY", FIXTURE_KEY),
            ("MICU_BASEURL", &self.http.url),
            ("MICU_CONTRACT_TESTING", "1"),
            ("MICU_TEST_API_TIMEOUT_MS", "5000"),
        ]
    }

    pub fn requests(&self) -> Vec<Capture> {
        self.state
            .lock()
            .map(|state| state.requests.clone())
            .unwrap_or_default()
    }

    pub fn proxy_env(&self) -> Vec<(&str, &str)> {
        let mut env = self.env();
        env.extend([
            ("MICU_USE_SHELL_PROXY", "1"),
            ("HTTP_PROXY", self.http.url.as_str()),
            ("HTTPS_PROXY", self.http.url.as_str()),
            ("ALL_PROXY", self.http.url.as_str()),
            ("NO_PROXY", ""),
        ]);
        env
    }

    pub fn metrics(&self) -> Metrics {
        self.state
            .lock()
            .map(|state| Metrics {
                max_active: state.max_active,
                gaps_seconds: state
                    .starts
                    .windows(2)
                    .map(|pair| pair[1].duration_since(pair[0]).as_secs_f64())
                    .collect(),
            })
            .unwrap_or(Metrics {
                max_active: 0,
                gaps_seconds: vec![],
            })
    }
}
