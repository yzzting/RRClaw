use color_eyre::eyre::Result;
use tracing::{debug, info, warn};

use tokio::sync::mpsc;

use crate::memory::{Memory, MemoryCategory};
use crate::providers::{ChatMessage, ConversationMessage, Provider, StreamEvent, ToolSpec, ToolStatusKind};
use crate::security::{AutonomyLevel, SecurityPolicy};
use crate::skills::SkillMeta;
use crate::tools::Tool;

const MAX_TOOL_ITERATIONS: usize = 10;
const MAX_HISTORY_SIZE: usize = 50;

/// history 条数达到此值时触发压缩
const COMPACT_THRESHOLD: usize = 40;
/// 每次压缩的窗口大小（前 N 条被摘要）
const COMPACT_WINDOW: usize = 30;
/// 压缩生成的摘要最大字符数
const COMPACT_SUMMARY_MAX_CHARS: usize = 1500;

/// Phase 1 路由结果
#[derive(Debug, Clone, PartialEq)]
pub enum RouteResult {
    /// 命中一个或多个 skill，携带 skill 名称列表
    Skills(Vec<String>),
    /// 意图清晰，无需 skill，直接进 Phase 2 执行
    Direct,
    /// 意图模糊，需要向用户澄清，携带澄清问题
    NeedClarification(String),
}

/// 从可能包含 markdown 代码块的文本中提取 JSON 字符串
fn extract_json(text: &str) -> &str {
    let text = text.trim();
    // 处理 ```json ... ``` 或 ``` ... ```
    if let Some(start) = text.find('{') {
        if let Some(end) = text.rfind('}') {
            return &text[start..=end];
        }
    }
    text
}

