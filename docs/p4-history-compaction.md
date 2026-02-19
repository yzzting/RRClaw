# P4-D: History Auto-Compaction 实现计划

## 背景

当前 `trim_history()` 采用硬截断策略：history 超过 50 条时直接丢弃最早的消息。这导致长对话中早期重要上下文（用户偏好、项目约定、已解决的问题）永久丢失。

**目标**：当 history 接近上限时，用 LLM 生成摘要替代旧消息，保留上下文语义的同时控制 token 消耗。

---

## 一、架构设计

```
process_message() / process_message_stream()
  └── 步骤 6: trim_history()  ← 改为 compact_history_if_needed()
        ├── history.len() < COMPACT_THRESHOLD (40) → 不触发，直接返回
        └── history.len() >= COMPACT_THRESHOLD
              ├── 取前 COMPACT_WINDOW (30) 条消息
              ├── 调用 LLM 生成摘要（≤ 1500 字符）
              ├── 用单条摘要消息替换这 30 条
              └── 保留最近 10 条不压缩（保持对话连贯性）
```

触发条件：history ≥ 40 条
压缩窗口：前 30 条
保留窗口：最近 10 条
摘要上限：1500 字符

---

## 二、数据结构与实现

### 2.1 新增常量（src/agent/loop_.rs）

```rust
// 在文件顶部常量区
const MAX_TOOL_ITERATIONS: usize = 10;
const MAX_HISTORY_SIZE: usize = 50;

// 新增：
/// history 条数达到此值时触发压缩
const COMPACT_THRESHOLD: usize = 40;
/// 每次压缩的窗口大小（前 N 条被摘要）
const COMPACT_WINDOW: usize = 30;
/// 压缩生成的摘要最大字符数
const COMPACT_SUMMARY_MAX_CHARS: usize = 1500;
```

### 2.2 compact_history_if_needed（核心方法）

