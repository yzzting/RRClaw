# P4-C: ReliableProvider 实现计划

## 背景

当前 Agent 对 Provider 的调用没有任何容错机制：一次超时或 API 报错就会中断整个对话。需要：
1. **重试 + 指数退避**：网络抖动自动重试
2. **Fallback Chain**：主 Provider 持续失败时切换备用 Provider

使用**装饰器模式**实现，不修改任何现有 Provider 代码。

---

## 一、架构设计

```
Agent
  └── Box<dyn Provider>
        └── ReliableProvider (新增，包装层)
              ├── inner: Box<dyn Provider>      // 主 Provider
              ├── fallbacks: Vec<Box<dyn Provider>> // 备用链
              └── config: RetryConfig
```

`ReliableProvider` 实现 `Provider` trait，调用时先重试 `inner`，全部失败后依次尝试 `fallbacks`。

---

## 二、数据结构与实现

### 2.1 新增文件

```rust
// src/providers/reliable.rs
use async_trait::async_trait;
use color_eyre::eyre::Result;
use tokio::time::{sleep, Duration};
use tracing::{debug, warn};

use super::traits::{ChatResponse, ConversationMessage, Provider, StreamEvent, ToolSpec};

/// 重试配置
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// 最大重试次数（不含首次尝试）
    pub max_retries: usize,
    /// 初始退避时间（毫秒）
    pub initial_backoff_ms: u64,
    /// 退避乘数（每次失败后乘以该值）
    pub backoff_multiplier: f64,
    /// 最大退避时间上限（毫秒）
    pub max_backoff_ms: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_backoff_ms: 500,
            backoff_multiplier: 2.0,
            max_backoff_ms: 10_000,
        }
    }
}

/// 可靠 Provider 包装层：自动重试 + Fallback Chain
pub struct ReliableProvider {
    /// 主 Provider
    inner: Box<dyn Provider>,
    /// 备用 Provider 链（按顺序尝试）
    fallbacks: Vec<Box<dyn Provider>>,
    /// 重试配置
    config: RetryConfig,
}

impl ReliableProvider {
    /// 创建只有重试的包装（无 fallback）
    pub fn new(inner: Box<dyn Provider>, config: RetryConfig) -> Self {
        Self {
            inner,
            fallbacks: vec![],
            config,
        }
    }

    /// 创建带 fallback chain 的包装
    pub fn with_fallbacks(
        inner: Box<dyn Provider>,
        fallbacks: Vec<Box<dyn Provider>>,
        config: RetryConfig,
    ) -> Self {
        Self { inner, fallbacks, config }
    }
}

#[async_trait]
impl Provider for ReliableProvider {
    async fn chat_with_tools(
        &self,
        messages: &[ConversationMessage],
        tools: &[ToolSpec],
        model: &str,
        temperature: f64,
    ) -> Result<ChatResponse> {
        // 先重试主 Provider
        match retry_with_backoff(
            &*self.inner,
            messages,
            tools,
            model,
            temperature,
            &self.config,
            false,
            None,
        ).await {
            Ok(resp) => return Ok(resp),
            Err(e) => {
                warn!("主 Provider 全部重试失败: {:#}", e);
            }
        }

        // 依次尝试 fallback
        for (i, fallback) in self.fallbacks.iter().enumerate() {
            warn!("尝试 Fallback Provider #{}", i + 1);
            match retry_with_backoff(
                &**fallback,
                messages,
                tools,
                model,
                temperature,
                &self.config,
                false,
                None,
            ).await {
                Ok(resp) => return Ok(resp),
                Err(e) => warn!("Fallback #{} 失败: {:#}", i + 1, e),
            }
        }

        color_eyre::eyre::bail!(
            "所有 Provider 均失败（主 Provider + {} 个 fallback）",
            self.fallbacks.len()
        )
    }

    async fn chat_stream(
        &self,
        messages: &[ConversationMessage],
        tools: &[ToolSpec],
        model: &str,
        temperature: f64,
        tx: tokio::sync::mpsc::Sender<StreamEvent>,
    ) -> Result<ChatResponse> {
        // 流式模式：先尝试主 Provider 重试
        match retry_with_backoff(
            &*self.inner,
            messages,
            tools,
            model,
            temperature,
            &self.config,
            true,
            Some(tx.clone()),
        ).await {
            Ok(resp) => return Ok(resp),
            Err(e) => warn!("主 Provider 流式重试全部失败: {:#}", e),
        }

        // Fallback 链（流式）
        for (i, fallback) in self.fallbacks.iter().enumerate() {
            warn!("流式: 尝试 Fallback Provider #{}", i + 1);
            match retry_with_backoff(
                &**fallback,
                messages,
                tools,
                model,
                temperature,
                &self.config,
                true,
                Some(tx.clone()),
            ).await {
                Ok(resp) => return Ok(resp),
                Err(e) => warn!("流式 Fallback #{} 失败: {:#}", i + 1, e),
            }
        }

        color_eyre::eyre::bail!(
            "流式: 所有 Provider 均失败（主 Provider + {} 个 fallback）",
            self.fallbacks.len()
        )
    }
}

/// 对单个 Provider 执行重试逻辑（含指数退避）
async fn retry_with_backoff(
    provider: &dyn Provider,
    messages: &[ConversationMessage],
    tools: &[ToolSpec],
    model: &str,
    temperature: f64,
    config: &RetryConfig,
    is_stream: bool,
    tx: Option<tokio::sync::mpsc::Sender<StreamEvent>>,
) -> Result<ChatResponse> {
    let mut backoff_ms = config.initial_backoff_ms;

    for attempt in 0..=config.max_retries {
        let result = if is_stream {
            if let Some(tx) = &tx {
                provider
                    .chat_stream(messages, tools, model, temperature, tx.clone())
                    .await
            } else {
                provider.chat_with_tools(messages, tools, model, temperature).await
            }
        } else {
            provider.chat_with_tools(messages, tools, model, temperature).await
        };

        match result {
            Ok(resp) => {
                if attempt > 0 {
                    debug!("重试成功（第 {} 次尝试）", attempt + 1);
                }
                return Ok(resp);
            }
            Err(e) => {
                if attempt == config.max_retries {
                    // 最后一次尝试也失败了
                    return Err(e);
                }

                // 判断是否是可重试的错误
                let err_str = format!("{:#}", e);
                if !is_retryable(&err_str) {
                    warn!("不可重试的错误，停止: {}", err_str);
                    return Err(e);
                }

                warn!(
                    "第 {} 次尝试失败，{} ms 后重试: {}",
                    attempt + 1,
                    backoff_ms,
                    truncate_error(&err_str)
                );
                sleep(Duration::from_millis(backoff_ms)).await;

                // 指数退避，不超过上限
                backoff_ms = ((backoff_ms as f64) * config.backoff_multiplier) as u64;
                backoff_ms = backoff_ms.min(config.max_backoff_ms);
            }
        }
    }

    unreachable!()
}

/// 判断错误是否可重试
/// 可重试：超时、网络连接失败、5xx 服务端错误、速率限制(429)
/// 不可重试：4xx 客户端错误（除 429）、认证失败等
fn is_retryable(err_str: &str) -> bool {
    // 明确不可重试的错误
    let non_retryable = [
        "401", "403",      // 认证/权限错误
        "400",             // 请求参数错误
        "404",             // 端点不存在
        "invalid_api_key", // API key 无效
        "authentication",  // 认证失败
    ];
    for keyword in &non_retryable {
        if err_str.to_lowercase().contains(keyword) {
            return false;
        }
    }
    true  // 默认可重试（超时、网络、5xx、429 等）
}

/// 截断错误信息用于日志
fn truncate_error(s: &str) -> &str {
    &s[..s.len().min(150)]
}
```

