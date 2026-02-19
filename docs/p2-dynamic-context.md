# P2: 动态上下文 — 解决 System Prompt 臃肿 + Agent 元认知

## 一、问题

### 1.1 System Prompt 臃肿

当前 system prompt 由 7 个硬编码段落拼接而成（`build_system_prompt`），每加一个功能就想往里塞更多信息。例如：

- 模型不知道 SQLite DB 在 `~/.rrclaw/data/memory.db`，想加到 system prompt
- 模型不知道 config.toml 的结构，想加到 system prompt
- 模型不知道自己当前用的 provider/model，想加到 system prompt
- 未来更多：日志路径、已安装的工具列表、会话统计……

**System prompt 越长 → token 越多 → 成本越高 → 模型注意力越分散。**

截图中的实际问题：用户说"查看 conversation_history 中的 tool call 记录"，模型不知道 DB 路径，去 `find ~ -name "*.db"` 大海捞针。

### 1.2 Agent 不知道自己不知道

更深层的问题：**模型遇到不确定的事情时，倾向于猜测和瞎执行，而不是主动问用户。**

现象：
- 不知道 DB 路径 → 盲目 `find` 全盘搜索（浪费 tool call 迭代次数）
- 不确定用户意图 → 猜测性执行命令（可能造成不可逆操作）
- 工具失败后 → 换一种方式重试，而不是问用户"你是不是想要 X？"

期望行为：
- **有工具能查 → 先查** （SelfInfoTool）
- **查不到但能推理 → 推理后说明** （"根据路径规律，DB 应该在 ~/.rrclaw/data/"）
- **真的不知道 → 主动问用户** （"你的数据库文件放在哪里？"）

这是两层能力的结合：**自我信息查询（SelfInfoTool）** + **元认知提示（system prompt 引导模型识别自身知识边界）**

---

## 二、现状分析

### 当前 system prompt 结构（7 段）

| # | 内容 | 类型 | 大约 token |
|---|------|------|-----------|
| 1 | 身份描述 | 硬编码 | ~20 |
| 2 | 工具名称+描述 | 动态（工具注册表） | ~100 |
| 3 | 安全模式规则 | 半动态（按 autonomy） | ~60 |
| 4 | 相关记忆 | 动态（BM25 recall） | ~200（5条） |
| 5 | 环境信息（目录/时间/白名单） | 动态 | ~80 |
| 6 | 工具结果格式指南 | 硬编码 | ~200 |
| 7 | 行为准则 | 硬编码 | ~150 |

**问题集中在第 6、7 段**：大量硬编码的"教模型怎么做"文本，且不管用户问什么都全量注入。

### 当前 memory recall 局限

- 只用用户原始消息做 BM25 搜索，关键词可能匹配不到运维知识
- 只存了 `"User: ...\nAssistant: ..."` 格式的对话摘要，没有结构化的自身知识
- 没有"agent 自我认知"类的记忆条目

---

## 三、方案设计

### 核心思路：SelfInfoTool + 精简 System Prompt

不把所有知识塞进 system prompt，而是给模型一个 **SelfInfoTool**，让它按需查询自身信息。

### 3.1 SelfInfoTool（新工具）

```rust
/// Agent 自我信息查询工具
pub struct SelfInfoTool {
    config: Arc<Config>,
    data_dir: PathBuf,     // ~/.rrclaw/data/
    log_dir: PathBuf,      // ~/.rrclaw/logs/
}
```

**参数 schema**:
```json
{
  "type": "object",
  "properties": {
    "query": {
      "type": "string",
      "enum": ["config", "paths", "provider", "stats", "help"],
      "description": "要查询的信息类型"
    }
  },
  "required": ["query"]
}
```

**各查询返回内容**:

| query | 返回内容 |
|-------|---------|
| `config` | 当前 provider/model/temperature、autonomy 级别、白名单命令（API key 脱敏） |
| `paths` | data_dir、log_dir、config_path、DB 路径、tantivy 索引路径 |
| `provider` | 当前 provider 名称、model、base_url、支持的能力（streaming/reasoning） |
| `stats` | 当前会话消息数、memory 条目总数、今日对话轮数 |
| `help` | 可用斜杠命令列表及说明 |

**优势**：
- 模型只在需要时调用，不浪费 token
- 信息永远是实时的（从 Config/DB 动态读取）
- 新信息只需加一个 query 枚举值，不用改 system prompt

### 3.2 元认知引导（System Prompt 改造）

