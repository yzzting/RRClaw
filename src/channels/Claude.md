# Channels 模块

## 职责

消息通道抽象，MVP 实现 CLI REPL 交互。

## Channel trait（预留扩展）

```rust
#[async_trait]
pub trait Channel: Send + Sync {
    fn name(&self) -> &str;
    async fn send(&self, message: &str, recipient: &str) -> Result<()>;
    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> Result<()>;
}
```

## CliChannel

MVP 实现：基于 reedline 的交互式 REPL。

功能:
- 交互式输入（历史记录、行编辑）
- 输入 "exit" / "quit" / Ctrl-D 退出
- 输入 "clear" 清空历史
- 每轮: 读取输入 → 调用 Agent::process_message() → 打印回复

## main.rs

clap 子命令:
- `rrclaw agent` — 进入交互式 REPL
- `rrclaw agent -m "消息"` — 单次消息模式
- `rrclaw init` — 初始化配置文件

## 文件结构

- `mod.rs` — Channel trait + re-exports
- `cli.rs` — CliChannel REPL 实现
