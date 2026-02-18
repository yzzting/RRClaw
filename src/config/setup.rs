use color_eyre::eyre::{Context, Result};
use dialoguer::{Input, Password, Select};

use super::schema::{Config, DefaultConfig, MemoryConfig, ProviderConfig, SecurityConfig};
use crate::security::AutonomyLevel;

/// 已知 Provider 信息（名称、默认 base_url、已知模型列表、认证方式）
pub struct ProviderInfo {
    pub name: &'static str,
    pub base_url: &'static str,
    pub models: &'static [&'static str],
    pub auth_style: Option<&'static str>,
}

/// 所有已知 Provider 列表
pub const PROVIDERS: &[ProviderInfo] = &[
    ProviderInfo {
        name: "deepseek",
        base_url: "https://api.deepseek.com/v1",
        models: &["deepseek-chat", "deepseek-reasoner"],
        auth_style: None,
    },
    ProviderInfo {
        name: "glm",
        base_url: "https://open.bigmodel.cn/api/paas/v4",
        models: &["glm-4.7", "glm-4-flash", "glm-4-plus", "glm-4-long"],
        auth_style: None,
    },
    ProviderInfo {
        name: "minimax",
        base_url: "https://api.minimax.chat/v1",
        models: &["MiniMax-M2.5", "MiniMax-Text-01"],
        auth_style: None,
    },
    ProviderInfo {
        name: "claude",
        base_url: "https://api.anthropic.com",
        models: &[
            "claude-sonnet-4-5-20250929",
            "claude-haiku-4-5-20251001",
            "claude-opus-4-6",
        ],
        auth_style: Some("x-api-key"),
    },
    ProviderInfo {
        name: "gpt",
        base_url: "https://api.openai.com/v1",
        models: &["gpt-4o", "gpt-4o-mini", "o1", "o3-mini"],
        auth_style: None,
    },
];

/// 根据名称查找 ProviderInfo
pub fn find_provider_info(name: &str) -> Option<&'static ProviderInfo> {
    PROVIDERS.iter().find(|p| p.name == name)
}

/// 运行交互式配置向导
pub fn run_setup() -> Result<()> {
    println!("🔧 RRClaw 配置向导\n");

    // 1. 选择 Provider
    let provider_names: Vec<&str> = PROVIDERS.iter().map(|p| p.name).collect();
    let provider_idx = Select::new()
        .with_prompt("选择默认 Provider")
        .items(&provider_names)
        .default(0)
        .interact()
        .wrap_err("选择 Provider 失败")?;

    let info = &PROVIDERS[provider_idx];
    println!();

    // 2. 输入 API Key
    let api_key: String = Password::new()
        .with_prompt(format!("{} API Key", info.name))
        .interact()
        .wrap_err("输入 API Key 失败")?;
    println!();

    // 3. 选择模型
    let model = select_model(info)?;
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
    providers.insert(
        info.name.to_string(),
        ProviderConfig {
            base_url: info.base_url.to_string(),
            api_key,
            model: model.clone(),
            auth_style: info.auth_style.map(|s| s.to_string()),
        },
    );

    let config = Config {
        default: DefaultConfig {
            provider: info.name.to_string(),
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

/// 从 ProviderInfo 的模型列表中选择模型（含"自定义"选项）
pub fn select_model(info: &ProviderInfo) -> Result<String> {
    let mut items: Vec<String> = info.models.iter().map(|m| m.to_string()).collect();
    items.push("自定义...".to_string());

    let idx = Select::new()
        .with_prompt("选择模型")
        .items(&items)
        .default(0)
        .interact()
        .wrap_err("选择模型失败")?;

    if idx < info.models.len() {
        Ok(info.models[idx].to_string())
    } else {
        let custom: String = Input::new()
            .with_prompt("输入模型名称")
            .interact_text()
            .wrap_err("输入模型名失败")?;
        Ok(custom)
    }
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
