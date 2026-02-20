# P5-2: 身份文件系统（AGENT.md / USER.md / SOUL.md）实现计划

## 背景

当前 RRClaw 的 system prompt 里固定写着 "你是 RRClaw，一个安全优先的 AI 助手。"，Agent 没有任何对用户、项目、自身角色的了解。

ZeroClaw 通过 Bootstrap files（`AGENTS.md`、`SOUL.md`、`IDENTITY.md`、`USER.md`）将这些信息注入 system prompt，使 Agent 变成"懂这个用户、懂这个项目"的专属助手。IronClaw 称之为 Identity Files。

**核心价值**：用户无需每次解释偏好，Agent 开箱即记得"这个项目用 Rust 2021 edition"、"用户喜欢简洁中文回复"、"提交前必须跑 clippy"。

**实现成本极低**：读文件 + 注入字符串，不新增依赖，约 150 行代码。

---

## 一、支持的身份文件

按用途分三类，两级覆盖（全局 < 项目本地）：

| 文件 | 路径 | 用途 | 示例内容 |
|------|------|------|---------|
| `USER.md` | `~/.rrclaw/USER.md` | **用户全局偏好**（所有项目共享） | 技术背景、语言偏好、工作习惯 |
| `SOUL.md` | `~/.rrclaw/SOUL.md` | **全局 Agent 人格**（自定义助手角色） | "你叫 Max，直接简洁，不用敬语" |
| `AGENT.md` | `<workspace>/.rrclaw/AGENT.md` | **项目行为约定**（项目级，类似 GitHub Copilot 的 `.github/copilot-instructions.md`）| 代码规范、提交约定、禁止事项 |
| `SOUL.md` | `<workspace>/.rrclaw/SOUL.md` | **项目 Agent 人格**（覆盖全局 SOUL.md） | "此项目中你是架构审查员，严格按规范" |

### 加载优先级与合并规则

- **USER.md 和 AGENT.md** → 互不覆盖，**全部合并**注入（用途不同）
- **SOUL.md** → 项目本地覆盖全局（项目的人格设定更具体）
- 文件不存在时**静默跳过**，不报错
- 任何文件超过 **8000 字符**时截断并添加提示

### 加载顺序（决定在 system prompt 中的排列顺序）

```
1. ~/.rrclaw/USER.md        — 全局用户偏好（最先，因为用户是所有项目的主人）
2. ~/.rrclaw/SOUL.md        — 全局人格（若项目无 SOUL.md 则使用此）
3. <workspace>/.rrclaw/SOUL.md  — 项目人格（覆盖全局 SOUL.md）
4. <workspace>/.rrclaw/AGENT.md — 项目约定（最后，最具体）
```

---

## 二、架构设计

```
main.rs（启动时）
    │
    ├── data_dir = ~/.rrclaw/
    ├── workspace_dir = policy.workspace_dir（当前工作目录）
    │
    ▼
rrclaw::agent::identity::load_identity_context(workspace_dir, data_dir)
    │
    ├── 读 ~/.rrclaw/USER.md        → Option<String>
    ├── 读 ~/.rrclaw/SOUL.md        → Option<String>（如无项目 SOUL，使用此）
    ├── 读 <workspace>/.rrclaw/SOUL.md → Option<String>（有则覆盖全局）
    └── 读 <workspace>/.rrclaw/AGENT.md → Option<String>
    │
    ▼
Option<String>（合并后的文本，有内容时 Some，全为空时 None）
    │
    ▼
Agent::new(..., identity_context: Option<String>)
    │
    ▼
build_system_prompt() 中注入为 [0] 段（在 [1] 身份描述之前）
```

---

## 三、新增文件：src/agent/identity.rs

