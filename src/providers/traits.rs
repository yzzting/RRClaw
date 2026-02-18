use async_trait::async_trait;
use color_eyre::eyre::Result;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// 聊天消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    /// DeepSeek/MiniMax 思考模式的推理内容（同一 Turn 内多轮 tool call 需回传）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
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
    /// DeepSeek/MiniMax 思考模式的推理内容
    pub reasoning_content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
}

/// 对话消息（支持多轮 tool call 交互）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConversationMessage {
    /// 普通聊天消息（system/user/assistant）
    Chat(ChatMessage),
    /// 助手发起的 tool call 响应
    AssistantToolCalls {
        text: Option<String>,
        /// DeepSeek/MiniMax 思考模式的推理内容（同一 Turn 内多轮 tool call 需回传）
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_content: Option<String>,
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
    /// 工具执行状态（用于 TUI 显示）
    ToolStatus {
        name: String,
        status: ToolStatusKind,
    },
    /// LLM 思考中（等待首个 token）
    Thinking,
    /// 流结束，返回完整响应
    Done(ChatResponse),
}

/// 工具执行状态类型
#[derive(Debug, Clone)]
pub enum ToolStatusKind {
    /// 开始执行
    Running(String),
    /// 执行成功（摘要）
    Success(String),
    /// 执行失败（错误信息）
    Failed(String),
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
