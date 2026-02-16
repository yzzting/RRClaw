use async_trait::async_trait;
use color_eyre::eyre::{Context, Result};
use std::path::Path;

use crate::security::SecurityPolicy;

use super::traits::{Tool, ToolResult};

/// 文件读取工具
pub struct FileReadTool;

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str {
        "file_read"
    }

    fn description(&self) -> &str {
        "读取文件内容。路径必须在 workspace 范围内。"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "要读取的文件路径"
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        policy: &SecurityPolicy,
    ) -> Result<ToolResult> {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| color_eyre::eyre::eyre!("缺少 path 参数"))?;

        let path = resolve_path(path_str, policy);

        // 安全检查: 路径限制
        if !policy.is_path_allowed(&path) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("路径不在允许范围内: {}", path.display())),
            });
        }

        match tokio::fs::read_to_string(&path).await {
            Ok(content) => Ok(ToolResult {
                success: true,
                output: content,
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("读取文件失败: {}", e)),
            }),
        }
    }
}

/// 文件写入工具
pub struct FileWriteTool;

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str {
        "file_write"
    }

    fn description(&self) -> &str {
        "写入文件内容。路径必须在 workspace 范围内。"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "要写入的文件路径"
                },
                "content": {
                    "type": "string",
                    "description": "要写入的内容"
                }
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        policy: &SecurityPolicy,
    ) -> Result<ToolResult> {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| color_eyre::eyre::eyre!("缺少 path 参数"))?;

        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| color_eyre::eyre::eyre!("缺少 content 参数"))?;

        // 安全检查: ReadOnly 模式拒绝
        if !policy.allows_execution() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("当前为只读模式，不允许写入文件".to_string()),
            });
        }

        let path = resolve_path(path_str, policy);

        // 安全检查: 路径限制
        if !policy.is_path_allowed(&path) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("路径不在允许范围内: {}", path.display())),
            });
        }

        // 确保父目录存在
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .wrap_err("创建目录失败")?;
            }
        }

        match tokio::fs::write(&path, content).await {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: format!("已写入 {} 字节到 {}", content.len(), path.display()),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("写入文件失败: {}", e)),
            }),
        }
    }
}

/// 解析路径：相对路径基于 workspace_dir
fn resolve_path(path_str: &str, policy: &SecurityPolicy) -> std::path::PathBuf {
    let path = Path::new(path_str);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        policy.workspace_dir.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::AutonomyLevel;

    fn test_policy(workspace: &std::path::Path) -> SecurityPolicy {
        // macOS: /var → /private/var, canonicalize 确保一致
        let canonical = workspace.canonicalize().unwrap_or_else(|_| workspace.to_path_buf());
        SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            allowed_commands: vec![],
            workspace_dir: canonical,
            blocked_paths: vec![],
        }
    }

    #[tokio::test]
    async fn file_read_success() {
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("test.txt");
        std::fs::write(&file_path, "hello world").unwrap();
        let policy = test_policy(tmp.path());

        let result = FileReadTool
            .execute(
                serde_json::json!({"path": file_path.to_str().unwrap()}),
                &policy,
            )
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output, "hello world");
    }

    #[tokio::test]
    async fn file_read_relative_path() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("data.txt"), "content").unwrap();
        let policy = test_policy(tmp.path());

        let result = FileReadTool
            .execute(serde_json::json!({"path": "data.txt"}), &policy)
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output, "content");
    }

    #[tokio::test]
    async fn file_read_outside_workspace_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let policy = test_policy(tmp.path());

        let result = FileReadTool
            .execute(serde_json::json!({"path": "/etc/passwd"}), &policy)
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("允许范围"));
    }

    #[tokio::test]
    async fn file_read_nonexistent() {
        let tmp = tempfile::tempdir().unwrap();
        let policy = test_policy(tmp.path());

        let result = FileReadTool
            .execute(serde_json::json!({"path": "nonexistent.txt"}), &policy)
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("读取文件失败"));
    }

    #[tokio::test]
    async fn file_write_success() {
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("output.txt");
        let policy = test_policy(tmp.path());

        let result = FileWriteTool
            .execute(
                serde_json::json!({"path": file_path.to_str().unwrap(), "content": "written"}),
                &policy,
            )
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(std::fs::read_to_string(&file_path).unwrap(), "written");
    }

    #[tokio::test]
    async fn file_write_creates_parent_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("sub").join("dir").join("file.txt");
        let policy = test_policy(tmp.path());

        let result = FileWriteTool
            .execute(
                serde_json::json!({"path": file_path.to_str().unwrap(), "content": "nested"}),
                &policy,
            )
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(std::fs::read_to_string(&file_path).unwrap(), "nested");
    }

    #[tokio::test]
    async fn file_write_readonly_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let mut policy = test_policy(tmp.path());
        policy.autonomy = AutonomyLevel::ReadOnly;

        let result = FileWriteTool
            .execute(
                serde_json::json!({"path": "file.txt", "content": "data"}),
                &policy,
            )
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("只读"));
    }

    #[tokio::test]
    async fn file_write_outside_workspace_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let policy = test_policy(tmp.path());

        let result = FileWriteTool
            .execute(
                serde_json::json!({"path": "/etc/evil.txt", "content": "hack"}),
                &policy,
            )
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("允许范围"));
    }

    #[test]
    fn tool_specs() {
        let read_spec = FileReadTool.spec();
        assert_eq!(read_spec.name, "file_read");

        let write_spec = FileWriteTool.spec();
        assert_eq!(write_spec.name, "file_write");
        assert!(write_spec.parameters["required"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("content")));
    }
}
