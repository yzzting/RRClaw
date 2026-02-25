//! Scheduler 集成测试
//!
//! 验证 tokio-cron-scheduler 的真实行为：不 mock scheduler，
//! 验证调度器真实触发、生命周期管理（add/delete/enable/disable）。
//!
//! # 设计原则
//! - 使用真实 JobScheduler（不 mock）
//! - LLM 执行被 test_config() 中的 port-1 URL 快速短路（connection refused）
//! - config.reliability.max_retries = 1 → 执行失败时不触发 5 分钟重试
//! - cron 使用每秒格式（"* * * * * *"），sleep 3s 等待触发
//! - 验证用 trigger_count（AtomicUsize）而非检查 LLM 输出
//!
//! # 已知限制
//! - persist_delete_routine / persist_set_enabled 不会从 scheduler 注销 cron job
//!   （代码未跟踪 job UUID）。删除/禁用后 trigger_count 仍可能增加，
//!   但 execute_routine 会在 "routine 不存在" / "routine 已禁用" 处 early-return。
//!   测试只验证内存/DB 状态一致性，不验证 scheduler 是否停止 fire。

mod common;

use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::time::sleep;

/// 每秒触发一次的 6 字段 cron 表达式（秒 分 时 日 月 周）
const EVERY_SECOND: &str = "* * * * * *";

// ─── S1-1: scheduler 真实启动并触发 ─────────────────────────────────────────

#[tokio::test]
async fn s1_1_scheduler_triggers_after_start() {
    let (engine, _tmp) =
        common::make_test_engine(vec![common::test_routine("s1-1-job", EVERY_SECOND)]).await;

    engine.clone().start().await.expect("scheduler 启动失败");

    // 等 3 秒，期间 scheduler 应至少触发一次
    sleep(Duration::from_secs(3)).await;

    let count = engine.trigger_count.load(Ordering::Relaxed);
    assert!(
        count >= 1,
        "scheduler 应至少触发一次，实际 trigger_count = {}。\
         \n可能原因：scheduler.start() 未被调用，或 cron 格式错误。",
        count
    );
}

// ─── S1-2: persist_add_routine 后立即可触发 ──────────────────────────────────

#[tokio::test]
async fn s1_2_persist_add_routine_schedules_immediately() {
    let (engine, _tmp) = common::make_test_engine(vec![]).await;

    // 先启动空调度器
    engine.clone().start().await.expect("scheduler 启动失败");

    // 等 1 秒确认无触发（空调度器）
    sleep(Duration::from_secs(1)).await;
    assert_eq!(
        engine.trigger_count.load(Ordering::Relaxed),
        0,
        "空调度器不应触发"
    );

    // 动态添加 routine（6字段 cron 也合法，persist_add_routine 接受 5 或 6 字段）
    let routine = common::test_routine("s1-2-job", EVERY_SECOND);
    engine
        .clone()
        .persist_add_routine(&routine)
        .await
        .expect("persist_add_routine 失败");

    // 等 3 秒，动态添加的 routine 应触发
    sleep(Duration::from_secs(3)).await;

    let count = engine.trigger_count.load(Ordering::Relaxed);
    assert!(
        count >= 1,
        "persist_add_routine 后 scheduler 应触发，实际 trigger_count = {}",
        count
    );
}

// ─── S1-3: persist_delete_routine 后内存状态立即更新 ─────────────────────────
//
// 注意：删除 routine 后 cron job 仍在 scheduler 中（未跟踪 job UUID，无法注销）。
// 这意味着 trigger_count 仍可能增加，但 execute_routine 会因"routine 不存在"而 early-return。
// 本测试只验证：内存/DB 状态立即更新，且 execute_routine 返回 error。

#[tokio::test]
async fn s1_3_persist_delete_updates_state_immediately() {
    let (engine, _tmp) = common::make_test_engine(vec![
        common::test_routine("s1-3-job", "0 8 * * *"), // 每天早 8 点，不会在测试期间触发
    ])
    .await;

    // 确认 routine 存在
    assert_eq!(engine.list_routines().len(), 1);

    // 删除 routine
    engine
        .persist_delete_routine("s1-3-job")
        .await
        .expect("persist_delete_routine 失败");

    // 内存列表应立即为空
    assert!(
        engine.list_routines().is_empty(),
        "删除后 list_routines 应为空（内存双写规范）"
    );

    // 手动执行被删除的 routine 应返回 error（"routine 不存在"）
    let result = engine.execute_routine("s1-3-job").await;
    assert!(result.is_err(), "执行已删除的 routine 应返回 error");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("不存在"),
        "错误信息应包含'不存在'，实际: {}",
        err
    );
}

// ─── S1-4: disable/enable 内存状态立即更新 ───────────────────────────────────

