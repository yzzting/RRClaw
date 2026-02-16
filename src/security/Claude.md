# Security 模块

## 职责
提供安全策略控制，限制 AI Agent 的工具执行范围。

## 核心类型

### AutonomyLevel
```rust
pub enum AutonomyLevel {
    ReadOnly,    // 不执行任何工具
    Supervised,  // 需用户确认
    Full,        // 自主执行
}
```

### SecurityPolicy
```rust
pub struct SecurityPolicy {
    pub autonomy: AutonomyLevel,
    pub allowed_commands: Vec<String>,
    pub workspace_dir: PathBuf,
    pub blocked_paths: Vec<PathBuf>,
}
```

## 方法
- `is_command_allowed(cmd: &str) -> bool` — 提取命令基础名，检查白名单
- `is_path_allowed(path: &Path) -> bool` — canonicalize 路径，检查 workspace 范围，拒绝 symlink 逃逸
- `requires_confirmation() -> bool` — Supervised 返回 true

## 默认值
- autonomy: `Supervised`
- allowed_commands: `["ls", "cat", "grep", "find", "echo", "pwd", "git", "head", "tail", "wc"]`
- blocked_paths: `["/etc", "/usr", "/bin", "/sbin", "/var", "/tmp", "/root"]`

## 测试用例
1. 白名单命令通过，非白名单命令拒绝
2. workspace 内路径通过，外部路径拒绝
3. `../` 路径遍历拒绝
4. symlink 指向 workspace 外的路径拒绝
5. ReadOnly 模式 requires_confirmation 返回 false（因为根本不执行）
6. Supervised 返回 true，Full 返回 false
