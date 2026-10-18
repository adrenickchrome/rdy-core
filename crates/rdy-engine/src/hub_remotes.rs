//! Remote configuration with no network or credential access (D-B8N21, p-38V30, p-PCXT7).
//!
//! [`RemoteService`] lists the remotes in effect and adds, removes or changes the mode of one
//! remote at one settings level: the project config (`.rdy.yaml`, the default), the checkout's
//! `.rdy/local.yaml` or the user config. A row's level is the level that supplies it in effect.
//! An edit changes only the selected level's entry, so removing it can make a less specific
//! level's entry effective again.
//!
//! Every edit runs in one order. The input is checked first. Then the user config's sibling lock
//! is taken: it orders every remote edit, whatever its level, so two edits at different levels
//! cannot each add a share remote. If the destination is another file, its own sibling lock is
//! taken second; the project and local locks are never held together, and the credential lock is
//! never taken. Under the locks all three levels are read again, and the selected level's entry,
//! the effective count (at most 64) and the single share remote are checked on the config as it
//! would be after the edit. Only the destination is saved, durably, with its mode and its other
//! values kept; the locks are released in reverse order.
//!
//! Nothing here opens a connection, reads a credential, confirms a host for sign-in, adds a
//! pinned origin or touches the project's store. A save that fails before its rename leaves the
//! old file; one that fails after it may leave the complete new file, which the next call reads.

use crate::config_edit::LockedConfig;
use crate::config_levels::{self, LOCAL_FILE, Level, PROJECT_FILE};
use crate::error::{E_ALREADY_EXISTS, E_BAD_INPUT, E_CONFIG, E_NOT_FOUND, RdyError, remote_exists};
use crate::hub_config::{
    self, BAD_REMOTE_NAME_FIX, BAD_REMOTE_NAME_MESSAGE, HubConfig, HubMerge, Remote, RemoteEntry,
    RemoteMode, is_remote_name,
};
use crate::hub_types::{LimitUnit, MAX_CONFIG_BYTES, MAX_REMOTES, Origin, RemoteUrl};
use rdy_schema::Issue;
use serde::{Deserialize, Serialize};
use serde_yaml_ng::{Mapping, Value};
use std::path::{Path, PathBuf};

/// The settings level a remote edit writes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RemoteLevel {
    /// The project config, `.rdy.yaml`, shared with the project.
    #[default]
    Project,
    /// This checkout's `.rdy/local.yaml`.
    Local,
    /// The user config.
    User,
}

impl RemoteLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            RemoteLevel::Project => "project",
            RemoteLevel::Local => "local",
            RemoteLevel::User => "user",
        }
    }

    pub fn parse(text: &str) -> Option<RemoteLevel> {
        match text {
            "project" => Some(RemoteLevel::Project),
            "local" => Some(RemoteLevel::Local),
            "user" => Some(RemoteLevel::User),
            _ => None,
        }
    }

    fn level(self) -> Level {
        match self {
            RemoteLevel::Project => Level::Project,
            RemoteLevel::Local => Level::Local,
            RemoteLevel::User => Level::User,
        }
    }

    /// The remote level of a file level; a remote never comes from the defaults.
    fn of(level: Level) -> RemoteLevel {
        match level {
            Level::Project | Level::Default => RemoteLevel::Project,
            Level::Local => RemoteLevel::Local,
            Level::User => RemoteLevel::User,
        }
    }
}

/// One remote: its name, canonical URL, mode, the level it comes from and its install origin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RemoteRow {
    pub name: String,
    pub url: String,
    pub mode: RemoteMode,
    pub level: RemoteLevel,
    pub install: Origin,
}

impl RemoteRow {
    fn of(r: &Remote) -> RemoteRow {
        RemoteRow {
            name: r.name.clone(),
            url: r.url.as_str().to_string(),
            mode: r.mode,
            level: RemoteLevel::of(r.level),
            install: r.url.origin().clone(),
        }
    }
}

/// The reply of an edit: the effective row after an add or a mode change, or the removed
/// selected-level row with `removed: true`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RemoteReply {
    pub remote: RemoteRow,
    pub removed: bool,
    pub warnings: Vec<Issue>,
}

/// The remotes in effect, sorted by name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RemoteList {
    pub remotes: Vec<RemoteRow>,
    pub warnings: Vec<Issue>,
}

/// `E_CONFIG`: there is no user config path to lock or save.
pub fn user_config_unavailable() -> RdyError {
    RdyError::new(
        E_CONFIG,
        "the user config path is not available",
        "set XDG_CONFIG_HOME or HOME before changing remotes",
    )
}

fn already_exists(name: &str) -> RdyError {
    RdyError::new(
        E_ALREADY_EXISTS,
        format!("remote {name} already exists at this config level"),
        "change that remote, or choose another name",
    )
}

