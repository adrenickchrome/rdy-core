//! The offline check of one stored signature against its subject (D-1Z951, D-17AA8).
//!
//! [`check_signature`] is one pure function: no I/O, clock, environment, mutation or network.
//! It keeps four results apart:
//!
//! - **signed**: the SSHSIG verifies (namespace, sha512, accepted key type), the statement is in
//!   the grammar and its kind's namespace is the record's, and the statement names the key that
//!   signed it. Otherwise `invalid` (`E_SIG_INVALID`).
//! - **statement**: the statement rebuilt from the subject, for the current repo id and then for
//!   the fork source (when the caller gives one), equals the signed bytes exactly. Otherwise
//!   `mismatch` (`E_SIG_OBJECT_MISMATCH` for a valid signature).
//! - **trust**: a covered signature's key is trusted for its principal and namespace by the
//!   caller's allowed-signers data. Otherwise `unknown`, with one `W_SIG_UNKNOWN_KEY` warning for
//!   a covered signature only.
//! - **presence**: the authenticated user-presence bit of a valid sk signature.
//!
//! A signature never changes assurance, and presence proves no reading of an approval.
//! `signed_by` and `at_ms` are the record's own claim, never a trusted time.
//!
//! [`read_check_context`] and [`signature_checks`] read the subject, the current repo id and the
//! fork source from one immutable read state through the typed readers.

use crate::allowed_signers::AllowedSigners;
use crate::read_state::ReadState;
use crate::signing_crypto::verify_sshsig;
use crate::{E_SIG_INVALID, E_SIG_OBJECT_MISMATCH, RdyError, sig_unknown_key, signing_limit};
use rdy_model::signature::{
    ParsedStatement, Signature, StatementKind, StatementSource, parse_statement, statement,
};
use rdy_model::{ActorRun, Answer, Hash, LandingRecord, RepoId, Tag};
use rdy_store::signature::MAX_PER_SUBJECT;
use rdy_store::{StoreError, kinds};
use serde::Serialize;

/// `signed`: the signature verifies and names its key.
pub const SIGNED_VALID: &str = "valid";
/// `signed`: it does not.
pub const SIGNED_INVALID: &str = "invalid";
/// `statement`: the rebuilt statement equals the signed bytes.
pub const STATEMENT_MATCH: &str = "match";
/// `statement`: it does not, or the signature is invalid.
pub const STATEMENT_MISMATCH: &str = "mismatch";
/// `trust`: the caller's allowed-signers data trusts the key for the principal and namespace.
pub const TRUST_TRUSTED: &str = "trusted";
/// `trust`: it does not, or the signature is invalid or not covered.
pub const TRUST_UNKNOWN: &str = "unknown";
/// `presence`: a valid sk signature with the user-presence bit set.
pub const PRESENCE_PRESENT: &str = "present";
/// `presence`: any other signature.
pub const PRESENCE_ABSENT: &str = "absent";

/// The record a signature names, as the caller read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignatureSubject {
    /// A landing, for an approval or the relay of one.
    Landing { hash: Hash, record: LandingRecord },
    /// A tag, for a tag statement or its relay; `answer` is the answer the tag's answer ref
    /// names (`answer/tag:<name>`), if any.
    Tag {
        hash: Hash,
        record: Tag,
        answer: Option<(Hash, Answer)>,
    },
    /// No landing or tag has the hash.
    Missing { hash: Hash },
}

impl SignatureSubject {
    /// The subject's hash.
    pub fn hash(&self) -> Hash {
        match self {
            SignatureSubject::Landing { hash, .. }
            | SignatureSubject::Tag { hash, .. }
            | SignatureSubject::Missing { hash } => *hash,
        }
    }
}

/// What a signature is checked against: its subject, the current repo id, and the source repo
/// id of a fork (O7). It claims nothing about when a signature was made or where it sits in the
/// log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckContext {
    pub subject: SignatureSubject,
    pub current_repo: RepoId,
    /// The current `meta/origin` repo id of a fork; `None` for a project that is not a fork.
    pub source_repo: Option<RepoId>,
}

