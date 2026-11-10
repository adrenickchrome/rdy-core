//! The five deterministic kinds across projects (repo links, D-Y49PX): tensions with a side
//! cited from a linked project, `SideRef::Foreign {repo, entry}`, shown
//! `foreign:<alias>@<12 hex>`.
//!
//! A foreign side is the cited repo and the FOREIGN entry that delivered the involved version.
//! It has no local actor and no landing, and its block is never looked up in canon, the views
//! or the key index: the cited blocks come from the foreign state of the read state
//! ([`crate::ForeignState`]). The closed triggers:
//!
//! - duplicate definition: a local block and a cited block of the current delivered view have
//!   the same normalized normative text, or define one name (citing pack's traits, name keys as
//!   locally); one tension per (local block, cited block), the name form first. Two cited
//!   instructions from live sources of two different team sets with the same normalized text
//!   also trigger, with two foreign sides (D-4W1ZR);
//! - value disagreement: a local block tied (a reference, a `describes` or an `affects` link) to
//!   a cited `value` states another number in its unit, read against the version its pin holds;
//! - restatement drift: a local block closely matched (drift's threshold and shingles) earlier
//!   text of a cited block, and the current delivered version removes a run they share. The
//!   earlier text is the pinned one when the local block pins the cited block, else each
//!   retained earlier delivered version (no citation needed);
//! - deprecated reference: a local block points (a reference or an impact link) at a cited
//!   block whose delivered facts say deprecated, superseded or revoked, or names a deprecated
//!   cited definition in its text without a tie to it;
//! - premise shift: an in-flight change's block has a premise edge with a pin to a cited block
//!   whose current delivered content differs from the pin.
//!
//! In flight (each detector, [`detect`]) a local side is a live write: a tension is kept when it
//! has a change side. The canon form ([`canon_tensions`]) reads canon's blocks as the local side:
//! a canon-or-foreign tension is a standing condition and stays in canon scope. It follows the
//! foreign state, which a delivery moves without moving canon, so it is joined per build and
//! never kept in canon's memo. Severity is the citing pack's `tensions_across`, else advisory
//! ([`super::severity_of`]). A live deviation covers the pairs of the blocks it governs and its
//! target ([`Deviations`]).
use super::drift::{DRIFT_MIN_WORDS, drifts, restating_type, text_of};
use super::index::{DetectCx, Writes, canon_index_of};
use super::value::{first_difference, value_parts};
use super::{
    DetectScope, Detector, Found, Involved, SideRef, Tension, TensionKind, name_keys_of,
    normalize_name, tension_from,
};
use crate::foreign_refs::RepoLinks;
use crate::lexicon::{Lexicon, NameKind, is_tie};
use crate::{RdyError, ReadState, Scope};
use rdy_graph::foreign::{ForeignEdge, foreign_edges_of};
use rdy_graph::{EdgeKind, IMPACT_LINKS, SUSPECT_LINKS};
use rdy_model::foreign::{ForeignDerived, ForeignId, ForeignSet};
use rdy_model::{Block, BlockId, Hash, RepoId, decision};
use rdy_schema::instruction::INSTRUCTION;
use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// One delivered version of a cited block: its repo, the entry that delivered it, its BLOCK
/// version, its content identity, the delivered facts and the block.
struct Cited {
    repo: RepoId,
    entry: Hash,
    version: Hash,
    content: Hash,
    encoding: u16,
    derived: ForeignDerived,
    block: Arc<Block>,
}

impl Cited {
    /// Whether `pin` records this version's content.
    fn pinned_by(&self, pin: &rdy_model::foreign::ForeignPin) -> bool {
        (self.content, self.encoding) == (pin.content, pin.encoding)
    }

    fn target(&self) -> ForeignId {
        ForeignId::new(self.repo, self.block.id.clone())
    }

    /// The involved item of this version, on its foreign side.
    fn involved(&self) -> Involved {
        Involved {
            side: SideRef::Foreign {
                repo: self.repo,
                entry: self.entry,
            },
            block: self.block.id.clone(),
            version: Some(self.version),
            content: Some(self.content),
        }
    }

    /// Whether the delivered facts say deprecated, superseded or revoked.
    fn deprecated(&self) -> bool {
        let d = &self.derived;
        d.lifecycle.as_deref() == Some("deprecated")
            || matches!(d.status.as_deref(), Some("superseded" | "revoked"))
            || !d.superseded_by.is_empty()
    }
}

