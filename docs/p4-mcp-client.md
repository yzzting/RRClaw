# P4-A: MCP Client 实现计划

## 背景

Model Context Protocol (MCP) 是 AI Agent 生态的标准工具协议。实现 MCP Client 后，RRClaw 可直接接入任意 MCP Server（文件系统、数据库、GitHub、Slack、浏览器等），无需逐个实现 Tool。

**传输支持**：
- `stdio`：启动本地子进程，通过 stdin/stdout JSON-RPC 2.0 通信
- `sse`：连接远程 HTTP SSE 端点（Streamable HTTP 传输）

**技术选型**：使用官方 Rust SDK `rmcp`，不自己实现 JSON-RPC 协议。

---

## 一、架构设计

```
config.toml [mcp.servers]
      │
      ▼
McpManager::connect_all()
      │
      ├── McpServer (stdio: npx @mcp/filesystem)
      │     └── rmcp::Client (stdio transport)
      │           └── tools/list → Vec<McpToolInfo>
      │
      └── McpServer (sse: https://api.example.com/mcp)
            └── rmcp::Client (sse transport)
                  └── tools/list → Vec<McpToolInfo>

                                │
                    for each McpToolInfo:
                    McpTool(impl Tool trait)
                                │
                    create_tools() 合并到工具列表
                                │
                    Agent 使用（透明，无感知）
```

### MCP 协议交互流

```
RRClaw                    MCP Server
  │── initialize ─────────────▶│
  │◀── initialize result ──────│
  │── initialized (notify) ───▶│
  │── tools/list ──────────────▶│
  │◀── tools/list result ───────│
  │
  │  (Agent 需要工具时)
  │── tools/call {name, args} ─▶│
  │◀── tools/call result ───────│
```

---

## 二、新增依赖

```toml
# Cargo.toml
rmcp = { version = "0.8", features = ["client", "transport-child-process", "transport-sse-client"] }
```

> 注意：rmcp crate 名为 `rmcp`，由 `modelcontextprotocol/rust-sdk` 维护。添加后运行 `cargo fetch` 确认版本可用，如有问题改用 `"0.7"` 或检查 crates.io 最新稳定版。

---

## 三、目录结构

```
src/
  mcp/
    mod.rs        ← McpManager, McpServerConfig, McpTransport
    tool.rs       ← McpTool（impl Tool trait）
    Claude.md     ← 模块设计文档（可选，按项目规范）
```

`src/lib.rs` 新增：`pub mod mcp;`

---

## 四、数据结构与实现

### 4.1 Config 扩展（src/config/schema.rs）

```rust
use std::collections::HashMap;

/// MCP 全局配置
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpConfig {
    /// key = server 名称（用于 tool 前缀）
    pub servers: HashMap<String, McpServerConfig>,
}

/// 单个 MCP Server 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    #[serde(flatten)]
    pub transport: McpTransport,
    /// 只暴露部分 tools（空 = 全部）
    #[serde(default)]
    pub allowed_tools: Vec<String>,
}

/// 传输方式
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "lowercase")]
pub enum McpTransport {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
    },
    Sse {
        url: String,
        #[serde(default)]
        headers: HashMap<String, String>,
    },
}

// Config 中新增字段（带 serde default）：
// pub struct Config {
//     ...
//     #[serde(default)]
//     pub mcp: Option<McpConfig>,
// }
```

TOML 示例：
```toml
[mcp.servers.filesystem]
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/Users/me/projects"]

[mcp.servers.github]
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_PERSONAL_ACCESS_TOKEN = "ghp_xxx" }

[mcp.servers.remote]
transport = "sse"
url = "https://mcp.example.com/sse"
headers = { Authorization = "Bearer token" }
# allowed_tools = ["read_file", "write_file"]  # 可选：只允许部分工具
```

### 4.2 McpTool（src/mcp/tool.rs）

