# P5-3: ActionTracker 操作速率限制 实现计划

## 背景

当前 Agent 每轮最多执行 10 次工具调用（`MAX_TOOL_ITERATIONS`），但没有跨轮次的总量控制。在 Full 自主模式下，如果 Agent 进入循环或遇到错误反复重试，可能在短时间内消耗大量 API 配额甚至触发计费风险。

参考 ZeroClaw 的 `ActionTracker`：**每小时操作数上限（默认 20 次/小时）**。

**ActionTracker 的核心作用**：
- 防止 Agent 失控（循环工具调用、错误重试风暴）
- 给用户一个明确的安全兜底，即使是 Full 模式也有总量上限
- 超限时向用户给出清晰提示，告知何时恢复

---

## 一、架构设计

```
config.toml
[security]
max_actions_per_hour = 20    ← 从此读取上限
            │
SecurityPolicy.max_actions_per_hour
            │
Agent::new() → ActionTracker::new(max_actions_per_hour)
            │
            ▼
   process_message() / process_message_stream()
            │
   工具调用循环（Tool Call Loop）
            │
   每次执行工具前: action_tracker.try_record()
   ├── true: 继续执行
   └── false: 立即停止循环，返回速率限制提示给用户
```

### 时间窗口策略

使用**滑动窗口（Sliding Window）**：
- 记录每次操作的时间戳（`Instant`）
- 检查时清除超过 1 小时的旧记录
- 已记录数 ≥ 上限 → 拒绝

优点：比固定窗口（每小时整点重置）更平滑，不会出现整点后突发大量请求。

---

## 二、ActionTracker 实现

### 2.1 新增文件：src/security/action_tracker.rs

```rust
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// 操作速率追踪器（滑动时间窗口）
///
/// 每次工具执行前调用 `try_record()`：
/// - 返回 `true`：允许执行，已计数
/// - 返回 `false`：已达速率上限，拒绝执行
///
/// 线程安全说明：`ActionTracker` 不是 `Sync`，由 `Agent` 独占持有（非共享）。
/// 不需要 Mutex，设计更简单。
pub struct ActionTracker {
    /// 每窗口最大操作数
    pub max_actions: usize,
    /// 时间窗口（默认 1 小时）
    window: Duration,
    /// 已记录操作的时间戳队列（最早在前）
    timestamps: VecDeque<Instant>,
}

impl ActionTracker {
    /// 创建 ActionTracker，使用 1 小时滑动窗口
    pub fn new(max_actions: usize) -> Self {
        Self {
            max_actions,
            window: Duration::from_secs(3600),
            timestamps: VecDeque::new(),
        }
    }

    /// 创建自定义时间窗口的 ActionTracker（主要用于测试）
    pub fn with_window(max_actions: usize, window: Duration) -> Self {
        Self {
            max_actions,
            window,
            timestamps: VecDeque::new(),
        }
    }

    /// 尝试记录一次操作
    ///
    /// - `true`：操作被允许，时间戳已记录
    /// - `false`：已达速率上限，操作被拒绝（不记录）
    pub fn try_record(&mut self) -> bool {
        self.prune();
        if self.timestamps.len() >= self.max_actions {
            return false;
        }
        self.timestamps.push_back(Instant::now());
        true
    }

    /// 当前窗口内已记录的操作数（先 prune 再返回，准确值）
    pub fn current_count(&mut self) -> usize {
        self.prune();
        self.timestamps.len()
    }

    /// 剩余可执行次数（当前窗口内）
    pub fn remaining(&mut self) -> usize {
        self.prune();
        self.max_actions.saturating_sub(self.timestamps.len())
    }

    /// 距离下一个 slot 释放还需等待多长时间
    ///
    /// 如果当前未达上限，返回 None（无需等待）
    /// 如果已达上限，返回最早记录距离过期的剩余时间
    pub fn next_slot_in(&mut self) -> Option<Duration> {
        self.prune();
        if self.timestamps.len() < self.max_actions {
            return None;
        }
        self.timestamps.front().map(|earliest| {
            let elapsed = earliest.elapsed();
            if elapsed >= self.window {
                Duration::ZERO
            } else {
                self.window - elapsed
            }
        })
    }

    /// 重置所有记录（用于测试或 /clear 命令后）
    pub fn reset(&mut self) {
        self.timestamps.clear();
    }

    /// 清除超出时间窗口的旧记录
    fn prune(&mut self) {
        while let Some(&front) = self.timestamps.front() {
            if front.elapsed() >= self.window {
                self.timestamps.pop_front();
            } else {
                // VecDeque 按时间顺序排列，前面不过期则后面也不过期
                break;
            }
        }
    }
}

impl Default for ActionTracker {
    fn default() -> Self {
        Self::new(20)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn fast_tracker(max: usize) -> ActionTracker {
        // 测试用：1秒窗口，避免 sleep 太长
        ActionTracker::with_window(max, Duration::from_millis(200))
    }

    #[test]
    fn allows_up_to_limit() {
        let mut tracker = ActionTracker::new(3);
        assert!(tracker.try_record()); // 1
        assert!(tracker.try_record()); // 2
        assert!(tracker.try_record()); // 3
        assert!(!tracker.try_record()); // 超限，拒绝
        assert!(!tracker.try_record()); // 还是拒绝
    }

    #[test]
    fn allows_zero_limit_always_rejects() {
        let mut tracker = ActionTracker::new(0);
        assert!(!tracker.try_record());
    }

    #[test]
    fn remaining_decreases_correctly() {
        let mut tracker = ActionTracker::new(5);
        assert_eq!(tracker.remaining(), 5);
        tracker.try_record();
        assert_eq!(tracker.remaining(), 4);
        tracker.try_record();
        assert_eq!(tracker.remaining(), 3);
    }

    #[test]
    fn current_count_accurate() {
        let mut tracker = ActionTracker::new(10);
        assert_eq!(tracker.current_count(), 0);
        tracker.try_record();
        tracker.try_record();
        assert_eq!(tracker.current_count(), 2);
    }

    #[test]
    fn reset_clears_all() {
        let mut tracker = ActionTracker::new(3);
        tracker.try_record();
        tracker.try_record();
        tracker.try_record();
        assert!(!tracker.try_record()); // 已满
        tracker.reset();
        assert!(tracker.try_record()); // 清空后可继续
    }

    #[test]
    fn next_slot_in_none_when_under_limit() {
        let mut tracker = ActionTracker::new(5);
        tracker.try_record();
        // 未达上限，不需要等待
        assert!(tracker.next_slot_in().is_none());
    }

    #[test]
    fn next_slot_in_some_when_at_limit() {
        let mut tracker = ActionTracker::new(2);
        tracker.try_record();
        tracker.try_record();
        // 已达上限，应该返回等待时间
        assert!(tracker.next_slot_in().is_some());
    }

    #[tokio::test]
    async fn slots_released_after_window() {
        let mut tracker = fast_tracker(2); // 2次/200ms 窗口
        tracker.try_record();
        tracker.try_record();
        assert!(!tracker.try_record()); // 超限

        // 等待窗口过期（250ms > 200ms）
        tokio::time::sleep(Duration::from_millis(250)).await;

        // 窗口内旧记录过期，应可以再次执行
        assert!(tracker.try_record());
    }

    #[test]
    fn default_max_is_20() {
        let tracker = ActionTracker::default();
        assert_eq!(tracker.max_actions, 20);
    }
}
```

