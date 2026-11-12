//! The local hook host (FW-HOOK D-NNQ0A, D-GHR0Y; FW-L3 D-54CKQ, refined by D-DC5D1; first edit
//! adapter D-KG0V6): it installs and removes the two optional hooks, outside the store writer.
//!
//! - The code hook is a Git `commit-msg` launcher in the effective hooks folder of a linked
//!   code repo's checkout (`git rev-parse --git-path hooks`, never `core.hooksPath` changed). It
//!   calls this binary with the project named and always exits 0 ([`crate::sys::code_launcher`]).
//! - The edit hook is one `PostToolUse` entry for `Edit` and `Write` in Claude Code's
//!   project-local settings, `.claude/settings.local.json`, of the main project root or one
//!   registered worktree: the exec form (an executable and an argument vector, no shell) with
//!   `onFailure: continue`.
//!
//! The host knows its binary and the project root; a request names only a repo, a harness and a
//! worktree. It creates only absent files and keeps an exact owned file. It never overwrites a
//! person's file or edit: an existing hook, a changed owned file, an external hooks path,
//! settings it cannot parse, a link and a file that changed under it each leave the file as it
//! is, with a `W_HOOK_CONFLICT` warning. It removes only exact owned bytes and entries. Every
//! entry is opened through its directory's handle without following a link, and every
//! replacement checks the file's identity and content again just before it is published. One
//! bounded file lock (`.rdy/hooks.lock`, [`HOOK_DEADLINE`]) serializes the installs of a
//! project; a busy lock changes nothing.
use rdy_engine::hooks::{
    CLAUDE_CODE, Conflict, FileResult, HOOK_DEADLINE, HookCheck, HookFile, HookHost, HookInstall,
    HookKind, HookTarget, HookWarning, MAX_HOOK_INPUT, MAX_LAUNCHER_BYTES, RunContext, Unavailable,
    form_error,
};
use rdy_engine::wc::state::{Place, WORKTREES_DIR, worktrees};
use rdy_engine::{E_BAD_INPUT, E_IO, E_LIMIT, RdyError};
use rdy_store::fileid::FileId;
use rdy_store::pio::{self, EntryKind};
use rdy_store::start_lock::FileLock;
use serde_json::{Map, Value, json};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The installer's lock file, in the project's `.rdy` folder.
pub const LOCK_FILE: &str = "hooks.lock";
/// The Git hook the code hook is.
pub const CODE_HOOK: &str = "commit-msg";
/// The folder of Claude Code's project settings, under a checkout's root.
pub const SETTINGS_DIR: &str = ".claude";
/// Claude Code's project-local settings file, in [`SETTINGS_DIR`].
pub const SETTINGS_FILE: &str = "settings.local.json";
/// The settings event the edit hook answers.
pub const EDIT_EVENT: &str = "PostToolUse";
/// The tools whose successful calls the edit hook sees (D-KG0V6).
pub const EDIT_MATCHER: &str = "Edit|Write";
/// How often a waiting install tries the lock again.
const LOCK_POLL: Duration = Duration::from_millis(10);

/// A test's last-moment change: run with the target's path right before a file is published.
type BeforePublish = Box<dyn FnMut(&Path) + Send>;

/// What a test makes the next publication do.
#[derive(Default)]
struct Faults {
    /// Run with the target's path right before a file is published.
    before_publish: Mutex<Option<BeforePublish>>,
    /// The next publication fails after its bytes are durable, and is cleaned up.
    fail_publish: AtomicBool,
}

/// The local [`HookHost`]: the project at `root` and the rdy binary `exe`.
pub struct LocalHooks {
    root: PathBuf,
    exe: PathBuf,
    faults: Arc<Faults>,
}

/// Where an install or remove acts, or its result when it cannot act.
enum Located {
    /// The file `name` in the folder `dir`; `make_dir` when the folder may be created.
    File {
        dir: PathBuf,
        make_dir: bool,
        name: &'static str,
    },
    /// Nothing to act on: the result says why.
    Done(HookInstall),
}

/// Install or remove.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Install,
    Remove,
}

