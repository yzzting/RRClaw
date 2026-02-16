# RRClaw

安全优先的 AI 助手基础设施，100% Rust，Trait 可插拔架构。

## 项目定位

面向个人助手和企业内部使用的 AI Agent CLI 工具。

**核心特性**:
- 多模型支持（GLM 智谱、MiniMax、DeepSeek、Claude、GPT）
- 安全沙箱（命令白名单、路径限制、权限分级）
- 持久化记忆（SQLite 存储 + tantivy 中文全文搜索）
- 工具执行（Shell 命令、文件读写）
- 可插拔架构（Trait 抽象，易于扩展）

**MVP 范围**:
- P0: CLI Channel + Agent Loop + 多模型 Provider + 基础 Tools + Security
- P1: Telegram Channel、向量搜索记忆
- P2: 更多 Channel、Tunnel 层、Heartbeat/Cron

---

## 架构总览

```
┌─────────────┐     ┌──────────────┐     ┌──────────────────┐
│  Channels    │     │ Security     │     │  AI Providers    │
│  ─────────   │     │ ──────────   │     │  ─────────────   │
│  CLI (MVP)   │     │ 命令白名单    │     │  GLM 智谱        │
│  Telegram(P1)│     │ 路径沙箱      │     │  MiniMax         │
│  + Channel   │     │ 权限分级      │     │  DeepSeek        │
│    trait      │     │ (RO/Super/   │     │  Claude          │
│              │     │   Full)      │     │  GPT             │
└──────┬───────┘     └──────┬───────┘     │  + Provider trait │
       │                    │             └────────┬─────────┘
       ▼                    ▼                      ▼
┌──────────────────────────────────────────────────────────┐
│                      Agent Loop                          │
│  Message In → Memory Recall → LLM exec → Tools → Out    │
│  (max 10 iterations per turn)                            │
└──────────┬──────────────────────────────┬────────────────┘
           ▼                              ▼
┌──────────────────┐           ┌──────────────────┐
│  Memory          │           │  Tools           │
│  ──────          │           │  ─────           │
│  SQLite 存储      │           │  Shell 命令执行   │
│  tantivy 全文搜索 │           │  文件读写         │
│  jieba 中文分词   │           │  + Tool trait     │
│  + Memory trait  │           │                  │
└──────────────────┘           └──────────────────┘
```

## 核心 Trait 设计

### Provider trait — AI 模型抽象

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    async fn chat_with_tools(
        &self,
        messages: &[ConversationMessage],
        tools: &[ToolSpec],
        model: &str,
        temperature: f64,
    ) -> Result<ChatResponse>;
}
```

关联类型:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,     // "system" | "user" | "assistant"
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,                        // provider 生成的调用 ID
    pub name: String,                      // tool 名称
    pub arguments: serde_json::Value,      // tool 参数 JSON
}

#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub text: Option<String>,              // 文本回复（可能为空，只有 tool_calls）
    pub tool_calls: Vec<ToolCall>,         // 模型请求执行的工具列表
}

#[derive(Debug, Clone)]
pub enum ConversationMessage {
    Chat(ChatMessage),
    AssistantToolCalls {
        text: Option<String>,
        tool_calls: Vec<ToolCall>,
    },
    ToolResult {
        tool_call_id: String,
        content: String,                   // tool 执行结果
    },
}
```

实现:
- `CompatibleProvider` — 统一处理所有 OpenAI 兼容 API（GLM/MiniMax/DeepSeek/GPT），自动拼接 endpoint
- `ClaudeProvider` — Anthropic Messages API（x-api-key auth，system prompt 独立传递）

### Tool trait — 工具抽象

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> serde_json::Value;
    async fn execute(&self, args: serde_json::Value, policy: &SecurityPolicy) -> Result<ToolResult>;

    fn spec(&self) -> ToolSpec { /* 默认实现 */ }
}
```

关联类型:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,     // JSON Schema 格式
}
```

MVP 工具:
- `ShellTool` — 命令执行，受 SecurityPolicy 约束
- `FileReadTool` / `FileWriteTool` — 文件读写，受路径沙箱约束

### Memory trait — 记忆抽象

```rust
#[async_trait]
pub trait Memory: Send + Sync {
    async fn store(&self, key: &str, content: &str, category: MemoryCategory) -> Result<()>;
    async fn recall(&self, query: &str, limit: usize) -> Result<Vec<MemoryEntry>>;
    async fn forget(&self, key: &str) -> Result<bool>;
    async fn count(&self) -> Result<usize>;
}
```

