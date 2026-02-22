# P7 E2E 测试设计

> P7 动态工具加载的 E2E 测试用例设计

---

## 测试框架概述

基于 `tests/common/mock_provider.rs` 和 `tests/common/mod.rs`：
- `MockProvider` 预置响应队列，每次调用 pop 一个响应
- `test_agent()` 创建带 MockProvider 的 Agent
- Phase 1 路由 → Phase 2 执行，每轮迭代记录在 history

---

## P7-1: MCP 工具懒加载测试

### 测试目标
- 验证 MCP 工具默认只加载 L1（name + 一句话简介）
- 验证首次调用后自动加载 L2（完整 description + parameters）

### 测试用例

#### E2E-P7-1-1: L1 Schema 只包含简化信息

```rust
// 用户说"你好" → 不需要 MCP 工具
// Phase 1 路由返回 direct: true
// Phase 2 只应收到内置工具，不应有 MCP filesystem L1
#[tokio::test]
async fn e2_p7_1_1_l1_schema_no_mcp_tools() {
    let tmp = tempfile::tempdir().unwrap();
    let mock = MockProvider::new(vec![
        MockProvider::direct_route(),
        MockProvider::text("你好"),
    ]);
    let mut agent = common::test_agent(mock, common::full_policy(tmp.path()));

    // 发送不涉及 MCP 的消息
    let result = agent.process_message("你好").await.expect("process_message 失败");

    // 验证：Phase 2 调用时 tools 参数不包含 MCP 工具
    // （通过检查 MockProvider 收到的 tools 长度）
    // 内置工具约 11 个，不应有 MCP filesystem
}
```

#### E2E-P7-1-2: 首次调用 MCP 工具后加载 L2

```rust
// 用户说"读写文件" → 需要 MCP filesystem
// Phase 1 路由: direct: true + mcp_tools: ["filesystem"]
// Phase 2: LLM 调用 file_read（缺参数）→ 返回错误 + L2 schema
// Phase 3: LLM 补充参数 → 执行成功
#[tokio::test]
async fn e2_p7_1_2_mcp_l2_loaded_after_first_call() {
    let tmp = tempfile::tempdir().unwrap();

    // 写入测试文件
    std::fs::write(tmp.path().join("test.txt"), "hello").unwrap();

    // Phase 1: 返回 direct + mcp_tools 提示
    // Phase 2: file_read 缺 path 参数 → 返回错误 + 完整 L2 schema
    // Phase 3: file_read 补全参数 → 执行成功
    let mock = MockProvider::new(vec![
        // Phase 1 路由
        MockProvider::text(r#"{"direct": true, "mcp_tools": ["filesystem"]}"#),
        // Phase 2: file_read 缺参数
        MockProvider::tool_call("tc-1", "file_read", serde_json::json!({})),
        // Phase 3: 补充完整参数
        MockProvider::tool_call("tc-2", "file_read", serde_json::json!({"path": "test.txt"})),
        // Phase 4: 最终回复
        MockProvider::text("文件内容: hello"),
    ]);
    let mut agent = common::test_agent_with_file_tool(mock, common::full_policy(tmp.path()));

    let result = agent.process_message("读取 test.txt 文件").await.expect("失败");

    // 验证：第二轮 Phase 2 应收到完整 L2 schema
    // ToolResult 应包含完整 parameters_schema 提示
}
```

---

## P7-2: 工具分组 + Phase 1 路由测试

### 测试目标
- 验证 Phase 1 能根据关键词路由到正确的工具分组
- 验证 Phase 1 返回格式包含 `tools` 字段

### 测试用例

#### E2E-P7-2-1: 文件操作路由到 file_ops 分组

```rust
// 用户说"改代码" → 应路由到 [file_read, file_write, shell, git]
#[tokio::test]
async fn e2_p7_2_1_file_ops_routing() {
    let tmp = tempfile::tempdir().unwrap();

    // Phase 1: 路由到 file_ops
    let route_response = rrclaw::providers::ChatResponse {
        text: Some(r#"{"direct": false, "skills": [], "tools": ["file_ops"]}"#.to_string()),
        reasoning_content: None,
        tool_calls: vec![],
    };

    // Phase 2: 收到 file_ops 相关的工具
    let mock = MockProvider::new(vec![
        route_response,
        MockProvider::shell_call("tc-1", "ls"),
        MockProvider::text("已列出文件"),
    ]);
    let mut agent = common::test_agent(mock, common::full_policy(tmp.path()));

    let result = agent.process_message("改代码").await.expect("失败");

    // 验证：Phase 2 的 tools 参数应只包含 file_ops 相关工具
    // 不应有 memory_store, http_request 等无关工具
}
```

#### E2E-P7-2-2: 记忆操作路由到 memory 分组