### 2.2 Config 扩展（可选）

如果需要从 config.toml 配置 retry 参数，可在 `src/config/schema.rs` 添加：

```rust
/// 可靠性配置（可选，有默认值）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReliabilityConfig {
    /// 最大重试次数，默认 3
    #[serde(default = "default_max_retries")]
    pub max_retries: usize,
    /// 初始退避毫秒，默认 500
    #[serde(default = "default_initial_backoff_ms")]
    pub initial_backoff_ms: u64,
    /// Fallback provider 名称列表（按顺序）
    #[serde(default)]
    pub fallback_providers: Vec<String>,
}

fn default_max_retries() -> usize { 3 }
fn default_initial_backoff_ms() -> u64 { 500 }

impl Default for ReliabilityConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_backoff_ms: 500,
            fallback_providers: vec![],
        }
    }
}

// Config 中新增：
pub struct Config {
    // ...existing fields...
    #[serde(default)]
    pub reliability: ReliabilityConfig,
}
```

TOML 示例：
```toml
[reliability]
max_retries = 3
initial_backoff_ms = 500
fallback_providers = ["glm", "minimax"]  # deepseek 失败时按顺序切换
```

### 2.3 main.rs 集成

```rust
// src/main.rs — run_agent() 中，创建 provider 后包装

use rrclaw::providers::reliable::{ReliableProvider, RetryConfig};

// 创建主 Provider
let main_provider = rrclaw::providers::create_provider(&provider_config);

// 创建 fallback providers（如果配置了）
let fallback_providers: Vec<Box<dyn Provider>> = config
    .reliability
    .fallback_providers
    .iter()
    .filter_map(|name| config.providers.get(name))
    .map(|pc| rrclaw::providers::create_provider(pc))
    .collect();

// 包装为 ReliableProvider
let retry_config = RetryConfig {
    max_retries: config.reliability.max_retries,
    initial_backoff_ms: config.reliability.initial_backoff_ms,
    ..Default::default()
};

let provider: Box<dyn Provider> = if fallback_providers.is_empty() {
    Box::new(ReliableProvider::new(main_provider, retry_config))
} else {
    Box::new(ReliableProvider::with_fallbacks(main_provider, fallback_providers, retry_config))
};

// 之后正常传给 Agent::new(provider, ...)
```

