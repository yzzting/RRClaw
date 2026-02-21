use color_eyre::eyre::{Context, Result, eyre};
use dialoguer::{Confirm, Input, Select};
use reedline::{DefaultPrompt, DefaultPromptSegment, Reedline, Signal};
use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tracing::{debug, info};

use crate::agent::Agent;
use crate::config::{Config, ProviderConfig, PROVIDERS};
use crate::memory::SqliteMemory;
use crate::providers::{StreamEvent, ToolStatusKind};
use crate::skills::{load_skill_content, validate_skill_name, SkillMeta, SkillSource};

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
pub async fn run_repl(
    agent: &mut Agent,
    memory: &SqliteMemory,
    config: &Config,
    skills: Vec<SkillMeta>,
    data_dir: &std::path::Path,
) -> Result<()> {
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
                    let workspace_dir = agent.policy().workspace_dir.clone();
                    handle_slash_command(cmd, agent, &session_id, memory, config, &skills, data_dir, workspace_dir).await?;
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
#[allow(clippy::too_many_arguments)]
async fn handle_slash_command(
    cmd: &str,
    agent: &mut Agent,
    session_id: &str,
    memory: &SqliteMemory,
    config: &Config,
    skills: &[SkillMeta],
    data_dir: &std::path::Path,
    workspace_dir: std::path::PathBuf,
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
        "switch" => {
            cmd_switch(agent, config)?;
        }
        "apikey" => {
            cmd_apikey(agent, config)?;
        }
        "skill" => {
            // 切掉命令名，剩余部分作为参数
            let rest = cmd["skill".len()..].trim();
            cmd_skill(rest, agent, skills)?;
        }
        "mcp" => {
            cmd_mcp(agent);
        }
        "mode" => {
            cmd_mode(agent)?;
        }
        "identity" => {
            // 切掉命令名，剩余部分作为参数
            let rest = cmd["identity".len()..].trim();
            cmd_identity(rest, agent, data_dir, workspace_dir)?;
        }
        _ => {
            println!("未知命令: /{}。输入 /help 查看可用命令。", name);
        }
    }
    Ok(())
}

/// /skill 命令入口 —— 解析子命令后分发
fn cmd_skill(rest: &str, agent: &mut Agent, skills: &[SkillMeta]) -> Result<()> {
    let mut parts = rest.splitn(2, ' ');
    let sub = parts.next().unwrap_or("").trim();
    let arg = parts.next().map(|s| s.trim());

    match sub {
        "" => cmd_skill_list(skills),
        "new" => cmd_skill_new(arg)?,
        "edit" => cmd_skill_edit(arg, skills)?,
        "delete" => cmd_skill_delete(arg, skills)?,
        "show" => cmd_skill_show(arg, skills)?,
        name => {
            // 默认行为：加载技能指令注入当前对话
            match load_skill_content(name, skills) {
                Ok(content) => {
                    agent.inject_skill_context(name, &content.instructions);
                    println!("✓ 已加载技能: {}（指令已注入对话）", name);
                }
                Err(e) => println!("✗ {}", e),
            }
        }
    }
    Ok(())
}

/// /skill — 列出所有可用技能
fn cmd_skill_list(skills: &[SkillMeta]) {
    if skills.is_empty() {
        println!("暂无可用技能。");
        println!("  使用 /skill new <name> 创建技能");
        println!("  或将技能目录放到 ~/.rrclaw/skills/<name>/SKILL.md");
        return;
    }
    println!("可用技能:\n");
    for s in skills {
        println!("  {} {} — {}", s.source.label(), s.name, s.description);
    }
    println!();
    println!("  /skill <name>         加载技能指令到当前对话");
    println!("  /skill show <name>    查看技能完整内容");
    println!("  /skill new <name>     创建新技能");
    println!("  /skill edit <name>    编辑技能（$EDITOR）");
    println!("  /skill delete <name>  删除技能");
}

/// /skill new <name> — 创建技能模板
fn cmd_skill_new(name: Option<&str>) -> Result<()> {
    let name = name.ok_or_else(|| eyre!("用法: /skill new <name>"))?;
    validate_skill_name(name)?;

    let global_dir = global_skills_dir()?;
    let skill_dir = global_dir.join(name);

    if skill_dir.exists() {
        println!("技能 '{}' 已存在。使用 /skill edit {} 编辑。", name, name);
        return Ok(());
    }

    std::fs::create_dir_all(&skill_dir)
        .wrap_err_with(|| format!("创建技能目录失败: {}", skill_dir.display()))?;

    let title = name.replace('-', " ");
    let template = format!(
        "---\nname: {}\ndescription: 简短描述这个技能做什么。当用户要求 XXX 时使用。\ntags: []\n---\n\n# {}\n\n## 步骤\n1. 用 file_read 读取相关文件\n2. 分析内容\n3. 输出结果\n\n## 注意事项\n- ...\n",
        name, title
    );
    let skill_path = skill_dir.join("SKILL.md");
    std::fs::write(&skill_path, &template)
        .wrap_err_with(|| format!("写入 SKILL.md 失败: {}", skill_path.display()))?;

    println!("✓ 已创建技能模板: {}", skill_path.display());
    println!("  使用 /skill edit {} 编辑内容。", name);
    Ok(())
}