关联类型:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MemoryCategory {
    Conversation,    // 对话历史
    Core,            // 核心知识/偏好
    Daily,           // 日常记录
    Custom(String),  // 自定义分类
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub key: String,
    pub content: String,
    pub category: MemoryCategory,
    pub created_at: String,       // ISO 8601
    pub updated_at: String,
    pub relevance_score: f32,     // recall() 返回时的相关性评分
}
```

MVP 实现: `SqliteMemory` — SQLite 结构化存储 + tantivy 全文搜索索引（jieba 中文分词 + BM25 排序）

### Channel trait — 消息通道抽象（预留扩展）

```rust
#[async_trait]
pub trait Channel: Send + Sync {
    fn name(&self) -> &str;
    async fn send(&self, message: &str, recipient: &str) -> Result<()>;
    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> Result<()>;
}
```

关联类型:

```rust
#[derive(Debug, Clone)]
pub struct ChannelMessage {
    pub id: String,           // 消息唯一 ID
    pub sender: String,       // 发送者标识
    pub content: String,      // 消息内容
    pub channel: String,      // 来源 channel 名称（用于路由回复）
    pub timestamp: u64,       // Unix 时间戳
}
```

MVP 实现: `CliChannel` — reedline 交互式 REPL

---

## 安全模型

```rust
pub enum AutonomyLevel {
    ReadOnly,    // 只读，不执行任何工具
    Supervised,  // 需用户确认后执行
    Full,        // 自主执行（企业内部可信环境）
}

pub struct SecurityPolicy {
    pub autonomy: AutonomyLevel,
    pub allowed_commands: Vec<String>,  // 命令白名单
    pub workspace_dir: PathBuf,         // 工作目录限制
    pub blocked_paths: Vec<PathBuf>,    // 禁止访问的路径
}
```

安全检查:
- `is_command_allowed()` — 检查命令是否在白名单中
- `is_path_allowed()` — 规范化路径 + workspace 范围检查 + symlink 防逃逸
- `requires_confirmation()` — Supervised 模式下返回 true

---

## Agent Loop 流程

```
1. 接收用户消息
2. Memory recall — 搜索相关历史上下文，注入 system prompt
3. 构造 messages + tool specs，调用 Provider
4. 解析响应:
   - 有 tool_calls → 逐个执行 tool（经 SecurityPolicy 检查）→ 结果推入 history → 回到 3
   - 无 tool_calls → 输出最终回复
5. Memory store — 保存本轮对话摘要
6. History 管理 — 保留最近 50 条消息
```

最大 tool call 迭代: 10 次/轮
Tool call 解析: 原生 JSON（OpenAI 格式）+ XML fallback

### System Prompt 构造

system prompt 按层拼接:

```
[1] 身份描述
    "你是 RRClaw，一个安全优先的 AI 助手。"

[2] 可用工具描述（自动生成）
    遍历 tools_registry，每个 tool 输出:
    - 名称、描述、参数 JSON Schema
    格式: "你可以使用以下工具:\n- shell: 执行命令...\n- file_read: ..."

[3] 安全规则
    当前 AutonomyLevel 下的行为约束:
    - Supervised: "执行工具前必须向用户展示命令并获得确认"
    - ReadOnly: "不要尝试执行任何工具"
    - Full: "你可以自主执行工具，但须遵守白名单限制"

[4] 记忆上下文（动态）
    Memory recall 返回的相关历史条目，格式:
    "[相关记忆]\n- {entry1.content}\n- {entry2.content}\n..."

[5] 当前环境信息
    - 工作目录、当前时间、OS 信息
