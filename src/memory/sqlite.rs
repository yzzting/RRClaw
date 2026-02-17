use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use color_eyre::eyre::{Context, Result};
use rusqlite::{params, Connection};
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::*;
use tantivy::{doc, Index, IndexWriter, ReloadPolicy, TantivyDocument, Term};
use tokio::sync::Mutex;

use crate::providers::ConversationMessage;
use super::traits::{Memory, MemoryCategory, MemoryEntry};

/// SQLite + tantivy 记忆实现
pub struct SqliteMemory {
    db: Arc<Mutex<Connection>>,
    index: Index,
    index_writer: Arc<Mutex<IndexWriter>>,
    key_field: Field,
    content_field: Field,
    category_field: Field,
}

impl SqliteMemory {
    /// 从文件路径创建（生产用）
    pub fn open(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir).wrap_err("创建数据目录失败")?;

        let db_path = data_dir.join("memory.db");
        let db = Connection::open(&db_path).wrap_err("打开 SQLite 失败")?;

        let index_path = data_dir.join("search_index");
        std::fs::create_dir_all(&index_path).wrap_err("创建索引目录失败")?;

        let (schema, key_field, content_field, category_field) = Self::build_schema();
        let dir = tantivy::directory::MmapDirectory::open(&index_path)
            .wrap_err("打开 tantivy 目录失败")?;
        let index =
            Index::open_or_create(dir, schema.clone()).wrap_err("创建 tantivy 索引失败")?;

        Self::finish_init(db, index, schema, key_field, content_field, category_field)
    }

    /// 从内存创建（测试用）
    pub fn in_memory() -> Result<Self> {
        let db = Connection::open_in_memory().wrap_err("打开内存 SQLite 失败")?;

        let (schema, key_field, content_field, category_field) = Self::build_schema();
        let index = Index::create_in_ram(schema.clone());

        Self::finish_init(db, index, schema, key_field, content_field, category_field)
    }

    fn build_schema() -> (Schema, Field, Field, Field) {
        let mut builder = Schema::builder();

        let key_field = builder.add_text_field("key", STRING | STORED);

        let content_options = TextOptions::default()
            .set_indexing_options(
                TextFieldIndexing::default()
                    .set_tokenizer("jieba")
                    .set_index_option(IndexRecordOption::WithFreqsAndPositions),
            )
            .set_stored();
        let content_field = builder.add_text_field("content", content_options);

        let category_field = builder.add_text_field("category", STRING | STORED);

        (builder.build(), key_field, content_field, category_field)
    }

    fn finish_init(
        db: Connection,
        index: Index,
        _schema: Schema,
        key_field: Field,
        content_field: Field,
        category_field: Field,
    ) -> Result<Self> {
        // 注册 jieba 分词器
        index
            .tokenizers()
            .register("jieba", tantivy_jieba::JiebaTokenizer::new());

        let index_writer = index
            .writer(50_000_000) // 50MB heap
            .wrap_err("创建 IndexWriter 失败")?;

        // 初始化 SQLite 表
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS memories (
                key TEXT PRIMARY KEY,
                content TEXT NOT NULL,
                category TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS conversation_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                seq INTEGER NOT NULL,
                payload TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_conv_session ON conversation_history(session_id);",
        )
        .wrap_err("创建数据库表失败")?;

        Ok(Self {
            db: Arc::new(Mutex::new(db)),
            index,
            index_writer: Arc::new(Mutex::new(index_writer)),
            key_field,
            content_field,
            category_field,
        })
    }

    /// 保存对话历史到指定 session
    pub async fn save_conversation_history(
        &self,
        session_id: &str,
        history: &[ConversationMessage],
    ) -> Result<()> {
        let db = self.db.lock().await;

        // 清除该 session 的旧历史
        db.execute(
            "DELETE FROM conversation_history WHERE session_id = ?1",
            params![session_id],
        )
        .wrap_err("清除旧对话历史失败")?;

        let now = chrono::Utc::now().to_rfc3339();
        for (i, msg) in history.iter().enumerate() {
            let payload = serde_json::to_string(msg).wrap_err("序列化对话消息失败")?;
            db.execute(
                "INSERT INTO conversation_history (session_id, seq, payload, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![session_id, i as i64, payload, now],
            )
            .wrap_err("写入对话历史失败")?;
        }

        Ok(())
    }

    /// 加载指定 session 的对话历史
    pub async fn load_conversation_history(
        &self,
        session_id: &str,
    ) -> Result<Vec<ConversationMessage>> {
        let db = self.db.lock().await;
        let mut stmt = db
            .prepare(
                "SELECT payload FROM conversation_history WHERE session_id = ?1 ORDER BY seq ASC",
            )
            .wrap_err("准备查询对话历史失败")?;

        let messages: Vec<ConversationMessage> = stmt
            .query_map(params![session_id], |row| {
                let payload: String = row.get(0)?;
                Ok(payload)
            })
            .wrap_err("查询对话历史失败")?
            .filter_map(|r| r.ok())
            .filter_map(|payload| serde_json::from_str(&payload).ok())
            .collect();

        Ok(messages)
    }

    /// 从 SQLite 根据 key 查询完整条目
    async fn get_from_sqlite(&self, key: &str) -> Result<Option<MemoryEntry>> {
        let db = self.db.lock().await;
        let mut stmt = db
            .prepare("SELECT key, content, category, created_at, updated_at FROM memories WHERE key = ?1")
            .wrap_err("准备查询语句失败")?;

        let entry = stmt
            .query_row(params![key], |row| {
                Ok(MemoryEntry {
                    key: row.get(0)?,
                    content: row.get(1)?,
                    category: MemoryCategory::parse(&row.get::<_, String>(2)?),
                    created_at: row.get(3)?,
                    updated_at: row.get(4)?,
                    relevance_score: 0.0,
                })
            })
            .ok();

        Ok(entry)
    }
}

