//! The Agent half of the exec-server chain (P23): the `CODEX_HOME` a
//! session's Codex is started with, and what goes in it. The Runtime half
//! is `crate::native`.
//!
//! Codex reads its exec-server transport from `CODEX_HOME/environments.toml`
//! and nowhere else (0.154.0, `EnvironmentManager::prepare_from_codex_home`;
//! `CODEX_EXEC_SERVER_URL` is only the fallback when that file is absent).
//! The profile directory is where the login lives and is shared by every
//! session of the instance, so a per-session file cannot go there. Each
//! session gets its own home instead:
//!
//! ```text
//! <session dir>/codex-home/
//! ├── environments.toml   one environment: this ccnm, `internal exec-transport`
//! ├── auth.json -> <profile>/auth.json
//! └── config.toml         trust for the Runtime root, nothing else
//! ```
//!
//! The symlink is the whole credential arrangement. ccnm neither reads nor
//! copies the login: Codex opens the path itself, and its own save writes
//! the file in place rather than replacing it (`login/src/auth/storage.rs`,
//! `OpenOptions::truncate`), so a refreshed token lands in the profile and
//! the link stays a link -- measured, see the P23 record. Codex's session
//! rollouts, history and caches land in this home too, and go with the
//! session directory.
//!
//! What the profile's own `config.toml` said no longer applies to such a
//! session, since Codex reads the one here. Print sessions never read it
//! either (`--ignore-user-config`); MCP interactive sessions still do. The
//! model comes from the instance registry, as before.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::process::Cmd;
use crate::session::Dir;

/// The environment id in `environments.toml`. Also what Codex shows.
pub const ENVIRONMENT_ID: &str = "ccnm";

/// The shape Codex parses (`exec-server/src/environment_toml.rs`,
/// `deny_unknown_fields` on its side too). `program` and `args` make it a
/// stdio transport; `url` would make it WebSocket, and the two are
/// exclusive there.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Environments {
    pub default: String,
    /// `false`: the local environment is not offered at all, so nothing
    /// Codex does can land on the Agent's own disk. With it absent Codex
    /// would include local and still default to it.
    pub include_local: bool,
    pub environments: Vec<Environment>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Environment {
    pub id: String,
    pub program: String,
    pub args: Vec<String>,
}

/// `[projects."<root>"] trust_level = "trusted"`, as Codex itself writes it
/// when a person answers its prompt. Without it every session asks again,
/// because a fresh home has no answer on file; `-c projects.<root>.trust_level`
/// on the command line was measured not to count. Trust here grants
/// nothing on the Runtime: P21 measured that the remote project's own
/// `.codex/config.toml` is not loaded even when trusted.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustConfig {
    pub projects: BTreeMap<String, Project>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub trust_level: String,
}

/// Make (or remake) the session's `CODEX_HOME` and return it. `transport`
/// is what Codex spawns, [`crate::session::transport::native_launcher_for`];
/// `profile` is the validated Agent-local Codex home; `root` is the
/// canonical Runtime root Codex is started with.
///
/// Idempotent: a supervisor that is started twice for one session finds
/// the same home. Nothing in `profile` is touched.
pub fn prepare_home(dir: &Dir, profile: &Path, root: &Path, transport: &Cmd) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let home = dir.codex_home();
    std::fs::create_dir_all(&home)?;
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700))?;
    link_login(&home.join("auth.json"), &profile.join("auth.json"))?;
    write_private(
        &home.join("environments.toml"),
        &render(&environments(transport)?)?,
    )?;
    write_private(&home.join("config.toml"), &render(&trust(root))?)?;
    Ok(home)
}

fn environments(transport: &Cmd) -> Result<Environments> {
    let word = |s: &std::ffi::OsStr| {
        s.to_str().map(str::to_owned).ok_or_else(|| {
            Error::internal("the transport command is not valid UTF-8, which TOML cannot carry")
        })
    };
    Ok(Environments {
        default: ENVIRONMENT_ID.to_string(),
        include_local: false,
        environments: vec![Environment {
            id: ENVIRONMENT_ID.to_string(),
            program: word(&transport.program)?,
            args: transport
                .args
                .iter()
                .map(|a| word(a))
                .collect::<Result<Vec<_>>>()?,
        }],
    })
}

fn trust(root: &Path) -> TrustConfig {
    TrustConfig {
        projects: BTreeMap::from([(
            root.to_string_lossy().into_owned(),
            Project {
                trust_level: "trusted".to_string(),
            },
        )]),
    }
}

fn render<T: Serialize>(value: &T) -> Result<String> {
    toml::to_string(value).map_err(|e| Error::internal("cannot render TOML").with_source(e))
}

/// `auth.json -> <profile>/auth.json`. A link already pointing there is
/// left alone; anything else in the way is replaced, since this home is the
/// session's own and nothing but a link belongs at that name.
fn link_login(link: &Path, target: &Path) -> Result<()> {
    match std::fs::symlink_metadata(link) {
        Ok(meta)
            if meta.file_type().is_symlink()
                && std::fs::read_link(link).ok().as_deref() == Some(target) =>
        {
            return Ok(());
        }
        Ok(_) => std::fs::remove_file(link)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    std::os::unix::fs::symlink(target, link)?;
    Ok(())
}

fn write_private(path: &Path, text: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, text)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}
