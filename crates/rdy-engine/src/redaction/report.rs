//! The complete blast radius of a redaction (D-6B1W1, D-14SAY, D-EPK1Z, D-33JR0).
//!
//! [`compute_report`] reads the log, never only current blocks: every stored block version
//! (closed views and withdrawn proposals included), every change that embeds a selected version,
//! every chunk of a selected blob and every blob that shares one. It names every landing, tag,
//! view, proposal, state snapshot, review, tension and answer that references affected content,
//! and every record that quotes a complete source string of a selected version. Quoted text is
//! returned in the transient quote rows only; the manifest a request binds holds record
//! identities, never the text.
use super::scan::{require_text_policies, visit_strings, walk};
use super::targets::{Checked, check_input, selector_text};
use super::{Limits, RedactInput, limits};
use crate::ReadState;
use crate::error::{
    E_BAD_INPUT, REDACTION_CAUSE_METADATA, REDACTION_CAUSE_NOT_BLOB, REDACTION_CAUSE_OTHER_BLOCK,
    REDACTION_CAUSE_UNKNOWN, RdyError, W_REDACTION_COPIES, W_REDACTION_COPIES_MESSAGE,
    W_REDACTION_INVENTORY, W_REDACTION_INVENTORY_MESSAGE, W_REDACTION_QUARANTINE,
    W_REDACTION_QUARANTINE_MESSAGE, W_REDACTION_QUOTES, W_REDACTION_QUOTES_MESSAGE,
    W_REDACTION_REUSE, W_REDACTION_REUSE_MESSAGE, W_REDACTION_SECRET, W_REDACTION_SECRET_MESSAGE,
    redaction_limit, redaction_target,
};
use rdy_model::redaction::{
    AssessmentSupport, BlockResidue, ChangeResidue, CopyKind, CopyRow, CopyStatus, DropSpan,
    FramePlace, ImpactKind, ImpactRelation, ImpactRow, ReasonClass, RedactTarget,
    RedactionManifest, RedactionResidue, RedactionWarning, encode_into,
};
use rdy_model::{
    AltSet, Answer, Block, BlockId, FieldValue, Hash, Intent, LandingRecord, Op, Proposal,
    Redaction, Review, ReviewSubject, SemanticTension, StateSnapshot, Tag,
};
use rdy_store::kinds::{self, BlobManifest, RefUpdate};
use rdy_store::redaction::limit;
use rdy_store::transition::Transitions;
use rdy_store::{StoreReader, ViewOp, ViewOpKind};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// The complete report of a redaction: what it selects and removes, everything inside the store
/// it affects, the transient quote rows, the six fixed outside-copy rows, the quarantine names,
/// the assessment support and the warnings. `report_hash` is the hash of the manifest a request
/// binds. Every list is in canonical order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedactionReport {
    #[serde(with = "rdy_store::transition::hex::one")]
    pub report_hash: Hash,
    pub targets: Vec<RedactTarget>,
    pub already_redacted: Vec<RedactTarget>,
    #[serde(with = "hex::pairs")]
    pub versions: Vec<(BlockId, Hash)>,
    #[serde(with = "hex::vec")]
    pub blobs: Vec<Hash>,
    #[serde(with = "hex::vec")]
    pub shared_blobs: Vec<Hash>,
    pub drop_frames: u64,
    pub drop_bytes: u64,
    pub inside: Vec<ImpactRow>,
    pub quotes: Vec<QuoteRow>,
    pub outside: Vec<CopyRow>,
    pub quarantine: Vec<String>,
    pub assessment: AssessmentSupport,
    pub warnings: Vec<RedactionWarning>,
}

/// One record that quotes a complete source string: the record's kind and identity, the quoted
/// string, and whether selecting that record could remove it. A transient reply row: it is never
/// written into a manifest, a residue, a card or any other stored record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuoteRow {
    pub kind: ImpactKind,
    pub record: String,
    pub quote: String,
    pub erasable: bool,
}

/// The names of the copies outside the log that the report lists: the quarantine names, as
/// `rdy_engine::quarantine::names` returns them (sorted and deduplicated again here).
#[derive(Clone, Copy, Debug)]
pub struct ReportCopies<'a> {
    pub quarantine: &'a [String],
}

/// The complete report of `input` over `state`, with `copies` for the quarantine names. Reads
/// the store and changes nothing.
pub fn compute_report(
    state: &ReadState,
    input: &RedactInput,
    copies: ReportCopies<'_>,
) -> Result<RedactionReport, RdyError> {
    Ok(compute(state, input, copies.quarantine)?.report)
}

impl ReadState {
    /// The complete report of `input` over this state: `compute_report` with the project's
    /// current quarantine names, listed once.
    pub fn redaction_report(&self, input: &RedactInput) -> Result<RedactionReport, RdyError> {
        let names = crate::quarantine::names(self.root());
        compute_report(self, input, ReportCopies { quarantine: &names })
    }
}

/// The six fixed outside-copy rows, in `CopyKind` order: every location unknown (D-14SAY).
pub fn outside_rows() -> Vec<CopyRow> {
    [
        CopyKind::Render,
        CopyKind::Export,
        CopyKind::Dump,
        CopyKind::Clone,
        CopyKind::Published,
        CopyKind::WorkingCopy,
    ]
    .into_iter()
    .map(|kind| CopyRow {
        kind,
        status: CopyStatus::Unknown,
    })
    .collect()
}

