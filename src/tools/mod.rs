mod batch;
mod common;
mod edit;
mod generate;
mod multi_reference;
mod server_info;

use std::sync::Arc;

use crate::{
    config::Config,
    download::SystemResolver,
    http_client::HttpExecutor,
    output::OutputSaver,
    providers::{Image2Provider, ImageProvider},
    storage::Storage,
};

pub use common::{SecretArg, ToolFailure};
pub use edit::EditParams;
pub use generate::GenerateParams;
pub use multi_reference::MultiReferenceParams;

#[derive(Clone)]
pub struct ToolEngine {
    pub(crate) config: Arc<Config>,
    pub(crate) storage: Storage,
    pub(crate) output: OutputSaver,
    pub(crate) provider: Arc<dyn ImageProvider>,
}

impl ToolEngine {
    pub fn new(
        config: Arc<Config>,
        storage: Storage,
        output: OutputSaver,
        provider: Arc<dyn ImageProvider>,
    ) -> Self {
        Self {
            config,
            storage,
            output,
            provider,
        }
    }

    pub fn production(config: Arc<Config>) -> Result<Self, String> {
        let storage = Storage::new(config.as_ref())?;
        let http = HttpExecutor::new(config.as_ref())?;
        let provider = Arc::new(Image2Provider::new(&config.base_url, http.clone())?);
        let output = OutputSaver::new(
            config.clone(),
            storage.clone(),
            http,
            Arc::new(SystemResolver),
        );
        Ok(Self::new(config, storage, output, provider))
    }
}
pub use batch::BatchParams;
