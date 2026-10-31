//! Duplicate definition: blocks that define overlapping names, or have the same normative text.
//!
//! A name group is the blocks whose name keys overlap, directly or through other members: each
//! defining block has the [`name_keys_of`] its name and its aliases give (D-JDW8A, O4), and two
//! blocks that share a key are in one group, so an alias that bridges two names joins them into
//! one group, never two overlapping pairs. A text group is the blocks that share one normalized
//! text of at least the pack's `duplicate_text_min_words` words. A decision's text is its
//! `statement`, never its body (founder ruling 2026-10-08: register boilerplate such as
//! "Disposition: … / Trigger: …" makes bodies match across unrelated decisions); a decision
//! without a statement has no text. Any other normative block's text is its body; texts never
//! fold plurals. In flight, a group is its live writes plus the canon blocks no change writes.
//! FW-TN2: a group of two or more members is ONE tension whose involved items are every member,
//! so a membership change (a member added, edited away or removed) gives a new id and vkey and
//! voids a dismissal. In flight a group is kept when a member has a change side; in canon
//! (FW-CH6, FW-CH6a) a group of canon blocks is a standing condition, so duplicates that land
//! keep their vkey until one is renamed or removed. A text group whose members all lie in one
//! name group is that name group's tension, reported once, as the name.
use super::{
    DetectCx, DetectScope, Detector, Found, SideRef, TensionKind, Write, name_keys_of,
    normalize_name,
};
use crate::error::E_LIMIT;
use crate::{Materialized, RdyError};
use rdy_model::{Block, BlockId, Hash};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// This kind's detector.
pub struct DuplicateDefinition;

/// One block of a group: the side that holds it and its version there.
struct Member {
    block: BlockId,
    side: SideRef,
    version: Option<Hash>,
}

/// The group key: a name key or a normalized text.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Name(String),
    Text(Hash),
}

/// The most blocks one name group walk takes in.
pub(crate) const MAX_GROUP_BLOCKS: usize = 100_000;
/// The most (key, member) visits one name group walk makes.
pub(crate) const MAX_GROUP_VISITS: usize = 1_000_000;
/// The fix of a name group over a limit.
const GROUP_FIX: &str = "give the entities distinct names and aliases";

/// A connected name group: its members and every key they have.
#[derive(Debug)]
pub(crate) struct NameGroup<N> {
    pub members: BTreeSet<N>,
    pub keys: BTreeSet<String>,
}

/// The name group of key `start`: the members `entries` lists under it, the members listed
/// under each key those have (`keys`), and so on until no key is new. Each member listed under
/// a key is one visit. `E_LIMIT` before the visit past [`MAX_GROUP_VISITS`] and before the
/// member past [`MAX_GROUP_BLOCKS`] goes in.
pub(crate) fn name_group<N: Ord, I: IntoIterator<Item = N>>(
    start: &str,
    mut entries: impl FnMut(&str) -> I,
    mut keys: impl FnMut(&N) -> Vec<String>,
) -> Result<NameGroup<N>, RdyError> {
    let mut g = NameGroup {
        members: BTreeSet::new(),
        keys: BTreeSet::from([start.to_string()]),
    };
    let mut todo = vec![start.to_string()];
    let mut visits = 0;
    while let Some(k) = todo.pop() {
        for e in entries(&k) {
            if visits == MAX_GROUP_VISITS {
                return Err(RdyError::new(
                    E_LIMIT,
                    format!("duplicate-definition group exceeds {MAX_GROUP_VISITS} name visits"),
                    GROUP_FIX,
                ));
            }
            visits += 1;
            if g.members.contains(&e) {
                continue;
            }
            if g.members.len() == MAX_GROUP_BLOCKS {
                return Err(RdyError::new(
                    E_LIMIT,
                    format!("duplicate-definition group exceeds {MAX_GROUP_BLOCKS} blocks"),
                    GROUP_FIX,
                ));
            }
            for next in keys(&e) {
                if !g.keys.contains(&next) {
                    g.keys.insert(next.clone());
                    todo.push(next);
                }
            }
            g.members.insert(e);
        }
    }
    Ok(g)
}

