//! Changing `config.toml` from the command line.
//!
//! `ccnm init` and `ccnm workspace add|remove` exist because the
//! alternative was a README section telling people to hand-write TOML,
//! and every field they get wrong there is a `CCNM_E_CONFIG` an hour
//! later.
//!
//! # What this must not do
//!
//! **Lose what someone wrote.** A config is not only data; it is the
//! place people leave the reason for a setting -- "temporary, until ccrun
//! exists" next to `allow_unconfined_exec`. Serializing a struct back over
//! the file would delete every one of those without saying so, which is
//! why this edits the document ([`toml_edit`]) rather than re-emitting it.
//!
//! **Surprise anyone.** Every operation here reports what it changed, in
//! the terms of the file: "added workspaces.x", "hosts.work.ssh: a -> b",
//! or "nothing to change". Running one twice is not an error and does not
//! do the work twice; the second run says the setting is already what was
//! asked for.

use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item, Table, value};

use crate::config::{Config, PermissionMode};
use crate::error::{Error, Result};

/// What an edit did, for the caller to print. Empty means the file
/// already said what was asked.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Changes(Vec<String>);

impl Changes {
    fn note(&mut self, text: impl Into<String>) {
        self.0.push(text.into());
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn lines(&self) -> &[String] {
        &self.0
    }
}

/// A config file open for editing. `None` for the document when the file
/// does not exist yet: [`save`](Self::save) creates it.
pub struct Edit {
    path: PathBuf,
    doc: DocumentMut,
    existed: bool,
}

impl Edit {
    pub fn open(path: &Path) -> Result<Edit> {
        let (doc, existed) = match std::fs::read_to_string(path) {
            Ok(text) => (
                text.parse::<DocumentMut>().map_err(|e| {
                    Error::config(format!(
                        "{} is not valid TOML, so ccnm will not rewrite it: {e}\nfix it by hand first",
                        path.display()
                    ))
                })?,
                true,
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (DocumentMut::new(), false),
            Err(e) => {
                return Err(
                    Error::config(format!("cannot read {}", path.display())).with_source(e)
                );
            }
        };
        Ok(Edit {
            path: path.to_path_buf(),
            doc,
            existed,
        })
    }

    pub fn existed(&self) -> bool {
        self.existed
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Point `hosts.<name>.<field>` at `alias`, creating the table if this
    /// is the first mention of that host.
    pub fn set_host(&mut self, name: &str, field: &str, alias: &str, changes: &mut Changes) {
        let hosts = self.table("hosts");
        let host = hosts
            .entry(name)
            .or_insert_with(|| Item::Table(implicit_table()));
        let Some(host) = host.as_table_mut() else {
            changes.note(format!("hosts.{name} is not a table; left alone"));
            return;
        };
        let before = host.get(field).and_then(Item::as_str).map(str::to_string);
        match before {
            Some(current) if current == alias => {}
            Some(current) => {
                host[field] = value(alias);
                changes.note(format!("hosts.{name}.{field}: {current} -> {alias}"));
            }
            None => {
                host[field] = value(alias);
                changes.note(format!("hosts.{name}.{field} = {alias}"));
            }
        }
    }

    /// Add or update `workspaces.<name>`.
    pub fn set_workspace(
        &mut self,
        name: &str,
        root: &Path,
        work_host: &str,
        permission_mode: Option<PermissionMode>,
        allow_unconfined_exec: Option<bool>,
        changes: &mut Changes,
    ) {
        let existed = self
            .doc
            .get("workspaces")
            .and_then(Item::as_table)
            .is_some_and(|t| t.contains_key(name));
        let workspaces = self.table("workspaces");
        let ws = workspaces
            .entry(name)
            .or_insert_with(|| Item::Table(implicit_table()));
        let Some(ws) = ws.as_table_mut() else {
            changes.note(format!("workspaces.{name} is not a table; left alone"));
            return;
        };
        if !existed {
            changes.note(format!("added workspaces.{name}"));
        }
        set_str(ws, "work_host", work_host, name, changes);
        set_str(ws, "root", &root.display().to_string(), name, changes);
        if let Some(mode) = permission_mode {
            set_str(
                ws,
                "claude_permission_mode",
                mode.as_cli_value(),
                name,
                changes,
            );
        }
        if let Some(allow) = allow_unconfined_exec {
            match ws.get("allow_unconfined_exec").and_then(Item::as_bool) {
                Some(current) if current == allow => {}
                _ => {
                    ws["allow_unconfined_exec"] = value(allow);
                    changes.note(format!("workspaces.{name}.allow_unconfined_exec = {allow}"));
                }
            }
        }
    }

    /// Remove `workspaces.<name>`. Removing one that is not there is not
    /// an error: the file already says what was asked.
    pub fn remove_workspace(&mut self, name: &str, changes: &mut Changes) -> bool {
        let Some(workspaces) = self.doc.get_mut("workspaces").and_then(Item::as_table_mut) else {
            return false;
        };
        if workspaces.remove(name).is_none() {
            return false;
        }
        changes.note(format!("removed workspaces.{name}"));
        if workspaces.is_empty() {
            self.doc.remove("workspaces");
        }
        true
    }

    fn table(&mut self, name: &str) -> &mut Table {
        if !self.doc.contains_key(name) {
            self.doc[name] = Item::Table(implicit_table());
        }
        self.doc[name]
            .as_table_mut()
            .expect("just created as a table")
    }

    /// Write it back, but only after it parses as a config.
    ///
    /// The check is the point: a command that leaves the file in a state
    /// the next command refuses to load has broken the thing it was asked
    /// to set up, and would have done it silently.
    pub fn save(&self, changes: &Changes) -> Result<()> {
        let text = self.doc.to_string();
        Config::parse(&text).map_err(|e| {
            Error::config(format!(
                "that change would leave {} unusable, so nothing was written: {}",
                self.path.display(),
                e.message()
            ))
        })?;
        if changes.is_empty() && self.existed {
            return Ok(());
        }
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| {
                Error::config(format!("cannot create {}", dir.display())).with_source(e)
            })?;
        }
        // Through a temporary file: a config half-written by a killed
        // process is worse than one that was never changed.
        let tmp = self.path.with_extension("toml.ccnm-new");
        std::fs::write(&tmp, &text)
            .map_err(|e| Error::config(format!("cannot write {}", tmp.display())).with_source(e))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| {
            Error::config(format!("cannot replace {}", self.path.display())).with_source(e)
        })?;
        Ok(())
    }

