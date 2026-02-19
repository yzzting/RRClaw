# P4-0: Skill 驱动的两阶段路由

> **实施优先级：P4 所有功能中最高，必须先于 Memory Tools / ReliableProvider / History Compaction / MCP Client 实施。**

---

## 背景与动机

随着功能增加，当前 system prompt 越来越臃肿：工具描述、安全规则、记忆上下文、环境信息、使用规范全部堆在一起。内容再精准，LLM 输出仍不可控。

核心问题：**行为指南被硬编码在 system prompt 里，无法按需加载，也无法被用户定制。**

**目标**：将行为指南封装为 skill，通过两阶段路由按需注入，让 base system prompt 保持极简、稳定，行为由 skill 驱动。

---

## 整体架构

```
用户消息
    │
    ▼
[Phase 1] 路由（轻量 LLM 调用）
  System prompt: 身份 + 安全约束 + Skill L1 目录
  不传工具 schema，不传记忆上下文
  输出: RouteResult（三路）
    │
    ├─ Skills(names) ──────────────────────────────────────────────┐
    │   代码加载对应 skill L2 内容                                    │
    │                                                              │
    ├─ Direct ──────────────────────────────────────────────────── ┤
    │   无需 skill，直接进 Phase 2                                    │
    │                                                              │
    └─ NeedClarification(question) ──→ 展示问题给用户 ──→ 用户回答后重新进 Phase 1
                                                                   │
                                                                   ▼
                                                        [Phase 2] 执行（正常 Agent Loop）
                                                          System prompt: 身份 + 安全约束
                                                            + 完整工具 schema
                                                            + 已加载 skill L2 内容（若有）
                                                            + 记忆上下文（动态）
                                                            + 环境信息（动态）
                                                          C 辅助: LLM 可在此阶段调用
                                                            SkillTool 自行加载额外 skill
                                                                   │
                                                                   ▼
                                                            Tool call loop → 最终回复
```

---

## 数据结构

### RouteResult

新增枚举，定义在 `src/agent/loop_.rs`（或 `src/agent/mod.rs` 如需对外暴露）：

```rust
/// Phase 1 路由结果
#[derive(Debug, Clone)]
pub enum RouteResult {
    /// 命中一个或多个 skill，携带 skill 名称列表
    Skills(Vec<String>),
    /// 意图清晰，无需 skill，直接进 Phase 2 执行
    Direct,
    /// 意图模糊，需要向用户澄清，携带澄清问题
    NeedClarification(String),
}
```

### Phase 1 LLM 输出 JSON Schema

Phase 1 LLM 必须严格输出以下三种 JSON 之一：

```json
// 路由到 skill
{"skills": ["git-workflow"], "direct": false}

// 无需 skill，直接执行
{"skills": [], "direct": true}

// 意图模糊，需要澄清
{"skills": [], "direct": false, "question": "你是想查看 git 状态，还是提交代码？"}
```

规则：
- `skills` 数组非空时，`direct` 必须为 `false`
- `skills` 为空且 `direct` 为 `false` 且有 `question` 字段 → NeedClarification
- `skills` 为空且 `direct` 为 `true` → Direct
- 解析失败时，降级为 `Direct`（Phase 1 故障不阻断用户请求）

---

## 核心函数

### 1. `build_routing_prompt(skills: &[SkillMeta]) -> String`

构造 Phase 1 的 system prompt，极简。