fn not_at_level(name: &str) -> RdyError {
    RdyError::new(
        E_NOT_FOUND,
        format!("remote {name} does not exist at this config level"),
        "list the remotes and use the level that defines it",
    )
}

fn check_name(name: &str) -> Result<(), RdyError> {
    if is_remote_name(name) {
        Ok(())
    } else {
        Err(RdyError::new(
            E_BAD_INPUT,
            BAD_REMOTE_NAME_MESSAGE,
            BAD_REMOTE_NAME_FIX,
        ))
    }
}

/// What an edit does to the selected level's entry.
enum Edit {
    Add(RemoteEntry),
    Remove,
    SetMode(RemoteMode),
}

/// The remote edits of one project and one user config.
#[derive(Clone, Debug)]
pub struct RemoteService {
    project: PathBuf,
    user_file: PathBuf,
}

impl RemoteService {
    /// The remotes of the project at `project`, with the user config at `user_file`
    /// (`<home>/rdy/config.yaml`, as `config_levels::user_file_from_env` names it).
    pub fn new(project: &Path, user_file: &Path) -> RemoteService {
        RemoteService {
            project: project.to_path_buf(),
            user_file: user_file.to_path_buf(),
        }
    }

    /// [`RemoteService::new`] with the environment's user config. `E_CONFIG` when neither
    /// `XDG_CONFIG_HOME` nor `HOME` is set.
    pub fn from_env(project: &Path) -> Result<RemoteService, RdyError> {
        let user_file = config_levels::user_file_from_env().ok_or_else(user_config_unavailable)?;
        Ok(RemoteService::new(project, &user_file))
    }

    /// The remotes in effect, sorted by name, with the hub settings' warnings.
    pub fn list(&self) -> Result<RemoteList, RdyError> {
        config_levels::require_project(&self.project)?;
        let c = HubConfig::load(Some(&self.project), Some(&self.user_file))?;
        Ok(RemoteList {
            remotes: c.remotes().iter().map(RemoteRow::of).collect(),
            warnings: c.warnings,
        })
    }

    /// Adds remote `name` at `level`. An entry already at that level is `E_ALREADY_EXISTS`.
    pub fn add(
        &self,
        name: &str,
        url: &str,
        mode: RemoteMode,
        level: RemoteLevel,
    ) -> Result<RemoteReply, RdyError> {
        check_name(name)?;
        let url = RemoteUrl::parse(url)?;
        self.edit(name, level, Edit::Add(RemoteEntry { url, mode }))
    }

    /// Removes remote `name` from `level` only. No entry at that level is `E_NOT_FOUND`.
    pub fn remove(&self, name: &str, level: RemoteLevel) -> Result<RemoteReply, RdyError> {
        check_name(name)?;
        self.edit(name, level, Edit::Remove)
    }

    /// Sets the mode of remote `name` at `level` only. No entry at that level is `E_NOT_FOUND`.
    pub fn set_mode(
        &self,
        name: &str,
        mode: RemoteMode,
        level: RemoteLevel,
    ) -> Result<RemoteReply, RdyError> {
        check_name(name)?;
        self.edit(name, level, Edit::SetMode(mode))
    }

    /// The user config's home and its path below it: `<home>/<dir>/<file>`.
    fn user_home(&self) -> Result<(PathBuf, PathBuf), RdyError> {
        let f = &self.user_file;
        let name = f.file_name().ok_or_else(user_config_unavailable)?;
        let dir = f.parent().ok_or_else(user_config_unavailable)?;
        let dir_name = dir.file_name().ok_or_else(user_config_unavailable)?;
        let home = dir
            .parent()
            .filter(|h| !h.as_os_str().is_empty())
            .ok_or_else(user_config_unavailable)?;
        Ok((home.to_path_buf(), Path::new(dir_name).join(name)))
    }

    fn shown(&self, level: Level) -> String {
        config_levels::file_name(level, Some(&self.user_file))
    }

    /// One locked file's settings; a missing file is empty. A file that does not parse is
    /// named, and nothing from it is quoted.
    fn parse(&self, locked: &LockedConfig, level: Level) -> Result<Mapping, RdyError> {
        hub_config::parse_settings(locked.bytes()).map_err(|e| {
            if e.code == E_CONFIG {
                hub_config::settings_invalid(Some(&self.shown(level)))
            } else {
                e
            }
        })
    }

    /// The hub settings in effect over `files` (each level with its whole settings mapping).
    fn merged(&self, files: &[(Level, &Mapping)]) -> HubMerge {
        let mut parts = Vec::new();
        let mut notes = Vec::new();
        for (level, map) in files {
            let mut map = (*map).clone();
            let (part, n) = hub_config::take_level(*level, &mut map, &self.shown(*level));
            parts.push((*level, part));
            notes.extend(n);
        }
        hub_config::merge(parts, notes, Some(&self.project), Some(&self.user_file))
    }