#[async_trait]
impl Memory for SqliteMemory {
    async fn store(&self, key: &str, content: &str, category: MemoryCategory) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let category_str = category.as_str().to_string();

        // 1. SQLite UPSERT
        {
            let db = self.db.lock().await;
            db.execute(
                "INSERT INTO memories (key, content, category, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(key) DO UPDATE SET content=?2, category=?3, updated_at=?5",
                params![key, content, category_str, now, now],
            )
            .wrap_err("SQLite 写入失败")?;
        }

        // 2. tantivy 索引更新
        {
            let mut writer = self.index_writer.lock().await;
            writer.delete_term(Term::from_field_text(self.key_field, key));
            writer.add_document(doc!(
                self.key_field => key,
                self.content_field => content,
                self.category_field => category_str,
            ))?;
            writer.commit().wrap_err("tantivy commit 失败")?;
        }

        Ok(())
    }

    async fn recall(&self, query: &str, limit: usize) -> Result<Vec<MemoryEntry>> {
        let reader = self
            .index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()
            .wrap_err("创建 IndexReader 失败")?;
        let searcher = reader.searcher();

        let query_parser = QueryParser::for_index(&self.index, vec![self.content_field]);
        let parsed_query = query_parser
            .parse_query(query)
            .wrap_err("解析搜索查询失败")?;

        let top_docs = searcher
            .search(&parsed_query, &TopDocs::with_limit(limit))
            .wrap_err("搜索失败")?;

        let mut results = Vec::new();
        for (score, doc_address) in top_docs {
            let doc: TantivyDocument = searcher.doc(doc_address).wrap_err("读取文档失败")?;
            if let Some(key_value) = doc.get_first(self.key_field) {
                if let Some(key) = key_value.as_str() {
                    if let Some(mut entry) = self.get_from_sqlite(key).await? {
                        entry.relevance_score = score;
                        results.push(entry);
                    }
                }
            }
        }

        Ok(results)
    }

    async fn forget(&self, key: &str) -> Result<bool> {
        // 1. SQLite DELETE
        let deleted = {
            let db = self.db.lock().await;
            db.execute("DELETE FROM memories WHERE key = ?1", params![key])
                .wrap_err("SQLite 删除失败")?
        };

        // 2. tantivy 删除
        {
            let mut writer = self.index_writer.lock().await;
            writer.delete_term(Term::from_field_text(self.key_field, key));
            writer.commit().wrap_err("tantivy commit 失败")?;
        }

        Ok(deleted > 0)
    }

    async fn count(&self) -> Result<usize> {
        let db = self.db.lock().await;
        let count: usize = db
            .query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))
            .wrap_err("查询计数失败")?;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn create_test_memory() -> SqliteMemory {
        SqliteMemory::in_memory().unwrap()
    }

    #[tokio::test]
    async fn store_and_count() {
        let mem = create_test_memory().await;

        mem.store("key1", "Hello world", MemoryCategory::Core)
            .await
            .unwrap();
        assert_eq!(mem.count().await.unwrap(), 1);

        mem.store("key2", "Another entry", MemoryCategory::Daily)
            .await
            .unwrap();
        assert_eq!(mem.count().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn store_upsert() {
        let mem = create_test_memory().await;

        mem.store("key1", "original", MemoryCategory::Core)
            .await
            .unwrap();
        mem.store("key1", "updated", MemoryCategory::Core)
            .await
            .unwrap();

        assert_eq!(mem.count().await.unwrap(), 1);

        let results = mem.recall("updated", 10).await.unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].content, "updated");
    }

    #[tokio::test]
    async fn recall_by_content() {
        let mem = create_test_memory().await;

        mem.store("rust", "Rust 是一门系统编程语言", MemoryCategory::Core)
            .await
            .unwrap();
        mem.store("python", "Python 是一门脚本语言", MemoryCategory::Core)
            .await
            .unwrap();

        let results = mem.recall("Rust 编程", 10).await.unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].key, "rust");
        assert!(results[0].relevance_score > 0.0);
    }

    #[tokio::test]
    async fn recall_chinese_search() {
        let mem = create_test_memory().await;

        mem.store("meeting", "今天的会议讨论了人工智能的应用", MemoryCategory::Daily)
            .await
            .unwrap();
        mem.store("lunch", "午餐吃了红烧肉", MemoryCategory::Daily)
            .await
            .unwrap();

        let results = mem.recall("人工智能", 10).await.unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].key, "meeting");
    }

    #[tokio::test]
    async fn forget_removes_entry() {
        let mem = create_test_memory().await;

        mem.store("temp", "temporary data", MemoryCategory::Conversation)
            .await
            .unwrap();
        assert_eq!(mem.count().await.unwrap(), 1);

        let deleted = mem.forget("temp").await.unwrap();
        assert!(deleted);
        assert_eq!(mem.count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn forget_nonexistent_returns_false() {
        let mem = create_test_memory().await;
        let deleted = mem.forget("nonexistent").await.unwrap();
        assert!(!deleted);
    }

    #[tokio::test]
    async fn recall_empty_returns_empty() {
        let mem = create_test_memory().await;
        let results = mem.recall("anything", 10).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn recall_respects_limit() {
        let mem = create_test_memory().await;

        for i in 0..5 {
            mem.store(
                &format!("entry_{}", i),
                &format!("这是第 {} 条测试数据", i),
                MemoryCategory::Core,
            )
            .await
            .unwrap();
        }

        let results = mem.recall("测试数据", 2).await.unwrap();
        assert!(results.len() <= 2);
    }

    #[tokio::test]
    async fn save_and_load_conversation_history() {
        use crate::providers::{ChatMessage, ConversationMessage, ToolCall};

        let mem = create_test_memory().await;
        let session_id = "test-session-2024";

        let history = vec![
            ConversationMessage::Chat(ChatMessage {
                role: "user".to_string(),
                content: "你好".to_string(),
            }),
            ConversationMessage::AssistantToolCalls {
                text: Some("让我查看".to_string()),
                tool_calls: vec![ToolCall {
                    id: "call_1".to_string(),
                    name: "shell".to_string(),
                    arguments: serde_json::json!({"command": "ls"}),
                }],
            },
            ConversationMessage::ToolResult {
                tool_call_id: "call_1".to_string(),
                content: "file.txt".to_string(),
            },
            ConversationMessage::Chat(ChatMessage {
                role: "assistant".to_string(),
                content: "目录中有 file.txt".to_string(),
            }),
        ];

        // 保存
        mem.save_conversation_history(session_id, &history)
            .await
            .unwrap();

        // 加载
        let loaded = mem.load_conversation_history(session_id).await.unwrap();
        assert_eq!(loaded.len(), 4);

        // 验证内容
        let payload = serde_json::to_string(&loaded[0]).unwrap();
        assert!(payload.contains("你好"));

        let payload = serde_json::to_string(&loaded[3]).unwrap();
        assert!(payload.contains("file.txt"));
    }

    #[tokio::test]
    async fn load_nonexistent_session_returns_empty() {
        let mem = create_test_memory().await;
        let loaded = mem.load_conversation_history("nonexistent").await.unwrap();
        assert!(loaded.is_empty());
    }

    #[tokio::test]
    async fn save_overwrites_previous_history() {
        use crate::providers::{ChatMessage, ConversationMessage};

        let mem = create_test_memory().await;
        let session_id = "overwrite-test";

        let history1 = vec![ConversationMessage::Chat(ChatMessage {
            role: "user".to_string(),
            content: "first".to_string(),
        })];
        mem.save_conversation_history(session_id, &history1)
            .await
            .unwrap();

        let history2 = vec![
            ConversationMessage::Chat(ChatMessage {
                role: "user".to_string(),
                content: "second".to_string(),
            }),
            ConversationMessage::Chat(ChatMessage {
                role: "assistant".to_string(),
                content: "reply".to_string(),
            }),
        ];
        mem.save_conversation_history(session_id, &history2)
            .await
            .unwrap();

        let loaded = mem.load_conversation_history(session_id).await.unwrap();
        assert_eq!(loaded.len(), 2);
        let payload = serde_json::to_string(&loaded[0]).unwrap();
        assert!(payload.contains("second"));
    }

    #[tokio::test]
    async fn memory_category_roundtrip() {
        let mem = create_test_memory().await;

        mem.store("custom", "custom category test", MemoryCategory::Custom("project_a".to_string()))
            .await
            .unwrap();

        let results = mem.recall("custom category", 10).await.unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].category, MemoryCategory::Custom("project_a".to_string()));
    }
}
