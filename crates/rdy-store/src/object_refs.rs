//! Object references (D-D2NPN `refs_of`): the stored objects one portable record names, read from
//! its bytes through the bounded decoder ([`crate::bounded`]). An edge says "this record needs
//! that object". Identity hashes (pins, card hashes, residue identities, report hashes, pair and
//! content keys, tension and conflict ids) are comparisons, not dependencies, and add no edge.
//!
//! Local effects (REF_UPDATE, BATCH_COMMIT) are not portable objects and are refused as `Kind`,
//! as is every unassigned kind.
use crate::bounded::{self, CHUNK_BYTES, MAX_CHUNK_BYTES, RecordError};
use crate::kinds::{
    self, ALT_SET, ANSWER, BLOB_CHUNK, BLOB_MANIFEST, BLOCK, BlobManifest, CHANGE, INTENT, LANDING,
    PROPOSAL, REDACTION, REDACTION_APPLY, REDACTION_RESIDUE, REVIEW, SIGNATURE, SNAPSHOT, TAG,
    TENSION, TRANSITION, VIEW_OP,
};
use crate::transition::{Cause, Side, Transitions, Why};
use crate::view::{ViewOp, ViewOpKind};
use rdy_model::redaction::{RedactTarget, Redaction, RedactionApply, RedactionResidue, ResidualOp};
use rdy_model::signature::Signature;
use rdy_model::{
    AltChoice, AltSet, AltStatus, Answer, Block, Change, ChangeLinks, FieldValue, Hash, Intent,
    LandingRecord, Op, Proposal, Review, SemanticTension, StateSnapshot, Tag,
};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Every assigned portable object kind, ascending: what [`refs_of`] accepts and what a kind-None
/// edge may resolve to. Kind 12 is unassigned; REF_UPDATE (8) and BATCH_COMMIT (14) are local.
pub const PORTABLE_KINDS: [u8; 19] = [
    kinds::BLOCK,
    kinds::CHANGE,
    kinds::PROPOSAL,
    kinds::LANDING,
    kinds::SNAPSHOT,
    kinds::BLOB_CHUNK,
    kinds::BLOB_MANIFEST,
    kinds::TAG,
    kinds::ANSWER,
    kinds::SIGNATURE,
    kinds::VIEW_OP,
    kinds::INTENT,
    kinds::ALT_SET,
    kinds::REVIEW,
    kinds::TENSION,
    kinds::TRANSITION,
    kinds::REDACTION,
    kinds::REDACTION_RESIDUE,
    kinds::REDACTION_APPLY,
];

/// Whether `kind` is an assigned portable object kind.
pub fn is_portable(kind: u8) -> bool {
    PORTABLE_KINDS.contains(&kind)
}

/// One stored object: its kind and hash, ordered by kind then hash. JSON (and any other
/// human-readable format) writes `{"kind": <decimal>, "hash": "<full lowercase hex>"}`; a binary
/// format writes the hash's own bytes. Display is `<kind>:<hash>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectKey {
    pub kind: u8,
    #[serde(with = "hex")]
    pub hash: Hash,
}

impl fmt::Display for ObjectKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.kind, self.hash)
    }
}

/// One edge of a record: the object it needs. `kind` is `None` when the field may name more
/// than one kind (a landing's checks, an answer's targets, a signature's subject); the edge must
/// resolve to exactly one stored portable object when it is checked. Ordered by kind, `None`
/// first, then hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectRef {
    pub kind: Option<u8>,
    pub hash: Hash,
}

