# RRClaw

**安全优先的 AI Agent CLI — 100% Rust，Trait 可插拔架构**

> 面向个人助手和企业内部使用的 AI 工具。多模型、持久化记忆、沙箱安全、Skills 行为驱动。

[English](README.md)

---

## 特性

- **多模型支持** — DeepSeek、Claude、GPT、智谱 GLM、MiniMax，统一 `Provider` trait 抽象
- **流式输出** — SSE 实时流式，含 thinking 动画
- **持久化记忆** — SQLite 结构化存储 + tantivy 全文搜索（中文 jieba 分词，英文 en_stem）
- **安全沙箱** — 命令白名单、workspace 路径限制、权限分级（只读 / 监督 / 全自动）
- **Skills 系统** — 三级渐进加载（L1 元数据 → L2 行为指南 → L3 完整内容），内置 + 用户自定义 skill
- **斜杠命令** — `/help` `/new` `/clear` `/config` `/switch` `/apikey` `/skill` `/telegram`
- **MCP 客户端** — 接入 MCP 协议工具服务器，动态加载工具
- **Telegram 频道** — Telegram Bot，多用户隔离会话
- **国际化** — 英文（默认）和中文 UI，运行时热切换无需重启

---

## 架构

```
┌─────────────┐     ┌──────────────┐     ┌──────────────────┐
│  Channels    │     │ Security     │     │  AI Providers    │
│  ─────────   │     │ ──────────   │     │  ─────────────   │
│  CLI         │     │ 命令白名单    │     │  DeepSeek        │
│  Telegram    │     │ 路径沙箱      │     │  Claude          │
│  + Channel   │     │ RO/Sup/Full  │     │  GPT / GLM       │
│    trait     │     │              │     │  MiniMax         │
└──────┬───────┘     └──────┬───────┘     └────────┬─────────┘
       │                    │                      │
       ▼                    ▼                      ▼
┌──────────────────────────────────────────────────────────┐
│                      Agent Loop                          │
│  Phase1:路由 → Phase2:执行 → Tool call loop → 输出        │
│  （两阶段 Skill 路由，最多 10 次 tool 迭代/轮）             │
└───────────┬──────────────────────┬───────────────────────┘
            ▼                      ▼                      ▼
┌──────────────────┐  ┌──────────────────────┐  ┌──────────────────┐
│  Memory          │  │  Tools               │  │  Skills          │
│  ──────          │  │  ─────               │  │  ──────          │
│  SQLite 存储      │  │  Shell / 文件         │  │  L1 元数据目录    │
│  tantivy 全文搜索 │  │  Git / Config        │  │  L2 行为指南      │
│  jieba / en_stem │  │  MCP / Skill         │  │  内置 + 用户定义  │
└──────────────────┘  └──────────────────────┘  └──────────────────┘
```

---

## 安装

### 方式一 — Homebrew（macOS / Linux，推荐）

```bash
brew tap yzzting/rrclaw
brew install rrclaw
```

### 方式二 — cargo install（需要 Rust 环境）

```bash
# 核心 CLI（不含 Telegram）
cargo install rrclaw

# 含 Telegram Bot 支持
cargo install rrclaw --features telegram
```

### 方式三 — 下载预编译二进制

