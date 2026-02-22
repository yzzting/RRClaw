// 每个集成测试文件只使用 MockProvider 的部分方法，dead_code 为预期行为
#![allow(dead_code)]

//! 测试专用 MockProvider
//!
//! 实现 Provider trait，预置响应队列（VecDeque），
//! 每次 chat_with_tools 调用从队列头部弹出一个 ChatResponse。
//!
//! # 用途
//!
//! - E2E 测试：无需真实 HTTP，直接在内存中返回预设响应
//! - 队列空时返回 Err，便于检测意外的额外 LLM 调用
//!
//! # 使用示例
//!
//! ```rust
//! let mock = MockProvider::new(vec![
//!     MockProvider::direct_route(),           // Phase 1 路由
//!     MockProvider::text("你好！"),            // Phase 2 回复
//! ]);
//! ```

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;
use color_eyre::eyre::{eyre, Result};

use rrclaw::providers::{ChatResponse, ConversationMessage, Provider, ToolCall, ToolSpec};

/// 可插拔 Mock Provider，按队列顺序返回预设响应
pub struct MockProvider {
    responses: Mutex<VecDeque<ChatResponse>>,
}

impl MockProvider {
    /// 创建带有预设响应的 MockProvider
    pub fn new(responses: Vec<ChatResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
        }
    }

    /// 构造 Phase 1 路由结果：Direct（无需加载 skill，直接执行）
    pub fn direct_route() -> ChatResponse {
        ChatResponse {
            text: Some("{\"direct\": true}".to_string()),
            reasoning_content: None,
            tool_calls: vec![],
        }
    }

    /// 构造纯文本回复（无 tool call）
    pub fn text(content: &str) -> ChatResponse {
        ChatResponse {
            text: Some(content.to_string()),
            reasoning_content: None,
            tool_calls: vec![],
        }
    }

    /// 构造单个 tool call 回复
    pub fn tool_call(id: &str, name: &str, args: serde_json::Value) -> ChatResponse {
        ChatResponse {
            text: None,
            reasoning_content: None,
            tool_calls: vec![ToolCall {
                id: id.to_string(),
                name: name.to_string(),
                arguments: args,
            }],
        }
    }

    /// 构造 shell tool call
    pub fn shell_call(id: &str, command: &str) -> ChatResponse {
        Self::tool_call(id, "shell", serde_json::json!({"command": command}))
    }
}

#[async_trait]
impl Provider for MockProvider {
    async fn chat_with_tools(
        &self,
        _messages: &[ConversationMessage],
        _tools: &[ToolSpec],
        _model: &str,
        _temperature: f64,
    ) -> Result<ChatResponse> {
        let mut queue = self.responses.lock().expect("MockProvider mutex 中毒");
        queue
            .pop_front()
            .ok_or_else(|| eyre!("MockProvider 响应队列已空：意外的额外 LLM 调用"))
    }
}