/// The warnings of a report, in the fixed order: copies, inventory, quotes, reuse; quarantine
/// when the list is not empty; secret for the secret class.
pub fn warnings(class: ReasonClass, quarantine: bool) -> Vec<RedactionWarning> {
    let w = |code: &str, message: &str| RedactionWarning {
        code: code.to_string(),
        message: message.to_string(),
        fix: String::new(),
    };
    let mut out = vec![
        w(W_REDACTION_COPIES, W_REDACTION_COPIES_MESSAGE),
        w(W_REDACTION_INVENTORY, W_REDACTION_INVENTORY_MESSAGE),
        w(W_REDACTION_QUOTES, W_REDACTION_QUOTES_MESSAGE),
        w(W_REDACTION_REUSE, W_REDACTION_REUSE_MESSAGE),
    ];
    if quarantine {
        out.push(w(W_REDACTION_QUARANTINE, W_REDACTION_QUARANTINE_MESSAGE));
    }
    if class == ReasonClass::Secret {
        out.push(w(W_REDACTION_SECRET, W_REDACTION_SECRET_MESSAGE));
    }
    out
}

/// A computed report with what a request records beside it.
#[derive(Clone, Debug)]
pub(crate) struct Full {
    pub(crate) report: RedactionReport,
    pub(crate) manifest: RedactionManifest,
    pub(crate) residues: Vec<RedactionResidue>,
    /// The targets not already wholly removed: the request's own targets.
    pub(crate) request_targets: Vec<RedactTarget>,
    pub(crate) class: ReasonClass,
    pub(crate) reason: String,
}

/// The `E_BAD_INPUT` of a cycle in a history the store keeps acyclic.
fn cycle() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "The redaction reference graph has a cycle.",
        "Keep the store unchanged and inspect it with rdy fsck.",
    )
}

fn over(l: &'static str) -> RdyError {
    redaction_limit(l)
}

/// The id-and-version record of a block version.
fn version_record(id: &BlockId, v: &Hash) -> String {
    format!("{id}@{v}")
}

/// The deepest nesting of list and map containers in `v` (a scalar is 0).
fn field_depth(v: &FieldValue) -> usize {
    match v {
        FieldValue::List(items) => 1 + items.iter().map(field_depth).max().unwrap_or(0),
        FieldValue::Map(m) => 1 + m.values().map(field_depth).max().unwrap_or(0),
        _ => 0,
    }
}

/// Refuses a block with a field nested deeper than `limits.levels`.
fn check_levels(b: &Block, limits: &Limits) -> Result<(), RdyError> {
    if b.fields.values().any(|v| field_depth(v) > limits.levels) {
        return Err(over(limit::LEVELS));
    }
    Ok(())
}

/// Every non-empty string leaf of `v`.
fn string_leaves<'a>(v: &'a FieldValue, out: &mut Vec<&'a str>) {
    match v {
        FieldValue::Str(s) if !s.is_empty() => out.push(s),
        FieldValue::List(items) => items.iter().for_each(|i| string_leaves(i, out)),
        FieldValue::Map(m) => m.values().for_each(|i| string_leaves(i, out)),
        _ => {}
    }
}

/// What the first pass knows of one change: its deps, the versions its ops produce and the
/// versions its ops name as bases.
#[derive(Debug, Default)]
struct ChangeInfo {
    deps: Vec<Hash>,
    produces: Vec<Hash>,
    bases: Vec<Hash>,
}

/// The quote matcher: the sorted source strings and the matches found, by record.
struct Quotes<'a> {
    sources: &'a [String],
    limits: Limits,
    found: BTreeMap<(u8, String, bool), BTreeSet<String>>,
}

impl Quotes<'_> {
    /// Scans every string of `value` for complete source strings; each match is a quote of the
    /// record `(kind, record)`.
    fn scan<T: Serialize + ?Sized>(
        &mut self,
        kind: ImpactKind,
        record: &str,
        erasable: bool,
        value: &T,
    ) -> Result<(), RdyError> {
        if self.sources.is_empty() {
            return Ok(());
        }
        let sources = self.sources;
        let mut hits: Vec<&str> = vec![];
        visit_strings(value, &self.limits, &mut |s: &str| {
            for src in sources {
                if s.contains(src.as_str()) {
                    hits.push(src);
                }
            }
        })?;
        if !hits.is_empty() {
            let set = self
                .found
                .entry((kind as u8, record.to_string(), erasable))
                .or_default();
            set.extend(hits.into_iter().map(str::to_string));
        }
        Ok(())
    }
}

/// The impact kind whose ordinal is `n`.
fn impact_kind(n: u8) -> ImpactKind {
    const ALL: [ImpactKind; 11] = [
        ImpactKind::BlockVersion,
        ImpactKind::Change,
        ImpactKind::Landing,
        ImpactKind::Tag,
        ImpactKind::View,
        ImpactKind::Proposal,
        ImpactKind::StateSnapshot,
        ImpactKind::AuditText,
        ImpactKind::Review,
        ImpactKind::Tension,
        ImpactKind::Answer,
    ];
    ALL[usize::from(n)]
}

