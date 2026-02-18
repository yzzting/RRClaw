# Tool Call 能力调研与修复计划

## 背景

RRClaw 的 `CompatibleProvider` 已经实现了 OpenAI 兼容格式的 tool call 处理（请求构造、响应解析、SSE 流式增量累积），但实际使用中模型可能没有正确触发 tool call。

本文档调研 DeepSeek、MiniMax、GLM 三家的 tool call 能力现状，并给出修复/优化计划。

**参考文档**:
- DeepSeek: https://api-docs.deepseek.com/zh-cn/guides/tool_calls + https://api-docs.deepseek.com/zh-cn/guides/thinking_mode
- MiniMax: https://platform.minimaxi.com/docs/guides/text-m2-function-call
- GLM: https://docs.bigmodel.cn/cn/guide/capabilities/function-calling

---

## 一、各 Provider Tool Call 能力调研

### 1. DeepSeek

**支持模型**:
- `deepseek-chat` (V3/V3.2) — 标准 Function Calling，OpenAI 兼容格式
- `deepseek-chat` + `thinking: {type: "enabled"}` — V3.2 思考模式 + Tool Call
- `deepseek-reasoner` (R1) — 原生 Thinking 模式 + Tool Call

**API 格式**: 完全 OpenAI 兼容
- 请求: `tools` 数组 + `tool_choice: "auto"`
- 响应: `message.tool_calls[].{id, type, function.{name, arguments}}`
- 流式: `delta.tool_calls[].{index, id, function.{name, arguments}}`
- Strict 模式 (Beta): `base_url="https://api.deepseek.com/beta"` + `"strict": true`

**关键: `reasoning_content` 处理规则**:
1. **同一 Turn 内的多轮 tool call**: 必须将上一轮响应的 `reasoning_content` 原样传回 assistant 消息中，否则返回 **400 错误**
2. **新 Turn（新用户问题）**: 应清空旧的 `reasoning_content`（API 会忽略，但浪费 token/带宽）
3. 清空建议: 在新 user 消息前，将 history 中旧 assistant 消息的 `reasoning_content` 设为 None

**思考模式不支持的参数**: `temperature`, `top_p`, `presence_penalty`, `frequency_penalty`（设置无效但不报错）

**当前代码问题**:
- `compatible.rs:48` — 所有 assistant 消息硬编码 `reasoning_content: ""`
- 这在 `deepseek-chat` 非思考模式下工作（被忽略），但在 Reasoner/思考模式多轮 tool call 时会丢失真实的 reasoning_content，导致 400 错误

### 2. MiniMax

**支持模型**:
- `MiniMax-M2.5` — 最新 Agentic Model，原生 **Interleaved Thinking** + Tool Call

**API 格式**: OpenAI 兼容（也支持 Anthropic 兼容）
- OpenAI SDK 端点: `https://api.minimaxi.com/v1`
- Anthropic SDK 端点: `https://api.minimaxi.com/anthropic`（官方推荐）
- 请求: 标准 `tools` + `tool_choice`
- 响应: `message.tool_calls[].{id, type, function.{name, arguments}, index}`

**关键: `reasoning_details` / `<think>` 处理**:

| 参数 | 行为 |
|------|------|
| `reasoning_split: true`（推荐） | thinking 内容单独输出到 `reasoning_details` 字段 |
| `reasoning_split: false` | thinking 以 `<think>...</think>` 标签嵌入 `content` 字段 |

**多轮 Tool Call 必须完整回传**:
- OpenAI SDK: 直接 `messages.append(response_message)`（含 `reasoning_details`）
- **不可截断或修改** thinking 内容，否则破坏 Interleaved Thinking 链

**与 DeepSeek 的对比**:
- DeepSeek 用 `reasoning_content` 字段（字符串）
- MiniMax 用 `reasoning_details` 字段（数组: `[{type: "reasoning.text", text: "..."}]`）
- 两者都要求在同一 Turn 的多轮 tool call 中回传

### 3. GLM（智谱）

**支持模型**:
- `glm-4.7` — 最新模型，原生 Function Calling（已确认）
- `glm-4.6` — 支持 Function Calling
- `glm-5` — 支持 Function Calling
- `glm-4-plus` / `glm-4-flash` / `glm-4-air` — 也支持

**API 格式**: 完全 OpenAI 兼容
- 请求: 标准 `tools` + `tool_choice: "auto"`（**仅支持 auto**）
- 响应: 标准 `message.tool_calls[].{id, type, function.{name, arguments}}`
- Tool 结果回传: `role: "tool"` + `tool_call_id` + `content`

