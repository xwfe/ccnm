//! An MCP client that spawns a transport command (`ssh ... ccnm internal
//! mcp-serve`, or the same binary locally), performs `initialize` and
//! `tools/list`, calls `workspace_info` N times, and reports what it saw.
//! Doctor uses it with N = 1 as the "Remote MCP handshake" row; `ccnm mcp
//! probe` uses N = 100 for the persistence proof of design doc section 27.
//!
//! Cleanup is part of the contract: the client closes the server's stdin,
//! waits for it to exit, and kills it if it does not, so doctor never
//! leaves a server behind (section 4).

use std::process::Stdio;
use std::time::{Duration, Instant};

use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::TokioChildProcess;
use tokio::io::AsyncReadExt as _;

use crate::error::{Error, ErrorCode, Result};
use crate::mcp::server::WorkspaceInfo;
use crate::process::Cmd;
use crate::protocol::mcp::ProbeReport;

/// How much of the transport's stderr is kept for an error message.
const STDERR_KEEP: usize = 4096;

/// Spawn `transport`, speak MCP to it `calls` times, shut it down.
/// `unreachable` is the code for a transport that never answered, since
/// which side is unreachable depends on where this runs.
pub fn probe(
    transport: &Cmd,
    calls: u32,
    timeout: Duration,
    unreachable: ErrorCode,
) -> Result<ProbeReport> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::internal("cannot start tokio runtime").with_source(e))?;
    rt.block_on(async {
        match tokio::time::timeout(timeout, run(transport, calls, unreachable)).await {
            Ok(result) => result,
            Err(_) => Err(Error::new(
                unreachable,
                format!(
                    "MCP probe of `{}` timed out after {timeout:?}",
                    transport.display()
                ),
            )),
        }
    })
}

async fn run(transport: &Cmd, calls: u32, unreachable: ErrorCode) -> Result<ProbeReport> {
    let mut command = tokio::process::Command::new(&transport.program);
    command.args(&transport.args);
    if let Some(dir) = &transport.cwd {
        command.current_dir(dir);
    }
    for key in &transport.env_remove {
        command.env_remove(key);
    }
    for (key, value) in &transport.env {
        command.env(key, value);
    }
    tracing::debug!(cmd = %transport.display(), "spawning MCP transport");

    let (child, stderr) = TokioChildProcess::builder(command)
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            Error::internal(format!(
                "cannot spawn {}",
                transport.program.to_string_lossy()
            ))
            .with_source(e)
        })?;
    // Drain stderr on its own task so a chatty ssh cannot block the child,
    // and keep the tail for error messages.
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut pipe) = stderr {
            let _ = pipe.read_to_end(&mut buf).await;
        }
        buf
    });
    let started = Instant::now();
    let client = match ().serve(child).await {
        Ok(client) => client,
        Err(e) => {
            let tail = stderr_tail(stderr_task).await;
            return Err(Error::new(
                unreachable,
                format!(
                    "MCP initialize failed over `{}`: {e}{tail}",
                    transport.display()
                ),
            ));
        }
    };
    let connect_us = elapsed_us(started);

    let info = client
        .peer_info()
        .ok_or_else(|| Error::internal("initialize succeeded but peer info is missing"))?;
    let (server_name, server_version) = info
        .server_info
        .as_ref()
        .map(|s| (s.name.clone(), s.version.clone()))
        .unwrap_or_default();
    let instructions_bytes = info.instructions.as_deref().map_or(0, str::len);

    let list = client
        .list_tools(None)
        .await
        .map_err(|e| Error::internal("tools/list failed").with_source(e))?;
    let tools_list_bytes = serde_json::to_string(&list)
        .map_err(|e| Error::internal("cannot serialize tools/list").with_source(e))?
        .len();
    let tools: Vec<String> = list.tools.iter().map(|t| t.name.to_string()).collect();

    let mut samples: Vec<u64> = Vec::with_capacity(calls as usize);
    let mut server_pid = 0u32;
    let mut single_process = true;
    for i in 0..calls {
        let t = Instant::now();
        let result = client
            .call_tool(CallToolRequestParams::new("workspace_info"))
            .await
            .map_err(|e| {
                Error::internal(format!("workspace_info call {} failed", i + 1)).with_source(e)
            })?;
        samples.push(elapsed_us(t));
        // The pid and counter are in the text, the one channel the model
        // is shown too, so the probe reads what the model would read.
        let text: String = result
            .content
            .iter()
            .filter_map(|block| block.as_text())
            .map(|t| t.text.as_str())
            .collect();
        let (pid, calls_served) = WorkspaceInfo::parse_server_line(&text).ok_or_else(|| {
            Error::internal(format!(
                "workspace_info did not end with its [server pid ..] line; got: {text:?}"
            ))
        })?;
        if i == 0 {
            server_pid = pid;
        } else if pid != server_pid {
            single_process = false;
        }
        if calls_served != u64::from(i) + 1 {
            single_process = false;
        }
    }

    // Close stdin, wait for the server to exit, kill it if it does not.
    let quit = client
        .cancel()
        .await
        .map_err(|e| Error::internal("MCP shutdown").with_source(e))?;
    tracing::debug!(?quit, "MCP transport closed");
    let _ = stderr_task.await;

    samples.sort_unstable();
    Ok(ProbeReport {
        connect_us,
        server_name,
        server_version,
        instructions_bytes,
        tools,
        tools_list_bytes,
        calls,
        call_p50_us: percentile(&samples, 0.50),
        call_p95_us: percentile(&samples, 0.95),
        call_max_us: samples.last().copied().unwrap_or(0),
        server_pid,
        single_process,
    })
}

