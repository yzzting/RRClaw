// src/tools/memory.rs
use async_trait::async_trait;
use color_eyre::eyre::Result;
use serde_json::json;
use std::sync::Arc;

use crate::memory::{Memory, MemoryCategory};
use crate::security::SecurityPolicy;
use super::traits::{Tool, ToolResult};

/// LLM 主动存储记忆
pub struct MemoryStoreTool {
    memory: Arc<dyn Memory>,
}

impl MemoryStoreTool {
    pub fn new(memory: Arc<dyn Memory>) -> Self {
        Self { memory }
    }
}

#[async_trait]
impl Tool for MemoryStoreTool {
    fn name(&self) -> &str { "memory_store" }

    fn description(&self) -> &str {
        "存储一条记忆。用于保存用户偏好、项目约定、学到的知识等需要长期记住的信息。\
         参数: key（唯一标识）, content（内容）, category（分类: core/daily/custom）"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "key": {
                    "type": "string",
                    "description": "记忆的唯一标识，如 'user_preference_language', 'project_deploy_cmd'"
                },
                "content": {
                    "type": "string",
                    "description": "要记住的内容"
                },
                "category": {
                    "type": "string",
                    "enum": ["core", "daily", "custom"],
                    "description": "分类: core(核心知识/偏好), daily(日常记录), custom(自定义)"
                }
            },
            "required": ["key", "content"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _policy: &SecurityPolicy,
    ) -> Result<ToolResult> {
        let key = match args.get("key").and_then(|v| v.as_str()) {
            Some(k) if !k.is_empty() => k,
            _ => return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("缺少 key 参数".to_string()),
            }),
        };

        let content = match args.get("content").and_then(|v| v.as_str()) {
            Some(c) if !c.is_empty() => c,
            _ => return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("缺少 content 参数".to_string()),
            }),
        };

        let category = args
            .get("category")
            .and_then(|v| v.as_str())
            .map(MemoryCategory::parse)
            .unwrap_or(MemoryCategory::Core);

        match self.memory.store(key, content, category).await {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: format!("已记住: [{}] {}", key, truncate(content, 100)),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("存储失败: {}", e)),
            }),
        }
    }
}

/// LLM 主动搜索记忆
pub struct MemoryRecallTool {
    memory: Arc<dyn Memory>,
}

impl MemoryRecallTool {
    pub fn new(memory: Arc<dyn Memory>) -> Self {
        Self { memory }
    }
}

#[async_trait]
impl Tool for MemoryRecallTool {
    fn name(&self) -> &str { "memory_recall" }

    fn description(&self) -> &str {
        "搜索记忆。根据查询关键词检索相关记忆。\
         当你需要回忆用户偏好、项目信息、之前的约定时使用。\
         参数: query（搜索关键词）, limit（返回条数，默认5）"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "搜索关键词"
                },
                "limit": {
                    "type": "integer",
                    "description": "最多返回条数，默认 5",
                    "default": 5
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _policy: &SecurityPolicy,
    ) -> Result<ToolResult> {
        let query = match args.get("query").and_then(|v| v.as_str()) {
            Some(q) if !q.is_empty() => q,
            _ => return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("缺少 query 参数".to_string()),
            }),
        };

        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as usize;

        match self.memory.recall(query, limit).await {
            Ok(entries) => {
                if entries.is_empty() {
                    return Ok(ToolResult {
                        success: true,
                        output: format!("未找到与 '{}' 相关的记忆。", query),
                        error: None,
                    });
                }

                let mut output = format!("找到 {} 条相关记忆:\n\n", entries.len());
                for (i, entry) in entries.iter().enumerate() {
                    output.push_str(&format!(
                        "{}. [{}] ({})\n{}\n更新于: {}\n\n",
                        i + 1,
                        entry.key,
                        entry.category.as_str(),
                        entry.content,
                        entry.updated_at,
                    ));
                }

                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("搜索失败: {}", e)),
            }),
        }
    }
}

/// LLM 主动遗忘记忆
pub struct MemoryForgetTool {
    memory: Arc<dyn Memory>,
}

impl MemoryForgetTool {
    pub fn new(memory: Arc<dyn Memory>) -> Self {
        Self { memory }
    }
}

#[async_trait]
impl Tool for MemoryForgetTool {
    fn name(&self) -> &str { "memory_forget" }

    fn description(&self) -> &str {
        "删除一条记忆。当用户要求忘记某些信息，或者记忆已过时需要清理时使用。\
         参数: key（要删除的记忆标识）"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "key": {
                    "type": "string",
                    "description": "要删除的记忆 key"
                }
            },
            "required": ["key"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _policy: &SecurityPolicy,
    ) -> Result<ToolResult> {
        let key = match args.get("key").and_then(|v| v.as_str()) {
            Some(k) if !k.is_empty() => k,
            _ => return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("缺少 key 参数".to_string()),
            }),
        };

        match self.memory.forget(key).await {
            Ok(true) => Ok(ToolResult {
                success: true,
                output: format!("已删除记忆: {}", key),
                error: None,
            }),
            Ok(false) => Ok(ToolResult {
                success: true,
                output: format!("未找到记忆: {}（可能已被删除）", key),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("删除失败: {}", e)),
            }),
        }
    }
}