/// One step's outcome for one file.
struct Step {
    result: FileResult,
    warning: Option<HookWarning>,
}

impl Step {
    fn done(result: FileResult) -> Step {
        Step {
            result,
            warning: None,
        }
    }

    fn conflict(path: &Path, reason: Conflict) -> Step {
        Step {
            result: FileResult::Skipped,
            warning: Some(HookWarning::conflict(&pio::display_path(path), reason)),
        }
    }

    fn unavailable(reason: Unavailable, cause: &RdyError) -> Step {
        Step {
            result: FileResult::Skipped,
            warning: Some(HookWarning::unavailable(reason, Some(cause))),
        }
    }

    fn io(path: &Path, e: io::Error) -> Step {
        Step::unavailable(Unavailable::Io, &io_error(path, e))
    }
}

fn io_error(path: &Path, e: io::Error) -> RdyError {
    RdyError::new(
        E_IO,
        format!("{}: {e}", pio::display_path(path)),
        "check the file and its folder",
    )
}

/// A result about no file, with one warning.
fn no_file(kind: HookKind, warning: HookWarning) -> HookInstall {
    HookInstall {
        kind,
        files: vec![],
        warnings: vec![warning],
    }
}

/// A result about one file.
fn one_file(kind: HookKind, path: &Path, step: Step) -> HookInstall {
    HookInstall {
        kind,
        files: vec![HookFile {
            path: pio::display_path(path),
            result: step.result,
        }],
        warnings: step.warning.into_iter().collect(),
    }
}

impl LocalHooks {
    /// The host of the project at `root`, whose hooks run the rdy binary `exe` (absolute).
    pub fn new(root: impl Into<PathBuf>, exe: impl Into<PathBuf>) -> LocalHooks {
        LocalHooks {
            root: root.into(),
            exe: exe.into(),
            faults: Arc::default(),
        }
    }

    /// The host of the project at `root`, run by this process's own binary.
    pub fn for_process(root: impl Into<PathBuf>) -> io::Result<LocalHooks> {
        Ok(LocalHooks::new(root, std::env::current_exe()?))
    }

    /// The project root, canonical when it resolves.
    fn root(&self) -> PathBuf {
        std::fs::canonicalize(&self.root).unwrap_or_else(|_| self.root.clone())
    }

    /// The `commit-msg` launcher this host writes for the code repo `repo`; `None` when a path
    /// or the name cannot be written into one ([`crate::sys::code_launcher`]).
    pub fn code_launcher(&self, repo: &str) -> Option<Vec<u8>> {
        crate::sys::code_launcher(&self.exe, &self.root(), repo)
    }

    /// The command entry this host puts in Claude Code's settings: the exec form, with the
    /// project named. `None` when a path is not Unicode.
    pub fn edit_command(&self) -> Option<Value> {
        let exe = crate::sys::hook_exec_path(&self.exe)?;
        let project = crate::sys::hook_exec_path(&self.root())?;
        Some(json!({
            "type": "command",
            "command": exe,
            "args": ["--project", project, "hook", "run-edit", "--harness", CLAUDE_CODE],
            "onFailure": "continue",
        }))
    }

    /// The `PostToolUse` entry this host owns: the matcher and its one command.
    pub fn edit_entry(&self) -> Option<Value> {
        Some(json!({"matcher": EDIT_MATCHER, "hooks": [self.edit_command()?]}))
    }

    /// The settings bytes this host writes when no settings file stands.
    pub fn fresh_settings(&self) -> Option<Vec<u8>> {
        settings_bytes(&json!({"hooks": {EDIT_EVENT: [self.edit_entry()?]}}))
    }

    /// Test hook: `f` runs with the target's path right before each publication, so a test can
    /// change the file at the last moment.
    #[doc(hidden)]
    pub fn before_publish_for_tests(&self, f: impl FnMut(&Path) + Send + 'static) {
        *self
            .faults
            .before_publish
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(Box::new(f));
    }

    /// Test hook: the next publication fails once its bytes are durable, and is cleaned up.
    #[doc(hidden)]
    pub fn fail_next_publish_for_tests(&self) {
        self.faults.fail_publish.store(true, Ordering::SeqCst);
    }

