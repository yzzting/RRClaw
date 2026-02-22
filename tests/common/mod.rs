//! 集成测试公共辅助函数
//!
//! 供 scheduler_integration.rs 和 e2e_agent.rs 共用。

// 每个集成测试文件只使用 common 的一部分，未用到的辅助函数属于预期 dead_code
#![allow(dead_code)]

pub mod mock_provider;
pub use mock_provider::MockProvider;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use rrclaw::agent::Agent;
use rrclaw::config::{Config, DefaultConfig, ProviderConfig, ReliabilityConfig};
use rrclaw::memory::NoopMemory;
use rrclaw::routines::{Routine, RoutineEngine, RoutineSource};
use rrclaw::security::{AutonomyLevel, SecurityPolicy};

/// 构造一个用于集成测试的最小 Config
///
/// - provider base_url 指向 127.0.0.1:1（必定 connection refused，快速失败）
/// - max_retries = 1（不触发 5 分钟重试等待）
pub fn test_config() -> Arc<Config> {
    let mut providers = HashMap::new();
    providers.insert(
        "test".to_string(),
        ProviderConfig {
            base_url: "http://127.0.0.1:1".to_string(), // port 1: immediate connection refused
            api_key: "test-key".to_string(),
            model: "test-model".to_string(),
            auth_style: None,
        },
    );

    Arc::new(Config {
        default: DefaultConfig {
            provider: "test".to_string(),
            model: "test-model".to_string(),
            temperature: 0.0,
        },
        providers,
        reliability: ReliabilityConfig {
            max_retries: 1, // 只尝试一次，不重试（避免 5 分钟等待）
            initial_backoff_ms: 0,
            fallback_providers: vec![],
        },
        ..Config::default()
    })
}

/// 构造一个用于测试的 Routine
pub fn test_routine(name: &str, schedule: &str) -> Routine {
    Routine {
        name: name.to_string(),
        schedule: schedule.to_string(),
        message: "test message".to_string(),
        channel: "cli".to_string(),
        enabled: true,
        source: RoutineSource::Dynamic,
    }
}

/// 创建一个用于集成测试的 RoutineEngine
///
/// - 使用临时 SQLite 文件（测试结束后自动清理）
/// - 使用 NoopMemory（不依赖真实 Memory）
/// - 使用 test_config()（快速失败的 Provider）
pub async fn make_test_engine(routines: Vec<Routine>) -> (Arc<RoutineEngine>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("创建临时目录失败");
    let db_path = tmp.path().join("test_routines.db");

    let engine = RoutineEngine::new(
        routines,
        test_config(),
        Arc::new(NoopMemory),
        &db_path,
    )
    .await
    .expect("创建 RoutineEngine 失败");

    (Arc::new(engine), tmp)
}

// ─── E2E Agent 辅助函数 ───────────────────────────────────────────────────────

/// 创建一个用于 E2E 测试的 Agent
///
/// - 使用 MockProvider（预置响应队列，不打真实 HTTP）
/// - 仅含 ShellTool（够验证 tool call 链路）
/// - injection_check=false（降低测试噪音）
pub fn test_agent(mock: MockProvider, policy: SecurityPolicy) -> Agent {
    Agent::new(
        Box::new(mock),
        vec![Box::new(rrclaw::tools::shell::ShellTool)],
        Box::new(NoopMemory),
        policy,
        "mock".to_string(),
        "http://mock".to_string(),
        "mock-model".to_string(),
        0.0,
        vec![], // 不加载 skills（简化测试）
        None,   // 不加载 identity
    )
}

/// 构造 Full 自主策略，allowed_commands=["echo"]，workspace=tmp_path
pub fn full_policy(workspace: &Path) -> SecurityPolicy {
    SecurityPolicy {
        autonomy: AutonomyLevel::Full,
        allowed_commands: vec!["echo".to_string()],
        workspace_dir: workspace.to_path_buf(),
        blocked_paths: vec![],
        http_allowed_hosts: vec![],
        injection_check: false,
    }
}

/// 构造 ReadOnly 策略（禁止执行任何工具）
pub fn readonly_policy(workspace: &Path) -> SecurityPolicy {
    SecurityPolicy {
        autonomy: AutonomyLevel::ReadOnly,
        allowed_commands: vec![],
        workspace_dir: workspace.to_path_buf(),
        blocked_paths: vec![],
        http_allowed_hosts: vec![],
        injection_check: false,
    }
}
