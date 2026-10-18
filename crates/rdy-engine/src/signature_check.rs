//! The offline check of one stored signature against its subject (D-1Z951, D-17AA8).
//!
//! [`check_signature`] is one pure function: no I/O, clock, environment, mutation or network.
//! It keeps four results apart:
//!
//! - **signed**: the SSHSIG verifies (namespace, sha512, accepted key type), the statement is in
//!   the grammar and its kind's namespace is the record's, and the statement names the key that
//!   signed it. Otherwise `invalid` (`E_SIG_INVALID`).
//! - **statement**: the statement rebuilt from the subject, for the current repo id and then for
//!   each fork source the caller gives, equals the signed bytes exactly. Otherwise `mismatch`
//!   (`E_SIG_OBJECT_MISMATCH` for a valid signature).
//! - **trust**: a covered signature's key is trusted for its principal and namespace by the
//!   caller's allowed-signers data. Otherwise `unknown`, with one `W_SIG_UNKNOWN_KEY` warning for
//!   a covered signature only.
//! - **presence**: the authenticated user-presence bit of a valid sk signature.
//!
//! A signature never changes assurance, and presence proves no reading of an approval.
//! `signed_by` and `at_ms` are the record's own claim, never a trusted time.
//!
//! [`read_check_context`] and [`signature_checks`] read the subject, the current repo id and the
//! fork sources of the log's fork lineage from one immutable read state through the typed
//! readers.

use crate::allowed_signers::AllowedSigners;
use crate::read_state::ReadState;
use crate::signing_crypto::verify_sshsig;
use crate::{E_SIG_INVALID, E_SIG_OBJECT_MISMATCH, RdyError, sig_unknown_key, signing_limit};
use rdy_model::signature::{
    MAX_NAMESPACE, MAX_SIG, MAX_SIGNATURE, MAX_SIGNER_TEXT, MAX_STATEMENT, ParsedStatement,
    Signature, StatementKind, StatementSource, parse_statement, statement,
};
use rdy_model::{ActorRun, Answer, Hash, LandingRecord, RepoId, Tag};
use rdy_store::kinds::RefUpdate;
use rdy_store::meta::{REF_ORIGIN, decode_origin};
use rdy_store::signature::MAX_PER_SUBJECT;
use rdy_store::{StoreError, kinds};
use serde::Serialize;
use std::collections::BTreeSet;

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

/// What a signature is checked against: its subject, the current repo id, and the other repo
/// ids it may name: locally, the source of every fork the log records ([`read_check_context`]).
/// It claims nothing about when a signature was made or where it sits in the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckContext {
    pub subject: SignatureSubject,
    pub current_repo: RepoId,
    /// The other repo ids a signature may name, tried in order after `current_repo`; locally
    /// the fork sources, ascending, and empty for a project never forked.
    pub source_repos: Vec<RepoId>,
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
    if !within_bounds(record) {
        // refused before anything is verified or copied: no field of the record is reported
        return SignatureCheck {
            kind: None,
            key: None,
            key_type: None,
            signed: SIGNED_INVALID.into(),
            statement: STATEMENT_MISMATCH.into(),
            trust: TRUST_UNKNOWN.into(),
            presence: PRESENCE_ABSENT.into(),
            signed_by: ActorRun {
                actor: String::new(),
                run: None,
            },
            at_ms: record.signed_by.at_ms,
            code: Some(E_SIG_INVALID.into()),
            warnings: vec![],
        };
    }
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