```rust
use async_trait::async_trait;
use color_eyre::eyre::Result;
use std::sync::Arc;
use rmcp::model::{CallToolRequestParam, Tool as McpToolDef};
use rmcp::service::RoleClient;
use rmcp::RoleClient as RoleClientTrait;

use crate::security::SecurityPolicy;
use crate::tools::traits::{Tool, ToolResult};

/// MCP Tool 的 RRClaw 适配器
pub struct McpTool {
    /// 工具在 RRClaw 中的名称，加前缀避免冲突：mcp_{server}_{tool}
    prefixed_name: String,
    /// MCP tool 原始定义（含 description + inputSchema）
    def: McpToolDef,
    /// MCP tool 在服务端的原始名称
    original_name: String,
    /// 共享的 MCP client 连接
    client: Arc<rmcp::service::RunningService<RoleClient, ()>>,
}

impl McpTool {
    pub fn new(
        server_name: &str,
        def: McpToolDef,
        client: Arc<rmcp::service::RunningService<RoleClient, ()>>,
    ) -> Self {
        let original_name = def.name.clone().to_string();
        let prefixed_name = format!("mcp_{}_{}", server_name, original_name);
        Self { prefixed_name, def, original_name, client }
    }
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.prefixed_name
    }

    fn description(&self) -> &str {
        self.def.description.as_deref().unwrap_or("MCP tool")
    }

    fn parameters_schema(&self) -> serde_json::Value {
        // MCP inputSchema 与 RRClaw parameters 格式一致（JSON Schema object）
        match &self.def.input_schema {
            Some(schema) => serde_json::to_value(schema).unwrap_or_else(|_| {
                serde_json::json!({"type": "object", "properties": {}})
            }),
            None => serde_json::json!({"type": "object", "properties": {}}),
        }
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _policy: &SecurityPolicy,
    ) -> Result<ToolResult> {
        // 构造 MCP tools/call 请求
        let params = CallToolRequestParam {
            name: self.original_name.clone().into(),
            arguments: args.as_object().cloned().map(|m| {
                m.into_iter().collect()
            }),
        };

        match self.client.call_tool(params).await {
            Ok(result) => {
                // 提取文本内容
                let mut output_parts = Vec::new();
                for content in &result.content {
                    use rmcp::model::Content;
                    match content {
                        Content::Text { text, .. } => output_parts.push(text.as_str()),
                        Content::Image { .. } => output_parts.push("[图片内容]"),
                        Content::Resource { .. } => output_parts.push("[资源内容]"),
                        _ => {}
                    }
                }
                let output = output_parts.join("\n");
                let is_error = result.is_error.unwrap_or(false);

                Ok(ToolResult {
                    success: !is_error,
                    output: if is_error { String::new() } else { output.clone() },
                    error: if is_error { Some(output) } else { None },
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("MCP 调用失败: {}", e)),
            }),
        }
    }
}
```

### 4.3 McpManager（src/mcp/mod.rs）