    fn edit(&self, name: &str, level: RemoteLevel, edit: Edit) -> Result<RemoteReply, RdyError> {
        config_levels::require_project(&self.project)?;
        let (home, rel) = self.user_home()?;
        // the user config lock orders every remote edit; the destination's lock comes second
        let mut user = LockedConfig::open(&home, &rel, false, MAX_CONFIG_BYTES)?;
        let dest_rel = match level {
            RemoteLevel::User => None,
            RemoteLevel::Project => Some(PROJECT_FILE),
            RemoteLevel::Local => Some(LOCAL_FILE),
        };
        let mut dest = match dest_rel {
            Some(rel) => Some(LockedConfig::open(
                &self.project,
                Path::new(rel),
                false,
                MAX_CONFIG_BYTES,
            )?),
            None => None,
        };
        let reply = self.edit_locked(name, level.level(), edit, &mut user, dest.as_mut());
        // released in reverse order
        drop(dest);
        drop(user);
        reply
    }

    fn edit_locked(
        &self,
        name: &str,
        level: Level,
        edit: Edit,
        user: &mut LockedConfig,
        dest: Option<&mut LockedConfig>,
    ) -> Result<RemoteReply, RdyError> {
        // all three levels, read again under the locks: a locked file from its bytes, the
        // project file that is not the destination from its path
        let user_map = self.parse(user, Level::User)?;
        let read = |l: Level, rel: &str| -> Result<Mapping, RdyError> {
            match &dest {
                Some(d) if l == level => self.parse(d, l),
                _ => Ok(
                    config_levels::read_file(&self.project.join(rel), &self.shown(l))?
                        .unwrap_or_default(),
                ),
            }
        };
        let project_map = read(Level::Project, PROJECT_FILE)?;
        let local_map = read(Level::Local, LOCAL_FILE)?;
        let current = [
            (Level::User, &user_map),
            (Level::Project, &project_map),
            (Level::Local, &local_map),
        ];
        let warnings = self.merged(&current).warnings;
        let refuse = |mut e: RdyError| {
            e.issues = warnings.clone();
            e
        };

        let old = match level {
            Level::User => &user_map,
            Level::Project => &project_map,
            _ => &local_map,
        };
        // an add refuses any entry the file holds under the name, one the loader drops included,
        // so it never replaces what the file keeps there
        let present = matches!(old.get(hub_config::REMOTES_KEY),
            Some(Value::Mapping(rs)) if rs.contains_key(name));
        // the selected level's entry, as the loader reads that file
        let at_level = self
            .merged(&[(level, old)])
            .remotes
            .into_iter()
            .find(|r| r.name == name);
        let (entry, removed) = match (edit, at_level) {
            // every entry the loader reads is a key of that mapping
            (Edit::Add(_), _) if present => return Err(refuse(already_exists(name))),
            (Edit::Add(e), _) => (Some(e), None),
            (_, None) => return Err(refuse(not_at_level(name))),
            (Edit::Remove, Some(r)) => (None, Some(r)),
            (Edit::SetMode(mode), Some(r)) => (Some(RemoteEntry { url: r.url, mode }), None),
        };
        // staged without a per-file share check: one file may hold a second share entry that
        // another level overrides, so the single share remote is checked on the merged config
        let mut next = old.clone();
        hub_config::put_remote(&mut next, name, entry).map_err(refuse)?;

        // the config as it would be after the edit, a row that a removal uncovers included
        let prospective: Vec<(Level, &Mapping)> = current
            .iter()
            .map(|&(l, m)| (l, if l == level { &next } else { m }))
            .collect();
        let after = self.merged(&prospective);
        if after.remotes.len() > MAX_REMOTES {
            return Err(refuse(hub_config::config_limit(
                MAX_REMOTES,
                LimitUnit::Remotes,
            )));
        }
        let shares = after
            .remotes
            .iter()
            .filter(|r| r.mode == RemoteMode::Share)
            .count();
        if shares > 1 {
            return Err(refuse(remote_exists()));
        }

        if next != *old {
            let bytes = hub_config::encode_settings(&next).map_err(refuse)?;
            dest.unwrap_or(user).replace(&bytes).map_err(refuse)?;
        }
        let remote = match &removed {
            Some(r) => RemoteRow::of(r),
            None => after
                .remotes
                .iter()
                .find(|r| r.name == name)
                .map(RemoteRow::of)
                .ok_or_else(|| refuse(not_at_level(name)))?,
        };
        Ok(RemoteReply {
            remote,
            removed: removed.is_some(),
            warnings: after.warnings,
        })
    }
}
