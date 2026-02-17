# P2: Slash Commands + ConfigTool 实现计划

## 背景

P1 全部完成（流式输出、Supervised 确认、History 持久化、Setup 向导、Telegram Channel）。

当前 REPL 只支持 `exit`/`quit`/`clear` 三个纯文本命令，没有 `/xxx` 斜杠命令体系。
用户还希望 AI 能通过自然语言修改配置（如"把模型换成 claude"），而不是手动编辑 TOML。

**需求**:
1. **基础斜杠命令** — `/help`, `/new`, `/clear` 等快捷操作，直接在 REPL 层处理
2. **ConfigTool** — 注册为 Agent 的 Tool，让 AI 可以读取/修改 `~/.rrclaw/config.toml`

两者互补：斜杠命令用于常用快捷操作，ConfigTool 让 AI 能响应自然语言配置请求。

---

## Part 1: REPL 斜杠命令

### 支持的命令

| 命令 | 功能 | 说明 |
|------|------|------|
| `/help` | 显示帮助 | 列出所有可用命令 |
| `/new` | 新建对话 | 清空 history，开始新的 session |
| `/clear` | 清屏 | 清除终端内容（保留 history） |
| `/config` | 显示配置 | 打印当前 config 摘要（provider、model、autonomy） |
| `/model <name>` | 切换模型 | 运行时临时切换模型（不修改配置文件） |
| `/provider <name>` | 切换 Provider | 运行时临时切换 Provider（不修改配置文件） |

### 实现方案

**改动文件**: `src/channels/cli.rs`

在 `run_repl` 的 input 处理中，现有 `match input` 块之后增加斜杠命令分发：

```rust
// cli.rs — run_repl loop 中
if let Some(cmd) = input.strip_prefix('/') {
    handle_slash_command(cmd, agent, &session_id, memory).await?;
    continue;
}
```

新增函数：
```rust
/// 处理斜杠命令，返回是否需要 continue
async fn handle_slash_command(
    cmd: &str,
    agent: &mut Agent,
    session_id: &str,
    memory: &SqliteMemory,
) -> Result<()> {
    let parts: Vec<&str> = cmd.splitn(2, ' ').collect();
    let name = parts[0];
    let arg = parts.get(1).map(|s| s.trim());

    match name {
        "help" => print_help(),
        "new" => {
            agent.clear_history();
            println!("已开始新对话。");
        }
        "clear" => {
            // 使用 ANSI escape 清屏
            print!("\x1b[2J\x1b[H");
            std::io::stdout().flush()?;
        }
        "config" => {
            // 打印当前配置摘要
            print_config_summary(agent);
        }
        "model" => {
            if let Some(model) = arg {
                agent.set_model(model.to_string());
                println!("模型已切换为: {}", model);
            } else {
                println!("当前模型: {}", agent.model());
                println!("用法: /model <model-name>");
            }
        }
        "provider" => {
            // 需要重新加载 config 获取 provider 配置
            // 这个较复杂，P2 可以先只做 /model
            println!("TODO: /provider 切换尚未实现");
        }
        _ => {
            println!("未知命令: /{}。输入 /help 查看可用命令。", name);
        }
    }
    Ok(())
}
```

### Agent 新增方法

```rust
// agent/loop_.rs
impl Agent {
    /// 清空对话历史（/new 命令用）
    pub fn clear_history(&mut self) {
        self.history.clear();
    }

    /// 获取当前模型名
    pub fn model(&self) -> &str {
        &self.model
    }

    /// 运行时切换模型
    pub fn set_model(&mut self, model: String) {
        self.model = model;
    }

    /// 获取当前温度
    pub fn temperature(&self) -> f64 {
        self.temperature
    }
}
```

### 提交策略

1. `feat: add model/history accessor methods to Agent`
2. `feat: add slash commands to CLI REPL (/help, /new, /clear, /config, /model)`

---

## Part 2: ConfigTool（AI 驱动配置修改）

### 设计思路

ConfigTool 注册为 Agent 的普通 Tool。当用户说"把模型换成 claude"时，AI 调用 ConfigTool 来读取或修改配置。

### Tool 接口

