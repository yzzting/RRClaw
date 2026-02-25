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
const GLOBAL_FILES: &[IdentityFile] = &[IdentityFile {
    section_name: "用户偏好",
    relative_path: "USER.md",
}];

/// 项目身份文件（相对于 workspace_dir）
const PROJECT_FILES: &[IdentityFile] = &[IdentityFile {
    section_name: "项目行为约定",
    relative_path: ".rrclaw/AGENT.md",
}];

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

    // 辅助闭包：只在内容非纯空白时加入
    let mut push_if_nonempty = |name: &str, content: String| {
        if !content.trim().is_empty() {
            sections.push((name.to_string(), content));
        }
    };

    // 1. 全局用户偏好文件
    for file in GLOBAL_FILES {
        let path = data_dir.join(file.relative_path);
        if let Some(content) = read_file_safe(&path) {
            push_if_nonempty(file.section_name, content);
        }
    }

    // 2. SOUL.md：项目优先，全局兜底
    let project_soul_path = workspace_dir.join(SOUL_PROJECT);
    let global_soul_path = data_dir.join(SOUL_GLOBAL);

    if let Some(content) = read_file_safe(&project_soul_path) {
        push_if_nonempty("Agent 人格（项目级）", content);
    } else if let Some(content) = read_file_safe(&global_soul_path) {
        push_if_nonempty("Agent 人格", content);
    }

    // 3. 项目行为约定文件
    for file in PROJECT_FILES {
        let path = workspace_dir.join(file.relative_path);
        if let Some(content) = read_file_safe(&path) {
            push_if_nonempty(file.section_name, content);
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
/// - 文件不存在：返回 None（静默，不报错）
/// - 超出大小限制：在 UTF-8 字符边界截断后返回
/// - 空文件或纯空白：由调用方过滤
fn read_file_safe(path: &Path) -> Option<String> {
    // 直接读取，在 Err 分支区分"不存在"与其他 IO 错误，避免 exists() 的 TOCTOU 窗口
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

            // 在 UTF-8 字符边界处截断：valid_up_to() 保证前缀合法，safe unwrap
            match std::str::from_utf8(truncated) {
                Ok(s) => Some(s.to_string()),
                Err(e) => {
                    let valid_up_to = e.valid_up_to();
                    if valid_up_to == 0 {
                        return None;
                    }
                    let s = std::str::from_utf8(&truncated[..valid_up_to])
                        .expect("valid_up_to guarantees valid UTF-8");
                    Some(format!("{}\n\n[文件内容已截断]", s))
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
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

    // --- load_identity_context 测试 ---

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
    fn whitespace_only_file_returns_none() {
        let workspace = tempdir().unwrap();
        let data_dir = tempdir().unwrap();
        write_file(data_dir.path(), "USER.md", "   \n\n  ");

        // 纯空白文件应被过滤，避免生成只有标题没有内容的空 section 注入 system prompt
        let result = load_identity_context(workspace.path(), data_dir.path());
        assert!(result.is_none(), "纯空白文件不应生成任何 identity context");
    }

    // --- read_file_safe 测试 ---

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
