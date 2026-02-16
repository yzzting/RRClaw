# Config 模块

## 职责

加载和管理 RRClaw 全局配置，支持多层合并（默认值 → TOML 文件 → 环境变量）。

## 配置文件

路径: `~/.rrclaw/config.toml`

```toml
[default]
provider = "deepseek"
model = "deepseek-chat"
temperature = 0.7

[providers.glm]
base_url = "https://open.bigmodel.cn/api/paas/v4"
api_key = "your-key"
model = "glm-4-flash"

[providers.minimax]
base_url = "https://api.minimax.chat/v1"
api_key = "your-key"
model = "MiniMax-Text-01"

[providers.deepseek]
base_url = "https://api.deepseek.com/v1"
api_key = "your-key"
model = "deepseek-chat"

[providers.claude]
base_url = "https://api.anthropic.com"
api_key = "your-key"
model = "claude-sonnet-4-5-20250929"
auth_style = "x-api-key"

[providers.gpt]
base_url = "https://api.openai.com/v1"
api_key = "your-key"
model = "gpt-4o"

[memory]
backend = "sqlite"
auto_save = true

[security]
autonomy = "supervised"
allowed_commands = ["ls", "cat", "grep", "find", "echo", "pwd", "git"]
workspace_only = true
```

## 结构体设计

```rust
Config {
    default: DefaultConfig,
    providers: HashMap<String, ProviderConfig>,
    memory: MemoryConfig,
    security: SecurityConfig,
}

DefaultConfig { provider: String, model: String, temperature: f64 }
ProviderConfig { base_url: String, api_key: String, model: String, auth_style: Option<String> }
MemoryConfig { backend: String, auto_save: bool }
SecurityConfig { autonomy: AutonomyLevel, allowed_commands: Vec<String>, workspace_only: bool }
```

- `SecurityConfig.autonomy` 复用 `crate::security::AutonomyLevel`
- 所有结构体实现 `Default`

## 加载逻辑 — `Config::load_or_init()`

1. 通过 `directories::BaseDirs` 获取 home 目录，拼接 `.rrclaw/config.toml`
2. 如果文件不存在:
   - 创建 `~/.rrclaw/` 目录
   - 写入默认配置 TOML
   - 返回默认 `Config`
3. 如果文件存在:
   - figment 合并: `Serialized::defaults(Config::default())` → `Toml::file(path)` → `Env::prefixed("RRCLAW_").split("_")`
   - 返回解析后的 `Config`

## 环境变量覆盖

前缀 `RRCLAW_`，下划线分隔嵌套:
- `RRCLAW_DEFAULT_PROVIDER=glm` → `config.default.provider`
- `RRCLAW_MEMORY_AUTO_SAVE=false` → `config.memory.auto_save`

## 文件结构

- `mod.rs` — 模块声明 + re-exports
- `schema.rs` — 结构体定义 + Default 实现 + `Config::load_or_init()` + 测试
