//! IPC protocol types for daemon ↔ client communication.
//!
//! Uses JSON Lines over Unix socket (one JSON object per line).

use serde::{Deserialize, Serialize};

// ─── Client → Daemon ─────────────────────────────────────────────────────────

/// Messages sent from the CLI client to the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Start a new conversation or continue chatting.
    Message { session_id: String, content: String },

    /// Response to a tool confirmation request (Supervised mode).
    ConfirmResponse { request_id: String, approved: bool },
}

// ─── Daemon → Client ─────────────────────────────────────────────────────────

/// Messages sent from the daemon to the CLI client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonMessage {
    /// A streaming token (partial response).
    Token { content: String },

    /// Agent finished its response.
    Done,

    /// Request tool execution confirmation from the user (Supervised mode).
    Confirm {
        request_id: String,
        tool: String,
        args: serde_json::Value,
    },

    /// An error occurred while processing the request.
    Error { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_message_serialize() {
        let msg = ClientMessage::Message {
            session_id: "cli-abc".to_string(),
            content: "hello".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"message\""));
        assert!(json.contains("\"session_id\":\"cli-abc\""));
    }

    #[test]
    fn client_confirm_response_serialize() {
        let msg = ClientMessage::ConfirmResponse {
            request_id: "req-1".to_string(),
            approved: true,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"confirm_response\""));
        assert!(json.contains("\"approved\":true"));
    }

    #[test]
    fn daemon_token_serialize() {
        let msg = DaemonMessage::Token {
            content: "hello".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"token\""));
    }

    #[test]
    fn daemon_done_serialize() {
        let msg = DaemonMessage::Done;
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"type":"done"}"#);
    }

    #[test]
    fn daemon_confirm_serialize() {
        let msg = DaemonMessage::Confirm {
            request_id: "r1".to_string(),
            tool: "shell".to_string(),
            args: serde_json::json!({"command": "ls"}),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"confirm\""));
        assert!(json.contains("\"tool\":\"shell\""));
    }

    #[test]
    fn daemon_error_serialize() {
        let msg = DaemonMessage::Error {
            message: "something went wrong".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"error\""));
    }

    #[test]
    fn client_message_roundtrip() {
        let msg = ClientMessage::Message {
            session_id: "s1".to_string(),
            content: "你好".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ClientMessage::Message {
                session_id,
                content,
            } => {
                assert_eq!(session_id, "s1");
                assert_eq!(content, "你好");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn daemon_message_roundtrip() {
        let msg = DaemonMessage::Token {
            content: "world".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: DaemonMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            DaemonMessage::Token { content } => assert_eq!(content, "world"),
            _ => panic!("wrong variant"),
        }
    }
}
