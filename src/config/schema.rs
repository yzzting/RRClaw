use std::collections::HashMap;
use std::path::PathBuf;

use color_eyre::eyre::{Context, Result};
use figment::providers::{Env, Format, Serialized, Toml};
use figment::Figment;
use serde::{Deserialize, Serialize};

use crate::security::AutonomyLevel;

/// 全局配置
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    pub default: DefaultConfig,
    pub providers: HashMap<String, ProviderConfig>,
    pub memory: MemoryConfig,
    pub security: SecurityConfig,
    #[serde(default)]
    pub telegram: Option<TelegramConfig>,
    #[serde(default)]
    pub reliability: ReliabilityConfig,
    #[serde(default)]
    pub mcp: Option<McpConfig>,
    #[serde(default)]
    pub routines: RoutinesConfig,
}

/// Telegram Bot 配置
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TelegramConfig {
    /// Bot Token（从 @BotFather 获取）
    #[serde(default)]
    pub bot_token: Option<String>,
    /// 允许的 chat ID 列表（空 = 允许所有）
    #[serde(default)]
    pub allowed_chat_ids: Vec<i64>,
}

/// 默认 Provider 设置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefaultConfig {
    pub provider: String,
    pub model: String,
    pub temperature: f64,
    /// Interface language: "en" (default) or "zh"
    /// Controls system prompt language, CLI messages, and builtin skill language.
    /// Does NOT affect LLM reply language (always follows the user's message language).
    #[serde(default = "default_language")]
    pub language: String,
}

fn default_language() -> String {
    "en".to_string()
}

/// 单个 Provider 的连接配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// Claude 使用 "x-api-key"，其他 Provider 为 None（默认 Bearer）
    pub auth_style: Option<String>,
}

/// 记忆系统配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    pub backend: String,
    pub auto_save: bool,
}

/// 安全策略配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    pub autonomy: AutonomyLevel,
    pub allowed_commands: Vec<String>,
    pub workspace_only: bool,
    /// HTTP 请求白名单，允许访问的 host/IP
    #[serde(default)]
    pub http_allowed_hosts: Vec<String>,
    /// 是否启用 Prompt Injection 检测，默认 true
    /// 设为 false 时完全跳过检测（适合完全信任所有工具输出的内部环境）
    #[serde(default = "default_injection_check")]
    pub injection_check: bool,
    /// HTML 响应 strip 后的最大字节数（KB），超出则触发 mini-LLM 提取或截断
    /// 默认 200（KB）；设为 0 禁用 strip（直接走原始 1MB 截断，旧行为）
    #[serde(default = "default_http_strip_threshold_kb")]
    pub http_strip_threshold_kb: usize,
}

fn default_injection_check() -> bool {
    true
}

fn default_http_strip_threshold_kb() -> usize {
    200
}

/// 可靠性配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReliabilityConfig {
    /// 最大重试次数，默认 3
    #[serde(default = "default_max_retries")]
    pub max_retries: usize,
    /// 初始退避毫秒，默认 500
    #[serde(default = "default_initial_backoff_ms")]
    pub initial_backoff_ms: u64,
    /// Fallback provider 名称列表（按顺序）
    #[serde(default)]
    pub fallback_providers: Vec<String>,
}

fn default_max_retries() -> usize {
    3
}

fn default_initial_backoff_ms() -> u64 {
    500
}

impl Default for ReliabilityConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_backoff_ms: 500,
            fallback_providers: vec![],
        }
    }
}

/// MCP 全局配置
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpConfig {
    /// key = server 名称（用于 tool 前缀）
    #[serde(default)]
    pub servers: HashMap<String, McpServerConfig>,
}

/// 定时任务配置
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoutinesConfig {
    /// 静态任务列表（从 config.toml 读取）
    #[serde(default)]
    pub jobs: Vec<RoutineJobConfig>,
}

/// 单个静态 Routine 的配置项（映射到 Routine struct）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutineJobConfig {
    pub name: String,
    pub schedule: String,
    pub message: String,
    #[serde(default = "default_routine_channel")]
    pub channel: String,
    #[serde(default = "default_routine_enabled")]
    pub enabled: bool,
}