---

## 三、SecurityPolicy 扩展

在 `src/security/policy.rs` 的 `SecurityPolicy` 结构体中新增字段：

```rust
#[derive(Debug, Clone)]
pub struct SecurityPolicy {
    pub autonomy: AutonomyLevel,
    pub allowed_commands: Vec<String>,
    pub workspace_dir: PathBuf,
    pub blocked_paths: Vec<PathBuf>,
    pub max_actions_per_hour: usize,  // ← 新增
}
```

在 `Default` 实现中补充默认值：
```rust
impl Default for SecurityPolicy {
    fn default() -> Self {
        Self {
            autonomy: AutonomyLevel::Supervised,
            allowed_commands: vec![/* 已有 */],
            workspace_dir: /* 已有 */,
            blocked_paths: vec![/* 已有 */],
            max_actions_per_hour: 20,  // ← 新增默认值
        }
    }
}
```

---

## 四、Config Schema 扩展

在 `src/config/schema.rs` 的 `SecurityConfig` 中新增字段：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    #[serde(default = "default_autonomy")]
    pub autonomy: String,

    #[serde(default = "default_allowed_commands")]
    pub allowed_commands: Vec<String>,

    #[serde(default = "default_workspace_only")]
    pub workspace_only: bool,

    #[serde(default = "default_max_actions_per_hour")]  // ← 新增
    pub max_actions_per_hour: usize,                    // ← 新增
}

