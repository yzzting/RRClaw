# Provider API 差异参考

所有 Provider 都实现同一个 `Provider` trait，但底层 HTTP 请求格式有差异。

## 分类

| Provider | 协议 | 实现 |
|----------|------|------|
| GLM 智谱 | OpenAI 兼容 | `CompatibleProvider` |
| MiniMax | OpenAI 兼容 | `CompatibleProvider` |
| DeepSeek | OpenAI 兼容 | `CompatibleProvider` |
| GPT | OpenAI 原生 | `CompatibleProvider` |
| Claude | Anthropic Messages API | `ClaudeProvider` |

## CompatibleProvider — OpenAI 兼容协议

### 通用请求格式

```
POST {base_url}/chat/completions
Authorization: Bearer {api_key}
Content-Type: application/json

{
  "model": "model-name",
  "messages": [
    {"role": "system", "content": "..."},
    {"role": "user", "content": "..."},
    {"role": "assistant", "content": "...", "tool_calls": [...]},
    {"role": "tool", "tool_call_id": "...", "content": "..."}
  ],
  "tools": [{
    "type": "function",
    "function": {
      "name": "tool_name",
      "description": "...",
      "parameters": { /* JSON Schema */ }
    }
  }],
  "temperature": 0.7
}
```

### 通用响应格式

```json
{
  "id": "...",
  "choices": [{
    "message": {
      "role": "assistant",
      "content": "文本回复",
      "tool_calls": [{
        "id": "call_xxx",
        "type": "function",
        "function": {
          "name": "tool_name",
          "arguments": "{\"key\": \"value\"}"
        }
      }]
    },
    "finish_reason": "stop" | "tool_calls"
  }],
  "usage": {
    "prompt_tokens": 100,
    "completion_tokens": 50,
    "total_tokens": 150
  }
}
```

### 各 Provider 差异

#### GLM 智谱

| 项目 | 值 |
|------|-----|
| Base URL | `https://open.bigmodel.cn/api/paas/v4` |
| Endpoint | `{base_url}/chat/completions` |
| Auth | `Authorization: Bearer {api_key}` |
| 模型 | `glm-4-flash`, `glm-4-plus`, `glm-5` |
| Tool Calling | 支持，最多 128 个 function |
| 特殊参数 | `thinking` (深度思考模式，可选) |
| 注意 | 完全兼容 OpenAI SDK 格式，直接改 base_url 即可 |

> 参考: https://docs.bigmodel.cn/api-reference/

#### MiniMax

| 项目 | 值 |
|------|-----|
| Base URL | `https://api.minimax.io/v1` |
| Endpoint | `{base_url}/chat/completions` |
| Auth | `Authorization: Bearer {api_key}` |
| 模型 | `MiniMax-M2`, `MiniMax-M2.1`, `MiniMax-M2.5` |
| Tool Calling | 支持 |
| 特殊参数 | `reasoning_split=true` (推理过程分离) |
| 注意 | 部分 OpenAI 参数被忽略（presence_penalty, frequency_penalty, logit_bias）|

> 参考: https://platform.minimax.io/docs/api-reference/text-openai-api

#### DeepSeek

| 项目 | 值 |
|------|-----|
| Base URL | `https://api.deepseek.com/v1` |
| Endpoint | `{base_url}/chat/completions` |
| Auth | `Authorization: Bearer {api_key}` |
| 模型 | `deepseek-chat`, `deepseek-reasoner` |
| Tool Calling | 支持 |
| 注意 | 标准 OpenAI 兼容，无特殊差异 |

> 参考: https://api-docs.deepseek.com/

#### GPT (OpenAI)

| 项目 | 值 |
|------|-----|
| Base URL | `https://api.openai.com/v1` |
| Endpoint | `{base_url}/chat/completions` |
| Auth | `Authorization: Bearer {api_key}` |
| 模型 | `gpt-4o`, `gpt-4o-mini`, `o1`, `o3-mini` |
| Tool Calling | 原生支持 |
| 注意 | 标准格式，其他 Provider 都是兼容它 |

> 参考: https://platform.openai.com/docs/api-reference/chat

---

## ClaudeProvider — Anthropic Messages API

Claude 使用完全不同的 API 格式，需要独立实现。

### 请求格式

```
POST https://api.anthropic.com/v1/messages
x-api-key: {api_key}
anthropic-version: 2023-06-01
Content-Type: application/json

{
  "model": "claude-sonnet-4-5-20250929",
  "max_tokens": 8192,
  "system": "system prompt 在这里，不在 messages 里",
  "messages": [
    {"role": "user", "content": "..."},
    {"role": "assistant", "content": [
      {"type": "text", "text": "..."},
      {"type": "tool_use", "id": "toolu_xxx", "name": "tool_name", "input": {...}}
    ]},
    {"role": "user", "content": [
      {"type": "tool_result", "tool_use_id": "toolu_xxx", "content": "..."}
    ]}
  ],
  "tools": [{
    "name": "tool_name",
    "description": "...",
    "input_schema": { /* JSON Schema */ }
  }]
}
```

### 响应格式

```json
{
  "id": "msg_xxx",
  "type": "message",
  "role": "assistant",
  "content": [
    {"type": "text", "text": "文本回复"},
    {"type": "tool_use", "id": "toolu_xxx", "name": "tool_name", "input": {...}}
  ],
  "stop_reason": "end_turn" | "tool_use",
  "usage": {
    "input_tokens": 100,
    "output_tokens": 50
  }
}
```

### 与 OpenAI 格式的关键差异

| 差异点 | OpenAI | Claude |
|--------|--------|--------|
| Auth header | `Authorization: Bearer` | `x-api-key` + `anthropic-version` |
| System prompt | messages 数组中 role=system | 顶层 `system` 字段 |
| Tool 定义 | `tools[].function.parameters` | `tools[].input_schema` |
| Tool call 响应 | `message.tool_calls[]` | `content[]` 中 type=tool_use |
| Tool result 传回 | role=tool, tool_call_id | role=user, content 含 type=tool_result |
| content 格式 | 字符串 | 数组（可混合 text + tool_use） |
| 必须参数 | - | `max_tokens` 必填 |

### ClaudeProvider 转换逻辑

`ConversationMessage` → Claude 请求的转换要点:
1. 从 messages 中提取所有 role=system 的 content 合并到顶层 `system` 字段
2. `AssistantToolCalls` → content 数组: `[{type: "text"}, {type: "tool_use"}]`
3. `ToolResult` → role=user, content: `[{type: "tool_result", tool_use_id, content}]`
4. `ToolSpec.parameters` → 改名为 `input_schema`
5. 响应解析: 遍历 `content[]`，text 拼接文本，tool_use 收集为 ToolCall