/// The changes whose dependency closure holds an affected change: depth-first over `deps`, with
/// a cycle refused.
fn dep_closure(
    changes: &HashMap<Hash, ChangeInfo>,
    affected: &BTreeSet<Hash>,
) -> Result<HashSet<Hash>, RdyError> {
    // 0 unseen, 1 on the stack, 2 done
    let mut state: HashMap<Hash, u8> = HashMap::new();
    let mut hit: HashSet<Hash> = HashSet::new();
    for start in changes.keys() {
        if state.contains_key(start) {
            continue;
        }
        let mut stack: Vec<(Hash, usize)> = vec![(*start, 0)];
        state.insert(*start, 1);
        while let Some((c, i)) = stack.last().copied() {
            let deps = changes.get(&c).map(|ci| ci.deps.as_slice()).unwrap_or(&[]);
            if i < deps.len() {
                stack.last_mut().expect("not empty").1 += 1;
                let d = deps[i];
                match state.get(&d) {
                    Some(1) => return Err(cycle()),
                    Some(_) => {}
                    None => {
                        state.insert(d, 1);
                        stack.push((d, 0));
                    }
                }
            } else {
                stack.pop();
                state.insert(c, 2);
                if affected.contains(&c) || deps.iter().any(|d| hit.contains(d)) {
                    hit.insert(c);
                }
            }
        }
    }
    Ok(hit)
}

/// The landings whose history (themselves and every earlier landing by `prev`) holds an affected
/// change, with a cycle refused.
fn landing_history(
    landings: &BTreeMap<Hash, LandingRecord>,
    direct: &HashSet<Hash>,
) -> Result<HashSet<Hash>, RdyError> {
    let mut known: HashMap<Hash, bool> = HashMap::new();
    for start in landings.keys() {
        let mut chain: Vec<Hash> = vec![];
        let mut on_chain: HashSet<Hash> = HashSet::new();
        let mut cur = Some(*start);
        let mut tail = false;
        while let Some(l) = cur {
            if let Some(k) = known.get(&l) {
                tail = *k;
                break;
            }
            if !on_chain.insert(l) {
                return Err(cycle());
            }
            chain.push(l);
            cur = landings.get(&l).and_then(|r| r.prev);
        }
        for l in chain.into_iter().rev() {
            tail = tail || direct.contains(&l);
            known.insert(l, tail);
        }
    }
    Ok(known
        .into_iter()
        .filter(|(_, v)| *v)
        .map(|(k, _)| k)
        .collect())
}

/// The block ids a review names: its findings and its subject's blocks.
fn review_blocks(r: &Review) -> Vec<&BlockId> {
    let mut out: Vec<&BlockId> = r.findings.iter().map(|f| &f.block).collect();
    match &r.subject {
        ReviewSubject::Tension { blocks, .. } => out.extend(blocks),
        ReviewSubject::Mention { block, entity } => {
            out.push(block);
            out.push(entity);
        }
        ReviewSubject::Pair { items, .. } => out.extend(items.iter().map(|i| &i.block)),
    }
    out
}

/// The changes a view op names.
fn view_op_changes(op: &ViewOp) -> Vec<Hash> {
    match &op.op {
        ViewOpKind::Fork { changes, .. } => changes.clone(),
        ViewOpKind::Commit { change } => vec![*change],
        ViewOpKind::Include { changes } | ViewOpKind::Exclude { changes } => changes.clone(),
        ViewOpKind::Rebase {
            dropped, remaining, ..
        } => dropped.iter().chain(remaining).copied().collect(),
        _ => vec![],
    }
}

/// Checks each normalized target that the index can check alone: a version must be a stored
/// block version of its id; a blob must be a stored blob manifest.
fn check_known(reader: &StoreReader, targets: &[RedactTarget]) -> Result<(), RdyError> {
    for t in targets {
        let name = selector_text(t);
        match t {
            RedactTarget::AllVersions(_) => {}
            RedactTarget::Version { id, version } => {
                if !reader.has_kind(kinds::BLOCK, version)? {
                    return Err(redaction_target(&name, REDACTION_CAUSE_UNKNOWN));
                }
                if reader.get_block(version)?.id != *id {
                    return Err(redaction_target(&name, REDACTION_CAUSE_OTHER_BLOCK));
                }
            }
            RedactTarget::Blob(h) => {
                if reader.has_kind(kinds::BLOB_MANIFEST, h)? {
                    continue;
                }
                let content = [kinds::BLOB_CHUNK, kinds::BLOCK, kinds::CHANGE];
                let mut is_content = false;
                for k in content {
                    is_content |= reader.has_kind(k, h)?;
                }
                let cause = if is_content {
                    REDACTION_CAUSE_NOT_BLOB
                } else if reader.has(h)? {
                    REDACTION_CAUSE_METADATA
                } else {
                    REDACTION_CAUSE_UNKNOWN
                };
                return Err(redaction_target(&name, cause));
            }
        }
    }
    Ok(())
}

/// A zeroed buffer of `n` bytes, allocated fallibly (`E_REDACTION_LIMIT` on failure).
fn buffer(n: usize) -> Result<Vec<u8>, RdyError> {
    let mut buf = Vec::new();
    buf.try_reserve_exact(n).map_err(|_| over(limit::RECORD))?;
    buf.resize(n, 0);
    Ok(buf)
}