/// One local block a run reads: its block and its involved item (a live write's change side in
/// flight, canon's side in the canon scope), with the base the write started from.
struct Local {
    block: Arc<Block>,
    item: Involved,
    base: Option<Hash>,
    /// The change of a live write, for its summary.
    change: Option<Hash>,
}

impl Local {
    fn who(&self) -> String {
        match self.change {
            Some(c) => format!("change {}", c.short()),
            None => "canon".to_string(),
        }
    }
}

/// What one run reads once: the context, the foreign state, the live repo links (for display)
/// and the current delivered view.
pub(crate) struct Across<'c, 'a> {
    cx: &'c DetectCx<'a>,
    links: RepoLinks,
    current: OnceCell<Vec<Cited>>,
}

impl<'c, 'a> Across<'c, 'a> {
    fn new(cx: &'c DetectCx<'a>) -> Result<Across<'c, 'a>, RdyError> {
        Ok(Across {
            cx,
            links: RepoLinks::of_view(&*cx.canon_m()?),
            current: OnceCell::new(),
        })
    }

    /// How a cited block shows in a summary: `<alias>:<id>`.
    fn shown(&self, repo: &RepoId, id: &BlockId) -> String {
        format!("{}:{id}", self.links.shown(repo))
    }

    /// A delivered version read from the store; `None` for a redacted one.
    fn cited(
        &self,
        repo: RepoId,
        entry: Hash,
        v: &rdy_model::foreign::ForeignVersion,
    ) -> Result<Option<Cited>, RdyError> {
        let block = self.cx.st.reader().get_block(&v.version)?;
        if block.is_redacted() || block.id != v.id {
            return Ok(None);
        }
        Ok(Some(Cited {
            repo,
            entry,
            version: v.version,
            content: v.content,
            encoding: v.encoding,
            derived: v.derived.clone(),
            block: Arc::new(block),
        }))
    }

    /// Every block of the current delivered view, by repo then id.
    fn current(&self) -> Result<&[Cited], RdyError> {
        if let Some(c) = self.current.get() {
            return Ok(c);
        }
        let foreign = self.cx.st.foreign()?;
        let mut out = vec![];
        for repo in foreign.repos() {
            for held in foreign.current(repo).into_iter().flat_map(|m| m.values()) {
                if let Some(c) = self.cited(*repo, held.entry, &held.version)? {
                    out.push(c);
                }
            }
        }
        Ok(self.current.get_or_init(|| out))
    }

    /// The current delivered version of `target`.
    fn current_of(&self, target: &ForeignId) -> Result<Option<Cited>, RdyError> {
        let foreign = self.cx.st.foreign()?;
        match foreign.current_version(target) {
            Some(held) => self.cited(target.repo, held.entry, &held.version),
            None => Ok(None),
        }
    }

    /// The version of `target` that `b`'s pin holds (the version in force here), with the
    /// first entry that delivered it; else the current one when `b` holds no pin for it.
    fn in_force(&self, b: &Block, target: &ForeignId) -> Result<Option<Cited>, RdyError> {
        let Some(pin) = b.foreign.pins.get(target) else {
            return self.current_of(target);
        };
        let st = self.cx.st;
        let Some(version) = st.foreign_held_pin(target, pin)? else {
            return Ok(None);
        };
        let Some((_, entry)) = st.foreign_carrier(target, &version)? else {
            return Ok(None);
        };
        let v = rdy_model::foreign::ForeignVersion {
            id: target.id.clone(),
            version,
            content: pin.content,
            encoding: pin.encoding,
            derived: ForeignDerived::default(),
        };
        self.cited(target.repo, entry, &v)
    }