/// /skill edit <name> — 用 $EDITOR 打开 SKILL.md
fn cmd_skill_edit(name: Option<&str>, skills: &[SkillMeta]) -> Result<()> {
    let name = name.ok_or_else(|| eyre!("用法: /skill edit <name>"))?;

    let skill_path = find_editable_skill_path(name, skills)?;
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());

    std::process::Command::new(&editor)
        .arg(skill_path.join("SKILL.md"))
        .status()
        .wrap_err_with(|| format!("启动编辑器 '{}' 失败", editor))?;

    println!("✓ 编辑完成。重启 rrclaw 后技能列表会刷新。");
    Ok(())
}

/// /skill delete <name> — 删除技能（带 [y/N] 确认，内置不可删）
fn cmd_skill_delete(name: Option<&str>, skills: &[SkillMeta]) -> Result<()> {
    let name = name.ok_or_else(|| eyre!("用法: /skill delete <name>"))?;

    let skill = skills
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| eyre!("未找到技能: {}", name))?;

    if skill.source == SkillSource::BuiltIn {
        println!("✗ 内置技能不可删除。");
        return Ok(());
    }

    let path = skill
        .path
        .as_ref()
        .ok_or_else(|| eyre!("技能路径为空"))?;

    print!("确认删除技能 '{}'? [y/N] ", name);
    let _ = std::io::stdout().flush();
    let mut input = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut input)
        .wrap_err("读取用户输入失败")?;

    if input.trim().to_lowercase() == "y" {
        std::fs::remove_dir_all(path)
            .wrap_err_with(|| format!("删除 {} 失败", path.display()))?;
        println!("✓ 已删除技能: {}", name);
    } else {
        println!("已取消。");
    }
    Ok(())
}

/// /skill show <name> — 打印技能全文（不注入对话）
fn cmd_skill_show(name: Option<&str>, skills: &[SkillMeta]) -> Result<()> {
    let name = name.ok_or_else(|| eyre!("用法: /skill show <name>"))?;
    let content = load_skill_content(name, skills)
        .map_err(|e| eyre!("{}", e))?;

    println!("=== {} [{}] ===\n", content.meta.name, content.meta.source.label());
    println!("{}", content.instructions);
    if !content.resources.is_empty() {
        println!("\n--- 附带资源 ---");
        for r in &content.resources {
            println!("  {}", r);
        }
    }
    Ok(())
}

// ─── /identity 命令实现 ─────────────────────────────────────────────────

/// /identity 命令入口 —— 解析子命令后分发
fn cmd_identity(rest: &str, agent: &mut Agent, data_dir: &std::path::Path, workspace_dir: std::path::PathBuf) -> Result<()> {
    let mut parts = rest.splitn(2, ' ');
    let sub = parts.next().unwrap_or("").trim();
    let arg = parts.next().map(|s| s.trim());

    match sub {
        "" | "status" => cmd_identity_status(data_dir, workspace_dir),
        "show" => cmd_identity_show(arg, data_dir, workspace_dir),
        "edit" => cmd_identity_edit(arg, data_dir, workspace_dir),
        "reload" => {
            agent.reload_identity(&workspace_dir, data_dir);
            println!("✓ 身份文件已重新加载，下次对话立即生效。");
            Ok(())
        }
        other => {
            println!("未知子命令 '{}'。用 /identity 查看帮助。", other);
            Ok(())
        }
    }
}

