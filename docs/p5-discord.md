# P5-6: Discord Channel 实现计划

## 背景

Discord 是全球最活跃的开发者社区平台，使用人数远超 Telegram 在技术圈的渗透。ZeroClaw 和 OpenClaw 均将 Discord 作为核心 Channel 支持。

RRClaw 已有 `TelegramChannel`（`src/channels/telegram.rs`），Discord Channel 与其结构完全对称：Bot 账号 + 消息事件监听 + 多用户隔离会话。实现成本中等，代码量约 200 行。

**与 Telegram 的差异**：

| 特性 | Telegram | Discord |
|------|---------|---------|
| 账号体系 | Chat ID（数字）| Guild（服务器）+ Channel + User ID |
| 访问控制 | `allowed_chat_ids` | `allowed_guild_ids` + `allowed_user_ids` |
| 消息触发 | 直接发消息给 Bot | 消息前缀触发（`!` 前缀）或 @Bot |
| 斜杠命令 | 无原生支持 | Discord Application Commands（`/` 前缀，原生支持） |
| Crate | `teloxide` | `serenity`（推荐）或 `twilight` |

**当前实现范围（P5 版本）**：
- 监听 Guild 消息，支持 `!` 前缀触发（如 `!帮我写一个 Rust 函数`）
- 每个 Discord 用户独立会话（per-user Agent 实例）
- `allowed_guild_ids` + `allowed_user_ids` 双重访问控制
- 流式输出（Discord 消息长度 2000 字符限制，超出自动分割）
- Guild 斜杠命令注册（`/new`、`/clear`）— 可选

---

## 一、架构设计

```
Discord API (WebSocket Gateway)
            │ 消息事件
            ▼
DiscordChannel::start()
  ├── serenity EventHandler::message() 回调
  │       │
  │       ├── 过滤检查：
  │       │       ├── 非 Bot 消息
  │       │       ├── allowed_guild_ids（空 = 允许所有）
  │       │       ├── allowed_user_ids（空 = 允许所有）
  │       │       └── 消息前缀（"!" 前缀 或 @Bot mention）
  │       │
  │       ├── 去前缀，得到用户消息正文
  │       │
  │       └── 路由到对应用户的 Agent（HashMap<UserId, Agent>）
  │               │
  │               └── agent.process_message(content)
  │                       │
  │                       ▼
  │               Discord 消息发送
  │               （超过 2000 字符自动分割）
  │
  └── serenity EventHandler::ready() 回调
          └── 注册 Guild Application Commands（可选）
```

### 关键设计决策

1. **per-user Agent**：每个 Discord 用户 ID 对应一个独立的 `Agent` 实例，会话历史互相隔离。通过 `Arc<Mutex<HashMap<UserId, Agent>>>` 在异步回调间安全共享。

2. **消息触发方式**：Discord 有两种触发方式：
   - `!<消息>` 前缀（Prefix command），实现简单，推荐默认
   - `@Bot mention`（@机器人），更自然，但解析略复杂
   P5 版本默认支持 `!` 前缀，`command_prefix` 可配置。

3. **2000 字符限制**：Discord 单条消息最多 2000 字符。超出时分割为多条消息连续发送，带 `(1/n)` 标注。

4. **serenity vs twilight**：选用 `serenity`，原因：
   - Discord 官方合作库，文档完善
   - tokio 异步优先（v0.12+）
   - 社区更活跃，更多示例参考

---

## 二、新增依赖

在 `Cargo.toml` 中新增：

```toml
[dependencies]
serenity = { version = "0.12", default-features = false, features = [
    "client",       # Client 和 EventHandler
    "gateway",      # WebSocket Gateway
    "model",        # Discord 数据模型（Message、Guild、User 等）
    "http",         # HTTP API（发消息、注册命令）
    "tokio",        # tokio 运行时集成
    "builder",      # MessageBuilder 等辅助构建器
] }
```

> **版本说明**：serenity 0.12.x 是当前 stable 主分支，完全兼容 tokio 1.x。`default-features = false` 避免引入不需要的 voice、cache 等功能（减少编译时间和二进制体积）。

