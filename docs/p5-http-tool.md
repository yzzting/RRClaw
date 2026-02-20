# P5-1: HTTP Request Tool 实现计划

## 背景

当前 RRClaw 无法让 LLM 主动发起 HTTP 请求。有了 HTTP 工具后，LLM 可以直接查天气、调 GitHub API、查汇率、请求任意 REST 接口，无需 MCP 就能集成大量外部服务。参考 ZeroClaw 和 IronClaw 均有此工具，是高收益低成本的核心能力。

**安全重点**：必须防御 SSRF（服务端请求伪造）——LLM 不能被诱导请求内网服务（localhost、私有 IP、云元数据接口）。

---

## 一、架构设计

```
用户消息 → Agent → LLM 决定调用 http_request
                                │
                    HttpRequestTool::pre_validate()
                    ├── ReadOnly 模式拒绝
                    ├── 仅允许 http/https scheme
                    └── SSRF 检查（私有 IP / localhost / 元数据接口）
                                │
                    确认（Supervised 模式）
                                │
                    HttpRequestTool::execute()
                    ├── 构造 reqwest::Client（带 timeout）
                    ├── 设置 method / headers / body
                    ├── 发送请求
                    ├── 读取响应（限制 1MB）
                    └── 格式化输出（status + headers + body）
```

### reqwest 依赖确认

`reqwest` 已在 `Cargo.toml` 中：
```toml
reqwest = { version = "0.13", default-features = false, features = ["rustls", "json", "stream"] }
```
**无需新增任何依赖**，`stream` feature 支持按字节流读取响应（用于大小限制）。

---

## 二、新增文件

```
src/tools/http.rs        ← 新增：HttpRequestTool
```

`src/tools/mod.rs` 注册（改动极小，见第五章）。

---

## 三、完整实现代码

### 3.1 src/tools/http.rs

