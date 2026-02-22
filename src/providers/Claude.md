# Providers 模块设计文档

统一抽象多个 AI 模型 API，通过 `Provider` trait 使上层 Agent Loop 与具体 Provider 解耦。

## Provider trait

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    /// 非流式调用（返回完整响应）
    async fn chat_with_tools(
        &self,
        messages: &[ConversationMessage],
        tools: &[ToolSpec],
        model: &str,
        temperature: f64,
    ) -> Result<ChatResponse>;

    /// 流式调用（逐步发送 StreamEvent，最终返回完整响应）
    /// 默认实现：回退到 chat_with_tools，将完整文本作为单次 Text 事件发送
    async fn chat_stream(
        &self,
        messages: &[ConversationMessage],
        tools: &[ToolSpec],
        model: &str,
        temperature: f64,
        tx: mpsc::Sender<StreamEvent>,
    ) -> Result<ChatResponse>;
}
```

## 关联类型

```rust
ChatMessage {
    role: String,               // "system" | "user" | "assistant"
    content: String,
    reasoning_content: Option<String>,  // DeepSeek/MiniMax 思考内容（多轮 tool call 需原样回传）
}

ToolCall { id: String, name: String, arguments: serde_json::Value }

ChatResponse {
    text: Option<String>,
    reasoning_content: Option<String>,  // DeepSeek/MiniMax 思考内容
    tool_calls: Vec<ToolCall>,
}

ConversationMessage:
  - Chat(ChatMessage)
  - AssistantToolCalls {
        text: Option<String>,
        reasoning_content: Option<String>,   // 多轮 tool call 时需原样回传
        tool_calls: Vec<ToolCall>,
    }
  - ToolResult { tool_call_id: String, content: String }

ToolSpec { name: String, description: String, parameters: serde_json::Value }
```

### reasoning_content 处理规则

DeepSeek-Reasoner 等思维模型在响应中会包含 `reasoning_content`（推理过程）：
- 第一轮 tool call 前：从 `ChatResponse` 提取 `reasoning_content`
- 构造 `AssistantToolCalls` 消息时：将 `reasoning_content` 原样写入
- 下一轮请求时：`AssistantToolCalls.reasoning_content` 原样传回 API
- **注意**：当前实现中多轮 tool call 传入空字符串有 bug，详见 `loop_.rs`

## 流式输出类型

```rust
pub enum StreamEvent {
    Text(String),              // 文本 token 增量
    ToolCallDelta {            // tool call 增量（各字段分步到达）
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments_delta: String,
    },
    ToolStatus {               // 工具执行状态（TUI 显示用）
        name: String,
        status: ToolStatusKind,
    },
    Thinking,                  // LLM 思考中（等待首个 token，用于 spinner）
    Done(ChatResponse),        // 流结束，完整响应
}

pub enum ToolStatusKind {
    Running(String),   // 开始执行（命令预览）
    Success(String),   // 执行成功（输出摘要）
    Failed(String),    // 执行失败（错误信息）
}
```

## 实现

### CompatibleProvider

处理所有 OpenAI 兼容 API（GLM/MiniMax/DeepSeek/GPT）。

- **Endpoint**: `{base_url}/chat/completions`
- **Auth**: `Authorization: Bearer {api_key}`
- **流式**: `stream: true` + SSE（`text/event-stream`），解析 `data: {...}` 行
- **SSE 增量解析**:
  - `choices[0].delta.content` → `Text` 事件
  - `choices[0].delta.tool_calls[i]` → `ToolCallDelta` 事件（多个 delta 拼接完整 JSON）
  - `data: [DONE]` → 触发 `Done` 事件

### ClaudeProvider

Anthropic Messages API，独立实现。

- **Endpoint**: `{base_url}/v1/messages`
- **Auth**: `x-api-key: {api_key}` + `anthropic-version: 2023-06-01`
- **system prompt**: 独立于 messages 数组，顶层 `system` 字段
- **Tool 定义**: 使用 `input_schema`（不是 `parameters`）
- **content 格式**: 数组（可混合 `text` + `tool_use`）
- **`max_tokens`**: 必填

#### 转换逻辑（ClaudeProvider）

1. 从 messages 提取 system 消息合并到顶层 `system` 字段
2. `AssistantToolCalls` → content 数组 `[{type:"text"}, {type:"tool_use"}]`
3. `ToolResult` → role=user, content `[{type:"tool_result"}]`
4. `ToolSpec.parameters` → 改名 `input_schema`
5. 响应: 遍历 content[]，text 拼接，tool_use 收集为 ToolCall

## 工厂函数

```rust
pub fn create_provider(config: &ProviderConfig) -> Box<dyn Provider>
```

根据 `auth_style` 判断：
- `Some("x-api-key")` → `ClaudeProvider`
- 其他 → `CompatibleProvider`

## 文件结构

```
src/providers/
├── Claude.md      # 本文件
├── mod.rs         # re-exports + create_provider() 工厂
├── traits.rs      # Provider trait + 所有关联类型 + StreamEvent
├── compatible.rs  # CompatibleProvider（含 SSE 流式）
└── claude.rs      # ClaudeProvider（Anthropic Messages API）
```

## 测试要求

- `CompatibleProvider`：构造请求体（含 tools）、解析响应（text + tool_calls）
- `ClaudeProvider`：system 提取、AssistantToolCalls 转换、ToolResult 转换、input_schema 改名
- reasoning_content 回传（多轮 tool call 时 AssistantToolCalls 字段正确）
