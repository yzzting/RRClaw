use std::path::{Path, PathBuf};

use color_eyre::eyre::{eyre, Result};

// 内置 skill 文件（编译时嵌入）
const BUILTIN_CODE_REVIEW: &str = include_str!("builtin/code-review.md");
const BUILTIN_RUST_DEV: &str = include_str!("builtin/rust-dev.md");
const BUILTIN_GIT_COMMIT: &str = include_str!("builtin/git-commit.md");
const BUILTIN_MCP_INSTALL: &str = include_str!("builtin/mcp-install.md");

/// Skill 来源（决定是否可删除、显示标签）
#[derive(Debug, Clone, PartialEq)]
pub enum SkillSource {
    BuiltIn,
    Global,
    Project,
}

impl SkillSource {
    pub fn label(&self) -> &'static str {
        match self {
            SkillSource::BuiltIn => "[内置]",
            SkillSource::Global => "[全局]",
            SkillSource::Project => "[项目]",
        }
    }
}

/// Skill 元数据（L1，常驻 system prompt）
#[derive(Debug, Clone)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    pub source: SkillSource,
    /// SKILL.md 所在目录，内置 skill 为 None
    pub path: Option<PathBuf>,
}

/// 完整 Skill 内容（L2，按需加载）
#[derive(Debug, Clone)]
pub struct SkillContent {
    pub meta: SkillMeta,
    /// SKILL.md 正文（去掉 frontmatter）
    pub instructions: String,
    /// 目录下其他文件名（L3 提示 LLM 可用 file_read 读取）
    pub resources: Vec<String>,
}

/// 解析 SKILL.md 的 YAML frontmatter
/// 返回 (name, description, tags, body)
pub fn parse_skill_md(content: &str) -> Result<(String, String, Vec<String>, String)> {
    let content = content.trim();
    if !content.starts_with("---") {
        return Err(eyre!("SKILL.md 缺少 frontmatter（应以 --- 开头）"));
    }

    let rest = &content[3..];
    let end = rest
        .find("---")
        .ok_or_else(|| eyre!("frontmatter 未闭合（缺少结束 ---）"))?;

    let frontmatter = rest[..end].trim();
    let body = rest[end + 3..].trim().to_string();

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
            let val = val.trim().trim_start_matches('[').trim_end_matches(']');
            tags = val
                .split(',')
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

/// 校验 skill name 合法性
/// 格式: ^[a-z0-9][a-z0-9-]*$，长度 1-64
pub fn validate_skill_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 64 {
        return Err(eyre!("skill name 长度必须在 1-64 之间"));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(eyre!(
            "skill name 只允许小写字母、数字和连字符，got: {}",
            name
        ));
    }
    if name.starts_with('-') {
        return Err(eyre!("skill name 不能以连字符开头"));
    }
    Ok(())
}

/// 扫描目录，加载所有 skill 的 L1 元数据
/// 每个子目录需包含 SKILL.md
pub fn scan_skills_dir(dir: &Path, source: SkillSource) -> Vec<SkillMeta> {
    let mut skills = Vec::new();

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return skills,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let skill_file = path.join("SKILL.md");
        if !skill_file.exists() {
            continue;
        }

        let content = match std::fs::read_to_string(&skill_file) {
            Ok(c) => c,
            Err(_) => continue,
        };

        match parse_skill_md(&content) {
            Ok((name, description, tags, _body)) => {
                skills.push(SkillMeta {
                    name,
                    description,
                    tags,
                    source: source.clone(),
                    path: Some(path),
                });
            }
            Err(e) => {
                tracing::warn!("跳过无效 skill {:?}: {}", skill_file, e);
            }
        }
    }

    skills
}

