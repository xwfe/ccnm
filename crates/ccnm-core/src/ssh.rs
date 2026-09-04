//! Building ssh command lines the ccnm way.
//!
//! ccnm owns multiplexing, not identity (design doc section 18): the alias,
//! HostName, User, IdentityFile and ProxyJump come from the user's
//! `~/.ssh/config`. ccnm only appends ControlMaster / ControlPath /
//! BatchMode and the SendEnv overrides on the command line, where OpenSSH
//! gives them precedence over the config file.
//!
//! Nothing here goes through a shell on this side, and every argument sent
//! to the remote side is checked against a no-quoting-needed character set,
//! so the remote login shell cannot misparse it either.

use std::path::PathBuf;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::config::{DEFAULT_CCNM_BIN, is_token};
use crate::error::{Error, ErrorCode, Result};
use crate::process::{Cmd, Output, ProcessRunner};
use crate::protocol::payload::{self, Protocol};

/// How long an idle master stays in the background.
pub const CONTROL_PERSIST: &str = "10m";
/// Seconds ssh waits for the TCP connection before giving up.
pub const CONNECT_TIMEOUT: &str = "10";
/// `sun_path[104]` on macOS: OpenSSH fails with "ControlPath too long" once
/// the expanded path reaches 104 bytes.
pub const CONTROL_PATH_MAX_LEN: usize = 103;
/// `%C` expands to a 40-character hex hash.
const HASH_LEN: usize = 40;

/// Whether this invocation may leave a master connection behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Master {
    /// `ControlMaster=no`: use a running master if there is one, otherwise a
    /// plain connection. Never starts a background process, which is what
    /// keeps doctor read-only.
    Reuse,
    /// `ControlMaster=auto`: start a master (kept for [`CONTROL_PERSIST`])
    /// if none is running. For commands that will be followed by many more.
    Auto,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ssh {
    alias: String,
    control_dir: PathBuf,
    /// Path of ccnm on the far side, as one word of the remote command.
    ccnm_bin: String,
}

/// What `ssh -G` says the alias will actually connect to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedSsh {
    pub hostname: String,
    pub user: String,
    pub port: u16,
    pub identity_files: Vec<String>,
    pub proxy_jump: Option<String>,
}

impl ResolvedSsh {
    /// `user@hostname`, or just `hostname` when ssh -G gave no user.
    pub fn target(&self) -> String {
        if self.user.is_empty() {
            self.hostname.clone()
        } else {
            format!("{}@{}", self.user, self.hostname)
        }
    }
}

/// How a remote invocation ended, before looking at its output.
#[derive(Debug)]
pub enum RemoteOutcome {
    /// ssh itself failed (exit 255) or the call timed out. The remote
    /// command never ran, or its result never arrived.
    Unreachable(String),
    /// The remote shell could not find the command (exit 127).
    CommandNotFound,
    /// The remote command ran; inspect the output.
    Completed(Output),
}

impl Ssh {
    /// `alias` must be a plain token so it can never be read as an option.
    pub fn new(alias: &str, control_dir: impl Into<PathBuf>) -> Result<Self> {
        if !is_token(alias) {
            return Err(Error::config(format!(
                "ssh alias must match [A-Za-z0-9._-]+ and not start with '-', got \"{alias}\""
            )));
        }
        Ok(Ssh {
            alias: alias.to_string(),
            control_dir: control_dir.into(),
            ccnm_bin: DEFAULT_CCNM_BIN.to_string(),
        })
    }

    /// Where ccnm lives on the far side (`hosts.<x>.ccnm_bin`, design doc
    /// section 7). Defaults to [`DEFAULT_CCNM_BIN`].
    pub fn with_ccnm_bin(mut self, bin: impl Into<String>) -> Self {
        self.ccnm_bin = bin.into();
        self
    }

    pub fn alias(&self) -> &str {
        &self.alias
    }

    pub fn ccnm_bin(&self) -> &str {
        &self.ccnm_bin
    }

    /// The ControlPath template handed to ssh, with `%C` unexpanded.
    pub fn control_path(&self) -> PathBuf {
        self.control_dir.join("%C")
    }

    /// Fail early with a readable message instead of letting ssh report
    /// "ControlPath too long" for every command.
    pub fn check_control_path(&self) -> Result<()> {
        let expanded = self.control_dir.as_os_str().len() + 1 + HASH_LEN;
        if expanded > CONTROL_PATH_MAX_LEN {
            return Err(Error::config(format!(
                "ControlPath {} expands to {expanded} bytes; macOS allows {CONTROL_PATH_MAX_LEN}\nmove the state directory somewhere shorter with XDG_STATE_HOME",
                self.control_path().display()
            )));
        }
        Ok(())
    }

