# P7-3: 动态工具 Schema 补充

## 背景

P7-1 和 P7-2 已经大幅减少了发给 LLM 的工具数量，但还有一个问题：

即使 Phase 1 路由到某个工具，发送给 LLM 的也只是**简化版 schema**（只有工具名和一句话描述）。如果 LLM 需要知道**具体参数**，怎么办？

## 核心思路

参考 MCP 的 "protocol" 思路：

```
Phase 2 第 1 轮：
  LLM 收到: "file_read: 读取文件"
  LLM 决定: 调用 file_read，但没有参数 schema
  ↓
  LLM 调用 tool → 工具返回: "参数不完整，需要 path"
  ↓
  Agent 检测到: LLM 调用了某工具但缺参数
  ↓
  第 2 轮：
  自动补充该工具的完整 parameters_schema
  ↓
  LLM 看到完整 schema，正确填充参数
```

## 实现方案

### 方案 A：工具返回"参数缺失"提示

最简单：在工具执行时，如果参数缺失，返回一个特殊错误：

```rust
// src/tools/file.rs

async fn execute(&self, args: serde_json::Value, policy: &SecurityPolicy) -> Result<ToolResult> {
    // 检查必填参数
    let path = args.get("path");

    if path.is_none() {
        return Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some("缺少必填参数: path".to_string()),
            // 关键：携带完整 schema 提示
            config_suggestion: Some(json!({
                "tool": "file_read",
                "missing": ["path"],
                "schema": self.parameters_schema()
            }).to_string()),
        });
    }
    // ...
}
```

### 方案 B：Agent 自动检测+补充

在 tool call 循环中，检测到工具调用但参数不完整时，自动补充 schema：

```rust
// src/agent/loop_.rs

for iteration in 0..MAX_TOOL_ITERATIONS {
    // ... 调用 LLM ...

    // 检测：如果返回的 tool_calls 参数为空或不完整
    for tc in &response.tool_calls {
        let tool = self.tools.iter().find(|t| t.name() == tc.name);

        // 检查参数是否完整
        if let Some(t) = tool {
            let schema = t.parameters_schema();
            let required = schema.get("required")
                .and_then(|r| r.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>());

            let args_obj = tc.arguments.as_object();
            let missing: Vec<_> = required
                .unwrap_or(&[])
                .iter()
                .filter(|r| !args_obj.map(|o| o.contains_key(*r)).unwrap_or(false))
                .collect();

            if !missing.is_empty() {
                // 参数不完整，记录下来
                // 下一轮自动补充完整 schema
                self.pending_tool_schemas.insert(tc.name.clone(), t.spec());
            }
        }
    }
}

// 在 build_system_prompt 时，如果有 pending schemas，一并发送
fn build_system_prompt(&self, ...) -> String {
    // ...

    // 如果有 pending 工具，补中完整 schema
    if !self.pending_tool_schemas.is_empty() {
        prompt.push_str("\n【补充工具 schema】\n");
        for (name, spec) in &self.pending_tool_schemas {
            prompt.push_str(&format!("- {}: {}\n{}\n",
                name, spec.description, spec.parameters));
        }
    }
}
```

## 推荐：方案 A + 方案 B 混合

1. **工具层面**（方案 A）：如果参数确实缺失，返回带 schema 的错误
2. **Agent 层面**（方案 B）：维护一个 `requested_tools` 集合，每轮检查是否需要补充 schema

```rust
// src/agent/loop_.rs

pub struct Agent {
    // ... 现有字段
    /// 已经请求过的工具（用于判断是否需要补充完整 schema）
    requested_tools: HashSet<String>,
    /// 完整 schema 已补充的工具
    expanded_tools: HashSet<String>,
}

impl Agent {
    /// 检查并补充工具 schema
    fn ensure_tool_schema(&mut self, tool_name: &str) -> Option<ToolSpec> {
        // 如果已经扩展过，返回缓存
        if self.expanded_tools.contains(tool_name) {
            return None;
        }

        // 标记为已请求
        self.requested_tools.insert(tool_name.to_string());

        // 找到工具，返回完整 spec
        self.tools.iter()
            .find(|t| t.name() == tool_name)
            .map(|t| {
                self.expanded_tools.insert(tool_name.to_string());
                t.spec()
            })
    }
}
```

## System Prompt 中的补充提示

在工具描述部分增加：

```
【补充说明】
- 如果某个工具的参数不完整，下一轮会自动补充该工具的完整 schema
- 当前已请求的工具: shell, file_read
```

## 改动范围

| 文件 | 改动 |
|------|------|
| `src/agent/loop_.rs` | requested_tools / expanded_tools 状态管理 |
| `src/tools/traits.rs` | 可选：ToolResult 新增 `schema_hint` 字段 |

## 效果

| 轮次 | LLM 看到的工具 | 说明 |
|------|---------------|------|
| 1 | shell (简化), file_read (简化) | Phase 1 路由结果 |
| 2 | shell (完整), file_read (完整) | 补充了完整 schema |
| 3+ | 同上 | 稳定 |

## 与 P7-1/P7-2 的关系

- **P7-1**: MCP 工具懒加载（L1 → L2）
- **P7-2**: 工具分组路由（减少工具数量）
- **P7-3**: 动态补充完整 schema（提高参数准确性）

三者可以独立工作，也可以组合使用：

```
P7-1 + P7-2 + P7-3 = 最优效果
```

## 提交策略

```
feat: add requested_tools tracking in Agent
feat: auto-expand tool schema on first call
test: add tool schema expansion tests
```
