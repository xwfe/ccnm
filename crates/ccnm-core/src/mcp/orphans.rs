//! What a session's commands leave behind outside their process groups (P84).
//!
//! Every command ccnm runs leads a process group of its own, and stopping a
//! command signals that group. A descendant that calls `setsid` -- a daemon,
//! a dev server that detaches, `nohup … &` turned into a session of its own
//! -- is in a group of its own, so no signal for the command reaches it. If
//! it also let go of the command's output pipes, nothing told ccnm it
//! existed: the session ended, the write guard was handed on, and it went on
//! changing the working tree beside the next writer (the gap P43 recorded).
//!
//! On Linux this server makes itself the subreaper of its descendants
//! (`PR_SET_CHILD_SUBREAPER`, [`adopt`]): such a process, once whatever
//! started it is gone, becomes this server's child instead of init's. So at
//! the end of a session every live child that ccnm did not start itself is
//! something the session left behind, and [`Sweeper`] ends it -- its group
//! and all -- before the guard is let go. What survives keeps the guard,
//! exactly like a command `stop_all` gave up on.
//!
//! Nothing is reaped here. An adopted child that ends stays a zombie until
//! this process exits and init takes it, which is what happens to every child
//! still here at exit anyway; reaping would race the waits `std::process`
//! keeps for the children ccnm spawned itself.
//!
//! macOS has no subreaper: [`adopt`] does nothing there and the sweeps find
//! nothing, which is how ccnm behaved before P84.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

/// Between TERM and KILL for what the sweep finds.
pub(crate) const GRACE: Duration = Duration::from_secs(2);
/// After KILL, how long to wait for it to go before giving up on it.
pub(crate) const GIVE_UP: Duration = Duration::from_secs(5);
/// How often the background sweep looks while the session is being ended.
const EVERY: Duration = Duration::from_millis(100);

/// Make this process the subreaper of its descendants. `false` when it is
/// not (any platform but Linux, or the call failed): the session then ends
/// the way it did before P84, which is not worse than before.
pub(crate) fn adopt() -> bool {
    #[cfg(target_os = "linux")]
    {
        match nix::sys::prctl::set_child_subreaper(true) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(%error, "cannot become the subreaper of this session's commands; what leaves their process groups is not collected");
                false
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// One line of `/proc/<pid>/stat`, the fields this module needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Stat {
    pub pid: u32,
    pub state: char,
    pub ppid: u32,
    pub pgrp: u32,
    pub session: u32,
}

impl Stat {
    /// The command name sits in parentheses and may itself contain spaces
    /// and parentheses, so fields are counted from the last `)`: state,
    /// ppid, pgrp, session. Not platform-gated so its tests run everywhere;
    /// only Linux calls it.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn parse(line: &str) -> Option<Stat> {
        let (head, rest) = line.rsplit_once(')')?;
        let pid = head.split_whitespace().next()?.parse().ok()?;
        let mut fields = rest.split_whitespace();
        let state = fields.next()?.chars().next()?;
        let ppid = fields.next()?.parse().ok()?;
        let pgrp = fields.next()?.parse().ok()?;
        let session = fields.next()?.parse().ok()?;
        Some(Stat {
            pid,
            state,
            ppid,
            pgrp,
            session,
        })
    }

    /// Gone in all but name: a zombie, or one being torn down.
    pub(crate) fn ended(&self) -> bool {
        matches!(self.state, 'Z' | 'X' | 'x')
    }
}

/// Who this process is, for telling its own children from adopted ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Me {
    pub pid: u32,
    pub pgrp: u32,
    pub session: u32,
}

/// A live child of `me` that left this session: it called `setsid`, and
/// reached this process by being adopted.
///
/// ccnm never starts anything in a session of its own, so this cannot be
/// one of ccnm's children. The group is no guide: a relayed MCP server is
/// ccnm's own child in a group another of ccnm's children leads (its anchor,
/// `relay::start`), and treating that as adopted ended the server and its
/// children the moment the session began to end (found by the P52 test on
/// Linux while P84 was being made). A command's descendant that stayed in
/// the command's group is ended with that group by `stop_all`; whatever is
/// left after that, the final sweep takes.
pub(crate) fn adopted(child: &Stat, me: &Me) -> bool {
    child.ppid == me.pid && !child.ended() && child.session != me.session
}

/// Ends what a session left behind while the session is being ended.
///
/// Started before the commands are stopped: stopping a command's group is
/// what hands its escaped descendants to this process, and one that holds
/// the command's output pipe keeps the command from being waited for until
/// it is gone. So it is ended as it arrives, rather than after `stop_all`
/// has waited for it in vain. [`Sweeper::finish`] then takes every child
/// still alive, adopted or not, and says what would not go.
pub(crate) struct Sweeper {
    adopting: bool,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Sweeper {
    /// A sweeper for this process, doing nothing unless `adopting` -- the
    /// [`adopt`] that made this process a subreaper succeeded. Until
    /// [`finish`](Self::finish) only adopted children are touched: ccnm's
    /// own (commands being stopped, relayed servers being closed) are in
    /// other hands.
    pub(crate) fn start(adopting: bool) -> Sweeper {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = imp::me().filter(|_| adopting).map(|me| {
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("ccnm-orphans".into())
                .spawn(move || {
                    let mut ending = imp::Ending::new();
                    while !stop.load(Ordering::Relaxed) {
                        let found: Vec<Stat> = imp::children(&me)
                            .into_iter()
                            .filter(|c| adopted(c, &me))
                            .collect();
                        ending.signal(&me, &found);
                        std::thread::sleep(EVERY);
                    }
                })
                .ok()
        });
        Sweeper {
            adopting,
            stop,
            thread: thread.flatten(),
        }
    }

    /// Stop the background sweep, then end every child still alive and
    /// name the ones that would not end within [`GRACE`] + [`GIVE_UP`].
    /// Empty when nothing was left -- and always on a platform with no
    /// subreaper, where nothing was ever adopted.
    pub(crate) fn finish(mut self) -> Vec<String> {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        match imp::me() {
            Some(me) if self.adopting => imp::end_all(&me, GRACE, GIVE_UP),
            _ => Vec::new(),
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    use nix::sys::signal::{Signal, kill, killpg};
    use nix::unistd::Pid;

    use super::{Me, Stat};

    pub(super) fn me() -> Option<Me> {
        let pid = std::process::id();
        let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
        let own = Stat::parse(&stat)?;
        Some(Me {
            pid,
            pgrp: own.pgrp,
            session: own.session,
        })
    }

    /// Every process whose parent is `me`, from `/proc`. One that ends
    /// between the listing and the read is simply not there.
    pub(super) fn children(me: &Me) -> Vec<Stat> {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.bytes().all(|b| b.is_ascii_digit()))
            })
            .filter_map(|entry| std::fs::read_to_string(entry.path().join("stat")).ok())
            .filter_map(|line| Stat::parse(&line))
            .filter(|stat| stat.ppid == me.pid)
            .collect()
    }

