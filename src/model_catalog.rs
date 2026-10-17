//! Credential-scoped, bounded discovery of compatible Image2 models.
use crate::{
    config::Config,
    domain::routing::{SUPPORTED_IMAGE_MODELS, canonical_model},
    http::client::HttpExecutor,
};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct ModelCatalog {
    config: Arc<Config>,
    entries: Arc<Mutex<HashMap<Vec<u8>, Entry>>>,
}
struct Entry {
    models: Vec<String>,
    checked: Option<Instant>,
    success: Option<u64>,
    online: bool,
}
#[derive(Clone)]
pub struct Snapshot {
    pub models: Vec<String>,
    pub source: &'static str,
    pub last_success: Option<u64>,
}
impl Snapshot {
    pub fn info(&self) -> Value {
        json!({"source":self.source,"refresh_status":if self.source=="online" {"fresh"} else {"unavailable"},"last_success_unix":self.last_success,"ttl_seconds":3600,"capabilities":"Images protocol compatibility; per-model edit capabilities are not verified"})
    }
}
impl ModelCatalog {
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            config,
            entries: Arc::default(),
        }
    }
    pub async fn snapshot(&self, key: &SecretString) -> Snapshot {
        // The digest is solely an in-memory identity partition, never a persisted credential.
        let identity = Sha256::digest(key.expose_secret().as_bytes()).to_vec();
        let mut entries = self.entries.lock().await;
        let entry = entries.entry(identity).or_insert_with(|| Entry {
            models: SUPPORTED_IMAGE_MODELS.iter().map(|s| (*s).into()).collect(),
            checked: None,
            success: None,
            online: false,
        });
        let ttl = if entry.online {
            self.config.catalog_ttl
        } else {
            self.config.catalog_ttl.min(Duration::from_secs(60))
        };
        if entry.checked.is_none_or(|checked| checked.elapsed() >= ttl) {
            let fetched = self.fetch(key).await;
            entry.checked = Some(Instant::now());
            entry.online = fetched.is_some();
            if let Some(models) = fetched {
                entry.models = models;
                entry.success = Some(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                );
            }
        }
        Snapshot {
            models: entry.models.clone(),
            source: if entry.online {
                "online"
            } else if entry.success.is_some() {
                "stale_cache"
            } else {
                "offline_baseline"
            },
            last_success: entry.success,
        }
    }
    async fn fetch(&self, key: &SecretString) -> Option<Vec<String>> {
        if key.expose_secret().trim().is_empty() {
            return None;
        }
        let response = HttpExecutor::fetch_models(&self.config, key).await.ok()?;
        let data = response.get("data")?.as_array()?;
        let mut models = Vec::new();
        for model in data {
            let id = model.get("id")?.as_str()?;
            // The current upstream provides endpoint types, but no image capability metadata.
            // Do not infer compatibility for unrelated families from a generic "openai" endpoint.
            let family = id == "gpt-image-2"
                || id.starts_with("gpt-image-2-")
                || id.starts_with("gpt-image-2.");
            let valid = id.len() <= 200
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-._".contains(&b));
            let endpoints_ok = model.get("supported_endpoint_types").is_none_or(|v| {
                v.as_array()
                    .is_some_and(|a| a.iter().any(|v| v == "openai"))
            });
            // Upstream model IDs are untrusted: discard reflected credentials rather
            // than publishing a redacted, but invalid, callable model identifier.
            let contains_secret = [key.expose_secret(), self.config.api_key.expose_secret()]
                .iter()
                .any(|secret| !secret.is_empty() && id.contains(*secret));
            if family && valid && endpoints_ok && !contains_secret {
                models.push(canonical_model(id).to_owned());
            }
        }
        models.sort();
        models.dedup();
        Some(models)
    }
}
