# P6: http_request 智能响应处理

## 一、背景与问题

当前 `http_request` 工具对所有响应一视同仁：拿到字节流，截断到 1MB，原样注入 LLM context。

这对以下场景造成严重问题：

| 场景 | 原始大小 | 实际有用内容 | 现状 |
|------|---------|------------|------|
| Yahoo Finance JSON | 35KB | 35KB | ✓ 正常 |
| 新闻文章 HTML | 400KB | ~10KB 正文 | ✗ 注入大量 JS/CSS |
| Google Finance SPA | 1MB | 0（无法解析） | ✗ LLM API 耗时 4.5 分钟后超时 |
| 技术文档 HTML | 600KB | ~80KB 正文 | ✗ 大量标签噪声 |

**根本矛盾**：截断的是字节数，不是"对 LLM 有用的信息量"。

---

## 二、设计方案：A → B 串联

```
收到 HTTP 响应
    │
    ▼
[A] Content-Type 感知处理
    ├── JSON / 纯文本 → 直接返回（不变，限制保持 1MB）
    └── text/html    → HTML Strip（去除标签/脚本/样式，保留文字内容）
                           │
                           ▼
                    Strip 后大小？
                    ├── ≤ 200KB → 直接返回（文章、文档场景 ✓）
                    └── > 200KB → [B] mini-LLM 提取
                                        │
                                        ├── extract 参数已提供 → 发起 mini-LLM 调用
                                        │   返回：提取结果（通常 < 1KB）
                                        └── extract 参数未提供 → 截断到 200KB + 警告
                                            "[响应过大，建议在 http_request 中加 extract 参数]"
```

### 各场景走哪条路

| 场景 | Strip 后 | 走哪条路 | 结果 |
|------|---------|---------|------|
| Yahoo Finance JSON | 35KB | A → 直接返回 | ✓ 正常 |
| 新闻文章（普通）| 8KB | A → ≤ 200KB 直接返回 | ✓ 全文可读 |
| 长篇技术文档 | 80KB | A → ≤ 200KB 直接返回 | ✓ 全文可读 |
| 超长 Wikipedia | 180KB | A → ≤ 200KB 直接返回 | ✓ 全文可读 |
| Google Finance SPA | ~200KB | A → > 200KB → B（如有 extract）| ✓ 返回股价 |
| Google Finance SPA | ~200KB | A → > 200KB → 截断 + 警告（无 extract）| △ 快速失败，LLM 知道下一步 |

**200KB 阈值依据**：
- 覆盖 99% 的真实文章/文档场景（strip 后极少超过 200KB）
- strip 后仍 > 200KB 的基本是 SPA 或超大文档，需要 mini-extraction 介入
- 200KB ≈ 50K tokens，对主 LLM context 来说是合理上限

---

## 三、参数变化

### 3.1 新增 `extract` 可选参数

```json
"extract": {
    "type": "string",
    "description": "（可选）当响应体较大时，指定要从中提取的目标信息。\
                    例如：\"当前股价和涨跌幅\"、\"文章正文\"、\"所有链接\"。\
                    仅在响应 strip 后仍超过 200KB 时触发 mini-LLM 提取；\
                    正常大小的响应直接返回全文，无需此参数。"
}
```

**使用场景**：LLM 在请求 SPA 类页面时，主动传入此参数：

```json
{
    "url": "https://finance.google.com/quote/TSLA:NASDAQ",
    "extract": "TSLA 当前股价、涨跌金额和涨跌幅"
}
```

不传此参数时（文章阅读、API 调用），行为与现在一致，strip 后直接返回。

### 3.2 description 更新

```
发起 HTTP 请求（GET/POST/PUT/PATCH/DELETE/HEAD）。
支持自定义 headers、请求体。
仅允许 http/https，禁止访问内网/localhost/云元数据接口（SSRF 防护）。
不自动跟随重定向（3xx 响应会直接返回 Location header）。

响应处理：
- JSON / 纯文本：直接返回，最大 1MB
- HTML 页面：自动 strip 标签/脚本/样式，保留文字内容，最大 200KB
  - strip 后 ≤ 200KB：直接返回全部文字（适合文章、文档）
  - strip 后 > 200KB：若提供了 extract 参数则触发精准提取，否则截断并给出提示
```

---

## 四、配置项

### 4.0 `SecurityConfig` 新增字段

阈值放在 `[security]` 段（与 `http_allowed_hosts` 同组，都是 HTTP 行为策略）：

**`src/config/schema.rs`**：

