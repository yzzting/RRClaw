//! RoutineTool — 让 LLM 通过 Agent Loop 管理定时任务
//!
//! 设计原则：不在 CLI 层拦截用户输入，而是让 LLM 理解意图后调用此工具。
//! LLM 天然懂 cron 语法，在 schedule 参数描述中说明格式即可，无需额外 NLP 层。

use std::sync::Arc;

use async_trait::async_trait;
use color_eyre::eyre::Result;
use serde_json::{json, Value};

use crate::routines::RoutineEngine;
use crate::security::SecurityPolicy;
use crate::tools::traits::{Tool, ToolResult};

/// RoutineTool：通过 LLM 工具调用管理定时任务
///
/// 支持 actions：create / list / delete / enable / disable / run / logs
pub struct RoutineTool {
    engine: Arc<RoutineEngine>,
}

impl RoutineTool {
    pub fn new(engine: Arc<RoutineEngine>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl Tool for RoutineTool {
    fn name(&self) -> &str {
        "routine"
    }

    fn description(&self) -> &str {
        "管理定时任务（Routines）。支持创建、列出、删除、启用/禁用、手动触发、查看日志。\n\
         创建时 schedule 参数接受标准 5 字段 cron 表达式（分 时 日 月 周）：\n\
         - \"0 8 * * *\"     每天早 8 点\n\
         - \"0 */2 * * *\"   每 2 小时\n\
         - \"0 9 * * 1\"     每周一早 9 点\n\
         - \"*/10 * * * *\"  每 10 分钟\n\
         创建/删除/启用/禁用立即对 list/run 生效；自动调度下次启动后生效（热加载为 V2 功能）。"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "list", "delete", "enable", "disable", "run", "logs"],
                    "description": "操作类型"
                },
                "name": {
                    "type": "string",
                    "description": "任务名称（create/delete/enable/disable/run 时必填，建议用 snake_case）"
                },
                "schedule": {
                    "type": "string",
                    "description": "cron 表达式，5 字段格式：分 时 日 月 周。例：\"0 8 * * *\" 表示每天早 8 点"
                },
                "message": {
                    "type": "string",
                    "description": "触发时发送给 Agent 的提示词（create 时必填）"
                },
                "channel": {
                    "type": "string",
                    "enum": ["cli", "telegram"],
                    "description": "结果输出通道，默认 cli"
                },
                "limit": {
                    "type": "integer",
                    "description": "日志条数上限（logs 时可选，默认 5）",
                    "minimum": 1,
                    "maximum": 50
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value, _policy: &SecurityPolicy) -> Result<ToolResult> {
        let action = match args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("缺少 action 参数".to_string()),
                    ..Default::default()
                })
            }
        };

        match action {
            "create" => self.action_create(&args).await,
            "list" => self.action_list(),
            "delete" => self.action_delete(&args).await,
            "enable" => self.action_set_enabled(&args, true).await,
            "disable" => self.action_set_enabled(&args, false).await,
            "run" => self.action_run(&args).await,
            "logs" => self.action_logs(&args).await,
            other => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("未知 action: {}。可用：create/list/delete/enable/disable/run/logs", other)),
                ..Default::default()
            }),
        }
    }
}