    /// The local blocks this run reads: in flight, the live writes (on `seeds` when given);
    /// in the canon scope, every canon block.
    fn locals(&self, seeds: Option<&BTreeSet<BlockId>>) -> Result<Vec<Local>, RdyError> {
        let cx = self.cx;
        let mut out = vec![];
        match cx.scope() {
            DetectScope::Inflight => {
                let writes: Vec<Arc<super::Write>> = match seeds {
                    Some(s) => s.iter().flat_map(|b| cx.writes.live_on(b)).collect(),
                    None => cx.writes.live().cloned().collect(),
                };
                for w in writes {
                    let Some(b) = w.new.clone() else { continue };
                    out.push(Local {
                        item: cx.of_write(&w),
                        block: b,
                        base: w.base,
                        change: Some(w.change),
                    });
                }
            }
            DetectScope::Canon => {
                let m = cx.canon_m()?;
                for b in m.iter() {
                    if seeds.is_some_and(|s| !s.contains(&b.id)) {
                        continue;
                    }
                    let Some((side, v)) = cx.canon_side(&b.id)? else {
                        continue;
                    };
                    out.push(Local {
                        item: Involved {
                            side,
                            block: b.id.clone(),
                            version: Some(v),
                            content: Some(cx.content_of(b)),
                        },
                        block: Arc::new(b.clone()),
                        base: None,
                        change: None,
                    });
                }
            }
        }
        Ok(out)
    }
}

/// A found tension of `kind` between `local` and `cited`.
fn pair(kind: TensionKind, local: &Local, cited: &Cited, summary: String) -> Found {
    Found {
        kind,
        involved: vec![local.item.clone(), cited.involved()],
        summary,
        stable: false,
    }
}

/// The group keys of `b` for duplicates: its name keys, then its normalized text.
fn dup_keys(cx: &DetectCx<'_>, b: &Block) -> (Vec<String>, Option<Hash>) {
    (name_keys_of(cx.st.pack(), b), cx.text_key(b))
}

/// Text clipped to `n` characters, with `…` when cut.
fn clipped(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut out: String = s.chars().take(n).collect();
    out.push('…');
    out
}

/// Duplicate definition across projects: (local, cited) pairs, and team-set instruction pairs
/// in the canon scope.
pub(crate) fn duplicates(
    ax: &Across<'_, '_>,
    kind: TensionKind,
    seeds: Option<&BTreeSet<BlockId>>,
    out: &mut Vec<Found>,
) -> Result<(), RdyError> {
    let cx = ax.cx;
    let locals = ax.locals(seeds)?;
    if !locals.is_empty() {
        // the cited blocks by name key and by text
        let current = ax.current()?;
        let mut names: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        let mut texts: BTreeMap<Hash, Vec<usize>> = BTreeMap::new();
        for (i, c) in current.iter().enumerate() {
            let (n, t) = dup_keys(cx, &c.block);
            for k in n {
                names.entry(k).or_default().push(i);
            }
            if let Some(t) = t {
                texts.entry(t).or_default().push(i);
            }
        }
        if !names.is_empty() || !texts.is_empty() {
            for l in &locals {
                let (n, t) = dup_keys(cx, &l.block);
                let mut seen: BTreeSet<usize> = BTreeSet::new();
                for k in &n {
                    for &i in names.get(k).into_iter().flatten() {
                        if seen.insert(i) {
                            let c = &current[i];
                            let what = format!("both define \"{}\"", clipped(k, 80));
                            let summary = format!(
                                "{} and {} {what}",
                                l.item.block,
                                ax.shown(&c.repo, &c.block.id)
                            );
                            out.push(pair(kind, l, c, summary));
                        }
                    }
                }
                for &i in t.iter().flat_map(|t| texts.get(t)).flatten() {
                    if seen.insert(i) {
                        let c = &current[i];
                        let text = cx.duplicate_text(&l.block).map(normalize_name);
                        let summary = format!(
                            "{} and {} have the same text \"{}\"",
                            l.item.block,
                            ax.shown(&c.repo, &c.block.id),
                            clipped(&text.unwrap_or_default(), 80)
                        );
                        out.push(pair(kind, l, c, summary));
                    }
                }
            }
        }
    }
    if cx.scope() == DetectScope::Canon && seeds.is_none() {
        team_collisions(ax, kind, out)?;
    }
    Ok(())
}

