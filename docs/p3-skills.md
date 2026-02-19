# P3: Skills 系统实现计划

## 背景

P0-P2 全部完成（87 commits, 104 tests passing, clippy 零警告）。
当前 Agent 有 5 个原子工具（shell/file_read/file_write/config/self_info），但缺乏**高级工作流编排**——用户无法教 Agent "如何做代码审查"或"如何部署项目"这类多步骤任务。

**Skills 的本质**：不是可执行代码，而是 **prompt 工程包**——教 LLM 何时、如何组合使用现有 Tools 完成复杂工作流。

参考：Anthropic Agent Skills 开放标准（Claude Code / OpenClaw 均采用），ZeroClaw 的 SKILL.toml 格式。

---

## 一、架构设计

### 1.1 三级渐进加载（Progressive Disclosure）

避免 system prompt 膨胀，按需加载：

| 级别 | 加载时机 | Token 开销 | 内容 |
|------|---------|-----------|------|
| **L1 元数据** | 启动时，始终在 system prompt | ~30 token/skill | name + description |
| **L2 指令** | LLM 调用 `skill` 工具 或 用户输入 `/skill <name>` | <2000 token | SKILL.md 正文 |
| **L3 资源** | LLM 按需用 file_read 读取 | 无上限 | 附带文件、脚本、模板 |

### 1.2 触发方式：双模式共存

**模式 A：LLM 自动触发**
- LLM 从 system prompt 的 `[可用技能]` 段自动判断何时调用 `skill` 工具
- 用户自然语言描述需求（如"帮我 review 代码"），LLM 自动匹配并加载

**模式 B：用户手动 `/skill` 斜杠命令**

| 子命令 | 功能 | 说明 |
|--------|------|------|
| `/skill` | 列出全部技能 | 显示 name + description + 来源（内置/全局/项目） |
| `/skill <name>` | 加载技能 | 读取 L2 指令注入当前对话，LLM 下一轮遵循 |
| `/skill new <name>` | 创建技能 | 在 `~/.rrclaw/skills/<name>/` 生成 SKILL.md 模板 |
| `/skill edit <name>` | 编辑技能 | 用 `$EDITOR`（默认 vi）打开 SKILL.md |
| `/skill delete <name>` | 删除技能 | 删除技能目录（带 `[y/N]` 确认），内置技能不可删除 |
| `/skill show <name>` | 查看技能内容 | 打印 SKILL.md 全文（不注入对话） |

### 1.3 目录优先级（3 级）

1. `<workspace>/.rrclaw/skills/` — 项目级（最高优先）
2. `~/.rrclaw/skills/` — 用户全局
3. 内置 skills — 编译时 `include_str!` 嵌入（最低优先）

同名 skill，高优先级覆盖低优先级。

---

## 二、SKILL.md 文件格式

兼容 Anthropic Agent Skills 标准。每个 skill 是一个目录，必须包含 `SKILL.md`：

```
~/.rrclaw/skills/
  code-review/
    SKILL.md          # 必须
    checklist.md      # 可选，L3 资源
  deploy/
    SKILL.md
    scripts/
      deploy.sh       # 可选，L3 脚本
```

**SKILL.md 格式**——YAML frontmatter + Markdown 正文：

```markdown
---
name: code-review
description: 代码审查工作流。检查代码质量、安全性、潜在 bug。当用户要求 review 或审查代码时使用。
tags: [dev, review]
---

# 代码审查

## 步骤
1. 用 file_read 读取目标文件
2. 检查以下维度：
   - 代码风格和可读性
   - 潜在 bug 和边界情况
   - 安全漏洞（注入、越权）
   - 测试覆盖率
3. 用 shell 运行 `cargo clippy` 检查
4. 输出结构化审查报告

## 报告格式
- **文件**: 文件路径
- **问题**: 严重/警告/建议
- **描述**: 具体说明
- **建议**: 修复方案
```

**Frontmatter 字段**：
- `name`（必须）：技能名，正则 `^[a-z0-9][a-z0-9-]*$`，最长 64 字符
- `description`（必须）：简短描述 + 触发条件说明，最长 256 字符
- `tags`（可选）：分类标签数组

---

## 三、数据结构与核心函数

### 3.1 数据结构

