use color_eyre::eyre::Result;
use tracing::{debug, info, warn};

use crate::memory::{Memory, MemoryCategory};
use crate::providers::{ChatMessage, ConversationMessage, Provider, ToolSpec};
use crate::security::{AutonomyLevel, SecurityPolicy};
use crate::tools::Tool;

const MAX_TOOL_ITERATIONS: usize = 10;
const MAX_HISTORY_SIZE: usize = 50;

/// AI Agent 核心
pub struct Agent {
    provider: Box<dyn Provider>,
    tools: Vec<Box<dyn Tool>>,
    memory: Box<dyn Memory>,
    policy: SecurityPolicy,
    model: String,
    temperature: f64,
    history: Vec<ConversationMessage>,
}

impl Agent {
    pub fn new(
        provider: Box<dyn Provider>,
        tools: Vec<Box<dyn Tool>>,
        memory: Box<dyn Memory>,
        policy: SecurityPolicy,
        model: String,
        temperature: f64,
    ) -> Self {
        Self {
            provider,
            tools,
            memory,
            policy,
            model,
            temperature,
            history: Vec::new(),
        }
    }

    /// 处理一条用户消息，返回 AI 最终回复
    pub async fn process_message(&mut self, user_msg: &str) -> Result<String> {
        // 1. Memory recall
        let memories = self.memory.recall(user_msg, 5).await.unwrap_or_default();

        // 2. 构造 system prompt
        let system_prompt = self.build_system_prompt(&memories);

        // 3. 添加用户消息到 history
        self.history.push(ConversationMessage::Chat(ChatMessage {
            role: "user".to_string(),
            content: user_msg.to_string(),
        }));

        // 4. Tool call 循环
        let tool_specs: Vec<ToolSpec> = self.tools.iter().map(|t| t.spec()).collect();
        let mut final_text = String::new();

        for iteration in 0..MAX_TOOL_ITERATIONS {
            // 构造消息列表：system + history
            let mut messages = vec![ConversationMessage::Chat(ChatMessage {
                role: "system".to_string(),
                content: system_prompt.clone(),
            })];
            messages.extend(self.history.clone());

            debug!("iteration={}, history_len={}", iteration, self.history.len());

            // 调用 Provider
            let response = self
                .provider
                .chat_with_tools(&messages, &tool_specs, &self.model, self.temperature)
                .await?;

            debug!(
                "response: text={:?}, tool_calls_count={}",
                response.text.as_deref().map(|t| truncate_str(t, 100)),
                response.tool_calls.len()
            );

            if response.tool_calls.is_empty() {
                // 无 tool calls — 最终回复
                final_text = response.text.unwrap_or_default();
                if final_text.is_empty() {
                    warn!("模型返回空文本回复");
                }
                self.history.push(ConversationMessage::Chat(ChatMessage {
                    role: "assistant".to_string(),
                    content: final_text.clone(),
                }));
                break;
            }

            // 有 tool calls — 记录并逐个执行
            self.history
                .push(ConversationMessage::AssistantToolCalls {
                    text: response.text.clone(),
                    tool_calls: response.tool_calls.clone(),
                });

            for tc in &response.tool_calls {
                info!("执行工具: {} args={}", tc.name, tc.arguments);
                let result = self.execute_tool(&tc.name, tc.arguments.clone()).await;
                debug!("工具结果: {}", truncate_str(&result, 200));
                self.history.push(ConversationMessage::ToolResult {
                    tool_call_id: tc.id.clone(),
                    content: result,
                });
            }
        }

        // 5. Memory store — 保存对话摘要
        let summary = format!("User: {}\nAssistant: {}", user_msg, final_text);
        let key = format!("conv_{}", chrono::Utc::now().timestamp_millis());
        let _ = self
            .memory
            .store(&key, &summary, MemoryCategory::Conversation)
            .await;

        // 6. 裁剪 history
        self.trim_history();

        Ok(final_text)
    }