/// An advisory result of a check, such as `W_SIG_UNKNOWN_KEY`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SigningWarning {
    pub code: String,
    pub message: String,
    pub fix: String,
}

impl From<RdyError> for SigningWarning {
    fn from(e: RdyError) -> Self {
        SigningWarning {
            code: e.code.to_string(),
            message: e.message,
            fix: e.fix,
        }
    }
}

/// The check of one signature. `kind`, `key` and `key_type` are `null` when they cannot be read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SignatureCheck {
    /// The statement kind: `approval`, `relay`, `tag` or `keybind`.
    pub kind: Option<String>,
    /// The SHA256 fingerprint of the key embedded in the SSHSIG.
    pub key: Option<String>,
    pub key_type: Option<String>,
    /// [`SIGNED_VALID`] or [`SIGNED_INVALID`].
    pub signed: String,
    /// [`STATEMENT_MATCH`] or [`STATEMENT_MISMATCH`].
    pub statement: String,
    /// [`TRUST_TRUSTED`] or [`TRUST_UNKNOWN`].
    pub trust: String,
    /// [`PRESENCE_PRESENT`] or [`PRESENCE_ABSENT`].
    pub presence: String,
    /// Who asked for the signature, as the record says.
    pub signed_by: ActorRun,
    /// When, as the record says: never a trusted time.
    pub at_ms: i64,
    /// `E_SIG_INVALID`, `E_SIG_OBJECT_MISMATCH`, or `null` for a valid, covered signature.
    pub code: Option<String>,
    pub warnings: Vec<SigningWarning>,
}

/// Checks `record` against `context`, with the caller's trust data (`None`: no allowed-signers
/// file, so no key is trusted). Pure: it reads nothing but its arguments.
pub fn check_signature(
    record: &Signature,
    context: &CheckContext,
    trust: Option<&AllowedSigners>,
) -> SignatureCheck {
    let parsed = parse_statement(&record.statement).ok();
    // one cryptographic verification, whatever repo ids the coverage then tries
    let crypto = verify_sshsig(&record.namespace, &record.statement, &record.sig);
    let mut out = SignatureCheck {
        kind: parsed.map(|p| p.kind().as_str().to_string()),
        key: crypto.key.clone(),
        key_type: crypto.key_type.clone(),
        signed: SIGNED_INVALID.into(),
        statement: STATEMENT_MISMATCH.into(),
        trust: TRUST_UNKNOWN.into(),
        presence: PRESENCE_ABSENT.into(),
        signed_by: ActorRun {
            actor: record.signed_by.actor.clone(),
            run: record.signed_by.run.clone(),
        },
        at_ms: record.signed_by.at_ms,
        code: Some(E_SIG_INVALID.into()),
        warnings: vec![],
    };
    let (Some(parsed), Some(key)) = (parsed, crypto.key.as_deref()) else {
        return out;
    };
    if !crypto.valid || parsed.namespace() != record.namespace || parsed.field("key") != Some(key) {
        return out;
    }
    out.signed = SIGNED_VALID.into();
    if crypto.presence {
        out.presence = PRESENCE_PRESENT.into();
    }
    let Some(principal) = covered(record, &parsed, key, context) else {
        out.code = Some(E_SIG_OBJECT_MISMATCH.into());
        return out;
    };
    out.statement = STATEMENT_MATCH.into();
    out.code = None;
    if trust.is_some_and(|t| t.trusts(principal, &record.namespace, key)) {
        out.trust = TRUST_TRUSTED.into();
    } else {
        out.warnings
            .push(sig_unknown_key(key, principal, &record.namespace).into());
    }
    out
}

