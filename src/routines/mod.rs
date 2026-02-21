//! Routines 定时任务系统
//!
//! 让 Agent 从被动响应转变为主动助手，定期自动执行任务。
//!
//! # 架构
//! - `Routine`：单个任务的配置（cron 表达式 + message + channel）
//! - `RoutineEngine`：管理所有 Routine，启动 cron 调度器
//!
//! # 使用示例
//! ```toml
//! # config.toml
//! [[routines.jobs]]
//! name = "morning_brief"
//! schedule = "0 8 * * *"
//! message = "用中文总结今天的工作计划"
//! channel = "cli"
//! enabled = true
//! ```

use std::sync::Arc;

use color_eyre::eyre::{eyre, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio_cron_scheduler::{Job, JobScheduler};
use tracing::{error, info, warn};

use crate::config::Config;
use crate::memory::Memory;

// ─── 数据结构 ─────────────────────────────────────────────────────────────────

/// 单个定时任务的配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Routine {
    /// 唯一名称（用于 `/routine` 命令标识）
    pub name: String,
    /// cron 表达式，标准 5 字段格式：分 时 日 月 周
    /// 示例：
    ///   "0 8 * * *"    每天早 8 点
    ///   "0 * * * *"    每小时整点
    ///   "0 9 * * 1"    每周一早 9 点
    pub schedule: String,
    /// 触发时发送给 Agent 的消息（即"用户提问"）
    pub message: String,
    /// 执行结果发送到哪个通道：
    ///   "cli"       → 打印到 stdout（带 [Routine] 前缀）
    ///   "telegram"  → 通过 Telegram Bot 发送（需配置 bot_token）
    #[serde(default = "default_channel")]
    pub channel: String,
    /// 是否启用（false 时跳过调度）
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// 来源：config.toml 配置 还是 /routine add 动态创建
    #[serde(default)]
    pub source: RoutineSource,
}

fn default_channel() -> String {
    "cli".to_string()
}

fn default_enabled() -> bool {
    true
}

/// Routine 的创建来源
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RoutineSource {
    #[default]
    Config,     // 来自 config.toml
    Dynamic,    // 来自 /routine add 命令（持久化到 SQLite）
}

/// 单次执行记录
#[derive(Debug, Clone)]
pub struct RoutineExecution {
    pub routine_name: String,
    pub started_at: String,    // ISO 8601
    pub finished_at: String,   // ISO 8601
    pub success: bool,
    pub output_preview: String, // 前 200 字符
    pub error: Option<String>,
}

// ─── RoutineEngine ───────────────────────────────────────────────────────────

/// 定时任务引擎
///
/// 持有调度器和所有 Routine 配置，负责启动调度和执行任务。
///
/// # 线程安全
/// `RoutineEngine` 通过 `Arc<RoutineEngine>` 在 tokio task 间共享。
/// `routines` 用 `std::sync::RwLock` 保护，支持 `&self` 方法动态增删（persist_* API）。
/// 调度器回调（Job handler）是 `async move` 闭包，内部 clone Arc 引用。
pub struct RoutineEngine {
    routines: std::sync::RwLock<Vec<Routine>>,
    scheduler: JobScheduler,
    config: Arc<Config>,
    memory: Arc<dyn Memory>,
    db: Arc<Mutex<Connection>>,
}

impl RoutineEngine {
    /// 创建 RoutineEngine
    ///
    /// # 参数
    /// - `routines`: 来自 config.toml 的静态 Routine 列表
    /// - `config`: 全局配置（用于创建 Agent 时的 Provider 配置）
    /// - `memory`: 共享 Memory（Routine Agent 和主 Agent 共享记忆）
    /// - `db_path`: SQLite 数据库路径（存储动态 Routine + 执行日志）
    pub async fn new(
        mut routines: Vec<Routine>,
        config: Arc<Config>,
        memory: Arc<dyn Memory>,
        db_path: &std::path::Path,
    ) -> Result<Self> {
        // 初始化数据库
        let conn = Connection::open(db_path)
            .map_err(|e| eyre!("打开 Routines 数据库失败: {}", e))?;
        Self::init_db(&conn)?;

        // 从 SQLite 加载动态创建的 Routine（合并到 config 来的列表）
        let dynamic_routines = Self::load_dynamic_routines(&conn)?;
        routines.extend(dynamic_routines);

        let scheduler = JobScheduler::new()
            .await
            .map_err(|e| eyre!("创建 JobScheduler 失败: {}", e))?;

        Ok(Self {
            routines: std::sync::RwLock::new(routines),
            scheduler,
            config,
            memory,
            db: Arc::new(Mutex::new(conn)),
        })
    }

