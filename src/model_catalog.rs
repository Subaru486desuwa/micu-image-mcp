//! Manually maintained Images-protocol model directory. Never performs network I/O.
use crate::domain::routing::SUPPORTED_IMAGE_MODELS;
use serde_json::{Value, json};

pub fn info(default_generate: &str, default_edit: &str) -> Value {
    json!({
        "source": "manual",
        "refresh_status": "not_applicable",
        "last_success_unix": null,
        "ttl_seconds": null,
        "capabilities": "Manually maintained Images protocol models; upstream availability and per-model edit capabilities are not verified",
        "default_generate_available": SUPPORTED_IMAGE_MODELS.contains(&default_generate),
        "default_edit_available": SUPPORTED_IMAGE_MODELS.contains(&default_edit)
    })
}
