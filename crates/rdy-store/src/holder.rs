//! The store mode (store format 15, D-BPYN8): a holder keeps uploaded objects and tags with no
//! canon. The ref `meta/store_mode` holds exactly the six bytes `holder` in a holder; a project
//! has no such ref. The mode is set only in the store's first sealed batch, with its format and
//! repo id (`Store::create_holder`), and never added, changed or removed later.
//!
//! A holder's refs are closed: `meta/store_format`, `meta/repo_id`, `meta/origin`, the creation's
//! `meta/store_mode`, `tag/<name>` and (O11) `answer/tag:<name>`, which stands only when
//! `tag/<name>` is in the same batch or an earlier one. Only the take writes an answer ref
//! there, through the store's crate-private path; a public ref write of one is refused. Canon,
//! views, proposals, pending and inbox refs, coordination heads and every other answer ref
//! never stand in a holder.
//!
//! One set of rules serves every path that adds refs: a write (`Store::write` and the seals),
//! the replay of the log into the index (an open's catch-up and a reindex), an ingest, and the
//! fsck (`StoreReader::check_meta`, which also checks the index's refs against the log's first
//! batch). The mode is read from the log's first batch before the log's other refs are judged.
use crate::error::StoreError;
use crate::format::REF_STORE_FORMAT;
use crate::kinds::{self, RefUpdate};
use crate::meta::{REF_ORIGIN, REF_REPO_ID};
use crate::segment::Location;
use crate::store::StoreReader;
use std::collections::BTreeSet;

