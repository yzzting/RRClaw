use async_trait::async_trait;
use color_eyre::eyre::{Context, Result};
use futures_util::StreamExt;
use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::{debug, trace, warn};

use crate::config::ProviderConfig;

use super::traits::{
    ChatMessage, ChatResponse, ConversationMessage, Provider, StreamEvent, ToolCall, ToolSpec,
};

/// OpenAI 兼容协议 Provider（GLM/MiniMax/DeepSeek/GPT）
pub struct CompatibleProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl CompatibleProvider {
    pub fn new(config: &ProviderConfig) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: config.base_url.trim_end_matches('/').to_string(),
            api_key: config.api_key.clone(),
        }
    }

    /// 构造请求 URL
    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }

    /// 将 ConversationMessage 转换为 OpenAI messages 格式
    fn build_messages(messages: &[ConversationMessage]) -> Vec<serde_json::Value> {
        let mut result = Vec::new();

        for msg in messages {
            match msg {
                ConversationMessage::Chat(ChatMessage { role, content, .. }) => {
                    let mut obj = serde_json::json!({
                        "role": role,
                        "content": content,
                    });
                    // DeepSeek Reasoner 要求 assistant 消息包含 reasoning_content
                    if role == "assistant" {
                        obj["reasoning_content"] = serde_json::json!("");
                    }
                    result.push(obj);
                }
                ConversationMessage::AssistantToolCalls { text, tool_calls, .. } => {
                    let mut obj = serde_json::json!({
                        "role": "assistant",
                        "reasoning_content": serde_json::json!(""),
                    });
                    if let Some(text) = text {
                        obj["content"] = serde_json::Value::String(text.clone());
                    }
                    if !tool_calls.is_empty() {
                        obj["tool_calls"] = tool_calls
                            .iter()
                            .map(|tc| {
                                serde_json::json!({
                                    "id": tc.id,
                                    "type": "function",
                                    "function": {
                                        "name": tc.name,
                                        "arguments": tc.arguments.to_string(),
                                    }
                                })
                            })
                            .collect();
                    }
                    result.push(obj);
                }
                ConversationMessage::ToolResult {
                    tool_call_id,
                    content,
                } => {
                    result.push(serde_json::json!({
                        "role": "tool",
                        "tool_call_id": tool_call_id,
                        "content": content,
                    }));
                }
            }
        }

        result
    }

    /// 将 ToolSpec 转换为 OpenAI tools 格式
    fn build_tools(tools: &[ToolSpec]) -> Vec<serde_json::Value> {
        tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    }
                })
            })
            .collect()
    }

    /// 构造请求体（stream/非stream 共用）
    fn build_request_body(
        messages: &[ConversationMessage],
        tools: &[ToolSpec],
        model: &str,
        temperature: f64,
        stream: bool,
    ) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": model,
            "messages": Self::build_messages(messages),
            "temperature": temperature,
        });

        let built_tools = Self::build_tools(tools);
        if !built_tools.is_empty() {
            body["tools"] = serde_json::Value::Array(built_tools);
        }

        if stream {
            body["stream"] = serde_json::json!(true);
        }

        body
    }

    /// 解析 OpenAI 响应
    fn parse_response(body: &OpenAIResponse) -> ChatResponse {
        let choice = match body.choices.first() {
            Some(c) => c,
            None => {
                return ChatResponse {
                    text: None,
                    reasoning_content: None,
                    tool_calls: vec![],
                }
            }
        };

        // 优先 content，回退到 reasoning_content（DeepSeek Reasoner）
        let text = choice.message.content.clone()
            .filter(|s| !s.is_empty())
            .or_else(|| choice.message.reasoning_content.clone()
                .filter(|s| !s.is_empty()));
        let tool_calls = choice
            .message
            .tool_calls
            .as_ref()
            .map(|tcs| {
                tcs.iter()
                    .map(|tc| ToolCall {
                        id: tc.id.clone(),
                        name: tc.function.name.clone(),
                        arguments: serde_json::from_str(&tc.function.arguments)
                            .unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
                    })
                    .collect()
            })
            .unwrap_or_default();

        ChatResponse { text, reasoning_content: None, tool_calls }
    }
}

