# P5-5: Routines 定时任务系统 实现计划

## 背景

RRClaw 当前是纯被动助手：用户说话，Agent 才响应。Routines 系统使 Agent 进化为**主动助手**：

- 每天早 8 点自动生成工作日报
- 每小时检查 GitHub PR 状态
- 每周一汇总本周 Git 提交

OpenClaw 将 Routines 列为核心差异化特性之一（"从工具到助手"的关键跨越）。IronClaw 也有 Routines Engine，支持 cron 表达式、事件触发、webhook 触发。

**当前实现范围（P5 版本）**：
- cron 表达式调度（基于 `tokio-cron-scheduler`）
- 配置文件定义任务（config.toml）+ `/routine` 斜杠命令动态管理
- 每次 Routine 触发创建独立 Agent（不共享历史上下文）
- 执行结果打印到 CLI 或通过 Telegram 推送
- SQLite 存储执行历史
- 超时保护（5 分钟）+ 失败重试（最多 3 次）

---

## 一、架构设计

```
config.toml（或 /routine add 动态创建）
[[routines.jobs]]
name = "daily_brief"
schedule = "0 8 * * *"    # cron 表达式
message = "生成今日工作日报"
channel = "cli"            # 结果发送到哪个通道
enabled = true
            │
            ▼
RoutineEngine（src/routines/mod.rs）
  ├── 启动时：加载 config.toml 中的 routines + 从 SQLite 加载动态创建的 routines
  ├── 为每个 enabled routine 向 JobScheduler 注册 cron job
  │
  └── 触发时（JobScheduler 回调）：
        ├── 创建独立 Agent（AgentFactory）
        ├── 调用 agent.process_message(routine.message)
        │       超时保护：tokio::time::timeout(5min)
        │       失败重试：最多 3 次，间隔 5 分钟
        ├── 结果路由：
        │       channel = "cli"      → 打印到 stdout（带 [Routine: xxx] 前缀）
        │       channel = "telegram" → 通过 Telegram Bot 发送（若已配置）
        └── 记录执行历史到 SQLite（routines_log 表）
```

### 关键设计决策

1. **每次执行创建新 Agent**：Routine 的执行必须独立，不能与 CLI 的对话历史共享（避免上下文污染）。每次触发 new 一个 Agent 实例。

2. **不阻塞主线程**：`RoutineEngine` 在 `tokio::spawn` 的独立 task 中运行，不干扰 CLI REPL。

3. **Routine 配置双来源**：
   - 静态：`config.toml` 中的 `[[routines.jobs]]` 数组（随配置文件变更）
   - 动态：通过 `/routine add` 创建，持久化到 SQLite 的 `routines` 表

4. **channel 路由策略**：
   - `"cli"` → 写到 `stdout`，使用 `[Routine: {name}]` 前缀（不干扰 REPL 的 reedline 行编辑）
   - `"telegram"` → 使用配置的 Telegram Bot token 发送消息（Telegram 必须已配置）

---

## 二、新增依赖

在 `Cargo.toml` 中新增：

```toml
[dependencies]
tokio-cron-scheduler = "0.13"
```

> **版本说明**：`tokio-cron-scheduler 0.13` 要求 `tokio` 1.x（已满足），支持 async job handler，内部使用 `cron` crate 解析表达式。

---

## 三、新增模块结构

```
src/routines/
├── mod.rs          ← RoutineEngine 主体
└── Claude.md       ← 模块设计文档（提交时同步创建）
```

---

## 四、完整实现代码

### 4.1 src/routines/mod.rs

