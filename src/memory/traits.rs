use async_trait::async_trait;
use color_eyre::eyre::Result;
use serde::{Deserialize, Serialize};

/// 记忆分类
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryCategory {
    Conversation,
    Core,
    Daily,
    Custom(String),
}

impl MemoryCategory {
    /// 转为字符串表示（用于 SQLite 和 tantivy 存储）
    pub fn as_str(&self) -> &str {
        match self {
            Self::Conversation => "conversation",
            Self::Core => "core",
            Self::Daily => "daily",
            Self::Custom(s) => s,
        }
    }

    /// 从字符串解析
    pub fn parse(s: &str) -> Self {
        match s {
            "conversation" => Self::Conversation,
            "core" => Self::Core,
            "daily" => Self::Daily,
            other => Self::Custom(other.to_string()),
        }
    }
}

/// 记忆条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub key: String,
    pub content: String,
    pub category: MemoryCategory,
    pub created_at: String,
    pub updated_at: String,
    pub relevance_score: f32,
}

/// 记忆抽象
#[async_trait]
pub trait Memory: Send + Sync {
    async fn store(&self, key: &str, content: &str, category: MemoryCategory) -> Result<()>;
    async fn recall(&self, query: &str, limit: usize) -> Result<Vec<MemoryEntry>>;
    async fn forget(&self, key: &str) -> Result<bool>;
    async fn count(&self) -> Result<usize>;
}