```rust
// src/skills/mod.rs
use std::path::PathBuf;

/// Skill 来源
#[derive(Debug, Clone, PartialEq)]
pub enum SkillSource {
    BuiltIn,    // 内置（include_str!）
    Global,     // ~/.rrclaw/skills/
    Project,    // <workspace>/.rrclaw/skills/
}

/// Skill 元数据（L1，常驻 system prompt）
#[derive(Debug, Clone)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    pub source: SkillSource,
    pub path: Option<PathBuf>,  // 内置 skill 无 path
}

/// 完整 Skill 内容（L2，按需加载）
#[derive(Debug, Clone)]
pub struct SkillContent {
    pub meta: SkillMeta,
    pub instructions: String,   // SKILL.md 正文（去掉 frontmatter）
    pub resources: Vec<String>, // 目录下其他文件名列表（L3 提示）
}
```

### 3.2 核心函数

```rust
/// 解析 SKILL.md 的 YAML frontmatter
/// 输入: SKILL.md 全文
/// 输出: (SkillMeta 的字段, 正文)
/// frontmatter 用 `---` 分隔，中间是 YAML
pub fn parse_skill_md(content: &str) -> Result<(String, String, Vec<String>, String)>
// 返回 (name, description, tags, body)

/// 扫描目录加载所有 skill 的 L1 元数据
/// 遍历 dir 下每个子目录，查找 SKILL.md，解析 frontmatter
pub fn scan_skills_dir(dir: &Path, source: SkillSource) -> Vec<SkillMeta>

/// 合并多级目录的 skills，高优先级覆盖同名低优先级
pub fn load_skills(
    workspace_dir: &Path,
    global_dir: &Path,
    builtin_skills: Vec<SkillMeta>,
) -> Vec<SkillMeta>

/// 按需加载完整 skill 内容（L2 + L3 文件清单）
pub fn load_skill_content(name: &str, skills: &[SkillMeta]) -> Result<SkillContent>

/// 校验 skill name 合法性
pub fn validate_skill_name(name: &str) -> Result<()>
// 正则: ^[a-z0-9][a-z0-9-]*$, 长度 1-64
```

### 3.3 Frontmatter 解析实现提示

**不需要引入 YAML 解析库**。frontmatter 格式简单，可以手动解析：

```rust
fn parse_skill_md(content: &str) -> Result<(String, String, Vec<String>, String)> {
    let content = content.trim();
    if !content.starts_with("---") {
        return Err(eyre!("SKILL.md 缺少 frontmatter"));
    }

    // 找到第二个 ---
    let rest = &content[3..];
    let end = rest.find("---").ok_or_else(|| eyre!("frontmatter 未闭合"))?;
    let frontmatter = &rest[..end].trim();
    let body = rest[end + 3..].trim().to_string();

    // 逐行解析 key: value
    let mut name = String::new();
    let mut description = String::new();
    let mut tags = Vec::new();

    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(val) = line.strip_prefix("name:") {
            name = val.trim().trim_matches('"').to_string();
        } else if let Some(val) = line.strip_prefix("description:") {
            description = val.trim().trim_matches('"').to_string();
        } else if let Some(val) = line.strip_prefix("tags:") {
            // 解析 [tag1, tag2] 格式
            let val = val.trim().trim_start_matches('[').trim_end_matches(']');
            tags = val.split(',')
                .map(|t| t.trim().trim_matches('"').to_string())
                .filter(|t| !t.is_empty())
                .collect();
        }
    }

    if name.is_empty() {
        return Err(eyre!("SKILL.md frontmatter 缺少 name 字段"));
    }
    if description.is_empty() {
        return Err(eyre!("SKILL.md frontmatter 缺少 description 字段"));
    }

    Ok((name, description, tags, body))
}
```

---

## 四、SkillTool 实现（LLM 自动触发路径）