/// One index entry of a name group: a canon block no change writes, or a live write.
#[derive(Clone)]
enum Entry {
    Canon(BlockId),
    Live(Arc<Write>),
}

impl Entry {
    fn block(&self) -> &BlockId {
        match self {
            Entry::Canon(b) => b,
            Entry::Live(w) => &w.block,
        }
    }

    fn order(&self) -> (u8, &BlockId, Option<&Hash>) {
        match self {
            Entry::Canon(b) => (0, b, None),
            Entry::Live(w) => (1, &w.block, Some(&w.change)),
        }
    }
}

impl PartialEq for Entry {
    fn eq(&self, other: &Entry) -> bool {
        self.order() == other.order()
    }
}

impl Eq for Entry {}

impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Entry) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Entry {
    fn cmp(&self, other: &Entry) -> Ordering {
        self.order().cmp(&other.order())
    }
}

/// The name group of key `start` in this run: canon's blocks that no change writes and the live
/// writes, joined by the name keys of canon's block (`canon` is canon's materialization) or of
/// the write's block.
fn component(
    cx: &DetectCx<'_>,
    canon: &Materialized,
    start: &str,
) -> Result<NameGroup<Entry>, RdyError> {
    let ix = cx.canon_index();
    let pack = cx.st.pack();
    name_group(
        start,
        |k: &str| {
            ix.names
                .get(k)
                .into_iter()
                .flatten()
                .filter(|id| cx.writes.on(id).is_empty())
                .map(|id| Entry::Canon(id.clone()))
                .chain(cx.writes.named(k).into_iter().map(Entry::Live))
        },
        |e: &Entry| {
            let b = match e {
                Entry::Canon(id) => canon.get(id),
                Entry::Live(w) => w.new.as_deref(),
            };
            b.map(|b| name_keys_of(pack, b)).unwrap_or_default()
        },
    )
}

/// A name group's canon blocks and live writes.
fn split(g: &NameGroup<Entry>) -> (Vec<BlockId>, Vec<Arc<Write>>) {
    let (mut canon, mut writes) = (vec![], vec![]);
    for e in &g.members {
        match e {
            Entry::Canon(b) => canon.push(b.clone()),
            Entry::Live(w) => writes.push(Arc::clone(w)),
        }
    }
    (canon, writes)
}

/// The members of a group: the live `writes` (one per block, the one that holds the block's
/// current version when there is one) and the `canon` blocks that no change writes.
fn members(
    cx: &DetectCx<'_>,
    canon: &[BlockId],
    writes: Vec<Arc<Write>>,
) -> Result<Vec<Member>, RdyError> {
    let mut by_block: BTreeMap<BlockId, Vec<Arc<Write>>> = BTreeMap::new();
    for w in writes {
        by_block.entry(w.block.clone()).or_default().push(w);
    }
    let mut out = vec![];
    for (block, ws) in by_block {
        let current = cx.inflight_m()?.version_of(&block);
        let w = ws.iter().find(|w| w.version == current).unwrap_or(&ws[0]);
        out.push(Member {
            block,
            side: SideRef::Change(w.change),
            version: w.version,
        });
    }
    for id in canon {
        if !cx.writes.all_on(id).is_empty() {
            continue;
        }
        if let Some((side, v)) = cx.canon_side(id)? {
            out.push(Member {
                block: id.clone(),
                side,
                version: Some(v),
            });
        }
    }
    out.sort_by(|a, b| a.block.cmp(&b.block));
    Ok(out)
}

/// A member's block: its write's (from `writes`), or canon's.
fn member_block<'b>(
    m: &Member,
    writes: &'b [Arc<Write>],
    canon: &'b Materialized,
) -> Option<&'b Block> {
    match m.side {
        SideRef::Change(c) => writes
            .iter()
            .find(|w| w.change == c && w.block == m.block)
            .and_then(|w| w.new.as_deref()),
        _ => canon.get(&m.block),
    }
}