```rust
pub struct SecurityConfig {
    pub autonomy: AutonomyLevel,
    pub allowed_commands: Vec<String>,
    pub workspace_only: bool,
    pub http_allowed_hosts: Vec<String>,
    pub injection_check: bool,
    /// HTML 响应 strip 后的最大字节数（KB），超出则触发 mini-LLM 提取或截断
    /// 默认 200（KB）；设为 0 禁用 strip（直接走原始 1MB 截断，旧行为）
    #[serde(default = "default_http_strip_threshold_kb")]
    pub http_strip_threshold_kb: usize,
}

fn default_http_strip_threshold_kb() -> usize {
    200
}
```

**`config.toml` 示例**：

```toml
[security]
autonomy = "supervised"
allowed_commands = ["ls", "cat", "grep", "git", "cargo"]
workspace_only = true
http_allowed_hosts = []
injection_check = true
http_strip_threshold_kb = 200   # HTML strip 后阈值，默认 200KB；0 = 禁用 strip
```

**`HttpRequestTool` 构造时注入**（在 `create_tools()` 里，已有 `app_config: Config`）：

```rust
Box::new(HttpRequestTool::new(
    Some(Arc::clone(&provider)),
    app_config.default.model.clone(),
    app_config.security.http_strip_threshold_kb * 1024,  // KB → bytes
)),
```

**`HttpRequestTool` struct**：

```rust
pub struct HttpRequestTool {
    provider: Option<Arc<dyn crate::providers::Provider>>,
    model: String,
    strip_threshold_bytes: usize,   // 0 = 禁用 strip
}
```

---

## 五、实现细节

### 5.1 HTML Strip

新增依赖（`Cargo.toml`）：

```toml
html2text = "0.12"   # HTML → 纯文本，支持中文，无额外系统依赖
```

Strip 逻辑（在 `execute()` body 解析之后插入）：

```rust
// 检测 Content-Type
let content_type = resp_headers
    .iter()
    .find(|h| h.to_lowercase().starts_with("content-type:"))
    .map(|h| h.to_lowercase())
    .unwrap_or_default();

let is_html = content_type.contains("text/html");

let (processed_body, was_stripped) = if is_html && body_str.len() > 0 {
    // HTML strip：去除所有标签，保留文字
    let stripped = html2text::from_read(body_str.as_bytes(), 120);  // 120 = 行宽
    (stripped, true)
} else {
    (body_str, false)
};
```

### 5.2 大小判断与路由

```rust
// self.strip_threshold_bytes 来自 config.security.http_strip_threshold_kb * 1024
// 0 表示禁用 strip（skip_strip = true）
let skip_strip = self.strip_threshold_bytes == 0;

if was_stripped && !skip_strip && processed_body.len() > self.strip_threshold_bytes {
    let extract_hint = args.get("extract").and_then(|v| v.as_str());

    match extract_hint {
        Some(hint) => {
            // 走 B：mini-LLM 提取
            let extracted = mini_extract(&processed_body, hint, &self.provider, &self.model).await?;
            // 返回提取结果
        }
        None => {
            // 无 extract 参数：截断到 200KB + 明确警告
            let truncated = &processed_body[..HTML_STRIP_MAX_BYTES];
            output = format!(
                "{}\n\n[Body（HTML strip 后，已截断至 200KB）]\n{}\n\n\
                 [提示] 页面 strip 后仍有 {}KB，可能是 SPA/动态页面。\
                 如需精确提取，请在 http_request 中加 extract 参数，\
                 例如：extract=\"目标信息描述\"",
                status_line,
                truncated,
                processed_body.len() / 1024
            );
        }
    }
}
```

### 5.3 mini-LLM 提取（B 阶段）

**架构注意**：`HttpRequestTool` 当前是无状态 `struct`，需要注入 Provider 才能发起 LLM 调用。

**方案**：`HttpRequestTool` 从无状态改为有状态，在 `create_tools()` 时注入：

```rust
// src/tools/http.rs
pub struct HttpRequestTool {
    // 用于 mini-LLM 提取；None 时跳过 B 阶段（降级为截断）
    provider: Option<Arc<dyn crate::providers::Provider>>,
    model: String,
}

impl HttpRequestTool {
    pub fn new(provider: Option<Arc<dyn crate::providers::Provider>>, model: String) -> Self {
        Self { provider, model }
    }
}
```

```rust
// src/tools/mod.rs — create_tools() 签名新增 provider 参数
pub fn create_tools(
    app_config: Config,
    provider: Arc<dyn crate::providers::Provider>,  // ← 新增
    ...
) -> Vec<Box<dyn Tool>> {
    ...
    Box::new(HttpRequestTool::new(
        Some(Arc::clone(&provider)),
        app_config.default.model.clone(),
    )),
    ...
}
```

