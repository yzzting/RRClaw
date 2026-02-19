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

## 工具清单

### SelfInfoTool（P2 新增）

- 参数: `query: enum["config", "paths", "provider", "stats", "help"]`
- 安全检查: 无（纯读取，无副作用）
- 用途: Agent 按需查询自身信息，替代 system prompt 硬编码
- API key 脱敏: 只显示前 4 位 + `****`
- 设计: 配合 system prompt 决策原则第 1 条"先查后做"使用

### ConfigTool

- 参数: `action: enum["get", "set", "list"]`, `key`, `value`
- 安全检查: `pre_validate` 禁止修改 `security.autonomy`
- 执行: 通过 `toml_edit` 读写 `~/.rrclaw/config.toml`

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
pub fn create_tools(
    app_config: Config,
    data_dir: PathBuf,
    log_dir: PathBuf,
    config_path: PathBuf,
) -> Vec<Box<dyn Tool>>
```

返回所有工具实例。`SelfInfoTool` 需要 Config 和路径信息。

## 文件结构

- `mod.rs` — re-exports + `create_tools()` 工厂
- `traits.rs` — Tool trait + ToolResult
- `shell.rs` — ShellTool
- `file.rs` — FileReadTool + FileWriteTool
- `config.rs` — ConfigTool（读写 config.toml）
- `self_info.rs` — SelfInfoTool（Agent 自我信息查询）
