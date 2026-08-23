use secrecy::ExposeSecret;
use serde_json::Value;

use crate::config::ResponseFormat;

use super::{ToolEngine, ToolFailure};

impl ToolEngine {
    pub fn server_info(&self) -> Result<Value, ToolFailure> {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../tests/contract/fixtures/python/server-info.json"
        ))
        .map_err(|error| ToolFailure(format!("server_info contract fixture 无法解析: {error}")))?;
        let mut info = fixture
            .pointer("/result/structuredContent")
            .cloned()
            .ok_or_else(|| {
                ToolFailure("server_info contract fixture 缺 structuredContent".into())
            })?;
        set_top(
            &mut info,
            "version",
            Value::String(env!("CARGO_PKG_VERSION").into()),
        )?;
        set_top(
            &mut info,
            "base_url",
            Value::String(self.config.base_url.as_str().trim_end_matches('/').into()),
        )?;
        set_top(
            &mut info,
            "grok_base_url",
            Value::String(self.config.grok_base_url.clone()),
        )?;
        set_top(
            &mut info,
            "default_model",
            Value::String(self.config.default_model.clone()),
        )?;
        set_top(
            &mut info,
            "grok_default_model",
            Value::String(self.config.xai_model.clone()),
        )?;
        let grok_size_mode = if matches!(
            self.config.grok_size_mode.as_str(),
            "backend" | "contain" | "cover" | "stretch"
        ) {
            self.config.grok_size_mode.clone()
        } else {
            "contain".into()
        };
        set_top(&mut info, "grok_size_mode", Value::String(grok_size_mode))?;
        set_top(
            &mut info,
            "default_save_dir",
            Value::String(path_text(&self.paths.default_save_dir, "default_save_dir")?),
        )?;
        set_top(
            &mut info,
            "api_key_configured",
            Value::Bool(!self.config.api_key.expose_secret().is_empty()),
        )?;
        set_top(
            &mut info,
            "grok_api_key_configured",
            Value::Bool(!self.config.grok_api_key.expose_secret().is_empty()),
        )?;
        let response_format = match self.config.response_format {
            ResponseFormat::Auto => "auto",
            ResponseFormat::Url => "url",
            ResponseFormat::B64Json => "b64_json",
        };
        set_top(
            &mut info,
            "response_format",
            Value::String(response_format.into()),
        )?;
        set_top(
            &mut info,
            "response_formats_to_try",
            Value::Array(
                self.config
                    .response_formats_to_try
                    .iter()
                    .map(|value| Value::String((*value).into()))
                    .collect(),
            ),
        )?;
        set_top(
            &mut info,
            "trusted_download_hosts",
            Value::Array(
                self.config
                    .trusted_download_hosts
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        )?;
        set_top(
            &mut info,
            "allow_fake_ip_download",
            Value::Bool(self.config.allow_fake_ip_download),
        )?;
        set_nested(
            &mut info,
            "safety_constraints",
            "save_dir_root",
            Value::String(format!(
                "所有输出强制落在 MICU_SAVE_DIR_ROOT={} 之下；传 root 之外路径会被拒",
                path_text(&self.paths.save_root, "save root")?
            )),
        )?;
        set_nested(
            &mut info,
            "safety_constraints",
            "input_image_validation",
            Value::String(input_validation_description(&self.paths)?),
        )?;
        set_nested(
            &mut info,
            "retry_policy",
            "concurrency_2k_4k",
            Value::String(format!(
                "双层锁: (1) 进程内 tokio::sync::Semaphore(1) 同 MCP 进程内并发本地排队; (2) 跨进程 fs4 try_lock + async sleep 轮询 @ {} —— Python/Rust 兼容期共用此锁；取消或错误返回由 RAII 释放锁和 file handle。",
                path_text(&self.paths.lock_file, "lock file")?
            )),
        )?;
        Ok(info)
    }
}

fn path_text(path: &std::path::Path, context: &str) -> Result<String, ToolFailure> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| ToolFailure(format!("{context} 不是合法 Unicode，无法写入 server_info")))
}

