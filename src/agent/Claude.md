# Agent 模块设计文档

核心 Agent Loop — 接收用户消息，调度 Provider / Tools / Memory，返回回复。

## Agent 结构体

```rust
pub struct Agent {
    provider: Box<dyn Provider>,
    tools: Vec<Box<dyn Tool>>,
    memory: Arc<dyn Memory>,
    policy: SecurityPolicy,
    model: String,
    temperature: f64,
    history: Vec<ConversationMessage>,
    confirm_fn: Option<ConfirmFn>,
    skills_meta: Vec<SkillMeta>,
    routed_skill_content: Option<String>,  // Phase 1 路由结果，每轮重置
    identity_context: Option<String>,      // USER.md/SOUL.md/AGENT.md 内容
    routine_name: Option<String>,          // 由 RoutineEngine 设置
}
```

## Agent Loop 流程（两阶段路由）

```
1. 接收用户消息
   斜杠命令在 CLI 层直接处理，不进入 Agent Loop

2. Phase 1：路由
   极简 system prompt（身份 + 安全 + Skill L1 目录）
   不传工具 schema，不传记忆上下文，temperature=0.1
   传入最近 N 条对话历史（提供上下文，避免路由误判）
   输出 RouteResult:
   - Skills(names)        → 加载 skill L2 内容，进入 Phase 2
   - Direct               → 直接进入 Phase 2
   - NeedClarification(q) → 通过 tx 发送澄清问题给用户，不执行工具
   Phase 1 失败时降级为 Direct

3. Phase 2：构造完整 system prompt
   [1] 身份描述（含 identity_context）
   [2] 可用工具描述（完整 schema）
   [2.5] 技能列表（L1 元数据）
   [3] 安全规则（AutonomyLevel 约束）
   [4] 记忆上下文（Memory recall）
   [4.5] 已路由的 skill 行为指南
   [5] 环境信息（工作目录 + 当前时间）
   [6] 决策原则（先查后做 / 失败反思等）
   若是 Routine 任务，追加 [Routine 执行规范] 段

4. 调用 Provider（chat_with_tools）

5. 解析响应：
   有 tool_calls → 逐个执行 → 注入检测 → 结果推入 history → 回到 4
   无 tool_calls → 输出最终回复

6. Memory store — 保存本轮对话摘要

7. History 管理 — 保留最近 50 条消息
```

## Prompt Injection 检测（P4）

工具执行结果在推入 history 前经过 `check_tool_result()` 检测：

- `Block`：高危，结果替换为警告文本
- `Review`：疑似，记录 WARN 日志，内容通过
- `Warn`：轻微，记录 INFO 日志，内容通过

**只检测外部数据工具**（`needs_injection_check(tool_name)`）：
- 检测：`shell`, `file_read`, `file_write`, `git`, `http_request`
- 跳过：`memory_*`, `skill`, `self_info`, `config`, `routine`

跳过内部工具的原因：memory_recall 返回格式化记忆列表，行数多，会误触发空行比例检查。

## reasoning_content 处理（DeepSeek/GLM 思维链）

部分 Provider（DeepSeek Reasoner、GLM）返回 `reasoning_content` 独立思维链字段：

- `ChatMessage` 和 `AssistantToolCalls` 都有 `reasoning_content: Option<String>` 字段
- history 中保留 reasoning_content（多轮对话需原样传回给 Provider）
- 每轮开始时清空上一轮的 reasoning_content（`clear_old_reasoning_content()`），节省 token

## Supervised 模式确认流程

```
pre_validate() → 拒绝 → 返回拒绝原因（不走确认）
              → 通过 → confirm_fn() [y/N/a] → execute()
```

- `pre_validate()` 在确认前检查安全策略
- Supervised 模式用户确认即放行，不受白名单限制（用户是最终安全决策者）
- 会话级自动批准（`a` 选项）：按基础命令名跟踪，同一 session 内不重复询问

## 关键接口

```rust
impl Agent {
    pub async fn process_message(&mut self, user_msg: &str) -> Result<String>;
    pub async fn process_message_stream(&mut self, user_msg: &str, tx: Sender<StreamEvent>) -> Result<String>;
    pub fn set_confirm_fn(&mut self, f: ConfirmFn);
    pub fn set_history(&mut self, history: Vec<ConversationMessage>);
    pub fn set_routine_name(&mut self, name: String);      // RoutineEngine 调用
    pub fn inject_skill_context(&mut self, content: String);
    pub fn inject_identity_context(&mut self, content: String);
}
```

`process_message` 用于 Routine 后台任务（无流式）；
`process_message_stream` 用于 CLI REPL（实时流式输出 + ToolStatus 事件）。

## 工具执行结果格式

| 状态 | 格式 |
|------|------|
| 成功 | 直接返回输出内容 |
| 失败 | `[失败] {error}`（可能含 `[部分输出]`） |
| 错误 | `[错误] {message}` |

## 约束

- 最大 tool call 迭代：10 次/轮
- History 保留：最近 50 条消息
- Shell 超时：120 秒

## 文件结构

```
src/agent/
├── Claude.md   # 本文件
├── mod.rs      # re-exports + Agent struct + 接口方法
└── loop_.rs    # process_message 核心循环 + system prompt 构造 + injection 检测
```
