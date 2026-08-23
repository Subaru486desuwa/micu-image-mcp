use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, SystemTime},
};

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::{
    Client, ClientBuilder, Method, NoProxy, Proxy, Request,
    header::{ACCEPT, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HeaderMap, HeaderValue},
    multipart::{Form, Part},
    redirect::Policy,
};
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;
use tokio::sync::Mutex;
use tokio_util::io::ReaderStream;
use url::Url;

use crate::{
    config::{AppPaths, Config},
    fs::input::ValidatedImage,
    fs::lock::BigRequestGate,
    http::download::ResolvedDownload,
    http::response::{error_detail, sanitize_sensitive},
    http::retry::{NETWORK_RETRY_DELAY, RETRY_JITTER_MAX, effective_retry_status, retry_delay},
};

pub const DOWNLOAD_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
pub const DOWNLOAD_WALL_TIMEOUT: Duration = Duration::from_secs(180);
pub const MAX_RESPONSE_BYTES: usize = 25 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct ApiResponse {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl ApiResponse {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryOptions {
    pub enabled: bool,
    pub big_size: bool,
}

#[async_trait]
pub trait RequestFactory: Send + Sync {
    async fn build(&self, client: &Client, key: &SecretString) -> Result<Request, String>;
}

#[derive(Clone)]
pub struct JsonRequestFactory {
    pub url: Url,
    pub body: Value,
}

#[async_trait]
impl RequestFactory for JsonRequestFactory {
    async fn build(&self, client: &Client, key: &SecretString) -> Result<Request, String> {
        let body = json_request_bytes(&self.body)?;
        client
            .request(Method::POST, self.url.clone())
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json; charset=utf-8")
            .header(AUTHORIZATION, bearer_header(key)?)
            .body(body)
            .build()
            .map_err(|error| format!("无法构造 JSON request: {error}"))
    }
}

#[derive(Clone)]
pub struct UploadPart {
    pub field_name: String,
    pub filename: String,
    pub mime: &'static str,
    pub file: Arc<tempfile::NamedTempFile>,
    pub size_bytes: u64,
}

impl UploadPart {
    pub fn from_validated(field_name: impl Into<String>, image: &ValidatedImage) -> Self {
        Self {
            field_name: field_name.into(),
            filename: image.filename.clone(),
            mime: image.mime,
            file: image.file.clone(),
            size_bytes: image.size_bytes,
        }
    }
}

#[derive(Clone)]
pub struct MultipartRequestFactory {
    pub url: Url,
    pub fields: Vec<(String, String)>,
    pub files: Vec<UploadPart>,
}

#[async_trait]
impl RequestFactory for MultipartRequestFactory {
    async fn build(&self, client: &Client, key: &SecretString) -> Result<Request, String> {
        let mut form = Form::new();
        for (name, value) in &self.fields {
            form = form.text(name.clone(), value.clone());
        }
        for upload in &self.files {
            let std_file = upload
                .file
                .reopen()
                .map_err(|error| format!("无法复制上传文件 handle: {error}"))?;
            let stream = ReaderStream::new(tokio::fs::File::from_std(std_file));
            let body = reqwest::Body::wrap_stream(stream);
            let part = Part::stream_with_length(body, upload.size_bytes)
                .file_name(upload.filename.clone())
                .mime_str(upload.mime)
                .map_err(|error| format!("无法设置 multipart MIME: {error}"))?;
            form = form.part(upload.field_name.clone(), part);
        }
        client
            .request(Method::POST, self.url.clone())
            .header(ACCEPT, "application/json")
            .header(AUTHORIZATION, bearer_header(key)?)
            .multipart(form)
            .build()
            .map_err(|error| format!("无法构造 multipart request: {error}"))
    }
}

#[derive(Clone)]
pub struct HttpExecutor {
    inner: Arc<HttpExecutorInner>,
}

struct HttpExecutorInner {
    api_client: Client,
    api_request_timeout: Duration,
    use_shell_proxy: bool,
    big_gate: BigRequestGate,
    download_clients: Mutex<HashMap<DownloadClientKey, Client>>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct DownloadClientKey {
    host: String,
    port: u16,
    addresses: Vec<std::net::IpAddr>,
}

impl HttpExecutor {
    pub fn new(config: &Config, paths: &AppPaths) -> Result<Self, String> {
        let api_client = configured_client_builder(config.use_shell_proxy)?
            .build()
            .map_err(|_| "无法初始化 HTTP client".to_owned())?;
        Ok(Self {
            inner: Arc::new(HttpExecutorInner {
                api_client,
                api_request_timeout: config.api_request_timeout,
                use_shell_proxy: config.use_shell_proxy,
                big_gate: BigRequestGate::new(paths.lock_file.clone()),
                download_clients: Mutex::new(HashMap::new()),
            }),
        })
    }

    pub async fn execute<F: RequestFactory>(
        &self,
        factory: &F,
        key: &SecretString,
        options: RetryOptions,
        notes: &mut Vec<String>,
    ) -> ApiResponse {
        let gate = if options.big_size {
            match self.inner.big_gate.acquire(notes).await {
                Ok(guard) => Some(guard),
                Err(error) => {
                    return synthetic_response(
                        0,
                        sanitize_sensitive(&error.to_string(), &[key.expose_secret()]),
                    );
                }
            }
        } else {
            None
        };
        let response = self.execute_locked(factory, key, options, notes).await;
        drop(gate);
        response
    }

    async fn execute_locked<F: RequestFactory>(
        &self,
        factory: &F,
        key: &SecretString,
        options: RetryOptions,
        notes: &mut Vec<String>,
    ) -> ApiResponse {
        let mut response = self.attempt(factory, key).await;
        let mut attempt_number = 1_usize;
        if response.status == 0 {
            append_retry_note(
                notes,
                0,
                NETWORK_RETRY_DELAY,
                attempt_number + 1,
                &error_detail(&response.body, &[key.expose_secret()]),
            );
            tokio::time::sleep(NETWORK_RETRY_DELAY).await;
            response = self.attempt(factory, key).await;
            attempt_number += 1;
        }
        if !options.enabled {
            return response;
        }
        let mut retry_attempt = 0_usize;
        while !response.is_success() {
            let detail = error_detail(&response.body, &[key.expose_secret()]);
            let effective = effective_retry_status(response.status, &detail);
            let jitter = Duration::from_secs_f64(rand::random_range(
                0.0_f64..=RETRY_JITTER_MAX.as_secs_f64(),
            ));
            let Some(delay) = retry_delay(
                effective,
                &response.headers,
                retry_attempt,
                options.big_size,
                SystemTime::now(),
                jitter,
            ) else {
                break;
            };
            append_retry_note(notes, effective, delay, attempt_number + 1, &detail);
            tokio::time::sleep(delay).await;
            retry_attempt += 1;
            response = self.attempt(factory, key).await;
            attempt_number += 1;
        }
        response
    }

    async fn attempt<F: RequestFactory>(&self, factory: &F, key: &SecretString) -> ApiResponse {
        let mut request = match factory.build(&self.inner.api_client, key).await {
            Ok(request) => request,
            Err(error) => {
                return synthetic_response(0, sanitize_sensitive(&error, &[key.expose_secret()]));
            }
        };
        *request.timeout_mut() = Some(self.inner.api_request_timeout);
        let response = match self.inner.api_client.execute(request).await {
            Ok(response) => response,
            Err(error) => {
                return synthetic_response(
                    0,
                    sanitize_sensitive(
                        &format!("{}: {error}", error_kind(&error)),
                        &[key.expose_secret()],
                    ),
                );
            }
        };
        read_response_capped(response, MAX_RESPONSE_BYTES).await
    }

    pub async fn download(&self, target: &ResolvedDownload) -> Result<reqwest::Response, String> {
        let client = self.download_client(target).await?;
        client
            .get(target.url.clone())
            .timeout(DOWNLOAD_REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|error| format!("远端图下载失败: {error}"))
    }

    async fn download_client(&self, target: &ResolvedDownload) -> Result<Client, String> {
        let key = DownloadClientKey {
            host: target.host.clone(),
            port: target.port,
            addresses: target.addresses.clone(),
        };
        let mut clients = self.inner.download_clients.lock().await;
        if let Some(client) = clients.get(&key) {
            return Ok(client.clone());
        }
        let mut builder = configured_client_builder(self.inner.use_shell_proxy)?;
        let socket_addresses = target
            .addresses
            .iter()
            .copied()
            .map(|ip| SocketAddr::new(ip, target.port))
            .collect::<Vec<_>>();
        if !socket_addresses.is_empty() {
            builder = builder.resolve_to_addrs(&target.host, &socket_addresses);
        }
        let client = builder
            .build()
            .map_err(|_| "无法初始化 DNS-pinned download client".to_owned())?;
        clients.insert(key, client.clone());
        Ok(client)
    }
}

pub fn json_request_bytes(body: &Value) -> Result<Vec<u8>, String> {
    serde_json::to_vec(body).map_err(|error| format!("JSON 序列化失败: {error}"))
}

async fn read_response_capped(response: reqwest::Response, limit: usize) -> ApiResponse {
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    if content_length(&headers).is_some_and(|length| length > limit as u64) {
        let length = content_length(&headers).unwrap_or_default();
        return ApiResponse {
            status: 413,
            headers,
            body: format!(
                "响应 Content-Length={:.1}MB 超过 {}MB 上限",
                length as f64 / 1024.0 / 1024.0,
                limit / 1024 / 1024
            )
            .into_bytes(),
        };
    }
    let mut body =
        Vec::with_capacity(content_length(&headers).unwrap_or(0).min(limit as u64) as usize);
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                return synthetic_response(0, format!("response stream error: {error}"));
            }
        };
        if body.len().saturating_add(chunk.len()) > limit {
            return ApiResponse {
                status: 413,
                headers,
                body: format!("响应体超过 {}MB 上限，已中断", limit / 1024 / 1024).into_bytes(),
            };
        }
        body.extend_from_slice(&chunk);
    }
    ApiResponse {
        status,
        headers,
        body,
    }
}