```rust
// src/agent/loop_.rs — 新增方法，替换 trim_history() 的调用位置

impl Agent {
    /// 压缩 history：超过阈值时用 LLM 摘要替代早期消息
    /// 如果 LLM 摘要失败，回退到旧的硬截断策略
    async fn compact_history_if_needed(&mut self) {
        if self.history.len() < COMPACT_THRESHOLD {
            return;
        }

        tracing::info!(
            "history 达到 {} 条，触发压缩（窗口: {} 条）",
            self.history.len(),
            COMPACT_WINDOW
        );

        // 取前 COMPACT_WINDOW 条作为压缩对象
        // 但要确保不截断 AssistantToolCalls + ToolResult 对
        let window_end = find_safe_window_end(&self.history, COMPACT_WINDOW);
        let to_compress = &self.history[..window_end];

        match self.summarize_history(to_compress).await {
            Ok(summary) => {
                tracing::debug!("摘要生成成功（{}字符）", summary.len());
                // 用摘要消息替换被压缩的部分
                let summary_msg = ConversationMessage::Chat(ChatMessage {
                    role: "system".to_string(),
                    content: format!("[对话摘要 - 早期上下文]\n{}", summary),
                    reasoning_content: None,
                });
                let remaining = self.history[window_end..].to_vec();
                self.history = vec![summary_msg];
                self.history.extend(remaining);
                tracing::info!(
                    "history 压缩完成: {} 条 → {} 条",
                    window_end + remaining.len(),
                    self.history.len()
                );
            }
            Err(e) => {
                tracing::warn!("摘要生成失败，回退到硬截断: {:#}", e);
                self.trim_history();
            }
        }
    }

    /// 调用 LLM 对指定 history 片段生成摘要
    async fn summarize_history(&self, messages: &[ConversationMessage]) -> color_eyre::eyre::Result<String> {
        // 将 history 序列化为可读文本
        let transcript = format_history_for_summary(messages);

        // 截断过长的 transcript（避免 token 超限）
        let transcript_truncated = if transcript.len() > 12_000 {
            format!("{}...[已截断]", &transcript[..12_000])
        } else {
            transcript
        };

        let summary_prompt = format!(
            "请将以下对话历史压缩成简洁摘要（不超过 {} 字符）。\n\
             保留：用户的核心需求、重要决策、已解决的问题、关键信息（路径/命令/配置）。\n\
             忽略：闲聊、重复内容、工具执行的详细输出。\n\
             用中文输出，以「对话摘要：」开头。\n\n\
             ---\n{}\n---",
            COMPACT_SUMMARY_MAX_CHARS,
            transcript_truncated
        );

        let summary_messages = vec![
            ConversationMessage::Chat(ChatMessage {
                role: "user".to_string(),
                content: summary_prompt,
                reasoning_content: None,
            })
        ];

        // 直接调用 provider，不传 tools（摘要不需要 tool call）
        let response = self.provider
            .chat_with_tools(&summary_messages, &[], &self.model, 0.3)
            .await?;

        let summary = response.text.unwrap_or_default();
        if summary.is_empty() {
            color_eyre::eyre::bail!("LLM 返回空摘要");
        }

        // 截断摘要到上限
        Ok(truncate_str(&summary, COMPACT_SUMMARY_MAX_CHARS))
    }
}

/// 找到安全的压缩窗口终点：不截断 AssistantToolCalls + ToolResult 对
/// 从 ideal_end 向前找，直到找到一个安全切割点
fn find_safe_window_end(history: &[ConversationMessage], ideal_end: usize) -> usize {
    let end = ideal_end.min(history.len());
    // 从 end 向前找第一个 Chat 消息（安全切割点）
    for i in (0..end).rev() {
        if matches!(history[i], ConversationMessage::Chat(_)) {
            return i + 1;
        }
    }
    // 找不到就用 0（全部压缩）
    0
}

/// 将 history 格式化为摘要 prompt 用的可读文本
fn format_history_for_summary(messages: &[ConversationMessage]) -> String {
    let mut out = String::new();
    for msg in messages {
        match msg {
            ConversationMessage::Chat(cm) => {
                if cm.role == "system" {
                    continue; // 跳过 system 消息
                }
                let role_label = if cm.role == "user" { "用户" } else { "助手" };
                let content = if cm.content.len() > 500 {
                    format!("{}...", &cm.content[..500])
                } else {
                    cm.content.clone()
                };
                out.push_str(&format!("[{}]: {}\n\n", role_label, content));
            }
            ConversationMessage::AssistantToolCalls { text, tool_calls, .. } => {
                if let Some(t) = text {
                    if !t.is_empty() {
                        out.push_str(&format!("[助手]: {}\n", t));
                    }
                }
                let tool_names: Vec<&str> = tool_calls.iter().map(|tc| tc.name.as_str()).collect();
                out.push_str(&format!("[工具调用]: {}\n\n", tool_names.join(", ")));
            }
            ConversationMessage::ToolResult { content, .. } => {
                let preview = if content.len() > 200 {
                    format!("{}...", &content[..200])
                } else {
                    content.clone()
                };
                out.push_str(&format!("[工具结果]: {}\n\n", preview));
            }
        }
    }
    out
}
```

### 2.3 替换 trim_history() 调用

`process_message()` 和 `process_message_stream()` 中各有一处：

```rust
// 原来（src/agent/loop_.rs 第 303 行 和 第 467 行）：
self.trim_history();

// 改为：
self.compact_history_if_needed().await;
```

**注意**：`trim_history()` 方法本身保留不删，作为 fallback 仍然需要。

---

## 三、改动范围

| 文件 | 改动 | 复杂度 |
|------|------|--------|
| `src/agent/loop_.rs` | 新增 3 个常量 + `compact_history_if_needed()` + `summarize_history()` + 2 个辅助函数 + 替换 2 处 `trim_history()` 调用 | 中 |

**不需要改动**：Provider、Tool、Memory、Security、CLI、Config。

---

## 四、提交策略

| # | 提交 | 说明 |
|---|------|------|
| 1 | `feat: add history auto-compaction with LLM summarization` | 核心实现：新增常量 + compact_history_if_needed + summarize_history + format_history_for_summary + find_safe_window_end |
| 2 | `feat: replace trim_history with compact_history_if_needed` | process_message 和 process_message_stream 中替换调用 |
| 3 | `test: add history compaction unit tests` | 所有测试 |

---

## 五、测试用例（~10 个）