**特点**:
- 最标准的 OpenAI 兼容实现
- **无 thinking/reasoning 特殊字段需要处理**
- `tool_choice` 仅支持 `"auto"`，不支持 `"none"` 或指定函数
- 参数 schema 支持 `enum`、`examples`、`default` 等扩展

---

## 二、三家对比总结

| 特性 | DeepSeek | MiniMax M2.5 | GLM-4.7 |
|------|----------|-------------|---------|
| Tool Call 格式 | OpenAI 兼容 | OpenAI 兼容 | OpenAI 兼容 |
| Thinking 字段 | `reasoning_content` (string) | `reasoning_details` (array) 或 `<think>` 标签 | 无 |
| Thinking 需回传 | 同一 Turn 内必须回传 | 同一 Turn 内必须回传 | N/A |
| tool_choice | auto / none / 指定 | auto / none / 指定 | 仅 auto |
| 流式 tool call | delta.tool_calls 增量 | delta.tool_calls 增量 | 需确认 |
| 特殊参数 | `thinking.type: "enabled"` | `reasoning_split: true` | 无 |

---

## 三、当前代码问题分析

### 问题 1: `reasoning_content` 硬编码空字符串

**文件**: `src/providers/compatible.rs:46-49`

```rust
// 当前代码
if role == "assistant" {
    obj["reasoning_content"] = serde_json::json!("");
}
```

**问题**:
- DeepSeek Reasoner/思考模式多轮 tool call 时，丢失了真实的 `reasoning_content`，导致 400 错误
- MiniMax M2.5 也需要回传 `reasoning_details`，当前完全没有处理
- 对 GLM 发送 `reasoning_content` 是多余的（被忽略）

### 问题 2: 响应中 thinking 内容未保存到 history

**文件**: `src/agent/loop_.rs:200-208`

```rust
// 无 tool calls — 最终回复
self.history.push(ConversationMessage::Chat(ChatMessage {
    role: "assistant".to_string(),
    content: final_text.clone(),
}));
```

**问题**: 当模型返回 `reasoning_content` + `content` 时，只保存了 `content`，丢失了 thinking 内容。多轮 tool call 时无法正确回传。

### 问题 3: 流式响应 `reasoning_content` 未分别累积

**文件**: `src/providers/compatible.rs:294-301`

当前流式处理将 `content` 和 `reasoning_content` 合并成同一个 `full_text`，无法区分。多轮 tool call 时无法正确回传给 API。

### 问题 4: MiniMax `reasoning_details` 完全未处理

当前代码没有解析或回传 MiniMax 的 `reasoning_details` 字段。

---

## 四、修复方案

### 设计原则

1. **统一抽象**: 用 `reasoning_content: Option<String>` 统一 DeepSeek 和 MiniMax 的 thinking 内容
2. **Provider 感知**: `build_messages` 根据 Provider 类型决定如何序列化 thinking 内容
3. **零开销**: GLM 等无 thinking 的 Provider，该字段为 `None`，不发送额外字段
4. **新 Turn 自动清空**: Agent Loop 在新 user 消息前，清空 history 中旧 assistant 的 `reasoning_content`

### 4.1 数据结构改动

**文件**: `src/providers/traits.rs`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,  // 新增
}

#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub text: Option<String>,
    pub reasoning_content: Option<String>,  // 新增: DeepSeek reasoning_content 或 MiniMax reasoning_details 文本
    pub tool_calls: Vec<ToolCall>,
}

pub enum ConversationMessage {
    Chat(ChatMessage),
    AssistantToolCalls {
        text: Option<String>,
        reasoning_content: Option<String>,  // 新增
        tool_calls: Vec<ToolCall>,
    },
    ToolResult { ... },  // 不变
}
```

### 4.2 CompatibleProvider 改动

**文件**: `src/providers/compatible.rs`

#### `build_messages` — 条件性传递 reasoning_content

```rust
ConversationMessage::Chat(ChatMessage { role, content, reasoning_content }) => {
    let mut obj = json!({ "role": role, "content": content });
    // 仅 assistant 消息且有 reasoning_content 时才传递
    if role == "assistant" {
        if let Some(rc) = reasoning_content {
            obj["reasoning_content"] = json!(rc);
        }
    }
    result.push(obj);
}

