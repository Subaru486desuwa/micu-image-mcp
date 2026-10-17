use std::sync::Arc;

use rmcp::{
    ErrorData as McpError, ServerHandler,
    model::{
        CacheScope, CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock,
        Implementation, ListPromptsResult, ListResourceTemplatesResult, ListResourcesResult,
        ListToolsResult, PaginatedRequestParams, PromptsCapability, ProtocolVersion,
        ResourcesCapability, ServerCapabilities, ServerInfo, Tool, ToolsCapability,
    },
    service::{MaybeSendFuture, RequestContext, RoleServer},
};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::tools::{ToolEngine, ToolFailure};

#[derive(Clone)]
pub struct MicuServer {
    engine: ToolEngine,
    tools: Arc<Vec<Tool>>,
}

impl MicuServer {
    pub fn new(engine: ToolEngine) -> Result<Self, String> {
        Ok(Self {
            engine,
            tools: Arc::new(load_tool_catalog()?),
        })
    }

    async fn dispatch(
        &self,
        name: &str,
        arguments: Option<rmcp::model::JsonObject>,
    ) -> Result<Value, ToolFailure> {
        let value = Value::Object(arguments.unwrap_or_default());
        match name {
            "image_generate" => {
                self.engine
                    .image_generate(parse_parameters(name, value)?)
                    .await
            }
            "image_edit" => self.engine.image_edit(parse_parameters(name, value)?).await,
            "image_batch_edit" => {
                self.engine
                    .image_batch_edit(parse_parameters(name, value)?)
                    .await
            }
            "image_multi_reference" => {
                self.engine
                    .image_multi_reference(parse_parameters(name, value)?)
                    .await
            }
            "server_info" => self.engine.catalog_server_info().await,
            _ => Err(ToolFailure(format!("未知 tool: {name}"))),
        }
    }
}

impl ServerHandler for MicuServer {
    fn get_info(&self) -> ServerInfo {
        let mut prompts = PromptsCapability::default();
        prompts.list_changed = Some(false);
        let mut resources = ResourcesCapability::default();
        resources.subscribe = Some(false);
        resources.list_changed = Some(false);
        let mut tools = ToolsCapability::default();
        tools.list_changed = Some(false);
        let capabilities = ServerCapabilities::builder()
            .enable_experimental()
            .enable_prompts_with(prompts)
            .enable_resources_with(resources)
            .enable_tools_with(tools)
            .build();
        ServerInfo::new(capabilities)
            .with_server_info(Implementation::new("micu-image", env!("CARGO_PKG_VERSION")))
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, McpError>> + MaybeSendFuture + '_ {
        let mut result = ListToolsResult::with_all_items(self.tools.as_ref().clone());
        if modern_cache_fields(&context) {
            result = result.with_ttl_ms(0).with_cache_scope(CacheScope::Private);
        }
        std::future::ready(Ok(result))
    }

    fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListPromptsResult, McpError>> + MaybeSendFuture + '_ {
        let mut result = ListPromptsResult::default();
        if modern_cache_fields(&context) {
            result = result.with_ttl_ms(0).with_cache_scope(CacheScope::Private);
        }
        std::future::ready(Ok(result))
    }

    fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourcesResult, McpError>> + MaybeSendFuture + '_ {
        let mut result = ListResourcesResult::default();
        if modern_cache_fields(&context) {
            result = result.with_ttl_ms(0).with_cache_scope(CacheScope::Private);
        }
        std::future::ready(Ok(result))
    }

    fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourceTemplatesResult, McpError>> + MaybeSendFuture + '_
    {
        let mut result = ListResourceTemplatesResult::default();
        if modern_cache_fields(&context) {
            result = result.with_ttl_ms(0).with_cache_scope(CacheScope::Private);
        }
        std::future::ready(Ok(result))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tools.iter().find(|tool| tool.name == name).cloned()
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let name = request.name.into_owned();
        let result = match self.dispatch(&name, request.arguments).await {
            Ok(value) => {
                structured_result(value).map_err(|error| McpError::internal_error(error, None))?
            }
            Err(error) => CallToolResult::error(vec![ContentBlock::text(format!(
                "Error executing tool {name}: {error}"
            ))]),
        };
        Ok(result.into())
    }
}

