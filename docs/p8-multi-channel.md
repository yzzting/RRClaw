# P8: 多 Channel 统一入口

> 一个 Agent 进程，同时支持 CLI 和 Telegram 输入

## 一、背景与问题

### 当前架构

```
┌─────────────────────────────────────────────────────┐
│  cargo run -- agent                                  │
│  └── CLI REPL（独立进程）                             │
│       └── 通过 run_repl() 启动                         │
└─────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────┐
│  cargo run -- telegram                               │
│  └── Telegram Bot（独立进程）                          │
│       └── 通过 run_telegram() 启动                    │
└─────────────────────────────────────────────────────┘
```

**问题**：
1. 需要开两个终端进程
2. Telegram 配置必须手动编辑 `config.toml`
3. 两个进程不共享 Agent 状态（Memory 独立）

### 目标架构

```
┌─────────────────────────────────────────────────────┐
│  cargo run -- agent                                  │
│                                                     │
│  ┌─────────────┐      ┌──────────────────┐        │
│  │ CLI REPL    │ ←──→ │ Agent Loop       │        │
│  └─────────────┘      └────────┬─────────┘        │
│                                │                    │
│  ┌─────────────┐              │                    │
│  │ Telegram    │ ←────────────┘                    │
│  │ Bot         │                                   │
│  └─────────────┘                                   │
└─────────────────────────────────────────────────────┘
```

**改进点**：
1. 一个进程，同时监听 CLI 和 Telegram
2. Telegram 配置可通过 Agent 对话完成（自然语言配置）
3. 共享 Memory、Skills、Agent 状态

---

## 二、功能清单

| 编号 | 功能 | 难度 | 效果 |
|------|------|------|------|
| P8-1 | Agent 多 Channel 监听 | 高 | CLI + Telegram 同时在线 |
| P8-2 | Telegram 配置向导 | 中 | 自然语言配置 Bot Token |
| P8-3 | 统一消息路由 | 中 | 消息来源透明，回复回原渠道 |

---

## 三、设计方案

### 3.1 整体流程

```
cargo run -- agent
    │
    ├── 加载 config.toml
    │
    ├── 检查 [telegram] 配置
    │   ├── 有 → 启动 Telegram Bot 任务
    │   └── 无 → 跳过 Telegram
    │
    ├── 启动 CLI REPL 任务
    │
    └── tokio::select! 同时运行多个任务
```

### 3.2 消息统一抽象

定义 `ChannelMessage` trait，统一 CLI 和 Telegram 的消息格式：

```rust
/// 统一消息来源
#[derive(Debug, Clone)]
pub enum MessageSource {
    Cli,
    Telegram { chat_id: i64 },
}

/// 统一消息结构
pub struct UnifiedMessage {
    pub source: MessageSource,
    pub content: String,
    pub reply_tx: oneshot::Sender<String>,  // 回复通道
}
```

### 3.3 Agent 多任务并发

```rust
// main.rs run_agent()

async fn run_agent(...) -> Result<()> {
    // ... 现有代码（加载配置、Provider、Tools、Skills、Memory）...

    // 创建 Agent
    let mut agent = Agent::new(...);

    // 创建消息通道
    let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<UnifiedMessage>();

    // 启动任务
    let mut handles = vec![];

    // 1. CLI 任务
    handles.push(tokio::spawn(async move {
        run_cli_channel(&mut agent, msg_tx).await
    }));

    // 2. Telegram 任务（如果配置了）
    if config.telegram.is_some() {
        handles.push(tokio::spawn(async move {
            run_telegram_channel(config, memory, msg_tx).await
        }));
    }

    // 3. Agent 处理任务
    handles.push(tokio::spawn(async move {
        agent_loop(&mut agent, &mut msg_rx).await
    }));

    // 等待任一任务结束
    let _ = futures::future::join_all(handles).await;

    Ok(())
}
```

### 3.4 Agent Loop 改造

```rust
async fn agent_loop(agent: &mut Agent, msg_rx: &mut mpsc::Receiver<UnifiedMessage>) {
    while let Some(msg) = msg_rx.recv().await {
        // 处理消息
        let response = agent.chat(&msg.content).await;

        // 按原路返回
        let _ = msg.reply_tx.send(response);
    }
}
```

---

## 四、Telegram 配置向导

### 4.1 现有配置结构

```toml
[telegram]
bot_token = "123456:ABC-DEF..."
allowed_chat_ids = []
```

### 4.2 配置方式

**方式 A：斜杠命令（推荐）**

在 CLI 里直接说：
```
/config telegram enable
```

Agent 引导：
```
请提供你的 Telegram Bot Token（从 @BotFather 获取）：
```

