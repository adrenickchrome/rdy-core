//! D-Y9DNZ, TASK-982 O1: the decisions a version's selected changes carry with an accepted or
//! rejected status recorded elsewhere, and who imported them.
//!
//! The walk reads the selected changes in proposal order, op by op. A change is imported when it
//! records an importer (`links.imported_by`, written by `rdy import`) or replays one: its
//! `replayed_from` chain, followed for at most [`MAX_REPLAY_STEPS`] edges, reaches a change that
//! records one. A cycle or a 65th edge is `E_LIMIT`; a source that cannot be read is its store
//! error.
//!
//! An imported create of a decision accepted or rejected, or an imported update that gives a
//! decision that status from another, makes the decision a member, with its importer. A later
//! write that keeps the status keeps it a member; a write with another status (or of a block
//! that is no longer a decision) or a delete takes it out. At the end a member counts only while
//! the candidate holds it as a decision with that status. Nothing outside the selected changes is
//! read: an imported decision that canon already holds and the version does not touch is no
//! member.
use crate::error::{E_LIMIT, RdyError};
use crate::readiness::check::MAX_REPLAY_STEPS;
use crate::{Materialized, ReadState};
use rdy_model::decision::{self, DecisionStatus};
use rdy_model::{ActorRun, BlockId, Change, Hash, Op};
use std::collections::{BTreeMap, BTreeSet};

/// The `E_LIMIT` message of a replay chain past [`MAX_REPLAY_STEPS`] edges, or a cycle.
pub const CHAIN_LIMIT: &str = "the import source chain exceeds 64 replay steps";
/// The fix of [`CHAIN_LIMIT`].
pub const CHAIN_LIMIT_FIX: &str =
    "exclude the replayed change and import the source tree into a new view";

/// What the walk found: the member decisions, distinct and in id order, and their importers,
/// distinct, by actor and then run (no run first), each compared as bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ImportedFacts {
    pub ids: Vec<BlockId>,
    pub importers: Vec<ActorRun>,
}

impl ImportedFacts {
    /// The `Imported` card fact's value: `<n> decisions recorded as accepted or rejected
    /// elsewhere, by <importer>` (`1 decision` for one), each importer's actor once, in
    /// [`ImportedFacts::importers`]' order, joined by `, `. None with no member.
    pub(crate) fn fact_value(&self) -> Option<String> {
        if self.ids.is_empty() {
            return None;
        }
        let mut actors: Vec<&str> = vec![];
        for a in &self.importers {
            if actors.last() != Some(&a.actor.as_str()) {
                actors.push(&a.actor);
            }
        }
        let what = match self.ids.len() {
            1 => "1 decision".to_string(),
            n => format!("{n} decisions"),
        };
        Some(format!(
            "{what} recorded as accepted or rejected elsewhere, by {}",
            actors.join(", ")
        ))
    }
}

/// The imported decision status of the selected changes `changes` (in proposal order), read in
/// `candidate`, the canon that landing them would give (see the module docs).
pub(crate) fn imported_facts(
    st: &ReadState,
    changes: &[Hash],
    candidate: &Materialized,
) -> Result<ImportedFacts, RdyError> {
    let reader = st.reader();
    let read = crate::gate::changes_of(st, changes)?;
    let source = |h: &Hash| reader.change(h).map_err(RdyError::from);
    let base_status = |h: &Hash| -> Result<Option<DecisionStatus>, RdyError> {
        Ok(decision::status(&reader.get_block(h)?))
    };
    walk(&read, candidate, source, base_status)
}

/// One importer, as the fact orders it: the actor, then the run (none first).
type Importer = (String, Option<String>);

/// The walk, with the store reads given: `source` reads a replay source, `base_status` the
/// decision status of an update's base version.
fn walk(
    changes: &[Change],
    candidate: &Materialized,
    source: impl Fn(&Hash) -> Result<Change, RdyError>,
    base_status: impl Fn(&Hash) -> Result<Option<DecisionStatus>, RdyError>,
) -> Result<ImportedFacts, RdyError> {
    let mut members: BTreeMap<BlockId, (DecisionStatus, BTreeSet<Importer>)> = BTreeMap::new();
    for c in changes {
        // the change's importer, read once and only when an op needs it
        let mut importer: Option<Option<Importer>> = None;
        for op in &c.ops {
            let block = match op {
                Op::Delete { id, .. } => {
                    members.remove(id);
                    continue;
                }
                Op::Create { block } | Op::Update { block, .. } => block,
            };
            let now = decision::status(block);
            let decided = matches!(
                now,
                Some(DecisionStatus::Accepted | DecisionStatus::Rejected)
            );
            let assigns = decided
                && match op {
                    Op::Update { base, .. } => base_status(base)? != now,
                    _ => true,
                };
            let by = if assigns {
                if importer.is_none() {
                    importer = Some(source_importer(c, &source)?);
                }
                importer.clone().flatten()
            } else {
                None
            };
            match (by, now) {
                (Some(by), Some(status)) => {
                    let entry = members
                        .entry(block.id.clone())
                        .or_insert_with(|| (status, BTreeSet::new()));
                    if entry.0 != status {
                        *entry = (status, BTreeSet::new());
                    }
                    entry.1.insert(by);
                }
                _ => {
                    if members.get(&block.id).is_none_or(|(s, _)| Some(*s) != now) {
                        members.remove(&block.id);
                    }
                }
            }
        }
    }
    members.retain(|id, (status, _)| {
        candidate
            .get(id)
            .is_some_and(|b| decision::status(b) == Some(*status))
    });
    let importers: BTreeSet<Importer> = members.values().flat_map(|(_, by)| by.clone()).collect();
    Ok(ImportedFacts {
        ids: members.into_keys().collect(),
        importers: importers
            .into_iter()
            .map(|(actor, run)| ActorRun { actor, run })
            .collect(),
    })
}

