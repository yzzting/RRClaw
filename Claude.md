# RRClaw

安全优先的 AI 助手基础设施，100% Rust，Trait 可插拔架构。

## 项目定位

面向个人助手和企业内部使用的 AI Agent CLI 工具。

**核心特性**:
- 多模型支持（GLM 智谱、MiniMax、DeepSeek、Claude、GPT）
- 安全沙箱（命令白名单、路径限制、权限分级）
- 持久化记忆（SQLite 存储 + tantivy 中文全文搜索）
- 工具执行（Shell、文件读写、Git、配置管理）
- Skills 系统（三级渐进加载，行为驱动）
- 斜杠命令（/help /new /clear /config /switch /apikey /skill）
- 可插拔架构（Trait 抽象，易于扩展）

**实现进度**:
- P0 ✅: CLI Channel + Agent Loop + 多模型 Provider + 基础 Tools + Security
- P1 ✅: 流式输出 + Supervised 确认 + History 持久化 + Setup 向导 + Telegram Channel
- P2 ✅: 斜杠命令（/help /new /clear /config /switch /apikey）+ ConfigTool
- P3 ✅: Skills 系统（三级加载）+ SkillTool + /skill CRUD 命令
- P4 🚧: Skill 驱动两阶段路由（最高优先级）+ GitTool ✅ + Memory Tools + ReliableProvider + History Compaction + MCP Client

---

## 架构总览

```
┌─────────────┐     ┌──────────────┐     ┌──────────────────┐
│  Channels    │     │ Security     │     │  AI Providers    │
│  ─────────   │     │ ──────────   │     │  ─────────────   │
│  CLI         │     │ 命令白名单    │     │  GLM 智谱        │
│  Telegram    │     │ 路径沙箱      │     │  MiniMax         │
│  + Channel   │     │ 权限分级      │     │  DeepSeek        │
│    trait      │     │ (RO/Super/   │     │  Claude          │
│              │     │   Full)      │     │  GPT             │
└──────┬───────┘     └──────┬───────┘     │  + Provider trait │
       │                    │             └────────┬─────────┘
       ▼                    ▼                      ▼
┌──────────────────────────────────────────────────────────┐
│                      Agent Loop                          │
│  Phase1:路由 → Phase2:执行 → Tool call loop → Out        │
│  (两阶段 Skill 路由，max 10 tool iterations/turn)         │
└───────────┬──────────────────────┬───────────────────────┘
            ▼                      ▼                      ▼
┌──────────────────┐  ┌──────────────────────┐  ┌──────────────────┐
│  Memory          │  │  Tools               │  │  Skills          │
│  ──────          │  │  ─────               │  │  ──────          │
│  SQLite 存储      │  │  Shell / File        │  │  L1 元数据目录    │
│  tantivy 全文搜索 │  │  Git / Config        │  │  L2 行为指南      │
│  jieba 中文分词   │  │  SelfInfo / Skill    │  │  内置 + 用户定义  │
│  + Memory trait  │  │  + Tool trait        │  │  驱动 Agent 行为  │
└──────────────────┘  └──────────────────────┘  └──────────────────┘
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
- `CompatibleProvider` — 统一处理所有 OpenAI 兼容 API（GLM/MiniMax/DeepSeek/GPT），自动拼接 endpoint，支持 SSE 流式
- `ClaudeProvider` — Anthropic Messages API（x-api-key auth，system prompt 独立传递）

### Tool trait — 工具抽象

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> serde_json::Value;
    async fn execute(&self, args: serde_json::Value, policy: &SecurityPolicy) -> Result<ToolResult>;

    /// 执行前预检，返回 Some(reason) 表示拒绝（在用户确认前调用，避免确认后被拒绝）
    fn pre_validate(&self, args: &serde_json::Value, policy: &SecurityPolicy) -> Option<String> {
        None
    }

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

已实现工具:
- `ShellTool` — 命令执行，受 SecurityPolicy 约束（白名单 + workspace 限制）
- `FileReadTool` / `FileWriteTool` — 文件读写，受路径沙箱约束
- `GitTool` — Git 版本控制（status/diff/log/add/commit/branch/checkout/push/pull/fetch），force push/checkout 安全拦截
- `ConfigTool` — AI 通过自然语言读写 config.toml（toml_edit 保留格式）
- `SelfInfoTool` — 返回 RRClaw 自身状态（版本、配置、路径、数据目录）
- `SkillTool` — 按需加载 skill L2 内容注入上下文（C 辅助路径，P3 已实现）

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

实现: `SqliteMemory` — SQLite 结构化存储 + tantivy 全文搜索索引（jieba 中文分词 + BM25 排序）+ conversation_history 表

### Channel trait — 消息通道抽象

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

已实现:
- `CliChannel` — reedline 交互式 REPL，支持 SSE 流式输出、thinking 动画、斜杠命令
- `TelegramChannel` — Telegram Bot（teloxide），支持多用户隔离会话

### Skills 系统

Skills 是 RRClaw 的行为驱动机制，将行为指南与核心代码解耦，支持用户自定义扩展。

#### 三级渐进加载

| 级别 | 内容 | 加载时机 |
|------|------|---------|
| L1 | 元数据（名称、描述、来源） | 启动时全部加载，注入 system prompt |
| L2 | 行为指南（精简指令，通常 < 500 字） | Phase 1 路由命中时按需加载 |
| L3 | 完整内容（详细说明、示例） | 用户显式 `/skill load` 时加载 |

#### 数据结构

```rust
pub struct SkillMeta {
    pub name: String,
    pub description: String,   // 包含触发场景提示，Phase 1 路由依赖此字段
    pub source: SkillSource,
    pub content_hash: Option<String>,
}