```rust
// src/tools/skill.rs
use async_trait::async_trait;
use color_eyre::eyre::Result;
use serde_json::json;

use crate::security::SecurityPolicy;
use crate::skills::{SkillMeta, load_skill_content};
use super::traits::{Tool, ToolResult};

/// LLM 通过调用此工具加载技能的 L2 指令
pub struct SkillTool {
    skills: Vec<SkillMeta>,
}

impl SkillTool {
    pub fn new(skills: Vec<SkillMeta>) -> Self {
        Self { skills }
    }

    /// 获取 skill 列表引用（供 system prompt 构建用）
    pub fn skills(&self) -> &[SkillMeta] {
        &self.skills
    }
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str { "skill" }

    fn description(&self) -> &str {
        "加载技能的详细指令。当你判断某个技能适用于当前任务时，\
         调用此工具获取完整操作指南。参数: name（技能名称）"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "要加载的技能名称"
                }
            },
            "required": ["name"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _policy: &SecurityPolicy,
    ) -> Result<ToolResult> {
        let name = match args.get("name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("缺少 name 参数".to_string()),
            }),
        };

        match load_skill_content(name, &self.skills) {
            Ok(content) => {
                let mut output = content.instructions;

                // 如果有 L3 资源文件，附带清单
                if !content.resources.is_empty() {
                    output.push_str("\n\n---\n附带资源文件（可用 file_read 查看）:\n");
                    for r in &content.resources {
                        output.push_str(&format!("- {}\n", r));
                    }
                }

                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }
            Err(e) => {
                // 列出可用技能帮助 LLM 修正
                let available: Vec<&str> = self.skills.iter().map(|s| s.name.as_str()).collect();
                Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "未找到技能 '{}'。可用技能: {}",
                        name,
                        available.join(", ")
                    )),
                })
            }
        }
    }
}
```

---

## 五、System Prompt 集成

### 5.1 build_system_prompt 改动

在 `src/agent/loop_.rs` 的 `build_system_prompt()` 方法中，在工具描述（Segment 2）和安全规则（Segment 3）之间新增技能列表段：

```rust
// 现有代码位置: build_system_prompt() 方法中，约 line 495 之后

// [2.5] 可用技能（仅当有 skills 时）
if !self.skills_meta.is_empty() {
    let mut skills_section = "[可用技能]（需要时用 skill 工具加载详细指令）\n".to_string();
    for skill in &self.skills_meta {
        skills_section.push_str(&format!("- {}: {}\n", skill.name, skill.description));
    }
    parts.push(skills_section);
}
```

### 5.2 Agent 结构体改动

Agent 需要持有 skills 元数据（只是引用，不拥有）：

```rust
// src/agent/loop_.rs — Agent 结构体新增字段
pub struct Agent {
    // ... 现有字段 ...
    skills_meta: Vec<SkillMeta>,  // L1 元数据，用于 system prompt
}

impl Agent {
    // 新增方法：手动注入技能上下文（/skill <name> 用）
    pub fn inject_skill_context(&mut self, skill_name: &str, instructions: &str) {
        let msg = ConversationMessage::Chat(ChatMessage {
            role: "user".to_string(),
            content: format!("[技能指令: {}]\n{}", skill_name, instructions),
            reasoning_content: None,
        });
        self.history.push(msg);
    }
}
```

### 5.3 Agent::new() 签名改动

```rust
pub fn new(
    provider: Box<dyn Provider>,
    tools: Vec<Box<dyn Tool>>,
    memory: Box<dyn Memory>,
    policy: SecurityPolicy,
    provider_name: String,
    base_url: String,
    model: String,
    temperature: f64,
    skills_meta: Vec<SkillMeta>,  // 新增参数
) -> Self
```

---

## 六、CLI `/skill` 斜杠命令实现

### 6.1 handle_slash_command 改动

在 `src/channels/cli.rs` 的 `handle_slash_command()` 函数中新增 `"skill"` 分支：

```rust
// handle_slash_command 需要新增参数: skills: &mut Vec<SkillMeta>
"skill" => {
    let sub_parts: Vec<&str> = arg.unwrap_or("").splitn(2, ' ').collect();
    match sub_parts[0] {
        "" => cmd_skill_list(skills),
        "new" => cmd_skill_new(sub_parts.get(1).copied())?,
        "edit" => cmd_skill_edit(sub_parts.get(1).copied())?,
        "delete" => cmd_skill_delete(sub_parts.get(1).copied(), skills)?,
        "show" => cmd_skill_show(sub_parts.get(1).copied(), skills)?,
        name => {
            // 默认行为：加载技能指令注入对话
            match load_skill_content(name, skills) {
                Ok(content) => {
                    agent.inject_skill_context(name, &content.instructions);
                    println!("✓ 已加载技能: {}", name);
                }
                Err(e) => println!("✗ {}", e),
            }
        }
    }
}
```

