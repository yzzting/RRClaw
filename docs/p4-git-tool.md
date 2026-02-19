# P4-B: Git Tool 实现计划

## 背景

当前 Agent 通过 ShellTool 执行 git 命令，缺乏结构化参数校验和安全防护。需要专用 Git 工具，覆盖 8 个操作：status, diff, log, add, commit, branch, checkout, push。

---

## 一、架构设计

单个 `GitTool` 实现 Tool trait，通过 `action` 参数区分操作（与 ConfigTool 同模式）。内部调用 `git` CLI 进程，在 `policy.workspace_dir` 下执行。

**不引入 git2 库**——通过 `tokio::process::Command` 调用系统 git，保持依赖轻量。

---

## 二、数据结构与实现

### 2.1 新增依赖

```toml
# Cargo.toml
shell-words = "1"  # 安全拆分命令参数（处理引号、转义）
```

### 2.2 GitTool 完整实现

```rust
// src/tools/git.rs
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
        "Git 版本控制操作。支持 action: status, diff, log, add, commit, branch, checkout, push。\
         所有操作在工作目录下执行。"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["status", "diff", "log", "add", "commit", "branch", "checkout", "push"],
                    "description": "Git 操作类型"
                },
                "args": {
                    "type": "string",
                    "description": "操作参数。如: diff 的文件路径, commit 的 -m \"message\", add 的文件列表(空格分隔), branch 的分支名, checkout 的目标分支, log 的 --oneline -10, push 的 origin main 等。可留空使用默认行为。"
                }
            },
            "required": ["action"]
        })
    }

    fn pre_validate(&self, args: &serde_json::Value, policy: &SecurityPolicy) -> Option<String> {
        // ReadOnly 模式拒绝所有 git 操作
        if !policy.allows_execution() {
            return Some("只读模式下不允许 Git 操作".to_string());
        }

        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let extra = args.get("args").and_then(|v| v.as_str()).unwrap_or("");

        // 禁止 force push
        if action == "push" && (extra.contains("--force") || extra.contains("-f")) {
            return Some("禁止 force push。如需强推请手动执行。".to_string());
        }

        // 禁止 checkout --force / checkout -f（可能丢失未提交改动）
        if action == "checkout" && (extra.contains("--force") || extra.contains("-f")) {
            return Some("禁止 force checkout。如需强制切换请手动执行。".to_string());
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
            .ok_or_else(|| eyre!("缺少 action 参数"))?;

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
                    })
                } else {
                    Ok(ToolResult {
                        success: false,
                        output: stdout,
                        error: Some(if stderr.is_empty() {
                            format!("git 退出码: {}", output.status.code().unwrap_or(-1))
                        } else {
                            stderr
                        }),
                    })
                }
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("执行 git 命令失败: {}", e)),
            }),
        }
    }
}

/// 根据 action + 额外参数构造 git 命令参数列表
fn build_git_args(action: &str, extra: &str) -> Result<Vec<String>> {
    // 验证 action 合法性
    let valid_actions = ["status", "diff", "log", "add", "commit", "branch", "checkout", "push"];
    if !valid_actions.contains(&action) {
        return Err(eyre!("未知 git action: '{}'。支持: {}", action, valid_actions.join(", ")));
    }

    let mut args = vec![action.to_string()];

    // 追加额外参数（安全拆分，处理引号）
    if !extra.is_empty() {
        let extra_args = shell_words::split(extra)
            .map_err(|e| eyre!("参数解析失败: {}。请检查引号是否匹配。", e))?;
        args.extend(extra_args);
    }

    Ok(args)
}
```

---

## 三、注册 Tool

```rust
// src/tools/mod.rs — 改动

pub mod git;  // 新增

use git::GitTool;  // 新增

pub fn create_tools(
    app_config: Config,
    data_dir: PathBuf,
    log_dir: PathBuf,
    config_path: PathBuf,
    skills: Vec<SkillMeta>,
) -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(ShellTool),
        Box::new(FileReadTool),
        Box::new(FileWriteTool),
        Box::new(ConfigTool),
        Box::new(SelfInfoTool::new(app_config, data_dir, log_dir, config_path)),
        Box::new(SkillTool::new(skills)),
        Box::new(GitTool),  // 新增
    ]
}
```

---

## 四、改动范围

| 文件 | 改动 | 复杂度 |
|------|------|--------|
| `Cargo.toml` | 添加 `shell-words = "1"` | 低 |
| `src/tools/git.rs` | **新增** — GitTool 完整实现 | 中 |
| `src/tools/mod.rs` | 添加 `pub mod git;` + 注册 GitTool | 低 |

**不需要改动**：Agent、Provider、Memory、Security、CLI、Config、其他 Tools。

---

## 五、提交策略

| # | 提交 | 说明 |
|---|------|------|
| 1 | `feat: add shell-words dependency` | Cargo.toml |
| 2 | `feat: add GitTool with 8 git operations` | src/tools/git.rs + mod.rs 注册 |
| 3 | `test: add GitTool unit tests` | build_git_args + pre_validate + execute |

---

## 六、测试用例（~12 个）

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use std::path::PathBuf;

    fn test_policy(workspace: &std::path::Path) -> SecurityPolicy {
        let canonical = workspace.canonicalize().unwrap_or_else(|_| workspace.to_path_buf());
        SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            allowed_commands: vec![],
            workspace_dir: canonical,
            blocked_paths: vec![],
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
        assert!(result.unwrap_err().to_string().contains("未知 git action"));
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
        assert!(result.unwrap().contains("force push"));
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
        assert_eq!(actions.len(), 8);
    }
}
```

---

## 七、关键注意事项

1. **shell-words crate**: 用于安全拆分用户传入的 args 字符串，正确处理引号（如 `-m "feat: add X"`）。不要用 `split_whitespace`，会破坏引号内的空格。

2. **不要用 git2 库**: 项目已有 `tokio::process`，调用系统 git 更简单，且避免引入大依赖。

3. **Supervised 模式**: GitTool 没有特殊的 `pre_validate` 拦截（除了 ReadOnly 和 force），写操作（add/commit/push/checkout）在 Supervised 模式下会走 Agent 的通用确认流程。

4. **测试**: `execute` 的集成测试需要用 `tempfile::tempdir()` 创建临时目录并 `git init`，在里面执行操作。

5. **action 白名单**: 只允许 8 个明确的 action，不支持任意 git 子命令（如 rebase, reset, stash），这是安全设计。
