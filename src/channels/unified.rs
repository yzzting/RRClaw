//! 统一消息抽象
//!
//! 用于支持多 Channel（CLI + Telegram）统一接入 Agent

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

/// 消息来源
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MessageSource {
    /// CLI 终端
    Cli,
    /// Telegram
    Telegram { chat_id: i64 },
}

impl MessageSource {
    /// 获取来源标识字符串
    pub fn as_str(&self) -> &'static str {
        match self {
            MessageSource::Cli => "cli",
            MessageSource::Telegram { .. } => "telegram",
        }
    }
}

/// 统一消息结构
///
/// 用于 CLI 和 Telegram 两种渠道的消息统一抽象
#[derive(Debug)]
pub struct UnifiedMessage {
    /// 消息来源
    pub source: MessageSource,
    /// 消息内容
    pub content: String,
    /// 回复通道（用于将 Agent 响应发送回原始渠道）
    pub reply_tx: oneshot::Sender<String>,
}

impl UnifiedMessage {
    /// 创建新的统一消息
    pub fn new(source: MessageSource, content: String, reply_tx: oneshot::Sender<String>) -> Self {
        Self {
            source,
            content,
            reply_tx,
        }
    }

    /// 从 CLI 创建消息
    pub fn from_cli(content: String) -> (Self, oneshot::Receiver<String>) {
        let (reply_tx, reply_rx) = oneshot::channel();
        (
            Self {
                source: MessageSource::Cli,
                content,
                reply_tx,
            },
            reply_rx,
        )
    }

    /// 从 Telegram 创建消息
    pub fn from_telegram(chat_id: i64, content: String) -> (Self, oneshot::Receiver<String>) {
        let (reply_tx, reply_rx) = oneshot::channel();
        (
            Self {
                source: MessageSource::Telegram { chat_id },
                content,
                reply_tx,
            },
            reply_rx,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unified_message_creation() {
        let (tx, _rx) = oneshot::channel();
        let msg = UnifiedMessage::new(MessageSource::Cli, "Hello".to_string(), tx);
        assert_eq!(msg.content, "Hello");
    }

    #[test]
    fn test_message_source_telegram() {
        let source = MessageSource::Telegram { chat_id: 12345 };
        match source {
            MessageSource::Telegram { chat_id } => {
                assert_eq!(chat_id, 12345);
            }
            _ => panic!("Expected Telegram source"),
        }
    }

    #[test]
    fn test_message_source_cli() {
        let source = MessageSource::Cli;
        match source {
            MessageSource::Cli => {}
            _ => panic!("Expected Cli source"),
        }
    }

    #[test]
    fn test_message_source_as_str() {
        assert_eq!(MessageSource::Cli.as_str(), "cli");
        assert_eq!(
            MessageSource::Telegram { chat_id: 123 }.as_str(),
            "telegram"
        );
    }

    #[test]
    fn test_from_cli() {
        let (msg, _rx) = UnifiedMessage::from_cli("test".to_string());
        assert!(matches!(msg.source, MessageSource::Cli));
        assert_eq!(msg.content, "test");
    }

    #[test]
    fn test_from_telegram() {
        let (msg, _rx) = UnifiedMessage::from_telegram(12345, "hello".to_string());
        assert!(matches!(
            msg.source,
            MessageSource::Telegram { chat_id: 12345 }
        ));
        assert_eq!(msg.content, "hello");
    }
}