/// The objects the record of kind `kind` with bytes `payload` names, sorted by (kind, hash) with
/// `None` first and without repeats. `Kind` for a kind that is not an assigned portable kind;
/// `Decode` or `Limit` for bytes that are not exactly one bounded record of the kind.
pub fn refs_of(kind: u8, payload: &[u8]) -> Result<Vec<ObjectRef>, RecordError> {
    let mut e = Edges(Vec::new());
    match kind {
        BLOCK => e.block(&bounded::decode::<Block>(payload)?)?,
        CHANGE => e.change(&bounded::decode::<Change>(payload)?)?,
        PROPOSAL => e.proposal(&bounded::decode::<Proposal>(payload)?),
        LANDING => e.landing(&bounded::decode::<LandingRecord>(payload)?),
        SNAPSHOT => {
            let s = bounded::decode::<StateSnapshot>(payload)?;
            e.all(BLOCK, s.blocks.values());
        }
        BLOB_CHUNK => {
            if payload.len() > MAX_CHUNK_BYTES {
                return Err(RecordError::limit(CHUNK_BYTES, MAX_CHUNK_BYTES));
            }
        }
        BLOB_MANIFEST => {
            let m = bounded::decode::<BlobManifest>(payload)?;
            e.all(BLOB_CHUNK, &m.chunks);
        }
        TAG => e.one(LANDING, bounded::decode::<Tag>(payload)?.landing),
        ANSWER => {
            for h in bounded::decode::<Answer>(payload)?.targets {
                e.any(h);
            }
        }
        SIGNATURE => e.any(bounded::decode::<Signature>(payload)?.subject),
        VIEW_OP => e.view_op(&bounded::decode::<ViewOp>(payload)?),
        INTENT => {
            let i = bounded::decode::<Intent>(payload)?;
            e.opt(INTENT, i.stable);
            e.opt(INTENT, i.prev);
        }
        ALT_SET => e.alt_set(&bounded::decode::<AltSet>(payload)?),
        REVIEW => {
            bounded::decode::<Review>(payload)?;
        }
        TENSION => {
            let t = bounded::decode::<SemanticTension>(payload)?;
            e.one(REVIEW, t.review);
            e.opt(TENSION, t.prev);
        }
        TRANSITION => e.transitions(&bounded::decode::<Transitions>(payload)?),
        REDACTION => e.redaction(&bounded::decode::<Redaction>(payload)?)?,
        REDACTION_RESIDUE => e.residue(&bounded::decode::<RedactionResidue>(payload)?),
        REDACTION_APPLY => {
            let a = bounded::decode::<RedactionApply>(payload)?;
            e.one(REDACTION, a.request);
            e.one(LANDING, a.landing);
        }
        _ => return Err(RecordError::Kind),
    }
    let mut out = e.0;
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// The edges found so far, unsorted.
struct Edges(Vec<ObjectRef>);

impl Edges {
    fn one(&mut self, kind: u8, hash: Hash) {
        self.0.push(ObjectRef {
            kind: Some(kind),
            hash,
        });
    }

    fn opt(&mut self, kind: u8, hash: Option<Hash>) {
        if let Some(h) = hash {
            self.one(kind, h);
        }
    }

    fn all<'a>(&mut self, kind: u8, hashes: impl IntoIterator<Item = &'a Hash>) {
        for h in hashes {
            self.one(kind, *h);
        }
    }

    /// An edge whose kind the field does not fix.
    fn any(&mut self, hash: Hash) {
        self.0.push(ObjectRef { kind: None, hash });
    }

    fn alt(&mut self, choice: &Option<AltChoice>) {
        self.opt(ALT_SET, choice.as_ref().map(|c| c.set));
    }

    fn links(&mut self, links: &ChangeLinks) {
        self.opt(INTENT, links.intent);
        self.alt(&links.alternative);
    }

    /// A figure's `blob` field names its blob manifest. A present field that is not a full hash
    /// is `Decode`. Parent, links and pins name blocks or content, not stored objects.
    fn block(&mut self, b: &Block) -> Result<(), RecordError> {
        if b.block_type != "figure" {
            return Ok(());
        }
        match b.fields.get("blob") {
            None => Ok(()),
            Some(FieldValue::Str(s)) => {
                let h = s.parse::<Hash>().map_err(|_| RecordError::Decode)?;
                self.one(BLOB_MANIFEST, h);
                Ok(())
            }
            Some(_) => Err(RecordError::Decode),
        }
    }

    fn change(&mut self, c: &Change) -> Result<(), RecordError> {
        self.all(CHANGE, &c.deps);
        for op in &c.ops {
            match op {
                Op::Create { block } => {
                    self.one(BLOCK, block.version_hash());
                    self.block(block)?;
                }
                Op::Update { base, block } => {
                    self.one(BLOCK, *base);
                    self.one(BLOCK, block.version_hash());
                    self.block(block)?;
                }
                Op::Delete { base, .. } => self.one(BLOCK, *base),
            }
        }
        self.links(&c.links);
        Ok(())
    }

    fn proposal(&mut self, p: &Proposal) {
        self.opt(PROPOSAL, p.stable);
        self.opt(PROPOSAL, p.prev);
        self.all(CHANGE, &p.changes);
        self.alt(&p.alternative);
        self.opt(LANDING, p.canon_at);
    }

    fn landing(&mut self, l: &LandingRecord) {
        self.one(PROPOSAL, l.proposal);
        self.opt(PROPOSAL, l.approved_version);
        self.opt(LANDING, l.prev);
        self.all(LANDING, &l.rebased_from);
        self.one(SNAPSHOT, l.state);
        self.all(CHANGE, &l.changes);
        self.alt(&l.chosen);
        for h in &l.checks {
            self.any(*h);
        }
    }

    fn view_op(&mut self, v: &ViewOp) {
        self.opt(VIEW_OP, v.prev);
        match &v.op {
            ViewOpKind::Create { base, .. } => self.opt(LANDING, *base),
            ViewOpKind::Fork {
                from_head,
                base,
                changes,
                ..
            } => {
                self.one(VIEW_OP, *from_head);
                self.opt(LANDING, *base);
                self.all(CHANGE, changes);
            }
            ViewOpKind::Commit { change } => self.one(CHANGE, *change),
            ViewOpKind::Include { changes } | ViewOpKind::Exclude { changes } => {
                self.all(CHANGE, changes)
            }
            ViewOpKind::Rebase {
                landing,
                dropped,
                remaining,
                conflicts,
            } => {
                self.one(LANDING, *landing);
                self.all(CHANGE, dropped);
                self.all(CHANGE, remaining);
                for c in conflicts {
                    self.opt(BLOCK, c.base);
                    for s in &c.sides {
                        self.opt(CHANGE, s.change);
                        self.opt(BLOCK, s.version);
                    }
                }
            }
            ViewOpKind::Undo { target } => self.one(VIEW_OP, *target),
            ViewOpKind::Close { .. } | ViewOpKind::Own { .. } => {}
        }
    }

    fn alt_set(&mut self, a: &AltSet) {
        self.opt(ALT_SET, a.stable);
        self.opt(ALT_SET, a.prev);
        if let AltStatus::Chosen { landing, .. } = &a.status {
            self.one(LANDING, *landing);
        }
    }

    fn transitions(&mut self, t: &Transitions) {
        for m in t.resolved.iter().chain(&t.opened) {
            for s in &m.sides {
                self.side(s);
            }
            self.why(&m.why);
        }
        for m in t.gone.iter().chain(&t.new) {
            self.why(&m.why);
        }
    }

    fn side(&mut self, s: &Side) {
        self.opt(CHANGE, s.change);
        self.opt(LANDING, s.landing);
    }

    /// The object that made a move, by its cause; causes without an object add none.
    fn why(&mut self, w: &Why) {
        let kind = match w.cause {
            Cause::Committed => CHANGE,
            Cause::Landed => LANDING,
            Cause::Dismissed | Cause::Reviewed => REVIEW,
            Cause::ViewChanged => VIEW_OP,
            Cause::AltSet => ALT_SET,
            Cause::Written | Cause::PackReload | Cause::Reopened | Cause::Recheck => return,
        };
        self.opt(kind, w.id);
    }

    /// A span names its dropped object by its own kind, which must be a portable kind.
    fn redaction(&mut self, r: &Redaction) -> Result<(), RecordError> {
        self.one(CHANGE, r.carrier);
        for t in &r.targets {
            match t {
                RedactTarget::AllVersions(_) => {}
                RedactTarget::Version { version, .. } => self.one(BLOCK, *version),
                RedactTarget::Blob(h) => self.one(BLOB_MANIFEST, *h),
            }
        }
        let m = &r.manifest;
        self.all(BLOCK, m.versions.iter().map(|(_, h)| h));
        self.all(BLOB_MANIFEST, &m.blobs);
        self.all(BLOB_MANIFEST, &m.shared_blobs);
        for s in &m.spans {
            if !is_portable(s.kind) {
                return Err(RecordError::Decode);
            }
            self.one(s.kind, s.hash);
            self.opt(REDACTION_RESIDUE, s.residue);
        }
        Ok(())
    }

    /// A block residue's version and content hash, and a change residue's change hash, identify
    /// the removed object: no edge.
    fn residue(&mut self, r: &RedactionResidue) {
        let RedactionResidue::Change(c) = r else {
            return;
        };
        for op in &c.ops {
            match op {
                ResidualOp::Create { version, .. } => self.one(BLOCK, *version),
                ResidualOp::Update { base, version, .. } => {
                    self.one(BLOCK, *base);
                    self.one(BLOCK, *version);
                }
                ResidualOp::Delete { base, .. } => self.one(BLOCK, *base),
            }
        }
        self.all(CHANGE, &c.deps);
        self.links(&c.links);
    }
}

/// A hash as full lowercase hex in a human-readable format, and as the hash's own serde
/// otherwise; `Hash`'s own serializer is unchanged.
mod hex {
    use rdy_model::Hash;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(h: &Hash, s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            s.collect_str(h)
        } else {
            h.serialize(s)
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Hash, D::Error> {
        if d.is_human_readable() {
            let text = String::deserialize(d)?;
            text.parse()
                .map_err(|_| serde::de::Error::custom(format!("bad hash {text:?}")))
        } else {
            Hash::deserialize(d)
        }
    }
}