/// 截断字符串用于输出摘要
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{MemoryEntry, MemoryCategory};
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn test_policy() -> SecurityPolicy {
        SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            allowed_commands: vec![],
            workspace_dir: PathBuf::from("/tmp"),
            blocked_paths: vec![],
        }
    }

    // --- Mock Memory ---
    struct MockMemory {
        stored: std::sync::Mutex<Vec<(String, String, String)>>, // (key, content, category)
    }

    impl MockMemory {
        fn new() -> Self {
            Self { stored: std::sync::Mutex::new(Vec::new()) }
        }
    }

    #[async_trait::async_trait]
    impl Memory for MockMemory {
        async fn store(&self, key: &str, content: &str, category: MemoryCategory) -> Result<()> {
            self.stored.lock().unwrap().push((
                key.to_string(),
                content.to_string(),
                category.as_str().to_string(),
            ));
            Ok(())
        }
        async fn recall(&self, query: &str, limit: usize) -> Result<Vec<MemoryEntry>> {
            let stored = self.stored.lock().unwrap();
            let results: Vec<MemoryEntry> = stored.iter()
                .filter(|(_, content, _)| content.contains(query))
                .take(limit)
                .map(|(key, content, cat)| MemoryEntry {
                    key: key.clone(),
                    content: content.clone(),
                    category: MemoryCategory::parse(cat),
                    created_at: "2024-01-01T00:00:00Z".to_string(),
                    updated_at: "2024-01-01T00:00:00Z".to_string(),
                    relevance_score: 1.0,
                })
                .collect();
            Ok(results)
        }
        async fn forget(&self, key: &str) -> Result<bool> {
            let mut stored = self.stored.lock().unwrap();
            let len_before = stored.len();
            stored.retain(|(k, _, _)| k != key);
            Ok(stored.len() < len_before)
        }
        async fn count(&self) -> Result<usize> {
            Ok(self.stored.lock().unwrap().len())
        }
    }

    // --- MemoryStoreTool 测试 ---

    #[tokio::test]
    async fn store_success() {
        let mem = Arc::new(MockMemory::new());
        let tool = MemoryStoreTool::new(mem.clone());
        let result = tool.execute(
            serde_json::json!({"key": "pref_lang", "content": "用户偏好 Rust", "category": "core"}),
            &test_policy(),
        ).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("pref_lang"));
        assert_eq!(mem.stored.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn store_missing_key() {
        let mem = Arc::new(MockMemory::new());
        let tool = MemoryStoreTool::new(mem);
        let result = tool.execute(
            serde_json::json!({"content": "something"}),
            &test_policy(),
        ).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("key"));
    }

    #[tokio::test]
    async fn store_missing_content() {
        let mem = Arc::new(MockMemory::new());
        let tool = MemoryStoreTool::new(mem);
        let result = tool.execute(
            serde_json::json!({"key": "k"}),
            &test_policy(),
        ).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("content"));
    }

    #[tokio::test]
    async fn store_default_category_is_core() {
        let mem = Arc::new(MockMemory::new());
        let tool = MemoryStoreTool::new(mem.clone());
        tool.execute(
            serde_json::json!({"key": "k", "content": "v"}),
            &test_policy(),
        ).await.unwrap();
        assert_eq!(mem.stored.lock().unwrap()[0].2, "core");
    }

    // --- MemoryRecallTool 测试 ---

    #[tokio::test]
    async fn recall_finds_matching() {
        let mem = Arc::new(MockMemory::new());
        mem.store("k1", "Rust 是最好的语言", MemoryCategory::Core).await.unwrap();
        mem.store("k2", "Python 也不错", MemoryCategory::Daily).await.unwrap();

        let tool = MemoryRecallTool::new(mem);
        let result = tool.execute(
            serde_json::json!({"query": "Rust"}),
            &test_policy(),
        ).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("Rust"));
        assert!(!result.output.contains("Python"));
    }

    #[tokio::test]
    async fn recall_no_results() {
        let mem = Arc::new(MockMemory::new());
        let tool = MemoryRecallTool::new(mem);
        let result = tool.execute(
            serde_json::json!({"query": "不存在的东西"}),
            &test_policy(),
        ).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("未找到"));
    }

    #[tokio::test]
    async fn recall_missing_query() {
        let mem = Arc::new(MockMemory::new());
        let tool = MemoryRecallTool::new(mem);
        let result = tool.execute(
            serde_json::json!({}),
            &test_policy(),
        ).await.unwrap();
        assert!(!result.success);
    }

    // --- MemoryForgetTool 测试 ---

    #[tokio::test]
    async fn forget_existing_key() {
        let mem = Arc::new(MockMemory::new());
        mem.store("k1", "content", MemoryCategory::Core).await.unwrap();

        let tool = MemoryForgetTool::new(mem.clone());
        let result = tool.execute(
            serde_json::json!({"key": "k1"}),
            &test_policy(),
        ).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("已删除"));
        assert_eq!(mem.count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn forget_nonexistent_key() {
        let mem = Arc::new(MockMemory::new());
        let tool = MemoryForgetTool::new(mem);
        let result = tool.execute(
            serde_json::json!({"key": "nonexistent"}),
            &test_policy(),
        ).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("未找到"));
    }

    // --- truncate 测试 ---

    #[test]
    fn truncate_short_string() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_long_string() {
        let result = truncate("这是一个很长的字符串", 10);
        assert!(result.ends_with("..."));
    }
}