fn configured_client_builder(use_shell_proxy: bool) -> Result<ClientBuilder, String> {
    let mut builder = Client::builder()
        .redirect(Policy::none())
        .pool_max_idle_per_host(20)
        .pool_idle_timeout(Duration::from_secs(90))
        .tcp_keepalive(Duration::from_secs(60))
        .user_agent("micu-image-mcp-rust/0.2.0");
    if !use_shell_proxy {
        return Ok(builder.no_proxy());
    }
    let no_proxy = NoProxy::from_env();
    let http_proxy = proxy_environment_value(&["HTTP_PROXY", "http_proxy"]);
    let https_proxy = proxy_environment_value(&["HTTPS_PROXY", "https_proxy"]);
    let all_proxy = proxy_environment_value(&["ALL_PROXY", "all_proxy"]);
    if let Some(value) = http_proxy {
        let proxy = Proxy::http(value)
            .map_err(|_| "HTTP_PROXY 配置无效".to_owned())?
            .no_proxy(no_proxy.clone());
        builder = builder.proxy(proxy);
    }
    if let Some(value) = https_proxy {
        let proxy = Proxy::https(value)
            .map_err(|_| "HTTPS_PROXY 配置无效".to_owned())?
            .no_proxy(no_proxy.clone());
        builder = builder.proxy(proxy);
    }
    if let Some(value) = all_proxy {
        let proxy = Proxy::all(value)
            .map_err(|_| "ALL_PROXY 配置无效".to_owned())?
            .no_proxy(no_proxy);
        builder = builder.proxy(proxy);
    }
    Ok(builder)
}

