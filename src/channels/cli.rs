use color_eyre::eyre::{Context, Result};
use reedline::{DefaultPrompt, DefaultPromptSegment, Reedline, Signal};
use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tracing::{debug, info};

use crate::agent::Agent;
use crate::config::{find_provider_info, select_model, Config, ProviderConfig, PROVIDERS};
use crate::memory::SqliteMemory;
use crate::providers::{StreamEvent, ToolStatusKind};

/// 当天日期作为 session ID
fn today_session_id() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// 从 shell 命令中提取基础命令名（如 "cargo test" → "cargo"）
fn extract_base_command(args: &serde_json::Value) -> Option<String> {
    args.get("command")
        .and_then(|v| v.as_str())
        .and_then(|cmd| cmd.split_whitespace().next())
        .and_then(|base| base.rsplit('/').next())
        .map(|s| s.to_string())
}

/// 生成自动批准的 key：shell 工具按基础命令名，其他工具按工具名
fn approval_key(tool_name: &str, args: &serde_json::Value) -> String {
    if tool_name == "shell" {
        if let Some(base_cmd) = extract_base_command(args) {
            return format!("shell:{}", base_cmd);
        }
    }
    tool_name.to_string()
}

/// 给 Agent 注入 CLI 确认回调（Supervised 模式下生效）
/// 支持会话级自动批准：y=本次, n=拒绝, a=本会话自动批准该工具/命令
pub fn setup_cli_confirm(agent: &mut Agent) {
    let approved: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));

    agent.set_confirm_fn(Box::new(move |name, args| {
        let key = approval_key(name, args);

        // 检查是否已自动批准
        if approved.lock().unwrap().contains(&key) {
            let display = if name == "shell" {
                extract_base_command(args).unwrap_or_else(|| name.to_string())
            } else {
                name.to_string()
            };
            println!("\n✓ 自动批准 '{}' (本会话已授权)", display);
            return true;
        }

        let args_str = serde_json::to_string_pretty(args).unwrap_or_else(|_| args.to_string());
        print!(
            "\n⚠ 执行工具 '{}'\n  参数: {}\n  确认执行? [y/N/a(本会话自动批准)] ",
            name, args_str
        );
        let _ = std::io::stdout().flush();

        let mut input = String::new();
        if std::io::stdin().lock().read_line(&mut input).is_ok() {
            let answer = input.trim().to_lowercase();
            match answer.as_str() {
                "a" | "always" => {
                    approved.lock().unwrap().insert(key);
                    true
                }
                "y" | "yes" => true,
                _ => false,
            }
        } else {
            false
        }
    }));
}

/// 运行 CLI REPL 交互循环（流式输出）
pub async fn run_repl(agent: &mut Agent, memory: &SqliteMemory, config: &Config) -> Result<()> {
    setup_cli_confirm(agent);

    // 加载今天的对话历史
    let session_id = today_session_id();
    let history = memory.load_conversation_history(&session_id).await?;
    if !history.is_empty() {
        info!("恢复 {} 条对话历史 (session: {})", history.len(), session_id);
        println!("(已恢复 {} 条对话历史)", history.len());
        agent.set_history(history);
    }

    let mut line_editor = Reedline::create();
    let prompt = DefaultPrompt::new(
        DefaultPromptSegment::Basic("rrclaw".to_string()),
        DefaultPromptSegment::Empty,
    );

    println!("RRClaw AI 助手 (输入 /help 查看命令, exit 退出)");
    println!();

    loop {
        let sig = line_editor.read_line(&prompt);
        match sig {
            Ok(Signal::Success(line)) => {
                let input = line.trim();

                if input.is_empty() {
                    continue;
                }

                match input {
                    "exit" | "quit" => {
                        println!("再见！");
                        break;
                    }
                    "clear" => {
                        line_editor.clear_scrollback().wrap_err("清屏失败")?;
                        continue;
                    }
                    _ => {}
                }

                // 斜杠命令
                if let Some(cmd) = input.strip_prefix('/') {
                    handle_slash_command(cmd, agent, &session_id, memory, config).await?;
                    continue;
                }

                println!();
                if let Err(e) = stream_message(agent, input).await {
                    eprintln!("错误: {:#}\n", e);
                }

                // 每轮对话后自动保存历史
                if let Err(e) = memory
                    .save_conversation_history(&session_id, agent.history())
                    .await
                {
                    debug!("保存对话历史失败: {:#}", e);
                }
            }
            Ok(Signal::CtrlD) | Ok(Signal::CtrlC) => {
                println!("\n再见！");
                break;
            }
            Err(e) => {
                eprintln!("输入错误: {}", e);
                break;
            }
        }
    }

    // 退出时最终保存一次
    if let Err(e) = memory
        .save_conversation_history(&session_id, agent.history())
        .await
    {
        debug!("退出时保存对话历史失败: {:#}", e);
    }

    Ok(())
}

