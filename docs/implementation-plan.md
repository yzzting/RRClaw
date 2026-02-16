# RRClaw MVP 实现计划

## 实现步骤（按顺序，每步遵循：文档 → 测试 → 代码 → 提交）

### Step 1: 项目脚手架 + 总架构文档
**提交 1**: `docs: add root Claude.md with architecture overview`
- 写入根目录 `Claude.md`（总架构文档）

**提交 2**: `chore: init cargo project with dependencies`
- `cargo init` 创建项目
- 配置 `Cargo.toml` 所有依赖
- 配置 release profile（opt-level, LTO, strip）

**提交 3**: `chore: create module directory structure`
- 创建所有目录 + 空 `mod.rs` 文件
- `lib.rs` 声明所有模块
- 确保 `cargo check` 通过

### Step 2: Security Policy（最先实现，因为其他模块依赖它）
**提交 4**: `docs: add security module Claude.md`
**提交 5**: `feat: add SecurityPolicy with autonomy levels`
- `security/policy.rs` - AutonomyLevel enum + SecurityPolicy struct
- `is_command_allowed()`, `is_path_allowed()`, `requires_confirmation()`
- 单元测试：白名单检查、路径限制、symlink 防逃逸

### Step 3: Config 系统
**提交 6**: `docs: add config module Claude.md`
**提交 7**: `feat: add Config schema and figment loading`
- `config/schema.rs` - Config 结构体定义
- `config/mod.rs` - `Config::load_or_init()` via figment（TOML + 环境变量自动合并）
- 单元测试：默认值、TOML 解析、环境变量覆盖

### Step 4: Provider Trait + 实现
**提交 8**: `docs: add providers module Claude.md`
**提交 9**: `feat: add Provider trait and message types`
- `providers/traits.rs` - Provider trait + ChatMessage/ChatResponse/ToolCall/ConversationMessage

**提交 10**: `feat: add OpenAI-compatible provider`
- `providers/compatible.rs` - 支持 GLM/MiniMax/DeepSeek/GPT
- 单元测试：URL 拼接、请求构造

**提交 11**: `feat: add Claude (Anthropic) provider`
- `providers/claude.rs` - Messages API 实现
- `providers/mod.rs` - 工厂函数

### Step 5: Tool Trait + 实现
**提交 12**: `docs: add tools module Claude.md`
**提交 13**: `feat: add Tool trait definitions`
- `tools/traits.rs` - Tool trait + ToolResult/ToolSpec

**提交 14**: `feat: add shell tool with security checks`
- `tools/shell.rs` - 命令执行 + 白名单 + 路径限制 + 用户确认
- 单元测试：白名单拦截、路径拦截

**提交 15**: `feat: add file read/write tools`
- `tools/file.rs` - 文件读写 + workspace 路径检查
- 单元测试：路径校验、dotfile 拦截

### Step 6: Memory 系统
**提交 16**: `docs: add memory module Claude.md`
**提交 17**: `feat: add Memory trait and SQLite+tantivy implementation`
- `memory/traits.rs` - Memory trait + MemoryEntry + MemoryCategory
- `memory/sqlite.rs` - SQLite 结构化存储 + tantivy 全文搜索索引（jieba 中文分词）
- 单元测试：store/recall/forget 完整流程，中文搜索验证

### Step 7: Agent Loop
**提交 18**: `docs: add agent module Claude.md`
**提交 19**: `feat: add agent loop with tool call cycle`
- `agent/loop_.rs` - 核心循环（Provider → Tool → Memory 串联）
- `agent/mod.rs` - agent::run() 入口
- 集成测试：mock provider 跑通完整循环

### Step 8: CLI Channel + main 入口
**提交 20**: `docs: add channels module Claude.md`
**提交 21**: `feat: add CLI channel with reedline REPL`
- `channels/cli.rs` - reedline REPL（历史、补全、vi/emacs 模式）
- `channels/mod.rs` - Channel trait

**提交 22**: `feat: add CLI entry point and subcommands`
- `main.rs` - clap 命令定义 + 子命令路由
  - `rrclaw agent` / `rrclaw agent -m "msg"`
  - `rrclaw init` / `rrclaw config`

### Step 9: 端到端验证
**提交 23**: `test: add integration tests for full agent flow`
- 端到端测试
- `cargo clippy` 清理
- Release profile 验证

## 验证方式

1. `cargo build --release` 编译通过
2. `cargo test` 所有测试通过
3. `cargo clippy -- -W clippy::all` 无警告
4. `rrclaw init` → 生成配置文件
5. `rrclaw agent -m "你好"` → 收到 AI 回复
6. `rrclaw agent` → 进入交互模式，测试多轮对话
7. 测试 tool 调用：让 AI 执行 `ls` 命令，验证安全确认流程
8. 测试路径限制：尝试访问 workspace 外的文件，应被拒绝