pub enum SkillSource {
    Builtin,                   // 编译期 include_str! 嵌入
    UserDefined(PathBuf),      // ~/.rrclaw/skills/{name}.md
}
```

#### 内置 Skills

- `git-workflow` — Git 操作工作流（提交规范、分支策略）
- `code-review` — 代码审查最佳实践
- `rust-dev` — Rust 开发规范（clippy、测试、错误处理）

用户可在 `~/.rrclaw/skills/` 下创建自定义 skill，格式：

```markdown
---
name: my-skill
description: 描述（包含触发场景，Phase 1 路由依赖此字段）
---
# Skill 内容
...
```

#### /skill 斜杠命令

| 命令 | 说明 |
|------|------|
| `/skill list` | 列出所有可用 skill |
| `/skill load <name>` | 加载 skill L3 完整内容到当前对话 |
| `/skill show <name>` | 查看 skill 内容 |
| `/skill new <name>` | 创建新的用户 skill |
| `/skill edit <name>` | 编辑现有 skill |
| `/skill delete <name>` | 删除用户 skill |

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
- `is_command_allowed()` — 检查命令是否在白名单中（仅 Full 模式强制）
- `is_path_allowed()` — 规范化路径 + workspace 范围检查 + symlink 防逃逸
- `requires_confirmation()` — Supervised 模式下返回 true
- `pre_validate()` — 工具执行前预检（在用户确认前调用，避免确认后被拒绝）

Supervised 模式安全策略:
- 用户确认 = 放行，不受白名单限制（用户是最终安全决策者）
- 支持会话级自动批准: `[y/N/a]` 中选 `a` 后同类命令自动放行
- Shell 按基础命令名跟踪（如 `cargo test`/`cargo build` 共享 `cargo` 批准）

---

## 日志系统

双层 tracing 架构，REPL 交互不受干扰，同时保留完整调试日志：

| 层 | 输出目标 | 默认级别 | 用途 |
|----|----------|----------|------|
| stderr | 终端 | `warn` | 运行时警告/错误，不干扰 REPL |
| 文件 | `~/.rrclaw/logs/rrclaw.log.YYYY-MM-DD` | `rrclaw=debug` | API 请求/响应、工具执行、agent loop 流程 |

日志文件按天滚动。可通过 `RUST_LOG` 环境变量覆盖文件日志级别：

```bash
# 查看完整请求体/响应体（含 API key 注意安全）
RUST_LOG=rrclaw=trace cargo run -- agent

# 查看日志
tail -f ~/.rrclaw/logs/rrclaw.log.*
```

关键日志点：
- `providers::compatible` — API 请求 URL/model、响应状态（debug），请求体/响应体（trace）
- `agent::loop_` — 每轮迭代编号、history 长度、响应摘要、工具执行参数和结果

---

## Agent Loop 流程

```
1. 接收用户消息
   - 斜杠命令（/help /new /clear /config /switch /apikey /skill）
     在 CLI 层直接处理，不进入 Agent Loop

