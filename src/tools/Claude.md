# Tools 模块

## 职责

提供 Agent 可调用的工具，所有工具受 SecurityPolicy 约束。

## Tool trait

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> serde_json::Value;
    async fn execute(&self, args: serde_json::Value, policy: &SecurityPolicy) -> Result<ToolResult>;

    fn spec(&self) -> ToolSpec { /* 默认实现：组合 name/description/parameters_schema */ }
}
```

## 关联类型

```rust
ToolResult { success: bool, output: String, error: Option<String> }
```

`ToolSpec` 已在 `providers::traits` 中定义，此模块 re-export。

## MVP 工具

### ShellTool

- 参数: `command: String`
- 安全检查:
  1. `policy.allows_execution()` — ReadOnly 模式拒绝
  2. `policy.is_command_allowed(cmd)` — 白名单检查
- 执行: `tokio::process::Command` 运行命令，捕获 stdout + stderr
- 超时: 30 秒
- 工作目录: `policy.workspace_dir`

### FileReadTool

- 参数: `path: String`
- 安全检查: `policy.is_path_allowed(path)`
- 执行: `tokio::fs::read_to_string()`

### FileWriteTool

- 参数: `path: String`, `content: String`
- 安全检查:
  1. `policy.allows_execution()` — ReadOnly 模式拒绝
  2. `policy.is_path_allowed(path)`
- 执行: `tokio::fs::write()`

## 工具注册

```rust
pub fn create_tools() -> Vec<Box<dyn Tool>>
```

返回所有 MVP 工具实例。

## 文件结构

- `mod.rs` — re-exports + `create_tools()` 工厂
- `traits.rs` — Tool trait + ToolResult
- `shell.rs` — ShellTool
- `file.rs` — FileReadTool + FileWriteTool
