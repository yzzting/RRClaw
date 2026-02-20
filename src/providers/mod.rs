pub mod claude;
pub mod compatible;
pub mod reliable;
pub mod traits;

pub use reliable::{ReliableProvider, RetryConfig};
pub use traits::{
    ChatMessage, ChatResponse, ConversationMessage, Provider, StreamEvent, ToolCall, ToolSpec,
    ToolStatusKind,
};

use crate::config::ProviderConfig;

/// 根据配置创建 Provider 实例
pub fn create_provider(config: &ProviderConfig) -> Box<dyn Provider> {
    match config.auth_style.as_deref() {
        Some("x-api-key") => Box::new(claude::ClaudeProvider::new(config)),
        _ => Box::new(compatible::CompatibleProvider::new(config)),
    }
}
