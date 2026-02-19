use std::collections::HashMap;
use std::sync::Arc;

use color_eyre::eyre::Result;
use teloxide::prelude::*;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::agent::Agent;
use crate::config::Config;
use crate::memory::SqliteMemory;
use crate::security::SecurityPolicy;

/// Agent 工厂: 为每个 chat 创建独立的 Agent
pub struct AgentFactory {
    config: Config,
    memory: Arc<SqliteMemory>,
}

impl AgentFactory {
    pub fn new(config: Config, memory: Arc<SqliteMemory>) -> Self {
        Self { config, memory }
    }

    /// 为指定 chat 创建一个 Agent
    fn create_agent(&self) -> Result<Agent> {
        let provider_key = &self.config.default.provider;
        let provider_config = self
            .config
            .providers
            .get(provider_key)
            .ok_or_else(|| color_eyre::eyre::eyre!("Provider '{}' 未配置", provider_key))?;

        let provider = crate::providers::create_provider(provider_config);
        let data_dir = {
            let base_dirs = directories::BaseDirs::new()
                .ok_or_else(|| color_eyre::eyre::eyre!("无法获取 home 目录"))?;
            base_dirs.home_dir().join(".rrclaw").join("data")
        };
        let log_dir = {
            let base_dirs = directories::BaseDirs::new()
                .ok_or_else(|| color_eyre::eyre::eyre!("无法获取 home 目录"))?;
            base_dirs.home_dir().join(".rrclaw").join("logs")
        };
        let config_path = crate::config::Config::config_path()?;
        let tools = crate::tools::create_tools(
            self.config.clone(),
            data_dir,
            log_dir,
            config_path,
            vec![], // Telegram 暂不加载 skills
        );
        let policy = SecurityPolicy {
            autonomy: self.config.security.autonomy.clone(),
            allowed_commands: self.config.security.allowed_commands.clone(),
            workspace_dir: std::env::current_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from(".")),
            blocked_paths: SecurityPolicy::default().blocked_paths,
        };

        Ok(Agent::new(
            provider,
            tools,
            Box::new(self.memory.clone()),
            policy,
            provider_key.to_string(),
            provider_config.base_url.clone(),
            self.config.default.model.clone(),
            self.config.default.temperature,
        ))
    }
}

/// 运行 Telegram Bot
pub async fn run_telegram(config: Config, memory: Arc<SqliteMemory>) -> Result<()> {
    let telegram_config = config
        .telegram
        .as_ref()
        .ok_or_else(|| color_eyre::eyre::eyre!("Telegram 未配置。请在 config.toml 中添加 [telegram] 配置。"))?;

    let bot = Bot::new(&telegram_config.bot_token);
    let allowed_ids: Vec<i64> = telegram_config.allowed_chat_ids.clone();

    let factory = Arc::new(AgentFactory::new(config, memory));
    let agents: Arc<Mutex<HashMap<ChatId, Agent>>> = Arc::new(Mutex::new(HashMap::new()));

    info!("Telegram Bot 启动中...");

    teloxide::repl(bot, move |bot: Bot, msg: Message| {
        let factory = factory.clone();
        let agents = agents.clone();
        let allowed_ids = allowed_ids.clone();

        async move {
            let chat_id = msg.chat.id;

            // 检查访问权限
            if !allowed_ids.is_empty() && !allowed_ids.contains(&chat_id.0) {
                debug!("拒绝未授权 chat: {}", chat_id);
                bot.send_message(chat_id, "⛔ 未授权的 Chat ID")
                    .await?;
                return Ok(());
            }

            let text = match msg.text() {
                Some(t) if !t.is_empty() => t.to_string(),
                _ => return Ok(()),
            };

            info!("收到消息 [chat={}]: {}", chat_id, text);

            // 获取或创建该 chat 的 Agent
            let mut agents_map = agents.lock().await;
            if let std::collections::hash_map::Entry::Vacant(e) = agents_map.entry(chat_id) {
                match factory.create_agent() {
                    Ok(agent) => {
                        e.insert(agent);
                    }
                    Err(err) => {
                        warn!("创建 Agent 失败: {:#}", err);
                        bot.send_message(chat_id, format!("Agent 创建失败: {}", err))
                            .await?;
                        return Ok(());
                    }
                }
            }

            let agent = agents_map.get_mut(&chat_id).unwrap();

            // 处理消息
            match agent.process_message(&text).await {
                Ok(reply) => {
                    if !reply.is_empty() {
                        // 分段发送（Telegram 消息限制 4096 字符）
                        for chunk in split_message(&reply, 4000) {
                            bot.send_message(chat_id, chunk).await?;
                        }
                    }
                }
                Err(e) => {
                    warn!("处理消息失败 [chat={}]: {:#}", chat_id, e);
                    bot.send_message(chat_id, format!("❌ 错误: {}", e))
                        .await?;
                }
            }

            Ok(())
        }
    })
    .await;

    Ok(())
}

/// 将长消息分段（Telegram 限制 4096 字符）
fn split_message(text: &str, max_len: usize) -> Vec<&str> {
    if text.len() <= max_len {
        return vec![text];
    }

    let mut chunks = Vec::new();
    let mut start = 0;

    while start < text.len() {
        let mut end = (start + max_len).min(text.len());
        // 确保在 UTF-8 字符边界
        while end > start && !text.is_char_boundary(end) {
            end -= 1;
        }
        chunks.push(&text[start..end]);
        start = end;
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_short_message() {
        let chunks = split_message("hello", 4000);
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn split_long_message() {
        let long_text = "a".repeat(5000);
        let chunks = split_message(&long_text, 4000);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 4000);
        assert_eq!(chunks[1].len(), 1000);
    }

    #[test]
    fn split_respects_utf8_boundary() {
        // 中文字符占 3 bytes
        let text = "你".repeat(2000); // 6000 bytes
        let chunks = split_message(&text, 4000);
        assert!(chunks.len() >= 2);
        // 确保每个 chunk 都是合法 UTF-8
        for chunk in &chunks {
            assert!(chunk.len() <= 4000);
            let _ = chunk.to_string(); // 不应 panic
        }
    }
}
