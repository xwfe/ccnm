//! What the machine holding a project keeps about handing one session's
//! work to the next (P115): the last thing fetched from an AI machine, and
//! whether it has gone to a new session already.
//!
//! The text itself lives where it was written -- the handoff in that
//! session's directory, the conversation in Claude's transcript -- and is
//! fetched again before every new session. What only this machine can know
//! is where the next session goes, and so whether the same text has already
//! been handed over; that is this file. Without it, every new session would
//! be handed the same handoff again, from wherever the project last ran.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::protocol::run::Carried;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Kept {
    pub carried: Carried,
    /// Set once it went to a new session, or was dropped with
    /// `ccnm handoff <project> --clear`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done: Option<Done>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Done {
    /// The machine the session it went to runs on; `None` when it was
    /// dropped instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Unix seconds.
    pub at: u64,
}

fn path(state: &Path, workspace: &str) -> PathBuf {
    crate::paths::workspace_dir(state, workspace).join("handoff.json")
}

pub fn kept(state: &Path, workspace: &str) -> Option<Kept> {
    let text = std::fs::read_to_string(path(state, workspace)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Whether `fetched` still has to go to a new session: it is not what was
/// already handed over or dropped. The same session with new text -- it
/// went on after a handover and wrote another handoff -- is new.
pub fn still_to_carry(kept: Option<&Kept>, fetched: &Carried) -> bool {
    !kept.is_some_and(|kept| {
        kept.done.is_some()
            && kept.carried.session == fetched.session
            && kept.carried.text == fetched.text
    })
}

pub fn keep(state: &Path, workspace: &str, kept: &Kept) -> Result<()> {
    let path = path(state, workspace);
    let dir = path.parent().expect("a workspace directory");
    let fail =
        |e: std::io::Error| Error::config(format!("cannot keep {}", path.display())).with_source(e);
    std::fs::create_dir_all(dir).map_err(fail)?;
    let temp = dir.join(format!(".handoff.{}", std::process::id()));
    let text = serde_json::to_string_pretty(kept)
        .map_err(|e| Error::internal("cannot encode the handoff record").with_source(e))?;
    std::fs::write(&temp, text)
        .and_then(|()| std::fs::rename(&temp, &path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&temp);
            fail(e)
        })
}

/// Everything P114 and P115 keep for one project on this machine, for when
/// the project is removed. What is not there is not an error.
pub fn forget(state: &Path, workspace: &str) -> Result<bool> {
    let mut removed = false;
    let dir = crate::paths::workspace_dir(state, workspace);
    for name in ["handoff.json", "on"] {
        match std::fs::remove_file(dir.join(name)) {
            Ok(()) => removed = true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(
                    Error::config(format!("cannot remove {}", dir.join(name).display()))
                        .with_source(e),
                );
            }
        }
    }
    // The directory goes too when nothing else of ccnm's is in it.
    let _ = std::fs::remove_dir(&dir);
    Ok(removed)
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::run::CarriedKind;

    fn temp(test: &str) -> ccnm_testdir::TestDir {
        let dir = std::env::temp_dir().join(format!("ccnm-handover-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        ccnm_testdir::TestDir::adopt(dir)
    }

    fn carried(session: &str, text: &str) -> Carried {
        Carried {
            kind: CarriedKind::Handoff,
            from: "agent".into(),
            session: session.into(),
            written: 1,
            text: text.into(),
        }
    }

    #[test]
    fn what_was_handed_over_is_not_handed_over_again() {
        let state = temp("again");
        let first = carried("s1", "[ccnm handoff]\na");
        assert!(still_to_carry(None, &first));
        let mut record = Kept {
            carried: first.clone(),
            done: None,
        };
        keep(state.path(), "xdo", &record).unwrap();
        let kept_now = kept(state.path(), "xdo");
        assert!(
            still_to_carry(kept_now.as_ref(), &first),
            "fetched, not yet handed"
        );
        record.done = Some(Done {
            to: Some("hpsrv".into()),
            at: 2,
        });
        keep(state.path(), "xdo", &record).unwrap();
        let kept_now = kept(state.path(), "xdo");
        assert_eq!(kept_now.as_ref(), Some(&record));
        assert!(!still_to_carry(kept_now.as_ref(), &first));
        // The same session wrote a new handoff after it: that one goes.
        assert!(still_to_carry(
            kept_now.as_ref(),
            &carried("s1", "[ccnm handoff]\nb")
        ));
        assert!(still_to_carry(
            kept_now.as_ref(),
            &carried("s2", "[ccnm handoff]\na")
        ));
    }

    #[test]
    fn removing_a_project_forgets_what_was_kept_for_it() {
        let state = temp("forget");
        assert!(!forget(state.path(), "xdo").unwrap());
        keep(
            state.path(),
            "xdo",
            &Kept {
                carried: carried("s1", "t"),
                done: None,
            },
        )
        .unwrap();
        crate::on::remember(state.path(), "xdo", Some("hpsrv")).unwrap();
        assert!(forget(state.path(), "xdo").unwrap());
        assert_eq!(kept(state.path(), "xdo"), None);
        assert_eq!(crate::on::remembered(state.path(), "xdo"), None);
        assert!(!crate::paths::workspace_dir(state.path(), "xdo").exists());
    }
}