    /// The `-o` pairs ccnm adds to every connection.
    pub fn options(&self, master: Master) -> Vec<String> {
        let master = match master {
            Master::Reuse => "no",
            Master::Auto => "auto",
        };
        [
            "BatchMode=yes".to_string(),
            format!("ConnectTimeout={CONNECT_TIMEOUT}"),
            format!("ControlMaster={master}"),
            format!("ControlPath={}", self.control_path().display()),
            format!("ControlPersist={CONTROL_PERSIST}"),
            "ServerAliveInterval=15".to_string(),
            "ServerAliveCountMax=3".to_string(),
            // Design doc section 32: clear any SendEnv the user's config
            // added, so Anthropic credentials never ride along.
            "SendEnv=-ANTHROPIC_*".to_string(),
            "SendEnv=-CLAUDE_*".to_string(),
        ]
        .into_iter()
        .flat_map(|opt| ["-o".to_string(), opt])
        .collect()
    }

    /// The `-o` pairs for the long-lived MCP transport (design doc
    /// sections 11 and 12). Unlike [`options`](Self::options) it never
    /// touches a ControlMaster: the session owns its own connection and
    /// must not die with a master that some other command started.
    /// `ClearAllForwardings` keeps a user's `LocalForward` lines out of a
    /// session that only needs stdio.
    pub fn transport_options(&self) -> Vec<String> {
        [
            "BatchMode=yes",
            "ConnectTimeout=10",
            "ClearAllForwardings=yes",
            "ControlMaster=no",
            "ControlPath=none",
            // Five minutes of silence before this connection is declared
            // dead, against 45 seconds for a control command. The
            // asymmetry is deliberate: a doctor probe should fail fast,
            // but this connection *is* the session's tools, and losing it
            // costs the person a `/mcp` reconnect. A laptop that slept for
            // two minutes, a Wi-Fi handover, a router reboot -- the TCP
            // survives all of those and so should the session.
            "ServerAliveInterval=15",
            "ServerAliveCountMax=20",
            "SendEnv=-ANTHROPIC_*",
            "SendEnv=-CLAUDE_*",
        ]
        .into_iter()
        .flat_map(|opt| ["-o".to_string(), opt.to_string()])
        .collect()
    }

    /// The exact command Claude Code's mcp.json will run: one ssh whose
    /// stdin/stdout are the MCP stream. `payload` is the encoded
    /// [`crate::protocol::mcp::ServePayload`].
    pub fn mcp_transport_cmd(&self, payload: &str) -> Result<Cmd> {
        let argv = [
            self.ccnm_bin.as_str(),
            "internal",
            "mcp-serve",
            "--payload",
            payload,
        ];
        if let Some(bad) = argv.iter().find(|a| !is_remote_safe(a)) {
            return Err(Error::internal(format!(
                "refusing to send `{bad}` over ssh: it would need shell quoting"
            )));
        }
        Ok(Cmd::new("ssh")
            .args(self.transport_options())
            .arg("-T")
            .arg(&self.alias)
            .args(argv))
    }

    /// `ssh -G alias`: print the resolved configuration without connecting.
    pub fn resolve_cmd(&self) -> Cmd {
        Cmd::new("ssh")
            .arg("-G")
            .arg(&self.alias)
            .timeout(Duration::from_secs(10))
    }

    /// `ssh -O check alias`: ask whether a master is running.
    pub fn check_master_cmd(&self) -> Cmd {
        self.control_cmd("check")
    }

    /// `ssh -O exit alias`: tell the master to shut down.
    pub fn exit_master_cmd(&self) -> Cmd {
        self.control_cmd("exit")
    }

    fn control_cmd(&self, ctl: &str) -> Cmd {
        Cmd::new("ssh")
            .arg("-o")
            .arg(format!("ControlPath={}", self.control_path().display()))
            .arg("-O")
            .arg(ctl)
            .arg(&self.alias)
            .timeout(Duration::from_secs(10))
    }