ConversationMessage::AssistantToolCalls { text, reasoning_content, tool_calls } => {
    let mut obj = json!({ "role": "assistant" });
    if let Some(rc) = reasoning_content {
        obj["reasoning_content"] = json!(rc);
    }
    // ... tool_calls 处理不变
}
```

**关键变化**: 不再硬编码 `reasoning_content: ""`，只在有值时才传递。

#### `parse_response` — 提取 reasoning_content

```rust
fn parse_response(body: &OpenAIResponse) -> ChatResponse {
    let choice = ...;
    let text = choice.message.content.clone().filter(|s| !s.is_empty());
    let reasoning_content = choice.message.reasoning_content.clone().filter(|s| !s.is_empty());

    // 不再用 reasoning_content 回退 text
    // DeepSeek Reasoner: text=最终回答, reasoning_content=思考过程
    // 如果两者都为空，说明只有 tool_calls

    ChatResponse { text, reasoning_content, tool_calls }
}
```

#### 流式响应 — 分别累积 content 和 reasoning_content

```rust
let mut full_text = String::new();
let mut full_reasoning = String::new();  // 新增

// 在 SSE 处理中:
if let Some(content) = choice.delta.content.as_deref().filter(|s| !s.is_empty()) {
    full_text.push_str(content);
    let _ = tx.send(StreamEvent::Text(content.to_string())).await;
}
if let Some(rc) = choice.delta.reasoning_content.as_deref().filter(|s| !s.is_empty()) {
    full_reasoning.push_str(rc);
    let _ = tx.send(StreamEvent::Thinking).await;
}

// 最终组装:
ChatResponse {
    text: if full_text.is_empty() { None } else { Some(full_text) },
    reasoning_content: if full_reasoning.is_empty() { None } else { Some(full_reasoning) },
    tool_calls,
}
```

### 4.3 Agent Loop 改动

**文件**: `src/agent/loop_.rs`

#### 保存响应时携带 reasoning_content

```rust
// 无 tool calls 时
self.history.push(ConversationMessage::Chat(ChatMessage {
    role: "assistant".to_string(),
    content: final_text.clone(),
    reasoning_content: response.reasoning_content.clone(),
}));

// 有 tool calls 时
self.history.push(ConversationMessage::AssistantToolCalls {
    text: response.text.clone(),
    reasoning_content: response.reasoning_content.clone(),
    tool_calls: response.tool_calls.clone(),
});
```

#### 新 Turn 开始时清空旧 reasoning_content

```rust
// process_message 开头，在添加新 user 消息之前:
self.clear_old_reasoning_content();

