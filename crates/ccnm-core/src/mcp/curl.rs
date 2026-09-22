//! Streamable HTTP to an MCP server, through this machine's `curl` (P50).
//!
//! The Agent's servers at a URL -- exa, DeepWiki -- are what `call_mcp_tool`
//! on the Agent Node offers by default, and ccnm has no HTTP client: the
//! Runtime relay (P49) left HTTP servers out so that ccnm would not grow
//! one with its TLS stack. `curl` is on every macOS and on the Linux
//! machines ccnm runs on, speaks TLS and HTTP/2, and reads the proxy
//! variables the way the rest of the account's tools do.
//!
//! One `curl` per message: the body goes in on stdin, the response headers
//! and body come back on stdout (`dump-header -`). The URL and headers go
//! in a config file (`-K`) in a directory only this account can read,
//! never on the command line, where any user of the machine can see them
//! with `ps` -- exa's key is part of its URL. The reply is a JSON body or
//! an SSE stream; in a stream, reading stops at the reply to this request,
//! since a server may keep the stream open.
//!
//! What it does not do: a server behind OAuth (401) is reported as needing
//! a login -- Claude Code keeps that token, ccnm cannot use it. The old
//! HTTP+SSE transport is not spoken at all.

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::Value;
use toexec_mcp::Error;
use toexec_mcp::installed::{display_url, is_loopback_url};
use toexec_mcp::sse::{answers, awaited_id, event_data, event_end};
use toexec_mcp::transport::{MAX_MESSAGE_BYTES, Recv, Transport};

/// Longest a connection may take to open, whatever the call's own budget.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// How much of an error body is quoted back.
const SAID_BYTES: usize = 400;

static NEXT: AtomicU64 = AtomicU64::new(0);

pub struct Curl {
    url: String,
    /// The config's own headers, already checked.
    headers: Vec<(String, String)>,
    loopback: bool,
    /// Holds the `-K` file and curl's stderr; removed on close.
    dir: PathBuf,
    session: Option<String>,
    protocol: Option<String>,
    inbox: VecDeque<String>,
    closed: bool,
}

