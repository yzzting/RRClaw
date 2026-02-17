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
    let config = rrclaw::config::Config::load_or_init()
        .wrap_err("加载配置失败")?;

    // 确定使用的 provider
    let provider_key = provider_name
        .as_deref()
        .unwrap_or(&config.default.provider);

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
    let model = model_override
        .unwrap_or_else(|| config.default.model.clone());

    // 创建 Provider
    let provider = rrclaw::providers::create_provider(provider_config);

    // 创建 Tools
    let tools = rrclaw::tools::create_tools();

    // 创建 Memory（Arc 共享给 Agent 和 CLI）
    let data_dir = data_dir()?;
    let memory = Arc::new(
        rrclaw::memory::SqliteMemory::open(&data_dir)
            .wrap_err("初始化 Memory 失败")?,
    );

    // 创建 SecurityPolicy
    let policy = rrclaw::security::SecurityPolicy {
        autonomy: config.security.autonomy.clone(),
        allowed_commands: config.security.allowed_commands.clone(),
        workspace_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        blocked_paths: rrclaw::security::SecurityPolicy::default().blocked_paths,
    };

    // 创建 Agent
    let mut agent = rrclaw::agent::Agent::new(
        provider,
        tools,
        Box::new(memory.clone()),
        policy,
        model,
        config.default.temperature,
    );

    // 运行
    match message {
        Some(msg) => rrclaw::channels::cli::run_single(&mut agent, &msg, &memory).await?,
        None => rrclaw::channels::cli::run_repl(&mut agent, &memory).await?,
    }

    Ok(())
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

    let content = std::fs::read_to_string(&config_path)
        .wrap_err("读取配置文件失败")?;
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
