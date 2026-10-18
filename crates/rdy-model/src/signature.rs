//! Signatures (kind `SIGNATURE`) and the canonical statements they sign (D-17AA8, D-1Z951).
//!
//! A statement is read off the record it covers: a landing's approval or relay, or a tag and
//! the answer that relayed it. No new approval object exists, and no existing record gains a
//! field (D-F0904). A statement is UTF-8 text in one closed grammar:
//!
//! ```text
//! rdy-statement: 1
//! kind: <approval | relay | tag | keybind>
//! <name>: <value>          one line per field, in the fixed order of its kind
//! ```
//!
//! Every line ends in LF, including the last. The parser borrows its input and never allocates;
//! the decoder checks each length against its limit and the remaining bytes before it
//! allocates.
use crate::{Answer, AnswerAct, Assurance, Hash, LandingRecord, Provenance, RepoId, Role, Tag};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// The longest statement, in bytes.
pub const MAX_STATEMENT: usize = 16384;
/// The longest value of one statement line, in bytes.
pub const MAX_VALUE: usize = 4096;
/// The longest namespace, in bytes.
pub const MAX_NAMESPACE: usize = 64;
/// The longest SSHSIG blob, in bytes.
pub const MAX_SIG: usize = 16384;
/// The longest encoded SIGNATURE, in bytes.
pub const MAX_SIGNATURE: usize = 65536;
/// The longest `signed_by` actor or run, in bytes.
pub const MAX_SIGNER_TEXT: usize = 4096;

/// The first line of every version-1 statement.
const HEADER: &str = "rdy-statement: 1\n";

/// One signature (kind `SIGNATURE`). The field order is the wire format (postcard): never add,
/// remove or reorder a field (D-F0904). Its id is [`crate::hash_of`] over the record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signature {
    /// The record the statement covers: a landing or a tag.
    pub subject: Hash,
    /// The SSHSIG namespace, the statement kind's [`StatementKind::namespace`].
    pub namespace: String,
    /// The exact statement bytes that were signed.
    pub statement: Vec<u8>,
    /// The SSHSIG blob.
    pub sig: Vec<u8>,
    /// Who asked for the signature.
    pub signed_by: Provenance,
}

impl Signature {
    /// `hash_of` over the signature's encoding.
    pub fn id(&self) -> Hash {
        crate::hash_of(self)
    }
}

/// The kind of a statement (a closed list).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StatementKind {
    Approval,
    Relay,
    Tag,
    Keybind,
}

impl StatementKind {
    /// The kind as the `kind:` line spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            StatementKind::Approval => "approval",
            StatementKind::Relay => "relay",
            StatementKind::Tag => "tag",
            StatementKind::Keybind => "keybind",
        }
    }

    /// The SSHSIG namespace a statement of this kind is signed in.
    pub fn namespace(self) -> &'static str {
        match self {
            StatementKind::Approval => "rdy-approval-v1",
            StatementKind::Relay => "rdy-relay-v1",
            StatementKind::Tag => "rdy-tag-v1",
            StatementKind::Keybind => "rdy-keybind-v1",
        }
    }

    fn parse(s: &str) -> Option<StatementKind> {
        Some(match s {
            "approval" => StatementKind::Approval,
            "relay" => StatementKind::Relay,
            "tag" => StatementKind::Tag,
            "keybind" => StatementKind::Keybind,
            _ => return None,
        })
    }
}

/// Why a statement or a signature could not be built, parsed or decoded. The engine maps it to
/// the shared public errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StatementError {
    /// The text or bytes break the grammar or the encoding.
    #[error("malformed signing statement or signature")]
    Syntax,
    /// The record cannot be the source of this statement.
    #[error("the record cannot be signed in this form")]
    Source,
    /// An input exceeds the limit it carries, in bytes.
    #[error("signing input exceeds {0} bytes")]
    Limit(usize),
}

