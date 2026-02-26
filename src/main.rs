use clap::{Parser, Subcommand};
use color_eyre::eyre::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;
use tracing_subscriber::prelude::*;

#[derive(Parser)]
#[command(name = "rrclaw", about = "安全优先的 AI 助手", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 启动 AI 助手（交互或单次模式）
    Agent {
        /// 单次消息模式
        #[arg(short, long)]
        message: Option<String>,

        /// 指定 Provider（覆盖配置文件中的 default）
        #[arg(short, long)]
        provider: Option<String>,

        /// 指定模型（覆盖配置文件中的 default）
        #[arg(long)]
        model: Option<String>,
    },
    /// 启动 Telegram Bot（需要 --features telegram 编译）
    #[cfg(feature = "telegram")]
    Telegram,
    /// Start daemon (background process with Telegram + IPC socket)
    Start,
    /// Connect to running daemon for interactive chat
    Chat,
    /// Stop the running daemon
    Stop,
    /// Restart the daemon (stop + start)
    Restart,
    /// Show daemon status
    Status,
    /// Internal: daemon worker process (do not call directly)
    #[command(hide = true)]
    DaemonWorker,
    /// 交互式配置向导
    Setup,
    /// 初始化配置文件
    Init,
    /// 显示当前配置
    Config,
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    init_tracing()?;

    let cli = Cli::parse();

    match cli.command {
        Commands::Agent {
            message,
            provider,
            model,
        } => run_agent(message, provider, model).await?,
        #[cfg(feature = "telegram")]
        Commands::Telegram => run_telegram().await?,
        Commands::Start => rrclaw::daemon::start()?,
        Commands::Chat => rrclaw::daemon::client::run_chat().await?,
        Commands::Stop => rrclaw::daemon::stop()?,
        Commands::Restart => rrclaw::daemon::restart()?,
        Commands::Status => rrclaw::daemon::status()?,
        Commands::DaemonWorker => rrclaw::daemon::server::run_daemon_worker().await?,
        Commands::Setup => rrclaw::config::run_setup()?,
        Commands::Init => run_init()?,
        Commands::Config => run_config()?,
    }

    Ok(())
}

