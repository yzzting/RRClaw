use async_trait::async_trait;
use color_eyre::eyre::Result;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

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

/// 流式输出事件
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// 文本 token 增量
    Text(String),
    /// tool call 增量（id, name, arguments 片段）
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments_delta: String,
    },
    /// 流结束，返回完整响应
    Done(ChatResponse),
}

/// AI 模型抽象
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
    /// 默认实现: 回退到 chat_with_tools
    async fn chat_stream(
        &self,
        messages: &[ConversationMessage],
        tools: &[ToolSpec],
        model: &str,
        temperature: f64,
        tx: mpsc::Sender<StreamEvent>,
    ) -> Result<ChatResponse> {
        let resp = self
            .chat_with_tools(messages, tools, model, temperature)
            .await?;
        // 将完整文本作为一次性 Text 事件发送
        if let Some(text) = &resp.text {
            let _ = tx.send(StreamEvent::Text(text.clone())).await;
        }
        let _ = tx.send(StreamEvent::Done(resp.clone())).await;
        Ok(resp)
    }
}
