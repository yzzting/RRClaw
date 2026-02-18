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

/// Anthropic Messages API Provider
pub struct ClaudeProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl ClaudeProvider {
    pub fn new(config: &ProviderConfig) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: config.base_url.trim_end_matches('/').to_string(),
            api_key: config.api_key.clone(),
        }
    }

    /// 构造请求 URL
    fn endpoint(&self) -> String {
        format!("{}/v1/messages", self.base_url)
    }

    /// 从 messages 中提取 system prompt，返回 (system_text, 非system消息)
    fn extract_system(messages: &[ConversationMessage]) -> (Option<String>, Vec<serde_json::Value>) {
        let mut system_parts = Vec::new();
        let mut claude_messages = Vec::new();

        for msg in messages {
            match msg {
                ConversationMessage::Chat(ChatMessage { role, content, .. }) => {
                    if role == "system" {
                        system_parts.push(content.clone());
                    } else {
                        claude_messages.push(serde_json::json!({
                            "role": role,
                            "content": content,
                        }));
                    }
                }
                ConversationMessage::AssistantToolCalls { text, tool_calls, .. } => {
                    let mut content = Vec::new();
                    if let Some(text) = text {
                        content.push(serde_json::json!({
                            "type": "text",
                            "text": text,
                        }));
                    }
                    for tc in tool_calls {
                        content.push(serde_json::json!({
                            "type": "tool_use",
                            "id": tc.id,
                            "name": tc.name,
                            "input": tc.arguments,
                        }));
                    }
                    claude_messages.push(serde_json::json!({
                        "role": "assistant",
                        "content": content,
                    }));
                }
                ConversationMessage::ToolResult {
                    tool_call_id,
                    content,
                } => {
                    claude_messages.push(serde_json::json!({
                        "role": "user",
                        "content": [{
                            "type": "tool_result",
                            "tool_use_id": tool_call_id,
                            "content": content,
                        }],
                    }));
                }
            }
        }

        let system = if system_parts.is_empty() {
            None
        } else {
            Some(system_parts.join("\n\n"))
        };

        (system, claude_messages)
    }

    /// 将 ToolSpec 转换为 Claude tools 格式
    fn build_tools(tools: &[ToolSpec]) -> Vec<serde_json::Value> {
        tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                })
            })
            .collect()
    }

    /// 构造请求体
    fn build_request_body(
        messages: &[ConversationMessage],
        tools: &[ToolSpec],
        model: &str,
        temperature: f64,
        stream: bool,
    ) -> serde_json::Value {
        let (system, claude_messages) = Self::extract_system(messages);

        let mut body = serde_json::json!({
            "model": model,
            "max_tokens": 8192,
            "messages": claude_messages,
            "temperature": temperature,
        });

        if let Some(system_text) = system {
            body["system"] = serde_json::Value::String(system_text);
        }

        let built_tools = Self::build_tools(tools);
        if !built_tools.is_empty() {
            body["tools"] = serde_json::Value::Array(built_tools);
        }

        if stream {
            body["stream"] = serde_json::json!(true);
        }

        body
    }

    /// 解析 Claude 响应
    fn parse_response(body: &ClaudeResponse) -> ChatResponse {
        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();

        for block in &body.content {
            match block.r#type.as_str() {
                "text" => {
                    if let Some(t) = &block.text {
                        text_parts.push(t.clone());
                    }
                }
                "tool_use" => {
                    if let (Some(id), Some(name), Some(input)) =
                        (&block.id, &block.name, &block.input)
                    {
                        tool_calls.push(ToolCall {
                            id: id.clone(),
                            name: name.clone(),
                            arguments: input.clone(),
                        });
                    }
                }
                _ => {}
            }
        }

        let text = if text_parts.is_empty() {
            None
        } else {
            Some(text_parts.join(""))
        };

        ChatResponse { text, reasoning_content: None, tool_calls }
    }
}