```rust
use async_trait::async_trait;
use color_eyre::eyre::{eyre, Result};
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::json;
use std::str::FromStr;
use std::time::Duration;
use tracing::{debug, warn};

use crate::security::SecurityPolicy;
use super::traits::{Tool, ToolResult};

/// 响应体最大字节数（1 MiB）
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
/// 默认超时（秒）
const DEFAULT_TIMEOUT_SECS: u64 = 30;
/// 最大超时上限（秒）
const MAX_TIMEOUT_SECS: u64 = 120;

pub struct HttpRequestTool;

#[async_trait]
impl Tool for HttpRequestTool {
    fn name(&self) -> &str {
        "http_request"
    }

    fn description(&self) -> &str {
        "发起 HTTP 请求（GET/POST/PUT/PATCH/DELETE/HEAD）。\
         支持自定义 headers、请求体。\
         仅允许 http/https，禁止访问内网/localhost/云元数据接口（SSRF 防护）。\
         响应体最大 1MB，超出部分截断。"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "请求 URL，必须以 http:// 或 https:// 开头"
                },
                "method": {
                    "type": "string",
                    "enum": ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"],
                    "description": "HTTP 方法，默认 GET"
                },
                "headers": {
                    "type": "object",
                    "description": "请求头，key-value 对象。如 {\"Authorization\": \"Bearer token\", \"Content-Type\": \"application/json\"}",
                    "additionalProperties": {"type": "string"}
                },
                "body": {
                    "type": "string",
                    "description": "请求体字符串。POST/PUT/PATCH 时使用，JSON 需自行序列化为字符串"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "超时秒数，默认 30，最大 120",
                    "default": 30
                }
            },
            "required": ["url"]
        })
    }

    fn pre_validate(&self, args: &serde_json::Value, policy: &SecurityPolicy) -> Option<String> {
        // 1. ReadOnly 模式：拒绝所有 HTTP 请求（HTTP 请求即使是 GET 也可能有副作用）
        if !policy.allows_execution() {
            return Some("只读模式下不允许发起 HTTP 请求".to_string());
        }

        // 2. 解析 URL
        let url_str = match args.get("url").and_then(|v| v.as_str()) {
            Some(u) if !u.is_empty() => u,
            _ => return Some("缺少 url 参数".to_string()),
        };

        // 3. Scheme 检查：只允许 http/https
        let url = match url::Url::parse(url_str) {
            Ok(u) => u,
            Err(_) => return Some(format!("无效的 URL: {}", url_str)),
        };

        let scheme = url.scheme();
        if scheme != "http" && scheme != "https" {
            return Some(format!(
                "不支持的 URL scheme '{}'，只允许 http 或 https",
                scheme
            ));
        }

        // 4. SSRF 检查：阻止内网访问
        let host = match url.host_str() {
            Some(h) => h,
            None => return Some("URL 缺少 host".to_string()),
        };

        if let Some(reason) = check_ssrf_risk(host) {
            return Some(reason);
        }

        None
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _policy: &SecurityPolicy,
    ) -> Result<ToolResult> {
        let url_str = args
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| eyre!("缺少 url 参数"))?;

        let method_str = args
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("GET")
            .to_uppercase();

        let timeout_secs = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .min(MAX_TIMEOUT_SECS);

        // 构建 reqwest Method
        let method = match reqwest::Method::from_bytes(method_str.as_bytes()) {
            Ok(m) => m,
            Err(_) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("不支持的 HTTP 方法: {}", method_str)),
                })
            }
        };

        // 构建 headers
        let mut header_map = HeaderMap::new();
        if let Some(headers_obj) = args.get("headers").and_then(|v| v.as_object()) {
            for (key, val) in headers_obj {
                if let (Ok(name), Some(value)) = (
                    HeaderName::from_str(key),
                    val.as_str(),
                ) {
                    if let Ok(hv) = HeaderValue::from_str(value) {
                        header_map.insert(name, hv);
                    } else {
                        warn!("http_request: 无效 header value，跳过: {}={}", key, value);
                    }
                } else {
                    warn!("http_request: 无效 header name，跳过: {}", key);
                }
            }
        }

        // 构建 client（每次请求新建，避免连接复用带来的超时状态问题）
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .map_err(|e| eyre!("构建 HTTP client 失败: {}", e))?;

        // 构建请求
        let mut request_builder = client.request(method.clone(), url_str).headers(header_map);

        // 设置 body（只对有 body 的方法生效）
        if let Some(body_str) = args.get("body").and_then(|v| v.as_str()) {
            if !body_str.is_empty() {
                request_builder = request_builder.body(body_str.to_string());
            }
        }

        debug!(
            "http_request: {} {} timeout={}s",
            method_str, url_str, timeout_secs
        );

        // 发送请求
        let response = match request_builder.send().await {
            Ok(r) => r,
            Err(e) => {
                let err_msg = if e.is_timeout() {
                    format!("请求超时（{}s）: {}", timeout_secs, e)
                } else if e.is_connect() {
                    format!("连接失败: {}", e)
                } else {
                    format!("请求失败: {}", e)
                };
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(err_msg),
                });
            }
        };

        let status = response.status();
        let status_line = format!("HTTP {} {}", status.as_u16(), status.canonical_reason().unwrap_or(""));

        // 读取响应 headers（只取前 20 个，避免过长）
        let resp_headers: Vec<String> = response
            .headers()
            .iter()
            .take(20)
            .map(|(k, v)| {
                format!(
                    "{}: {}",
                    k,
                    v.to_str().unwrap_or("<binary>")
                )
            })
            .collect();

        // 按字节流读取 body，限制大小
        let mut body_bytes = Vec::new();
        let mut truncated = false;
        let mut stream = response.bytes_stream();

        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(data) => {
                    if body_bytes.len() + data.len() > MAX_RESPONSE_BYTES {
                        // 只取剩余空间
                        let remaining = MAX_RESPONSE_BYTES - body_bytes.len();
                        body_bytes.extend_from_slice(&data[..remaining]);
                        truncated = true;
                        break;
                    }
                    body_bytes.extend_from_slice(&data);
                }
                Err(e) => {
                    warn!("http_request: 读取响应体失败: {}", e);
                    break;
                }
            }
        }

        // 尝试 UTF-8 解码，失败则显示字节数
        let body_str = match String::from_utf8(body_bytes.clone()) {
            Ok(s) => s,
            Err(_) => format!("<二进制响应，{} 字节>", body_bytes.len()),
        };

        // 格式化输出
        let mut output = String::new();
        output.push_str(&status_line);
        output.push('\n');

        if !resp_headers.is_empty() {
            output.push_str("\n[Headers]\n");
            output.push_str(&resp_headers.join("\n"));
            output.push('\n');
        }

        output.push_str("\n[Body]\n");
        output.push_str(&body_str);

        if truncated {
            output.push_str(&format!(
                "\n\n[响应体已截断：仅显示前 {} 字节]",
                MAX_RESPONSE_BYTES
            ));
        }

        let success = status.is_success();

        debug!(
            "http_request 完成: status={}, body_len={}, truncated={}",
            status.as_u16(),
            body_bytes.len(),
            truncated
        );

        Ok(ToolResult {
            success,
            output: if success { output.clone() } else { String::new() },
            error: if success {
                None
            } else {
                Some(output)
            },
        })
    }
}

/// 检查 host 是否有 SSRF 风险
/// 返回 Some(原因) 表示有风险，None 表示安全
fn check_ssrf_risk(host: &str) -> Option<String> {
    // 1. 阻止 localhost 变体
    let host_lower = host.to_lowercase();
    if host_lower == "localhost"
        || host_lower == "ip6-localhost"
        || host_lower == "ip6-loopback"
    {
        return Some(format!("禁止访问 localhost（SSRF 防护）: {}", host));
    }

    // 2. 阻止云平台元数据接口（AWS/GCP/Azure）
    if host == "169.254.169.254"
        || host == "metadata.google.internal"
        || host == "metadata.azure.internal"
        || host_lower.ends_with(".internal")
        || host_lower.ends_with(".local")
        || host_lower.ends_with(".localhost")
    {
        return Some(format!("禁止访问元数据/内网服务（SSRF 防护）: {}", host));
    }

    // 3. 尝试解析为 IP 地址，检查是否为私有 IP
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if is_private_ip(ip) {
            return Some(format!(
                "禁止访问私有/保留 IP 地址（SSRF 防护）: {}",
                ip
            ));
        }
    }

    None
}

/// 判断 IP 是否为私有/保留地址
fn is_private_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            // 127.0.0.0/8 (loopback)
            // 10.0.0.0/8 (RFC 1918)
            // 172.16.0.0/12 (RFC 1918)
            // 192.168.0.0/16 (RFC 1918)
            // 169.254.0.0/16 (link-local / 云元数据)
            // 0.0.0.0/8 (unspecified)
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || {
                    // 100.64.0.0/10 (CGNAT)
                    let octets = v4.octets();
                    octets[0] == 100 && (octets[1] & 0b1100_0000) == 64
                }
        }
        std::net::IpAddr::V6(v6) => {
            // ::1 (loopback)
            // :: (unspecified)
            // fc00::/7 (ULA, 私有)
            // fe80::/10 (link-local)
            v6.is_loopback()
                || v6.is_unspecified()
                || {
                    let segments = v6.segments();
                    // fc00::/7
                    (segments[0] & 0xfe00) == 0xfc00
                    // fe80::/10
                    || (segments[0] & 0xffc0) == 0xfe80
                }
        }
    }
}

// ===== 需要添加 url crate 依赖 =====
// Cargo.toml 中添加：
// url = "2"
// （url crate 是 reqwest 的间接依赖，直接声明确保版本稳定）

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use std::path::PathBuf;

    fn full_policy() -> SecurityPolicy {
        SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            allowed_commands: vec![],
            workspace_dir: PathBuf::from("/tmp"),
            blocked_paths: vec![],
        }
    }

    fn readonly_policy() -> SecurityPolicy {
        SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..full_policy()
        }
    }

    // ─── pre_validate 测试 ───────────────────────────────────────────

    #[test]
    fn pre_validate_readonly_rejected() {
        let tool = HttpRequestTool;
        let args = serde_json::json!({"url": "https://example.com"});
        let result = tool.pre_validate(&args, &readonly_policy());
        assert!(result.is_some());
        assert!(result.unwrap().contains("只读"));
    }

    #[test]
    fn pre_validate_missing_url_rejected() {
        let tool = HttpRequestTool;
        let args = serde_json::json!({});
        let result = tool.pre_validate(&args, &full_policy());
        assert!(result.is_some());
        assert!(result.unwrap().contains("url"));
    }

    #[test]
    fn pre_validate_invalid_url_rejected() {
        let tool = HttpRequestTool;
        let args = serde_json::json!({"url": "not-a-url"});
        let result = tool.pre_validate(&args, &full_policy());
        assert!(result.is_some());
    }

    #[test]
    fn pre_validate_file_scheme_rejected() {
        let tool = HttpRequestTool;
        let args = serde_json::json!({"url": "file:///etc/passwd"});
        let result = tool.pre_validate(&args, &full_policy());
        assert!(result.is_some());
        assert!(result.unwrap().contains("scheme"));
    }

    #[test]
    fn pre_validate_ftp_scheme_rejected() {
        let tool = HttpRequestTool;
        let args = serde_json::json!({"url": "ftp://example.com/file"});
        let result = tool.pre_validate(&args, &full_policy());
        assert!(result.is_some());
        assert!(result.unwrap().contains("scheme"));
    }

    #[test]
    fn pre_validate_localhost_rejected() {
        let tool = HttpRequestTool;
        for url in [
            "http://localhost/api",
            "http://localhost:8080/api",
            "https://localhost/secret",
        ] {
            let args = serde_json::json!({"url": url});
            let result = tool.pre_validate(&args, &full_policy());
            assert!(result.is_some(), "应拒绝: {}", url);
            assert!(result.unwrap().contains("SSRF"));
        }
    }

    #[test]
    fn pre_validate_loopback_ip_rejected() {
        let tool = HttpRequestTool;
        for url in [
            "http://127.0.0.1/api",
            "http://127.1.2.3/secret",
            "http://[::1]/api",
        ] {
            let args = serde_json::json!({"url": url});
            let result = tool.pre_validate(&args, &full_policy());
            assert!(result.is_some(), "应拒绝: {}", url);
        }
    }

    #[test]
    fn pre_validate_private_ip_rejected() {
        let tool = HttpRequestTool;
        for url in [
            "http://10.0.0.1/api",
            "http://192.168.1.100/api",
            "http://172.16.0.1/api",
            "http://172.31.255.255/api",
        ] {
            let args = serde_json::json!({"url": url});
            let result = tool.pre_validate(&args, &full_policy());
            assert!(result.is_some(), "应拒绝私有 IP: {}", url);
        }
    }

    #[test]
    fn pre_validate_metadata_ip_rejected() {
        let tool = HttpRequestTool;
        let args = serde_json::json!({"url": "http://169.254.169.254/latest/meta-data/"});
        let result = tool.pre_validate(&args, &full_policy());
        assert!(result.is_some());
    }

    #[test]
    fn pre_validate_public_url_allowed() {
        let tool = HttpRequestTool;
        for url in [
            "https://api.github.com/users/octocat",
            "http://httpbin.org/get",
            "https://jsonplaceholder.typicode.com/todos/1",
        ] {
            let args = serde_json::json!({"url": url});
            let result = tool.pre_validate(&args, &full_policy());
            assert!(result.is_none(), "应允许: {}", url);
        }
    }

    // ─── is_private_ip 单元测试 ──────────────────────────────────────

    #[test]
    fn private_ipv4_detected() {
        use std::net::IpAddr;
        assert!(is_private_ip("10.0.0.1".parse::<IpAddr>().unwrap()));
        assert!(is_private_ip("192.168.0.1".parse::<IpAddr>().unwrap()));
        assert!(is_private_ip("172.16.0.1".parse::<IpAddr>().unwrap()));
        assert!(is_private_ip("172.31.0.1".parse::<IpAddr>().unwrap()));
        assert!(is_private_ip("127.0.0.1".parse::<IpAddr>().unwrap()));
        assert!(is_private_ip("169.254.0.1".parse::<IpAddr>().unwrap()));
    }

    #[test]
    fn public_ipv4_allowed() {
        use std::net::IpAddr;
        assert!(!is_private_ip("8.8.8.8".parse::<IpAddr>().unwrap()));
        assert!(!is_private_ip("1.1.1.1".parse::<IpAddr>().unwrap()));
        assert!(!is_private_ip("93.184.216.34".parse::<IpAddr>().unwrap()));
    }

    #[test]
    fn private_ipv6_detected() {
        use std::net::IpAddr;
        assert!(is_private_ip("::1".parse::<IpAddr>().unwrap()));
        assert!(is_private_ip("fe80::1".parse::<IpAddr>().unwrap()));
        assert!(is_private_ip("fc00::1".parse::<IpAddr>().unwrap()));
    }

    // ─── execute 集成测试（需要网络，标记 ignore 在 CI 跳过）────────

    #[tokio::test]
    #[ignore = "需要网络连接"]
    async fn execute_get_public_api() {
        let tool = HttpRequestTool;
        let args = serde_json::json!({
            "url": "https://httpbin.org/get",
            "method": "GET"
        });
        let result = tool.execute(args, &full_policy()).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("HTTP 200"));
        assert!(result.output.contains("[Body]"));
    }

    #[tokio::test]
    #[ignore = "需要网络连接"]
    async fn execute_post_with_body() {
        let tool = HttpRequestTool;
        let args = serde_json::json!({
            "url": "https://httpbin.org/post",
            "method": "POST",
            "headers": {"Content-Type": "application/json"},
            "body": "{\"hello\": \"world\"}"
        });
        let result = tool.execute(args, &full_policy()).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("200"));
    }

    #[tokio::test]
    #[ignore = "需要网络连接"]
    async fn execute_404_returns_error() {
        let tool = HttpRequestTool;
        let args = serde_json::json!({
            "url": "https://httpbin.org/status/404"
        });
        let result = tool.execute(args, &full_policy()).await.unwrap();
        // 404 不是 success，错误信息里有 404
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("404"));
    }

    #[test]
    fn tool_spec_correct() {
        let spec = HttpRequestTool.spec();
        assert_eq!(spec.name, "http_request");
        assert!(spec.parameters["required"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("url")));
    }
}
```

