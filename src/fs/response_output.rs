use std::sync::Arc;

use reqwest::header::{CONTENT_LENGTH, LOCATION};

use crate::{
    config::Config,
    fs::output_store::{OutputDirectory, OutputStore, SavedImage},
    http::client::{DOWNLOAD_WALL_TIMEOUT, HttpExecutor},
    http::download::{Resolver, validate_download_url},
    http::response::{ImagePayload, extract_first_payload},
};

#[derive(Clone)]
pub struct OutputSaver {
    config: Arc<Config>,
    output_store: OutputStore,
    http: HttpExecutor,
    resolver: Arc<dyn Resolver>,
}

impl OutputSaver {
    pub fn new(
        config: Arc<Config>,
        output_store: OutputStore,
        http: HttpExecutor,
        resolver: Arc<dyn Resolver>,
    ) -> Self {
        Self {
            config,
            output_store,
            http,
            resolver,
        }
    }

    pub async fn save_first(
        &self,
        response_body: &[u8],
        location: &OutputDirectory,
        basename: &str,
    ) -> Result<SavedImage, String> {
        match extract_first_payload(response_body)? {
            ImagePayload::Base64(encoded) => {
                self.output_store
                    .save_base64(&encoded, location, basename)
                    .await
            }
            ImagePayload::Url(url) => self.save_url(&url, location, basename).await,
        }
    }

    async fn save_url(
        &self,
        url: &str,
        location: &OutputDirectory,
        basename: &str,
    ) -> Result<SavedImage, String> {
        let target =
            validate_download_url(self.config.as_ref(), url, self.resolver.as_ref()).await?;
        let operation = async {
            let response = self.http.download(&target).await?;
            if !response.status().is_success() {
                return Err(download_status_error(&response, url));
            }
            let content_length = response
                .headers()
                .get(CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse().ok());
            self.output_store
                .save_stream(
                    response.bytes_stream(),
                    content_length,
                    location,
                    basename,
                    &format!("远端图 {}", truncate_url(url)),
                )
                .await
        };
        tokio::time::timeout(DOWNLOAD_WALL_TIMEOUT, operation)
            .await
            .map_err(|_| {
                format!(
                    "远端图下载超过 {:.0}s 墙钟上限，已中断",
                    DOWNLOAD_WALL_TIMEOUT.as_secs_f64()
                )
            })?
    }
}

fn download_status_error(response: &reqwest::Response, url: &str) -> String {
    let status = response.status();
    if status.is_redirection() {
        let location = response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("<missing>");
        return format!(
            "Redirect response '{} {}' for url '{}'
Redirect location: '{}'",
            status.as_u16(),
            status.canonical_reason().unwrap_or("Unknown"),
            truncate_url(url),
            truncate_url(location)
        );
    }
    format!(
        "HTTP {} {} 下载失败: {}",
        status.as_u16(),
        status.canonical_reason().unwrap_or("Unknown"),
        truncate_url(url)
    )
}

fn truncate_url(url: &str) -> String {
    url.chars().take(200).collect()
}
