# Config 模块设计文档

加载和管理 RRClaw 全局配置，支持多层合并（默认值 → TOML 文件 → 环境变量）。

## 配置文件路径

`~/.rrclaw/config.toml`

## 结构体设计

```rust
Config {
    default:   DefaultConfig,
    providers: HashMap<String, ProviderConfig>,
    memory:    MemoryConfig,
    security:  SecurityConfig,
    telegram:  Option<TelegramConfig>,  // P1
    mcp:       Option<McpConfig>,       // P4
    routines:  RoutinesConfig,          // P5
}

DefaultConfig  { provider: String, model: String, temperature: f64 }
ProviderConfig { base_url: String, api_key: String, model: String, auth_style: Option<String> }
MemoryConfig   { backend: String, auto_save: bool }

SecurityConfig {
    autonomy: AutonomyLevel,
    allowed_commands: Vec<String>,
    workspace_only: bool,
    http_allowed_hosts: Vec<String>,  // P4：HttpRequestTool SSRF 白名单
    injection_check: bool,            // P4：启用 prompt injection 检测（默认 true）
}

TelegramConfig { bot_token: String, allowed_chat_ids: Vec<i64> }

McpConfig { servers: HashMap<String, McpServerConfig> }
McpServerConfig {
    transport: McpTransport,          // Stdio | Sse
    allowed_tools: Vec<String>,       // 空 = 允许全部
}
McpTransport::Stdio { command, args, env }
McpTransport::Sse   { url, headers }

RoutinesConfig { jobs: Vec<Routine> }  // config.toml 静态配置的任务
                                        // 动态任务（/routine add）存 SQLite
```

## 加载逻辑 — `Config::load_or_init()`

1. 通过 `directories::BaseDirs` 获取 home，拼接 `.rrclaw/config.toml`
2. 文件不存在 → 创建 `~/.rrclaw/` 目录，写入默认配置，返回默认 `Config`
3. 文件存在 → figment 合并：
   `Serialized::defaults(Config::default())` → `Toml::file(path)` → `Env::prefixed("RRCLAW_").split("_")`

## 环境变量覆盖

前缀 `RRCLAW_`，下划线分隔嵌套：
- `RRCLAW_DEFAULT_PROVIDER=glm`
- `RRCLAW_MEMORY_AUTO_SAVE=false`

## http_allowed_hosts 实时读取

`Config::get_http_allowed_hosts()` — 每次调用直接读 config.toml，不走内存缓存。

**原因**：HttpRequestTool 调用时 SecurityPolicy 已经是拷贝，写入 config 对已有拷贝不可见。实时读文件确保用户同意某个 host 后立即生效，无需重启。

## 配置文件示例

```toml
[default]
provider = "deepseek"
model = "deepseek-chat"
temperature = 0.7

[providers.deepseek]
base_url = "https://api.deepseek.com/v1"
api_key = "your-key"
model = "deepseek-chat"

[security]
autonomy = "supervised"
allowed_commands = ["ls", "cat", "grep", "find", "echo", "pwd", "git", "cargo"]
workspace_only = true
injection_check = true
# http_allowed_hosts = ["my-internal-api.company.com"]

[telegram]
bot_token = "your-bot-token"
allowed_chat_ids = [123456789]

[mcp.servers.filesystem]
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]

[[routines.jobs]]
name = "morning_brief"
schedule = "0 8 * * *"
message = "生成今日工作计划"
channel = "cli"
enabled = true
```

## 文件结构

```
src/config/
├── Claude.md   # 本文件
├── mod.rs      # 模块声明 + re-exports + PROVIDERS 常量
└── schema.rs   # 所有结构体 + Default 实现 + load_or_init() + get_http_allowed_hosts()
```
