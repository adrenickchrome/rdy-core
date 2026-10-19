//! Versions (addendum A): immutable SemVer tags on canon's landings, created only by an approver.
use crate::answer::relay::{RelayInput, relayed};
use crate::error::*;
use crate::{Rdy, ReadState, Scope, Session};
use rdy_model::{ActorRun, Answer, AnswerAct, EvidenceKind, Hash, LandingRecord, Op, Role, Tag};
use rdy_schema::Pack;
use rdy_store::StoreError;
use rdy_store::holder::StoreMode;
use rdy_store::kinds::{self, BlobManifest};
use serde::Serialize;
use std::cmp::Ordering;
use std::collections::hash_map::Entry;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

/// How many as-of (`tag:`/`landing:`) snapshots, with their graphs, stay cached at once.
pub const AS_OF_CACHE_ENTRIES: usize = 4;

/// How many version positions stay cached at once; the cache is cleared when it is full.
const VPOS_CACHE_ENTRIES: usize = 1024;

/// What a cached version position is for. A view's key holds its head op; a proposal's, its
/// version hash (a version never changes).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum VposScope {
    Canon,
    At(Hash),
    View(String, Option<Hash>),
    Proposal(Hash),
}

/// One cached position's key: the canon head and tag count of the state that asked, and the
/// scope. Every input is in the key, so the cache is shared by all states and never stale.
type VposKey = (Option<Hash>, usize, VposScope);

/// What the cached tag markers are for: the canon head, the tag count and the redaction epoch
/// of the state that asked. Tags are immutable and only added, and an apply lands with its
/// request's carrier, so these name one set of markers under one pack.
type MarkKey = (Option<Hash>, usize, u64);

/// The pack a cached marker entry was read under: which types are assets decides which `blob`
/// fields are followed. Compared by pointer; the entry holds the `Arc`, so the address is never
/// reused while the entry lives (a state held from before a pack reload keeps the old pack).
#[derive(Clone)]
struct PackId(Arc<Pack>);

impl PackId {
    fn is(&self, pack: &Arc<Pack>) -> bool {
        Arc::ptr_eq(&self.0, pack)
    }
}

/// Each landing's own requests (see `ReadState::landing_requests`), by landing hash, for one
/// redaction epoch and one pack: the active mask of an epoch never changes, and a landing never
/// changes.
type LandingMarks = (u64, PackId, Arc<HashMap<Hash, Vec<Hash>>>);

/// The tag markers of one state (see `MarkKey`), with the pack they were read under.
type TagMarks = (MarkKey, PackId, Arc<Vec<Vec<Hash>>>);

/// The cached positions, shared by every published state (see `VposKey`), the tag markers of one
/// state (see `MarkKey`) and the per-landing requests they are built from (see `LandingMarks`).
#[derive(Default)]
pub(crate) struct VposCache {
    entries: HashMap<VposKey, VersionPosition>,
    marks: Option<TagMarks>,
    landings: Option<LandingMarks>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VersionInfo {
    pub name: String,
    /// The full hash of the tag object.
    pub tag: String,
    /// The full hash of the landing the version names.
    pub landing: String,
    pub message: Option<String>,
    pub tagged_by: String,
    pub at_ms: i64,
    /// rdx6: true when the version's history (its landing, the landings before it and the
    /// changes they carried) holds content a redaction removed; absent otherwise. The tag record
    /// itself never changes.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub redacted: bool,
    /// rdx6: the requests that removed that content (full hashes, ascending); absent when none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub redactions: Vec<String>,
}

/// What `Rdy::tag_as` wrote: the version, and for a relayed tag the agent that relayed it and
/// the `ANSWER` that holds the relay (D-DGCH7).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TagInfo {
    #[serde(flatten)]
    pub version: VersionInfo,
    pub relayed_by: Option<ActorRun>,
    /// The `ANSWER` record's hash.
    pub answer: Option<String>,
}

fn info(t: &Tag) -> VersionInfo {
    VersionInfo {
        name: t.name.clone(),
        tag: t.id().to_string(),
        landing: t.landing.to_string(),
        message: t.message.clone(),
        tagged_by: t.tagged_by.actor.clone(),
        at_ms: t.tagged_by.at_ms,
        redacted: false,
        redactions: vec![],
    }
}

/// `info(t)` with the markers of the requests in `marks` (ascending).
fn marked(t: &Tag, marks: &[Hash]) -> VersionInfo {
    let mut v = info(t);
    v.redacted = !marks.is_empty();
    v.redactions = marks.iter().map(Hash::to_string).collect();
    v
}