fn default_routine_channel() -> String {
    "cli".to_string()
}

fn default_routine_enabled() -> bool {
    true
}

/// 单个 MCP Server 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    #[serde(flatten)]
    pub transport: McpTransport,
    /// 只暴露部分 tools（空 = 全部）
    #[serde(default)]
    pub allowed_tools: Vec<String>,
}

/// MCP 传输方式
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "lowercase")]
pub enum McpTransport {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
    },
    Sse {
        url: String,
        #[serde(default)]
        headers: HashMap<String, String>,
    },
}

impl Default for DefaultConfig {
    fn default() -> Self {
        Self {
            provider: "deepseek".to_string(),
            model: "deepseek-chat".to_string(),
            temperature: 0.7,
            language: default_language(),
        }
    }
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            backend: "sqlite".to_string(),
            auto_save: true,
        }
    }
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            autonomy: AutonomyLevel::Supervised,
            allowed_commands: vec![
                "ls", "cat", "grep", "find", "echo", "pwd", "git", "head", "tail", "wc", "cargo",
                "rustc",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            workspace_only: true,
            http_allowed_hosts: vec![],
            injection_check: true,
            http_strip_threshold_kb: 200,
        }
    }
}

/// 默认配置 TOML 模板
const DEFAULT_CONFIG_TOML: &str = r#"[default]
provider = "deepseek"
model = "deepseek-chat"
temperature = 0.7
language = "en"     # Interface language: "en" or "zh"

# 在下方添加你的 Provider 配置
# [providers.deepseek]
# base_url = "https://api.deepseek.com/v1"
# api_key = "your-key"
# model = "deepseek-chat"

# [providers.claude]
# base_url = "https://api.anthropic.com"
# api_key = "your-key"
# model = "claude-sonnet-4-5-20250929"
# auth_style = "x-api-key"

[memory]
backend = "sqlite"
auto_save = true

[security]
autonomy = "supervised"
allowed_commands = ["ls", "cat", "grep", "find", "echo", "pwd", "git", "head", "tail", "wc", "cargo", "rustc"]
workspace_only = true

# 可靠性配置（可选）
# [reliability]
# max_retries = 3
# initial_backoff_ms = 500
# fallback_providers = ["glm", "minimax"]  # 主 Provider 失败时按顺序切换
"#;

impl Config {
    /// 返回配置文件路径: `~/.rrclaw/config.toml`
    pub fn config_path() -> Result<PathBuf> {
        let base_dirs = directories::BaseDirs::new()
            .ok_or_else(|| color_eyre::eyre::eyre!("无法获取 home 目录"))?;
        Ok(base_dirs.home_dir().join(".rrclaw").join("config.toml"))
    }

    /// 从配置文件读取 http_allowed_hosts（实时读取，无需重启）
    /// 从配置文件读取 http_allowed_hosts（实时读取，无需重启）
    pub fn get_http_allowed_hosts() -> Vec<String> {
        #[cfg(test)]
        {
            vec![]
        }
        #[cfg(not(test))]
        {
            let config_path = match Self::config_path() {
                Ok(p) => p,
                Err(_) => return vec![],
            };
            let content = match std::fs::read_to_string(&config_path) {
                Ok(c) => c,
                Err(_) => return vec![],
            };
            let doc = match content.parse::<toml_edit::DocumentMut>() {
                Ok(d) => d,
                Err(_) => return vec![],
            };

            // 从 [security] 段读取 http_allowed_hosts
            if let Some(security) = doc.get("security") {
                if let Some(http_hosts) = security.get("http_allowed_hosts") {
                    if let Some(arr) = http_hosts.as_array() {
                        return arr
                            .iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect();
                    }
                }
            }
            vec![]
        }
    }