/// The record a statement is read off.
#[derive(Clone, Copy, Debug)]
pub enum StatementSource<'a> {
    /// A landing's approval (Local or Relayed assurance).
    Approval(&'a LandingRecord),
    /// The relay of a landing's approval.
    RelayApproval(&'a LandingRecord),
    /// A tag, with its id.
    Tag { hash: Hash, tag: &'a Tag },
    /// The relay of a tag: the tag and the stored answer that relayed it, with their ids.
    RelayTag {
        hash: Hash,
        tag: &'a Tag,
        answer_hash: Hash,
        answer: &'a Answer,
    },
    /// A key binding challenge; never a stored signature's subject.
    Keybind {
        account: &'a str,
        challenge: &'a str,
    },
}

/// The shape a statement value must have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ty {
    /// Nonempty UTF-8 with no control character and no leading or trailing ASCII space.
    Text,
    /// 64 lowercase hex digits.
    Hash,
    /// A hash, or `none` for an absent base.
    Base,
    /// 32 lowercase hex digits.
    Repo,
    /// `SHA256:` and 43 canonical unpadded base64 characters.
    Key,
    /// `local` or `relayed`.
    Assurance,
    /// `approve` or `tag`.
    Act,
}

type Layout = &'static [(&'static str, Ty)];

const APPROVAL: Layout = &[
    ("repo", Ty::Repo),
    ("proposal_version", Ty::Hash),
    ("base", Ty::Base),
    ("actor", Ty::Text),
    ("assurance", Ty::Assurance),
    ("key", Ty::Key),
];
const RELAY_APPROVE: Layout = &[
    ("repo", Ty::Repo),
    ("act", Ty::Act),
    ("proposal_version", Ty::Hash),
    ("base", Ty::Base),
    ("actor", Ty::Text),
    ("relayed_by", Ty::Text),
    ("evidence", Ty::Hash),
    ("key", Ty::Key),
];
const TAG: Layout = &[
    ("repo", Ty::Repo),
    ("tag", Ty::Hash),
    ("name", Ty::Text),
    ("landing", Ty::Hash),
    ("actor", Ty::Text),
    ("key", Ty::Key),
];
const RELAY_TAG: Layout = &[
    ("repo", Ty::Repo),
    ("act", Ty::Act),
    ("tag", Ty::Hash),
    ("name", Ty::Text),
    ("landing", Ty::Hash),
    ("actor", Ty::Text),
    ("relayed_by", Ty::Text),
    ("answer", Ty::Hash),
    ("key", Ty::Key),
];
const KEYBIND: Layout = &[
    ("account", Ty::Text),
    ("challenge", Ty::Text),
    ("key", Ty::Key),
];

/// The most fields after `kind` in any layout.
const MAX_FIELDS: usize = 9;

/// A parsed statement. It borrows the text it was parsed from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParsedStatement<'a> {
    text: &'a str,
    kind: StatementKind,
    fields: [(&'a str, &'a str); MAX_FIELDS],
    len: usize,
}

impl<'a> ParsedStatement<'a> {
    pub fn kind(&self) -> StatementKind {
        self.kind
    }

    /// The namespace of the statement's kind.
    pub fn namespace(&self) -> &'static str {
        self.kind.namespace()
    }

    /// The relayed act (`approve` or `tag`); `None` unless the kind is relay.
    pub fn act(&self) -> Option<&'a str> {
        match self.kind {
            StatementKind::Relay => self.field("act"),
            _ => None,
        }
    }

    /// The value of the field `name`, one of the fields after `kind`.
    pub fn field(&self, name: &str) -> Option<&'a str> {
        self.fields().find(|(n, _)| *n == name).map(|(_, v)| v)
    }

    /// The fields after `kind`, in order.
    pub fn fields(&self) -> impl Iterator<Item = (&'a str, &'a str)> + '_ {
        self.fields[..self.len].iter().copied()
    }

    /// The whole statement text.
    pub fn text(&self) -> &'a str {
        self.text
    }
}

