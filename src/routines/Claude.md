# Routines 模块设计文档

定时任务系统，让 Agent 从被动响应转变为主动助手，定期自动执行任务。

## 核心数据结构

```rust
pub struct Routine {
    pub name: String,       // 唯一标识，用于 /routine 命令
    pub schedule: String,   // 标准 5 字段 cron（模块内部自动转 6 字段）
    pub message: String,    // 触发时发给 Agent 的消息
    pub channel: String,    // 结果路由："cli" | "telegram"
    pub enabled: bool,
    pub source: RoutineSource, // Config（来自 config.toml）| Dynamic（/routine add）
}
```

## 关键实现细节（已踩坑，必读）

### tokio-cron-scheduler 的行为

**坑1：scheduler 需要显式 `.start()`**

创建 `JobScheduler` 后不会自动运行，必须调用 `scheduler.start().await`。
当前实现：`start_scheduler()` 方法在启动时调用，`persist_add_routine()` 也会在需要时自动触发。

**坑2：cron 格式是 6 字段，不是标准 5 字段**

标准 cron：`分 时 日 月 周`（5 字段）
tokio-cron-scheduler：`秒 分 时 日 月 周`（6 字段，首位是秒）

config.toml 和用户面向的 API 统一使用标准 5 字段，内部由 `convert_5field_to_6field()` 自动转换。
**不要在任何对外 API 里暴露 6 字段格式。**

### 自然语言时间解析

**坑：正则解析中文自然语言不可行**

最初用正则匹配"每天早上8点"等表达式，覆盖不全、维护成本高、用户体验差。

**现在的方案：LLM 解析（`parse_schedule_to_cron()`）**

用 LLM 将用户输入转为标准 5 字段 cron，失败时返回明确错误提示。
- 不要再引入正则解析自然语言
- 如果 LLM 无法解析，直接让用户提供标准 cron 格式

### 状态一致性：持久化 + 内存双写

Routine 列表同时维护内存（`RwLock<Vec<Routine>>`）和 SQLite。

`persist_add_routine()` / `persist_delete_routine()` / `persist_set_enabled()` 必须：
1. 先更新调度器（add_job / remove）
2. 再写 SQLite
3. 最后更新内存 `Vec<Routine>`

**顺序不能错**：若先写 DB、调度器失败，则 DB 和调度器状态不一致。

### CLI 通知输出

Routine 结果通过 reedline `ExternalPrinter` 发送到 CLI，不能直接 `eprintln!`。

**坑：reedline 在 raw mode 下，`\n` 只做 LF 不含 CR**，`eprintln!` 会导致每行从当前光标列开始打印，产生阶梯乱排。

当前实现：
- `RoutineEngine` 持有 `cli_notifier: OnceLock<mpsc::Sender<String>>`
- `run_repl` 创建 `ExternalPrinter<String>`，通过桥接 task 转发

## 测试要求

### 单元测试（当前覆盖）
- `convert_5field_to_6field()`：各种输入格式验证
- `parse_schedule_to_cron()`：自然语言 → cron 转换（mock LLM）
- DB 操作：persist_add/delete/enable 的增删查
- 状态一致性：persist 操作后内存和 DB 一致

### 集成测试（必须）
涉及调度器真实行为的测试**不可 mock scheduler**：

```rust
// 示例：验证调度器真的会触发
#[tokio::test]
async fn integration_scheduler_actually_fires() {
    // 创建真实 RoutineEngine（不 mock）
    // 添加一个每秒触发的 routine
    // 等待 > 1 秒
    // 验证 execute_routine 被调用过
}
```

**教训**：P5 实现时只有 mock 单元测试，导致"scheduler 从不启动"和"cron 格式错误"两个 bug 全部漏网，只在手动测试时才发现。

## 文件结构

```
src/routines/
├── Claude.md       # 本文件
└── mod.rs          # 全部实现（RoutineEngine + Routine + 调度逻辑）
```

SQLite 表：
- `routines`：动态创建的 Routine（/routine add）
- `routines_log`：执行历史记录

## 配置格式

```toml
# config.toml 静态配置（启动时加载）
[[routines.jobs]]
name = "morning_brief"
schedule = "0 8 * * *"   # 标准 5 字段
message = "生成今日工作计划"
channel = "cli"
enabled = true
```

动态创建（/routine add）保存在 SQLite，重启后从 DB 恢复。