    /// 实时读取 config.toml 中的 language 字段（无需重启即可热生效）
    /// 失败时回退到 LANG 环境变量推断
    pub fn get_language() -> crate::i18n::Language {
        #[cfg(test)]
        {
            crate::i18n::Language::English
        }
        #[cfg(not(test))]
        {
            let config_path = match Self::config_path() {
                Ok(p) => p,
                Err(_) => return crate::i18n::Language::from_locale(),
            };
            let content = match std::fs::read_to_string(&config_path) {
                Ok(c) => c,
                Err(_) => return crate::i18n::Language::from_locale(),
            };
            let doc = match content.parse::<toml_edit::DocumentMut>() {
                Ok(d) => d,
                Err(_) => return crate::i18n::Language::from_locale(),
            };
            let lang_str = doc
                .get("default")
                .and_then(|d| d.get("language"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            crate::i18n::Language::detect(lang_str)
        }
    }

    /// 加载配置，如果配置文件不存在则创建默认配置
    pub fn load_or_init() -> Result<Self> {
        let config_path = Self::config_path()?;

        if !config_path.exists() {
            if let Some(parent) = config_path.parent() {
                std::fs::create_dir_all(parent).wrap_err("创建配置目录失败")?;
            }
            std::fs::write(&config_path, DEFAULT_CONFIG_TOML).wrap_err("写入默认配置失败")?;
        }

        Self::load_from_path(&config_path)
    }

    /// 从指定路径加载配置（figment 多层合并）
    pub fn load_from_path(path: &std::path::Path) -> Result<Self> {
        let config: Config = Figment::new()
            .merge(Serialized::defaults(Config::default()))
            .merge(Toml::file(path))
            .merge(Env::prefixed("RRCLAW_").split("_"))
            .extract()
            .wrap_err("解析配置文件失败")?;

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_sensible_values() {
        let config = Config::default();
        assert_eq!(config.default.provider, "deepseek");
        assert_eq!(config.default.model, "deepseek-chat");
        assert!((config.default.temperature - 0.7).abs() < f64::EPSILON);
        assert_eq!(config.memory.backend, "sqlite");
        assert!(config.memory.auto_save);
        assert_eq!(config.security.autonomy, AutonomyLevel::Supervised);
        assert!(config.security.workspace_only);
        assert!(config.security.allowed_commands.contains(&"ls".to_string()));
        assert!(config
            .security
            .allowed_commands
            .contains(&"cargo".to_string()));
    }

    #[test]
    fn load_from_toml_file() {
        let tmp = tempfile::tempdir().unwrap();
        let toml_path = tmp.path().join("config.toml");
        std::fs::write(
            &toml_path,
            r#"
[default]
provider = "glm"
model = "glm-4-flash"
temperature = 0.5

[providers.glm]
base_url = "https://open.bigmodel.cn/api/paas/v4"
api_key = "test-key"
model = "glm-4-flash"

[memory]
backend = "sqlite"
auto_save = false

[security]
autonomy = "full"
allowed_commands = ["ls", "git"]
workspace_only = false
"#,
        )
        .unwrap();

        let config = Config::load_from_path(&toml_path).unwrap();
        assert_eq!(config.default.provider, "glm");
        assert_eq!(config.default.model, "glm-4-flash");
        assert!((config.default.temperature - 0.5).abs() < f64::EPSILON);
        assert!(!config.memory.auto_save);
        assert_eq!(config.security.autonomy, AutonomyLevel::Full);
        assert!(!config.security.workspace_only);
        assert_eq!(config.security.allowed_commands.len(), 2);

        let glm = config.providers.get("glm").unwrap();
        assert_eq!(glm.api_key, "test-key");
        assert_eq!(glm.model, "glm-4-flash");
        assert!(glm.auth_style.is_none());
    }

    #[test]
    fn provider_with_auth_style() {
        let tmp = tempfile::tempdir().unwrap();
        let toml_path = tmp.path().join("config.toml");
        std::fs::write(
            &toml_path,
            r#"
[providers.claude]
base_url = "https://api.anthropic.com"
api_key = "sk-ant-test"
model = "claude-sonnet-4-5-20250929"
auth_style = "x-api-key"
"#,
        )
        .unwrap();

        let config = Config::load_from_path(&toml_path).unwrap();
        let claude = config.providers.get("claude").unwrap();
        assert_eq!(claude.auth_style.as_deref(), Some("x-api-key"));
    }

    #[test]
    fn missing_fields_use_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let toml_path = tmp.path().join("config.toml");
        // 只写一个 section，其他应该用默认值
        std::fs::write(
            &toml_path,
            r#"
[default]
provider = "minimax"
"#,
        )
        .unwrap();

        let config = Config::load_from_path(&toml_path).unwrap();
        assert_eq!(config.default.provider, "minimax");
        // 其他字段保持默认
        assert_eq!(config.memory.backend, "sqlite");
        assert!(config.security.workspace_only);
    }

    #[test]
    fn load_or_init_creates_default_file() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join(".rrclaw").join("config.toml");
        assert!(!config_path.exists());

        // 直接测试文件创建逻辑（不走 load_or_init 因为它用固定 home 路径）
        let parent = config_path.parent().unwrap();
        std::fs::create_dir_all(parent).unwrap();
        std::fs::write(&config_path, DEFAULT_CONFIG_TOML).unwrap();

        assert!(config_path.exists());
        let config = Config::load_from_path(&config_path).unwrap();
        assert_eq!(config.default.provider, "deepseek");
    }

    #[test]
    fn config_path_ends_with_rrclaw() {
        let path = Config::config_path().unwrap();
        assert!(path.ends_with(".rrclaw/config.toml"));
    }

    #[test]
    fn mcp_stdio_config_parses() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[mcp.servers.filesystem]
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
"#,
        )
        .unwrap();
        let config = Config::load_from_path(&path).unwrap();
        let mcp = config.mcp.unwrap();
        let fs_server = mcp.servers.get("filesystem").unwrap();
        match &fs_server.transport {
            McpTransport::Stdio { command, args, .. } => {
                assert_eq!(command, "npx");
                assert_eq!(args[0], "-y");
                assert_eq!(args.len(), 3);
            }
            _ => panic!("应该是 stdio 传输"),
        }
        assert!(fs_server.allowed_tools.is_empty());
    }

    #[test]
    fn mcp_sse_config_parses() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[mcp.servers.remote]