async fn run_agent(
    message: Option<String>,
    provider_name: Option<String>,
    model_override: Option<String>,
) -> Result<()> {
    let config = rrclaw::config::Config::load_or_init().wrap_err("加载配置失败")?;

    // 确定使用的 provider
    let provider_key = provider_name.as_deref().unwrap_or(&config.default.provider);

    let provider_config = config
        .providers
        .get(provider_key)
        .ok_or_else(|| {
            color_eyre::eyre::eyre!(
                "Provider '{}' 未在配置文件中配置。请编辑 ~/.rrclaw/config.toml 添加 [providers.{}] 配置。",
                provider_key,
                provider_key
            )
        })?;

    // 确定模型
    let model = model_override.unwrap_or_else(|| config.default.model.clone());

    // 创建 Provider
    let main_provider = rrclaw::providers::create_provider(provider_config);

    // 创建 fallback providers（如果配置了）
    let fallback_providers: Vec<Box<dyn rrclaw::providers::Provider>> = config
        .reliability
        .fallback_providers
        .iter()
        .filter_map(|name| config.providers.get(name))
        .map(|pc| rrclaw::providers::create_provider(pc))
        .collect();

    // 包装为 ReliableProvider
    let retry_config = rrclaw::providers::RetryConfig {
        max_retries: config.reliability.max_retries,
        initial_backoff_ms: config.reliability.initial_backoff_ms,
        ..Default::default()
    };

    // Arc<dyn Provider> 用于 HttpRequestTool 的 mini-LLM 提取
    let provider_arc: Arc<dyn rrclaw::providers::Provider> = if fallback_providers.is_empty() {
        Arc::new(rrclaw::providers::ReliableProvider::new(
            main_provider,
            retry_config.clone(),
        ))
    } else {
        Arc::new(rrclaw::providers::ReliableProvider::with_fallbacks(
            main_provider,
            fallback_providers,
            retry_config.clone(),
        ))
    };

    // Box<dyn Provider> 用于 Agent（重新创建，因为上面的 main_provider 和 fallback_providers 已移动）
    let fallback_providers_for_box: Vec<Box<dyn rrclaw::providers::Provider>> = config
        .reliability
        .fallback_providers
        .iter()
        .filter_map(|name| config.providers.get(name))
        .map(|pc| rrclaw::providers::create_provider(pc))
        .collect();
    let main_provider_for_box = rrclaw::providers::create_provider(provider_config);
    let provider: Box<dyn rrclaw::providers::Provider> = if fallback_providers_for_box.is_empty() {
        Box::new(rrclaw::providers::ReliableProvider::new(
            main_provider_for_box,
            retry_config,
        ))
    } else {
        Box::new(rrclaw::providers::ReliableProvider::with_fallbacks(
            main_provider_for_box,
            fallback_providers_for_box,
            retry_config,
        ))
    };

    // 创建 Memory（Arc 共享给 Agent 和 CLI）
    let data_dir = data_dir()?;
    let log_dir = log_dir()?;
    let config_path = rrclaw::config::Config::config_path()?;

    // 加载 Skills（内置 > 全局 > 项目级）
    let workspace_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let global_skills_dir = {
        let base_dirs = directories::BaseDirs::new()
            .ok_or_else(|| color_eyre::eyre::eyre!("无法获取 home 目录"))?;
        base_dirs.home_dir().join(".rrclaw").join("skills")
    };
    let builtin = rrclaw::skills::builtin_skills(rrclaw::config::Config::get_language());
    let skills = rrclaw::skills::load_skills(&workspace_dir, &global_skills_dir, builtin);

    // 创建 Memory（Arc 共享给 Tools）
    let memory =
        Arc::new(rrclaw::memory::SqliteMemory::open(&data_dir).wrap_err("初始化 Memory 失败")?);

    // ─── RoutineEngine 初始化 ────────────────────────────────────────────
    // 构建 Routine 列表（从 config 的静态配置转换）
    let static_routines: Vec<rrclaw::routines::Routine> = config
        .routines
        .jobs
        .iter()
        .map(|job| rrclaw::routines::Routine {
            name: job.name.clone(),
            schedule: job.schedule.clone(),
            message: job.message.clone(),
            channel: job.channel.clone(),
            enabled: job.enabled,
            source: rrclaw::routines::RoutineSource::Config,
        })
        .collect();

    // 初始化 RoutineEngine
    let routines_db_path = data_dir.join("routines.db");
    let routine_engine = match rrclaw::routines::RoutineEngine::new(
        static_routines,
        Arc::new(config.clone()),
        memory.clone() as Arc<dyn rrclaw::memory::Memory>,
        &routines_db_path,
    )
    .await
    {
        Ok(engine) => {
            let engine = Arc::new(engine);
            // 后台启动调度器（不阻塞 REPL）
            let engine_clone = Arc::clone(&engine);
            tokio::spawn(async move {
                if let Err(e) = engine_clone.start().await {
                    tracing::error!("RoutineEngine 启动失败: {}", e);
                }
            });
            Some(engine)
        }
        Err(e) => {
            tracing::warn!("初始化 RoutineEngine 失败，跳过定时任务: {}", e);
            None
        }
    };
    // ─── RoutineEngine 初始化结束 ────────────────────────────────────────

    // 创建 Tools（SelfInfoTool 需要 config 和路径信息，SkillTool 需要 skills，MemoryTools 需要 memory，HttpRequestTool 需要 provider）
    let mut tools = rrclaw::tools::create_tools(
        config.clone(),
        provider_arc,
        data_dir.clone(),
        log_dir.clone(),
        config_path.clone(),
        skills.clone(),
        memory.clone() as Arc<dyn rrclaw::memory::Memory>,
        routine_engine.clone(),
    );

    // MCP 工具加载（可选，配置了才加载）
    let mcp_manager = if let Some(mcp_config) = &config.mcp {
        if !mcp_config.servers.is_empty() {
            let mgr = rrclaw::mcp::McpManager::connect_all(&mcp_config.servers).await;
            let mcp_tools = mgr.tools_l1().await;
            if !mcp_tools.is_empty() {
                tracing::info!("已加载 {} 个 MCP 工具", mcp_tools.len());
                tools.extend(mcp_tools);
            }
            Some(mgr)
        } else {
            None
        }
    } else {
        None
    };

    // 种入核心知识（upsert，每次启动保持最新）
    memory
        .seed_core_knowledge(&data_dir, &log_dir, &config_path)
        .await
        .wrap_err("种入核心知识失败")?;

    // 创建 SecurityPolicy
    let policy = rrclaw::security::SecurityPolicy {
        autonomy: config.security.autonomy.clone(),
        allowed_commands: config.security.allowed_commands.clone(),
        workspace_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        blocked_paths: rrclaw::security::SecurityPolicy::default().blocked_paths,
        http_allowed_hosts: config.security.http_allowed_hosts.clone(),
        injection_check: config.security.injection_check,
    };

    // ─── 身份文件加载（P5-2）────────────────────────────────────────────
    // identity 文件在 ~/.rrclaw/，而 data_dir 是 ~/.rrclaw/data/，取父目录
    let rrclaw_home = data_dir
        .parent()
        .unwrap_or(data_dir.as_path())
        .to_path_buf();
    let identity_context =
        rrclaw::agent::identity::load_identity_context(&policy.workspace_dir, &rrclaw_home);
    if identity_context.is_some() {
        tracing::info!("已加载用户身份文件");
    }
    // ─── 身份文件加载结束 ────────────────────────────────────────────────

    // 创建 Agent
    let mut agent = rrclaw::agent::Agent::new(
        provider,
        tools,
        Box::new(memory.clone()),
        policy,
        provider_key.to_string(),
        provider_config.base_url.clone(),
        model,
        config.default.temperature,
        skills.clone(),
        identity_context,
    );

    // 创建 Telegram 运行时管理器
    let telegram_runtime = Arc::new(rrclaw::channels::cli::TelegramRuntime::new());
    #[cfg(feature = "telegram")]
    let telegram_config = {
        let cfg = config.telegram.clone();
        if let Some(ref tg_cfg) = cfg {
            telegram_runtime.set_config(rrclaw::config::Config {
                telegram: Some(tg_cfg.clone()),
                ..config.clone()
            });
        }
        cfg
    };

    // 运行
    match message {
        Some(msg) => rrclaw::channels::cli::run_single(&mut agent, &msg, &memory).await?,
        None => {
            #[cfg(feature = "telegram")]
            {
                if telegram_config.is_some() {
                    // 同时启动 CLI 和 Telegram
                    run_cli_with_telegram(
                        &mut agent,
                        &memory,
                        &config,
                        skills,
                        rrclaw_home,
                        routine_engine,
                        telegram_runtime,
                    )
                    .await?;
                } else {
                    // 只启动 CLI
                    rrclaw::channels::cli::run_repl(
                        &mut agent,
                        &memory,
                        &config,
                        skills,
                        &rrclaw_home,
                        routine_engine,
                        Some(telegram_runtime),
                    )
                    .await?;
                }
            }
            #[cfg(not(feature = "telegram"))]
            rrclaw::channels::cli::run_repl(
                &mut agent,
                &memory,
                &config,
                skills,
                &rrclaw_home,
                routine_engine,
                Some(telegram_runtime),
            )
            .await?;
        }
    }

    // 退出时关闭 MCP 连接
    if let Some(mgr) = mcp_manager {
        mgr.shutdown().await;
    }

    Ok(())
}