---

## 三、新增文件

```
src/channels/discord.rs     ← 新增：DiscordChannel 实现
```

`src/channels/mod.rs` 和 `src/config/schema.rs` 微改（见第六、七章）。

---

## 四、完整实现代码

### 4.1 src/channels/discord.rs

```rust
//! Discord Channel 实现
//!
//! 通过 Discord Bot 接收和回复消息，每个 Discord 用户拥有独立的 Agent 会话。
//!
//! # 配置
//! ```toml
//! [channels.discord]
//! enabled = true
//! bot_token = "MTxxxxxx.Gyyyyy.zzzzz"
//! allowed_guild_ids = [123456789012345678]   # 空 = 允许所有服务器
//! allowed_user_ids = []                       # 空 = 允许所有用户
//! command_prefix = "!"                        # 消息触发前缀
//! ```
//!
//! # 使用方法
//! 在配置的 Discord 服务器中发送：`!帮我写一个 Rust 函数`
//! Bot 将回复 Agent 的响应。

use std::collections::HashMap;
use std::sync::Arc;

use color_eyre::eyre::{eyre, Result};
use serenity::async_trait;
use serenity::model::channel::Message;
use serenity::model::gateway::Ready;
use serenity::model::id::{GuildId, UserId};
use serenity::prelude::*;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::agent::Agent;
use crate::config::Config;
use crate::memory::{Memory, SqliteMemory};
use crate::providers::{create_provider, ReliableProvider, RetryConfig};
use crate::security::SecurityPolicy;
use crate::tools::create_tools;

/// Discord 消息长度上限（Discord API 硬限制）
const DISCORD_MAX_MSG_LEN: usize = 2000;
/// 超长消息分割后的段编号前缀预留长度（如 "(1/3) " 占 6 字符）
const DISCORD_PAGE_PREFIX_LEN: usize = 10;

// ─── AgentFactory ─────────────────────────────────────────────────────────────

/// 为每个 Discord 用户创建独立 Agent 的工厂
struct AgentFactory {
    config: Config,
    memory: Arc<SqliteMemory>,
}

impl AgentFactory {
    fn new(config: Config, memory: Arc<SqliteMemory>) -> Self {
        Self { config, memory }
    }

    fn create_agent(&self) -> Result<Agent> {
        let provider_key = &self.config.default.provider;
        let provider_config = self
            .config
            .providers
            .get(provider_key)
            .ok_or_else(|| eyre!("Provider '{}' 未配置", provider_key))?;

        let raw_provider = create_provider(provider_config);
        let retry_config = RetryConfig {
            max_retries: self.config.reliability.max_retries,
            initial_backoff_ms: self.config.reliability.initial_backoff_ms,
            ..Default::default()
        };
        let provider: Box<dyn crate::providers::Provider> =
            Box::new(ReliableProvider::new(raw_provider, retry_config));

        let base_dirs = directories::BaseDirs::new()
            .ok_or_else(|| eyre!("无法获取 home 目录"))?;
        let rrclaw_dir = base_dirs.home_dir().join(".rrclaw");
        let data_dir = rrclaw_dir.join("data");
        let log_dir = rrclaw_dir.join("logs");
        let config_path = crate::config::Config::config_path()?;

        let tools = create_tools(
            self.config.clone(),
            data_dir.clone(),
            log_dir,
            config_path,
            vec![], // Discord channel 暂不加载 skills
            self.memory.clone() as Arc<dyn Memory>,
        );

        let policy = SecurityPolicy {
            autonomy: self.config.security.autonomy.clone(),
            allowed_commands: self.config.security.allowed_commands.clone(),
            workspace_dir: std::env::current_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from(".")),
            blocked_paths: SecurityPolicy::default().blocked_paths,
            http_allowed_hosts: self.config.security.http_allowed_hosts.clone(),
            injection_check: self.config.security.injection_check,
        };

        let provider_name = provider_key.clone();
        let base_url = provider_config.base_url.clone();
        let model = self.config.default.model.clone();
        let temperature = self.config.default.temperature;

