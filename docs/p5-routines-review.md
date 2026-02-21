# P5-5 Routines 代码审查与修复计划

## 一、Code Review 问题清单

### 🔴 严重问题（影响正确性）

#### 1. `detect_routine_intent()` 整个系统不应存在（cli.rs:38-312）

这是本次 review 的核心问题。同事在 REPL 循环里加了一层"自然语言意图识别"，
**在用户输入到达 LLM 之前将其拦截**，用硬编码关键词判断是否为 Routine 操作。

```
用户输入
  ├─ 硬编码时间词 × 动作词 → [拦截] → 硬编码 NLP 处理 → Routine 操作（不完整）
  └─ 其余 → LLM
```

**问题**：
- 这绕过了我们已有的 LLM，用规则模拟语义理解，比 LLM 差得多
- 和 RRClaw 的整体设计哲学矛盾：Tools 是 LLM 的扩展，不是 CLI 的扩展
- 关键词列表永远不完整（`每半小时`、`隔天`等无法识别）
- 用户说"帮我删除日报"时，正则匹配结果不可预测

**涉及的函数（全部应删除）**：
- `detect_routine_intent()` (cli.rs:38-97)
- `extract_time_description()` (cli.rs:99-131)
- `extract_task_message()` (cli.rs:133-157)
- `extract_routine_name_from_action()` (cli.rs:159-181)
- `generate_routine_name()` (cli.rs:183-209)
- `normalize_routine_name()` (cli.rs:211-241)
- `handle_routine_intent()` (cli.rs:243-312)
- `RoutineIntent` enum (cli.rs:17-35)

#### 2. `/routine add` / `delete` / `enable` / `disable` 是完全无效的桩代码

`cmd_routine_add`（cli.rs:1542-1596）：解析了参数、解析了 cron，但 `Some(_) =>` 分支只打印一行字，
**routine 对象被完全丢弃**，没有写入数据库，没有调用 `engine.add_routine()`。

```rust
// 当前代码（问题所在）
Some(_) => {
    println!("Routine '{}' 已保存，下次启动 RRClaw 时生效。", routine.name);
    // ↑ 实际上什么都没做，routine 变量在此处被 drop
}
```

`cmd_routine_delete`、`cmd_routine_enable` 同样只打印提示，没有实际操作。

**根因**：`RoutineEngine` 被包装在 `Arc<RoutineEngine>` 里，而 `add_routine`/`delete_routine`/`set_enabled`
需要 `&mut self`，`Arc` 无法提供可变引用，所以这些方法永远无法被调用。

#### 3. `parse_schedule_to_cron()` 有越界 bug（routines/mod.rs）

```rust
// 每天下午 X 点
let hour = hour + 12;
if hour < 24 { return Ok(...) }  // 下午12点 = 12+12=24，被拒绝（正确）
                                  // 下午11点 = 11+12=23，正确
                                  // 但下午12点应该是 12:00 = 24:00-hour，实际应是12点
```

更严重的是：
- "每天晚上12点"→ hour=12, hour+12=24，条件 `< 24` 不成立，**返回解析错误**
- "每天早上0点"实际是午夜，被归入早上处理，正确
- "每天8点"（无修饰词）走 pattern 4，但也可能被 pattern 1 优先匹配到

---

### 🟡 设计问题（影响架构）

#### 4. `RoutineEngine` 持有 `Arc<SqliteMemory>` 而非 `Arc<dyn Memory>`

```rust
pub struct RoutineEngine {
    memory: Arc<SqliteMemory>,  // ← 具体类型
    ...
}
```

这破坏了 Memory trait 的抽象。`run_once()` 里调用 `Arc::clone(&self.memory) as Arc<dyn Memory>`，
强转说明本意是 trait object，但字段类型写死了具体实现。

#### 5. `NoopMemory` 定义在 `routines/mod.rs` 位置不对

`NoopMemory` 是 Memory trait 的实现，应在 `memory` 模块下，或至少在 `memory/mod.rs` 中。
当前放在 `routines/mod.rs` 里是临时位置，会导致跨模块引用混乱。

查看 main.rs 中的 `run_once()` 调用：
```rust
Box::new(NoopMemory), // 但 memory::NoopMemory 在哪里？
```
实际上 `crate::memory::NoopMemory` 已经存在于 `src/lib.rs` 或别处，两处有重复定义风险。

#### 6. `send_telegram` 函数接受 `bot_token: &str` 参数但完全不使用它

```rust
async fn send_telegram(&self, bot_token: &str, message: &str) -> Result<()> {
    let tg_config = self.config.telegram.as_ref()...;
    // bot_token 参数被忽略，使用 self.config.telegram 的 bot_token
}
```

调用处传入 `&tg_config.bot_token`，函数内又重新获取一次 `self.config.telegram`，
参数完全是多余的，是重构遗留的死代码。

