# Security 模块设计文档

提供安全策略控制和 Prompt Injection 检测，限制 AI Agent 的工具执行范围和防御外部数据投毒。

## SecurityPolicy

```rust
pub struct SecurityPolicy {
    pub autonomy: AutonomyLevel,
    pub allowed_commands: Vec<String>,
    pub workspace_dir: PathBuf,
    pub blocked_paths: Vec<PathBuf>,
    pub injection_check: bool,   // P4 新增，默认 true
}

pub enum AutonomyLevel {
    ReadOnly,    // 不执行任何工具
    Supervised,  // 需用户确认
    Full,        // 自主执行
}
```

### 关键方法

- `is_command_allowed(cmd)` — 提取基础命令名（去路径），检查白名单
- `is_path_allowed(path)` — canonicalize（解析 symlink）→ 检查 workspace 范围 → 拒绝逃逸
- `requires_confirmation()` — Supervised 返回 true

**macOS symlink 坑**：`/var` 是 `/private/var` 的 symlink，canonicalize 时需要兼容处理，已用 `canonicalize_with_ancestors` 修复。

### 默认值

- autonomy: `Supervised`
- allowed_commands: `["ls","cat","grep","find","echo","pwd","git","head","tail","wc","cargo","rustc"]`
- blocked_paths: `["/etc","/usr","/bin","/sbin","/var","/tmp","/root"]`
- injection_check: `true`

## Prompt Injection 检测（P4）

模块：`src/security/injection.rs`

检测工具执行结果中的 prompt injection 攻击，在工具结果推入 history 前调用。

### 检测级别

| 级别 | 行为 | 触发条件 |
|------|------|---------|
| `Block` | 替换为警告文本，拒绝传给 LLM | 直接包含注入指令（"ignore previous instructions" 等关键词） |
| `Review` | 记录 WARN 日志，内容通过 | 空行比例异常（> 1行/40字节），可能用于隐藏注入内容 |
| `Warn` | 记录 INFO 日志，内容通过 | 控制字符（\x00、\x0b、\x0c 等） |

### needs_injection_check()

**只检测外部数据工具**：

```rust
fn needs_injection_check(tool_name: &str) -> bool {
    matches!(tool_name, "shell" | "file_read" | "file_write" | "git" | "http_request")
}
```

跳过内部工具（memory_*、skill、self_info、config、routine）：
- 这些工具返回受控内容，不来自外部攻击面
- memory_recall 返回格式化列表，行数多，曾误触发 Review WARN（P5 修复）

### InjectionResult

```rust
pub struct InjectionResult {
    pub severity: Option<InjectionSeverity>,
    pub reason: Option<String>,
    pub sanitized: String,  // Block 时替换，其他情况等于原始内容
}
```

## 文件结构

```
src/security/
├── Claude.md      # 本文件
├── mod.rs         # 模块入口 + re-exports
├── policy.rs      # SecurityPolicy + AutonomyLevel
└── injection.rs   # check_tool_result() + InjectionSeverity + needs_injection_check()
```

## 测试要求

- SecurityPolicy：白名单、路径沙箱、symlink 防逃逸（已有）
- injection：Block/Review/Warn 各级别触发（已有）
- needs_injection_check：内部工具跳过，外部工具检测（已有）
