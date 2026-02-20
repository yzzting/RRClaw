---
name: mcp-install
description: MCP Server 安装助手。当用户要求安装、添加、配置 MCP 时使用此技能。根据输入类型自动判断安装方式。
tags: [mcp, install, setup]
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
env = {}
allowed_tools = []
```

**示例**：
- 输入：`@modelcontextprotocol/server-filesystem /home/user/projects`
- 命令：`npx -y @modelcontextprotocol/server-filesystem /home/user/projects`

---

### 类型 2：GitHub 仓库

**判断依据**：输入以 `https://github.com/` 开头

**安装步骤**：
1. 从 URL 提取 `org/repo` 格式
2. 克隆仓库：`git clone https://github.com/{org}/{repo}.git /tmp/mcp-{name}`
3. 进入目录：`cd /tmp/mcp-{name}`
4. 执行 `npm install` 或 `npm run build`
5. 找到入口文件：通常是 `dist/index.js`、`build/index.js` 或 package.json 的 `bin` 字段

**配置模板**：
```toml
[mcp.servers.{name}]
transport = "stdio"
command = "node"
args = ["/tmp/mcp-{name}/dist/index.js"]
env = {}
```

---

### 类型 3：SSE URL

**判断依据**：输入以 `https://` 开头，且包含 `/sse` 或以 `/` 结尾但不是 GitHub

**配置模板**：
```toml
[mcp.servers.{name}]
transport = "sse"
url = "https://xxx/sse"
headers = {}
```

---

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

---

## 完整安装流程

1. **解析输入**：判断用户提供的 MCP 类型
2. **生成配置**：根据类型填充配置模板（{name} 替换为简短名称）
3. **执行安装**：
   - npm 包：直接使用
   - GitHub：git clone + npm install
   - 本地：检查并安装
   - SSE：无需安装
4. **写入配置**：用 `config` 工具的 `append` 操作追加到 `~/.rrclaw/config.toml`
5. **加载 MCP**：通知用户需要重启才能生效

---

## config.toml 追加示例

使用 `config` 工具（**不要用 shell `cat >>`**，因为 shell 工具受 workspace_only 限制无法写入 ~/.rrclaw）：

```json
{
  "action": "append",
  "value": "[mcp.servers.{name}]\ntransport = \"stdio\"\ncommand = \"npx\"\nargs = [\"-y\", \"@org/package\"]"
}
```

等价的 TOML 内容：
```toml
[mcp.servers.{name}]
transport = "stdio"
command = "npx"
args = ["-y", "@org/package"]
```

---

## 常见 MCP Server

| MCP Server | 安装命令 | 用途 |
|------------|----------|------|
| `@modelcontextprotocol/server-filesystem` | `npx -y @modelcontextprotocol/server-filesystem /path` | 文件系统 |
| `@modelcontextprotocol/server-github` | `npx -y @modelcontextprotocol/server-github` | GitHub API |
| `@modelcontextprotocol/server-postgres` | `npx -y @modelcontextprotocol/server-postgres "postgresql://..."` | PostgreSQL |
| `@modelcontextprotocol/server-brave-search` | `npx -y @modelcontextprotocol/server-brave-search` | 网页搜索 |
| `@modelcontextprotocol/server-slack` | `npx -y @modelcontextprotocol/server-slack` | Slack |

---

## 注意事项

1. **用户确认**：生成安装命令后，必须等待用户确认才能执行
2. **安全**：只允许安装官方或可信的 MCP Server
3. **环境变量**：某些 MCP 需要 API Key 等环境变量，可通过 `env` 配置
4. **重启生效**：配置写入后需要重启 RRClaw 才能加载新 MCP