/// 解析 Phase 1 LLM 输出，独立纯函数（便于测试）
fn parse_route_result(text: &str) -> RouteResult {
    // 从文本中提取 JSON（LLM 有时会在 JSON 前后加 markdown ```）
    let json_str = extract_json(text);

    let Ok(value) = serde_json::from_str::<serde_json::Value>(json_str) else {
        debug!("Phase 1 输出解析失败，降级为 Direct: {:?}", text);
        return RouteResult::Direct;
    };

    let skills = value["skills"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if !skills.is_empty() {
        return RouteResult::Skills(skills);
    }

    let direct = value["direct"].as_bool().unwrap_or(false);
    if direct {
        return RouteResult::Direct;
    }

    if let Some(question) = value["question"].as_str() {
        if !question.is_empty() {
            return RouteResult::NeedClarification(question.to_string());
        }
    }

    // 兜底：无法判断时降级为 Direct
    RouteResult::Direct
}

/// 构造 Phase 1 的 system prompt，按语言分发
fn build_routing_prompt(skills: &[SkillMeta], lang: crate::i18n::Language) -> String {
    match lang {
        crate::i18n::Language::English => build_routing_prompt_en(skills),
        crate::i18n::Language::Chinese => build_routing_prompt_zh(skills),
    }
}

fn build_routing_prompt_en(skills: &[SkillMeta]) -> String {
    let mut prompt = String::new();

    // [1] Identity
    prompt.push_str("You are RRClaw's routing assistant. Your only job is to analyze the user's message and decide which behavior guides (skills) to load.\n\n");

    // [2] Constraints (hard-coded, non-skippable)
    prompt.push_str("[Constraints]\n");
    prompt.push_str("- Do not call any tools\n");
    prompt.push_str("- Output JSON only, no other text\n\n");

    // [3] Available skills (L1 metadata)
    if skills.is_empty() {
        prompt.push_str("[Available Skills]\nNo skills available.\n\n");
    } else {
        prompt.push_str("[Available Skills]\n");
        for skill in skills {
            prompt.push_str(&format!("- {}: {}\n", skill.name, skill.description));
        }
        prompt.push('\n');
    }

    // [4] Output format
    prompt.push_str("[Output Format]\n");
    prompt.push_str("Output valid JSON, one of three cases:\n\n");
    prompt.push_str("1. Skills needed (clear intent, matching skill found):\n");
    prompt.push_str("   {\"skills\": [\"skill-name\"], \"direct\": false}\n\n");
    prompt.push_str("2. No skill needed, intent is clear:\n");
    prompt.push_str("   {\"skills\": [], \"direct\": true}\n\n");
    prompt.push_str("3. Intent unclear, need clarification (only when truly ambiguous):\n");
    prompt.push_str("   {\"skills\": [], \"direct\": false, \"question\": \"your clarification question\"}\n\n");

    // [5] Principles
    prompt.push_str("[Principles]\n");
    prompt.push_str("- When intent is clear, choose direct: true even if no skill matches\n");
    prompt.push_str("- Skills are enhancements, not gates — don't ask the user when no skill matches\n");
    prompt.push_str("- Only return a question when the intent is genuinely ambiguous and proceeding would go wrong\n");
    prompt.push_str("- Use the same language as the user in the question field\n");

    prompt
}

fn build_routing_prompt_zh(skills: &[SkillMeta]) -> String {
    let mut prompt = String::new();

    // [1] 身份
    prompt.push_str("你是 RRClaw 的路由助手。你的唯一任务是分析用户消息，决定需要加载哪些行为指南（skill）。\n\n");

    // [2] 安全约束（硬编码，不可跳过）
    prompt.push_str("【约束】\n");
    prompt.push_str("- 禁止调用任何工具\n");
    prompt.push_str("- 只输出 JSON，不做其他任何操作\n\n");

    // [3] 可用 Skill 目录（L1 元数据）
    if skills.is_empty() {
        prompt.push_str("【可用 Skill】\n暂无可用 skill。\n\n");
    } else {
        prompt.push_str("【可用 Skill】\n");
        for skill in skills {
            prompt.push_str(&format!("- {}: {}\n", skill.name, skill.description));
        }
        prompt.push('\n');
    }

    // [4] 输出格式说明
    prompt.push_str("【输出格式】\n");
    prompt.push_str("必须输出合法 JSON，三种情况之一：\n\n");
    prompt.push_str("1. 需要加载 skill（意图明确且有匹配 skill）：\n");
    prompt.push_str("   {\"skills\": [\"skill-name\"], \"direct\": false}\n\n");
    prompt.push_str("2. 无需 skill，意图清晰可直接执行：\n");
    prompt.push_str("   {\"skills\": [], \"direct\": true}\n\n");
    prompt.push_str("3. 意图模糊，需要向用户澄清（仅在真正无法判断时使用）：\n");
    prompt.push_str("   {\"skills\": [], \"direct\": false, \"question\": \"你的澄清问题\"}\n\n");

    // [5] 判断原则
    prompt.push_str("【判断原则】\n");
    prompt.push_str("- 用户意图清晰时，即使没有匹配的 skill，也应选择 direct: true\n");
    prompt.push_str("- skill 是增强，不是门槛——没有 skill 可以匹配时不要问用户\n");
    prompt.push_str("- 只有用户表达含糊、继续执行会走错方向时，才返回 question\n");
    prompt.push_str("- question 字段使用中文，简洁明确\n");

    prompt
}

/// 工具执行确认回调
/// 参数: (tool_name, tool_arguments) → 返回 true 表示允许执行
pub type ConfirmFn = Box<dyn Fn(&str, &serde_json::Value) -> bool + Send + Sync>;

/// AI Agent 核心
pub struct Agent {
    provider: Box<dyn Provider>,
    tools: Vec<Box<dyn Tool>>,
    memory: Box<dyn Memory>,
    policy: SecurityPolicy,
    provider_name: String,
    base_url: String,
    model: String,
    temperature: f64,
    history: Vec<ConversationMessage>,
    confirm_fn: Option<ConfirmFn>,
    /// L1 元数据，用于 system prompt 技能列表（不含 SkillTool 本身）
    skills_meta: Vec<SkillMeta>,
    /// Phase 1 路由后加载的 skill 内容，每次 process_message 重置
    routed_skill_content: Option<String>,
    /// Phase 1.5 关键词路由后的工具名列表，每次 process_message 重置
    /// 空列表表示降级：暴露所有工具
    routed_tool_names: Vec<String>,
    /// 启动时加载的身份文件内容
    identity_context: Option<String>,
    /// 当前执行的 Routine 名称（None 表示普通对话模式）
    routine_name: Option<String>,
    /// P7-3: 本轮已处理参数缺失并注入完整 schema 的工具名集合（每轮重置）
    expanded_tools: std::collections::HashSet<String>,
}

impl Agent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: Box<dyn Provider>,
        tools: Vec<Box<dyn Tool>>,
        memory: Box<dyn Memory>,
        policy: SecurityPolicy,
        provider_name: String,
        base_url: String,
        model: String,
        temperature: f64,
        skills_meta: Vec<SkillMeta>,
        identity_context: Option<String>,
    ) -> Self {
        Self {
            provider,
            tools,
            memory,
            policy,
            provider_name,
            base_url,
            model,
            temperature,
            history: Vec::new(),
            confirm_fn: None,
            skills_meta,
            routed_skill_content: None,
            routed_tool_names: Vec::new(),
            identity_context,
            routine_name: None,
            expanded_tools: std::collections::HashSet::new(),
        }
    }

    /// 手动注入技能上下文（/skill <name> 用）
    /// 将技能指令作为 user 消息推入 history，LLM 下一轮自然遵循
    pub fn inject_skill_context(&mut self, skill_name: &str, instructions: &str) {
        let msg = ConversationMessage::Chat(ChatMessage {
            role: "user".to_string(),
            content: format!("[技能指令: {}]\n{}", skill_name, instructions),
            reasoning_content: None,
        });
        self.history.push(msg);
    }

    /// 设置工具执行确认回调（用于 Supervised 模式）
    pub fn set_confirm_fn(&mut self, f: ConfirmFn) {
        self.confirm_fn = Some(f);
    }

    /// Phase 1 路由：调用轻量 LLM 决定需要加载哪些 skill
    async fn route(&self, user_message: &str) -> Result<RouteResult> {
        let lang = crate::config::Config::get_language();
        let routing_prompt = build_routing_prompt(&self.skills_meta, lang);

        // 取最近 2 条纯文本历史（跳过 ToolCalls/ToolResults），
        // 让路由 LLM 理解对话上下文，避免对"方案B"/"继续"等短消息误判为 NeedClarification
        let recent_context: Vec<ConversationMessage> = self
            .history
            .iter()
            .rev()
            .filter(|m| matches!(m, ConversationMessage::Chat(_)))
            .take(2)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();

        let mut messages = vec![ConversationMessage::Chat(ChatMessage {
            role: "system".to_string(),
            content: routing_prompt,
            reasoning_content: None,
        })];
        messages.extend(recent_context);
        messages.push(ConversationMessage::Chat(ChatMessage {
            role: "user".to_string(),
            content: user_message.to_string(),
            reasoning_content: None,
        }));

        // Phase 1 不传工具，温度极低保证输出稳定
        let response = self
            .provider
            .chat_with_tools(
                &messages,
                &[], // 空工具列表，Phase 1 禁止工具调用
                &self.model,
                0.1, // 低温度，路由输出要确定性
            )
            .await;

        match response {
            Err(e) => {
                // Phase 1 调用失败，降级为 Direct，不阻断请求
                debug!("Phase 1 路由失败，降级为 Direct: {}", e);
                Ok(RouteResult::Direct)
            }
            Ok(resp) => {
                let text = resp.text.unwrap_or_default();
                Ok(parse_route_result(&text))
            }
        }
    }

    /// 加载 skill L2 内容，存到临时字段，Phase 2 构建 system prompt 时使用
    fn inject_routed_skills(&mut self, skill_names: &[String]) {
        let mut content = String::new();
        for name in skill_names {
            // 使用 src/skills/mod.rs 中的 load_skill_content(name, skills) -> Result<SkillContent>
            // SkillContent.instructions 是去除 frontmatter 后的正文
            if let Ok(skill_content) = crate::skills::load_skill_content(name, &self.skills_meta, crate::config::Config::get_language()) {
                content.push_str(&format!(
                    "\n\n---\n## Skill: {}\n{}",
                    name, skill_content.instructions
                ));
            }
        }
        if !content.is_empty() {
            self.routed_skill_content = Some(content);
        } else {
            self.routed_skill_content = None;
        }
    }

    /// 获取当前对话历史（用于持久化）
    pub fn history(&self) -> &[ConversationMessage] {
        &self.history
    }

    /// 设置对话历史（用于恢复持久化的对话）
    /// 自动清理开头孤立的 ToolResult，避免 API 报错
    pub fn set_history(&mut self, history: Vec<ConversationMessage>) {
        self.history = history;
        self.sanitize_history();
    }

    /// 清空对话历史（/new 命令用）
    pub fn clear_history(&mut self) {
        self.history.clear();
    }

    /// 获取当前 Provider 名
    pub fn provider_name(&self) -> &str {
        &self.provider_name
    }

    /// 获取当前 base_url
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// 获取当前模型名
    pub fn model(&self) -> &str {
        &self.model
    }

    /// 运行时切换模型（同 Provider 下）
    pub fn set_model(&mut self, model: String) {
        self.model = model;
    }

    /// 运行时切换 Provider、模型和 base_url
    pub fn switch_provider(
        &mut self,
        provider: Box<dyn Provider>,
        provider_name: String,
        base_url: String,
        model: String,
    ) {
        self.provider = provider;
        self.provider_name = provider_name;
        self.base_url = base_url;
        self.model = model;
    }

    /// 获取当前温度
    pub fn temperature(&self) -> f64 {
        self.temperature
    }

    /// 获取安全策略引用
    pub fn policy(&self) -> &SecurityPolicy {
        &self.policy
    }

    /// 切换自主级别（运行时生效，不持久化）
    pub fn set_autonomy(&mut self, level: crate::security::AutonomyLevel) {
        self.policy.autonomy = level;
    }

    /// 标记当前 Agent 为 Routine 执行模式（注入 Routine 专属 system prompt 段）
    pub fn set_routine_name(&mut self, name: String) {
        self.routine_name = Some(name);
    }

    /// 重新加载身份文件（无需重启）
    /// 调用方需提供 data_dir（Agent 自身不存储，避免扩大结构体）
    pub fn reload_identity(&mut self, workspace_dir: &std::path::Path, data_dir: &std::path::Path) {
        self.identity_context = crate::agent::identity::load_identity_context(
            workspace_dir,
            data_dir,
        );
        if self.identity_context.is_some() {
            tracing::info!("身份文件已重新加载");
        }
    }

    /// 获取所有已加载工具的名称列表
    pub fn tool_names(&self) -> Vec<&str> {
        self.tools.iter().map(|t| t.name()).collect()
    }

    /// 清理 history 中无效的消息序列
    /// - 移除开头孤立的 ToolResult（没有对应的 AssistantToolCalls）
    /// - 移除中间孤立的 ToolResult（前面不是 AssistantToolCalls 或 ToolResult）
    fn sanitize_history(&mut self) {
        if self.history.is_empty() {
            return;
        }
        // 逐条检查，保留合法序列
        let mut cleaned = Vec::with_capacity(self.history.len());
        for msg in self.history.drain(..) {
            match &msg {
                ConversationMessage::ToolResult { .. } => {
                    // ToolResult 只能出现在 AssistantToolCalls 或另一个 ToolResult 之后
                    let prev_ok = cleaned.last().is_some_and(|prev| {
                        matches!(
                            prev,
                            ConversationMessage::AssistantToolCalls { .. }
                                | ConversationMessage::ToolResult { .. }
                        )
                    });
                    if prev_ok {
                        cleaned.push(msg);
                    } else {
                        debug!("清理孤立 ToolResult: {:?}", msg);
                    }
                }
                _ => cleaned.push(msg),
            }
        }
        self.history = cleaned;
    }

    /// 新 Turn 开始前，清空 history 中旧的 reasoning_content
    /// DeepSeek/MiniMax 文档建议：新用户问题开始时删除旧 reasoning_content 以节省带宽
    fn clear_old_reasoning_content(&mut self) {
        for msg in &mut self.history {
            match msg {
                ConversationMessage::Chat(cm) if cm.role == "assistant" => {
                    cm.reasoning_content = None;
                }
                ConversationMessage::AssistantToolCalls { reasoning_content, .. } => {
                    *reasoning_content = None;
                }
                _ => {}
            }
        }
    }

    /// 处理一条用户消息，返回 AI 最终回复
    pub async fn process_message(&mut self, user_msg: &str) -> Result<String> {
        // 0. 新 Turn: 清空旧 reasoning_content（节省 token，DeepSeek/MiniMax 文档建议）
        self.clear_old_reasoning_content();

        // ─── Phase 1: 路由 ───────────────────────────────────────────
        let route_result = self.route(user_msg).await?;

        match route_result {
            RouteResult::NeedClarification(question) => {
                // 直接返回澄清问题字符串，不写入 history，不执行任何工具
                // CLI/Telegram 层收到后直接展示给用户
                return Ok(question);
            }
            RouteResult::Skills(skill_names) => {
                // 加载对应 skill 的 L2 内容，注入到本次 Phase 2 的 system prompt
                self.inject_routed_skills(&skill_names);
            }
            RouteResult::Direct => {
                // 清空本次临时注入的 skill（上一轮可能有残留）
                self.routed_skill_content = None;
            }
        }

        // ─── Phase 1.5: 关键词工具路由 ────────────────────────────────
        self.routed_tool_names = crate::agent::tool_groups::route_tools(user_msg);
        if !self.routed_tool_names.is_empty() {
            debug!("Phase 1.5 工具路由: {:?}", self.routed_tool_names);
        }

        // ─── Phase 2: 正常 Agent Loop ────────────────────────────────
        // 1. Memory recall
        let memories = self.memory.recall(user_msg, 5).await.unwrap_or_default();

        // 2. 构造 system prompt（使用路由后的工具列表）
        let system_prompt = self.build_system_prompt(&memories);

        // 3. 添加用户消息到 history
        self.history.push(ConversationMessage::Chat(ChatMessage {
            role: "user".to_string(),
            content: user_msg.to_string(),
            reasoning_content: None,
        }));

        // 4. Tool call 循环（工具 spec 由 build_tool_specs 统一管理）
        // P7-3: tool_specs 可变，允许在循环内按需升级工具 schema
        let mut tool_specs = self.build_tool_specs(user_msg);
        // P7-3: 每轮重置已扩展集合
        self.expanded_tools.clear();
        let mut final_text = String::new();

        for iteration in 0..MAX_TOOL_ITERATIONS {
            // 构造消息列表：system + history
            let mut messages = vec![ConversationMessage::Chat(ChatMessage {
                role: "system".to_string(),
                content: system_prompt.clone(),
                reasoning_content: None,
            })];
            messages.extend(self.history.clone());

            debug!("iteration={}, history_len={}", iteration, self.history.len());
            debug!("system_prompt:\n{}", system_prompt);
            debug!("messages_to_llm: {:?}", messages);

            // 调用 Provider
            let response = self
                .provider
                .chat_with_tools(&messages, &tool_specs, &self.model, self.temperature)
                .await?;

            debug!(
                "response: text={:?}, tool_calls_count={}",
                response.text.as_deref().map(|t| truncate_str(t, 100)),
                response.tool_calls.len()
            );

            if response.tool_calls.is_empty() {
                // 无 tool calls — 最终回复
                final_text = response.text.unwrap_or_default();
                if final_text.is_empty() {
                    warn!("模型返回空文本回复");
                }
                self.history.push(ConversationMessage::Chat(ChatMessage {
                    role: "assistant".to_string(),
                    content: final_text.clone(),
                    reasoning_content: response.reasoning_content.clone(),
                }));
                break;
            }

            // 有 tool calls — 记录并逐个执行
            self.history
                .push(ConversationMessage::AssistantToolCalls {
                    text: response.text.clone(),
                    reasoning_content: response.reasoning_content.clone(),
                    tool_calls: response.tool_calls.clone(),
                });

            for tc in &response.tool_calls {
                // 预验证: 在确认前检查安全策略（避免确认后被拒绝）
                if let Some(tool) = self.tools.iter().find(|t| t.name() == tc.name) {
                    if let Some(rejection) = tool.pre_validate(&tc.arguments, &self.policy) {
                        info!("工具预验证失败: {} - {}", tc.name, rejection);
                        self.history.push(ConversationMessage::ToolResult {
                            tool_call_id: tc.id.clone(),
                            content: format!("[失败] {}", rejection),
                        });
                        continue;
                    }
                }

                // ─── P7-3: 动态 Schema 补充 ──────────────────────────────────────────
                // 检测必填参数缺失（每轮每个工具只触发一次，避免死循环）
                if !self.expanded_tools.contains(&tc.name) {
                    let missing = {
                        self.tools
                            .iter()
                            .find(|t| t.name() == tc.name)
                            .map(|t| find_missing_required_params(&t.parameters_schema(), &tc.arguments))
                            .unwrap_or_default()
                    };
                    if !missing.is_empty() {
                        self.expanded_tools.insert(tc.name.clone());
                        // 升级 MCP 工具为 L2 完整 schema（对内置工具无副作用）
                        if let Some(tool) = self.tools.iter_mut().find(|t| t.name() == tc.name) {
                            tool.load_full_schema();
                        }
                        // 更新 tool_specs 供下一迭代使用
                        if let Some(tool) = self.tools.iter().find(|t| t.name() == tc.name) {
                            let new_spec = tool.spec();
                            if let Some(spec) = tool_specs.iter_mut().find(|s| s.name == tc.name) {
                                *spec = new_spec;
                            } else {
                                tool_specs.push(new_spec);
                            }
                        }
                        debug!("P7-3: 工具 '{}' 缺少参数 {:?}，已注入完整 schema", tc.name, missing);
                        self.history.push(ConversationMessage::ToolResult {
                            tool_call_id: tc.id.clone(),
                            content: format!(
                                "[参数缺失] 工具 '{}' 缺少必填参数: {}。完整参数说明已在工具列表中更新，请用正确参数重新调用。",
                                tc.name,
                                missing.join(", ")
                            ),
                        });
                        continue;
                    }
                }
                // ─── P7-3 结束 ────────────────────────────────────────────────────────

                // Supervised 模式: 执行前需用户确认
                if self.policy.requires_confirmation() {
                    if let Some(confirm) = &self.confirm_fn {
                        if !confirm(&tc.name, &tc.arguments) {
                            info!("用户拒绝执行工具: {}", tc.name);
                            self.history.push(ConversationMessage::ToolResult {
                                tool_call_id: tc.id.clone(),
                                content: "用户拒绝执行该工具".to_string(),
                            });
                            continue;
                        }
                    }
                }

                info!("执行工具: {} args={}", tc.name, tc.arguments);
                let result = self.execute_tool(&tc.name, tc.arguments.clone()).await;
                debug!("工具结果: {}", truncate_str(&result, 200));

                // MCP 工具首次调用后升级为 L2 完整 schema（下轮用户消息生效）
                if tc.name.starts_with("mcp_") {
                    if let Some(tool) = self.tools.iter_mut().find(|t| t.name() == tc.name) {
                        if !tool.is_full_schema_loaded() {
                            tool.load_full_schema();
                            debug!("MCP 工具 '{}' 已升级为 L2 完整 schema", tc.name);
                        }
                    }
                }

                // ─── Prompt Injection 检测 ───────────────────────────────────────────
                // 只检测外部数据工具（shell/file_read/git/http_request）；
                // 内部工具（memory_*/skill/self_info/config）返回受控内容，跳过检测
                let final_content = if self.policy.injection_check && needs_injection_check(&tc.name) {
                    let injection = crate::security::injection::check_tool_result(&result);
                    if let Some(ref sev) = injection.severity {
                        info!(
                            tool = %tc.name,
                            severity = ?sev,
                            reason = ?injection.reason,
                            "Prompt injection detected in tool result"
                        );
                    }
                    injection.sanitized
                } else {
                    result
                };
                // ─── 检测结束 ─────────────────────────────────────────────────────────

                self.history.push(ConversationMessage::ToolResult {
                    tool_call_id: tc.id.clone(),
                    content: final_content,
                });
            }
        }

        // 5. Memory store — 保存对话摘要
        let summary = format!("User: {}\nAssistant: {}", user_msg, final_text);
        let key = format!("conv_{}", chrono::Utc::now().timestamp_millis());
        let _ = self
            .memory
            .store(&key, &summary, MemoryCategory::Conversation)
            .await;

        // 6. 裁剪 history
        self.compact_history_if_needed().await;

        Ok(final_text)
    }

    /// 处理一条用户消息（流式版本）
    /// 文本 token 通过 tx 实时发送给调用方，最终返回完整文本
    pub async fn process_message_stream(
        &mut self,
        user_msg: &str,
        tx: mpsc::Sender<StreamEvent>,
    ) -> Result<String> {
        // 0. 新 Turn: 清空旧 reasoning_content（节省 token，DeepSeek/MiniMax 文档建议）
        self.clear_old_reasoning_content();

        // ─── Phase 1: 路由 ───────────────────────────────────────────
        let route_result = self.route(user_msg).await?;

        match route_result {
            RouteResult::NeedClarification(question) => {
                // 通过 tx 发送澄清问题，不写入 history，不执行任何工具
                // 必须走 tx 发送，否则 stream_message 里 Ok(_) 会丢弃返回值
                let _ = tx.send(StreamEvent::Text(question.clone())).await;
                return Ok(question);
            }
            RouteResult::Skills(skill_names) => {
                // 加载对应 skill 的 L2 内容，注入到本次 Phase 2 的 system prompt
                self.inject_routed_skills(&skill_names);
            }
            RouteResult::Direct => {
                // 清空本次临时注入的 skill（上一轮可能有残留）
                self.routed_skill_content = None;
            }
        }

        // ─── Phase 1.5: 关键词工具路由 ────────────────────────────────
        self.routed_tool_names = crate::agent::tool_groups::route_tools(user_msg);
        if !self.routed_tool_names.is_empty() {
            debug!("Phase 1.5 工具路由(stream): {:?}", self.routed_tool_names);
        }

        // ─── Phase 2: 正常 Agent Loop ────────────────────────────────
        // 1. Memory recall
        let memories = self.memory.recall(user_msg, 5).await.unwrap_or_default();

        // 2. 构造 system prompt（使用路由后的工具列表）
        let system_prompt = self.build_system_prompt(&memories);

        // 3. 添加用户消息到 history
        self.history.push(ConversationMessage::Chat(ChatMessage {
            role: "user".to_string(),
            content: user_msg.to_string(),
            reasoning_content: None,
        }));

        // 4. Tool call 循环（工具 spec 由 build_tool_specs 统一管理）
        // P7-3: tool_specs 可变，允许在循环内按需升级工具 schema
        let mut tool_specs = self.build_tool_specs(user_msg);
        // P7-3: 每轮重置已扩展集合（stream 版本共享同一 expanded_tools）
        self.expanded_tools.clear();
        let mut final_text = String::new();

        for iteration in 0..MAX_TOOL_ITERATIONS {
            let mut messages = vec![ConversationMessage::Chat(ChatMessage {
                role: "system".to_string(),
                content: system_prompt.clone(),
                reasoning_content: None,
            })];
            messages.extend(self.history.clone());

            debug!("stream iteration={}, history_len={}", iteration, self.history.len());
            debug!("system_prompt:\n{}", system_prompt);
            debug!("messages_to_llm: {:?}", messages);

            // 发送 Thinking 状态
            let _ = tx.send(StreamEvent::Thinking).await;

            // 流式调用 Provider
            let response = self
                .provider
                .chat_stream(&messages, &tool_specs, &self.model, self.temperature, tx.clone())
                .await?;

            debug!(
                "stream response: text={:?}, tool_calls_count={}",
                response.text.as_deref().map(|t| truncate_str(t, 100)),
                response.tool_calls.len()
            );

            if response.tool_calls.is_empty() {
                final_text = response.text.unwrap_or_default();
                if final_text.is_empty() {
                    warn!("流式: 模型返回空文本回复");
                }
                self.history.push(ConversationMessage::Chat(ChatMessage {
                    role: "assistant".to_string(),
                    content: final_text.clone(),
                    reasoning_content: response.reasoning_content.clone(),
                }));
                break;
            }

            // 有 tool calls — 先停止 thinking spinner（避免和确认提示冲突）
            let _ = tx.send(StreamEvent::Done(response.clone())).await;
            // 等待 print_handle 处理 Done 事件（清理 spinner），避免和确认提示竞争
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;

            // 有 tool calls — tool call 阶段不流式输出文本给用户
            self.history
                .push(ConversationMessage::AssistantToolCalls {
                    text: response.text.clone(),
                    reasoning_content: response.reasoning_content.clone(),
                    tool_calls: response.tool_calls.clone(),
                });

            for tc in &response.tool_calls {
                // 预验证: 在确认前检查安全策略（避免确认后被拒绝）
                if let Some(tool) = self.tools.iter().find(|t| t.name() == tc.name) {
                    if let Some(rejection) = tool.pre_validate(&tc.arguments, &self.policy) {
                        info!("工具预验证失败: {} - {}", tc.name, rejection);
                        self.history.push(ConversationMessage::ToolResult {
                            tool_call_id: tc.id.clone(),
                            content: format!("[失败] {}", rejection),
                        });
                        continue;
                    }
                }

                // ─── P7-3: 动态 Schema 补充 ──────────────────────────────────────────
                // 检测必填参数缺失（每轮每个工具只触发一次，避免死循环）
                if !self.expanded_tools.contains(&tc.name) {
                    let missing = {
                        self.tools
                            .iter()
                            .find(|t| t.name() == tc.name)
                            .map(|t| find_missing_required_params(&t.parameters_schema(), &tc.arguments))
                            .unwrap_or_default()
                    };
                    if !missing.is_empty() {
                        self.expanded_tools.insert(tc.name.clone());
                        // 升级 MCP 工具为 L2 完整 schema（对内置工具无副作用）
                        if let Some(tool) = self.tools.iter_mut().find(|t| t.name() == tc.name) {
                            tool.load_full_schema();
                        }
                        // 更新 tool_specs 供下一迭代使用
                        if let Some(tool) = self.tools.iter().find(|t| t.name() == tc.name) {
                            let new_spec = tool.spec();
                            if let Some(spec) = tool_specs.iter_mut().find(|s| s.name == tc.name) {
                                *spec = new_spec;
                            } else {
                                tool_specs.push(new_spec);
                            }
                        }
                        debug!("P7-3(stream): 工具 '{}' 缺少参数 {:?}，已注入完整 schema", tc.name, missing);
                        self.history.push(ConversationMessage::ToolResult {
                            tool_call_id: tc.id.clone(),
                            content: format!(
                                "[参数缺失] 工具 '{}' 缺少必填参数: {}。完整参数说明已在工具列表中更新，请用正确参数重新调用。",
                                tc.name,
                                missing.join(", ")
                            ),
                        });
                        continue;
                    }
                }
                // ─── P7-3 结束 ────────────────────────────────────────────────────────

                // Supervised 模式: 执行前需用户确认
                if self.policy.requires_confirmation() {
                    if let Some(confirm) = &self.confirm_fn {
                        if !confirm(&tc.name, &tc.arguments) {
                            info!("用户拒绝执行工具: {}", tc.name);
                            self.history.push(ConversationMessage::ToolResult {
                                tool_call_id: tc.id.clone(),
                                content: "用户拒绝执行该工具".to_string(),
                            });
                            continue;
                        }
                    }
                }

                // 发送执行状态
                let cmd_summary = if tc.name == "shell" {
                    tc.arguments.get("command").and_then(|v| v.as_str()).unwrap_or(&tc.name).to_string()
                } else {
                    tc.name.clone()
                };
                let _ = tx.send(StreamEvent::ToolStatus {
                    name: tc.name.clone(),
                    status: ToolStatusKind::Running(cmd_summary.clone()),
                }).await;

                info!("执行工具: {} args={}", tc.name, tc.arguments);
                let result = self.execute_tool(&tc.name, tc.arguments.clone()).await;
                debug!("工具结果: {}", truncate_str(&result, 200));

                // MCP 工具首次调用后升级为 L2 完整 schema（下轮用户消息生效）
                if tc.name.starts_with("mcp_") {
                    if let Some(tool) = self.tools.iter_mut().find(|t| t.name() == tc.name) {
                        if !tool.is_full_schema_loaded() {
                            tool.load_full_schema();
                            debug!("MCP 工具 '{}' 已升级为 L2 完整 schema", tc.name);
                        }
                    }
                }

                // 发送执行结果状态
                if result.starts_with("[失败]") || result.starts_with("[错误]") {
                    let _ = tx.send(StreamEvent::ToolStatus {
                        name: tc.name.clone(),
                        status: ToolStatusKind::Failed(truncate_str(&result, 200)),
                    }).await;
                } else {
                    // 成功时显示首行预览
                    let summary = if result.len() > 80 {
                        let first_line = result.lines().next().unwrap_or("");
                        let preview = truncate_str(first_line, 60);
                        format!("{} (共{}字节)", preview, result.len())
                    } else {
                        truncate_str(&result, 80)
                    };
                    let _ = tx.send(StreamEvent::ToolStatus {
                        name: tc.name.clone(),
                        status: ToolStatusKind::Success(summary),
                    }).await;
                }

                // ─── Prompt Injection 检测 ───────────────────────────────────────────
                // 只检测外部数据工具（shell/file_read/git/http_request）；
                // 内部工具（memory_*/skill/self_info/config）返回受控内容，跳过检测
                let final_content = if self.policy.injection_check && needs_injection_check(&tc.name) {
                    let injection = crate::security::injection::check_tool_result(&result);
                    if let Some(ref sev) = injection.severity {
                        info!(
                            tool = %tc.name,
                            severity = ?sev,
                            reason = ?injection.reason,
                            "Prompt injection detected in tool result"
                        );
                    }
                    injection.sanitized
                } else {
                    result
                };
                // ─── 检测结束 ─────────────────────────────────────────────────────────

                self.history.push(ConversationMessage::ToolResult {
                    tool_call_id: tc.id.clone(),
                    content: final_content,
                });
            }
        }

        // 5. Memory store
        let summary = format!("User: {}\nAssistant: {}", user_msg, final_text);
        let key = format!("conv_{}", chrono::Utc::now().timestamp_millis());
        let _ = self
            .memory
            .store(&key, &summary, MemoryCategory::Conversation)
            .await;

        // 6. 裁剪 history
        self.compact_history_if_needed().await;

        Ok(final_text)
    }

    /// 执行工具，返回结果文本
    async fn execute_tool(&self, name: &str, args: serde_json::Value) -> String {
        let tool = match self.tools.iter().find(|t| t.name() == name) {
            Some(t) => t,
            None => return format!("[错误] 未知工具: {}", name),
        };

        match tool.execute(args, &self.policy).await {
            Ok(result) => {
                if result.success {
                    result.output
                } else {
                    // 保留 output + error，让 LLM 自己判断
                    let error = result.error.unwrap_or_else(|| "未知错误".to_string());
                    if result.output.is_empty() {
                        format!("[失败] {}", error)
                    } else {
                        format!("[失败] {}\n[部分输出]\n{}", error, result.output)
                    }
                }
            }
            Err(e) => format!("[错误] {}", e),
        }
    }

    /// 构造 system prompt，实时读取语言配置后分发到对应语言版本
    fn build_system_prompt(&self, memories: &[crate::memory::MemoryEntry]) -> String {
        let lang = crate::config::Config::get_language();
        match lang {
            crate::i18n::Language::English => self.build_system_prompt_en(memories),
            crate::i18n::Language::Chinese => self.build_system_prompt_zh(memories),
        }
    }

    fn build_system_prompt_en(&self, memories: &[crate::memory::MemoryEntry]) -> String {
        let mut parts = Vec::new();

        // [0] Custom user context (identity file)
        if let Some(identity) = &self.identity_context {
            parts.push(format!("[Custom Context]\n{}", identity));
        }

        // [1] Identity
        parts.push("You are RRClaw, a safety-first AI assistant.".to_string());

        // [2] Available tools (filtered by Phase 1.5 routing; empty list = show all)
        if !self.tools.is_empty() {
            let mut tools_desc = "You can use the following tools:\n".to_string();

            for tool in &self.tools {
                if tool.name().starts_with("mcp_") {
                    continue;
                }
                let is_active = self.routed_tool_names.is_empty()
                    || self.routed_tool_names.iter().any(|n| n == tool.name())
                    || tool.name() == "skill";
                if is_active {
                    tools_desc.push_str(&format!("- {}: {}\n", tool.name(), tool.description()));
                }
            }

            let mcp_tools: Vec<_> = self.tools.iter()
                .filter(|t| t.name().starts_with("mcp_"))
                .collect();
            if !mcp_tools.is_empty() {
                tools_desc.push_str("\n[MCP Tools] (available on demand; full parameter schema is loaded automatically on first call):\n");
                for tool in &mcp_tools {
                    tools_desc.push_str(&format!("- {}: {}\n", tool.name(), tool.description()));
                }
            }

            parts.push(tools_desc);
        }

        // [2.5] Available skills (L1 metadata, excluding SkillTool itself)
        let display_skills: Vec<&SkillMeta> = self
            .skills_meta
            .iter()
            .filter(|s| s.name != "skill")
            .collect();
        if !display_skills.is_empty() {
            let mut skills_section =
                "[Available Skills] (use the skill tool to load detailed instructions when needed)\n".to_string();
            for skill in &display_skills {
                skills_section.push_str(&format!("- {}: {}\n", skill.name, skill.description));
            }
            parts.push(skills_section);
        }

        // [3] Security rules
        let security_rules = match self.policy.autonomy {
            AutonomyLevel::ReadOnly => "Read-only mode: do not attempt to call any tools.",
            AutonomyLevel::Supervised => concat!(
                "Supervised mode: call tools directly. ",
                "The system will automatically prompt the user for confirmation before execution. ",
                "Do not ask for confirmation in your text — just issue the tool call."
            ),
            AutonomyLevel::Full => "Full mode: you can execute tools autonomously within the allowed-commands list.",
        };
        parts.push(security_rules.to_string());

        // [4] Memory context
        if !memories.is_empty() {
            let mut memory_section = "[Relevant Memories]\n".to_string();
            for entry in memories {
                memory_section.push_str(&format!("- {}\n", entry.content));
            }
            parts.push(memory_section);
        }

        // [4.5] Routed skill L2 behavior guide (Phase 1 result, reset each turn)
        if let Some(skill_content) = &self.routed_skill_content {
            parts.push(format!("[Behavior Guide]\n{}", skill_content));
        }

        // [4.6] Routine execution rules (only in routine mode)
        if let Some(name) = &self.routine_name {
            parts.push(format!(
                "[Routine Execution Rules]\n\
                 You are executing scheduled task '{name}'. This is an automated task with no user interaction.\n\
                 - If the message starts with [Previously successful approach], try that approach first\n\
                 - After completing the task successfully, record the effective method with memory_store:\n\
                 \x20 - key: \"routine:{name}:approach\"\n\
                 \x20 - category: \"custom\"\n\
                 \x20 - content: describe the successful method (URL, headers, data extraction path, etc.)\n\
                 - If you find a better method, overwrite the existing record\n\
                 - Do not update the record on failure",
                name = name,
            ));
        }

        // [5] Environment info
        let workspace = self.policy.workspace_dir.display();
        let env_info = format!(
            "Working directory: {}\nCurrent time: {}",
            workspace,
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
        );
        parts.push(env_info);

        // [6] Decision principles
        parts.push(concat!(
            "[Decision Principles]\n",
            "1. Check before acting: use self_info to query uncertain info (paths, config, capabilities) — don't guess\n",
            "2. Ask when stuck: if you can't find or infer the answer, ask the user directly\n",
            "3. Explain intent: briefly explain why you need a tool before calling it\n",
            "4. Reflect on failure: analyze root cause before retrying\n",
            "   - 1st failure: analyze cause, try a different approach\n",
            "   - 2nd failure: explain the situation to the user and ask for guidance\n",
            "   - Don't attempt the same goal more than 3 times\n",
            "5. Reply in the user's language\n",
            "6. Use memory: store user preferences with memory_store when told; use memory_recall when unsure if something was discussed before\n",
            "7. When HTTP requests are blocked by SSRF protection: explain the situation to the user and ask if they want to add the address to the allowlist. After agreement, use the config tool to add it (e.g., set security.http_allowed_hosts to [\"localhost\"]), then retry",
        ).to_string());

        parts.join("\n\n")
    }

    fn build_system_prompt_zh(&self, memories: &[crate::memory::MemoryEntry]) -> String {
        let mut parts = Vec::new();

        // [0] 用户定制上下文（身份文件）
        if let Some(identity) = &self.identity_context {
            parts.push(format!("[用户定制上下文]\n{}", identity));
        }

        // [1] 身份描述
        parts.push("你是 RRClaw，一个安全优先的 AI 助手。".to_string());

        // [2] 可用工具描述（根据 Phase 1.5 路由结果过滤；空列表 = 显示所有）
        if !self.tools.is_empty() {
            let mut tools_desc = "你可以使用以下工具:\n".to_string();

            for tool in &self.tools {
                if tool.name().starts_with("mcp_") {
                    continue;
                }
                let is_active = self.routed_tool_names.is_empty()
                    || self.routed_tool_names.iter().any(|n| n == tool.name())
                    || tool.name() == "skill";
                if is_active {
                    tools_desc.push_str(&format!("- {}: {}\n", tool.name(), tool.description()));
                }
            }

            let mcp_tools: Vec<_> = self.tools.iter()
                .filter(|t| t.name().starts_with("mcp_"))
                .collect();
            if !mcp_tools.is_empty() {
                tools_desc.push_str("\n[MCP 工具]（需要时可用，首次调用后自动获取完整参数说明）:\n");
                for tool in &mcp_tools {
                    tools_desc.push_str(&format!("- {}: {}\n", tool.name(), tool.description()));
                }
            }

            parts.push(tools_desc);
        }

        // [2.5] 可用技能列表（L1 元数据，仅当有 skills 时注入）
        let display_skills: Vec<&SkillMeta> = self
            .skills_meta
            .iter()
            .filter(|s| s.name != "skill")
            .collect();
        if !display_skills.is_empty() {
            let mut skills_section =
                "[可用技能]（需要时用 skill 工具加载详细指令）\n".to_string();
            for skill in &display_skills {
                skills_section.push_str(&format!("- {}: {}\n", skill.name, skill.description));
            }
            parts.push(skills_section);
        }

        // [3] 安全规则
        let security_rules = match self.policy.autonomy {
            AutonomyLevel::ReadOnly => "当前为只读模式，不要尝试执行任何工具。",
            AutonomyLevel::Supervised => concat!(
                "当前为 Supervised 模式。你应该直接调用工具，系统会自动弹出确认提示让用户决定是否执行。",
                "不要在文本中请求用户确认，直接发起 tool call 即可。"
            ),
            AutonomyLevel::Full => "你可以自主执行工具，但须遵守白名单限制。",
        };
        parts.push(security_rules.to_string());

        // [4] 记忆上下文
        if !memories.is_empty() {
            let mut memory_section = "[相关记忆]\n".to_string();
            for entry in memories {
                memory_section.push_str(&format!("- {}\n", entry.content));
            }
            parts.push(memory_section);
        }

        // [4.5] 已路由的 skill L2 行为指南（Phase 1 结果，每轮重置）
        if let Some(skill_content) = &self.routed_skill_content {
            parts.push(format!("[行为指南]\n{}", skill_content));
        }

        // [4.6] Routine 执行规范（仅在 Routine 模式下注入）
        if let Some(name) = &self.routine_name {
            parts.push(format!(
                "[Routine 执行规范]\n\
                 你正在执行定时任务 '{name}'，这是一个自动化任务，不会有用户交互。\n\
                 - 如果消息前缀有 [历史成功方法参考]，优先尝试该方法\n\
                 - 成功完成任务后，用 memory_store 记录有效方法：\n\
                 \x20 - key: \"routine:{name}:approach\"\n\
                 \x20 - category: \"custom\"\n\
                 \x20 - content: 描述成功方法（使用的 URL、headers、数据提取路径等）\n\
                 - 如果发现更好的方法，直接覆盖旧记录\n\
                 - 失败时不要更新记录",
                name = name,
            ));
        }

        // [5] 环境信息
        let workspace = self.policy.workspace_dir.display();
        let env_info = format!(
            "工作目录: {}\n当前时间: {}",
            workspace,
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
        );
        parts.push(env_info);

        // [6] 决策原则
        parts.push(concat!(
            "[决策原则]\n",
            "1. 先查后做: 不确定的信息（路径、配置、能力）先用 self_info 工具查询，不要猜测\n",
            "2. 不知道就问: 如果查不到也推理不出，直接问用户，不要盲目尝试\n",
            "3. 说明意图: 调用工具前简短说明为什么需要这个工具\n",
            "4. 失败时反思: 工具失败后先分析原因，再决定下一步\n",
            "   - 第 1 次失败: 分析原因，换一种方式\n",
            "   - 第 2 次失败: 向用户说明情况，询问建议\n",
            "   - 不要同一个目标尝试超过 3 次\n",
            "5. 用中文回复，除非用户使用其他语言\n",
            "6. 善用记忆: 当用户告知偏好或重要信息时，用 memory_store 保存；不确定之前是否讨论过时，用 memory_recall 检索\n",
            "7. HTTP 请求被 SSRF 防护阻止时: 向用户说明情况，询问是否要将该地址加入白名单。用户同意后，用 config 工具添加（如 /config set security.http_allowed_hosts 添加 [\"localhost\"]），然后重新尝试请求",
        ).to_string());

        parts.join("\n\n")
    }

    /// 构造本轮对话的工具 spec 列表（传给 Provider）
    ///
    /// 优先级：
    /// 1. pre_select_tool 命中 → 只返回该单一工具
    /// 2. routed_tool_names 非空 → 返回路由工具 + skill 工具（始终保留）
    /// 3. 兜底 → 所有工具
    fn build_tool_specs(&self, user_msg: &str) -> Vec<ToolSpec> {
        // Priority 1: forced tool (git 命令直接路由到 git 工具)
        if let Some(tool_name) = self.pre_select_tool(user_msg) {
            debug!("强制使用工具: {}", tool_name);
            return self.tools.iter().filter(|t| t.name() == tool_name).map(|t| t.spec()).collect();
        }

        // Priority 2: Phase 1.5 关键词路由结果
        if !self.routed_tool_names.is_empty() {
            debug!("工具路由激活: {:?}", self.routed_tool_names);
            return self
                .tools
                .iter()
                .filter(|t| {
                    self.routed_tool_names.iter().any(|n| n == t.name())
                        || t.name() == "skill" // skill 工具始终可用（C 辅助路径）
                })
                .map(|t| t.spec())
                .collect();
        }

        // Fallback: 所有工具（无关键词匹配）
        self.tools.iter().map(|t| t.spec()).collect()
    }

    /// 预处理用户输入，尝试自动路由到专用工具
    /// 返回 Some(tool_name) 表示强制使用该工具，None 表示让 LLM 自行选择
    fn pre_select_tool(&self, user_input: &str) -> Option<&str> {
        let input_lower = user_input.to_lowercase();

        // 检测 git 操作（排除 github 等）
        // 匹配模式: git 开头，或包含 git 命令（git log, git status 等）
        let git_patterns = [
            "git ",      // git 开头
            "git\n",     // git 换行
            "git status",
            "git log",
            "git diff",
            "git add",
            "git commit",
            "git branch",
            "git checkout",
            "git push",
            "git pull",
            "git fetch",
        ];

        for pattern in &git_patterns {
            if input_lower.contains(*pattern) && !input_lower.contains("github") {
                // 检查 git 工具是否可用
                if self.tools.iter().any(|t| t.name() == "git") {
                    debug!("自动路由到 git 工具 (matched: {})", pattern);
                    return Some("git");
                }
            }
        }

        None
    }

    /// 裁剪 history 保持在最大限制内
    /// 确保裁剪后不会留下孤立的 ToolResult（必须紧跟 AssistantToolCalls）
    fn trim_history(&mut self) {
        if self.history.len() <= MAX_HISTORY_SIZE {
            return;
        }
        let excess = self.history.len() - MAX_HISTORY_SIZE;
        self.history.drain(..excess);

        // 跳过开头的孤立 ToolResult（它们的 AssistantToolCalls 已被裁掉）
        let skip = self
            .history
            .iter()
            .take_while(|msg| matches!(msg, ConversationMessage::ToolResult { .. }))
            .count();
        if skip > 0 {
            self.history.drain(..skip);
        }
    }

    /// 压缩 history：超过阈值时用 LLM 摘要替代早期消息
    /// 如果 LLM 摘要失败，回退到旧的硬截断策略
    async fn compact_history_if_needed(&mut self) {
        if self.history.len() < COMPACT_THRESHOLD {
            return;
        }

        tracing::info!(
            "history 达到 {} 条，触发压缩（窗口: {} 条）",
            self.history.len(),
            COMPACT_WINDOW
        );

        // 取前 COMPACT_WINDOW 条作为压缩对象
        // 但要确保不截断 AssistantToolCalls + ToolResult 对
        let window_end = find_safe_window_end(&self.history, COMPACT_WINDOW);
        let to_compress = &self.history[..window_end];

        match self.summarize_history(to_compress).await {
            Ok(summary) => {
                tracing::debug!("摘要生成成功（{}字符）", summary.len());
                // 用摘要消息替换被压缩的部分
                let summary_msg = ConversationMessage::Chat(ChatMessage {
                    role: "system".to_string(),
                    content: format!("[对话摘要 - 早期上下文]\n{}", summary),
                    reasoning_content: None,
                });
                let remaining = self.history[window_end..].to_vec();
                let remaining_len = remaining.len();
                self.history = vec![summary_msg];
                self.history.extend(remaining);
                tracing::info!(
                    "history 压缩完成: {} 条 → {} 条",
                    window_end + remaining_len,
                    self.history.len()
                );
            }
            Err(e) => {
                tracing::warn!("摘要生成失败，回退到硬截断: {:#}", e);
                self.trim_history();
            }
        }
    }

    /// 调用 LLM 对指定 history 片段生成摘要
    async fn summarize_history(&self, messages: &[ConversationMessage]) -> color_eyre::eyre::Result<String> {
        // 将 history 序列化为可读文本
        let transcript = format_history_for_summary(messages);

        // 截断过长的 transcript（避免 token 超限，用 truncate_str 保证 UTF-8 安全）
        let transcript_truncated = if transcript.len() > 12_000 {
            format!("{}...[已截断]", truncate_str(&transcript, 12_000))
        } else {
            transcript
        };

        let summary_prompt = format!(
            "请将以下对话历史压缩成简洁摘要（不超过 {} 字符）。\n\
             保留：用户的核心需求、重要决策、已解决的问题、关键信息（路径/命令/配置）。\n\
             忽略：闲聊、重复内容、工具执行的详细输出。\n\
             用中文输出，以「对话摘要：」开头。\n\n\
             ---\n{}\n---",
            COMPACT_SUMMARY_MAX_CHARS,
            transcript_truncated
        );

        let summary_messages = vec![
            ConversationMessage::Chat(ChatMessage {
                role: "user".to_string(),
                content: summary_prompt,
                reasoning_content: None,
            })
        ];

        // 直接调用 provider，不传 tools（摘要不需要 tool call）
        let response = self.provider
            .chat_with_tools(&summary_messages, &[], &self.model, 0.3)
            .await?;

        let summary = response.text.unwrap_or_default();
        if summary.is_empty() {
            color_eyre::eyre::bail!("LLM 返回空摘要");
        }

        // 截断摘要到上限
        Ok(truncate_str(&summary, COMPACT_SUMMARY_MAX_CHARS))
    }
}

