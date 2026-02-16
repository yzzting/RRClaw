use async_trait::async_trait;
use color_eyre::eyre::{Context, Result};
use serde::Deserialize;

use crate::config::ProviderConfig;

use super::traits::{
    ChatMessage, ChatResponse, ConversationMessage, Provider, ToolCall, ToolSpec,
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
                ConversationMessage::Chat(ChatMessage { role, content }) => {
                    result.push(serde_json::json!({
                        "role": role,
                        "content": content,
                    }));
                }
                ConversationMessage::AssistantToolCalls { text, tool_calls } => {
                    let mut obj = serde_json::json!({
                        "role": "assistant",
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

    /// 解析 OpenAI 响应
    fn parse_response(body: &OpenAIResponse) -> ChatResponse {
        let choice = match body.choices.first() {
            Some(c) => c,
            None => {
                return ChatResponse {
                    text: None,
                    tool_calls: vec![],
                }
            }
        };

        let text = choice.message.content.clone();
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

        ChatResponse { text, tool_calls }
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
        let mut body = serde_json::json!({
            "model": model,
            "messages": Self::build_messages(messages),
            "temperature": temperature,
        });

        let built_tools = Self::build_tools(tools);
        if !built_tools.is_empty() {
            body["tools"] = serde_json::Value::Array(built_tools);
        }

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
            }),
            ConversationMessage::Chat(ChatMessage {
                role: "user".to_string(),
                content: "Hello".to_string(),
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
