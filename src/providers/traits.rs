use async_trait::async_trait;
use color_eyre::eyre::Result;
use serde::{Deserialize, Serialize};

/// 聊天消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

/// 模型请求的工具调用
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// 模型响应
#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub text: Option<String>,
    pub tool_calls: Vec<ToolCall>,
}

/// 对话消息（支持多轮 tool call 交互）
#[derive(Debug, Clone)]
pub enum ConversationMessage {
    /// 普通聊天消息（system/user/assistant）
    Chat(ChatMessage),
    /// 助手发起的 tool call 响应
    AssistantToolCalls {
        text: Option<String>,
        tool_calls: Vec<ToolCall>,
    },
    /// 工具执行结果
    ToolResult {
        tool_call_id: String,
        content: String,
    },
}

/// 工具规格描述（传递给 LLM）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// AI 模型抽象
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