/// 找到安全的压缩窗口终点：不截断 AssistantToolCalls + ToolResult 对
/// 从 ideal_end 向前找，直到找到一个安全切割点
fn find_safe_window_end(history: &[ConversationMessage], ideal_end: usize) -> usize {
    let end = ideal_end.min(history.len());
    // 从 end 向前找第一个 Chat 消息（安全切割点）
    for i in (0..end).rev() {
        if matches!(history[i], ConversationMessage::Chat(_)) {
            return i + 1;
        }
    }
    // 找不到就用 0（全部压缩）
    0
}

/// 将 history 格式化为摘要 prompt 用的可读文本
fn format_history_for_summary(messages: &[ConversationMessage]) -> String {
    let mut out = String::new();
    for msg in messages {
        match msg {
            ConversationMessage::Chat(cm) => {
                if cm.role == "system" {
                    continue; // 跳过 system 消息
                }
                let role_label = if cm.role == "user" { "用户" } else { "助手" };
                let content = if cm.content.len() > 500 {
                    truncate_str(&cm.content, 500)
                } else {
                    cm.content.clone()
                };
                out.push_str(&format!("[{}]: {}\n\n", role_label, content));
            }
            ConversationMessage::AssistantToolCalls { text, tool_calls, .. } => {
                if let Some(t) = text {
                    if !t.is_empty() {
                        out.push_str(&format!("[助手]: {}\n", t));
                    }
                }
                let tool_names: Vec<&str> = tool_calls.iter().map(|tc| tc.name.as_str()).collect();
                out.push_str(&format!("[工具调用]: {}\n\n", tool_names.join(", ")));
            }
            ConversationMessage::ToolResult { content, .. } => {
                let preview = if content.len() > 200 {
                    truncate_str(content, 200)
                } else {
                    content.clone()
                };
                out.push_str(&format!("[工具结果]: {}\n\n", preview));
            }
        }
    }
    out
}

