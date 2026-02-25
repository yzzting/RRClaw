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
        lines.push(format!(
            "Current Provider: {}",
            self.config.default.provider
        ));
        lines.push(format!("Current Model: {}", self.config.default.model));
        lines.push(format!("Temperature: {}", self.config.default.temperature));
        lines.push(format!(
            "Security Mode: {:?}",
            self.config.security.autonomy
        ));
        lines.push(format!(
            "Command Allowlist: [{}]",
            self.config.security.allowed_commands.join(", ")
        ));
        lines.push(format!(
            "Workspace-only: {}",
            if self.config.security.workspace_only {
                "yes"
            } else {
                "no"
            }
        ));

        lines.push(String::new());
        lines.push("Configured Providers:".to_string());
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
        lines.push(format!("Config File: {}", self.config_path.display()));
        lines.push(format!("Data Directory: {}", self.data_dir.display()));
        lines.push(format!("SQLite Database: {}", db_path.display()));
        lines.push(format!("tantivy Search Index: {}", tantivy_path.display()));
        lines.push(format!("Log Directory: {}", self.log_dir.display()));
        lines.push(format!(
            "Log File: {}/rrclaw.log.YYYY-MM-DD",
            self.log_dir.display()
        ));
        lines.join("\n")
    }

    fn query_provider(&self) -> String {
        let provider_key = &self.config.default.provider;
        let mut lines = Vec::new();
        lines.push(format!("Current Provider: {}", provider_key));
        lines.push(format!("Current Model: {}", self.config.default.model));

        if let Some(pc) = self.config.providers.get(provider_key) {
            lines.push(format!("Base URL: {}", pc.base_url));
            lines.push(format!(
                "Auth Style: {}",
                pc.auth_style.as_deref().unwrap_or("Bearer token")
            ));
        } else {
            lines.push(format!("(Provider '{}' not found in config)", provider_key));
        }

        lines.join("\n")
    }

    fn query_stats(&self) -> String {
        let db_path = self.data_dir.join("memory.db");
        let db_exists = db_path.exists();
        let db_size = if db_exists {
            std::fs::metadata(&db_path)
                .map(|m| format_bytes(m.len()))
                .unwrap_or_else(|_| "unknown".to_string())
        } else {
            "Database not found".to_string()
        };

        let mut lines = Vec::new();
        lines.push(format!("Database Size: {}", db_size));
        lines.push(format!(
            "Configured Providers: {}",
            self.config.providers.len()
        ));
        lines.join("\n")
    }

    fn query_help(&self) -> String {
        let lines = vec![
            "Available slash commands:",
            "  /help   — Show help information",
            "  /new    — Start a new conversation (clears current history)",
            "  /clear  — Clear the screen",
            "  /switch — Switch provider/model",
            "  /apikey — Set API key",
            "",
            "Other operations:",
            "  exit / quit / Ctrl-D — Exit",
            "  Ctrl-C — Interrupt current operation",
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
        "Query RRClaw's own status (config, paths, provider, stats, help). Use only when you need to know the current state; do not call every turn."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "enum": ["config", "paths", "provider", "stats", "help"],
                    "description": "Information type: config=configuration overview, paths=file paths, provider=current provider details, stats=statistics, help=available commands"
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
        let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("help");

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
                        "Unknown query type: '{}'. Options: config, paths, provider, stats, help",
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

    use crate::config::{
        DefaultConfig, MemoryConfig, ProviderConfig, RoutinesConfig, SecurityConfig,
    };

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
                language: "en".to_string(),
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
        assert!(result.error.unwrap().contains("Unknown query type"));
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
