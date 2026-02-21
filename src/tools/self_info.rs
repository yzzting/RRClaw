use std::path::PathBuf;

use async_trait::async_trait;
use color_eyre::eyre::Result;
use serde_json::json;

use crate::config::Config;
use crate::security::SecurityPolicy;

use super::traits::{Tool, ToolResult};

/// Agent 自我信息查询工具（纯读取，无副作用）
pub struct SelfInfoTool {
    config: Config,
    data_dir: PathBuf,
    log_dir: PathBuf,
    config_path: PathBuf,
}

impl SelfInfoTool {
    pub fn new(config: Config, data_dir: PathBuf, log_dir: PathBuf, config_path: PathBuf) -> Self {
        Self {
            config,
            data_dir,
            log_dir,
            config_path,
        }
    }

    fn query_config(&self) -> String {
        let mut lines = Vec::new();
        lines.push(format!("当前 Provider: {}", self.config.default.provider));
        lines.push(format!("当前模型: {}", self.config.default.model));
        lines.push(format!("Temperature: {}", self.config.default.temperature));
        lines.push(format!(
            "安全模式: {:?}",
            self.config.security.autonomy
        ));
        lines.push(format!(
            "命令白名单: [{}]",
            self.config.security.allowed_commands.join(", ")
        ));
        lines.push(format!(
            "工作目录限制: {}",
            if self.config.security.workspace_only {
                "是"
            } else {
                "否"
            }
        ));

        // 列出已配置的 providers（API key 脱敏）
        lines.push(String::new());
        lines.push("已配置的 Providers:".to_string());
        for (name, pc) in &self.config.providers {
            lines.push(format!(
                "  - {}: model={}, base_url={}, api_key={}",
                name,
                pc.model,
                pc.base_url,
                mask_api_key(&pc.api_key),
            ));
        }

        lines.join("\n")
    }

    fn query_paths(&self) -> String {
        let db_path = self.data_dir.join("memory.db");
        let tantivy_path = self.data_dir.join("tantivy_index");

        let mut lines = Vec::new();
        lines.push(format!(
            "配置文件: {}",
            self.config_path.display()
        ));
        lines.push(format!("数据目录: {}", self.data_dir.display()));
        lines.push(format!("SQLite 数据库: {}", db_path.display()));
        lines.push(format!(
            "tantivy 搜索索引: {}",
            tantivy_path.display()
        ));
        lines.push(format!("日志目录: {}", self.log_dir.display()));
        lines.push(format!(
            "日志文件: {}/rrclaw.log.YYYY-MM-DD",
            self.log_dir.display()
        ));
        lines.join("\n")
    }

    fn query_provider(&self) -> String {
        let provider_key = &self.config.default.provider;
        let mut lines = Vec::new();
        lines.push(format!("当前 Provider: {}", provider_key));
        lines.push(format!("当前模型: {}", self.config.default.model));

        if let Some(pc) = self.config.providers.get(provider_key) {
            lines.push(format!("Base URL: {}", pc.base_url));
            lines.push(format!(
                "认证方式: {}",
                pc.auth_style.as_deref().unwrap_or("Bearer token")
            ));
        } else {
            lines.push(format!("（Provider '{}' 未在配置中找到）", provider_key));
        }

        lines.join("\n")
    }

    fn query_stats(&self) -> String {
        // 统计信息从文件系统读取
        let db_path = self.data_dir.join("memory.db");
        let db_exists = db_path.exists();
        let db_size = if db_exists {
            std::fs::metadata(&db_path)
                .map(|m| format_bytes(m.len()))
                .unwrap_or_else(|_| "未知".to_string())
        } else {
            "数据库不存在".to_string()
        };

        let mut lines = Vec::new();
        lines.push(format!("数据库大小: {}", db_size));
        lines.push(format!(
            "已配置 Provider 数: {}",
            self.config.providers.len()
        ));
        lines.join("\n")
    }

    fn query_help(&self) -> String {
        let lines = vec![
            "可用斜杠命令:",
            "  /help   — 显示帮助信息",
            "  /new    — 开始新对话（清空当前历史）",
            "  /clear  — 清空屏幕",
            "  /switch — 切换 Provider/模型",
            "  /apikey — 设置 API Key",
            "",
            "其他操作:",
            "  exit / quit / Ctrl-D — 退出",
            "  Ctrl-C — 中断当前操作",
        ];
        lines.join("\n")
    }
}

#[async_trait]
impl Tool for SelfInfoTool {
    fn name(&self) -> &str {
        "self_info"
    }