/// /identity（状态总览）实现
fn cmd_identity_status(data_dir: &std::path::Path, workspace_dir: std::path::PathBuf) -> Result<()> {
    println!("身份文件状态:\n");

    let files = [
        ("USER.md（全局用户偏好）", data_dir.join("USER.md"), true),
        ("SOUL.md（全局 Agent 人格）", data_dir.join("SOUL.md"), true),
        ("SOUL.md（项目 Agent 人格）", workspace_dir.join(".rrclaw/SOUL.md"), false),
        ("AGENT.md（项目行为约定）", workspace_dir.join(".rrclaw/AGENT.md"), false),
    ];

    for (label, path, is_global) in &files {
        let scope = if *is_global { "全局" } else { "项目" };
        match std::fs::metadata(path) {
            Ok(meta) => {
                let size = meta.len();
                println!("  ✓ {} [{}]", label, scope);
                println!("    路径: {}", path.display());
                println!("    大小: {} 字节", size);
            }
            Err(_) => {
                println!("  ✗ {} [{}]（未创建）", label, scope);
                println!("    路径: {}", path.display());
            }
        }
        println!();
    }

    println!("命令:");
    println!("  /identity edit user     编辑全局用户偏好");
    println!("  /identity edit soul    编辑 Agent 人格");
    println!("  /identity edit agent   编辑项目行为约定");
    println!("  /identity show <type>  查看文件内容");
    println!("  /identity reload       重新加载（立即生效）");
    Ok(())
}

/// /identity edit <type> — 引导式问答入口
fn cmd_identity_edit(file_type: Option<&str>, data_dir: &std::path::Path, workspace_dir: std::path::PathBuf) -> Result<()> {
    let file_type = file_type.ok_or_else(|| eyre!("用法: /identity edit <user|soul|agent>"))?;
    match file_type {
        "user"  => guided_edit_user(data_dir),
        "soul"  => guided_edit_soul(data_dir, &workspace_dir),
        "agent" => guided_edit_agent(&workspace_dir),
        other   => Err(eyre!("未知类型 '{}'。支持: user, soul, agent", other)),
    }
}

// ─── 引导式编辑辅助函数 ───────────────────────────────────────────────────

/// 从文件内容中提取 `- {prefix}：{value}` 格式的单行字段值
fn extract_field(content: &str, prefix: &str) -> String {
    let needle = format!("- {}：", prefix);
    for line in content.lines() {
        if let Some(rest) = line.trim().strip_prefix(&needle) {
            return rest.trim().to_string();
        }
    }
    String::new()
}

/// 提取指定 `## 节名` 下所有 `- item` 条目（遇到下一个 `##` 停止）
fn extract_section_items(content: &str, section_header: &str) -> Vec<String> {
    let header = format!("## {}", section_header);
    let mut in_section = false;
    let mut items = Vec::new();
    for line in content.lines() {
        if line.trim() == header {
            in_section = true;
            continue;
        }
        if in_section {
            if line.starts_with("## ") {
                break;
            }
            if let Some(item) = line.trim().strip_prefix("- ") {
                let item = item.trim().to_string();
                if !item.is_empty() {
                    items.push(item);
                }
            }
        }
    }
    items
}

/// 显示现有条目，询问保留与否，然后循环追问新条目
/// `prompt_first`：空列表时首条提示，`prompt_more`：后续条提示
fn collect_list_items(prompt_first: &str, prompt_more: &str, existing: Vec<String>) -> Result<Vec<String>> {
    let mut items: Vec<String> = if !existing.is_empty() {
        println!("  当前已有 {} 条：", existing.len());
        for (i, item) in existing.iter().enumerate() {
            println!("    {}. {}", i + 1, item);
        }
        let keep = Confirm::new()
            .with_prompt("保留这些条目")
            .default(true)
            .interact()
            .wrap_err("确认输入失败")?;
        if keep { existing } else { vec![] }
    } else {
        vec![]
    };

    loop {
        let prompt = if items.is_empty() { prompt_first } else { prompt_more };
        let item: String = Input::new()
            .with_prompt(prompt)
            .allow_empty(true)
            .interact_text()
            .wrap_err("输入失败")?;
        let item = item.trim().to_string();
        if item.is_empty() {
            break;
        }
        items.push(item);
    }
    Ok(items)
}

/// 写入文件（自动创建父目录）
fn write_identity_file(path: &std::path::Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .wrap_err_with(|| format!("创建目录失败: {}", parent.display()))?;
    }
    std::fs::write(path, content)
        .wrap_err_with(|| format!("写入文件失败: {}", path.display()))?;
    println!("\n✓ 已保存: {}", path.display());
    println!("  使用 /identity reload 立即生效。");
    Ok(())
}

// ─── USER.md 引导式编辑 ───────────────────────────────────────────────────