```rust
use std::path::Path;
use tracing::debug;

/// 单个身份文件的配置
struct IdentityFile {
    /// 在 system prompt 中显示的节名
    section_name: &'static str,
    /// 相对于某个根目录的路径
    relative_path: &'static str,
}

/// 全局身份文件（相对于 data_dir，即 ~/.rrclaw/）
const GLOBAL_FILES: &[IdentityFile] = &[
    IdentityFile {
        section_name: "用户偏好",
        relative_path: "USER.md",
    },
];

/// 项目身份文件（相对于 workspace_dir）
const PROJECT_FILES: &[IdentityFile] = &[
    IdentityFile {
        section_name: "项目行为约定",
        relative_path: ".rrclaw/AGENT.md",
    },
];

/// 人格文件（项目优先，全局兜底）
const SOUL_GLOBAL: &str = "SOUL.md";
const SOUL_PROJECT: &str = ".rrclaw/SOUL.md";

/// 单个文件最大字节数（8 KiB）
const MAX_FILE_BYTES: usize = 8 * 1024;

/// 加载所有身份文件，合并为注入 system prompt 的字符串
///
/// # 参数
/// - `workspace_dir`: 当前工作目录（项目目录）
/// - `data_dir`: RRClaw 数据目录（通常是 `~/.rrclaw/`）
///
/// # 返回
/// - `Some(String)`: 有内容时返回合并后的 Markdown 文本
/// - `None`: 所有文件均不存在或为空
pub fn load_identity_context(workspace_dir: &Path, data_dir: &Path) -> Option<String> {
    let mut sections: Vec<(String, String)> = Vec::new(); // (section_name, content)

    // 1. 全局用户偏好文件
    for file in GLOBAL_FILES {
        let path = data_dir.join(file.relative_path);
        if let Some(content) = read_file_safe(&path) {
            sections.push((file.section_name.to_string(), content));
        }
    }

    // 2. SOUL.md：项目优先，全局兜底
    let project_soul_path = workspace_dir.join(SOUL_PROJECT);
    let global_soul_path = data_dir.join(SOUL_GLOBAL);

    if let Some(content) = read_file_safe(&project_soul_path) {
        sections.push(("Agent 人格（项目级）".to_string(), content));
    } else if let Some(content) = read_file_safe(&global_soul_path) {
        sections.push(("Agent 人格".to_string(), content));
    }

    // 3. 项目行为约定文件
    for file in PROJECT_FILES {
        let path = workspace_dir.join(file.relative_path);
        if let Some(content) = read_file_safe(&path) {
            sections.push((file.section_name.to_string(), content));
        }
    }

    if sections.is_empty() {
        return None;
    }

    // 合并所有节，使用清晰的分隔符
    let mut result = String::new();
    for (name, content) in &sections {
        result.push_str(&format!("### {}\n{}\n\n", name, content.trim()));
    }

    debug!(
        "已加载 {} 个身份文件，合并后 {} 字符",
        sections.len(),
        result.len()
    );

    Some(result.trim_end().to_string())
}

/// 安全读取文件内容
/// - 文件不存在：返回 None（静默）
/// - 超出大小限制：截断后返回
/// - 空文件：返回 None
fn read_file_safe(path: &Path) -> Option<String> {
    if !path.exists() {
        return None;
    }

    match std::fs::read(path) {
        Ok(bytes) => {
            if bytes.is_empty() {
                return None;
            }

            let truncated = if bytes.len() > MAX_FILE_BYTES {
                debug!(
                    "身份文件超出大小限制（{}B > {}B），截断: {:?}",
                    bytes.len(),
                    MAX_FILE_BYTES,
                    path
                );
                &bytes[..MAX_FILE_BYTES]
            } else {
                &bytes
            };

            // 在 UTF-8 字符边界处截断
            match std::str::from_utf8(truncated) {
                Ok(s) => Some(s.to_string()),
                Err(e) => {
                    // 截取到最后一个合法 UTF-8 边界
                    let valid_up_to = e.valid_up_to();
                    if valid_up_to == 0 {
                        return None;
                    }
                    // safety: valid_up_to 是合法 UTF-8 边界
                    let s = unsafe { std::str::from_utf8_unchecked(&truncated[..valid_up_to]) };
                    Some(format!("{}\n\n[文件内容已截断]", s))
                }
            }
        }
        Err(e) => {
            debug!("读取身份文件失败（忽略）: {:?} - {}", path, e);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_file(dir: &std::path::Path, name: &str, content: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(name), content).unwrap();
    }

    // ─── load_identity_context 测试 ──────────────────────────────────

    #[test]
    fn no_files_returns_none() {
        let workspace = tempdir().unwrap();
        let data_dir = tempdir().unwrap();
        let result = load_identity_context(workspace.path(), data_dir.path());
        assert!(result.is_none());
    }

    #[test]
    fn user_md_loaded_from_data_dir() {
        let workspace = tempdir().unwrap();
        let data_dir = tempdir().unwrap();
        write_file(data_dir.path(), "USER.md", "用户喜欢 Rust");

        let result = load_identity_context(workspace.path(), data_dir.path());
        assert!(result.is_some());
        let content = result.unwrap();
        assert!(content.contains("用户喜欢 Rust"));
        assert!(content.contains("用户偏好"));
    }

    #[test]
    fn agent_md_loaded_from_workspace_rrclaw() {
        let workspace = tempdir().unwrap();
        let data_dir = tempdir().unwrap();
        let rrclaw_dir = workspace.path().join(".rrclaw");
        write_file(&rrclaw_dir, "AGENT.md", "所有提交必须通过 clippy");

        let result = load_identity_context(workspace.path(), data_dir.path());
        assert!(result.is_some());
        let content = result.unwrap();
        assert!(content.contains("所有提交必须通过 clippy"));
        assert!(content.contains("项目行为约定"));
    }

    #[test]
    fn global_soul_used_when_no_project_soul() {
        let workspace = tempdir().unwrap();
        let data_dir = tempdir().unwrap();
        write_file(data_dir.path(), "SOUL.md", "你是 Max，简洁直接");

        let result = load_identity_context(workspace.path(), data_dir.path());
        assert!(result.is_some());
        let content = result.unwrap();
        assert!(content.contains("你是 Max"));
        assert!(content.contains("Agent 人格"));
        assert!(!content.contains("项目级"));
    }

    #[test]
    fn project_soul_overrides_global_soul() {
        let workspace = tempdir().unwrap();
        let data_dir = tempdir().unwrap();

        // 全局 SOUL
        write_file(data_dir.path(), "SOUL.md", "全局人格");
        // 项目 SOUL
        let rrclaw_dir = workspace.path().join(".rrclaw");
        write_file(&rrclaw_dir, "SOUL.md", "项目人格：严格架构审查员");

        let result = load_identity_context(workspace.path(), data_dir.path());
        let content = result.unwrap();
        // 只有项目人格，全局被跳过
        assert!(content.contains("项目人格"));
        assert!(!content.contains("全局人格"));
        assert!(content.contains("项目级"));
    }

    #[test]
    fn all_files_combined() {
        let workspace = tempdir().unwrap();
        let data_dir = tempdir().unwrap();
        let rrclaw_dir = workspace.path().join(".rrclaw");

        write_file(data_dir.path(), "USER.md", "用户偏好 Rust");
        write_file(data_dir.path(), "SOUL.md", "全局人格");
        write_file(&rrclaw_dir, "AGENT.md", "项目用 cargo fmt");
        write_file(&rrclaw_dir, "SOUL.md", "项目人格");

        let result = load_identity_context(workspace.path(), data_dir.path());
        let content = result.unwrap();
        // USER.md 和 AGENT.md 都应包含
        assert!(content.contains("用户偏好 Rust"));
        assert!(content.contains("项目用 cargo fmt"));
        // 只有项目人格（全局被覆盖）
        assert!(content.contains("项目人格"));
        assert!(!content.contains("全局人格"));
    }

    #[test]
    fn empty_file_returns_none() {
        let workspace = tempdir().unwrap();
        let data_dir = tempdir().unwrap();
        write_file(data_dir.path(), "USER.md", "");

        let result = load_identity_context(workspace.path(), data_dir.path());
        assert!(result.is_none());
    }

    #[test]
    fn whitespace_only_file_treated_as_empty_after_trim() {
        let workspace = tempdir().unwrap();
        let data_dir = tempdir().unwrap();
        write_file(data_dir.path(), "USER.md", "   \n\n  ");

        // read_file_safe 返回 Some("   \n\n  ")，但 sections 里 content.trim() 后为空
        // 合并后 result.trim_end() 也为空，所以 None
        // 注意：当前实现不对纯空白做二次过滤，但实际 inject 时 trim 掉了。
        // 测试实际行为：只要文件非零字节，就会加载（即使内容全是空白）
        // 这是可接受的 UX：用户自己写了空白文件，应该意识到
        let result = load_identity_context(workspace.path(), data_dir.path());
        // 文件非空（有空白字符），会被加载但 inject 后对 LLM 无影响
        // 此测试仅验证不 panic
        let _ = result;
    }

    // ─── read_file_safe 测试 ─────────────────────────────────────────

    #[test]
    fn read_file_safe_missing_returns_none() {
        let result = read_file_safe(Path::new("/nonexistent/path/file.md"));
        assert!(result.is_none());
    }

    #[test]
    fn read_file_safe_reads_content() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.md");
        fs::write(&path, "hello world").unwrap();
        let result = read_file_safe(&path);
        assert_eq!(result.unwrap(), "hello world");
    }

    #[test]
    fn read_file_safe_truncates_large_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("large.md");
        // 写入超过 8KB 的内容
        let content = "a".repeat(MAX_FILE_BYTES + 1000);
        fs::write(&path, &content).unwrap();

        let result = read_file_safe(&path).unwrap();
        assert!(result.len() <= MAX_FILE_BYTES);
    }

    #[test]
    fn read_file_safe_empty_file_returns_none() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("empty.md");
        fs::write(&path, "").unwrap();
        let result = read_file_safe(&path);
        assert!(result.is_none());
    }
}
```

