use async_trait::async_trait;
use color_eyre::eyre::Result;
use serde_json::json;

use crate::config::Config;
use crate::security::SecurityPolicy;

use super::traits::{Tool, ToolResult};

/// AI 驱动的配置读写工具
pub struct ConfigTool;

#[async_trait]
impl Tool for ConfigTool {
    fn name(&self) -> &str {
        "config"
    }

    fn description(&self) -> &str {
        "读取或修改 RRClaw 配置。支持操作: \
         get（读取配置项）、set（修改已有配置项）、list（列出所有配置）、\
         append（追加新配置段，用于添加 MCP server 等新节）。\
         修改会写入 ~/.rrclaw/config.toml，部分设置重启后生效。"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["get", "set", "list", "append"],
                    "description": "操作类型: get 读取, set 修改已有项, list 列出全部, append 追加新配置段（如 MCP server）"
                },
                "key": {
                    "type": "string",
                    "description": "配置项路径，用 . 分隔。如 'default.model', 'security.autonomy', 'providers.deepseek.model'"
                },
                "value": {
                    "type": "string",
                    "description": "set 操作时的新值；append 操作时为要追加的 TOML 文本（如 '[mcp.servers.xxx]\\ntransport = \"stdio\"\\n...'）"
                }
            },
            "required": ["action"]
        })
    }

    fn pre_validate(&self, args: &serde_json::Value, _policy: &SecurityPolicy) -> Option<String> {
        if let Some(action) = args.get("action").and_then(|v| v.as_str()) {
            if action == "set" {
                if let Some(key) = args.get("key").and_then(|v| v.as_str()) {
                    if key == "security.autonomy" {
                        return Some(
                            "不允许通过 AI 修改安全级别，请手动编辑配置文件".to_string(),
                        );
                    }
                }
            }
        }
        None
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _policy: &SecurityPolicy,
    ) -> Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("list");

        match action {
            "list" => config_list(),
            "get" => config_get(args.get("key").and_then(|v| v.as_str())),
            "set" => config_set(
                args.get("key").and_then(|v| v.as_str()),
                args.get("value").and_then(|v| v.as_str()),
            ),
            "append" => config_append(args.get("value").and_then(|v| v.as_str())),
            _ => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("未知操作: {}", action)),
                ..Default::default()
            }),
        }
    }
}

/// 列出所有配置（API Key 脱敏）
fn config_list() -> Result<ToolResult> {
    let config_path = Config::config_path()?;
    let content = std::fs::read_to_string(&config_path)?;
    let sanitized = sanitize_api_keys(&content);
    Ok(ToolResult {
        success: true,
        output: sanitized,
        error: None,
        ..Default::default()
    })
}

/// 读取指定配置项
fn config_get(key: Option<&str>) -> Result<ToolResult> {
    let key = match key {
        Some(k) => k,
        None => {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("缺少 key 参数".to_string()),
                ..Default::default()
            });
        }
    };

    let config_path = Config::config_path()?;
    let content = std::fs::read_to_string(&config_path)?;
    let doc = content
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| color_eyre::eyre::eyre!("解析配置文件失败: {}", e))?;

    let parts: Vec<&str> = key.split('.').collect();
    let value = navigate_toml(&doc, &parts);

    match value {
        Some(v) => {
            let display = v.to_string().trim().to_string();
            // 脱敏 API Key
            let display = if key.ends_with("api_key") {
                sanitize_single_key(&display)
            } else {
                display
            };
            Ok(ToolResult {
                success: true,
                output: format!("{} = {}", key, display),
                error: None,
                ..Default::default()
            })
        }
        None => Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!("配置项 '{}' 不存在", key)),
            ..Default::default()
        }),
    }
}

/// 修改指定配置项
fn config_set(key: Option<&str>, value: Option<&str>) -> Result<ToolResult> {
    let key = match key {
        Some(k) => k,
        None => {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("缺少 key 参数".to_string()),
                ..Default::default()
            });
        }
    };
    let value = match value {
        Some(v) => v,
        None => {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("缺少 value 参数".to_string()),
                ..Default::default()
            });
        }
    };

    let config_path = Config::config_path()?;
    let content = std::fs::read_to_string(&config_path)?;
    let mut doc = content
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| color_eyre::eyre::eyre!("解析配置文件失败: {}", e))?;

    let parts: Vec<&str> = key.split('.').collect();
    if !set_toml_value(&mut doc, &parts, value) {
        return Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!("无法设置配置项 '{}'，路径不存在或不合法", key)),
            ..Default::default()
        });
    }

    std::fs::write(&config_path, doc.to_string())?;

    Ok(ToolResult {
        success: true,
        output: format!("已将 {} 设置为 {}。部分设置重启后生效。", key, value),
        error: None,
        ..Default::default()
    })
}