fn default_max_actions_per_hour() -> usize { 20 }
```

config.toml 示例：
```toml
[security]
autonomy = "supervised"
allowed_commands = ["ls", "cat", "grep", "git", "cargo"]
workspace_only = true
max_actions_per_hour = 20   # 可选，默认 20。设为 0 禁用限制
```

> **特殊值：`max_actions_per_hour = 0`** 表示禁用速率限制（无上限）。

---

## 五、SecurityPolicy 构建时读取配置

在 `src/security/mod.rs` 或 `main.rs` 中，构建 `SecurityPolicy` 时补充新字段：

```rust
// 原来构建 SecurityPolicy 的地方（在 main.rs 的 run_agent 函数中）
let policy = SecurityPolicy {
    autonomy: /* 已有 */,
    allowed_commands: /* 已有 */,
    workspace_dir: /* 已有 */,
    blocked_paths: /* 已有 */,
    max_actions_per_hour: config.security.max_actions_per_hour,  // ← 新增
};
```

---

## 六、Agent 集成（核心改动）

### 6.1 src/agent/loop_.rs — Agent 结构体新增字段

```rust
// 在 Agent 结构体中新增：
pub struct Agent {
    provider: Box<dyn Provider>,
    tools: Vec<Box<dyn Tool>>,
    memory: Box<dyn Memory>,
    policy: SecurityPolicy,
    provider_name: String,
    base_url: String,
    model: String,
    temperature: f64,
    history: Vec<ConversationMessage>,
    confirm_fn: Option<ConfirmFn>,
    skills_meta: Vec<SkillMeta>,
    routed_skill_content: Option<String>,
    action_tracker: ActionTracker,  // ← 新增
}
```

在文件顶部新增 import：
```rust
use crate::security::ActionTracker;  // ← 新增
```

### 6.2 Agent::new() 中初始化 ActionTracker

```rust
impl Agent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(/* 现有参数不变 */) -> Self {
        let max_actions = policy.max_actions_per_hour;  // ← 新增：从 policy 读取
        Self {
            provider,
            tools,
            memory,
            policy,
            provider_name,
            base_url,
            model,
            temperature,
            history: Vec::new(),
            confirm_fn: None,
            skills_meta,
            routed_skill_content: None,
            action_tracker: ActionTracker::new(max_actions),  // ← 新增
        }
    }
}
```

### 6.3 process_message() — 工具调用循环中加入速率检查

在现有的工具调用 for 循环中，找到"有 tool calls — 记录并逐个执行"部分，添加速率限制检查：

**改动位置**：`for tc in &response.tool_calls { ... }` 循环内，**在 pre_validate 之前**。

```rust
// 原来的循环（精简版）：
for iteration in 0..MAX_TOOL_ITERATIONS {
    // ... 调用 LLM ...

    if response.tool_calls.is_empty() {
        // ... 无 tool calls，结束 ...
        break;
    }

    // 有 tool calls — 记录并逐个执行
    self.history.push(ConversationMessage::AssistantToolCalls { ... });

    // ↓↓↓ 改动：在 for tc in &response.tool_calls 循环内添加速率检查 ↓↓↓
    let mut rate_limited = false;  // ← 新增：速率限制标志
    for tc in &response.tool_calls {

        // ─── 新增：速率限制检查 ──────────────────────────────────
        if self.action_tracker.max_actions > 0 // 0 表示禁用限制
            && !self.action_tracker.try_record()
        {
            let wait_msg = self.action_tracker
                .next_slot_in()
                .map(|d| {
                    let mins = d.as_secs() / 60 + 1;
                    format!("约 {} 分钟后", mins)
                })
                .unwrap_or_else(|| "稍后".to_string());

            let rate_limit_msg = format!(
                "已达到每小时操作上限（{} 次）。{} 可继续使用，或在 config.toml 中调整 max_actions_per_hour。",
                self.action_tracker.max_actions,
                wait_msg,
            );

            info!("工具调用被速率限制: {}", tc.name);
            // 将速率限制消息作为 tool result 推入 history（让 LLM 感知）
            self.history.push(ConversationMessage::ToolResult {
                tool_call_id: tc.id.clone(),
                content: format!("[速率限制] {}", rate_limit_msg),
            });
            rate_limited = true;
            // ← 不 break 内层循环，继续处理剩余 tool calls（全部添加速率限制消息）
            // 这样 LLM 能在 history 中看到所有工具都被限制，自然停止重试
            continue;
        }
        // ─── 速率限制检查结束 ─────────────────────────────────────

        // 以下是原有的 pre_validate / confirm / execute 逻辑（不变）
        // ...
    }

    // ← 新增：速率限制后立即结束整个 iteration 循环
    if rate_limited {
        // 给 LLM 最后一次机会根据速率限制消息生成回复
        // （不 break，让下一个 iteration 继续，LLM 会看到所有 tool result 并返回文本）
        // 注意：下一个 iteration 中 LLM 应该不再调用工具，直接返回速率限制提示给用户
    }
}
```

> **设计决策**：当速率限制触发时，我们不立即 break 整个 iteration 循环，而是：
> 1. 把当前批次所有 tool calls 都标为"速率限制"的 ToolResult
> 2. 让 LLM 看到这些 ToolResult，自然地生成一条"已达上限"的最终回复
> 3. 这样用户能看到 LLM 的解释，而不是一个裸的系统错误

### 6.4 process_message_stream() 同步改动

`process_message_stream()` 和 `process_message()` 有相同的工具调用循环，需要做**完全相同**的改动，复制粘贴并调整。

---

## 七、src/security/mod.rs — 模块注册

```rust
// src/security/mod.rs 现有内容中新增：
mod action_tracker;
pub use action_tracker::ActionTracker;
```

---

## 八、改动范围汇总

| 文件 | 改动类型 | 说明 |
|------|---------|------|
| `src/security/action_tracker.rs` | **新增文件** | ActionTracker 完整实现 |
| `src/security/mod.rs` | 微改 | 2 行：mod + pub use |
| `src/security/policy.rs` | 微改 | SecurityPolicy 新增 `max_actions_per_hour: usize` 字段 + Default 默认值 |
| `src/config/schema.rs` | 微改 | SecurityConfig 新增 `max_actions_per_hour: usize` + serde default |
| `src/agent/loop_.rs` | 中等改动 | Agent 结构体新增字段 + new() 初始化 + 两处工具循环添加速率检查 |
| `src/main.rs` | 微改 | SecurityPolicy 构建时填充 `max_actions_per_hour` |

**不需要改动**：Provider、Memory、Tool（所有现有工具）、CLI、Telegram channel。

---

## 九、提交策略

| # | 提交 message | 内容 |
|---|-------------|------|
| 1 | `docs: add P5-3 ActionTracker design` | 本文件 |
| 2 | `feat: add ActionTracker with sliding window rate limiting` | src/security/action_tracker.rs + mod.rs |
| 3 | `feat: add max_actions_per_hour to SecurityPolicy and Config` | policy.rs + schema.rs |
| 4 | `feat: integrate ActionTracker into Agent tool call loop` | agent/loop_.rs + main.rs |
| 5 | `test: add ActionTracker unit tests` | 已在 action_tracker.rs 内 |

---

## 十、测试执行方式

```bash
# 运行 ActionTracker 单元测试
cargo test -p rrclaw security::action_tracker