#[async_trait]
impl Provider for ClaudeProvider {
    async fn chat_with_tools(
        &self,
        messages: &[ConversationMessage],
        tools: &[ToolSpec],
        model: &str,
        temperature: f64,
    ) -> Result<ChatResponse> {
        let body = Self::build_request_body(messages, tools, model, temperature, false);

        debug!("Claude API 请求: {} model={}", self.endpoint(), model);
        trace!("请求体: {}", serde_json::to_string_pretty(&body).unwrap_or_default());

        let resp = self
            .client
            .post(self.endpoint())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .wrap_err("发送请求失败")?;

        let status = resp.status();
        let resp_text = resp.text().await.wrap_err("读取响应失败")?;

        if !status.is_success() {
            return Err(color_eyre::eyre::eyre!(
                "API 请求失败 ({}): {}",
                status,
                resp_text
            ));
        }

        let parsed: ClaudeResponse =
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

        debug!("Claude API 流式请求: {} model={}", self.endpoint(), model);
        trace!("请求体: {}", serde_json::to_string_pretty(&body).unwrap_or_default());

        let resp = self
            .client
            .post(self.endpoint())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .wrap_err("发送流式请求失败")?;

        let status = resp.status();
        if !status.is_success() {
            let err_text = resp.text().await.wrap_err("读取错误响应失败")?;
            return Err(color_eyre::eyre::eyre!(
                "Claude API 流式请求失败 ({}): {}",
                status,
                err_text
            ));
        }

        debug!("Claude API 流式响应状态: {}", status);

        // 累积状态
        let mut text_parts = Vec::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut current_tool_input = String::new();
        let mut line_buf = String::new();

        let mut byte_stream = resp.bytes_stream();
        while let Some(chunk) = byte_stream.next().await {
            let chunk = chunk.wrap_err("读取 SSE 数据块失败")?;
            let chunk_str = String::from_utf8_lossy(&chunk);
            line_buf.push_str(&chunk_str);

            while let Some(newline_pos) = line_buf.find('\n') {
                let line = line_buf[..newline_pos].trim().to_string();
                line_buf = line_buf[newline_pos + 1..].to_string();

                if line.is_empty() {
                    continue;
                }

                let json_str = match line.strip_prefix("data: ") {
                    Some(s) => s,
                    None => continue,
                };

                let event: serde_json::Value = match serde_json::from_str(json_str) {
                    Ok(v) => v,
                    Err(e) => {
                        warn!("Claude SSE JSON 解析失败: {} line={}", e, json_str);
                        continue;
                    }
                };

                let event_type = event["type"].as_str().unwrap_or("");
                match event_type {
                    "content_block_start" => {
                        let block = &event["content_block"];
                        if block["type"].as_str() == Some("tool_use") {
                            // 新 tool_use block 开始
                            let id = block["id"].as_str().unwrap_or("").to_string();
                            let name = block["name"].as_str().unwrap_or("").to_string();
                            tool_calls.push(ToolCall {
                                id,
                                name,
                                arguments: serde_json::Value::Object(serde_json::Map::new()),
                            });
                            current_tool_input.clear();
                        }
                    }
                    "content_block_delta" => {
                        let delta = &event["delta"];
                        match delta["type"].as_str() {
                            Some("text_delta") => {
                                if let Some(text) = delta["text"].as_str() {
                                    if !text.is_empty() {
                                        text_parts.push(text.to_string());
                                        let _ = tx.send(StreamEvent::Text(text.to_string())).await;
                                    }
                                }
                            }
                            Some("input_json_delta") => {
                                if let Some(partial) = delta["partial_json"].as_str() {
                                    current_tool_input.push_str(partial);
                                    let idx = if tool_calls.is_empty() { 0 } else { tool_calls.len() - 1 };
                                    let _ = tx
                                        .send(StreamEvent::ToolCallDelta {
                                            index: idx,
                                            id: None,
                                            name: None,
                                            arguments_delta: partial.to_string(),
                                        })
                                        .await;
                                }
                            }
                            _ => {}
                        }
                    }
                    "content_block_stop" => {
                        // 当前 block 结束，如果是 tool_use，解析累积的 input
                        if !current_tool_input.is_empty() {
                            if let Some(tc) = tool_calls.last_mut() {
                                tc.arguments = serde_json::from_str(&current_tool_input)
                                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                            }
                            current_tool_input.clear();
                        }
                    }
                    "message_stop" => {
                        break;
                    }
                    _ => {}
                }
            }
        }

        let text = if text_parts.is_empty() {
            None
        } else {
            Some(text_parts.join(""))
        };

        let response = ChatResponse { text, reasoning_content: None, tool_calls };
        let _ = tx.send(StreamEvent::Done(response.clone())).await;

        debug!(
            "Claude 流式响应完成: text_len={}, tool_calls={}",
            response.text.as_ref().map(|t| t.len()).unwrap_or(0),
            response.tool_calls.len()
        );

        Ok(response)
    }
}

// --- Claude 响应结构体（仅用于反序列化）---

#[derive(Debug, Deserialize)]
struct ClaudeResponse {
    content: Vec<ClaudeContentBlock>,
}

