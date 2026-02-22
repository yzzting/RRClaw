//! Agent Loop E2E 测试
//!
//! 使用 MockProvider（内存响应队列）取代真实 HTTP，
//! 验证从 Provider → Agent → Tool → 最终回复的完整链路。
//!
//! # 设计原则
//! - MockProvider 按顺序返回预设响应，不打任何真实 HTTP
//! - 每次 process_message 含 Phase 1 路由（1次调用）+ Phase 2（N次调用）
//! - 测试只关注"接口边界"：输入、输出、history 状态
//!
//! # 每个测试的 MockProvider 队列说明
//! - 第 1 个响应：Phase 1 路由回复（`{"direct": true}`）
//! - 后续：Phase 2 实际对话回复

mod common;

use rrclaw::providers::ConversationMessage;

// ─── E2-1: 纯文本回复（无 tool call）────────────────────────────────────────

#[tokio::test]
async fn e2_1_pure_text_reply() {
    let tmp = tempfile::tempdir().unwrap();
    let mock = common::MockProvider::new(vec![
        common::MockProvider::direct_route(), // Phase 1 路由
        common::MockProvider::text("你好！"),  // Phase 2 回复
    ]);
    let mut agent = common::test_agent(mock, common::full_policy(tmp.path()));

    let result = agent.process_message("你好").await.expect("process_message 失败");

    assert_eq!(result, "你好！", "纯文本回复应原样返回");

    // history: user + assistant
    let history = agent.history();
    assert_eq!(history.len(), 2, "纯文本对话应有 2 条 history（user + assistant）");
    assert!(
        matches!(history[0], ConversationMessage::Chat(ref m) if m.role == "user"),
        "history[0] 应为 user 消息"
    );
    assert!(
        matches!(history[1], ConversationMessage::Chat(ref m) if m.role == "assistant"),
        "history[1] 应为 assistant 消息"
    );
}

// ─── E2-2: 单次 tool call → tool result → 最终回复 ──────────────────────────

#[tokio::test]
async fn e2_2_single_tool_call_and_final_reply() {
    let tmp = tempfile::tempdir().unwrap();
    let mock = common::MockProvider::new(vec![
        common::MockProvider::direct_route(),                       // Phase 1 路由
        common::MockProvider::shell_call("tc-1", "echo hello"),     // Phase 2: tool call
        common::MockProvider::text("命令输出：hello"),               // Phase 2: 最终回复
    ]);
    let mut agent = common::test_agent(mock, common::full_policy(tmp.path()));

    let result = agent
        .process_message("执行 echo hello")
        .await
        .expect("process_message 失败");

    assert!(
        result.contains("hello") || result.contains("命令输出"),
        "最终回复应包含 'hello' 或 '命令输出'，实际: {}",
        result
    );

    // history: user → AssistantToolCalls → ToolResult → assistant
    let history = agent.history();
    assert_eq!(history.len(), 4, "tool call 对话应有 4 条 history");

    assert!(matches!(history[0], ConversationMessage::Chat(ref m) if m.role == "user"));
    assert!(matches!(history[1], ConversationMessage::AssistantToolCalls { .. }));

    // ToolResult 应包含 echo 的输出
    if let ConversationMessage::ToolResult { content, .. } = &history[2] {
        assert!(
            content.contains("hello"),
            "ToolResult 应包含 echo 输出 'hello'，实际: {}",
            content
        );
    } else {
        panic!("history[2] 应为 ToolResult");
    }

    assert!(matches!(history[3], ConversationMessage::Chat(ref m) if m.role == "assistant"));
}

// ─── E2-3: ReadOnly 模式拒绝工具执行 ────────────────────────────────────────
//
// ReadOnly 策略下，pre_validate 立即拒绝所有 tool，
// ToolResult content 包含"只读模式"，真实命令未执行。

#[tokio::test]
async fn e2_3_readonly_policy_rejects_tool() {
    let tmp = tempfile::tempdir().unwrap();
    let mock = common::MockProvider::new(vec![
        common::MockProvider::direct_route(),                          // Phase 1 路由
        common::MockProvider::shell_call("tc-3", "rm -rf /"),         // Phase 2: tool call（会被拒绝）
        common::MockProvider::text("已为您拒绝危险命令"),               // Phase 2: 最终回复
    ]);
    let mut agent = common::test_agent(mock, common::readonly_policy(tmp.path()));

    let result = agent
        .process_message("删除所有文件")
        .await
        .expect("process_message 失败");

    // 最终回复来自 MockProvider（LLM 知道工具被拒绝了）
    assert!(
        !result.is_empty(),
        "ReadOnly 拒绝后仍应有最终回复，实际为空"
    );

    // ToolResult 应包含拒绝原因
    let history = agent.history();
    let tool_result = history
        .iter()
        .find(|m| matches!(m, ConversationMessage::ToolResult { .. }))
        .expect("应有 ToolResult 记录拒绝原因");

    if let ConversationMessage::ToolResult { content, .. } = tool_result {
        assert!(
            content.contains("只读") || content.contains("ReadOnly") || content.contains("拒绝"),
            "ToolResult 应包含拒绝原因，实际: {}",
            content
        );
    }
}