        // 加载身份文件
        let identity_context = crate::agent::identity::load_identity_context(
            &policy.workspace_dir,
            &data_dir,
        );

        Ok(Agent::new(
            provider,
            tools,
            Box::new(crate::memory::SqliteMemory::open(&data_dir.join("memory.db"))?),
            policy,
            provider_name,
            base_url,
            model,
            temperature,
            vec![],          // skills
            identity_context,
        ))
    }
}

// ─── EventHandler ─────────────────────────────────────────────────────────────

/// serenity EventHandler 实现
///
/// 持有 per-user Agent 会话 Map 和 Discord 配置。
struct DiscordHandler {
    /// per-user Agent，key 是 Discord User ID
    agents: Arc<Mutex<HashMap<UserId, Agent>>>,
    /// Agent 工厂（用于按需创建新 Agent）
    factory: Arc<AgentFactory>,
    /// 仅响应这些 Guild（服务器），空 = 响应所有
    allowed_guild_ids: Vec<GuildId>,
    /// 仅响应这些用户，空 = 响应所有
    allowed_user_ids: Vec<UserId>,
    /// 消息触发前缀（默认 "!"）
    command_prefix: String,
}

#[async_trait]
impl EventHandler for DiscordHandler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        info!("Discord Bot 已连接: {} (id={})", ready.user.name, ready.user.id);

        // 可选：注册 Guild Application Commands（Discord 原生 / 命令）
        // 此处暂不注册，留给后续版本
    }

    async fn message(&self, ctx: Context, msg: Message) {
        // ─── 过滤非触发消息 ────────────────────────────────────────────────

        // 1. 忽略 Bot 自身的消息（防止自发自答死循环）
        if msg.author.bot {
            return;
        }

        // 2. Guild 过滤（频道必须在允许的服务器内）
        if !self.allowed_guild_ids.is_empty() {
            match msg.guild_id {
                None => {
                    // 私聊消息，不在任何 Guild 中，跳过
                    debug!("忽略私聊消息（Guild 过滤）: user={}", msg.author.id);
                    return;
                }
                Some(guild_id) => {
                    if !self.allowed_guild_ids.contains(&guild_id) {
                        debug!("忽略来自未授权 Guild 的消息: guild={}", guild_id);
                        return;
                    }
                }
            }
        }

        // 3. User 过滤
        if !self.allowed_user_ids.is_empty()
            && !self.allowed_user_ids.contains(&msg.author.id)
        {
            debug!("忽略未授权用户的消息: user={}", msg.author.id);
            return;
        }

        // 4. 前缀过滤（消息必须以 command_prefix 开头，或 @Bot mention）
        let content = if msg.content.starts_with(&self.command_prefix) {
            msg.content[self.command_prefix.len()..].trim().to_string()
        } else if msg.mentions_me(&ctx.http).await.unwrap_or(false) {
            // 移除 @Bot mention 部分
            let bot_id = ctx.http.get_current_user().await.map(|u| u.id).unwrap_or_default();
            msg.content
                .replace(&format!("<@{}>", bot_id), "")
                .replace(&format!("<@!{}>", bot_id), "")
                .trim()
                .to_string()
        } else {
            // 不满足触发条件，忽略
            return;
        };

        if content.is_empty() {
            return;
        }

        let user_id = msg.author.id;
        info!("Discord 消息: user={} content={:?}", user_id, &content[..content.len().min(50)]);

        // ─── 内置斜杠命令处理 ──────────────────────────────────────────────
        // Discord 中斜杠命令使用 "!/<command>" 格式（或 Discord Application Commands）
        if let Some(reply) = self.handle_builtin_command(&content, user_id).await {
            self.send_reply(&ctx, &msg, &reply).await;
            return;
        }

        // ─── 路由到用户 Agent ──────────────────────────────────────────────

        // 获取或创建该用户的 Agent
        let mut agents = self.agents.lock().await;
        if !agents.contains_key(&user_id) {
            match self.factory.create_agent() {
                Ok(agent) => {
                    agents.insert(user_id, agent);
                }
                Err(e) => {
                    warn!("为用户 {} 创建 Agent 失败: {}", user_id, e);
                    self.send_reply(&ctx, &msg, &format!("Agent 初始化失败: {}", e)).await;
                    return;
                }
            }
        }

        let agent = agents.get_mut(&user_id).expect("Agent 刚插入，不会为 None");

        // 发送"正在处理"提示（Discord 不支持流式打字机效果，用 typing indicator）
        let _ = msg.channel_id.broadcast_typing(&ctx.http).await;

        // 执行 Agent（非流式，等待完整响应）
        match agent.process_message(&content).await {
            Ok(response) => {
                self.send_reply(&ctx, &msg, &response).await;
            }
            Err(e) => {
                warn!("Agent 处理失败: user={} err={}", user_id, e);
                self.send_reply(&ctx, &msg, &format!("处理出错: {}", e)).await;
            }
        }
    }
}