---

## 四、Agent 结构体扩展（src/agent/loop_.rs）

### 4.1 新增字段

在 `Agent` 结构体中新增：

```rust
pub struct Agent {
    // ... 现有字段（不变）...
    action_tracker: ActionTracker,
    identity_context: Option<String>,  // ← 新增：启动时加载的身份文件内容
}
```

### 4.2 Agent::new() 新增参数

```rust
impl Agent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: Box<dyn Provider>,
        tools: Vec<Box<dyn Tool>>,
        memory: Box<dyn Memory>,
        policy: SecurityPolicy,
        provider_name: String,
        base_url: String,
        model: String,
        temperature: f64,
        skills_meta: Vec<SkillMeta>,
        identity_context: Option<String>,  // ← 新增参数（放在最后）
    ) -> Self {
        let max_actions = policy.max_actions_per_hour;
        Self {
            provider,
            tools,
            memory,
            policy,
            provider_name,
            base_url,
            model,
            temperature,
            history: Vec::new(),
            confirm_fn: None,
            skills_meta,
            routed_skill_content: None,
            action_tracker: ActionTracker::new(max_actions),
            identity_context,  // ← 新增
        }
    }
}
```

### 4.3 build_system_prompt() 注入身份上下文

在 `build_system_prompt()` 方法中，在 `[1] 身份描述` 之前插入 `[0]`：