    /// `ssh <options> -T alias argv...`. Every element of `argv` must pass
    /// [`is_remote_safe`]; ccnm never sends anything that needs quoting.
    pub fn remote_cmd(&self, master: Master, argv: &[&str], timeout: Duration) -> Result<Cmd> {
        if let Some(bad) = argv.iter().find(|a| !is_remote_safe(a)) {
            return Err(Error::internal(format!(
                "refusing to send `{bad}` over ssh: it would need shell quoting"
            )));
        }
        Ok(Cmd::new("ssh")
            .args(self.options(master))
            .arg("-T")
            .arg(&self.alias)
            .args(argv)
            .timeout(timeout))
    }

    /// `ssh <options> -t alias <remote ccnm> subcommand...`: the far side
    /// gets a terminal.
    ///
    /// The only ccnm command that asks for one. Like
    /// [`call_ccnm`](Self::call_ccnm) it puts the remote ccnm binary at the
    /// front — the subcommand alone would be run as a program name, and the
    /// login shell would answer `command not found: internal`. It carries
    /// no timeout: what is on the other end is a person's session, and a
    /// watchdog that killed it after N seconds would be a bug, not a safety
    /// net. The caller runs it with [`crate::process::run_attached`].
    pub fn interactive_ccnm_cmd(&self, subcommand: &[&str]) -> Result<Cmd> {
        let mut argv: Vec<&str> = vec![self.ccnm_bin.as_str()];
        argv.extend_from_slice(subcommand);
        if let Some(bad) = argv.iter().find(|a| !is_remote_safe(a)) {
            return Err(Error::internal(format!(
                "refusing to send `{bad}` over ssh: it would need shell quoting"
            )));
        }
        Ok(Cmd::new("ssh")
            .args(self.options(Master::Reuse))
            .arg("-t")
            .arg(&self.alias)
            .args(argv))
    }

    pub fn resolve(&self, runner: &dyn ProcessRunner) -> Result<ResolvedSsh> {
        let out = runner.run(&self.resolve_cmd())?;
        if !out.success() {
            return Err(Error::config(format!(
                "ssh -G {} failed: {}",
                self.alias,
                out.stderr_lossy().trim()
            )));
        }
        parse_resolved(&out.stdout_lossy())
    }

    pub fn master_running(&self, runner: &dyn ProcessRunner) -> Result<bool> {
        Ok(runner.run(&self.check_master_cmd())?.success())
    }

    /// Run `<ccnm_bin> <subcommand> --payload <request>` on the other
    /// machine and decode its JSON reply. `unreachable` is the code to
    /// report when ssh itself fails, since which side is unreachable
    /// depends on the caller.
    pub fn call_ccnm<Req, Rep>(
        &self,
        runner: &dyn ProcessRunner,
        master: Master,
        subcommand: &[&str],
        request: &Req,
        timeout: Duration,
        unreachable: ErrorCode,
    ) -> Result<Rep>
    where
        Req: Serialize,
        Rep: DeserializeOwned + Protocol,
    {
        let ccnm_bin = self.ccnm_bin.as_str();
        let wire = payload::encode(request)?;
        let mut argv: Vec<&str> = vec![ccnm_bin];
        argv.extend_from_slice(subcommand);
        argv.push("--payload");
        argv.push(&wire);
        let cmd = self.remote_cmd(master, &argv, timeout)?;
        tracing::debug!(alias = %self.alias, sub = ?subcommand, "calling remote ccnm");
        let out = runner.run(&cmd)?;
        match classify(out) {
            RemoteOutcome::Unreachable(why) => Err(Error::new(
                unreachable,
                format!("ssh {}: {why}", self.alias),
            )),
            RemoteOutcome::CommandNotFound => Err(Error::new(
                ErrorCode::Version,
                format!(
                    "{ccnm_bin} not found on {} (the login shell exited 127)\ninstall the same ccnm build there, or set ccnm_bin for that host in config.toml",
                    self.alias
                ),
            )),
            RemoteOutcome::Completed(out) if !out.success() => {
                Err(remote_failure(&self.alias, subcommand, &out))
            }
            RemoteOutcome::Completed(out) => payload::decode_json(&out.stdout),
        }
    }
}

/// Characters that no POSIX shell treats specially, so a remote command
/// line built from them means the same thing on every login shell. `~` is
/// the one deliberate exception: a leading `~/` is expanded to the remote
/// home by every login shell, which is exactly how the default ccnm path
/// is found (design doc section 7).
pub fn is_remote_safe(arg: &str) -> bool {
    !arg.is_empty()
        && arg.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '-' | '_' | '.' | '/' | '=' | ':' | '@' | '+' | ',' | '~')
        })
}