fn guided_edit_user(data_dir: &std::path::Path) -> Result<()> {
    let path = data_dir.join("USER.md");

    // 读取现有内容用于预填充
    let existing = std::fs::read_to_string(&path).unwrap_or_default();

    println!("\n─── 全局用户偏好设置 (USER.md) ───────────────────────────\n");
    println!("  所有项目的 AI 对话都会读取此文件，用于告知 AI 你的背景和偏好。\n");

    let tech_stack: String = Input::new()
        .with_prompt("主要技术栈（如：Rust, Python）")
        .default(extract_field(&existing, "主要技术栈"))
        .allow_empty(true)
        .interact_text()
        .wrap_err("输入失败")?;

    let work_lang: String = Input::new()
        .with_prompt("工作语言偏好（如：中文）")
        .default({
            let v = extract_field(&existing, "工作语言");
            if v.is_empty() { "中文".to_string() } else { v }
        })
        .interact_text()
        .wrap_err("输入失败")?;

    let reply_style: String = Input::new()
        .with_prompt("回复风格（如：简洁直接、先结论后解释）")
        .default(extract_field(&existing, "回复风格"))
        .allow_empty(true)
        .interact_text()
        .wrap_err("输入失败")?;

    let timezone: String = Input::new()
        .with_prompt("时区（如：Asia/Shanghai，留空跳过）")
        .default(extract_field(&existing, "时区"))
        .allow_empty(true)
        .interact_text()
        .wrap_err("输入失败")?;

    println!("\n  额外约定（留空结束追加）：");
    let extras = collect_list_items(
        "添加约定（留空跳过）",
        "再加一条（留空完成）",
        extract_section_items(&existing, "偏好约定"),
    )?;

    // 构建输出
    let mut content = String::from("## 用户信息\n\n");
    if !tech_stack.trim().is_empty() {
        content.push_str(&format!("- 主要技术栈：{}\n", tech_stack.trim()));
    }
    content.push_str(&format!("- 工作语言：{}\n", work_lang.trim()));
    if !reply_style.trim().is_empty() {
        content.push_str(&format!("- 回复风格：{}\n", reply_style.trim()));
    }
    if !timezone.trim().is_empty() {
        content.push_str(&format!("- 时区：{}\n", timezone.trim()));
    }
    if !extras.is_empty() {
        content.push_str("\n## 偏好约定\n\n");
        for item in &extras {
            content.push_str(&format!("- {}\n", item));
        }
    }
    content.push('\n');

    write_identity_file(&path, &content)
}

// ─── SOUL.md 引导式编辑 ───────────────────────────────────────────────────

fn guided_edit_soul(data_dir: &std::path::Path, workspace_dir: &std::path::Path) -> Result<()> {
    // 先让用户选范围
    let global_path = data_dir.join("SOUL.md");
    let project_path = workspace_dir.join(".rrclaw/SOUL.md");

    let scope_labels = [
        format!("全局 ({}) — 所有项目共享", global_path.display()),
        format!("项目级 ({}) — 仅本项目", project_path.display()),
    ];
    let scope_idx = Select::new()
        .with_prompt("编辑哪个级别的 SOUL.md")
        .items(&scope_labels)
        .default(0)
        .interact()
        .wrap_err("选择失败")?;
    let path = if scope_idx == 0 { &global_path } else { &project_path };

    let existing = std::fs::read_to_string(path).unwrap_or_default();

    println!("\n─── Agent 人格设置 (SOUL.md) ──────────────────────────────\n");
    println!("  告知 AI 它的角色定位和说话风格，留空字段将被忽略。\n");

    // 从 "你叫 {name}。" 提取名字
    let existing_name = existing
        .lines()
        .find_map(|line| {
            let line = line.trim();
            line.strip_prefix("你叫 ")
                .and_then(|rest| rest.strip_suffix('。'))
                .map(|s| s.to_string())
        })
        .unwrap_or_default();

    let name: String = Input::new()
        .with_prompt("Agent 名字（如：Claw，留空使用默认 RRClaw）")
        .default(existing_name)
        .allow_empty(true)
        .interact_text()
        .wrap_err("输入失败")?;

    let style: String = Input::new()
        .with_prompt("说话风格（如：直接简洁，不废话）")
        .default(extract_field(&existing, "说话风格"))
        .allow_empty(true)
        .interact_text()
        .wrap_err("输入失败")?;

    let forbidden: String = Input::new()
        .with_prompt("禁止开头语（如：当然！好的！，留空跳过）")
        .default({
            // 从 `- 不说"..."等废话开头` 提取
            existing.lines()
                .find_map(|line| {
                    let line = line.trim();
                    if line.starts_with("- 不说\"") || line.starts_with("- 不用\"") {
                        let start = line.find('"').map(|i| i + 1)?;
                        let end = line.rfind('"')?;
                        Some(line[start..end].to_string())
                    } else {
                        None
                    }
                })
                .unwrap_or_default()
        })
        .allow_empty(true)
        .interact_text()
        .wrap_err("输入失败")?;

    // 其余 `- ` 行作为已有 traits（排除已处理的字段行）
    let existing_traits: Vec<String> = existing.lines()
        .filter_map(|line| {
            let line = line.trim();
            if !line.starts_with("- ") { return None; }
            let item = &line[2..];
            if item.starts_with("说话风格：") { return None; }
            if item.starts_with("不说\"") || item.starts_with("不用\"") { return None; }
            Some(item.to_string())
        })
        .collect();

    println!("\n  额外个性特征（留空结束追加）：");
    let traits = collect_list_items(
        "添加特征（留空跳过）",
        "再加一条（留空完成）",
        existing_traits,
    )?;

    // 构建输出
    let mut content = String::new();
    if name.trim().is_empty() {
        content.push_str("你是 RRClaw，一个 AI 助手。\n");
    } else {
        content.push_str(&format!("你叫 {}。\n", name.trim()));
    }
    content.push('\n');
    if !style.trim().is_empty() {
        content.push_str(&format!("- 说话风格：{}\n", style.trim()));
    }
    if !forbidden.trim().is_empty() {
        content.push_str(&format!("- 不说\"{}\"等废话开头\n", forbidden.trim()));
    }
    for t in &traits {
        content.push_str(&format!("- {}\n", t));
    }
    content.push('\n');

    write_identity_file(path, &content)
}