### 6.2 各子命令实现

```rust
/// /skill — 列出所有技能
fn cmd_skill_list(skills: &[SkillMeta]) {
    if skills.is_empty() {
        println!("暂无可用技能。使用 /skill new <name> 创建。");
        return;
    }
    println!("可用技能:\n");
    for s in skills {
        let source_label = match s.source {
            SkillSource::BuiltIn => "[内置]",
            SkillSource::Global  => "[全局]",
            SkillSource::Project => "[项目]",
        };
        println!("  {} {} — {}", source_label, s.name, s.description);
    }
    println!("\n使用 /skill <name> 加载技能，/skill show <name> 查看内容。");
}

/// /skill new <name> — 创建技能模板
fn cmd_skill_new(name: Option<&str>) -> Result<()> {
    let name = name.ok_or_else(|| eyre!("用法: /skill new <name>"))?;
    validate_skill_name(name)?;

    let global_dir = /* ~/.rrclaw/skills/ */;
    let skill_dir = global_dir.join(name);
    if skill_dir.exists() {
        println!("技能 '{}' 已存在。使用 /skill edit {} 编辑。", name, name);
        return Ok(());
    }

    std::fs::create_dir_all(&skill_dir)?;
    let template = format!(
        "---\nname: {}\ndescription: 简短描述这个技能做什么。当用户要求 XXX 时使用。\ntags: []\n---\n\n# {}\n\n## 步骤\n1. 用 file_read 读取相关文件\n2. 分析内容\n3. 输出结果\n\n## 注意事项\n- ...\n",
        name,
        name.replace('-', " ") // 标题用空格
    );
    std::fs::write(skill_dir.join("SKILL.md"), &template)?;

    println!("✓ 已创建技能: {}/SKILL.md", skill_dir.display());
    println!("  使用 /skill edit {} 编辑内容。", name);
    Ok(())
}

/// /skill edit <name> — 用 $EDITOR 编辑
fn cmd_skill_edit(name: Option<&str>) -> Result<()> {
    let name = name.ok_or_else(|| eyre!("用法: /skill edit <name>"))?;
    let skill_path = find_skill_path(name)?;  // 在全局/项目目录中查找
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());

    // 注意：这会暂时接管终端
    std::process::Command::new(&editor)
        .arg(skill_path.join("SKILL.md"))
        .status()?;

    println!("✓ 编辑完成。技能将在下次加载时生效。");
    Ok(())
}

/// /skill delete <name> — 删除技能（带确认）
fn cmd_skill_delete(name: Option<&str>, skills: &mut Vec<SkillMeta>) -> Result<()> {
    let name = name.ok_or_else(|| eyre!("用法: /skill delete <name>"))?;

    // 内置技能不可删除
    if let Some(s) = skills.iter().find(|s| s.name == name) {
        if s.source == SkillSource::BuiltIn {
            println!("✗ 内置技能不可删除。");
            return Ok(());
        }
    }

    print!("确认删除技能 '{}'? [y/N] ", name);
    std::io::stdout().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    if input.trim().to_lowercase() != "y" {
        println!("已取消。");
        return Ok(());
    }

    let skill_path = find_skill_path(name)?;
    std::fs::remove_dir_all(&skill_path)?;
    skills.retain(|s| s.name != name);
    println!("✓ 已删除技能: {}", name);
    Ok(())
}

/// /skill show <name> — 打印全文
fn cmd_skill_show(name: Option<&str>, skills: &[SkillMeta]) -> Result<()> {
    let name = name.ok_or_else(|| eyre!("用法: /skill show <name>"))?;
    let content = load_skill_content(name, skills)?;
    println!("--- {} ---\n{}", name, content.instructions);
    Ok(())
}
```

---

## 七、main.rs 改动

### 7.1 启动时加载 skills