核心原则：**让模型知道"不知道"是正常的，主动问比猜测执行更好。**

改造后的 system prompt 只保留 **身份 + 工具列表 + 决策原则**：

```
[1] 身份（精简）
    "你是 RRClaw，安全优先的 AI 助手。"

[2] 工具描述（保留）
    自动生成的工具列表（含 self_info）

[3] 安全模式（精简）
    一句话说明当前模式

[4] 相关记忆（保留）
    BM25 recall 结果

[5] 环境信息（精简）
    只保留工作目录和当前时间
    移除：白名单列表、禁止路径（通过 self_info 查询）

[6] 工具结果格式（删除）
    → 现代 LLM 已不需要这些指导

[7] 决策原则（替代原"行为准则"，核心改动）
    → 见下方详细设计
```

**新 [7] 决策原则** — 重点是教模型 **怎么决策**，而不是列一堆规矩：

```text
[决策原则]
1. 先查后做: 不确定的信息先用 self_info 工具查询，不要猜测
2. 不知道就问: 如果查不到也推理不出，直接问用户，不要盲目尝试
3. 说明意图: 调用工具前简短说明为什么需要这个工具
4. 失败时反思: 工具失败后先分析原因，再决定重试/换方式/问用户
   - 第 1 次失败: 分析原因，换一种方式
   - 第 2 次失败: 向用户说明情况，询问建议
   - 不要同一个目标尝试超过 3 次
5. 用中文回复，除非用户使用其他语言
```

**关键区别**：
- 旧版列了 7 条"不要做 X"的禁令 → 模型经常无视
- 新版改为 5 条"遇到 Y 情况做 Z"的决策流程 → 模型更容易遵循

**这是改动最大、影响最深的部分。** 需要通过实际对话测试反复调优措辞。

**预计节省**: 从 ~800 token 降到 ~300 token（节省约 60%）。

### 3.3 知识种子（可选增强）

在首次运行时，向 memory 存入几条"自我认知"条目：

```rust
// 首次初始化时存入
memory.store("rrclaw_db_path",
    "RRClaw 的 SQLite 数据库位于 ~/.rrclaw/data/memory.db，包含 memories 和 conversation_history 两张表",
    MemoryCategory::Core).await?;

memory.store("rrclaw_log_path",
    "RRClaw 的日志文件位于 ~/.rrclaw/logs/rrclaw.log.YYYY-MM-DD，默认 debug 级别",
    MemoryCategory::Core).await?;
```

这样当用户问"数据库在哪"时，BM25 recall 就能匹配到，模型不需要额外调用工具。

---

## 四、架构影响分析

### 4.1 需要改动的文件

| 文件 | 改动 | 复杂度 |
|------|------|--------|
| `src/tools/self_info.rs` | **新增** — SelfInfoTool 实现 | 中 |
| `src/tools/mod.rs` | 注册 SelfInfoTool，调整 `create_tools()` 签名 | 低 |
| `src/agent/loop_.rs` | 重写 `build_system_prompt` 第 5/6/7 段 | **高（核心改动）** |
| `src/memory/sqlite.rs` | 添加知识种子初始化逻辑（可选） | 低 |
| `src/tools/Claude.md` | 更新工具模块设计文档 | 低 |
| `src/agent/Claude.md` | 更新 system prompt 设计说明 | 低 |

### 4.2 不需要改动的部分

- Provider 层（tool call 格式不变）
- Security 层（SelfInfoTool 纯读取，不需要安全检查）
- Channel 层（CLI/Telegram 不感知 system prompt 内容）
- 对话历史格式（ConversationMessage 不变）

### 4.3 风险评估

| 风险 | 影响 | 缓解 |
|------|------|------|
| 精简 system prompt 后模型行为退化 | 高 | 分步精简，每步 A/B 测试 |
| 模型过度调用 self_info（每轮都查） | 中 | 工具描述中明确"仅在需要时查询" |
| 决策原则措辞不够好，模型不遵循 | 中 | 迭代调优，参考其他 agent 项目的 prompt |
| 知识种子跟实际配置不同步 | 低 | 每次启动时重新写入（upsert） |

---

## 五、实现计划

### Commit 1: docs
- [x] 本文档

### Commit 2: SelfInfoTool trait 实现
- 新文件 `src/tools/self_info.rs`
- 实现 `Tool` trait，支持 5 种 query（config/paths/provider/stats/help）
- 纯读取，`execute()` 不检查 SecurityPolicy
- API key 脱敏：只显示前 4 位 + `****`