```rust
fn build_routing_prompt(skills: &[SkillMeta]) -> String {
    let mut prompt = String::new();

    // [1] 身份
    prompt.push_str("你是 RRClaw 的路由助手。你的唯一任务是分析用户消息，决定需要加载哪些行为指南（skill）。\n\n");

    // [2] 安全约束（硬编码，不可跳过）
    prompt.push_str("【约束】\n");
    prompt.push_str("- 禁止调用任何工具\n");
    prompt.push_str("- 只输出 JSON，不做其他任何操作\n\n");

    // [3] 可用 Skill 目录（L1 元数据）
    if skills.is_empty() {
        prompt.push_str("【可用 Skill】\n暂无可用 skill。\n\n");
    } else {
        prompt.push_str("【可用 Skill】\n");
        for skill in skills {
            prompt.push_str(&format!("- {}: {}\n", skill.name, skill.description));
        }
        prompt.push('\n');
    }

    // [4] 输出格式说明
    prompt.push_str("【输出格式】\n");
    prompt.push_str("必须输出合法 JSON，三种情况之一：\n\n");
    prompt.push_str("1. 需要加载 skill（意图明确且有匹配 skill）：\n");
    prompt.push_str("   {\"skills\": [\"skill-name\"], \"direct\": false}\n\n");
    prompt.push_str("2. 无需 skill，意图清晰可直接执行：\n");
    prompt.push_str("   {\"skills\": [], \"direct\": true}\n\n");
    prompt.push_str("3. 意图模糊，需要向用户澄清（仅在真正无法判断时使用）：\n");
    prompt.push_str("   {\"skills\": [], \"direct\": false, \"question\": \"你的澄清问题\"}\n\n");

    // [5] 判断原则
    prompt.push_str("【判断原则】\n");
    prompt.push_str("- 用户意图清晰时，即使没有匹配的 skill，也应选择 direct: true\n");
    prompt.push_str("- skill 是增强，不是门槛——没有 skill 可以匹配时不要问用户\n");
    prompt.push_str("- 只有用户表达含糊、继续执行会走错方向时，才返回 question\n");
    prompt.push_str("- question 字段使用中文，简洁明确\n");

    prompt
}
```

### 2. `route(user_message: &str) -> Result<RouteResult>`

在 `Agent` impl 块中新增，调用 Provider 执行 Phase 1。

```rust
async fn route(&self, user_message: &str) -> Result<RouteResult> {
    use crate::providers::traits::{ChatMessage, ConversationMessage};

    let routing_prompt = build_routing_prompt(&self.skills);

    let messages = vec![
        ConversationMessage::Chat(ChatMessage {
            role: "system".to_string(),
            content: routing_prompt,
        }),
        ConversationMessage::Chat(ChatMessage {
            role: "user".to_string(),
            content: user_message.to_string(),
        }),
    ];

    // Phase 1 不传工具，温度极低保证输出稳定
    let response = self
        .provider
        .chat_with_tools(
            &messages,
            &[],       // 空工具列表，Phase 1 禁止工具调用
            &self.model,
            0.1,       // 低温度，路由输出要确定性
        )
        .await;

    match response {
        Err(e) => {
            // Phase 1 调用失败，降级为 Direct，不阻断请求
            debug!("Phase 1 路由失败，降级为 Direct: {}", e);
            Ok(RouteResult::Direct)
        }
        Ok(resp) => {
            let text = resp.text.unwrap_or_default();
            Ok(parse_route_result(&text))
        }
    }
}
```

### 3. `parse_route_result(text: &str) -> RouteResult`

解析 Phase 1 LLM 输出，独立纯函数（便于测试）。

```rust
fn parse_route_result(text: &str) -> RouteResult {
    // 从文本中提取 JSON（LLM 有时会在 JSON 前后加 markdown ```）
    let json_str = extract_json(text);

    let Ok(value) = serde_json::from_str::<serde_json::Value>(&json_str) else {
        debug!("Phase 1 输出解析失败，降级为 Direct: {:?}", text);
        return RouteResult::Direct;
    };

    let skills = value["skills"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if !skills.is_empty() {
        return RouteResult::Skills(skills);
    }

    let direct = value["direct"].as_bool().unwrap_or(false);
    if direct {
        return RouteResult::Direct;
    }

    if let Some(question) = value["question"].as_str() {
        if !question.is_empty() {
            return RouteResult::NeedClarification(question.to_string());
        }
    }

    // 兜底：无法判断时降级为 Direct
    RouteResult::Direct
}