    /// TERM first, KILL after `super::GRACE`, per child, remembered across
    /// passes so a child is not sent TERM again every time it is seen.
    #[derive(Default)]
    pub(super) struct Ending {
        termed: HashMap<u32, Instant>,
    }

    impl Ending {
        pub(super) fn new() -> Self {
            Ending::default()
        }

        pub(super) fn signal(&mut self, me: &Me, found: &[Stat]) {
            for child in found {
                let first = *self.termed.entry(child.pid).or_insert_with(|| {
                    send(me, child, Signal::SIGTERM);
                    Instant::now()
                });
                if first.elapsed() >= super::GRACE {
                    send(me, child, Signal::SIGKILL);
                }
            }
        }
    }

    /// The child and the group it leads or belongs to -- never this
    /// process's own group, and never a group number that means "every
    /// process" or "this group".
    fn send(me: &Me, child: &Stat, signal: Signal) {
        if child.pgrp > 1 && child.pgrp != me.pgrp {
            let _ = killpg(Pid::from_raw(child.pgrp as i32), signal);
        }
        let _ = kill(Pid::from_raw(child.pid as i32), signal);
    }

    pub(super) fn end_all(me: &Me, grace: Duration, give_up: Duration) -> Vec<String> {
        let deadline = Instant::now() + grace + give_up;
        let mut ending = Ending::new();
        loop {
            let alive: Vec<Stat> = children(me).into_iter().filter(|c| !c.ended()).collect();
            if alive.is_empty() {
                return Vec::new();
            }
            if Instant::now() >= deadline {
                return alive.iter().map(describe).collect();
            }
            ending.signal(me, &alive);
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn describe(child: &Stat) -> String {
        let command = std::fs::read(format!("/proc/{}/cmdline", child.pid))
            .map(|raw| {
                String::from_utf8_lossy(&raw)
                    .split('\0')
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        let command: String = command.chars().take(80).collect();
        format!("pid {} ({command})", child.pid)
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use std::time::Duration;

    use super::{Me, Stat};

    pub(super) fn me() -> Option<Me> {
        None
    }

    pub(super) fn children(_: &Me) -> Vec<Stat> {
        Vec::new()
    }

    pub(super) struct Ending;

    impl Ending {
        pub(super) fn new() -> Self {
            Ending
        }

        pub(super) fn signal(&mut self, _: &Me, _: &[Stat]) {}
    }

    pub(super) fn end_all(_: &Me, _: Duration, _: Duration) -> Vec<String> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: Me = Me {
        pid: 100,
        pgrp: 90,
        session: 80,
    };

    fn child(pid: u32, pgrp: u32, session: u32, state: char) -> Stat {
        Stat {
            pid,
            state,
            ppid: ME.pid,
            pgrp,
            session,
        }
    }

    #[test]
    fn a_stat_line_is_read_from_the_last_parenthesis() {
        let line = "4242 (my (odd) name) S 100 4242 80 34816 4242 4194304 0 0";
        assert_eq!(
            Stat::parse(line),
            Some(Stat {
                pid: 4242,
                state: 'S',
                ppid: 100,
                pgrp: 4242,
                session: 80,
            })
        );
        assert_eq!(Stat::parse("garbage"), None);
        assert_eq!(Stat::parse("12 (x) S 1"), None);
    }

    #[test]
    fn only_children_that_left_the_session_count_as_adopted() {
        // ccnm's own: this session, leading a group of its own, in this
        // process's group, or in a group another child leads -- a relayed
        // server in its anchor's group.
        assert!(!adopted(&child(200, 200, ME.session, 'S'), &ME));
        assert!(!adopted(&child(201, ME.pgrp, ME.session, 'S'), &ME));
        assert!(!adopted(&child(202, 200, ME.session, 'S'), &ME));
        // Called setsid: another session.
        assert!(adopted(&child(300, 300, 300, 'S'), &ME));
        // Already ended, or someone else's child.
        assert!(!adopted(&child(302, 302, 302, 'Z'), &ME));
        let mut other = child(303, 303, 303, 'S');
        other.ppid = 1;
        assert!(!adopted(&other, &ME));
    }

    #[test]
    fn without_a_subreaper_nothing_is_found() {
        if cfg!(target_os = "linux") {
            return;
        }
        assert!(!adopt());
        assert!(Sweeper::start(true).finish().is_empty());
    }
}
