# RRClaw

**Security-first AI Agent CLI — 100% Rust, pluggable trait architecture**

> Personal assistant & enterprise internal tooling. Multi-model, persistent memory, sandbox security, skills-driven behavior.

[中文文档](README.zh.md)

---

## Features

- **Multi-model support** — DeepSeek, Claude, GPT, GLM (Zhipu), MiniMax via a unified `Provider` trait
- **Streaming output** — SSE real-time streaming with thinking animation
- **Persistent memory** — SQLite storage + tantivy full-text search (jieba for Chinese, en_stem for English)
- **Security sandbox** — command whitelist, workspace path restriction, permission levels (ReadOnly / Supervised / Full)
- **Skills system** — three-tier lazy loading (L1 metadata → L2 behavior guide → L3 full content), built-in + user-defined skills
- **Slash commands** — `/help` `/new` `/clear` `/config` `/switch` `/apikey` `/skill` `/telegram`
- **MCP client** — connect to MCP servers, dynamic tool loading
- **Telegram channel** — multi-user isolated sessions via Telegram Bot
- **Daemon mode** — background process (`rrclaw start/stop/chat`); close the terminal without killing Telegram
- **Internationalization** — English (default) and Chinese UI, hot-switchable without restart

---

## Architecture

```
┌─────────────┐     ┌──────────────┐     ┌──────────────────┐
│  Channels   │     │  Security    │     │  AI Providers    │
│  ─────────  │     │  ──────────  │     │  ─────────────   │
│  CLI        │     │  Cmd whitelist│    │  DeepSeek        │
│  Telegram   │     │  Path sandbox │    │  Claude          │
│  + Channel  │     │  RO/Sup/Full  │    │  GPT / GLM       │
│    trait    │     │              │     │  MiniMax         │
└──────┬──────┘     └──────┬───────┘     └────────┬─────────┘
       │                   │                      │
       ▼                   ▼                      ▼
┌──────────────────────────────────────────────────────────┐
│                      Agent Loop                          │
│  Phase1: routing → Phase2: execute → tool call loop     │
│  (two-phase skill routing, max 10 tool iters/turn)       │
└───────────┬──────────────────────┬───────────────────────┘
            ▼                      ▼                     ▼
┌──────────────────┐  ┌──────────────────────┐  ┌──────────────────┐
│  Memory          │  │  Tools               │  │  Skills          │
│  ──────          │  │  ─────               │  │  ──────          │
│  SQLite          │  │  Shell / File        │  │  L1 catalog      │
│  tantivy search  │  │  Git / Config        │  │  L2 behavior     │
│  jieba / en_stem │  │  MCP / Skill         │  │  builtin + user  │
└──────────────────┘  └──────────────────────┘  └──────────────────┘
```

---

## Installation

### Option 1 — Homebrew (macOS / Linux, recommended)

```bash
brew tap yzzting/rrclaw
brew install rrclaw
```

### Option 2 — cargo install (requires Rust)

```bash
# Core CLI only
cargo install rrclaw

# With Telegram Bot support
cargo install rrclaw --features telegram
```

### Option 3 — Download prebuilt binary