/// The ref naming a holder: present only in a holder, with the value `HOLDER_MODE`.
pub const REF_STORE_MODE: &str = "meta/store_mode";
/// The value of `meta/store_mode` in a holder: exactly these six bytes.
pub const HOLDER_MODE: &[u8] = b"holder";
/// The prefix of a tag ref.
pub const TAG_REF: &str = "tag/";
/// The prefix of the answer ref that relays a tag (O11): `answer/tag:<name>`.
pub const ANSWER_TAG_REF: &str = "answer/tag:";

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
    /// `meta/store_format`, `meta/repo_id` or `meta/origin`.
    Meta,
    /// `meta/store_mode`: the creation's only.
    Mode,
    /// `tag/<name>`.
    Tag(&'a str),
    /// `answer/tag:<name>`.
    AnswerTag(&'a str),
    /// Anything else: never in a holder.
    Other,
}

fn family(name: &str) -> Family<'_> {
    if name == REF_STORE_FORMAT || name == REF_REPO_ID || name == REF_ORIGIN {
        Family::Meta
    } else if name == REF_STORE_MODE {
        Family::Mode
    } else if let Some(t) = name.strip_prefix(TAG_REF) {
        Family::Tag(t)
    } else if let Some(t) = name.strip_prefix(ANSWER_TAG_REF) {
        Family::AnswerTag(t)
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

/// The rules for refs the log holds, in log order: the replay of an open or a reindex, and an
/// ingest. A mode ref outside the first batch is refused, and so is a mode value other than
/// `holder` in it. In a holder, every ref must be in a holder family, and an `answer/tag:<name>`
/// needs `tag/<name>` in its batch or an earlier one (checked at the batch's marker).
pub(crate) struct RefRules {
    mode: StoreMode,
    /// A holder's tag names of the batches before this one.
    tags: BTreeSet<String>,
    /// A holder's tag names of this batch.
    batch_tags: BTreeSet<String>,
    /// A holder's answer refs of this batch.
    batch_answers: Vec<String>,
}

impl RefRules {
    /// The rules for a log whose first batch set `mode`, with the tag names `tags` already in
    /// the batches before the first one these rules see.
    pub fn new(mode: StoreMode, tags: BTreeSet<String>) -> RefRules {
        RefRules {
            mode,
            tags,
            batch_tags: BTreeSet::new(),
            batch_answers: Vec::new(),
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
        if self.mode == StoreMode::Project {
            return Ok(());
        }
        match family(name) {
            Family::Meta | Family::Mode => Ok(()),
            Family::Tag(t) => {
                self.batch_tags.insert(t.to_string());
                Ok(())
            }
            Family::AnswerTag(_) => {
                self.batch_answers.push(name.to_string());
                Ok(())
            }
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

    /// Checks the end of a batch: each answer ref of it needs its tag in it or before it.
    pub fn check_marker(&self) -> Result<(), StoreError> {
        for name in &self.batch_answers {
            let t = name.strip_prefix(ANSWER_TAG_REF).unwrap_or(name);
            if !self.batch_tags.contains(t) && !self.tags.contains(t) {
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
    /// The take's crate-private write: a holder's `answer/tag:<name>` beside its tag.
    Take,
    /// The creation batch, and the test hook that forges refs: no ref rule.
    Unchecked,
}

/// The ref rules of a write of `items` to a store of `mode`, before anything is appended. The
/// mode ref is never written (`store mode cannot change`). In a holder, a ref outside the
/// holder families is refused, and so is an answer ref outside the take; the take's answer ref
/// needs `tag/<name>` in `items` or already in the store (`has_tag`).
pub(crate) fn check_ref_writes(
    mode: StoreMode,
    items: &[(u8, Vec<u8>)],
    how: RefWrite,
    has_tag: impl Fn(&str) -> Result<bool, StoreError>,
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
            Family::Meta | Family::Mode | Family::Tag(_) => {}
            Family::AnswerTag(t) => {
                let beside = refs.iter().any(|r| r.name.strip_prefix(TAG_REF) == Some(t));
                if how != RefWrite::Take || !(beside || has_tag(t)?) {
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
    /// refs are all in its families, each `answer/tag:<name>` beside its `tag/<name>`.
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
                Family::Meta | Family::Mode | Family::Tag(_) => {}
                Family::AnswerTag(t) => {
                    let tag = format!("{TAG_REF}{t}");
                    if refs
                        .binary_search_by(|(n, _)| n.as_str().cmp(&tag))
                        .is_err()
                    {
                        return Err(not_allowed(name));
                    }
                }
                Family::Other => return Err(not_allowed(name)),
            }
        }
        Ok(())
    }

    /// The log's first batch, read from the origin to its marker, up to the index's end.
    pub(crate) fn read_creation(&self) -> Result<Creation, StoreError> {
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
            Ok(()) | Err(Walk::Done) => Ok(first.creation(upto)),
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

    #[test]
    fn ha4_take_writes_an_answer_tag_beside_its_tag() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h");
        let mut s = Store::create_holder(&path, RepoId([7; 16])).unwrap();
        let ans: &[u8] = &[4; 32];
        // no tag yet: refused, and nothing is written
        let end = s.log_end();
        let items = refs(end, &[("answer/tag:v1", ans)]);
        assert!(matches!(
            s.write_take(items),
            Err(StoreError::Refused(m)) if m == "a holder store cannot write ref answer/tag:v1"
        ));
        assert_eq!(s.log_end(), end);
        // beside its tag in the same write
        let items = refs(s.log_end(), &[("answer/tag:v1", ans), ("tag/v1", &[5; 32])]);
        s.write_take(items).unwrap();
        // beside a tag of an earlier batch
        let items = refs(s.log_end(), &[("tag/v2", &[6; 32])]);
        s.write(items).unwrap();
        let items = refs(s.log_end(), &[("answer/tag:v2", ans)]);
        s.write_take(items).unwrap();
        // the take never writes another family, nor the mode
        for name in ["canon", "answer/q-1", REF_STORE_MODE] {
            let items = refs(s.log_end(), &[(name, ans)]);
            assert!(matches!(s.write_take(items), Err(StoreError::Refused(_))));
        }
        s.check_meta().unwrap();
        drop(s);
        // the log the take wrote reopens and reindexes
        let mut s = Store::open(&path).unwrap();
        s.reindex().unwrap();
        assert_eq!(s.get_ref("answer/tag:v2").unwrap(), Some(ans.to_vec()));
        assert_eq!(s.mode().unwrap(), StoreMode::Holder);
    }
}