    /// 初始化 SQLite 表
    fn init_db(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS routines (
                name        TEXT PRIMARY KEY,
                schedule    TEXT NOT NULL,
                message     TEXT NOT NULL,
                channel     TEXT NOT NULL DEFAULT 'cli',
                enabled     INTEGER NOT NULL DEFAULT 1,
                created_at  TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS routines_log (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                routine_name TEXT NOT NULL,
                started_at   TEXT NOT NULL,
                finished_at  TEXT NOT NULL,
                success      INTEGER NOT NULL,
                output       TEXT NOT NULL DEFAULT '',
                error        TEXT
            );
            "#,
        )
        .map_err(|e| eyre!("初始化 Routines 数据库失败: {}", e))?;
        Ok(())
    }

    /// 从 SQLite 加载动态 Routine（/routine add 创建的）
    fn load_dynamic_routines(conn: &Connection) -> Result<Vec<Routine>> {
        let mut stmt = conn
            .prepare("SELECT name, schedule, message, channel, enabled FROM routines")
            .map_err(|e| eyre!("查询动态 Routines 失败: {}", e))?;

        let routines = stmt
            .query_map([], |row| {
                Ok(Routine {
                    name: row.get(0)?,
                    schedule: row.get(1)?,
                    message: row.get(2)?,
                    channel: row.get(3)?,
                    enabled: row.get::<_, i32>(4)? != 0,
                    source: RoutineSource::Dynamic,
                })
            })
            .map_err(|e| eyre!("解析动态 Routines 失败: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(routines)
    }

    /// 启动所有已启用的 Routine 调度
    ///
    /// 为每个 enabled Routine 注册 cron job，然后启动调度器。
    /// 调度器在后台 tokio task 中运行，不阻塞调用方。
    pub async fn start(self: Arc<Self>) -> Result<()> {
        let enabled_routines: Vec<Routine> = self
            .routines
            .read()
            .unwrap()
            .iter()
            .filter(|r| r.enabled)
            .cloned()
            .collect();

        if enabled_routines.is_empty() {
            info!("没有已启用的 Routine，跳过调度器启动");
            return Ok(());
        }

        for routine in &enabled_routines {
            info!("注册 Routine: {} (schedule={})", routine.name, routine.schedule);
            let engine = Arc::clone(&self);
            let routine_name = routine.name.clone();
            let routine_schedule = routine.schedule.clone();

            // 构造 cron job（async handler）
            let job = Job::new_async(routine_schedule.as_str(), move |_uuid, _lock| {
                let engine = Arc::clone(&engine);
                let name = routine_name.clone();
                Box::pin(async move {
                    info!("Routine 触发: {}", name);
                    if let Err(e) = engine.execute_routine(&name).await {
                        error!("Routine 执行失败: {} - {}", name, e);
                    }
                })
            })
            .map_err(|e| eyre!("创建 cron job 失败 ({}): {}", routine.name, e))?;

            self.scheduler
                .add(job)
                .await
                .map_err(|e| eyre!("添加 job 到调度器失败: {}", e))?;
        }

        self.scheduler
            .start()
            .await
            .map_err(|e| eyre!("启动 JobScheduler 失败: {}", e))?;

        info!("RoutineEngine 已启动，共 {} 个活跃任务", enabled_routines.len());
        Ok(())
    }

    /// 执行单个 Routine（含超时保护 + 失败重试）
    ///
    /// 对外暴露，供 `/routine run <name>` 命令手动触发。
    pub async fn execute_routine(&self, name: &str) -> Result<String> {
        let routine = self
            .routines
            .read()
            .unwrap()
            .iter()
            .find(|r| r.name == name)
            .ok_or_else(|| eyre!("Routine '{}' 不存在", name))?
            .clone();

        if !routine.enabled {
            return Ok(format!("Routine '{}' 已禁用，跳过执行。", name));
        }

        const MAX_RETRIES: usize = 3;
        const RETRY_DELAY_SECS: u64 = 300; // 5 分钟
        const TIMEOUT_SECS: u64 = 300;     // 5 分钟超时

        let started_at = chrono::Utc::now().to_rfc3339();
        let mut last_error = String::new();

        for attempt in 0..MAX_RETRIES {
            if attempt > 0 {
                info!("Routine '{}' 第 {} 次重试，等待 {}s...", name, attempt, RETRY_DELAY_SECS);
                tokio::time::sleep(std::time::Duration::from_secs(RETRY_DELAY_SECS)).await;
            }

            match tokio::time::timeout(
                std::time::Duration::from_secs(TIMEOUT_SECS),
                self.run_once(&routine),
            )
            .await
            {
                Ok(Ok(output)) => {
                    let finished_at = chrono::Utc::now().to_rfc3339();
                    info!("Routine '{}' 执行成功", name);
                    self.log_execution(RoutineExecution {
                        routine_name: name.to_string(),
                        started_at,
                        finished_at,
                        success: true,
                        output_preview: output.chars().take(200).collect(),
                        error: None,
                    })
                    .await;
                    self.send_result(&routine, &output).await;
                    return Ok(output);
                }
                Ok(Err(e)) => {
                    warn!("Routine '{}' 执行出错（第 {} 次）: {}", name, attempt + 1, e);
                    last_error = e.to_string();
                }
                Err(_) => {
                    warn!("Routine '{}' 执行超时（第 {} 次，限制 {}s）", name, attempt + 1, TIMEOUT_SECS);
                    last_error = format!("执行超时（超过 {} 秒）", TIMEOUT_SECS);
                }
            }
        }

        // 全部重试失败
        let finished_at = chrono::Utc::now().to_rfc3339();
        error!("Routine '{}' 全部 {} 次重试均失败，最后错误: {}", name, MAX_RETRIES, last_error);
        self.log_execution(RoutineExecution {
            routine_name: name.to_string(),
            started_at,
            finished_at,
            success: false,
            output_preview: String::new(),
            error: Some(last_error.clone()),
        })
        .await;
        let error_msg = format!("[Routine: {}] 执行失败（{} 次重试后）: {}", name, MAX_RETRIES, last_error);
        self.send_result(&routine, &error_msg).await;
        Err(eyre!("{}", error_msg))
    }

    /// 创建独立 Agent 并执行一次任务消息
    async fn run_once(&self, routine: &Routine) -> Result<String> {
        use crate::agent::Agent;
        use crate::providers::{create_provider, ReliableProvider, RetryConfig};
        use crate::security::SecurityPolicy;
        use crate::tools::create_tools;

        let provider_key = &self.config.default.provider;
        let provider_config = self
            .config
            .providers
            .get(provider_key)
            .ok_or_else(|| eyre!("Provider '{}' 未配置", provider_key))?;

        let raw_provider = create_provider(provider_config);
        let retry_config = RetryConfig {
            max_retries: self.config.reliability.max_retries,
            initial_backoff_ms: self.config.reliability.initial_backoff_ms,
            ..Default::default()
        };
        let provider: Box<dyn crate::providers::Provider> =
            Box::new(ReliableProvider::new(raw_provider, retry_config));

        let base_dirs = directories::BaseDirs::new()
            .ok_or_else(|| eyre!("无法获取 home 目录"))?;
        let rrclaw_dir = base_dirs.home_dir().join(".rrclaw");
        let data_dir = rrclaw_dir.join("data");
        let log_dir = rrclaw_dir.join("logs");
        let config_path = crate::config::Config::config_path()?;

        let policy = SecurityPolicy {
            autonomy: self.config.security.autonomy.clone(),
            allowed_commands: self.config.security.allowed_commands.clone(),
            workspace_dir: std::env::current_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from(".")),
            blocked_paths: SecurityPolicy::default().blocked_paths,
            http_allowed_hosts: self.config.security.http_allowed_hosts.clone(),
            injection_check: self.config.security.injection_check,
        };

        let tools = create_tools(
            (*self.config).clone(),
            data_dir.clone(),
            log_dir,
            config_path,
            vec![], // Routine 不加载 skills（保持执行简洁）
            Arc::clone(&self.memory),
            None,   // Routine 内部 Agent 不注册 RoutineTool（避免循环调度）
        );

        let provider_name = provider_key.clone();
        let model = self.config.default.model.clone();
        let temperature = self.config.default.temperature;

        // ★ Step 0: 从共享 Memory 召回上次成功的方法描述
        let memory_key = format!("routine:{}:approach", routine.name);
        let recalled = self.memory
            .recall(&memory_key, 1)
            .await
            .unwrap_or_default();

        // ★ Step 1: 构造增强版 message（有历史方法时注入前缀）
        let enhanced_message = build_enhanced_message(&recalled, &routine.message);

        // ★ Step 2: 传入共享 Memory（LLM 可通过 memory_store 保存有效方法）
        let mut agent = Agent::new(
            provider,
            tools,
            Box::new(Arc::clone(&self.memory)), // 共享 Memory，读写均生效
            policy,
            provider_name,
            provider_config.base_url.clone(),
            model,
            temperature,
            vec![],  // 无 skills
            None,    // 无身份文件上下文（Routine 是系统任务，不需要用户偏好）
        );

        // Routine 在 Full 模式下执行（不需要用户逐一确认，无交互界面）
        agent.set_autonomy(crate::security::AutonomyLevel::Full);
        // 注入 Routine 专属 system prompt 段
        agent.set_routine_name(routine.name.clone());

        let output = agent.process_message(&enhanced_message).await?;
        Ok(output)
    }

    /// 将执行结果路由到指定通道
    async fn send_result(&self, routine: &Routine, output: &str) {
        let message = format!("\n[Routine: {}]\n{}\n", routine.name, output);
        match routine.channel.as_str() {
            "cli" => {
                // 直接打印到 stdout
                // 使用 eprintln 而非 println，避免干扰 reedline 的行编辑状态
                eprintln!("{}", message);
            }
            "telegram" => {
                if self.config.telegram.is_some() {
                    if let Err(e) = self.send_telegram(output).await {
                        warn!("Routine '{}' Telegram 发送失败: {}", routine.name, e);
                    }
                } else {
                    warn!("Routine '{}' 配置了 channel=telegram，但未找到 Telegram 配置", routine.name);
                    eprintln!("{}", message); // 降级打印到 CLI
                }
            }
            other => {
                warn!("Routine '{}' 使用了未知 channel: {}，降级为 cli", routine.name, other);
                eprintln!("{}", message);
            }
        }
    }

    /// 通过 Telegram Bot API 发送消息（使用已有的 reqwest 依赖）
    async fn send_telegram(&self, message: &str) -> Result<()> {
        let tg_config = self
            .config
            .telegram
            .as_ref()
            .ok_or_else(|| eyre!("Telegram 未配置"))?;

        // 发送给第一个允许的 chat_id（如未限制则无法发送）
        let chat_id = tg_config
            .allowed_chat_ids
            .first()
            .ok_or_else(|| eyre!("Telegram allowed_chat_ids 为空，无法确定 Routine 结果发送对象。\n请在 config.toml 中设置 [telegram] allowed_chat_ids = [your_chat_id]"))?;

        let url = format!(
            "https://api.telegram.org/bot{}/sendMessage",
            tg_config.bot_token
        );

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        let resp = client
            .post(&url)
            .json(&serde_json::json!({
                "chat_id": chat_id,
                "text": message,
                "parse_mode": "Markdown"
            }))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(eyre!("Telegram API 返回错误: {} - {}", status, body));
        }

        Ok(())
    }

