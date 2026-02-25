pub mod cli;
#[cfg(feature = "telegram")]
pub mod telegram;
pub mod unified;

use serde::{Deserialize, Serialize};

pub use unified::{MessageSource, UnifiedMessage};

/// 通道消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMessage {
    pub id: String,
    pub sender: String,
    pub content: String,
    pub channel: String,
    pub timestamp: u64,
}