/// The report, the manifest it hashes, the residues a request records and the request's own
/// targets. `quarantine` is sorted and deduplicated here.
pub(crate) fn compute(
    state: &ReadState,
    input: &RedactInput,
    quarantine: &[String],
) -> Result<Full, RdyError> {
    if let Some(e) = &state.error {
        return Err(e.clone());
    }
    let Checked {
        targets,
        class,
        reason,
    } = check_input(input)?;
    let limits = limits();
    let reader = state.reader();
    let mut quarantine = quarantine.to_vec();
    quarantine.sort();
    quarantine.dedup();
    check_known(reader, &targets)?;

    // the walk, under the frame and byte bounds
    let frames = walk(reader, state.seq(), &limits)?;
    let present_kinds: BTreeSet<u8> = frames.iter().map(|f| f.kind).collect();
    require_text_policies(&present_kinds.into_iter().collect::<Vec<_>>())?;

    // pass 1: block ids, blob references, changes and blob manifests
    let mut block_ids: HashMap<Hash, BlockId> = HashMap::new();
    let mut by_id: BTreeMap<BlockId, BTreeSet<Hash>> = BTreeMap::new();
    let mut blob_refs: Vec<(Hash, BlockId, Hash)> = vec![];
    let mut changes: HashMap<Hash, ChangeInfo> = HashMap::new();
    let mut manifests: BTreeMap<Hash, Vec<Hash>> = BTreeMap::new();
    for f in &frames {
        match f.kind {
            kinds::BLOCK if !block_ids.contains_key(&f.hash) => {
                let b = reader.get_block(&f.hash)?;
                if let Some(h) = b
                    .fields
                    .get("blob")
                    .and_then(FieldValue::as_str)
                    .and_then(|s| s.parse::<Hash>().ok())
                {
                    blob_refs.push((h, b.id.clone(), f.hash));
                }
                by_id.entry(b.id.clone()).or_default().insert(f.hash);
                block_ids.insert(f.hash, b.id);
            }
            kinds::CHANGE if !changes.contains_key(&f.hash) => {
                let c = reader.change(&f.hash)?;
                let mut info = ChangeInfo {
                    deps: c.deps.clone(),
                    ..ChangeInfo::default()
                };
                for op in &c.ops {
                    match op {
                        Op::Create { block } => info.produces.push(block.version_hash()),
                        Op::Update { base, block } => {
                            info.produces.push(block.version_hash());
                            info.bases.push(*base);
                        }
                        Op::Delete { base, .. } => info.bases.push(*base),
                    }
                }
                changes.insert(f.hash, info);
            }
            kinds::BLOB_MANIFEST if !manifests.contains_key(&f.hash) => {
                let m: BlobManifest = reader.get_typed(kinds::BLOB_MANIFEST, &f.hash)?;
                manifests.insert(f.hash, m.chunks);
            }
            _ => {}
        }
    }

    // the expansion, and what is already wholly removed
    let removed = |kind: u8, h: &Hash| -> Result<bool, RdyError> {
        Ok(reader.redaction_of(kind, h)?.is_some())
    };
    let mut expanded: Vec<(RedactTarget, Vec<(BlockId, Hash)>)> = vec![];
    for t in &targets {
        let versions = match t {
            RedactTarget::AllVersions(id) => match by_id.get(id) {
                Some(vs) => vs.iter().map(|v| (id.clone(), *v)).collect(),
                None => {
                    return Err(redaction_target(&selector_text(t), REDACTION_CAUSE_UNKNOWN));
                }
            },
            RedactTarget::Version { id, version } => vec![(id.clone(), *version)],
            RedactTarget::Blob(_) => vec![],
        };
        expanded.push((t.clone(), versions));
    }
    let mut already: Vec<RedactTarget> = vec![];
    let mut request_targets: Vec<RedactTarget> = vec![];
    for (t, versions) in &expanded {
        let gone = match t {
            RedactTarget::Blob(h) => {
                let chunks = manifests.get(h).map(Vec::as_slice).unwrap_or(&[]);
                if chunks.is_empty() {
                    removed(kinds::BLOB_MANIFEST, h)?
                } else {
                    let mut all = true;
                    for c in chunks {
                        all &= removed(kinds::BLOB_CHUNK, c)?;
                    }
                    all
                }
            }
            _ => {
                let mut all = true;
                for (_, v) in versions {
                    all &= removed(kinds::BLOCK, v)?;
                }
                all
            }
        };
        if gone {
            already.push(t.clone());
        } else {
            request_targets.push(t.clone());
        }
    }
    let mut selected: BTreeSet<(BlockId, Hash)> = BTreeSet::new();
    let mut blobs: BTreeSet<Hash> = BTreeSet::new();
    for (t, versions) in &expanded {
        if !request_targets.contains(t) {
            continue;
        }
        selected.extend(versions.iter().cloned());
        if let RedactTarget::Blob(h) = t {
            blobs.insert(*h);
        }
    }
    if selected
        .len()
        .checked_add(blobs.len())
        .is_none_or(|n| n > limits.selected)
    {
        return Err(over(limit::SELECTED));
    }
    let outside = outside_rows();
    let warnings = warnings(class, !quarantine.is_empty());
    if request_targets.is_empty() {
        // everything selected was removed already: no source strings, nothing to drop
        let manifest = RedactionManifest {
            versions: vec![],
            blobs: vec![],
            shared_blobs: vec![],
            spans: vec![],
            inside: vec![],
            outside: outside.clone(),
            quarantine: quarantine.clone(),
            assessment: AssessmentSupport::NotImplemented,
        };
        return finish(
            Draft {
                targets,
                already,
                manifest,
                quotes: vec![],
                drop_frames: 0,
                drop_bytes: 0,
                warnings,
            },
            vec![],
            request_targets,
            class,
            reason,
            &limits,
        );
    }

    // the selection's closure: chunks, shared blobs, blob references, producing changes
    let versions: HashSet<Hash> = selected.iter().map(|(_, v)| *v).collect();
    let chunks: HashSet<Hash> = blobs
        .iter()
        .flat_map(|b| manifests.get(b).cloned().unwrap_or_default())
        .collect();
    let shared: BTreeSet<Hash> = manifests
        .iter()
        .filter(|(h, cs)| !blobs.contains(h) && cs.iter().any(|c| chunks.contains(c)))
        .map(|(h, _)| *h)
        .collect();
    let referencing: BTreeSet<(BlockId, Hash)> = blob_refs
        .iter()
        .filter(|(b, _, v)| (blobs.contains(b) || shared.contains(b)) && !versions.contains(v))
        .map(|(_, id, v)| (id.clone(), *v))
        .collect();
    let affected_versions: HashSet<Hash> = versions
        .iter()
        .copied()
        .chain(referencing.iter().map(|(_, v)| *v))
        .collect();
    let affected_ids: HashSet<&BlockId> = selected
        .iter()
        .chain(&referencing)
        .map(|(id, _)| id)
        .collect();
    let mut producers: BTreeSet<Hash> = BTreeSet::new();
    for v in &versions {
        producers.extend(reader.producers(v)?);
    }
    let mut affected_changes: BTreeSet<Hash> = producers.clone();
    for (h, c) in &changes {
        if c.produces
            .iter()
            .chain(&c.bases)
            .any(|v| affected_versions.contains(v))
        {
            affected_changes.insert(*h);
        }
    }

    // the spans, and a residue for each block and change span
    let mut buf = buffer(limits.record)?;
    let mut spans: Vec<DropSpan> = vec![];
    let mut residues: Vec<RedactionResidue> = vec![];
    let mut residue_ids: HashMap<Hash, Hash> = HashMap::new();
    let mut drop_bytes: u64 = 0;
    for f in &frames {
        let take = f.present
            && match f.kind {
                kinds::BLOCK => versions.contains(&f.hash),
                kinds::CHANGE => producers.contains(&f.hash),
                kinds::BLOB_CHUNK => chunks.contains(&f.hash),
                _ => false,
            };
        if !take {
            continue;
        }
        let residue = match f.kind {
            kinds::BLOB_CHUNK => None,
            _ => Some(match residue_ids.get(&f.hash) {
                Some(r) => *r,
                None => {
                    let r = if f.kind == kinds::BLOCK {
                        RedactionResidue::Block(BlockResidue::of(&reader.get_block(&f.hash)?))
                    } else {
                        RedactionResidue::Change(ChangeResidue::of(&reader.change(&f.hash)?))
                    };
                    let id = Hash::of(encode_into(&r, &mut buf).map_err(|_| over(limit::RECORD))?);
                    residue_ids.insert(f.hash, id);
                    residues.push(r);
                    id
                }
            }),
        };
        drop_bytes = drop_bytes
            .checked_add(u64::from(f.len))
            .ok_or_else(|| over(limit::BYTES))?;
        spans.push(DropSpan {
            place: FramePlace {
                segment: f.loc.segment,
                offset: f.loc.offset,
            },
            kind: f.kind,
            hash: f.hash,
            len: f.len,
            residue,
        });
    }
    drop(buf);

    // the source strings: each selected version's body and string field leaves
    let mut sources: Vec<String> = vec![];
    for (_, v) in &selected {
        if removed(kinds::BLOCK, v)? {
            continue;
        }
        let b = reader.get_block(v)?;
        check_levels(&b, &limits)?;
        let mut leaves: Vec<&str> = vec![];
        if !b.body.is_empty() {
            leaves.push(&b.body);
        }
        b.fields
            .values()
            .for_each(|f| string_leaves(f, &mut leaves));
        for s in leaves {
            if s.len() > limits.string {
                return Err(over(limit::STRING));
            }
            sources.push(s.to_string());
        }
    }
    sources.sort();
    sources.dedup();
    if sources.len() > limits.sources {
        return Err(over(limit::SOURCES));
    }

    // pass 2: references and quotes
    let mut quotes = Quotes {
        sources: &sources,
        limits,
        found: BTreeMap::new(),
    };
    let mut seen: HashSet<(u8, Hash)> = HashSet::new();
    let mut proposals: Vec<(Hash, Vec<Hash>)> = vec![];
    let mut landings: BTreeMap<Hash, LandingRecord> = BTreeMap::new();
    let mut snapshots: HashSet<Hash> = HashSet::new();
    let mut tags: Vec<(Hash, String, Hash)> = vec![];
    let mut view_refs: Vec<(String, Vec<Hash>)> = vec![];
    let mut reviews: Vec<(Hash, Review)> = vec![];
    let mut tensions: Vec<(Hash, SemanticTension)> = vec![];
    let mut answers: Vec<(Hash, Answer)> = vec![];
    let audit = ImpactKind::AuditText;
    for f in &frames {
        if !seen.insert((f.kind, f.hash)) {
            continue;
        }
        let h = f.hash;
        let rec = h.to_string();
        match f.kind {
            kinds::BLOCK => {
                if !f.present || versions.contains(&h) {
                    continue;
                }
                let b = reader.get_block(&h)?;
                check_levels(&b, &limits)?;
                quotes.scan(
                    ImpactKind::BlockVersion,
                    &version_record(&b.id, &h),
                    true,
                    &b,
                )?;
            }
            kinds::CHANGE => {
                let c = reader.change(&h)?;
                quotes.scan(
                    ImpactKind::Change,
                    &rec,
                    false,
                    &(
                        &c.view,
                        &c.provenance,
                        &c.justification,
                        &c.message,
                        &c.links,
                    ),
                )?;
                for op in &c.ops {
                    if let Op::Create { block } | Op::Update { block, .. } = op {
                        let v = block.version_hash();
                        if block.is_redacted() || versions.contains(&v) {
                            continue;
                        }
                        check_levels(block, &limits)?;
                        quotes.scan(
                            ImpactKind::BlockVersion,
                            &version_record(&block.id, &v),
                            true,
                            block,
                        )?;
                    }
                }
            }
            kinds::PROPOSAL => {
                let p: Proposal = reader.get_typed(kinds::PROPOSAL, &h)?;
                quotes.scan(ImpactKind::Proposal, &rec, false, &p)?;
                proposals.push((h, p.changes));
            }
            kinds::LANDING => {
                let l: LandingRecord = reader.get_typed(kinds::LANDING, &h)?;
                quotes.scan(ImpactKind::Landing, &rec, false, &l)?;
                landings.insert(h, l);
            }
            kinds::SNAPSHOT => {
                let s: StateSnapshot = reader.get_typed(kinds::SNAPSHOT, &h)?;
                if s.blocks.values().any(|v| affected_versions.contains(v)) {
                    snapshots.insert(h);
                }
            }
            kinds::TAG => {
                let t: Tag = reader.get_typed(kinds::TAG, &h)?;
                quotes.scan(ImpactKind::Tag, &t.name, false, &t)?;
                tags.push((h, t.name, t.landing));
            }
            kinds::VIEW_OP => {
                let op: ViewOp = reader.get_typed(kinds::VIEW_OP, &h)?;
                quotes.scan(audit, &rec, false, &op)?;
                let named = view_op_changes(&op);
                view_refs.push((op.view, named));
            }
            kinds::REVIEW => {
                let r: Review = reader.get_typed(kinds::REVIEW, &h)?;
                quotes.scan(ImpactKind::Review, &rec, false, &r)?;
                reviews.push((h, r));
            }
            kinds::TENSION => {
                let t: SemanticTension = reader.get_typed(kinds::TENSION, &h)?;
                quotes.scan(ImpactKind::Tension, &rec, false, &t)?;
                tensions.push((h, t));
            }
            kinds::ANSWER => {
                let a: Answer = reader.get_typed(kinds::ANSWER, &h)?;
                quotes.scan(ImpactKind::Answer, &rec, false, &a)?;
                answers.push((h, a));
            }
            kinds::INTENT => {
                let i: Intent = reader.get_typed(kinds::INTENT, &h)?;
                quotes.scan(audit, &rec, false, &i)?;
            }
            kinds::ALT_SET => {
                let s: AltSet = reader.get_typed(kinds::ALT_SET, &h)?;
                quotes.scan(audit, &rec, false, &s)?;
            }
            kinds::TRANSITION => {
                let t: Transitions = reader.get_typed(kinds::TRANSITION, &h)?;
                quotes.scan(audit, &rec, false, &t)?;
            }
            kinds::REDACTION => {
                let r: Redaction = reader.get_typed(kinds::REDACTION, &h)?;
                quotes.scan(audit, &rec, false, &r)?;
            }
            kinds::REF_UPDATE => {
                let r: RefUpdate = reader.get_typed(kinds::REF_UPDATE, &h)?;
                quotes.scan(audit, &rec, false, &r.name)?;
            }
            // residues report under their original identity; the rest hold no text
            _ => {}
        }
    }

    // the closure of references over the histories
    let dep_hit = dep_closure(&changes, &affected_changes)?;
    let hit = |c: &Hash| dep_hit.contains(c) || affected_changes.contains(c);
    let mut inside: BTreeSet<(u8, String, u8, bool)> = BTreeSet::new();
    let mut row = |kind: ImpactKind, record: String, relation: ImpactRelation, erasable: bool| {
        inside.insert((kind as u8, record, relation as u8, erasable));
    };
    let refs = ImpactRelation::References;
    for (id, v) in &selected {
        row(
            ImpactKind::BlockVersion,
            version_record(id, v),
            ImpactRelation::Removed,
            true,
        );
    }
    for (id, v) in &referencing {
        row(ImpactKind::BlockVersion, version_record(id, v), refs, false);
    }
    for c in &affected_changes {
        if producers.contains(c) {
            row(
                ImpactKind::Change,
                c.to_string(),
                ImpactRelation::Embeds,
                true,
            );
        } else {
            row(ImpactKind::Change, c.to_string(), refs, false);
        }
    }
    for s in &snapshots {
        row(ImpactKind::StateSnapshot, s.to_string(), refs, false);
    }
    let mut affected_proposals: HashSet<Hash> = HashSet::new();
    for (p, cs) in &proposals {
        if cs.iter().any(&hit) {
            affected_proposals.insert(*p);
            row(ImpactKind::Proposal, p.to_string(), refs, false);
        }
    }
    let mut views_hit: BTreeSet<&str> = BTreeSet::new();
    for (name, cs) in &view_refs {
        if cs.iter().any(&hit) {
            views_hit.insert(name);
        }
    }
    for v in views_hit {
        row(ImpactKind::View, v.to_string(), refs, false);
    }
    let landing_direct: HashSet<Hash> = landings
        .iter()
        .filter(|(_, l)| l.changes.iter().any(&hit))
        .map(|(h, _)| *h)
        .collect();
    let history = landing_history(&landings, &landing_direct)?;
    let mut affected_landings: HashSet<Hash> = HashSet::new();
    for (h, l) in &landings {
        if landing_direct.contains(h) || snapshots.contains(&l.state) {
            affected_landings.insert(*h);
            row(ImpactKind::Landing, h.to_string(), refs, false);
        }
    }
    let mut affected_tags: HashSet<Hash> = HashSet::new();
    for (h, name, landing) in &tags {
        let snap = landings
            .get(landing)
            .is_some_and(|l| snapshots.contains(&l.state));
        if snap || history.contains(landing) {
            affected_tags.insert(*h);
            row(ImpactKind::Tag, name.clone(), refs, false);
        }
    }
    let mut affected_reviews: HashSet<Hash> = HashSet::new();
    for (h, r) in &reviews {
        if review_blocks(r).iter().any(|b| affected_ids.contains(b)) {
            affected_reviews.insert(*h);
        }
    }
    // a tension version through its items or its review, then through its prev versions
    let mut affected_tensions: HashSet<Hash> = tensions
        .iter()
        .filter(|(_, t)| {
            t.items.iter().any(|i| affected_ids.contains(&i.block))
                || affected_reviews.contains(&t.review)
        })
        .map(|(h, _)| *h)
        .collect();
    loop {
        let before = affected_tensions.len();
        for (h, t) in &tensions {
            if t.prev.is_some_and(|p| affected_tensions.contains(&p)) {
                affected_tensions.insert(*h);
            }
        }
        if affected_tensions.len() == before {
            break;
        }
    }
    let affected_answers: Vec<Hash> = answers
        .iter()
        .filter(|(_, a)| {
            let target = a.targets.iter().any(|t| {
                affected_versions.contains(t)
                    || hit(t)
                    || affected_reviews.contains(t)
                    || affected_tensions.contains(t)
                    || affected_proposals.contains(t)
                    || affected_tags.contains(t)
                    || affected_landings.contains(t)
            });
            target || item_hits(&a.item, &selected, &referencing)
        })
        .map(|(h, _)| *h)
        .collect();
    for h in &affected_reviews {
        row(ImpactKind::Review, h.to_string(), refs, false);
    }
    for h in &affected_tensions {
        row(ImpactKind::Tension, h.to_string(), refs, false);
    }
    for h in &affected_answers {
        row(ImpactKind::Answer, h.to_string(), refs, false);
    }
    let mut quote_rows: Vec<QuoteRow> = vec![];
    for ((kind, record, erasable), texts) in quotes.found {
        row(
            impact_kind(kind),
            record.clone(),
            ImpactRelation::Quotes,
            erasable,
        );
        for quote in texts {
            quote_rows.push(QuoteRow {
                kind: impact_kind(kind),
                record: record.clone(),
                quote,
                erasable,
            });
        }
    }
    let inside: Vec<ImpactRow> = inside
        .into_iter()
        .map(|(kind, record, relation, erasable)| ImpactRow {
            kind: impact_kind(kind),
            record,
            relation: relation_of(relation),
            erasable,
        })
        .collect();
    let manifest = RedactionManifest {
        versions: selected.into_iter().collect(),
        blobs: blobs.into_iter().collect(),
        shared_blobs: shared.into_iter().collect(),
        spans,
        inside,
        outside,
        quarantine,
        assessment: AssessmentSupport::NotImplemented,
    };
    let drop_frames = u64::try_from(manifest.spans.len()).map_err(|_| over(limit::FRAMES))?;
    finish(
        Draft {
            targets,
            already,
            manifest,
            quotes: quote_rows,
            drop_frames,
            drop_bytes,
            warnings,
        },
        residues,
        request_targets,
        class,
        reason,
        &limits,
    )
}