/// 处理斜杠命令
async fn handle_slash_command(
    cmd: &str,
    agent: &mut Agent,
    session_id: &str,
    memory: &SqliteMemory,
    config: &Config,
) -> Result<()> {
    let name = cmd.split_whitespace().next().unwrap_or(cmd);

    match name {
        "help" | "h" => {
            print_help();
        }
        "new" => {
            if let Err(e) = memory
                .save_conversation_history(session_id, agent.history())
                .await
            {
                debug!("保存对话历史失败: {:#}", e);
            }
            agent.clear_history();
            println!("已开始新对话。");
        }
        "clear" => {
            print!("\x1b[2J\x1b[H");
            let _ = std::io::stdout().flush();
        }
        "config" => {
            cmd_config(agent);
        }
        "provider" => {
            cmd_provider(agent, config)?;
        }
        "model" => {
            cmd_model(agent)?;
        }
        "apikey" => {
            cmd_apikey(agent, config)?;
        }
        _ => {
            println!("未知命令: /{}。输入 /help 查看可用命令。", name);
        }
    }
    Ok(())
}

/// /config — 显示当前配置
fn cmd_config(agent: &Agent) {
    let policy = agent.policy();
    println!("当前配置:");
    println!("  Provider: {}", agent.provider_name());
    println!("  Base URL: {}", agent.base_url());
    println!("  模型: {}", agent.model());
    println!("  温度: {}", agent.temperature());
    println!("  安全模式: {:?}", policy.autonomy);
    println!("  工作目录: {}", policy.workspace_dir.display());
}

/// /provider — 交互式切换 Provider
fn cmd_provider(agent: &mut Agent, config: &Config) -> Result<()> {
    use dialoguer::{Input, Password, Select};

    println!("当前: {} ({})\n", agent.provider_name(), agent.base_url());

    // 构建选项列表：已知 provider + 已配置标记
    let items: Vec<String> = PROVIDERS
        .iter()
        .map(|p| {
            if config.providers.contains_key(p.name) {
                format!("{} (已配置 ✓)", p.name)
            } else {
                p.name.to_string()
            }
        })
        .collect();

    let idx = Select::new()
        .with_prompt("选择 Provider")
        .items(&items)
        .default(0)
        .interact()
        .wrap_err("选择 Provider 失败")?;

    let info = &PROVIDERS[idx];

    if let Some(pc) = config.providers.get(info.name) {
        // 已配置 → 直接切换
        let new_provider = crate::providers::create_provider(pc);
        agent.switch_provider(
            new_provider,
            info.name.to_string(),
            pc.base_url.clone(),
            pc.model.clone(),
        );
        println!(
            "已切换到 {} (模型: {}, URL: {})",
            info.name, pc.model, pc.base_url
        );
    } else {
        // 未配置 → 引导输入
        let api_key: String = Password::new()
            .with_prompt(format!("{} API Key", info.name))
            .interact()
            .wrap_err("输入 API Key 失败")?;

        let base_url: String = Input::new()
            .with_prompt("Base URL")
            .default(info.base_url.to_string())
            .interact_text()
            .wrap_err("输入 Base URL 失败")?;

        let model = select_model(info)?;

        // 写入 config.toml
        let pc = ProviderConfig {
            base_url: base_url.clone(),
            api_key,
            model: model.clone(),
            auth_style: info.auth_style.map(|s| s.to_string()),
        };
        save_provider_to_config(info.name, &pc)?;

        // 切换
        let new_provider = crate::providers::create_provider(&pc);
        agent.switch_provider(
            new_provider,
            info.name.to_string(),
            base_url.clone(),
            model.clone(),
        );
        println!(
            "已配置并切换到 {} (模型: {}, URL: {})",
            info.name, model, base_url
        );
    }

    Ok(())
}

