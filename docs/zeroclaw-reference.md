# ZeroClaw 架构调研参考

> 来源: https://github.com/zeroclaw-labs/zeroclaw
> ~3.4MB binary, <10ms startup, 1,017 tests, 22+ providers, 8 traits, 18,900+ lines Rust

## 核心 Trait 体系

ZeroClaw 的所有子系统都通过 Rust trait 抽象，使用 `async_trait` + `Send + Sync` bound，通过 `Box<dyn Trait>` 或 `Arc<dyn Trait>` 做动态分发。

| Trait | 职责 | 方法签名概要 |
|-------|------|-------------|
| `Provider` | AI 模型调用 | `chat_with_system()`, `chat_with_history()`, `warmup()` |
| `Channel` | 消息通道 | `send()`, `listen(tx: mpsc::Sender)`, `health_check()` |
| `Memory` | 持久化记忆 | `store()`, `recall()`, `get()`, `list()`, `forget()`, `count()` |
| `Tool` | 工具执行 | `name()`, `description()`, `parameters_schema()`, `execute()` |
| `Sandbox` | 安全沙箱 | `wrap_command()`, `is_available()` (同步，非 async) |

## Provider 系统

### 工厂模式
`create_provider(name, api_key) -> Box<dyn Provider>` 支持 25+ provider。

### compatible.rs — OpenAI 兼容适配器
- 接收 `base_url` + `api_key` + `AuthStyle`
- URL 自动探测: 如果 base_url 已经以 `/chat/completions` 结尾则原样使用，否则自动拼接
- AuthStyle: `Bearer` / `XApiKey` / `Custom(String)`
- 404 Fallback: chat/completions 返回 404 时自动重试 Responses API

### 弹性层
- `ReliableProvider`: 重试 + 指数退避 + fallback chain
- `RouterProvider`: 根据 model hint 路由到不同 provider
- API key 解析优先级: 显式传入 > provider 环境变量 > `ZEROCLAW_API_KEY` > `API_KEY`
- 错误消息脱敏: 清理 `sk-`, `xoxb-`, `xoxp-` 前缀

## Agent Loop

### 核心函数
```rust
pub(crate) async fn run_tool_call_loop(
    provider: &dyn Provider,
    history: &mut Vec<ChatMessage>,
    tools_registry: &[Box<dyn Tool>],
    observer: &dyn Observer,
    provider_name: &str,
    model: &str,
    temperature: f64,
) -> Result<String>
```

### 循环流程
1. 调用 `provider.chat_with_history()` 获取 `ChatResponse`
2. 解析 tool calls（优先级: 原生 JSON → XML `<tool_call>` → 文本内 JSON）
3. 逐个执行 tool，结果包裹在 `<tool_result name="...">` XML 中
4. 推入 history 作为 user message，回到步骤 1
5. 无 tool calls 时返回文本作为最终回复
6. 上限: `MAX_TOOL_ITERATIONS = 10`

### History 管理
- `trim_history()`: 硬性上限 50 条非 system 消息
- `auto_compact_history()`: 用 LLM 做摘要压缩，摘要上限 2000 字符
- 压缩 transcript 上限 12,000 字符

### Tool Call 解析（多格式 fallback）
```
1. response.tool_calls (原生 OpenAI 格式)
2. <tool_call>{"name":"...","arguments":{...}}</tool_call> (XML 包裹 JSON)
3. 文本中的裸 JSON 对象
```

## Channel 系统

### 架构
- `ChannelRuntimeContext`: 捆绑 channels + provider + memory + tools + observer + config
- 所有 channel 通过 `mpsc::channel(100)` 统一消息总线
- `spawn_supervised_listener()`: 每个 channel 的 listen() 用指数退避自动重启
- 并发控制: `Semaphore` 限制 in-flight 消息数（每 channel 4 并发，总计 8-64）

### 消息处理
```
channel.listen(tx) → rx.recv() → typing indicator → memory enrichment
→ LLM 调用 (90s timeout) → channel.send(response, sender)
```

## Memory 系统

### SQLite 实现
- embeddings 存储为 BLOB
- FTS5 关键词搜索 + BM25 排序
- 混合融合: 加权 vector + keyword 结果
- Embedding 缓存 + LRU 淘汰
- Markdown-aware chunking

### 工厂函数
```rust
create_memory(config) → match config.backend {
    "sqlite" => SqliteMemory::with_embedder(workspace_dir, embedder, ...),
    "markdown" => MarkdownMemory::new(workspace_dir),
    "none" => NoopMemory,
}
```

## Security 模型

### AutonomyLevel
- `ReadOnly`: 不执行任何操作
- `Supervised`: 中/高风险操作需用户确认
- `Full`: 自主执行

### 命令风险分级
- High: rm, sudo, curl, ssh...
- Medium: git commit, npm install, cargo publish, 文件操作
- Low: 只读命令

### 安全措施
- workspace-only 路径限制
- 系统目录禁止访问
- 路径遍历检测
- 每小时操作数上限 (ActionTracker, 默认 20/hour)
- 命令白名单
- SecretStore 加密/解密
- 可选 OS 沙箱: Docker / Bubblewrap / Firejail / Landlock

## System Prompt 构造

ZeroClaw 的 `build_system_prompt()` 按层构造:
1. Tool descriptions（所有注册工具的 spec）
2. Safety rules（安全规则）
3. Skills（已加载的 skill 描述）
4. Workspace context（工作区文件）
5. Bootstrap files（AGENTS.md, SOUL.md, IDENTITY.md, USER.md 等）
6. DateTime + runtime info

## Tools 注册表

### 默认工具 (3 个)
- shell, file_read, file_write

### 完整工具集 (条件启用)
- + memory_store, memory_recall, memory_forget
- + git_operations, browser, browser_open
- + http_request, screenshot, image_info
- + composio, delegate

每个 tool 接收 `Arc<SecurityPolicy>` 执行前做安全检查。

## Config 系统

`Config` 通过 TOML 文件加载，支持环境变量覆盖。子配置:
- `AutonomyConfig` — 权限级别、命令白名单
- `MemoryConfig` — 后端选择、自动保存
- `ReliabilityConfig` — 重试策略
- `ObservabilityConfig` — 日志/metrics
- `IdentityConfig` — 身份系统
- 各 Channel 的独立配置（Telegram/Discord/Slack/Matrix 等）

## 关键设计模式总结

| 模式 | 用法 |
|------|------|
| Trait 对象动态分发 | `Box<dyn Provider>`, `Arc<dyn Tool>` |
| 工厂函数 | `create_provider()`, `create_memory()`, `all_tools()` |
| 装饰器/包装器 | `ReliableProvider` 包装重试, `RouterProvider` 包装路由 |
| 并发控制 | `Semaphore` 限流, `JoinSet` 并行任务 |
| 监督重启 | Channel listener 指数退避循环 |
| 多格式兼容 | Tool call 三级 fallback 解析 |
| 自动压缩 | LLM 驱动的会话摘要 |