/// 追加新配置段到 config.toml（用于添加 MCP server 等新节）
fn config_append(value: Option<&str>) -> Result<ToolResult> {
    let toml_text = match value {
        Some(v) if !v.trim().is_empty() => v,
        _ => {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("缺少 value 参数（要追加的 TOML 内容）".to_string()),
                ..Default::default()
            });
        }
    };

    // 验证追加内容是合法的 TOML
    if let Err(e) = toml_text.parse::<toml_edit::DocumentMut>() {
        return Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!("追加内容不是合法的 TOML: {}", e)),
            ..Default::default()
        });
    }

    let config_path = Config::config_path()?;
    let existing = std::fs::read_to_string(&config_path)?;

    // 确保文件末尾有换行，再追加新内容
    let mut new_content = existing;
    if !new_content.ends_with('\n') {
        new_content.push('\n');
    }
    new_content.push('\n');
    new_content.push_str(toml_text.trim_start());
    if !new_content.ends_with('\n') {
        new_content.push('\n');
    }

    std::fs::write(&config_path, &new_content)?;

    Ok(ToolResult {
        success: true,
        output: "配置已追加，重启 RRClaw 后生效。".to_string(),
        error: None,
        ..Default::default()
    })
}

/// 在 TOML 文档中按路径导航获取值
fn navigate_toml<'a>(doc: &'a toml_edit::DocumentMut, parts: &[&str]) -> Option<&'a toml_edit::Item> {
    let mut current: &toml_edit::Item = doc.as_item();
    for part in parts {
        current = current.get(part)?;
    }
    Some(current)
}

/// 在 TOML 文档中按路径设置值
fn set_toml_value(doc: &mut toml_edit::DocumentMut, parts: &[&str], value: &str) -> bool {
    if parts.is_empty() {
        return false;
    }

    // 先用只读方式验证路径存在
    {
        let mut check: &toml_edit::Item = doc.as_item();
        for part in parts {
            match check.get(part) {
                Some(item) => check = item,
                None => return false,
            }
        }
    }

    // 导航到倒数第二层
    let mut current: &mut toml_edit::Item = doc.as_item_mut();
    for part in &parts[..parts.len() - 1] {
        match current.get_mut(part) {
            Some(item) => current = item,
            None => return false,
        }
    }

    let last_key = parts[parts.len() - 1];

    // 检查目标是否存在，根据原值类型决定新值类型
    let existing = current.get(last_key);
    let new_value = match existing {
        Some(item) if item.is_bool() => {
            match value.to_lowercase().as_str() {
                "true" => toml_edit::value(true),
                "false" => toml_edit::value(false),
                _ => toml_edit::value(value),
            }
        }
        Some(item) if item.is_float() => {
            if let Ok(f) = value.parse::<f64>() {
                toml_edit::value(f)
            } else {
                toml_edit::value(value)
            }
        }
        Some(item) if item.is_integer() => {
            if let Ok(i) = value.parse::<i64>() {
                toml_edit::value(i)
            } else {
                toml_edit::value(value)
            }
        }
        _ => toml_edit::value(value),
    };

    match current.get_mut(last_key) {
        Some(item) => {
            *item = new_value;
            true
        }
        None => false,
    }
}

/// 对配置内容中的 API Key 进行脱敏
fn sanitize_api_keys(content: &str) -> String {
    let mut result = String::new();
    for line in content.lines() {
        if line.trim_start().starts_with("api_key") {
            // 找到等号后的值
            if let Some(eq_pos) = line.find('=') {
                let prefix = &line[..=eq_pos];
                let raw_value = line[eq_pos + 1..].trim().trim_matches('"');
                result.push_str(prefix);
                result.push(' ');
                result.push('"');
                result.push_str(&sanitize_single_key(raw_value));
                result.push('"');
            } else {
                result.push_str(line);
            }
        } else {
            result.push_str(line);
        }
        result.push('\n');
    }
    result
}