/// Whether a group can hold two distinct blocks: an upper bound on `members`, from the index
/// alone. Most canon keys are singletons; `members` reads the store for every canon block, so a
/// group that cannot form a pair must not reach it.
fn can_pair(canon: &[BlockId], writes: &[Arc<Write>]) -> bool {
    let mut blocks: BTreeSet<&BlockId> = canon.iter().collect();
    blocks.extend(writes.iter().map(|w| &w.block));
    blocks.len() >= 2
}

/// The keys of block `b`: its name keys (when it defines a name) and its text key.
fn keys_of(cx: &DetectCx<'_>, b: &Block, out: &mut BTreeSet<Key>) {
    out.extend(name_keys_of(cx.st.pack(), b).into_iter().map(Key::Name));
    if let Some(h) = cx.text_key(b) {
        out.insert(Key::Text(h));
    }
}

/// The keys to examine: every key of the scope, or in flight with `seeds` every key a seed had
/// or has: those of every write on a seed (live or not) and of its canon version. In canon with
/// `seeds` (a landing's carry, plan M2e2), the keys of the seeds' canon versions.
fn keys(cx: &DetectCx<'_>, seeds: Option<&BTreeSet<BlockId>>) -> Result<BTreeSet<Key>, RdyError> {
    let mut out = BTreeSet::new();
    match (seeds, cx.scope()) {
        (None, _) => {
            for w in cx.writes.live() {
                if let Some(b) = w.new.as_deref() {
                    keys_of(cx, b, &mut out);
                }
            }
        }
        (Some(seeds), DetectScope::Inflight) => {
            let canon = cx.canon_m()?;
            for s in seeds {
                for w in cx.writes.all_on(s) {
                    if let Some(b) = w.new.as_deref() {
                        keys_of(cx, b, &mut out);
                    }
                }
                if let Some(b) = canon.get(s) {
                    keys_of(cx, b, &mut out);
                }
            }
        }
        (Some(seeds), DetectScope::Canon) => {
            let canon = cx.canon_m()?;
            for s in seeds {
                if let Some(b) = canon.get(s) {
                    keys_of(cx, b, &mut out);
                }
            }
        }
    }
    if seeds.is_none() && cx.scope() == DetectScope::Canon {
        let ix = cx.canon_index();
        out.extend(ix.names.keys().cloned().map(Key::Name));
        out.extend(ix.texts.keys().copied().map(Key::Text));
    }
    Ok(out)
}

/// How many members a summary names before "and N more".
const NAMED: usize = 5;

/// The members of a group as a summary names them: `A and B`, `A, B and C`, or the first
/// [`NAMED`] and `and N more`.
fn listed(group: &[Member]) -> String {
    let ids: Vec<String> = group.iter().map(|m| m.block.to_string()).collect();
    if ids.len() > NAMED {
        return format!("{} and {} more", ids[..NAMED].join(", "), ids.len() - NAMED);
    }
    match ids.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => ids.join(""),
    }
}

/// A text clipped to `n` characters, with an ellipsis when cut.
fn clipped(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut out: String = s.chars().take(n).collect();
    out.push('…');
    out
}

/// The name a name group's summary quotes: the least key two of its members share, or with
/// none (two writes of one block bridged it) the group's least key.
fn shared_key(
    cx: &DetectCx<'_>,
    g: &NameGroup<Entry>,
    group: &[Member],
    writes: &[Arc<Write>],
    canon: &Materialized,
) -> String {
    let mut count: BTreeMap<String, usize> = BTreeMap::new();
    for m in group {
        if let Some(b) = member_block(m, writes, canon) {
            for k in name_keys_of(cx.st.pack(), b) {
                *count.entry(k).or_default() += 1;
            }
        }
    }
    count
        .into_iter()
        .find(|(_, n)| *n >= 2)
        .map(|(k, _)| k)
        .or_else(|| g.keys.first().cloned())
        .unwrap_or_default()
}

impl Detector for DuplicateDefinition {
    fn kind(&self) -> TensionKind {
        TensionKind::DuplicateDefinition
    }

    /// A canon group is the canon blocks of one name group or one text: a landing changes only
    /// the groups of the keys its blocks had or have, and the carry re-detects around every
    /// member of those.
    fn canon_seeded(&self) -> bool {
        true
    }

