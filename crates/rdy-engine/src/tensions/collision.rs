//! Edit collision: two in-flight changes edit one block from one base, or an in-flight edit
//! against a canon version it did not build on.
//!
//! Each live write is judged with its chain: the in-flight writes of the block it builds on (a
//! write whose base is the version of an in-flight write its change lists in its deps), back to
//! the chain's root base. A view that writes a block twice edits it from its first base.
//!
//! (a) Every pair of live writes on one block from distinct changes with equal root bases and
//! different outcomes, where neither chain lists the other change in its deps and the two are not
//! a member pair of one open alternative set (FW-CH6a: the set's own rules govern them). (b) A
//! live write whose chain meets canon's version only after its first write (canon's version is
//! neither the root base nor the version of the chain's first write: the merge reads the first
//! write as clean only then, and a later chain write does not cover canon's side of the conflict
//! the first write records), and whose chain does not depend on canon's version: on the producer
//! of canon's version (directly, or through in-flight changes that write the block), or, when canon has no version of the block, on an in-flight write of the
//! block (the merge reads that as the resolution of the delete). A single write with canon's
//! outcome is its own first write, so equal outcomes never collide. No canon-scope form
//! (FW-CH6a): canon holds one version per block, and the trigger needs in-flight work.
use super::{DetectCx, DetectScope, Detector, Found, SideRef, TensionKind, Write};
use crate::RdyError;
use rdy_model::{BlockId, Hash};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// This kind's detector.
pub struct EditCollision;

/// A live write's chain on its block: the root base, the version of the chain's first write (the
/// write whose base is the root), and the changes on the chain, the write's own change among them.
struct Chain {
    root: Option<Hash>,
    first: Option<Hash>,
    changes: BTreeSet<Hash>,
}

impl Detector for EditCollision {
    fn kind(&self) -> TensionKind {
        TensionKind::EditCollision
    }

    fn detect(
        &self,
        cx: &DetectCx<'_>,
        seeds: Option<&BTreeSet<BlockId>>,
        out: &mut Vec<Found>,
    ) -> Result<(), RdyError> {
        if cx.scope() == DetectScope::Canon {
            return Ok(());
        }
        let blocks: Vec<BlockId> = match seeds {
            Some(s) => s.iter().cloned().collect(),
            None => {
                let mut v: Vec<BlockId> = cx.writes.live().map(|w| w.block.clone()).collect();
                v.dedup();
                v
            }
        };
        let members = &cx.st.coord_heads().alt_members;
        let canon = cx.canon_m()?;
        for b in blocks {
            let live = cx.writes.live_on(&b);
            if live.is_empty() {
                continue;
            }
            let all = cx.writes.all_on(&b);
            let chains: Vec<Chain> = live.iter().map(|w| chain(cx, &all, w)).collect();
            let mut found = vec![];
            // (a) pairs from one root base, in change-id order
            for (i, (w1, ch1)) in live.iter().zip(&chains).enumerate() {
                for (w2, ch2) in live[i + 1..].iter().zip(&chains[i + 1..]) {
                    if w1.change == w2.change
                        || w1.version == w2.version
                        || ch1.root != ch2.root
                        || chain_depends(cx, ch1, &w2.change)
                        || chain_depends(cx, ch2, &w1.change)
                        || member_pair(cx, members, &w1.change, &w2.change)
                    {
                        continue;
                    }
                    found.push((
                        vec![cx.of_write(w1), cx.of_write(w2)],
                        format!("two changes edit {b} from one base"),
                    ));
                }
            }
            // (b) a live write whose chain's first write is against canon's version
            let cv = canon.version_of(&b);
            let stale = |ch: &Chain| ch.root != cv && ch.first != cv;
            let against: Vec<(&Arc<Write>, &Chain)> = live
                .iter()
                .zip(&chains)
                .filter(|(_, ch)| stale(ch))
                .collect();
            if !against.is_empty() {
                let producer = match cv {
                    Some(v) => cx.st.reader().producer(&v)?,
                    None => None,
                };
                let canon_side = match cx.canon_side(&b)? {
                    Some((side, v)) => cx.involved(side, &b, Some(v))?,
                    None => cx.involved(SideRef::Canon { landing: None }, &b, None)?,
                };
                for (w, ch) in against {
                    let built_on = match (cv, producer) {
                        (Some(_), Some(p)) => chain_reaches(cx, ch, &all, &p),
                        (Some(_), None) => false,
                        // canon deleted b: a side with no change counts as covered in the merge,
                        // so a chain that depends on an in-flight write of b resolves the delete
                        (None, _) => ch.changes.iter().any(|c| {
                            all.iter()
                                .any(|y| y.change != *c && depends(cx, c, &y.change))
                        }),
                    };
                    if built_on {
                        continue;
                    }
                    found.push((
                        vec![cx.of_write(w), canon_side.clone()],
                        format!(
                            "{b} changed in canon since change {} edited it",
                            w.change.short()
                        ),
                    ));
                }
            }
            if found.is_empty() {
                continue;
            }
            let stable = cx.stable(&b)?;
            out.extend(found.into_iter().map(|(involved, summary)| Found {
                kind: TensionKind::EditCollision,
                involved,
                summary,
                stable,
            }));
        }
        Ok(())
    }
}

