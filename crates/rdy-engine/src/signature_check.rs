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
//!   (`E_SIG_OBJECT_MISMATCH` for a valid signature). In a holder only (ruling task-13), a valid
//!   signature whose statement, rebuilt for the repo id it names, equals the signed bytes, but
//!   names a repo id other than the holder's own, is `foreign_repo`, with that repo id: signed in
//!   another project and not verified here. A holder holds no fork history and trusts none (H2
//!   brings hub-recorded lineage); it is neither a mismatch nor an error code.
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
//! readers. A holder finds a tag's relay answer by the tag object
//! (`answer/tag-object:<hash>`), a project by the tag name (`answer/tag:<name>`).

use crate::allowed_signers::AllowedSigners;
use crate::read_state::ReadState;
use crate::signing_crypto::verify_sshsig;
use crate::{E_SIG_INVALID, E_SIG_OBJECT_MISMATCH, RdyError, sig_unknown_key, signing_limit};
use rdy_model::signature::{
    MAX_NAMESPACE, MAX_SIG, MAX_SIGNATURE, MAX_SIGNER_TEXT, MAX_STATEMENT, ParsedStatement,
    Signature, StatementKind, StatementSource, parse_statement, statement,
};
use rdy_model::{ActorRun, Answer, Hash, LandingRecord, RepoId, Tag};
use rdy_store::holder::{StoreMode, answer_tag_object_ref};
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
/// `statement`: in a holder only, the signature is valid over its subject but its statement
/// names another project's repo id, which the holder cannot verify (no fork history).
pub const STATEMENT_FOREIGN_REPO: &str = "foreign_repo";
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
    /// names (`answer/tag:<name>` in a project, `answer/tag-object:<hash>` in a holder), if
    /// any.
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
    /// the fork sources, ascending, and empty for a project never forked or a holder.
    pub source_repos: Vec<RepoId>,
    /// The subject is read in a holder: a valid signature that names a repo id other than
    /// `current_repo` and `source_repos` is [`STATEMENT_FOREIGN_REPO`], not a mismatch.
    pub holder: bool,
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
    /// [`STATEMENT_MATCH`], [`STATEMENT_MISMATCH`] or, in a holder only,
    /// [`STATEMENT_FOREIGN_REPO`].
    pub statement: String,
    /// [`TRUST_TRUSTED`] or [`TRUST_UNKNOWN`].
    pub trust: String,
    /// [`PRESENCE_PRESENT`] or [`PRESENCE_ABSENT`].
    pub presence: String,
    /// Who asked for the signature, as the record says.
    pub signed_by: ActorRun,
    /// When, as the record says: never a trusted time.
    pub at_ms: i64,
    /// `E_SIG_INVALID`, `E_SIG_OBJECT_MISMATCH`, or `null` for a valid, covered signature or
    /// a valid one of another project in a holder.
    pub code: Option<String>,
    pub warnings: Vec<SigningWarning>,
    /// In a holder only: the repo id a valid signature's statement names when it is not the
    /// holder's own ([`STATEMENT_FOREIGN_REPO`]). Absent from the JSON otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub foreign_repo: Option<String>,
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
            foreign_repo: None,
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
        foreign_repo: None,
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
    let principal = match covered(record, &parsed, key, context) {
        Some(Coverage::Covered(principal)) => principal,
        Some(Coverage::Foreign(repo)) => {
            // signed in another project: not verified here, so no trust and no warning
            out.statement = STATEMENT_FOREIGN_REPO.into();
            out.code = None;
            out.foreign_repo = Some(repo.to_string());
            return out;
        }
        None => {
            out.code = Some(E_SIG_OBJECT_MISMATCH.into());
            return out;
        }
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

/// What a valid signature's statement covers.
enum Coverage<'c> {
    /// The subject, in the current repo or a fork source: the principal trust is checked for.
    Covered(&'c str),
    /// In a holder, the subject as made in this other repo, which the holder cannot verify.
    Foreign(RepoId),
}

/// What the statement `parsed` of `record` (signed by `key`) covers in `context`: the principal
/// trust is checked for, when the record names the subject, the statement's form fits it, and
/// the statement rebuilt for the current repo id, or else for one of the fork sources, equals
/// the signed bytes. In a holder, a statement that equals the one rebuilt for the other repo id
/// it names is `Foreign`. `None` otherwise.
fn covered<'c>(
    record: &Signature,
    parsed: &ParsedStatement<'_>,
    key: &str,
    context: &'c CheckContext,
) -> Option<Coverage<'c>> {
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
            Ok(bytes) if bytes == record.statement => return Some(Coverage::Covered(principal)),
            Ok(_) => {}
            // the record cannot be the source of this statement for any repo id
            Err(_) => return None,
        }
    }
    if !context.holder {
        return None;
    }
    // a holder: the repo id the statement names, when it is not one already tried
    let named: RepoId = parsed.field("repo")?.parse().ok()?;
    if named == context.current_repo || context.source_repos.contains(&named) {
        return None;
    }
    match statement(source, Some(named), key) {
        Ok(bytes) if bytes == record.statement => Some(Coverage::Foreign(named)),
        _ => None,
    }
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

