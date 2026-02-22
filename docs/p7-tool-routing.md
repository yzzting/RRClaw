# P7-2: 工具分组 + Phase 1 路由

## 背景

P7-1 解决了 MCP 工具过多的问题，但还有改进空间：
- 内置工具也有 18 个
- 用户每次输入其实只需要 2-3 个工具

Phase 1 已经能做 skill 路由，可以用类似的思路**路由工具**。

## 核心思路

```
用户输入
    │
    ▼
┌─────────────────────────────────────┐
│  Phase 1.5: 工具路由                │
│  根据用户意图，决定需要哪些工具组   │
└─────────────────────────────────────┘
    │
    ├── "改代码" → [file, shell, git]
    ├── "查天气" → [http_request]
    ├── "记事情" → [memory_store, memory_recall]
    ├── "配 RRClaw" → [config, self_info]
    └── 模糊 → 所有工具（降级）
    │
    ▼
Phase 2: 只给 LLM 这几个组的工具
```

## 工具分组设计

### 预定义工具组

```rust
// src/agent/tool_groups.rs

/// 工具分组定义
pub struct ToolGroup {
    /// 分组名称（用于日志）
    pub name: &'static str,
    /// 匹配关键词（简单匹配，后续可用 LLM 路由）
    pub keywords: Vec<&'static str>,
    /// 包含的工具名（精确匹配）
    pub tools: Vec<&'static str>,
}

pub const TOOL_GROUPS: &[ToolGroup] = &[
    ToolGroup {
        name: "file_ops",
        keywords: &["文件", "读", "写", "改", "编辑", "查看代码", "read", "write", "edit", "file"],
        tools: &["file_read", "file_write", "shell", "git"],
    },
    ToolGroup {
        name: "web",
        keywords: &["请求", "HTTP", "API", "天气", "查询", "网络", "http", "request", "fetch", "api"],
        tools: &["http_request"],
    },
    ToolGroup {
        name: "memory",
        keywords: &["记住", "记忆", "存储", "recall", "store", "memory", "记得"],
        tools: &["memory_store", "memory_recall", "memory_forget"],
    },
    ToolGroup {
        name: "config",
        keywords: &["配置", "设置", "config", "RRClaw", "帮助", "help", "info"],
        tools: &["config", "self_info"],
    },
    ToolGroup {
        name: "git_ops",
        keywords: &["提交", "commit", "push", "pull", "分支", "branch", "git", "版本"],
        tools: &["git", "shell"],
    },
    ToolGroup {
        name: "mcp",
        keywords: &["mcp", "外部", "plugin"],
        tools: &[], // MCP 工具单独处理
    },
    ToolGroup {
        name: "routine",
        keywords: &["定时", "任务", "routine", "schedule", "cron"],
        tools: &["routine"],
    },
];

/// 根据用户输入，返回应该暴露的工具名列表
pub fn route_tools(user_input: &str) -> Vec<String> {
    let input_lower = user_input.to_lowercase();
    let mut matched_groups: Vec<&ToolGroup> = Vec::new();

    // 1. 关键词匹配
    for group in TOOL_GROUPS {
        for kw in &group.keywords {
            if input_lower.contains(&kw.to_lowercase()) {
                matched_groups.push(group);
                break;
            }
        }
    }

    if matched_groups.is_empty() {
        // 2. 降级：返回所有工具（当前行为）
        return vec![];
    }

    // 3. 收集工具名（去重）
    let mut tools = Vec::new();
    for group in matched_groups {
        for tool in &group.tools {
            if !tools.contains(tool) {
                tools.push(tool.to_string());
            }
        }
    }

    // 4. 如果有 MCP 关键词，添加所有 MCP 工具
    if input_lower.contains("mcp") || input_lower.contains("外部") {
        // 从 tools 列表中找出所有 mcp_ 开头的
        // 这需要在 Agent 中传递 MCP 工具列表
    }

    tools
}
```

## Phase 1 扩展

现有的 `RouteResult` 扩展：

```rust
// src/agent/loop_.rs

#[derive(Debug, Clone)]
pub enum RouteResult {
    Skills(Vec<String>),           // 现有
    Tools(Vec<String>),            // 新增：路由到特定工具
    Direct,                       // 现有
    NeedClarification(String),    // 现有
}
```

```rust
// 扩展 build_routing_prompt
fn build_routing_prompt(skills: &[SkillMeta]) -> String {
    let mut prompt = "你是 RRClaw 路由助手。分析用户输入，决定需要加载哪些技能和工具。\n\n".to_string();

    // ... skill 已有部分 ...

    // 新增：工具分组说明
    prompt.push_str("【工具分组】（选择最相关的 1-2 组）\n");
    prompt.push_str("- 文件操作: 读、写、编辑代码\n");
    prompt.push_str("- 网络请求: HTTP API 调用\n");
    prompt.push_str("- 记忆: 存储/检索信息\n");
    prompt.push_str("- 配置: RRClaw 设置\n");
    prompt.push_str("- Git: 版本控制\n");
    prompt.push_str("- 定时任务: schedule/cron\n\n");

    prompt.push_str("【输出格式】\n");
    prompt.push_str(r#"{"skills": [], "tools": ["file_ops", "git_ops"], "direct": false}
# 或只有 skills: {"skills": ["rust-dev"], "tools": [], "direct": false}
# 或不需要任何工具: {"skills": [], "tools": [], "direct": true}
"#);
    prompt
}
```

## Agent 集成

```rust
// src/agent/loop_.rs

pub struct Agent {
    // ... 现有字段
    /// 所有可用工具（完整列表）
    all_tools: Vec<Box<dyn Tool>>,
    /// 当前轮次启用的工具（Phase 1 路由结果）
    active_tools: Vec<Box<dyn Tool>>,
}

impl Agent {
    /// Phase 1.5: 路由工具
    fn route_tools(&self, user_input: &str) -> Vec<String> {
        crate::agent::tool_groups::route_tools(user_input)
    }

    /// 根据 active_tools 构建 system prompt
    fn build_prompt_with_tools(&self, ...) -> String {
        // 只把 active_tools 的 schema 发给 LLM
    }
}
```

## 改动范围

| 文件 | 改动 |
|------|------|
| `src/agent/tool_groups.rs` | **新增**：工具分组定义 + 路由函数 |
| `src/agent/loop_.rs` | Phase 1 扩展、build_system_prompt() 按 active_tools 构建 |
| `src/agent/mod.rs` | `pub mod tool_groups;` |

## 效果

| 用户输入 | 路由结果 | 工具数量 |
|----------|-----------|---------|
| "帮我改一下这个函数" | file_ops, git | 4 个 |
| "今天的天气怎么样" | web | 1 个 |
| "启动一个每分钟的定时任务" | routine | 1 个 |
| "你好" | 降级所有 | ~18 个 |

## 提交策略

```
feat: add tool groups definition and routing
feat: extend Phase 1 to route tools
feat: build system prompt with active tools only
test: add tool routing unit tests
```