/// 对单个 API Key 值进行脱敏：显示前4字符 + ***
fn sanitize_single_key(key: &str) -> String {
    let key = key.trim_matches('"');
    if key.len() <= 4 {
        "***".to_string()
    } else {
        format!("{}***", &key[..4])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_single_key_short() {
        assert_eq!(sanitize_single_key("abc"), "***");
    }

    #[test]
    fn sanitize_single_key_long() {
        assert_eq!(sanitize_single_key("sk-abcdefgh"), "sk-a***");
    }

    #[test]
    fn sanitize_api_keys_in_content() {
        let content = r#"[providers.deepseek]
base_url = "https://api.deepseek.com/v1"
api_key = "sk-secret-key-12345"
model = "deepseek-chat"
"#;
        let result = sanitize_api_keys(content);
        assert!(result.contains("sk-s***"));
        assert!(!result.contains("sk-secret-key-12345"));
        assert!(result.contains("deepseek-chat")); // model 不受影响
    }

    #[test]
    fn pre_validate_blocks_autonomy_change() {
        let tool = ConfigTool;
        let args = serde_json::json!({
            "action": "set",
            "key": "security.autonomy",
            "value": "full"
        });
        let policy = SecurityPolicy::default();
        assert!(tool.pre_validate(&args, &policy).is_some());
    }

    #[test]
    fn pre_validate_allows_normal_set() {
        let tool = ConfigTool;
        let args = serde_json::json!({
            "action": "set",
            "key": "default.model",
            "value": "gpt-4o"
        });
        let policy = SecurityPolicy::default();
        assert!(tool.pre_validate(&args, &policy).is_none());
    }

    #[test]
    fn pre_validate_allows_get() {
        let tool = ConfigTool;
        let args = serde_json::json!({
            "action": "get",
            "key": "security.autonomy"
        });
        let policy = SecurityPolicy::default();
        assert!(tool.pre_validate(&args, &policy).is_none());
    }

    #[tokio::test]
    async fn config_set_and_get_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(
            &config_path,
            r#"[default]
provider = "deepseek"
model = "deepseek-chat"
temperature = 0.7
"#,
        )
        .unwrap();

        // 直接测试 set_toml_value 和 navigate_toml
        let content = std::fs::read_to_string(&config_path).unwrap();
        let mut doc = content.parse::<toml_edit::DocumentMut>().unwrap();

        assert!(set_toml_value(&mut doc, &["default", "model"], "gpt-4o"));
        std::fs::write(&config_path, doc.to_string()).unwrap();

        let content = std::fs::read_to_string(&config_path).unwrap();
        let doc = content.parse::<toml_edit::DocumentMut>().unwrap();
        let val = navigate_toml(&doc, &["default", "model"]).unwrap();
        assert_eq!(val.as_str(), Some("gpt-4o"));
    }

    #[test]
    fn set_toml_value_preserves_type() {
        let content = r#"[default]
temperature = 0.7
"#;
        let mut doc = content.parse::<toml_edit::DocumentMut>().unwrap();
        assert!(set_toml_value(&mut doc, &["default", "temperature"], "0.5"));
        let val = navigate_toml(&doc, &["default", "temperature"]).unwrap();
        assert_eq!(val.as_float(), Some(0.5));
    }

    #[test]
    fn set_toml_value_nonexistent_key_fails() {
        let content = r#"[default]
model = "test"
"#;
        let mut doc = content.parse::<toml_edit::DocumentMut>().unwrap();
        assert!(!set_toml_value(&mut doc, &["nonexistent", "key"], "value"));
    }

    #[test]
    fn config_append_adds_new_section() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[default]\nprovider = \"deepseek\"\n",
        )
        .unwrap();

        // 测试 config_append 的核心逻辑：追加后文件包含新 section
        let new_toml = "[mcp.servers.test]\ntransport = \"stdio\"\ncommand = \"npx\"\n";
        let existing = std::fs::read_to_string(&config_path).unwrap();
        let mut new_content = existing;
        if !new_content.ends_with('\n') {
            new_content.push('\n');
        }
        new_content.push('\n');
        new_content.push_str(new_toml.trim_start());

        std::fs::write(&config_path, &new_content).unwrap();

        let result = std::fs::read_to_string(&config_path).unwrap();
        assert!(result.contains("[default]"));
        assert!(result.contains("[mcp.servers.test]"));
        assert!(result.contains("transport = \"stdio\""));
        // 验证合并后的 TOML 仍然合法
        assert!(result.parse::<toml_edit::DocumentMut>().is_ok());
    }

    #[test]
    fn config_append_rejects_invalid_toml() {
        let result = config_append(Some("not valid [[ toml"));
        assert!(result.is_err() || matches!(result.unwrap(), r if !r.success));
    }

    #[test]
    fn config_append_rejects_missing_value() {
        let result = config_append(None);
        let tool_result = result.unwrap();
        assert!(!tool_result.success);
        assert!(tool_result.error.is_some());
    }
}
