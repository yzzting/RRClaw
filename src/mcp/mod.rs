pub mod tool;

use color_eyre::eyre::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{info, warn};

use rmcp::service::{Peer, RoleClient, RunningService};
use rmcp::transport::child_process::{TokioChildProcess, ConfigureCommandExt};
use rmcp::ServiceExt;

use crate::config::{McpServerConfig, McpTransport};
use crate::tools::traits::Tool;
use tool::McpTool;

/// 已连接的单个 MCP Server
struct McpServer {
    name: String,
    service: RunningService<RoleClient, ()>,
    peer: Arc<Peer<RoleClient>>,
    allowed_tools: Vec<String>,
}

/// 管理所有 MCP Server 连接
pub struct McpManager {
    servers: Vec<McpServer>,
}

impl McpManager {
    /// 根据配置连接所有 MCP Server，失败的跳过并记录警告
    pub async fn connect_all(configs: &HashMap<String, McpServerConfig>) -> Self {
        let mut servers = Vec::new();

        for (name, config) in configs {
            match connect_server(name, config).await {
                Ok(service) => {
                    info!("MCP Server '{}' 连接成功", name);
                    let peer = Arc::new(service.peer().clone());
                    servers.push(McpServer {
                        name: name.clone(),
                        service,
                        peer,
                        allowed_tools: config.allowed_tools.clone(),
                    });
                }
                Err(e) => {
                    warn!("MCP Server '{}' 连接失败（跳过）: {:#}", name, e);
                }
            }
        }

        Self { servers }
    }

    /// 获取所有 MCP tools，转换为 RRClaw Tool trait 对象
    pub async fn tools(&self) -> Vec<Box<dyn Tool>> {
        let mut result: Vec<Box<dyn Tool>> = Vec::new();

        for server in &self.servers {
            match server.peer.list_all_tools().await {
                Ok(tools) => {
                    let mut count = 0;
                    for tool_def in tools {
                        let tool_name = tool_def.name.as_ref();
                        // 过滤：如果 allowed_tools 非空，只保留白名单内的工具
                        if !server.allowed_tools.is_empty()
                            && !server.allowed_tools.iter().any(|a| a == tool_name)
                        {
                            continue;
                        }
                        result.push(Box::new(McpTool::new(
                            &server.name,
                            tool_def,
                            server.peer.clone(),
                        )));
                        count += 1;
                    }
                    info!("MCP Server '{}' 加载了 {} 个工具", server.name, count);
                }
                Err(e) => {
                    warn!(
                        "获取 MCP Server '{}' 工具列表失败: {:#}",
                        server.name, e
                    );
                }
            }
        }

        result
    }

    /// 优雅关闭所有 MCP 连接
    pub async fn shutdown(self) {
        for server in self.servers {
            let name = server.name;
            match server.service.cancel().await {
                Ok(_) => info!("MCP Server '{}' 已关闭", name),
                Err(e) => warn!("MCP Server '{}' 关闭失败: {:#}", name, e),
            }
        }
    }
}

/// 连接单个 MCP Server
async fn connect_server(
    name: &str,
    config: &McpServerConfig,
) -> Result<RunningService<RoleClient, ()>> {
    match &config.transport {
        McpTransport::Stdio { command, args, env } => {
            let env_clone = env.clone();
            let args_clone = args.clone();
            // 用 builder().stderr(null) 抑制子进程日志，TokioChildProcess::new()
            // 内部 builder 默认 stderr=inherit 会覆盖 configure 回调里的设置
            let (transport, _) = TokioChildProcess::builder(
                tokio::process::Command::new(command).configure(|cmd| {
                    cmd.args(&args_clone);
                    for (k, v) in &env_clone {
                        cmd.env(k, v);
                    }
                }),
            )
            .stderr(std::process::Stdio::null())
            .spawn()?;

            ().serve(transport)
                .await
                .map_err(|e| color_eyre::eyre::eyre!("{}", e))
                .wrap_err_with(|| format!("MCP stdio 握手失败: {}", name))
        }
        McpTransport::Sse { url, headers } => {
            use rmcp::transport::streamable_http_client::{
                StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
            };

            let mut transport_config = StreamableHttpClientTransportConfig::with_uri(url.as_str());

            // 设置自定义 headers
            for (k, v) in headers {
                if k.eq_ignore_ascii_case("authorization") {
                    transport_config.auth_header = Some(v.clone());
                } else {
                    use reqwest::header::{HeaderName, HeaderValue};
                    if let (Ok(hname), Ok(hvalue)) =
                        (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v))
                    {
                        transport_config.custom_headers.insert(hname, hvalue);
                    }
                }
            }

            let transport = StreamableHttpClientTransport::with_client(
                reqwest::Client::new(),
                transport_config,
            );

            ().serve(transport)
                .await
                .map_err(|e| color_eyre::eyre::eyre!("{}", e))
                .wrap_err_with(|| format!("MCP SSE 握手失败: {}", name))
        }
    }
}
