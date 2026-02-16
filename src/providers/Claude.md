# Providers 模块

## 职责

统一抽象多个 AI 模型 API，通过 `Provider` trait 使上层 Agent Loop 与具体 Provider 解耦。

## Provider trait

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

## 关联类型

```rust
ChatMessage { role: String, content: String }
ToolCall { id: String, name: String, arguments: serde_json::Value }
ChatResponse { text: Option<String>, tool_calls: Vec<ToolCall> }

ConversationMessage:
  - Chat(ChatMessage)
  - AssistantToolCalls { text: Option<String>, tool_calls: Vec<ToolCall> }
  - ToolResult { tool_call_id: String, content: String }

ToolSpec { name: String, description: String, parameters: serde_json::Value }
```

## 实现

### CompatibleProvider

处理所有 OpenAI 兼容 API（GLM/MiniMax/DeepSeek/GPT）。

- Endpoint: `{base_url}/chat/completions`
- Auth: `Authorization: Bearer {api_key}`
- 请求/响应格式统一，差异仅在 base_url 和 model

### ClaudeProvider

Anthropic Messages API，独立实现。

- Endpoint: `{base_url}/v1/messages`
- Auth: `x-api-key: {api_key}` + `anthropic-version: 2023-06-01`
- System prompt 独立于 messages
- Tool 定义使用 `input_schema`（不是 `parameters`）
- content 是数组格式（可混合 text + tool_use）
- `max_tokens` 必填

### 转换逻辑（ClaudeProvider）

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

- `mod.rs` — re-exports + `create_provider()` 工厂
- `traits.rs` — Provider trait + 所有关联类型
- `compatible.rs` — CompatibleProvider
- `claude.rs` — ClaudeProvider