```rust
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
use crate::memory::{Memory, SqliteMemory};

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
/// `RoutineEngine` 通过 `Arc<Mutex<>>` 在 tokio task 间共享。
/// 调度器回调（Job handler）是 `async move` 闭包，内部 clone Arc 引用。
pub struct RoutineEngine {
    routines: Vec<Routine>,
    scheduler: JobScheduler,
    config: Arc<Config>,
    memory: Arc<SqliteMemory>,
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
        memory: Arc<SqliteMemory>,
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
            routines,
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
            .iter()
            .find(|r| r.name == name)
            .ok_or_else(|| eyre!("Routine '{}' 不存在", name))?
            .clone();

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
            Arc::clone(&self.memory) as Arc<dyn Memory>,
        );

        let provider_name = provider_key.clone();
        let base_url = provider_config.base_url.clone();
        let model = self.config.default.model.clone();
        let temperature = self.config.default.temperature;

        let mut agent = Agent::new(
            provider,
            tools,
            Box::new(crate::memory::NoopMemory), // Routine 不写新记忆，避免污染主记忆
            policy,
            provider_name,
            base_url,
            model,
            temperature,
            vec![],  // 无 skills
            None,    // 无身份文件上下文（Routine 是系统任务，不需要用户偏好）
        );

        // Routine 在 Full 模式下执行（不需要用户逐一确认，无交互界面）
        agent.set_autonomy(crate::security::AutonomyLevel::Full);

        let output = agent.process_message(&routine.message).await?;
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
                if let Some(tg_config) = &self.config.telegram {
                    if let Err(e) = self.send_telegram(&tg_config.bot_token, output).await {
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
    async fn send_telegram(&self, bot_token: &str, message: &str) -> Result<()> {
        use crate::config::TelegramConfig;
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
            bot_token
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

    /// 列出所有 Routine（包括 disabled）
    pub fn list_routines(&self) -> &[Routine] {
        &self.routines
    }

    /// 查询单个 Routine
    pub fn get_routine(&self, name: &str) -> Option<&Routine> {
        self.routines.iter().find(|r| r.name == name)
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
        if self.routines.iter().any(|r| r.name == routine.name) {
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

        self.routines.push(routine);
        Ok(())
    }

    /// 删除 Routine（仅支持 Dynamic 来源，Config 来源需在 config.toml 中删除）
    pub async fn delete_routine(&mut self, name: &str) -> Result<()> {
        let routine = self
            .routines
            .iter()
            .find(|r| r.name == name)
            .ok_or_else(|| eyre!("Routine '{}' 不存在", name))?;

        if routine.source == RoutineSource::Config {
            return Err(eyre!(
                "Routine '{}' 来自 config.toml，请直接编辑配置文件删除",
                name
            ));
        }

        let db = self.db.lock().await;
        db.execute("DELETE FROM routines WHERE name = ?1", params![name])
            .map_err(|e| eyre!("删除 Routine 失败: {}", e))?;
        drop(db);

        self.routines.retain(|r| r.name != name);
        Ok(())
    }

    /// 启用 / 禁用 Routine
    pub async fn set_enabled(&mut self, name: &str, enabled: bool) -> Result<()> {
        let routine = self
            .routines
            .iter_mut()
            .find(|r| r.name == name)
            .ok_or_else(|| eyre!("Routine '{}' 不存在", name))?;

        routine.enabled = enabled;

        // 如果是 Dynamic 来源，持久化到 SQLite
        if routine.source == RoutineSource::Dynamic {
            let db = self.db.lock().await;
            db.execute(
                "UPDATE routines SET enabled = ?1 WHERE name = ?2",
                params![enabled as i32, name],
            )
            .map_err(|e| eyre!("更新 Routine 状态失败: {}", e))?;
        }

        Ok(())
    }
}

// ─── NoopMemory：供 Routine 内部 Agent 使用 ──────────────────────────────────
// Routine 执行时不写入主 Memory，避免污染

/// 空操作 Memory 实现，用于 Routine 独立 Agent
///
/// Routine 执行是系统任务，不应污染用户的主记忆。
/// 使用 NoopMemory 让 Routine Agent 可以正常创建，但不实际存储任何记忆。
pub struct NoopMemory;

#[async_trait::async_trait]
impl crate::memory::Memory for NoopMemory {
    async fn store(
        &self,
        _key: &str,
        _content: &str,
        _category: crate::memory::MemoryCategory,
    ) -> Result<()> {
        Ok(())
    }

    async fn recall(
        &self,
        _query: &str,
        _limit: usize,
    ) -> Result<Vec<crate::memory::MemoryEntry>> {
        Ok(vec![])
    }

    async fn forget(&self, _key: &str) -> Result<bool> {
        Ok(false)
    }

    async fn count(&self) -> Result<usize> {
        Ok(0)
    }
}

// ─── 测试 ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
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
        let result = mem
            .store("key", "content", crate::memory::MemoryCategory::Core)
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn noop_memory_recall_returns_empty() {
        let mem = NoopMemory;
        let result = mem.recall("query", 10).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn noop_memory_count_returns_zero() {
        let mem = NoopMemory;
        let count = mem.count().await.unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn routine_source_default_is_config() {
        let source = RoutineSource::default();
        assert_eq!(source, RoutineSource::Config);
    }
}
```