### Commit 3: 注册 SelfInfoTool + 测试
- 修改 `src/tools/mod.rs`，在工具注册表中添加 `SelfInfoTool`
- SelfInfoTool 需要 `Config` 和路径信息，调整 `create_tools()` 签名
- 单元测试：各 query 返回正确内容、API key 脱敏验证

### Commit 4: 重写 system prompt（核心）
- 修改 `src/agent/loop_.rs` 的 `build_system_prompt`
- 删除第 6 段（工具结果格式 → 现代 LLM 不需要）
- 替换第 7 段（行为准则 → 决策原则）
- 精简第 5 段（移除白名单/禁止路径 → 通过 self_info 查询）
- 测试：精简前后 system prompt 长度对比

### Commit 5: 知识种子
- 在 `SqliteMemory` 添加 `seed_core_knowledge()` 方法
- 启动时调用，使用 upsert 语义（已存在则更新）
- 种子内容：DB 路径、日志路径、config 路径、基本能力说明
- 测试：种子存入后 recall 能命中

### Commit 6: 更新模块 Claude.md
- `src/tools/Claude.md` — 添加 SelfInfoTool 设计说明
- `src/agent/Claude.md` — 更新 system prompt 设计说明

---

## 六、验证方式

### 6.1 自动化测试
- SelfInfoTool 各 query 返回格式正确
- API key 脱敏逻辑
- system prompt 字符数对比（精简前 vs 后）
- 知识种子 recall 命中率

### 6.2 对话测试（手动，最重要）

**场景 A: 信息查询 — 应该用 self_info**
```
用户: "你现在用的什么模型？"
期望: 调用 self_info(query="provider") → 回答具体 model 名
不期望: 猜测或回答"我不知道"
```

**场景 B: 路径查询 — 应该 recall 或 self_info**
```
用户: "数据库文件在哪？"
期望: memory recall 命中知识种子，或调用 self_info(query="paths")
不期望: find ~ -name "*.db"
```

**场景 C: 元认知 — 不知道就问**
```
用户: "帮我把日志发到 Slack"
期望: "RRClaw 目前没有 Slack 集成。你有 Slack webhook URL 吗？我可以用 shell 工具通过 curl 发送。"
不期望: 盲目尝试安装 slack CLI 或猜测 webhook URL
```

**场景 D: 失败后反思**
```
用户: "读取 /tmp/secret.txt"
（假设文件不存在或被安全策略阻止）
期望: 说明失败原因 + 询问用户"文件路径是否正确？"
不期望: 换 cat/head/shell 反复尝试读同一个文件
```

### 6.3 回归测试
- 现有 92 个测试全部通过
- 现有 tool call 流程不受影响（shell/file_read/file_write）

---

## 七、不在本次范围

- **AskUserTool**（让模型在 tool call 循环中暂停等用户输入）— 改动太大，需重构 agent loop，且当前模型可以通过文本回复来提问，下一轮用户自然回答
- **按 query 意图动态选择 system prompt 段落**（过于复杂，收益不确定）
- **ConfigTool**（P2 另一个独立特性，见 `docs/p2-slash-commands-and-config-tool.md`）
- **向量搜索 memory**（当前 BM25 已够用）
- **多 session 管理**（当前按天分 session 已满足需求）

### 关于 AskUserTool 的思考（留档，不实现）

一个更激进的方案是增加 `AskUserTool`：模型在 tool call 循环中途遇到不确定的事情时，调用这个工具暂停循环，等待用户输入后继续。

```rust
// 伪代码 — 不在本次实现
pub struct AskUserTool;
// execute() 时暂停 agent loop，通过 channel 发问题给用户
// 用户回答后注入 ToolResult，循环继续
```

**不实现的原因**：
1. 需要重构 agent loop — 当前 `process_message` 是同步循环，需要改成协程/状态机才能中途暂停
2. 流式输出场景更复杂 — `process_message_stream` 中暂停意味着要在 StreamEvent 流中插入一个"等待用户"事件
3. 当前已经可以实现 — 模型回复文本提问 → 用户下一轮回答 → 模型根据回答继续工作。只是少了"一轮中断"的能力，但多数场景下分两轮也能用
4. 投入产出比低 — 大量架构改动只为解决少数"需要中途追问"的场景

如果后续发现"分两轮"体验太差，再考虑实现 AskUserTool。