    /// 记录执行历史到 SQLite
    async fn log_execution(&self, exec: RoutineExecution) {
        let db = self.db.lock().await;
        let _ = db.execute(
            "INSERT INTO routines_log \
             (routine_name, started_at, finished_at, success, output, error) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                exec.routine_name,
                exec.started_at,
                exec.finished_at,
                exec.success as i32,
                exec.output_preview,
                exec.error,
            ],
        );
    }

    // ─── 动态管理 API（供 /routine 斜杠命令使用）───────────────────────────

    /// 列出所有 Routine（包括 disabled），返回当前快照
    pub fn list_routines(&self) -> Vec<Routine> {
        self.routines.read().unwrap().clone()
    }

    /// 查询单个 Routine（返回 clone）
    pub fn get_routine(&self, name: &str) -> Option<Routine> {
        self.routines.read().unwrap().iter().find(|r| r.name == name).cloned()
    }

    /// 查询最近 N 条执行记录
    pub async fn get_recent_logs(&self, limit: usize) -> Vec<RoutineExecution> {
        let db = self.db.lock().await;
        let mut stmt = match db.prepare(
            "SELECT routine_name, started_at, finished_at, success, output, error \
             FROM routines_log ORDER BY id DESC LIMIT ?1",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };

        stmt.query_map(params![limit as i64], |row| {
            Ok(RoutineExecution {
                routine_name: row.get(0)?,
                started_at: row.get(1)?,
                finished_at: row.get(2)?,
                success: row.get::<_, i32>(3)? != 0,
                output_preview: row.get(4)?,
                error: row.get(5)?,
            })
        })
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    }

    /// 添加动态 Routine 并持久化到 SQLite
    ///
    /// 注意：新添加的 Routine 不会立即生效（需要重启 RoutineEngine 才能注册到调度器）。
    /// 当前实现简化为：添加后提示用户重启 RRClaw 生效。
    pub async fn add_routine(&mut self, routine: Routine) -> Result<()> {
        // 检查名称是否重复
        if self.routines.read().unwrap().iter().any(|r| r.name == routine.name) {
            return Err(eyre!("Routine '{}' 已存在，请先删除再添加", routine.name));
        }

        // 验证 cron 表达式（尝试用 tokio-cron-scheduler 解析）
        // 最简单的验证：字段数量检查（5 字段 cron）
        let field_count = routine.schedule.split_whitespace().count();
        if field_count != 5 {
            return Err(eyre!(
                "cron 表达式格式错误：应为 5 个字段（分 时 日 月 周），当前 {} 个字段。\n示例：\"0 8 * * *\" 表示每天早 8 点",
                field_count
            ));
        }

        // 持久化到 SQLite
        {
            let db = self.db.lock().await;
            db.execute(
                "INSERT OR REPLACE INTO routines \
                 (name, schedule, message, channel, enabled, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    routine.name,
                    routine.schedule,
                    routine.message,
                    routine.channel,
                    routine.enabled as i32,
                    chrono::Utc::now().to_rfc3339(),
                ],
            )
            .map_err(|e| eyre!("保存 Routine 失败: {}", e))?;
        }

        self.routines.write().unwrap().push(routine);
        Ok(())
    }

    /// 删除 Routine（仅支持 Dynamic 来源，Config 来源需在 config.toml 中删除）
    pub async fn delete_routine(&mut self, name: &str) -> Result<()> {
        let source = {
            let guard = self.routines.read().unwrap();
            let routine = guard
                .iter()
                .find(|r| r.name == name)
                .ok_or_else(|| eyre!("Routine '{}' 不存在", name))?;
            routine.source.clone()
        };

        if source == RoutineSource::Config {
            return Err(eyre!(
                "Routine '{}' 来自 config.toml，请直接编辑配置文件删除",
                name
            ));
        }

        let db = self.db.lock().await;
        db.execute("DELETE FROM routines WHERE name = ?1", params![name])
            .map_err(|e| eyre!("删除 Routine 失败: {}", e))?;
        drop(db);

        self.routines.write().unwrap().retain(|r| r.name != name);
        Ok(())
    }

    /// 启用 / 禁用 Routine
    pub async fn set_enabled(&mut self, name: &str, enabled: bool) -> Result<()> {
        let source = {
            let guard = self.routines.read().unwrap();
            guard
                .iter()
                .find(|r| r.name == name)
                .ok_or_else(|| eyre!("Routine '{}' 不存在", name))?
                .source
                .clone()
        };

        // 如果是 Dynamic 来源，持久化到 SQLite
        if source == RoutineSource::Dynamic {
            let db = self.db.lock().await;
            db.execute(
                "UPDATE routines SET enabled = ?1 WHERE name = ?2",
                params![enabled as i32, name],
            )
            .map_err(|e| eyre!("更新 Routine 状态失败: {}", e))?;
        }

        self.routines
            .write()
            .unwrap()
            .iter_mut()
            .filter(|r| r.name == name)
            .for_each(|r| r.enabled = enabled);

        Ok(())
    }

    // ─── 持久化 API（写 SQLite + 同步更新内存 Vec）─────────────────────────
    // 设计说明：
    // RoutineEngine 被包装在 Arc<RoutineEngine> 中，调度器回调也持有这个 Arc。
    // routines 用 std::sync::RwLock 保护，&self 方法可安全修改内存状态。
    // 注意：新添加的 Routine 需重启才会被调度器自动触发（热加载为 V2 功能）；
    //       但 list/run/execute 立即可见新添加的 Routine，无需重启。

    /// 持久化新增 Routine 到 SQLite 并同步更新内存 Vec
    pub async fn persist_add_routine(&self, routine: &Routine) -> Result<()> {
        // 重复检查（先持有 read lock，检查完立即释放）
        {
            if self.routines.read().unwrap().iter().any(|r| r.name == routine.name) {
                return Err(eyre!("Routine '{}' 已存在，请先删除再添加", routine.name));
            }
        }
        let field_count = routine.schedule.split_whitespace().count();
        if field_count != 5 {
            return Err(eyre!(
                "schedule 格式错误：应为 5 个字段的 cron 表达式，当前 {} 个字段。\n示例：\"0 8 * * *\" 表示每天早 8 点",
                field_count
            ));
        }
        // 写 DB（持有 Mutex，完成后立即释放）
        {
            let db = self.db.lock().await;
            db.execute(
                "INSERT OR REPLACE INTO routines \
                 (name, schedule, message, channel, enabled, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    routine.name,
                    routine.schedule,
                    routine.message,
                    routine.channel,
                    routine.enabled as i32,
                    chrono::Utc::now().to_rfc3339(),
                ],
            )
            .map_err(|e| eyre!("保存 Routine 失败: {}", e))?;
        }
        // 同步更新内存 Vec（write lock，短暂持有）
        self.routines.write().unwrap().push(routine.clone());
        Ok(())
    }

    /// 从 SQLite 删除 Routine 并同步更新内存 Vec
    pub async fn persist_delete_routine(&self, name: &str) -> Result<()> {
        {
            let guard = self.routines.read().unwrap();
            let routine = guard
                .iter()
                .find(|r| r.name == name)
                .ok_or_else(|| eyre!("Routine '{}' 不存在", name))?;
            if routine.source == RoutineSource::Config {
                return Err(eyre!(
                    "Routine '{}' 来自 config.toml，请直接编辑配置文件删除",
                    name
                ));
            }
        }
        {
            let db = self.db.lock().await;
            db.execute("DELETE FROM routines WHERE name = ?1", params![name])
                .map_err(|e| eyre!("删除 Routine 失败: {}", e))?;
        }
        // 同步从内存移除（调度器残留 job 下次触发时会得到"不存在"错误，无害）
        self.routines.write().unwrap().retain(|r| r.name != name);
        Ok(())
    }

    /// 在 SQLite 中更新 enabled 状态并同步更新内存 Vec
    pub async fn persist_set_enabled(&self, name: &str, enabled: bool) -> Result<()> {
        {
            if !self.routines.read().unwrap().iter().any(|r| r.name == name) {
                return Err(eyre!("Routine '{}' 不存在", name));
            }
        }
        {
            let db = self.db.lock().await;
            db.execute(
                "UPDATE routines SET enabled = ?1 WHERE name = ?2",
                params![enabled as i32, name],
            )
            .map_err(|e| eyre!("更新 Routine 状态失败: {}", e))?;
        }
        self.routines
            .write()
            .unwrap()
            .iter_mut()
            .filter(|r| r.name == name)
            .for_each(|r| r.enabled = enabled);
        Ok(())
    }
}