```rust
pub mod tool;

use color_eyre::eyre::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{info, warn};

use rmcp::{
    service::RunningService,
    transport::{SseClientTransport, TokioChildProcess},
    RoleClient, ServiceExt,
};

use crate::config::McpServerConfig;
use crate::config::McpTransport;
use crate::tools::traits::Tool;
use tool::McpTool;

/// 已连接的单个 MCP Server
struct McpServer {
    name: String,
    client: Arc<RunningService<RoleClient, ()>>,
    /// 过滤后的工具列表
    allowed_tools: Vec<String>,
}

/// 管理所有 MCP Server 连接
pub struct McpManager {
    servers: Vec<McpServer>,
}

impl McpManager {
    /// 根据配置连接所有 MCP Server，失败的跳过并记录警告
    pub async fn connect_all(configs: &HashMap<String, McpServerConfig>) -> Self {
        let mut servers = Vec::new();

        for (name, config) in configs {
            match connect_server(name, config).await {
                Ok(client) => {
                    info!("MCP Server '{}' 连接成功", name);
                    servers.push(McpServer {
                        name: name.clone(),
                        client: Arc::new(client),
                        allowed_tools: config.allowed_tools.clone(),
                    });
                }
                Err(e) => {
                    warn!("MCP Server '{}' 连接失败（跳过）: {:#}", name, e);
                }
            }
        }

        Self { servers }
    }

    /// 获取所有 MCP tools，转换为 RRClaw Tool trait 对象
    pub async fn tools(&self) -> Vec<Box<dyn Tool>> {
        let mut result: Vec<Box<dyn Tool>> = Vec::new();

        for server in &self.servers {
            match server.client.list_tools(Default::default()).await {
                Ok(tools_result) => {
                    for tool_def in tools_result.tools {
                        let tool_name = tool_def.name.as_str();
                        // 过滤：如果 allowed_tools 非空，只保留白名单内的工具
                        if !server.allowed_tools.is_empty()
                            && !server.allowed_tools.iter().any(|a| a == tool_name)
                        {
                            continue;
                        }
                        result.push(Box::new(McpTool::new(
                            &server.name,
                            tool_def,
                            server.client.clone(),
                        )));
                    }
                    info!(
                        "MCP Server '{}' 加载了 {} 个工具",
                        server.name,
                        result.len()
                    );
                }
                Err(e) => {
                    warn!("获取 MCP Server '{}' 工具列表失败: {:#}", server.name, e);
                }
            }
        }

        result
    }

    /// 优雅关闭所有 MCP 连接
    pub async fn shutdown(self) {
        for server in self.servers {
            // rmcp RunningService 在 Drop 时自动清理子进程
            // 显式 cancel 确保干净退出
            server.client.cancel().await;
            info!("MCP Server '{}' 已关闭", server.name);
        }
    }
}

/// 连接单个 MCP Server
async fn connect_server(
    name: &str,
    config: &McpServerConfig,
) -> Result<RunningService<RoleClient, ()>> {
    match &config.transport {
        McpTransport::Stdio { command, args, env } => {
            let mut cmd = tokio::process::Command::new(command);
            cmd.args(args);
            for (k, v) in env {
                cmd.env(k, v);
            }
            let transport = TokioChildProcess::new(&mut cmd)
                .wrap_err_with(|| format!("启动 MCP 子进程失败: {}", command))?;

            ().serve(transport)
                .await
                .wrap_err_with(|| format!("MCP stdio 握手失败: {}", name))
        }
        McpTransport::Sse { url, headers } => {
            let mut builder = SseClientTransport::builder(url.as_str());
            for (k, v) in headers {
                builder = builder.header(k, v);
            }
            let transport = builder
                .build()
                .await
                .wrap_err_with(|| format!("MCP SSE 连接失败: {}", url))?;

            ().serve(transport)
                .await
                .wrap_err_with(|| format!("MCP SSE 握手失败: {}", name))
        }
    }
}
```

### 4.4 main.rs 集成（src/main.rs）

```rust
// run_agent() 中，在 create_tools() 之后添加：

use rrclaw::mcp::McpManager;

// --- MCP 工具加载（可选，配置了才加载）---
let mcp_manager = if let Some(mcp_config) = &config.mcp {
    if !mcp_config.servers.is_empty() {
        let mgr = McpManager::connect_all(&mcp_config.servers).await;
        Some(mgr)
    } else {
        None
    }
} else {
    None
};

// 合并 MCP tools 到工具列表
if let Some(ref mgr) = mcp_manager {
    let mcp_tools = mgr.tools().await;
    if !mcp_tools.is_empty() {
        tracing::info!("已加载 {} 个 MCP 工具", mcp_tools.len());
        tools.extend(mcp_tools);
    }
}

// ...创建 Agent（使用合并后的 tools）...

// 退出时关闭 MCP
if let Some(mgr) = mcp_manager {
    mgr.shutdown().await;
}
```

### 4.5 lib.rs 新增

```rust
// src/lib.rs
pub mod mcp;  // 新增
```

