mod batch;
mod common;
mod edit;
mod edit_session;
mod generate;
mod multi_reference;
mod server_info;
mod types;

use std::sync::Arc;

use crate::{
    config::{AppPaths, Config},
    fs::input::InputStore,
    fs::output_store::OutputStore,
    fs::response_output::OutputSaver,
    providers::ImageProvider,
};

pub use common::{SecretArg, ToolFailure};
pub use types::{BatchParams, EditParams, GenerateParams, MultiReferenceParams};

#[derive(Clone)]
pub struct ToolEngine {
    pub(crate) config: Arc<Config>,
    pub(crate) catalog: crate::model_catalog::ModelCatalog,
    pub(crate) paths: Arc<AppPaths>,
    pub(crate) input_store: InputStore,
    pub(crate) output_store: OutputStore,
    pub(crate) output: OutputSaver,
    pub(crate) provider: Arc<dyn ImageProvider>,
}

impl ToolEngine {
    pub fn new(
        config: Arc<Config>,
        paths: Arc<AppPaths>,
        output_store: OutputStore,
        output: OutputSaver,
        provider: Arc<dyn ImageProvider>,
    ) -> Self {
        let input_store = InputStore::new(paths.as_ref());
        Self {
            catalog: crate::model_catalog::ModelCatalog::new(config.clone()),
            config,
            paths,
            input_store,
            output_store,
            output,
            provider,
        }
    }
}

impl ToolEngine {
    async fn model_error(
        &self,
        requested: Option<&str>,
        default: &str,
        key: Option<&SecretArg>,
    ) -> Option<String> {
        let override_key = key.map(|k| {
            secrecy::SecretString::from(
                secrecy::ExposeSecret::expose_secret(&k.0).trim().to_owned(),
            )
        });
        let key = override_key
            .as_ref()
            .filter(|k| !secrecy::ExposeSecret::expose_secret(*k).trim().is_empty())
            .unwrap_or(&self.config.api_key);
        let snapshot = self.catalog.snapshot(key).await;
        let model = crate::domain::routing::canonical_model(requested.unwrap_or(default));
        if snapshot.models.iter().any(|v| v == model) {
            None
        } else {
            let model = crate::http::response::sanitize_sensitive(
                model,
                &[
                    secrecy::ExposeSecret::expose_secret(key),
                    secrecy::ExposeSecret::expose_secret(&self.config.api_key),
                ],
            );
            Some(format!(
                "不支持 model={model:?}；当前可用模型: {}",
                snapshot.models.join(" / ")
            ))
        }
    }
    pub async fn catalog_server_info(&self) -> Result<serde_json::Value, ToolFailure> {
        let snapshot = self.catalog.snapshot(&self.config.api_key).await;
        let mut info = self.server_info()?;
        info["available_models"] = serde_json::json!(snapshot.models);
        info["model_catalog"] = snapshot.info();
        info["model_catalog"]["default_generate_available"] =
            serde_json::json!(snapshot.models.contains(&self.config.default_model));
        info["model_catalog"]["default_edit_available"] =
            serde_json::json!(snapshot.models.contains(&self.config.default_edit_model));
        Ok(info)
    }
}