```rust
// 用户说"记住这件事" → 应路由到 [memory_store, memory_recall]
#[tokio::test]
async fn e2_p7_2_2_memory_routing() {
    let tmp = tempfile::tempdir().unwrap();

    let route_response = rrclaw::providers::ChatResponse {
        text: Some(r#"{"direct": false, "skills": [], "tools": ["memory"]}"#.to_string()),
        reasoning_content: None,
        tool_calls: vec![],
    };

    let mock = MockProvider::new(vec![
        route_response,
        MockProvider::tool_call("tc-1", "memory_store", serde_json::json!({"key": "test", "content": "hello"})),
        MockProvider::text("已记住"),
    ]);
    let mut agent = common::test_agent_with_memory_tool(mock, common::full_policy(tmp.path()));

    let result = agent.process_message("记住这件事").await.expect("失败");

    // 验证：Phase 2 只收到 memory 相关工具
}
```

#### E2E-P7-2-3: 模糊输入降级到所有工具

```rust
// 用户说"帮我看看" → 关键词不明确 → 降级到所有工具
#[tokio::test]
async fn e2_p7_2_3_fuzzy_input_falls_back_to_all() {
    let tmp = tempfile::tempdir().unwrap();

    // Phase 1 返回 direct: true（降级）
    let mock = MockProvider::new(vec![
        MockProvider::direct_route(),
        MockProvider::text("好的"),
    ]);
    let mut agent = common::test_agent(mock, common::full_policy(tmp.path()));

    let result = agent.process_message("帮我看看").await.expect("失败");

    // 验证：Phase 2 收到所有工具
}
```

---

## P7-3: 动态 Schema 补充测试

### 测试目标
- 验证工具首次调用缺参数时，返回完整 schema 提示
- 验证第二轮自动补充完整 parameters_schema

### 测试用例

#### E2E-P7-3-1: 缺参数时返回完整 schema

```rust
// Phase 1 路由 → Phase 2: LLM 调用 shell 但缺参数
// → ToolResult 返回错误 + 完整 parameters_schema
// → Phase 3: LLM 补充参数 → 执行成功
#[tokio::test]
async fn e2_p7_3_1_schema_supplemented_after_missing_params() {
    let tmp = tempfile::tempdir().unwrap();

    // Phase 1: direct
    // Phase 2: shell 缺参数 {} → 返回错误 + schema
    // Phase 3: shell 补充 command 参数 → 成功
    // Phase 4: 最终回复
    let mock = MockProvider::new(vec![
        MockProvider::direct_route(),
        MockProvider::shell_call("tc-1", "echo hello"), // 缺参数
        MockProvider::shell_call("tc-2", "echo hello"), // 补充参数
        MockProvider::text("命令执行完成"),
    ]);
    let mut agent = common::test_agent(mock, common::full_policy(tmp.path()));

    let result = agent.process_message("执行 echo").await.expect("失败");

    // 验证 history：第二轮 ToolResult 应包含完整 schema 提示
    let history = agent.history();
    // ToolResult 应包含类似: "缺少 command 参数，请参考完整 schema: {...}"
}
```

#### E2E-P7-3-2: 跟踪 requested_tools 避免重复补充

```rust
// 同一工具第二轮不应重复补充 schema
#[tokio::test]
async fn e2_p7_3_2_no_duplicate_schema_supplement() {
    let tmp = tempfile::tempdir().unwrap();

    // Phase 1: direct
    // Phase 2: shell 缺参数 → 返回 schema
    // Phase 3: shell 补充参数 → 执行成功
    // Phase 4: shell 再次调用 → 不再返回 schema（已补充过）
    let mock = MockProvider::new(vec![
        MockProvider::direct_route(),
        MockProvider::shell_call("tc-1", "echo a"),
        MockProvider::shell_call("tc-2", "echo b"),
        MockProvider::shell_call("tc-3", "echo c"),
        MockProvider::text("完成"),
    ]);
    let mut agent = common::test_agent(mock, common::full_policy(tmp.path()));

    let result = agent.process_message("执行多个命令").await.expect("失败");

    // 验证：只有第一轮缺参数时返回 schema，后续不再重复
}
```

---

## 辅助函数设计

需要新增的测试辅助函数：

```rust
/// 创建包含 ShellTool + FileReadTool + MemoryStoreTool 的测试 Agent
pub fn test_agent_with_memory_tool(mock: MockProvider, policy: SecurityPolicy) -> Agent {
    Agent::new(
        Box::new(mock),
        vec![
            Box::new(rrclaw::tools::shell::ShellTool),
            Box::new(rrclaw::tools::file::FileReadTool),
            Box::new(rrclaw::tools::memory::MemoryStoreTool),
        ],
        Box::new(NoopMemory),
        policy,
        // ... 其他参数
    )
}
```

---

## 测试执行顺序

建议按依赖顺序执行：

1. **P7-2 先行** — 工具分组路由是基础，Phase 1 输出格式确定后 P7-1 和 P7-3 才能测试
2. **P7-1 其次** — MCP 懒加载依赖 Phase 1 返回的 `mcp_tools` 字段
3. **P7-3 最后** — 动态 Schema 补充在工具路由之后验证

---

## 与现有测试的关系

| 现有测试 | P7 新增 |
|---------|---------|
| E2-1 ~ E2-9 | 新增 E2-P7-1-x, E2-P7-2-x, E2-P7-3-x |
| Phase 1 只路由 skills | Phase 1 同时路由 tools |
| tools 参数固定 11 个内置 | tools 参数动态变化（L1/L2、分组） |

现有 `e2e_agent.rs` 不需要修改，只追加新的测试用例。