// ─── AGENT.md 引导式编辑 ─────────────────────────────────────────────────

fn guided_edit_agent(workspace_dir: &std::path::Path) -> Result<()> {
    let path = workspace_dir.join(".rrclaw/AGENT.md");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();

    println!("\n─── 项目行为约定设置 (AGENT.md) ───────────────────────────\n");
    println!("  仅对本项目生效，告知 AI 项目的代码规范、提交约定和禁止事项。\n");

    println!("  【代码规范】");
    let code_standards = collect_list_items(
        "添加代码规范（如：必须通过 clippy，留空跳过）",
        "再加一条（留空完成）",
        extract_section_items(&existing, "代码规范"),
    )?;

    println!("\n  【Git 提交规范】");
    let git_conventions = collect_list_items(
        "添加提交规范（如：feat/fix/docs 前缀，留空跳过）",
        "再加一条（留空完成）",
        extract_section_items(&existing, "Git 提交规范"),
    )?;

    println!("\n  【禁止事项】");
    let forbidden_items = collect_list_items(
        "添加禁止事项（如：禁止 unwrap()，留空跳过）",
        "再加一条（留空完成）",
        extract_section_items(&existing, "禁止事项"),
    )?;

    // 构建输出（空节省略）
    let mut content = String::new();
    let mut write_section = |header: &str, items: &[String]| {
        if items.is_empty() { return; }
        content.push_str(&format!("## {}\n\n", header));
        for item in items {
            content.push_str(&format!("- {}\n", item));
        }
        content.push('\n');
    };
    write_section("代码规范", &code_standards);
    write_section("Git 提交规范", &git_conventions);
    write_section("禁止事项", &forbidden_items);

    if content.trim().is_empty() {
        println!("\n  未输入任何内容，文件未修改。");
        return Ok(());
    }

    write_identity_file(&path, &content)
}

/// /identity show <type> 实现
fn cmd_identity_show(file_type: Option<&str>, data_dir: &std::path::Path, workspace_dir: std::path::PathBuf) -> Result<()> {
    let file_type = file_type.ok_or_else(|| eyre!("用法: /identity show <user|soul|agent>"))?;

    let path = match file_type {
        "user"  => data_dir.join("USER.md"),
        "soul"  => {
            let project = workspace_dir.join(".rrclaw/SOUL.md");
            if project.exists() { project } else { data_dir.join("SOUL.md") }
        }
        "agent" => workspace_dir.join(".rrclaw/AGENT.md"),
        other   => return Err(eyre!("未知类型 '{}'。支持: user, soul, agent", other)),
    };

    match std::fs::read_to_string(&path) {
        Ok(content) => {
            println!("=== {} ===\n", path.display());
            println!("{}", content);
        }
        Err(_) => {
            println!("文件不存在: {}", path.display());
            println!("使用 /identity edit {} 创建。", file_type);
        }
    }
    Ok(())
}

/// 获取用户全局 skills 目录 ~/.rrclaw/skills/
fn global_skills_dir() -> Result<std::path::PathBuf> {
    let base_dirs = directories::BaseDirs::new()
        .ok_or_else(|| eyre!("无法获取 home 目录"))?;
    Ok(base_dirs.home_dir().join(".rrclaw").join("skills"))
}

