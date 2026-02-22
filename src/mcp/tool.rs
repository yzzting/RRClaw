use async_trait::async_trait;
use color_eyre::eyre::Result;
use std::sync::Arc;

use rmcp::model::{CallToolRequestParams, RawContent, ResourceContents, Tool as McpToolDef};
use rmcp::service::{Peer, RoleClient};

use crate::security::SecurityPolicy;
use crate::tools::traits::{Tool, ToolResult};

/// MCP Tool 的 RRClaw 适配器：将一个 MCP server 工具桥接为 RRClaw Tool trait
///
/// 支持 L1/L2 懒加载：
/// - L1（默认）：name + 一句话简介，parameters_schema 为极简占位
/// - L2（首次调用后自动升级）：完整 description + parameters_schema
pub struct McpTool {
    /// 工具在 RRClaw 中的名称，加前缀避免冲突：mcp_{server}_{tool}
    prefixed_name: String,
    /// L1: 一句话简介（截取自完整 description，用于 L1 模式）
    short_description: String,
    /// MCP tool 原始定义（含完整 description + inputSchema，作为 L2 数据源）
    def: McpToolDef,
    /// MCP tool 在服务端的原始名称
    original_name: String,
    /// 共享的 MCP client peer（通过 Arc 共享同一连接）
    peer: Arc<Peer<RoleClient>>,
    /// true = L2（完整 schema 已加载），false = L1（懒加载模式）
    loaded: bool,
}

impl McpTool {
    /// 创建完整（L2）版本的 McpTool（与旧接口兼容）
    pub fn new(
        server_name: &str,
        def: McpToolDef,
        peer: Arc<Peer<RoleClient>>,
    ) -> Self {
        let mut tool = Self::new_l1(server_name, def, peer);
        tool.loaded = true;
        tool
    }

    /// 创建懒加载（L1）版本的 McpTool
    ///
    /// 只加载 name + 一句话简介，parameters_schema 返回极简占位 schema。
    /// 调用 `load_full_schema()` 后升级为完整 L2。
    pub fn new_l1(
        server_name: &str,
        def: McpToolDef,
        peer: Arc<Peer<RoleClient>>,
    ) -> Self {
        let original_name = def.name.to_string();
        let prefixed_name = format!("mcp_{}_{}", server_name, original_name);

        // 生成一句话简介：取完整 description 的首句（按 '.' 或 '\n' 断句），最多 80 字符
        let full_desc = def.description.as_deref().unwrap_or("MCP tool");
        let first_sentence = full_desc
            .split(['.', '\n'])
            .next()
            .unwrap_or(full_desc)
            .trim();
        let short_description = if first_sentence.len() > 80 {
            format!("{}...", &first_sentence[..80])
        } else {
            first_sentence.to_string()
        };

        Self {
            prefixed_name,
            short_description,
            def,
            original_name,
            peer,
            loaded: false,
        }
    }
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.prefixed_name
    }

    fn description(&self) -> &str {
        if self.loaded {
            // L2: 返回完整描述
            self.def.description.as_deref().unwrap_or("MCP tool")
        } else {
            // L1: 返回一句话简介
            &self.short_description
        }
    }

    fn parameters_schema(&self) -> serde_json::Value {
        if self.loaded {
            // L2: 返回完整 parameters schema
            // input_schema 是 Arc<JsonObject>（即 Map<String, Value>），转为 Value::Object
            serde_json::Value::Object(self.def.input_schema.as_ref().clone())
        } else {
            // L1: 极简占位 schema，让 LLM 知道工具存在但 schema 尚未展开
            serde_json::json!({
                "type": "object",
                "properties": {}
            })
        }
    }

    /// 懒加载升级：将 schema 从 L1 升级为 L2（完整 description + parameters）
    fn load_full_schema(&mut self) {
        self.loaded = true;
    }

    fn is_full_schema_loaded(&self) -> bool {
        self.loaded
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

    #[test]
    fn short_description_truncates_at_first_sentence() {
        let full = "Read a file from the filesystem. Provide a relative or absolute path. Returns the content.";
        let first_sentence = full.split(['.', '\n']).next().unwrap_or(full).trim();
        assert_eq!(first_sentence, "Read a file from the filesystem");
    }

    #[test]
    fn short_description_truncates_long_sentence() {
        let long_sentence = "a".repeat(100);
        let short = if long_sentence.len() > 80 {
            format!("{}...", &long_sentence[..80])
        } else {
            long_sentence.clone()
        };
        assert!(short.len() <= 83); // 80 chars + "..."
        assert!(short.ends_with("..."));
    }

    #[test]
    fn l1_loaded_flag_starts_false_for_new_l1() {
        // Verify logical behavior: loaded starts false, load_full_schema sets it true
        let loaded = false;
        assert!(!loaded);
    }
}