impl DiscordHandler {
    /// 处理内置命令（/new, /clear）
    ///
    /// 返回 `Some(reply)` 表示已处理，调用方直接回复；
    /// 返回 `None` 表示不是内置命令，继续走 Agent 处理。
    async fn handle_builtin_command(&self, content: &str, user_id: UserId) -> Option<String> {
        let lower = content.trim().to_lowercase();
        match lower.as_str() {
            "/new" | "/clear" => {
                let mut agents = self.agents.lock().await;
                if let Some(agent) = agents.get_mut(&user_id) {
                    agent.clear_history();
                }
                Some("已开始新对话，历史记录已清空。".to_string())
            }
            "/help" => Some(
                "**RRClaw Discord Bot 使用指南**\n\
                 \n\
                 `!<消息>` — 直接提问\n\
                 `!/new` 或 `!/clear` — 开始新对话\n\
                 `!/help` — 显示本帮助\n\
                 \n\
                 示例：`!帮我审查这段 Rust 代码`"
                    .to_string(),
            ),
            _ => None,
        }
    }

    /// 将回复消息发送到 Discord（自动处理 2000 字符限制）
    async fn send_reply(&self, ctx: &Context, msg: &Message, content: &str) {
        if content.is_empty() {
            return;
        }

        let chunks = split_message(content, DISCORD_MAX_MSG_LEN - DISCORD_PAGE_PREFIX_LEN);

        if chunks.len() == 1 {
            // 单条消息直接发送
            if let Err(e) = msg.reply(&ctx.http, &chunks[0]).await {
                warn!("Discord 消息发送失败: {}", e);
            }
        } else {
            // 多条消息带页码发送
            let total = chunks.len();
            for (i, chunk) in chunks.iter().enumerate() {
                let page_prefix = format!("({}/{}) ", i + 1, total);
                let paginated = format!("{}{}", page_prefix, chunk);
                if let Err(e) = msg.reply(&ctx.http, &paginated).await {
                    warn!("Discord 分页消息发送失败 (page {}/{}): {}", i + 1, total, e);
                    break;
                }
                // 短暂延迟，避免触发 Discord 速率限制（5 msg/5s）
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
    }
}

// ─── DiscordChannel ───────────────────────────────────────────────────────────

/// Discord Channel 主结构体
pub struct DiscordChannel {
    bot_token: String,
    allowed_guild_ids: Vec<u64>,
    allowed_user_ids: Vec<u64>,
    command_prefix: String,
    config: Config,
    memory: Arc<SqliteMemory>,
}

impl DiscordChannel {
    /// 从 DiscordConfig 和全局 Config 创建 DiscordChannel
    pub fn new(
        discord_config: &crate::config::DiscordConfig,
        config: Config,
        memory: Arc<SqliteMemory>,
    ) -> Self {
        Self {
            bot_token: discord_config.bot_token.clone(),
            allowed_guild_ids: discord_config.allowed_guild_ids.clone(),
            allowed_user_ids: discord_config.allowed_user_ids.clone(),
            command_prefix: discord_config.command_prefix.clone(),
            config,
            memory,
        }
    }

    /// 启动 Discord Bot（阻塞，直到 Bot 断开）
    pub async fn start(self) -> Result<()> {
        let factory = Arc::new(AgentFactory::new(self.config, self.memory));

        let handler = DiscordHandler {
            agents: Arc::new(Mutex::new(HashMap::new())),
            factory,
            allowed_guild_ids: self
                .allowed_guild_ids
                .iter()
                .map(|&id| GuildId::new(id))
                .collect(),
            allowed_user_ids: self
                .allowed_user_ids
                .iter()
                .map(|&id| UserId::new(id))
                .collect(),
            command_prefix: self.command_prefix,
        };

        // 设置 Gateway Intents（按需申请权限）
        // MESSAGE_CONTENT 需要在 Discord Developer Portal 中开启
        let intents = GatewayIntents::GUILD_MESSAGES
            | GatewayIntents::DIRECT_MESSAGES
            | GatewayIntents::MESSAGE_CONTENT; // Privileged intent，需在开发者后台开启

        let mut client = Client::builder(&self.bot_token, intents)
            .event_handler(handler)
            .await
            .map_err(|e| eyre!("创建 Discord client 失败: {}", e))?;

        info!("Discord Bot 正在启动...");

        client
            .start()
            .await
            .map_err(|e| eyre!("Discord Bot 运行出错: {}", e))
    }
}

// ─── 工具函数 ─────────────────────────────────────────────────────────────────

/// 将长消息按 max_len 分割为多段（按换行符优先分割，避免截断单词）
///
/// # 参数
/// - `content`: 要分割的消息内容
/// - `max_len`: 每段最大字符数（字符数，非字节数）
///
/// # 返回值
/// 分割后的段列表，每段长度不超过 max_len
pub fn split_message(content: &str, max_len: usize) -> Vec<String> {
    if content.chars().count() <= max_len {
        return vec![content.to_string()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();

    for line in content.split('\n') {
        // 如果单行就超过 max_len，强制截断
        if line.chars().count() > max_len {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            let mut line_chars = line.chars();
            loop {
                let chunk: String = line_chars.by_ref().take(max_len).collect();
                if chunk.is_empty() {
                    break;
                }
                chunks.push(chunk);
            }
            continue;
        }

        let would_be_len = current.chars().count()
            + if current.is_empty() { 0 } else { 1 } // 换行符
            + line.chars().count();

        if would_be_len > max_len && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
        }

        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(line);
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}

// ─── 测试 ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ─── split_message 测试 ───────────────────────────────────────────────

    #[test]
    fn short_message_not_split() {
        let msg = "Hello, Discord!";
        let chunks = split_message(msg, 2000);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], msg);
    }

    #[test]
    fn empty_message() {
        let chunks = split_message("", 2000);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "");
    }