```rust
// src/tools/config.rs
pub struct ConfigTool;

#[async_trait]
impl Tool for ConfigTool {
    fn name(&self) -> &str { "config" }

    fn description(&self) -> &str {
        "读取或修改 RRClaw 配置。支持操作: \
         get（读取配置项）、set（修改配置项）、list（列出所有配置）。\
         修改会立即写入 ~/.rrclaw/config.toml。"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["get", "set", "list"],
                    "description": "操作类型"
                },
                "key": {
                    "type": "string",
                    "description": "配置项路径，如 'default.model', 'security.autonomy', 'providers.deepseek.api_key'"
                },
                "value": {
                    "type": "string",
                    "description": "set 操作时的新值"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _policy: &SecurityPolicy,
    ) -> Result<ToolResult> {
        let action = args.get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("list");

        match action {
            "list" => config_list(),
            "get" => config_get(args.get("key").and_then(|v| v.as_str())),
            "set" => config_set(
                args.get("key").and_then(|v| v.as_str()),
                args.get("value").and_then(|v| v.as_str()),
            ),
            _ => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("未知操作: {}", action)),
            }),
        }
    }
}
```

### 配置读写实现

**读取**: 直接加载 `Config::load_or_init()`，将整个 config 格式化为可读文本返回给 AI。

**修改**: 采用 TOML 原文编辑方式（`toml_edit` crate），避免丢失注释和格式：

```rust
fn config_set(key: Option<&str>, value: Option<&str>) -> Result<ToolResult> {
    let key = key.ok_or_else(|| eyre!("缺少 key 参数"))?;
    let value = value.ok_or_else(|| eyre!("缺少 value 参数"))?;

    let config_path = Config::config_path()?;
    let content = std::fs::read_to_string(&config_path)?;
    let mut doc = content.parse::<toml_edit::DocumentMut>()?;

    // 按 "." 分割 key 路径，逐层访问
    let parts: Vec<&str> = key.split('.').collect();
    // ... 设置值 ...

    std::fs::write(&config_path, doc.to_string())?;

    Ok(ToolResult {
        success: true,
        output: format!("已将 {} 设置为 {}", key, value),
        error: None,
    })
}
```

### 安全考虑

1. **Supervised 模式下受确认保护** — ConfigTool 是普通 Tool，Supervised 模式下会弹出确认提示
2. **pre_validate 检查** — 禁止通过 ConfigTool 修改 `security.autonomy`（防止 AI 自己提权）
3. **API Key 脱敏** — `get`/`list` 操作中，API Key 只显示前 4 字符 + `***`

```rust
fn pre_validate(&self, args: &serde_json::Value, _policy: &SecurityPolicy) -> Option<String> {
    // 禁止修改安全级别
    if let Some(key) = args.get("key").and_then(|v| v.as_str()) {
        if key == "security.autonomy" {
            return Some("不允许通过 AI 修改安全级别，请手动编辑配置文件".to_string());
        }
    }
    None
}
```

### 运行时生效

ConfigTool 修改的是磁盘上的 config.toml。有两种策略：

**方案 A（推荐）**: 仅修改文件，下次启动生效。简单可靠。
- ConfigTool 的返回信息告知用户"配置已保存，部分设置重启后生效"
- 对于 `default.model` 等简单字段，可同时调用 `agent.set_model()` 立即生效

**方案 B**: 修改文件后热重载。复杂度高，暂不做。

### 新增依赖

- `toml_edit` — 保留格式的 TOML 编辑（不丢失注释）

### 注册 Tool

```rust
// src/tools/mod.rs
pub mod config;

pub fn create_tools() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(ShellTool),
        Box::new(FileReadTool),
        Box::new(FileWriteTool),
        Box::new(config::ConfigTool),  // 新增
    ]
}
```

### 提交策略

1. `feat: add toml_edit dependency`
2. `feat: implement ConfigTool for AI-driven config changes`
3. `feat: register ConfigTool in tools registry`
4. `test: add ConfigTool unit tests`

---

## 总提交计划

| # | 提交 | 涉及文件 |
|---|------|---------|
| 1 | feat: add model/history accessor methods to Agent | agent/loop_.rs |
| 2 | feat: add slash commands to CLI REPL | channels/cli.rs |
| 3 | feat: add toml_edit dependency | Cargo.toml |
| 4 | feat: implement ConfigTool | tools/config.rs, tools/mod.rs |
| 5 | test: add ConfigTool unit tests | tools/config.rs |

共 ~5 commits，新增 ~300-400 行代码。

---

## 验证方式

1. **斜杠命令**: REPL 中输入 `/help` 显示帮助，`/new` 清空对话，`/model deepseek-reasoner` 切换模型
2. **ConfigTool**: REPL 中说"把默认模型改成 gpt-4o"，AI 调用 config tool 修改文件，检查 config.toml 已更新
3. **安全**: Supervised 模式下 ConfigTool 执行前弹出确认，尝试修改 `security.autonomy` 被拒绝
4. `cargo test` 全部通过
5. `cargo clippy -- -W clippy::all` 零警告