### 3.2 Cargo.toml 新增依赖

在 `Cargo.toml` 的 `[dependencies]` 节增加：

```toml
url = "2"
```

> **说明**：`url` 是 `reqwest` 的传递依赖，显式声明版本避免未来升级带来的类型不兼容。`reqwest` 0.13 内部已用 `url = "2.x"`，不会产生冲突。

---

## 四、Cargo.toml 中的 reqwest features 确认

当前配置：
```toml
reqwest = { version = "0.13", default-features = false, features = ["rustls", "json", "stream"] }
```

**`stream` feature 已经包含**，`bytes_stream()` 方法可用，无需改动。

---

## 五、注册 Tool（src/tools/mod.rs）

在 `src/tools/mod.rs` 中：

```rust
// 1. 在文件顶部新增模块声明：
pub mod http;

// 2. 在 use 区域新增：
use http::HttpRequestTool;

// 3. 在 create_tools() 的 vec! 中新增：
Box::new(HttpRequestTool),
```

**完整 create_tools 函数对比**（改动用注释标注）：

```rust
pub fn create_tools(
    app_config: Config,
    data_dir: PathBuf,
    log_dir: PathBuf,
    config_path: PathBuf,
    skills: Vec<SkillMeta>,
    memory: Arc<dyn Memory>,
) -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(ShellTool),
        Box::new(FileReadTool),
        Box::new(FileWriteTool),
        Box::new(ConfigTool),
        Box::new(SelfInfoTool::new(app_config, data_dir, log_dir, config_path)),
        Box::new(SkillTool::new(skills)),
        Box::new(GitTool),
        Box::new(MemoryStoreTool::new(memory.clone())),
        Box::new(MemoryRecallTool::new(memory.clone())),
        Box::new(MemoryForgetTool::new(memory)),
        Box::new(HttpRequestTool),  // ← 新增这一行
    ]
}
```