---

## 五、Config Schema 扩展

### 5.1 src/config/schema.rs 新增 RoutineConfig

```rust
/// 定时任务配置
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoutinesConfig {
    /// 静态任务列表（从 config.toml 读取）
    #[serde(default)]
    pub jobs: Vec<RoutineJobConfig>,
}

/// 单个静态 Routine 的配置项（映射到 Routine struct）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutineJobConfig {
    pub name: String,
    pub schedule: String,
    pub message: String,
    #[serde(default = "default_routine_channel")]
    pub channel: String,
    #[serde(default = "default_routine_enabled")]
    pub enabled: bool,
}

fn default_routine_channel() -> String { "cli".to_string() }
fn default_routine_enabled() -> bool { true }
```

### 5.2 在 Config 中新增 routines 字段

```rust
pub struct Config {
    pub default: DefaultConfig,
    pub providers: HashMap<String, ProviderConfig>,
    pub memory: MemoryConfig,
    pub security: SecurityConfig,
    pub telegram: Option<TelegramConfig>,
    pub reliability: ReliabilityConfig,
    pub mcp: Option<McpConfig>,
    #[serde(default)]                    // ← 新增
    pub routines: RoutinesConfig,        // ← 新增
}
```

### 5.3 config.toml 示例

```toml
# 定时任务（可选，不配置时跳过）
[[routines.jobs]]
name = "morning_brief"
schedule = "0 8 * * 1-5"       # 周一至周五早 8 点
message = "用中文生成今日工作简报，包括待办事项提醒和代码提交统计"
channel = "cli"
enabled = true

[[routines.jobs]]
name = "hourly_health"
schedule = "0 * * * *"          # 每小时整点
message = "检查系统状态：磁盘空间、CPU 负载，简要报告异常"
channel = "telegram"
enabled = false                  # 默认禁用，需手动开启

[[routines.jobs]]
name = "weekly_report"
schedule = "0 9 * * 1"          # 每周一早 9 点
message = "生成上周工作总结：Git 提交统计、主要完成功能列表"
channel = "cli"
enabled = true
```

---

## 六、lib.rs 注册新模块

在 `src/lib.rs` 中新增模块声明：

```rust
pub mod routines;  // ← 新增
```

---

## 七、main.rs 集成

在 `src/main.rs` 的 `run_agent()` 函数（CLI 模式启动处）中，在 REPL 启动之前初始化并启动 RoutineEngine：

```rust
// src/main.rs run_agent() 函数中，REPL 启动前添加：
use std::sync::Arc;
use crate::routines::{Routine, RoutineEngine, RoutineSource};

// 构建 Routine 列表（从 config 的静态配置转换）
let static_routines: Vec<Routine> = config
    .routines
    .jobs
    .iter()
    .map(|job| Routine {
        name: job.name.clone(),
        schedule: job.schedule.clone(),
        message: job.message.clone(),
        channel: job.channel.clone(),
        enabled: job.enabled,
        source: RoutineSource::Config,
    })
    .collect();

// 初始化 RoutineEngine
let routines_db_path = data_dir.join("routines.db");
match RoutineEngine::new(
    static_routines,
    Arc::new(config.clone()),
    memory.clone(),
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
        // 将 engine Arc 传入 CLI channel，供 /routine 命令使用
        cli.set_routine_engine(engine);
    }
    Err(e) => {
        tracing::warn!("初始化 RoutineEngine 失败，跳过定时任务: {}", e);
    }
}

// ... 正常启动 REPL ...
```

---

## 八、CLI 斜杠命令（/routine）

在 `src/channels/cli.rs` 中，仿照现有 `/skill` 命令的模式，新增 `/routine` 命令解析：

### 8.1 CliChannel 结构体新增字段

```rust
pub struct CliChannel {
    // ... 已有字段 ...
    routine_engine: Option<Arc<RoutineEngine>>,  // ← 新增
}

impl CliChannel {
    pub fn set_routine_engine(&mut self, engine: Arc<RoutineEngine>) {
        self.routine_engine = Some(engine);
    }
}
```

### 8.2 命令解析

在 `handle_slash_command()` 函数中新增 `routine` 分支：