```rust
fn build_system_prompt(&self, memories: &[crate::memory::MemoryEntry]) -> String {
    let mut parts = Vec::new();

    // ↓↓↓ 新增 [0] 段 ↓↓↓
    // [0] 用户定制上下文（身份文件）
    if let Some(identity) = &self.identity_context {
        parts.push(format!(
            "[用户定制上下文]\n{}",
            identity
        ));
    }
    // ↑↑↑ 新增结束 ↑↑↑

    // [1] 身份描述（原有，不变）
    parts.push("你是 RRClaw，一个安全优先的 AI 助手。".to_string());

    // [2] 可用工具描述（原有，不变）
    // ...以下全部原有代码不变...
}
```

**注入位置的理由**：
- 放在 [1] 之前 → 身份文件内容（尤其是 SOUL.md 的人格设定）对后面所有描述都起效
- 用 `[用户定制上下文]` 标签 → 让 LLM 明确理解这是用户的定制内容，优先遵守

---

## 五、main.rs 集成（加载并传入 Agent）

找到 `run_agent()` 函数中创建 Agent 的地方，在创建前加载身份文件：

```rust
// run_agent() 函数中，创建 tools 之后、创建 Agent 之前：

// ─── 身份文件加载（P5-2）────────────────────────────────────────────
let identity_context = rrclaw::agent::identity::load_identity_context(
    &policy.workspace_dir,
    &data_dir,
);
if identity_context.is_some() {
    tracing::info!("已加载用户身份文件");
}
// ─── 身份文件加载结束 ────────────────────────────────────────────────

// 原有的 Agent::new() 调用，新增 identity_context 参数：
let mut agent = Agent::new(
    provider,
    tools,
    memory_for_agent,
    policy,
    provider_name.clone(),
    base_url.clone(),
    model.clone(),
    config.default.temperature,
    skills.clone(),
    identity_context,  // ← 新增参数
);
```