```rust
// run_agent() 中，在创建 tools 之前

let workspace_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
let global_skills_dir = base_dirs.home_dir().join(".rrclaw").join("skills");

// 加载内置 skills
let builtin_skills = rrclaw::skills::builtin_skills();

// 合并：项目级 > 全局 > 内置
let skills = rrclaw::skills::load_skills(
    &workspace_dir,
    &global_skills_dir,
    builtin_skills,
);

// 创建 Tools（SkillTool 需要 skills）
let tools = rrclaw::tools::create_tools(
    config.clone(),
    data_dir.clone(),
    log_dir.clone(),
    config_path.clone(),
    skills.clone(),  // 新增参数
);

// 创建 Agent（传入 skills_meta）
let mut agent = rrclaw::agent::Agent::new(
    provider,
    tools,
    Box::new(memory.clone()),
    policy,
    provider_key.to_string(),
    provider_config.base_url.clone(),
    model,
    config.default.temperature,
    skills.clone(),  // 新增参数
);

// run_repl 也需要 skills（供 /skill 命令用）
rrclaw::channels::cli::run_repl(&mut agent, &memory, &config, skills).await?;
```

### 7.2 create_tools 签名改动

```rust
// src/tools/mod.rs
pub fn create_tools(
    app_config: Config,
    data_dir: PathBuf,
    log_dir: PathBuf,
    config_path: PathBuf,
    skills: Vec<SkillMeta>,  // 新增
) -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(ShellTool),
        Box::new(FileReadTool),
        Box::new(FileWriteTool),
        Box::new(ConfigTool),
        Box::new(SelfInfoTool::new(app_config, data_dir, log_dir, config_path)),
        Box::new(SkillTool::new(skills)),  // 新增
    ]
}
```

---

## 八、内置示例 Skills

3 个内置 skill，用 `include_str!` 嵌入：

### 8.1 文件位置

```
src/skills/
  mod.rs          # 模块主文件
  builtin/
    code-review.md
    rust-dev.md
    git-commit.md
```

### 8.2 code-review.md

```markdown
---
name: code-review
description: 代码审查工作流。检查代码质量、安全性、潜在 bug。当用户要求 review 或审查代码时使用。
tags: [dev, review]
---

# 代码审查

## 审查流程
1. 用 file_read 读取目标文件
2. 逐项检查以下维度：
   - **可读性**: 命名、结构、注释是否清晰
   - **正确性**: 逻辑错误、边界条件、错误处理
   - **安全性**: 注入风险、越权访问、敏感信息泄露
   - **性能**: 不必要的分配、O(n²) 循环、阻塞操作
3. 如果是 Rust 项目，用 shell 运行 `cargo clippy -- -W clippy::all`
4. 输出结构化报告

## 报告格式
对每个发现的问题输出：
- **文件:行号** — 位置
- **级别** — 🔴 严重 / 🟡 警告 / 🔵 建议
- **问题** — 描述
- **建议** — 修复方案或代码示例

最后给出总结：总问题数、按级别分布、整体评价。
```

### 8.3 rust-dev.md

```markdown
---
name: rust-dev
description: Rust 开发辅助。代码生成、错误处理模式、性能优化、cargo 命令指导。当用户进行 Rust 开发时使用。
tags: [dev, rust]
---

# Rust 开发辅助

## 代码规范
- 使用 `thiserror` 定义错误类型，`color_eyre` 处理顶层错误
- 异步代码使用 `tokio`，trait 用 `async_trait`
- 序列化使用 `serde` + `serde_json`
- 优先使用 `&str` 而非 `String`，避免不必要的 clone

## 工作流程
1. 用 file_read 阅读相关代码了解上下文
2. 生成代码时遵循项目现有风格
3. 用 shell 运行 `cargo check` 验证编译
4. 用 shell 运行 `cargo test` 验证测试
5. 用 shell 运行 `cargo clippy -- -W clippy::all` 检查 lint

## 常用 cargo 命令
- `cargo check` — 快速编译检查
- `cargo test` — 运行测试
- `cargo test <name>` — 运行特定测试
- `cargo clippy -- -W clippy::all` — lint 检查
- `cargo build --release` — 发布构建
- `cargo doc --open` — 生成并查看文档
```

### 8.4 git-commit.md

