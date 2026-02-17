# RRClaw P1 实现计划

## Context

P0 MVP 已完成（27 commits, 61 tests），端到端可用。但当前体验有明显短板：
- 响应需等全部生成完才显示（无流式输出）
- Supervised 模式的确认完全靠 prompt，没有程序化保障
- 退出 REPL 后对话历史丢失
- 只有 CLI 通道，无法通过 Telegram 使用
- 初始配置需手动编辑 TOML，无引导式 setup

用户决策：
- Telegram 使用 `teloxide` 库
- 向量记忆暂不做，放 P2
- Setup 用 `dialoguer` CLI 向导

---

## P1 功能列表（按实现顺序）

### P1-1: 流式输出（Streaming）
### P1-2: Supervised 模式程序化确认
### P1-3: History 持久化
### P1-4: Setup 配置向导
### P1-5: Telegram Channel

---

## P1-1: 流式输出（Streaming）

**目标**: 模型回复逐字显示，而非等完整响应

**改动文件**:
- `src/providers/traits.rs` — Provider trait 新增 `chat_stream()` 方法
- `src/providers/compatible.rs` — 实现 OpenAI SSE stream 解析
- `src/providers/claude.rs` — 实现 Anthropic SSE stream 解析
- `src/agent/loop_.rs` — `process_message` 支持传入 output channel
- `src/channels/cli.rs` — 接收 stream 逐字打印

**设计**:

```rust
// traits.rs — 新增流式事件类型
pub enum StreamEvent {
    Text(String),           // 文本 token
    ToolCallStart(ToolCall), // tool call 开始
    Done(ChatResponse),     // 流结束，返回完整响应
}

// Provider trait 新增方法（带默认实现）
async fn chat_stream(
    &self,
    messages: &[ConversationMessage],
    tools: &[ToolSpec],
    model: &str,
    temperature: f64,
    tx: tokio::sync::mpsc::Sender<StreamEvent>,
) -> Result<ChatResponse> {
    // 默认实现: 调用 chat_with_tools，一次性发送
    let resp = self.chat_with_tools(messages, tools, model, temperature).await?;
    let _ = tx.send(StreamEvent::Done(resp.clone())).await;
    Ok(resp)
}
```

**Agent loop 改动**:
- `process_message` 新增 `tx: Option<mpsc::Sender<StreamEvent>>` 参数
- 有 tx 时调用 `chat_stream`，否则走原逻辑（不破坏现有测试）
- CLI REPL 创建 channel，spawn task 打印收到的 StreamEvent::Text

**SSE 解析**（compatible.rs）:
- 请求体加 `"stream": true`
- 用 `resp.bytes_stream()` 逐行读取 `data: {...}` 行
- 解析 `delta.content` / `delta.tool_calls` 增量
- 检测 `[DONE]` 结束

**提交策略**:
1. `feat: add StreamEvent type and chat_stream trait method`
2. `feat: implement SSE streaming for CompatibleProvider`
3. `feat: implement SSE streaming for ClaudeProvider`
4. `feat: add streaming support to agent loop`
5. `feat: add streaming output to CLI REPL`

---

## P1-2: Supervised 模式程序化确认

**目标**: Supervised 模式下，tool 执行前在终端显示确认提示，等用户输入 y/n

**改动文件**:
- `src/agent/loop_.rs` — tool 执行前检查 `policy.requires_confirmation()`
- `src/channels/cli.rs` — 提供确认回调
- `src/agent/mod.rs` — Agent 接受 confirm callback

**设计**:

```rust
// Agent 新增确认回调
pub type ConfirmFn = Box<dyn Fn(&str, &serde_json::Value) -> bool + Send + Sync>;

impl Agent {
    pub fn set_confirm_fn(&mut self, f: ConfirmFn) { ... }
}

// loop_.rs — execute_tool 前
if self.policy.requires_confirmation() {
    let desc = format!("执行工具 '{}': {}", tc.name, tc.arguments);
    if let Some(confirm) = &self.confirm_fn {
        if !confirm(&tc.name, &tc.arguments) {
            // 跳过此 tool call，返回 "用户拒绝执行" 作为 ToolResult
            continue;
        }
    }
}
```

**CLI 注入**:
```rust
// cli.rs
agent.set_confirm_fn(Box::new(|name, args| {
    print!("执行 {} {}? [y/N] ", name, args);
    // 读取一行输入判断
}));
```

**提交策略**:
1. `feat: add confirmation callback to Agent for Supervised mode`
2. `feat: wire CLI confirmation prompt for tool execution`

---

## P1-3: History 持久化

**目标**: REPL 退出后对话历史保留，重启可继续

**改动文件**:
- `src/agent/loop_.rs` — history 存取接口
- `src/memory/sqlite.rs` — 新增 `conversation_history` 表
- `src/channels/cli.rs` — 启动时加载 / 退出时保存

**设计**:

SQLite 新增表:
```sql
CREATE TABLE IF NOT EXISTS conversation_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    message_type TEXT NOT NULL,  -- "chat" | "tool_calls" | "tool_result"
    payload TEXT NOT NULL,       -- JSON 序列化的 ConversationMessage
    created_at TEXT NOT NULL
);
```

Agent 新增方法:
```rust
pub fn save_history(&self, memory: &SqliteMemory, session_id: &str) -> Result<()>;
pub fn load_history(&mut self, memory: &SqliteMemory, session_id: &str) -> Result<()>;
```