/// 从可能包含 markdown 代码块的文本中提取 JSON 字符串
fn extract_json(text: &str) -> &str {
    let text = text.trim();
    // 处理 ```json ... ``` 或 ``` ... ```
    if let Some(start) = text.find('{') {
        if let Some(end) = text.rfind('}') {
            return &text[start..=end];
        }
    }
    text
}
```

### 4. 修改 `process_message()` — 接入两阶段路由

在现有 `process_message()` 开头插入 Phase 1 路由逻辑：

```rust
pub async fn process_message(
    &mut self,
    user_msg: &str,
    on_text: impl Fn(&str),
) -> Result<String> {
    // ─── Phase 1: 路由 ───────────────────────────────────────────
    let route_result = self.route(user_msg).await?;

    match route_result {
        RouteResult::NeedClarification(question) => {
            // 直接返回澄清问题，不执行任何工具
            on_text(&question);
            return Ok(question);
        }
        RouteResult::Skills(skill_names) => {
            // 加载对应 skill 的 L2 内容，注入到本次 Phase 2 的 system prompt
            self.inject_routed_skills(&skill_names);
        }
        RouteResult::Direct => {
            // 清空本次临时注入的 skill（上一轮可能有残留）
            self.routed_skill_content = None;
        }
    }

    // ─── Phase 2: 正常 Agent Loop（现有逻辑不变）────────────────────
    // ... 现有 process_message 代码 ...
}
```

同样修改 `process_message_stream()`，Phase 1 本身不需要流式（JSON 输出），只有 Phase 2 流式。

### 5. `inject_routed_skills(skill_names: &[String])`

加载 skill L2 内容，存到临时字段，Phase 2 构建 system prompt 时使用。

```rust
fn inject_routed_skills(&mut self, skill_names: &[String]) {
    let mut content = String::new();
    for name in skill_names {
        if let Some(skill_content) = load_skill_content_by_name(&self.skills, name) {
            content.push_str(&format!("\n\n---\n## Skill: {}\n{}", name, skill_content));
        }
    }
    if !content.is_empty() {
        self.routed_skill_content = Some(content);
    }
}
```

### 6. 修改 `build_system_prompt()` — Phase 2 注入 skill 内容

Phase 2 的 system prompt 需要包含已路由的 skill 内容：

```rust
fn build_system_prompt(&self, memory_context: Option<&str>) -> String {
    let mut prompt = self.base_system_prompt(); // 身份 + 安全约束

    // 工具描述（完整 schema，Phase 2 才注入）
    prompt.push_str(&self.build_tools_section());

    // 已路由的 skill L2 内容（Phase 1 结果）
    if let Some(skill_content) = &self.routed_skill_content {
        prompt.push_str("\n\n[行为指南]\n");
        prompt.push_str(skill_content);
    }

    // 记忆上下文（动态，Phase 2 才注入）
    if let Some(ctx) = memory_context {
        prompt.push_str(&format!("\n\n[相关记忆]\n{}", ctx));
    }

    // 环境信息
    prompt.push_str(&self.build_env_section());

    prompt
}
```

---

## Agent 结构体变更

在 `Agent` 结构体中新增一个字段，存储本次对话 Phase 1 路由结果的 skill 内容（每轮对话重置）：

```rust
pub struct Agent {
    // ... 现有字段 ...