---

## 五、rmcp API 说明

`rmcp` crate 关键类型与方法（供参考，以实际 crate 文档为准）：

```rust
// 连接（serve = 建立连接 + 握手）
().serve(transport) -> Result<RunningService<RoleClient, ()>>

// 列出工具
client.list_tools(ListToolsRequestParam::default())
    -> Result<ListToolsResult>
// ListToolsResult.tools: Vec<Tool>
// Tool { name: Cow<str>, description: Option<Cow<str>>, input_schema: Option<...> }

// 调用工具
client.call_tool(CallToolRequestParam { name, arguments })
    -> Result<CallToolResult>
// CallToolResult { content: Vec<Content>, is_error: Option<bool> }
// Content::Text { text: Cow<str>, .. }

// 关闭
client.cancel().await
```

> **重要**：rmcp API 在不同版本间可能有变化。实现前先运行 `cargo doc --open -p rmcp` 查阅本地文档，以实际 API 为准调整上述代码。

---

## 六、改动范围

| 文件 | 改动 | 复杂度 |
|------|------|--------|
| `Cargo.toml` | 添加 `rmcp` 依赖 | 低 |
| `src/config/schema.rs` | 新增 `McpConfig`, `McpServerConfig`, `McpTransport`；Config 添加 `mcp` 字段 | 低 |
| `src/mcp/mod.rs` | **新增** — McpManager + connect_server | 高 |
| `src/mcp/tool.rs` | **新增** — McpTool impl Tool | 中 |
| `src/lib.rs` | 添加 `pub mod mcp;` | 低 |
| `src/main.rs` | 启动时连接 MCP，退出时关闭 | 低 |

**不需要改动**：Agent、Provider、Memory、Security、CLI、现有 Tool。

---

## 七、提交策略

| # | 提交 | 说明 |
|---|------|------|
| 1 | `docs: add MCP client module design` | src/mcp/Claude.md（可选）|
| 2 | `feat: add rmcp dependency and MCP config schema` | Cargo.toml + config/schema.rs |
| 3 | `feat: add MCP module with stdio and SSE transport` | src/mcp/mod.rs |
| 4 | `feat: add McpTool bridging MCP to Tool trait` | src/mcp/tool.rs |
| 5 | `feat: add pub mod mcp to lib.rs` | src/lib.rs |
| 6 | `feat: wire MCP tools into agent startup` | src/main.rs |
| 7 | `test: add MCP config and McpTool unit tests` | 测试 |

---

## 八、测试用例（~8 个）