/// 找到可编辑的 skill 路径（全局或项目级，非内置）
fn find_editable_skill_path(name: &str, skills: &[SkillMeta]) -> Result<std::path::PathBuf> {
    let skill = skills
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| eyre!("未找到技能 '{}'。使用 /skill new {} 创建。", name, name))?;

    if skill.source == SkillSource::BuiltIn {
        return Err(eyre!(
            "内置技能不可直接编辑。\n\
             如需自定义，请用 /skill new {} 在全局目录创建同名技能（会覆盖内置版本）。",
            name
        ));
    }

    skill
        .path
        .clone()
        .ok_or_else(|| eyre!("技能路径为空"))
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

/// /switch — 一站式切换 Provider + 模型
fn cmd_switch(agent: &mut Agent, config: &Config) -> Result<()> {
    use dialoguer::{Input, Password, Select};

    println!(
        "当前: {} / {} ({})\n",
        agent.provider_name(),
        agent.model(),
        agent.base_url()
    );

    // ① 选择 Provider
    let current_name = agent.provider_name().to_string();
    let mut default_idx = 0;
    let items: Vec<String> = PROVIDERS
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let mut label = p.name.to_string();
            if p.name == current_name {
                label.push_str(" (当前 ✓)");
                default_idx = i;
            } else if config.providers.contains_key(p.name) {
                label.push_str(" (已配置)");
            }
            label
        })
        .collect();

    let provider_idx = Select::new()
        .with_prompt("选择 Provider")
        .items(&items)
        .default(default_idx)
        .interact()
        .wrap_err("选择 Provider 失败")?;

    let info = &PROVIDERS[provider_idx];

    // ② 选择模型
    let current_model = agent.model().to_string();
    let mut model_default = 0;
    let mut model_items: Vec<String> = info
        .models
        .iter()
        .enumerate()
        .map(|(i, m)| {
            if info.name == current_name && *m == current_model {
                model_default = i;
                format!("{} (当前 ✓)", m)
            } else {
                m.to_string()
            }
        })
        .collect();
    model_items.push("自定义...".to_string());

    let model_idx = Select::new()
        .with_prompt("选择模型")
        .items(&model_items)
        .default(model_default)
        .interact()
        .wrap_err("选择模型失败")?;

    let model = if model_idx < info.models.len() {
        info.models[model_idx].to_string()
    } else {
        Input::new()
            .with_prompt("输入模型名称")
            .interact_text()
            .wrap_err("输入模型名失败")?
    };

    // 检查是否有变化
    if info.name == current_name && model == current_model {
        println!("无变化。");
        return Ok(());
    }

    // ③ 如果未配置 → 输入 API Key + Base URL → 写入 config.toml
    if let Some(pc) = config.providers.get(info.name) {
        // 已配置 → 直接切换
        let new_provider = crate::providers::create_provider(pc);
        agent.switch_provider(
            new_provider,
            info.name.to_string(),
            pc.base_url.clone(),
            model.clone(),
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

        let pc = ProviderConfig {
            base_url: base_url.clone(),
            api_key,
            model: model.clone(),
            auth_style: info.auth_style.map(|s| s.to_string()),
        };
        save_provider_to_config(info.name, &pc, None)?;

        let new_provider = crate::providers::create_provider(&pc);
        agent.switch_provider(
            new_provider,
            info.name.to_string(),
            base_url,
            model.clone(),
        );
    }

    // 持久化: 更新 config.toml 的 [default] 段
    save_default_to_config(info.name, &model, None)?;

    // 切换 provider 或模型后清空对话历史，避免旧上下文干扰新模型
    agent.clear_history();
    println!("已切换到 {} / {}", info.name, model);
    Ok(())
}