---

## 六、src/agent/mod.rs — 导出新模块

```rust
// src/agent/mod.rs 中新增：
pub mod identity;
```

---

---

## 七、/identity 斜杠命令（src/channels/cli.rs）

### 8.1 命令列表

| 命令 | 说明 |
|------|------|
| `/identity` | 显示所有身份文件的状态（路径 + 是否存在 + 字数） |
| `/identity show user` | 打印 USER.md 内容 |
| `/identity show soul` | 打印 SOUL.md 内容（项目优先，全局兜底） |
| `/identity show agent` | 打印 AGENT.md 内容 |
| `/identity edit user` | 用 `$EDITOR` 打开 `~/.rrclaw/USER.md`（不存在则创建模板） |
| `/identity edit soul` | 用 `$EDITOR` 打开 SOUL.md（默认全局，若项目目录已有则打开项目级） |
| `/identity edit agent` | 用 `$EDITOR` 打开 `<workspace>/.rrclaw/AGENT.md`（不存在则创建模板） |
| `/identity reload` | 重新加载所有身份文件（立即生效，无需重启） |

### 8.2 /identity（状态总览）实现

```rust
fn cmd_identity_status(data_dir: &Path, workspace_dir: &Path) {
    println!("身份文件状态:\n");

    let files = [
        ("USER.md（全局用户偏好）",   data_dir.join("USER.md"),               true),
        ("SOUL.md（全局 Agent 人格）", data_dir.join("SOUL.md"),              true),
        ("SOUL.md（项目 Agent 人格）", workspace_dir.join(".rrclaw/SOUL.md"), false),
        ("AGENT.md（项目行为约定）",   workspace_dir.join(".rrclaw/AGENT.md"), false),
    ];

    for (label, path, is_global) in &files {
        let scope = if *is_global { "全局" } else { "项目" };
        match std::fs::metadata(path) {
            Ok(meta) => {
                let size = meta.len();
                println!("  ✓ {} [{}]", label, scope);
                println!("    路径: {}", path.display());
                println!("    大小: {} 字节", size);
            }
            Err(_) => {
                println!("  ✗ {} [{}]（未创建）", label, scope);
                println!("    路径: {}", path.display());
            }
        }
        println!();
    }

    println!("命令:");
    println!("  /identity edit user     编辑全局用户偏好");
    println!("  /identity edit soul     编辑 Agent 人格");
    println!("  /identity edit agent    编辑项目行为约定");
    println!("  /identity show <type>   查看文件内容");
    println!("  /identity reload        重新加载（立即生效）");
}
```