// ─── 自然语言时间解析 ───────────────────────────────────────────────────────

use regex::Regex;

/// 根据 Memory recall 结果构造增强版 message
///
/// 若召回到上次成功方法，注入 `[历史成功方法参考]` 前缀供 LLM 优先参考。
/// 未找到历史记录时返回原始 message，行为与之前一致。
pub(crate) fn build_enhanced_message(
    recalled: &[crate::memory::MemoryEntry],
    message: &str,
) -> String {
    if let Some(entry) = recalled.first() {
        format!(
            "[历史成功方法参考]\n{}\n\n---\n{}",
            entry.content, message
        )
    } else {
        message.to_string()
    }
}

/// 将自然语言时间描述或 cron 表达式转换为标准 5 字段 cron 表达式
///
/// - 若输入已是 5 字段 cron 格式，直接原样返回
/// - 否则尝试规则匹配中文自然语言时间描述
/// - 失败则返回错误（V2 可扩展 LLM 回退）
pub fn parse_schedule_to_cron(desc: &str) -> Result<String> {
    let desc = desc.trim();

    // 0. 若已是 cron 表达式（5 个非空字段），直接返回
    let parts: Vec<&str> = desc.split_whitespace().collect();
    if parts.len() == 5 {
        return Ok(desc.to_string());
    }

    // 1. 每天早上 X 点
    if let Ok(re) = Regex::new(r"每?天早上(\d{1,2})点?") {
        if let Some(caps) = re.captures(desc) {
            let hour: u32 = caps.get(1).unwrap().as_str().parse().map_err(|_| {
                eyre!("无效的小时数")
            })?;
            if hour < 24 {
                return Ok(format!("0 {} * * *", hour));
            }
        }
    }

    // 2. 每天下午 X 点（下午1点=13点，下午12点=12点即中午）
    if let Ok(re) = Regex::new(r"每?天下午(\d{1,2})点?") {
        if let Some(caps) = re.captures(desc) {
            let hour: u32 = caps.get(1).unwrap().as_str().parse().map_err(|_| {
                eyre!("无效的小时数")
            })?;
            let hour_24 = if hour == 12 { 12u32 } else { hour + 12 };
            if hour_24 < 24 {
                return Ok(format!("0 {} * * *", hour_24));
            }
        }
    }

    // 3. 每天晚上 X 点（晚上8点=20点，晚上12点=0点即午夜）
    if let Ok(re) = Regex::new(r"每?天晚上(\d{1,2})点?") {
        if let Some(caps) = re.captures(desc) {
            let hour: u32 = caps.get(1).unwrap().as_str().parse().map_err(|_| {
                eyre!("无效的小时数")
            })?;
            let hour_24 = if hour == 12 { 0u32 } else { hour + 12 };
            if hour_24 < 24 {
                return Ok(format!("0 {} * * *", hour_24));
            }
        }
    }

    // 4. 每天 X 点（通用）
    if let Ok(re) = Regex::new(r"每?天(\d{1,2})点?") {
        if let Some(caps) = re.captures(desc) {
            let hour: u32 = caps.get(1).unwrap().as_str().parse().map_err(|_| {
                eyre!("无效的小时数")
            })?;
            if hour < 24 {
                return Ok(format!("0 {} * * *", hour));
            }
        }
    }

    // 5. 每小时
    if desc == "每小时" || desc == "每小时整点" || desc == "每时" {
        return Ok("0 * * * *".to_string());
    }

    // 5.1. 每 X 分钟
    if let Ok(re) = Regex::new(r"每(\d+)分钟") {
        if let Some(caps) = re.captures(desc) {
            let minutes: u32 = caps.get(1).unwrap().as_str().parse().map_err(|_| {
                eyre!("无效的分钟数")
            })?;
            if minutes > 0 && minutes <= 59 {
                return Ok(format!("*/{} * * * *", minutes));
            }
        }
    }

    // 6. 每 X 小时
    if let Ok(re) = Regex::new(r"每(\d+)小时") {
        if let Some(caps) = re.captures(desc) {
            let hours: u32 = caps.get(1).unwrap().as_str().parse().map_err(|_| {
                eyre!("无效的小时数")
            })?;
            if hours > 0 && hours <= 24 {
                return Ok(format!("0 */{} * * *", hours));
            }
        }
    }

    // 7. 每周 X 早上/下午/晚上
    let week_patterns = [
        ("周一", 1), ("周二", 2), ("周三", 3), ("周四", 4),
        ("周五", 5), ("周六", 6), ("周日", 7), ("周末", 6),
    ];
    for (day_name, day_num) in week_patterns {
        // 每周X早上X点
        let pattern = format!(r"每{}早上(\d{{1,2}})点?", day_name);
        if let Ok(re) = Regex::new(&pattern) {
            if let Some(caps) = re.captures(desc) {
                let hour: u32 = caps.get(1).unwrap().as_str().parse().map_err(|_| {
                    eyre!("无效的小时数")
                })?;
                if hour < 24 {
                    return Ok(format!("0 {} * * {}", hour, day_num));
                }
            }
        }
        // 每周X下午X点（下午12点=12点即中午）
        let pattern = format!(r"每{}下午(\d{{1,2}})点?", day_name);
        if let Ok(re) = Regex::new(&pattern) {
            if let Some(caps) = re.captures(desc) {
                let hour: u32 = caps.get(1).unwrap().as_str().parse().map_err(|_| {
                    eyre!("无效的小时数")
                })?;
                let hour_24 = if hour == 12 { 12u32 } else { hour + 12 };
                if hour_24 < 24 {
                    return Ok(format!("0 {} * * {}", hour_24, day_num));
                }
            }
        }
        // 每周X X点（通用）
        let pattern = format!(r"每{}(\d{{1,2}})点?", day_name);
        if let Ok(re) = Regex::new(&pattern) {
            if let Some(caps) = re.captures(desc) {
                let hour: u32 = caps.get(1).unwrap().as_str().parse().map_err(|_| {
                    eyre!("无效的小时数")
                })?;
                if hour < 24 {
                    return Ok(format!("0 {} * * {}", hour, day_num));
                }
            }
        }
    }

    // 8. 每月 X 号
    if let Ok(re) = Regex::new(r"每月(\d{1,2})号?\s*(?:早上|上午|下午|晚上)?(\d{1,2})点?") {
        if let Some(caps) = re.captures(desc) {
            let day: u32 = caps.get(1).unwrap().as_str().parse().map_err(|_| {
                eyre!("无效的日期")
            })?;
            let hour = if let Some(h) = caps.get(2) {
                h.as_str().parse().map_err(|_| eyre!("无效的小时数"))?
            } else {
                0
            };
            if day <= 31 && hour < 24 {
                return Ok(format!("0 {} {} * *", hour, day));
            }
        }
    }

    // 无法解析
    Err(eyre!(
        "无法解析时间描述 '{}'。支持格式：\n\
         - 每5分钟 / 每30分钟\n\
         - 每天早上8点 / 每天下午3点 / 每天晚上8点\n\
         - 每小时 / 每2小时\n\
         - 每周一早上9点 / 每周五下午5点\n\
         - 每月15号上午10点",
        desc
    ))
}