/// 同时运行 CLI REPL 和 Telegram Bot
#[cfg(feature = "telegram")]
#[allow(clippy::too_many_arguments)]
async fn run_cli_with_telegram(
    agent: &mut rrclaw::agent::Agent,
    memory: &Arc<rrclaw::memory::SqliteMemory>,
    config: &rrclaw::config::Config,
    skills: Vec<rrclaw::skills::SkillMeta>,
    rrclaw_home: std::path::PathBuf,
    routine_engine: Option<Arc<rrclaw::routines::RoutineEngine>>,
    telegram_runtime: Arc<rrclaw::channels::cli::TelegramRuntime>,
) -> Result<()> {
    const CYAN: &str = "\x1b[36m";
    const RESET: &str = "\x1b[0m";
    const YELLOW: &str = "\x1b[33m";

    println!("{}RRClaw{} AI 助手 - CLI + Telegram 模式", CYAN, RESET);
    println!("CLI: 直接输入消息");
    println!("Telegram: 已启用，请向你的 Bot 发送消息");
    println!("输入 {}exit{} 退出\n", YELLOW, RESET);

    // 克隆必要的资源用于 Telegram
    let tg_config = config.telegram.clone().unwrap();
    let memory_clone = memory.clone();
    let config_clone = config.clone();

    // 启动 Telegram Bot 任务（后台运行）
    let tg_handle = tokio::spawn(async move {
        // Telegram Bot 使用独立的 AgentFactory（每个 chat 独立）
        if let Err(e) = rrclaw::channels::telegram::run_telegram(
            rrclaw::config::Config {
                telegram: Some(tg_config),
                ..config_clone
            },
            memory_clone,
        )
        .await
        {
            tracing::error!("Telegram Bot 运行错误: {:#}", e);
        }
    });

    // 运行 CLI REPL（主任务）
    let cli_result = rrclaw::channels::cli::run_repl(
        agent,
        memory,
        config,
        skills,
        rrclaw_home.as_path(),
        routine_engine,
        Some(telegram_runtime),
    )
    .await;

    // CLI 退出后，关闭 Telegram
    tg_handle.abort();

    cli_result
}