/// Parse `ssh -G` output: one `key value` per line, keys lowercase.
pub fn parse_resolved(text: &str) -> Result<ResolvedSsh> {
    let mut resolved = ResolvedSsh {
        hostname: String::new(),
        user: String::new(),
        port: 22,
        identity_files: Vec::new(),
        proxy_jump: None,
    };
    for line in text.lines() {
        let (key, value) = match line.split_once(' ') {
            Some((k, v)) => (k, v.trim()),
            None => (line, ""),
        };
        match key {
            "hostname" => resolved.hostname = value.to_string(),
            "user" => resolved.user = value.to_string(),
            "port" => resolved.port = value.parse().unwrap_or(22),
            "identityfile" => resolved.identity_files.push(value.to_string()),
            "proxyjump" if !value.is_empty() && value != "none" => {
                resolved.proxy_jump = Some(value.to_string());
            }
            _ => {}
        }
    }
    if resolved.hostname.is_empty() {
        return Err(Error::config("ssh -G printed no hostname"));
    }
    Ok(resolved)
}

/// ssh exits 255 for its own failures (connect, auth, host key) and passes
/// the remote command's status through otherwise; 127 is the remote shell
/// saying "command not found".
pub fn classify(out: Output) -> RemoteOutcome {
    if out.timed_out {
        return RemoteOutcome::Unreachable(format!("timed out after {:?}", out.duration));
    }
    match out.exit_code {
        Some(255) => {
            let stderr = out.stderr_lossy();
            let why = stderr
                .lines()
                .rev()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("ssh exited 255 without a message");
            RemoteOutcome::Unreachable(why.to_string())
        }
        Some(127) => RemoteOutcome::CommandNotFound,
        _ => RemoteOutcome::Completed(out),
    }
}

