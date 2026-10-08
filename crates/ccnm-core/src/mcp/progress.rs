//! Progress for a call that waits on a command (P82).
//!
//! Claude Code puts a `progressToken` on every `tools/call` (2.1.286 and
//! 2.1.293, recorded by the P81 probe) and shows what comes back on it.
//! With nothing coming back, a five-minute `cargo test` is a spinner nobody
//! can tell from a hang. So while `exec_command` waits for a command, or
//! `read_output` waits for one to finish, the Host hears every [`EVERY`] how
//! long it has been and the last line the command wrote.
//!
//! Only when the request carried a token -- without one the protocol leaves
//! nothing to send it on -- and never after the result: the caller stops
//! ticking before it answers.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rmcp::RoleServer;
use rmcp::model::{ProgressNotificationParam, ProgressToken};
use rmcp::service::{Peer, RequestContext};

/// How often. Claude Code's own `claude mcp serve` reports every 30 s
/// (CHANGELOG 2.1.271); a third of that answers "is it still doing
/// something?" before anyone has to ask, and over ssh each report is one
/// short line.
pub(crate) const EVERY: Duration = Duration::from_secs(10);

/// The most of the last line a report carries, in characters. A status
/// line, not a log: the full output is what `read_output` is for.
const LINE_CHARS: usize = 120;

/// How far back from the end of a stream to look for its last line.
const TAIL_BYTES: u64 = 4096;

/// One call's reporter: where to send, under which token, since when.
pub(crate) struct Reporter {
    peer: Peer<RoleServer>,
    token: ProgressToken,
    started: Instant,
}

impl Reporter {
    /// `None` when the request carried no token: then nothing is sent.
    pub(crate) fn for_call(context: &RequestContext<RoleServer>) -> Option<Self> {
        Some(Reporter {
            peer: context.peer.clone(),
            token: context.meta.get_progress_token()?,
            started: Instant::now(),
        })
    }

    /// Ticks that start one [`EVERY`] from now: a command that finishes
    /// sooner is never reported on at all.
    pub(crate) fn ticks() -> tokio::time::Interval {
        let mut ticks = tokio::time::interval_at(tokio::time::Instant::now() + EVERY, EVERY);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticks
    }

    /// Say how long it has been. `run` is the command's run directory once
    /// it has one; `waiting_for` names the `output_ref` when the call is a
    /// `read_output` wait rather than the command itself.
    pub(crate) async fn report(&self, run: Option<PathBuf>, waiting_for: Option<&str>) {
        let elapsed = self.started.elapsed();
        let line = match run {
            Some(dir) => tokio::task::spawn_blocking(move || last_line(&dir))
                .await
                .ok()
                .flatten(),
            None => None,
        };
        let param = ProgressNotificationParam::new(self.token.clone(), elapsed.as_secs() as f64)
            .with_message(message(elapsed, waiting_for, line.as_deref()));
        // A Host that has gone away is noticed where it matters -- the call
        // is cancelled, the session ends. A status line it never reads is
        // not worth failing the call over.
        let _ = self.peer.notify_progress(param).await;
    }
}

/// `running for 40 s · Compiling ccnm-core`, or for a wait
/// `waited 40 s for r-… · …`.
pub(crate) fn message(elapsed: Duration, waiting_for: Option<&str>, line: Option<&str>) -> String {
    let secs = elapsed.as_secs();
    let head = match waiting_for {
        Some(reference) => format!("waited {secs} s for {reference}"),
        None => format!("running for {secs} s"),
    };
    match line {
        Some(line) => format!("{head} · {line}"),
        None => head,
    }
}

/// The last line the command wrote, from whichever of its streams was
/// written most recently, cleaned up to be shown as one line of status.
///
/// `None` when nothing printable has been written yet, or the run is gone.
pub(crate) fn last_line(run: &Path) -> Option<String> {
    let newest = ["stdout", "stderr"]
        .iter()
        .map(|name| run.join(name))
        .filter_map(|path| {
            let meta = std::fs::metadata(&path).ok()?;
            (meta.len() > 0).then(|| (meta.modified().ok(), path))
        })
        .max_by_key(|(modified, _)| *modified)?
        .1;
    let tail = read_tail(&newest)?;
    String::from_utf8_lossy(&tail)
        .lines()
        .rev()
        .map(clean)
        .find(|line| !line.is_empty())
        .map(|line| shorten(&line))
}