fn proxy_environment_value(names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    })
}

fn bearer_header(key: &SecretString) -> Result<HeaderValue, String> {
    HeaderValue::from_str(&format!("Bearer {}", key.expose_secret()))
        .map_err(|_| "API key 含非法 header 字符".to_owned())
}

fn content_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
}

fn synthetic_response(status: u16, text: String) -> ApiResponse {
    ApiResponse {
        status,
        headers: HeaderMap::new(),
        body: text.into_bytes(),
    }
}

fn append_retry_note(
    notes: &mut Vec<String>,
    status: u16,
    delay: Duration,
    next_attempt: usize,
    detail: &str,
) {
    let reason = if detail.is_empty() {
        String::new()
    } else {
        format!("；原因：{detail}")
    };
    notes.push(format!(
        "HTTP {status} 可重试，等待 {:.1}s 后第 {next_attempt} 次尝试{reason}",
        delay.as_secs_f64()
    ));
}

fn error_kind(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "TimeoutError"
    } else if error.is_connect() {
        "ConnectError"
    } else if error.is_request() {
        "RequestError"
    } else if error.is_body() {
        "BodyError"
    } else {
        "NetworkError"
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn json_request_is_compact_literal_utf8() {
        let bytes = json_request_bytes(&json!({"prompt": "一只猫", "n": 1}))
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(
            bytes
                .windows("一只猫".len())
                .any(|window| window == "一只猫".as_bytes())
        );
        assert!(!bytes.windows(2).any(|window| window == b"\\u"));
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap_or_else(|error| panic!("{error}")),
            json!({"prompt": "一只猫", "n": 1})
        );
    }

    #[test]
    fn authorization_header_is_constructed_only_from_secret_wrapper() {
        let key: SecretString = "sk-test-secret".into();
        let header = bearer_header(&key).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(header.as_bytes(), b"Bearer sk-test-secret");
        assert!(!format!("{key:?}").contains("sk-test-secret"));
    }
}