#### 7. `RoutineEngine::start()` 启动后无法动态添加 job

JobScheduler 启动后，新增的 Routine 不会被自动调度。这本身是可以接受的设计简化，
但 `add_routine` / `delete_routine` 函数注释说"重启生效"，用户体验差。
实际上 `tokio-cron-scheduler` 支持向运行中的 scheduler 添加 job，可以做到即时生效。

---

### 🟢 实现正确的部分

- `RoutineEngine` 核心数据结构设计合理（Routine、RoutineExecution、RoutineSource）
- SQLite 表结构设计正确（routines + routines_log）
- 超时保护 + 失败重试逻辑正确
- `send_result()` channel 路由逻辑正确
- `/routine run <name>` 是当前唯一真正工作的命令（直接调用 `execute_routine`，不需要 `&mut self`）
- `/routine list` 和 `/routine logs` 也正常工作
- config schema 新增的 `RoutinesConfig` 设计正确
- main.rs 集成 RoutineEngine 初始化正确

---

## 二、正确架构：RoutineTool

### 核心思路

**删除 NLP 拦截层，改为 RoutineTool**。

```
用户: "每天早上8点帮我生成日报"
  │
  └─ 正常进 Agent Loop（不拦截）
       LLM 理解意图
       → 调用 RoutineTool(action="create", name="daily_brief", schedule="0 8 * * *", message="生成今日日报")
         └─ RoutineTool 写入 RoutineEngine → 返回成功
```

LLM 天然懂 cron 语法，无需我们额外做转换。在 RoutineTool 的 `schedule` 参数描述里说明即可：
> "标准 5 字段 cron 表达式（分 时 日 月 周），如 '0 8 * * *' 表示每天早 8 点"

### RoutineTool 设计

```rust
// src/tools/routine.rs
pub struct RoutineTool {
    engine: Arc<Mutex<RoutineEngine>>,
}

// actions:
// create  name, schedule(cron), message, channel?
// list    → 返回所有 routine 列表
// delete  name
// enable  name
// disable name
// run     name → 立即执行一次
// logs    limit?
```

同时修复 `RoutineEngine` 的 Arc mutability 问题：将 `Arc<RoutineEngine>` 改为
`Arc<tokio::sync::Mutex<RoutineEngine>>`，这样 RoutineTool 和 cli.rs 斜杠命令都可以获取可变引用。

---

## 三、改动范围

### 需要删除的代码

| 位置 | 内容 |
|------|------|
| cli.rs:17-35 | `RoutineIntent` enum |
| cli.rs:37-96 | `detect_routine_intent()` |
| cli.rs:99-131 | `extract_time_description()` |
| cli.rs:133-157 | `extract_task_message()` |
| cli.rs:159-181 | `extract_routine_name_from_action()` |
| cli.rs:183-209 | `generate_routine_name()` |
| cli.rs:211-241 | `normalize_routine_name()` |
| cli.rs:243-312 | `handle_routine_intent()` |
| cli.rs:439-443 | REPL 循环中的意图检测块 |
| routines/mod.rs | `parse_schedule_to_cron()` 函数（regex 实现） |
| routines/mod.rs | `parse_schedule_to_cron` 相关测试 |

### 需要修改的代码

| 文件 | 修改内容 |
|------|---------|
| `src/routines/mod.rs` | `Arc<RoutineEngine>` → `Arc<Mutex<RoutineEngine>>`；修复 `RoutineEngine::memory` 类型为 `Arc<dyn Memory>`；删除多余的 `bot_token` 参数；`NoopMemory` 移至 `memory` 模块 |
| `src/channels/cli.rs` | `routine_engine` 类型改为 `Option<Arc<Mutex<RoutineEngine>>>`；`cmd_routine_add/delete/enable/disable` 真正实现（lock mutex 调用方法） |
| `src/tools/mod.rs` | 在 `create_tools()` 中注册 `RoutineTool` |
| `src/main.rs` | 传给 tools 和 cli 的 engine 类型统一改为 `Arc<Mutex<RoutineEngine>>` |

### 需要新增的代码

| 文件 | 内容 |
|------|------|
| `src/tools/routine.rs` | `RoutineTool` 实现 |
| `src/memory/mod.rs` | `NoopMemory` 迁移到此处（re-export） |

---

## 四、具体实现

### 4.1 修复 RoutineEngine 可变性问题

将 engine 的类型改为 `Arc<tokio::sync::Mutex<RoutineEngine>>`：

```rust
// main.rs
let routine_engine: Option<Arc<tokio::sync::Mutex<RoutineEngine>>> = ...;

// cli.rs 参数类型
routine_engine: Option<Arc<tokio::sync::Mutex<RoutineEngine>>>

// 调用 add_routine
if let Some(engine) = engine {
    let mut eng = engine.lock().await;
    match eng.add_routine(routine).await {
        Ok(()) => println!("✓ Routine '{}' 已创建", name),
        Err(e) => println!("✗ 创建失败: {}", e),
    }
}
```