/// Two cited instructions from live sources of two different team sets with one normalized text:
/// one tension per pair, both sides foreign.
fn team_collisions(
    ax: &Across<'_, '_>,
    kind: TensionKind,
    out: &mut Vec<Found>,
) -> Result<(), RdyError> {
    let cx = ax.cx;
    let foreign = cx.st.foreign()?;
    let mut by_text: BTreeMap<Hash, Vec<(String, Cited)>> = BTreeMap::new();
    for (_, view) in foreign.sources() {
        let Some(ForeignSet::Team(team)) = &view.set else {
            continue;
        };
        if view.revoked {
            continue;
        }
        for held in view.members.values() {
            let Some(c) = ax.cited(view.repo, held.entry, &held.version)? else {
                continue;
            };
            if c.block.block_type != INSTRUCTION {
                continue;
            }
            if let Some(t) = cx.text_key(&c.block) {
                by_text.entry(t).or_default().push((team.clone(), c));
            }
        }
    }
    for group in by_text.values() {
        for (i, (ta, a)) in group.iter().enumerate() {
            for (tb, b) in &group[i + 1..] {
                if ta == tb || a.target() == b.target() {
                    continue;
                }
                let text = normalize_name(&a.block.body);
                out.push(Found {
                    kind,
                    involved: vec![a.involved(), b.involved()],
                    summary: format!(
                        "{} (team {ta}) and {} (team {tb}) have the same text \"{}\"",
                        ax.shown(&a.repo, &a.block.id),
                        ax.shown(&b.repo, &b.block.id),
                        clipped(&text, 80)
                    ),
                    stable: false,
                });
            }
        }
    }
    Ok(())
}

/// Each foreign edge of `l` to a distinct target that `keep` accepts, in edge order.
fn edges(l: &Local, keep: impl Fn(&ForeignEdge) -> bool) -> Vec<ForeignEdge> {
    let mut seen: BTreeSet<ForeignId> = BTreeSet::new();
    foreign_edges_of(&l.block)
        .into_iter()
        .filter(|e| keep(e) && seen.insert(e.to.clone()))
        .collect()
}

/// Value disagreement across projects.
pub(crate) fn values(
    ax: &Across<'_, '_>,
    kind: TensionKind,
    seeds: Option<&BTreeSet<BlockId>>,
    out: &mut Vec<Found>,
) -> Result<(), RdyError> {
    for l in ax.locals(seeds)? {
        for e in edges(&l, |e| is_tie(&e.kind)) {
            let Some(c) = ax.in_force(&l.block, &e.to)? else {
                continue;
            };
            let Some((unit, written, number)) = value_parts(&c.block) else {
                continue;
            };
            let Some(stated) = first_difference(&l.block, &unit, &number) else {
                continue;
            };
            let name = c.block.field_str("name").unwrap_or_default();
            let summary = format!(
                "{} states {stated} {unit} for {name} ({}), which is {written} {unit}",
                l.item.block,
                ax.shown(&c.repo, &c.block.id)
            );
            out.push(pair(kind, &l, &c, summary));
        }
    }
    Ok(())
}

/// One retained earlier text of a cited block that a local block without a pin for it is read
/// against: the index of the block's current version in [`Across::current`] and the earlier
/// version's shingles.
struct Earlier {
    now: usize,
    set: Vec<u64>,
}

/// Every earlier delivered text of the current delivered view that drift reads: per current
/// cited block of a restating type, each other held version of a restating type and drift's
/// length, with an inverted index from each shingle to the earlier texts that hold it.
fn earlier_texts(
    ax: &Across<'_, '_>,
) -> Result<(Vec<Earlier>, BTreeMap<u64, Vec<usize>>), RdyError> {
    let cx = ax.cx;
    let pack = cx.st.pack();
    let reader = cx.st.reader();
    let mut texts = vec![];
    let mut holders: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
    for (i, now) in ax.current()?.iter().enumerate() {
        if !restating_type(pack, &now.block) {
            continue;
        }
        for v in cx.st.foreign_versions(&now.target())? {
            if v == now.version {
                continue;
            }
            let was = reader.get_block(&v)?;
            if was.is_redacted() || was.id != now.block.id || !restating_type(pack, &was) {
                continue;
            }
            let p = text_of(&was.body);
            if p.words < DRIFT_MIN_WORDS {
                continue;
            }
            for s in &p.set {
                holders.entry(*s).or_default().push(texts.len());
            }
            texts.push(Earlier { now: i, set: p.set });
        }
    }
    Ok((texts, holders))
}