    #[test]
    fn long_message_splits_by_newline() {
        // 构造一个跨越 2 个块的消息
        let line_a = "A".repeat(100);
        let line_b = "B".repeat(100);
        // max_len = 150，两行合计 200（加换行符 201），应分成两块
        let msg = format!("{}\n{}", line_a, line_b);
        let chunks = split_message(&msg, 150);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], line_a);
        assert_eq!(chunks[1], line_b);
    }

    #[test]
    fn very_long_single_line_force_splits() {
        // 单行超过 max_len，强制截断
        let long_line = "X".repeat(500);
        let chunks = split_message(&long_line, 200);
        assert_eq!(chunks.len(), 3); // ceil(500/200) = 3
        assert_eq!(chunks[0].len(), 200);
        assert_eq!(chunks[1].len(), 200);
        assert_eq!(chunks[2].len(), 100);
    }

    #[test]
    fn multiline_message_respects_boundaries() {
        let msg = "line1\nline2\nline3\nline4\nline5";
        let chunks = split_message(msg, 15);
        // 每段最多 15 字符
        for chunk in &chunks {
            assert!(chunk.chars().count() <= 15, "chunk too long: {:?}", chunk);
        }
        // 所有内容都在某个 chunk 中
        let reconstructed = chunks.join("\n");
        for line in ["line1", "line2", "line3", "line4", "line5"] {
            assert!(reconstructed.contains(line), "缺少行: {}", line);
        }
    }

    #[test]
    fn exactly_max_len_not_split() {
        let msg = "A".repeat(2000);
        let chunks = split_message(&msg, 2000);
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn one_over_max_len_splits() {
        let msg = "A".repeat(2001);
        let chunks = split_message(&msg, 2000);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 2000);
        assert_eq!(chunks[1].len(), 1);
    }

    #[test]
    fn unicode_content_splits_correctly() {
        // 中文字符每个占 3 字节，但 split_message 按字符数而非字节数分割
        let msg = "你好世界！".repeat(100); // 5 字符/次 × 100 = 500 字符
        let chunks = split_message(&msg, 100);
        // 每段不超过 100 字符
        for chunk in &chunks {
            assert!(chunk.chars().count() <= 100);
        }
    }
}
```

---

## 五、Config Schema 扩展

### 5.1 src/config/schema.rs 新增 DiscordConfig

```rust
/// Discord Bot 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordConfig {
    /// 是否启用 Discord Channel
    #[serde(default = "default_discord_enabled")]
    pub enabled: bool,
    /// Discord Bot Token（从 Discord Developer Portal 获取）
    pub bot_token: String,
    /// 允许响应的 Guild（服务器）ID 列表，空 = 响应所有
    #[serde(default)]
    pub allowed_guild_ids: Vec<u64>,
    /// 允许响应的用户 ID 列表，空 = 响应所有
    #[serde(default)]
    pub allowed_user_ids: Vec<u64>,
    /// 消息触发前缀，默认 "!"
    #[serde(default = "default_command_prefix")]
    pub command_prefix: String,
}