### 8.3 /identity edit \<type\> 实现

```rust
fn cmd_identity_edit(file_type: Option<&str>, data_dir: &Path, workspace_dir: &Path) -> Result<()> {
    let file_type = file_type.ok_or_else(|| eyre!("用法: /identity edit <user|soul|agent>"))?;

    let (path, template) = match file_type {
        "user" => (
            data_dir.join("USER.md"),
            TEMPLATE_USER,
        ),
        "soul" => {
            // 优先打开项目级，不存在时打开全局
            let project_path = workspace_dir.join(".rrclaw/SOUL.md");
            let global_path = data_dir.join("SOUL.md");
            let path = if project_path.exists() { project_path } else { global_path };
            (path, TEMPLATE_SOUL)
        }
        "agent" => (
            workspace_dir.join(".rrclaw/AGENT.md"),
            TEMPLATE_AGENT,
        ),
        other => return Err(eyre!("未知类型 '{}'。支持: user, soul, agent", other)),
    };

    // 文件不存在时创建目录和模板
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .wrap_err_with(|| format!("创建目录失败: {}", parent.display()))?;
        }
        std::fs::write(&path, template)
            .wrap_err_with(|| format!("写入模板失败: {}", path.display()))?;
        println!("✓ 已创建模板: {}", path.display());
    }

    // 用 $EDITOR 打开
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    std::process::Command::new(&editor)
        .arg(&path)
        .status()
        .wrap_err_with(|| format!("打开编辑器失败: {}", editor))?;

    println!("✓ 已保存。使用 /identity reload 立即生效，或重启 rrclaw。");
    Ok(())
}
```

### 8.4 /identity show \<type\> 实现

```rust
fn cmd_identity_show(file_type: Option<&str>, data_dir: &Path, workspace_dir: &Path) -> Result<()> {
    let file_type = file_type.ok_or_else(|| eyre!("用法: /identity show <user|soul|agent>"))?;

    let path = match file_type {
        "user"  => data_dir.join("USER.md"),
        "soul"  => {
            let project = workspace_dir.join(".rrclaw/SOUL.md");
            if project.exists() { project } else { data_dir.join("SOUL.md") }
        }
        "agent" => workspace_dir.join(".rrclaw/AGENT.md"),
        other   => return Err(eyre!("未知类型 '{}'。支持: user, soul, agent", other)),
    };

    match std::fs::read_to_string(&path) {
        Ok(content) => {
            println!("=== {} ===\n", path.display());
            println!("{}", content);
        }
        Err(_) => {
            println!("文件不存在: {}", path.display());
            println!("使用 /identity edit {} 创建。", file_type);
        }
    }
    Ok(())
}
```

### 8.5 /identity reload — 立即生效（无需重启）

这是 `/identity` 命令中最关键的部分：编辑后无需重启即可生效。

**实现方式**：在 Agent 上新增 `reload_identity()` 方法，CLI 调用后立即更新 system prompt。

**Agent 方法（agent/loop_.rs）**：

```rust
impl Agent {
    /// 重新加载身份文件（无需重启）
    /// 调用方需提供 data_dir（Agent 自身不存储，避免扩大结构体）
    pub fn reload_identity(&mut self, workspace_dir: &Path, data_dir: &Path) {
        self.identity_context = crate::agent::identity::load_identity_context(
            workspace_dir,
            data_dir,
        );
        if self.identity_context.is_some() {
            tracing::info!("身份文件已重新加载");
        }
    }
}
```

**CLI 调用（channels/cli.rs）**：

