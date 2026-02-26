//! Daemon server — runs as a background process.
//!
//! Listens on a Unix domain socket for CLI client connections,
//! and optionally starts the Telegram Bot channel.

use color_eyre::eyre::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::memory::SqliteMemory;

use super::protocol::{ClientMessage, DaemonMessage};

/// Entry point for the daemon worker process (`rrclaw daemon-worker`).
///
/// This function does not return until the daemon is shut down.
pub async fn run_daemon_worker() -> Result<()> {
    let config = Config::load_or_init().wrap_err("Failed to load config")?;
    let data_dir = data_dir()?;
    let sock_path = super::sock_path()?;

    // Remove stale socket file
    let _ = std::fs::remove_file(&sock_path);

    // Initialize shared memory
    let memory = Arc::new(SqliteMemory::open(&data_dir).wrap_err("Failed to initialize memory")?);

    // Seed core knowledge
    let log_dir = log_dir()?;
    let config_path = Config::config_path()?;
    memory
        .seed_core_knowledge(&data_dir, &log_dir, &config_path)
        .await
        .wrap_err("Failed to seed core knowledge")?;

    // Start Telegram bot if configured
    #[cfg(feature = "telegram")]
    if config.telegram.is_some() {
        let tg_config = config.clone();
        let tg_memory = memory.clone();
        tokio::spawn(async move {
            info!("Starting Telegram Bot channel");
            if let Err(e) = crate::channels::telegram::run_telegram(tg_config, tg_memory).await {
                error!("Telegram Bot error: {:#}", e);
            }
        });
    }

    // Start Unix socket listener
    let listener = UnixListener::bind(&sock_path)
        .wrap_err_with(|| format!("Failed to bind socket: {}", sock_path.display()))?;
    info!("Daemon listening on {}", sock_path.display());

    // Register signal handler for graceful shutdown
    let sock_path_cleanup = sock_path.clone();
    tokio::spawn(async move {
        if let Ok(()) = tokio::signal::ctrl_c().await {
            info!("Received shutdown signal");
            let _ = std::fs::remove_file(&sock_path_cleanup);
            std::process::exit(0);
        }
    });

    // Accept client connections
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let config = config.clone();
                let memory = memory.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_client(stream, config, memory).await {
                        warn!("Client session error: {:#}", e);
                    }
                });
            }
            Err(e) => {
                error!("Failed to accept connection: {}", e);
            }
        }
    }
}

/// Handle a single CLI client connection.
///
/// Each client gets its own Agent instance (channel isolation).
async fn handle_client(
    stream: tokio::net::UnixStream,
    config: Config,
    memory: Arc<SqliteMemory>,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    info!("New CLI client connected");

    // Create a dedicated Agent for this session
    let provider_key = config.default.provider.clone();
    let provider_config = config
        .providers
        .get(&provider_key)
        .ok_or_else(|| color_eyre::eyre::eyre!("Provider '{}' not found in config", provider_key))?
        .clone();

    let model = config.default.model.clone();
    let temperature = config.default.temperature;

    while let Some(line) = lines.next_line().await? {
        let msg: ClientMessage = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(e) => {
                let err = DaemonMessage::Error {
                    message: format!("Invalid message: {}", e),
                };
                send_message(&mut writer, &err).await?;
                continue;
            }
        };

        match msg {
            ClientMessage::Message {
                session_id: _,
                content,
            } => {
                // Build a one-shot agent and process the message
                let response = process_message(
                    &content,
                    &config,
                    &provider_config,
                    &provider_key,
                    &model,
                    temperature,
                    &memory,
                )
                .await;

                match response {
                    Ok(text) => {
                        send_message(&mut writer, &DaemonMessage::Token { content: text }).await?;
                        send_message(&mut writer, &DaemonMessage::Done).await?;
                    }
                    Err(e) => {
                        send_message(
                            &mut writer,
                            &DaemonMessage::Error {
                                message: format!("{:#}", e),
                            },
                        )
                        .await?;
                    }
                }
            }
            ClientMessage::ConfirmResponse { .. } => {
                // TODO: forward to pending confirm request in Agent
                send_message(
                    &mut writer,
                    &DaemonMessage::Error {
                        message: "Confirm not yet implemented in daemon mode".to_string(),
                    },
                )
                .await?;
            }
        }
    }

    info!("CLI client disconnected");
    Ok(())
}