    /// Test hook: holds the installer's lock as a concurrent install would.
    #[doc(hidden)]
    pub fn hold_lock_for_tests(&self) -> io::Result<FileLock> {
        FileLock::try_take(&self.lock_path())?
            .ok_or_else(|| io::Error::new(io::ErrorKind::WouldBlock, "the hooks lock is held"))
    }

    fn lock_path(&self) -> PathBuf {
        self.root().join(".rdy").join(LOCK_FILE)
    }

    /// The installer's lock, tried at once and then until the earlier of the run's deadline
    /// and [`HOOK_DEADLINE`] from now. `Ok(None)`: busy.
    fn lock(&self, ctx: &RunContext) -> Result<Option<FileLock>, RdyError> {
        let path = self.lock_path();
        let mut until = Instant::now() + HOOK_DEADLINE;
        if let Some(d) = ctx.deadline() {
            until = until.min(d);
        }
        loop {
            match FileLock::try_take(&path) {
                Ok(Some(lock)) => return Ok(Some(lock)),
                Ok(None) => {}
                Err(e) => return Err(io_error(&path, e)),
            }
            let now = Instant::now();
            if now >= until || ctx.is_cancelled() {
                return Ok(None);
            }
            std::thread::sleep(LOCK_POLL.min(until - now));
        }
    }

