//! 集成测试公共辅助函数
//!
//! 供 scheduler_integration.rs 和后续的 e2e_agent.rs 共用。

use std::collections::HashMap;
use std::sync::Arc;

use rrclaw::config::{Config, DefaultConfig, ProviderConfig, ReliabilityConfig};
use rrclaw::memory::NoopMemory;
use rrclaw::routines::{Routine, RoutineEngine, RoutineSource};

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
