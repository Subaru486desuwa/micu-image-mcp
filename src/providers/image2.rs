use async_trait::async_trait;
use secrecy::SecretString;
use serde_json::{Map, Value};
use url::Url;

use crate::{
    http::client::{
        ApiResponse, HttpExecutor, JsonRequestFactory, MultipartRequestFactory, RetryOptions,
        UploadPart,
    },
    providers::{EditRequest, GenerateRequest, ImageProvider},
};

#[derive(Clone)]
pub struct Image2Provider {
    generations_url: Url,
    edits_url: Url,
    http: HttpExecutor,
}

impl Image2Provider {
    pub fn new(base_url: &Url, http: HttpExecutor) -> Result<Self, String> {
        Ok(Self {
            generations_url: endpoint(base_url, "/v1/images/generations")?,
            edits_url: endpoint(base_url, "/v1/images/edits")?,
            http,
        })
    }
}

#[async_trait]
impl ImageProvider for Image2Provider {
    async fn generate(
        &self,
        request: GenerateRequest<'_>,
        key: &SecretString,
        retry: RetryOptions,
        notes: &mut Vec<String>,
    ) -> ApiResponse {
        let mut body = Map::new();
        body.insert("model".into(), Value::String(request.model.into()));
        body.insert("prompt".into(), Value::String(request.prompt.into()));
        body.insert("n".into(), Value::from(1));
        body.insert("size".into(), Value::String(request.size.into()));
        body.insert(
            "response_format".into(),
            Value::String(request.response_format.into()),
        );
        if let Some(quality) = request.quality {
            body.insert("quality".into(), Value::String(quality.into()));
        }
        self.http
            .execute(
                &JsonRequestFactory {
                    url: self.generations_url.clone(),
                    body: Value::Object(body),
                },
                key,
                retry,
                notes,
            )
            .await
    }

    async fn edit(
        &self,
        request: EditRequest<'_>,
        key: &SecretString,
        retry: RetryOptions,
        notes: &mut Vec<String>,
    ) -> ApiResponse {
        let fields = vec![
            ("model".into(), request.model.into()),
            ("prompt".into(), request.prompt.into()),
            ("size".into(), request.size.into()),
            ("response_format".into(), request.response_format.into()),
        ];
        let mut files = request
            .images
            .iter()
            .map(|(field_name, image)| UploadPart::from_validated(*field_name, image))
            .collect::<Vec<_>>();
        if let Some(mask) = request.mask {
            files.push(UploadPart {
                field_name: "mask".into(),
                filename: "mask.png".into(),
                mime: "image/png",
                file: mask.file.clone(),
                size_bytes: mask.size_bytes,
            });
        }
        self.http
            .execute(
                &MultipartRequestFactory {
                    url: self.edits_url.clone(),
                    fields,
                    files,
                },
                key,
                retry,
                notes,
            )
            .await
    }
}

fn endpoint(base_url: &Url, path: &str) -> Result<Url, String> {
    let raw = format!("{}{}", base_url.as_str().trim_end_matches('/'), path);
    Url::parse(&raw).map_err(|error| format!("无法构造 Image2 endpoint: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_preserves_optional_base_path_without_double_slash() {
        let root = Url::parse("https://example.test/").unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            endpoint(&root, "/v1/images/edits")
                .unwrap_or_else(|error| panic!("{error}"))
                .as_str(),
            "https://example.test/v1/images/edits"
        );
        let prefixed =
            Url::parse("https://example.test/proxy/").unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            endpoint(&prefixed, "/v1/images/edits")
                .unwrap_or_else(|error| panic!("{error}"))
                .as_str(),
            "https://example.test/proxy/v1/images/edits"
        );
    }
}