---

## 六、改动范围汇总

| 文件 | 改动类型 | 说明 |
|------|---------|------|
| `Cargo.toml` | 新增依赖 | `url = "2"` |
| `src/tools/http.rs` | **新增文件** | HttpRequestTool 完整实现 |
| `src/tools/mod.rs` | 微改 | 3 处：pub mod / use / vec! 各 1 行 |

**不需要改动**：Agent、Provider、Memory、Security、CLI、Config schema。

---

## 七、提交策略

| # | 提交 message | 内容 |
|---|-------------|------|
| 1 | `docs: add P5-1 HTTP request tool design` | 本文件 |
| 2 | `feat: add url dependency for HTTP tool` | Cargo.toml |
| 3 | `feat: add HttpRequestTool with SSRF protection` | src/tools/http.rs |
| 4 | `feat: register HttpRequestTool in create_tools` | src/tools/mod.rs |
| 5 | `test: add HttpRequestTool unit tests` | http.rs 内测试（无网络的部分）|

---

## 八、测试执行方式

```bash
# 运行所有单元测试（不需要网络）
cargo test -p rrclaw tools::http

# 运行需要网络的集成测试（手动触发）
cargo test -p rrclaw tools::http -- --ignored

# 跑全部测试确保没有回归
cargo test -p rrclaw

# clippy 检查
cargo clippy -p rrclaw -- -D warnings
```