fn input_validation_description(paths: &crate::config::AppPaths) -> Result<String, ToolFailure> {
    let relative_rule = if let Some(root) = &paths.input_root {
        format!(
            "相对输入路径相对于 MICU_INPUT_ROOT={} 解析，所有输入均受该 capability root 限制",
            path_text(root, "input root")?
        )
    } else {
        format!(
            "未设置 MICU_INPUT_ROOT 时，相对输入路径相对于 server 启动时捕获的 cwd={} 解析",
            path_text(&paths.startup_cwd, "startup cwd")?
        )
    };
    Ok(format!(
        "所有输入图先检查文件大小与 magic，再以 allocation/像素/边长硬上限执行完整解码；仅允许 PNG/JPEG/WebP，截断、损坏、伪装格式和解压炸弹会在请求前拒绝；{relative_rule}"
    ))
}

fn set_top(info: &mut Value, key: &str, value: Value) -> Result<(), ToolFailure> {
    let object = info
        .as_object_mut()
        .ok_or_else(|| ToolFailure("server_info template 顶层不是 object".into()))?;
    object.insert(key.into(), value);
    Ok(())
}

fn set_nested(info: &mut Value, section: &str, key: &str, value: Value) -> Result<(), ToolFailure> {
    let object = info
        .get_mut(section)
        .and_then(Value::as_object_mut)
        .ok_or_else(|| ToolFailure(format!("server_info template 缺 section={section}")))?;
    object.insert(key.into(), value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Arc};

    use async_trait::async_trait;
    use secrecy::SecretString;

    use crate::{
        config::{Config, test_paths},
        fs::output_store::OutputStore,
        fs::response_output::OutputSaver,
        http::client::{ApiResponse, HttpExecutor, RetryOptions},
        http::download::SystemResolver,
        providers::{EditRequest, GenerateRequest, ImageProvider},
    };

    use super::*;

    struct NeverProvider;

    #[async_trait]
    impl ImageProvider for NeverProvider {
        async fn generate(
            &self,
            _request: GenerateRequest<'_>,
            _key: &SecretString,
            _retry: RetryOptions,
            _notes: &mut Vec<String>,
        ) -> ApiResponse {
            panic!("server_info must not call provider")
        }
        async fn edit(
            &self,
            _request: EditRequest<'_>,
            _key: &SecretString,
            _retry: RetryOptions,
            _notes: &mut Vec<String>,
        ) -> ApiResponse {
            panic!("server_info must not call provider")
        }
    }

    #[test]
    fn server_info_preserves_compatibility_keys_but_reports_rust_runtime_truth() {
        let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
        let out = temp.path().join("out");
        let environment = BTreeMap::from([
            (
                "MICU_SAVE_DIR_ROOT".into(),
                out.to_string_lossy().into_owned(),
            ),
            ("MICU_SAVE_DIR".into(), out.to_string_lossy().into_owned()),
            ("MICU_API_KEY".into(), "sk-test".into()),
        ]);
        let config =
            Arc::new(Config::from_map(&environment).unwrap_or_else(|error| panic!("{error}")));
        let paths = Arc::new(test_paths(temp.path(), environment));
        let storage = OutputStore::new(paths.as_ref()).unwrap_or_else(|error| panic!("{error}"));
        let http = HttpExecutor::new(config.as_ref(), paths.as_ref())
            .unwrap_or_else(|error| panic!("{error}"));
        let output = OutputSaver::new(
            config.clone(),
            storage.clone(),
            http,
            Arc::new(SystemResolver),
        );
        let engine = ToolEngine::new(config, paths, storage, output, Arc::new(NeverProvider));
        let info = engine
            .server_info()
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(info["version"], "0.3.0");
        assert_eq!(
            info["available_models"],
            serde_json::json!(["gpt-image-2", "gpt-image-2-openai"])
        );
        assert_eq!(info["grok_channel_enabled"], false);
        assert_eq!(info["api_key_configured"], true);
        assert!(info["retry_policy"]["concurrency_2k_4k"].as_str().is_some_and(|text| text.contains("tokio::sync::Semaphore") && text.contains("fs4")));
        assert!(
            info["safety_constraints"]["input_image_validation"]
                .as_str()
                .is_some_and(|text| {
                    text.contains("完整解码")
                        && text.contains("allocation")
                        && text.contains("server 启动时捕获的 cwd")
                })
        );
        assert!(
            info["retry_policy"]["concurrency_2k_4k"]
                .as_str()
                .is_some_and(|text| text.contains("bigsize.lock") && text.contains("Python/Rust"))
        );
    }
}