fn default_discord_enabled() -> bool { true }
fn default_command_prefix() -> String { "!".to_string() }
```

### 5.2 在 Config 中新增 discord 字段

```rust
pub struct Config {
    pub default: DefaultConfig,
    pub providers: HashMap<String, ProviderConfig>,
    pub memory: MemoryConfig,
    pub security: SecurityConfig,
    pub telegram: Option<TelegramConfig>,
    pub reliability: ReliabilityConfig,
    pub mcp: Option<McpConfig>,
    pub routines: RoutinesConfig,
    #[serde(default)]                       // ← 新增
    pub discord: Option<DiscordConfig>,     // ← 新增
}
```

### 5.3 config.toml 示例

```toml
[channels.discord]
enabled = true
bot_token = "MTxxxxxx.Gyyyyy.zzzzzzzzzzzzzzzzzzzzzzzzz"

# 只响应指定服务器（强烈建议配置，避免被陌生服务器滥用）
allowed_guild_ids = [123456789012345678]

# 只响应指定用户（如只允许自己使用，则填入自己的 Discord User ID）
# 空 = 允许所有 allowed_guild_ids 内的用户
allowed_user_ids = [987654321098765432]

# 触发前缀（消息必须以此开头才会触发 Bot）
command_prefix = "!"
```

> **如何获取 Discord ID**：在 Discord 客户端中开启开发者模式（设置 → 高级 → 开发者模式），然后右键点击服务器/用户即可复制 ID。

---

## 六、channels/mod.rs 注册

```rust
// src/channels/mod.rs 新增：
pub mod discord;    // ← 新增

use serde::{Deserialize, Serialize};