fn clear_old_reasoning_content(&mut self) {
    for msg in &mut self.history {
        match msg {
            ConversationMessage::Chat(cm) if cm.role == "assistant" => {
                cm.reasoning_content = None;
            }
            ConversationMessage::AssistantToolCalls { reasoning_content, .. } => {
                *reasoning_content = None;
            }
            _ => {}
        }
    }
}
```

这满足 DeepSeek 文档的建议："新 Turn 开始时删除历史中的 reasoning_content 以节省带宽"。

### 4.4 MiniMax reasoning_details 处理

MiniMax M2.5 的 `reasoning_details` 格式为:
```json
[{"type": "reasoning.text", "text": "...thinking content..."}]
```

**方案**: 在 `parse_response` 中将 `reasoning_details` 数组拼接为单个字符串，存入 `ChatResponse.reasoning_content`。回传时，由于我们使用 OpenAI 兼容格式（MiniMax 支持），直接将消息对象回传即可。

**注意**: 如果 MiniMax 使用 `reasoning_split: false`（默认），thinking 内容以 `<think>...</think>` 标签嵌入 `content` 中，不需要特殊处理——原样回传 content 即可。

**建议**: 对 MiniMax Provider，在请求中追加 `reasoning_split: true`，通过 `extra_body` 或直接在 request body 中设置。这样 thinking 内容与正文分离，处理更干净。

```rust
// build_request_body 中，针对 MiniMax:
if is_minimax {
    body["reasoning_split"] = json!(true);
}
```

**如何判断是否为 MiniMax**: 可以通过 `base_url` 包含 `minimax` 来判断，或在 CompatibleProvider 中增加一个 `provider_hint` 字段。

### 4.5 History 持久化兼容

**文件**: `src/memory/sqlite.rs`

`conversation_history` 表需要增加 `reasoning_content` 列:

```sql
ALTER TABLE conversation_history ADD COLUMN reasoning_content TEXT;
```

使用 `ALTER TABLE ... ADD COLUMN` 做向后兼容迁移（SQLite 支持，旧行默认 NULL）。

### 4.6 显示层: reasoning_content 只用于回传，不影响展示

- Agent 返回给用户的 `final_text` 仍然取 `response.text`（正文内容）
- `reasoning_content` 仅用于多轮 tool call 时回传给 API
- 如果 `text` 为 None 且 `reasoning_content` 有值（DeepSeek Reasoner 纯思考阶段），`final_text` 可考虑显示 reasoning_content 或显示"思考中..."
- 流式中 thinking 动画已有处理（`StreamEvent::Thinking`），不需要大改

---

## 五、实现计划

### 提交策略（原子化）

| # | 提交消息 | 涉及文件 | 说明 |
|---|---------|---------|------|
| 1 | `docs: add tool call investigation and fix plan` | `docs/tool-call-investigation.md` | 本文档 |
| 2 | `feat: add reasoning_content to ChatMessage and ChatResponse` | `providers/traits.rs` | 数据结构扩展 |
| 3 | `refactor: update all ChatMessage constructors for reasoning_content` | 多文件 | 编译通过的最小改动，所有构造处加 `reasoning_content: None` |
| 4 | `fix: properly handle reasoning_content in CompatibleProvider` | `providers/compatible.rs` | 核心修复：条件传递、分别解析、流式分别累积 |
| 5 | `feat: preserve reasoning_content in agent loop history` | `agent/loop_.rs` | agent 保存 reasoning_content + 新 Turn 清空旧 reasoning |
| 6 | `feat: add reasoning_content column to conversation_history` | `memory/sqlite.rs` | 数据库迁移 |
| 7 | `test: add reasoning_content round-trip tests` | `providers/compatible.rs`, `agent/loop_.rs` | 测试覆盖 |

### 预计改动量

- 新增/修改: ~250 行代码
- 新增测试: ~100 行
- 总计: ~350 行，7 commits

---

## 六、验证方式

1. **单元测试**:
   - `build_messages`: 有 reasoning_content 时正确传递，无则省略
   - `build_messages`: 不再为非 DeepSeek 的 assistant 消息添加空 reasoning_content
   - `parse_response`: 正确分离 text 和 reasoning_content
   - 流式累积: content 和 reasoning_content 分别累积
   - Agent: 多轮 tool call 时 history 中保留 reasoning_content
   - Agent: 新 Turn 时旧 reasoning_content 被清空

2. **端到端测试（手动）**:
   - `deepseek-chat` + tool call → 正常触发和执行
   - `deepseek-reasoner` + tool call → reasoning_content 正确回传，不报 400
   - `glm-4.7` + tool call → 正常工作，无多余字段
   - MiniMax M2.5 + tool call → reasoning_details 正确处理

3. **回归**:
   - `cargo test` 全部通过
   - `cargo clippy -- -W clippy::all` 零警告
   - 现有 REPL 流式对话功能不受影响

---

## 七、风险与注意事项

1. **Provider 识别**: MiniMax 需要 `reasoning_split: true` 参数，需要在 `build_request_body` 中根据 Provider 类型条件设置。方案: 通过 `base_url` 包含 `minimax` 判断，或增加 `provider_hint` 配置字段
2. **MiniMax `reasoning_details` 序列化**: 它是数组格式 `[{type, text}]`，而 DeepSeek 是字符串。存入 `Option<String>` 时需要拼接文本；回传时需要还原为数组格式。**如果复杂度过高，可以先用 `reasoning_split: false` 让 thinking 嵌入 content，简化处理**
3. **GLM `tool_choice` 仅支持 `auto`**: 当前代码不传 `tool_choice`（依赖默认值），与 GLM 兼容
4. **DeepSeek 思考模式参数限制**: `temperature` 等参数在思考模式下被忽略，但不报错，暂不需要特殊处理
5. **History 数据库迁移**: 新增列用 `ALTER TABLE ADD COLUMN`，旧数据的 `reasoning_content` 为 NULL，向后兼容
6. **DeepSeek Reasoner 纯思考无正文**: 如果 `text=None, reasoning_content=Some(...)`，Agent 的 `final_text` 会为空字符串。需要决定是否将 reasoning_content 作为 fallback 展示给用户

---

## 八、后续优化（不在本次范围）

1. **MiniMax Anthropic SDK 格式**: 官方推荐 Anthropic 兼容接口，未来可以考虑为 MiniMax 增加 Claude-style 的 Provider 实现
2. **DeepSeek Strict Mode**: 强制 JSON Schema 约束，减少 tool call 参数错误
3. **并行 Tool Call**: 当模型返回多个 tool_calls 时，可并行执行提高效率
4. **Thinking 展示优化**: 将 reasoning_content 以折叠/渐隐方式展示给用户，提升 UX