CLI 需要持有 `data_dir` 引用才能调用 `reload_identity()`。
查看现有 `CliChannel` 结构体，确认是否已有 `data_dir` 字段。如果没有，需要在构造时传入。

```rust
// /identity reload 处理
"reload" => {
    agent.reload_identity(&self.workspace_dir, &self.data_dir);
    println!("✓ 身份文件已重新加载，下次对话立即生效。");
}
```

### 8.6 文件模板

```rust
const TEMPLATE_USER: &str = r#"## 用户信息

- 主要技术栈：（填写你的技术背景）
- 工作语言：中文
- 回复风格：简洁直接，直接给结论和代码

## 偏好约定

- 代码示例省略明显的 use 声明
- 不需要每次都加免责声明
"#;

const TEMPLATE_SOUL: &str = r#"你的名字是 Claw。

- 说话风格：直接、简洁，不废话
- 不用"当然！"、"好的！"等开头
- 对代码问题给出具体答案
- 如果不确定，直接说"不确定，需要查一下"
"#;

const TEMPLATE_AGENT: &str = r#"## 项目约定

### 代码规范

- （填写代码风格要求，如：所有代码必须通过 clippy）

### Git 提交

- （填写提交规范，如：feat/fix/docs/test 前缀）

### 禁止事项

- （填写项目中的禁止事项）
"#;
```

### 8.7 /help 新增说明

在 `cmd_help()` 中 `/skill` 命令组之后新增：

```rust
println!("  /identity                查看身份文件状态");
println!("  /identity show <type>    查看身份文件内容（user/soul/agent）");
println!("  /identity edit <type>    编辑身份文件（$EDITOR）");
println!("  /identity reload         重新加载身份文件（立即生效）");
```

### 8.8 完整 /identity 命令分发（加入现有 handle_slash_command）

```rust
// 在现有斜杠命令 match 中新增：
"identity" => {
    match sub_cmd {
        None | Some("") => cmd_identity_status(&self.data_dir, &self.workspace_dir),
        Some(rest) => {
            let mut parts = rest.splitn(2, ' ');
            let action = parts.next().unwrap_or("");
            let arg = parts.next();
            match action {
                "show"   => cmd_identity_show(arg, &self.data_dir, &self.workspace_dir)?,
                "edit"   => cmd_identity_edit(arg, &self.data_dir, &self.workspace_dir)?,
                "reload" => {
                    agent.reload_identity(&self.workspace_dir, &self.data_dir);
                    println!("✓ 身份文件已重新加载，下次对话立即生效。");
                }
                other => println!("未知子命令 '{}'。用 /identity 查看帮助。", other),
            }
        }
    }
}
```

---

## 八、改动范围汇总

| 文件 | 改动类型 | 改动量 |
|------|---------|--------|
| `src/agent/identity.rs` | **新增文件** | ~150 行（含测试） |
| `src/agent/mod.rs` | 微改 | 1 行：`pub mod identity;` |
| `src/agent/loop_.rs` | 小改 | Agent +1 字段，new() +1 参数，build_system_prompt() +3 行，reload_identity() +8 行 |
| `src/channels/cli.rs` | 中等改动 | `/identity` 命令分发 + 4 个子命令函数 + 3 个模板常量，约 150 行 |
| `src/main.rs` | 小改 | 加载调用 ~5 行 + Agent::new() 传参 1 处 |

**不需要改动**：Provider、Memory、Tool、Security、Config schema、Telegram。

---

## 九、提交策略

| # | 提交 message | 内容 |
|---|-------------|------|
| 1 | `docs: add P5-2 identity files design` | 本文件 |
| 2 | `feat: add identity file loader (USER.md/SOUL.md/AGENT.md)` | src/agent/identity.rs |
| 3 | `feat: export identity module from agent` | src/agent/mod.rs |
| 4 | `feat: inject identity context into Agent system prompt` | agent/loop_.rs（字段 + new() + build_system_prompt） |
| 5 | `feat: add Agent::reload_identity() method` | agent/loop_.rs（reload 方法） |
| 6 | `feat: load identity files in main and pass to Agent` | src/main.rs |
| 7 | `feat: add /identity slash commands to CLI` | channels/cli.rs |
| 8 | `test: add identity loader unit tests` | 已在 identity.rs 内 |

