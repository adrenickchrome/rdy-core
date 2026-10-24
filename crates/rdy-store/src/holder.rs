//! The store mode (store format 15, D-BPYN8): a holder keeps uploaded objects and tags with no
//! canon. The ref `meta/store_mode` holds exactly the six bytes `holder` in a holder; a project
//! has no such ref. The mode is set only in the store's first sealed batch, with its format and
//! repo id (`Store::create_holder`), and never added, changed or removed later.
//!
//! A holder's refs are closed: `meta/store_features`, `meta/upgrade`, a dump's historical
//! `meta/store_format`, `meta/repo_id`, `meta/origin`, the creation's
//! `meta/store_mode`, `tag/<name>` and (O11, ruling task-13) `answer/tag-object:<hash>`, which
//! names the relay answer of the TAG object whose hash is `<hash>` (64 lowercase hex digits):
//! an ANSWER of act `tag` and item `tag:<that tag's name>` whose targets are exactly that TAG
//! (`is_relay_answer_of`).
//! It stands only when that TAG object is in the same batch or an earlier one. A holder may
//! keep several tag objects of one version (the first stays the version; a twin's tag is kept
//! beside it), so its relay answers are found by tag object, never by tag name: a local
//! project's `answer/tag:<name>` never stands in a holder. Only the take writes an answer ref
//! there, through the store's crate-private path; a public ref write of one is refused. Canon,
//! views, proposals, plans (`plan/`), pending and inbox refs, coordination heads and every other
//! answer ref never stand in a holder. A holder keeps the PLAN objects a take brings (a seal and
//! its chain, D-4JMPH) and their derived tables, with no `plan/` ref.
//!
//! One set of rules serves every path that adds refs: a write (`Store::write` and the seals),
//! the replay of the log into the index (an open's catch-up and a reindex), an ingest, and the
//! fsck (`StoreReader::check_meta`, which also checks the index's refs against the log's first
//! batch). The mode is read from the log's first batch before the log's other refs are judged.
//!
//! The log, not the index's final refs, is what these rules judge: a late mode write of the
//! creation's own value, or an answer ref in a batch before its tag's, leaves refs that look
//! right. So the fsck walks the whole indexed log, and a holder's open walks the log up to where
//! its index stands, whether or not anything is left to replay; the replay then goes on with the
//! walk's rules, so a batch that a failed catch-up left half indexed is judged whole again.
use crate::error::StoreError;
use crate::format::{REF_STORE_FEATURES, REF_STORE_FORMAT, REF_UPGRADE};
use crate::kinds::{self, RefUpdate};
use crate::meta::{REF_ORIGIN, REF_REPO_ID};
use crate::segment::{Holes, Location, SegmentReader};
use crate::store::StoreReader;
use rdy_model::{Answer, AnswerAct, Hash};
use std::collections::BTreeSet;

/// The ref naming a holder: present only in a holder, with the value `HOLDER_MODE`.
pub const REF_STORE_MODE: &str = "meta/store_mode";
/// The value of `meta/store_mode` in a holder: exactly these six bytes.
pub const HOLDER_MODE: &[u8] = b"holder";
/// The prefix of a tag ref.
pub const TAG_REF: &str = "tag/";
/// The prefix of a holder's answer ref that relays a tag object (O11, ruling task-13):
/// `answer/tag-object:<64 lowercase hex digits of the TAG's hash>`.
pub const ANSWER_TAG_OBJECT_REF: &str = "answer/tag-object:";

/// A holder's ref of the relay answer of the TAG object `tag`: `answer/tag-object:<hex>`.
pub fn answer_tag_object_ref(tag: &Hash) -> String {
    format!("{ANSWER_TAG_OBJECT_REF}{tag}")
}

/// The TAG hash an `answer/tag-object:` ref names: exactly 64 lowercase hex digits after the
/// prefix; None for any other name.
pub fn answer_tag_object(name: &str) -> Option<Hash> {
    tag_object(name.strip_prefix(ANSWER_TAG_OBJECT_REF)?)
}