transport = "sse"
url = "https://mcp.example.com/mcp"
[mcp.servers.remote.headers]
Authorization = "Bearer token"
"#,
        )
        .unwrap();
        let config = Config::load_from_path(&path).unwrap();
        let mcp = config.mcp.unwrap();
        let remote = mcp.servers.get("remote").unwrap();
        match &remote.transport {
            McpTransport::Sse { url, headers } => {
                assert_eq!(url, "https://mcp.example.com/mcp");
                assert_eq!(headers.get("Authorization").unwrap(), "Bearer token");
            }
            _ => panic!("应该是 sse 传输"),
        }
    }

    #[test]
    fn mcp_allowed_tools_filter() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[mcp.servers.fs]
transport = "stdio"
command = "npx"
args = []
allowed_tools = ["read_file", "list_dir"]
"#,
        )
        .unwrap();
        let config = Config::load_from_path(&path).unwrap();
        let mcp = config.mcp.unwrap();
        let server = mcp.servers.get("fs").unwrap();
        assert_eq!(server.allowed_tools, vec!["read_file", "list_dir"]);
    }

    #[test]
    fn no_mcp_config_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[default]
provider = "deepseek"
"#,
        )
        .unwrap();
        let config = Config::load_from_path(&path).unwrap();
        assert!(config.mcp.is_none());
    }

    #[test]
    fn mcp_stdio_with_env() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[mcp.servers.github]
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
[mcp.servers.github.env]
GITHUB_TOKEN = "ghp_xxx"
"#,
        )
        .unwrap();
        let config = Config::load_from_path(&path).unwrap();
        let mcp = config.mcp.unwrap();
        let github = mcp.servers.get("github").unwrap();
        match &github.transport {
            McpTransport::Stdio { env, .. } => {
                assert_eq!(env.get("GITHUB_TOKEN").unwrap(), "ghp_xxx");
            }
            _ => panic!("应该是 stdio 传输"),
        }
    }
}
