# P7-1: MCP 工具懒加载（Tool Lazy Loading）

## 背景

当前每次 Agent 调用都把**所有**工具的 schema 发送给 LLM，包括：
- 18 个内置工具
- 20+ MCP 工具（每个工具的 description + parameters 可能几百字符）

问题：
- **Prompt 过长**：从 38 个工具 → token 消耗大
- **LLM 决策困难**：在大量工具里选错
- **MCP 工具尤其冗余**：一个 MCP server 可能暴露 20 个工具，但用户可能只用其中 1-2 个

## 核心思路

类似 Skills 的 L1/L2/L3 分级，MCP 工具也采用**懒加载**：

```
注册时：
  - L1: 只存 name + 一句话简介（~20 字符）
  - L2: 完整 description + parameters（几百字符）
  - 默认不加载 L2

Phase 1 路由时：
  - 可选：路由到特定 MCP server

真正需要时：
  - 用户明确说"用 MCP" → 加载该 MCP server 的完整工具列表
  - 或 LLM 第一次调用某 MCP 工具后，后续迭代自动补充完整 schema
```

## 当前 MCP 工具注册方式（已存在）

在 `src/mcp/mod.rs` 中，`McpManager::tools()` 返回 `Vec<Box<dyn Tool>>`：

```rust
// 当前：每次都返回完整工具
pub async fn tools(&self) -> Vec<Box<dyn Tool>> {
    for server in &self.servers {
        for tool_def in tools_result.tools {
            result.push(Box::new(McpTool::new(&server.name, tool_def, client.clone())));
        }
    }
}
```

## 改动设计

### 1. McpTool 扩展 L1/L2 结构

```rust
// src/mcp/tool.rs

/// MCP 工具的懒加载包装器
pub struct McpTool {
    /// 工具在 RRClaw 中的名称
    prefixed_name: String,
    /// L1: 一句话简介（用于工具选择阶段）
    short_description: String,
    /// L2: 完整描述（懒加载）
    full_description: Option<String>,
    /// L2: 完整参数 schema（懒加载）
    parameters_schema_full: Option<serde_json::Value>,
    /// MCP tool 原始定义
    def: McpToolDef,
    original_name: String,
    client: Arc<rmcp::Client>,
    /// 是否已加载完整 schema
    loaded: bool,
}

impl McpTool {
    /// 创建时只加载 L1
    pub fn new_l1(server_name: &str, def: McpToolDef, client: Arc<rmcp::Client>) -> Self {
        let short_description = format!(
            "MCP {}: {}",
            server_name,
            def.description.as_deref().unwrap_or("MCP tool")
        );
        Self {
            prefixed_name: format!("mcp_{}_{}", server_name, def.name),
            short_description,
            full_description: None,
            parameters_schema_full: None,
            def,
            original_name: def.name.to_string(),
            client,
            loaded: false,
        }
    }

    /// 懒加载 L2（首次调用时触发）
    pub fn load_full(&mut self) {
        if self.loaded { return; }

        self.full_description = self.def.description.clone();
        self.parameters_schema_full = self.def.input_schema
            .as_ref()
            .map(|s| serde_json::to_value(s).ok())
            .flatten();
        self.loaded = true;
    }
}

impl Tool for McpTool {
    fn name(&self) -> &str { &self.prefixed_name }

    fn description(&self) -> &str {
        // L1 模式：只返回一句话简介
        &self.short_description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        // L1 模式：返回简化 schema（只有必填参数名）
        if self.loaded {
            self.parameters_schema_full.clone().unwrap_or_else(|| json!({
                "type": "object",
                "properties": {}
            }))
        } else {
            // 懒加载阶段：返回极简 schema，触发 LLM 后续调用
            json!({
                "type": "object",
                "properties": {
                    "__lazy": {
                        "type": "object",
                        "description": "（此工具正在加载完整参数...）"
                    }
                }
            })
        }
    }

    // load_full() 被调用时更新 description
}
```

### 2. McpManager 支持懒加载模式

```rust
// src/mcp/mod.rs

pub struct McpManager {
    servers: Vec<McpServer>,
    /// 是否启用懒加载（默认 true）
    lazy_loading: bool,
}

impl McpManager {
    /// 返回 L1 工具列表（懒加载模式）
    pub async fn tools_l1(&self) -> Vec<Box<dyn Tool>> {
        // 每个 McpTool 只创建 L1 版本
    }

    /// 加载指定 MCP server 的完整工具（显式加载）
    pub async fn load_server_tools(&self, server_name: &str) -> Vec<Box<dyn Tool>> {
        // 返回完整 L2 版本
    }
}
```

### 3. Agent 集成

```rust
// src/agent/loop_.rs

// 改动 build_system_prompt() 中的工具描述部分
fn build_system_prompt(&self, ...) -> String {
    // ...

    // 工具描述：区分内置工具和 MCP 工具
    let mut tools_desc = "你可以使用以下工具:\n".to_string();

    // 内置工具：完整描述
    for tool in &self.tools {
        if !tool.name().starts_with("mcp_") {
            tools_desc.push_str(&format!("- {}: {}\n", tool.name(), tool.description()));
        }
    }

    // MCP 工具：L1 简介 + 提示
    let mcp_tools: Vec<_> = self.tools.iter()
        .filter(|t| t.name().starts_with("mcp_"))
        .collect();

    if !mcp_tools.is_empty() {
        tools_desc.push_str("\n[MCP 工具]（需要时可用）:\n");
        for tool in mcp_tools {
            tools_desc.push_str(&format!("- {}\n", tool.description())); // L1 简介
        }
        tools_desc.push_str("\n提示：如需使用某个 MCP 工具但参数不完整，下一轮迭代会自动补充完整 schema。\n");
    }

    // ...
}
```

### 4. 懒加载触发机制

两种方案：

**方案 A：自动触发**（推荐）
```rust
// 工具执行循环中
for tc in &response.tool_calls {
    // 找到对应的 MCP 工具，触发懒加载
    if let Some(tool) = self.tools.iter_mut().find(|t| t.name() == tc.name) {
        if let Some(mcp_tool) = tool.as_any().downcast_mut::<McpTool>() {
            mcp_tool.load_full();
        }
    }
    // 下一次 build_system_prompt() 就会用完整 schema
}
```

**方案 B：Skill 驱动**
用户说"帮我用 MCP filesystem 读写文件" → 加载对应 MCP server 的完整工具

## 改动范围

| 文件 | 改动 |
|------|------|
| `src/mcp/tool.rs` | McpTool 支持 L1/L2 分离，load_full() 方法 |
| `src/mcp/mod.rs` | McpManager 新增 tools_l1() 方法 |
| `src/agent/loop_.rs` | build_system_prompt() 区分内置/MCP 工具 |

## 预期效果

| 场景 | 改动前 | 改动后 |
|------|--------|--------|
| 用户只问"你好" | 38 个工具 schema | ~18 个内置工具 |
| 用户说"读写文件" | 38 个工具 | 18 个 + MCP filesystem L1 |
| MCP 工具第一次调用 | 完整 schema | L1 → 触发加载 L2 → 下轮完整 |

## 提交策略

```
feat: add McpTool lazy loading support (L1/L2)
feat: add McpManager::tools_l1() for lazy mode
feat: distinguish MCP tools in system prompt
test: add lazy loading unit tests
```