/// The relation whose ordinal is `n`.
fn relation_of(n: u8) -> ImpactRelation {
    [
        ImpactRelation::Removed,
        ImpactRelation::Embeds,
        ImpactRelation::References,
        ImpactRelation::Quotes,
    ][usize::from(n)]
}

/// Whether an answer's item ref names an affected block version: the block form
/// `<id>@<12 hex digits>` (`answer::is_item_ref`) whose id and version prefix match one.
fn item_hits(
    item: &str,
    selected: &BTreeSet<(BlockId, Hash)>,
    referencing: &BTreeSet<(BlockId, Hash)>,
) -> bool {
    if !crate::answer::is_item_ref(item) {
        return false;
    }
    let Some((id, prefix)) = item.split_once('@') else {
        return false;
    };
    let Ok(id) = BlockId::parse(id) else {
        return false;
    };
    selected
        .iter()
        .chain(referencing)
        .any(|(i, v)| *i == id && v.to_string().starts_with(prefix))
}

/// A report before its hash and bounds.
struct Draft {
    targets: Vec<RedactTarget>,
    already: Vec<RedactTarget>,
    manifest: RedactionManifest,
    quotes: Vec<QuoteRow>,
    drop_frames: u64,
    drop_bytes: u64,
    warnings: Vec<RedactionWarning>,
}