### 4.2 RoutineTool

```rust
// src/tools/routine.rs

use tokio::sync::Mutex;

pub struct RoutineTool {
    engine: Arc<Mutex<RoutineEngine>>,
}

// Tool parameters schema:
// {
//   "action": "create|list|delete|enable|disable|run|logs",
//   "name": "...",          // create/delete/enable/disable/run 时必填
//   "schedule": "0 8 * * *", // create 时必填（cron 表达式）
//   "message": "...",        // create 时必填
//   "channel": "cli",        // create 时可选，默认 cli
//   "limit": 5               // logs 时可选
// }
```

Tool description:
```
管理定时任务（Routines）。支持创建、列出、删除、启用/禁用、手动触发和查看日志。

创建时 schedule 接受标准 5 字段 cron 表达式（分 时 日 月 周）：
- "0 8 * * *"     每天早 8 点
- "0 */2 * * *"   每 2 小时
- "0 9 * * 1"     每周一早 9 点
- "*/10 * * * *"  每 10 分钟
```

### 4.3 NoopMemory 迁移

将 `NoopMemory` 从 `routines/mod.rs` 迁移到 `src/memory/mod.rs`，在那里 `pub use`。

### 4.4 删除 detect_routine_intent

从 REPL 循环中移除：
```rust
// 删除这整个块（cli.rs:439-443）：
// if let Some(intent) = detect_routine_intent(input) {
//     handle_routine_intent(intent, &routine_engine).await;
//     continue;
// }
```

### 4.5 修复 parse_schedule_to_cron（保留 /routine add 场景）

`/routine add` 斜杠命令仍需要自然语言→cron 解析，因为这是显式命令，用户期望自然语言输入。
保留 regex 解析，但修复越界 bug：

```rust
// 修复下午/晚上 X 点越界
let hour_24 = match time_of_day {
    "下午" if hour <= 12 => hour + 12,
    "下午" => hour,  // 下午12点就是12点（中午）
    "晚上" if hour == 12 => 0,  // 晚上12点=0点
    "晚上" => hour + 12,
    _ => hour,
};
if hour_24 >= 24 { return Err(...); }
```

对于 RoutineTool，直接要求 LLM 提供 cron，不做自然语言解析。

---

## 五、提交策略

| # | commit message | 内容 |
|---|----------------|------|
| 1 | `docs: add P5-5 routines review and refactor plan` | 本文件 |
| 2 | `refactor: move NoopMemory to memory module` | memory/mod.rs |
| 3 | `fix: wrap RoutineEngine in Arc<Mutex> for mutability` | routines/mod.rs, main.rs 类型变更 |
| 4 | `feat: add RoutineTool for LLM-driven routine management` | src/tools/routine.rs |
| 5 | `feat: register RoutineTool in create_tools` | tools/mod.rs |
| 6 | `refactor: remove NLP interception layer from CLI` | cli.rs 删除 detect_routine_intent 及 helpers |
| 7 | `fix: implement cmd_routine_add/delete/enable/disable` | cli.rs，真正调用 engine 方法 |
| 8 | `fix: repair parse_schedule_to_cron overflow bugs` | routines/mod.rs |
| 9 | `refactor: remove unused bot_token param in send_telegram` | routines/mod.rs |
| 10 | `test: add RoutineTool unit tests` | tools/routine.rs |

---

## 六、验证方式

```bash
# 单元测试
cargo test -p rrclaw -- routines
cargo test -p rrclaw -- tools::routine

# 全量测试确保无回归
cargo test -p rrclaw

# clippy 零警告
cargo clippy -p rrclaw -- -D warnings

# 手动验证（启动后）
cargo run -- agent
> /routine list                                 # 应显示"暂无任务"
> /routine add daily_brief "每天早上8点" "生成日报"  # 应真正写入数据库
> /routine list                                 # 应显示刚创建的任务
> /routine run daily_brief                     # 应触发执行
> /routine logs                                # 应显示执行记录
> /routine delete daily_brief                  # 应真正删除
> /routine list                                # 应再次显示"暂无任务"

# 自然语言走 LLM（不再被拦截）
> 每天早上8点帮我生成日报    # 进入 agent loop，LLM 调用 RoutineTool
> 帮我查看当前有哪些定时任务   # LLM 调用 RoutineTool(action="list")
```

---

## 七、不在本次范围内

- 动态热加载（`/routine add` 后无需重启即生效）— 需要向运行中 scheduler 添加 job，留 V2
- 自然语言时间解析的 LLM 回退 — 当前 regex 修复后足够用，LLM 方案留 V2
- Routine 执行结果通知（除 CLI/Telegram 外的通道）