impl Curl {
    pub fn new(url: &str, headers: &BTreeMap<String, String>) -> Result<Curl, Error> {
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(Error::Start(format!(
                "the url does not start with http:// or https://: {}",
                display_url(url)
            )));
        }
        if url.contains(['\r', '\n']) {
            return Err(Error::Start("the url has a line break in it".into()));
        }
        for (name, value) in headers {
            let token = !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b));
            if !token {
                return Err(Error::Start(format!(
                    "header {name} is not a valid HTTP header name"
                )));
            }
            if value.contains(['\r', '\n']) {
                return Err(Error::Start(format!(
                    "the value of header {name} has a line break in it"
                )));
            }
        }
        let dir = std::env::temp_dir().join(format!(
            "ccnm-curl-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|e| Error::Start(format!("cannot make a private directory for curl: {e}")))?;
        Ok(Curl {
            url: url.to_string(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            loopback: is_loopback_url(url),
            dir,
            session: None,
            protocol: None,
            inbox: VecDeque::new(),
            closed: false,
        })
    }

    /// The `-K` file for one request.
    fn config(&self, method: &str, timeout: Duration) -> String {
        let mut lines = vec![
            format!("url = {}", quoted(&self.url)),
            format!("request = {}", quoted(method)),
            "silent".to_string(),
            "show-error".to_string(),
            "no-buffer".to_string(),
            "suppress-connect-headers".to_string(),
            "dump-header = \"-\"".to_string(),
            "proto = \"=http,https\"".to_string(),
            format!("max-time = \"{:.1}\"", timeout.as_secs_f64().max(1.0)),
            format!(
                "connect-timeout = \"{:.1}\"",
                timeout.min(CONNECT_TIMEOUT).as_secs_f64().max(1.0)
            ),
        ];
        // A proxy would answer for 127.0.0.1 itself: measured on the dev
        // machine, a relay through HTTP_PROXY got a 502 (toexec v4 step 2).
        if self.loopback {
            lines.push("noproxy = \"*\"".to_string());
        }
        if method == "POST" {
            lines.push("data-binary = \"@-\"".to_string());
        }
        let mut header = |name: &str, value: &str| {
            lines.push(format!("header = {}", quoted(&format!("{name}: {value}"))));
        };
        let own = |name: &str| {
            self.headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case(name))
        };
        // Without one exa's address answers 403 (Cloudflare 1010).
        if !own("user-agent") {
            header("User-Agent", concat!("ccnm/", env!("CARGO_PKG_VERSION")));
        }
        if method == "POST" {
            header("Content-Type", "application/json");
            header("Accept", "application/json, text/event-stream");
        }
        if let Some(session) = &self.session {
            header("Mcp-Session-Id", session);
        }
        if let Some(protocol) = &self.protocol {
            header("MCP-Protocol-Version", protocol);
        }
        for (name, value) in &self.headers {
            header(name, value);
        }
        lines.join("\n") + "\n"
    }

    fn spawn(&self, method: &str, timeout: Duration) -> Result<Child, Error> {
        let config = self.dir.join("request");
        let stderr = self.dir.join("stderr");
        let write = || -> std::io::Result<fs::File> {
            fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&config)?
                .write_all(self.config(method, timeout).as_bytes())?;
            fs::File::create(&stderr)
        };
        let stderr =
            write().map_err(|e| Error::Start(format!("cannot write curl's request: {e}")))?;
        Command::new("curl")
            .arg("-K")
            .arg(&config)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    Error::Start(
                        "curl is not on this machine's PATH; ccnm reaches HTTP MCP servers through it"
                            .into(),
                    )
                } else {
                    Error::Start(format!("cannot start curl: {e}"))
                }
            })
    }

    fn post(&mut self, line: &str, timeout: Duration) -> Result<(), Error> {
        let waiting_for = awaited_id(line);
        let mut child = self.spawn("POST", timeout)?;
        let result = self.exchange(&mut child, line, waiting_for.as_ref(), timeout);
        // A stream the server keeps open is cut here, reply in hand.
        let _ = child.kill();
        let status = child.wait().ok().and_then(|s| s.code());
        match result {
            Err(Exchange::NoResponse) => Err(self.curl_failed(status, timeout)),
            Err(Exchange::Failed(error)) => Err(error),
            Ok(()) => Ok(()),
        }
    }

    fn exchange(
        &mut self,
        child: &mut Child,
        line: &str,
        waiting_for: Option<&Value>,
        timeout: Duration,
    ) -> Result<(), Exchange> {
        if let Some(mut stdin) = child.stdin.take() {
            // A server that answers before reading the body closes the pipe;
            // its answer is what counts.
            let _ = stdin.write_all(line.as_bytes());
        }
        let stdout = child.stdout.take().ok_or(Exchange::NoResponse)?;
        let mut reader = BufReader::new(stdout);
        let head = read_head(&mut reader).ok_or(Exchange::NoResponse)?;
        if let Some(session) = head.header("mcp-session-id") {
            self.session = Some(session.to_string());
        }
        if head.status == 401 {
            return Err(Exchange::Failed(Error::NeedsLogin { status: 401 }));
        }
        // The session expired: the protocol wants a new handshake. Reported
        // as a closed connection, so the pool drops this one.
        if head.status == 404 && self.session.is_some() {
            return Err(Exchange::Failed(Error::Closed {
                during: String::new(),
                said: "the server no longer knows this session (HTTP 404)".into(),
            }));
        }
        if !(200..300).contains(&head.status) {
            let mut body = Vec::new();
            let _ = reader.take(SAID_BYTES as u64).read_to_end(&mut body);
            return Err(Exchange::Failed(Error::Http {
                status: head.status,
                said: String::from_utf8_lossy(&body).trim().to_string(),
            }));
        }
        let stream = head
            .header("content-type")
            .is_some_and(|value| value.starts_with("text/event-stream"));
        if stream {
            return self.read_stream(reader, waiting_for, timeout);
        }
        let mut body = Vec::new();
        reader
            .take(MAX_MESSAGE_BYTES as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|e| Exchange::Failed(closed(e.to_string())))?;
        if body.len() > MAX_MESSAGE_BYTES {
            return Err(Exchange::Failed(too_long(body.len())));
        }
        let text = String::from_utf8_lossy(&body);
        if !text.trim().is_empty() {
            self.take(text.trim());
        }
        Ok(())
    }

    /// Reads SSE until the reply to `waiting_for`, one event a message.
    fn read_stream(
        &mut self,
        mut reader: impl Read,
        waiting_for: Option<&Value>,
        timeout: Duration,
    ) -> Result<(), Exchange> {
        let mut buffer: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 16 * 1024];
        let mut total = 0usize;
        loop {
            let n = reader
                .read(&mut chunk)
                .map_err(|e| Exchange::Failed(closed(e.to_string())))?;
            if n == 0 {
                // The last event may end without its blank line.
                if let Some(data) = event_data(&buffer) {
                    self.take(&data);
                }
                // curl stops at max-time; a stream that ended without the
                // reply is a timeout when that was the reason.
                if waiting_for.is_some() && self.inbox.is_empty() {
                    return Err(Exchange::Failed(Error::Timeout {
                        during: String::new(),
                        waited: timeout,
                    }));
                }
                return Ok(());
            }
            total += n;
            if total > MAX_MESSAGE_BYTES {
                return Err(Exchange::Failed(too_long(total)));
            }
            buffer.extend_from_slice(&chunk[..n]);
            while let Some((end, skip)) = event_end(&buffer) {
                let event: Vec<u8> = buffer.drain(..end + skip).take(end).collect();
                let Some(data) = event_data(&event) else {
                    continue;
                };
                let done = waiting_for.is_some_and(|id| answers(&data, id));
                self.take(&data);
                if done {
                    return Ok(());
                }
            }
        }
    }

    /// Keeps one message, or each of a batch. The handshake's reply names
    /// the protocol version, which every later request carries.
    fn take(&mut self, text: &str) {
        let Ok(parsed) = serde_json::from_str::<Value>(text) else {
            return;
        };
        let messages = match parsed {
            Value::Array(batch) => batch,
            single => vec![single],
        };
        for message in messages {
            if let Some(version) = message
                .get("result")
                .and_then(|result| result.get("protocolVersion"))
                .and_then(Value::as_str)
            {
                self.protocol = Some(version.to_string());
            }
            self.inbox.push_back(message.to_string());
        }
    }

    /// Why curl gave no response, from its exit code and what it said.
    fn curl_failed(&self, status: Option<i32>, timeout: Duration) -> Error {
        let said = fs::read_to_string(self.dir.join("stderr"))
            .unwrap_or_default()
            .trim()
            .to_string();
        let what = display_url(&self.url);
        match status {
            // CURLE_OPERATION_TIMEDOUT
            Some(28) => Error::Timeout {
                during: String::new(),
                waited: timeout,
            },
            // Could not resolve the proxy or the host, could not connect.
            Some(5..=7) => Error::Start(format!("cannot reach {what}: {said}")),
            // TLS: handshake, certificate.
            Some(35 | 51 | 58 | 59 | 60 | 77 | 83 | 90 | 91) => {
                Error::Start(format!("TLS with {what} failed: {said}"))
            }
            _ => closed(if said.is_empty() {
                format!("curl ended without a response (exit {status:?})")
            } else {
                said
            }),
        }
    }
}