#[tokio::test]
async fn s1_4_disable_enable_updates_state_immediately() {
    let (engine, _tmp) =
        common::make_test_engine(vec![common::test_routine("s1-4-job", "0 8 * * *")]).await;

    // 禁用 routine
    engine
        .persist_set_enabled("s1-4-job", false)
        .await
        .expect("persist_set_enabled 失败");

    // 内存状态立即更新
    let routines = engine.list_routines();
    let job = routines
        .iter()
        .find(|r| r.name == "s1-4-job")
        .expect("找不到 routine");
    assert!(!job.enabled, "禁用后 list_routines 应返回 enabled=false");

    // 手动执行已禁用的 routine 应跳过执行（返回 Ok，不是 Err）
    let result = engine.execute_routine("s1-4-job").await;
    assert!(result.is_ok(), "禁用的 routine 应 skip 而不是 error");
    let msg = result.unwrap();
    assert!(
        msg.contains("已禁用"),
        "跳过消息应包含'已禁用'，实际: {}",
        msg
    );

    // 重新启用
    engine
        .persist_set_enabled("s1-4-job", true)
        .await
        .expect("persist_set_enabled 失败");
    let routines = engine.list_routines();
    let job = routines.iter().find(|r| r.name == "s1-4-job").unwrap();
    assert!(job.enabled, "重新启用后 list_routines 应返回 enabled=true");
}

// ─── S1-5: 5字段 cron 被自动转换为 6 字段 ────────────────────────────────────

#[tokio::test]
async fn s1_5_five_field_cron_auto_converted() {
    // tokio-cron-scheduler 要求 6 字段 cron（秒 分 时 日 月 周）
    // 代码应自动在前面加 "0 " 转换为 6 字段，两种格式都能成功注册

    let routine_6field = common::test_routine("s1-5-6field", EVERY_SECOND);
    let routine_5field = common::test_routine("s1-5-5field", "* * * * *"); // 5字段

    let (engine, _tmp) = common::make_test_engine(vec![routine_6field, routine_5field]).await;

    // start() 不应 panic 或返回 error（两种格式都可以成功注册）
    engine
        .clone()
        .start()
        .await
        .expect("5字段 cron 应被自动转换为6字段，start() 不应失败");

    // 等 3 秒，6字段的每秒 routine 应触发
    sleep(Duration::from_secs(3)).await;
    let count = engine.trigger_count.load(Ordering::Relaxed);
    assert!(
        count >= 1,
        "6字段 cron routine 应触发，trigger_count = {}",
        count
    );
}

// ─── S1-6: list_routines 与 DB 状态一致（不依赖 scheduler） ─────────────────

#[tokio::test]
async fn s1_6_list_routines_reflects_persist_changes() {
    let (engine, _tmp) = common::make_test_engine(vec![]).await;

    // 初始为空
    assert!(engine.list_routines().is_empty(), "初始应无 routine");

    // persist_add 后立即可 list
    let r = common::test_routine("s1-6-job", "0 8 * * *");
    engine
        .clone()
        .persist_add_routine(&r)
        .await
        .expect("persist_add_routine 失败");

    let routines = engine.list_routines();
    assert_eq!(routines.len(), 1, "添加后应有 1 个 routine");
    assert_eq!(routines[0].name, "s1-6-job");
    assert!(routines[0].enabled);

    // persist_set_enabled(false) 后立即可见
    engine
        .persist_set_enabled("s1-6-job", false)
        .await
        .expect("persist_set_enabled 失败");
    let r = engine.list_routines();
    assert!(!r[0].enabled, "禁用后应立即可见");

    // persist_delete 后立即从 list 消失
    engine
        .persist_delete_routine("s1-6-job")
        .await
        .expect("persist_delete_routine 失败");
    assert!(engine.list_routines().is_empty(), "删除后列表应为空");
}

// ─── S1-7: 执行触发后 DB 日志有记录 ─────────────────────────────────────────
//
// 直接调用 execute_routine（不通过 scheduler）验证 DB 日志写入。
// 使用本地 mock HTTP server（立即返回错误），确保 execute_routine 快速完成。

#[tokio::test]
async fn s1_7_execution_logs_written_to_db() {
    use tokio::io::AsyncWriteExt;

    // 启动一个本地 TCP server：立即返回 HTTP 503，让 reqwest 快速失败
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑定本地端口失败");
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let _ = stream
                .write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n")
                .await;
        }
    });

    // 创建指向本地 mock server 的 engine
    let mut config = (*common::test_config()).clone();
    let provider = config.providers.get_mut("test").unwrap();
    provider.base_url = format!("http://127.0.0.1:{}", port);
    let config = std::sync::Arc::new(config);

    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("s1-7.db");
    let engine = rrclaw::routines::RoutineEngine::new(
        vec![common::test_routine("s1-7-job", "0 8 * * *")],
        config,
        std::sync::Arc::new(rrclaw::memory::NoopMemory),
        &db_path,
    )
    .await
    .expect("创建 engine 失败");
    let engine = std::sync::Arc::new(engine);

    // 直接调用 execute_routine（即使失败也应写 log）
    let _ = engine.execute_routine("s1-7-job").await;

    let logs = engine.get_recent_logs(10).await;
    assert!(
        !logs.is_empty(),
        "execute_routine 应写入执行日志，即使 LLM 调用失败"
    );

    let log = &logs[0];
    assert_eq!(log.routine_name, "s1-7-job");
    // LLM 返回 503，执行必然失败
    assert!(!log.success, "LLM 返回 503 时执行应标记为失败");
    assert!(log.error.is_some(), "失败时 error 字段应有值");
}