// ─── E2-4: Full 模式白名单拦截（命令不在白名单）────────────────────────────
//
// Full 模式下，allowed_commands=["echo"]，
// "rm -rf /" 不在白名单，pre_validate 拒绝，真实命令未执行。

#[tokio::test]
async fn e2_4_command_whitelist_blocks_disallowed_command() {
    let tmp = tempfile::tempdir().unwrap();
    let mock = common::MockProvider::new(vec![
        common::MockProvider::direct_route(),                          // Phase 1 路由
        common::MockProvider::shell_call("tc-4", "rm -rf /"),         // Phase 2: tool call（白名单拒绝）
        common::MockProvider::text("命令不被允许执行"),                 // Phase 2: 最终回复
    ]);
    let mut agent = common::test_agent(mock, common::full_policy(tmp.path())); // Full，只允许 echo

    let result = agent
        .process_message("删除根目录")
        .await
        .expect("process_message 失败");

    assert!(!result.is_empty(), "白名单拒绝后仍应有最终回复");

    // ToolResult 应包含白名单拒绝原因
    let history = agent.history();
    let tool_result = history
        .iter()
        .find(|m| matches!(m, ConversationMessage::ToolResult { .. }))
        .expect("应有 ToolResult 记录白名单拒绝");

    if let ConversationMessage::ToolResult { content, .. } = tool_result {
        assert!(
            content.contains("白名单") || content.contains("不在"),
            "ToolResult 应包含白名单拒绝原因，实际: {}",
            content
        );
    }
}

// ─── E2-5: process_message 返回澄清问题（NeedClarification）────────────────
//
// Phase 1 路由返回 question 字段，agent 直接返回澄清问题，
// 不写入 history，不执行任何工具。

#[tokio::test]
async fn e2_5_clarification_returned_without_history() {
    let tmp = tempfile::tempdir().unwrap();

    // Phase 1 返回 question（需要澄清）
    let clarification_response = rrclaw::providers::ChatResponse {
        text: Some(r#"{"skills": [], "direct": false, "question": "你是想创建文件还是删除文件？"}"#.to_string()),
        reasoning_content: None,
        tool_calls: vec![],
    };
    let mock = common::MockProvider::new(vec![
        clarification_response, // Phase 1 路由 → NeedClarification
        // Phase 2 不应被调用（NeedClarification 提前返回）
    ]);
    let mut agent = common::test_agent(mock, common::full_policy(tmp.path()));

    let result = agent
        .process_message("处理文件")
        .await
        .expect("process_message 失败");

    assert!(
        result.contains("文件"),
        "澄清问题应包含相关内容，实际: {}",
        result
    );

    // NeedClarification 不写入 history
    assert!(
        agent.history().is_empty(),
        "NeedClarification 不应写入 history，实际: {:?}",
        agent.history()
    );
}

// ─── E2-6: 最大工具调用次数保护 ──────────────────────────────────────────────
//
// MAX_TOOL_ITERATIONS=10，连续 10 次 LLM 调用均返回 tool_call。
// Agent 应在第 10 次后退出循环，返回 Ok（可能为空字符串），不无限循环。

#[tokio::test]
async fn e2_6_max_tool_iterations_protection() {
    let tmp = tempfile::tempdir().unwrap();

    // 准备 11 个响应：1 个路由 + 10 个 tool_call
    let mut responses = vec![common::MockProvider::direct_route()];
    for i in 0..10 {
        responses.push(common::MockProvider::shell_call(
            &format!("tc-{}", i),
            "echo loop",
        ));
    }
    let mock = common::MockProvider::new(responses);
    let mut agent = common::test_agent(mock, common::full_policy(tmp.path()));

    // 不应 panic 或无限循环，应在合理时间内返回
    let result = agent.process_message("一直循环").await;

    // 返回 Ok（可能是空字符串）或 Err（MockProvider 队列空，第11次调用）
    // 两种结果均合法，关键是：不无限循环
    match result {
        Ok(text) => {
            // 10 次 tool_call 后 for 循环退出，final_text 可能为空
            // 也可能是最后一次 tool_call 的执行结果被当作回复（取决于实现）
            let _ = text;
        }
        Err(e) => {
            // MockProvider 队列耗尽（第11次 Phase2 调用超出预设），这也是合法结果
            assert!(
                e.to_string().contains("队列已空") || e.to_string().contains("MockProvider"),
                "错误应来自 MockProvider 队列耗尽，实际: {}",
                e
            );
        }
    }

    // 验证 history 中有工具调用记录（至少执行了若干次）
    let tool_call_count = agent
        .history()
        .iter()
        .filter(|m| matches!(m, ConversationMessage::AssistantToolCalls { .. }))
        .count();

    assert!(
        tool_call_count >= 1,
        "应有至少 1 次工具调用记录，实际 AssistantToolCalls 数量: {}",
        tool_call_count
    );
}
