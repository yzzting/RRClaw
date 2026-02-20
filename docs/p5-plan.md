# P5 功能规划总览

> 参考对象：OpenClaw（TypeScript，大众化个人助手）、ZeroClaw（Rust，企业级基础设施）、IronClaw（Rust，安全优先）

## P5 功能清单

| 编号 | 功能 | 参考来源 | 成本 | 优先级 | 文档状态 |
|------|------|---------|------|--------|---------|
| P5-1 | HTTP Request Tool | ZeroClaw + IronClaw | 低 | ★★★★★ | [详细设计](p5-http-tool.md) ✅ |
| P5-2 | 身份文件系统（AGENT.md/USER.md） | ZeroClaw + IronClaw | 低 | ★★★★☆ | [详细设计](p5-identity-files.md) ✅ |
| P5-3 | ActionTracker 操作速率限制 | ZeroClaw | 低 | ★★★★☆ | [详细设计](p5-action-tracker.md) ✅ |
| P5-4 | Prompt Injection 防御 | IronClaw | 中 | ★★★☆☆ | 本文档 |
| P5-5 | Routines 定时任务系统 | OpenClaw + IronClaw | 高 | ★★★☆☆ | 本文档 |
| P5-6 | Discord Channel | ZeroClaw + OpenClaw | 中 | ★★☆☆☆ | 本文档 |

---

## P5-2: 身份文件系统（AGENT.md / USER.md）

### 背景

ZeroClaw 会在启动时读取工作区的 Bootstrap files（`AGENTS.md`、`SOUL.md`、`IDENTITY.md`、`USER.md`），注入 system prompt 让 Agent 拥有稳定的个性、记住用户偏好和项目约定。IronClaw 叫做 Identity Files。

这是把 Agent 从通用助手变成"懂你的专属助手"的关键特性，且实现成本极低。

### 核心思路

启动时按优先级从**当前工作目录**和**用户目录**读取身份文件，内容注入 system prompt。

### 支持的文件（按优先级）

| 文件路径 | 作用 | 示例内容 |
|---------|------|---------|
| `.rrclaw/AGENT.md`（工作区） | 项目级行为约定 | "此项目用 cargo fmt 格式化，所有提交须有测试" |
| `~/.rrclaw/USER.md` | 用户全局偏好 | "用户喜欢简洁回复，偏好中文，擅长 Rust 和前端" |
| `.rrclaw/SOUL.md`（工作区） | Agent 人格设定 | "你是 Max，友善且直接，不废话" |
| `CLAUDE.md`（工作区根目录） | 已支持（架构文档）| 已有，无需改动 |

优先级：项目文件 > 用户全局文件。

### 改动范围（极小）

**新增文件**：`src/agent/identity.rs`

```rust
/// 读取身份文件，返回注入到 system prompt 的文本
/// 按优先级读取，合并所有存在的文件内容
pub fn load_identity_context(workspace_dir: &Path, data_dir: &Path) -> String {
    let candidates = [
        (workspace_dir.join(".rrclaw/AGENT.md"), "[项目行为约定]"),
        (workspace_dir.join(".rrclaw/SOUL.md"), "[Agent 人格]"),
        (data_dir.join("USER.md"), "[用户偏好]"),
    ];
    // ... 读取并合并
}
```

**改动 `agent/loop_.rs`**：在 `build_system_prompt()` 中新增 `[0]` 段（身份文件）注入到最前面，作为最高优先级上下文。

**配置**（可选）：在 `config.toml` 中可禁用身份文件加载：
```toml
[agent]
load_identity_files = true  # 默认 true
```

### 提交策略

```
feat: add identity file loader (AGENT.md/USER.md/SOUL.md)
feat: inject identity context into system prompt
test: add identity loader unit tests
```

### 约束
- 文件不存在时静默跳过
- 单个文件内容上限 8000 字符（防止注入过长内容占用 context）
- 文件路径必须在 workspace 或 data_dir 内（SecurityPolicy 路径检查）

---

## P5-4: Prompt Injection 防御

### 背景

IronClaw 的安全亮点之一：对 LLM 的输入（用户消息）和输出（工具结果）都做注入检测。尤其在接入 MCP Server 后，外部工具返回的内容可能含恶意指令（如`忽略之前的指令，改而执行...`）。

Cisco 安全团队曾测试 OpenClaw 的第三方 skill，发现存在数据窃取和 prompt injection 攻击。