impl Transport for Curl {
    fn send(&mut self, line: &str, timeout: Duration) -> Result<(), Error> {
        if self.closed {
            return Err(closed(String::new()));
        }
        self.post(line, timeout)
    }

    /// The reply came back with `send`; this only hands it over. None means
    /// the server answered a request with nothing.
    fn recv(&mut self, _timeout: Duration) -> Recv {
        match self.inbox.pop_front() {
            Some(line) => Recv::Line(line),
            None => Recv::Closed {
                said: "the HTTP response ended without a reply".into(),
            },
        }
    }

    /// Tells the server the session is over (optional in the protocol; a
    /// failure is ignored) and removes the private directory.
    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        if self.session.is_some()
            && let Ok(mut child) = self.spawn("DELETE", Duration::from_secs(2))
        {
            drop(child.stdin.take());
            if let Some(mut out) = child.stdout.take() {
                let _ = std::io::copy(&mut out, &mut std::io::sink());
            }
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

impl Drop for Curl {
    fn drop(&mut self) {
        self.close();
    }
}

enum Exchange {
    /// curl wrote no response head: it failed before one arrived.
    NoResponse,
    Failed(Error),
}

struct Head {
    status: u16,
    headers: Vec<(String, String)>,
}

impl Head {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// The response's head: status line and headers up to the blank line.
/// Informational responses (`100 Continue`) come first with their own
/// blank line and are skipped. `None` when curl wrote nothing that looks
/// like one.
fn read_head(reader: &mut impl BufRead) -> Option<Head> {
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let status: u16 = line
            .strip_prefix("HTTP/")?
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()?;
        let mut headers = Vec::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).ok()? == 0 {
                break;
            }
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                headers.push((name.trim().to_string(), value.trim().to_string()));
            }
        }
        if !(100..200).contains(&status) {
            return Some(Head { status, headers });
        }
    }
}

