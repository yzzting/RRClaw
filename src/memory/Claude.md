# Memory 模块设计文档

持久化存储和检索 Agent 记忆，支持中文全文搜索。同时维护对话历史表（conversation_history）用于 session 恢复。

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

关联类型：

```rust
MemoryCategory { Conversation, Core, Daily, Custom(String) }
MemoryEntry { key, content, category, created_at, updated_at, relevance_score }
```

## Arc<dyn Memory> 实现（P5）

`Memory` trait 为 `Arc<dyn Memory>` 实现了透传，允许 Agent 和 RoutineEngine 共享同一个 Memory 实例。

```rust
// Arc<dyn Memory> 共享记忆
let memory = Arc::new(SqliteMemory::new(...));
let main_agent = Agent::new(..., memory.clone(), ...);
let routine_engine = RoutineEngine::new(..., memory.clone(), ...);
```

## NoopMemory

用于测试的无操作 Memory 实现（定义在 `memory::mod`）：
- `store()` → 静默丢弃
- `recall()` → 返回空列表
- `forget()` → 返回 false

## SqliteMemory 实现

### 双存储策略

- **SQLite** — 结构化存储（UPSERT by key）
- **tantivy** — 全文搜索索引（jieba 中文分词 + BM25 排序）

### 存储路径

```
~/.rrclaw/data/
├── memory.db          # SQLite（含 memory + conversation_history 两张表）
└── search_index/      # tantivy 索引
```

### conversation_history 表（P1）

独立于 memory 表，专门存储每日对话历史，用于 REPL session 恢复：

```sql
CREATE TABLE conversation_history (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id  TEXT NOT NULL,   -- 当天日期 YYYY-MM-DD
    role        TEXT NOT NULL,   -- "user" | "assistant" | "tool_result" | ...
    content     TEXT NOT NULL,
    created_at  TEXT NOT NULL
);
```

- `save_conversation_history(session_id, messages)` — 批量保存（替换当天记录）
- `load_conversation_history(session_id)` — 加载当天历史，转为 `Vec<ConversationMessage>`

### tantivy Schema

- `key`: STRING | STORED — 精确匹配
- `content`: jieba 分词 + STORED — 全文搜索
- `category`: STRING | STORED — 过滤

### 操作流程

- `store()`: SQLite UPSERT → tantivy delete+add+commit
- `recall()`: tantivy search → 取 key+score → SQLite 查完整 entry
- `forget()`: SQLite DELETE → tantivy delete_term+commit
- `count()`: SQLite COUNT(*)

### 注意事项

- IndexWriter 用 `tokio::sync::Mutex` 包装（tantivy 单线程写）
- jieba 首次加载约 100-200ms（warm-up 在 `create_memory()` 时发生）
- 测试使用 `RAMDirectory` 避免文件系统依赖

## 文件结构

```
src/memory/
├── Claude.md   # 本文件
├── mod.rs      # re-exports + create_memory() + NoopMemory
├── traits.rs   # Memory trait + MemoryEntry + MemoryCategory
└── sqlite.rs   # SqliteMemory（含 conversation_history）
```
