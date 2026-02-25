use async_trait::async_trait;
use color_eyre::eyre::{eyre, Result};
use serde_json::json;
use tracing::debug;

use crate::security::SecurityPolicy;
use super::traits::{Tool, ToolResult};

pub struct GitTool;

#[async_trait]
impl Tool for GitTool {
    fn name(&self) -> &str {
        "git"
    }

    fn description(&self) -> &str {
        "Git version control (preferred; has safety guardrails). Supports action: status, diff, log, add, commit, branch, checkout, push, pull, fetch. \
         Safer than the shell tool: force push/checkout is blocked, action allowlist enforced."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["status", "diff", "log", "add", "commit", "branch", "checkout", "push", "pull", "fetch"],
                    "description": "Git operation type"
                },
                "args": {
                    "type": "string",
                    "description": "Operation arguments. Examples: file path for diff, -m \"message\" for commit, space-separated files for add, branch name for branch/checkout, --oneline -10 for log, origin main for push/pull. Leave empty for default behavior."
                }
            },
            "required": ["action"]
        })
    }

    fn pre_validate(&self, args: &serde_json::Value, policy: &SecurityPolicy) -> Option<String> {
        // ReadOnly 模式拒绝所有 git 操作
        if !policy.allows_execution() {
            return Some("Read-only mode: Git operations not allowed".to_string());
        }

        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let extra = args.get("args").and_then(|v| v.as_str()).unwrap_or("");

        // 禁止 force push
        if action == "push" && (extra.contains("--force") || extra.contains("-f")) {
            return Some("Force push is blocked. Please run manually if needed.".to_string());
        }

        // 禁止 checkout --force / checkout -f（可能丢失未提交改动）
        if action == "checkout" && (extra.contains("--force") || extra.contains("-f")) {
            return Some("Force checkout is blocked. Please run manually if needed.".to_string());
        }

        None
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        policy: &SecurityPolicy,
    ) -> Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| eyre!("Missing 'action' parameter"))?;

        let extra = args
            .get("args")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let git_args = match build_git_args(action, extra) {
            Ok(args) => args,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("{}", e)),
                    ..Default::default()
                });
            }
        };

        debug!("执行 git {:?} in {}", git_args, policy.workspace_dir.display());

        let output = tokio::process::Command::new("git")
            .args(&git_args)
            .current_dir(&policy.workspace_dir)
            .output()
            .await;

        match output {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();

                if output.status.success() {
                    Ok(ToolResult {
                        success: true,
                        output: if stdout.is_empty() { stderr } else { stdout },
                        error: None,
                        ..Default::default()
                    })
                } else {
                    Ok(ToolResult {
                        success: false,
                        output: stdout,
                        error: Some(if stderr.is_empty() {
                            format!("git exited with code: {}", output.status.code().unwrap_or(-1))
                        } else {
                            stderr
                        }),
                        ..Default::default()
                    })
                }
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to execute git command: {}", e)),
                ..Default::default()
            }),
        }
    }
}