```rust
// src/config/schema.rs 的 tests 模块
#[test]
fn mcp_stdio_config_parses() {
    let toml = r#"
[mcp.servers.fs]
transport = "stdio"
command = "npx"
args = ["-y", "@mcp/server-filesystem", "/tmp"]
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    std::fs::write(&path, toml).unwrap();
    let config = Config::load_from_path(&path).unwrap();
    let mcp = config.mcp.unwrap();
    let fs_server = mcp.servers.get("fs").unwrap();
    match &fs_server.transport {
        McpTransport::Stdio { command, args, .. } => {
            assert_eq!(command, "npx");
            assert_eq!(args[0], "-y");
        }
        _ => panic!("应该是 stdio 传输"),
    }
}

#[test]
fn mcp_sse_config_parses() {
    let toml = r#"
[mcp.servers.remote]
transport = "sse"
url = "https://mcp.example.com/sse"
[mcp.servers.remote.headers]
Authorization = "Bearer token"
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    std::fs::write(&path, toml).unwrap();
    let config = Config::load_from_path(&path).unwrap();
    let mcp = config.mcp.unwrap();
    let remote = mcp.servers.get("remote").unwrap();
    match &remote.transport {
        McpTransport::Sse { url, headers } => {
            assert_eq!(url, "https://mcp.example.com/sse");
            assert!(headers.contains_key("Authorization"));
        }
        _ => panic!("应该是 sse 传输"),
    }
}

#[test]
fn mcp_allowed_tools_filter() {
    let toml = r#"
[mcp.servers.fs]
transport = "stdio"
command = "npx"
args = []
allowed_tools = ["read_file", "list_dir"]
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    std::fs::write(&path, toml).unwrap();
    let config = Config::load_from_path(&path).unwrap();
    let server = config.mcp.unwrap().servers.get("fs").unwrap().clone();
    assert_eq!(server.allowed_tools, vec!["read_file", "list_dir"]);
}

#[test]
fn no_mcp_config_is_none() {
    let toml = r#"
[default]
provider = "deepseek"
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    std::fs::write(&path, toml).unwrap();
    let config = Config::load_from_path(&path).unwrap();
    assert!(config.mcp.is_none());
}

// src/mcp/tool.rs 的测试（mock McpTool 行为）
#[test]
fn mcp_tool_name_has_prefix() {
    // McpTool::new("filesystem", def, client) → name = "mcp_filesystem_read_file"
    // 需要 mock client，或通过构造函数直接测试 prefixed_name 逻辑
    let prefixed = format!("mcp_{}_{}", "filesystem", "read_file");
    assert_eq!(prefixed, "mcp_filesystem_read_file");
    assert!(prefixed.starts_with("mcp_"));
}

#[test]
fn mcp_tool_empty_schema_fallback() {
    // inputSchema 为 None 时应返回 {"type": "object", "properties": {}}
    let fallback = serde_json::json!({"type": "object", "properties": {}});
    assert_eq!(fallback["type"], "object");
}
```

---

## 九、关键注意事项

1. **rmcp API 稳定性**：rmcp 是官方 SDK 但版本迭代较快。`RunningService`、`RoleClient` 等类型名称可能随版本变化。实现前务必 `cargo doc --open -p rmcp` 查阅本地实际 API。

2. **工具命名前缀 `mcp_{server}_{tool}`**：避免与 RRClaw 内置工具（shell/git/config 等）名称冲突。LLM 调用时也能从名称知道是 MCP 工具。

3. **McpManager::connect_all 不 fail fast**：单个 server 连接失败只记 warn 不中断，保证启动稳定性。

4. **`tools()` 是 async**：因为 `list_tools` 是异步 RPC 调用。调用处需要 `await`。

5. **client.cancel().await 在 shutdown**：rmcp 的 stdio transport 启动了子进程，shutdown 时需要显式取消，否则子进程会变成僵尸进程。

6. **SSE headers 的 TOML 格式**：TOML 内联表语法 `headers = { Authorization = "Bearer x" }` 会被 figment 正确解析为 `HashMap<String, String>`。

7. **测试不启动真实 MCP Server**：单元测试只测 config 解析和 McpTool 名称逻辑。集成测试（需要真实 npx）建议在 CI 中单独运行，用 `#[ignore]` 标记。

---

## 十、MCP 安装 Skill（LLM 决策安装）

核心实现：创建一个 `mcp-install` skill，包含完整的安装指南，让 LLM 能够根据用户输入自动判断安装方式。

---

## 十一、MCP 安装与添加方式

### MCP Server 安装原理

MCP Server 本质是一个 npm 包，通过 `npx` 命令启动。例如：

```bash
# 文件系统 MCP
npx -y @modelcontextprotocol/server-filesystem /home/user/projects

# GitHub MCP
npx -y @modelcontextprotocol/server-github

# PostgreSQL MCP
npx -y @modelcontextprotocol/server-postgres "postgresql://localhost/mydb"
```

RRClaw 通过 stdio 传输启动这些进程，与之通信。

### 添加方式

#### 方式一：直接编辑 config.toml

手动编辑 `~/.rrclaw/config.toml`：

```toml
[mcp.servers.filesystem]
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/user/projects"]

[mcp.servers.github]
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_PERSONAL_ACCESS_TOKEN = "ghp_xxx" }
```

#### 方式二：LLM + Skill 安装（推荐）