    /// 执行工具，返回结果文本
    async fn execute_tool(&self, name: &str, args: serde_json::Value) -> String {
        let tool = match self.tools.iter().find(|t| t.name() == name) {
            Some(t) => t,
            None => return format!("未知工具: {}", name),
        };

        match tool.execute(args, &self.policy).await {
            Ok(result) => {
                if result.success {
                    result.output
                } else {
                    format!(
                        "工具执行失败: {}",
                        result.error.unwrap_or_else(|| "未知错误".to_string())
                    )
                }
            }
            Err(e) => format!("工具执行错误: {}", e),
        }
    }

    /// 构造 system prompt
    fn build_system_prompt(&self, memories: &[crate::memory::MemoryEntry]) -> String {
        let mut parts = Vec::new();

        // [1] 身份描述
        parts.push("你是 RRClaw，一个安全优先的 AI 助手。".to_string());

        // [2] 可用工具描述
        if !self.tools.is_empty() {
            let mut tools_desc = "你可以使用以下工具:\n".to_string();
            for tool in &self.tools {
                tools_desc.push_str(&format!("- {}: {}\n", tool.name(), tool.description()));
            }
            parts.push(tools_desc);
        }

        // [3] 安全规则
        let security_rules = match self.policy.autonomy {
            AutonomyLevel::ReadOnly => "当前为只读模式，不要尝试执行任何工具。",
            AutonomyLevel::Supervised => "执行工具前必须向用户展示命令并获得确认。",
            AutonomyLevel::Full => "你可以自主执行工具，但须遵守白名单限制。",
        };
        parts.push(security_rules.to_string());

        // [4] 记忆上下文
        if !memories.is_empty() {
            let mut memory_section = "[相关记忆]\n".to_string();
            for entry in memories {
                memory_section.push_str(&format!("- {}\n", entry.content));
            }
            parts.push(memory_section);
        }

        // [5] 环境信息
        let workspace = self.policy.workspace_dir.display();
        parts.push(format!(
            "工作目录: {}\n当前时间: {}",
            workspace,
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        ));

        parts.join("\n\n")
    }

    /// 裁剪 history 保持在最大限制内
    fn trim_history(&mut self) {
        if self.history.len() > MAX_HISTORY_SIZE {
            let excess = self.history.len() - MAX_HISTORY_SIZE;
            self.history.drain(..excess);
        }
    }
}