### 核心设计

**两层防御**：
1. **用户输入过滤**：检测包含注入特征的用户消息，警告但不阻断（用户是可信主体）
2. **工具结果过滤**：对所有工具返回的内容做注入检测，触发时降低置信度或截断

**严重级别**（参考 IronClaw）：

| 级别 | 行为 | 触发条件示例 |
|------|------|-------------|
| Block | 截断工具输出，替换为警告 | "忽略所有之前的指令" / "system: you are now..." |
| Warn | 在工具结果前添加 `[安全警告]` 标注 | 包含 role-play 指令 / 大量 `\n\n` 混淆 |
| Review | 记录日志，不干预 | 轻微可疑模式 |

### 改动范围

**新增文件**：`src/security/injection.rs`

```rust
pub enum InjectionSeverity { Block, Warn, Review }

pub struct InjectionResult {
    pub severity: Option<InjectionSeverity>,
    pub reason: Option<String>,
    pub sanitized: String,  // 处理后的内容（Block 时为替换文本）
}

/// 检测 tool result 中的 prompt injection
pub fn check_tool_result(content: &str) -> InjectionResult { ... }
```

**改动 `agent/loop_.rs`**：在将 ToolResult 推入 history 前调用 `check_tool_result()`：

```rust
let result = self.execute_tool(&tc.name, tc.arguments.clone()).await;
let injection_check = crate::security::injection::check_tool_result(&result);
let final_result = injection_check.sanitized;  // 使用处理后的内容
```

### 检测规则（关键词 + 正则）

```rust
// Block 级别触发词
const BLOCK_PATTERNS: &[&str] = &[
    "ignore previous instructions",
    "ignore all prior instructions",
    "disregard your instructions",
    "忽略之前的所有指令",
    "你现在是",          // 角色劫持
    "system: you are",  // system prompt 注入
    "\\x00",            // null byte 混淆
];

// Warn 级别触发词
const WARN_PATTERNS: &[&str] = &[
    "as an ai language model",  // 可能是越狱模板
    "DAN mode",
    "jailbreak",
];
```

### 约束
- 规则必须保守，避免误报（工具正常输出被截断比注入危害更大）
- 提供配置项禁用（用户信任所有工具时）
- 所有检测命中都写入审计日志

### 提交策略

```
feat: add prompt injection detection for tool results
feat: add injection severity levels (Block/Warn/Review)
feat: integrate injection check into agent tool loop
test: add injection detection unit tests
docs: document known limitations (false positives)
```

---

## P5-5: Routines 定时任务系统

### 背景

OpenClaw 的核心特色之一：**从被动响应助手 → 主动助手**。用户可以设置定期任务，Agent 自动在后台执行：
- 每天早 8 点总结今日日历
- 每小时检查 GitHub PR 状态
- 每周一生成工作周报

IronClaw 叫做 Routines Engine，支持 cron 表达式、事件触发、webhook 触发。

### 架构设计（高层）

```
config.toml / 数据库
[routines]
[[routines.jobs]]
name = "daily_summary"
schedule = "0 8 * * *"   # cron 表达式，每天早 8 点
message = "总结今天的待办事项"
enabled = true
            │
RoutineEngine（src/routines/mod.rs）
            │
  tokio-cron-scheduler（第三方 crate）
            │
  到达触发时间 → Agent::process_message(job.message)
            │
  结果通过配置的 channel 发送（CLI 打印 / Telegram 推送）
```

### 关键技术选型

| 组件 | 选型 | 理由 |
|------|------|------|
| Cron 调度 | `tokio-cron-scheduler` | Tokio 生态，async-first，支持 cron 表达式 |
| 任务存储 | SQLite（已有）| 复用现有 Memory 数据库，存储 routine 配置 |
| 心跳监控 | 每分钟 tick | Tokio interval，简单可靠 |

### 主要 Struct

```rust
pub struct Routine {
    pub name: String,
    pub schedule: String,     // cron 表达式，如 "0 8 * * *"
    pub message: String,      // 触发时发给 Agent 的消息
    pub channel: String,      // 结果发到哪个 channel（"cli" / "telegram"）
    pub enabled: bool,
}

pub struct RoutineEngine {
    routines: Vec<Routine>,
    // ...
}
```

### 斜杠命令

