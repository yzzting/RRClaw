/// 预定义工具分组，用于 Phase 1.5 关键词路由
///
/// 根据用户输入的关键词，决定本轮对话只暴露哪些工具给 LLM。
/// 无匹配时降级为暴露所有工具（当前默认行为）。
pub struct ToolGroup {
    /// 分组名称（用于日志）
    pub name: &'static str,
    /// 匹配关键词（中英文均支持，contains 检测）
    pub keywords: &'static [&'static str],
    /// 包含的工具名（精确匹配 Tool::name()）
    pub tools: &'static [&'static str],
}

pub static TOOL_GROUPS: &[ToolGroup] = &[
    ToolGroup {
        name: "file_ops",
        keywords: &[
            "文件",
            "读",
            "写",
            "改",
            "编辑",
            "查看代码",
            "代码",
            "read",
            "write",
            "edit",
            "file",
        ],
        tools: &["file_read", "file_write", "shell", "git"],
    },
    ToolGroup {
        name: "web",
        keywords: &[
            "请求", "HTTP", "API", "天气", "网络", "http", "request", "fetch", "api", "url", "URL",
        ],
        tools: &["http_request"],
    },
    ToolGroup {
        name: "memory",
        keywords: &[
            "记住", "记忆", "存储", "recall", "store", "memory", "记得", "忘了",
        ],
        tools: &["memory_store", "memory_recall", "memory_forget"],
    },
    ToolGroup {
        name: "config",
        keywords: &["配置", "设置", "config", "RRClaw", "自身", "info"],
        tools: &["config", "self_info"],
    },
    ToolGroup {
        name: "git_ops",
        keywords: &[
            "提交", "commit", "push", "pull", "分支", "branch", "git", "版本", "stash",
        ],
        tools: &["git", "shell"],
    },
    ToolGroup {
        name: "routine",
        keywords: &["定时", "routine", "schedule", "cron", "周期"],
        tools: &["routine"],
    },
];

/// 根据用户输入关键词，返回应该暴露的工具名列表。
///
/// - 返回空 Vec 表示无匹配，调用方应降级为暴露所有工具。
/// - 返回非空 Vec 时，调用方额外追加 `skill` 工具（始终可用）。
/// - 多组匹配时取并集，工具名自动去重。
pub fn route_tools(user_input: &str) -> Vec<String> {
    let input_lower = user_input.to_lowercase();
    let mut matched: Vec<&str> = Vec::new();

    for group in TOOL_GROUPS {
        if group
            .keywords
            .iter()
            .any(|kw| input_lower.contains(&kw.to_lowercase()))
        {
            for tool in group.tools {
                if !matched.contains(tool) {
                    matched.push(tool);
                }
            }
        }
    }

    matched.into_iter().map(String::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_keywords_route_to_file_ops() {
        let result = route_tools("帮我读一下这个文件");
        assert!(
            result.contains(&"file_read".to_string()),
            "file_read missing: {:?}",
            result
        );
        assert!(result.contains(&"file_write".to_string()));
        assert!(result.contains(&"shell".to_string()));
    }

    #[test]
    fn git_keywords_route_to_git_ops() {
        let result = route_tools("帮我 commit 一下改动");
        assert!(
            result.contains(&"git".to_string()),
            "git missing: {:?}",
            result
        );
        assert!(result.contains(&"shell".to_string()));
    }

    #[test]
    fn memory_keywords_route_to_memory() {
        let result = route_tools("请记住我的名字是张三");
        assert!(
            result.contains(&"memory_store".to_string()),
            "memory_store missing: {:?}",
            result
        );
        assert!(result.contains(&"memory_recall".to_string()));
    }

    #[test]
    fn config_keywords_route_to_config() {
        let result = route_tools("帮我配置一下 RRClaw");
        assert!(
            result.contains(&"config".to_string()),
            "config missing: {:?}",
            result
        );
        assert!(result.contains(&"self_info".to_string()));
    }

    #[test]
    fn routine_keywords_route_to_routine() {
        let result = route_tools("创建一个定时任务");
        assert!(
            result.contains(&"routine".to_string()),
            "routine missing: {:?}",
            result
        );
    }

    #[test]
    fn no_match_returns_empty() {
        let result = route_tools("讲一个笑话");
        assert!(result.is_empty(), "expected empty, got: {:?}", result);
    }

    #[test]
    fn unrelated_greeting_returns_empty() {
        let result = route_tools("你好，能帮我做什么");
        assert!(result.is_empty(), "expected empty, got: {:?}", result);
    }

    #[test]
    fn multi_group_returns_union() {
        // "改" matches file_ops, "git" matches git_ops
        let result = route_tools("改完代码后 git push");
        assert!(result.contains(&"file_read".to_string()));
        assert!(result.contains(&"git".to_string()));
    }

    #[test]
    fn shell_tool_not_duplicated() {
        // Both file_ops and git_ops include "shell"
        let result = route_tools("改代码然后 git commit");
        let shell_count = result.iter().filter(|t| t.as_str() == "shell").count();
        assert_eq!(
            shell_count, 1,
            "shell should not be duplicated, got: {:?}",
            result
        );
    }

    #[test]
    fn english_keywords_also_work() {
        let result = route_tools("please read this file");
        assert!(
            result.contains(&"file_read".to_string()),
            "file_read missing: {:?}",
            result
        );
    }

    #[test]
    fn git_english_keywords() {
        let result = route_tools("git push to origin");
        assert!(result.contains(&"git".to_string()));
    }
}