/// Parses a version-1 statement. Only the canonical text is valid: no CR, blank line, comment,
/// extra, repeated or reordered field, and a final LF.
pub fn parse_statement(bytes: &[u8]) -> Result<ParsedStatement<'_>, StatementError> {
    if bytes.len() > MAX_STATEMENT {
        return Err(StatementError::Limit(MAX_STATEMENT));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| StatementError::Syntax)?;
    let body = text
        .strip_prefix(HEADER)
        .and_then(|rest| rest.strip_suffix('\n'))
        .ok_or(StatementError::Syntax)?;
    let mut lines = body.split('\n').map(|line| {
        line.split_once(": ")
            .ok_or(StatementError::Syntax)
            .and_then(|(name, value)| {
                if value.len() > MAX_VALUE {
                    return Err(StatementError::Limit(MAX_VALUE));
                }
                Ok((name, value))
            })
    });
    let kind = match lines.next().transpose()? {
        Some(("kind", value)) => StatementKind::parse(value).ok_or(StatementError::Syntax)?,
        _ => return Err(StatementError::Syntax),
    };
    let mut fields = [("", ""); MAX_FIELDS];
    let mut len = 0;
    for line in lines {
        let slot = fields.get_mut(len).ok_or(StatementError::Syntax)?;
        *slot = line?;
        len += 1;
    }
    let layout = match kind {
        StatementKind::Approval => APPROVAL,
        StatementKind::Tag => TAG,
        StatementKind::Keybind => KEYBIND,
        StatementKind::Relay => match fields.get(1) {
            Some(&("act", "approve")) => RELAY_APPROVE,
            Some(&("act", "tag")) => RELAY_TAG,
            _ => return Err(StatementError::Syntax),
        },
    };
    if len != layout.len() {
        return Err(StatementError::Syntax);
    }
    for ((name, value), (want, ty)) in fields[..len].iter().zip(layout) {
        if name != want {
            return Err(StatementError::Syntax);
        }
        check_value(*ty, value)?;
    }
    Ok(ParsedStatement {
        text,
        kind,
        fields,
        len,
    })
}

/// The statement for `source`, in `repo`, signed by the key with fingerprint `key`. A key
/// binding takes no repo; every other source requires one.
pub fn statement(
    source: StatementSource<'_>,
    repo: Option<RepoId>,
    key: &str,
) -> Result<Vec<u8>, StatementError> {
    let repo = match (source, repo) {
        (StatementSource::Keybind { .. }, None) => String::new(),
        (StatementSource::Keybind { .. }, Some(_)) | (_, None) => {
            return Err(StatementError::Source);
        }
        (_, Some(repo)) => repo.to_string(),
    };
    match source {
        StatementSource::Approval(landing) => {
            let assurance = match landing.assurance {
                Assurance::Local | Assurance::Relayed => landing.assurance.as_str(),
                Assurance::Policy => return Err(StatementError::Source),
            };
            let (version, base) = approval_fields(landing);
            let actor = &landing.approved_by.actor;
            emit(
                StatementKind::Approval,
                APPROVAL,
                &[&repo, &version, &base, actor, assurance, key],
            )
        }
        StatementSource::RelayApproval(landing) => {
            if landing.assurance != Assurance::Relayed {
                return Err(StatementError::Source);
            }
            let relayer = landing.relayed_by.as_ref().ok_or(StatementError::Source)?;
            let relayed_by = join_relayer(&relayer.actor, relayer.run.as_deref())?;
            let evidence = relay_evidence_hash(landing)?.to_string();
            let (version, base) = approval_fields(landing);
            let actor = &landing.approved_by.actor;
            emit(
                StatementKind::Relay,
                RELAY_APPROVE,
                &[
                    &repo,
                    "approve",
                    &version,
                    &base,
                    actor,
                    &relayed_by,
                    &evidence,
                    key,
                ],
            )
        }
        StatementSource::Tag { hash, tag } => emit(
            StatementKind::Tag,
            TAG,
            &[
                &repo,
                &hash.to_string(),
                &tag.name,
                &tag.landing.to_string(),
                &tag.tagged_by.actor,
                key,
            ],
        ),
        StatementSource::RelayTag {
            hash,
            tag,
            answer_hash,
            answer,
        } => {
            let item_matches = answer.item.strip_prefix("tag:") == Some(tag.name.as_str());
            if answer.act != AnswerAct::Tag
                || !item_matches
                || answer.person.actor != tag.tagged_by.actor
            {
                return Err(StatementError::Source);
            }
            let relayer = answer.relayed_by.as_ref().ok_or(StatementError::Source)?;
            let relayed_by = join_relayer(&relayer.actor, relayer.run.as_deref())?;
            emit(
                StatementKind::Relay,
                RELAY_TAG,
                &[
                    &repo,
                    "tag",
                    &hash.to_string(),
                    &tag.name,
                    &tag.landing.to_string(),
                    &tag.tagged_by.actor,
                    &relayed_by,
                    &answer_hash.to_string(),
                    key,
                ],
            )
        }
        StatementSource::Keybind { account, challenge } => {
            emit(StatementKind::Keybind, KEYBIND, &[account, challenge, key])
        }
    }
}