```markdown
---
name: git-commit
description: Git 提交规范。生成规范的 commit message，检查暂存区，执行原子化提交。当用户要求提交代码时使用。
tags: [dev, git]
---

# Git 提交规范

## 提交流程
1. 用 shell 运行 `git status` 查看暂存区状态
2. 用 shell 运行 `git diff --cached` 查看已暂存的变更
3. 分析变更内容，生成 commit message
4. 用 shell 执行 `git commit -m "<message>"`

## Commit Message 格式
```
<type>: <简短描述>
```

type 取值:
- `feat` — 新功能
- `fix` — Bug 修复
- `docs` — 文档变更
- `test` — 测试相关
- `refactor` — 重构（不改变行为）
- `chore` — 构建/依赖/配置变更

## 原则
- 每个 commit 只做一件事
- 英文 commit message
- 描述 **为什么** 而不是 **做了什么**
- 如果暂存区有多种不相关的改动，建议拆分成多个 commit
```

### 8.5 builtin_skills() 函数

```rust
// src/skills/mod.rs

const BUILTIN_CODE_REVIEW: &str = include_str!("builtin/code-review.md");
const BUILTIN_RUST_DEV: &str = include_str!("builtin/rust-dev.md");
const BUILTIN_GIT_COMMIT: &str = include_str!("builtin/git-commit.md");

pub fn builtin_skills() -> Vec<SkillMeta> {
    let mut skills = Vec::new();
    for content in [BUILTIN_CODE_REVIEW, BUILTIN_RUST_DEV, BUILTIN_GIT_COMMIT] {
        if let Ok((name, desc, tags, _body)) = parse_skill_md(content) {
            skills.push(SkillMeta {
                name,
                description: desc,
                tags,
                source: SkillSource::BuiltIn,
                path: None,
            });
        }
    }
    skills
}
```

内置 skill 的 L2 加载：`load_skill_content()` 对 `SkillSource::BuiltIn` 直接从 `include_str!` 的常量中解析正文，不走文件系统。

---

## 九、改动范围

| 文件 | 改动 | 复杂度 |
|------|------|--------|
| `src/skills/mod.rs` | **新增** — SkillMeta/SkillContent/SkillSource + parse/scan/load 函数 + builtin_skills() | 中 |
| `src/skills/builtin/*.md` | **新增** — 3 个内置 skill 文件 | 低 |
| `src/tools/skill.rs` | **新增** — SkillTool 实现 Tool trait | 中 |
| `src/tools/mod.rs` | 注册 SkillTool，create_tools() 新增 skills 参数 | 低 |
| `src/agent/loop_.rs` | Agent 新增 skills_meta 字段 + build_system_prompt 技能段 + inject_skill_context() + new() 签名 | 低 |
| `src/channels/cli.rs` | handle_slash_command 新增 /skill 分支 + CRUD 子命令函数 | 中 |
| `src/main.rs` | 启动时加载 skills，传入 create_tools() 和 Agent::new() 和 run_repl() | 低 |
| `src/lib.rs` | `pub mod skills;` | 低 |

**不需要改动**：Provider trait、Memory trait、Security、现有 5 个 Tools。

---

## 十、提交策略