    /// Runs the test's last-moment change, right before a file is published.
    fn publishing(&self, path: &Path) {
        if let Some(f) = self
            .faults
            .before_publish
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            f(path);
        }
    }

    /// The test's injected failure, once a file's bytes are durable.
    fn injected(&self) -> io::Result<()> {
        if self.faults.fail_publish.swap(false, Ordering::SeqCst) {
            return Err(io::Error::other("injected failure after a durable write"));
        }
        Ok(())
    }

    /// Where the code hook of `repo` goes: the effective hooks folder of its checkout, which
    /// must be in that checkout's Git administration area.
    fn locate_code(&self, repo: &str) -> Result<Located, RdyError> {
        let kind = HookKind::Code;
        let user = rdy_engine::config_levels::user_file_from_env();
        let linked = match rdy_engine::config_levels::linked_repos(&self.root(), user.as_deref()) {
            Ok((linked, _)) => linked,
            Err(e) => {
                let w = HookWarning::unavailable(Unavailable::Project, Some(&e));
                return Ok(Located::Done(no_file(kind, w)));
            }
        };
        let Some(entry) = linked.iter().find(|l| l.name == repo) else {
            let names: Vec<&str> = linked.iter().map(|l| l.name.as_str()).collect();
            let hint = match rdy_engine::did_you_mean::nearest(repo, &names) {
                Some(near) => format!(" (did you mean \"{near}\"?)"),
                None => String::new(),
            };
            return Err(form_error(format!(
                "repo \"{repo}\" is no code_repos entry with a local checkout{hint}"
            )));
        };
        let unavailable = |reason: Unavailable, e: &RdyError| {
            Ok(Located::Done(no_file(
                kind,
                HookWarning::unavailable(reason, Some(e)),
            )))
        };
        let checkout = match std::fs::canonicalize(&entry.path) {
            Ok(p) => p,
            Err(e) => return unavailable(Unavailable::Checkout, &io_error(&entry.path, e)),
        };
        let (common, hooks) = match git_hook_paths(&checkout) {
            Ok(paths) => paths,
            Err(e) => return unavailable(Unavailable::Checkout, &e),
        };
        let file = hooks.join(CODE_HOOK);
        let common = match std::fs::canonicalize(&common) {
            Ok(p) => p,
            Err(e) => return unavailable(Unavailable::Checkout, &io_error(&common, e)),
        };
        // the folder that holds the hooks folder: the hooks folder itself is checked unfollowed
        let inside = hooks.parent().and_then(|p| std::fs::canonicalize(p).ok());
        if hooks.file_name().is_none() || !inside.is_some_and(|p| p.starts_with(&common)) {
            let step = Step::conflict(&file, Conflict::ExternalHooksPath);
            return Ok(Located::Done(one_file(kind, &file, step)));
        }
        Ok(Located::File {
            dir: hooks,
            make_dir: true,
            name: CODE_HOOK,
        })
    }

    /// Where the edit hook of `harness` goes: `.claude/settings.local.json` under the main
    /// project root, or under the registered worktree `worktree`.
    fn locate_edit(&self, worktree: Option<&str>) -> Result<Located, RdyError> {
        let root = self.root();
        let checkout = match worktree {
            None => root.clone(),
            Some(name) => {
                let all = worktrees(&root);
                let Some(wt) = all.iter().find(|w| w.name == name) else {
                    let names: Vec<&str> = all.iter().map(|w| w.name.as_str()).collect();
                    let hint = match rdy_engine::did_you_mean::nearest(name, &names) {
                        Some(near) => format!(" (did you mean \"{near}\"?)"),
                        None => String::new(),
                    };
                    return Err(form_error(format!(
                        "worktree \"{name}\" is no registered worktree{hint}"
                    )));
                };
                match registered_dir(&root, &wt.name, &wt.dir) {
                    Some(dir) => dir,
                    None => {
                        let e = RdyError::new(
                            E_IO,
                            format!("worktree \"{name}\" has no folder that points back to it"),
                            "check the worktree's folder",
                        );
                        let w = HookWarning::unavailable(Unavailable::Checkout, Some(&e));
                        return Ok(Located::Done(no_file(HookKind::Edit, w)));
                    }
                }
            }
        };
        Ok(Located::File {
            dir: checkout.join(SETTINGS_DIR),
            make_dir: true,
            name: SETTINGS_FILE,
        })
    }

    fn apply(
        &self,
        target: &HookTarget,
        ctx: &RunContext,
        mode: Mode,
    ) -> Result<HookInstall, RdyError> {
        let kind = target.kind();
        let located = match target {
            HookTarget::Code { repo } => self.locate_code(repo)?,
            HookTarget::Edit { worktree, .. } => self.locate_edit(worktree.as_deref())?,
        };
        let (dir, make_dir, name) = match located {
            Located::Done(r) => return Ok(r),
            Located::File {
                dir,
                make_dir,
                name,
            } => (dir, make_dir, name),
        };
        let path = dir.join(name);
        let content = match target {
            HookTarget::Code { repo } => self.code_launcher(repo).map(Content::Launcher),
            HookTarget::Edit { .. } => self.edit_entry().map(Content::Settings),
        };
        let Some(content) = content else {
            return Ok(one_file(
                kind,
                &path,
                Step::conflict(&path, Conflict::UnsafePath),
            ));
        };
        let Some(_lock) = self.lock(ctx)? else {
            return Ok(one_file(
                kind,
                &path,
                Step::conflict(&path, Conflict::InstallerBusy),
            ));
        };
        let step = match open_folder(&dir, make_dir && mode == Mode::Install) {
            Folder::Open(handle) => self.step(&handle, &path, name, &content, mode),
            Folder::Absent => Step::done(FileResult::Kept),
            Folder::Unsafe => Step::conflict(&path, Conflict::UnsafePath),
            Folder::Failed(e) => Step::io(&dir, e),
        };
        Ok(one_file(kind, &path, step))
    }

    fn step(&self, dir: &File, path: &Path, name: &str, content: &Content, mode: Mode) -> Step {
        let name = OsStr::new(name);
        let existing = match pio::entry_kind_at(dir, name) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Step::io(path, e),
            Ok(EntryKind::File) => match read_entry(dir, name, content.limit()) {
                Ok(read) => Some(read),
                Err(e) if pio::is_link_refusal(&e) => {
                    return Step::conflict(path, Conflict::UnsafePath);
                }
                Err(e) => return Step::io(path, e),
            },
            Ok(_) => return Step::conflict(path, Conflict::UnsafePath),
        };
        match content {
            Content::Launcher(bytes) => self.launcher_step(dir, path, name, bytes, existing, mode),
            Content::Settings(entry) => self.settings_step(dir, path, name, entry, existing, mode),
        }
    }

    fn launcher_step(
        &self,
        dir: &File,
        path: &Path,
        name: &OsStr,
        ours: &[u8],
        existing: Option<Existing>,
        mode: Mode,
    ) -> Step {
        let Some(old) = existing else {
            return match mode {
                Mode::Install => self.create(dir, path, name, ours, true),
                Mode::Remove => Step::done(FileResult::Kept),
            };
        };
        let Some(bytes) = old.bytes.as_deref() else {
            return Step::conflict(path, Conflict::ExistingHook);
        };
        if bytes != ours {
            let owned = bytes.starts_with(crate::sys::LAUNCHER_HEAD.as_bytes());
            return Step::conflict(
                path,
                if owned {
                    Conflict::ChangedOwnedFile
                } else {
                    Conflict::ExistingHook
                },
            );
        }
        match mode {
            Mode::Install => Step::done(FileResult::Kept),
            Mode::Remove => self.remove(dir, path, name, &old),
        }
    }

    fn settings_step(
        &self,
        dir: &File,
        path: &Path,
        name: &OsStr,
        entry: &Value,
        existing: Option<Existing>,
        mode: Mode,
    ) -> Step {
        let Some(old) = existing else {
            return match (
                mode,
                settings_bytes(&json!({"hooks": {EDIT_EVENT: [entry]}})),
            ) {
                (Mode::Install, Some(bytes)) => self.create(dir, path, name, &bytes, false),
                (Mode::Install, None) => Step::conflict(path, Conflict::InvalidSettings),
                (Mode::Remove, _) => Step::done(FileResult::Kept),
            };
        };
        let Some(bytes) = old.bytes.as_deref() else {
            return Step::conflict(path, Conflict::InvalidSettings);
        };
        let Ok(Value::Object(mut settings)) = serde_json::from_slice::<Value>(bytes) else {
            return Step::conflict(path, Conflict::InvalidSettings);
        };
        let Some(command) = entry["hooks"].get(0) else {
            return Step::conflict(path, Conflict::InvalidSettings);
        };
        let Some(found) = find_entry(&settings, entry, command) else {
            return Step::conflict(path, Conflict::InvalidSettings);
        };
        match (mode, found) {
            (Mode::Install, Found::Exact) => Step::done(FileResult::Kept),
            (_, Found::Changed) => Step::conflict(path, Conflict::ChangedOwnedFile),
            (Mode::Remove, Found::None) => Step::done(FileResult::Kept),
            (Mode::Install, Found::None) => {
                let hooks = settings
                    .entry("hooks")
                    .or_insert_with(|| Value::Object(Map::new()));
                if let Some(hooks) = hooks.as_object_mut() {
                    let list = hooks
                        .entry(EDIT_EVENT)
                        .or_insert_with(|| Value::Array(vec![]));
                    if let Some(list) = list.as_array_mut() {
                        list.push(entry.clone());
                    }
                }
                match settings_bytes(&Value::Object(settings)) {
                    Some(new) => self.replace(dir, path, name, &old, &new, FileResult::Written),
                    None => Step::conflict(path, Conflict::InvalidSettings),
                }
            }
            (Mode::Remove, Found::Exact) => {
                strip_entry(&mut settings, entry);
                // the whole file goes only when it is exactly what an install writes fresh
                if self.fresh_settings().as_deref() == Some(bytes) {
                    return self.remove(dir, path, name, &old);
                }
                match settings_bytes(&Value::Object(settings)) {
                    Some(new) => self.replace(dir, path, name, &old, &new, FileResult::Removed),
                    None => Step::conflict(path, Conflict::InvalidSettings),
                }
            }
        }
    }

    /// Creates the absent file `name` with `bytes`, exclusively and never through a link. A
    /// file that appeared meanwhile is left as it is. A failed write removes what it made.
    fn create(&self, dir: &File, path: &Path, name: &OsStr, bytes: &[u8], exec: bool) -> Step {
        self.publishing(path);
        let f = match pio::create_private_at(dir, name) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                return Step::conflict(path, Conflict::FileChanged);
            }
            Err(e) => return Step::io(path, e),
        };
        let made = FileId::of_file(&f);
        let written = write_durable(&f, bytes)
            .and_then(|()| {
                if exec {
                    crate::sys::make_launcher_executable(dir, name)
                } else {
                    Ok(())
                }
            })
            .and_then(|()| pio::sync_open_dir(dir, path.parent().unwrap_or(path)))
            .and_then(|()| self.injected());
        // closed before any removal: Windows removes no file this handle keeps open
        drop(f);
        match written {
            Ok(()) => Step::done(FileResult::Written),
            Err(e) => {
                // only the file made here, never a file put in its place
                if let Ok(made) = made
                    && FileId::lstat(path).is_ok_and(|now| now == made)
                {
                    let _ = pio::remove_file_at(dir, name);
                }
                Step::io(path, e)
            }
        }
    }

    /// Replaces the file `name`, read as `old`, with `bytes`: a private temporary file in the
    /// same folder, then a rename over it once the file is still `old`, by identity and by
    /// content. A file that changed is left as it is.
    fn replace(
        &self,
        dir: &File,
        path: &Path,
        name: &OsStr,
        old: &Existing,
        bytes: &[u8],
        done: FileResult,
    ) -> Step {
        let tmp = temp_name(name);
        let f = match pio::create_private_at(dir, &tmp) {
            Ok(f) => f,
            Err(e) => return Step::io(path, e),
        };
        let drop_tmp = || {
            let _ = pio::remove_file_at(dir, &tmp);
        };
        let written = write_durable(&f, bytes).and_then(|()| self.injected());
        // closed before the rename or the removal: Windows moves no file this handle keeps open
        drop(f);
        if let Err(e) = written {
            drop_tmp();
            return Step::io(path, e);
        }
        self.publishing(path);
        if !old.still_at(dir, path, name) {
            drop_tmp();
            return Step::conflict(path, Conflict::FileChanged);
        }
        match pio::rename_at(dir, &tmp, name)
            .and_then(|()| pio::sync_open_dir(dir, path.parent().unwrap_or(path)))
        {
            Ok(()) => Step::done(done),
            Err(e) => {
                drop_tmp();
                Step::io(path, e)
            }
        }
    }

    /// Removes the file `name`, read as `old`, once it is still `old`.
    fn remove(&self, dir: &File, path: &Path, name: &OsStr, old: &Existing) -> Step {
        if let Err(e) = self.injected() {
            return Step::io(path, e);
        }
        self.publishing(path);
        if !old.still_at(dir, path, name) {
            return Step::conflict(path, Conflict::FileChanged);
        }
        match pio::remove_file_at(dir, name)
            .and_then(|()| pio::sync_open_dir(dir, path.parent().unwrap_or(path)))
        {
            Ok(()) => Step::done(FileResult::Removed),
            Err(e) => Step::io(path, e),
        }
    }
}

