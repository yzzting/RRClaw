use async_trait::async_trait;
use color_eyre::eyre::Result;
use serde::{Deserialize, Serialize};

use crate::providers::ToolSpec;
use crate::security::SecurityPolicy;

/// 工具执行结果
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolResult {
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
    /// 是否可配置解决（如添加到白名单），返回配置建议
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_suggestion: Option<String>,
}

/// 工具抽象
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> serde_json::Value;
    async fn execute(&self, args: serde_json::Value, policy: &SecurityPolicy) -> Result<ToolResult>;

    /// 预验证：在 Supervised 确认前检查安全策略
    /// 返回 None 表示通过，Some(error) 表示拒绝（不会弹出确认提示）
    fn pre_validate(&self, _args: &serde_json::Value, _policy: &SecurityPolicy) -> Option<String> {
        None
    }

    /// 生成 ToolSpec 供 Provider 使用
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters: self.parameters_schema(),
        }
    }
}
