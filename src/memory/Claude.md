# Memory 模块

## 职责

持久化存储和检索 Agent 记忆，支持中文全文搜索。

## Memory trait

```rust
#[async_trait]
pub trait Memory: Send + Sync {
    async fn store(&self, key: &str, content: &str, category: MemoryCategory) -> Result<()>;
    async fn recall(&self, query: &str, limit: usize) -> Result<Vec<MemoryEntry>>;
    async fn forget(&self, key: &str) -> Result<bool>;
    async fn count(&self) -> Result<usize>;
}
```

## 关联类型

```rust
MemoryCategory { Conversation, Core, Daily, Custom(String) }
MemoryEntry { key, content, category, created_at, updated_at, relevance_score }
```

## SqliteMemory 实现

双存储策略:
- **SQLite** — 结构化存储（UPSERT by key）
- **tantivy** — 全文搜索索引（jieba 中文分词 + BM25）

### 存储路径

```
~/.rrclaw/data/
├── memory.db          # SQLite
└── search_index/      # tantivy 索引
```

### tantivy Schema

- `key`: STRING | STORED — 精确匹配
- `content`: jieba 分词 + STORED — 全文搜索
- `category`: STRING | STORED — 过滤

### 流程

- `store()`: SQLite UPSERT → tantivy delete+add+commit
- `recall()`: tantivy search → 取 key+score → SQLite 查完整 entry
- `forget()`: SQLite DELETE → tantivy delete_term+commit
- `count()`: SQLite COUNT(*)

### 注意

- IndexWriter 用 `tokio::sync::Mutex` 包装（单线程写）
- jieba 首次加载约 100-200ms
- 测试使用 RAMDirectory 避免文件系统依赖

## 文件结构

- `mod.rs` — re-exports + `create_memory()` 工厂
- `traits.rs` — Memory trait + MemoryEntry + MemoryCategory
- `sqlite.rs` — SqliteMemory 实现
