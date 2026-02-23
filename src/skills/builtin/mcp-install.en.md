---
name: mcp-install
description: MCP Server installation assistant. Use when the user asks to install, add, or configure an MCP. Automatically determines the installation method based on the input type.
tags: [mcp, install, setup]
---

# MCP Installation Guide

Given a user-provided MCP address or package name, determine the type and execute the corresponding installation steps.

## Detection Logic

Determine the MCP type from the input format:

| Input format | Type | Example |
|---|---|---|
| `@org/package` | npm package | `@modelcontextprotocol/server-filesystem` |
| `org/package` | npm package (shorthand) | `server-github` |
| `https://github.com/xxx` | GitHub repository | `https://github.com/vercel-labs/agent-skills` |
| `https://xxx/sse` | SSE URL | `https://mcp.example.com/sse` |
| `/path/to/xxx` | Local path | `/Users/me/my-mcp-server` |

---

## Installation Steps

### Type 1: npm Package

**Detection rule**: Input starts with `@`, or contains `/` but is not a URL

**Install command**:
```bash
pnpm dlx @org/package [args...]
```

**Config template**:
```toml
[mcp.servers.{name}]
transport = "stdio"
command = "pnpm"
args = ["dlx", "@org/package"]
env = {}
allowed_tools = []
```

**Example**:
- Input: `@modelcontextprotocol/server-filesystem /home/user/projects`
- Command: `pnpm dlx @modelcontextprotocol/server-filesystem /home/user/projects`

---

### Type 2: GitHub Repository

**Detection rule**: Input starts with `https://github.com/`

**Installation steps**:
1. Extract `org/repo` from the URL
2. Clone the repository: `git clone https://github.com/{org}/{repo}.git /tmp/mcp-{name}`
3. Enter the directory and run `pnpm install` + `pnpm run build`
4. Locate the entry point: typically `dist/index.js`, `build/index.js`, or the `bin` field in `package.json`

**Config template**:
```toml
[mcp.servers.{name}]
transport = "stdio"
command = "node"
args = ["/tmp/mcp-{name}/dist/index.js"]
env = {}
```

---

### Type 3: SSE URL

**Detection rule**: Input starts with `https://` and contains `/sse`, or ends with `/` but is not a GitHub URL

**Config template**:
```toml
[mcp.servers.{name}]
transport = "sse"
url = "https://xxx/sse"
headers = {}
```

---

### Type 4: Local Path

**Detection rule**: Input starts with `/` (absolute path)

**Installation steps**:
1. Check whether the path exists
2. If it is a directory, check for a `package.json`
3. If `package.json` is present, run `pnpm install` + `pnpm run build`
4. Locate the entry point

**Config template**:
```toml
[mcp.servers.{name}]
transport = "stdio"
command = "node"
args = ["/path/to/entry.js"]
env = {}
```

---

## Full Installation Flow

1. **Parse input**: determine the MCP type from what the user provided
2. **Generate config**: fill in the config template for the detected type (replace `{name}` with a short identifier)
3. **Execute installation**:
   - npm package: use directly (`pnpm dlx` requires no pre-installation)
   - GitHub: `git clone` + `pnpm install`
   - Local: check and install
   - SSE: no installation needed
4. **Write config**: use the `config` tool's `append` action to append to `~/.rrclaw/config.toml`
5. **Load MCP**: notify the user that a restart is required for the new MCP to take effect

---

## config.toml Append Example

Use the `config` tool (**do not use `shell cat >>`**, because the shell tool is subject to `workspace_only` restrictions and cannot write to `~/.rrclaw`):

```json
{
  "action": "append",
  "value": "[mcp.servers.{name}]\ntransport = \"stdio\"\ncommand = \"pnpm\"\nargs = [\"dlx\", \"@org/package\"]"
}
```

Equivalent TOML content:
```toml
[mcp.servers.{name}]
transport = "stdio"
command = "pnpm"
args = ["dlx", "@org/package"]
```

---

## Common MCP Servers

| MCP Server | Install command | Purpose |
|---|---|---|
| `@modelcontextprotocol/server-filesystem` | `pnpm dlx @modelcontextprotocol/server-filesystem /path` | Filesystem access |
| `@modelcontextprotocol/server-github` | `pnpm dlx @modelcontextprotocol/server-github` | GitHub API |
| `@modelcontextprotocol/server-postgres` | `pnpm dlx @modelcontextprotocol/server-postgres "postgresql://..."` | PostgreSQL |
| `@modelcontextprotocol/server-brave-search` | `pnpm dlx @modelcontextprotocol/server-brave-search` | Web search |
| `@modelcontextprotocol/server-slack` | `pnpm dlx @modelcontextprotocol/server-slack` | Slack |

---

## Notes

1. **User confirmation**: after generating the install command, always wait for user confirmation before executing
2. **Security**: only install official or trusted MCP Servers
3. **Environment variables**: some MCPs require API keys or other env vars; configure them via the `env` field
4. **Restart required**: after writing the config, RRClaw must be restarted for the new MCP to be loaded