/// A value in curl's config-file syntax: double quotes, with `\` and `"`
/// escaped.
fn quoted(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn closed(said: String) -> Error {
    Error::Closed {
        during: String::new(),
        said,
    }
}

fn too_long(bytes: usize) -> Error {
    Error::Protocol(format!(
        "the server sent more than {MAX_MESSAGE_BYTES} bytes in one reply ({bytes} so far)"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use toexec_mcp::Client;

    /// What one request carried.
    #[derive(Debug, Default, Clone)]
    struct Heard {
        method: String,
        agent: Option<String>,
        session: Option<String>,
        key: Option<String>,
    }

    /// A streamable-HTTP MCP server on 127.0.0.1, one request per
    /// connection: JSON for initialize, SSE (a notification first, then
    /// the reply) for everything else, 401 when `login` is set.
    fn serve(login: bool) -> (String, Arc<Mutex<Vec<Heard>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let heard = Arc::new(Mutex::new(Vec::new()));
        let log = heard.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                if reader.read_line(&mut first).is_err() {
                    continue;
                }
                let mut heard = Heard {
                    method: first.split_whitespace().next().unwrap_or("").to_string(),
                    ..Heard::default()
                };
                let mut length = 0usize;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let line = line.trim_end();
                    if line.is_empty() {
                        break;
                    }
                    let (name, value) = line.split_once(':').unwrap();
                    let value = value.trim().to_string();
                    match name.to_ascii_lowercase().as_str() {
                        "content-length" => length = value.parse().unwrap(),
                        "user-agent" => heard.agent = Some(value),
                        "mcp-session-id" => heard.session = Some(value),
                        "x-api-key" => heard.key = Some(value),
                        _ => {}
                    }
                }
                let mut body = vec![0u8; length];
                reader.read_exact(&mut body).unwrap();
                log.lock().unwrap().push(heard);
                let mut out = stream;
                if login {
                    let _ = out.write_all(
                        b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                    );
                    continue;
                }
                let message: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let Some(id) = message.get("id").cloned() else {
                    let _ = out.write_all(
                        b"HTTP/1.1 202 Accepted\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                    );
                    continue;
                };
                let result = match message["method"].as_str() {
                    Some("initialize") => serde_json::json!({
                        "protocolVersion": "2025-06-18",
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "web", "version": "1"}
                    }),
                    Some("tools/list") => serde_json::json!({"tools": [
                        {"name": "echo", "inputSchema": {"type": "object"}}
                    ]}),
                    _ => serde_json::json!({"content": [
                        {"type": "text", "text": message["params"]["arguments"].to_string()}
                    ]}),
                };
                let reply = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
                if message["method"] == "initialize" {
                    let body = reply.to_string();
                    let _ = write!(
                        out,
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nmcp-session-id: s-1\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                } else {
                    // The stream stays open after the reply: the client
                    // has to stop reading by itself.
                    let _ = write!(
                        out,
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n: hello\n\nevent: message\ndata: {{\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\"}}\n\ndata: {reply}\n\n"
                    );
                    let _ = out.flush();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_secs(30));
                        drop(out);
                    });
                }
            }
        });
        (url, heard)
    }

    fn headers(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn a_session_is_carried_and_an_sse_reply_is_picked_out_of_an_open_stream() {
        let (url, heard) = serve(false);
        let transport = Curl::new(&url, &headers(&[("x-api-key", "k-1")])).unwrap();
        let dir = transport.dir.clone();
        let started = std::time::Instant::now();
        let mut client = Client::connect(
            Box::new(transport),
            ("ccnm", "test"),
            Duration::from_secs(20),
        )
        .unwrap();
        let tools = client.list_tools(Duration::from_secs(20)).unwrap();
        assert_eq!(tools[0]["name"], "echo");
        let result = client
            .call_tool(
                "echo",
                serde_json::json!({"q": {"nested": [1, 2]}}),
                Duration::from_secs(20),
            )
            .unwrap();
        assert_eq!(result["content"][0]["text"], r#"{"q":{"nested":[1,2]}}"#);
        // Not held for the 30 seconds the stream stays open.
        assert!(started.elapsed() < Duration::from_secs(20));

        let heard = heard.lock().unwrap().clone();
        assert!(heard.iter().all(|h| h.method == "POST"));
        assert!(heard.iter().all(|h| h.key.as_deref() == Some("k-1")));
        assert!(
            heard
                .iter()
                .all(|h| h.agent.as_deref().is_some_and(|a| a.starts_with("ccnm/")))
        );
        assert_eq!(heard[0].session, None);
        assert!(
            heard[1..]
                .iter()
                .all(|h| h.session.as_deref() == Some("s-1"))
        );
        // The key was in a file only this account can read, and never on
        // curl's command line.
        let mode = fs::metadata(&dir).unwrap().permissions();
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777,
            0o700
        );
        drop(client);
        assert!(!dir.exists(), "the private directory is removed on close");
    }

    #[test]
    fn a_login_wall_is_named_as_such() {
        let (url, _) = serve(true);
        let error = match Client::connect(
            Box::new(Curl::new(&url, &BTreeMap::new()).unwrap()),
            ("ccnm", "test"),
            Duration::from_secs(20),
        ) {
            Err(error) => error,
            Ok(_) => panic!("a 401 must not connect"),
        };
        assert!(
            matches!(error, Error::NeedsLogin { status: 401 }),
            "{error}"
        );
    }

    #[test]
    fn nobody_listening_is_a_start_failure() {
        let port = {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let error = match Client::connect(
            Box::new(Curl::new(&format!("http://127.0.0.1:{port}/mcp"), &BTreeMap::new()).unwrap()),
            ("ccnm", "test"),
            Duration::from_secs(20),
        ) {
            Err(error) => error,
            Ok(_) => panic!("nothing listens there"),
        };
        assert!(matches!(error, Error::Start(_)), "{error}");
    }

    #[test]
    fn a_bad_url_or_header_is_refused_before_curl_runs() {
        assert!(Curl::new("ftp://example.invalid/mcp", &BTreeMap::new()).is_err());
        assert!(Curl::new("https://example.invalid/\nmcp", &BTreeMap::new()).is_err());
        assert!(
            Curl::new(
                "https://example.invalid/mcp",
                &headers(&[("bad name", "v")])
            )
            .is_err()
        );
        assert!(Curl::new("https://example.invalid/mcp", &headers(&[("x", "a\r\nb")])).is_err());
    }

    #[test]
    fn config_values_are_quoted_for_curl() {
        assert_eq!(quoted(r#"a"b\c"#), r#""a\"b\\c""#);
    }
}
