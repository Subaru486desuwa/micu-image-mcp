//! Read-only acceptance of the actual configured MCP launch, without image calls.
use micu_devtools::stdio::Session;
use micu_image_mcp::installer::codex::parse_config_launch;
use serde_json::json;
use std::{io::Write, process::Command};
fn probe() -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("expected configuration path")?;
    let text = std::fs::read_to_string(path)?;
    let launch = parse_config_launch(&text)?.ok_or("missing MCP launch")?;
    let mut command = Command::new(launch.command());
    command.args(launch.args()).envs(launch.env());
    let mut session = Session::spawn(command)?;
    let initialized = session.initialize()?;
    let tools = session.request("tools/list", json!({}))?;
    let info = session.call("server_info", json!({}))?;
    Ok(
        json!({"initialized_version":initialized["result"]["serverInfo"]["version"],"version":info["version"],"tool_count":tools["result"]["tools"].as_array().map(Vec::len),"models":info["available_models"],"model_catalog":info["model_catalog"],"api_key_configured":info["api_key_configured"],"grok_exposed":info.to_string().to_ascii_lowercase().contains("grok")}),
    )
}
fn main() {
    match probe() {
        Ok(value) => {
            let _result = writeln!(std::io::stdout(), "{value}");
        }
        Err(_) => {
            let _result = writeln!(
                std::io::stderr(),
                "MCP 配置启动验收失败；未输出配置或原始错误"
            );
            std::process::exit(1);
        }
    }
}
