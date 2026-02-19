# Agent 模块

## 职责

核心 Agent Loop — 接收用户消息，调度 Provider / Tools / Memory，返回回复。

## Agent Loop 流程

```
1. 接收用户消息
2. Memory recall — 搜索相关历史，注入 system prompt
3. 构造 messages + tool specs，调用 Provider
4. 解析响应:
   - 有 tool_calls → 逐个执行（SecurityPolicy 检查）→ 结果推入 history → 回到 3
   - 无 tool_calls → 输出最终回复
5. Memory store — 保存本轮对话摘要
6. History 管理 — 保留最近 50 条消息
```

## Agent 结构体

```rust
pub struct Agent {
    provider: Box<dyn Provider>,
    tools: Vec<Box<dyn Tool>>,
    memory: Box<dyn Memory>,
    policy: SecurityPolicy,
    model: String,
    temperature: f64,
    history: Vec<ConversationMessage>,
}
```

## System Prompt 构造（P2 精简版）

按层拼接（6 段，从旧版 7 段精简）:
1. 身份描述
2. 可用工具描述（自动生成，含 self_info）
3. 安全规则（按 AutonomyLevel）
4. 记忆上下文（Memory recall 结果，含知识种子）
5. 环境信息（仅工作目录 + 当前时间）
6. **决策原则**（替代旧版"行为准则"+"工具结果格式"）:
   - 先查后做: 不确定的信息先用 self_info 查询
   - 不知道就问: 查不到就问用户
   - 说明意图: 调用工具前说明原因
   - 失败时反思: 分析原因，第 2 次失败就问用户
   - 用中文回复

## 工具执行健壮性：代码 vs LLM 职责边界

**代码层负责**：不丢数据 + 安全 + 基础设施
**LLM 层负责**：理解结果 + 错误恢复 + 向用户解释

### execute_tool() 结果格式

| 状态 | 格式 | 说明 |
|------|------|------|
| 成功 | 直接返回输出内容 | 无前缀 |
| 失败 | `[失败] {error}` | 可能包含 `[部分输出]` 段 |
| 错误 | `[错误] {message}` | 系统级异常 |

关键设计：失败时保留 output（部分输出），不丢弃信息。LLM 通过 system prompt 中的 `[工具使用规则]` 学习如何处理各种结果。

### ToolStatus 流式事件

工具执行过程中向 TUI 发送实时状态：
- `Running(cmd)` — 开始执行
- `Success(summary)` — 成功，显示首行预览
- `Failed(err)` — 失败，显示前 3 行错误详情

### Supervised 模式确认流程

```
pre_validate() → 确认提示 [y/N/a] → execute()
                  ↑ 会话级自动批准（a 选项）
```

- `pre_validate()` 在确认前检查安全策略（ReadOnly 拒绝、Full 模式白名单）
- Supervised 模式用户确认即放行，不受白名单限制
- 会话级自动批准：按基础命令名跟踪（如 `cargo`），同一 session 内不重复询问

## 约束

- 最大 tool call 迭代: 10 次/轮
- History 保留: 最近 50 条消息
- Supervised 模式: tool 执行前需要确认（返回确认信息，由 Channel 层处理）
- shell 超时: 120 秒

## 接口

```rust
impl Agent {
    pub fn new(...) -> Self;
    pub fn set_confirm_fn(&mut self, f: ConfirmFn);
    pub async fn process_message(&mut self, user_msg: &str) -> Result<String>;
    pub async fn process_message_stream(&mut self, user_msg: &str, tx: Sender<StreamEvent>) -> Result<String>;
    pub fn history(&self) -> &[ConversationMessage];
    pub fn set_history(&mut self, history: Vec<ConversationMessage>);
}
```

`process_message` 是核心入口，`process_message_stream` 是流式版本（支持 ToolStatus 事件）。

## 文件结构

- `mod.rs` — re-exports + Agent struct
- `loop_.rs` — process_message 核心循环 + system prompt 构造