```

---

## 技术选型

| 依赖 | 用途 | 版本 |
|------|------|------|
| `tokio` | 异步运行时 | 1.x |
| `reqwest` | HTTP 客户端（AI API 调用） | 0.12 |
| `serde` + `serde_json` | 序列化 | 1.x |
| `clap` | CLI 参数解析（derive） | 4.x |
| `rusqlite` | SQLite 结构化存储（bundled） | 0.32+ |
| `tantivy` | 全文搜索引擎（Rust 原生，替代 FTS5） | 0.22 |
| `jieba-rs` | 中文分词（配合 tantivy） | 0.7 |
| `figment` | 配置加载（TOML + 环境变量多层合并） | 0.10 |
| `color-eyre` + `thiserror` | 错误处理（彩色 span trace，CLI 友好） | latest |
| `async-trait` | 异步 trait 支持 | 0.1 |
| `tracing` + `tracing-subscriber` | 日志 | 0.1 |
| `reedline` | CLI 行编辑器（历史、补全、高亮、vi/emacs） | 0.37 |
| `directories` | 跨平台配置路径 | 5.x |
| `chrono` | 时间处理 | 0.4 |
| `uuid` | 唯一标识生成 | 1.x |

---

## 项目结构

```
rrclaw/
├── Claude.md                  # 总架构文档（本文件）
├── Cargo.toml
├── docs/
│   └── implementation-plan.md # 实现计划与提交策略
├── src/
│   ├── main.rs                # CLI 入口 (clap subcommands)
│   ├── lib.rs                 # 模块声明
│   ├── config/
│   │   ├── Claude.md          # Config 模块设计文档
│   │   ├── mod.rs             # Config::load_or_init() via figment
│   │   └── schema.rs          # Config / ProviderConfig / MemoryConfig / SecurityConfig
│   ├── providers/
│   │   ├── Claude.md          # Provider 模块设计文档
│   │   ├── mod.rs             # create_provider() 工厂函数
│   │   ├── traits.rs          # Provider trait + ChatMessage/ChatResponse/ToolCall
│   │   ├── compatible.rs      # OpenAI 兼容协议（GLM/MiniMax/DeepSeek/GPT）
│   │   └── claude.rs          # Anthropic Messages API
│   ├── agent/
│   │   ├── Claude.md          # Agent Loop 模块设计文档
│   │   ├── mod.rs             # agent::run() 入口
│   │   └── loop_.rs           # Tool call loop 核心循环
│   ├── channels/
│   │   ├── Claude.md          # Channel 模块设计文档
│   │   ├── mod.rs             # Channel trait + 消息分发
│   │   └── cli.rs             # CLI REPL 实现
│   ├── tools/
│   │   ├── Claude.md          # Tools 模块设计文档
│   │   ├── mod.rs             # Tool 注册表 + 工厂函数
│   │   ├── traits.rs          # Tool trait + ToolResult/ToolSpec
│   │   ├── shell.rs           # Shell 命令执行
│   │   └── file.rs            # 文件读写
│   ├── memory/
│   │   ├── Claude.md          # Memory 模块设计文档
│   │   ├── mod.rs             # create_memory() 工厂
│   │   ├── traits.rs          # Memory trait + MemoryEntry/MemoryCategory
│   │   └── sqlite.rs          # SQLite 存储 + tantivy 搜索索引
│   └── security/
│       ├── Claude.md          # Security 模块设计文档
│       ├── mod.rs             # 模块入口 + re-exports
│       └── policy.rs          # SecurityPolicy + AutonomyLevel
```

---

## 配置文件格式

```toml
# ~/.rrclaw/config.toml

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

---

## 开发规范

### 文档驱动开发
- 根目录 `Claude.md` 作为总架构文档
- 每个功能目录 `src/<module>/Claude.md` 作为子模块需求/设计文档
- **代码改动流程**: 先更新对应 `Claude.md` → 写/更新测试 → 改代码 → 跑通测试 → 提交

### 测试要求
- 每个模块必须有单元测试
- 每次代码改动必须先跑通所有测试
- 使用 mock 测试外部依赖（AI API、文件系统）

### Git 提交策略
- 原子化提交：每个提交只做一件事
- 最小化提交：尽量小的变更集
- 提交顺序：docs → trait → impl → test → fix/refactor

---

## 参考

- 架构参考: [ZeroClaw](https://github.com/zeroclaw-labs/zeroclaw) — Rust trait-based 可插拔 AI agent 架构，8 个核心 trait 设计
- 详细实现计划: [docs/implementation-plan.md](docs/implementation-plan.md)
- ZeroClaw 调研笔记: [docs/zeroclaw-reference.md](docs/zeroclaw-reference.md)
- Provider API 差异: [docs/provider-api-reference.md](docs/provider-api-reference.md)
- tantivy + jieba 集成: [docs/tantivy-integration.md](docs/tantivy-integration.md)
