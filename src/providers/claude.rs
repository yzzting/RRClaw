use async_trait::async_trait;
use color_eyre::eyre::{Context, Result};
use serde::Deserialize;

use crate::config::ProviderConfig;

use super::traits::{
    ChatMessage, ChatResponse, ConversationMessage, Provider, ToolCall, ToolSpec,
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
                ConversationMessage::Chat(ChatMessage { role, content }) => {
                    if role == "system" {
                        system_parts.push(content.clone());
                    } else {
                        claude_messages.push(serde_json::json!({
                            "role": role,
                            "content": content,
                        }));
                    }
                }
                ConversationMessage::AssistantToolCalls { text, tool_calls } => {
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

        ChatResponse { text, tool_calls }
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
            }),
            ConversationMessage::Chat(ChatMessage {
                role: "user".to_string(),
                content: "Hello".to_string(),
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
            }),
            ConversationMessage::Chat(ChatMessage {
                role: "system".to_string(),
                content: "Part 2".to_string(),
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
