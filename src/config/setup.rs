use color_eyre::eyre::{Context, Result};
use dialoguer::{Input, Password, Select};

use super::schema::{Config, DefaultConfig, MemoryConfig, ProviderConfig, SecurityConfig};
use crate::security::AutonomyLevel;

/// Provider 选项
const PROVIDERS: &[(&str, &str, &str)] = &[
    ("deepseek", "https://api.deepseek.com/v1", "deepseek-chat"),
    (
        "glm",
        "https://open.bigmodel.cn/api/paas/v4",
        "glm-4-flash",
    ),
    (
        "minimax",
        "https://api.minimax.chat/v1",
        "MiniMax-Text-01",
    ),
    (
        "claude",
        "https://api.anthropic.com",
        "claude-sonnet-4-5-20250929",
    ),
    ("gpt", "https://api.openai.com/v1", "gpt-4o"),
];

/// 运行交互式配置向导
pub fn run_setup() -> Result<()> {
    println!("🔧 RRClaw 配置向导\n");

    // 1. 选择 Provider
    let provider_names: Vec<&str> = PROVIDERS.iter().map(|(name, _, _)| *name).collect();
    let provider_idx = Select::new()
        .with_prompt("选择默认 Provider")
        .items(&provider_names)
        .default(0)
        .interact()
        .wrap_err("选择 Provider 失败")?;

    let (provider_name, base_url, default_model) = PROVIDERS[provider_idx];
    println!();

    // 2. 输入 API Key
    let api_key: String = Password::new()
        .with_prompt(format!("{} API Key", provider_name))
        .interact()
        .wrap_err("输入 API Key 失败")?;
    println!();

    // 3. 选择/输入模型
    let model: String = Input::new()
        .with_prompt("默认模型")
        .default(default_model.to_string())
        .interact_text()
        .wrap_err("输入模型失败")?;
    println!();

    // 4. 设置 temperature
    let temperature: f64 = Input::new()
        .with_prompt("Temperature (0.0-2.0)")
        .default(0.7)
        .interact_text()
        .wrap_err("输入 temperature 失败")?;
    println!();

    // 5. 选择安全模式
    let autonomy_options = ["supervised (需确认后执行)", "full (自主执行)", "readonly (只读)"];
    let autonomy_idx = Select::new()
        .with_prompt("安全模式")
        .items(autonomy_options)
        .default(0)
        .interact()
        .wrap_err("选择安全模式失败")?;

    let autonomy = match autonomy_idx {
        0 => AutonomyLevel::Supervised,
        1 => AutonomyLevel::Full,
        _ => AutonomyLevel::ReadOnly,
    };
    println!();

    // 构造配置
    let mut providers = std::collections::HashMap::new();
    let auth_style = if provider_name == "claude" {
        Some("x-api-key".to_string())
    } else {
        None
    };

    providers.insert(
        provider_name.to_string(),
        ProviderConfig {
            base_url: base_url.to_string(),
            api_key,
            model: model.clone(),
            auth_style,
        },
    );

    let config = Config {
        default: DefaultConfig {
            provider: provider_name.to_string(),
            model,
            temperature,
        },
        providers,
        memory: MemoryConfig::default(),
        security: SecurityConfig {
            autonomy,
            ..SecurityConfig::default()
        },
        telegram: None,
    };

    // 写入配置文件
    let config_path = Config::config_path()?;
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).wrap_err("创建配置目录失败")?;
    }

    let toml_str = toml_from_config(&config);
    std::fs::write(&config_path, &toml_str).wrap_err("写入配置文件失败")?;

    println!("✅ 配置已保存到: {}", config_path.display());
    println!("\n你可以随时编辑该文件添加更多 Provider 或调整设置。");

    Ok(())
}

/// 将 Config 转为可读的 TOML 字符串
fn toml_from_config(config: &Config) -> String {
    let mut lines = Vec::new();

    lines.push("[default]".to_string());
    lines.push(format!("provider = \"{}\"", config.default.provider));
    lines.push(format!("model = \"{}\"", config.default.model));
    lines.push(format!("temperature = {}", config.default.temperature));
    lines.push(String::new());

    for (name, pc) in &config.providers {
        lines.push(format!("[providers.{}]", name));
        lines.push(format!("base_url = \"{}\"", pc.base_url));
        lines.push(format!("api_key = \"{}\"", pc.api_key));
        lines.push(format!("model = \"{}\"", pc.model));
        if let Some(auth) = &pc.auth_style {
            lines.push(format!("auth_style = \"{}\"", auth));
        }
        lines.push(String::new());
    }

    lines.push("[memory]".to_string());
    lines.push(format!("backend = \"{}\"", config.memory.backend));
    lines.push(format!("auto_save = {}", config.memory.auto_save));
    lines.push(String::new());

    lines.push("[security]".to_string());
    let autonomy_str = match config.security.autonomy {
        AutonomyLevel::ReadOnly => "readonly",
        AutonomyLevel::Supervised => "supervised",
        AutonomyLevel::Full => "full",
    };
    lines.push(format!("autonomy = \"{}\"", autonomy_str));
    let cmds: Vec<String> = config
        .security
        .allowed_commands
        .iter()
        .map(|c| format!("\"{}\"", c))
        .collect();
    lines.push(format!("allowed_commands = [{}]", cmds.join(", ")));
    lines.push(format!("workspace_only = {}", config.security.workspace_only));
    lines.push(String::new());

    lines.join("\n")
}
