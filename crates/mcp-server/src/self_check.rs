//! A real, in-process MCP round trip used by `orbit doctor` (and by this
//! crate's own tests) to verify a server can actually initialize and
//! answer `tools/list` -- not just that its constructor doesn't panic.
//!
//! No subprocess and no real `stdio` are involved: an in-memory duplex
//! pipe stands in for the transport, so this is fast and side-effect-free,
//! but it exercises the genuine MCP `initialize` handshake and
//! `tools/list` request through the real [`ServerHandler`](rmcp::ServerHandler)
//! implementation.

use std::time::Duration;

use orbit_core::OrbitError;
use rmcp::ServiceExt;

use crate::OrbitMcpServer;

#[derive(Debug, Clone)]
pub struct SelfCheckReport {
    pub tool_count: usize,
    pub tool_names: Vec<String>,
}

const SELF_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

pub async fn self_check(server: OrbitMcpServer) -> Result<SelfCheckReport, OrbitError> {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);

    let server_task = tokio::spawn(async move {
        let Ok(running) = server.serve(server_io).await else {
            return;
        };
        let _ = running.waiting().await;
    });

    let result = tokio::time::timeout(SELF_CHECK_TIMEOUT, async {
        let client = ()
            .serve(client_io)
            .await
            .map_err(|e| OrbitError::Mcp(format!("self-check failed to initialize: {e}")))?;

        let tools = client
            .list_tools(Default::default())
            .await
            .map_err(|e| OrbitError::Mcp(format!("self-check tools/list failed: {e}")))?;

        client.cancel().await.ok();

        Ok(SelfCheckReport {
            tool_count: tools.tools.len(),
            tool_names: tools
                .tools
                .into_iter()
                .map(|t| t.name.to_string())
                .collect(),
        })
    })
    .await
    .map_err(|_| OrbitError::Mcp("self-check timed out".to_string()))?;

    let _ = tokio::time::timeout(SELF_CHECK_TIMEOUT, server_task).await;
    result
}
