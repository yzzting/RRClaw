# P7: 动态工具加载 — 让 Agent 更"聪明"

> 从"工具箱"到"决策者"：让 Agent 自己判断需要什么工具

## 背景

当前问题：
- 每次给 LLM 18+ 内置工具 + 20+ MCP 工具 = **38+ 个工具 schema**
- Prompt 过长，token 消耗大
- LLM 在大量工具里选错

参考 ZeroClaw 和 Claude Code 的做法：通过**动态加载**让 Agent 更精准。

---

## P7 功能清单

| 编号 | 功能 | 难度 | 效果 |
|------|------|------|------|
| P7-1 | MCP 工具懒加载 | 低 | MCP 工具默认不暴露完整 schema，用到才加载 |
| P7-2 | 工具分组 + Phase 1 路由 | 中 | Phase 1 同时路由技能和工具，只给 LLM 相关的 2-3 个 |
| P7-3 | 动态 Schema 补充 | 中 | LLM 选工具后自动补充完整参数 |

---

## P7-1: MCP 工具懒加载

### 问题

MCP 工具 description 很长，每个几百字符。20 个 MCP 工具 = 几千字符的 prompt。

### 方案

类似 Skills 的 L1/L2：

```
L1: name + 一句话简介（~20 字符）
L2: 完整 description + parameters（几百字符）

默认只加载 L1
用到时（用户明确说"用 MCP"或 LLM 第一次调用）→ 加载 L2
```

### 预期效果

| 场景 | 改动前 | 改动后 |
|------|--------|--------|
| 用户只问"你好" | 38 个工具 | 18 个内置工具 |
| 用户说"读写文件" | 38 个 | 18 个 + MCP filesystem L1 |

---

## P7-2: 工具分组 + Phase 1 路由

### 问题

即使 MCP 懒加载，内置工具还是有 18 个。用户每次输入其实只需要 2-3 个。

### 方案

扩展现有 Phase 1，同时路由**技能**和**工具**：

```
用户输入 → Phase 1.5: 工具路由
  ├── "改代码" → [file, shell, git]
  ├── "查天气" → [http_request]
  ├── "记事情" → [memory_*]
  └── 模糊 → 所有工具（降级）

Phase 2: 只给 LLM 这 2-3 个工具的 schema
```

### 工具分组定义

| 分组 | 关键词 | 包含工具 |
|------|--------|---------|
| file_ops | 文件、读、写、改、edit | file_read, file_write, shell, git |
| web | 请求、HTTP、API、天气 | http_request |
| memory | 记住、记忆、store、recall | memory_store, memory_recall |
| config | 配置、设置、config | config, self_info |
| git_ops | 提交、commit、push、branch | git, shell |
| routine | 定时、schedule、cron | routine |

---

## P7-3: 动态 Schema 补充

### 问题

Phase 1 路由后，工具 schema 还是简化版。LLM 知道要调用 file_read，但不知道具体参数。

### 方案

```
第 1 轮：LLM 收到简化 schema (file_read: 读取文件)
      ↓ LLM 调用但缺参数
第 2 轮：自动补充 file_read 的完整 parameters_schema
      ↓ LLM 看到完整参数，正确填充
后续轮次：使用完整 schema
```

实现方式：
1. 工具执行时如果缺参数，返回带完整 schema 的错误提示
2. Agent 跟踪 `requested_tools`，每轮检查是否需要补充

---

## 三步的关系

```
P7-1: MCP 懒加载        → 减少 MCP 工具数量
P7-2: 工具分组路由     → 减少内置工具数量
P7-3: 动态 Schema      → 提高参数准确性

可以独立使用，推荐组合：
P7-1 + P7-2 + P7-3 = 最优效果
```

---

## 改动范围汇总

| 文件 | P7-1 | P7-2 | P7-3 |
|------|-------|-------|-------|
| `src/mcp/tool.rs` | ✅ L1/L2 分离 | - | - |
| `src/mcp/mod.rs` | ✅ tools_l1() | - | - |
| `src/agent/tool_groups.rs` | - | ✅ 新增 | - |
| `src/agent/loop_.rs` | ✅ 区分 MCP/内置 | ✅ Phase 1 扩展 | ✅ requested_tools |
| `src/agent/mod.rs` | - | ✅ pub mod | - |

---

## 文档链接

- [P7-1: MCP 懒加载详细设计](p7-mcp-lazy-loading.md)
- [P7-2: 工具分组路由详细设计](p7-tool-routing.md)
- [P7-3: 动态 Schema 补充详细设计](p7-dynamic-schema.md)