/// Live write `w`'s chain among `all`, the writes on its block: walk back while the current base
/// is the version of an in-flight write whose change the current change lists in its deps.
fn chain(cx: &DetectCx<'_>, all: &[Arc<Write>], w: &Write) -> Chain {
    let mut changes = BTreeSet::from([w.change]);
    let (mut root, mut first, mut at) = (w.base, w.version, w.change);
    while let Some(x) = all
        .iter()
        .find(|x| !changes.contains(&x.change) && x.version == root && depends(cx, &at, &x.change))
    {
        changes.insert(x.change);
        root = x.base;
        first = x.version;
        at = x.change;
    }
    Chain {
        root,
        first,
        changes,
    }
}

/// Whether in-flight change `c` lists `d` in its deps.
fn depends(cx: &DetectCx<'_>, c: &Hash, d: &Hash) -> bool {
    cx.writes.change(c).is_some_and(|f| f.deps.contains(d))
}

/// Whether some change on chain `ch` lists `d` in its deps.
fn chain_depends(cx: &DetectCx<'_>, ch: &Chain, d: &Hash) -> bool {
    ch.changes.iter().any(|c| depends(cx, c, d))
}

/// Whether a change on chain `ch` reaches `p` through deps, walking only the changes of `all`
/// (the in-flight writes of the block): the merge drops a side once a change that lists it in
/// its deps is applied to the block, so a chain that depends on such a change is built on that
/// side, as when it lists it itself.
fn chain_reaches(cx: &DetectCx<'_>, ch: &Chain, all: &[Arc<Write>], p: &Hash) -> bool {
    let writers: BTreeSet<Hash> = all.iter().map(|w| w.change).collect();
    let mut todo: Vec<Hash> = ch.changes.iter().copied().collect();
    let mut seen: BTreeSet<Hash> = todo.iter().copied().collect();
    while let Some(c) = todo.pop() {
        let Some(f) = cx.writes.change(&c) else {
            continue;
        };
        for d in &f.deps {
            if d == p {
                return true;
            }
            if writers.contains(d) && seen.insert(*d) {
                todo.push(*d);
            }
        }
    }
    false
}

/// Whether changes `c1` and `c2` are a member pair: every open view of each is a member view of
/// one open alternative set, the same for both. A change that a non-member view also holds is
/// not a member's change alone, so it still collides with the members.
fn member_pair(cx: &DetectCx<'_>, members: &BTreeMap<String, Hash>, c1: &Hash, c2: &Hash) -> bool {
    let set = |c: &Hash| -> Option<Hash> {
        let views = &cx.writes.change(c)?.views;
        let s = *members.get(views.first()?)?;
        views
            .iter()
            .all(|v| members.get(v) == Some(&s))
            .then_some(s)
    };
    set(c1).is_some_and(|s| set(c2) == Some(s))
}