Download from [GitHub Releases](https://github.com/yzzting/rrclaw/releases), extract, and move to your `PATH`:

```bash
# macOS Apple Silicon example
curl -L https://github.com/yzzting/rrclaw/releases/latest/download/rrclaw-macos-aarch64.tar.gz | tar xz
sudo mv rrclaw /usr/local/bin/
```

### Option 4 — Build from source

```bash
# Requires Rust 1.75+
git clone https://github.com/yzzting/rrclaw.git
cd rrclaw
cargo build --release
# Binary at: ./target/release/rrclaw
```

---

## Quick Start

### First Run

```bash
rrclaw setup
```

The setup wizard will guide you through provider selection and API key configuration. Config is stored at `~/.rrclaw/config.toml`.

### Interactive Mode

```bash
rrclaw agent
```

### One-shot Mode

```bash
rrclaw agent -m "Review the git diff and suggest improvements"
```

### Daemon Mode (Telegram + CLI in background)

```bash
# Start daemon — Telegram Bot runs in background, terminal is free
rrclaw start

# Connect from any terminal
rrclaw chat

# Check daemon status
rrclaw status

# Stop daemon
rrclaw stop
```

When the daemon is running, closing the terminal does **not** kill Telegram. Run `rrclaw chat` any time to reconnect.

---

## Configuration

```toml
# ~/.rrclaw/config.toml

[default]
provider = "deepseek"
model = "deepseek-chat"
temperature = 0.7
language = "en"          # "en" or "zh"

[providers.deepseek]
base_url = "https://api.deepseek.com/v1"
api_key = "sk-..."
model = "deepseek-chat"

[providers.claude]
base_url = "https://api.anthropic.com"
api_key = "sk-ant-..."
model = "claude-sonnet-4-5-20250929"

[providers.gpt]
base_url = "https://api.openai.com/v1"
api_key = "sk-..."
model = "gpt-4o"

[memory]
backend = "sqlite"
auto_save = true

# Optional: Telegram Bot (required for daemon Telegram channel)
[telegram]
token = "your-bot-token"

[security]
autonomy = "supervised"   # "readonly" | "supervised" | "full"
allowed_commands = ["ls", "cat", "grep", "git", "cargo"]
workspace_only = true
```

**Switch provider at runtime:**

```
/switch claude
/switch deepseek
```

**Switch language at runtime:**

```
switch to Chinese
→ Agent writes config and hot-reloads on next turn
```

---

## Skills System

Skills drive agent behavior without touching core code. Three-tier lazy loading keeps startup fast.

| Tier | Content | Loaded when |
|------|---------|-------------|
| L1 | Metadata (name, description) | Startup — injected into system prompt |
| L2 | Behavior guide (< 500 words) | Phase 1 routes to this skill |
| L3 | Full content + examples | User runs `/skill load <name>` |

**Built-in skills:**

| Skill | Description |
|-------|-------------|
| `git-workflow` | Git commit conventions, branch strategy |
| `code-review` | Code review best practices |
| `rust-dev` | Rust development standards (clippy, tests, error handling) |
| `mcp-install` | Guide to adding MCP servers |
| `find-skills` | Help finding the right skill |

**User-defined skills** — create `~/.rrclaw/skills/<name>.md`:

```markdown
---
name: my-skill
description: Trigger description (used by Phase 1 routing)
---
# Skill content here
```

**Skill commands:**

```
/skill list
/skill load <name>
/skill show <name>
/skill new <name>
/skill edit <name>
/skill delete <name>
```

---

## Slash Commands

| Command | Description |
|---------|-------------|
| `/help` | Show available commands |
| `/new` | Start a new conversation (clear history) |
| `/clear` | Clear conversation history |
| `/config` | View or edit configuration |
| `/switch <provider>` | Switch AI provider |
| `/apikey <provider> <key>` | Update API key |
| `/skill <subcommand>` | Manage skills |
| `/telegram` | Manage Telegram channel |

---

## Security Model

Three autonomy levels — choose based on trust level:

| Level | Behavior |
|-------|----------|
| `readonly` | No tool execution allowed |
| `supervised` | User confirms each tool call before execution |
| `full` | Autonomous execution (trusted enterprise environments) |

In `supervised` mode, select `a` (auto-approve) to skip confirmation for the same command class for the rest of the session.

Path access is restricted to `workspace_dir`. Symlink escape attempts are blocked via full path canonicalization.

---

## MCP Client

Connect to any MCP-compatible tool server:

```toml
[[mcp_servers]]
name = "filesystem"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
```

Tools from MCP servers are dynamically loaded and available to the agent alongside built-in tools.

---

## Logging

Dual-layer tracing — REPL output is never polluted by logs:

| Layer | Target | Default level | Purpose |
|-------|--------|---------------|---------|
| stderr | terminal | `warn` | Runtime warnings only |
| file | `~/.rrclaw/logs/rrclaw.log.YYYY-MM-DD` | `rrclaw=debug` | Full debug trace |

```bash
# Enable trace logging (includes full request/response bodies)
RUST_LOG=rrclaw=trace rrclaw agent

# Tail logs
tail -f ~/.rrclaw/logs/rrclaw.log.*
```

---

## Implementation Status

| Phase | Feature | Status |
|-------|---------|--------|
| P0 | CLI + Agent Loop + Multi-model + Tools + Security | ✅ |
| P1 | Streaming + Supervised confirm + History + Setup + Telegram | ✅ |
| P2 | Slash commands + ConfigTool | ✅ |
| P3 | Skills system (3-tier) + SkillTool + /skill CRUD | ✅ |
| P4 | Two-phase skill routing + GitTool + Memory Tools + ReliableProvider + History compaction + MCP client | ✅ |
| P5 | Routines (cron scheduling) | ✅ |
| P6 | Integration tests + E2E tests + Bug fixes | ✅ |
| P7 | Dynamic tool loading + Tool group routing | ✅ |
| P8 | Multi-channel unified entry + Telegram runtime management | ✅ |
| P9 | Internationalization (English/Chinese) | ✅ |
| P10 | Daemon mode (background process + Unix socket IPC) | ✅ |

**Test coverage:** 380+ tests (unit + integration + E2E), clippy zero warnings.

---

## License

MIT
