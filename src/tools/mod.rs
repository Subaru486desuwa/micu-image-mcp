mod batch;
mod common;
mod edit;
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
            config,
            paths,
            input_store,
            output_store,
            output,
            provider,
        }
    }
}