    /// FW-TN2: a write on a seed can change the membership of every group the seed was or is
    /// in, so the carry re-detects around every block of those groups (from the index alone). A
    /// name group is walked whole from each key the seed had or has, so a removed bridge reaches
    /// both parts.
    fn reach(
        &self,
        cx: &DetectCx<'_>,
        seeds: &BTreeSet<BlockId>,
    ) -> Result<BTreeSet<BlockId>, RdyError> {
        let canon_m = cx.canon_m()?;
        let ix = cx.canon_index();
        let mut covered: BTreeSet<String> = BTreeSet::new();
        let mut out = BTreeSet::new();
        for key in keys(cx, Some(seeds))? {
            match &key {
                Key::Name(n) => {
                    if covered.contains(n) {
                        continue;
                    }
                    let g = component(cx, &canon_m, n)?;
                    out.extend(g.members.iter().map(|e| e.block().clone()));
                    for k in &g.keys {
                        out.extend(ix.names.get(k).into_iter().flatten().cloned());
                    }
                    covered.extend(g.keys);
                }
                Key::Text(h) => {
                    out.extend(ix.texts.get(h).into_iter().flatten().cloned());
                    out.extend(cx.writes.texted(h).iter().map(|w| w.block.clone()));
                }
            }
        }
        Ok(out)
    }

    fn detect(
        &self,
        cx: &DetectCx<'_>,
        seeds: Option<&BTreeSet<BlockId>>,
        out: &mut Vec<Found>,
    ) -> Result<(), RdyError> {
        // names before texts (`Key`'s order): a text group whose members all lie in one name
        // group is reported once, as that name
        let canon_m = cx.canon_m()?;
        let ix = cx.canon_index();
        let mut covered: BTreeSet<String> = BTreeSet::new();
        for key in keys(cx, seeds)? {
            let (group, what) = match &key {
                Key::Name(n) => {
                    if covered.contains(n) {
                        continue;
                    }
                    let g = component(cx, &canon_m, n)?;
                    covered.extend(g.keys.iter().cloned());
                    let (canon, writes) = split(&g);
                    if !can_pair(&canon, &writes) {
                        continue;
                    }
                    let group = members(cx, &canon, writes.clone())?;
                    if group.len() < 2 {
                        continue;
                    }
                    let verb = if group.len() == 2 { "both" } else { "all" };
                    let name = shared_key(cx, &g, &group, &writes, &canon_m);
                    let what = format!("{verb} define \"{}\"", clipped(&name, 80));
                    (group, what)
                }
                Key::Text(h) => {
                    let canon = ix.texts.get(h).map_or(&[][..], Vec::as_slice);
                    let writes = cx.writes.texted(h);
                    if !can_pair(canon, &writes) {
                        continue;
                    }
                    let group = members(cx, canon, writes.clone())?;
                    if group.len() < 2 {
                        continue;
                    }
                    let blocks: Vec<Option<&Block>> = group
                        .iter()
                        .map(|m| member_block(m, &writes, &canon_m))
                        .collect();
                    if one_name_group(cx, &canon_m, &group, &blocks)? {
                        continue;
                    }
                    let text = blocks
                        .iter()
                        .find_map(|b| b.and_then(|b| cx.duplicate_text(b)))
                        .map(normalize_name);
                    let what = format!(
                        "have the same text \"{}\"",
                        clipped(&text.unwrap_or_default(), 80)
                    );
                    (group, what)
                }
            };
            let anchored =
                cx.canon_groups() || group.iter().any(|m| matches!(m.side, SideRef::Change(_)));
            if !anchored {
                continue;
            }
            let mut stable = false;
            for m in &group {
                if cx.stable(&m.block)? {
                    stable = true;
                    break;
                }
            }
            out.push(Found {
                kind: TensionKind::DuplicateDefinition,
                summary: format!("{} {what}", listed(&group)),
                stable,
                involved: group
                    .iter()
                    .map(|m| cx.involved(m.side.clone(), &m.block, m.version))
                    .collect::<Result<_, _>>()?,
            });
        }
        Ok(())
    }
}