/// UTF-8 安全的字符串截断
fn truncate_str(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    // 找到不超过 max_bytes 的最近 char boundary
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...(共{}字节)", &s[..end], s.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryEntry;
    use crate::providers::{ChatResponse, ToolCall};
    use crate::tools::ToolResult;
    use std::path::PathBuf;

    // --- Mock Provider ---
    struct MockProvider {
        responses: std::sync::Mutex<Vec<ChatResponse>>,
    }

    impl MockProvider {
        fn new(responses: Vec<ChatResponse>) -> Self {
            Self {
                responses: std::sync::Mutex::new(responses),
            }
        }
    }

    #[async_trait::async_trait]
    impl Provider for MockProvider {
        async fn chat_with_tools(
            &self,
            _messages: &[ConversationMessage],
            _tools: &[ToolSpec],
            _model: &str,
            _temperature: f64,
        ) -> Result<ChatResponse> {
            let mut responses = self.responses.lock().unwrap();
            if responses.is_empty() {
                Ok(ChatResponse {
                    text: Some("默认回复".to_string()),
                    tool_calls: vec![],
                })
            } else {
                Ok(responses.remove(0))
            }
        }
    }

    // --- Mock Memory ---
    struct MockMemory;

    #[async_trait::async_trait]
    impl Memory for MockMemory {
        async fn store(&self, _key: &str, _content: &str, _category: MemoryCategory) -> Result<()> {
            Ok(())
        }
        async fn recall(&self, _query: &str, _limit: usize) -> Result<Vec<MemoryEntry>> {
            Ok(vec![])
        }
        async fn forget(&self, _key: &str) -> Result<bool> {
            Ok(false)
        }
        async fn count(&self) -> Result<usize> {
            Ok(0)
        }
    }

    // --- Mock Tool ---
    struct MockTool {
        tool_name: String,
        result: String,
    }

    #[async_trait::async_trait]
    impl Tool for MockTool {
        fn name(&self) -> &str {
            &self.tool_name
        }
        fn description(&self) -> &str {
            "Mock tool"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn execute(
            &self,
            _args: serde_json::Value,
            _policy: &SecurityPolicy,
        ) -> Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: self.result.clone(),
                error: None,
            })
        }
    }

    fn test_policy() -> SecurityPolicy {
        SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            allowed_commands: vec!["ls".to_string()],
            workspace_dir: PathBuf::from("/tmp"),
            blocked_paths: vec![],
        }
    }

    #[tokio::test]
    async fn simple_text_response() {
        let provider = MockProvider::new(vec![ChatResponse {
            text: Some("你好！".to_string()),
            tool_calls: vec![],
        }]);

        let mut agent = Agent::new(
            Box::new(provider),
            vec![],
            Box::new(MockMemory),
            test_policy(),
            "test-model".to_string(),
            0.7,
        );

        let reply = agent.process_message("你好").await.unwrap();
        assert_eq!(reply, "你好！");
    }

    #[tokio::test]
    async fn tool_call_then_text() {
        let provider = MockProvider::new(vec![
            // First response: tool call
            ChatResponse {
                text: Some("让我查看一下".to_string()),
                tool_calls: vec![ToolCall {
                    id: "call_1".to_string(),
                    name: "shell".to_string(),
                    arguments: serde_json::json!({"command": "ls"}),
                }],
            },
            // Second response: final text
            ChatResponse {
                text: Some("目录中有 file.txt".to_string()),
                tool_calls: vec![],
            },
        ]);

        let mock_tool = MockTool {
            tool_name: "shell".to_string(),
            result: "file.txt".to_string(),
        };

        let mut agent = Agent::new(
            Box::new(provider),
            vec![Box::new(mock_tool)],
            Box::new(MockMemory),
            test_policy(),
            "test-model".to_string(),
            0.7,
        );

        let reply = agent.process_message("列出文件").await.unwrap();
        assert_eq!(reply, "目录中有 file.txt");
    }

    #[tokio::test]
    async fn unknown_tool_handled() {
        let provider = MockProvider::new(vec![
            ChatResponse {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "call_1".to_string(),
                    name: "nonexistent".to_string(),
                    arguments: serde_json::json!({}),
                }],
            },
            ChatResponse {
                text: Some("抱歉".to_string()),
                tool_calls: vec![],
            },
        ]);

        let mut agent = Agent::new(
            Box::new(provider),
            vec![],
            Box::new(MockMemory),
            test_policy(),
            "test-model".to_string(),
            0.7,
        );

        let reply = agent.process_message("test").await.unwrap();
        assert_eq!(reply, "抱歉");
    }

    #[test]
    fn system_prompt_contains_identity() {
        let agent = Agent::new(
            Box::new(MockProvider::new(vec![])),
            vec![],
            Box::new(MockMemory),
            test_policy(),
            "test".to_string(),
            0.7,
        );
        let prompt = agent.build_system_prompt(&[]);
        assert!(prompt.contains("RRClaw"));
    }

    #[test]
    fn system_prompt_includes_tools() {
        let tool = MockTool {
            tool_name: "shell".to_string(),
            result: String::new(),
        };
        let agent = Agent::new(
            Box::new(MockProvider::new(vec![])),
            vec![Box::new(tool)],
            Box::new(MockMemory),
            test_policy(),
            "test".to_string(),
            0.7,
        );
        let prompt = agent.build_system_prompt(&[]);
        assert!(prompt.contains("shell"));
    }

    #[test]
    fn trim_history_works() {
        let mut agent = Agent::new(
            Box::new(MockProvider::new(vec![])),
            vec![],
            Box::new(MockMemory),
            test_policy(),
            "test".to_string(),
            0.7,
        );

        for i in 0..60 {
            agent.history.push(ConversationMessage::Chat(ChatMessage {
                role: "user".to_string(),
                content: format!("msg {}", i),
            }));
        }
        assert_eq!(agent.history.len(), 60);
        agent.trim_history();
        assert_eq!(agent.history.len(), MAX_HISTORY_SIZE);
    }
}
