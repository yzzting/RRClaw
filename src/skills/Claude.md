# Skills 模块设计文档

管理 Skills 系统：加载、解析、合并来自多个目录的 skill 定义文件（SKILL.md），
向 Agent 提供 L1 元数据（注入 system prompt）和按需加载的 L2 完整指令。

## 核心概念

**Skills 本质是 prompt 工程包**，不是可执行代码。它们教 LLM 何时、如何组合使用
现有 Tools（shell/file_read 等）完成复杂工作流。

## 三级加载模型

| 级别 | 时机 | 内容 |
|------|------|------|
| L1 元数据 | 启动时，常驻 system prompt | name + description |
| L2 指令 | 按需（LLM 调用 skill 工具 或 /skill load <name>） | SKILL.md 正文 |
| L3 资源 | LLM 按需用 file_read 读取 | 附带文件、脚本 |

## 文件格式（Anthropic Agent Skills 标准）

每个 skill 是一个目录，包含 `SKILL.md`：

```markdown
---
name: code-review
description: 代码审查工作流。当用户要求 review 代码时使用。
tags: [dev, review]
---

# 正文指令...
```

name 格式: `^[a-z0-9][a-z0-9-]*$`，最长 64 字符

## 目录优先级（高 → 低）

1. `<workspace>/.rrclaw/skills/` — 项目级
2. `~/.rrclaw/skills/` — 用户全局
3. 内置 skills（`include_str!` 编译时嵌入）

同名 skill，高优先级覆盖低优先级。

## 数据结构

```rust
pub enum SkillSource { BuiltIn, Global, Project }

pub struct SkillMeta {
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    pub source: SkillSource,
    pub path: Option<PathBuf>,  // 内置 skill 为 None
}

pub struct SkillContent {
    pub meta: SkillMeta,
    pub instructions: String,   // SKILL.md 正文
    pub resources: Vec<String>, // 目录下其他文件名（L3 提示）
}
```

## 公开 API

```rust
pub fn builtin_skills() -> Vec<SkillMeta>
pub fn load_skills(workspace_dir, global_dir, builtin) -> Vec<SkillMeta>
pub fn load_skill_content(name, skills) -> Result<SkillContent>
pub fn validate_skill_name(name) -> Result<()>

// 内部
fn parse_skill_md(content) -> Result<(name, description, tags, body)>
fn scan_skills_dir(dir, source) -> Vec<SkillMeta>
fn list_resources(dir) -> Vec<String>  // L3 资源清单（除 SKILL.md 外的文件）
```

## 内置 Skills（4 个，编译期 `include_str!` 嵌入）

| 名称 | 用途 |
|------|------|
| `code-review` | 代码审查最佳实践 |
| `rust-dev` | Rust 开发规范（clippy、测试、错误处理） |
| `git-commit` | Git 提交工作流（原子提交、消息格式） |
| `mcp-install` | MCP Server 安装助手（npm/GitHub/SSE/本地路径四种方式） |

### mcp-install skill 说明

- **触发场景**：用户要求安装、添加、配置 MCP Server 时
- **核心能力**：根据输入自动判断 MCP 类型（`@org/package` / GitHub URL / SSE URL / 本地路径），生成对应安装步骤和 config.toml 配置片段
- **配置写入**：使用 `config` 工具的 `append` action（不用 shell `cat >>`，因 workspace_only 限制）
- **注意**：配置写入后需重启 RRClaw 才能加载新 MCP

## 文件结构

```
src/skills/
├── Claude.md       # 本文件
├── mod.rs          # 全部实现
└── builtin/
    ├── code-review.md
    ├── rust-dev.md
    ├── git-commit.md
    └── mcp-install.md   # P4 新增
```

## 测试要求

- `parse_skill_md`：有效 frontmatter、缺少 `---`、未闭合、缺字段（已有）
- `validate_skill_name`：合法/非法名称（已有）
- `scan_skills_dir`：空目录、有效目录、无 SKILL.md 的目录（已有）
- `load_skills`：三级优先级合并、同名覆盖（已有）
- `builtin_skills`：返回 4 个内置 skill，description 非空（已有）
- `load_skill_content`：内置 skill 加载、文件系统 skill 加载、未知 skill 报错（已有）
