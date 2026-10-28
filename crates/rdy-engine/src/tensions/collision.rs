//! Edit collision: two in-flight changes edit one block from one base, or an in-flight edit
//! against a canon version it did not build on.
//!
//! Each live write is judged with its chain: the in-flight writes of the block it builds on (a
//! write whose base is the version of an in-flight write its change lists in its deps), back to
//! the chain's root base. A view that writes a block twice edits it from its first base.
//!
//! The chain is then judged against canon as the merge does (hotfix 11cf3d3). It is against
//! canon when canon's version is neither its root base nor the version of its first write (the
//! merge reads the first write as clean only then, and a later chain write does not cover
//! canon's side of the conflict the first write records), and the chain is not built on
//! canon's side: it does not depend (directly, or through in-flight changes that write the
//! block) on the producer of canon's version or, when canon has no version of the block, on
//! the change that deleted it (as of canon's head). A canon version with no known producer is
//! settled by a chain that depends on another in-flight write of the block; a delete with no
//! known deleter is never undone. A canon version that differs from the root base only by its
//! order key is built on, as the merge reads it (D-TX3KH). A chain that is not against canon edits, for the merge, from
//! canon's version: that is its root, so a resolution pairs with a fresh edit from canon.
//!
//! (a) Every pair of live writes on one block from distinct changes with equal roots and
//! different outcomes, where neither chain lists the other change in its deps and the two are not
//! a member pair of one open alternative set (FW-CH6a: the set's own rules govern them). (b) A
//! live write whose chain is against canon. A single write with canon's outcome is its own first
//! write, so equal outcomes never collide. No canon-scope form (FW-CH6a): canon holds one
//! version per block, and the trigger needs in-flight work.
use super::{DetectCx, DetectScope, Detector, Found, SideRef, TensionKind, Write};
use crate::RdyError;
use rdy_model::{BlockId, Hash};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// This kind's detector.
pub struct EditCollision;

/// A live write's chain on its block: the root base (canon's version when the merge reads the
/// chain as built on it), the version of the chain's first write (the write whose base is the
/// root), the changes on the chain, the write's own change among them, and whether the merge
/// reads it as against canon's version.
struct Chain {
    root: Option<Hash>,
    first: Option<Hash>,
    changes: BTreeSet<Hash>,
    against: bool,
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
            let cv = canon.version_of(&b);
            let mut chains: Vec<Chain> = live.iter().map(|w| chain(cx, &all, w)).collect();
            if chains.iter().any(|ch| ch.root != cv && ch.first != cv) {
                // canon's side as the merge reads it: the producer of canon's version, or the
                // change that deleted the block (as of canon's head)
                let reader = cx.st.reader();
                let owner = match cv {
                    Some(v) => Owner::Producer(reader.producer(&v)?),
                    None => Owner::Deleter(reader.deleter_at(&b, cx.canon_head().as_ref())?),
                };
                for ch in &mut chains {
                    judge(cx, ch, &all, cv, &owner);
                }
            } else {
                for ch in &mut chains {
                    ch.root = cv;
                }
            }
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
            // (b) a live write whose chain the merge reads as against canon's version
            let against: Vec<&Arc<Write>> = live
                .iter()
                .zip(&chains)
                .filter(|(_, ch)| ch.against)
                .map(|(w, _)| w)
                .collect();
            if !against.is_empty() {
                let canon_side = match cx.canon_side(&b)? {
                    Some((side, v)) => cx.involved(side, &b, Some(v))?,
                    None => cx.involved(SideRef::Canon { landing: None }, &b, None)?,
                };
                for w in against {
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

/// Canon's side of a block as the merge reads it.
enum Owner {
    /// Canon holds the block: the producer of its version (`None`: no known producer, which
    /// the merge reads as settled).
    Producer(Option<Hash>),
    /// Canon lacks the block: the change that deleted it (`None`: no known deleter, which no
    /// change can undo).
    Deleter(Option<Hash>),
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
        against: false,
    }
}

/// Judges chain `ch` (among `all`, the writes on its block) against canon's version `cv`, held
/// by `owner`, as the merge does: the chain is against canon when its first write is (canon's
/// version is neither the root base nor the chain's first version) and the chain is not built
/// on canon's side. A chain that is not against canon edits, for the merge, from canon's
/// version, which becomes its root: a resolution pairs with a fresh edit from that version.
fn judge(cx: &DetectCx<'_>, ch: &mut Chain, all: &[Arc<Write>], cv: Option<Hash>, owner: &Owner) {
    // the chain's first write is an edit that keeps its base's place, and canon's version
    // differs from that base only by its order key (a landing's re-key, a landed move): the
    // merge applies the first write, so the chain is built on canon's version (D-TX3KH). A
    // move, a parent change, a re-key or a delete stays against canon (final ruling round 4).
    let order_only = || match (cv, ch.root, ch.first) {
        (Some(c), Some(r), Some(f)) => {
            let reader = cx.st.reader();
            reader
                .get_block(&f)
                .is_ok_and(|new| reader.order_only_edit(&c, &r, &new))
        }
        _ => false,
    };
    if ch.root == cv || ch.first == cv || order_only() {
        ch.root = cv;
        return;
    }
    let built_on = match owner {
        Owner::Producer(Some(p)) | Owner::Deleter(Some(p)) => chain_reaches(cx, ch, all, p),
        // a version with no known producer: the merge settles canon's side at the next write
        // of the block that covers the chain's own sides, as when a chain depends on another
        // in-flight write of the block
        Owner::Producer(None) => ch.changes.iter().any(|c| {
            all.iter()
                .any(|y| y.change != *c && depends(cx, c, &y.change))
        }),
        // a delete with no known deleter: no change can undo it
        Owner::Deleter(None) => false,
    };
    if built_on {
        ch.root = cv;
    } else {
        ch.against = true;
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