/// 根据 action + 额外参数构造 git 命令参数列表
fn build_git_args(action: &str, extra: &str) -> Result<Vec<String>> {
    // 验证 action 合法性
    let valid_actions = ["status", "diff", "log", "add", "commit", "branch", "checkout", "push", "pull", "fetch"];
    if !valid_actions.contains(&action) {
        return Err(eyre!("Unknown git action: '{}'. Supported: {}", action, valid_actions.join(", ")));
    }

    let mut args = vec![action.to_string()];

    // 追加额外参数（安全拆分，处理引号）
    if !extra.is_empty() {
        let extra_args = shell_words::split(extra)
            .map_err(|e| eyre!("Failed to parse arguments: {}. Please check that quotes are balanced.", e))?;
        args.extend(extra_args);
    }

    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};

    fn test_policy(workspace: &std::path::Path) -> SecurityPolicy {
        let canonical = workspace.canonicalize().unwrap_or_else(|_| workspace.to_path_buf());
        SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            allowed_commands: vec![],
            workspace_dir: canonical,
            blocked_paths: vec![],
            http_allowed_hosts: vec![],
            injection_check: true,
        }
    }

    // --- build_git_args 测试 ---

    #[test]
    fn build_args_status() {
        let args = build_git_args("status", "").unwrap();
        assert_eq!(args, vec!["status"]);
    }

    #[test]
    fn build_args_commit_with_message() {
        let args = build_git_args("commit", "-m \"feat: add something\"").unwrap();
        assert_eq!(args, vec!["commit", "-m", "feat: add something"]);
    }

    #[test]
    fn build_args_log_with_flags() {
        let args = build_git_args("log", "--oneline -10").unwrap();
        assert_eq!(args, vec!["log", "--oneline", "-10"]);
    }

    #[test]
    fn build_args_add_multiple_files() {
        let args = build_git_args("add", "src/main.rs src/lib.rs").unwrap();
        assert_eq!(args, vec!["add", "src/main.rs", "src/lib.rs"]);
    }

    #[test]
    fn build_args_unknown_action() {
        let result = build_git_args("rebase", "-i HEAD~3");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unknown git action"));
    }

    #[test]
    fn build_args_unmatched_quotes() {
        let result = build_git_args("commit", "-m \"unclosed");
        assert!(result.is_err());
    }

    // --- pre_validate 测试 ---

    #[test]
    fn pre_validate_readonly_rejected() {
        let mut policy = test_policy(std::path::Path::new("/tmp"));
        policy.autonomy = AutonomyLevel::ReadOnly;
        let args = serde_json::json!({"action": "status"});
        assert!(GitTool.pre_validate(&args, &policy).is_some());
    }

    #[test]
    fn pre_validate_force_push_rejected() {
        let policy = test_policy(std::path::Path::new("/tmp"));
        let args = serde_json::json!({"action": "push", "args": "--force origin main"});
        let result = GitTool.pre_validate(&args, &policy);
        assert!(result.is_some());
        assert!(result.unwrap().contains("Force push"));
    }

    #[test]
    fn pre_validate_force_push_short_flag_rejected() {
        let policy = test_policy(std::path::Path::new("/tmp"));
        let args = serde_json::json!({"action": "push", "args": "-f origin main"});
        assert!(GitTool.pre_validate(&args, &policy).is_some());
    }

    #[test]
    fn pre_validate_normal_push_allowed() {
        let policy = test_policy(std::path::Path::new("/tmp"));
        let args = serde_json::json!({"action": "push", "args": "origin main"});
        assert!(GitTool.pre_validate(&args, &policy).is_none());
    }

    #[test]
    fn pre_validate_force_checkout_rejected() {
        let policy = test_policy(std::path::Path::new("/tmp"));
        let args = serde_json::json!({"action": "checkout", "args": "--force main"});
        assert!(GitTool.pre_validate(&args, &policy).is_some());
    }

    // --- execute 集成测试（需要真实 git repo）---

    #[tokio::test]
    async fn execute_status_in_temp_repo() {
        let tmp = tempfile::tempdir().unwrap();
        // 初始化一个 git repo
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(tmp.path())
            .output()
            .unwrap();

        let policy = test_policy(tmp.path());
        let result = GitTool
            .execute(serde_json::json!({"action": "status"}), &policy)
            .await
            .unwrap();

        assert!(result.success);
        // 新 repo 应该包含 "nothing to commit" 或类似信息
    }

    #[tokio::test]
    async fn execute_log_empty_repo() {
        let tmp = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(tmp.path())
            .output()
            .unwrap();

        let policy = test_policy(tmp.path());
        let result = GitTool
            .execute(serde_json::json!({"action": "log"}), &policy)
            .await
            .unwrap();

        // 空 repo 的 log 会失败（没有 commit）
        assert!(!result.success);
    }

    #[test]
    fn tool_spec_correct() {
        let spec = GitTool.spec();
        assert_eq!(spec.name, "git");
        assert!(spec.description.contains("status"));
        let actions = spec.parameters["properties"]["action"]["enum"]
            .as_array()
            .unwrap();
        assert_eq!(actions.len(), 10);
    }
}