    /// Phase 1 路由后加载的 skill 内容，每次 process_message 重置
    routed_skill_content: Option<String>,
}
```

`routed_skill_content` 在每次 `process_message` 调用开始时被覆写（`inject_routed_skills` 或清空），不跨轮持久化。

---

## Skill 描述格式规范（重要）

Phase 1 LLM 依赖 skill 的 `description` 字段来判断是否命中。描述必须包含**触发场景提示**，格式：

```
{功能简介}（{触发场景关键词}）
```

内置 skill 描述更新示例：

| Skill | 当前描述 | 更新后描述 |
|-------|---------|-----------|
| `git-workflow` | Git 操作工作流指南 | Git 操作工作流指南（用户提到 git、提交代码、分支、版本控制时加载） |
| `code-review` | 代码审查最佳实践 | 代码审查最佳实践（用户请求 review 代码、检查代码质量时加载） |
| `rust-dev` | Rust 开发规范 | Rust 开发规范（用户进行 Rust 开发、调试 Rust 编译错误时加载） |

需要更新：`src/skills/builtin/` 下各 skill 文件的 frontmatter `description` 字段，或在 `builtin_skills()` 函数中直接更新对应的 `description` 字符串。

---

## C 辅助路径（SkillTool 自驱动）

Phase 2 执行阶段，如果 LLM 发现需要额外 skill，可以直接调用 `SkillTool`：

```
tool_call: skill(name="shell-safety")
→ 返回 skill L2 内容作为 tool result
→ LLM 读取内容，按指南继续执行
```

**无需额外代码改动**，`SkillTool` 已经实现了这个能力。Phase 1 路由失败/未覆盖的场景由 C 路径兜底。

---

## 改动范围

| 文件 | 改动内容 |
|------|---------|
| `src/agent/loop_.rs` | 新增 `RouteResult` 枚举、`route()`、`parse_route_result()`、`extract_json()`、`inject_routed_skills()`；修改 `process_message()`、`process_message_stream()`、`build_system_prompt()`；`Agent` 结构体新增 `routed_skill_content` 字段 |
| `src/skills/builtin/*.md` | 更新各 skill 的 `description` 字段，加入触发场景提示 |
| `src/channels/cli.rs` | `NeedClarification` 结果已经通过 `on_text` 回调透传，CLI 侧无需额外改动（除非需要特殊展示样式） |

**不需要改动**：
- `src/tools/` — 工具层不感知路由
- `src/providers/` — Provider 层不感知路由
- `src/memory/` — 记忆层不感知路由
- `src/config/` — 无新配置项（Phase 1 复用当前 model + provider）

---

## 提交策略

```
1. docs: add p4-skill-routing.md (本文件)

2. feat(agent): add RouteResult enum and parse_route_result()
   - 新增 RouteResult { Skills / Direct / NeedClarification }
   - 新增 parse_route_result() 纯函数
   - 新增 extract_json() 辅助函数
   - 新增对应单元测试

3. feat(agent): add build_routing_prompt() for Phase 1
   - 极简 system prompt：身份 + 安全约束 + Skill L1 目录
   - 新增单元测试：验证 prompt 不含工具 schema

4. feat(agent): add route() method to Agent
   - 调用 provider.chat_with_tools(&[], temperature=0.1)
   - 失败时降级为 Direct
   - Agent 结构体新增 routed_skill_content 字段

5. feat(agent): integrate two-phase routing into process_message
   - process_message() 和 process_message_stream() 接入 route()
   - inject_routed_skills() 加载 skill L2 内容
   - build_system_prompt() 注入 routed_skill_content

6. feat(skills): update builtin skill descriptions with trigger hints
   - 更新 git-workflow、code-review、rust-dev 的 description

7. test: add integration tests for two-phase routing
```

---

## 测试用例

### 单元测试（纯函数，无需 mock）

```rust
#[test]
fn parse_route_result_skills() {
    let result = parse_route_result(r#"{"skills": ["git-workflow"], "direct": false}"#);
    assert!(matches!(result, RouteResult::Skills(s) if s == ["git-workflow"]));
}

#[test]
fn parse_route_result_direct() {
    let result = parse_route_result(r#"{"skills": [], "direct": true}"#);
    assert!(matches!(result, RouteResult::Direct));
}

#[test]
fn parse_route_result_clarification() {
    let result = parse_route_result(
        r#"{"skills": [], "direct": false, "question": "你是想查看还是提交？"}"#
    );
    assert!(matches!(result, RouteResult::NeedClarification(q) if q.contains("查看")));
}

#[test]
fn parse_route_result_fallback_on_invalid_json() {
    // 解析失败时降级为 Direct
    let result = parse_route_result("这不是 JSON");
    assert!(matches!(result, RouteResult::Direct));
}

#[test]
fn parse_route_result_strips_markdown_code_block() {
    let result = parse_route_result("```json\n{\"skills\": [], \"direct\": true}\n```");
    assert!(matches!(result, RouteResult::Direct));
}

#[test]
fn parse_route_result_multiple_skills() {
    let result = parse_route_result(
        r#"{"skills": ["git-workflow", "code-review"], "direct": false}"#
    );
    match result {
        RouteResult::Skills(s) => assert_eq!(s.len(), 2),
        _ => panic!("expected Skills"),
    }
}

#[test]
fn build_routing_prompt_no_tools() {
    let skills = vec![];
    let prompt = build_routing_prompt(&skills);
    // Phase 1 prompt 不包含工具 schema
    assert!(!prompt.contains("shell"));
    assert!(!prompt.contains("file_read"));
    assert!(prompt.contains("JSON"));
}

#[test]
fn build_routing_prompt_contains_skill_names() {
    let skills = vec![SkillMeta {
        name: "git-workflow".to_string(),
        description: "Git 操作指南（用户提到 git 时加载）".to_string(),
        source: SkillSource::Builtin,
        content_hash: None,
    }];
    let prompt = build_routing_prompt(&skills);
    assert!(prompt.contains("git-workflow"));
    assert!(prompt.contains("Git 操作指南"));
}
```

### 集成测试（需要 mock Provider）

```rust
#[tokio::test]
async fn route_returns_direct_on_provider_failure() {
    // mock provider 总是返回错误
    // route() 应降级为 Direct，不 panic
    let agent = build_agent_with_failing_provider();
    let result = agent.route("git status").await.unwrap();
    assert!(matches!(result, RouteResult::Direct));
}

#[tokio::test]
async fn process_message_returns_clarification_without_tool_execution() {
    // mock provider Phase 1 返回 NeedClarification
    // 验证没有任何工具被调用
    // 验证返回文本就是 question 内容
}
```

---

## 关键注意事项

### 1. Phase 1 温度必须低
`route()` 调用 provider 时 `temperature = 0.1`，保证路由输出的确定性。高温度会导致随机输出无法解析为 JSON。

### 2. Phase 1 不传工具列表
`chat_with_tools(&messages, &[], ...)` 第二个参数必须为空切片。否则部分模型会忽略指令直接调用工具。

### 3. routed_skill_content 每轮重置
`process_message` 每次调用都会覆写 `routed_skill_content`（无论注入新内容还是清空），避免上一轮的 skill 污染当前轮。

### 4. NeedClarification 不写入 history
当返回澄清问题时，不应将这条问题写入 conversation history（那样会影响下一轮的上下文），只展示给用户。用户回答后作为新的用户消息重新进入 Phase 1。

### 5. DeepSeek Reasoner 兼容
Phase 1 调用也需要注意 `reasoning_content` 字段的处理，与现有 Phase 2 保持一致（参考 `src/providers/compatible.rs` 的现有处理逻辑）。

### 6. pre_select_tool 与 Phase 1 的关系
MiniMax 已实现的 `pre_select_tool()` 是基于关键词的规则路由（方案 A），与 Phase 1（方案 B）存在功能重叠。**实施两阶段路由后，`pre_select_tool()` 可以保留作为备用，或在 Phase 1 稳定后移除。** 建议先保留，两者并行运行，验证 Phase 1 覆盖率后再决定是否删除 `pre_select_tool()`。

### 7. Skill 描述是路由质量的关键
Phase 1 的路由准确率直接依赖 skill description 的质量。描述必须包含清晰的触发场景（见"Skill 描述格式规范"章节）。新增 skill 时，description 字段的触发场景描述是必填项。