---

## 九、关键注意事项

### 9.1 url crate 解析与 reqwest 版本
- `url = "2"` 与 `reqwest = "0.13"` 使用相同的 url 版本，不会出现类型不兼容
- `url::Url::parse()` 会 percent-encode 特殊字符，是标准 URL 解析行为

### 9.2 is_private_ip 稳定 API
- `Ipv4Addr::is_private()` 在 Rust stable 1.34+ 可用 ✅
- `Ipv4Addr::is_link_local()` 在 Rust stable 1.0+ 可用 ✅
- 注意：`Ipv4Addr::is_global()` 是 nightly-only，**不要用**

### 9.3 DNS 重绑定攻击
SSRF 检查只在 URL 解析时做一次。攻击者可以先返回一个公网 IP 通过检查，再在 TTL 过期后把 DNS 指向内网（DNS rebinding）。当前实现**不防御 DNS 重绑定**，这是已知限制。在个人助手场景中风险可接受，文档中应注明。

### 9.4 ToolResult success 逻辑
- HTTP 2xx → `success: true`，输出在 `output`
- HTTP 4xx/5xx → `success: false`，输出在 `error`
- 这样 LLM 能区分"请求成功但内容为 404"和"网络错误"

### 9.5 reqwest::Client 每次新建
每次调用新建 client 而非使用全局连接池，原因：
- Tool 是无状态的（`pub struct HttpRequestTool;`），无法存储 client
- 避免跨 session 的连接状态污染
- 性能影响可接受（reqwest client 构建很快）
- 如后续需要优化，可通过构造函数注入 `Arc<reqwest::Client>`