/// Process a single user message through the Agent and return the text response.
async fn process_message(
    content: &str,
    config: &Config,
    provider_config: &crate::config::ProviderConfig,
    provider_key: &str,
    model: &str,
    temperature: f64,
    memory: &Arc<SqliteMemory>,
) -> Result<String> {
    let data_dir = data_dir()?;
    let log_dir = log_dir()?;
    let config_path = Config::config_path()?;
    let workspace_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // Create provider
    let provider = crate::providers::create_provider(provider_config);
    let retry_config = crate::providers::RetryConfig {
        max_retries: config.reliability.max_retries,
        initial_backoff_ms: config.reliability.initial_backoff_ms,
        ..Default::default()
    };
    let provider: Box<dyn crate::providers::Provider> = Box::new(
        crate::providers::ReliableProvider::new(provider, retry_config),
    );

    // Load skills
    let global_skills_dir = {
        let base_dirs = directories::BaseDirs::new()
            .ok_or_else(|| color_eyre::eyre::eyre!("Cannot determine home directory"))?;
        base_dirs.home_dir().join(".rrclaw").join("skills")
    };
    let builtin = crate::skills::builtin_skills(Config::get_language());
    let skills = crate::skills::load_skills(&workspace_dir, &global_skills_dir, builtin);

    // Create provider Arc for HttpRequestTool
    let provider_arc: Arc<dyn crate::providers::Provider> =
        Arc::new(crate::providers::ReliableProvider::new(
            crate::providers::create_provider(provider_config),
            crate::providers::RetryConfig {
                max_retries: config.reliability.max_retries,
                initial_backoff_ms: config.reliability.initial_backoff_ms,
                ..Default::default()
            },
        ));

    // Create tools (no routine engine in daemon for now)
    let tools = crate::tools::create_tools(
        config.clone(),
        provider_arc,
        data_dir.clone(),
        log_dir.clone(),
        config_path.clone(),
        skills.clone(),
        memory.clone() as Arc<dyn crate::memory::Memory>,
        None,
    );

    // Security policy
    let policy = crate::security::SecurityPolicy {
        autonomy: config.security.autonomy.clone(),
        allowed_commands: config.security.allowed_commands.clone(),
        workspace_dir,
        blocked_paths: crate::security::SecurityPolicy::default().blocked_paths,
        http_allowed_hosts: config.security.http_allowed_hosts.clone(),
        injection_check: config.security.injection_check,
    };

    // Identity
    let rrclaw_home = data_dir
        .parent()
        .unwrap_or(data_dir.as_path())
        .to_path_buf();
    let identity_context =
        crate::agent::identity::load_identity_context(&policy.workspace_dir, &rrclaw_home);

    // Create agent
    let mut agent = crate::agent::Agent::new(
        provider,
        tools,
        Box::new(memory.clone()),
        policy,
        provider_key.to_string(),
        provider_config.base_url.clone(),
        model.to_string(),
        temperature,
        skills,
        identity_context,
    );

    // Process message (non-streaming for now)
    let response = agent.process_message(content).await?;
    Ok(response)
}

/// Send a DaemonMessage as a JSON line over the writer.
async fn send_message(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    msg: &DaemonMessage,
) -> Result<()> {
    let mut json = serde_json::to_string(msg)?;
    json.push('\n');
    writer.write_all(json.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

fn data_dir() -> Result<PathBuf> {
    let base_dirs = directories::BaseDirs::new()
        .ok_or_else(|| color_eyre::eyre::eyre!("Cannot determine home directory"))?;
    Ok(base_dirs.home_dir().join(".rrclaw").join("data"))
}

fn log_dir() -> Result<PathBuf> {
    let base_dirs = directories::BaseDirs::new()
        .ok_or_else(|| color_eyre::eyre::eyre!("Cannot determine home directory"))?;
    Ok(base_dirs.home_dir().join(".rrclaw").join("logs"))
}