    #[cfg(test)]
    fn text(&self) -> String {
        self.doc.to_string()
    }
}

fn set_str(table: &mut Table, field: &str, wanted: &str, name: &str, changes: &mut Changes) {
    let before = table.get(field).and_then(Item::as_str).map(str::to_string);
    match before {
        Some(current) if current == wanted => {}
        Some(current) => {
            table[field] = value(wanted);
            changes.note(format!("workspaces.{name}.{field}: {current} -> {wanted}"));
        }
        None => {
            table[field] = value(wanted);
            changes.note(format!("workspaces.{name}.{field} = {wanted}"));
        }
    }
}

/// A table that prints as `[hosts.work]` rather than as an inline table.
fn implicit_table() -> Table {
    let mut table = Table::new();
    table.set_implicit(true);
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ccnm-cfgedit-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("config.toml")
    }

    #[test]
    fn a_config_can_be_created_from_nothing_and_loads_back() {
        let path = temp("create");
        let mut edit = Edit::open(&path).unwrap();
        assert!(!edit.existed());
        let mut changes = Changes::default();
        edit.set_host("work", "ssh", "fodelf", &mut changes);
        edit.set_host("home", "ssh_from_work", "xdwmbp", &mut changes);
        edit.set_workspace(
            "xshun",
            Path::new("/Users/bing/code/xshun"),
            "work",
            None,
            None,
            &mut changes,
        );
        edit.save(&changes).unwrap();

        let config = Config::load(&path).unwrap();
        assert_eq!(config.version, None, "nothing writes a version any more");
        let resolved = config.workspace("xshun").unwrap();
        assert_eq!(resolved.work_ssh, "fodelf");
        assert_eq!(resolved.home_alias, "xdwmbp");
        assert_eq!(resolved.workspace.root, Path::new("/Users/bing/code/xshun"));
    }

    /// The reason this edits instead of re-emitting: people explain
    /// themselves in their config, and a tool that eats those notes is
    /// one they stop trusting with the file.
    #[test]
    fn comments_and_unrelated_settings_survive_an_edit() {
        let path = temp("comments");
        std::fs::write(
            &path,
            "# the work machine is the mac mini\n\
             [hosts.work]\n\
             ssh = \"fodelf\"\n\
             \n\
             [workspaces.old]\n\
             work_host = \"work\"\n\
             root = \"/a\"\n\
             # temporary, until ccrun exists\n\
             allow_unconfined_exec = true\n",
        )
        .unwrap();

        let mut edit = Edit::open(&path).unwrap();
        let mut changes = Changes::default();
        edit.set_host("home", "ssh_from_work", "xdwmbp", &mut changes);
        edit.set_workspace("new", Path::new("/b"), "work", None, None, &mut changes);
        edit.save(&changes).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("# the work machine is the mac mini"),
            "{text}"
        );
        assert!(text.contains("# temporary, until ccrun exists"), "{text}");
        assert!(text.contains("[workspaces.old]"), "{text}");
        assert!(text.contains("[workspaces.new]"), "{text}");
    }

    /// Running the same command twice is not an error and does not write
    /// the same thing twice.
    #[test]
    fn setting_what_is_already_set_changes_nothing() {
        let path = temp("idempotent");
        let mut first = Edit::open(&path).unwrap();
        let mut changes = Changes::default();
        first.set_host("work", "ssh", "fodelf", &mut changes);
        first.set_host("home", "ssh_from_work", "xdwmbp", &mut changes);
        first.set_workspace("x", Path::new("/a"), "work", None, None, &mut changes);
        first.save(&changes).unwrap();
        let after_first = std::fs::read_to_string(&path).unwrap();
        assert!(!changes.is_empty());

        let mut again = Edit::open(&path).unwrap();
        let mut changes = Changes::default();
        again.set_host("work", "ssh", "fodelf", &mut changes);
        again.set_host("home", "ssh_from_work", "xdwmbp", &mut changes);
        again.set_workspace("x", Path::new("/a"), "work", None, None, &mut changes);
        again.save(&changes).unwrap();
        assert!(changes.is_empty(), "{:?}", changes.lines());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), after_first);
    }

    #[test]
    fn changing_a_setting_reports_both_values() {
        let path = temp("change");
        let mut edit = Edit::open(&path).unwrap();
        let mut changes = Changes::default();
        edit.set_host("work", "ssh", "old-alias", &mut changes);
        edit.save(&changes).unwrap();

        let mut edit = Edit::open(&path).unwrap();
        let mut changes = Changes::default();
        edit.set_host("work", "ssh", "new-alias", &mut changes);
        assert_eq!(changes.lines(), ["hosts.work.ssh: old-alias -> new-alias"]);
    }

    #[test]
    fn removing_a_workspace_that_is_not_there_is_not_an_error() {
        let path = temp("remove");
        let mut edit = Edit::open(&path).unwrap();
        let mut changes = Changes::default();
        edit.set_host("work", "ssh", "w", &mut changes);
        edit.set_host("home", "ssh_from_work", "h", &mut changes);
        edit.set_workspace("x", Path::new("/a"), "work", None, None, &mut changes);
        edit.save(&changes).unwrap();

        let mut edit = Edit::open(&path).unwrap();
        let mut changes = Changes::default();
        assert!(!edit.remove_workspace("never-existed", &mut changes));
        assert!(changes.is_empty());
        assert!(edit.remove_workspace("x", &mut changes));
        edit.save(&changes).unwrap();
        assert!(!edit.text().contains("workspaces"), "{}", edit.text());
        Config::load(&path).unwrap();
    }

    /// A change that would leave the file unloadable is refused with the
    /// file untouched, rather than written and discovered by the next
    /// command.
    #[test]
    fn an_edit_that_breaks_the_config_writes_nothing() {
        let path = temp("invalid");
        let mut edit = Edit::open(&path).unwrap();
        let mut changes = Changes::default();
        // A workspace whose work_host is not defined anywhere.
        edit.set_workspace("x", Path::new("/a"), "nowhere", None, None, &mut changes);
        let err = edit.save(&changes).unwrap_err();
        assert!(err.message().contains("nothing was written"), "{err}");
        assert!(!path.exists(), "the file must not have been created");
    }
}
