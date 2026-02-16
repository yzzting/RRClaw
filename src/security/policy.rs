use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AutonomyLevel {
    ReadOnly,
    Supervised,
    Full,
}

#[derive(Debug, Clone)]
pub struct SecurityPolicy {
    pub autonomy: AutonomyLevel,
    pub allowed_commands: Vec<String>,
    pub workspace_dir: PathBuf,
    pub blocked_paths: Vec<PathBuf>,
}

impl Default for SecurityPolicy {
    fn default() -> Self {
        Self {
            autonomy: AutonomyLevel::Supervised,
            allowed_commands: vec![
                "ls", "cat", "grep", "find", "echo", "pwd", "git", "head", "tail", "wc",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            workspace_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            blocked_paths: vec![
                PathBuf::from("/etc"),
                PathBuf::from("/usr"),
                PathBuf::from("/bin"),
                PathBuf::from("/sbin"),
                PathBuf::from("/var"),
                PathBuf::from("/tmp"),
                PathBuf::from("/root"),
            ],
        }
    }
}

impl SecurityPolicy {
    /// 检查命令是否在白名单中（只检查基础命令名）
    pub fn is_command_allowed(&self, cmd: &str) -> bool {
        let base_cmd = cmd
            .split_whitespace()
            .next()
            .unwrap_or("")
            .rsplit('/')
            .next()
            .unwrap_or("");

        self.allowed_commands.iter().any(|c| c == base_cmd)
    }

    /// 检查路径是否在 workspace 范围内
    /// 会 canonicalize 路径以防 symlink 和 `..` 逃逸
    pub fn is_path_allowed(&self, path: &Path) -> bool {
        // 先尝试 canonicalize（解析 symlink 和 ..）
        let resolved = match path.canonicalize() {
            Ok(p) => p,
            Err(_) => {
                // 文件可能不存在，用 workspace_dir 拼接后检查
                let joined = if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    self.workspace_dir.join(path)
                };
                let normalized = normalize_path(&joined);
                // 向上查找可 canonicalize 的祖先目录，解析 symlink
                canonicalize_with_ancestors(&normalized)
            }
        };

        // 必须是 workspace_dir 的子路径
        let workspace_canonical = self
            .workspace_dir
            .canonicalize()
            .unwrap_or_else(|_| self.workspace_dir.clone());

        if !resolved.starts_with(&workspace_canonical) {
            return false;
        }

        // 检查是否命中 blocked_paths
        for blocked in &self.blocked_paths {
            if resolved.starts_with(blocked) {
                return false;
            }
        }

        true
    }

    /// Supervised 模式下需要用户确认
    pub fn requires_confirmation(&self) -> bool {
        self.autonomy == AutonomyLevel::Supervised
    }

    /// ReadOnly 模式下不允许执行任何工具
    pub fn allows_execution(&self) -> bool {
        self.autonomy != AutonomyLevel::ReadOnly
    }
}

/// 手动规范化路径（处理 `.` 和 `..`，不访问文件系统）
fn normalize_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                components.pop();
            }
            std::path::Component::CurDir => {}
            other => components.push(other),
        }
    }
    components.iter().collect()
}

/// 向上查找可 canonicalize 的祖先目录，解析中间 symlink
/// 例如 /var/folders/.../sub/dir/file.txt，如果 sub/dir 不存在，
/// 会 canonicalize /var/folders/... 再拼接 sub/dir/file.txt
fn canonicalize_with_ancestors(path: &Path) -> PathBuf {
    let mut current = path.to_path_buf();
    let mut suffix_parts = Vec::new();

    loop {
        match current.canonicalize() {
            Ok(canonical) => {
                let mut result = canonical;
                for part in suffix_parts.into_iter().rev() {
                    result = result.join(part);
                }
                return result;
            }
            Err(_) => {
                if let Some(file_name) = current.file_name() {
                    suffix_parts.push(file_name.to_os_string());
                    current = current
                        .parent()
                        .map(|p| p.to_path_buf())
                        .unwrap_or(current);
                } else {
                    // 到达根目录仍无法 canonicalize，返回原路径
                    return path.to_path_buf();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_policy(workspace: &Path) -> SecurityPolicy {
        SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            allowed_commands: vec!["ls", "cat", "grep", "git"]
                .into_iter()
                .map(String::from)
                .collect(),
            workspace_dir: workspace.to_path_buf(),
            blocked_paths: vec![PathBuf::from("/etc"), PathBuf::from("/root")],
        }
    }

    #[test]
    fn allowed_command_passes() {
        let policy = test_policy(Path::new("/tmp/test_workspace"));
        assert!(policy.is_command_allowed("ls"));
        assert!(policy.is_command_allowed("ls -la"));
        assert!(policy.is_command_allowed("cat file.txt"));
        assert!(policy.is_command_allowed("git status"));
    }

    #[test]
    fn disallowed_command_rejected() {
        let policy = test_policy(Path::new("/tmp/test_workspace"));
        assert!(!policy.is_command_allowed("rm -rf /"));
        assert!(!policy.is_command_allowed("sudo anything"));
        assert!(!policy.is_command_allowed("curl http://evil.com"));
        assert!(!policy.is_command_allowed("ssh root@server"));
    }

    #[test]
    fn command_with_full_path_extracts_basename() {
        let policy = test_policy(Path::new("/tmp/test_workspace"));
        assert!(policy.is_command_allowed("/usr/bin/ls -la"));
        assert!(!policy.is_command_allowed("/usr/bin/rm file"));
    }

    #[test]
    fn empty_command_rejected() {
        let policy = test_policy(Path::new("/tmp/test_workspace"));
        assert!(!policy.is_command_allowed(""));
        assert!(!policy.is_command_allowed("  "));
    }

    #[test]
    fn path_inside_workspace_allowed() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path();
        let policy = test_policy(workspace);

        let test_file = workspace.join("test.txt");
        std::fs::write(&test_file, "hello").unwrap();

        assert!(policy.is_path_allowed(&test_file));
    }

    #[test]
    fn path_outside_workspace_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let policy = test_policy(tmp.path());

        assert!(!policy.is_path_allowed(Path::new("/etc/passwd")));
        assert!(!policy.is_path_allowed(Path::new("/root/.ssh/id_rsa")));
    }

    #[test]
    fn path_traversal_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let policy = test_policy(tmp.path());

        assert!(!policy.is_path_allowed(Path::new("../../../etc/passwd")));
    }

    #[test]
    fn symlink_escape_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path();
        let policy = test_policy(workspace);

        let link_path = workspace.join("evil_link");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/passwd", &link_path).unwrap();
            assert!(!policy.is_path_allowed(&link_path));
        }
    }

    #[test]
    fn autonomy_levels() {
        let mut policy = SecurityPolicy::default();

        policy.autonomy = AutonomyLevel::ReadOnly;
        assert!(!policy.allows_execution());
        assert!(!policy.requires_confirmation());

        policy.autonomy = AutonomyLevel::Supervised;
        assert!(policy.allows_execution());
        assert!(policy.requires_confirmation());

        policy.autonomy = AutonomyLevel::Full;
        assert!(policy.allows_execution());
        assert!(!policy.requires_confirmation());
    }

    #[test]
    fn default_policy_is_supervised() {
        let policy = SecurityPolicy::default();
        assert_eq!(policy.autonomy, AutonomyLevel::Supervised);
        assert!(policy.requires_confirmation());
    }
}