用户输入 Token 后，保存到 config.toml，重启 Telegram 监听。

**方式 B：自然语言**

```
用户：帮我配置 Telegram
Agent：我来帮你配置 Telegram Bot。请先找 @BotFather 创建一个新机器人，获取 Bot Token。
Agent 拿到 Token 后，我会帮你保存到配置文件中。

请提供 Bot Token：
```

### 4.3 实现

```rust
// channels/mod.rs

pub trait Channel: Send + Sync {
    fn name(&self) -> &str;
    /// 启动 channel，返回消息接收 channel
    fn run(&self) -> impl Stream<Item = UnifiedMessage> + Send;
    /// 发送回复
    fn send(&self, msg: UnifiedMessage, response: String) -> impl Future<Output = ()> + Send;
}
```

或者更简单的方式：复用现有 `run_repl()` 和 `run_telegram()`，只在 main.rs 层面并发启动。

---

## 五、改动范围

### 5.1 涉及文件

| 文件 | 改动 | 说明 |
|------|------|------|
| `src/main.rs` | 重构 | `run_agent()` 改为多任务并发 |
| `src/channels/mod.rs` | 新增 | 统一消息抽象 |
| `src/channels/cli.rs` | 改造 | 支持通过消息 channel 接收输入 |
| `src/channels/telegram.rs` | 改造 | 支持通过消息 channel 发送回复 |
| `src/config/schema.rs` | 新增 | 可选：Telegram 运行时开关 |

### 5.2 新增文件

| 文件 | 说明 |
|------|------|
| `src/channels/unified.rs` | 统一消息类型 + Agent 多任务并发 |

---

## 六、配置变化

### 6.1 运行时开关（可选）

```toml
[telegram]
# 现有配置
bot_token = "xxx"

# 新增：运行时开关（默认 true）
enabled = true
```

### 6.2 不改动现有配置兼容性

- 保留 `[telegram]` 原有字段
- 有配置则自动启用，无配置则不启动 Telegram

---

## 七、测试方案设计

### 7.1 测试策略

采用**分层测试**策略，从单元测试到集成测试：

```
┌─────────────────────────────────────────────────────────┐
│  集成测试 (E2E)                                          │
│  • 多 Channel 同时在线                                    │
│  • 消息路由正确性                                         │
│  • 配置向导流程                                           │
└─────────────────────────────────────────────────────────┘
                          │
┌─────────────────────────────────────────────────────────┐
│  集成测试 (模块级)                                        │
│  • CLI Channel 消息收发                                   │
│  • Telegram Channel 消息收发                              │
│  • 统一消息路由                                           │
└─────────────────────────────────────────────────────────┘
                          │
┌─────────────────────────────────────────────────────────┐
│  单元测试                                                │
│  • UnifiedMessage 序列化                                  │
│  • MessageSource 枚举                                      │
│  • 消息 channel 行为                                      │
└─────────────────────────────────────────────────────────┘
```

### 7.2 单元测试

#### 7.2.1 统一消息类型测试

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unified_message_creation() {
        let (tx, _rx) = oneshot::channel();
        let msg = UnifiedMessage {
            source: MessageSource::Cli,
            content: "Hello".to_string(),
            reply_tx: tx,
        };
        assert_eq!(msg.content, "Hello");
    }

    #[test]
    fn test_message_source_telegram() {
        let source = MessageSource::Telegram { chat_id: 12345 };
        match source {
            MessageSource::Telegram { chat_id } => {
                assert_eq!(chat_id, 12345);
            }
            _ => panic!("Expected Telegram source"),
        }
    }

    #[test]
    fn test_message_source_cli() {
        let source = MessageSource::Cli;
        match source {
            MessageSource::Cli => {}
            _ => panic!("Expected Cli source"),
        }
    }
}
```

#### 7.2.2 消息 Channel 测试

```rust
#[tokio::test]
async fn test_message_channel_send_receive() {
    let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<UnifiedMessage>();

    // 发送消息
    let (reply_tx, _reply_rx) = oneshot::channel();
    msg_tx.send(UnifiedMessage {
        source: MessageSource::Cli,
        content: "test".to_string(),
        reply_tx,
    }).unwrap();

    // 接收消息
    let msg = msg_rx.recv().await.unwrap();
    assert_eq!(msg.content, "test");
}

#[tokio::test]
async fn test_message_channel_multi_sender() {
    let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<UnifiedMessage>();

    // 多个发送者
    for i in 0..3 {
        let (reply_tx, _reply_rx) = oneshot::channel();
        msg_tx.send(UnifiedMessage {
            source: MessageSource::Telegram { chat_id: i },
            content: format!("msg-{}", i),
            reply_tx,
        }).unwrap();
    }

    // 验证接收顺序
    for i in 0..3 {
        let msg = msg_rx.recv().await.unwrap();
        assert_eq!(msg.content, format!("msg-{}", i));
    }
}
```

### 7.3 集成测试

#### 7.3.1 CLI Channel 测试

```rust
// tests/channels/cli_unified.rs