---

## 三、模块注册

```rust
// src/providers/mod.rs 新增
pub mod reliable;

pub use reliable::{ReliableProvider, RetryConfig};
```

---

## 四、改动范围

| 文件 | 改动 | 复杂度 |
|------|------|--------|
| `src/providers/reliable.rs` | **新增** — ReliableProvider + RetryConfig + retry_with_backoff | 中 |
| `src/providers/mod.rs` | 添加 `pub mod reliable;` + re-export | 低 |
| `src/config/schema.rs` | 新增 `ReliabilityConfig`（可选），Config 新增字段 | 低 |
| `src/main.rs` | 用 ReliableProvider 包装主 provider | 低 |

**不需要改动**：Agent、Tool、Memory、Security、CLI、现有 Provider 实现。

---

## 五、提交策略

| # | 提交 | 说明 |
|---|------|------|
| 1 | `feat: add ReliableProvider with retry and exponential backoff` | reliable.rs + providers/mod.rs |
| 2 | `feat: add ReliabilityConfig to config schema` | config/schema.rs |
| 3 | `feat: wrap provider with ReliableProvider in main` | main.rs 集成 |
| 4 | `test: add ReliableProvider unit tests` | 所有测试 |

---

## 六、测试用例（~12 个）

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{ChatResponse, ConversationMessage};
    use std::sync::{Arc, Mutex};

    struct FlakyProvider {
        /// 失败次数计数，每次调用减 1；归零后返回成功
        fail_count: Arc<Mutex<usize>>,
        success_response: ChatResponse,
    }

    impl FlakyProvider {
        fn new(failures: usize) -> Self {
            Self {
                fail_count: Arc::new(Mutex::new(failures)),
                success_response: ChatResponse {
                    text: Some("成功".to_string()),
                    reasoning_content: None,
                    tool_calls: vec![],
                },
            }
        }
    }

    #[async_trait::async_trait]
    impl Provider for FlakyProvider {
        async fn chat_with_tools(&self, _m: &[ConversationMessage], _t: &[ToolSpec], _mo: &str, _te: f64) -> Result<ChatResponse> {
            let mut count = self.fail_count.lock().unwrap();
            if *count > 0 {
                *count -= 1;
                color_eyre::eyre::bail!("模拟超时错误 (还剩 {} 次)", *count)
            }
            Ok(self.success_response.clone())
        }
    }

    struct AlwaysFailProvider;

    #[async_trait::async_trait]
    impl Provider for AlwaysFailProvider {
        async fn chat_with_tools(&self, _m: &[ConversationMessage], _t: &[ToolSpec], _mo: &str, _te: f64) -> Result<ChatResponse> {
            color_eyre::eyre::bail!("始终失败")
        }
    }

    struct AlwaysSucceedProvider {
        label: String,
    }

    #[async_trait::async_trait]
    impl Provider for AlwaysSucceedProvider {
        async fn chat_with_tools(&self, _m: &[ConversationMessage], _t: &[ToolSpec], _mo: &str, _te: f64) -> Result<ChatResponse> {
            Ok(ChatResponse {
                text: Some(format!("来自 {}", self.label)),
                reasoning_content: None,
                tool_calls: vec![],
            })
        }
    }

    fn fast_retry() -> RetryConfig {
        RetryConfig {
            max_retries: 3,
            initial_backoff_ms: 1,   // 测试用：1ms 退避
            backoff_multiplier: 1.0,
            max_backoff_ms: 5,
        }
    }

    // --- 重试测试 ---

    #[tokio::test]
    async fn retries_and_succeeds() {
        // 失败 2 次后成功，max_retries=3，应该成功
        let provider = ReliableProvider::new(
            Box::new(FlakyProvider::new(2)),
            fast_retry(),
        );
        let result = provider
            .chat_with_tools(&[], &[], "m", 0.7)
            .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().text.as_deref(), Some("成功"));
    }

    #[tokio::test]
    async fn fails_after_max_retries() {
        // 失败 5 次，max_retries=3，应该失败
        let provider = ReliableProvider::new(
            Box::new(FlakyProvider::new(5)),
            fast_retry(),
        );
        let result = provider.chat_with_tools(&[], &[], "m", 0.7).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn success_on_first_try_no_retry() {
        // 第一次就成功，不应重试
        let provider = ReliableProvider::new(
            Box::new(FlakyProvider::new(0)),
            fast_retry(),
        );
        let result = provider.chat_with_tools(&[], &[], "m", 0.7).await;
        assert!(result.is_ok());
    }

    // --- Fallback 测试 ---

    #[tokio::test]
    async fn fallback_used_when_primary_fails() {
        let provider = ReliableProvider::with_fallbacks(
            Box::new(AlwaysFailProvider),
            vec![Box::new(AlwaysSucceedProvider { label: "fallback1".to_string() })],
            fast_retry(),
        );
        let result = provider.chat_with_tools(&[], &[], "m", 0.7).await;
        assert!(result.is_ok());
        assert!(result.unwrap().text.unwrap().contains("fallback1"));
    }

    #[tokio::test]
    async fn fallback_chain_tried_in_order() {
        let provider = ReliableProvider::with_fallbacks(
            Box::new(AlwaysFailProvider),
            vec![
                Box::new(AlwaysFailProvider),
                Box::new(AlwaysSucceedProvider { label: "fallback2".to_string() }),
            ],
            fast_retry(),
        );
        let result = provider.chat_with_tools(&[], &[], "m", 0.7).await;
        assert!(result.is_ok());
        assert!(result.unwrap().text.unwrap().contains("fallback2"));
    }

    #[tokio::test]
    async fn all_fallbacks_fail_returns_error() {
        let provider = ReliableProvider::with_fallbacks(
            Box::new(AlwaysFailProvider),
            vec![Box::new(AlwaysFailProvider)],
            fast_retry(),
        );
        let result = provider.chat_with_tools(&[], &[], "m", 0.7).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("所有 Provider 均失败"));
    }

    // --- is_retryable 测试 ---

    #[test]
    fn auth_error_not_retryable() {
        assert!(!is_retryable("401 Unauthorized invalid_api_key"));
        assert!(!is_retryable("403 Forbidden"));
        assert!(!is_retryable("400 Bad Request"));
    }

    #[test]
    fn timeout_is_retryable() {
        assert!(is_retryable("connection timeout"));
        assert!(is_retryable("500 Internal Server Error"));
        assert!(is_retryable("429 Too Many Requests"));
        assert!(is_retryable("network error"));
    }

    // --- RetryConfig 默认值测试 ---

    #[test]
    fn default_retry_config() {
        let config = RetryConfig::default();
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.initial_backoff_ms, 500);
        assert!((config.backoff_multiplier - 2.0).abs() < f64::EPSILON);
    }
}
```

---

## 七、关键注意事项

1. **退避不能太短**：生产环境 `initial_backoff_ms` 建议 ≥ 500ms。测试时用 1ms 加速。

2. **流式模式的 Fallback**：流式 fallback 时，`tx` 已经可能被前一次失败的尝试发送过一些 `Thinking` 事件，CLI 端接收到 channel 关闭前不会报错，不影响 UX。

3. **非可重试错误立即返回**：401/403/400/404 等错误重试无意义，应立即传播给上层处理。

4. **不修改 Agent**：Agent 拿到的只是 `Box<dyn Provider>`，完全无感知是否包了 ReliableProvider。这是装饰器模式的核心优势。

5. **Fallback 的 model 参数**：当前设计 fallback provider 使用相同的 `model` 参数。如果 fallback provider 不支持该 model 名称，会失败。实际使用时 fallback provider 应配置为兼容 model。