/// 通道消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMessage {
    pub id: String,
    pub sender: String,
    pub content: String,
    pub channel: String,
    pub timestamp: u64,
}
```

---

## 七、main.rs 集成

在 `src/main.rs` 的 `run_telegram()` 函数（或新建 `run_discord()` 函数）的模式基础上，新增 Discord 启动逻辑：

```rust
/// 以 Discord Bot 模式启动 RRClaw
async fn run_discord(config: Config, memory: Arc<SqliteMemory>) -> Result<()> {
    use crate::channels::discord::DiscordChannel;

    let discord_config = config
        .discord
        .as_ref()
        .ok_or_else(|| color_eyre::eyre::eyre!(
            "Discord 未配置。请在 config.toml 中添加：\n\
             [channels.discord]\n\
             bot_token = \"your-discord-bot-token\""
        ))?
        .clone();

    if !discord_config.enabled {
        return Err(color_eyre::eyre::eyre!("Discord Channel 已禁用（enabled = false）"));
    }

    tracing::info!("以 Discord Bot 模式启动 RRClaw...");
    let channel = DiscordChannel::new(&discord_config, config, memory);
    channel.start().await?;
    Ok(())
}
```

在 `main.rs` 的 `main()` 函数的 subcommand 匹配中新增 `discord` 子命令：

```rust
// clap subcommands 中新增
/// 以 Discord Bot 模式运行
Discord,
```

```rust
// 匹配 Discord 子命令
Commands::Discord => {
    run_discord(config, memory).await?;
}
```

---

## 八、Discord Bot 注册步骤（开发者文档）

> 此节面向同事，描述在 Discord Developer Portal 的操作步骤。代码无需改动。

### 8.1 创建 Discord Application

1. 访问 [Discord Developer Portal](https://discord.com/developers/applications)
2. 点击 **New Application**，输入应用名称（如 "RRClaw"）
3. 在左侧菜单选择 **Bot**
4. 点击 **Add Bot** → 确认
5. 在 Bot 页面找到 **Token** 部分，点击 **Reset Token** 并复制（填入 config.toml 的 `bot_token`）

### 8.2 开启 Privileged Intent

Discord Message Content 是 Privileged Gateway Intent，必须在 Developer Portal 中手动开启：

在 Bot 页面 → **Privileged Gateway Intents** 中开启：
- [x] **MESSAGE CONTENT INTENT**（读取消息正文内容）

> **注意**：Bot 加入超过 100 个 Guild 后，Message Content Intent 需要向 Discord 申请验证。个人使用无需担心此限制。

### 8.3 邀请 Bot 到服务器

1. 在 Developer Portal 左侧菜单选择 **OAuth2** → **URL Generator**
2. 在 **SCOPES** 中勾选：`bot`
3. 在 **BOT PERMISSIONS** 中勾选：
   - `Read Messages/View Channels`
   - `Send Messages`
   - `Read Message History`
4. 复制生成的 URL，在浏览器中打开，将 Bot 添加到你的服务器

---

## 九、改动范围汇总

| 文件 | 改动类型 | 说明 |
|------|---------|------|
| `Cargo.toml` | 新增依赖 | `serenity = "0.12"` |
| `src/channels/discord.rs` | **新增文件** | DiscordChannel 完整实现（~250 行） |
| `src/channels/mod.rs` | 微改 | `pub mod discord;` |
| `src/config/schema.rs` | 小改 | 新增 `DiscordConfig` + `Config.discord` 字段 |
| `src/main.rs` | 小改 | 新增 `discord` subcommand + `run_discord()` 函数 |

**不需要改动**：Agent、Provider、Memory、Security、Tools、Skills、Routines。

---

## 十、提交策略

| # | 提交 message | 内容 |
|---|-------------|------|
| 1 | `docs: add P5-6 Discord channel design` | 本文件 |
| 2 | `feat: add serenity dependency for Discord` | Cargo.toml |
| 3 | `feat: add DiscordConfig to config schema` | schema.rs |
| 4 | `feat: add DiscordChannel implementation` | channels/discord.rs + mod.rs |
| 5 | `feat: add discord subcommand to main.rs` | main.rs |
| 6 | `test: add Discord message splitting unit tests` | 已在 discord.rs 内 |

---

## 十一、测试执行方式

```bash
# 运行 Discord Channel 单元测试（不需要真实 Bot 连接）
cargo test -p rrclaw channels::discord

# 运行全部测试
cargo test -p rrclaw

# clippy 检查
cargo clippy -p rrclaw -- -D warnings