use rrclaw::channels::cli;
use rrclaw::agent::Agent;
use std::sync::Arc;

/// 测试 CLI 通过消息 channel 发送和接收
#[tokio::test]
async fn test_cli_message_channel() {
    // 创建测试 Agent（使用 mock provider）
    let agent = create_test_agent().await;

    // 创建消息 channel
    let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<UnifiedMessage>();

    // 启动 CLI 任务（后台）
    let cli_handle = tokio::spawn(async move {
        run_cli_with_channel(agent, msg_tx).await
    });

    // 模拟用户输入
    let (reply_tx, mut reply_rx) = oneshot::channel();
    msg_tx.send(UnifiedMessage {
        source: MessageSource::Cli,
        content: "你好".to_string(),
        reply_tx,
    }).unwrap();

    // 验证收到回复
    let response = reply_rx.await.unwrap();
    assert!(!response.is_empty());

    // 清理
    cli_handle.abort();
}
```

#### 7.3.2 Telegram Channel Mock 测试

```rust
// tests/channels/telegram_unified.rs

use rrclaw::channels::telegram;
use rrclaw::config::Config;
use rrclaw::memory::SqliteMemory;
use std::sync::Arc;

/// Mock Telegram Bot 用于测试
struct MockTelegramBot {
    messages: Arc<Mutex<Vec<String>>>,
    responses: Arc<Mutex<Vec<String>>>,
}

impl MockTelegramBot {
    fn new() -> Self {
        Self {
            messages: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(Vec::new())),
        }
    }

    async fn send_message(&self, chat_id: i64, text: &str) {
        let mut responses = self.responses.lock().await;
        responses.push(format!("[chat={}] {}", chat_id, text));
    }
}

/// 测试 Telegram 消息路由到 Agent
#[tokio::test]
async fn test_telegram_message_routing() {
    let bot = Arc::new(MockTelegramBot::new());
    let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<UnifiedMessage>();

    // 模拟 Telegram 收到消息
    let (reply_tx, mut reply_rx) = oneshot::channel();
    msg_tx.send(UnifiedMessage {
        source: MessageSource::Telegram { chat_id: 12345 },
        content: "帮我写一个 hello world".to_string(),
        reply_tx,
    }).unwrap();

    // Agent 处理（mock）
    let response = "```rust\nfn main() {\n    println!(\"Hello, World!\");\n}\n```".to_string();

    // 验证回复发送回 Telegram
    let reply = reply_rx.await.unwrap();
    assert!(reply.contains("Hello"));

    // 验证 Bot 发送了消息
    let responses = bot.responses.lock().await;
    assert!(!responses.is_empty());
}
```

#### 7.3.3 多 Channel 同时在线测试

```rust
// tests/e2e/multi_channel.rs

/// E2E 测试：CLI 和 Telegram 同时在线
#[tokio::test]
async fn test_multi_channel_concurrent() {
    // 1. 准备环境
    let config = TestConfig::with_telegram();
    let memory = create_test_memory().await;
    let agent = create_test_agent().await;

    // 2. 创建消息 channel
    let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<UnifiedMessage>();

    // 3. 启动 Agent 任务
    let agent_handle = tokio::spawn(async move {
        agent_loop(&mut agent.clone(), &mut msg_rx).await
    });

    // 4. 同时从两个 Channel 发送消息
    let mut handles = vec![];

    // CLI 消息
    let (cli_reply_tx, cli_reply_rx) = oneshot::channel();
    let cli_tx = msg_tx.clone();
    handles.push(tokio::spawn(async move {
        cli_tx.send(UnifiedMessage {
            source: MessageSource::Cli,
            content: "CLI 消息".to_string(),
            reply_tx: cli_reply_tx,
        }).unwrap();
    }));

    // Telegram 消息
    let (tg_reply_tx, tg_reply_rx) = oneshot::channel();
    handles.push(tokio::spawn(async move {
        msg_tx.send(UnifiedMessage {
            source: MessageSource::Telegram { chat_id: 12345 },
            content: "Telegram 消息".to_string(),
            reply_tx: tg_reply_tx,
        }).unwrap();
    }));

    // 5. 等待所有消息发送完成
    futures::future::join_all(handles).await;

    // 6. 验证两边都收到回复
    let cli_response = cli_reply_rx.await.unwrap();
    let tg_response = tg_reply_rx.await.unwrap();

    assert!(!cli_response.is_empty(), "CLI 应收到回复");
    assert!(!tg_response.is_empty(), "Telegram 应收到回复");

    // 7. 清理
    agent_handle.abort();
}
```

#### 7.3.4 配置向导测试

```rust
// tests/e2e/config_wizard.rs

