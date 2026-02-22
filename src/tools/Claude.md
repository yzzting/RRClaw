# Tools 模块设计文档

提供 Agent 可调用的工具，所有工具受 SecurityPolicy 约束。

## Tool trait

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> serde_json::Value;
    async fn execute(&self, args: serde_json::Value, policy: &SecurityPolicy) -> Result<ToolResult>;

    /// 执行前预检，返回 Some(reason) 表示拒绝（在用户确认前调用）
    fn pre_validate(&self, args: &serde_json::Value, policy: &SecurityPolicy) -> Option<String> {
        None
    }

    fn spec(&self) -> ToolSpec { /* 默认实现 */ }
}
```

关联类型（`ToolSpec` 定义在 `providers::traits`，此模块 re-export）：

```rust
ToolResult { success: bool, output: String, error: Option<String> }
```

## 工具清单

### ShellTool（P0）

- 参数：`command: String`
- 安全检查：ReadOnly 拒绝 → 白名单检查（Full 模式） → Supervised 走用户确认
- 执行：`tokio::process::Command`，timeout 30s，工作目录 = `policy.workspace_dir`

### FileReadTool / FileWriteTool（P0）

- 参数：`path: String` / `path + content`
- 安全检查：`policy.is_path_allowed(path)`（workspace 范围 + symlink 防逃逸）
- FileWriteTool 额外检查：ReadOnly 模式拒绝

### ConfigTool（P2）

- 参数：`action: enum["get","set","list","append"]`, `key`, `value`
- 执行：`toml_edit` 读写 `~/.rrclaw/config.toml`，保留注释和格式
- 安全检查：`pre_validate` 禁止修改 `security.autonomy`（防止 LLM 自我提权）

### SelfInfoTool（P2）

- 参数：`query: enum["config","paths","provider","stats","help"]`
- 执行：纯读取，返回 RRClaw 自身状态。API key 脱敏（前4位 + `****`）
- 设计：配合 system prompt 决策原则"先查后做"使用，不要每轮都调用

### SkillTool（P3）

- 参数：`name: String`（skill 名称）
- 执行：加载 skill L2 内容（`~/.rrclaw/skills/{name}/SKILL.md` 或内置）
- 用途：Phase 1 未路由到的 skill，LLM 可在 Phase 2 自行调用兜底

### GitTool（P4）

- 参数：`action: enum["status","diff","log","add","commit","branch","checkout","push","pull","fetch"]`, `extra: String`（可选）
- 安全拦截（`pre_validate`）：
  - `push --force` / `push -f` → 拒绝
  - `checkout --force` / `checkout -f` → 拒绝
- 执行：`git {action} {extra}`，在 `policy.workspace_dir` 下运行
- 比 ShellTool 更安全：action 白名单、强制操作前置拦截

### HttpRequestTool（P4）

- 参数：`method`, `url`, `headers`（可选）, `body`（可选）, `extract`（可选）
- SSRF 防护：阻止 localhost / 内网 IP / 云元数据接口（169.254.x.x 等）
- `allowed_hosts` 白名单：用户可在 config.toml 添加受信任的内网地址，**实时读文件**（无需重启）
- 响应处理：
  - JSON / 纯文本：直接返回，最大 1MB
  - HTML：自动 strip 标签/脚本，最大 200KB
  - strip 后 > 200KB 且有 `extract` 参数：mini-LLM 提取目标信息
- 不自动跟随重定向（3xx 直接返回 Location header）

### MemoryStoreTool / MemoryRecallTool / MemoryForgetTool（P4）

三个工具共享同一个 `Arc<dyn Memory>` 实例（与主 Agent 共享记忆）。

| 工具 | 参数 | 用途 |
|------|------|------|
| `memory_store` | key, content, category | 保存用户偏好/约定/知识 |
| `memory_recall` | query, limit(默认5) | 语义搜索相关记忆 |
| `memory_forget` | key | 删除指定记忆 |

**注意**：memory 工具的结果**不做 injection 检测**（返回受控内容，见 `needs_injection_check()`）。

### RoutineTool（P5）

- 参数：`action: enum["list","add","delete","enable","disable","run","logs"]`，各 action 有额外参数
- 执行：通过 `Arc<RoutineEngine>` 管理定时任务（LLM 驱动的 CRUD）
- 时间解析：调用 LLM 将自然语言转 cron，而非正则（P5 教训）

### McpTool（P4，动态生成）

- 从 `McpManager` 动态生成，每个 MCP server 工具对应一个 McpTool 实例
- 命名：`mcp_{server}_{tool}`，避免与内置工具冲突
- 执行：透传 `peer.call_tool(params)` 到 MCP server
- 详见 `src/mcp/Claude.md`

## Injection 检测白名单

`needs_injection_check(tool_name)` 决定哪些工具结果需要 prompt injection 检测：

```
需要检测（外部数据来源）: shell, file_read, file_write, git, http_request
跳过检测（受控内容）:    memory_*, skill, self_info, config, routine
```

**背景**：`memory_recall` 返回格式化记忆列表，行数多但完全受控，历史上误触发 Review WARN。

## 工具注册

```rust
pub fn create_tools(
    app_config: Config,
    data_dir: PathBuf,
    log_dir: PathBuf,
    config_path: PathBuf,
    memory: Arc<dyn Memory>,
    routine_engine: Option<Arc<RoutineEngine>>,
    mcp_tools: Vec<Box<dyn Tool>>,
) -> Vec<Box<dyn Tool>>
```

MCP tools 由 `McpManager::tools()` 获取后传入，统一注册。

## 文件结构

```
src/tools/
├── Claude.md     # 本文件
├── mod.rs        # create_tools() 工厂 + re-exports
├── traits.rs     # Tool trait + ToolResult
├── shell.rs      # ShellTool
├── file.rs       # FileReadTool + FileWriteTool
├── config.rs     # ConfigTool
├── self_info.rs  # SelfInfoTool
├── skill.rs      # SkillTool
├── git.rs        # GitTool（含 pre_validate 安全拦截）
├── http.rs       # HttpRequestTool（含 SSRF 防护）
├── memory.rs     # MemoryStoreTool / MemoryRecallTool / MemoryForgetTool
└── routine.rs    # RoutineTool
```

## 测试要求

- 每个工具的 `pre_validate` 必须有单元测试（含拦截案例）
- SecurityPolicy 各级别（ReadOnly/Supervised/Full）分别测
- GitTool：force push/checkout 拦截测试（已有）
- HttpRequestTool：SSRF 防护测试、HTML strip 测试（已有）
- MemoryTools：store → recall → forget 完整流程测（已有）