/// Whether `record` keeps the shared bounds a stored SIGNATURE keeps: each field within its own
/// bound, and the whole postcard encoding within [`MAX_SIGNATURE`] bytes, counted with checked
/// arithmetic and without encoding it.
fn within_bounds(record: &Signature) -> bool {
    let g = record;
    let run = g.signed_by.run.as_ref();
    let fields = [
        (g.namespace.len(), MAX_NAMESPACE),
        (g.statement.len(), MAX_STATEMENT),
        (g.sig.len(), MAX_SIG),
        (g.signed_by.actor.len(), MAX_SIGNER_TEXT),
        (run.map_or(0, String::len), MAX_SIGNER_TEXT),
    ];
    if fields.iter().any(|(len, limit)| len > limit) {
        return false;
    }
    // a length-prefixed field: its varint length, then its bytes
    let field = |len: usize| varint_len(len as u64).checked_add(len);
    let at_ms = g.signed_by.at_ms;
    let zigzag = ((at_ms << 1) ^ (at_ms >> 63)) as u64;
    let total = [
        Some(32),
        field(g.namespace.len()),
        field(g.statement.len()),
        field(g.sig.len()),
        field(g.signed_by.actor.len()),
        // the run's option tag, then the run
        run.map_or(Some(1), |r| field(r.len()).and_then(|n| n.checked_add(1))),
        // the role's variant
        Some(1),
        Some(varint_len(zigzag)),
    ]
    .into_iter()
    .try_fold(0usize, |sum, n| sum.checked_add(n?));
    total.is_some_and(|n| n <= MAX_SIGNATURE)
}

/// The bytes of `v` as a LEB128 varint.
fn varint_len(v: u64) -> usize {
    (64 - v.max(1).leading_zeros() as usize).div_ceil(7)
}

/// The principal trust is checked for, when the statement `parsed` of `record` (signed by `key`)
/// covers the subject of `context`: the record names the subject, the statement's form fits it,
/// and the statement rebuilt for the current repo id, or else for one of the fork sources,
/// equals the signed bytes. `None` otherwise.
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
        .source_repos
        .iter()
        .copied()
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
/// current repo id, and the source repo id of every fork the log records.
pub fn read_check_context(
    state: &ReadState,
    signature: &Signature,
) -> Result<CheckContext, RdyError> {
    let (current, sources) = repo_ids(state)?;
    subject_context(state, &signature.subject, current, sources)
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
    let (current, sources) = repo_ids(state)?;
    let context = subject_context(state, subject, current, sources)?;
    ids.iter()
        .map(|id| Ok(check_signature(&reader.signature(id)?, &context, trust)))
        .collect()
}

/// The repo ids a signature in `state` may name (ruling task-6, D-17AA8, D-DAGNB): the
/// project's own `meta/repo_id`, and the `repo_id` of every `meta/origin` value with
/// `fork: true` that the log records, in any batch, current or earlier, ascending and without
/// the project's own. A clone keeps its source's log (D-C8C70), so an ordinary clone of a fork
/// keeps the fork's origin batch, and a fork of a fork both of its ancestors'. One pass over the
/// stored ref updates; nothing is written. A project with no `meta/origin` was never cloned:
/// only a clone's origin batch writes that ref, and nothing removes it, so its log holds no
/// origin value and the pass is skipped.
pub(crate) fn repo_ids(state: &ReadState) -> Result<(RepoId, Vec<RepoId>), RdyError> {
    let current = state.repo_id()?;
    if state.origin()?.is_none() {
        return Ok((current, vec![]));
    }
    let reader = state.reader();
    let mut sources = BTreeSet::new();
    reader.for_each_object_of(kinds::REF_UPDATE, |hash| {
        let update: RefUpdate = reader.get_typed(kinds::REF_UPDATE, &hash)?;
        if update.name == REF_ORIGIN {
            // a value that does not decode names no repo id: it widens nothing
            if let Some(origin) = decode_origin(&update.value)
                && origin.fork
                && origin.repo_id != current
            {
                sources.insert(origin.repo_id);
            }
        }
        Ok(())
    })?;
    Ok((current, sources.into_iter().collect()))
}

/// The context of a signature of `subject` in `state`, with the repo ids [`repo_ids`] read.
pub(crate) fn subject_context(
    state: &ReadState,
    subject: &Hash,
    current_repo: RepoId,
    source_repos: Vec<RepoId>,
) -> Result<CheckContext, RdyError> {
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
        source_repos,
    })
}

#[cfg(test)]
mod tests {
    use super::varint_len;

    #[test]
    fn sig6_varint_lengths_follow_leb128() {
        for (v, want) in [
            (0u64, 1usize),
            (127, 1),
            (128, 2),
            (16_383, 2),
            (16_384, 3),
            (65_536, 3),
            (u64::MAX, 10),
        ] {
            assert_eq!(varint_len(v), want, "{v}");
        }
    }
}
