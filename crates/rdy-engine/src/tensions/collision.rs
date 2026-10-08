//! Edit collision: two in-flight changes edit one block from one base, or an in-flight edit
//! against a canon version it did not build on.
//!
//! (a) Every pair of live writes on one block from distinct changes with equal bases, where
//! neither change lists the other in its deps and the two are not members of one open
//! alternative set (FW-CH6a: the set's own rules govern them). (b) A live write whose base is
//! stale against canon, is the version of no in-flight write on the block, and whose change does
//! not depend on the producer of canon's version. No canon-scope form (FW-CH6a): canon holds one
//! version per block, and the trigger needs in-flight work.
use super::{DetectCx, DetectScope, Detector, Found, Involved, SideRef, TensionKind};
use crate::RdyError;
use rdy_model::{BlockId, Hash};
use std::collections::{BTreeMap, BTreeSet};

/// This kind's detector.
pub struct EditCollision;

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
            let mut found = vec![];
            // (a) pairs from one base, in change-id order
            for (i, w1) in live.iter().enumerate() {
                for w2 in &live[i + 1..] {
                    if w1.change == w2.change
                        || w1.base != w2.base
                        || depends(cx, &w1.change, &w2.change)
                        || depends(cx, &w2.change, &w1.change)
                        || member_pair(cx, members, &w1.change, &w2.change)
                    {
                        continue;
                    }
                    found.push((
                        vec![
                            Involved {
                                side: SideRef::Change(w1.change),
                                block: b.clone(),
                                version: w1.version,
                            },
                            Involved {
                                side: SideRef::Change(w2.change),
                                block: b.clone(),
                                version: w2.version,
                            },
                        ],
                        format!("two changes edit {b} from one base"),
                    ));
                }
            }
            // (b) a live write against a canon version it did not build on
            let cv = canon.version_of(&b);
            let stale = |base: &Option<Hash>| match base {
                None => cv.is_some(),
                Some(_) => cv != *base,
            };
            if live.iter().any(|w| stale(&w.base)) {
                let all = cx.writes.all_on(&b);
                let producer = match cv {
                    Some(v) => cx.st.reader().producer(&v)?,
                    None => None,
                };
                let canon_side = match cx.canon_side(&b)? {
                    Some((side, v)) => (side, Some(v)),
                    None => (SideRef::Canon { landing: None }, None),
                };
                for w in &live {
                    if !stale(&w.base)
                        || all.iter().any(|x| x.version == w.base)
                        || producer.is_some_and(|p| depends(cx, &w.change, &p))
                    {
                        continue;
                    }
                    found.push((
                        vec![
                            Involved {
                                side: SideRef::Change(w.change),
                                block: b.clone(),
                                version: w.version,
                            },
                            Involved {
                                side: canon_side.0.clone(),
                                block: b.clone(),
                                version: canon_side.1,
                            },
                        ],
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

/// Whether in-flight change `c` lists `d` in its deps.
fn depends(cx: &DetectCx<'_>, c: &Hash, d: &Hash) -> bool {
    cx.writes.change(c).is_some_and(|f| f.deps.contains(d))
}

/// Whether changes `c1` and `c2` are a member pair: some view of each is a member view of one
/// open alternative set.
fn member_pair(cx: &DetectCx<'_>, members: &BTreeMap<String, Hash>, c1: &Hash, c2: &Hash) -> bool {
    let sets = |c: &Hash| -> BTreeSet<Hash> {
        cx.writes
            .change(c)
            .map(|f| {
                f.views
                    .iter()
                    .filter_map(|v| members.get(v).copied())
                    .collect()
            })
            .unwrap_or_default()
    };
    let s1 = sets(c1);
    !s1.is_empty() && !s1.is_disjoint(&sets(c2))
}