/// Checks the row bound, hashes the manifest and checks the encoded report against the record
/// bound.
fn finish(
    d: Draft,
    residues: Vec<RedactionResidue>,
    request_targets: Vec<RedactTarget>,
    class: ReasonClass,
    reason: String,
    limits: &Limits,
) -> Result<Full, RdyError> {
    let m = &d.manifest;
    let rows = [
        m.inside.len(),
        d.quotes.len(),
        m.outside.len(),
        m.quarantine.len(),
    ]
    .into_iter()
    .try_fold(0usize, usize::checked_add);
    if rows.is_none_or(|n| n > limits.rows) {
        return Err(over(limit::ROWS));
    }
    let mut buf = buffer(limits.record)?;
    let report_hash = m.report_hash(&mut buf).map_err(|_| over(limit::RECORD))?;
    let report = RedactionReport {
        report_hash,
        targets: d.targets,
        already_redacted: d.already,
        versions: m.versions.clone(),
        blobs: m.blobs.clone(),
        shared_blobs: m.shared_blobs.clone(),
        drop_frames: d.drop_frames,
        drop_bytes: d.drop_bytes,
        inside: m.inside.clone(),
        quotes: d.quotes,
        outside: m.outside.clone(),
        quarantine: m.quarantine.clone(),
        assessment: m.assessment,
        warnings: d.warnings,
    };
    encode_into(&report, &mut buf).map_err(|_| over(limit::RECORD))?;
    Ok(Full {
        report,
        manifest: d.manifest,
        residues,
        request_targets,
        class,
        reason,
    })
}