/// 测试 Telegram 配置向导流程
#[tokio::test]
async fn test_telegram_config_wizard() {
    // 1. 准备空配置
    let config = Config::default();
    assert!(config.telegram.is_none());

    // 2. 模拟用户输入 /config telegram enable
    let wizard = TelegramConfigWizard::new();

    // 3. 第一步：引导获取 Token
    let step1 = wizard.next_step(None).await;
    assert!(step1.prompt.contains("BotFather"));
    assert!(step1.expected_input == InputType::BotToken);

    // 4. 第二步：验证 Token（使用无效 Token）
    let step2 = wizard.next_step(Some("invalid-token")).await;
    assert!(step2.error.contains("无效"));

    // 5. 第三步：使用有效 Token
    let step3 = wizard.next_step(Some("123456:ABC-DEF")).await;
    assert!(step3.success);

    // 6. 验证配置已保存
    let config = Config::load_or_init().unwrap();
    assert!(config.telegram.is_some());
    assert!(config.telegram.unwrap().bot_token == "123456:ABC-DEF");
}
```

### 7.4 测试数据

#### 7.4.1 Mock Provider

```rust
// tests/mocks/provider.rs

pub struct MockProvider {
    responses: Vec<ChatResponse>,
}

impl MockProvider {
    pub fn new(responses: Vec<ChatResponse>) -> Self {
        Self { responses }
    }
}

#[async_trait]
impl Provider for MockProvider {
    async fn chat_with_tools(&self, ...) -> Result<ChatResponse> {
        Ok(self.responses.pop().unwrap_or_else(|| {
            ChatResponse {
                text: Some("Mock response".to_string()),
                tool_calls: vec![],
            }
        }))
    }
}
```

#### 7.4.2 Test Config

```rust
// tests/helpers/mod.rs

pub fn test_config() -> Config {
    Config {
        default: DefaultConfig {
            provider: "mock".to_string(),
            model: "mock-model".to_string(),
            temperature: 0.7,
        },
        providers: HashMap::new(),
        memory: MemoryConfig::default(),
        security: SecurityConfig::default(),
        telegram: Some(TelegramConfig {
            bot_token: "test-token".to_string(),
            allowed_chat_ids: vec![12345],
        }),
        ..Default::default()
    }
}
```

### 7.5 测试覆盖矩阵

| 功能点 | 单元测试 | 集成测试 | E2E 测试 |
|--------|---------|---------|---------|
| UnifiedMessage 创建 | ✅ | - | - |
| MessageSource 枚举 | ✅ | - | - |
| 消息 Channel 收发 | ✅ | - | - |
| CLI 消息路由 | - | ✅ | - |
| Telegram 消息路由 | - | ✅ | - |
| 多 Channel 并发 | - | - | ✅ |
| 配置向导流程 | - | ✅ | - |
| 消息来源识别 | ✅ | ✅ | ✅ |
| 回复路由正确性 | - | ✅ | ✅ |

### 7.6 运行测试

```bash
# 运行所有测试
cargo test --workspace

# 只运行 channels 模块测试
cargo test --package rrclaw channels

# 只运行 multi-channel 相关测试
cargo test multi_channel

# 带日志运行
RUST_LOG=trace cargo test -- --nocapture
```

---

## 八、验证方式（手动验证）

### 8.1 功能验证

| 场景 | 验证方式 |
|------|---------|
| CLI 对话正常 | CLI 输入问题，Agent 正常回复 |
| Telegram 对话正常 | Telegram 发送问题，Bot 正常回复 |
| 同时在线 | CLI 和 Telegram 同时发消息，都收到回复 |
| 配置向导 | `/config telegram enable` 引导配置流程 |

---

## 九、提交策略

| 步骤 | 改动 | 提交 |
|------|------|------|
| 1 | 统一消息抽象 | feat: add unified message type |
| 2 | CLI 消息 channel 改造 | refactor: cli supports message channel |
| 3 | Telegram 消息 channel 改造 | refactor: telegram supports message channel |
| 4 | Agent 多任务并发 | feat: agent supports multi-channel |
| 5 | 测试 | test: add multi-channel tests |

---

## 十、文档链接

- [Channel trait 设计](channels/Claude.md)
- [Telegram Bot 实现](channels/telegram.rs)
- [P7 动态工具加载](p7-plan.md)（参考架构）