/// 判断工具结果是否需要注入检测
///
/// 外部数据工具（shell、file_read、git、http_request）需要检测，
/// 因为其内容来自外部/用户环境，存在恶意构造的可能。
///
/// 内部工具（memory_*、skill、self_info、config）返回的是系统自身受控内容，
/// 不需要检测：
/// - memory_recall 返回的是之前我们自己存储的记忆，行数多但完全可信
/// - skill / self_info / config 返回格式化的系统信息
fn needs_injection_check(tool_name: &str) -> bool {
    matches!(tool_name, "shell" | "file_read" | "file_write" | "git" | "http_request")
}

/// P7-3: 检测工具调用缺少的必填参数
///
/// 根据工具的 JSON Schema `required` 字段，返回 `args` 中缺失的参数名列表。
/// 如果 schema 没有 `required` 字段，或所有必填参数都已提供，返回空 Vec。
fn find_missing_required_params(schema: &serde_json::Value, args: &serde_json::Value) -> Vec<String> {
    let required = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect::<Vec<_>>())
        .unwrap_or_default();

    if required.is_empty() {
        return vec![];
    }

    let args_obj = args.as_object();
    required
        .into_iter()
        .filter(|r| !args_obj.map(|o| o.contains_key(r.as_str())).unwrap_or(false))
        .collect()
}