#[async_trait]
impl Provider for CompatibleProvider {
    async fn chat_with_tools(
        &self,
        messages: &[ConversationMessage],
        tools: &[ToolSpec],
        model: &str,
        temperature: f64,
    ) -> Result<ChatResponse> {
        let body = Self::build_request_body(messages, tools, model, temperature, false);

        debug!("API 请求: {} model={}", self.endpoint(), model);
        trace!("请求体: {}", serde_json::to_string_pretty(&body).unwrap_or_default());

        let resp = self
            .client
            .post(self.endpoint())
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .wrap_err("发送请求失败")?;

        let status = resp.status();
        let resp_text = resp.text().await.wrap_err("读取响应失败")?;

        debug!("API 响应状态: {}", status);
        trace!("响应体: {}", resp_text);

        if !status.is_success() {
            return Err(color_eyre::eyre::eyre!(
                "API 请求失败 ({}): {}",
                status,
                resp_text
            ));
        }

        let parsed: OpenAIResponse =
            serde_json::from_str(&resp_text).wrap_err("解析响应 JSON 失败")?;

        Ok(Self::parse_response(&parsed))
    }

    async fn chat_stream(
        &self,
        messages: &[ConversationMessage],
        tools: &[ToolSpec],
        model: &str,
        temperature: f64,
        tx: mpsc::Sender<StreamEvent>,
    ) -> Result<ChatResponse> {
        let body = Self::build_request_body(messages, tools, model, temperature, true);

        debug!("API 流式请求: {} model={}", self.endpoint(), model);
        trace!("请求体: {}", serde_json::to_string_pretty(&body).unwrap_or_default());

        let resp = self
            .client
            .post(self.endpoint())
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .wrap_err("发送流式请求失败")?;

        let status = resp.status();
        if !status.is_success() {
            let err_text = resp.text().await.wrap_err("读取错误响应失败")?;
            return Err(color_eyre::eyre::eyre!(
                "API 流式请求失败 ({}): {}",
                status,
                err_text
            ));
        }

        debug!("API 流式响应状态: {}", status);

        // 累积状态
        let mut full_text = String::new();
        // tool_calls 累积: index → (id, name, arguments_buffer)
        let mut tool_calls_acc: Vec<(String, String, String)> = Vec::new();
        let mut line_buf = String::new();

        let mut byte_stream = resp.bytes_stream();
        while let Some(chunk) = byte_stream.next().await {
            let chunk = chunk.wrap_err("读取 SSE 数据块失败")?;
            let chunk_str = String::from_utf8_lossy(&chunk);

            // SSE 协议: 每行 "data: {...}\n\n"，可能一个 chunk 包含多行
            line_buf.push_str(&chunk_str);

            // 按行处理
            while let Some(newline_pos) = line_buf.find('\n') {
                let line = line_buf[..newline_pos].trim().to_string();
                line_buf = line_buf[newline_pos + 1..].to_string();

                if line.is_empty() {
                    continue;
                }

                if line == "data: [DONE]" {
                    break;
                }

                let json_str = match line.strip_prefix("data: ") {
                    Some(s) => s,
                    None => continue,
                };

                let parsed: SSEStreamResponse = match serde_json::from_str(json_str) {
                    Ok(p) => p,
                    Err(e) => {
                        warn!("SSE JSON 解析失败: {} line={}", e, json_str);
                        continue;
                    }
                };

                if let Some(choice) = parsed.choices.first() {
                    // 文本增量（优先 content，回退到 reasoning_content）
                    let text_delta = choice.delta.content.as_deref()
                        .filter(|s| !s.is_empty())
                        .or_else(|| choice.delta.reasoning_content.as_deref()
                            .filter(|s| !s.is_empty()));
                    if let Some(content) = text_delta {
                        full_text.push_str(content);
                        let _ = tx.send(StreamEvent::Text(content.to_string())).await;
                    }

                    // tool call 增量
                    if let Some(tc_deltas) = &choice.delta.tool_calls {
                        for tc in tc_deltas {
                            let idx = tc.index.unwrap_or(0);

                            // 扩展 tool_calls_acc 到足够大小
                            while tool_calls_acc.len() <= idx {
                                tool_calls_acc.push((String::new(), String::new(), String::new()));
                            }

                            if let Some(id) = &tc.id {
                                tool_calls_acc[idx].0 = id.clone();
                            }
                            if let Some(func) = &tc.function {
                                if let Some(name) = &func.name {
                                    tool_calls_acc[idx].1 = name.clone();
                                }
                                if let Some(args) = &func.arguments {
                                    tool_calls_acc[idx].2.push_str(args);
                                    let _ = tx
                                        .send(StreamEvent::ToolCallDelta {
                                            index: idx,
                                            id: tc.id.clone(),
                                            name: tc
                                                .function
                                                .as_ref()
                                                .and_then(|f| f.name.clone()),
                                            arguments_delta: args.clone(),
                                        })
                                        .await;
                                }
                            }
                        }
                    }
                }
            }
        }

        // 组装最终 ChatResponse
        let tool_calls: Vec<ToolCall> = tool_calls_acc
            .into_iter()
            .filter(|(id, name, _)| !id.is_empty() || !name.is_empty())
            .map(|(id, name, args)| ToolCall {
                id,
                name,
                arguments: serde_json::from_str(&args)
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
            })
            .collect();

        let response = ChatResponse {
            text: if full_text.is_empty() {
                None
            } else {
                Some(full_text)
            },
            reasoning_content: None,
            tool_calls,
        };

        let _ = tx.send(StreamEvent::Done(response.clone())).await;

        debug!(
            "流式响应完成: text_len={}, tool_calls={}",
            response.text.as_ref().map(|t| t.len()).unwrap_or(0),
            response.tool_calls.len()
        );

        Ok(response)
    }
}