/// The relay evidence hash (statement v1): BLAKE3 of the postcard encodings of the landing's
/// `relay_evidence` and `evidence`, concatenated in that order, each with its Option
/// discriminant. A landing with no relay evidence has no relay to hash.
pub fn relay_evidence_hash(landing: &LandingRecord) -> Result<Hash, StatementError> {
    if landing.relay_evidence.is_none() {
        return Err(StatementError::Source);
    }
    let mut hasher = blake3::Hasher::new();
    let mut sink = HashSink(&mut hasher);
    postcard::to_extend(&landing.relay_evidence, &mut sink).map_err(|_| StatementError::Source)?;
    postcard::to_extend(&landing.evidence, &mut sink).map_err(|_| StatementError::Source)?;
    Ok(Hash(*hasher.finalize().as_bytes()))
}

/// Feeds postcard's output straight into a hasher, so hashing allocates nothing.
struct HashSink<'h>(&'h mut blake3::Hasher);

impl Extend<u8> for &mut HashSink<'_> {
    fn extend<I: IntoIterator<Item = u8>>(&mut self, iter: I) {
        let mut buf = [0u8; 256];
        let mut n = 0;
        for b in iter {
            buf[n] = b;
            n += 1;
            if n == buf.len() {
                self.0.update(&buf);
                n = 0;
            }
        }
        self.0.update(&buf[..n]);
    }
}

/// Decodes a stored SIGNATURE: the exact five-field postcard encoding, canonical varints and no
/// trailing bytes. Each length is checked against its limit and the remaining input before any
/// allocation.
pub fn decode_signature(bytes: &[u8]) -> Result<Signature, StatementError> {
    if bytes.len() > MAX_SIGNATURE {
        return Err(StatementError::Limit(MAX_SIGNATURE));
    }
    let mut c = Cursor { buf: bytes, pos: 0 };
    let mut subject = [0u8; 32];
    subject.copy_from_slice(c.take(32)?);
    let namespace = c.string(MAX_NAMESPACE)?;
    let statement = c.bytes(MAX_STATEMENT)?;
    let sig = c.bytes(MAX_SIG)?;
    let actor = c.string(MAX_SIGNER_TEXT)?;
    let run = match c.byte()? {
        0 => None,
        1 => Some(c.string(MAX_SIGNER_TEXT)?),
        _ => return Err(StatementError::Syntax),
    };
    let role = match c.byte()? {
        0 => Role::Reader,
        1 => Role::Writer,
        2 => Role::Reviewer,
        3 => Role::Approver,
        _ => return Err(StatementError::Syntax),
    };
    let zigzag = c.varint()?;
    let at_ms = ((zigzag >> 1) as i64) ^ -((zigzag & 1) as i64);
    if c.pos != bytes.len() {
        return Err(StatementError::Syntax);
    }
    Ok(Signature {
        subject: Hash(subject),
        namespace,
        statement,
        sig,
        signed_by: Provenance {
            actor,
            run,
            role,
            at_ms,
        },
    })
}

/// A checked reader over postcard bytes.
struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], StatementError> {
        let end = self.pos.checked_add(n).ok_or(StatementError::Syntax)?;
        let out = self.buf.get(self.pos..end).ok_or(StatementError::Syntax)?;
        self.pos = end;
        Ok(out)
    }

    fn byte(&mut self) -> Result<u8, StatementError> {
        Ok(self.take(1)?[0])
    }

    /// A canonical (shortest) LEB128 varint of at most 10 bytes that fits a u64.
    fn varint(&mut self) -> Result<u64, StatementError> {
        let mut value = 0u64;
        for i in 0..10 {
            let b = self.byte()?;
            if i == 9 && b > 1 {
                return Err(StatementError::Syntax);
            }
            value |= u64::from(b & 0x7f) << (7 * i);
            if b & 0x80 == 0 {
                if i > 0 && b == 0 {
                    return Err(StatementError::Syntax);
                }
                return Ok(value);
            }
        }
        Err(StatementError::Syntax)
    }

    /// A length prefix of at most `limit`, then that many bytes, copied with fallible growth.
    fn bytes(&mut self, limit: usize) -> Result<Vec<u8>, StatementError> {
        let n = self.varint()?;
        let n = usize::try_from(n)
            .ok()
            .filter(|n| *n <= limit)
            .ok_or(StatementError::Limit(limit))?;
        let slice = self.take(n)?;
        let mut out = Vec::new();
        out.try_reserve_exact(n)
            .map_err(|_| StatementError::Limit(limit))?;
        out.extend_from_slice(slice);
        Ok(out)
    }

    fn string(&mut self, limit: usize) -> Result<String, StatementError> {
        String::from_utf8(self.bytes(limit)?).map_err(|_| StatementError::Syntax)
    }
}