/// UTF-8 安全的字符串截断
fn truncate_str(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    // 找到不超过 max_bytes 的最近 char boundary
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...(共{}字节)", &s[..end], s.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryEntry;
    use crate::providers::{ChatResponse, ToolCall};
    use crate::skills::SkillSource;
    use crate::tools::ToolResult;
    use std::path::PathBuf;

    // --- Mock Provider ---
    struct MockProvider {
        responses: std::sync::Mutex<Vec<ChatResponse>>,
    }

    impl MockProvider {
        fn new(responses: Vec<ChatResponse>) -> Self {
            Self {
                responses: std::sync::Mutex::new(responses),
            }
        }
    }

    #[async_trait::async_trait]
    impl Provider for MockProvider {
        async fn chat_with_tools(
            &self,
            _messages: &[ConversationMessage],
            _tools: &[ToolSpec],
            _model: &str,
            _temperature: f64,
        ) -> Result<ChatResponse> {
            let mut responses = self.responses.lock().unwrap();
            if responses.is_empty() {
                Ok(ChatResponse {
                    text: Some("默认回复".to_string()),
                    reasoning_content: None,
                    tool_calls: vec![],
                })
            } else {
                Ok(responses.remove(0))
            }
        }
    }

    // --- Mock Memory ---
    struct MockMemory;

    #[async_trait::async_trait]
    impl Memory for MockMemory {
        async fn store(&self, _key: &str, _content: &str, _category: MemoryCategory) -> Result<()> {
            Ok(())
        }
        async fn recall(&self, _query: &str, _limit: usize) -> Result<Vec<MemoryEntry>> {
            Ok(vec![])
        }
        async fn forget(&self, _key: &str) -> Result<bool> {
            Ok(false)
        }
        async fn count(&self) -> Result<usize> {
            Ok(0)
        }
    }

    // --- Mock Tool ---
    struct MockTool {
        tool_name: String,
        result: String,
    }

    #[async_trait::async_trait]
    impl Tool for MockTool {
        fn name(&self) -> &str {
            &self.tool_name
        }
        fn description(&self) -> &str {
            "Mock tool"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn execute(
            &self,
            _args: serde_json::Value,
            _policy: &SecurityPolicy,
        ) -> Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: self.result.clone(),
                error: None,
                ..Default::default()
            })
        }
    }

    fn test_policy() -> SecurityPolicy {
        SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            allowed_commands: vec!["ls".to_string()],
            workspace_dir: PathBuf::from("/tmp"),
            blocked_paths: vec![],
            http_allowed_hosts: vec![],
            injection_check: true,
        }
    }

    #[tokio::test]
    async fn simple_text_response() {
        // Need 2 responses: 1 for Phase 1 routing, 1 for main conversation
        let provider = MockProvider::new(vec![
            ChatResponse {
                // Phase 1 routing response
                text: Some(r#"{"skills": [], "direct": true}"#.to_string()),
                reasoning_content: None,
                tool_calls: vec![],
            },
            ChatResponse {
                text: Some("你好！".to_string()),
                reasoning_content: None,
                tool_calls: vec![],
            },
        ]);

        let mut agent = Agent::new(
            Box::new(provider),
            vec![],
            Box::new(MockMemory),
            test_policy(),
            "test".to_string(),
            "http://test".to_string(),
            "test-model".to_string(),
            0.7,
            vec![],
            None,
        );

        let reply = agent.process_message("你好").await.unwrap();
        assert_eq!(reply, "你好！");
    }

    #[tokio::test]
    async fn tool_call_then_text() {
        let provider = MockProvider::new(vec![
            // Phase 1 routing response
            ChatResponse {
                text: Some(r#"{"skills": [], "direct": true}"#.to_string()),
                reasoning_content: None,
                tool_calls: vec![],
            },
            // Phase 2 first response: tool call
            ChatResponse {
                text: Some("让我查看一下".to_string()),
                reasoning_content: None,
                tool_calls: vec![ToolCall {
                    id: "call_1".to_string(),
                    name: "shell".to_string(),
                    arguments: serde_json::json!({"command": "ls"}),
                }],
            },
            // Phase 2 second response: final text
            ChatResponse {
                text: Some("目录中有 file.txt".to_string()),
                reasoning_content: None,
                tool_calls: vec![],
            },
        ]);

        let mock_tool = MockTool {
            tool_name: "shell".to_string(),
            result: "file.txt".to_string(),
        };

        let mut agent = Agent::new(
            Box::new(provider),
            vec![Box::new(mock_tool)],
            Box::new(MockMemory),
            test_policy(),
            "test".to_string(),
            "http://test".to_string(),
            "test-model".to_string(),
            0.7,
            vec![],
            None,
        );

        let reply = agent.process_message("列出文件").await.unwrap();
        assert_eq!(reply, "目录中有 file.txt");
    }

    #[tokio::test]
    async fn unknown_tool_handled() {
        let provider = MockProvider::new(vec![
            // Phase 1 routing response
            ChatResponse {
                text: Some(r#"{"skills": [], "direct": true}"#.to_string()),
                reasoning_content: None,
                tool_calls: vec![],
            },
            // Phase 2 first response: unknown tool call
            ChatResponse {
                text: None,
                reasoning_content: None,
                tool_calls: vec![ToolCall {
                    id: "call_1".to_string(),
                    name: "nonexistent".to_string(),
                    arguments: serde_json::json!({}),
                }],
            },
            // Phase 2 second response: final text
            ChatResponse {
                text: Some("抱歉".to_string()),
                reasoning_content: None,
                tool_calls: vec![],
            },
        ]);

        let mut agent = Agent::new(
            Box::new(provider),
            vec![],
            Box::new(MockMemory),
            test_policy(),
            "test".to_string(),
            "http://test".to_string(),
            "test-model".to_string(),
            0.7,
            vec![],
            None,
        );

        let reply = agent.process_message("test").await.unwrap();
        assert_eq!(reply, "抱歉");
    }

    #[test]
    fn system_prompt_contains_identity() {
        let agent = Agent::new(
            Box::new(MockProvider::new(vec![])),
            vec![],
            Box::new(MockMemory),
            test_policy(),
            "test".to_string(),
            "http://test".to_string(),
            "test".to_string(),
            0.7,
            vec![],
            None,
        );
        let prompt = agent.build_system_prompt(&[]);
        assert!(prompt.contains("RRClaw"));
    }

    #[test]
    fn system_prompt_injects_identity_context_before_rrclaw_description() {
        let identity = "### 用户偏好\n你是专属助手 Max，简洁直接".to_string();
        let agent = Agent::new(
            Box::new(MockProvider::new(vec![])),
            vec![],
            Box::new(MockMemory),
            test_policy(),
            "test".to_string(),
            "http://test".to_string(),
            "test".to_string(),
            0.7,
            vec![],
            Some(identity),
        );
        let prompt = agent.build_system_prompt(&[]);
        // identity 内容应出现在 prompt 中（tests use English, section label is "[Custom Context]"）
        assert!(prompt.contains("你是专属助手 Max"));
        assert!(prompt.contains("[Custom Context]"));
        // identity 段应在 RRClaw 身份描述之前
        let identity_pos = prompt.find("[Custom Context]").unwrap();
        let rrclaw_pos = prompt.find("RRClaw").unwrap();
        assert!(
            identity_pos < rrclaw_pos,
            "[Custom Context] should appear before RRClaw description, identity_pos={}, rrclaw_pos={}",
            identity_pos,
            rrclaw_pos
        );
    }

    #[test]
    fn system_prompt_no_identity_section_when_none() {
        let agent = Agent::new(
            Box::new(MockProvider::new(vec![])),
            vec![],
            Box::new(MockMemory),
            test_policy(),
            "test".to_string(),
            "http://test".to_string(),
            "test".to_string(),
            0.7,
            vec![],
            None,
        );
        let prompt = agent.build_system_prompt(&[]);
        // Neither English nor Chinese identity section should appear
        assert!(!prompt.contains("[Custom Context]"));
        assert!(!prompt.contains("[用户定制上下文]"));
    }

    #[test]
    fn system_prompt_includes_tools() {
        let tool = MockTool {
            tool_name: "shell".to_string(),
            result: String::new(),
        };
        let agent = Agent::new(
            Box::new(MockProvider::new(vec![])),
            vec![Box::new(tool)],
            Box::new(MockMemory),
            test_policy(),
            "test".to_string(),
            "http://test".to_string(),
            "test".to_string(),
            0.7,
            vec![],
            None,
        );
        let prompt = agent.build_system_prompt(&[]);
        assert!(prompt.contains("shell"));
    }

    #[test]
    fn system_prompt_has_decision_principles() {
        let agent = Agent::new(
            Box::new(MockProvider::new(vec![])),
            vec![],
            Box::new(MockMemory),
            test_policy(),
            "test".to_string(),
            "http://test".to_string(),
            "test".to_string(),
            0.7,
            vec![],
            None,
        );
        let prompt = agent.build_system_prompt(&[]);
        // Tests run in English mode — check English decision principle keywords
        assert!(prompt.contains("Check before acting"));
        assert!(prompt.contains("Ask when stuck"));
        assert!(prompt.contains("self_info"));
        // Legacy verbose sections should be absent
        assert!(!prompt.contains("[工具结果格式]"));
        assert!(!prompt.contains("[行为准则]"));
        assert!(!prompt.contains("Shell 命令白名单"));
    }

    #[test]
    fn system_prompt_is_lean() {
        let agent = Agent::new(
            Box::new(MockProvider::new(vec![])),
            vec![],
            Box::new(MockMemory),
            test_policy(),
            "test".to_string(),
            "http://test".to_string(),
            "test".to_string(),
            0.7,
            vec![],
            None,
        );
        let prompt = agent.build_system_prompt(&[]);
        // 精简后应明显短于旧版（旧版约 800+ 字符）
        // 精简后约 735 字符（旧版含白名单+工具格式+行为准则约 1200+ 字符）
        // 注：P4-memory-tools 添加了"善用记忆"原则后约 881 字符
        // 注：P5-http-tool 添加了"HTTP SSRF 防护"原则后约 1129 字符
        assert!(
            prompt.len() < 1200,
            "system prompt 应精简到 1200 字符以内，实际 {} 字符",
            prompt.len()
        );
    }

    // --- pre_select_tool 测试 ---

    #[test]
    fn pre_select_tool_routes_git_commands() {
        let agent = Agent::new(
            Box::new(MockProvider::new(vec![])),
            vec![Box::new(crate::tools::git::GitTool)],
            Box::new(MockMemory),
            test_policy(),
            "test".to_string(),
            "http://test".to_string(),
            "test".to_string(),
            0.7,
            vec![],
            None,
        );

        // 应该路由到 git 工具的场景
        assert_eq!(agent.pre_select_tool("git status"), Some("git"));
        assert_eq!(agent.pre_select_tool("git log"), Some("git"));
        assert_eq!(agent.pre_select_tool("git diff"), Some("git"));
        assert_eq!(agent.pre_select_tool("git add ."), Some("git"));
        assert_eq!(agent.pre_select_tool("git commit -m \"test\""), Some("git"));
        assert_eq!(agent.pre_select_tool("执行 git push"), Some("git"));
        assert_eq!(agent.pre_select_tool("git pull origin main"), Some("git"));
    }

    #[test]
    fn pre_select_tool_ignores_github() {
        let agent = Agent::new(
            Box::new(MockProvider::new(vec![])),
            vec![Box::new(crate::tools::git::GitTool)],
            Box::new(MockMemory),
            test_policy(),
            "test".to_string(),
            "http://test".to_string(),
            "test".to_string(),
            0.7,
            vec![],
            None,
        );

        // GitHub CLI 不应该触发路由
        assert_eq!(agent.pre_select_tool("gh pr status"), None);
        assert_eq!(agent.pre_select_tool("github 仓库"), None);
    }

    #[test]
    fn pre_select_tool_allows_llm_for_other() {
        let agent = Agent::new(
            Box::new(MockProvider::new(vec![])),
            vec![Box::new(crate::tools::shell::ShellTool)],
            Box::new(MockMemory),
            test_policy(),
            "test".to_string(),
            "http://test".to_string(),
            "test".to_string(),
            0.7,
            vec![],
            None,
        );

        // 普通命令让 LLM 自行选择
        assert_eq!(agent.pre_select_tool("列出当前目录"), None);
        assert_eq!(agent.pre_select_tool("读取文件 src/main.rs"), None);
    }

    #[tokio::test]
    async fn supervised_confirm_allows_execution() {
        let provider = MockProvider::new(vec![
            // Phase 1 routing response
            ChatResponse {
                text: Some(r#"{"skills": [], "direct": true}"#.to_string()),
                reasoning_content: None,
                tool_calls: vec![],
            },
            // Phase 2 first response: tool call
            ChatResponse {
                text: None,
                reasoning_content: None,
                tool_calls: vec![ToolCall {
                    id: "call_1".to_string(),
                    name: "shell".to_string(),
                    arguments: serde_json::json!({"command": "ls"}),
                }],
            },
            // Phase 2 second response: final text after tool execution
            ChatResponse {
                text: Some("执行完成".to_string()),
                reasoning_content: None,
                tool_calls: vec![],
            },
        ]);

        let mock_tool = MockTool {
            tool_name: "shell".to_string(),
            result: "file.txt".to_string(),
        };

        let mut policy = test_policy();
        policy.autonomy = AutonomyLevel::Supervised;

        let mut agent = Agent::new(
            Box::new(provider),
            vec![Box::new(mock_tool)],
            Box::new(MockMemory),
            policy,
            "test".to_string(),
            "http://test".to_string(),
            "test-model".to_string(),
            0.7,
            vec![],
            None,
        );

        // 确认回调: 始终允许
        agent.set_confirm_fn(Box::new(|_name, _args| true));

        let reply = agent.process_message("列出文件").await.unwrap();
        assert_eq!(reply, "执行完成");
    }

    #[tokio::test]
    async fn supervised_confirm_denies_execution() {
        let provider = MockProvider::new(vec![
            // Phase 1 routing response
            ChatResponse {
                text: Some(r#"{"skills": [], "direct": true}"#.to_string()),
                reasoning_content: None,
                tool_calls: vec![],
            },
            // Phase 2 first response: dangerous tool call
            ChatResponse {
                text: None,
                reasoning_content: None,
                tool_calls: vec![ToolCall {
                    id: "call_1".to_string(),
                    name: "shell".to_string(),
                    arguments: serde_json::json!({"command": "rm -rf /"}),
                }],
            },
            // Phase 2 second response: after tool was denied
            ChatResponse {
                text: Some("好的，已取消".to_string()),
                reasoning_content: None,
                tool_calls: vec![],
            },
        ]);

        let mock_tool = MockTool {
            tool_name: "shell".to_string(),
            result: "should not run".to_string(),
        };

        let mut policy = test_policy();
        policy.autonomy = AutonomyLevel::Supervised;

        let mut agent = Agent::new(
            Box::new(provider),
            vec![Box::new(mock_tool)],
            Box::new(MockMemory),
            policy,
            "test".to_string(),
            "http://test".to_string(),
            "test-model".to_string(),
            0.7,
            vec![],
            None,
        );

        // 确认回调: 始终拒绝
        agent.set_confirm_fn(Box::new(|_name, _args| false));

        let reply = agent.process_message("删除所有文件").await.unwrap();
        assert_eq!(reply, "好的，已取消");
    }

    #[tokio::test]
    async fn full_mode_skips_confirmation() {
        let provider = MockProvider::new(vec![
            // Phase 1 routing response
            ChatResponse {
                text: Some(r#"{"skills": [], "direct": true}"#.to_string()),
                reasoning_content: None,
                tool_calls: vec![],
            },
            // Phase 2 first response: tool call
            ChatResponse {
                text: None,
                reasoning_content: None,
                tool_calls: vec![ToolCall {
                    id: "call_1".to_string(),
                    name: "shell".to_string(),
                    arguments: serde_json::json!({"command": "ls"}),
                }],
            },
            // Phase 2 second response: final text (no confirm prompt in Full mode)
            ChatResponse {
                text: Some("完成".to_string()),
                reasoning_content: None,
                tool_calls: vec![],
            },
        ]);

        let mock_tool = MockTool {
            tool_name: "shell".to_string(),
            result: "file.txt".to_string(),
        };

        // Full 模式 — 不需要确认
        let mut agent = Agent::new(
            Box::new(provider),
            vec![Box::new(mock_tool)],
            Box::new(MockMemory),
            test_policy(), // Full mode
            "test".to_string(),
            "http://test".to_string(),
            "test-model".to_string(),
            0.7,
            vec![],
            None,
        );

        // 设置一个会 panic 的确认回调（不应被调用）
        agent.set_confirm_fn(Box::new(|_name, _args| {
            panic!("Full 模式不应调用确认回调");
        }));

        let reply = agent.process_message("列出文件").await.unwrap();
        assert_eq!(reply, "完成");
    }

    #[test]
    fn trim_history_works() {
        let mut agent = Agent::new(
            Box::new(MockProvider::new(vec![])),
            vec![],
            Box::new(MockMemory),
            test_policy(),
            "test".to_string(),
            "http://test".to_string(),
            "test".to_string(),
            0.7,
            vec![],
            None,
        );

        for i in 0..60 {
            agent.history.push(ConversationMessage::Chat(ChatMessage {
                role: "user".to_string(),
                content: format!("msg {}", i),
                reasoning_content: None,
            }));
        }
        assert_eq!(agent.history.len(), 60);
        agent.trim_history();
        assert_eq!(agent.history.len(), MAX_HISTORY_SIZE);
    }

    #[tokio::test]
    async fn reasoning_content_preserved_in_tool_call_loop() {
        // 模拟 DeepSeek Reasoner: 返回 reasoning_content + tool call，然后最终回复
        // Need 3 responses: 1 for Phase 1 routing, 2 for main conversation
        let provider = MockProvider::new(vec![
            ChatResponse {
                // Phase 1 routing response
                text: Some(r#"{"skills": [], "direct": true}"#.to_string()),
                reasoning_content: None,
                tool_calls: vec![],
            },
            ChatResponse {
                text: None,
                reasoning_content: Some("让我先查看文件列表".to_string()),
                tool_calls: vec![ToolCall {
                    id: "call_1".to_string(),
                    name: "shell".to_string(),
                    arguments: serde_json::json!({"command": "ls"}),
                }],
            },
            ChatResponse {
                text: Some("目录中有 file.txt".to_string()),
                reasoning_content: Some("好的，我看到了文件".to_string()),
                tool_calls: vec![],
            },
        ]);

        let mock_tool = MockTool {
            tool_name: "shell".to_string(),
            result: "file.txt".to_string(),
        };

        let mut agent = Agent::new(
            Box::new(provider),
            vec![Box::new(mock_tool)],
            Box::new(MockMemory),
            test_policy(),
            "test".to_string(),
            "http://test".to_string(),
            "deepseek-reasoner".to_string(),
            0.7,
            vec![],
            None,
        );

        let reply = agent.process_message("列出文件").await.unwrap();
        assert_eq!(reply, "目录中有 file.txt");

        // 验证 history 中 AssistantToolCalls 保留了 reasoning_content
        let has_reasoning = agent.history().iter().any(|msg| {
            matches!(msg, ConversationMessage::AssistantToolCalls { reasoning_content: Some(rc), .. } if !rc.is_empty())
        });
        assert!(has_reasoning, "AssistantToolCalls 应保留 reasoning_content");

        // 验证最终 assistant 消息也保留了 reasoning_content
        let last = agent.history().last().unwrap();
        if let ConversationMessage::Chat(cm) = last {
            assert_eq!(cm.reasoning_content.as_deref(), Some("好的，我看到了文件"));
        } else {
            panic!("最后一条消息应该是 Chat");
        }
    }

    #[tokio::test]
    async fn clear_old_reasoning_on_new_turn() {
        // 先运行一轮带 reasoning_content 的对话
        // Need 4 responses: 2 for first round (routing + main), 2 for second round
        let provider = MockProvider::new(vec![
            // First round: routing
            ChatResponse {
                text: Some(r#"{"skills": [], "direct": true}"#.to_string()),
                reasoning_content: None,
                tool_calls: vec![],
            },
            // First round: main
            ChatResponse {
                text: Some("你好！".to_string()),
                reasoning_content: Some("用户打招呼".to_string()),
                tool_calls: vec![],
            },
            // Second round: routing
            ChatResponse {
                text: Some(r#"{"skills": [], "direct": true}"#.to_string()),
                reasoning_content: None,
                tool_calls: vec![],
            },
            // Second round: main
            ChatResponse {
                text: Some("再见！".to_string()),
                reasoning_content: None,
                tool_calls: vec![],
            },
        ]);

        let mut agent = Agent::new(
            Box::new(provider),
            vec![],
            Box::new(MockMemory),
            test_policy(),
            "test".to_string(),
            "http://test".to_string(),
            "test-model".to_string(),
            0.7,
            vec![],
            None,
        );

        // 第一轮
        agent.process_message("你好").await.unwrap();
        // 验证第一轮 assistant 有 reasoning_content
        let first_assistant = agent.history().iter().find(|msg| {
            matches!(msg, ConversationMessage::Chat(cm) if cm.role == "assistant" && cm.reasoning_content.is_some())
        });
        assert!(first_assistant.is_some(), "第一轮应有 reasoning_content");

        // 第二轮 — 新 Turn 开始时应清空旧 reasoning_content
        agent.process_message("再见").await.unwrap();
        // 验证旧的 reasoning_content 已被清空（第一轮的 assistant 消息）
        let old_reasoning = agent.history().iter().any(|msg| {
            match msg {
                ConversationMessage::Chat(cm) if cm.role == "assistant" => {
                    // 只有最后一条 assistant 消息可能有（但这轮没有），其余应被清空
                    cm.reasoning_content.as_deref() == Some("用户打招呼")
                }
                _ => false,
            }
        });
        assert!(!old_reasoning, "旧 Turn 的 reasoning_content 应被清空");
    }

    // --- Phase 1 路由测试 ---

    #[test]
    fn parse_route_result_skills() {
        let result = parse_route_result(r#"{"skills": ["git-commit"], "direct": false}"#);
        assert!(matches!(result, RouteResult::Skills(s) if s == ["git-commit"]));
    }

    #[test]
    fn parse_route_result_direct() {
        let result = parse_route_result(r#"{"skills": [], "direct": true}"#);
        assert!(matches!(result, RouteResult::Direct));
    }

    #[test]
    fn parse_route_result_clarification() {
        let result = parse_route_result(
            r#"{"skills": [], "direct": false, "question": "你是想查看还是提交？"}"#
        );
        assert!(matches!(result, RouteResult::NeedClarification(q) if q.contains("查看")));
    }

    #[test]
    fn parse_route_result_fallback_on_invalid_json() {
        // 解析失败时降级为 Direct
        let result = parse_route_result("这不是 JSON");
        assert!(matches!(result, RouteResult::Direct));
    }

    #[test]
    fn parse_route_result_strips_markdown_code_block() {
        let result = parse_route_result("```json\n{\"skills\": [], \"direct\": true}\n```");
        assert!(matches!(result, RouteResult::Direct));
    }

    #[test]
    fn parse_route_result_multiple_skills() {
        let result = parse_route_result(
            r#"{"skills": ["git-commit", "code-review"], "direct": false}"#
        );
        match result {
            RouteResult::Skills(s) => assert_eq!(s.len(), 2),
            _ => panic!("expected Skills"),
        }
    }

    #[test]
    fn build_routing_prompt_no_tools() {
        let skills = vec![];
        // English version
        let prompt = build_routing_prompt(&skills, crate::i18n::Language::English);
        assert!(!prompt.contains("shell"));
        assert!(!prompt.contains("file_read"));
        assert!(prompt.contains("JSON"));
        // Chinese version
        let prompt_zh = build_routing_prompt(&skills, crate::i18n::Language::Chinese);
        assert!(prompt_zh.contains("JSON"));
    }

    #[test]
    fn build_routing_prompt_contains_skill_names() {
        let skills = vec![SkillMeta {
            name: "git-commit".to_string(),
            description: "Git commit workflow".to_string(),
            tags: vec![],
            source: SkillSource::BuiltIn,
            path: None,
        }];
        // English
        let prompt = build_routing_prompt(&skills, crate::i18n::Language::English);
        assert!(prompt.contains("git-commit"));
        assert!(prompt.contains("Git commit workflow"));
        // Chinese
        let prompt_zh = build_routing_prompt(&skills, crate::i18n::Language::Chinese);
        assert!(prompt_zh.contains("git-commit"));
    }

    #[test]
    fn build_routing_prompt_empty_skills() {
        let skills = vec![];
        // English: "No skills available"
        let prompt = build_routing_prompt(&skills, crate::i18n::Language::English);
        assert!(prompt.contains("No skills available"));
        // Chinese: "暂无可用 skill"
        let prompt_zh = build_routing_prompt(&skills, crate::i18n::Language::Chinese);
        assert!(prompt_zh.contains("暂无可用 skill"));
    }

    #[test]
    fn extract_json_strips_markdown() {
        let text = "```json\n{\"direct\": true}\n```";
        let json = extract_json(text);
        assert!(json.contains("direct"));
    }

    #[test]
    fn extract_json_handles_plain_json() {
        let text = r#"{"direct": true}"#;
        let json = extract_json(text);
        assert!(json.contains("direct"));
    }

    // --- History Compaction Tests ---

    fn make_chat(role: &str, content: &str) -> ConversationMessage {
        ConversationMessage::Chat(ChatMessage {
            role: role.to_string(),
            content: content.to_string(),
            reasoning_content: None,
        })
    }

    fn fill_history(agent: &mut Agent, count: usize) {
        for i in 0..count {
            agent.history.push(make_chat("user", &format!("消息 {}", i)));
            agent.history.push(make_chat("assistant", &format!("回复 {}", i)));
        }
    }

    #[tokio::test]
    async fn no_compaction_below_threshold() {
        // history < 40，不触发压缩
        let provider = MockProvider::new(vec![]);
        let mut agent = Agent::new(
            Box::new(provider),
            vec![],
            Box::new(MockMemory),
            test_policy(),
            "test".to_string(), "http://test".to_string(),
            "test-model".to_string(), 0.7, vec![], None,
        );
        fill_history(&mut agent, 19); // 38 条
        let original_len = agent.history.len();
        agent.compact_history_if_needed().await;
        assert_eq!(agent.history.len(), original_len); // 未变化
    }

    #[tokio::test]
    async fn compaction_triggers_at_threshold() {
        // history = 40，触发压缩，LLM 返回摘要
        let summary_response = ChatResponse {
            text: Some("对话摘要：用户询问了多个问题，助手逐一回答。".to_string()),
            reasoning_content: None,
            tool_calls: vec![],
        };
        let provider = MockProvider::new(vec![summary_response]);
        let mut agent = Agent::new(
            Box::new(provider),
            vec![],
            Box::new(MockMemory),
            test_policy(),
            "test".to_string(), "http://test".to_string(),
            "test-model".to_string(), 0.7, vec![], None,
        );
        fill_history(&mut agent, 20); // 40 条
        agent.compact_history_if_needed().await;
        // 压缩后 history 应该明显少于 40
        assert!(agent.history.len() < 40);
        // 第一条应该是摘要消息
        if let ConversationMessage::Chat(cm) = &agent.history[0] {
            assert!(cm.content.contains("对话摘要"));
        } else {
            panic!("第一条应该是摘要 Chat 消息");
        }
    }

    #[tokio::test]
    async fn compaction_fallback_to_trim_on_llm_failure() {
        // LLM 返回空响应 → 触发 fallback trim_history
        let empty_response = ChatResponse {
            text: None,  // 空响应触发 summarize_history 报错
            reasoning_content: None,
            tool_calls: vec![],
        };
        let provider = MockProvider::new(vec![empty_response]);
        let mut agent = Agent::new(
            Box::new(provider),
            vec![],
            Box::new(MockMemory),
            test_policy(),
            "test".to_string(), "http://test".to_string(),
            "test-model".to_string(), 0.7, vec![], None,
        );
        fill_history(&mut agent, 25); // 50 条
        agent.compact_history_if_needed().await;
        // fallback trim_history 应将 history 裁到 50 条内
        assert!(agent.history.len() <= MAX_HISTORY_SIZE);
    }

    #[tokio::test]
    async fn compaction_preserves_recent_messages() {
        // 压缩后，最近 10 条消息应保留
        let summary_response = ChatResponse {
            text: Some("对话摘要：早期上下文。".to_string()),
            reasoning_content: None,
            tool_calls: vec![],
        };
        let provider = MockProvider::new(vec![summary_response]);
        let mut agent = Agent::new(
            Box::new(provider),
            vec![],
            Box::new(MockMemory),
            test_policy(),
            "test".to_string(), "http://test".to_string(),
            "test-model".to_string(), 0.7, vec![], None,
        );
        fill_history(&mut agent, 20); // 40 条
        // 记录最后 10 条内容
        let last_10: Vec<String> = agent.history[30..].iter().map(|m| {
            if let ConversationMessage::Chat(cm) = m { cm.content.clone() } else { String::new() }
        }).collect();

        agent.compact_history_if_needed().await;

        // 最近 10 条应在压缩后 history 的末尾
        let after_len = agent.history.len();
        let recent: Vec<String> = agent.history[(after_len - 10)..].iter().map(|m| {
            if let ConversationMessage::Chat(cm) = m { cm.content.clone() } else { String::new() }
        }).collect();
        assert_eq!(last_10, recent);
    }

    // --- find_safe_window_end 测试 ---

    #[test]
    fn safe_window_end_stops_before_tool_result() {
        let history = vec![
            make_chat("user", "请执行命令"),
            make_chat("assistant", "好的"),
            ConversationMessage::AssistantToolCalls {
                text: None,
                reasoning_content: None,
                tool_calls: vec![ToolCall { id: "1".into(), name: "shell".into(), arguments: serde_json::json!({}) }],
            },
            ConversationMessage::ToolResult { tool_call_id: "1".into(), content: "结果".into() },
            make_chat("user", "谢谢"),
        ];
        // ideal_end=3 时应退到 2（Chat 消息后），不截断 ToolCalls+ToolResult 对
        let end = find_safe_window_end(&history, 3);
        assert!(end <= 2);
    }

    #[test]
    fn safe_window_end_all_chat_messages() {
        let history: Vec<ConversationMessage> = (0..5)
            .map(|i| make_chat("user", &format!("msg {}", i)))
            .collect();
        assert_eq!(find_safe_window_end(&history, 3), 3);
    }

    // --- format_history_for_summary 测试 ---

    #[test]
    fn format_skips_system_messages() {
        let messages = vec![
            make_chat("system", "系统 prompt"),
            make_chat("user", "你好"),
            make_chat("assistant", "你好！"),
        ];
        let output = format_history_for_summary(&messages);
        assert!(!output.contains("系统 prompt"));
        assert!(output.contains("你好"));
        assert!(output.contains("助手"));
    }

    #[test]
    fn format_truncates_long_content() {
        let long_content = "X".repeat(1000);
        let messages = vec![make_chat("user", &long_content)];
        let output = format_history_for_summary(&messages);
        assert!(output.len() < 600); // 500字符截断 + 标签
    }

    #[test]
    fn format_includes_tool_call_names() {
        let messages = vec![
            ConversationMessage::AssistantToolCalls {
                text: Some("我来执行".to_string()),
                reasoning_content: None,
                tool_calls: vec![
                    ToolCall { id: "1".into(), name: "shell".into(), arguments: serde_json::json!({}) },
                ],
            },
            ConversationMessage::ToolResult { tool_call_id: "1".into(), content: "output".into() },
        ];
        let output = format_history_for_summary(&messages);
        assert!(output.contains("shell"));
        assert!(output.contains("工具调用"));
    }

    // --- routine_name / build_system_prompt 测试 ---

    fn make_agent_no_skills() -> Agent {
        Agent::new(
            Box::new(MockProvider::new(vec![])),
            vec![],
            Box::new(MockMemory),
            test_policy(),
            "test".into(),
            "http://test".into(),
            "model".into(),
            0.7,
            vec![],
            None,
        )
    }

    #[test]
    fn routine_system_prompt_injected_when_routine_name_set() {
        let mut agent = make_agent_no_skills();
        agent.set_routine_name("tesla_stock_monitor".to_string());
        let prompt = agent.build_system_prompt(&[]);
        // Tests run in English mode
        assert!(prompt.contains("[Routine Execution Rules]"), "should contain routine rules section");
        assert!(prompt.contains("tesla_stock_monitor"), "should contain routine name");
        assert!(prompt.contains("memory_store"), "should contain memory_store instruction");
        assert!(prompt.contains("routine:tesla_stock_monitor:approach"), "should contain correct key");
    }

    #[test]
    fn routine_system_prompt_absent_in_normal_mode() {
        let agent = make_agent_no_skills();
        let prompt = agent.build_system_prompt(&[]);
        assert!(!prompt.contains("[Routine Execution Rules]"), "normal mode should not contain routine rules");
        assert!(!prompt.contains("[Routine 执行规范]"), "normal mode should not contain routine rules (zh)");
    }

    #[test]
    fn set_routine_name_overwrites_previous() {
        let mut agent = make_agent_no_skills();
        agent.set_routine_name("first".to_string());
        agent.set_routine_name("second".to_string());
        let prompt = agent.build_system_prompt(&[]);
        assert!(prompt.contains("second"), "should contain current routine name");
        // Check the routine name specifically in context, not as any substring
        assert!(!prompt.contains("task 'first'"), "should not contain old routine name in task context");
    }

    // --- summarize_history 测试 ---

    #[tokio::test]
    async fn summarize_returns_llm_text() {
        let provider = MockProvider::new(vec![ChatResponse {
            text: Some("对话摘要：用户询问了一些问题。".to_string()),
            reasoning_content: None,
            tool_calls: vec![],
        }]);
        let agent = Agent::new(
            Box::new(provider), vec![], Box::new(MockMemory),
            test_policy(), "t".into(), "h".into(), "m".into(), 0.7, vec![], None,
        );
        let messages = vec![make_chat("user", "你好")];
        let result = agent.summarize_history(&messages).await.unwrap();
        assert!(result.contains("摘要"));
    }

    // --- P7-3: 有 required 字段的 Mock 工具 ---

    struct StrictMockTool {
        tool_name: String,
        result: String,
    }

    #[async_trait::async_trait]
    impl Tool for StrictMockTool {
        fn name(&self) -> &str {
            &self.tool_name
        }
        fn description(&self) -> &str {
            "Strict tool requiring a query parameter"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query"
                    }
                }
            })
        }
        async fn execute(
            &self,
            _args: serde_json::Value,
            _policy: &SecurityPolicy,
        ) -> Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: self.result.clone(),
                error: None,
                ..Default::default()
            })
        }
    }

    // --- P7-3: find_missing_required_params 单元测试 ---

    #[test]
    fn find_missing_required_params_no_required() {
        let schema = serde_json::json!({"type": "object", "properties": {}});
        let args = serde_json::json!({});
        let missing = find_missing_required_params(&schema, &args);
        assert!(missing.is_empty(), "无 required 字段时应返回空");
    }

    #[test]
    fn find_missing_required_params_all_present() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["path", "mode"],
            "properties": {}
        });
        let args = serde_json::json!({"path": "/foo", "mode": "r"});
        let missing = find_missing_required_params(&schema, &args);
        assert!(missing.is_empty(), "所有必填参数都存在时应返回空");
    }

    #[test]
    fn find_missing_required_params_some_missing() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["path", "mode"],
            "properties": {}
        });
        let args = serde_json::json!({"path": "/foo"});
        let missing = find_missing_required_params(&schema, &args);
        assert_eq!(missing, vec!["mode".to_string()], "应返回缺失的参数名");
    }

    #[test]
    fn find_missing_required_params_all_missing() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["a", "b"],
            "properties": {}
        });
        let args = serde_json::json!({});
        let mut missing = find_missing_required_params(&schema, &args);
        missing.sort();
        assert_eq!(missing, vec!["a".to_string(), "b".to_string()], "应返回所有缺失参数");
    }

    // --- P7-3: schema 动态扩展集成测试 ---

    #[tokio::test]
    async fn schema_expansion_triggered_on_missing_params() {
        // LLM 调用 strict_tool 时缺少 "query" → P7-3 注入 schema 提示
        // LLM 再次调用并提供正确参数 → 执行成功
        let provider = MockProvider::new(vec![
            // Phase 1 routing
            ChatResponse {
                text: Some(r#"{"skills": [], "direct": true}"#.to_string()),
                reasoning_content: None,
                tool_calls: vec![],
            },
            // Phase 2 iter 1: 缺少 "query"
            ChatResponse {
                text: None,
                reasoning_content: None,
                tool_calls: vec![ToolCall {
                    id: "call_1".to_string(),
                    name: "strict_tool".to_string(),
                    arguments: serde_json::json!({}),
                }],
            },
            // Phase 2 iter 2: 提供正确参数（看到 schema 提示后）
            ChatResponse {
                text: None,
                reasoning_content: None,
                tool_calls: vec![ToolCall {
                    id: "call_2".to_string(),
                    name: "strict_tool".to_string(),
                    arguments: serde_json::json!({"query": "hello"}),
                }],
            },
            // Phase 2 iter 3: 最终回复
            ChatResponse {
                text: Some("搜索完成".to_string()),
                reasoning_content: None,
                tool_calls: vec![],
            },
        ]);

        let mut agent = Agent::new(
            Box::new(provider),
            vec![Box::new(StrictMockTool {
                tool_name: "strict_tool".to_string(),
                result: "搜索结果".to_string(),
            })],
            Box::new(MockMemory),
            test_policy(),
            "test".to_string(),
            "http://test".to_string(),
            "test-model".to_string(),
            0.7,
            vec![],
            None,
        );

        let reply = agent.process_message("搜索 hello").await.unwrap();
        assert_eq!(reply, "搜索完成");

        // history 中应包含 P7-3 参数缺失提示
        let has_hint = agent.history().iter().any(|msg| {
            matches!(msg, ConversationMessage::ToolResult { content, .. } if content.contains("[参数缺失]"))
        });
        assert!(has_hint, "P7-3 应在 history 中留下参数缺失提示");

        // expanded_tools 应包含 strict_tool
        assert!(
            agent.expanded_tools.contains("strict_tool"),
            "expanded_tools 应包含 strict_tool"
        );
    }

    #[tokio::test]
    async fn schema_expansion_not_triggered_when_params_present() {
        // LLM 一开始就提供完整参数 → 不触发 P7-3
        let provider = MockProvider::new(vec![
            // Phase 1 routing
            ChatResponse {
                text: Some(r#"{"skills": [], "direct": true}"#.to_string()),
                reasoning_content: None,
                tool_calls: vec![],
            },
            // Phase 2 iter 1: 参数完整
            ChatResponse {
                text: None,
                reasoning_content: None,
                tool_calls: vec![ToolCall {
                    id: "call_1".to_string(),
                    name: "strict_tool".to_string(),
                    arguments: serde_json::json!({"query": "test"}),
                }],
            },
            // Phase 2 iter 2: 最终回复
            ChatResponse {
                text: Some("正常完成".to_string()),
                reasoning_content: None,
                tool_calls: vec![],
            },
        ]);

        let mut agent = Agent::new(
            Box::new(provider),
            vec![Box::new(StrictMockTool {
                tool_name: "strict_tool".to_string(),
                result: "result".to_string(),
            })],
            Box::new(MockMemory),
            test_policy(),
            "test".to_string(),
            "http://test".to_string(),
            "test-model".to_string(),
            0.7,
            vec![],
            None,
        );

        let reply = agent.process_message("搜索").await.unwrap();
        assert_eq!(reply, "正常完成");

        // 不应有参数缺失提示
        let has_hint = agent.history().iter().any(|msg| {
            matches!(msg, ConversationMessage::ToolResult { content, .. } if content.contains("[参数缺失]"))
        });
        assert!(!has_hint, "参数完整时不应触发 P7-3");

        // expanded_tools 应为空
        assert!(agent.expanded_tools.is_empty(), "expanded_tools 应为空");
    }

    #[tokio::test]
    async fn schema_expansion_only_once_per_tool_per_turn() {
        // 同一工具缺参数两次 → P7-3 只触发一次（第二次直接执行，避免死循环）
        let provider = MockProvider::new(vec![
            // Phase 1 routing
            ChatResponse {
                text: Some(r#"{"skills": [], "direct": true}"#.to_string()),
                reasoning_content: None,
                tool_calls: vec![],
            },
            // Phase 2 iter 1: 缺参数 → P7-3 触发
            ChatResponse {
                text: None,
                reasoning_content: None,
                tool_calls: vec![ToolCall {
                    id: "call_1".to_string(),
                    name: "strict_tool".to_string(),
                    arguments: serde_json::json!({}),
                }],
            },
            // Phase 2 iter 2: 仍缺参数 → P7-3 不再触发（已在 expanded_tools），直接执行
            ChatResponse {
                text: None,
                reasoning_content: None,
                tool_calls: vec![ToolCall {
                    id: "call_2".to_string(),
                    name: "strict_tool".to_string(),
                    arguments: serde_json::json!({}),
                }],
            },
            // Phase 2 iter 3: 最终回复
            ChatResponse {
                text: Some("完成".to_string()),
                reasoning_content: None,
                tool_calls: vec![],
            },
        ]);

        let mut agent = Agent::new(
            Box::new(provider),
            vec![Box::new(StrictMockTool {
                tool_name: "strict_tool".to_string(),
                result: "ok".to_string(),
            })],
            Box::new(MockMemory),
            test_policy(),
            "test".to_string(),
            "http://test".to_string(),
            "test-model".to_string(),
            0.7,
            vec![],
            None,
        );

        let reply = agent.process_message("test").await.unwrap();
        assert_eq!(reply, "完成");

        // 参数缺失提示应恰好出现 1 次
        let hint_count = agent.history().iter().filter(|msg| {
            matches!(msg, ConversationMessage::ToolResult { content, .. } if content.contains("[参数缺失]"))
        }).count();
        assert_eq!(hint_count, 1, "P7-3 每工具每轮只触发一次");
    }
}