### 9.6 为什么不用 `response.text().await`
`response.text().await` 会读取整个响应体到内存，无大小限制。`bytes_stream()` 可以在读到 1MB 时立即截断，保护内存安全。

### 9.7 与 MCP 的关系
MCP 工具（P4 已规划）也可以调 HTTP API，但：
- MCP 需要配置 server，适合稳定的集成
- HttpRequestTool 是临时/探索性 API 调用的首选
- 两者互补，不冲突

---

## 十、已知限制（文档中说明）

| 限制 | 说明 | 改进方向 |
|------|------|---------|
| DNS 重绑定 | 不防御 | 可在 reqwest 中 hook DNS 解析做二次检查（复杂） |
| HTTPS 证书验证 | 始终开启（rustls） | 已是最佳实践，不提供跳过选项 |
| 重定向 | reqwest 默认跟随（最多 10 次） | 重定向后的 URL 不再做 SSRF 检查（已知限制）|
| 认证 | 依赖 headers 字段 | 不内置 Basic/OAuth，LLM 自行构造 Authorization header |
| 响应编码 | 只处理 UTF-8 + 二进制提示 | 未来可加 charset 检测 |

---

## 十一、动态白名单添加（LLM 询问用户）

### 11.1 背景

当用户请求一个不在白名单的内网地址时，当前流程直接拒绝。用户希望：
1. LLM 可以询问用户"是否允许访问这个地址？"
2. 用户同意后，自动添加到白名单
3. 重新执行请求

### 11.2 设计方案

**解决方案**：在错误信息中用 `|` 分隔符编码可配置解决提示。

http_request 被 SSRF 拦截时，返回带配置建议的错误信息：
```
"禁止访问私有IP 192.168.1.100|可使用 /config set security.http_allowed_hosts 添加 ["192.168.1.100"] 到白名单"
```

LLM 通过解析 `|` 字符识别这是可配置解决的错误，询问用户后可通过 ConfigTool 添加白名单。

### 11.3 用户交互流程

```
用户：帮我请求 http://192.168.1.100:8080/api
LLM：[调用 http_request]

http_request 返回错误：
"禁止访问私有IP 192.168.1.100|可使用 /config set security.http_allowed_hosts 添加 ["192.168.1.100"] 到白名单"

LLM：检测到可配置解决，询问用户：
"检测到 192.168.1.100 是内网地址，不在白名单中。是否允许 RRClaw 访问此地址？
[是(Y)/否(N)]"

用户：Y

LLM：[调用 ConfigTool 添加白名单]
[重新调用 http_request]
```
