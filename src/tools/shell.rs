use async_trait::async_trait;
use color_eyre::eyre::{Context, Result};
use std::time::Duration;
use tokio::process::Command;

use crate::security::SecurityPolicy;

use super::traits::{Tool, ToolResult};

/// Shell 命令执行工具
pub struct ShellTool;

const SHELL_TIMEOUT: Duration = Duration::from_secs(30);

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "执行 shell 命令。命令必须在白名单中。"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "要执行的 shell 命令"
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        policy: &SecurityPolicy,
    ) -> Result<ToolResult> {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| color_eyre::eyre::eyre!("缺少 command 参数"))?;

        // 安全检查: ReadOnly 模式拒绝
        if !policy.allows_execution() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("当前为只读模式，不允许执行命令".to_string()),
            });
        }

        // 安全检查: 命令白名单
        if !policy.is_command_allowed(command) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("命令不在白名单中: {}", command)),
            });
        }

        // 执行命令
        let result = tokio::time::timeout(
            SHELL_TIMEOUT,
            Command::new("sh")
                .arg("-c")
                .arg(command)
                .current_dir(&policy.workspace_dir)
                .output(),
        )
        .await;

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();

                if output.status.success() {
                    Ok(ToolResult {
                        success: true,
                        output: stdout,
                        error: if stderr.is_empty() {
                            None
                        } else {
                            Some(stderr)
                        },
                    })
                } else {
                    Ok(ToolResult {
                        success: false,
                        output: stdout,
                        error: Some(format!(
                            "命令退出码: {}\n{}",
                            output.status.code().unwrap_or(-1),
                            stderr
                        )),
                    })
                }
            }
            Ok(Err(e)) => Err(e).wrap_err("执行命令失败"),
            Err(_) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("命令执行超时 ({}s)", SHELL_TIMEOUT.as_secs())),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::AutonomyLevel;

    fn test_policy(workspace: &std::path::Path) -> SecurityPolicy {
        let canonical = workspace.canonicalize().unwrap_or_else(|_| workspace.to_path_buf());
        SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            allowed_commands: vec!["ls", "echo", "cat", "pwd"]
                .into_iter()
                .map(String::from)
                .collect(),
            workspace_dir: canonical,
            blocked_paths: vec![],
        }
    }

    #[tokio::test]
    async fn shell_executes_allowed_command() {
        let tmp = tempfile::tempdir().unwrap();
        let policy = test_policy(tmp.path());

        let result = ShellTool
            .execute(serde_json::json!({"command": "echo hello"}), &policy)
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output.trim(), "hello");
    }

    #[tokio::test]
    async fn shell_rejects_disallowed_command() {
        let tmp = tempfile::tempdir().unwrap();
        let policy = test_policy(tmp.path());

        let result = ShellTool
            .execute(serde_json::json!({"command": "rm -rf /"}), &policy)
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("白名单"));
    }

    #[tokio::test]
    async fn shell_rejects_readonly_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let mut policy = test_policy(tmp.path());
        policy.autonomy = AutonomyLevel::ReadOnly;

        let result = ShellTool
            .execute(serde_json::json!({"command": "ls"}), &policy)
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("只读"));
    }

    #[tokio::test]
    async fn shell_runs_in_workspace_dir() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "content").unwrap();
        let policy = test_policy(tmp.path());

        let result = ShellTool
            .execute(serde_json::json!({"command": "ls"}), &policy)
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("test.txt"));
    }

    #[tokio::test]
    async fn shell_missing_command_param() {
        let tmp = tempfile::tempdir().unwrap();
        let policy = test_policy(tmp.path());

        let result = ShellTool
            .execute(serde_json::json!({}), &policy)
            .await;

        assert!(result.is_err());
    }

    #[test]
    fn shell_spec() {
        let spec = ShellTool.spec();
        assert_eq!(spec.name, "shell");
        assert!(spec.parameters["required"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("command")));
    }
}