/// /apikey — 修改已有 Provider 的 API Key 或 Base URL
fn cmd_apikey(agent: &mut Agent, config: &Config) -> Result<()> {
    use dialoguer::{Input, Password, Select};

    // 列出已配置的 provider
    let configured: Vec<&String> = config.providers.keys().collect();
    if configured.is_empty() {
        println!("没有已配置的 Provider。请先用 /switch 添加。");
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

/// 更新 config.toml 的 [default] 段（provider + model）
/// 如果提供了 path 则使用它，否则使用 Config::config_path()
fn save_default_to_config(provider: &str, model: &str, path: Option<&std::path::Path>) -> Result<()> {
    let config_path = if let Some(p) = path {
        p.to_path_buf()
    } else {
        Config::config_path()?
    };
    let content = std::fs::read_to_string(&config_path)?;
    let mut doc = content
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| color_eyre::eyre::eyre!("解析配置文件失败: {}", e))?;

    doc["default"]["provider"] = toml_edit::value(provider);
    doc["default"]["model"] = toml_edit::value(model);

    std::fs::write(&config_path, doc.to_string())?;
    Ok(())
}

/// 将新 Provider 配置写入 config.toml
/// 如果提供了 path 则使用它，否则使用 Config::config_path()
fn save_provider_to_config(name: &str, pc: &ProviderConfig, path: Option<&std::path::Path>) -> Result<()> {
    let config_path = if let Some(p) = path {
        p.to_path_buf()
    } else {
        Config::config_path()?
    };
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

/// /mode — 切换 Agent 自主级别（ReadOnly / Supervised / Full）
fn cmd_mode(agent: &mut Agent) -> Result<()> {
    use crate::security::AutonomyLevel;
    use dialoguer::Select;

    let current = &agent.policy().autonomy;
    let modes = [
        ("supervised", "Supervised — 执行前需要用户确认（默认）"),
        ("full",       "Full       — 自主执行，无需确认"),
        ("read-only",  "ReadOnly   — 只读，不执行任何工具"),
    ];

    let default_idx = modes
        .iter()
        .position(|(k, _)| match current {
            AutonomyLevel::Supervised => *k == "supervised",
            AutonomyLevel::Full => *k == "full",
            AutonomyLevel::ReadOnly => *k == "read-only",
        })
        .unwrap_or(0);

    let labels: Vec<&str> = modes.iter().map(|(_, label)| *label).collect();
    let idx = Select::new()
        .with_prompt("选择安全模式")
        .items(&labels)
        .default(default_idx)
        .interact()
        .wrap_err("选择安全模式失败")?;

    let (key, _) = modes[idx];
    let new_level = match key {
        "full" => AutonomyLevel::Full,
        "read-only" => AutonomyLevel::ReadOnly,
        _ => AutonomyLevel::Supervised,
    };

    if new_level == *current {
        println!("无变化。");
        return Ok(());
    }

    // 运行时切换
    agent.set_autonomy(new_level);

    // 持久化到 config.toml
    let config_path = Config::config_path()?;
    let content = std::fs::read_to_string(&config_path)?;
    let mut doc = content
        .parse::<toml_edit::DocumentMut>()
        .wrap_err("解析配置文件失败")?;
    doc["security"]["autonomy"] = toml_edit::value(key);
    std::fs::write(&config_path, doc.to_string())?;

    println!("已切换到 {} 模式。", key);
    Ok(())
}

/// /mcp — 列出当前已加载的 MCP 工具
fn cmd_mcp(agent: &Agent) {
    let all_tools = agent.tool_names();
    let mcp_tools: Vec<&str> = all_tools
        .iter()
        .copied()
        .filter(|n| n.starts_with("mcp_"))
        .collect();

    if mcp_tools.is_empty() {
        println!("当前没有已加载的 MCP 工具。");
        println!("在 ~/.rrclaw/config.toml 中配置 [mcp.servers.<name>] 后重启生效。");
        return;
    }

    // 按 server 分组
    use std::collections::BTreeMap;
    let mut by_server: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for name in &mcp_tools {
        // name 格式: mcp_{server}_{tool}
        let rest = &name["mcp_".len()..];
        // server name 可能含 '-'，tool name 部分以第一个 '_' 分隔
        let (server, tool) = if let Some(pos) = rest.find('_') {
            (&rest[..pos], &rest[pos + 1..])
        } else {
            (rest, "")
        };
        by_server.entry(server).or_default().push(tool);
    }

    println!("已加载 MCP 工具（共 {} 个）:", mcp_tools.len());
    for (server, tools) in &by_server {
        println!("  [{}] {} 个工具:", server, tools.len());
        for tool in tools {
            println!("    mcp_{}_{}", server, tool);
        }
    }
}

/// 打印帮助信息
fn print_help() {
    println!("可用命令:");
    println!("  /help, /h              显示此帮助");
    println!("  /new                   新建对话（清空历史）");
    println!("  /clear                 清屏");
    println!("  /config                显示当前配置");
    println!("  /switch                切换 Provider + 模型");
    println!("  /apikey                修改 API Key 或 Base URL");
    println!();
    println!("  /mode                  切换安全模式（supervised/full/read-only）");
    println!("  /mcp                   列出已加载的 MCP 工具");
    println!();
    println!("  /skill                 列出所有可用技能");
    println!("  /skill <name>          加载技能指令到当前对话");
    println!("  /skill show <name>     查看技能完整内容");
    println!("  /skill new <name>      创建新技能");
    println!("  /skill edit <name>     编辑技能（$EDITOR）");
    println!("  /skill delete <name>   删除技能");
    println!();
    println!("  /identity              查看身份文件状态");
    println!("  /identity show <type>  查看身份文件内容（user/soul/agent）");
    println!("  /identity edit <type>  编辑身份文件（$EDITOR）");
    println!("  /identity reload       重新加载身份文件（立即生效）");
    println!();
    println!("  exit, quit             退出");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProviderConfig;
    use std::fs;
    use tempfile::TempDir;

    /// 创建临时 config.toml 用于测试
    fn temp_config(content: &str) -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, content).unwrap();
        (dir, path)
    }

    #[test]
    fn save_default_to_config_updates_default_section() {
        let (_dir, path) = temp_config(
            r#"
[default]
provider = "deepseek"
model = "deepseek-chat"
temperature = 0.7

[providers.deepseek]
base_url = "https://api.deepseek.com/v1"
api_key = "sk-test"
model = "deepseek-chat"
"#,
        );

        // 执行
        save_default_to_config("glm", "glm-4.7", Some(&path)).unwrap();

        // 验证
        let content = fs::read_to_string(&path).unwrap();
        let doc: toml_edit::DocumentMut = content.parse().unwrap();

        assert_eq!(doc["default"]["provider"].as_str(), Some("glm"));
        assert_eq!(doc["default"]["model"].as_str(), Some("glm-4.7"));
        // 原有 provider 配置不应被删除 - 检查能否读取到值
        assert!(doc["providers"]["deepseek"]["base_url"].is_str());
    }

    #[test]
    fn save_provider_to_config_adds_new_provider() {
        let (_dir, path) = temp_config(
            r#"
[default]
provider = "deepseek"
model = "deepseek-chat"

[providers.deepseek]
base_url = "https://api.deepseek.com/v1"
api_key = "sk-test"
model = "deepseek-chat"
"#,
        );

        let pc = ProviderConfig {
            base_url: "https://open.bigmodel.cn/api/paas/v4".to_string(),
            api_key: "glm-key-123".to_string(),
            model: "glm-4.7".to_string(),
            auth_style: None,
        };

        // 执行
        save_provider_to_config("glm", &pc, Some(&path)).unwrap();

        // 验证
        let content = fs::read_to_string(&path).unwrap();
        let doc: toml_edit::DocumentMut = content.parse().unwrap();

        assert_eq!(doc["providers"]["glm"]["base_url"].as_str(), Some("https://open.bigmodel.cn/api/paas/v4"));
        assert_eq!(doc["providers"]["glm"]["api_key"].as_str(), Some("glm-key-123"));
        assert_eq!(doc["providers"]["glm"]["model"].as_str(), Some("glm-4.7"));
        // 原有配置应保留
        assert_eq!(doc["default"]["provider"].as_str(), Some("deepseek"));
    }

    // ─── extract_field 测试 ────────────────────────────────────────────

    #[test]
    fn extract_field_finds_value() {
        let content = "## 用户信息\n\n- 主要技术栈：Rust, Python\n- 工作语言：中文\n";
        assert_eq!(extract_field(content, "主要技术栈"), "Rust, Python");
        assert_eq!(extract_field(content, "工作语言"), "中文");
    }

    #[test]
    fn extract_field_returns_empty_when_missing() {
        let content = "- 工作语言：中文\n";
        assert_eq!(extract_field(content, "时区"), "");
    }

    #[test]
    fn extract_field_trims_whitespace() {
        // 行首 trim 处理：行本身有多余空格
        let content2 = "- 回复风格： 简洁直接 \n";
        assert_eq!(extract_field(content2, "回复风格"), "简洁直接");
    }

    // ─── extract_section_items 测试 ──────────────────────────────────

    #[test]
    fn extract_section_items_collects_lines() {
        let content = "## 代码规范\n\n- 通过 clippy\n- 禁止 unwrap()\n\n## Git 提交规范\n\n- feat/fix 前缀\n";
        let items = extract_section_items(content, "代码规范");
        assert_eq!(items, vec!["通过 clippy", "禁止 unwrap()"]);
    }

    #[test]
    fn extract_section_items_stops_at_next_header() {
        let content = "## 代码规范\n- item1\n## 禁止事项\n- item2\n";
        let items = extract_section_items(content, "代码规范");
        assert_eq!(items, vec!["item1"]);
    }

    #[test]
    fn extract_section_items_returns_empty_for_missing_section() {
        let content = "## 其他节\n- item1\n";
        let items = extract_section_items(content, "代码规范");
        assert!(items.is_empty());
    }
}