/// /model — 交互式切换模型（当前 Provider 下）
fn cmd_model(agent: &mut Agent) -> Result<()> {
    use dialoguer::Select;

    let current_provider = agent.provider_name().to_string();
    println!(
        "当前: {} (Provider: {})\n",
        agent.model(),
        current_provider
    );

    // 查找当前 provider 的已知模型
    let info = find_provider_info(&current_provider);
    let models: Vec<&str> = info.map(|i| i.models).unwrap_or(&[]).to_vec();

    if models.is_empty() {
        println!("未找到 {} 的已知模型列表。", current_provider);
        let custom: String = dialoguer::Input::new()
            .with_prompt("输入模型名称")
            .interact_text()
            .wrap_err("输入模型名失败")?;
        agent.set_model(custom.clone());
        println!("模型已切换为: {}", custom);
        return Ok(());
    }

    let mut items: Vec<String> = models.iter().map(|m| m.to_string()).collect();
    items.push("自定义...".to_string());

    let idx = Select::new()
        .with_prompt("选择模型")
        .items(&items)
        .default(0)
        .interact()
        .wrap_err("选择模型失败")?;

    let model = if idx < models.len() {
        models[idx].to_string()
    } else {
        dialoguer::Input::new()
            .with_prompt("输入模型名称")
            .interact_text()
            .wrap_err("输入模型名失败")?
    };

    agent.set_model(model.clone());
    println!("模型已切换为: {}", model);
    Ok(())
}

/// /apikey — 修改已有 Provider 的 API Key 或 Base URL
fn cmd_apikey(agent: &mut Agent, config: &Config) -> Result<()> {
    use dialoguer::{Input, Password, Select};

    // 列出已配置的 provider
    let configured: Vec<&String> = config.providers.keys().collect();
    if configured.is_empty() {
        println!("没有已配置的 Provider。请先用 /provider 添加。");
        return Ok(());
    }

    let items: Vec<String> = configured
        .iter()
        .map(|name| {
            if name.as_str() == agent.provider_name() {
                format!("{} (当前)", name)
            } else {
                name.to_string()
            }
        })
        .collect();

    let idx = Select::new()
        .with_prompt("选择 Provider")
        .items(&items)
        .default(0)
        .interact()
        .wrap_err("选择 Provider 失败")?;

    let provider_name = configured[idx].as_str();

    // 选择修改什么
    let modify_options = ["API Key", "Base URL", "两者都改"];
    let modify_idx = Select::new()
        .with_prompt("修改什么")
        .items(modify_options)
        .default(0)
        .interact()
        .wrap_err("选择修改项失败")?;

    let config_path = Config::config_path()?;
    let content = std::fs::read_to_string(&config_path)?;
    let mut doc = content
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| color_eyre::eyre::eyre!("解析配置文件失败: {}", e))?;

    match modify_idx {
        0 | 2 => {
            let new_key: String = Password::new()
                .with_prompt(format!("{} API Key", provider_name))
                .interact()
                .wrap_err("输入 API Key 失败")?;
            doc["providers"][provider_name]["api_key"] = toml_edit::value(&new_key);
            println!("API Key 已更新。");
        }
        _ => {}
    }

    match modify_idx {
        1 | 2 => {
            let old_url = config
                .providers
                .get(provider_name)
                .map(|pc| pc.base_url.as_str())
                .unwrap_or("");
            let new_url: String = Input::new()
                .with_prompt("Base URL")
                .default(old_url.to_string())
                .interact_text()
                .wrap_err("输入 Base URL 失败")?;
            doc["providers"][provider_name]["base_url"] = toml_edit::value(&new_url);
            println!("Base URL 已更新。");
        }
        _ => {}
    }

    std::fs::write(&config_path, doc.to_string())?;

    // 如果修改的是当前 provider，重建 Provider 实例使之立即生效
    if provider_name == agent.provider_name() {
        // 重新加载 config 获取最新值
        let new_config = Config::load_from_path(&config_path)?;
        if let Some(pc) = new_config.providers.get(provider_name) {
            let new_provider = crate::providers::create_provider(pc);
            agent.switch_provider(
                new_provider,
                provider_name.to_string(),
                pc.base_url.clone(),
                pc.model.clone(),
            );
            println!("当前 session 已更新。");
        }
    }

    Ok(())
}

