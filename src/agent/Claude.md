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

## System Prompt 构造

按层拼接:
1. 身份描述
2. 可用工具描述（自动生成）
3. 安全规则（按 AutonomyLevel）
4. 记忆上下文（Memory recall 结果）
5. 环境信息

## 约束

- 最大 tool call 迭代: 10 次/轮
- History 保留: 最近 50 条消息
- Supervised 模式: tool 执行前需要确认（返回确认信息，由 Channel 层处理）

## 接口

```rust
impl Agent {
    pub fn new(...) -> Self;
    pub async fn process_message(&mut self, user_msg: &str) -> Result<String>;
}
```

`process_message` 是核心入口，返回 AI 最终回复文本。

## 文件结构

- `mod.rs` — re-exports + Agent struct
- `loop_.rs` — process_message 核心循环 + system prompt 构造
