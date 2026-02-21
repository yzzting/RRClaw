pub mod sqlite;
pub mod traits;

pub use sqlite::SqliteMemory;
pub use traits::{Memory, MemoryCategory, MemoryEntry};

/// 空操作 Memory 实现，用于不需要持久化记忆的临时 Agent（如 Routine 执行）
pub struct NoopMemory;

#[async_trait::async_trait]
impl Memory for NoopMemory {
    async fn store(&self, _key: &str, _content: &str, _category: MemoryCategory) -> color_eyre::eyre::Result<()> {
        Ok(())
    }

    async fn recall(&self, _query: &str, _limit: usize) -> color_eyre::eyre::Result<Vec<MemoryEntry>> {
        Ok(vec![])
    }

    async fn forget(&self, _key: &str) -> color_eyre::eyre::Result<bool> {
        Ok(false)
    }

    async fn count(&self) -> color_eyre::eyre::Result<usize> {
        Ok(0)
    }
}
