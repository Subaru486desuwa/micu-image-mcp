use async_trait::async_trait;
use secrecy::SecretString;
use serde_json::{Map, Value};
use url::Url;

use crate::{
    domain::routing::suffix_fallback,
    http::client::{
        HttpExecutor, JsonRequestFactory, MultipartRequestFactory, RequestFactory, RetryOptions,
        UploadPart,
    },
    http::response::model_rejected,
    providers::{EditRequest, GenerateRequest, ImageProvider, ImageResponse},
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

    async fn execute<F: ImageRequestFactory>(
        &self,
        mut factory: F,
        model: &str,
        key: &SecretString,
        retry: RetryOptions,
        notes: &mut Vec<String>,
    ) -> ImageResponse {
        let response = self.http.execute(&factory, key, retry, notes).await;
        let Some(fallback) = suffix_fallback(model)
            .filter(|_| model_rejected(response.status, &response.body, model))
        else {
            return ImageResponse::new(response, model);
        };
        notes.push(format!(
            "上游明确拒绝模型 {model} → 使用兼容 ID {fallback} 补试一次"
        ));
        factory.set_model(fallback);
        ImageResponse {
            response: self.http.execute(&factory, key, retry, notes).await,
            model: fallback.to_owned(),
            used_fallback: true,
        }
    }
}

trait ImageRequestFactory: RequestFactory {
    fn set_model(&mut self, model: &str);
}
impl ImageRequestFactory for JsonRequestFactory {
    fn set_model(&mut self, model: &str) {
        self.body["model"] = Value::String(model.to_owned());
    }
}
impl ImageRequestFactory for MultipartRequestFactory {
    fn set_model(&mut self, model: &str) {
        for (name, value) in &mut self.fields {
            if name == "model" {
                *value = model.to_owned();
            }
        }
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
    ) -> ImageResponse {
        let mut body = Map::new();
        body.insert("model".into(), Value::String(request.model.into()));
        body.insert("prompt".into(), Value::String(request.prompt.into()));
        body.insert("n".into(), Value::from(1));
        body.insert("size".into(), Value::String(request.size.into()));
        if let Some(response_format) = request.response_format {
            body.insert(
                "response_format".into(),
                Value::String(response_format.into()),
            );
        }
        if let Some(quality) = request.quality {
            body.insert("quality".into(), Value::String(quality.into()));
        }
        self.execute(
            JsonRequestFactory {
                url: self.generations_url.clone(),
                body: Value::Object(body),
            },
            request.model,
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
    ) -> ImageResponse {
        let mut fields = vec![
            ("model".into(), request.model.into()),
            ("prompt".into(), request.prompt.into()),
            ("size".into(), request.size.into()),
        ];
        if let Some(response_format) = request.response_format {
            fields.push(("response_format".into(), response_format.into()));
        }
        if let Some(quality) = request.quality {
            fields.push(("quality".into(), quality.into()));
        }
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
        self.execute(
            MultipartRequestFactory {
                url: self.edits_url.clone(),
                fields,
                files,
            },
            request.model,
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
