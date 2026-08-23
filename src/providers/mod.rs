mod image2;

use async_trait::async_trait;
use secrecy::SecretString;

use crate::{
    fs::input::ValidatedImage,
    http::client::{ApiResponse, RetryOptions},
};

pub use image2::Image2Provider;

pub struct GenerateRequest<'a> {
    pub model: &'a str,
    pub prompt: &'a str,
    pub size: &'a str,
    pub quality: Option<&'a str>,
    pub response_format: &'a str,
}

pub struct EditRequest<'a> {
    pub model: &'a str,
    pub prompt: &'a str,
    pub size: &'a str,
    pub response_format: &'a str,
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
    ) -> ApiResponse;

    async fn edit(
        &self,
        request: EditRequest<'_>,
        key: &SecretString,
        retry: RetryOptions,
        notes: &mut Vec<String>,
    ) -> ApiResponse;
}