2. Phase 1: 路由（P4-skill-routing 实施后生效）
   极简 system prompt（身份 + 安全约束 + Skill L1 目录）
   不传工具 schema，不传记忆上下文，temperature=0.1
   输出 RouteResult:
   - Skills(names)          → 加载对应 skill L2 内容，进入 Phase 2
   - Direct                 → 无需 skill，直接进入 Phase 2
   - NeedClarification(q)   → 返回澄清问题给用户，不执行任何工具
   Phase 1 失败时降级为 Direct，不阻断请求

3. Skill 注入（Phase 1 结果为 Skills 时）
   加载对应 skill L2 内容，存入 routed_skill_content（每轮重置）

4. Phase 2: 构造完整 system prompt + Memory recall
   [1] 身份描述
   [2] 可用工具描述（完整 schema）
   [2.5] 技能列表（L1 元数据，供 LLM 使用 SkillTool 自驱动）
   [3] 安全规则（AutonomyLevel 约束）
   [4] 记忆上下文（Memory recall 结果）
   [4.5] 已加载 skill 行为指南（Phase 1 路由结果）
   [5] 当前环境信息（工作目录、当前时间）
   [6] 工具结果格式 + 使用规则（LLM 兜底指南）

5. 调用 Provider（chat_with_tools）

6. 解析响应:
   - 有 tool_calls → 逐个执行 tool（经 SecurityPolicy 检查）
                  → 结果推入 history → 回到步骤 5
   - 无 tool_calls → 输出最终回复

7. Memory store — 保存本轮对话摘要

8. History 管理 — 保留最近 50 条消息
   （P4-history-compaction: 超出阈值时 LLM 自动摘要压缩替代硬截断）