# 运行全部测试（确保无回归）
cargo test -p rrclaw

# clippy
cargo clippy -p rrclaw -- -D warnings
```

---

## 十一、关键注意事项

### 11.1 `max_actions_per_hour = 0` 禁用限制
在代码中明确检查 `max_actions > 0` 才启用限制：
```rust
if self.action_tracker.max_actions > 0 && !self.action_tracker.try_record() {
    // 速率限制逻辑
}
```
配置为 0 时完全跳过，给 Full 自主模式的高级用户提供禁用出口。

### 11.2 ActionTracker 不是 Arc<Mutex<>>
`ActionTracker` 由 `Agent` 独占，Agent 本身是 `&mut self` 方法调用，天然单线程访问。不需要锁，性能更好。

### 11.3 ReadOnly 模式下 ActionTracker 不计数
ReadOnly 模式下所有工具在 pre_validate 阶段就被拦截，永远不会到达速率检查的位置。ActionTracker 无需针对 ReadOnly 做特殊处理。

### 11.4 Instant 不能跨进程持久化
ActionTracker 只记录当前进程内的操作。每次重启 RRClaw，计数器归零。这是预期行为（与会话周期一致）。

### 11.5 两处工具循环必须同步改动
`process_message()` 和 `process_message_stream()` 有几乎相同的工具调用逻辑，**两处都要加速率检查**，否则流式模式不受限制。

### 11.6 history 中速率限制 ToolResult 的格式
```
[速率限制] 已达到每小时操作上限（20 次）。约 45 分钟后可继续使用...
```
前缀 `[速率限制]` 让 LLM 能识别这不是工具的正常输出，会生成合适的用户提示而不是重试。

---

## 十二、用户体感示例

**触发速率限制时的对话流程**：

```
用户: 帮我分析这个目录下所有 Rust 文件
LLM: 调用 shell → 调用 file_read → ... (累计 20 次后)
LLM: 调用 memory_recall

[速率限制] 已达到每小时操作上限（20 次）。约 32 分钟后可继续使用...

LLM 回复: 我已完成了主要分析，但受限于每小时操作上限（20 次），
          部分文件未能完成读取。大约 32 分钟后，我可以继续。
          如需立即继续，您可以在 ~/.rrclaw/config.toml 中将
          max_actions_per_hour 调大，或运行 /clear 开启新对话。

用户: 好的，我明白了。把已经分析的结果告诉我吧。
```
