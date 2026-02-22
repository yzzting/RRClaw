# Channels 模块设计文档

消息通道抽象，已实现 CLI REPL 和 Telegram Bot 两个通道。

## Channel trait

```rust
#[async_trait]
pub trait Channel: Send + Sync {
    fn name(&self) -> &str;
    async fn send(&self, message: &str, recipient: &str) -> Result<()>;
    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> Result<()>;
}
```

## CliChannel（主通道）

基于 reedline 的交互式 REPL，支持流式输出和所有斜杠命令。

### 核心特性

- **流式输出**：`process_message_stream` + SSE → 实时打印 token
- **Thinking 动画**：LLM 生成期间显示旋转动画（spinner）
- **ToolStatus 显示**：工具执行时实时显示 `▶ 执行 shell: cargo test...`
- **ExternalPrinter**：后台 Routine 任务结果通过 reedline ExternalPrinter 打印，避免 raw mode 下文字乱排（P5 修复）

### ExternalPrinter 架构

reedline 在 raw mode 下，直接 `eprintln!` 会因 `\n` 不含 `\r` 导致文字从当前光标列开始（阶梯乱排）。

```
RoutineEngine
    └── cli_notifier: OnceLock<mpsc::Sender<String>>
                                ↓ tokio mpsc channel
                    桥接 task (tokio::spawn)
                                ↓ crossbeam send
                    ExternalPrinter<String>
                                ↓ reedline 内部管理
                    正确插入到提示符上方
```

`run_repl` 启动时：创建 ExternalPrinter → 创建 tokio mpsc channel → 设置 `engine.set_cli_notifier(tx)` → 启动桥接 task → reedline editor 绑定 printer。

### 斜杠命令清单

| 命令 | 说明 | 实现版本 |
|------|------|---------|
| `/help` | 显示帮助 | P2 |
| `/new` | 新建会话（清空 history） | P2 |
| `/clear` | 清空终端屏幕 | P2 |
| `/config` | 查看/修改配置 | P2 |
| `/switch <provider>` | 切换 AI Provider（持久化到 config.toml） | P2 |
| `/apikey <provider> <key>` | 设置 API Key | P2 |
| `/skill list/load/show/new/edit/delete` | Skill CRUD | P3 |
| `/identity show/edit/reload` | 身份文件管理 | P4 |
| `/routine list/add/delete/enable/disable/run/logs` | 定时任务管理 | P5 |
| `/mcp list` | 查看已连接的 MCP server 和工具 | P4 |

**斜杠命令在 CLI 层直接处理，不进入 Agent Loop。**

### `/switch` 命令

替代旧版 `/provider` 和 `/model` 命令，支持：
- `/switch deepseek` — 切换 Provider（使用默认模型）
- 持久化写入 `config.toml`，立即生效（清空当前会话 history）
- 用 dialoguer Select 菜单选择

### `/routine` 命令

通过 `RoutineEngine` 管理定时任务（sliced 到 `cmd_routine` 函数）。
`/routine add` 的时间参数支持自然语言（LLM 解析）。

## TelegramChannel（P1）

基于 teloxide 的 Telegram Bot，支持多用户隔离会话。

- 每个 chat_id 独立 Agent 实例（各自 history 隔离）
- Routine 结果通过 `send_telegram()` 发送到配置的 chat_id

## 文件结构

```
src/channels/
├── Claude.md      # 本文件
├── mod.rs         # Channel trait + re-exports
├── cli.rs         # CLI REPL（reedline，流式，所有斜杠命令）
└── telegram.rs    # Telegram Bot（teloxide）
```