```

最大 tool call 迭代: 10 次/轮
Tool call 解析: 原生 JSON（OpenAI 格式）+ XML fallback

### C 辅助路径（SkillTool 自驱动）

Phase 2 执行阶段，LLM 可自行调用 `SkillTool` 加载额外 skill 内容：
- Phase 1 未覆盖的模糊场景由此兜底
- SkillTool 返回内容作为 tool result，LLM 读取后按指南执行
- 无需额外代码，P3 已实现

---

## 技术选型

| 依赖 | 用途 | 版本 |
|------|------|------|
| `tokio` | 异步运行时 | 1.x |
| `reqwest` | HTTP 客户端（AI API 调用，含 SSE 流式） | 0.12 |
| `serde` + `serde_json` | 序列化 | 1.x |
| `clap` | CLI 参数解析（derive） | 4.x |
| `rusqlite` | SQLite 结构化存储（bundled） | 0.32+ |
| `tantivy` | 全文搜索引擎（Rust 原生，替代 FTS5） | 0.22 |
| `jieba-rs` | 中文分词（配合 tantivy） | 0.7 |
| `figment` | 配置加载（TOML + 环境变量多层合并） | 0.10 |
| `toml_edit` | 保留格式的 TOML 读写（ConfigTool） | 0.22 |
| `color-eyre` + `thiserror` | 错误处理（彩色 span trace，CLI 友好） | latest |
| `async-trait` | 异步 trait 支持 | 0.1 |
| `tracing` + `tracing-subscriber` + `tracing-appender` | 日志（双层：stderr warn + 文件 debug） | 0.1/0.2 |
| `reedline` | CLI 行编辑器（历史、补全、高亮、vi/emacs） | 0.37 |
| `teloxide` | Telegram Bot SDK | 0.13 |
| `dialoguer` | 交互式终端表单（setup 向导） | 0.11 |
| `shell-words` | 安全的命令行参数拆分（GitTool） | 1.x |
| `directories` | 跨平台配置路径 | 5.x |
| `chrono` | 时间处理 | 0.4 |
| `uuid` | 唯一标识生成 | 1.x |
| `tempfile` | 测试用临时文件/目录 | 3.x |

---

## 项目结构

```
rrclaw/
├── CLAUDE.md                  # 总架构文档（本文件）
├── Cargo.toml
├── docs/
│   ├── implementation-plan.md # 实现计划与提交策略
│   ├── p1-plan.md             # P1 实现计划
│   ├── p2-slash-commands-and-config-tool.md
│   ├── p3-skills.md           # P3 Skills 系统设计
│   ├── p4-skill-routing.md    # P4-0 两阶段路由（最高优先级）★
│   ├── p4-git-tool.md         # P4 GitTool 设计
│   ├── p4-memory-tools.md     # P4 Memory Tools 设计
│   ├── p4-reliable-provider.md # P4 ReliableProvider 设计
│   ├── p4-history-compaction.md # P4 History 压缩设计
│   └── p4-mcp-client.md       # P4 MCP Client 设计
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
│   │   ├── traits.rs          # Provider trait + ChatMessage/ChatResponse/ToolCall/ToolSpec
│   │   ├── compatible.rs      # OpenAI 兼容协议（GLM/MiniMax/DeepSeek/GPT，含 SSE 流式）
│   │   └── claude.rs          # Anthropic Messages API
│   ├── agent/
│   │   ├── Claude.md          # Agent Loop 模块设计文档
│   │   ├── mod.rs             # agent::run() 入口
│   │   └── loop_.rs           # 两阶段路由 + Tool call loop 核心循环
│   ├── channels/
│   │   ├── Claude.md          # Channel 模块设计文档
│   │   ├── mod.rs             # Channel trait + 消息分发
│   │   ├── cli.rs             # CLI REPL（reedline，流式，斜杠命令）
│   │   └── telegram.rs        # Telegram Bot（teloxide）
│   ├── tools/
│   │   ├── Claude.md          # Tools 模块设计文档
│   │   ├── mod.rs             # Tool 注册表 + create_tools() 工厂
│   │   ├── traits.rs          # Tool trait + ToolResult（ToolSpec 定义在 providers::traits）
│   │   ├── shell.rs           # Shell 命令执行
│   │   ├── file.rs            # 文件读写
│   │   ├── git.rs             # Git 版本控制（10 种操作 + 安全拦截）
│   │   ├── config.rs          # ConfigTool（toml_edit 读写）
│   │   ├── self_info.rs       # SelfInfoTool（RRClaw 自身状态）
│   │   └── skill.rs           # SkillTool（按需加载 skill L2 内容）
│   ├── memory/
│   │   ├── Claude.md          # Memory 模块设计文档
│   │   ├── mod.rs             # create_memory() 工厂
│   │   ├── traits.rs          # Memory trait + MemoryEntry/MemoryCategory
│   │   └── sqlite.rs          # SQLite 存储 + tantivy 搜索 + conversation_history 表
│   ├── skills/
│   │   ├── mod.rs             # SkillMeta/SkillSource/load_skills/builtin_skills/load_skill_content
│   │   └── builtin/           # 内置 skill 文件（include_str! 编译期嵌入）
│   │       ├── git-workflow.md
│   │       ├── code-review.md
│   │       └── rust-dev.md
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
allowed_commands = ["ls", "cat", "grep", "find", "echo", "pwd", "git", "head", "tail", "wc", "cargo", "rustc"]
workspace_only = true
```

---

## 开发规范

### 计划先行（强制）
**任何非 trivial 的功能开发，必须先写计划文档让用户审核，审核通过后再动代码。**

流程：
1. **写计划文档** — 在 `docs/` 下创建计划 markdown（如 `docs/p4-xxx.md`），包含：改动范围、设计方案、提交策略、验证方式
2. **提交计划文档** — `git commit` 计划文档
3. **等用户审核** — 明确告知用户"计划已写好，请审核"，等用户确认后再继续
4. **按计划实现** — 写测试 → 改代码 → 跑通测试 → 提交
5. **每完成一个原子步骤就提交** — 不要攒一堆改动最后才提交

什么算 trivial：单文件的小 bug fix、clippy 修复、文档 typo。其他都需要计划。

### 文档驱动开发
- 根目录 `CLAUDE.md` 作为总架构文档
- 每个功能目录 `src/<module>/Claude.md` 作为子模块需求/设计文档
- **代码改动流程**: 先更新对应 `Claude.md` → 写/更新测试 → 改代码 → 跑通测试 → 提交

### 新引入外部库：必须先做 Spike（强制）

凡是计划文档中依赖**新引入**的外部库（crate），在写设计方案之前必须先验证其核心 API 行为，结论写进计划文档。

Spike 要验证的内容：
- 初始化方式（构造即生效？还是需要显式 `.start()`？）
- 数据格式要求（如 cron 是几字段？字段顺序？）
- 错误处理方式（panic？Result？）
- 与我们已有架构的兼容性

**教训**（来自 `tokio-cron-scheduler`）：
- 未验证 scheduler 需要显式 `.start()` → 调度器创建了但从不触发
- 未验证 cron 格式是 6 字段（秒+标准5字段）而非标准 5 字段 → 所有时间表达式失效

### 实现决策必须显式写在计划文档里

计划文档不只写"做什么"，**有多种实现方案时，必须列出选项并注明选择理由**，让用户审核后再实现。禁止在实现阶段自行决策后不知会用户。

**教训**（来自 routines 自然语言解析）：
- 文档只写"支持自然语言时间输入"，我自行选择了正则解析
- 正则覆盖中文自然语言本就是错的方向，应选 LLM 解析
- 这类选择应在文档里写出"方案A：正则 / 方案B：LLM，选B，理由是……"

### 测试要求
- **每个功能必须有测试覆盖，无例外**。交互式 UI 需拆分纯逻辑函数，使其可测试
- 每次代码改动必须先跑通所有测试
- 使用 mock 测试外部依赖（AI API、文件系统）
- 禁止用"手动验证"替代自动化测试
- **涉及外部库调度/触发行为的功能，必须补充集成测试**（不可 mock 调度器本身，需验证真实触发）

**教训**（来自 routines）：单元测试 mock 了 scheduler，导致"scheduler 从不启动"和"cron 格式错误"两个 bug 完全漏网。

### 状态一致性规范（禁止"重启后生效"）

**任何用户触发的变更，必须在当前进程内立即对所有读取路径可见，禁止要求用户重启。**

本项目已经在两处掉入同一个坑：

| 案例 | 问题根因 | 修复方案 |
|------|----------|----------|
| `http_allowed_hosts` 用户同意后仍被拒 | `SecurityPolicy` 在调用时已经拷贝，后续 config 写入不可见 | `get_http_allowed_hosts()` 每次调用时实时读文件 |
| `/routine list` 创建后为空 | `persist_add_routine` 只写 DB，内存 `Vec<Routine>` 未更新 | 改为 `RwLock<Vec<Routine>>`，`persist_*` 同时更新内存 |

**设计时的检查清单（凡涉及"持久化 + 内存缓存"的结构，必须逐项确认）：**

1. `persist_add/delete/update()` 是否同时更新了内存中的缓存？
2. 读取方法（`list_*/get_*`）读的是内存缓存还是 DB？两者是否一致？
3. 如果用 `RwLock` / `Mutex` 包裹缓存，guard 是否在任何 `.await` 前已 drop？（禁止跨 await 持锁）
4. 对外暴露的方法，用户调用后能否在同一进程内立即看到变更结果？

**两种合规模式：**

- **无缓存模式**（简单）：每次读取直接查 DB 或文件，不维护内存副本。适合低频读取场景（如 `http_allowed_hosts`）。
- **双写模式**（高频读取）：`persist_*` 同时写 DB + 更新内存结构（用 `RwLock`），读取走内存。适合需要高性能列举的场景（如 `routines`）。

**不允许的模式**：只写 DB，读取走内存缓存，且不同步 —— 这会导致"重启后才生效"。

### Git 提交策略
- 原子化提交：每个提交只做一件事
- 最小化提交：尽量小的变更集
- 提交顺序：docs → trait → impl → test → fix/refactor
- 提交模版：feat，chore，docs，fix，refactor，test，使用英文 commit message
- **每完成一个原子步骤就立即提交，不要攒改动**

### Session 切换协议
当上下文即将满（>85%）时执行：
1. 更新 `~/.claude/projects/.../memory/MEMORY.md` 中的实现进度
2. 提示用户开启新 session
3. 新 session 会自动加载 MEMORY.md，读取本文件和 `docs/implementation-plan.md` 即可无缝衔接
4. 新 session 首句说"继续开发 RRClaw"即可

---

## 参考

- 架构参考: [ZeroClaw](https://github.com/zeroclaw-labs/zeroclaw) — Rust trait-based 可插拔 AI agent 架构，8 个核心 trait 设计
- 详细实现计划: [docs/implementation-plan.md](docs/implementation-plan.md)
- ZeroClaw 调研笔记: [docs/zeroclaw-reference.md](docs/zeroclaw-reference.md)
- Provider API 差异: [docs/provider-api-reference.md](docs/provider-api-reference.md)
- tantivy + jieba 集成: [docs/tantivy-integration.md](docs/tantivy-integration.md)
- P4 设计文档: [docs/p4-skill-routing.md](docs/p4-skill-routing.md)（最高优先级）
