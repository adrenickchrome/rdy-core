//! The signature check (D-1Z951, D-17AA8; O11 as amended by ruling task-13), as SIG's engine
//! gives it. `check_signature` is pure: it reads only its arguments, and trust is the caller's
//! parsed allowed-signers input (`AllowedSigners::parse`), or none. This module reads no
//! environment, no agent and no allowed-signers file. [`Snapshot::check_context`] reads what a
//! signature is checked against from a captured snapshot; in a holder, a relay-tag subject finds
//! its answer through the holder's answer ref of that tag object. `KEY_TYPES` lists the key types
//! a signature may use.
use crate::store::Snapshot;
use rdy_engine::RdyError;
use rdy_engine::signature_check::read_check_context;

pub use rdy_engine::allowed_signers::AllowedSigners;
pub use rdy_engine::signature_check::{
    CheckContext, SignatureCheck, SignatureSubject, SigningWarning, check_signature,
};
pub use rdy_engine::signing_crypto::KEY_TYPES;
pub use rdy_model::signature::{Signature, StatementKind};
pub use rdy_model::{ActorRun, RepoId};

impl Snapshot {
    /// The context `signature` is checked against in this snapshot: its subject (a landing, a
    /// tag with its relay answer, or missing), the repo id and the repo ids it may name. A
    /// snapshot from before a removal is refused (`E_REDACTED_STATE`), as every content read is.
    pub fn check_context(&self, signature: &Signature) -> Result<CheckContext, RdyError> {
        self.read_state().ensure_current_epoch()?;
        read_check_context(self.read_state(), signature)
    }
}
