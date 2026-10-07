//! The working-copy commands (M2f-a): files on disk that a person edits and commits into a view.
//! Every command takes the working copy's lock first and holds it to the end.

use crate::Env;
use clap::Subcommand;
use rdy_daemon::ErrorReply;
use rdy_engine::wc::ignore::Ignore;
use rdy_engine::wc::layout::{WcTree, read_tree};
use rdy_engine::wc::state::{LOCK_WAIT, Place, WcLock, WcState};
use rdy_engine::{E_BAD_INPUT, project_pack};
use rdy_schema::Pack;
use serde_json::Value;

mod add;
mod brief;
mod checkout;
mod commit;
mod new_files;
mod status;
mod update;
mod worktree;

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
}

pub fn run(cmd: WcCmd, env: &Env) -> Result<(Value, String), ErrorReply> {
    match cmd {
        WcCmd::Checkout { view, force } => checkout::run(env, view, force),
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
/// state; `E_WC_NOT_BOUND` when `require_bound` and nothing is checked out.
pub(crate) fn open(env: &Env, require_bound: bool) -> Result<Open, ErrorReply> {
    let place = env.place.clone().ok_or_else(|| {
        ErrorReply::new(
            E_BAD_INPUT,
            "no working copy here",
            "run the command inside an rdy project",
        )
    })?;
    let lock = WcLock::take(&place.state, LOCK_WAIT)?;
    let state = WcState::load(&place)?;
    if require_bound {
        state.require_bound()?;
    }
    Ok(Open { place, lock, state })
}

/// The project's pack and the working tree under the place's root, read with its `.rdyignore`.
pub(crate) fn tree(env: &Env, place: &Place) -> Result<(Pack, WcTree), ErrorReply> {
    let pack = project_pack(&env.project)?;
    let ignore = Ignore::load(&place.root)?;
    let tree = read_tree(&place.root, &ignore, &pack);
    Ok((pack, tree))
}