/// Whether `a` is the relay answer of the TAG object `tag` named `name` (O11, ruling task-13):
/// act `tag`, item `tag:<name>`, and targets exactly that TAG, as `Store::prepare_tag` writes it.
/// Two tag objects of one name never share a relay answer.
pub fn is_relay_answer_of(a: &Answer, tag: &Hash, name: &str) -> bool {
    a.act == AnswerAct::Tag
        && a.item.strip_prefix("tag:") == Some(name)
        && a.targets.as_slice() == [*tag]
}

/// `hex` as a hash: exactly 64 lowercase hex digits.
fn tag_object(hex: &str) -> Option<Hash> {
    if hex.len() != 64 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    hex.parse().ok()
}

/// What kind of store this is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreMode {
    /// A project: canon, views and the rest (no `meta/store_mode`).
    Project,
    /// A holder: uploaded objects and tags with no canon (`meta/store_mode` is `holder`).
    Holder,
}

/// The refusal of a `meta/store_mode` value other than `holder`.
const BAD_MODE: &str = "meta/store_mode must be holder when present";
/// The refusal of a mode ref outside the first batch, or of an index whose mode is not the one
/// the first batch set.
const LATE_MODE: &str = "holder mode must be set in the first batch";
/// The refusal of a write of the mode ref.
const CHANGE_MODE: &str = "store mode cannot change";

fn bad(m: impl Into<String>) -> StoreError {
    StoreError::Decode(m.into())
}

/// The refusal of a holder ref that the log or the index holds.
pub(crate) fn not_allowed(name: &str) -> StoreError {
    bad(format!("holder ref {name} is not allowed"))
}

/// The refusal of a holder ref write.
pub(crate) fn cannot_write(name: &str) -> StoreError {
    StoreError::Refused(format!("a holder store cannot write ref {name}"))
}

/// The refusal of a write of `meta/store_mode`.
pub(crate) fn cannot_change() -> StoreError {
    StoreError::Refused(CHANGE_MODE.into())
}

/// The refusal of a mode ref that is not the creation's.
pub(crate) fn late_mode() -> StoreError {
    bad(LATE_MODE)
}

/// The mode a `meta/store_mode` value names: none is a project, exactly `holder` is a holder,
/// and anything else is `StoreError::Decode`.
pub fn decode_mode(value: Option<&[u8]>) -> Result<StoreMode, StoreError> {
    match value {
        None => Ok(StoreMode::Project),
        Some(HOLDER_MODE) => Ok(StoreMode::Holder),
        Some(_) => Err(bad(BAD_MODE)),
    }
}

/// Where a ref belongs, for a holder.
enum Family<'a> {
    /// `meta/store_features`, `meta/upgrade`, `meta/store_format`, `meta/repo_id` or
    /// `meta/origin`.
    Meta,
    /// `meta/store_mode`: the creation's only.
    Mode,
    /// `tag/<name>`.
    Tag,
    /// `answer/tag-object:<hex>`, with the text after the prefix.
    AnswerTagObject(&'a str),
    /// Anything else: never in a holder.
    Other,
}

fn family(name: &str) -> Family<'_> {
    if [
        REF_STORE_FEATURES,
        REF_UPGRADE,
        REF_STORE_FORMAT,
        REF_REPO_ID,
        REF_ORIGIN,
    ]
    .contains(&name)
    {
        Family::Meta
    } else if name == REF_STORE_MODE {
        Family::Mode
    } else if name.starts_with(TAG_REF) {
        Family::Tag
    } else if let Some(t) = name.strip_prefix(ANSWER_TAG_OBJECT_REF) {
        Family::AnswerTagObject(t)
    } else {
        Family::Other
    }
}

/// What the log's first sealed batch says: the mode it sets and where it ends (just past its
/// marker). An empty log has no first batch yet: a project, ending at the origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Creation {
    pub mode: StoreMode,
    pub end: Location,
}