#[cfg(feature = "telegram")]
async fn run_telegram() -> Result<()> {
    let config = rrclaw::config::Config::load_or_init().wrap_err("加载配置失败")?;

    let data_dir = data_dir()?;
    let memory =
        Arc::new(rrclaw::memory::SqliteMemory::open(&data_dir).wrap_err("初始化 Memory 失败")?);

    rrclaw::channels::telegram::run_telegram(config, memory).await
}

fn run_init() -> Result<()> {
    let config_path = rrclaw::config::Config::config_path()?;

    if config_path.exists() {
        println!("配置文件已存在: {}", config_path.display());
        println!("如需重新初始化，请先删除该文件。");
    } else {
        let _ = rrclaw::config::Config::load_or_init()?;
        println!("已创建配置文件: {}", config_path.display());
        println!("请编辑该文件添加你的 API Key。");
    }

    Ok(())
}

fn run_config() -> Result<()> {
    let config_path = rrclaw::config::Config::config_path()?;

    if !config_path.exists() {
        println!("配置文件不存在。运行 `rrclaw init` 创建。");
        return Ok(());
    }

    let content = std::fs::read_to_string(&config_path).wrap_err("读取配置文件失败")?;
    println!("配置文件: {}\n", config_path.display());
    println!("{}", content);

    Ok(())
}

/// 获取数据目录: ~/.rrclaw/data/
fn data_dir() -> Result<PathBuf> {
    let base_dirs = directories::BaseDirs::new()
        .ok_or_else(|| color_eyre::eyre::eyre!("无法获取 home 目录"))?;
    Ok(base_dirs.home_dir().join(".rrclaw").join("data"))
}

/// 获取日志目录: ~/.rrclaw/logs/
fn log_dir() -> Result<PathBuf> {
    let base_dirs = directories::BaseDirs::new()
        .ok_or_else(|| color_eyre::eyre::eyre!("无法获取 home 目录"))?;
    Ok(base_dirs.home_dir().join(".rrclaw").join("logs"))
}

/// 初始化 tracing: stderr 只输出 warn+，日志文件输出 debug+
fn init_tracing() -> Result<()> {
    let log_dir = log_dir()?;
    std::fs::create_dir_all(&log_dir)
        .wrap_err_with(|| format!("创建日志目录失败: {}", log_dir.display()))?;

    // 文件日志: 按天滚动，debug 级别
    let file_appender = tracing_appender::rolling::daily(&log_dir, "rrclaw.log");
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(file_appender)
        .with_ansi(false)
        .with_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("rrclaw=debug")),
        );

    // stderr: 只输出 warn+（不干扰 REPL 交互）
    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(tracing_subscriber::EnvFilter::new("warn"));

    tracing_subscriber::registry()
        .with(file_layer)
        .with(stderr_layer)
        .init();

    Ok(())
}