/// Whether every member of a text `group` (with its `blocks`) defines a name and all of them lie
/// in one name group: that name group's tension reports them.
fn one_name_group(
    cx: &DetectCx<'_>,
    canon: &Materialized,
    group: &[Member],
    blocks: &[Option<&Block>],
) -> Result<bool, RdyError> {
    let mut first = None;
    for b in blocks {
        match b.map(|b| name_keys_of(cx.st.pack(), b)) {
            Some(keys) if !keys.is_empty() => {
                if first.is_none() {
                    first = keys.into_iter().next();
                }
            }
            _ => return Ok(false),
        }
    }
    let Some(start) = first else {
        return Ok(false);
    };
    let g = component(cx, canon, &start)?;
    let inside: BTreeSet<&BlockId> = g.members.iter().map(Entry::block).collect();
    Ok(group.iter().all(|m| inside.contains(&m.block)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(change: &[u8], block: &BlockId) -> Arc<Write> {
        Arc::new(Write {
            change: Hash::of(change),
            block: block.clone(),
            base: None,
            version: None,
            new: None,
            live: true,
        })
    }

    #[test]
    fn can_pair_needs_two_distinct_blocks_before_any_store_read() {
        let (a, b) = (BlockId::new("blk"), BlockId::new("blk"));
        assert!(!can_pair(&[], &[]), "empty group");
        assert!(!can_pair(std::slice::from_ref(&a), &[]), "one canon block");
        assert!(!can_pair(&[], &[write(b"1", &a)]), "one written block");
        assert!(
            !can_pair(
                std::slice::from_ref(&a),
                &[write(b"1", &a), write(b"2", &a)]
            ),
            "one block, however many writes and canon entries"
        );
        assert!(can_pair(&[a.clone(), b.clone()], &[]), "two canon blocks");
        assert!(
            can_pair(std::slice::from_ref(&a), &[write(b"1", &b)]),
            "canon and a write"
        );
        assert!(
            can_pair(&[], &[write(b"1", &a), write(b"2", &b)]),
            "two writes"
        );
    }

    /// The walk of one group: keys `0..k`, each listing every member `0..n` (pulled one by one,
    /// counted in `pulled`); member 0 has every key, any other member key 0 only.
    fn walk(n: u32, k: u32, pulled: &std::cell::Cell<usize>) -> Result<NameGroup<u32>, RdyError> {
        name_group(
            "0",
            |_key: &str| (0..n).inspect(|_| pulled.set(pulled.get() + 1)),
            |m: &u32| {
                if *m == 0 {
                    (0..k).map(|i| i.to_string()).collect()
                } else {
                    vec!["0".to_string()]
                }
            },
        )
    }

    #[test]
    fn nb1_name_group_bounds_are_checked_before_growth() {
        let pulled = std::cell::Cell::new(0);
        let g = walk(MAX_GROUP_BLOCKS as u32, 1, &pulled).expect("at the block limit");
        assert_eq!(g.members.len(), MAX_GROUP_BLOCKS);
        assert_eq!(g.keys.len(), 1);

        pulled.set(0);
        let e = walk(MAX_GROUP_BLOCKS as u32 + 5, 1, &pulled).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                crate::error::E_LIMIT,
                "duplicate-definition group exceeds 100000 blocks",
                "give the entities distinct names and aliases"
            )
        );
        assert_eq!(
            pulled.get(),
            MAX_GROUP_BLOCKS + 1,
            "stops at the first excess block"
        );

        // ten keys over 100,000 members: exactly 1,000,000 visits
        pulled.set(0);
        let g = walk(MAX_GROUP_BLOCKS as u32, 10, &pulled).expect("at the visit limit");
        assert_eq!((g.members.len(), g.keys.len()), (MAX_GROUP_BLOCKS, 10));
        assert_eq!(pulled.get(), MAX_GROUP_VISITS);

        pulled.set(0);
        let e = walk(MAX_GROUP_BLOCKS as u32, 11, &pulled).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                crate::error::E_LIMIT,
                "duplicate-definition group exceeds 1000000 name visits",
                "give the entities distinct names and aliases"
            )
        );
        assert_eq!(
            pulled.get(),
            MAX_GROUP_VISITS + 1,
            "stops at the first excess visit"
        );
    }
}