/// The last [`STDERR_KEEP`] bytes the transport wrote, formatted for an
/// error message; empty when it wrote nothing.
async fn stderr_tail(task: tokio::task::JoinHandle<Vec<u8>>) -> String {
    let buf = task.await.unwrap_or_default();
    let text = String::from_utf8_lossy(&buf);
    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }
    let mut start = text.len().saturating_sub(STDERR_KEEP);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("\nstderr: {}", &text[start..])
}

fn elapsed_us(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_micros()).unwrap_or(u64::MAX)
}

/// Nearest-rank percentile over sorted samples; 0 when empty.
fn percentile(sorted: &[u64], q: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = ((sorted.len() as f64 - 1.0) * q).round() as usize;
    sorted[rank.min(sorted.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_is_nearest_rank() {
        assert_eq!(percentile(&[], 0.5), 0);
        assert_eq!(percentile(&[7], 0.95), 7);
        let s: Vec<u64> = (1..=100).collect();
        assert_eq!(percentile(&s, 0.50), 51);
        assert_eq!(percentile(&s, 0.95), 95);
    }

    #[test]
    fn unspawnable_transport_is_internal() {
        let cmd = Cmd::new("ccnm-definitely-not-installed");
        let err = probe(&cmd, 1, Duration::from_secs(5), ErrorCode::HomeUnreachable).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Internal);
        assert!(
            err.message().contains("ccnm-definitely-not-installed"),
            "{err}"
        );
    }

    #[test]
    fn a_process_that_is_not_an_mcp_server_is_unreachable_with_its_stderr() {
        let cmd = Cmd::new("sh").args(["-c", "echo nope >&2; exit 3"]);
        let err = probe(&cmd, 1, Duration::from_secs(5), ErrorCode::HomeUnreachable).unwrap_err();
        assert_eq!(err.code(), ErrorCode::HomeUnreachable);
        assert!(err.message().contains("stderr: nope"), "{err}");
    }

    #[test]
    fn a_silent_process_times_out() {
        let cmd = Cmd::new("sleep").arg("30");
        let started = Instant::now();
        let err = probe(
            &cmd,
            1,
            Duration::from_millis(300),
            ErrorCode::WorkUnreachable,
        )
        .unwrap_err();
        assert_eq!(err.code(), ErrorCode::WorkUnreachable);
        assert!(err.message().contains("timed out"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