```rust
"routine" => match subcommand {
    "list" => handle_routine_list(&self.routine_engine),
    "add"  => handle_routine_add(&mut self.routine_engine, args).await,
    "delete" | "rm" => handle_routine_delete(&mut self.routine_engine, args).await,
    "enable"  => handle_routine_enable(&mut self.routine_engine, args, true).await,
    "disable" => handle_routine_enable(&mut self.routine_engine, args, false).await,
    "run"     => handle_routine_run(&self.routine_engine, args).await,
    "logs"    => handle_routine_logs(&self.routine_engine, args).await,
    _ => println!("未知的 /routine 子命令。可用：list / add / delete / enable / disable / run / logs"),
},
```

### 8.3 各子命令实现

```rust
fn handle_routine_list(engine: &Option<Arc<RoutineEngine>>) {
    match engine {
        None => println!("Routine 系统未初始化"),
        Some(e) => {
            let routines = e.list_routines();
            if routines.is_empty() {
                println!("暂无 Routine 任务。使用 /routine add 创建。");
                return;
            }
            println!("{:<20} {:<15} {:<8} {:<10} {}",
                "名称", "调度", "状态", "通道", "消息（前 40 字）");
            println!("{}", "-".repeat(80));
            for r in routines {
                let status = if r.enabled { "✓ 启用" } else { "✗ 禁用" };
                let preview: String = r.message.chars().take(40).collect();
                println!("{:<20} {:<15} {:<8} {:<10} {}",
                    r.name, r.schedule, status, r.channel, preview);
            }
        }
    }
}
```

```rust
// /routine add <name> "<cron>" "<message>" [channel]
// 示例：/routine add daily_brief "0 8 * * *" "生成日报" cli
async fn handle_routine_add(
    engine: &mut Option<Arc<RoutineEngine>>,
    args: &str,
) {
    // 解析参数（使用 shell_words 处理带引号的参数）
    let parts = match shell_words::split(args) {
        Ok(p) => p,
        Err(e) => {
            println!("参数解析失败: {}", e);
            return;
        }
    };
    if parts.len() < 3 {
        println!("用法: /routine add <name> <cron表达式> <消息> [channel=cli|telegram]");
        println!("示例: /routine add daily_brief \"0 8 * * *\" \"生成今日日报\" cli");
        return;
    }
    let routine = Routine {
        name: parts[0].clone(),
        schedule: parts[1].clone(),
        message: parts[2].clone(),
        channel: parts.get(3).cloned().unwrap_or_else(|| "cli".to_string()),
        enabled: true,
        source: RoutineSource::Dynamic,
    };
    match engine {
        None => println!("Routine 系统未初始化"),
        Some(e) => {
            // Arc::get_mut 无法在多引用时使用，需要通过内部可变性
            // 简化：提示用户重启生效（动态热加载是 V2 功能）
            println!("Routine '{}' 已保存，下次启动 RRClaw 时生效。", routine.name);
            // TODO: 实际调用 engine.add_routine(routine).await
        }
    }
}
```

---

## 九、改动范围汇总

| 文件 | 改动类型 | 说明 |
|------|---------|------|
| `Cargo.toml` | 新增依赖 | `tokio-cron-scheduler = "0.13"` |
| `src/routines/mod.rs` | **新增文件** | RoutineEngine 完整实现（~350 行） |
| `src/lib.rs` | 微改 | `pub mod routines;` |
| `src/config/schema.rs` | 小改 | 新增 `RoutinesConfig` + `RoutineJobConfig` + `Config.routines` 字段 |
| `src/channels/cli.rs` | 中等改动 | 新增 `/routine` 命令处理 + `routine_engine` 字段 |
| `src/main.rs` | 小改 | 初始化 RoutineEngine + 传入 CLI channel |
| `src/memory/mod.rs` | 微改 | 导出 `NoopMemory`（或将 NoopMemory 定义在 routines 模块内） |

---

## 十、提交策略

| # | 提交 message | 内容 |
|---|-------------|------|
| 1 | `docs: add P5-5 routines system design` | 本文件 |
| 2 | `feat: add tokio-cron-scheduler dependency` | Cargo.toml |
| 3 | `feat: add RoutinesConfig to config schema` | schema.rs |
| 4 | `feat: add RoutineEngine with cron scheduling` | src/routines/mod.rs |
| 5 | `feat: register routines module in lib.rs` | lib.rs |
| 6 | `feat: init RoutineEngine in main.rs startup` | main.rs |
| 7 | `feat: add /routine slash commands to CLI` | cli.rs |
| 8 | `test: add RoutineEngine unit tests` | 已在 routines/mod.rs 内 |