// --- OpenAI 响应结构体（仅用于反序列化）---

#[derive(Debug, Deserialize)]
struct OpenAIResponse {
    choices: Vec<OpenAIChoice>,
}

#[derive(Debug, Deserialize)]
struct OpenAIChoice {
    message: OpenAIMessage,
}

#[derive(Debug, Deserialize)]
struct OpenAIMessage {
    content: Option<String>,
    /// DeepSeek Reasoner 的思考过程
    reasoning_content: Option<String>,
    tool_calls: Option<Vec<OpenAIToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OpenAIToolCall {
    id: String,
    function: OpenAIFunction,
}

#[derive(Debug, Deserialize)]
struct OpenAIFunction {
    name: String,
    arguments: String,
}

// --- SSE 流式响应结构体 ---

#[derive(Debug, Deserialize)]
struct SSEStreamResponse {
    choices: Vec<SSEStreamChoice>,
}

#[derive(Debug, Deserialize)]
struct SSEStreamChoice {
    delta: SSEDelta,
}

#[derive(Debug, Deserialize)]
struct SSEDelta {
    content: Option<String>,
    /// DeepSeek Reasoner 的思考过程（可能包含最终回答）
    reasoning_content: Option<String>,
    tool_calls: Option<Vec<SSEToolCallDelta>>,
}

#[derive(Debug, Deserialize)]
struct SSEToolCallDelta {
    index: Option<usize>,
    id: Option<String>,
    function: Option<SSEFunctionDelta>,
}

#[derive(Debug, Deserialize)]
struct SSEFunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_construction() {
        let config = ProviderConfig {
            base_url: "https://api.deepseek.com/v1".to_string(),
            api_key: "test".to_string(),
            model: "deepseek-chat".to_string(),
            auth_style: None,
        };
        let provider = CompatibleProvider::new(&config);
        assert_eq!(
            provider.endpoint(),
            "https://api.deepseek.com/v1/chat/completions"
        );
    }