/// 将新 Provider 配置写入 config.toml
fn save_provider_to_config(name: &str, pc: &ProviderConfig) -> Result<()> {
    let config_path = Config::config_path()?;
    let content = std::fs::read_to_string(&config_path)?;
    let mut doc = content
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| color_eyre::eyre::eyre!("解析配置文件失败: {}", e))?;

    // 确保 [providers] 表存在
    if doc.get("providers").is_none() {
        doc["providers"] = toml_edit::Item::Table(toml_edit::Table::new());
    }

    // 创建 provider 子表
    let mut table = toml_edit::InlineTable::new();
    table.insert("base_url", pc.base_url.as_str().into());
    table.insert("api_key", pc.api_key.as_str().into());
    table.insert("model", pc.model.as_str().into());
    if let Some(auth) = &pc.auth_style {
        table.insert("auth_style", auth.as_str().into());
    }

    // 用普通 Table 写入（更可读）
    doc["providers"][name] = toml_edit::Item::Table(toml_edit::Table::new());
    doc["providers"][name]["base_url"] = toml_edit::value(&pc.base_url);
    doc["providers"][name]["api_key"] = toml_edit::value(&pc.api_key);
    doc["providers"][name]["model"] = toml_edit::value(&pc.model);
    if let Some(auth) = &pc.auth_style {
        doc["providers"][name]["auth_style"] = toml_edit::value(auth);
    }

    std::fs::write(&config_path, doc.to_string())?;
    Ok(())
}

/// 打印帮助信息
fn print_help() {
    println!("可用命令:");
    println!("  /help, /h     显示此帮助");
    println!("  /new          新建对话（清空历史）");
    println!("  /clear        清屏");
    println!("  /config       显示当前配置");
    println!("  /provider     切换 Provider（交互式选择）");
    println!("  /model        切换模型（交互式选择）");
    println!("  /apikey       修改 API Key 或 Base URL");
    println!();
    println!("  exit, quit    退出");
    println!("  clear         清屏");
    println!();
    println!("其他输入会发送给 AI 处理。");
}