/// The signatures of `subject` that `state` holds, in signature hash order: those whose frames
/// lie before the state's log end. A signature of a batch that is still open, or one sealed
/// after `state` was published, is not in it: the index already holds such a batch's entries,
/// and the batch may yet be abandoned. More than [`MAX_PER_SUBJECT`] is the shared signing
/// `E_LIMIT`.
pub(crate) fn state_signatures(state: &ReadState, subject: &Hash) -> Result<Vec<Hash>, RdyError> {
    state
        .reader()
        .signatures_before(subject, state.seq())
        .map_err(|e| match e {
            // signatures_before refuses only a subject over the response bound
            StoreError::Refused(_) => {
                signing_limit(&format!("{MAX_PER_SUBJECT} signatures per subject"))
            }
            e => e.into(),
        })
}

/// The checks of every signature of `subject` that `state` holds ([`state_signatures`]), in
/// signature hash order, against one context. More than [`MAX_PER_SUBJECT`] signatures is the
/// shared signing `E_LIMIT`.
pub fn signature_checks(
    state: &ReadState,
    subject: &Hash,
    trust: Option<&AllowedSigners>,
) -> Result<Vec<SignatureCheck>, RdyError> {
    Lineage::new(state).checks(subject, trust)
}

/// The repo ids a signature in one read state may name ([`repo_ids`]), read at most once, at
/// the first check that needs them, and shared by every check of one report on that state. A
/// report of many signed subjects so makes one pass over the stored ref updates, not one per
/// subject, and every check sees the same state.
pub(crate) struct Lineage<'s> {
    state: &'s ReadState,
    ids: std::cell::OnceCell<(RepoId, Vec<RepoId>)>,
}

impl<'s> Lineage<'s> {
    pub(crate) fn new(state: &'s ReadState) -> Lineage<'s> {
        Lineage {
            state,
            ids: std::cell::OnceCell::new(),
        }
    }

    /// The state the lineage is read from.
    pub(crate) fn state(&self) -> &'s ReadState {
        self.state
    }

    /// The current repo id and the fork sources, read on the first call.
    fn ids(&self) -> Result<&(RepoId, Vec<RepoId>), RdyError> {
        if let Some(ids) = self.ids.get() {
            return Ok(ids);
        }
        let ids = repo_ids(self.state)?;
        Ok(self.ids.get_or_init(|| ids))
    }

    /// [`signature_checks`] of `subject` on this lineage's state.
    pub(crate) fn checks(
        &self,
        subject: &Hash,
        trust: Option<&AllowedSigners>,
    ) -> Result<Vec<SignatureCheck>, RdyError> {
        let reader = self.state.reader();
        let ids = state_signatures(self.state, subject)?;
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let (current, sources) = self.ids()?;
        let context = subject_context(self.state, subject, *current, sources.clone())?;
        ids.iter()
            .map(|id| Ok(check_signature(&reader.signature(id)?, &context, trust)))
            .collect()
    }
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
    // a holder trusts no lineage: the upload carries none (ruling task-13)
    if state.origin()?.is_none() || is_holder(state)? {
        return Ok((current, vec![]));
    }
    #[cfg(test)]
    LINEAGE_SCANS.with(|n| n.set(n.get() + 1));
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

#[cfg(test)]
thread_local! {
    /// How many passes over the stored ref updates [`repo_ids`] made on this thread: the
    /// structural count of the lineage reads a report makes.
    pub(crate) static LINEAGE_SCANS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Whether `state` is a holder's.
fn is_holder(state: &ReadState) -> Result<bool, RdyError> {
    Ok(state.reader().mode()? == StoreMode::Holder)
}

/// The context of a signature of `subject` in `state`, with the repo ids [`repo_ids`] read. A
/// tag's relay answer is the one its answer ref names: in a project `answer/tag:<name>`, in a
/// holder `answer/tag-object:<hash>` (ruling task-13), as a holder may keep two tag objects of
/// one name. The context is the log's as `state` captured it: a subject or answer whose frame
/// lies at or after the state's log end (a batch still open, or sealed after the state was
/// published) is not held, so a held state never follows a later write.
pub(crate) fn subject_context(
    state: &ReadState,
    subject: &Hash,
    current_repo: RepoId,
    source_repos: Vec<RepoId>,
) -> Result<CheckContext, RdyError> {
    let reader = state.reader();
    let holder = is_holder(state)?;
    let end = state.seq();
    let subject = if reader.has_kind_before(kinds::LANDING, subject, end)? {
        SignatureSubject::Landing {
            hash: *subject,
            record: reader.landing(subject)?,
        }
    } else if reader.has_kind_before(kinds::TAG, subject, end)? {
        let record: Tag = reader.get_typed(kinds::TAG, subject)?;
        let answer = if holder {
            relay_answer_by_object(state, subject)?
        } else {
            reader.answer_of(&format!("tag:{}", record.name))?
        };
        // the refs are live: an answer written after the state is not its answer
        let answer = match answer {
            Some((h, _)) if !reader.has_kind_before(kinds::ANSWER, &h, end)? => None,
            answer => answer,
        };
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
        source_repos: if holder { vec![] } else { source_repos },
        holder,
    })
}

/// A holder's relay answer of the TAG object `tag`: the answer `answer/tag-object:<tag>` names.
fn relay_answer_by_object(
    state: &ReadState,
    tag: &Hash,
) -> Result<Option<(Hash, Answer)>, RdyError> {
    let reader = state.reader();
    let name = answer_tag_object_ref(tag);
    let Some(value) = reader.get_ref(&name)? else {
        return Ok(None);
    };
    let bytes = <[u8; 32]>::try_from(value.as_slice()).map_err(|_| {
        RdyError::from(StoreError::Decode(format!(
            "answer ref {name:?} is not a hash"
        )))
    })?;
    let h = Hash(bytes);
    Ok(Some((h, reader.get_typed(kinds::ANSWER, &h)?)))
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