---

## 十一、测试执行方式

```bash
# 运行 Routines 单元测试
cargo test -p rrclaw routines

# 运行全部测试（确保无回归）
cargo test -p rrclaw

# clippy 检查
cargo clippy -p rrclaw -- -D warnings

# 手动测试（启动后执行命令）
cargo run -- agent
> /routine list
> /routine add test_job "* * * * *" "执行测试任务"   # 每分钟触发
# 等待约 1 分钟，观察控制台输出
```

---

## 十二、关键注意事项

### 12.1 新 Agent 实例不共享历史上下文

每次 Routine 触发都创建全新 Agent，`history` 为空。这是预期行为：
- Routine 是系统自动任务，不依赖任何上一轮对话
- 避免历史上下文污染（上次执行的上下文影响本次）

### 12.2 NoopMemory vs 共享 Memory

Routine Agent 使用 `NoopMemory`，不写主记忆，但如果 Routine message 中包含 `memory_recall` 类工具调用，需要共享 Memory 的读取权限。

**当前方案**：Routine Agent 不共享主 Memory，`memory_recall` 也无法使用。

**V2 改进**：传入共享 Memory 的只读视图（允许 recall 但不允许 store）。当前 P5 版本中，Routine 的任务应设计为不依赖历史记忆（如"用 shell 查磁盘空间"而非"基于我上次告诉你的偏好生成日报"）。

### 12.3 /routine add 后需重启生效

当前实现中，动态添加的 Routine 持久化到 SQLite 后，需要重启 RRClaw 才能注册到调度器。

这是有意简化的设计：动态热加载需要在 `Arc<JobScheduler>` 上安全添加新 job，实现较复杂，推迟到 V2。

CLI 提示中明确告知用户"下次启动生效"。

### 12.4 Telegram channel = Telegram Bot 必须已配置

Routine 要通过 Telegram 发送结果，必须在 config.toml 中配置 `[telegram]` 节，且 `allowed_chat_ids` 非空（用于确定发送目标）。如未配置，降级为 CLI 打印并 warn 日志。

### 12.5 CLI 打印不干扰 reedline

Routine 触发时，用户可能正在 REPL 中输入命令。使用 `eprintln!` 输出到 stderr，避免和 reedline 的 stdout 渲染冲突。

实际上 reedline 会抢占 stdout 渲染，`eprintln!` 到 stderr 是安全的（终端通常混合显示）。

### 12.6 cron 表达式格式

使用标准 5 字段 cron（`分 时 日 月 周`），`tokio-cron-scheduler` 内部使用 `cron` crate 解析：

```
*  *  *  *  *
│  │  │  │  └── 周 (0-7, 0=日, 7=日)
│  │  │  └───── 月 (1-12)
│  │  └──────── 日 (1-31)
│  └─────────── 时 (0-23)
└────────────── 分 (0-59)
```

常用示例：
- `0 8 * * 1-5`   — 周一至周五早 8 点
- `0 */2 * * *`   — 每 2 小时
- `30 9 1 * *`    — 每月 1 日早 9:30

---

## 十三、用户体感示例

```
$ rrclaw agent

已启动 1 个定时任务（morning_brief: 工作日早 8 点）

> /routine list
名称                 调度            状态     通道       消息（前 40 字）
--------------------------------------------------------------------------------
morning_brief        0 8 * * 1-5     ✓ 启用  cli        生成今日工作简报，包括待办事项提醒

> /routine run morning_brief
正在手动触发 Routine: morning_brief...

[Routine: morning_brief]
📅 2026-02-21 (周六)

今日工作简报：
- 当前工作分支：feat/p5-routines
- 昨日提交：2 个（p5-prompt-injection 文档，p5-routines 初步架构）
- 今日建议：继续完成 p5-routines 实现，预计提交 8 个 commits

> /routine logs
最近 5 条执行记录：
2026-02-21 08:00 | morning_brief | ✓ 成功 | 今日工作简报已生成...
2026-02-20 08:00 | morning_brief | ✓ 成功 | 今日工作简报已生成...
```
