# P6：E2E 测试 + 模块文档补全

## 背景

P5 Routines 开发过程暴露了两个问题：
1. **文档滞后**：各模块 Claude.md 自 P3/P4 后未更新，新功能（GitTool、MemoryTools、MCP、Routines、injection 检测等）均未记录
2. **测试层次缺失**：只有 mock 单元测试，缺少集成测试和 E2E 测试，导致"scheduler 不启动""cron 格式错误"等外部库行为 bug 完全漏网

---

## 任务 B：模块文档补全（先做）

### 优先级顺序

| 优先级 | 文件 | 状态 | 主要缺失内容 |
|--------|------|------|-------------|
| P1 | `src/mcp/Claude.md` | ❌ 不存在 | 整个 MCP Client 模块 |
| P1 | `src/tools/Claude.md` | 严重过期 | GitTool / HttpRequestTool / MemoryTools / RoutineTool / McpTool |
| P2 | `src/agent/Claude.md` | 过期 | injection check 集成 / reasoning_content / Phase1 传 recent history |
| P2 | `src/channels/Claude.md` | 过期 | ExternalPrinter / /routine / /mcp / /identity / /switch |
| P3 | `src/security/Claude.md` | 过期 | injection 检测模块（Block/Review/Warn）/ needs_injection_check |
| P3 | `src/config/Claude.md` | 过期 | RoutinesConfig / injection_check 字段 |
| P4 | `src/memory/Claude.md` | 轻微过期 | memory tools 暴露 / Arc<dyn Memory> impl |
| P4 | `src/providers/Claude.md` | 轻微过期 | reasoning_content 字段 |
| P5 | `src/skills/Claude.md` | 基本完整 | mcp-install 内置 skill |

### 每个文件的更新策略

每个 Claude.md 按以下格式组织：
1. 模块职责（一句话）
2. 核心数据结构 / Trait（当前实际代码）
3. 已实现功能清单（含版本标注 P0~P5）
4. 关键实现细节和已踩的坑
5. 测试要求（单元 + 集成）

### 提交策略

每个文件独立提交：`docs(module): update Claude.md for P4/P5 changes`

---

## 任务 A：E2E 测试（需更详细计划，暂缓实现）

### A1：Scheduler 集成测试

**目标**：验证 `tokio-cron-scheduler` 的真实行为，不 mock scheduler。

**需要设计的问题**：
1. RoutineEngine.execute_routine 会调用 LLM，测试中如何隔离？
   - 方案1：为 RoutineEngine 添加 `execute_fn` 回调（测试时注入）
   - 方案2：直接测试底层 JobScheduler（不经过 RoutineEngine）
   - 方案3：使用 mock provider（已有 MockProvider）
2. 测试时间敏感性：每秒触发的 job，CI 环境下 2 秒超时是否稳定？
3. 测试文件放在 `src/routines/mod.rs` 的 `#[cfg(test)]` 还是 `tests/` 独立文件？

**初步倾向**：方案3（mock provider）+ 独立 `tests/` 文件，标注 `#[ignore]` 防止 CI 慢。

**待决定后再实现**。

### A2：Agent Loop E2E

**目标**：mock HTTP 层（不打真实 API），验证完整的 Provider → Agent → Tool 链路。

**需要设计的问题**：
1. HTTP mock 库选型：`httpmock` vs `wiremock` vs `mockito`
   - `httpmock`：API 简单，适合点对点验证
   - `wiremock`：功能更强，但稍复杂
   - 倾向：`httpmock`（轻量，一个 dev dep）
2. 测试覆盖的场景：
   - 场景1：纯文本回复（无 tool call）
   - 场景2：一次 tool call → tool result → 最终回复
   - 场景3：streaming 模式（SSE）
3. 是否需要测试 ClaudeProvider（Anthropic API 格式不同）？
4. 测试文件组织：`tests/e2e_agent.rs`？

**依赖**：需引入 `httpmock = "0.7"` 作为 dev dependency。

**待 A1 完成后再详细设计 A2**。

---

## 验收标准

### 任务 B 完成标志
- [ ] 所有 9 个 Claude.md 文件反映当前代码实际状态
- [ ] `src/mcp/Claude.md` 创建完成
- [ ] 每个文件包含"已踩的坑"和"测试要求"章节

### 任务 A 完成标志（后续计划）
- [ ] A1：scheduler 集成测试通过，不 mock scheduler
- [ ] A2：agent loop E2E 测试通过，覆盖 tool call 场景
- [ ] 两个测试都在 CI 环境下稳定运行