impl HookHost for LocalHooks {
    fn install(&self, target: &HookTarget, ctx: &RunContext) -> Result<HookInstall, RdyError> {
        self.apply(target, ctx, Mode::Install)
    }

    fn remove(&self, target: &HookTarget, ctx: &RunContext) -> Result<HookInstall, RdyError> {
        self.apply(target, ctx, Mode::Remove)
    }
}

/// What a hook file holds when rdy owns it.
enum Content {
    /// A launcher's exact bytes.
    Launcher(Vec<u8>),
    /// A settings entry, merged into what stands.
    Settings(Value),
}

impl Content {
    /// The most bytes an existing file of this kind is read for.
    fn limit(&self) -> usize {
        match self {
            Content::Launcher(_) => MAX_LAUNCHER_BYTES,
            Content::Settings(_) => MAX_HOOK_INPUT,
        }
    }
}

/// An existing file as read: its identity and its bytes, `None` when it is over the limit (not
/// read in full).
struct Existing {
    id: FileId,
    bytes: Option<Vec<u8>>,
}

impl Existing {
    /// Whether the file `name` is still this one, by identity and by content.
    fn still_at(&self, dir: &File, path: &Path, name: &OsStr) -> bool {
        if !FileId::lstat(path).is_ok_and(|now| now == self.id) {
            return false;
        }
        let limit = self.bytes.as_ref().map_or(0, Vec::len);
        match read_entry(dir, name, limit) {
            Ok(again) => again.id == self.id && again.bytes == self.bytes,
            Err(_) => false,
        }
    }
}