/// Restatement drift across projects. A local block that pins a cited block is read against the
/// pinned text; one without a pin for it, against each retained earlier delivered text. Either
/// way drift's threshold and shared-shingle removal decide, and the current version is the
/// foreign side.
pub(crate) fn drift(
    ax: &Across<'_, '_>,
    kind: TensionKind,
    seeds: Option<&BTreeSet<BlockId>>,
    out: &mut Vec<Found>,
) -> Result<(), RdyError> {
    let cx = ax.cx;
    let pack = cx.st.pack();
    let threshold = pack.drift_threshold();
    let mut earlier: Option<(Vec<Earlier>, BTreeMap<u64, Vec<usize>>)> = None;
    let summary = |now: &Cited, l: &Local| {
        format!(
            "{} (delivered) changed text that {} restates",
            ax.shown(&now.repo, &now.block.id),
            l.item.block
        )
    };
    for l in ax.locals(seeds)? {
        if !restating_type(pack, &l.block) {
            continue;
        }
        let y = text_of(&l.block.body);
        if y.words < DRIFT_MIN_WORDS {
            continue;
        }
        for (target, pin) in &l.block.foreign.pins {
            let Some(now) = ax.current_of(target)? else {
                continue;
            };
            if now.pinned_by(pin) {
                continue;
            }
            let Some(was) = ax.in_force(&l.block, target)? else {
                continue;
            };
            if !restating_type(pack, &was.block) {
                continue;
            }
            let p = text_of(&was.block.body);
            if p.words < DRIFT_MIN_WORDS {
                continue;
            }
            let n = text_of(&now.block.body);
            if drifts(&p.set, &y.set, &n.set, threshold) {
                out.push(pair(kind, &l, &now, summary(&now, &l)));
            }
        }
        // no pin: the retained earlier texts that share a shingle with this block
        if earlier.is_none() {
            earlier = Some(earlier_texts(ax)?);
        }
        let Some((texts, holders)) = &earlier else {
            continue;
        };
        let near: BTreeSet<usize> = y
            .set
            .iter()
            .filter_map(|s| holders.get(s))
            .flatten()
            .copied()
            .collect();
        let current = ax.current()?;
        let mut raised: BTreeSet<usize> = BTreeSet::new();
        for e in near.into_iter().map(|k| &texts[k]) {
            let now = &current[e.now];
            if raised.contains(&e.now) || l.block.foreign.pins.contains_key(&now.target()) {
                continue;
            }
            let n = text_of(&now.block.body);
            if drifts(&e.set, &y.set, &n.set, threshold) {
                raised.insert(e.now);
                out.push(pair(kind, &l, now, summary(now, &l)));
            }
        }
    }
    Ok(())
}

/// Whether `e` points at its target: a reference or an impact link, never a local decision's own
/// `supersedes` or `revokes` link.
fn pointing(e: &ForeignEdge) -> bool {
    match &e.kind {
        EdgeKind::References => true,
        EdgeKind::Link(n) => {
            IMPACT_LINKS.contains(&n.as_str())
                && n != decision::SUPERSEDES
                && n != decision::REVOKES
        }
        EdgeKind::Contains => false,
    }
}

