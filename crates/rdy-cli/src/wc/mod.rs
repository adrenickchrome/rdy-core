//! The working-copy commands (M2f-a): files on disk that a person edits and commits into a view.
//! Every command takes the working copy's lock first and holds it to the end.

use crate::Env;
use clap::Subcommand;
use rdy_daemon::ErrorReply;
use rdy_engine::wc::ignore::Ignore;
use rdy_engine::wc::layout::{WcTree, read_tree};
use rdy_engine::wc::state::{LOCK_WAIT, Place, WC_DIR, WcLock, WcState};
use rdy_engine::{E_BAD_INPUT, project_pack};
use rdy_schema::Pack;
use serde_json::Value;
use std::path::PathBuf;

mod add;
pub(crate) mod brief;
mod checkout;
mod commit;
pub(crate) mod new_files;
pub(crate) mod status;
mod update;
pub(crate) mod worktree;

#[derive(Subcommand)]
pub enum WcCmd {
    /// Write a view as files in this working copy and bind the working copy to it.
    Checkout {
        /// The view (default: the session's, `work` for the human).
        view: Option<String>,
        /// Discard uncommitted edits and overwrite untracked or ignored files in the way.
        #[arg(long)]
        force: bool,
    },
    /// Stage files, directories, deletions and renames for the next commit.
    Add {
        /// The files or directories to stage (`.` for everything).
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Record the staged files as one change in the bound view and write them back.
    Commit {
        /// The change's message.
        #[arg(short = 'm', long)]
        message: String,
        /// A decision this change carries out (id, key or path); repeat for more.
        #[arg(long)]
        implements: Vec<String>,
        /// An editorial change: its class (typo, restructure or clarify) and the reason. Give it
        /// once.
        #[arg(long, num_args = 2, value_names = ["CLASS", "REASON"], conflicts_with = "implements")]
        editorial: Vec<String>,
        /// A source this change follows (id, key or path); repeat for more.
        #[arg(long, value_name = "SOURCE", conflicts_with_all = ["implements", "editorial"])]
        reflects: Vec<String>,
        /// Record the code state of each written block with reflects globs.
        #[arg(long, conflicts_with_all = ["implements", "editorial"])]
        reflects_code: bool,
    },
    /// Bring the bound view's changes made elsewhere into the files, keeping local edits; what
    /// changed on both sides is written beside the file as .theirs.yaml.
    Update,
    /// Extra checkouts of this project, each bound to its own view.
    Worktree {
        #[command(subcommand)]
        cmd: worktree::WorktreeCmd,
    },
}

pub fn run(cmd: WcCmd, env: &Env) -> Result<(Value, String), ErrorReply> {
    match cmd {
        WcCmd::Checkout { view, force } => checkout::run(env, view, force),
        WcCmd::Add { paths } => add::run(env, paths),
        WcCmd::Commit {
            message,
            implements,
            editorial,
            reflects,
            reflects_code,
        } => commit::run(
            env,
            message,
            commit::Justify {
                implements,
                editorial,
                reflects,
                reflects_code,
            },
        ),
        WcCmd::Update => update::run(env),
        WcCmd::Worktree { cmd } => worktree::run(env, cmd),
    }
}

/// A working copy opened for one command: its place, its lock (held while this lives) and its
/// state.
pub(crate) struct Open {
    pub place: Place,
    #[allow(dead_code)] // held, never read: dropping it releases the lock
    pub lock: WcLock,
    pub state: WcState,
}

/// The working copy the command runs in: its place, the lock (waiting up to `LOCK_WAIT`) and its
/// state, with an interrupted commit finished or dropped first (`commit::recover`);
/// `E_WC_NOT_BOUND` when `require_bound` and nothing is checked out.
pub(crate) fn open(env: &Env, require_bound: bool) -> Result<Open, ErrorReply> {
    let place = env.place.clone().ok_or_else(|| {
        ErrorReply::new(
            E_BAD_INPUT,
            "no working copy here",
            "run the command inside an rdy project",
        )
    })?;
    open_at(env, place, require_bound)
}

/// `open` for the working copy at `place` (a worktree the command names, not the one it runs in).
pub(crate) fn open_at(env: &Env, place: Place, require_bound: bool) -> Result<Open, ErrorReply> {
    open_with(env, place, require_bound, None)
}

/// `open_at`. A worktree directory that is a copy of the real one (msw16, `Place::copy_of`) is
/// refused with `E_BAD_INPUT` and the given fix (default: work in the real directory); a moved
/// worktree records its new directory and goes on.
pub(crate) fn open_with(
    env: &Env,
    place: Place,
    require_bound: bool,
    copy_fix: Option<&str>,
) -> Result<Open, ErrorReply> {
    let lock = WcLock::take(&place.state, LOCK_WAIT)?;
    if let Some(real) = place.copy_of()? {
        return Err(copy_error(&place, &real, copy_fix));
    }
    let mut state = WcState::load(&place)?;
    commit::recover(env, &place, &mut state)?;
    if require_bound {
        state.require_bound()?;
    }
    Ok(Open { place, lock, state })
}

/// msw16: the refusal of a command run in `place`, a copy of the worktree at `real`
/// (`Place::copy_of`), with the given fix (default: work in the real directory).
fn copy_error(place: &Place, real: &str, copy_fix: Option<&str>) -> ErrorReply {
    let name = place
        .state
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let fix = copy_fix.map_or_else(
        || format!("run the command in {real}; delete a copied directory by hand"),
        String::from,
    );
    ErrorReply::new(
        E_BAD_INPUT,
        format!(
            "{} is not a worktree of this project: its pointer names worktree {name}, which is at {real}",
            place.root.display()
        ),
        fix,
    )
}

/// msw16: the refusal `open` gives in `place` when it is a copied worktree directory, checked
/// under the working copy's lock and released; `None` for the main place, a worktree, or a
/// moved worktree (which records its new directory). A command that writes to the store before
/// it opens the working copy checks this first.
pub(crate) fn copy_refusal(place: &Place) -> Result<Option<ErrorReply>, ErrorReply> {
    // the main place is never a copy, and the lock must not create a state directory
    if place.state == place.project.join(WC_DIR) || !place.state.is_dir() {
        return Ok(None);
    }
    let _lock = WcLock::take(&place.state, LOCK_WAIT)?;
    Ok(place.copy_of()?.map(|real| copy_error(place, &real, None)))
}

/// The project's pack and the working tree under the place's root, read with its `.rdyignore`.
pub(crate) fn tree(env: &Env, place: &Place) -> Result<(Pack, WcTree), ErrorReply> {
    let pack = project_pack(&env.project)?;
    let ignore = Ignore::load(&place.root)?;
    let tree = read_tree(&place.root, &ignore, &pack);
    Ok((pack, tree))
}
