use async_trait::async_trait;
use color_eyre::eyre::Result;
use std::sync::Arc;

use rmcp::model::{CallToolRequestParams, RawContent, ResourceContents, Tool as McpToolDef};
use rmcp::service::{Peer, RoleClient};

use crate::security::SecurityPolicy;
use crate::tools::traits::{Tool, ToolResult};

/// MCP Tool 的 RRClaw 适配器：将一个 MCP server 工具桥接为 RRClaw Tool trait
pub struct McpTool {
    /// 工具在 RRClaw 中的名称，加前缀避免冲突：mcp_{server}_{tool}
    prefixed_name: String,
    /// MCP tool 原始定义（含 description + inputSchema）
    def: McpToolDef,
    /// MCP tool 在服务端的原始名称
    original_name: String,
    /// 共享的 MCP client peer（通过 Arc 共享同一连接）
    peer: Arc<Peer<RoleClient>>,
}

impl McpTool {
    pub fn new(
        server_name: &str,
        def: McpToolDef,
        peer: Arc<Peer<RoleClient>>,
    ) -> Self {
        let original_name = def.name.to_string();
        let prefixed_name = format!("mcp_{}_{}", server_name, original_name);
        Self {
            prefixed_name,
            def,
            original_name,
            peer,
        }
    }
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.prefixed_name
    }

    fn description(&self) -> &str {
        self.def
            .description
            .as_deref()
            .unwrap_or("MCP tool")
    }

    fn parameters_schema(&self) -> serde_json::Value {
        // input_schema 是 Arc<JsonObject>（即 Map<String, Value>），转为 Value::Object
        serde_json::Value::Object(self.def.input_schema.as_ref().clone())
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _policy: &SecurityPolicy,
    ) -> Result<ToolResult> {
        let params = CallToolRequestParams {
            meta: None,
            name: self.original_name.clone().into(),
            arguments: args.as_object().cloned(),
            task: None,
        };

        match self.peer.call_tool(params).await {
            Ok(result) => {
                let mut output_parts: Vec<String> = Vec::new();
                for content in &result.content {
                    // Content = Annotated<RawContent>，Deref 到 RawContent
                    match &**content {
                        RawContent::Text(text_content) => {
                            output_parts.push(text_content.text.clone());
                        }
                        RawContent::Image { .. } => {
                            output_parts.push("[图片内容]".to_string());
                        }
                        RawContent::Resource(res) => {
                            // RawEmbeddedResource.resource 是 ResourceContents
                            match &res.resource {
                                ResourceContents::TextResourceContents { text, .. } => {
                                    output_parts.push(text.clone());
                                }
                                _ => {
                                    output_parts.push("[资源内容]".to_string());
                                }
                            }
                        }
                        _ => {}
                    }
                }
                let output = output_parts.join("\n");
                let is_error = result.is_error.unwrap_or(false);

                Ok(ToolResult {
                    success: !is_error,
                    output: if is_error { String::new() } else { output.clone() },
                    error: if is_error { Some(output) } else { None },
                    ..Default::default()
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("MCP 调用失败: {}", e)),
                ..Default::default()
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn mcp_tool_name_has_prefix() {
        let prefixed = format!("mcp_{}_{}", "filesystem", "read_file");
        assert_eq!(prefixed, "mcp_filesystem_read_file");
        assert!(prefixed.starts_with("mcp_"));
    }

    #[test]
    fn mcp_tool_name_with_special_chars() {
        let prefixed = format!("mcp_{}_{}", "my-server", "list_directory");
        assert_eq!(prefixed, "mcp_my-server_list_directory");
    }
}