/// Deprecated reference across projects: the edge form and the mention form.
pub(crate) fn deprecated(
    ax: &Across<'_, '_>,
    kind: TensionKind,
    seeds: Option<&BTreeSet<BlockId>>,
    out: &mut Vec<Found>,
) -> Result<(), RdyError> {
    let cx = ax.cx;
    let reader = cx.st.reader();
    let locals = ax.locals(seeds)?;
    let canon = cx.scope() == DetectScope::Canon;
    // deprecated content that cites deprecated content raises nothing in canon
    let live = |l: &Local| -> Result<bool, RdyError> { Ok(!canon || !cx.deprecated(&l.block.id)?) };
    let base_edges = |l: &Local| -> Result<Vec<ForeignEdge>, RdyError> {
        Ok(match l.base {
            Some(v) => foreign_edges_of(&reader.get_block(&v)?),
            None => vec![],
        })
    };
    for l in &locals {
        let es = edges(l, pointing);
        if es.is_empty() || !live(l)? {
            continue;
        }
        let before = base_edges(l)?;
        for e in es {
            // in flight, only a new edge raises it (an old one is canon's)
            if before.iter().any(|o| o.to == e.to && o.kind == e.kind) {
                continue;
            }
            let Some(c) = ax.current_of(&e.to)? else {
                continue;
            };
            if c.deprecated() {
                let summary = format!(
                    "{} ({}) references deprecated {}",
                    l.item.block,
                    l.who(),
                    ax.shown(&c.repo, &c.block.id)
                );
                out.push(pair(kind, l, &c, summary));
            }
        }
    }
    // the mention form: a deprecated cited definition named in a local text without a tie
    let defs: Vec<&Cited> = ax
        .current()?
        .iter()
        .filter(|c| c.deprecated() && cx.defines(&c.block))
        .collect();
    if defs.is_empty() || locals.is_empty() {
        return Ok(());
    }
    let lexicon = Lexicon::build(cx.st.pack(), defs.iter().map(|c| &*c.block));
    let mut by_id: BTreeMap<String, Vec<&Cited>> = BTreeMap::new();
    for c in &defs {
        by_id.entry(c.block.id.to_string()).or_default().push(c);
    }
    // in the canon scope only a block that holds every word of a name can name it
    let holders: Option<BTreeSet<BlockId>> = if canon {
        let words = cx.lexicon()?;
        let mut ys = BTreeSet::new();
        for c in &defs {
            for (kind, p) in Lexicon::names_of(&c.block) {
                if kind != NameKind::Key {
                    ys.extend(words.blocks_with_words(&p));
                }
            }
        }
        Some(ys)
    } else {
        None
    };
    for l in &locals {
        if holders.as_ref().is_some_and(|h| !h.contains(&l.block.id)) || !live(l)? {
            continue;
        }
        let mentions = lexicon.mentions(&l.block);
        if mentions.is_empty() {
            continue;
        }
        let tied: BTreeSet<ForeignId> = foreign_edges_of(&l.block)
            .into_iter()
            .filter(|e| is_tie(&e.kind) || pointing(e))
            .map(|e| e.to)
            .collect();
        let named_before: BTreeSet<String> = match l.base {
            Some(v) => lexicon
                .mentions(&reader.get_block(&v)?)
                .into_iter()
                .map(|m| m.entity)
                .collect(),
            None => BTreeSet::new(),
        };
        let mut seen: BTreeSet<ForeignId> = BTreeSet::new();
        for m in mentions {
            if named_before.contains(&m.entity) {
                continue;
            }
            for c in by_id.get(&m.entity).into_iter().flatten() {
                let target = c.target();
                if tied.contains(&target) || !seen.insert(target) {
                    continue;
                }
                let summary = format!(
                    "{} ({}) names deprecated {} ({}) in its text",
                    l.item.block,
                    l.who(),
                    ax.shown(&c.repo, &c.block.id),
                    c.block.field_str("name").unwrap_or_default()
                );
                out.push(pair(kind, l, c, summary));
            }
        }
    }
    Ok(())
}

/// Whether `e` is a premise edge: a reference, or a suspect link other than `reflects`.
fn premise(e: &ForeignEdge) -> bool {
    match &e.kind {
        EdgeKind::References => true,
        EdgeKind::Link(n) => n != "reflects" && SUSPECT_LINKS.contains(&n.as_str()),
        EdgeKind::Contains => false,
    }
}

/// Premise shift across projects: in flight only.
pub(crate) fn premises(
    ax: &Across<'_, '_>,
    kind: TensionKind,
    seeds: Option<&BTreeSet<BlockId>>,
    out: &mut Vec<Found>,
) -> Result<(), RdyError> {
    if ax.cx.scope() != DetectScope::Inflight {
        return Ok(());
    }
    for l in ax.locals(seeds)? {
        for e in edges(&l, premise) {
            let Some(pin) = &e.pin else { continue };
            let Some(now) = ax.current_of(&e.to)? else {
                continue;
            };
            if now.pinned_by(pin) {
                continue;
            }
            let summary = format!(
                "{} changed in a delivery; {} ({}) still cites it at the pinned version",
                ax.shown(&now.repo, &now.block.id),
                l.item.block,
                l.who()
            );
            out.push(pair(kind, &l, &now, summary));
        }
    }
    Ok(())
}

/// One kind's form across projects: `f` run over the scope of `cx` (with `seeds`, over the local
/// blocks among them) and appended to `out`. Each detector of a kind that has one calls this from
/// [`Detector::across`] with its own reader. Nothing is read when no source delivered.
pub(crate) fn run(
    cx: &DetectCx<'_>,
    kind: TensionKind,
    seeds: Option<&BTreeSet<BlockId>>,
    out: &mut Vec<Found>,
    f: Reader,
) -> Result<(), RdyError> {
    if cx.st.foreign()?.is_empty() {
        return Ok(());
    }
    f(&Across::new(cx)?, kind, seeds, out)
}

