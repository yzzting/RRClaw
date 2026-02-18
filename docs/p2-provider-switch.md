# P2: 运行时 Provider 切换

## 问题

当前 `/model <name>` 只修改模型名字符串，但不同模型可能属于不同 Provider（如 `deepseek-chat` 用 DeepSeek API，`claude-sonnet` 用 Anthropic API），需要不同的 api_key、base_url。单纯换模型名会导致请求发到错误的 API。

## 方案

### 核心改动：Agent 支持运行时切换 Provider

Agent 新增 `set_provider()` 方法，允许替换底层 Provider 实例：

```rust
impl Agent {
    /// 运行时切换 Provider 和模型
    pub fn switch_provider(&mut self, provider: Box<dyn Provider>, model: String) {
        self.provider = provider;
        self.model = model;
    }
}
```

### CLI 改动：`/model` 和 `/provider` 命令

`run_repl` 需要接收 `Config` 参数，才能在运行时查找 provider 配置并创建新 Provider。

**`/provider <name>`** — 切换到指定 provider，同时使用该 provider 配置的默认模型：
```
rrclaw〉/provider claude
已切换到 claude (模型: claude-sonnet-4-5-20250929)
```

**`/model <provider>/<model>`** — 切换 provider 并指定模型（用 `/` 分隔）：
```
rrclaw〉/model claude/claude-sonnet-4-5-20250929
已切换到 claude/claude-sonnet-4-5-20250929
```

**`/model <model>`**（不含 `/`）— 仅切换模型名，不换 Provider（适用于同 Provider 下切模型，如 `deepseek-chat` → `deepseek-reasoner`）。

### 改动文件

| 文件 | 改动 |
|------|------|
| `src/agent/loop_.rs` | 新增 `switch_provider()` 方法 |
| `src/channels/cli.rs` | `run_repl`/`run_single` 接收 `&Config`；`/provider` 命令；`/model` 支持 `provider/model` 格式 |
| `src/main.rs` | 传 `&config` 给 `run_repl`/`run_single` |

### 提交策略

1. `feat: add switch_provider method to Agent`
2. `feat: add /provider command and enhance /model with provider switching`

### 验证

1. `/provider claude` → 后续消息走 Anthropic API
2. `/model deepseek/deepseek-reasoner` → 切到 DeepSeek Reasoner
3. `/model gpt-4o-mini` → 同 provider 下仅换模型
4. `cargo test` 全部通过
