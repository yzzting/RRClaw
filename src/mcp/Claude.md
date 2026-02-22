# MCP Client 模块设计文档

让 RRClaw Agent 能够调用外部 MCP（Model Context Protocol）Server 提供的工具，无缝扩展能力边界。

## 架构

```
config.toml [mcp.servers.*]
       ↓
McpManager::connect_all()   -- 启动时并发连接所有配置的 MCP Server
       ↓
McpServer (per server)      -- 持有 rmcp RunningService + Peer
       ↓
McpTool (per tool)          -- 实现 RRClaw Tool trait，桥接 MCP call_tool
       ↓
Agent create_tools()        -- 与 ShellTool/GitTool 等一起注入 Agent
```

## 核心结构

```rust
// 管理所有 MCP Server 连接
pub struct McpManager {
    servers: Vec<McpServer>,
}

// 单个 MCP Server（内部）
struct McpServer {
    name: String,
    service: RunningService<RoleClient, ()>,
    peer: Arc<Peer<RoleClient>>,
    allowed_tools: Vec<String>,  // 空 = 允许全部工具
}

// 单个 MCP Tool 的 RRClaw 适配器
pub struct McpTool {
    prefixed_name: String,   // "mcp_{server}_{tool}"，避免与内置工具冲突
    def: McpToolDef,         // MCP 原始定义（含 description + inputSchema）
    original_name: String,   // 发给 MCP server 时用原始名
    peer: Arc<Peer<RoleClient>>,
}
```

## 工具命名规则

MCP tool 在 RRClaw 中的名称加 `mcp_{server}_` 前缀：

```
MCP server "filesystem" 的工具 "read_file"
→ RRClaw tool name: "mcp_filesystem_read_file"
```

**原因**：防止与内置工具名冲突（如 MCP server 也有叫 `git` 的工具）。

调用时 McpTool.execute 用 `original_name` 发给 MCP server，对 LLM 暴露 `prefixed_name`。

## 传输层

支持两种传输协议（`McpTransport` enum）：

| 协议 | 配置 | 说明 |
|------|------|------|
| `stdio` | command + args + env | 子进程通信，最常用（filesystem、fetch 等） |
| `sse` | url + headers | HTTP SSE，用于远程 MCP server |

**stdio 注意事项**：
- 用 `TokioChildProcess::builder` 而非 `TokioChildProcess::new`
- 必须设置 `.stderr(Stdio::null())` 抑制子进程日志，否则污染终端
- 子进程 stderr 默认 inherit，在 builder API 里才能覆盖（坑过一次）

## 配置格式

```toml
# config.toml
[mcp.servers.filesystem]
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
# allowed_tools = ["read_file", "list_directory"]  # 可选白名单，空=全部允许

[mcp.servers.remote]
transport = "sse"
url = "https://my-mcp-server.example.com/sse"
[mcp.servers.remote.headers]
Authorization = "Bearer my-token"
```

## 工具过滤

每个 server 可配置 `allowed_tools` 白名单：
- 空列表（默认）= 允许该 server 的所有工具
- 非空列表 = 只暴露白名单内的工具给 Agent

## 内置 Skill：mcp-install

`src/skills/builtin/mcp-install.md` — 指导 Agent 安装 MCP server（通过 pnpm dlx）。
当用户说"安装 XXX MCP"时，Phase 1 路由会匹配到此 skill。

## 错误处理策略

- 单个 server 连接失败：记录 warn 日志，跳过，不影响其他 server 和主流程
- 单个 server 工具列表获取失败：记录 warn 日志，该 server 贡献 0 个工具
- MCP call_tool 失败：返回 `ToolResult { success: false, error: Some(...) }`，不 panic

**设计原则**：MCP 是可选扩展，任何 MCP 相关失败都不应影响核心 Agent 功能。

## 生命周期

```
startup:  McpManager::connect_all() → 注入 create_tools()
shutdown: McpManager::shutdown()    → 优雅 cancel 所有连接
```

`shutdown` 在 main.rs 的 Ctrl+C 信号处理中调用。

## 文件结构

```
src/mcp/
├── Claude.md   # 本文件
├── mod.rs      # McpManager + McpServer + connect_server()
└── tool.rs     # McpTool（实现 Tool trait）
```

## 测试要求

- `mcp_tool_name_has_prefix`：验证命名规则
- 集成测试暂缺：需要真实 MCP server 进程，标记 `#[ignore]`
- 可用 stdio echo 工具做轻量集成测试（future work）