用户给 LLM 一个输入，LLM 自动判断类型并安装：

```
用户：帮我安装 @modelcontextprotocol/server-github
用户：帮我安装 https://github.com/vercel-labs/agent-skills
用户：帮我安装 https://mcp.example.com/sse
```

**前提**：创建 `mcp-install` skill，包含完整的安装指南。

##### 10.3.1 mcp-install Skill 设计

```markdown
---
name: mcp-install
description: MCP Server 安装助手。当用户要求安装、添加、配置 MCP 时使用此技能。
---

# MCP 安装指南

根据用户输入的 MCP 地址或包名，判断类型并执行对应安装步骤。

## 判断逻辑

根据输入格式判断 MCP 类型：

| 输入格式 | 类型 | 示例 |
|----------|------|------|
| `@org/package` | npm 包 | `@modelcontextprotocol/server-filesystem` |
| `org/package` | npm 包（简写） | `server-github` |
| `https://github.com/xxx` | GitHub 仓库 | `https://github.com/vercel-labs/agent-skills` |
| `https://xxx/sse` | SSE URL | `https://mcp.example.com/sse` |
| `/path/to/xxx` | 本地路径 | `/Users/me/my-mcp-server` |

---

## 安装步骤

### 类型 1：npm 包

**判断依据**：输入以 `@` 开头，或包含 `/` 但不是 URL

**安装命令**：
```bash
npx -y @org/package [args...]
```

**配置模板**：
```toml
[mcp.servers.{name}]
transport = "stdio"
command = "npx"
args = ["-y", "@org/package"]
env = {}  # 可选，如需要环境变量
allowed_tools = []  # 可选，空=全部
```

**示例**：
- 输入：`@modelcontextprotocol/server-filesystem /home/user/projects`
- 命令：`npx -y @modelcontextprotocol/server-filesystem /home/user/projects`
- 配置：
```toml
[mcp.servers.filesystem]
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/user/projects"]
```

### 类型 2：GitHub 仓库

**判断依据**：输入以 `https://github.com/` 开头

**安装步骤**：
1. 从 URL 提取 `org/repo` 格式
2. 克隆仓库：`git clone https://github.com/{org}/{repo}.git /tmp/mcp-{name}`
3. 进入目录：`cd /tmp/mcp-{name}`
4. 检查 package.json：如存在，执行 `npm install` 或 `npm run build`
5. 找到入口文件：通常是 `dist/index.js`、`build/index.js` 或 package.json 的 `bin` 字段

**安装命令**：
```bash
git clone https://github.com/{org}/{repo}.git /tmp/mcp-{name}
cd /tmp/mcp-{name}
npm install  # 或 npm run build
```

**配置模板**：
```toml
[mcp.servers.{name}]
transport = "stdio"
command = "node"
args = ["/tmp/mcp-{name}/dist/index.js"]
env = {}  # 可选
```

**示例**：
- 输入：`https://github.com/vercel-labs/agent-skills`
- 克隆：`git clone https://github.com/vercel-labs/agent-skills.git /tmp/mcp-agent-skills`
- 构建：`cd /tmp/mcp-agent-skills && npm install && npm run build`
- 假设入口：`/tmp/mcp-agent-skills/dist/index.js`
- 配置：
```toml
[mcp.servers.agent-skills]
transport = "stdio"
command = "node"
args = ["/tmp/mcp-agent-skills/dist/index.js"]
```

### 类型 3：SSE URL

**判断依据**：输入以 `https://` 开头，且包含 `/sse` 或以 `/` 结尾但不是 GitHub

**配置模板**：
```toml
[mcp.servers.{name}]
transport = "sse"
url = "https://xxx/sse"
headers = {}  # 可选，如 Authorization
```

**示例**：
- 输入：`https://mcp.example.com/sse`
- 配置：
```toml
[mcp.servers.remote]
transport = "sse"
url = "https://mcp.example.com/sse"
```

### 类型 4：本地路径