/// The approved version (falling back to the proposal) and the base (`none` for a first
/// landing).
fn approval_fields(landing: &LandingRecord) -> (String, String) {
    let version = landing.approved_version.unwrap_or(landing.proposal);
    let base = landing
        .prev
        .map_or_else(|| "none".to_string(), |h| h.to_string());
    (version.to_string(), base)
}

/// The relayer's actor, then `/` and the run when one is present. An actor that already holds
/// `/` is kept as it is. The length is checked before anything is copied: an actor alone is
/// borrowed, and a joined value is built with fallible growth bounded by the value limit.
fn join_relayer<'a>(actor: &'a str, run: Option<&str>) -> Result<Cow<'a, str>, StatementError> {
    let len = match run {
        None => actor.len(),
        Some(run) => actor
            .len()
            .checked_add(1)
            .and_then(|n| n.checked_add(run.len()))
            .ok_or(StatementError::Limit(MAX_VALUE))?,
    };
    if len > MAX_VALUE {
        return Err(StatementError::Limit(MAX_VALUE));
    }
    let Some(run) = run else {
        return Ok(Cow::Borrowed(actor));
    };
    let mut out = String::new();
    out.try_reserve_exact(len)
        .map_err(|_| StatementError::Limit(MAX_VALUE))?;
    out.push_str(actor);
    out.push('/');
    out.push_str(run);
    Ok(Cow::Owned(out))
}

/// Writes the header, the kind and each `layout` field with its value, checking every value
/// and the total length.
fn emit(kind: StatementKind, layout: Layout, values: &[&str]) -> Result<Vec<u8>, StatementError> {
    debug_assert_eq!(layout.len(), values.len());
    for ((_, ty), value) in layout.iter().zip(values) {
        check_value(*ty, value)?;
    }
    let mut out = Vec::with_capacity(256);
    let mut push = |parts: &[&str]| {
        let add = parts.iter().map(|p| p.len()).sum::<usize>();
        if out.len() + add > MAX_STATEMENT {
            return Err(StatementError::Limit(MAX_STATEMENT));
        }
        for p in parts {
            out.extend_from_slice(p.as_bytes());
        }
        Ok(())
    };
    push(&[HEADER, "kind: ", kind.as_str(), "\n"])?;
    for ((name, _), value) in layout.iter().zip(values) {
        push(&[name, ": ", value, "\n"])?;
    }
    Ok(out)
}

fn check_value(ty: Ty, value: &str) -> Result<(), StatementError> {
    if value.len() > MAX_VALUE {
        return Err(StatementError::Limit(MAX_VALUE));
    }
    let ok = match ty {
        Ty::Text => is_text(value),
        Ty::Hash => is_lower_hex(value, 64),
        Ty::Base => value == "none" || is_lower_hex(value, 64),
        Ty::Repo => is_lower_hex(value, 32),
        Ty::Key => is_fingerprint(value),
        Ty::Assurance => matches!(value, "local" | "relayed"),
        Ty::Act => matches!(value, "approve" | "tag"),
    };
    if ok {
        Ok(())
    } else {
        Err(StatementError::Syntax)
    }
}

fn is_text(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with(' ')
        && !value.ends_with(' ')
        && !value.chars().any(char::is_control)
}

fn is_lower_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `SHA256:` and the canonical unpadded base64 of 32 bytes: 43 characters of the standard
/// alphabet whose last one carries no stray low bits.
fn is_fingerprint(value: &str) -> bool {
    let Some(b64) = value.strip_prefix("SHA256:") else {
        return false;
    };
    let digit = |b: u8| match b {
        b'A'..=b'Z' => Some(b - b'A'),
        b'a'..=b'z' => Some(b - b'a' + 26),
        b'0'..=b'9' => Some(b - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    b64.len() == 43
        && b64.bytes().all(|b| digit(b).is_some())
        && digit(b64.as_bytes()[42]).is_some_and(|d| d & 0b11 == 0)
}
