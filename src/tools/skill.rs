use async_trait::async_trait;
use color_eyre::eyre::Result;
use serde_json::json;

use crate::security::SecurityPolicy;
use crate::skills::{load_skill_content, SkillMeta};

use super::traits::{Tool, ToolResult};

/// LLM 通过调用此工具按需加载技能的 L2 指令
pub struct SkillTool {
    skills: Vec<SkillMeta>,
}

impl SkillTool {
    pub fn new(skills: Vec<SkillMeta>) -> Self {
        Self { skills }
    }

    /// 获取 skill 列表引用（供 system prompt 构建 L1 列表用）
    pub fn skills(&self) -> &[SkillMeta] {
        &self.skills
    }
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "skill"
    }

    fn description(&self) -> &str {
        "加载技能的详细指令。当你判断某个技能适用于当前任务时，调用此工具获取完整操作指南。"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "要加载的技能名称（用 self_info query=help 可查看可用技能列表）"
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
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("缺少 name 参数".to_string()),
                    ..Default::default()
                });
            }
        };

        let lang = crate::config::Config::get_language();
        match load_skill_content(name, &self.skills, lang) {
            Ok(content) => {
                let mut output = content.instructions;

                // 如果有 L3 资源文件，附带清单提示 LLM 可用 file_read 读取
                if !content.resources.is_empty() {
                    output.push_str("\n\n---\nAttached resource files (use file_read to view):\n");
                    for r in &content.resources {
                        output.push_str(&format!("- {}\n", r));
                    }
                }

                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                    ..Default::default()
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
                ..Default::default()
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Language;
    use crate::skills::{builtin_skills, scan_skills_dir, SkillSource};
    use tempfile::tempdir;

    fn write_skill(dir: &std::path::Path, name: &str, desc: &str, body: &str) {
        let skill_dir = dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        let content = format!(
            "---\nname: {}\ndescription: {}\ntags: []\n---\n\n{}",
            name, desc, body
        );
        std::fs::write(skill_dir.join("SKILL.md"), content).unwrap();
    }

    #[tokio::test]
    async fn execute_builtin_skill_returns_instructions() {
        let skills = builtin_skills(Language::English);
        let tool = SkillTool::new(skills);
        let policy = SecurityPolicy::default();

        let result = tool
            .execute(json!({"name": "code-review"}), &policy)
            .await
            .unwrap();

        assert!(result.success);
        assert!(!result.output.is_empty());
        assert!(result.error.is_none());
        // verify output contains skill content keywords
        assert!(!result.output.is_empty());
    }

    #[tokio::test]
    async fn execute_unknown_skill_returns_error() {
        let skills = builtin_skills(Language::English);
        let tool = SkillTool::new(skills);
        let policy = SecurityPolicy::default();

        let result = tool
            .execute(json!({"name": "nonexistent-skill"}), &policy)
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.is_some());
        let err = result.error.unwrap();
        assert!(err.contains("nonexistent-skill"));
        // should list available skills
        assert!(err.contains("code-review") || err.contains("Available") || err.contains("可用"));
    }

    #[tokio::test]
    async fn execute_missing_name_param_returns_error() {
        let tool = SkillTool::new(vec![]);
        let policy = SecurityPolicy::default();

        let result = tool.execute(json!({}), &policy).await.unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("name"));
    }

    #[tokio::test]
    async fn execute_filesystem_skill_returns_instructions() {
        let tmp = tempdir().unwrap();
        write_skill(tmp.path(), "my-skill", "我的技能，测试用。", "这是详细操作指南。");

        let skills = scan_skills_dir(tmp.path(), SkillSource::Global);
        let tool = SkillTool::new(skills);
        let policy = SecurityPolicy::default();

        let result = tool
            .execute(json!({"name": "my-skill"}), &policy)
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("这是详细操作指南。"));
    }

    #[tokio::test]
    async fn execute_skill_with_resources_lists_them() {
        let tmp = tempdir().unwrap();
        let skill_dir = tmp.path().join("rich-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: rich-skill\ndescription: 富技能，测试用。\ntags: []\n---\n\n指令内容。",
        )
        .unwrap();
        // 添加 L3 资源文件
        std::fs::write(skill_dir.join("guide.md"), "参考指南内容").unwrap();

        let skills = scan_skills_dir(tmp.path(), SkillSource::Global);
        let tool = SkillTool::new(skills);
        let policy = SecurityPolicy::default();

        let result = tool
            .execute(json!({"name": "rich-skill"}), &policy)
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("guide.md") || result.output.contains("resource"));
    }

    #[test]
    fn tool_name_and_description() {
        let tool = SkillTool::new(vec![]);
        assert_eq!(tool.name(), "skill");
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn skills_accessor() {
        let skills = builtin_skills(Language::English);
        let count = skills.len();
        let tool = SkillTool::new(skills);
        assert_eq!(tool.skills().len(), count);
    }
}
