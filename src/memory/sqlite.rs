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

use super::traits::{Memory, MemoryCategory, MemoryEntry};
use crate::providers::ConversationMessage;

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
    /// 从文件路径创建（生产用）。
    /// 根据 `Config::get_language()` 自动选择分词器：
    /// - English → `en_stem`（英文词干还原，tantivy 内置）
    /// - Chinese  → `jieba`（中文分词）
    ///
    /// 若分词器与上次启动不同，自动删除旧索引并重建（SQLite 数据保留）。
    pub fn open(data_dir: &Path) -> Result<Self> {
        let lang = crate::config::Config::get_language();
        let desired_tokenizer = if lang.is_english() {
            "en_stem"
        } else {
            "jieba"
        };
        Self::open_with_tokenizer(data_dir, desired_tokenizer)
    }

    /// 内部实现：以指定分词器打开或创建索引（供生产和测试共用）。
    fn open_with_tokenizer(data_dir: &Path, desired_tokenizer: &str) -> Result<Self> {
        std::fs::create_dir_all(data_dir).wrap_err("创建数据目录失败")?;

        let db_path = data_dir.join("memory.db");
        let db = Connection::open(&db_path).wrap_err("打开 SQLite 失败")?;

        // 提前建 search_meta 表，用于读取上次使用的分词器
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS search_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .wrap_err("初始化 search_meta 表失败")?;

        // 读取上次存储的分词器名称
        let stored_tokenizer: Option<String> = db
            .query_row(
                "SELECT value FROM search_meta WHERE key = 'tokenizer'",
                [],
                |row| row.get(0),
            )
            .ok();

        let index_path = data_dir.join("search_index");

        // 若分词器发生变更，删除旧索引以便重建（SQLite memories 表数据保留）
        if stored_tokenizer.as_deref() != Some(desired_tokenizer) && index_path.exists() {
            tracing::info!(
                "Search tokenizer changed ({} → {}), rebuilding index",
                stored_tokenizer.as_deref().unwrap_or("none"),
                desired_tokenizer
            );
            std::fs::remove_dir_all(&index_path).wrap_err("删除旧搜索索引失败")?;
        }

        std::fs::create_dir_all(&index_path).wrap_err("创建索引目录失败")?;

        let (schema, key_field, content_field, category_field) =
            Self::build_schema(desired_tokenizer);
        let dir = tantivy::directory::MmapDirectory::open(&index_path)
            .wrap_err("打开 tantivy 目录失败")?;
        let index = Index::open_or_create(dir, schema.clone()).wrap_err("创建 tantivy 索引失败")?;

        Self::finish_init(
            db,
            index,
            schema,
            key_field,
            content_field,
            category_field,
            desired_tokenizer,
        )
    }

    /// 从内存创建（测试用）。始终使用 jieba 以保持中文搜索测试兼容性。
    pub fn in_memory() -> Result<Self> {
        let db = Connection::open_in_memory().wrap_err("打开内存 SQLite 失败")?;

        let (schema, key_field, content_field, category_field) = Self::build_schema("jieba");
        let index = Index::create_in_ram(schema.clone());

        Self::finish_init(
            db,
            index,
            schema,
            key_field,
            content_field,
            category_field,
            "jieba",
        )
    }

    /// 构建 tantivy Schema，以 `tokenizer_name` 作为内容字段的分词器。
    fn build_schema(tokenizer_name: &str) -> (Schema, Field, Field, Field) {
        let mut builder = Schema::builder();

        let key_field = builder.add_text_field("key", STRING | STORED);

        let content_options = TextOptions::default()
            .set_indexing_options(
                TextFieldIndexing::default()
                    .set_tokenizer(tokenizer_name)
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
        tokenizer_name: &str,
    ) -> Result<Self> {
        // jieba 需要显式注册；en_stem 是 tantivy 内置分词器，无需注册
        if tokenizer_name == "jieba" {
            index
                .tokenizers()
                .register("jieba", tantivy_jieba::JiebaTokenizer::new());
        }

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
            CREATE INDEX IF NOT EXISTS idx_conv_session ON conversation_history(session_id);
            CREATE TABLE IF NOT EXISTS search_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );",
        )
        .wrap_err("创建数据库表失败")?;

        // 记录当前分词器名称，供下次启动对比
        db.execute(
            "INSERT INTO search_meta (key, value) VALUES ('tokenizer', ?1)
             ON CONFLICT(key) DO UPDATE SET value = ?1",
            params![tokenizer_name],
        )
        .wrap_err("写入 tokenizer 元信息失败")?;

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

    /// 种入核心知识条目（启动时调用，upsert 语义）
    /// 让 BM25 recall 能匹配到 RRClaw 自身信息，减少模型盲猜
    pub async fn seed_core_knowledge(
        &self,
        data_dir: &Path,
        log_dir: &Path,
        config_path: &Path,
    ) -> Result<()> {
        let seeds = [
            (
                "rrclaw_db_path",
                format!(
                    "RRClaw 的 SQLite 数据库位于 {}，包含 memories 和 conversation_history 两张表",
                    data_dir.join("memory.db").display()
                ),
            ),
            (
                "rrclaw_log_path",
                format!(
                    "RRClaw 的日志文件位于 {}/rrclaw.log.YYYY-MM-DD，默认 debug 级别，trace 级别可看完整 API 请求体",
                    log_dir.display()
                ),
            ),
            (
                "rrclaw_config_path",
                format!(
                    "RRClaw 的配置文件位于 {}，TOML 格式，包含 providers/memory/security 配置",
                    config_path.display()
                ),
            ),
            (
                "rrclaw_capabilities",
                "RRClaw 支持的工具: shell（命令执行）、file_read（读文件）、file_write（写文件）、config（读写配置）、self_info（查询自身信息）".to_string(),
            ),
        ];

        for (key, content) in &seeds {
            self.store(key, content, MemoryCategory::Core).await?;
        }

        tracing::debug!("已种入 {} 条核心知识", seeds.len());
        Ok(())
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

// 支持 Arc<SqliteMemory> 作为 Box<dyn Memory> 使用
#[async_trait]
impl Memory for Arc<SqliteMemory> {
    async fn store(&self, key: &str, content: &str, category: MemoryCategory) -> Result<()> {
        SqliteMemory::store(self, key, content, category).await
    }
    async fn recall(&self, query: &str, limit: usize) -> Result<Vec<MemoryEntry>> {
        SqliteMemory::recall(self, query, limit).await
    }
    async fn forget(&self, key: &str) -> Result<bool> {
        SqliteMemory::forget(self, key).await
    }
    async fn count(&self) -> Result<usize> {
        SqliteMemory::count(self).await
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

        mem.store(
            "meeting",
            "今天的会议讨论了人工智能的应用",
            MemoryCategory::Daily,
        )
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
                reasoning_content: None,
            }),
            ConversationMessage::AssistantToolCalls {
                text: Some("让我查看".to_string()),
                reasoning_content: None,
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
                reasoning_content: None,
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
            reasoning_content: None,
        })];
        mem.save_conversation_history(session_id, &history1)
            .await
            .unwrap();

        let history2 = vec![
            ConversationMessage::Chat(ChatMessage {
                role: "user".to_string(),
                content: "second".to_string(),
                reasoning_content: None,
            }),
            ConversationMessage::Chat(ChatMessage {
                role: "assistant".to_string(),
                content: "reply".to_string(),
                reasoning_content: None,
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
    async fn conversation_history_reasoning_content_roundtrip() {
        use crate::providers::{ChatMessage, ConversationMessage, ToolCall};

        let mem = create_test_memory().await;
        let session_id = "reasoning-test";

        let history = vec![
            ConversationMessage::Chat(ChatMessage {
                role: "user".to_string(),
                content: "查看文件".to_string(),
                reasoning_content: None,
            }),
            ConversationMessage::AssistantToolCalls {
                text: Some("让我查看".to_string()),
                reasoning_content: Some("用户需要查看文件列表".to_string()),
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
                reasoning_content: Some("工具返回了文件列表".to_string()),
            }),
        ];

        mem.save_conversation_history(session_id, &history)
            .await
            .unwrap();
        let loaded = mem.load_conversation_history(session_id).await.unwrap();
        assert_eq!(loaded.len(), 4);

        // 验证 AssistantToolCalls 的 reasoning_content 保留
        if let ConversationMessage::AssistantToolCalls {
            reasoning_content, ..
        } = &loaded[1]
        {
            assert_eq!(reasoning_content.as_deref(), Some("用户需要查看文件列表"));
        } else {
            panic!("第2条消息应是 AssistantToolCalls");
        }

        // 验证 Chat(assistant) 的 reasoning_content 保留
        if let ConversationMessage::Chat(cm) = &loaded[3] {
            assert_eq!(cm.reasoning_content.as_deref(), Some("工具返回了文件列表"));
        } else {
            panic!("第4条消息应是 Chat");
        }
    }

    #[tokio::test]
    async fn conversation_history_backward_compat() {
        // 模拟旧格式 JSON（没有 reasoning_content 字段）
        let mem = create_test_memory().await;
        let session_id = "old-format";

        // 手动插入旧格式 payload
        let old_payload = r#"{"Chat":{"role":"assistant","content":"你好"}}"#;
        let db = mem.db.lock().await;
        db.execute(
            "INSERT INTO conversation_history (session_id, seq, payload, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![session_id, 0i64, old_payload, "2024-01-01T00:00:00Z"],
        ).unwrap();
        drop(db);

        let loaded = mem.load_conversation_history(session_id).await.unwrap();
        assert_eq!(loaded.len(), 1);
        if let ConversationMessage::Chat(cm) = &loaded[0] {
            assert_eq!(cm.content, "你好");
            assert_eq!(cm.reasoning_content, None); // 旧数据应默认 None
        } else {
            panic!("应为 Chat 消息");
        }
    }

    #[tokio::test]
    async fn seed_core_knowledge_stores_and_recalls() {
        let mem = create_test_memory().await;
        let data_dir = std::path::Path::new("/tmp/rrclaw/data");
        let log_dir = std::path::Path::new("/tmp/rrclaw/logs");
        let config_path = std::path::Path::new("/tmp/rrclaw/config.toml");

        mem.seed_core_knowledge(data_dir, log_dir, config_path)
            .await
            .unwrap();

        // 验证种子数量
        let count = mem.count().await.unwrap();
        assert_eq!(count, 4);

        // 验证 recall 能命中 "SQLite 数据库" 关键词
        let results = mem.recall("SQLite 数据库", 5).await.unwrap();
        assert!(!results.is_empty(), "应能 recall 到数据库相关知识");
        assert!(results[0].content.contains("memory.db"));

        // 验证 recall 能命中 "日志" 关键词
        let results = mem.recall("日志", 5).await.unwrap();
        assert!(!results.is_empty(), "应能 recall 到日志相关知识");
        assert!(results[0].content.contains("rrclaw.log"));

        // 验证重复调用是 upsert（不会重复）
        mem.seed_core_knowledge(data_dir, log_dir, config_path)
            .await
            .unwrap();
        let count = mem.count().await.unwrap();
        assert_eq!(count, 4);
    }

    #[tokio::test]
    async fn memory_category_roundtrip() {
        let mem = create_test_memory().await;

        mem.store(
            "custom",
            "custom category test",
            MemoryCategory::Custom("project_a".to_string()),
        )
        .await
        .unwrap();

        let results = mem.recall("custom category", 10).await.unwrap();
        assert!(!results.is_empty());
        assert_eq!(
            results[0].category,
            MemoryCategory::Custom("project_a".to_string())
        );
    }

    // ── P9-4: tokenizer selection tests ───────────────────────────────────────

    #[tokio::test]
    async fn open_jieba_stores_tokenizer_in_search_meta() {
        let tmp = tempfile::tempdir().unwrap();
        let _ = SqliteMemory::open_with_tokenizer(tmp.path(), "jieba").unwrap();

        let db = Connection::open(tmp.path().join("memory.db")).unwrap();
        let tok: String = db
            .query_row(
                "SELECT value FROM search_meta WHERE key = 'tokenizer'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tok, "jieba");
    }

    #[tokio::test]
    async fn open_en_stem_stores_tokenizer_in_search_meta() {
        let tmp = tempfile::tempdir().unwrap();
        let _ = SqliteMemory::open_with_tokenizer(tmp.path(), "en_stem").unwrap();

        let db = Connection::open(tmp.path().join("memory.db")).unwrap();
        let tok: String = db
            .query_row(
                "SELECT value FROM search_meta WHERE key = 'tokenizer'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tok, "en_stem");
    }

    #[tokio::test]
    async fn open_en_stem_can_recall_english_text() {
        let tmp = tempfile::tempdir().unwrap();
        let mem = SqliteMemory::open_with_tokenizer(tmp.path(), "en_stem").unwrap();

        mem.store(
            "prog",
            "programming with Rust is great",
            MemoryCategory::Core,
        )
        .await
        .unwrap();
        mem.store(
            "cook",
            "cooking pasta for dinner tonight",
            MemoryCategory::Core,
        )
        .await
        .unwrap();

        // "program" should stem-match "programming"
        let results = mem.recall("program Rust", 5).await.unwrap();
        assert!(
            !results.is_empty(),
            "en_stem should recall stemmed English words"
        );
        assert_eq!(results[0].key, "prog");
    }

    #[tokio::test]
    async fn tokenizer_change_rebuilds_index_and_preserves_sqlite_data() {
        let tmp = tempfile::tempdir().unwrap();

        // First open with jieba, store a memory
        {
            let mem = SqliteMemory::open_with_tokenizer(tmp.path(), "jieba").unwrap();
            mem.store("k1", "今天的会议讨论了计划", MemoryCategory::Core)
                .await
                .unwrap();
            assert_eq!(mem.count().await.unwrap(), 1);
        }

        let index_path = tmp.path().join("search_index");
        assert!(index_path.exists(), "index should exist after first open");

        // Reopen with en_stem — should detect change, rebuild index
        {
            let mem = SqliteMemory::open_with_tokenizer(tmp.path(), "en_stem").unwrap();
            // SQLite data is preserved even after index rebuild
            assert_eq!(
                mem.count().await.unwrap(),
                1,
                "SQLite data survives index rebuild"
            );

            // Verify the new tokenizer is now recorded in search_meta
            let db = Connection::open(tmp.path().join("memory.db")).unwrap();
            let tok: String = db
                .query_row(
                    "SELECT value FROM search_meta WHERE key = 'tokenizer'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(tok, "en_stem");
        }

        // Third open with same tokenizer — no rebuild, verify idempotent
        {
            let mem = SqliteMemory::open_with_tokenizer(tmp.path(), "en_stem").unwrap();
            assert_eq!(mem.count().await.unwrap(), 1);
        }
    }
}