/// 合并多级目录的 skills：项目级 > 全局 > 内置
/// 同名 skill 高优先级覆盖低优先级
pub fn load_skills(
    workspace_dir: &Path,
    global_dir: &Path,
    builtin: Vec<SkillMeta>,
) -> Vec<SkillMeta> {
    let project_dir = workspace_dir.join(".rrclaw").join("skills");
    let project_skills = scan_skills_dir(&project_dir, SkillSource::Project);
    let global_skills = scan_skills_dir(global_dir, SkillSource::Global);

    // 按优先级合并（后者被前者覆盖）：内置 → 全局 → 项目
    let mut result: Vec<SkillMeta> = Vec::new();

    for skill in builtin.into_iter().chain(global_skills).chain(project_skills) {
        if let Some(pos) = result.iter().position(|s| s.name == skill.name) {
            result[pos] = skill; // 高优先级覆盖
        } else {
            result.push(skill);
        }
    }

    // 按 name 排序，输出稳定
    result.sort_by(|a, b| a.name.cmp(&b.name));
    result
}

/// 按需加载完整 skill 内容（L2 指令 + L3 文件清单）
pub fn load_skill_content(name: &str, skills: &[SkillMeta]) -> Result<SkillContent> {
    let meta = skills
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| {
            let available: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
            eyre!(
                "未找到技能 '{}'。可用技能: {}",
                name,
                if available.is_empty() {
                    "（无）".to_string()
                } else {
                    available.join(", ")
                }
            )
        })?
        .clone();

    // 内置 skill：从编译时嵌入的常量中读取
    let (instructions, resources) = if meta.source == SkillSource::BuiltIn {
        let raw = match meta.name.as_str() {
            "code-review" => BUILTIN_CODE_REVIEW,
            "rust-dev" => BUILTIN_RUST_DEV,
            "git-commit" => BUILTIN_GIT_COMMIT,
            _ => return Err(eyre!("内置技能 '{}' 缺少内容", meta.name)),
        };
        let (_name, _desc, _tags, body) = parse_skill_md(raw)?;
        (body, vec![])
    } else {
        // 文件系统 skill
        let path = meta.path.as_ref().ok_or_else(|| eyre!("skill 路径为空"))?;
        let skill_file = path.join("SKILL.md");
        let content = std::fs::read_to_string(&skill_file)
            .map_err(|e| eyre!("读取 {} 失败: {}", skill_file.display(), e))?;

        let (_name, _desc, _tags, body) = parse_skill_md(&content)?;

        // 列出 L3 资源文件（除 SKILL.md 外的其他文件）
        let resources = list_resources(path);
        (body, resources)
    };

    Ok(SkillContent {
        meta,
        instructions,
        resources,
    })
}

/// 列出目录下除 SKILL.md 外的所有文件（L3 资源清单）
fn list_resources(dir: &Path) -> Vec<String> {
    let mut resources = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(name) = path.file_name() {
                    let name = name.to_string_lossy();
                    if name != "SKILL.md" {
                        resources.push(format!("{}", path.display()));
                    }
                }
            }
        }
    }
    resources.sort();
    resources
}