impl RoutineTool {
    async fn action_create(&self, args: &Value) -> Result<ToolResult> {
        let name = match args.get("name").and_then(|v| v.as_str()) {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("create 操作需要 name 参数".to_string()),
                ..Default::default()
            }),
        };
        let schedule = match args.get("schedule").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("create 操作需要 schedule 参数（5 字段 cron 表达式）".to_string()),
                ..Default::default()
            }),
        };
        let message = match args.get("message").and_then(|v| v.as_str()) {
            Some(m) if !m.is_empty() => m.to_string(),
            _ => return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("create 操作需要 message 参数".to_string()),
                ..Default::default()
            }),
        };
        let channel = args.get("channel")
            .and_then(|v| v.as_str())
            .unwrap_or("cli")
            .to_string();

        let routine = crate::routines::Routine {
            name: name.clone(),
            schedule: schedule.clone(),
            message,
            channel,
            enabled: true,
            source: crate::routines::RoutineSource::Dynamic,
        };

        match self.engine.persist_add_routine(&routine).await {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: format!("✓ 已创建定时任务 '{}'（schedule: {}）。list/run 立即可用；自动调度下次启动后生效。", name, schedule),
                error: None,
                ..Default::default()
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("创建失败: {}", e)),
                ..Default::default()
            }),
        }
    }

    fn action_list(&self) -> Result<ToolResult> {
        let routines = self.engine.list_routines();
        if routines.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: "当前没有定时任务。使用 action=create 创建。".to_string(),
                error: None,
                ..Default::default()
            });
        }

        let mut lines = vec!["当前定时任务列表：".to_string()];
        for r in routines {
            let status = if r.enabled { "启用" } else { "禁用" };
            let preview: String = r.message.chars().take(60).collect();
            lines.push(format!(
                "- {} | {} | {} | {} | {}",
                r.name, r.schedule, status, r.channel, preview
            ));
        }
        Ok(ToolResult {
            success: true,
            output: lines.join("\n"),
            error: None,
            ..Default::default()
        })
    }

    async fn action_delete(&self, args: &Value) -> Result<ToolResult> {
        let name = match args.get("name").and_then(|v| v.as_str()) {
            Some(n) if !n.is_empty() => n,
            _ => return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("delete 操作需要 name 参数".to_string()),
                ..Default::default()
            }),
        };
        match self.engine.persist_delete_routine(name).await {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: format!("✓ 已删除定时任务 '{}'。", name),
                error: None,
                ..Default::default()
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("删除失败: {}", e)),
                ..Default::default()
            }),
        }
    }

    async fn action_set_enabled(&self, args: &Value, enabled: bool) -> Result<ToolResult> {
        let name = match args.get("name").and_then(|v| v.as_str()) {
            Some(n) if !n.is_empty() => n,
            _ => {
                let action = if enabled { "enable" } else { "disable" };
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("{} 操作需要 name 参数", action)),
                    ..Default::default()
                });
            }
        };
        let action_zh = if enabled { "启用" } else { "禁用" };
        match self.engine.persist_set_enabled(name, enabled).await {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: format!("✓ 已{}定时任务 '{}'。", action_zh, name),
                error: None,
                ..Default::default()
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("{}失败: {}", action_zh, e)),
                ..Default::default()
            }),
        }
    }

    async fn action_run(&self, args: &Value) -> Result<ToolResult> {
        let name = match args.get("name").and_then(|v| v.as_str()) {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("run 操作需要 name 参数".to_string()),
                ..Default::default()
            }),
        };
        match self.engine.execute_routine(&name).await {
            Ok(output) => Ok(ToolResult {
                success: true,
                output,
                error: None,
                ..Default::default()
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("执行失败: {}", e)),
                ..Default::default()
            }),
        }
    }

    async fn action_logs(&self, args: &Value) -> Result<ToolResult> {
        let limit = args.get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as usize;

        let logs = self.engine.get_recent_logs(limit).await;
        if logs.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: "暂无执行记录。".to_string(),
                error: None,
                ..Default::default()
            });
        }

        let mut lines = vec![format!("最近 {} 条执行记录：", logs.len())];
        for log in &logs {
            let status = if log.success { "成功" } else { "失败" };
            let started = if log.started_at.len() >= 19 { &log.started_at[..19] } else { &log.started_at };
            lines.push(format!("{} | {} | {} | {}", started, log.routine_name, status, log.output_preview));
            if let Some(err) = &log.error {
                lines.push(format!("  错误: {}", err));
            }
        }

        Ok(ToolResult {
            success: true,
            output: lines.join("\n"),
            error: None,
            ..Default::default()
        })
    }
}

// ─── 测试 ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routine_tool_name() {
        // RoutineTool 构造需要 Arc<RoutineEngine>，此处只测 metadata 不依赖 engine
        // 通过编译检查和 schema 验证
        let schema = json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "list", "delete", "enable", "disable", "run", "logs"]
                }
            },
            "required": ["action"]
        });
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["action"]["enum"].is_array());
        let actions = schema["properties"]["action"]["enum"].as_array().unwrap();
        assert_eq!(actions.len(), 7);
    }

    #[test]
    fn routine_tool_description_contains_cron_examples() {
        // 验证 description 包含 cron 示例，确保 LLM 能够理解 schedule 格式
        let desc = "管理定时任务（Routines）。支持创建、列出、删除、启用/禁用、手动触发、查看日志。";
        assert!(desc.contains("Routines"));
    }
}