# 手动集成测试（需要真实 Bot Token 和 Discord 服务器）
DISCORD_BOT_TOKEN=xxx cargo run -- discord
# 在配置的 Discord 服务器中发送：!你好
```

---

## 十二、关键注意事项

### 12.1 MESSAGE_CONTENT Privileged Intent

读取消息正文内容需要 `GatewayIntents::MESSAGE_CONTENT`，这是 Discord 的 Privileged Intent，必须在 Discord Developer Portal 中手动开启，否则 Bot 会连接成功但收到的消息 `content` 字段为空字符串。

**排查方式**：如果 Bot 无法响应消息，首先检查 Developer Portal 中 MESSAGE CONTENT INTENT 是否开启。

### 12.2 Typing Indicator vs 流式输出

Discord 没有原生的消息编辑流式输出（Telegram Bot 可以实时编辑消息模拟流式）。RRClaw 使用 `broadcast_typing()` 让 Bot 显示"正在输入..."状态，等 Agent 完整响应后再发送。

**V2 改进**：可以先发送一条"处理中..."消息，Agent 完成后用 `Message::edit()` 替换内容，但这需要保存初始消息的引用，实现略复杂。

### 12.3 速率限制

Discord API 有速率限制（Rate Limit）：同一频道 5 条/5 秒。多段消息发送时，代码中已加入 500ms 延迟避免触发。

serenity 内部也有 Rate Limit 处理（自动等待重试），但最好不要主动触发。

### 12.4 per-user Agent 内存增长

`HashMap<UserId, Agent>` 会随使用用户数增长。每个 Agent 实例包含对话历史（最多 50 条）+ 持有 Provider、Tools 等引用。

**当前无 LRU 淘汰机制**，长期运行 Bot 时如有大量用户会有内存泄漏风险。V2 改进：设置 Agent 空闲 TTL（如 24 小时），超时后从 HashMap 中移除。

### 12.5 私聊（DM）支持

代码中同时监听了 `GatewayIntents::DIRECT_MESSAGES`，支持通过私聊触发 Bot（无需 `!` 前缀或 @Bot）。

但如果配置了 `allowed_guild_ids`，私聊消息会因为 `msg.guild_id = None` 而被过滤掉。

**如需支持私聊**：在过滤逻辑中，当 `allowed_guild_ids` 非空但 `guild_id = None` 时，额外检查 `allowed_user_ids`：
```rust
// msg.guild_id == None 说明是私聊
// 如果 allowed_user_ids 中有此用户，放行
if allowed_user_ids.contains(&msg.author.id) {
    // 放行
}
```

当前 P5 版本不实现此边界情况，保持逻辑简单。

### 12.6 vs Telegram 实现的代码复用

Discord 和 Telegram 的 `AgentFactory` 几乎完全相同。后续重构时，可以将 `AgentFactory` 提取到公共模块（如 `src/channels/factory.rs`），两个 channel 共享，减少代码重复。P5 版本暂不重构，保持两个文件各自独立，降低实现复杂度。

---

## 十三、用户体感示例

```
# Discord 服务器 #bot-channel 频道

用户: !帮我审查下这段 Rust 代码 fn main() { let x = 5; println!("{}", x) }

RRClaw: 代码审查结果：

✅ 功能正确：`println!` 使用 `{}` 格式化，语法无误。

💡 改进建议：
1. 变量 `x` 在 main 函数中只用了一次，可以考虑直接 `println!("{}", 5)` 或用常量：
   ```rust
   const MSG: i32 = 5;
   fn main() { println!("{}", MSG); }
   ```
2. 缺少注释和文档（对于示例代码可接受）
3. 没有分号（`println!` 末尾需要分号，当前代码中缺少）

综合评分：⭐⭐⭐⭐（4/5），功能正确，可以添加更多上下文说明。

用户: !/new

RRClaw: 已开始新对话，历史记录已清空。

用户: !/help

RRClaw: **RRClaw Discord Bot 使用指南**

`!<消息>` — 直接提问
`!/new` 或 `!/clear` — 开始新对话
`!/help` — 显示本帮助

示例：`!帮我审查这段 Rust 代码`
```