impl Creation {
    /// The creation of an empty log.
    pub const EMPTY: Creation = Creation {
        mode: StoreMode::Project,
        end: Location {
            segment: 0,
            offset: 0,
        },
    };

    /// Whether the frame at `loc` is in the first batch.
    pub fn in_first(&self, loc: Location) -> bool {
        loc < self.end
    }
}

/// Reads the first batch of a log frame by frame, from its origin: `frame` until it returns
/// true (the marker), then `creation`.
#[derive(Default)]
pub(crate) struct FirstBatch {
    holder: bool,
    end: Option<Location>,
}

impl FirstBatch {
    /// Takes the frame `kind` whose payload is `payload` (None for an authorized hole) and whose
    /// next frame is at `next`. Returns true at the batch's marker. A mode ref whose value is not
    /// `holder` is `StoreError::Decode`.
    pub fn frame(
        &mut self,
        kind: u8,
        payload: Option<&[u8]>,
        next: Location,
    ) -> Result<bool, StoreError> {
        if kind == kinds::BATCH_COMMIT {
            self.end = Some(next);
            return Ok(true);
        }
        if kind == kinds::REF_UPDATE
            && let Some(u) = payload.and_then(decode_ref)
            && u.name == REF_STORE_MODE
        {
            decode_mode(Some(&u.value))?;
            self.holder = true;
        }
        Ok(false)
    }

    /// The creation, once the walk ended: at the marker, or at `log_end` (an empty log, or a
    /// log with no marker yet).
    pub fn creation(&self, log_end: Location) -> Creation {
        Creation {
            mode: if self.holder {
                StoreMode::Holder
            } else {
                StoreMode::Project
            },
            end: self.end.unwrap_or(log_end),
        }
    }
}

/// A `REF_UPDATE` payload, if it decodes: one that does not is damage the index's own decode
/// refuses.
pub(crate) fn decode_ref(payload: &[u8]) -> Option<RefUpdate> {
    postcard::from_bytes(payload).ok()
}

/// Whether the `REF_UPDATE` payload `payload` names the mode ref, read as the ingest and the
/// open read it ([`decode_ref`]). A caller that must agree with the store's own judgement of a
/// log's mode (a load that branches on it) asks here, not through a stricter decoder.
pub fn names_store_mode(payload: &[u8]) -> bool {
    decode_ref(payload).is_some_and(|u| u.name == REF_STORE_MODE)
}

/// The rules for refs the log holds, in log order: the replay of an open or a reindex, and an
/// ingest. A mode ref outside the first batch is refused, and so is a mode value other than
/// `holder` in it. In a holder, every ref must be in a holder family, and an
/// `answer/tag-object:<hash>` needs its TAG object in its batch or an earlier one (checked at
/// the batch's marker).
pub(crate) struct RefRules {
    creation: Creation,
    /// A holder's TAG objects of the batches before this one.
    tags: BTreeSet<Hash>,
    /// A holder's TAG objects of this batch.
    batch_tags: BTreeSet<Hash>,
    /// A holder's answer refs of this batch, each with the TAG it names.
    batch_answers: Vec<(String, Hash)>,
}

impl RefRules {
    /// The rules for a log whose first batch is `creation`, from its origin.
    pub fn new(creation: Creation) -> RefRules {
        RefRules {
            creation,
            tags: BTreeSet::new(),
            batch_tags: BTreeSet::new(),
            batch_answers: Vec::new(),
        }
    }

    /// The rules over the frames of the log `segments` reads from its origin up to `upto`
    /// (`holes` are its authorized holes), as a replay from its origin would judge them. A batch
    /// that `upto` cuts is pending: its refs count, and its end is judged when the frames past
    /// `upto` reach its marker.
    pub fn of_log(
        segments: &SegmentReader,
        holes: &Holes,
        creation: Creation,
        upto: Location,
    ) -> Result<RefRules, StoreError> {
        let mut rules = RefRules::new(creation);
        segments.for_each_view_from(Creation::EMPTY.end, upto, holes, |loc, v, _| {
            rules.on_frame(loc, v.kind, v.payload)
        })?;
        Ok(rules)
    }