/// 流式处理消息并实时打印
async fn stream_message(agent: &mut Agent, input: &str) -> Result<()> {
    let (tx, mut rx) = mpsc::channel::<StreamEvent>(64);

    // 在后台 task 中消费 stream events 并打印
    let print_handle = tokio::spawn(async move {
        let mut has_output = false;
        // Thinking 动画: 收到 Thinking 后启动，收到首个 Text/ToolStatus/Done 后停止
        let mut thinking_handle: Option<tokio::task::JoinHandle<()>> = None;
        let thinking_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));

        while let Some(event) = rx.recv().await {
            match event {
                StreamEvent::Thinking => {
                    // 启动 thinking 动画
                    let flag = thinking_flag.clone();
                    flag.store(true, std::sync::atomic::Ordering::Relaxed);
                    thinking_handle = Some(tokio::spawn(async move {
                        let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                        let mut i = 0;
                        while flag.load(std::sync::atomic::Ordering::Relaxed) {
                            print!("\r{} 思考中...", frames[i % frames.len()]);
                            let _ = std::io::stdout().flush();
                            i += 1;
                            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                        }
                    }));
                }
                StreamEvent::Text(text) => {
                    // 停止 thinking 动画
                    if let Some(handle) = thinking_handle.take() {
                        thinking_flag.store(false, std::sync::atomic::Ordering::Relaxed);
                        let _ = handle.await;
                        print!("\r\x1b[K"); // 清除 thinking 行
                        let _ = std::io::stdout().flush();
                    }
                    print!("{}", text);
                    let _ = std::io::stdout().flush();
                    has_output = true;
                }
                StreamEvent::ToolStatus { name, status } => {
                    // 停止 thinking 动画
                    if let Some(handle) = thinking_handle.take() {
                        thinking_flag.store(false, std::sync::atomic::Ordering::Relaxed);
                        let _ = handle.await;
                        print!("\r\x1b[K"); // 清除 thinking 行
                        let _ = std::io::stdout().flush();
                    }
                    match &status {
                        ToolStatusKind::Running(cmd) => {
                            print!("\n⏳ {} ...", cmd);
                            let _ = std::io::stdout().flush();
                        }
                        ToolStatusKind::Success(summary) => {
                            println!(" ✓ {}", summary);
                        }
                        ToolStatusKind::Failed(err) => {
                            println!(" ✗ {} 失败", name);
                            // 显示前几行错误详情
                            for line in err.lines().take(3) {
                                println!("    {}", line);
                            }
                        }
                    }
                }
                StreamEvent::Done(_) => {
                    // 停止 thinking 动画
                    if let Some(handle) = thinking_handle.take() {
                        thinking_flag.store(false, std::sync::atomic::Ordering::Relaxed);
                        let _ = handle.await;
                        print!("\r\x1b[K");
                        let _ = std::io::stdout().flush();
                    }
                }
                StreamEvent::ToolCallDelta { .. } => {
                    // tool call 增量不打印给用户
                }
            }
        }
        // channel 关闭后清理残留的 thinking 动画
        if let Some(handle) = thinking_handle.take() {
            thinking_flag.store(false, std::sync::atomic::Ordering::Relaxed);
            let _ = handle.await;
            print!("\r\x1b[K");
            let _ = std::io::stdout().flush();
        }
        has_output
    });

    // 调用流式处理
    let result = agent.process_message_stream(input, tx).await;

    // 等待打印完成
    let has_output = print_handle.await.unwrap_or(false);

    match result {
        Ok(_) => {
            if has_output {
                println!("\n");
            } else {
                println!();
            }
        }
        Err(e) => {
            println!();
            return Err(e);
        }
    }

    Ok(())
}

/// 单次消息模式（流式输出）
pub async fn run_single(agent: &mut Agent, message: &str, memory: &SqliteMemory) -> Result<()> {
    setup_cli_confirm(agent);

    let (tx, mut rx) = mpsc::channel::<StreamEvent>(64);

    let print_handle = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            if let StreamEvent::Text(text) = event {
                print!("{}", text);
                let _ = std::io::stdout().flush();
            }
        }
    });

    let result = agent.process_message_stream(message, tx).await;
    let _ = print_handle.await;
    println!();

    if let Err(e) = result {
        eprintln!("错误: {:#}", e);
    }

    // 单次消息也保存历史
    let session_id = today_session_id();
    if let Err(e) = memory
        .save_conversation_history(&session_id, agent.history())
        .await
    {
        debug!("保存对话历史失败: {:#}", e);
    }

    Ok(())
}