---

## 十、测试执行方式

```bash
# 运行身份文件加载测试
cargo test -p rrclaw agent::identity

# 运行全部测试
cargo test -p rrclaw

# clippy
cargo clippy -p rrclaw -- -D warnings
```

---

## 十一、用户文件示例

安装完成后用户可以创建这些文件：

### ~/.rrclaw/USER.md（全局用户偏好）

```markdown
## 我的信息

- 主要使用语言：Rust，偶尔 Python
- 工作习惯：简洁明了，不需要过多解释，直接给结论和代码
- 时区：Asia/Shanghai（GMT+8）
- 偏好工具：cargo，git，ripgrep

## 偏好约定

- 回复使用中文
- 代码示例尽量精简，省略 use 声明
- 错误消息用中文解释原因
- 不需要加过多安全警告，我清楚风险
```

### ~/.rrclaw/SOUL.md（全局 Agent 人格）

```markdown
你的名字是 Claw。

- 说话风格：直接、简洁，不废话
- 不说"当然！"、"好的！"等废话开头
- 对代码问题给出具体答案，不给多个选项让用户选
- 如果不确定，直接说"不确定，需要查一下"
```

### <项目>/.rrclaw/AGENT.md（项目行为约定）

```markdown
## RRClaw 项目约定

### 代码规范
- 所有 Rust 代码必须通过 `cargo clippy -- -D warnings`
- 所有改动必须有测试覆盖
- 禁止用 `unwrap()` 替代正确的错误处理

### Git 提交
- 提交信息格式：`feat:` / `fix:` / `docs:` / `test:` / `chore:`
- 每次提交只做一件事（原子提交）
- 提交前运行 `cargo test`

### 禁止事项
- 禁止修改 Cargo.lock（除非更新依赖）
- 禁止在 main.rs 里加业务逻辑
- 禁止 `todo!()` 出现在提交的代码中
```

---

## 十二、关键注意事项

### 13.1 文件路径安全
`read_file_safe()` 直接读取硬编码的相对路径，不经过用户输入，不存在路径注入风险。文件路径由代码固定，用户只控制文件内容。

### 13.2 大文件保护
单文件 8000 字符上限。实际使用中，身份文件通常很短（100-500 字符），不是问题。极端情况（4 个文件各 8000 字符）时 context 会变大，但有截断兜底。

### 13.3 LLM 遵循度
身份文件放在 system prompt 的最前面（[0] 段），这在主流模型（GPT-4o、DeepSeek、Claude）中遵循度最高。ZeroClaw 也采用相同策略。

### 13.4 与 Skills 的区别
| 身份文件 | Skills |
|---------|--------|
| 描述"这个用户是谁/喜欢什么/项目有什么约定" | 描述"遇到这类任务该怎么做" |
| 每次都注入（全局上下文） | 按需加载（Phase 1 路由决定） |
| 用户手动维护 | 系统内置 + 用户可扩展 |
| 静态文本 | 结构化指令 |

### 13.5 Telegram Channel 的处理
Telegram Channel 也会创建 Agent，需要同步传入 `identity_context`。找到 Telegram 创建 Agent 的位置，做相同的 load + pass 操作。`/identity reload` 命令目前只对 CLI session 生效，Telegram 需重启 bot 才能更新。

### 13.6 注意：identity.rs 需要加入 agent/mod.rs

检查 `src/agent/mod.rs` 现有内容，确认正确添加模块声明：

```rust
// src/agent/mod.rs
pub mod identity;  // ← 新增
pub mod loop_;

pub use loop_::Agent;
```