/// Hashes as full lowercase hex in JSON and as `Hash`'s own bytes in postcard, for the report's
/// lists (the scalar and optional forms are `rdy_store::transition::hex`'s).
mod hex {
    /// A list of hashes.
    pub mod vec {
        use rdy_model::Hash;
        use serde::{Deserialize, Deserializer, Serialize, Serializer};

        pub fn serialize<S: Serializer>(h: &[Hash], s: S) -> Result<S::Ok, S::Error> {
            if s.is_human_readable() {
                s.collect_seq(h.iter().map(Hash::to_string))
            } else {
                h.serialize(s)
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Hash>, D::Error> {
            if d.is_human_readable() {
                Vec::<String>::deserialize(d)?
                    .iter()
                    .map(|t| {
                        t.parse()
                            .map_err(|_| serde::de::Error::custom(format!("bad hash {t:?}")))
                    })
                    .collect()
            } else {
                Vec::<Hash>::deserialize(d)
            }
        }
    }

    /// A list of (block id, hash) pairs.
    pub mod pairs {
        use rdy_model::{BlockId, Hash};
        use serde::{Deserialize, Deserializer, Serialize, Serializer};

        pub fn serialize<S: Serializer>(h: &[(BlockId, Hash)], s: S) -> Result<S::Ok, S::Error> {
            if s.is_human_readable() {
                s.collect_seq(h.iter().map(|(id, h)| (id, h.to_string())))
            } else {
                h.serialize(s)
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(
            d: D,
        ) -> Result<Vec<(BlockId, Hash)>, D::Error> {
            if d.is_human_readable() {
                Vec::<(BlockId, String)>::deserialize(d)?
                    .into_iter()
                    .map(|(id, t)| {
                        let h = t
                            .parse()
                            .map_err(|_| serde::de::Error::custom(format!("bad hash {t:?}")))?;
                        Ok((id, h))
                    })
                    .collect()
            } else {
                Vec::<(BlockId, Hash)>::deserialize(d)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hx(n: u8) -> Hash {
        Hash([n; 32])
    }

    fn info(deps: &[Hash]) -> ChangeInfo {
        ChangeInfo {
            deps: deps.to_vec(),
            ..ChangeInfo::default()
        }
    }

    #[test]
    fn rdx5_a_cycle_in_a_history_is_refused_and_shared_edges_fold() {
        // a diamond of deps reaches the affected change once per change; no error
        let changes: HashMap<Hash, ChangeInfo> = [
            (hx(1), info(&[])),
            (hx(2), info(&[hx(1)])),
            (hx(3), info(&[hx(1)])),
            (hx(4), info(&[hx(2), hx(3), hx(2)])),
            (hx(5), info(&[])),
        ]
        .into_iter()
        .collect();
        let hit = dep_closure(&changes, &BTreeSet::from([hx(1)])).unwrap();
        assert_eq!(hit, HashSet::from([hx(1), hx(2), hx(3), hx(4)]));
        // a dependency cycle
        let mut cyclic = changes;
        cyclic.insert(hx(1), info(&[hx(4)]));
        let e = dep_closure(&cyclic, &BTreeSet::from([hx(5)])).unwrap_err();
        assert_eq!(e.code, E_BAD_INPUT);
        assert_eq!(e.message, "The redaction reference graph has a cycle.");
        assert_eq!(
            e.fix,
            "Keep the store unchanged and inspect it with rdy fsck."
        );
        // a landing chain: history reaches back through prev; a prev cycle is refused
        let landing = |prev: Option<Hash>| LandingRecord {
            proposal: hx(0),
            prev,
            state: hx(0),
            approved_by: rdy_model::Provenance {
                actor: "human:t".into(),
                run: None,
                role: rdy_model::Role::Approver,
                at_ms: 0,
            },
            changes: vec![],
            chosen: None,
            checks: vec![],
            assurance: rdy_model::Assurance::Local,
            relayed_by: None,
            relay_evidence: None,
            approved_version: None,
            rebased_from: vec![],
            evidence: None,
        };
        let chain: BTreeMap<Hash, LandingRecord> = [
            (hx(10), landing(None)),
            (hx(11), landing(Some(hx(10)))),
            (hx(12), landing(Some(hx(11)))),
            (hx(13), landing(None)),
        ]
        .into_iter()
        .collect();
        let hist = landing_history(&chain, &HashSet::from([hx(11)])).unwrap();
        assert_eq!(hist, HashSet::from([hx(11), hx(12)]));
        let mut looped = chain;
        looped.insert(hx(10), landing(Some(hx(12))));
        let e = landing_history(&looped, &HashSet::new()).unwrap_err();
        assert_eq!(e.message, "The redaction reference graph has a cycle.");
    }
}