    #[test]
    fn endpoint_strips_trailing_slash() {
        let config = ProviderConfig {
            base_url: "https://api.openai.com/v1/".to_string(),
            api_key: "test".to_string(),
            model: "gpt-4o".to_string(),
            auth_style: None,
        };
        let provider = CompatibleProvider::new(&config);
        assert_eq!(
            provider.endpoint(),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn build_messages_chat() {
        let msgs = vec![
            ConversationMessage::Chat(ChatMessage {
                role: "system".to_string(),
                content: "You are helpful.".to_string(),
                reasoning_content: None,
            }),
            ConversationMessage::Chat(ChatMessage {
                role: "user".to_string(),
                content: "Hello".to_string(),
                reasoning_content: None,
            }),
        ];
        let built = CompatibleProvider::build_messages(&msgs);
        assert_eq!(built.len(), 2);
        assert_eq!(built[0]["role"], "system");
        assert_eq!(built[1]["content"], "Hello");
    }

    #[test]
    fn build_messages_with_tool_calls() {
        let msgs = vec![
            ConversationMessage::AssistantToolCalls {
                text: Some("Let me check.".to_string()),
                reasoning_content: None,
                tool_calls: vec![ToolCall {
                    id: "call_1".to_string(),
                    name: "shell".to_string(),
                    arguments: serde_json::json!({"command": "ls"}),
                }],
            },
            ConversationMessage::ToolResult {
                tool_call_id: "call_1".to_string(),
                content: "file1.txt\nfile2.txt".to_string(),
            },
        ];
        let built = CompatibleProvider::build_messages(&msgs);
        assert_eq!(built.len(), 2);
        assert_eq!(built[0]["role"], "assistant");
        assert_eq!(built[0]["tool_calls"][0]["function"]["name"], "shell");
        assert_eq!(built[1]["role"], "tool");
        assert_eq!(built[1]["tool_call_id"], "call_1");
    }

    #[test]
    fn build_tools_format() {
        let tools = vec![ToolSpec {
            name: "shell".to_string(),
            description: "Execute a command".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"}
                },
                "required": ["command"]
            }),
        }];
        let built = CompatibleProvider::build_tools(&tools);
        assert_eq!(built.len(), 1);
        assert_eq!(built[0]["type"], "function");
        assert_eq!(built[0]["function"]["name"], "shell");
        assert!(built[0]["function"]["parameters"]["properties"]["command"].is_object());
    }

    #[test]
    fn parse_text_response() {
        let resp = OpenAIResponse {
            choices: vec![OpenAIChoice {
                message: OpenAIMessage {
                    content: Some("Hello!".to_string()),
                    reasoning_content: None,
                    tool_calls: None,
                },
            }],
        };
        let parsed = CompatibleProvider::parse_response(&resp);
        assert_eq!(parsed.text.as_deref(), Some("Hello!"));
        assert!(parsed.tool_calls.is_empty());
    }

    #[test]
    fn parse_tool_call_response() {
        let resp = OpenAIResponse {
            choices: vec![OpenAIChoice {
                message: OpenAIMessage {
                    content: None,
                    reasoning_content: None,
                    tool_calls: Some(vec![OpenAIToolCall {
                        id: "call_abc".to_string(),
                        function: OpenAIFunction {
                            name: "shell".to_string(),
                            arguments: r#"{"command":"ls"}"#.to_string(),
                        },
                    }]),
                },
            }],
        };
        let parsed = CompatibleProvider::parse_response(&resp);
        assert!(parsed.text.is_none());
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].name, "shell");
        assert_eq!(parsed.tool_calls[0].arguments["command"], "ls");
    }

    #[test]
    fn parse_empty_choices() {
        let resp = OpenAIResponse { choices: vec![] };
        let parsed = CompatibleProvider::parse_response(&resp);
        assert!(parsed.text.is_none());
        assert!(parsed.tool_calls.is_empty());
    }
}