/// 加载内置 skills 的 L1 元数据
pub fn builtin_skills() -> Vec<SkillMeta> {
    let mut skills = Vec::new();
    let builtins = [
        ("code-review", BUILTIN_CODE_REVIEW),
        ("rust-dev", BUILTIN_RUST_DEV),
        ("git-commit", BUILTIN_GIT_COMMIT),
        ("mcp-install", BUILTIN_MCP_INSTALL),
    ];
    for (key, content) in builtins {
        match parse_skill_md(content) {
            Ok((name, description, tags, _body)) => {
                skills.push(SkillMeta {
                    name,
                    description,
                    tags,
                    source: SkillSource::BuiltIn,
                    path: None,
                });
            }
            Err(e) => {
                tracing::warn!("内置 skill '{}' 解析失败: {}", key, e);
            }
        }
    }
    skills
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_skill(dir: &Path, name: &str, desc: &str, body: &str) {
        let skill_dir = dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        let content = format!(
            "---\nname: {}\ndescription: {}\ntags: [test]\n---\n\n{}",
            name, desc, body
        );
        std::fs::write(skill_dir.join("SKILL.md"), content).unwrap();
    }

    // --- parse_skill_md 测试 ---

    #[test]
    fn parse_valid_frontmatter() {
        let content = "---\nname: my-skill\ndescription: 做某事。当用户需要时使用。\ntags: [dev, test]\n---\n\n# 指令\n做这个做那个。";
        let (name, desc, tags, body) = parse_skill_md(content).unwrap();
        assert_eq!(name, "my-skill");
        assert_eq!(desc, "做某事。当用户需要时使用。");
        assert_eq!(tags, vec!["dev", "test"]);
        assert!(body.contains("# 指令"));
    }

    #[test]
    fn parse_missing_frontmatter_delimiter() {
        let content = "name: my-skill\ndescription: test";
        assert!(parse_skill_md(content).is_err());
    }

    #[test]
    fn parse_unclosed_frontmatter() {
        let content = "---\nname: my-skill\ndescription: test\n";
        assert!(parse_skill_md(content).is_err());
    }

    #[test]
    fn parse_missing_name_field() {
        let content = "---\ndescription: 某功能\n---\n\nbody";
        assert!(parse_skill_md(content).is_err());
    }

    #[test]
    fn parse_missing_description_field() {
        let content = "---\nname: my-skill\n---\n\nbody";
        assert!(parse_skill_md(content).is_err());
    }

    #[test]
    fn parse_empty_tags() {
        let content = "---\nname: my-skill\ndescription: test desc\ntags: []\n---\n\nbody";
        let (_, _, tags, _) = parse_skill_md(content).unwrap();
        assert!(tags.is_empty());
    }

    // --- validate_skill_name 测试 ---

    #[test]
    fn validate_valid_names() {
        assert!(validate_skill_name("code-review").is_ok());
        assert!(validate_skill_name("rust-dev").is_ok());
        assert!(validate_skill_name("abc123").is_ok());
        assert!(validate_skill_name("a").is_ok());
    }

    #[test]
    fn validate_invalid_names() {
        assert!(validate_skill_name("").is_err());
        assert!(validate_skill_name("-starts-with-dash").is_err());
        assert!(validate_skill_name("HasUpperCase").is_err());
        assert!(validate_skill_name("has space").is_err());
        assert!(validate_skill_name("has_underscore").is_err());
        assert!(validate_skill_name(&"a".repeat(65)).is_err());
    }

    // --- scan_skills_dir 测试 ---

    #[test]
    fn scan_empty_dir_returns_empty() {
        let tmp = tempdir().unwrap();
        let skills = scan_skills_dir(tmp.path(), SkillSource::Global);
        assert!(skills.is_empty());
    }

    #[test]
    fn scan_dir_with_skills() {
        let tmp = tempdir().unwrap();
        write_skill(tmp.path(), "skill-a", "描述 A，使用时机 A。", "指令 A");
        write_skill(tmp.path(), "skill-b", "描述 B，使用时机 B。", "指令 B");
        let skills = scan_skills_dir(tmp.path(), SkillSource::Global);
        assert_eq!(skills.len(), 2);
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"skill-a"));
        assert!(names.contains(&"skill-b"));
    }

    #[test]
    fn scan_dir_ignores_dirs_without_skill_md() {
        let tmp = tempdir().unwrap();
        // 只创建目录，不创建 SKILL.md
        std::fs::create_dir(tmp.path().join("empty-dir")).unwrap();
        write_skill(tmp.path(), "valid-skill", "有效技能，测试用。", "指令");
        let skills = scan_skills_dir(tmp.path(), SkillSource::Global);
        assert_eq!(skills.len(), 1);
    }

    // --- load_skills 优先级测试 ---

    #[test]
    fn project_skill_overrides_global() {
        let global_tmp = tempdir().unwrap();
        let workspace_tmp = tempdir().unwrap();

        write_skill(global_tmp.path(), "my-skill", "全局版本，测试用。", "全局指令");

        let project_skills_dir = workspace_tmp.path().join(".rrclaw").join("skills");
        std::fs::create_dir_all(&project_skills_dir).unwrap();
        write_skill(&project_skills_dir, "my-skill", "项目版本，测试用。", "项目指令");

        let skills = load_skills(workspace_tmp.path(), global_tmp.path(), vec![]);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].description, "项目版本，测试用。");
        assert_eq!(skills[0].source, SkillSource::Project);
    }

    #[test]
    fn global_skill_overrides_builtin() {
        let global_tmp = tempdir().unwrap();
        let workspace_tmp = tempdir().unwrap();

        let builtin = vec![SkillMeta {
            name: "code-review".to_string(),
            description: "内置版本，测试用。".to_string(),
            tags: vec![],
            source: SkillSource::BuiltIn,
            path: None,
        }];

        write_skill(
            global_tmp.path(),
            "code-review",
            "全局覆盖版本，测试用。",
            "自定义指令",
        );

        let skills = load_skills(workspace_tmp.path(), global_tmp.path(), builtin);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].description, "全局覆盖版本，测试用。");
        assert_eq!(skills[0].source, SkillSource::Global);
    }

    #[test]
    fn multiple_sources_merged() {
        let global_tmp = tempdir().unwrap();
        let workspace_tmp = tempdir().unwrap();

        let builtin = vec![SkillMeta {
            name: "builtin-only".to_string(),
            description: "内置独有，测试用。".to_string(),
            tags: vec![],
            source: SkillSource::BuiltIn,
            path: None,
        }];

        write_skill(global_tmp.path(), "global-only", "全局独有，测试用。", "指令");

        let project_dir = workspace_tmp.path().join(".rrclaw").join("skills");
        std::fs::create_dir_all(&project_dir).unwrap();
        write_skill(&project_dir, "project-only", "项目独有，测试用。", "指令");

        let skills = load_skills(workspace_tmp.path(), global_tmp.path(), builtin);
        assert_eq!(skills.len(), 3);
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"builtin-only"));
        assert!(names.contains(&"global-only"));
        assert!(names.contains(&"project-only"));
    }

    // --- builtin_skills 测试 ---

    #[test]
    fn builtin_skills_returns_four() {
        let skills = builtin_skills();
        assert_eq!(skills.len(), 4);
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"code-review"));
        assert!(names.contains(&"rust-dev"));
        assert!(names.contains(&"git-commit"));
        assert!(names.contains(&"mcp-install"));
        // 所有内置 skill 都应有非空 description
        for s in &skills {
            assert!(!s.description.is_empty(), "skill '{}' description 为空", s.name);
        }
    }

    // --- load_skill_content 测试 ---

    #[test]
    fn load_builtin_skill_content() {
        let skills = builtin_skills();
        let content = load_skill_content("code-review", &skills).unwrap();
        assert_eq!(content.meta.name, "code-review");
        assert!(!content.instructions.is_empty());
        assert_eq!(content.meta.source, SkillSource::BuiltIn);
    }

    #[test]
    fn load_unknown_skill_returns_error() {
        let skills = builtin_skills();
        let err = load_skill_content("nonexistent", &skills).unwrap_err();
        assert!(err.to_string().contains("未找到技能"));
        assert!(err.to_string().contains("nonexistent"));
    }

    #[test]
    fn load_filesystem_skill_content() {
        let tmp = tempdir().unwrap();
        write_skill(tmp.path(), "test-skill", "测试技能，测试用。", "这是详细指令。");

        let skills = scan_skills_dir(tmp.path(), SkillSource::Global);
        let content = load_skill_content("test-skill", &skills).unwrap();
        assert!(content.instructions.contains("这是详细指令。"));
    }
}
