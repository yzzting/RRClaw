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
         不自动跟随重定向（3xx 响应会直接返回 Location header）。\
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
        // 使用 host() 获取 IpAddr，避免 IPv6 URL 带方括号的问题
        let host_ip = match url.host() {
            Some(url::Host::Ipv6(ip)) => Some(ip.to_string()),
            Some(url::Host::Ipv4(ip)) => Some(ip.to_string()),
            Some(url::Host::Domain(h)) => Some(h.to_string()),
            None => None,
        };

        let host_str = host_ip.as_deref().unwrap_or("");
        if let Some(reason) = check_ssrf_risk(host_str) {
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
        // 禁用自动重定向：重定向目标 URL 不会再次经过 SSRF 检查，存在绕过风险
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| eyre!("构建 HTTP client 失败: {}", e))?;

        // 构建请求
        let mut request_builder = client.request(method, url_str).headers(header_map);

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
        let body_len = body_bytes.len();
        let body_str = match String::from_utf8(body_bytes) {
            Ok(s) => s,
            Err(_) => format!("<二进制响应，{} 字节>", body_len),
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
            body_len,
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
            // 0.0.0.0 (unspecified，仅精确匹配，非整个 /8 段)
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