/// The remote ccnm printed `CCNM_E_X:` on its first stderr line if it
/// failed for a known reason; keep that code instead of flattening
/// everything to Internal.
fn remote_failure(alias: &str, subcommand: &[&str], out: &Output) -> Error {
    let stderr = out.stderr_lossy();
    let stderr = stderr.trim();
    let mut lines = stderr.lines();
    let (code, rest) = match lines
        .next()
        .and_then(|first| first.strip_suffix(':'))
        .and_then(ErrorCode::from_name)
    {
        Some(code) => (code, lines.collect::<Vec<_>>().join("\n")),
        None => (ErrorCode::Internal, stderr.to_string()),
    };
    let exit = out
        .exit_code
        .map_or_else(|| "signal".to_string(), |c| c.to_string());
    let rest = if rest.trim().is_empty() {
        "no output".to_string()
    } else {
        rest
    };
    Error::new(
        code,
        format!(
            "ccnm {} on {alias} failed (exit {exit}): {rest}",
            subcommand.join(" ")
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{FakeRunner, SystemRunner};

    fn ssh() -> Ssh {
        Ssh::new("work", "/Users/me/.local/state/ccnm/ssh").unwrap()
    }

    #[test]
    fn options_add_only_multiplexing_and_safety() {
        let opts = ssh().options(Master::Reuse);
        let pairs: Vec<&str> = opts.iter().map(String::as_str).collect();
        assert_eq!(
            pairs,
            [
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                "-o",
                "ControlMaster=no",
                "-o",
                "ControlPath=/Users/me/.local/state/ccnm/ssh/%C",
                "-o",
                "ControlPersist=10m",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=3",
                "-o",
                "SendEnv=-ANTHROPIC_*",
                "-o",
                "SendEnv=-CLAUDE_*",
            ]
        );
        assert!(
            ssh()
                .options(Master::Auto)
                .contains(&"ControlMaster=auto".to_string())
        );
        // Identity is the user's business: never an -i, never a HostName.
        assert!(
            !opts
                .iter()
                .any(|o| o.starts_with("HostName") || o.starts_with("IdentityFile") || o == "-i")
        );
    }

    /// Caught in the real thing: without the binary at the front, the
    /// login shell is handed `internal` as a program name and answers
    /// `command not found: internal`, which reads like a broken install
    /// rather than a malformed command line.
    #[test]
    fn an_interactive_command_asks_for_a_terminal_and_names_the_remote_binary() {
        let cmd = ssh()
            .with_ccnm_bin("~/.local/bin/ccnm")
            .interactive_ccnm_cmd(&["internal", "attach", "--payload", "abc_-9"])
            .unwrap();
        let text = cmd.display();
        assert!(
            text.ends_with("-t work ~/.local/bin/ccnm internal attach --payload abc_-9"),
            "{text}"
        );
        assert!(!text.contains("-T "), "an attach needs a terminal: {text}");
    }

    #[test]
    fn remote_cmd_layout() {
        let cmd = ssh()
            .remote_cmd(
                Master::Reuse,
                &[
                    "~/.local/bin/ccnm",
                    "internal",
                    "probe",
                    "--payload",
                    "abc_-9",
                ],
                Duration::from_secs(5),
            )
            .unwrap();
        let text = cmd.display();
        assert!(text.starts_with("ssh -o BatchMode=yes"), "{text}");
        assert!(
            text.ends_with("-T work ~/.local/bin/ccnm internal probe --payload abc_-9"),
            "{text}"
        );
        assert_eq!(cmd.timeout, Duration::from_secs(5));
        assert!(cmd.stdin.is_none(), "stdin must be /dev/null");
    }

    #[test]
    fn remote_cmd_refuses_anything_needing_quotes() {
        for bad in ["a b", "$HOME", "x;y", "'q'", "", "a|b", "*", "`id`", "a\nb"] {
            let err = ssh()
                .remote_cmd(Master::Reuse, &["ccnm", bad], Duration::from_secs(1))
                .unwrap_err();
            assert_eq!(err.code(), ErrorCode::Internal, "{bad}");
        }
        assert!(is_remote_safe("~/.local/bin/ccnm"));
        assert!(is_remote_safe("/opt/ccnm-0.1/bin/ccnm"));
    }

    #[test]
    fn mcp_transport_cmd_is_one_plain_ssh_without_control_master() {
        let cmd = ssh()
            .with_ccnm_bin("/Users/ccrun/.local/bin/ccnm")
            .mcp_transport_cmd("eyJwIjoxfQ")
            .unwrap();
        let text = cmd.display();
        assert_eq!(
            text,
            "ssh -o BatchMode=yes -o ConnectTimeout=10 -o ClearAllForwardings=yes -o ControlMaster=no -o ControlPath=none -o ServerAliveInterval=15 -o ServerAliveCountMax=20 -o SendEnv=-ANTHROPIC_* -o SendEnv=-CLAUDE_* -T work /Users/ccrun/.local/bin/ccnm internal mcp-serve --payload eyJwIjoxfQ"
        );
        assert!(cmd.stdin.is_none(), "the MCP client owns stdin, not Cmd");
        assert!(ssh().mcp_transport_cmd("has space").is_err());
        // The session's tools hang off this connection, so it waits five
        // minutes before giving up where a control command waits 45
        // seconds. Losing it costs the person a /mcp reconnect.
        let control = ssh().options(Master::Reuse).join(" ");
        assert!(control.contains("ServerAliveCountMax=3"), "{control}");
    }

    #[test]
    fn alias_must_be_a_token() {
        assert!(Ssh::new("-oProxyCommand=evil", "/tmp").is_err());
        assert!(Ssh::new("a b", "/tmp").is_err());
        assert!(Ssh::new("ccnm-home.tail.ts.net", "/tmp").is_ok());
    }

    #[test]
    fn control_path_length_limit() {
        let dir62 = format!("/{}", "d".repeat(61));
        assert_eq!(dir62.len(), 62);
        Ssh::new("h", &dir62).unwrap().check_control_path().unwrap();
        let dir63 = format!("/{}", "d".repeat(62));
        let err = Ssh::new("h", &dir63)
            .unwrap()
            .check_control_path()
            .unwrap_err();
        assert!(err.message().contains("104 bytes"), "{err}");
    }

    #[test]
    fn control_cmds() {
        assert_eq!(
            ssh().check_master_cmd().display(),
            "ssh -o ControlPath=/Users/me/.local/state/ccnm/ssh/%C -O check work"
        );
        assert_eq!(
            ssh().exit_master_cmd().display(),
            "ssh -o ControlPath=/Users/me/.local/state/ccnm/ssh/%C -O exit work"
        );
        assert_eq!(ssh().resolve_cmd().display(), "ssh -G work");
    }

    #[test]
    fn parse_resolved_reads_the_fields_that_matter() {
        let text = "user bing\nhostname xdwmbp\nport 2222\nproxyjump none\nidentityfile ~/.ssh/id_ed25519_xdwmbp\nidentityfile ~/.ssh/id_rsa\ncontrolmaster false\nsendenv LANG\n";
        let r = parse_resolved(text).unwrap();
        assert_eq!(r.hostname, "xdwmbp");
        assert_eq!(r.user, "bing");
        assert_eq!(r.port, 2222);
        assert_eq!(r.identity_files.len(), 2);
        assert_eq!(r.proxy_jump, None);
        assert_eq!(r.target(), "bing@xdwmbp");

        let r = parse_resolved("hostname h\nproxyjump jump.example\n").unwrap();
        assert_eq!(r.proxy_jump.as_deref(), Some("jump.example"));
        assert_eq!(r.target(), "h");

        assert!(parse_resolved("user x\n").is_err());
    }

    #[test]
    fn real_ssh_g_resolves_localhost() {
        // ssh -G never connects, so this is safe anywhere ssh is installed.
        let r = Ssh::new("localhost", "/tmp")
            .unwrap()
            .resolve(&SystemRunner)
            .unwrap();
        assert_eq!(r.hostname, "localhost");
        assert!(!r.user.is_empty());
    }

    #[test]
    fn classify_exit_codes() {
        let mut out = Output::exited(255, "");
        out.stderr = b"ssh: connect to host x port 22: Connection refused\n".to_vec();
        match classify(out) {
            RemoteOutcome::Unreachable(why) => assert!(why.contains("Connection refused")),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            classify(Output::exited(127, "")),
            RemoteOutcome::CommandNotFound
        ));
        assert!(matches!(
            classify(Output::exited(3, "x")),
            RemoteOutcome::Completed(_)
        ));
        let mut out = Output::exited(0, "");
        out.timed_out = true;
        out.exit_code = None;
        assert!(matches!(classify(out), RemoteOutcome::Unreachable(_)));
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Ping {
        protocol: u32,
        n: u32,
    }

    impl Protocol for Ping {
        fn protocol(&self) -> u32 {
            self.protocol
        }
    }

    #[test]
    fn call_ccnm_decodes_reply_and_records_argv() {
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, r#"{"protocol":1,"n":7}"#));
        let reply: Ping = ssh()
            .call_ccnm(
                &fake,
                Master::Reuse,
                &["internal", "hello"],
                &Ping { protocol: 1, n: 1 },
                Duration::from_secs(1),
                ErrorCode::HomeUnreachable,
            )
            .unwrap();
        assert_eq!(reply.n, 7);
        let call = &fake.calls()[0];
        let text = call.display();
        assert!(
            text.contains(" -T work ~/.local/bin/ccnm internal hello --payload "),
            "{text}"
        );
        let wire = call.args.last().unwrap().to_string_lossy().into_owned();
        assert!(is_remote_safe(&wire));
        assert_eq!(payload::decode::<Ping>(&wire).unwrap().n, 1);
    }

    #[test]
    fn call_ccnm_maps_failures_to_codes() {
        let fake = FakeRunner::new();
        let mut unreachable = Output::exited(255, "");
        unreachable.stderr = b"Connection timed out\n".to_vec();
        fake.push(unreachable);
        fake.push(Output::exited(127, ""));
        let mut remote_err = Output::exited(22, "");
        remote_err.stderr = b"CCNM_E_MOUNT:\nmount failed\nbecause reasons\n".to_vec();
        fake.push(remote_err);
        let mut plain_err = Output::exited(1, "");
        plain_err.stderr = b"panic!\n".to_vec();
        fake.push(plain_err);
        fake.push(Output::exited(0, "not json"));

        let call = || {
            ssh()
                .with_ccnm_bin("/opt/bin/ccnm")
                .call_ccnm::<Ping, Ping>(
                    &fake,
                    Master::Reuse,
                    &["internal", "probe"],
                    &Ping { protocol: 1, n: 0 },
                    Duration::from_secs(1),
                    ErrorCode::WorkUnreachable,
                )
        };
        let e = call().unwrap_err();
        assert_eq!(e.code(), ErrorCode::WorkUnreachable);
        assert!(e.message().contains("Connection timed out"), "{e}");

        let e = call().unwrap_err();
        assert_eq!(e.code(), ErrorCode::Version);
        assert!(
            e.message().starts_with("/opt/bin/ccnm not found on work"),
            "{e}"
        );

        let e = call().unwrap_err();
        assert_eq!(e.code(), ErrorCode::Mount);
        assert_eq!(
            e.message(),
            "ccnm internal probe on work failed (exit 22): mount failed\nbecause reasons"
        );

        let e = call().unwrap_err();
        assert_eq!(e.code(), ErrorCode::Internal);
        assert!(e.message().contains("panic!"), "{e}");

        let e = call().unwrap_err();
        assert_eq!(e.code(), ErrorCode::Version);
    }
}