#[derive(Debug, Deserialize)]
struct ClaudeContentBlock {
    r#type: String,
    text: Option<String>,
    id: Option<String>,
    name: Option<String>,
    input: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_construction() {
        let config = ProviderConfig {
            base_url: "https://api.anthropic.com".to_string(),
            api_key: "test".to_string(),
            model: "claude-sonnet-4-5-20250929".to_string(),
            auth_style: Some("x-api-key".to_string()),
        };
        let provider = ClaudeProvider::new(&config);
        assert_eq!(provider.endpoint(), "https://api.anthropic.com/v1/messages");
    }

    #[test]
    fn extract_system_separates_correctly() {
        let msgs = vec![
            ConversationMessage::Chat(ChatMessage {
                role: "system".to_string(),
                content: "You are RRClaw.".to_string(),
                reasoning_content: None,
            }),
            ConversationMessage::Chat(ChatMessage {
                role: "user".to_string(),
                content: "Hello".to_string(),
                reasoning_content: None,
            }),
        ];
        let (system, claude_msgs) = ClaudeProvider::extract_system(&msgs);
        assert_eq!(system.as_deref(), Some("You are RRClaw."));
        assert_eq!(claude_msgs.len(), 1);
        assert_eq!(claude_msgs[0]["role"], "user");
    }

    #[test]
    fn extract_system_merges_multiple() {
        let msgs = vec![
            ConversationMessage::Chat(ChatMessage {
                role: "system".to_string(),
                content: "Part 1".to_string(),
                reasoning_content: None,
            }),
            ConversationMessage::Chat(ChatMessage {
                role: "system".to_string(),
                content: "Part 2".to_string(),
                reasoning_content: None,
            }),
        ];
        let (system, _) = ClaudeProvider::extract_system(&msgs);
        assert_eq!(system.as_deref(), Some("Part 1\n\nPart 2"));
    }

    #[test]
    fn extract_tool_calls_and_results() {
        let msgs = vec![
            ConversationMessage::AssistantToolCalls {
                text: Some("Checking...".to_string()),
                reasoning_content: None,
                tool_calls: vec![ToolCall {
                    id: "toolu_1".to_string(),
                    name: "shell".to_string(),
                    arguments: serde_json::json!({"command": "ls"}),
                }],
            },
            ConversationMessage::ToolResult {
                tool_call_id: "toolu_1".to_string(),
                content: "file.txt".to_string(),
            },
        ];
        let (_, claude_msgs) = ClaudeProvider::extract_system(&msgs);
        assert_eq!(claude_msgs.len(), 2);

        // assistant with content array
        assert_eq!(claude_msgs[0]["role"], "assistant");
        let content = claude_msgs[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "tool_use");
        assert_eq!(content[1]["name"], "shell");

        // tool result as user
        assert_eq!(claude_msgs[1]["role"], "user");
        let result_content = claude_msgs[1]["content"].as_array().unwrap();
        assert_eq!(result_content[0]["type"], "tool_result");
        assert_eq!(result_content[0]["tool_use_id"], "toolu_1");
    }

    #[test]
    fn build_tools_uses_input_schema() {
        let tools = vec![ToolSpec {
            name: "shell".to_string(),
            description: "Run command".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let built = ClaudeProvider::build_tools(&tools);
        assert_eq!(built[0]["input_schema"]["type"], "object");
        assert!(built[0].get("parameters").is_none());
    }

    #[test]
    fn parse_text_response() {
        let resp = ClaudeResponse {
            content: vec![ClaudeContentBlock {
                r#type: "text".to_string(),
                text: Some("Hello!".to_string()),
                id: None,
                name: None,
                input: None,
            }],
        };
        let parsed = ClaudeProvider::parse_response(&resp);
        assert_eq!(parsed.text.as_deref(), Some("Hello!"));
        assert!(parsed.tool_calls.is_empty());
    }

    #[test]
    fn parse_tool_use_response() {
        let resp = ClaudeResponse {
            content: vec![
                ClaudeContentBlock {
                    r#type: "text".to_string(),
                    text: Some("Let me run that.".to_string()),
                    id: None,
                    name: None,
                    input: None,
                },
                ClaudeContentBlock {
                    r#type: "tool_use".to_string(),
                    text: None,
                    id: Some("toolu_abc".to_string()),
                    name: Some("shell".to_string()),
                    input: Some(serde_json::json!({"command": "ls"})),
                },
            ],
        };
        let parsed = ClaudeProvider::parse_response(&resp);
        assert_eq!(parsed.text.as_deref(), Some("Let me run that."));
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].id, "toolu_abc");
        assert_eq!(parsed.tool_calls[0].name, "shell");
    }
}