从 [GitHub Releases](https://github.com/yzzting/rrclaw/releases) 下载对应平台的压缩包，解压后放入 `PATH`：

```bash
# macOS Apple Silicon 示例
curl -L https://github.com/yzzting/rrclaw/releases/latest/download/rrclaw-macos-aarch64.tar.gz | tar xz
sudo mv rrclaw /usr/local/bin/
```

### 方式四 — 从源码构建

```bash
# 需要 Rust 1.75+
git clone https://github.com/yzzting/rrclaw.git
cd rrclaw
cargo build --release
# 二进制在: ./target/release/rrclaw
```

---

## 快速开始

### 初次运行

```bash
rrclaw setup
```

交互式向导引导完成 provider 选择和 API Key 配置。配置文件保存在 `~/.rrclaw/config.toml`。

### 交互模式

```bash
rrclaw agent
```

### 单次执行

```bash
rrclaw agent -m "帮我看一下 git diff，给出改进建议"
```

---

## 配置

```toml
# ~/.rrclaw/config.toml

[default]
provider = "deepseek"
model = "deepseek-chat"
temperature = 0.7
language = "zh"          # "en" 英文 或 "zh" 中文

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

[security]
autonomy = "supervised"   # "readonly" | "supervised" | "full"
allowed_commands = ["ls", "cat", "grep", "git", "cargo"]
workspace_only = true
```

**运行时切换 Provider：**

```
/switch claude
/switch deepseek
```

**运行时切换语言：**

```
切换到英文
→ Agent 写入 config，下一条消息生效（热加载）
```

---

## Skills 系统

Skills 驱动 Agent 行为，与核心代码解耦。三级渐进加载保持启动速度。

| 级别 | 内容 | 加载时机 |
|------|------|---------|
| L1 | 元数据（名称、描述） | 启动时全部加载，注入 system prompt |
| L2 | 行为指南（< 500 字） | Phase 1 路由命中时按需加载 |
| L3 | 完整内容 + 示例 | 用户执行 `/skill load <name>` 时加载 |

**内置 Skills：**

| Skill | 说明 |
|-------|------|
| `git-workflow` | Git 提交规范、分支策略 |
| `code-review` | 代码审查最佳实践 |
| `rust-dev` | Rust 开发规范（clippy、测试、错误处理） |
| `mcp-install` | MCP 服务器接入指南 |
| `find-skills` | 帮助找到合适的 skill |

**用户自定义 skill** — 创建 `~/.rrclaw/skills/<name>.md`：

```markdown
---
name: my-skill
description: 触发场景描述（Phase 1 路由依赖此字段）
---
# Skill 内容
...
```

**Skill 命令：**

```
/skill list           列出所有 skill
/skill load <name>    加载 skill L3 完整内容
/skill show <name>    查看 skill 内容
/skill new <name>     创建新 skill
/skill edit <name>    编辑 skill
/skill delete <name>  删除用户 skill
```

---

## 斜杠命令

| 命令 | 说明 |
|------|------|
| `/help` | 显示可用命令 |
| `/new` | 开始新对话（清空历史） |
| `/clear` | 清空对话历史 |
| `/config` | 查看或修改配置 |
| `/switch <provider>` | 切换 AI Provider |
| `/apikey <provider> <key>` | 更新 API Key |
| `/skill <子命令>` | 管理 Skills |
| `/telegram` | 管理 Telegram 频道 |

---

## 安全模型

三种自主级别，按信任程度选择：

| 级别 | 行为 |
|------|------|
| `readonly` | 禁止执行任何工具 |
| `supervised` | 每次工具调用需用户确认 |
| `full` | 自主执行（企业内部可信环境） |

`supervised` 模式下，在确认提示选 `a`（auto-approve）可对本次会话同类命令自动放行。

路径访问限制在 `workspace_dir` 内。通过完整路径规范化阻止 symlink 逃逸攻击。

---

## MCP 客户端

接入任意 MCP 协议工具服务器：

```toml
[[mcp_servers]]
name = "filesystem"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
```

MCP 工具与内置工具统一调度，Agent 可直接使用。

---

## 日志系统

双层 tracing 架构，REPL 交互不受干扰：

| 层 | 输出 | 默认级别 | 用途 |
|----|------|----------|------|
| stderr | 终端 | `warn` | 运行时警告，不干扰 REPL |
| 文件 | `~/.rrclaw/logs/rrclaw.log.YYYY-MM-DD` | `rrclaw=debug` | 完整调试日志 |

```bash
# 开启 trace 级别（含完整请求/响应体）
RUST_LOG=rrclaw=trace rrclaw agent

# 查看日志
tail -f ~/.rrclaw/logs/rrclaw.log.*
```

---

## 实现进度

| 阶段 | 功能 | 状态 |
|------|------|------|
| P0 | CLI + Agent Loop + 多模型 + 基础工具 + 安全 | ✅ |
| P1 | 流式输出 + Supervised 确认 + History 持久化 + Setup 向导 + Telegram | ✅ |
| P2 | 斜杠命令 + ConfigTool | ✅ |
| P3 | Skills 系统（三级加载）+ SkillTool + /skill CRUD | ✅ |
| P4 | 两阶段 Skill 路由 + GitTool + Memory Tools + ReliableProvider + History 压缩 + MCP 客户端 | ✅ |
| P5 | 定时任务（cron 调度） | ✅ |
| P6 | 集成测试 + E2E 测试 + Bug 修复 | ✅ |
| P7 | 动态工具加载 + 工具组路由 | ✅ |
| P8 | 多 Channel 统一入口 + Telegram 运行时管理 | ✅ |
| P9 | 国际化（中英文） | ✅ |

**测试覆盖：** 302+ 测试（单元 + 集成 + E2E），clippy 零警告。

---

## License

MIT
