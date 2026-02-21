pub mod sqlite;
pub mod traits;

pub use sqlite::SqliteMemory;
pub use traits::{Memory, MemoryCategory, MemoryEntry};

/// 空操作 Memory 实现，用于不需要持久化记忆的临时 Agent（如 Routine 执行）
pub struct NoopMemory;

/// 允许将 Arc<dyn Memory> 直接装箱传给 Agent（Routine 共享 Memory 场景）
#[async_trait::async_trait]
impl Memory for std::sync::Arc<dyn Memory> {
    async fn store(&self, key: &str, content: &str, category: MemoryCategory) -> color_eyre::eyre::Result<()> {
        (**self).store(key, content, category).await
    }

    async fn recall(&self, query: &str, limit: usize) -> color_eyre::eyre::Result<Vec<MemoryEntry>> {
        (**self).recall(query, limit).await
    }

    async fn forget(&self, key: &str) -> color_eyre::eyre::Result<bool> {
        (**self).forget(key).await
    }

    async fn count(&self) -> color_eyre::eyre::Result<usize> {
        (**self).count().await
    }
}

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