/// Reads the regular file `name` of `dir`, never through a link, up to `limit` bytes; a larger
/// file is not read in full.
fn read_entry(dir: &File, name: &OsStr, limit: usize) -> io::Result<Existing> {
    let f = pio::open_read_at(dir, name)?;
    let meta = f.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    let id = FileId::of_file(&f)?;
    if meta.len() > limit as u64 {
        return Ok(Existing { id, bytes: None });
    }
    let mut bytes = Vec::new();
    (&f).take(limit as u64 + 1).read_to_end(&mut bytes)?;
    Ok(Existing {
        id,
        bytes: (bytes.len() <= limit).then_some(bytes),
    })
}

fn write_durable(mut f: &File, bytes: &[u8]) -> io::Result<()> {
    f.write_all(bytes)?;
    f.sync_all()
}

/// A temporary name beside `name`, unique in this process.
fn temp_name(name: &OsStr) -> OsString {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut t = OsString::from(".");
    t.push(name);
    t.push(format!(
        ".rdy-{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    t
}

/// A hook file's folder.
enum Folder {
    Open(File),
    /// It does not exist (and is not to be made).
    Absent,
    /// A link or something other than a folder stands there.
    Unsafe,
    Failed(io::Error),
}

/// Opens the folder `dir` through its parent's handle, never through a link at its own name;
/// when absent and `make` is set, creates it first.
fn open_folder(dir: &Path, make: bool) -> Folder {
    let (Some(parent), Some(name)) = (dir.parent(), dir.file_name()) else {
        return Folder::Unsafe;
    };
    let parent = match pio::open_dir(parent) {
        Ok(p) => p,
        Err(e) => return Folder::Failed(e),
    };
    match pio::entry_kind_at(&parent, name) {
        Ok(EntryKind::Dir) => {}
        Ok(_) => return Folder::Unsafe,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if !make {
                return Folder::Absent;
            }
            match pio::create_private_dir_at(&parent, name) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Folder::Failed(e),
            }
        }
        Err(e) => return Folder::Failed(e),
    }
    match pio::open_dir_at(&parent, name) {
        Ok(d) => Folder::Open(d),
        Err(e) if pio::is_link_refusal(&e) => Folder::Unsafe,
        Err(e) => Folder::Failed(e),
    }
}

