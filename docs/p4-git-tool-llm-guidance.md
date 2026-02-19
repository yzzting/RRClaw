# P4-GitTool-工具选择优化计划

## 背景

GitTool 已实现并注册到工具列表，但 LLM 仍然使用 ShellTool 执行 git 命令，没有使用专门的 GitTool。

---

## 一、问题分析

### 1.1 现象

用户要求执行 `git log` 时，LLM 选择用 shell 工具执行 `git log` 命令，而不是使用 GitTool。

### 1.2 原因

**Prompt 方案的局限性**：
- 在 system prompt 中写规则无法保证 LLM 遵守
- LLM 的随机性导致每次行为不可控
- 工具描述过长会导致 prompt 膨胀

---

## 二、解决方案：Agent 层自动路由 + Skill 工作流

### 2.1 核心思路

| 层级 | 职责 | 随机性 |
|------|------|--------|
| Agent 层 | 自动路由（检测关键字，强制选择专用工具） | 0% |
| Skill 层 | 复杂场景的工具选择工作流 | 中 |

### 2.2 方案设计

#### 阶段 1：Agent 层自动路由（推荐优先实现）

在 Agent 代码层面做关键字检测，自动选择专用工具：

```rust
// Agent 选择工具时的预处理
fn select_tool(user_input: &str, available_tools: &[Box<dyn Tool>]) -> Option<&Box<dyn Tool>> {
    let input_lower = user_input.to_lowercase();

    // 检测 git 相关操作
    if input_lower.contains("git")
        && !input_lower.contains("github")  // 排除 GitHub CLI
    {
        return available_tools.iter().find(|t| t.name() == "git");
    }

    None  // 返回 None 表示让 LLM 自行选择
}
```

**优点**：
- 确定性 100%，不依赖 LLM 选择
- 实现简单，改动可控

#### 阶段 2：创建 tool-selector Skill

创建通用工具选择 skill，处理更复杂的场景：

```
# Skill: tool-selector

## 触发条件
当需要选择工具但不确定使用哪个时

## 工作流
1. 分析用户意图
2. 列出可用工具
3. 根据意图匹配合适的工具
4. 说明为什么选择这个工具
5. 执行

## Git 场景优先规则
- 用户输入包含 git 操作 → 优先使用 git 工具
- git 工具提供安全保护（force push/checkout 拦截）
```

---

## 三、实现计划

### 3.1 阶段 1：Agent 层自动路由

| 文件 | 改动 |
|------|------|
| `src/agent/loop_.rs` | 添加 `pre_select_tool` 预处理函数 |
| `src/tools/git.rs` | 优化 description |

**改动详情**：

```rust
// src/agent/loop_.rs - 新增函数
/// 预处理用户输入，尝试自动路由到专用工具
/// 返回 Some(tool) 表示强制使用该工具，None 表示让 LLM 自行选择
fn pre_select_tool(&self, user_input: &str) -> Option<&Box<dyn Tool>> {
    let input_lower = user_input.to_lowercase();

    // 检测 git 操作
    if input_lower.contains("git ") || input_lower.starts_with("git ") {
        if let Some(tool) = self.tools.iter().find(|t| t.name() == "git") {
            debug!("自动路由到 git 工具");
            return Some(tool);
        }
    }

    None
}
```

### 3.2 阶段 2：创建 tool-selector Skill

| 文件 | 改动 |
|------|------|
| `skills/tool-selector/SKILL.md` | 新建 skill 目录和定义文件 |

---

## 四、测试用例

```rust
#[test]
fn pre_select_tool_routes_git_to_git_tool() {
    // 用户输入包含 git 命令
    assert!(agent.pre_select_tool("执行 git log").is_some());
    assert!(agent.pre_select_tool("git status 怎么样").is_some());
}

#[test]
fn pre_select_tool_allows_llm_for_other() {
    // 普通命令让 LLM 自行选择
    assert!(agent.pre_select_tool("列出当前目录文件").is_none());
}
```

---

## 五、提交策略

| # | 提交 | 说明 |
|---|------|------|
| 1 | `feat: add tool pre-select routing in Agent` | Agent 层自动路由 |
| 2 | `feat: create tool-selector skill` | 创建通用工具选择 skill |
| 3 | `test: add tool routing tests` | 添加测试 |

---

## 六、风险评估

| 风险 | 等级 | 缓解 |
|------|------|------|
| 关键字检测误判 | 低 | 仅检测明确场景，其他交给 LLM |
| 路由逻辑复杂化 | 中 | 保持简单，仅处理明确场景 |