/// The most bytes a new version name may hold, and the longest name the live site serves.
/// Only new writes take this bound (ruling final): `rdy tag` refuses a longer name
/// ([`new_version_name_ok`]), and the site serves no page for one (`site_scope::tag_name_ok`).
/// A stored tag reads at any length: an older release could store a longer name, and reads,
/// fsck, dump and load keep it ([`parse_version`] has no bound). The live site's request head
/// (`MAX_HEAD_BYTES` in rdy-daemon) is sized to carry the longest served name with its page path
/// and the other request-head bytes (rdy-daemon asserts the fit at compile time).
pub const MAX_VERSION_BYTES: usize = 256;

/// The bound on a new version name (ruling final (2)): at most [`MAX_VERSION_BYTES`] bytes. A
/// rule for new writes only: a stored tag never meets it.
pub fn new_version_name_ok(name: &str) -> Result<(), RdyError> {
    if name.len() > MAX_VERSION_BYTES {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!(
                "the version name is {} bytes; a new version name has at most {MAX_VERSION_BYTES}",
                name.len()
            ),
            "use a shorter name",
        ));
    }
    Ok(())
}

/// Parses a version name: one optional leading `v`, then a SemVer 2.0.0 version without build
/// metadata (SemVer ignores build metadata when ordering, so two versions would share a position).
/// It has no length bound: it judges stored tags too, which keep every name the release that
/// wrote them accepted (ruling final (1)).
pub fn parse_version(name: &str) -> Result<semver::Version, RdyError> {
    let bad = |why: String| {
        RdyError::new(
            E_BAD_TAG_NAME,
            format!("{name:?} is not a version: {why}"),
            "name versions like v1.2.0 or 2.0.0-rc.1 (Semantic Versioning, one optional leading v, no +build metadata)",
        )
    };
    let bare = name.strip_prefix('v').unwrap_or(name);
    let v = semver::Version::parse(bare).map_err(|e| bad(e.to_string()))?;
    if !v.build.is_empty() {
        return Err(bad(
            "build metadata (+…) is not allowed: it doesn't take part in version order".into(),
        ));
    }
    Ok(v)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VersionRef {
    pub name: String,
    pub landing: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ViewPosition {
    pub name: String,
    pub unlanded_changes: u32,
}

/// Where a scope stands relative to the versions: shown by rdy status, MCP and every rendered page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VersionPosition {
    /// The highest version at or before the scope's base landing.
    pub version: Option<VersionRef>,
    /// Landings after that version, up to and including the base landing.
    pub landings_since: u32,
    /// For a view: its name and its changes not yet in canon.
    pub view: Option<ViewPosition>,
    /// The scope is an as-of snapshot (tag: or landing:), which never changes.
    pub frozen: bool,
    pub label: String,
}

fn plural(n: u32, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn label_of(
    version: &Option<VersionRef>,
    since: u32,
    view: &Option<ViewPosition>,
    frozen: bool,
) -> String {
    let mut out = match version {
        None => "no version yet".to_string(),
        Some(v) if since == 0 => v.name.clone(),
        Some(v) => format!("{} + {}", v.name, plural(since, "landing", "landings")),
    };
    if let Some(v) = view {
        out.push_str(&format!(
            " · view {}, {}",
            v.name,
            plural(v.unlanded_changes, "unlanded change", "unlanded changes")
        ));
    }
    if frozen {
        out.push_str(" · frozen snapshot");
    }
    out
}

/// `text` as a landing id spells it, lowercased: a full hash, or a hex prefix of at least 6
/// characters. Anything else is `E_NOT_FOUND`.
pub(crate) fn landing_spelling(text: &str) -> Result<String, RdyError> {
    let want = text.to_ascii_lowercase();
    if want.len() < 6 || !want.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(RdyError::new(
            E_NOT_FOUND,
            format!("{text:?} is not a landing id"),
            "pass a landing hash, or at least 6 of its leading hex characters (see rdy log)",
        ));
    }
    Ok(want)
}

/// The candidates `text` names: a full hash, or a unique hex prefix of at least 6 characters.
pub(crate) fn match_prefix(candidates: &[Hash], text: &str) -> Result<Hash, RdyError> {
    let want = landing_spelling(text)?;
    let hits: Vec<Hash> = candidates
        .iter()
        .copied()
        .filter(|h| h.to_string().starts_with(&want))
        .collect();
    match hits.as_slice() {
        [one] => Ok(*one),
        [] => Err(RdyError::new(
            E_NOT_FOUND,
            format!("no landing in canon's history matches {text:?}"),
            "list landings with rdy log",
        )),
        many => Err(RdyError::new(
            E_NOT_FOUND,
            format!(
                "{text:?} matches {} landings: {}",
                many.len(),
                many.iter()
                    .map(|h| h.short())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            "give more characters of the landing id",
        )),
    }
}

/// One `tag/*` ref checked by the rules every store keeps (fsck): its value is a 32-byte hash
/// naming a stored tag of the ref's own name, and that name is a valid version. Returns the
/// version and the tag, or the problem in fsck's words.
fn check_tag_ref(
    reader: &rdy_store::StoreReader,
    refname: &str,
    value: &[u8],
) -> Result<(semver::Version, Tag), String> {
    let Ok(bytes) = <[u8; 32]>::try_from(value) else {
        return Err(format!("{refname}: the ref is not a 32-byte hash"));
    };
    let h = Hash(bytes);
    let tag = match reader.read_tag(&h) {
        Ok(t) => t,
        Err(rdy_store::StoreError::NotFound(_)) => {
            return Err(format!(
                "{refname} names {}, which is not in the store",
                h.short()
            ));
        }
        Err(e) => {
            return Err(format!(
                "{refname} names {}, which is not a tag: {e}",
                h.short()
            ));
        }
    };
    if refname["tag/".len()..] != tag.name {
        return Err(format!("{refname} holds the tag {:?}", tag.name));
    }
    let Ok(v) = parse_version(&tag.name) else {
        return Err(format!("{refname}: {:?} is not a valid version", tag.name));
    };
    Ok((v, tag))
}

/// Whether two adjacent versions of a precedence-sorted list denote the same version; if so,
/// fsck's problem for them.
fn same_version(lo: (&semver::Version, &str), hi: (&semver::Version, &str)) -> Option<String> {
    (lo.0.cmp_precedence(hi.0) == Ordering::Equal)
        .then(|| format!("{} and {} denote the same version", lo.1, hi.1))
}

/// fsck's result for the tag problems `problems` among `good` good tags.
fn tag_check_result(problems: Vec<String>, good: usize) -> Result<usize, RdyError> {
    if problems.is_empty() {
        return Ok(good);
    }
    Err(RdyError::new(
        E_IO,
        format!(
            "{} version problem(s): {}",
            problems.len(),
            problems.join("; ")
        ),
        "a tag ref is damaged or was written outside rdy tag: restore the store from a backup",
    ))
}

impl Rdy {
    /// Checks every `tag/*` ref (see FsckReport); `chain` is canon's landings, head first.
    pub(crate) fn check_tags(&self, chain: &[Hash]) -> Result<usize, RdyError> {
        let mut problems = vec![];
        let mut good: Vec<(semver::Version, String, usize)> = vec![];
        for (refname, value) in self.store.refs_with_prefix("tag/")? {
            let (v, tag) = match check_tag_ref(&self.store, &refname, &value) {
                Ok(t) => t,
                Err(p) => {
                    problems.push(p);
                    continue;
                }
            };
            let Some(pos) = chain.iter().position(|c| *c == tag.landing) else {
                problems.push(format!(
                    "{refname}: landing {} is not in canon's chain",
                    tag.landing.short()
                ));
                continue;
            };
            good.push((v, refname, pos));
        }
        good.sort_by(|a, b| a.0.cmp_precedence(&b.0));
        for pair in good.windows(2) {
            let ((lo, lo_ref, lo_pos), (hi, hi_ref, hi_pos)) = (&pair[0], &pair[1]);
            if let Some(p) = same_version((lo, lo_ref), (hi, hi_ref)) {
                problems.push(p);
            } else if hi_pos > lo_pos {
                problems.push(format!("{hi_ref} names an earlier landing than {lo_ref}"));
            }
        }
        tag_check_result(problems, good.len())
    }

    /// A holder's `check_tags`: the same rules for each ref and for versions that denote the
    /// same version, with the same texts. A holder has no canon and its tags name divergent
    /// histories, so a tag's landing is not looked up in a chain and versions keep no
    /// chronological order. Each tag's history is checked by `StoreReader::check_holder`.
    pub(crate) fn check_holder_tags(&self) -> Result<usize, RdyError> {
        let mut problems = vec![];
        let mut good: Vec<(semver::Version, String)> = vec![];
        for (refname, value) in self.store.refs_with_prefix("tag/")? {
            match check_tag_ref(&self.store, &refname, &value) {
                Ok((v, _)) => good.push((v, refname)),
                Err(p) => problems.push(p),
            }
        }
        good.sort_by(|a, b| a.0.cmp_precedence(&b.0));
        for pair in good.windows(2) {
            let ((lo, lo_ref), (hi, hi_ref)) = (&pair[0], &pair[1]);
            problems.extend(same_version((lo, lo_ref), (hi, hi_ref)));
        }
        tag_check_result(problems, good.len())
    }

    /// Every version as (parsed version, tag), in SemVer precedence order, read from the store's
    /// refs: what `publish` captures into a state. Tag refs whose name is
    /// not a version, and refs that cannot be read as a tag, are skipped here so one damaged ref
    /// does not fail every read; `rdy fsck` reports them. Reads go through `ReadState`.
    pub(crate) fn version_tags(&self) -> Result<Vec<(semver::Version, Tag)>, RdyError> {
        let mut out: Vec<(semver::Version, Tag)> = self
            .store
            .refs_with_prefix("tag/")?
            .into_iter()
            .filter_map(|(_, value)| {
                let bytes = <[u8; 32]>::try_from(value.as_slice()).ok()?;
                let t = self.store.read_tag(&Hash(bytes)).ok()?;
                parse_version(&t.name).ok().map(|v| (v, t))
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp_precedence(&b.0));
        Ok(out)
    }

    /// Names a landing (default: canon's head) as version `name`. Approver only; the version must
    /// be new, above every existing version, and on the current version's landing or a later one.
    pub fn tag(
        &mut self,
        s: &Session,
        name: &str,
        landing: Option<&str>,
        message: Option<String>,
    ) -> Result<VersionInfo, RdyError> {
        let tag = self.new_tag(s, name, landing, message)?;
        self.store.put_tag(&tag)?;
        self.publish(crate::Delta::Tags)?;
        self.read().version_info(&tag)
    }

    /// `tag` under the relay table (`answer::relay::relayed`, D-HA7YZ, D-KQ4CA). `person` is the
    /// project's human actor, from the caller (O18); `implicit` is true over MCP, where the
    /// session is the relay.
    /// - A human session with no relay tags as itself, as `tag` does: no `ANSWER`.
    /// - A relayed tag runs as the person: `tagged_by` is the person, and every rule of `tag`
    ///   holds. The `TAG` is encoded as before (D-F0904); an `ANSWER` (act `tag`, item
    ///   `tag:<name>`, `targets` the tag, evidence `text`) holds the relay. Both are one batch
    ///   (O14, D-DGCH7).
    /// - Any other case is the table's refusal, in the answer's words (O19).
    #[allow(clippy::too_many_arguments)]
    pub fn tag_as(
        &mut self,
        s: &Session,
        person: &str,
        relay: &RelayInput,
        implicit: bool,
        name: &str,
        landing: Option<&str>,
        message: Option<String>,
    ) -> Result<TagInfo, RdyError> {
        let prepared = self.prepare_tag_as(s, person, relay, implicit, name, landing, message)?;
        self.commit_tag_as(prepared, &[])
    }

    /// Every rule of `tag_as` and the tag (with its relay's `ANSWER`) it would write, under the
    /// writer and without writing anything. [`Rdy::commit_tag_as`] writes it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prepare_tag_as(
        &self,
        s: &Session,
        person: &str,
        relay: &RelayInput,
        implicit: bool,
        name: &str,
        landing: Option<&str>,
        message: Option<String>,
    ) -> Result<PreparedTagAct, RdyError> {
        let (who, relay) = relayed(s, person, relay, implicit).map_err(|e| e.error())?;
        let Some(relay) = relay else {
            let s = Session {
                role: Role::Approver,
                ..s.clone()
            };
            let tag = self.new_tag(&s, name, landing, message)?;
            let store = self.store.prepare_tag(&tag, None)?;
            return Ok(PreparedTagAct {
                store,
                relayed_by: None,
            });
        };
        let tag = self.new_tag(&who, name, landing, message)?;
        let a = Answer {
            item: format!("tag:{}", tag.name),
            act: AnswerAct::Tag,
            option: None,
            label: None,
            options: vec![],
            card_hash: None,
            targets: vec![],
            person: tag.tagged_by.clone(),
            relayed_by: Some(relay.by.clone()),
            relay_evidence: Some(relay.evidence),
            evidence_kind: Some(EvidenceKind::Text),
            note: None,
        };
        let store = self
            .store
            .prepare_tag(&tag, Some(a))
            .map_err(answer_write_error)?;
        Ok(PreparedTagAct {
            store,
            relayed_by: Some(relay.by),
        })
    }

    /// Writes the tag `prepared` holds, with `signatures` of it in the same batch after the tag
    /// and its answer, and publishes it.
    pub(crate) fn commit_tag_as(
        &mut self,
        prepared: PreparedTagAct,
        signatures: &[rdy_model::signature::Signature],
    ) -> Result<TagInfo, RdyError> {
        let tag = prepared.store.tag().clone();
        let relayed = prepared.relayed_by.is_some();
        let (_, answer) = self
            .store
            .commit_tag(prepared.store, signatures)
            .map_err(|e| {
                if relayed {
                    answer_write_error(e)
                } else {
                    e.into()
                }
            })?;
        self.publish(crate::Delta::Tags)?;
        Ok(TagInfo {
            version: self.read().version_info(&tag)?,
            relayed_by: prepared.relayed_by,
            answer: answer.map(|a| a.to_string()),
        })
    }

    /// The tag `s` would write as version `name` on `landing` (default: canon's head), after
    /// every rule of `tag`.
    fn new_tag(
        &self,
        s: &Session,
        name: &str,
        landing: Option<&str>,
        message: Option<String>,
    ) -> Result<Tag, RdyError> {
        if !s.role.can_land() {
            return Err(RdyError::new(
                E_PERMISSION,
                format!("role {:?} cannot create versions", s.role),
                "a version is a release statement: an approver creates it with rdy tag",
            ));
        }
        new_version_name_ok(name)?;
        let version = parse_version(name)?;
        let state = self.read();
        let chain: Vec<Hash> = state.landings()?.into_iter().map(|(h, _)| h).collect();
        let head = *chain.first().ok_or_else(|| {
            RdyError::new(
                E_NOT_FOUND,
                "canon has no landings to version",
                "land a proposal first",
            )
        })?;
        let target = match landing {
            Some(text) => match_prefix(&chain, text)?,
            None => head,
        };
        let existing = state.version_tags()?;
        if let Some((_, t)) = existing
            .iter()
            .find(|(v, _)| v.cmp_precedence(&version) == Ordering::Equal)
        {
            return Err(RdyError::new(
                E_TAG_EXISTS,
                format!(
                    "version {name} already exists as {} (landing {})",
                    t.name,
                    t.landing.short()
                ),
                "versions are immutable: choose the next version number",
            ));
        }
        if let Some((current, t)) = existing.last() {
            let refuse = |why: &str| {
                RdyError::new(
                    E_VERSION_ORDER,
                    format!(
                        "{name} {why}: the current version is {} at landing {}",
                        t.name,
                        t.landing.short()
                    ),
                    format!(
                        "name a version above {} on landing {} or a later one",
                        t.name,
                        t.landing.short()
                    ),
                )
            };
            if version.cmp_precedence(current) != Ordering::Greater {
                return Err(refuse("is not above the current version"));
            }
            // chain is head first: a smaller index is a later landing
            let pos = |h: &Hash| chain.iter().position(|c| c == h);
            match (pos(&target), pos(&t.landing)) {
                (Some(new), Some(cur)) if new <= cur => {}
                _ => return Err(refuse("names an earlier landing than the current version")),
            }
        }
        Ok(Tag {
            name: name.to_string(),
            landing: target,
            message,
            tagged_by: s.provenance(),
        })
    }
}

/// A tag prepared under the writer by [`Rdy::prepare_tag_as`] and not yet written: the store's
/// prepared batch and, for a relayed tag, the agent that relayed it.
pub(crate) struct PreparedTagAct {
    pub(crate) store: rdy_store::PreparedTag,
    pub(crate) relayed_by: Option<ActorRun>,
}

/// A store error from a write with an `ANSWER`. A refusal means rdy built an answer the store's
/// rules reject: a bug, not bad input.
pub(crate) fn answer_write_error(e: StoreError) -> RdyError {
    match e {
        StoreError::Refused(message) => {
            RdyError::new(E_INTERNAL, message, "report this as a bug in rdy")
        }
        e => e.into(),
    }
}

impl ReadState {
    /// Every version as (parsed version, tag), in SemVer precedence order: the tags this state
    /// was published with.
    pub(crate) fn version_tags(&self) -> Result<Vec<(semver::Version, Tag)>, RdyError> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        Ok(self.tags.as_ref().clone())
    }

    /// Canon's landings from this state's head, head first.
    pub fn landings(&self) -> Result<Vec<(Hash, LandingRecord)>, RdyError> {
        self.landing_walk().collect()
    }

    /// `landings`, each record read only when the walk reaches it; the walk ends after an error.
    pub(crate) fn landing_walk(
        &self,
    ) -> impl Iterator<Item = Result<(Hash, LandingRecord), RdyError>> + '_ {
        let mut failed = self.error.clone();
        let mut cur = self.canon_head;
        std::iter::from_fn(move || {
            if let Some(e) = failed.take() {
                cur = None;
                return Some(Err(e));
            }
            let h = cur.take()?;
            Some(match self.reader.landing(&h) {
                Ok(rec) => {
                    cur = rec.prev;
                    Ok((h, rec))
                }
                Err(e) => Err(e.into()),
            })
        })
    }

    /// Every version, oldest first (precedence order, which the order rules make canon order).
    /// A version whose history holds removed content carries `redacted` and `redactions`.
    pub fn versions(&self) -> Result<Vec<VersionInfo>, RdyError> {
        let tags = self.version_tags()?;
        let marks = self.tag_marks(&tags)?;
        Ok(tags
            .iter()
            .zip(marks.iter())
            .map(|((_, t), m)| marked(t, m))
            .collect())
    }

    /// The version `tag` as `versions` lists it, after a publish that wrote it.
    fn version_info(&self, tag: &Tag) -> Result<VersionInfo, RdyError> {
        let id = tag.id().to_string();
        Ok(self
            .versions()?
            .into_iter()
            .find(|v| v.tag == id)
            .unwrap_or_else(|| info(tag)))
    }

    /// For each of `tags` (in order), the requests that removed content its history holds,
    /// ascending and without repeats: the union of `landing_requests` over the tag's landing and
    /// every landing before it. Read once per canon head, tag count, redaction epoch and pack,
    /// then kept in the cache every state shares; each landing's own requests are kept per epoch
    /// and pack, so a new landing reads only its own changes.
    fn tag_marks(&self, tags: &[(semver::Version, Tag)]) -> Result<Arc<Vec<Vec<Hash>>>, RdyError> {
        if tags.is_empty() {
            return Ok(Arc::new(vec![]));
        }
        let epoch = self.reader.captured_redaction_epoch();
        let key = (self.canon_head, tags.len(), epoch);
        let lock = || self.vpos.lock().unwrap_or_else(|e| e.into_inner());
        let known = {
            let cache = lock();
            if let Some((k, p, marks)) = &cache.marks
                && *k == key
                && p.is(&self.pack)
            {
                return Ok(Arc::clone(marks));
            }
            match &cache.landings {
                Some((e, p, known)) if *e == epoch && p.is(&self.pack) => Arc::clone(known),
                _ => Arc::new(HashMap::new()),
            }
        };
        // canon's landings, head first; a landing not read in this epoch yet reads its changes
        let mut chain: Vec<Hash> = vec![];
        let mut fresh: HashMap<Hash, Vec<Hash>> = HashMap::new();
        let mut blobs: HashMap<Hash, Vec<Hash>> = HashMap::new();
        for landing in self.landing_walk() {
            let (h, rec) = landing?;
            if !known.contains_key(&h) && !fresh.contains_key(&h) {
                fresh.insert(h, self.landing_requests(&rec, &mut blobs)?);
            }
            chain.push(h);
        }
        // oldest first, the requests found so far, kept at each tagged landing
        let tagged: HashSet<Hash> = tags.iter().map(|(_, t)| t.landing).collect();
        let mut at: HashMap<Hash, Vec<Hash>> = HashMap::new();
        let mut seen: BTreeSet<Hash> = BTreeSet::new();
        for h in chain.iter().rev() {
            if let Some(found) = known.get(h).or_else(|| fresh.get(h)) {
                seen.extend(found.iter().copied());
            }
            if tagged.contains(h) {
                at.insert(*h, seen.iter().copied().collect());
            }
        }
        let marks: Arc<Vec<Vec<Hash>>> = Arc::new(
            tags.iter()
                .map(|(_, t)| at.get(&t.landing).cloned().unwrap_or_default())
                .collect(),
        );
        let mut cache = lock();
        let pack = PackId(Arc::clone(&self.pack));
        if !fresh.is_empty() {
            let mut all = (*known).clone();
            all.extend(fresh);
            cache.landings = Some((epoch, pack.clone(), Arc::new(all)));
        }
        cache.marks = Some((key, pack, Arc::clone(&marks)));
        Ok(marks)
    }

    /// The requests whose applied masks remove something landing `rec` carried, ascending and
    /// without repeats. For each change: the request that holds the change itself; the request
    /// named by each block version it writes, read through the store's projection (a removed
    /// version reads as its stub, so a sibling removed later by another request is found too);
    /// and, for a written version whose type has the pack's `asset` trait, the requests that hold
    /// the manifest or a chunk of the blob its `blob` field names (on any other type that field
    /// is plain text). `blobs` keeps each blob's requests across the landings of one read.
    fn landing_requests(
        &self,
        rec: &LandingRecord,
        blobs: &mut HashMap<Hash, Vec<Hash>>,
    ) -> Result<Vec<Hash>, RdyError> {
        let mut found: BTreeSet<Hash> = BTreeSet::new();
        for c in &rec.changes {
            if let Some(request) = self.reader.redaction_of(kinds::CHANGE, c)? {
                found.insert(request);
            }
            let change = match self.reader.change(c) {
                Ok(change) => change,
                // a removed change that no residue reads: its request is the whole answer
                Err(StoreError::Redacted { request }) => {
                    found.insert(request);
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            for op in &change.ops {
                let (Op::Create { block } | Op::Update { block, .. }) = op else {
                    continue;
                };
                if let Some(request) = block.redaction() {
                    found.insert(request);
                }
                // only an asset's `blob` field names a blob; on any other type it is plain text
                if !self.pack.is_asset(&block.block_type) {
                    continue;
                }
                let Some(h) = block.field_str("blob").and_then(|s| s.parse::<Hash>().ok()) else {
                    continue;
                };
                let requests = match blobs.entry(h) {
                    Entry::Occupied(e) => e.into_mut(),
                    Entry::Vacant(e) => e.insert(self.blob_requests(&h)?),
                };
                found.extend(requests.iter().copied());
            }
        }
        Ok(found.into_iter().collect())
    }

    /// The requests whose applied masks hold blob `h`'s manifest or any of its chunks, ascending
    /// and without repeats; none when `h` names no blob.
    fn blob_requests(&self, h: &Hash) -> Result<Vec<Hash>, RdyError> {
        let mut found: BTreeSet<Hash> = BTreeSet::new();
        if let Some(request) = self.reader.redaction_of(kinds::BLOB_MANIFEST, h)? {
            found.insert(request);
        }
        let manifest: BlobManifest = match self.reader.get_typed(kinds::BLOB_MANIFEST, h) {
            Ok(m) => m,
            // not a blob, or a selected manifest (its request is already found)
            Err(StoreError::NotFound(_) | StoreError::Redacted { .. }) => {
                return Ok(found.into_iter().collect());
            }
            Err(e) => return Err(e.into()),
        };
        for c in &manifest.chunks {
            if let Some(request) = self.reader.redaction_of(kinds::BLOB_CHUNK, c)? {
                found.insert(request);
            }
        }
        Ok(found.into_iter().collect())
    }

    /// A landing in canon's chain, by full hash or unique prefix of at least 6 hex characters.
    /// In a holder, which has no canon, a landing in the chain of one of its tags.
    pub fn resolve_landing(&self, text: &str) -> Result<Hash, RdyError> {
        if self.canon_head.is_none() && self.reader.mode()? == StoreMode::Holder {
            return match_prefix(&self.tag_landings()?, text);
        }
        let chain: Vec<Hash> = self.landings()?.into_iter().map(|(h, _)| h).collect();
        match_prefix(&chain, text)
    }

    /// The distinct landings of every tag's chain, in hash order: a holder's landings.
    /// The chains are walked together, so each landing is read once.
    pub(crate) fn tag_landings(&self) -> Result<Vec<Hash>, RdyError> {
        let heads: Vec<Hash> = self
            .version_tags()?
            .into_iter()
            .map(|(_, t)| t.landing)
            .collect();
        self.reader
            .tag_histories(&heads)
            .map_err(crate::holder::holder_error)
    }

    /// scope1: like `resolve_scope`, for reads. A view must have a record (it was written,
    /// proposed or landed), unless it is `own_view`, the reading session's view, which is
    /// created lazily. Reading a view that doesn't exist would silently read canon.
    pub fn resolve_read_scope(
        &self,
        text: &str,
        own_view: Option<&str>,
    ) -> Result<Scope, RdyError> {
        let scope = self.resolve_scope(text)?;
        if let Scope::View(v) = &scope
            && Some(v.as_str()) != own_view
            && !self.has_view(v)
        {
            let hex = (7..=64).contains(&v.len()) && v.bytes().all(|b| b.is_ascii_hexdigit());
            let fix = if hex {
                format!("{v:?} looks like a landing id: read it with landing:{v}")
            } else {
                "list views with rdy status (MCP: rdy_overview), or read canon".to_string()
            };
            return Err(RdyError::new(E_NOT_FOUND, format!("no view {v:?}"), fix));
        }
        Ok(scope)
    }

    /// Reads scope text: `canon`, `tag:<version>`, `landing:<id>`, `proposal:P-x@n` (also
    /// `proposal:P-x`, the latest version, or `proposal:<version hash>`), `view:<name>`, or a
    /// view name.
    pub fn resolve_scope(&self, text: &str) -> Result<Scope, RdyError> {
        // a holder has no canon: its scopes are tags and landings only
        if !text.starts_with("tag:") && !text.starts_with("landing:") {
            self.refuse_holder_canon()?;
        }
        if text == "canon" {
            return Ok(Scope::Canon);
        }
        // insc1: exactly `inflight` is the in-flight scope (`view:inflight` is a view name,
        // which writes refuse)
        if text == crate::session::INFLIGHT_SCOPE {
            return Ok(Scope::Inflight);
        }
        if let Some(name) = text.strip_prefix("tag:") {
            let want = parse_version(name)?;
            return self
                .version_tags()?
                .into_iter()
                .find(|(v, _)| v.cmp_precedence(&want) == Ordering::Equal)
                .map(|(_, t)| Scope::At(t.landing))
                .ok_or_else(|| {
                    RdyError::new(
                        E_NOT_FOUND,
                        format!("no version {name}"),
                        "list versions with rdy tags",
                    )
                });
        }
        if let Some(id) = text.strip_prefix("landing:") {
            return Ok(Scope::At(self.resolve_landing(id)?));
        }
        // ls8: a proposal version reads as its base plus its changes
        if let Some(r) = text.strip_prefix("proposal:") {
            return Ok(Scope::Proposal(crate::gate::resolve_in(self, r)?.version));
        }
        let view = text.strip_prefix("view:").unwrap_or(text);
        if view.is_empty() {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "a view name is empty",
                "name a view, e.g. view:work",
            ));
        }
        Ok(Scope::View(view.to_string()))
    }

    /// The version position of `scope` (see `VersionPosition`). Cached in the cache every state
    /// shares, under a key that holds every input (this state's canon head and tag count, and
    /// for a view its head op), so no entry is ever stale and a held state keeps its answer.
    pub fn version_position(&self, scope: &Scope) -> Result<VersionPosition, RdyError> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        if !matches!(scope, Scope::At(_)) {
            self.refuse_holder_canon()?;
        }
        let (key, head) = match scope {
            // the in-flight state stands where canon stands
            Scope::Inflight => return self.version_position(&Scope::Canon),
            Scope::Canon => (VposScope::Canon, None),
            Scope::At(h) => (VposScope::At(*h), None),
            Scope::Proposal(v) => (VposScope::Proposal(*v), None),
            Scope::View(name) => {
                let head = self.view_head(name).cloned();
                (
                    VposScope::View(name.clone(), head.as_ref().map(|h| h.head)),
                    head,
                )
            }
        };
        let key = (self.canon_head, self.tags.len(), key);
        let lock = || self.vpos.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(hit) = lock().entries.get(&key) {
            return Ok(hit.clone());
        }
        let computed = self.compute_version_position(scope, head)?;
        let mut cache = lock();
        if cache.entries.len() >= VPOS_CACHE_ENTRIES {
            cache.entries.clear();
        }
        cache.entries.insert(key, computed.clone());
        Ok(computed)
    }

    /// Test hook: how many positions are cached (the cache is shared by every state).
    #[doc(hidden)]
    pub fn version_position_cache_len(&self) -> usize {
        self.vpos
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .len()
    }

    fn compute_version_position(
        &self,
        scope: &Scope,
        head: Option<rdy_store::ViewHead>,
    ) -> Result<VersionPosition, RdyError> {
        let canon = self.canon_head;
        let (base, view) = match scope {
            Scope::Canon | Scope::Inflight => (canon, None),
            Scope::At(h) => (Some(*h), None),
            // ls8: a proposal version stands where its base stands, and reads as a draft named
            // by its title (no proposal ref or hash reaches a page)
            Scope::Proposal(v) => {
                let p = self.proposal_version(v)?;
                let pos = ViewPosition {
                    name: p.title,
                    unlanded_changes: p.changes.len() as u32,
                };
                (p.canon_at, Some(pos))
            }
            Scope::View(name) => {
                let pos = ViewPosition {
                    name: name.clone(),
                    unlanded_changes: head.as_ref().map_or(0, |h| h.len as u32),
                };
                // a floating view stands where canon stands
                (self.effective_base(name), Some(pos))
            }
        };
        // the highest version on each landing (version_tags is in ascending order)
        let mut on: HashMap<Hash, String> = HashMap::new();
        for (_, t) in self.tags.iter() {
            on.insert(t.landing, t.name.clone());
        }
        let (mut since, mut cur, mut version) = (0u32, base, None);
        while let Some(h) = cur {
            if let Some(name) = on.get(&h) {
                version = Some(VersionRef {
                    name: name.clone(),
                    landing: h.to_string(),
                });
                break;
            }
            since += 1;
            cur = self.reader.landing(&h)?.prev;
        }
        let frozen = matches!(scope, Scope::At(_));
        Ok(VersionPosition {
            label: label_of(&version, since, &view, frozen),
            version,
            landings_since: since,
            view,
            frozen,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ruling final: the one bound is for new names and the site; a stored name reads at any
    /// length.
    #[test]
    fn sr2_a_version_name_has_one_bound() {
        let at = format!("v1.0.0-{}", "a".repeat(MAX_VERSION_BYTES - 7));
        assert_eq!(at.len(), MAX_VERSION_BYTES);
        assert!(parse_version(&at).is_ok());
        assert!(new_version_name_ok(&at).is_ok());
        assert!(crate::site_scope::tag_name_ok(&at));
        let over = format!("{at}a");
        assert!(parse_version(&over).is_ok(), "a stored name has no bound");
        let e = new_version_name_ok(&over).unwrap_err();
        assert_eq!(e.code, E_BAD_INPUT);
        assert_eq!(
            e.message,
            "the version name is 257 bytes; a new version name has at most 256"
        );
        assert_eq!(e.fix, "use a shorter name");
        assert!(!crate::site_scope::tag_name_ok(&over));
    }

    #[test]
    fn an_ambiguous_prefix_lists_the_candidates() {
        let mut a = [0xab; 32];
        let mut b = [0xab; 32];
        a[31] = 1;
        b[31] = 2;
        let e = match_prefix(&[Hash(a), Hash(b)], "abababab").unwrap_err();
        assert_eq!(e.code, E_NOT_FOUND);
        assert!(e.message.contains("matches 2 landings"), "{e}");
        assert_eq!(
            match_prefix(&[Hash(a), Hash(b)], &Hash(a).to_string()).unwrap(),
            Hash(a)
        );
    }
}