    fn description(&self) -> &str {
        "查询 RRClaw 自身信息（配置、路径、Provider、统计、帮助）。仅在需要了解自身状态时使用，不要每轮都调用。"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "enum": ["config", "paths", "provider", "stats", "help"],
                    "description": "要查询的信息类型: config=配置总览, paths=文件路径, provider=当前Provider详情, stats=统计信息, help=可用命令"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _policy: &SecurityPolicy,
    ) -> Result<ToolResult> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("help");

        let output = match query {
            "config" => self.query_config(),
            "paths" => self.query_paths(),
            "provider" => self.query_provider(),
            "stats" => self.query_stats(),
            "help" => self.query_help(),
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "未知查询类型: '{}'. 可选: config, paths, provider, stats, help",
                        query
                    )),
                    ..Default::default()
                });
            }
        };

        Ok(ToolResult {
            success: true,
            output,
            error: None,
            ..Default::default()
        })
    }
}

/// API Key 脱敏：显示前 4 位 + ****
fn mask_api_key(key: &str) -> String {
    if key.len() <= 4 {
        "****".to_string()
    } else {
        format!("{}****", &key[..4])
    }
}

/// 格式化字节数
fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use crate::config::{DefaultConfig, MemoryConfig, ProviderConfig, RoutinesConfig, SecurityConfig};

    fn test_config() -> Config {
        let mut providers = HashMap::new();
        providers.insert(
            "deepseek".to_string(),
            ProviderConfig {
                base_url: "https://api.deepseek.com/v1".to_string(),
                api_key: "sk-secret-key-12345".to_string(),
                model: "deepseek-chat".to_string(),
                auth_style: None,
            },
        );
        Config {
            default: DefaultConfig {
                provider: "deepseek".to_string(),
                model: "deepseek-chat".to_string(),
                temperature: 0.7,
            },
            providers,
            memory: MemoryConfig::default(),
            security: SecurityConfig::default(),
            telegram: None,
            reliability: crate::config::ReliabilityConfig::default(),
            mcp: None,
            routines: RoutinesConfig::default(),
        }
    }

    fn test_tool() -> SelfInfoTool {
        let tmp = std::env::temp_dir().join("rrclaw-test-selfinfo");
        SelfInfoTool::new(
            test_config(),
            tmp.join("data"),
            tmp.join("logs"),
            tmp.join("config.toml"),
        )
    }

    #[test]
    fn mask_api_key_short() {
        assert_eq!(mask_api_key("abc"), "****");
    }

    #[test]
    fn mask_api_key_long() {
        assert_eq!(mask_api_key("sk-secret-key-12345"), "sk-s****");
    }

    #[test]
    fn format_bytes_various_sizes() {
        assert_eq!(format_bytes(500), "500 B");
        assert_eq!(format_bytes(2048), "2.0 KB");
        assert_eq!(format_bytes(1_500_000), "1.4 MB");
    }

    #[tokio::test]
    async fn query_config_shows_provider_and_masks_key() {
        let tool = test_tool();
        let policy = SecurityPolicy::default();
        let result = tool
            .execute(json!({"query": "config"}), &policy)
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("deepseek"));
        assert!(result.output.contains("sk-s****"));
        assert!(!result.output.contains("sk-secret-key-12345"));
    }

    #[tokio::test]
    async fn query_paths_shows_db_and_log() {
        let tool = test_tool();
        let policy = SecurityPolicy::default();
        let result = tool
            .execute(json!({"query": "paths"}), &policy)
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("memory.db"));
        assert!(result.output.contains("tantivy_index"));
        assert!(result.output.contains("rrclaw.log"));
    }

    #[tokio::test]
    async fn query_provider_shows_current() {
        let tool = test_tool();
        let policy = SecurityPolicy::default();
        let result = tool
            .execute(json!({"query": "provider"}), &policy)
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("deepseek"));
        assert!(result.output.contains("Bearer token"));
    }

    #[tokio::test]
    async fn query_help_lists_commands() {
        let tool = test_tool();
        let policy = SecurityPolicy::default();
        let result = tool
            .execute(json!({"query": "help"}), &policy)
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("/help"));
        assert!(result.output.contains("/new"));
        assert!(result.output.contains("/switch"));
    }

    #[tokio::test]
    async fn unknown_query_returns_error() {
        let tool = test_tool();
        let policy = SecurityPolicy::default();
        let result = tool
            .execute(json!({"query": "unknown"}), &policy)
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("未知查询类型"));
    }

    #[tokio::test]
    async fn missing_query_defaults_to_help() {
        let tool = test_tool();
        let policy = SecurityPolicy::default();
        let result = tool.execute(json!({}), &policy).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("/help"));
    }
}