// ─── 测试 ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::NoopMemory;
    use tempfile::tempdir;

    fn make_routine(name: &str, schedule: &str) -> Routine {
        Routine {
            name: name.to_string(),
            schedule: schedule.to_string(),
            message: format!("执行 {} 任务", name),
            channel: "cli".to_string(),
            enabled: true,
            source: RoutineSource::Dynamic,
        }
    }

    fn open_test_db(dir: &std::path::Path) -> Connection {
        let db_path = dir.join("test_routines.db");
        let conn = Connection::open(&db_path).unwrap();
        RoutineEngine::init_db(&conn).unwrap();
        conn
    }

    #[test]
    fn init_db_creates_tables() {
        let dir = tempdir().unwrap();
        let conn = open_test_db(dir.path());
        // 验证两个表存在
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('routines', 'routines_log')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn load_dynamic_routines_empty() {
        let dir = tempdir().unwrap();
        let conn = open_test_db(dir.path());
        let routines = RoutineEngine::load_dynamic_routines(&conn).unwrap();
        assert!(routines.is_empty());
    }

    #[test]
    fn routine_serialization() {
        let r = make_routine("test", "0 8 * * *");
        let json = serde_json::to_string(&r).unwrap();
        let r2: Routine = serde_json::from_str(&json).unwrap();
        assert_eq!(r.name, r2.name);
        assert_eq!(r.schedule, r2.schedule);
        assert_eq!(r.channel, r2.channel);
        assert!(r2.enabled);
    }

    #[test]
    fn routine_default_values() {
        let r: Routine = serde_json::from_str(
            r#"{"name":"x","schedule":"0 * * * *","message":"test"}"#
        )
        .unwrap();
        assert_eq!(r.channel, "cli");
        assert!(r.enabled);
    }

    #[test]
    fn cron_field_count_validation() {
        // 5 字段有效
        let valid = "0 8 * * *";
        assert_eq!(valid.split_whitespace().count(), 5);

        // 不足字段无效
        let invalid = "0 8 * *";
        assert_eq!(invalid.split_whitespace().count(), 4);

        // 多余字段无效
        let too_many = "0 8 * * * *";
        assert_eq!(too_many.split_whitespace().count(), 6);
    }

    #[test]
    fn noop_memory_trait_works() {
        // 同步测试 NoopMemory 的 trait 实现编译通过
        let _: &dyn crate::memory::Memory = &NoopMemory;
    }

    #[tokio::test]
    async fn noop_memory_store_returns_ok() {
        let mem = NoopMemory;
        let result: color_eyre::eyre::Result<()> = mem
            .store("key", "content", crate::memory::MemoryCategory::Core)
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn noop_memory_recall_returns_empty() {
        let mem = NoopMemory;
        let result: Vec<crate::memory::MemoryEntry> = mem.recall("query", 10).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn noop_memory_count_returns_zero() {
        let mem = NoopMemory;
        let count: usize = mem.count().await.unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn routine_source_default_is_config() {
        let source = RoutineSource::default();
        assert_eq!(source, RoutineSource::Config);
    }

    // ─── parse_schedule_to_cron 测试 ────────────────────────────────────

    #[test]
    fn parse_daily_morning() {
        let cron = parse_schedule_to_cron("每天早上8点").unwrap();
        assert_eq!(cron, "0 8 * * *");
    }

    #[test]
    fn parse_daily_afternoon() {
        let cron = parse_schedule_to_cron("每天下午3点").unwrap();
        assert_eq!(cron, "0 15 * * *");
    }

    #[test]
    fn parse_daily_evening() {
        let cron = parse_schedule_to_cron("每天晚上8点").unwrap();
        assert_eq!(cron, "0 20 * * *");
    }

    #[test]
    fn parse_hourly() {
        let cron = parse_schedule_to_cron("每小时").unwrap();
        assert_eq!(cron, "0 * * * *");
    }

    #[test]
    fn parse_every_2_hours() {
        let cron = parse_schedule_to_cron("每2小时").unwrap();
        assert_eq!(cron, "0 */2 * * *");
    }

    #[test]
    fn parse_every_5_minutes() {
        let cron = parse_schedule_to_cron("每5分钟").unwrap();
        assert_eq!(cron, "*/5 * * * *");
    }

    #[test]
    fn parse_weekly_monday_morning() {
        let cron = parse_schedule_to_cron("每周一早上9点").unwrap();
        assert_eq!(cron, "0 9 * * 1");
    }

    #[test]
    fn parse_weekly_friday_afternoon() {
        let cron = parse_schedule_to_cron("每周五下午5点").unwrap();
        assert_eq!(cron, "0 17 * * 5");
    }

    #[test]
    fn parse_monthly() {
        let cron = parse_schedule_to_cron("每月15号上午10点").unwrap();
        assert_eq!(cron, "0 10 15 * *");
    }

    #[test]
    fn parse_invalid_returns_error() {
        let result = parse_schedule_to_cron("随便输入");
        assert!(result.is_err());
    }

    // --- build_enhanced_message 测试 ---

    fn make_memory_entry(content: &str) -> crate::memory::MemoryEntry {
        crate::memory::MemoryEntry {
            key: "routine:test:approach".to_string(),
            content: content.to_string(),
            category: crate::memory::MemoryCategory::Custom("routine".to_string()),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            relevance_score: 1.0,
        }
    }

    #[test]
    fn enhanced_message_with_recalled_entry_injects_prefix() {
        let entry = make_memory_entry("GET https://api.example.com/price, headers: User-Agent: Mozilla");
        let recalled = vec![entry];
        let msg = build_enhanced_message(&recalled, "查询股价");
        assert!(msg.starts_with("[历史成功方法参考]"), "应以历史参考前缀开头");
        assert!(msg.contains("GET https://api.example.com/price"), "应包含历史方法内容");
        assert!(msg.contains("查询股价"), "应包含原始任务消息");
    }

    #[test]
    fn enhanced_message_without_recalled_returns_original() {
        let msg = build_enhanced_message(&[], "查询股价");
        assert_eq!(msg, "查询股价", "无召回时应原样返回任务消息");
    }

    #[test]
    fn enhanced_message_uses_only_first_recalled_entry() {
        let entry1 = make_memory_entry("方法一");
        let entry2 = make_memory_entry("方法二");
        let recalled = vec![entry1, entry2];
        let msg = build_enhanced_message(&recalled, "任务");
        assert!(msg.contains("方法一"), "应包含第一条记录");
        assert!(!msg.contains("方法二"), "不应包含第二条记录");
    }
}