/// The settings text rdy writes: pretty JSON and a final newline. `None` past the settings
/// bound.
fn settings_bytes(v: &Value) -> Option<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(v).ok()?;
    bytes.push(b'\n');
    (bytes.len() <= MAX_HOOK_INPUT).then_some(bytes)
}

/// What the settings hold of rdy's entry.
enum Found {
    /// The exact entry.
    Exact,
    /// rdy's command, in an entry that someone changed.
    Changed,
    None,
}

/// Finds rdy's `entry` (with its one `command`) among the settings' `PostToolUse` entries.
/// `None`: the settings' `hooks` is not an object or its `PostToolUse` not a list, so nothing
/// can be merged safely.
fn find_entry(settings: &Map<String, Value>, entry: &Value, command: &Value) -> Option<Found> {
    let Some(hooks) = settings.get("hooks") else {
        return Some(Found::None);
    };
    let hooks = hooks.as_object()?;
    let Some(list) = hooks.get(EDIT_EVENT) else {
        return Some(Found::None);
    };
    let list = list.as_array()?;
    if list.iter().any(|e| e == entry) {
        return Some(Found::Exact);
    }
    let ours = |c: &Value| {
        c.get("command") == command.get("command") && c.get("args") == command.get("args")
    };
    let changed = list.iter().any(|e| {
        e.get("hooks")
            .and_then(Value::as_array)
            .is_some_and(|cs| cs.iter().any(ours))
    });
    Some(if changed { Found::Changed } else { Found::None })
}