/// The importer of change `c`: its own `imported_by`, else its replay chain's, followed for at
/// most [`MAX_REPLAY_STEPS`] edges. None when the chain ends at a change that records no
/// importer. A cycle or a 65th edge is `E_LIMIT`; a source `source` cannot read is its error.
fn source_importer(
    c: &Change,
    source: &impl Fn(&Hash) -> Result<Change, RdyError>,
) -> Result<Option<Importer>, RdyError> {
    let of = |c: &Change| {
        c.links
            .imported_by
            .as_ref()
            .map(|a| (a.actor.clone(), a.run.clone()))
    };
    if let Some(by) = of(c) {
        return Ok(Some(by));
    }
    let mut seen: BTreeSet<Hash> = BTreeSet::new();
    let mut next = c.links.replayed_from;
    for _ in 0..MAX_REPLAY_STEPS {
        let Some(src) = next else {
            return Ok(None);
        };
        if !seen.insert(src) {
            return Err(chain_limit());
        }
        let s = source(&src)?;
        if let Some(by) = of(&s) {
            return Ok(Some(by));
        }
        next = s.links.replayed_from;
    }
    match next {
        Some(_) => Err(chain_limit()),
        None => Ok(None),
    }
}

fn chain_limit() -> RdyError {
    RdyError::new(E_LIMIT, CHAIN_LIMIT, CHAIN_LIMIT_FIX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdy_model::{Block, ChangeLinks, FieldValue, Provenance, Role};
    use std::collections::HashMap;

    fn h(n: u8) -> Hash {
        Hash::of(&[n])
    }

    fn change(imported_by: Option<&str>, replayed_from: Option<Hash>, ops: Vec<Op>) -> Change {
        Change {
            view: "v".into(),
            ops,
            deps: vec![],
            provenance: Provenance {
                actor: "agent:w".into(),
                run: None,
                role: Role::Writer,
                at_ms: 0,
            },
            justification: None,
            message: String::new(),
            links: ChangeLinks {
                imported_by: imported_by.map(|a| ActorRun {
                    actor: a.into(),
                    run: None,
                }),
                replayed_from,
                ..ChangeLinks::default()
            },
            read_identity: None,
        }
    }

    fn decision(id: &str, status: &str) -> Block {
        Block {
            id: BlockId::parse(id).unwrap(),
            block_type: "decision".into(),
            parent: None,
            order: "a1".into(),
            fields: [("status".to_string(), FieldValue::Str(status.into()))]
                .into_iter()
                .collect(),
            body: String::new(),
            links: Default::default(),
            pins: Default::default(),
            read_identity: None,
        }
    }

    fn run(changes: &[Change], store: &HashMap<Hash, Change>) -> Result<ImportedFacts, RdyError> {
        let candidate = Materialized::default().with(&[Op::Create {
            block: decision("D-AAAAA", "accepted"),
        }]);
        walk(
            changes,
            &candidate,
            |s: &Hash| {
                store
                    .get(s)
                    .cloned()
                    .ok_or_else(|| RdyError::new("E_NOT_FOUND", "missing", "none"))
            },
            |_| Ok(None),
        )
    }

    fn create() -> Vec<Op> {
        vec![Op::Create {
            block: decision("D-AAAAA", "accepted"),
        }]
    }

    /// A chain of `edges` replay steps from a change that replays `h(1)` back to the import,
    /// stored as `h(1)`..`h(edges)`; returns the selected change.
    fn chain(edges: u8) -> (Change, HashMap<Hash, Change>) {
        let mut store = HashMap::new();
        store.insert(h(edges), change(Some("agent:i"), None, create()));
        for k in 1..edges {
            store.insert(h(k), change(None, Some(h(k + 1)), create()));
        }
        (change(None, Some(h(1)), create()), store)
    }

    #[test]
    fn ui2_a_replay_chain_of_64_edges_keeps_its_importer_and_a_65th_or_a_cycle_refuses() {
        let (c, store) = chain(64);
        let got = run(&[c], &store).unwrap();
        assert_eq!(got.ids, vec![BlockId::parse("D-AAAAA").unwrap()]);
        assert_eq!(got.importers[0].actor, "agent:i");
        let (c, store) = chain(65);
        let e = run(&[c], &store).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (E_LIMIT, CHAIN_LIMIT, CHAIN_LIMIT_FIX)
        );
        // a cycle: h(1) replays h(2), which replays h(1)
        let mut store = HashMap::new();
        store.insert(h(1), change(None, Some(h(2)), create()));
        store.insert(h(2), change(None, Some(h(1)), create()));
        let e = run(&[change(None, Some(h(1)), create())], &store).unwrap_err();
        assert_eq!((e.code, e.message.as_str()), (E_LIMIT, CHAIN_LIMIT));
        // a source that cannot be read is its own error
        let e = run(&[change(None, Some(h(9)), create())], &HashMap::new()).unwrap_err();
        assert_eq!(e.code, "E_NOT_FOUND");
    }

    /// TASK-982 O1: membership is net of the later writes, in proposal order. A delete or a
    /// write with another status takes a decision out; a write that keeps the status keeps it; an
    /// imported update counts only when it assigns the status; a repeated import counts once and
    /// adds its importer; a member the candidate holds with another status does not count.
    #[test]
    fn ui2_membership_is_net_of_later_writes_in_proposal_order() {
        let d = |id: &str, status: &str| decision(id, status);
        let create = |id: &str, status: &str| Op::Create {
            block: d(id, status),
        };
        let update = |base: u8, id: &str, status: &str| Op::Update {
            base: h(base),
            block: d(id, status),
        };
        let mut edited = d("D-YYYYY", "accepted");
        edited.body = "edited".into();
        let changes = vec![
            change(
                Some("agent:i"),
                None,
                vec![
                    create("D-AAAAA", "accepted"),
                    create("D-RRRRR", "rejected"),
                    create("D-XXXXX", "accepted"),
                    create("D-YYYYY", "accepted"),
                    create("D-PPPPP", "proposed"),
                    create("D-QQQQQ", "accepted"),
                ],
            ),
            change(
                None,
                None,
                vec![
                    Op::Delete {
                        base: h(1),
                        id: BlockId::parse("D-RRRRR").unwrap(),
                    },
                    update(2, "D-XXXXX", "rejected"),
                    Op::Update {
                        base: h(3),
                        block: edited.clone(),
                    },
                ],
            ),
            change(
                Some("agent:j"),
                None,
                vec![
                    update(4, "D-ZZZZZ", "accepted"),
                    update(5, "D-WWWWW", "accepted"),
                ],
            ),
            change(Some("agent:k"), None, vec![create("D-AAAAA", "accepted")]),
        ];
        let candidate = Materialized::default().with(&[
            create("D-AAAAA", "accepted"),
            create("D-XXXXX", "rejected"),
            Op::Create { block: edited },
            create("D-PPPPP", "proposed"),
            create("D-QQQQQ", "rejected"),
            create("D-ZZZZZ", "accepted"),
            create("D-WWWWW", "accepted"),
        ]);
        // the bases' statuses: X and Y accepted, Z proposed, W accepted
        let base = |b: &Hash| -> Result<Option<DecisionStatus>, RdyError> {
            Ok(if *b == h(4) {
                Some(DecisionStatus::Proposed)
            } else {
                Some(DecisionStatus::Accepted)
            })
        };
        let got = walk(
            &changes,
            &candidate,
            |_: &Hash| Err(RdyError::new("E_NOT_FOUND", "missing", "none")),
            base,
        )
        .unwrap();
        let ids: Vec<String> = got.ids.iter().map(ToString::to_string).collect();
        assert_eq!(ids, ["D-AAAAA", "D-YYYYY", "D-ZZZZZ"]);
        let actors: Vec<&str> = got.importers.iter().map(|a| a.actor.as_str()).collect();
        assert_eq!(actors, ["agent:i", "agent:j", "agent:k"]);
        assert_eq!(
            got.fact_value().unwrap(),
            "3 decisions recorded as accepted or rejected elsewhere, by agent:i, agent:j, agent:k"
        );
    }

    #[test]
    fn ui2_the_fact_names_each_actor_once_in_byte_order() {
        let ar = |a: &str, r: Option<&str>| ActorRun {
            actor: a.into(),
            run: r.map(str::to_string),
        };
        let f = ImportedFacts {
            ids: vec![BlockId::parse("D-AAAAA").unwrap()],
            importers: vec![
                ar("agent:a", None),
                ar("agent:a", Some("r1")),
                ar("agent:b", None),
            ],
        };
        assert_eq!(
            f.fact_value().unwrap(),
            "1 decision recorded as accepted or rejected elsewhere, by agent:a, agent:b"
        );
        assert_eq!(ImportedFacts::default().fact_value(), None);
    }
}