fn read_tail(path: &Path) -> Option<Vec<u8>> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(TAIL_BYTES)))
        .ok()?;
    let mut tail = Vec::new();
    file.take(TAIL_BYTES).read_to_end(&mut tail).ok()?;
    Some(tail)
}

/// One line as a terminal would have left it showing.
///
/// A progress bar redraws itself with `\r`, so only what follows the last
/// one is still on screen. Colour and cursor sequences (`ESC [ … m` and the
/// like, `ESC ] … BEL` titles) are dropped, and any other control
/// character becomes a space, so the line cannot do anything to the
/// Host's display either.
fn clean(line: &str) -> String {
    let line = line.trim_end_matches('\r');
    let visible = line.rsplit('\r').next().unwrap_or(line);
    let mut out = String::with_capacity(visible.len());
    let mut chars = visible.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.peek() {
                // CSI: parameters, then one final byte in @..~.
                Some('[') => {
                    chars.next();
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC: up to BEL, or ESC \.
                Some(']') => {
                    chars.next();
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' {
                            break;
                        }
                        if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                // A two-character escape.
                Some(_) => {
                    chars.next();
                }
                None => {}
            },
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out.trim().to_string()
}

fn shorten(line: &str) -> String {
    if line.chars().count() <= LINE_CHARS {
        return line.to_string();
    }
    let kept: String = line.chars().take(LINE_CHARS - 1).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run directory with these two streams. Each test names its own, so
    /// tests running at once never share one.
    fn run_with(test: &str, stdout: &[u8], stderr: &[u8]) -> ccnm_testdir::TestDir {
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("ccnm-progress-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("stdout"), stdout).unwrap();
        std::fs::write(dir.join("stderr"), stderr).unwrap();
        ccnm_testdir::TestDir::adopt(dir)
    }

    #[test]
    fn the_last_line_that_shows_something() {
        let run = run_with("last", b"Compiling a\nCompiling b\n\n   \n", b"");
        assert_eq!(last_line(run.path()).as_deref(), Some("Compiling b"));
    }

    #[test]
    fn nothing_written_yet_is_no_line() {
        let run = run_with("empty", b"", b"");
        assert_eq!(last_line(run.path()), None);
        let gone = run.path().join("not-a-run");
        assert_eq!(last_line(&gone), None);
    }

    #[test]
    fn the_stream_written_last_wins() {
        let run = run_with("newest", b"from stdout\n", b"");
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(run.path().join("stderr"), b"warning: from stderr\n").unwrap();
        assert_eq!(
            last_line(run.path()).as_deref(),
            Some("warning: from stderr")
        );
    }

    #[test]
    fn a_redrawn_progress_bar_shows_only_its_last_frame() {
        let run = run_with(
            "bar",
            b"Downloading  10%\rDownloading  55%\rDownloading  90%\r\n",
            b"",
        );
        assert_eq!(last_line(run.path()).as_deref(), Some("Downloading  90%"));
    }

    #[test]
    fn colour_titles_and_control_characters_do_not_reach_the_host() {
        let run = run_with(
            "ansi",
            b"\x1b[1;32m   Compiling\x1b[0m ccnm\x1b]0;title\x07 v0.13\tdone\x08\n",
            b"",
        );
        assert_eq!(
            last_line(run.path()).as_deref(),
            Some("Compiling ccnm v0.13 done")
        );
    }

    #[test]
    fn a_long_line_is_cut_on_a_character() {
        let long = "中".repeat(200);
        let run = run_with("long", format!("{long}\n").as_bytes(), b"");
        let line = last_line(run.path()).unwrap();
        assert_eq!(line.chars().count(), LINE_CHARS);
        assert!(line.ends_with('…'), "{line}");
    }

    #[test]
    fn only_the_tail_of_a_big_stream_is_read() {
        let mut big = vec![b'x'; 1 << 20];
        big.extend_from_slice(b"\nthe end\n");
        let run = run_with("big", &big, b"");
        assert_eq!(last_line(run.path()).as_deref(), Some("the end"));
    }

    #[test]
    fn messages_say_what_is_being_waited_for() {
        let secs = Duration::from_secs(40);
        assert_eq!(message(secs, None, None), "running for 40 s");
        assert_eq!(
            message(secs, None, Some("Compiling b")),
            "running for 40 s · Compiling b"
        );
        assert_eq!(
            message(secs, Some("r-0123456789abcdef"), None),
            "waited 40 s for r-0123456789abcdef"
        );
    }
}
