mod image2;

use async_trait::async_trait;
use secrecy::SecretString;

use crate::{
    fs::input::ValidatedImage,
    http::client::{ApiResponse, RetryOptions},
};

pub use image2::Image2Provider;

pub struct ImageResponse {
    pub response: ApiResponse,
    pub model: String,
    pub used_fallback: bool,
}

impl ImageResponse {
    pub fn new(response: ApiResponse, model: &str) -> Self {
        Self {
            response,
            model: model.to_owned(),
            used_fallback: false,
        }
    }
}

pub struct GenerateRequest<'a> {
    pub model: &'a str,
    pub prompt: &'a str,
    pub size: &'a str,
    pub quality: Option<&'a str>,
    pub response_format: Option<&'a str>,
}

pub struct EditRequest<'a> {
    pub model: &'a str,
    pub prompt: &'a str,
    pub size: &'a str,
    pub quality: Option<&'a str>,
    pub response_format: Option<&'a str>,
    pub images: &'a [(&'a str, &'a ValidatedImage)],
    pub mask: Option<&'a ValidatedImage>,
}

#[async_trait]
pub trait ImageProvider: Send + Sync {
    async fn generate(
        &self,
        request: GenerateRequest<'_>,
        key: &SecretString,
        retry: RetryOptions,
        notes: &mut Vec<String>,
    ) -> ImageResponse;

    async fn edit(
        &self,
        request: EditRequest<'_>,
        key: &SecretString,
        retry: RetryOptions,
        notes: &mut Vec<String>,
    ) -> ImageResponse;
}
