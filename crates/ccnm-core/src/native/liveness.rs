//! Noticing that the Codex on the other end of an exec-server chain is gone
//! (P26).
//!
//! An Agent that drops off the network without closing anything leaves this
//! side reading a stdin that never ends: no FIN, no RST, so sshd waits and
//! the supervisor keeps the workspace write guard for a session nobody is
//! in (P24 blackhole, 5 of 5). The MCP entry does not have this problem
//! because its heartbeat puts bytes on the connection; the exec-server
//! protocol has no ping of its own for that.
//!
//! What it has is Codex 0.154.0's client answering every server-to-client
//! **request** it does not know with `-32601` and carrying on (its
//! `exec-server/src/client_recovery.rs`; measured in P26.1: 155 requests
//! idle and mid-command, every one answered within 11 ms, no reconnect,
//! nothing shown in the TUI). An unknown **notification** would make it drop the
//! connection instead, so a liveness message here is always a request.
//!
//! The rule, and why it is not the MCP rule: after [`PING_AFTER`] of client
//! silence, ask; after [`GIVE_UP_AFTER`] without a single byte from the
//! client, end the session through the normal shutdown. MCP never ends a
//! session over silence, because a sleeping peer keeps its TCP and comes
//! back. Here the user chose the other trade: a laptop asleep or offline for
//! longer than ten minutes loses its native session (Codex has to `/exit`
//! and start again), in exchange for the workspace not staying locked until
//! somebody kills an sshd by hand.

use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

/// Client silence before asking. The MCP heartbeat's value: the same cost
/// for the same kind of connection.
pub const PING_AFTER: Duration = crate::mcp::server::HEARTBEAT;

/// Client silence after which the session is over. Twenty unanswered asks.
/// Shorter takes a native session away from an Agent that only lost Wi-Fi
/// for a few minutes; longer keeps the workspace locked that much longer
/// after one that is really gone.
pub const GIVE_UP_AFTER: Duration = Duration::from_secs(10 * 60);

/// Every liveness request's id starts with this. A string, while Codex's own
/// requests are numbered, so an answer can never be mistaken for one to
/// exec-server.
const ID_PREFIX: &str = "ccnm-liveness-";

#[derive(Debug, Clone, Copy)]
pub struct Timing {
    pub ping_after: Duration,
    pub give_up_after: Duration,
    /// How often the supervisor looks. Sets how late a ping or the give-up
    /// can be, nothing else.
    pub tick: Duration,
}

impl Timing {
    pub const DEFAULT: Timing = Timing {
        ping_after: PING_AFTER,
        give_up_after: GIVE_UP_AFTER,
        tick: Duration::from_secs(1),
    };
}

#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Wait,
    Ping,
    GiveUp,
}

/// What to do at `now`, given when the client was last heard from and when
/// the last ping went out. Asks again every `ping_after` for as long as the
/// silence lasts: an ask costs one line, and one that sat behind a stuck
/// write should not be the only one.
pub fn step(timing: &Timing, now: Instant, heard: Instant, last_ping: Option<Instant>) -> Step {
    let silence = now.saturating_duration_since(heard);
    if silence >= timing.give_up_after {
        return Step::GiveUp;
    }
    if silence < timing.ping_after {
        return Step::Wait;
    }
    match last_ping {
        Some(ping) if ping >= heard && now.saturating_duration_since(ping) < timing.ping_after => {
            Step::Wait
        }
        _ => Step::Ping,
    }
}

/// When the client was last heard from, shared by the threads that can hear
/// it. Only moves forward.
#[derive(Debug)]
pub struct Heard {
    base: Instant,
    millis: AtomicU64,
}

impl Heard {
    pub fn new() -> Arc<Heard> {
        Arc::new(Heard {
            base: Instant::now(),
            millis: AtomicU64::new(0),
        })
    }

    pub fn touch(&self) {
        let millis = u64::try_from(self.base.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.millis.fetch_max(millis, Ordering::Relaxed);
    }

    pub fn at(&self) -> Instant {
        self.base + Duration::from_millis(self.millis.load(Ordering::Relaxed))
    }
}

/// A reader that counts every byte from the client as hearing from it, not
/// every whole message: a 30 MiB `fs/writeFile` crawling over a slow link is
/// a client that is there.
pub struct Touching<R> {
    inner: R,
    heard: Arc<Heard>,
}

impl<R> Touching<R> {
    pub fn new(inner: R, heard: Arc<Heard>) -> Self {
        Touching { inner, heard }
    }
}

impl<R: Read> Read for Touching<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.heard.touch();
        }
        Ok(n)
    }
}

/// The `n`th liveness request, newline included. No `jsonrpc` field, like
/// every other message on this connection.
pub fn ping(n: u64) -> Vec<u8> {
    let mut line = serde_json::json!({
        "id": format!("{ID_PREFIX}{n}"),
        "method": "ccnm/liveness",
        "params": {},
    })
    .to_string()
    .into_bytes();
    line.push(b'\n');
    line
}

/// Whether a client message answers a liveness request. Those are this
/// supervisor's, and exec-server never asked them.
pub fn is_answer(message: &Value) -> bool {
    message.get("method").is_none()
        && message
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| id.starts_with(ID_PREFIX))
}

#[cfg(test)]
mod tests;