/// A kind's reader across projects.
pub(crate) type Reader = fn(
    &Across<'_, '_>,
    TensionKind,
    Option<&BTreeSet<BlockId>>,
    &mut Vec<Found>,
) -> Result<(), RdyError>;

/// The canon form across projects for the kinds of `detectors`: each kind run in the canon
/// scope, kept when every side is canon or foreign, each marked by the live deviations. None, at
/// no cost, when no source delivered.
pub(crate) fn canon_tensions(
    st: &ReadState,
    detectors: &[&dyn Detector],
) -> Result<Vec<Arc<Tension>>, RdyError> {
    if st.foreign()?.is_empty() {
        return Ok(vec![]);
    }
    let writes = Writes::empty();
    let cx = DetectCx::new(st, &writes, canon_index_of(st)?, DetectScope::Canon);
    let devs = Deviations::cell();
    let mut out = vec![];
    for d in detectors {
        let mut found = vec![];
        d.across(&cx, None, &mut found)?;
        for f in found {
            if let Some(t) = tension_from(st.pack(), None, f, false, DetectScope::Canon) {
                out.push(Arc::new(mark(st, &devs, t)?));
            }
        }
    }
    Ok(out)
}

/// The live deviations of canon (D-8VJAZ): per cited target, the deviating decisions in force,
/// and every block that departs from it (a decision that departs from a target is not governed
/// by another deviation from it).
#[derive(Default)]
pub(crate) struct Deviations {
    live: BTreeMap<ForeignId, BlockId>,
    departing: BTreeMap<ForeignId, BTreeSet<BlockId>>,
}

/// A lazily read [`Deviations`].
pub(crate) type DeviationsCell = OnceCell<Result<Deviations, RdyError>>;

impl Deviations {
    /// An unread cell: the deviations are read on the first tension with a foreign side.
    pub(crate) fn cell() -> DeviationsCell {
        OnceCell::new()
    }

    fn read(st: &ReadState) -> Result<Deviations, RdyError> {
        let mut out = Deviations::default();
        for d in st.deviations(&Scope::Canon)? {
            out.departing
                .entry(d.target.clone())
                .or_default()
                .insert(d.decision.clone());
            if d.live {
                match out.live.get(&d.target) {
                    Some(have) if *have <= d.decision => {}
                    _ => {
                        out.live.insert(d.target, d.decision);
                    }
                }
            }
        }
        Ok(out)
    }

    /// The deviation that covers local block `b` (as its version holds it) against `target`:
    /// a live deviation from `target` governs `b` when `b` cites `target` and does not itself
    /// depart from it.
    fn covering(&self, b: &Block, target: &ForeignId) -> Option<&BlockId> {
        let d = self.live.get(target)?;
        let departs = self
            .departing
            .get(target)
            .is_some_and(|s| s.contains(&b.id));
        (b.foreign.pins.contains_key(target) && !departs).then_some(d)
    }
}

/// `t` with its deviation: when it has a local and a foreign side, and a live deviation covers
/// each pair of a local item and a foreign item, the smallest covering decision. A tension
/// without a foreign side is returned as it is, reading nothing.
pub(crate) fn mark(
    st: &ReadState,
    devs: &DeviationsCell,
    mut t: Tension,
) -> Result<Tension, RdyError> {
    if !t.has_foreign() {
        return Ok(t);
    }
    let devs = match devs.get_or_init(|| Deviations::read(st)) {
        Ok(d) => d,
        Err(e) => return Err(e.clone()),
    };
    if devs.live.is_empty() {
        return Ok(t);
    }
    let (foreign, local): (Vec<&Involved>, Vec<&Involved>) =
        t.involved.iter().partition(|i| i.side.is_foreign());
    if local.is_empty() {
        return Ok(t);
    }
    let reader = st.reader();
    let mut covered: Option<BlockId> = None;
    for l in local {
        let Some(v) = l.version else { return Ok(t) };
        let b = reader.get_block(&v)?;
        for f in &foreign {
            let SideRef::Foreign { repo, .. } = &f.side else {
                continue;
            };
            match devs.covering(&b, &ForeignId::new(*repo, f.block.clone())) {
                Some(d) => {
                    if covered.as_ref().is_none_or(|c| d < c) {
                        covered = Some(d.clone());
                    }
                }
                None => return Ok(t),
            }
        }
    }
    t.deviated = covered;
    Ok(t)
}