Session ID 策略: 用日期 `YYYY-MM-DD` 作为默认 session_id，同一天重启自动续接。

**提交策略**:
1. `feat: add conversation_history table to SqliteMemory`
2. `feat: add history save/load to Agent`
3. `feat: auto-persist history in CLI REPL`

---

## P1-4: Setup 配置向导

**目标**: `rrclaw setup` 交互式引导配置

**新增依赖**: `dialoguer = "0.11"`

**改动文件**:
- `Cargo.toml` — 添加 dialoguer
- `src/main.rs` — 添加 `Setup` subcommand
- `src/config/setup.rs` — 新文件，向导逻辑
- `src/config/mod.rs` — 导出 setup 模块

**向导流程**:
```
1. 选择默认 Provider: [deepseek / glm / minimax / claude / gpt]
2. 输入该 Provider 的 API Key (密码输入)
3. 选择默认模型 (根据 provider 提供选项)
4. 设置 temperature (默认 0.7)
5. 选择安全模式: [Supervised / Full / ReadOnly]
6. (可选) 配置 Telegram Bot Token
7. 确认并写入 ~/.rrclaw/config.toml
```

**提交策略**:
1. `feat: add dialoguer dependency`
2. `feat: add interactive setup wizard (rrclaw setup)`

---

## P1-5: Telegram Channel

**目标**: 通过 Telegram Bot 与 RRClaw 交互

**新增依赖**: `teloxide = "0.13"` (或最新稳定版)

**改动文件**:
- `Cargo.toml` — 添加 teloxide
- `src/channels/mod.rs` — 定义 Channel trait + ChannelMessage
- `src/channels/telegram.rs` — 新文件，TelegramChannel 实现
- `src/config/schema.rs` — 添加 `TelegramConfig`
- `src/main.rs` — 添加 `Telegram` subcommand

**设计**:

Channel trait (在 mod.rs 中正式定义):
```rust
#[async_trait]
pub trait Channel: Send + Sync {
    fn name(&self) -> &str;
    async fn start(&self, agent_factory: AgentFactory) -> Result<()>;
}
```

注意: 原 CLAUDE.md 设计的 `send/listen` 接口偏底层。Telegram 场景下每个 chat 需要独立 Agent 实例，建议改为 `start(agent_factory)` 模式，由 Channel 自己管理消息循环和 Agent 生命周期。

TelegramChannel:
```rust
pub struct TelegramChannel {
    bot_token: String,
    allowed_chat_ids: Vec<i64>,  // 空 = 允许所有
}

// 内部使用 teloxide dispatcher
// 每个 chat_id 维护独立 Agent 实例（HashMap<i64, Agent>）
// 消息到达 → 找/创建 Agent → process_message → 回复
```

Config 扩展:
```toml
[channels.telegram]
bot_token = "your-bot-token"
allowed_chat_ids = [123456789]  # 空数组 = 允许所有
```

**提交策略**:
1. `docs: add Channel trait design to channels/Claude.md`
2. `feat: define Channel trait and ChannelMessage`
3. `feat: add TelegramConfig to config schema`
4. `feat: implement TelegramChannel with teloxide`
5. `feat: add telegram subcommand to CLI`

---

## 总提交计划

| # | 提交 | 涉及模块 |
|---|------|---------|
| 28 | feat: add StreamEvent type and chat_stream trait method | providers/traits |
| 29 | feat: implement SSE streaming for CompatibleProvider | providers/compatible |
| 30 | feat: implement SSE streaming for ClaudeProvider | providers/claude |
| 31 | feat: add streaming support to agent loop | agent/loop_ |
| 32 | feat: add streaming output to CLI REPL | channels/cli |
| 33 | feat: add confirmation callback for Supervised mode | agent/loop_ |
| 34 | feat: wire CLI confirmation prompt for tool execution | channels/cli |
| 35 | feat: add conversation_history table to SqliteMemory | memory/sqlite |
| 36 | feat: add history save/load to Agent | agent/loop_ |
| 37 | feat: auto-persist history in CLI REPL | channels/cli |
| 38 | feat: add dialoguer dependency and setup wizard | config/setup, main |
| 39 | docs: add Channel trait design | channels/Claude.md |
| 40 | feat: define Channel trait and ChannelMessage | channels/mod |
| 41 | feat: add TelegramConfig to config schema | config/schema |
| 42 | feat: implement TelegramChannel with teloxide | channels/telegram |
| 43 | feat: add telegram subcommand | main |

共 ~16 commits，预计新增 ~1500-2000 行代码。

---

## 验证方式

1. **流式输出**: `cargo run -- agent` 输入问题，看到回复逐字出现
2. **Supervised 确认**: 配置 `autonomy = "supervised"`，执行 tool 时终端显示确认提示
3. **History 持久化**: REPL 退出后重启，发送"刚才聊了什么"，AI 能回忆上文
4. **Setup 向导**: `cargo run -- setup` 走完流程，检查 `~/.rrclaw/config.toml` 正确生成
5. **Telegram**: 配置 bot token 后 `cargo run -- telegram`，在 Telegram 中发消息收到回复
6. `cargo test` 全部通过
7. `cargo clippy -- -W clippy::all` 零警告