```rust
#[cfg(test)]
mod compaction_tests {
    use super::*;
    use crate::providers::{ChatResponse, ToolCall};
    use crate::tools::ToolResult;
    use std::path::PathBuf;

    // 复用 loop_.rs 中已有的 MockProvider、MockMemory、MockTool、test_policy()

    fn make_chat(role: &str, content: &str) -> ConversationMessage {
        ConversationMessage::Chat(ChatMessage {
            role: role.to_string(),
            content: content.to_string(),
            reasoning_content: None,
        })
    }

    fn fill_history(agent: &mut Agent, count: usize) {
        for i in 0..count {
            agent.history.push(make_chat("user", &format!("消息 {}", i)));
            agent.history.push(make_chat("assistant", &format!("回复 {}", i)));
        }
    }

    // --- compact_history_if_needed 测试 ---

    #[tokio::test]
    async fn no_compaction_below_threshold() {
        // history < 40，不触发压缩
        let provider = MockProvider::new(vec![]);
        let mut agent = Agent::new(
            Box::new(provider),
            vec![],
            Box::new(MockMemory),
            test_policy(),
            "test".to_string(), "http://test".to_string(),
            "test-model".to_string(), 0.7, vec![],
        );
        fill_history(&mut agent, 19); // 38 条
        let original_len = agent.history.len();
        agent.compact_history_if_needed().await;
        assert_eq!(agent.history.len(), original_len); // 未变化
    }

    #[tokio::test]
    async fn compaction_triggers_at_threshold() {
        // history = 40，触发压缩，LLM 返回摘要
        let summary_response = ChatResponse {
            text: Some("对话摘要：用户询问了多个问题，助手逐一回答。".to_string()),
            reasoning_content: None,
            tool_calls: vec![],
        };
        let provider = MockProvider::new(vec![summary_response]);
        let mut agent = Agent::new(
            Box::new(provider),
            vec![],
            Box::new(MockMemory),
            test_policy(),
            "test".to_string(), "http://test".to_string(),
            "test-model".to_string(), 0.7, vec![],
        );
        fill_history(&mut agent, 20); // 40 条
        agent.compact_history_if_needed().await;
        // 压缩后 history 应该明显少于 40
        assert!(agent.history.len() < 40);
        // 第一条应该是摘要消息
        if let ConversationMessage::Chat(cm) = &agent.history[0] {
            assert!(cm.content.contains("对话摘要"));
        } else {
            panic!("第一条应该是摘要 Chat 消息");
        }
    }

    #[tokio::test]
    async fn compaction_fallback_to_trim_on_llm_failure() {
        // LLM 返回空响应 → 触发 fallback trim_history
        let empty_response = ChatResponse {
            text: None,  // 空响应触发 summarize_history 报错
            reasoning_content: None,
            tool_calls: vec![],
        };
        let provider = MockProvider::new(vec![empty_response]);
        let mut agent = Agent::new(
            Box::new(provider),
            vec![],
            Box::new(MockMemory),
            test_policy(),
            "test".to_string(), "http://test".to_string(),
            "test-model".to_string(), 0.7, vec![],
        );
        fill_history(&mut agent, 25); // 50 条
        agent.compact_history_if_needed().await;
        // fallback trim_history 应将 history 裁到 50 条内
        assert!(agent.history.len() <= MAX_HISTORY_SIZE);
    }

    #[tokio::test]
    async fn compaction_preserves_recent_messages() {
        // 压缩后，最近 10 条消息应保留
        let summary_response = ChatResponse {
            text: Some("对话摘要：早期上下文。".to_string()),
            reasoning_content: None,
            tool_calls: vec![],
        };
        let provider = MockProvider::new(vec![summary_response]);
        let mut agent = Agent::new(
            Box::new(provider),
            vec![],
            Box::new(MockMemory),
            test_policy(),
            "test".to_string(), "http://test".to_string(),
            "test-model".to_string(), 0.7, vec![],
        );
        fill_history(&mut agent, 20); // 40 条
        // 记录最后 10 条内容
        let last_10: Vec<String> = agent.history[30..].iter().map(|m| {
            if let ConversationMessage::Chat(cm) = m { cm.content.clone() } else { String::new() }
        }).collect();

        agent.compact_history_if_needed().await;

        // 最近 10 条应在压缩后 history 的末尾
        let after_len = agent.history.len();
        let recent: Vec<String> = agent.history[(after_len - 10)..].iter().map(|m| {
            if let ConversationMessage::Chat(cm) = m { cm.content.clone() } else { String::new() }
        }).collect();
        assert_eq!(last_10, recent);
    }

    // --- find_safe_window_end 测试 ---

    #[test]
    fn safe_window_end_stops_before_tool_result() {
        let history = vec![
            make_chat("user", "请执行命令"),
            make_chat("assistant", "好的"),
            ConversationMessage::AssistantToolCalls {
                text: None,
                reasoning_content: None,
                tool_calls: vec![ToolCall { id: "1".into(), name: "shell".into(), arguments: serde_json::json!({}) }],
            },
            ConversationMessage::ToolResult { tool_call_id: "1".into(), content: "结果".into() },
            make_chat("user", "谢谢"),
        ];
        // ideal_end=3 时应退到 2（Chat 消息后），不截断 ToolCalls+ToolResult 对
        let end = find_safe_window_end(&history, 3);
        assert!(end <= 2);
    }

    #[test]
    fn safe_window_end_all_chat_messages() {
        let history: Vec<ConversationMessage> = (0..5)
            .map(|i| make_chat("user", &format!("msg {}", i)))
            .collect();
        assert_eq!(find_safe_window_end(&history, 3), 3);
    }

    // --- format_history_for_summary 测试 ---

    #[test]
    fn format_skips_system_messages() {
        let messages = vec![
            make_chat("system", "系统 prompt"),
            make_chat("user", "你好"),
            make_chat("assistant", "你好！"),
        ];
        let output = format_history_for_summary(&messages);
        assert!(!output.contains("系统 prompt"));
        assert!(output.contains("你好"));
        assert!(output.contains("助手"));
    }

    #[test]
    fn format_truncates_long_content() {
        let long_content = "X".repeat(1000);
        let messages = vec![make_chat("user", &long_content)];
        let output = format_history_for_summary(&messages);
        assert!(output.len() < 600); // 500字符截断 + 标签
    }

    #[test]
    fn format_includes_tool_call_names() {
        let messages = vec![
            ConversationMessage::AssistantToolCalls {
                text: Some("我来执行".to_string()),
                reasoning_content: None,
                tool_calls: vec![
                    ToolCall { id: "1".into(), name: "shell".into(), arguments: serde_json::json!({}) },
                ],
            },
            ConversationMessage::ToolResult { tool_call_id: "1".into(), content: "output".into() },
        ];
        let output = format_history_for_summary(&messages);
        assert!(output.contains("shell"));
        assert!(output.contains("工具调用"));
    }

    // --- summarize_history 测试 ---

    #[tokio::test]
    async fn summarize_returns_llm_text() {
        let provider = MockProvider::new(vec![ChatResponse {
            text: Some("对话摘要：用户询问了一些问题。".to_string()),
            reasoning_content: None,
            tool_calls: vec![],
        }]);
        let agent = Agent::new(
            Box::new(provider), vec![], Box::new(MockMemory),
            test_policy(), "t".into(), "h".into(), "m".into(), 0.7, vec![],
        );
        let messages = vec![make_chat("user", "你好")];
        let result = agent.summarize_history(&messages).await.unwrap();
        assert!(result.contains("摘要"));
    }
}
```

---

## 六、关键注意事项

1. **summarize_history 用 temperature=0.3**：摘要任务需要确定性输出，不要用 agent 当前的 temperature。

2. **COMPACT_WINDOW 不能压缩 ToolCalls+ToolResult 对**：`find_safe_window_end()` 从窗口末尾向前找 Chat 消息作为安全切割点，确保不留悬空的 ToolResult。

3. **摘要失败时 fallback**：网络抖动或 LLM 返回空内容时，`compact_history_if_needed` 捕获错误并调用 `trim_history()` 硬截断，保证 agent 不崩溃。

4. **摘要消息用 role="system"**：格式为 `[对话摘要 - 早期上下文]\n{摘要}` 且 role 为 system，这样 LLM 不会误认为是用户输入，且 `format_history_for_summary` 会自动跳过它（避免递归摘要中的无用内容）。

5. **summarize_history 不传 tools**：摘要 LLM 调用不需要工具列表，传空切片 `&[]` 即可，同时也避免了 LLM 摘要过程中意外触发 tool call。

6. **对 trim_history() 测试的影响**：现有的 `trim_history_works` 测试测的是硬截断逻辑，不受影响，因为 `trim_history()` 方法本身未修改。新增的压缩测试验证新路径。