| # | 提交 | 说明 |
|---|------|------|
| 1 | `docs: add P3 skills system design` | 本文档 |
| 2 | `docs: add skills module Claude.md` | `src/skills/Claude.md` |
| 3 | `feat: add skills module with SKILL.md loader` | `src/skills/mod.rs` — 数据结构 + parse + scan + load + builtin |
| 4 | `test: add skills loading tests` | frontmatter 解析、目录扫描、优先级覆盖、name 校验 |
| 5 | `feat: add SkillTool for on-demand skill loading` | `src/tools/skill.rs` — Tool trait 实现 |
| 6 | `test: add SkillTool execution tests` | name 查找、L2 返回、L3 清单、未知 name 错误 |
| 7 | `feat: integrate skills into agent system prompt` | loop_.rs — skills_meta 字段 + prompt 段 + inject + new() |
| 8 | `feat: add /skill slash command with load and list` | cli.rs — /skill 列出 + /skill <name> 加载注入 |
| 9 | `feat: add /skill CRUD subcommands (new, edit, delete, show)` | cli.rs — 创建模板、$EDITOR 编辑、删除确认、查看 |
| 10 | `feat: wire skills loading in main.rs` | main.rs — 启动扫描 + 传参 + run_repl 签名 |
| 11 | `feat: add built-in example skills (code-review, rust-dev, git-commit)` | src/skills/builtin/*.md |

共 ~11 commits，预计新增 ~700-900 行代码。

---

## 十一、验证方式

### 自动化测试（~15 个）
- `parse_skill_md` 正常解析、缺少 name 报错、缺少 description 报错、无 frontmatter 报错
- `scan_skills_dir` 空目录返回空、多个 skill 目录正确扫描、忽略无 SKILL.md 的目录
- `load_skills` 项目级覆盖全局同名 skill、内置被全局覆盖
- `validate_skill_name` 合法/非法名称
- `builtin_skills()` 返回 3 个内置 skill
- SkillTool `execute` 正常返回 L2 内容 + L3 清单
- SkillTool `execute` 未知 name 返回可用列表
- System prompt 有 skills 时包含 `[可用技能]` 段
- System prompt 无 skills 时不包含技能段

### 手动端到端测试

**场景 A：LLM 自动触发**
```
1. cargo run -- agent（内置 skills 自动加载）
2. 输入 "帮我 review src/main.rs"
3. 期望: LLM 调用 skill(name="code-review") → 获取指令 → 按指令用 file_read + shell 审查
```

**场景 B：用户手动触发**
```
1. cargo run -- agent
2. /skill → 列出 [内置] code-review、rust-dev、git-commit
3. /skill code-review → "✓ 已加载技能: code-review"
4. "review src/main.rs" → LLM 按注入的指令执行
```

**场景 C：CRUD 管理**
```
1. /skill new my-helper → 创建 ~/.rrclaw/skills/my-helper/SKILL.md
2. /skill edit my-helper → $EDITOR 打开编辑
3. /skill show my-helper → 打印全文
4. /skill → 列表中出现 [全局] my-helper
5. /skill delete my-helper → 确认后删除
```

### 回归
- `cargo test` 全部通过（现有 104 + 新增 ~15）
- `cargo clippy -- -W clippy::all` 零警告
- `cargo build --release` 通过

---

## 十二、关键注意事项（给接力实现者）

### 项目规范
1. **文档驱动**：先写/更新 Claude.md → 写测试 → 改代码 → 跑通测试 → 提交
2. **原子化提交**：每个 commit 只做一件事，按提交策略顺序执行
3. **测试覆盖**：每个功能必须有测试，不允许"手动验证"替代

### 代码风格
- `ToolSpec` 定义在 `src/providers/traits.rs`，tools 模块通过 `use crate::providers::ToolSpec` 引用
- Tool trait 定义在 `src/tools/traits.rs`，`pre_validate()` 有默认实现返回 None
- 错误处理用 `color_eyre::eyre::Result` + `thiserror`
- 日志用 `tracing::debug!` / `tracing::warn!`
- 测试用 `tempfile::tempdir()` 创建临时目录

### 现有代码关键位置
- `src/agent/loop_.rs:19-30` — Agent 结构体定义
- `src/agent/loop_.rs:482-540` — `build_system_prompt()` 方法
- `src/agent/loop_.rs:60-80` — `Agent::new()` 构造函数
- `src/tools/mod.rs:18-31` — `create_tools()` 工厂函数
- `src/channels/cli.rs:167-208` — `handle_slash_command()` 斜杠命令路由
- `src/channels/cli.rs:82-164` — `run_repl()` 主循环
- `src/main.rs:62-143` — `run_agent()` 启动流程
- `src/lib.rs` — 模块声明（需新增 `pub mod skills;`）

### 依赖
- 不需要引入新的 crate
- YAML frontmatter 用手动解析（逐行 `strip_prefix`），不引入 yaml 库
- 内置 skill 用 `include_str!` 编译时嵌入

### 容易出错的点
- `run_repl()` 签名变更会影响 `main.rs` 调用，确保同步更新
- `Agent::new()` 新增参数后，所有现有测试中构造 Agent 的地方都需要补上 `skills_meta: vec![]`
- macOS 上 `/var` → `/private/var` symlink 问题已在 security 模块用 `canonicalize_with_ancestors` 修复，skills 目录扫描不受影响
- `handle_slash_command` 当前参数是 `(cmd, agent, session_id, memory, config)`，新增 `skills` 参数
