# tantivy + jieba 集成方案

## 架构

Memory 模块使用双存储策略:
- **SQLite** — 结构化存储（主键、分类、时间戳等元数据）
- **tantivy** — 全文搜索索引（中文分词、BM25 排序）

```
store(key, content, category)
  → SQLite INSERT/UPSERT (结构化数据)
  → tantivy add_document (搜索索引)

recall(query, limit)
  → tantivy search (BM25 排序，返回 key + score)
  → SQLite 按 key 批量查询完整 MemoryEntry
  → 合并 score，返回

forget(key)
  → SQLite DELETE
  → tantivy delete_term
```

## 依赖

```toml
[dependencies]
tantivy = "0.22"
tantivy-jieba = "0.18"    # jieba 分词器的 tantivy 适配
rusqlite = { version = "0.32", features = ["bundled"] }
```

> `tantivy-jieba` 提供 `JiebaTokenizer`，直接注册到 tantivy 的 TokenizerManager 即可。
> 参考: https://crates.io/crates/tantivy-jieba

## tantivy Schema 设计

```rust
use tantivy::schema::*;

fn build_schema() -> Schema {
    let mut builder = Schema::builder();

    // key 字段: 精确匹配用，不分词
    builder.add_text_field("key", STRING | STORED);

    // content 字段: 使用 jieba 分词，支持全文搜索
    let content_options = TextOptions::default()
        .set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer("jieba")
                .set_index_option(IndexRecordOption::WithFreqsAndPositions)
        )
        .set_stored();
    builder.add_text_field("content", content_options);

    // category 字段: 精确匹配过滤
    builder.add_text_field("category", STRING | STORED);

    builder.build()
}
```

## Tokenizer 注册

```rust
use tantivy::Index;
use tantivy_jieba::JiebaTokenizer;

fn create_index(index_path: &Path) -> tantivy::Result<Index> {
    let schema = build_schema();

    // 创建或打开索引目录
    let dir = tantivy::directory::MmapDirectory::open(index_path)?;
    let index = Index::open_or_create(dir, schema)?;

    // 注册 jieba 分词器
    index.tokenizers().register("jieba", JiebaTokenizer {});

    Ok(index)
}
```

## 索引文件存储位置

```
~/.rrclaw/
├── config.toml
├── data/
│   ├── memory.db          # SQLite 数据库
│   └── search_index/      # tantivy 索引目录
│       ├── meta.json
│       ├── .managed.json
│       └── *.segment       # segment 文件
```

## 写入流程

```rust
async fn store(&self, key: &str, content: &str, category: MemoryCategory) -> Result<()> {
    // 1. SQLite UPSERT
    self.db.execute(
        "INSERT INTO memories (key, content, category, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(key) DO UPDATE SET content=?2, category=?3, updated_at=?5",
        params![key, content, category_str, now, now],
    )?;

    // 2. tantivy 索引更新（先删旧的再加新的）
    let key_field = self.schema.get_field("key")?;
    let content_field = self.schema.get_field("content")?;
    let category_field = self.schema.get_field("category")?;

    let mut writer = self.index_writer.lock().await;
    writer.delete_term(Term::from_field_text(key_field, key));
    writer.add_document(doc!(
        key_field => key,
        content_field => content,
        category_field => category_str,
    ))?;
    writer.commit()?;

    Ok(())
}
```

## 搜索流程

```rust
async fn recall(&self, query: &str, limit: usize) -> Result<Vec<MemoryEntry>> {
    let reader = self.index.reader()?;
    let searcher = reader.searcher();

    let content_field = self.schema.get_field("content")?;
    let key_field = self.schema.get_field("key")?;

    // 使用 QueryParser 解析查询（自动使用 jieba 分词）
    let query_parser = tantivy::query::QueryParser::for_index(&self.index, vec![content_field]);
    let query = query_parser.parse_query(query)?;

    // BM25 排序搜索
    let top_docs = searcher.search(&query, &tantivy::collector::TopDocs::with_limit(limit))?;

    // 从结果中提取 key + score
    let mut results = Vec::new();
    for (score, doc_address) in top_docs {
        let doc: TantivyDocument = searcher.doc(doc_address)?;
        if let Some(key_value) = doc.get_first(key_field) {
            let key = key_value.as_str().unwrap_or_default();
            // 用 key 从 SQLite 查完整 MemoryEntry
            if let Some(mut entry) = self.get_from_sqlite(key).await? {
                entry.relevance_score = score;
                results.push(entry);
            }
        }
    }

    Ok(results)
}
```

## 注意事项

1. **tantivy IndexWriter 是单线程的** — 需要 `Mutex` 或 `tokio::sync::Mutex` 包装
2. **commit 开销较大** — 可以攒批后 commit，或使用 `commit_with_policy`
3. **jieba 初始化耗时** — 首次加载词典约 100-200ms，之后复用。可在启动时预热
4. **索引一致性** — SQLite 和 tantivy 需要保持同步。如果不一致可重建索引:
   - 读取 SQLite 全量数据
   - 清空 tantivy 索引
   - 逐条重新索引
5. **内存占用** — tantivy 会 mmap 索引文件，实际内存占用取决于索引大小