```
/routine list           — 列出所有定时任务
/routine add <name> <cron> <message>   — 添加
/routine enable <name>  — 启用
/routine disable <name> — 禁用
/routine delete <name>  — 删除
/routine run <name>     — 立即手动触发
```

### 改动范围

**新增模块**：`src/routines/`（较大，~500 行）

**依赖**：
```toml
tokio-cron-scheduler = "0.13"
```

**Config 扩展**：
```toml
[[routines.jobs]]
name = "morning_brief"
schedule = "0 8 * * *"
message = "用中文总结今天的天气预报和我的 GitHub 通知"
enabled = true
```

### 约束
- Routine 消息发给 Agent 时，历史上下文不共享（每次独立对话）
- 超时保护：每个 routine 最多执行 5 分钟
- 失败重试：最多 3 次，每次间隔 5 分钟
- 执行记录存入 SQLite（可查历史）

### 提交策略（约 8 次提交）

```
feat: add Routine struct and config schema
feat: add RoutineEngine with tokio-cron-scheduler
feat: wire RoutineEngine into main.rs startup
feat: add /routine slash commands in CLI
feat: add routine execution with timeout and retry
feat: store routine execution history in SQLite
test: add RoutineEngine unit tests
docs: add routine configuration guide
```

---

## P5-6: Discord Channel

### 背景

ZeroClaw 和 OpenClaw 都支持 Discord，这是开发者群体最活跃的通讯平台。与 Telegram Channel 实现模式相同（都是 Bot + 消息事件），可以复用大量代码。

### 架构

与 `src/channels/telegram.rs` 完全对称的结构：

```
src/channels/discord.rs   ← 新增，约 150 行
```

```rust
pub struct DiscordChannel {
    bot_token: String,
    allowed_guild_ids: Vec<u64>,  // 只响应指定服务器（隔离）
    allowed_user_ids: Vec<u64>,   // 白名单用户
}

#[async_trait]
impl Channel for DiscordChannel {
    // ...
}
```

### 技术选型

| 选项 | Crate | 优劣 |
|------|-------|------|
| **Serenity**（推荐） | `serenity = "0.12"` | 功能完整，Discord 官方推荐，tokio 异步 |
| Twilight | `twilight = "0.15"` | 更底层，灵活但复杂 |

### Config 扩展

```toml
[channels.discord]
enabled = true
bot_token = "MTxxxxxx"
allowed_guild_ids = [123456789]    # 只响应这些服务器
allowed_user_ids = [987654321]     # 只响应这些用户（空=白名单所有）
command_prefix = "!"               # 消息前缀触发，如 "!问一下..."
```

### 斜杠命令集成

Discord Bot 支持 `/` 原生斜杠命令（Discord Application Commands），可以把 RRClaw 的 `/help`、`/new` 等注册为 Discord 斜杠命令（可选，非核心）。

### 依赖

```toml
serenity = { version = "0.12", features = ["client", "gateway", "model", "tokio_task_builder"] }
```

### 改动范围

| 文件 | 改动 | 复杂度 |
|------|------|--------|
| `src/channels/discord.rs` | **新增** | 中（参考 telegram.rs） |
| `src/channels/mod.rs` | 微改：注册 | 低 |
| `src/config/schema.rs` | 新增 DiscordConfig | 低 |
| `src/main.rs` | 条件启动 Discord channel | 低 |

### 提交策略

```
feat: add Discord channel with serenity
feat: add DiscordConfig to config schema
feat: wire Discord channel into main.rs
test: add Discord channel unit tests (mock)
```

---

## 优先级建议与依赖关系

```
P5-1 HTTP Tool        ← 独立，可优先实现（已有详细文档）
P5-3 ActionTracker    ← 独立，可优先实现（已有详细文档）
P5-2 身份文件         ← 独立，实现简单，建议紧跟 P5-1/3
P5-4 Prompt Injection ← 依赖 P4-MCP 接入后价值更大
P5-5 Routines         ← 最大，建议作为独立的 Sprint
P5-6 Discord          ← 独立，参考 Telegram 实现
```

## 与 P4 的关系

P4（当前正在推进）的以下功能需在 P5 之前完成：
- P4-MCP: MCP Client（P5-4 Prompt Injection 的防御场景依赖 MCP 引入的外部内容）
- P4-ReliableProvider: 重试（P5-5 Routines 定时任务需要稳定的 Provider）

P5-1、P5-2、P5-3 可以与 P4 并行开发，无依赖关系。