mini-LLM 调用（轻量 prompt，不传 tool schema，temperature=0）：

```rust
async fn mini_extract(
    content: &str,
    hint: &str,
    provider: &Arc<dyn Provider>,
    model: &str,
) -> Result<String> {
    use crate::providers::traits::{ChatMessage, ConversationMessage};

    // 截取前 150KB 给 mini-LLM（避免超过模型 context 限制）
    let content_excerpt = if content.len() > 150 * 1024 {
        &content[..150 * 1024]
    } else {
        content
    };

    let messages = vec![
        ConversationMessage::Chat(ChatMessage {
            role: "system".to_string(),
            content: "你是一个精准的信息提取助手。从给定内容中提取用户指定的信息，\
                      只返回提取到的内容，不加解释，不加前缀。\
                      如果找不到，返回\"未找到: {原因}\"。".to_string(),
            reasoning_content: None,
        }),
        ConversationMessage::Chat(ChatMessage {
            role: "user".to_string(),
            content: format!(
                "从以下内容中提取：{}\n\n---\n{}",
                hint, content_excerpt
            ),
            reasoning_content: None,
        }),
    ];

    let resp = provider
        .chat_with_tools(&messages, &[], model, 0.0)
        .await?;

    Ok(resp.text.unwrap_or_else(|| "（提取结果为空）".to_string()))
}
```

---

## 五、改动文件汇总

| 文件 | 改动内容 | 估计行数 |
|------|---------|---------|
| `Cargo.toml` | 新增 `html2text = "0.12"` | +1 |
| `src/config/schema.rs` | `SecurityConfig` 新增 `http_strip_threshold_kb` 字段 + default fn | +6 |
| `src/tools/http.rs` | HttpRequestTool 有状态化（provider + model + strip_threshold_bytes）；execute() 加 strip + 路由；新增 mini_extract() | +85 |
| `src/tools/mod.rs` | create_tools() 新增 provider 参数；HttpRequestTool::new() 传 provider + threshold | +6 |
| `src/main.rs` | 调用 create_tools() 时传入 provider（Arc::clone）| +2 |
| `src/routines/mod.rs` | run_once() 内的 create_tools() 调用同步更新 | +2 |

> channels/telegram.rs 等其他 create_tools() 调用处同步更新，每处 +1 行。

---

## 六、测试计划

```rust
// 纯文本响应不受影响
#[test]
fn json_response_not_stripped() { ... }

// HTML strip 基本功能
#[test]
fn html_strip_removes_tags() {
    let html = "<html><head><script>var x=1</script></head><body><p>Hello</p></body></html>";
    let stripped = html2text::from_read(html.as_bytes(), 120);
    assert!(stripped.contains("Hello"));
    assert!(!stripped.contains("<script>"));
    assert!(!stripped.contains("<p>"));
}

// strip 后 ≤ 200KB 直接返回（无截断）
#[test]
fn small_html_returned_fully() { ... }

// strip 后 > 200KB 无 extract → 截断 + 警告
#[test]
fn large_html_no_extract_truncated_with_hint() { ... }

// strip 后 > 200KB 有 extract → 调用 mini-LLM（mock provider）
#[tokio::test]
async fn large_html_with_extract_calls_mini_llm() { ... }

// content-type 检测：application/json 不走 strip
#[test]
fn json_content_type_skips_strip() { ... }
```

---

## 七、提交策略

| # | commit message | 内容 |
|---|---------------|------|
| 1 | `docs: add p6 http smart response design` | 本文件 |
| 2 | `feat: add http_strip_threshold_kb to SecurityConfig` | schema.rs（配置项 + default） |
| 3 | `feat: add html2text dependency` | Cargo.toml |
| 4 | `feat: make HttpRequestTool stateful with provider and threshold injection` | http.rs + mod.rs + main.rs + telegram.rs + routines/mod.rs |
| 5 | `feat: add html strip for text/html responses in http_request` | http.rs A 阶段 |
| 6 | `feat: add mini-LLM extraction for large html responses` | http.rs B 阶段（mini_extract） |
| 7 | `test: add html strip and response routing tests` | http.rs 测试 |

---

## 八、V2 待办（暂不实现）

- **分页参数**：`page` / `chunk_size` 支持超大文档分次读取（学术论文等）
- **流式 strip**：边下载边 strip，避免先缓冲 1MB 再处理
- **自适应 model**：mini-LLM 用轻量模型（如 `glm-4-flash`），主 Agent 用默认模型，降低成本