    /// Takes the log frame `kind` at `loc` whose payload is `payload` (None for an authorized
    /// hole): a ref, a batch's marker, a TAG object, or a frame no rule reads.
    pub fn on_frame(
        &mut self,
        loc: Location,
        kind: u8,
        payload: Option<&[u8]>,
    ) -> Result<(), StoreError> {
        match (kind, payload) {
            (kinds::REF_UPDATE, Some(p)) => self.on_ref_frame(self.creation.in_first(loc), p),
            (kinds::BATCH_COMMIT, _) => self.on_marker(),
            (kinds::TAG, Some(p)) => {
                self.on_tag(p);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Takes a TAG object of the batch being judged (its payload): an answer ref of the batch
    /// or of a later one may name it. A project keeps none.
    pub fn on_tag(&mut self, payload: &[u8]) {
        if self.creation.mode == StoreMode::Holder {
            self.batch_tags.insert(Hash::of(payload));
        }
    }

    /// Takes a ref of the log: `name` set to `value`, in the first batch when `in_first`.
    pub fn on_ref(&mut self, in_first: bool, name: &str, value: &[u8]) -> Result<(), StoreError> {
        if name == REF_STORE_MODE {
            if !in_first {
                return Err(late_mode());
            }
            decode_mode(Some(value))?;
            return Ok(());
        }
        if self.creation.mode == StoreMode::Project {
            return Ok(());
        }
        match family(name) {
            Family::Meta | Family::Mode => Ok(()),
            Family::Tag => Ok(()),
            Family::AnswerTagObject(t) => match tag_object(t) {
                Some(h) => {
                    self.batch_answers.push((name.to_string(), h));
                    Ok(())
                }
                None => Err(not_allowed(name)),
            },
            Family::Other => Err(not_allowed(name)),
        }
    }

    /// Takes a `REF_UPDATE` frame's payload; one that does not decode is left to the index's
    /// own decode.
    pub fn on_ref_frame(&mut self, in_first: bool, payload: &[u8]) -> Result<(), StoreError> {
        match decode_ref(payload) {
            Some(u) => self.on_ref(in_first, &u.name, &u.value),
            None => Ok(()),
        }
    }

    /// Forgets the batch being judged, as if it had never been seen.
    pub fn abandon_batch(&mut self) {
        self.batch_tags.clear();
        self.batch_answers.clear();
    }

    /// Ends a batch: `check_marker`, then `commit_batch`.
    pub fn on_marker(&mut self) -> Result<(), StoreError> {
        self.check_marker()?;
        self.commit_batch();
        Ok(())
    }

    /// Checks the end of a batch: each answer ref of it needs its TAG object in it or before
    /// it.
    pub fn check_marker(&self) -> Result<(), StoreError> {
        for (name, h) in &self.batch_answers {
            if !self.batch_tags.contains(h) && !self.tags.contains(h) {
                return Err(not_allowed(name));
            }
        }
        Ok(())
    }

    /// Keeps the batch's tags for the batches after it.
    pub fn commit_batch(&mut self) {
        self.batch_answers.clear();
        self.tags.append(&mut self.batch_tags);
    }
}

/// How a write may touch refs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RefWrite {
    /// Every public write and seal.
    Public,
    /// The take's crate-private write: a holder's `answer/tag-object:<hash>` beside its TAG.
    Take,
    /// The creation batch, and the test hook that forges refs: no ref rule.
    Unchecked,
}

/// The ref rules of a write of `items` to a store of `mode`, before anything is appended. The
/// mode ref is never written (`store mode cannot change`). In a holder, a ref outside the
/// holder families is refused, and so is an answer ref outside the take; the take's
/// `answer/tag-object:<hash>` needs its TAG object in `items` or already in the store
/// (`has_tag`).
pub(crate) fn check_ref_writes(
    mode: StoreMode,
    items: &[(u8, Vec<u8>)],
    how: RefWrite,
    has_tag: impl Fn(&Hash) -> Result<bool, StoreError>,
) -> Result<(), StoreError> {
    if how == RefWrite::Unchecked {
        return Ok(());
    }
    let decoded = || {
        items
            .iter()
            .filter(|(k, _)| *k == kinds::REF_UPDATE)
            .filter_map(|(_, p)| decode_ref(p))
    };
    if mode == StoreMode::Project {
        // a project's only rule: the mode ref is never written
        if decoded().any(|u| u.name == REF_STORE_MODE) {
            return Err(cannot_change());
        }
        return Ok(());
    }
    let refs: Vec<RefUpdate> = decoded().collect();
    for u in &refs {
        if u.name == REF_STORE_MODE {
            return Err(cannot_change());
        }
        match family(&u.name) {
            Family::Meta | Family::Mode | Family::Tag => {}
            Family::AnswerTagObject(t) => {
                let Some(h) = tag_object(t) else {
                    return Err(cannot_write(&u.name));
                };
                let beside = items
                    .iter()
                    .any(|(k, p)| *k == kinds::TAG && Hash::of(p) == h);
                if how != RefWrite::Take || !(beside || has_tag(&h)?) {
                    return Err(cannot_write(&u.name));
                }
            }
            Family::Other => return Err(cannot_write(&u.name)),
        }
    }
    Ok(())
}

impl StoreReader {
    /// The store's mode: `meta/store_mode` absent is a project, `holder` a holder; any other
    /// value is `StoreError::Decode`.
    pub fn mode(&self) -> Result<StoreMode, StoreError> {
        decode_mode(self.get_ref(REF_STORE_MODE)?.as_deref())
    }

    /// The index's refs against what the log's first batch set (`creation`): the mode ref is
    /// the creation's (`holder mode must be set in the first batch` otherwise), and a holder's
    /// refs are all in its families, each `answer/tag-object:<hash>` naming a stored TAG.
    pub(crate) fn check_mode_against(&self, creation: Creation) -> Result<(), StoreError> {
        let stored = self.get_ref(REF_STORE_MODE)?;
        let matches = match (creation.mode, stored.as_deref()) {
            (StoreMode::Project, None) => true,
            (StoreMode::Holder, Some(v)) => v == HOLDER_MODE,
            _ => false,
        };
        if !matches {
            return Err(late_mode());
        }
        if creation.mode == StoreMode::Project {
            return Ok(());
        }
        let refs = self.index().refs_with_prefix("")?;
        for (name, _) in &refs {
            match family(name) {
                Family::Meta | Family::Mode | Family::Tag => {}
                Family::AnswerTagObject(t) => match tag_object(t) {
                    Some(h) if self.has_kind(kinds::TAG, &h)? => {}
                    _ => return Err(not_allowed(name)),
                },
                Family::Other => return Err(not_allowed(name)),
            }
        }
        Ok(())
    }

    /// fsck: the log's first batch (`read_creation`), then every ref of the indexed log judged
    /// by the rules of that creation, in log order, through the end of the last batch indexed.
    /// Returns the creation.
    pub(crate) fn check_ref_history(&self) -> Result<Creation, StoreError> {
        let (creation, holes, upto) = self.read_creation()?;
        RefRules::of_log(self.segments(), &holes, creation, upto)?.check_marker()?;
        Ok(creation)
    }

    /// The log's first batch, read from the origin to its marker, up to the index's end; with
    /// the holes of the indexed log and that end.
    fn read_creation(&self) -> Result<(Creation, Holes, Location), StoreError> {
        let upto = self.index().indexed_upto()?;
        let spans = self.index().redaction_spans_in(Creation::EMPTY.end, upto)?;
        let holes = crate::store::holes_of(&spans);
        let mut first = FirstBatch::default();
        let walked =
            self.segments()
                .for_each_view_from(Creation::EMPTY.end, upto, &holes, |_, v, next| match first
                    .frame(v.kind, v.payload, next)
                {
                    Ok(true) => Err(Walk::Done),
                    Ok(false) => Ok(()),
                    Err(e) => Err(Walk::Failed(e)),
                });
        match walked {
            Ok(()) | Err(Walk::Done) => Ok((first.creation(upto), holes, upto)),
            Err(Walk::Failed(e)) => Err(e),
        }
    }
}

/// A walk of the first batch that ends at its marker.
pub(crate) enum Walk {
    Done,
    Failed(StoreError),
}

impl From<std::io::Error> for Walk {
    fn from(e: std::io::Error) -> Walk {
        Walk::Failed(e.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use rdy_model::RepoId;

    fn refs(at: Location, refs: &[(&str, &[u8])]) -> Vec<(u8, Vec<u8>)> {
        refs.iter()
            .enumerate()
            .map(|(i, (name, value))| {
                let u = RefUpdate {
                    name: (*name).into(),
                    value: value.to_vec(),
                    at: Location {
                        segment: at.segment,
                        offset: at.offset + i as u64,
                    },
                };
                (kinds::REF_UPDATE, crate::store::encode(&u))
            })
            .collect()
    }

    /// A TAG record's frame (its bytes need not decode for the ref rules).
    fn tag_frame(n: u8) -> (u8, Vec<u8>) {
        (kinds::TAG, vec![n; 40])
    }

    #[test]
    fn ha4_take_writes_an_answer_tag_object_beside_its_tag() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h");
        let mut s = Store::create_holder(&path, RepoId([7; 16])).unwrap();
        let ans: &[u8] = &[4; 32];
        let (t1, t2) = (tag_frame(1), tag_frame(2));
        let (n1, n2) = (
            answer_tag_object_ref(&Hash::of(&t1.1)),
            answer_tag_object_ref(&Hash::of(&t2.1)),
        );
        // no tag object yet: refused, and nothing is written
        let end = s.log_end();
        let items = refs(end, &[(&n1, ans)]);
        assert!(matches!(
            s.write_take(items),
            Err(StoreError::Refused(m)) if m == format!("a holder store cannot write ref {n1}")
        ));
        assert_eq!(s.log_end(), end);
        // beside its tag object in the same write
        let mut items = vec![t1.clone()];
        items.extend(refs(s.log_end(), &[(&n1, ans), ("tag/v1", &[5; 32])]));
        s.write_take(items).unwrap();
        // beside a tag object of an earlier batch
        s.write(vec![t2.clone()]).unwrap();
        let items = refs(s.log_end(), &[(&n2, ans)]);
        s.write_take(items).unwrap();
        // a name that is not 64 lowercase hex digits, a local project's answer ref, another
        // family and the mode: never
        let upper = n2
            .to_uppercase()
            .replace("ANSWER/TAG-OBJECT:", ANSWER_TAG_OBJECT_REF);
        for name in [
            upper.as_str(),
            &n2[..n2.len() - 1],
            "answer/tag:v1",
            "canon",
            "answer/q-1",
            REF_STORE_MODE,
        ] {
            let items = refs(s.log_end(), &[(name, ans)]);
            assert!(
                matches!(s.write_take(items), Err(StoreError::Refused(_))),
                "{name}"
            );
        }
        s.check_meta().unwrap();
        drop(s);
        // the log the take wrote reopens and reindexes
        let mut s = Store::open(&path).unwrap();
        s.reindex().unwrap();
        assert_eq!(s.get_ref(&n2).unwrap(), Some(ans.to_vec()));
        assert_eq!(s.mode().unwrap(), StoreMode::Holder);
    }
}