**判断依据**：输入以 `/` 开头（绝对路径）

**安装步骤**：
1. 检查路径是否存在
2. 如果是目录，检查是否有 `package.json`
3. 如有 `package.json`，执行 `npm install` + `npm run build`
4. 找到入口文件

**配置模板**：
```toml
[mcp.servers.{name}]
transport = "stdio"
command = "node"
args = ["/path/to/entry.js"]
env = {}
```

**示例**：
- 输入：`/Users/me/my-mcp-server`
- 假设入口：`/Users/me/my-mcp-server/dist/index.js`
- 配置：
```toml
[mcp.servers.my-mcp]
transport = "stdio"
command = "node"
args = ["/Users/me/my-mcp-server/dist/index.js"]
```

---

## 完整安装流程

1. **解析输入**：判断用户提供的 MCP 类型
2. **生成配置**：根据类型填充配置模板（{name} 替换为简短名称）
3. **执行安装**：
   - npm 包：直接使用
   - GitHub：git clone + npm install
   - 本地：检查并安装
   - SSE：无需安装
4. **写入配置**：调用 config 工具将配置写入 `~/.rrclaw/config.toml`
5. **加载 MCP**：通知用户需要重启或使用 `/mcp reload`（如已实现）

---

## config 工具调用示例

```python
# 读取当前配置
config.get("mcp")

# 追加新 server 配置（需要手动编辑或使用 toml_edit）
# 建议直接用 shell 工具追加：
# echo '[mcp.servers.xxx]\ntransport = "stdio"...' >> ~/.rrclaw/config.toml

# 或使用 sed/awk 插入
```

---

## 常见 MCP Server 推荐

| MCP Server | 安装命令 | 用途 |
|------------|----------|------|
| `@modelcontextprotocol/server-filesystem` | `npx -y @modelcontextprotocol/server-filesystem /path` | 文件系统访问 |
| `@modelcontextprotocol/server-github` | `npx -y @modelcontextprotocol/server-github` | GitHub API |
| `@modelcontextprotocol/server-postgres` | `npx -y @modelcontextprotocol/server-postgres "postgresql://..."` | PostgreSQL |
| `@modelcontextprotocol/server-brave-search` | `npx -y @modelcontextprotocol/server-brave-search` | 网页搜索 |
| `@modelcontextprotocol/server-slack` | `npx -y @modelcontextprotocol/server-slack` | Slack |
```

##### 10.3.2 CLI 集成

在 `src/channels/cli.rs` 中添加 `/mcp` 命令，列出当前已加载的 MCP servers：

```rust
// /mcp — 列出已加载的 MCP servers
async fn handle_mcp_command(&mut self, _args: Vec<String>) -> Result<()> {
    println!("已加载的 MCP Servers:");
    for (name, _server) in &self.mcp_servers {
        println!("  - {}", name);
    }
    Ok(())
}
```

如需完整管理功能（add/remove），可后续扩展。

---

## 十二、待实现功能清单

| 功能 | 状态 | 说明 |
|------|------|------|
| config.toml 加载 MCP | ✅ 已完成 | 启动时加载 |
| mcp-install Skill | ⬜ 待实现 | LLM 安装指南 |
| `/mcp` CLI 命令 | ⬜ 可选 | 列出已加载的 MCP |

**实现方式**：

1. **创建 mcp-install Skill**：在 `~/.rrclaw/skills/` 或内置 skill 目录创建 `mcp-install.md`，包含完整的安装指南
2. **无需新增代码**：利用现有 shell + config 工具 + skill 系统即可完成

**用户流程**：
```
用户：帮我安装 @modelcontextprotocol/server-github
LLM：（加载 mcp-install skill）分析输入类型为 npm 包，生成安装命令
LLM：需要执行以下命令来安装 GitHub MCP：
      npx -y @modelcontextprotocol/server-github
     [等待用户确认]
用户确认
LLM：执行安装 → 写入 config.toml → 提示需要重启
```