/// The principal trust is checked for, when the statement `parsed` of `record` (signed by `key`)
/// covers the subject of `context`: the record names the subject, the statement's form fits it,
/// and the statement rebuilt for the current repo id, or else the fork source's, equals the
/// signed bytes. `None` otherwise.
fn covered<'c>(
    record: &Signature,
    parsed: &ParsedStatement<'_>,
    key: &str,
    context: &'c CheckContext,
) -> Option<&'c str> {
    if record.subject != context.subject.hash() {
        return None;
    }
    let (source, principal) = match (&context.subject, parsed.kind(), parsed.act()) {
        (SignatureSubject::Landing { record: l, .. }, StatementKind::Approval, _) => {
            (StatementSource::Approval(l), l.approved_by.actor.as_str())
        }
        (SignatureSubject::Landing { record: l, .. }, StatementKind::Relay, Some("approve")) => (
            StatementSource::RelayApproval(l),
            l.relayed_by.as_ref()?.actor.as_str(),
        ),
        (
            SignatureSubject::Tag {
                hash, record: t, ..
            },
            StatementKind::Tag,
            _,
        ) => (
            StatementSource::Tag {
                hash: *hash,
                tag: t,
            },
            t.tagged_by.actor.as_str(),
        ),
        (
            SignatureSubject::Tag {
                hash,
                record: t,
                answer: Some((answer_hash, a)),
            },
            StatementKind::Relay,
            Some("tag"),
        ) => {
            // the answer wrote exactly this tag
            if a.targets.as_slice() != [*hash] {
                return None;
            }
            (
                StatementSource::RelayTag {
                    hash: *hash,
                    tag: t,
                    answer_hash: *answer_hash,
                    answer: a,
                },
                a.relayed_by.as_ref()?.actor.as_str(),
            )
        }
        _ => return None,
    };
    let fallback = context
        .source_repo
        .filter(|source| *source != context.current_repo);
    for repo in std::iter::once(context.current_repo).chain(fallback) {
        match statement(source, Some(repo), key) {
            Ok(bytes) if bytes == record.statement => return Some(principal),
            Ok(_) => {}
            // the record cannot be the source of this statement for any repo id
            Err(_) => return None,
        }
    }
    None
}

/// The context of `signature` in `state`: its subject, read through the typed readers, the
/// current repo id, and the source repo id when the current `meta/origin` is a fork's.
pub fn read_check_context(
    state: &ReadState,
    signature: &Signature,
) -> Result<CheckContext, RdyError> {
    context_of(state, &signature.subject)
}

/// The checks of every signature of `subject`, in signature hash order, against one context.
/// More than [`MAX_PER_SUBJECT`] signatures is the shared signing `E_LIMIT`.
pub fn signature_checks(
    state: &ReadState,
    subject: &Hash,
    trust: Option<&AllowedSigners>,
) -> Result<Vec<SignatureCheck>, RdyError> {
    let reader = state.reader();
    let ids = reader.signatures_of(subject).map_err(|e| match e {
        // signatures_of refuses only a subject over the response bound
        StoreError::Refused(_) => {
            signing_limit(&format!("{MAX_PER_SUBJECT} signatures per subject"))
        }
        e => e.into(),
    })?;
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let context = context_of(state, subject)?;
    ids.iter()
        .map(|id| Ok(check_signature(&reader.signature(id)?, &context, trust)))
        .collect()
}

/// The context of a signature of `subject` in `state`.
pub(crate) fn context_of(state: &ReadState, subject: &Hash) -> Result<CheckContext, RdyError> {
    let current_repo = state.repo_id()?;
    let source_repo = state.origin()?.filter(|o| o.fork).map(|o| o.repo_id);
    let reader = state.reader();
    let subject = if reader.has_kind(kinds::LANDING, subject)? {
        SignatureSubject::Landing {
            hash: *subject,
            record: reader.landing(subject)?,
        }
    } else if reader.has_kind(kinds::TAG, subject)? {
        let record: Tag = reader.get_typed(kinds::TAG, subject)?;
        let answer = reader.answer_of(&format!("tag:{}", record.name))?;
        SignatureSubject::Tag {
            hash: *subject,
            record,
            answer,
        }
    } else {
        SignatureSubject::Missing { hash: *subject }
    };
    Ok(CheckContext {
        subject,
        current_repo,
        source_repo,
    })
}