// Modern clients require both cache fields even when caching is disabled. Older
// handshakes retain the frozen wire shape; no session/image data enters a shared cache.
fn modern_cache_fields(context: &RequestContext<RoleServer>) -> bool {
    context.protocol_version() == Some(ProtocolVersion::V_2026_07_28)
}

fn parse_parameters<T: DeserializeOwned>(name: &str, value: Value) -> Result<T, ToolFailure> {
    serde_json::from_value(value)
        .map_err(|error| ToolFailure(format!("参数校验失败 ({name}): {error}")))
}

fn structured_result(value: Value) -> Result<CallToolResult, String> {
    let text = serde_json::to_string_pretty(&value)
        .map_err(|error| format!("tool result JSON 序列化失败: {error}"))?;
    let mut result = CallToolResult::structured(value);
    result.content = vec![ContentBlock::text(text)];
    Ok(result)
}

fn load_tool_catalog() -> Result<Vec<Tool>, String> {
    let fixture: Value = serde_json::from_str(include_str!("contracts/tool_contract.json"))
        .map_err(|error| format!("Rust tools/list contract 无法解析: {error}"))?;
    let tools = fixture
        .pointer("/result/tools")
        .cloned()
        .ok_or_else(|| "Rust tools/list contract 缺 result.tools".to_owned())?;
    serde_json::from_value(tools)
        .map_err(|error| format!("tools/list contract 无法转换为 rmcp Tool: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_tool_catalog_matches_the_rust_contract() {
        let catalog = load_tool_catalog().unwrap_or_else(|error| panic!("{error}"));
        let actual = serde_json::to_value(&catalog).unwrap_or_else(|error| panic!("{error}"));
        let fixture: Value = serde_json::from_str(include_str!("contracts/tool_contract.json"))
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(actual, fixture["result"]["tools"]);
        assert_eq!(catalog.len(), 5);
    }

    #[test]
    fn edit_schema_exposes_optional_session_state_without_adding_tools() {
        let fixture: Value =
            serde_json::from_str(include_str!("contracts/tool_contract.json")).unwrap();
        let edit = fixture["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "image_edit")
            .unwrap();
        let schema = &edit["inputSchema"];
        assert_eq!(schema["required"], serde_json::json!(["prompt"]));
        assert_eq!(
            schema["properties"]["session_id"]["anyOf"][0]["pattern"],
            "^(new|edit_[0-9a-f]{32})$"
        );
        assert_eq!(
            schema["properties"]["context"]["additionalProperties"]["anyOf"][1]["type"],
            "null"
        );
        assert_eq!(
            schema["properties"]["parent_revision"]["anyOf"][0]["minimum"],
            1
        );
    }

    #[test]
    fn generation_and_edit_share_session_fields_and_keep_legacy_generation_required_fields() {
        let fixture: Value =
            serde_json::from_str(include_str!("contracts/tool_contract.json")).unwrap();
        let tools = fixture["result"]["tools"].as_array().unwrap();
        let generate = tools
            .iter()
            .find(|tool| tool["name"] == "image_generate")
            .unwrap();
        let edit = tools
            .iter()
            .find(|tool| tool["name"] == "image_edit")
            .unwrap();
        assert_eq!(
            generate["inputSchema"]["required"],
            serde_json::json!(["prompt"])
        );
        for field in ["parent_revision", "context"] {
            assert_eq!(
                generate["inputSchema"]["properties"][field],
                edit["inputSchema"]["properties"][field]
            );
        }
        assert_eq!(
            generate["inputSchema"]["properties"]["session_id"]["anyOf"],
            edit["inputSchema"]["properties"]["session_id"]["anyOf"]
        );
    }
}
