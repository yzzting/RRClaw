use async_trait::async_trait;
use color_eyre::eyre::{Context, Result};
use std::time::Duration;
use tokio::process::Command;

use crate::security::SecurityPolicy;

use super::traits::{Tool, ToolResult};

/// Shell 命令执行工具
pub struct ShellTool;

const SHELL_TIMEOUT: Duration = Duration::from_secs(120);

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Execute a shell command. In Supervised mode any command is allowed after user confirmation; in Full mode the command must be on the allowlist."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Shell command to execute"
                }
            },
            "required": ["command"]
        })
    }

    fn pre_validate(&self, args: &serde_json::Value, policy: &SecurityPolicy) -> Option<String> {
        // ReadOnly 模式: 绝对拒绝
        if !policy.allows_execution() {
            return Some("Read-only mode: command execution not allowed".to_string());
        }
        // Full 模式: 白名单是唯一防线（无人工确认）
        // Supervised 模式: 不在此拦截，由用户确认决定
        if !policy.requires_confirmation() {
            if let Some(command) = args.get("command").and_then(|v| v.as_str()) {
                if !policy.is_command_allowed(command) {
                    return Some(format!("Command not in allowlist: {}", command));
                }
            }
        }
        None
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        policy: &SecurityPolicy,
    ) -> Result<ToolResult> {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| color_eyre::eyre::eyre!("Missing 'command' parameter"))?;

        // ReadOnly 模式: 绝对拒绝
        if !policy.allows_execution() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Read-only mode: command execution not allowed".to_string()),
                ..Default::default()
            });
        }

        // Full 模式: 白名单强制检查（无人工确认，这是唯一防线）
        // Supervised 模式: 用户已通过 [y/N] 确认，跳过白名单
        if !policy.requires_confirmation() && !policy.is_command_allowed(command) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Command not in allowlist: {}", command)),
                ..Default::default()
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
                    // 合并 stdout + stderr（cargo 等工具将编译信息输出到 stderr）
                    let combined = if stderr.is_empty() {
                        stdout
                    } else if stdout.is_empty() {
                        stderr
                    } else {
                        format!("{}\n[stderr]\n{}", stdout, stderr)
                    };
                    Ok(ToolResult {
                        success: true,
                        output: combined,
                        error: None,
                        ..Default::default()
                    })
                } else {
                    Ok(ToolResult {
                        success: false,
                        output: stdout,
                        error: Some(format!(
                            "Command exited with code: {}\n{}",
                            output.status.code().unwrap_or(-1),
                            stderr
                        )),
                        ..Default::default()
                    })
                }
            }
            Ok(Err(e)) => Err(e).wrap_err("执行命令失败"),
            Err(_) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Command timed out ({}s)", SHELL_TIMEOUT.as_secs())),
                ..Default::default()
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
            http_allowed_hosts: vec![],
            injection_check: true,
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
        assert!(result.error.unwrap().contains("allowlist"));
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
        assert!(result.error.unwrap().contains("Read-only"));
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