/// Removes every exact copy of rdy's `entry`, then an emptied `PostToolUse` list and an emptied
/// `hooks` object. Nothing else changes.
fn strip_entry(settings: &mut Map<String, Value>, entry: &Value) {
    let Some(Value::Object(hooks)) = settings.get_mut("hooks") else {
        return;
    };
    if let Some(Value::Array(list)) = hooks.get_mut(EDIT_EVENT) {
        list.retain(|e| e != entry);
        if list.is_empty() {
            hooks.remove(EDIT_EVENT);
        }
    }
    if hooks.is_empty() {
        settings.remove("hooks");
    }
}

/// The folder of the registered worktree `name` (its recorded `dir`), when it holds a pointer
/// back to that registration, as the edit check finds worktrees.
fn registered_dir(project: &Path, name: &str, dir: &str) -> Option<PathBuf> {
    if dir.is_empty() {
        return None;
    }
    let place = Place::from_pointer(Path::new(dir))?;
    let state = project.join(WORKTREES_DIR).join(name);
    let same = matches!(
        (std::fs::canonicalize(&place.state), std::fs::canonicalize(&state)),
        (Ok(a), Ok(b)) if a == b
    );
    if !same {
        return None;
    }
    std::fs::canonicalize(dir).ok()
}

/// The most bytes read from `git rev-parse` for the hook paths.
const MAX_REV_PARSE: u64 = 3 * 4096 + 64;

/// The checkout's common Git folder and its effective hooks folder, as Git resolves them
/// (`git rev-parse --git-common-dir --git-path hooks`); relative answers are under `checkout`.
fn git_hook_paths(checkout: &Path) -> Result<(PathBuf, PathBuf), RdyError> {
    let failed = |why: String| {
        RdyError::new(
            E_IO,
            format!("git rev-parse in {}: {why}", pio::display_path(checkout)),
            "check that the checkout is a Git checkout and that git runs",
        )
    };
    let mut c = rdy_engine::code_index::git_command(checkout);
    c.args(["rev-parse", "--git-common-dir", "--git-path", "hooks"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child = c.spawn().map_err(|e| failed(e.to_string()))?;
    let mut out = Vec::new();
    let read = match child.stdout.take() {
        Some(s) => s.take(MAX_REV_PARSE + 1).read_to_end(&mut out).map(drop),
        None => Err(io::Error::other("no output pipe")),
    };
    if read.is_err() || out.len() as u64 > MAX_REV_PARSE {
        let _ = child.kill();
    }
    let status = child.wait().map_err(|e| failed(e.to_string()))?;
    read.map_err(|e| failed(e.to_string()))?;
    if out.len() as u64 > MAX_REV_PARSE {
        return Err(failed("its answer is too long".into()));
    }
    if !status.success() {
        return Err(failed(format!("it ended with {status}")));
    }
    let text = String::from_utf8(out).map_err(|_| failed("its answer is not UTF-8".into()))?;
    let mut lines = text.lines();
    let (Some(common), Some(hooks), None) = (lines.next(), lines.next(), lines.next()) else {
        return Err(failed("its answer is not two paths".into()));
    };
    let under = |p: &str| checkout.join(p);
    Ok((under(common), under(hooks)))
}

// ---------------------------------------------------------------------------------------------
// Host runs
// ---------------------------------------------------------------------------------------------

/// A host run's result: the check's own result, or for a refusal the `W_HOOK_UNAVAILABLE` check
/// that says why, never an error. A host entry point (Git's launcher, a harness's command) turns
/// every operational failure into this warning and exits 0. A refused form is `invalid event`,
/// an input past a bound `input limit`, a file or Git failure `I/O failure`, anything else
/// `project unavailable`. The underlying error stays in the warning's diagnostic.
pub fn warn_only(kind: HookKind, result: Result<Value, RdyError>) -> Value {
    let e = match result {
        Ok(v) => return v,
        Err(e) => e,
    };
    let reason = match e.code {
        E_BAD_INPUT => Unavailable::InvalidEvent,
        E_LIMIT => Unavailable::InputLimit,
        E_IO => Unavailable::Io,
        _ => Unavailable::Project,
    };
    let check = HookCheck {
        kind,
        checked: false,
        warnings: vec![HookWarning::unavailable(reason, Some(&e))],
    };
    serde_json::to_value(check).unwrap_or(Value::Null)
}
