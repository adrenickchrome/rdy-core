//! The citing side's pure inspection reads for a host that embeds rdy core (D-DAPR8, D-Y49PX,
//! D-4W1ZR, D-0DNKT, D-WDKWQ; hub tb-G28CM rows 78 to 83).
//!
//! Every method here is a read of one captured [`Snapshot`]: it writes nothing, takes no lock
//! and asks no clock. Core gives facts; the host applies its own rules. No MCP tool and no CLI
//! command carries any of them. Each result names the cited repo, and where it has one the
//! FOREIGN entry that delivered the version it reports, so a host can trace a fact to its
//! delivery.
//!
//! | Method | Fact |
//! |---|---|
//! | [`Snapshot::foreign_refs`] | the refs and pins to cited content, by cited repo |
//! | [`Snapshot::selection_changes`] | what a link's deliveries changed in its selection |
//! | [`Snapshot::foreign_blast_radius`] | the local blocks that cite changed blocks, with owners |
//! | [`Snapshot::foreign_signals`] | the foreign suspects and the open tensions across projects |
//! | [`Snapshot::foreign_tension_gate`] | the open foreign tensions of a landing request version |
//! | [`Snapshot::lock_facts`] | new deviations from locked instructions in a landing request |
//! | [`Snapshot::lock_changes`] | a set owner's changes to locks in a landing request |
//! | [`Snapshot::unadopted_locked`] | the delivered lock changes the adopted baseline lacks |
//! | [`Snapshot::link_adoption`] | each repo link's newest delivery and adopted delivery |
//!
//! A landing request is named by its proposal version: the hash of one version of a proposal.
//! An unknown version is `E_NOT_FOUND`.

use crate::store::Snapshot;
use rdy_engine::policy::{live_policy, owners::owner_entry};
use rdy_engine::{E_BAD_INPUT, E_LIMIT, RdyError, Scope};
use rdy_model::foreign::{ForeignEvent, ForeignId, ForeignPin, ForeignSet, ForeignSource};
use rdy_model::{Block, BlockId, Hash, RepoId};
use rdy_schema::instruction::INSTRUCTION_PREFIX;
use rdy_schema::repo_link::REPO_LINK;
use rdy_store::foreign::MAX_CHAIN;
use std::collections::{BTreeMap, BTreeSet};

pub use rdy_engine::foreign_facts::ForeignGateFact;
pub use rdy_engine::foreign_locks::{LockChange, LockEdit, LockFact, UnadoptedLock};

/// One citation of a cited block by a local block: [`Snapshot::foreign_refs`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForeignRef {
    /// The local block that cites.
    pub citer: BlockId,
    /// The cited block.
    pub target: ForeignId,
    /// The pin the citer holds; none for a link a block holds with no pin.
    pub pin: Option<ForeignPin>,
    /// The delivered version the pin names, when a delivery before the snapshot held it.
    pub held: Option<Hash>,
    /// The version that the live sources of the repo deliver now, when one does.
    pub current: Option<Hash>,
    /// The link kinds through which the citer cites the target, in name order.
    pub links: Vec<String>,
    /// Whether the pin is an instruction baseline pin of a repo link: it cites nothing.
    pub baseline: bool,
    /// The ref name of the source whose entry first delivered the held version.
    pub source: Option<String>,
    /// The FOREIGN entry that first delivered the held version.
    pub entry: Option<Hash>,
}

/// The citations of one cited repo: [`Snapshot::foreign_refs`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CitedRepoRefs {
    pub repo: RepoId,
    /// The live aliases of the repo, in sorted order.
    pub aliases: Vec<String>,
    /// The citations, sorted by citing block, then cited block.
    pub refs: Vec<ForeignRef>,
}

/// How one cited block moved in a link's selection: [`Snapshot::selection_changes`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SelectionEdit {
    /// The window's end selects it and its start did not.
    Added,
    /// Both select it, at different versions.
    Changed,
    /// The window's start selected it and its end does not.
    Removed,
}

/// One cited block whose selected version differs between the two ends of a window of a link's
/// deliveries: [`Snapshot::selection_changes`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionChange {
    pub repo: RepoId,
    /// The repo link block whose source delivered it.
    pub link: BlockId,
    /// The cited block.
    pub id: BlockId,
    pub edit: SelectionEdit,
    /// The selected version at the window's start, none when none was selected.
    pub before: Option<Hash>,
    /// The selected version at the window's end, none when none is selected.
    pub after: Option<Hash>,
    /// The newest entry in the window that touched the block.
    pub entry: Hash,
}

/// One local block that cites a changed cited block: [`Snapshot::foreign_blast_radius`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlastBlock {
    /// The citing block.
    pub block: BlockId,
    /// The changed cited blocks it cites, in id order.
    pub cites: Vec<ForeignId>,
    /// The owners the authority policy names for the block, in the entry's order. Empty with no
    /// policy, or when no entry covers the block.
    pub owners: Vec<String>,
    /// The `path` of the owner entry the owners came from.
    pub owner_path: Option<String>,
}

/// A suspect that follows a foreign edge: [`Snapshot::foreign_signals`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForeignSuspectFact {
    /// The local block that cites.
    pub block: BlockId,
    /// The cited block.
    pub target: ForeignId,
    /// `changed`, `deleted`, `superseded` or `revoked`.
    pub reason: String,
    /// The superseding block, when the cited repo's view holds it.
    pub successor: Option<BlockId>,
    /// The FOREIGN entry that delivered the version in force now, else the one that delivered the
    /// version the citer pins.
    pub entry: Option<Hash>,
}

/// The signals across projects of canon: [`Snapshot::foreign_signals`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForeignSignals {
    /// The suspects, in citing block then cited block order.
    pub suspects: Vec<ForeignSuspectFact>,
    /// One fact per foreign side of each open tension of canon that has one.
    pub tensions: Vec<ForeignGateFact>,
}

/// The state of a repo link's source: [`Snapshot::link_adoption`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkState {
    /// No delivery has arrived.
    None,
    /// A live source has delivered.
    Delivered,
    /// A revocation ended the source.
    Revoked,
}

/// The newest delivery of a link: [`Snapshot::link_adoption`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LastDelivery {
    /// The FOREIGN entry of the delivery.
    pub entry: Hash,
    /// The time the delivery was sent, in milliseconds since the epoch; 0 for a source with none.
    pub at_ms: i64,
    /// The set it serves: none, the organization (`organization`) or a team (`team:<name>`).
    pub set: Option<ForeignSet>,
    /// How many instructions it selects. A first delivery with none raises no adopt item.
    pub instructions: usize,
}

/// The delivery the link's last landed adopt answer named: [`Snapshot::link_adoption`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdoptedDelivery {
    pub entry: Hash,
    pub at_ms: i64,
    pub set: Option<ForeignSet>,
}

/// One repo link and where its deliveries stand against its adoptions: see
/// [`Snapshot::link_adoption`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkAdoption {
    /// The repo link block.
    pub link: BlockId,
    pub alias: String,
    /// The cited repo.
    pub repo: RepoId,
    pub state: LinkState,
    /// The newest delivery; absent before the first. A revoked link keeps its last delivery.
    pub last: Option<LastDelivery>,
    /// The delivery that the link's last landed adopt answer named; absent when no adopt answer
    /// moved the baseline.
    pub adopted: Option<AdoptedDelivery>,
}

fn bad_input(what: String, fix: &str) -> RdyError {
    RdyError::new(E_BAD_INPUT, what, fix)
}

fn limit() -> RdyError {
    RdyError::new(
        E_LIMIT,
        "a source's delivery history exceeds its record limit",
        "ask the source for a smaller history",
    )
}

/// The repo and alias of `b` when it is a repo link with both fields.
fn link_of(b: &Block) -> Option<(String, RepoId)> {
    if b.block_type != REPO_LINK {
        return None;
    }
    let repo = b.field_str("repo")?.parse().ok()?;
    Some((b.field_str("alias")?.to_string(), repo))
}

fn is_instruction(id: &BlockId) -> bool {
    id.prefix() == INSTRUCTION_PREFIX
}

/// The pins of `b` that its repo link holds as its adopted instruction baseline.
fn baseline_targets(b: &Block) -> BTreeSet<&ForeignId> {
    let Some((_, repo)) = link_of(b) else {
        return BTreeSet::new();
    };
    b.foreign
        .pins
        .keys()
        .filter(|t| t.repo == repo && is_instruction(&t.id))
        .collect()
}

impl Snapshot {
    /// The delivery time of `entry`: its link source's `at_ms`, else 0.
    fn entry_at(&self, entry: &Hash) -> Result<i64, RdyError> {
        Ok(
            match self.read_state().reader().foreign_entry(entry)?.source {
                ForeignSource::Link { at_ms, .. } => at_ms,
                ForeignSource::Adr { .. } => 0,
            },
        )
    }

    /// The proposal version `landing` and the changes it holds.
    fn landing_changes(&self, landing: &Hash) -> Result<Vec<Hash>, RdyError> {
        Ok(self.read_state().proposal_version(landing)?.changes)
    }

    /// The refs and pins of canon to cited content, by cited repo (hub row 78): the list of the
    /// repos this project cites, each with its live aliases and its citations. A citation is a
    /// pin, a foreign link or both; a repo link's pins of its repo's instructions are its adopted
    /// baseline and are marked `baseline`. A redacted block lists none. Repos are in id order. A
    /// project with no citation lists none.
    pub fn foreign_refs(&self) -> Result<Vec<CitedRepoRefs>, RdyError> {
        let st = self.read_state();
        let foreign = st.foreign()?;
        let m = st.materialized(&Scope::Canon)?;
        let mut aliases: BTreeMap<RepoId, BTreeSet<String>> = BTreeMap::new();
        for b in m.iter() {
            if let Some((alias, repo)) = link_of(b) {
                aliases.entry(repo).or_default().insert(alias);
            }
        }
        let mut out: BTreeMap<RepoId, Vec<ForeignRef>> = BTreeMap::new();
        for b in m
            .iter()
            .filter(|b| !b.foreign.is_empty() && !b.is_redacted())
        {
            let baseline = baseline_targets(b);
            let mut kinds: BTreeMap<&ForeignId, BTreeSet<&str>> = BTreeMap::new();
            for (kind, links) in &b.foreign.links {
                for l in links {
                    kinds.entry(&l.target).or_default().insert(kind);
                }
            }
            let targets: BTreeSet<&ForeignId> =
                b.foreign.pins.keys().chain(kinds.keys().copied()).collect();
            for target in targets {
                let pin = b.foreign.pins.get(target).cloned();
                let held = match &pin {
                    Some(p) => st.foreign_held_pin(target, p)?,
                    None => None,
                };
                let carrier = match held {
                    Some(v) => st.foreign_carrier(target, &v)?,
                    None => None,
                };
                out.entry(target.repo).or_default().push(ForeignRef {
                    citer: b.id.clone(),
                    target: target.clone(),
                    pin,
                    held,
                    current: foreign.current_version(target).map(|h| h.version.version),
                    links: kinds
                        .get(target)
                        .map(|k| k.iter().map(|s| s.to_string()).collect())
                        .unwrap_or_default(),
                    baseline: baseline.contains(target),
                    source: carrier.as_ref().map(|(s, _)| s.clone()),
                    entry: carrier.map(|(_, e)| e),
                });
            }
        }
        Ok(out
            .into_iter()
            .map(|(repo, mut refs)| {
                refs.sort_by(|a, b| (&a.citer, &a.target).cmp(&(&b.citer, &b.target)));
                CitedRepoRefs {
                    repo,
                    aliases: aliases
                        .get(&repo)
                        .map(|a| a.iter().cloned().collect())
                        .unwrap_or_default(),
                    refs,
                }
            })
            .collect())
    }

    /// What the deliveries of the repo link block `link` changed in its selection, from the
    /// selection after the entry `since` to the selection of its newest delivery (hub rows 78 and
    /// 79's inputs): one row per cited block whose selected version differs, in id order. With
    /// `since` none the window starts before the first delivery, so every selected block is
    /// `Added`. A revocation changes nothing: a revoked link reads its frozen last selection. A
    /// link with no delivery lists none. A `since` that is not an entry of the link's delivery
    /// history is `E_BAD_INPUT`.
    pub fn selection_changes(
        &self,
        link: &BlockId,
        since: Option<Hash>,
    ) -> Result<Vec<SelectionChange>, RdyError> {
        let st = self.read_state();
        let Some(view) = st.foreign()?.link_source(link) else {
            return Ok(vec![]);
        };
        let reader = st.reader();
        // the chain, newest first
        let mut chain: Vec<(Hash, ForeignEvent)> = vec![];
        let mut next = Some(view.head);
        while let Some(h) = next {
            if chain.len() >= MAX_CHAIN {
                return Err(limit());
            }
            let event = reader.foreign_entry(&h)?.event;
            next = match &event {
                ForeignEvent::Delivery { prev, .. } => *prev,
                ForeignEvent::Revoked { prev } => Some(*prev),
                ForeignEvent::Removal { .. } => None,
            };
            chain.push((h, event));
        }
        if let Some(s) = since
            && !chain.iter().any(|(h, _)| *h == s)
        {
            return Err(bad_input(
                format!("entry {s} is not in the delivery history of link {link}"),
                "name an entry that the link's source delivered, or omit it",
            ));
        }
        let mut selected: BTreeMap<BlockId, Hash> = BTreeMap::new();
        let mut start: Option<BTreeMap<BlockId, Hash>> = since.is_none().then(BTreeMap::new);
        let mut touched: BTreeMap<BlockId, Hash> = BTreeMap::new();
        for (h, event) in chain.into_iter().rev() {
            if let ForeignEvent::Delivery {
                added,
                changed,
                removed,
                ..
            } = event
            {
                for v in added.into_iter().chain(changed) {
                    touched.insert(v.id.clone(), h);
                    selected.insert(v.id, v.version);
                }
                for id in removed {
                    selected.remove(&id);
                    touched.insert(id, h);
                }
            }
            if Some(h) == since {
                start = Some(selected.clone());
                touched.clear();
            }
        }
        let start = start.unwrap_or_default();
        let ids: BTreeSet<&BlockId> = start.keys().chain(selected.keys()).collect();
        let mut out = vec![];
        for id in ids {
            let (before, after) = (start.get(id).copied(), selected.get(id).copied());
            let edit = match (before, after) {
                (None, Some(_)) => SelectionEdit::Added,
                (Some(_), None) => SelectionEdit::Removed,
                (Some(b), Some(a)) if b != a => SelectionEdit::Changed,
                _ => continue,
            };
            out.push(SelectionChange {
                repo: view.repo,
                link: link.clone(),
                id: id.clone(),
                edit,
                before,
                after,
                entry: touched.get(id).copied().unwrap_or(view.head),
            });
        }
        Ok(out)
    }

    /// The local blocks of canon that would go suspect if the cited blocks `changed` of `repo`
    /// changed (hub row 79), each with the owners the live authority policy names for it: one row
    /// per citing block, in id order, each with the changed blocks it cites. A block cites
    /// through a pin or a foreign link; a repo link's instruction baseline pins cite nothing and
    /// a redacted block lists none. The owners are the most specific `owners` entry of the policy
    /// that covers the block; with no policy that reads, or no entry, they are empty.
    pub fn foreign_blast_radius(
        &self,
        repo: &RepoId,
        changed: &[BlockId],
    ) -> Result<Vec<BlastBlock>, RdyError> {
        let st = self.read_state();
        let m = st.materialized(&Scope::Canon)?;
        let policy = live_policy(st, &m).and_then(|l| l.fields);
        let changed: BTreeSet<&BlockId> = changed.iter().collect();
        let mut out = vec![];
        for b in m
            .iter()
            .filter(|b| !b.foreign.is_empty() && !b.is_redacted())
        {
            let baseline = baseline_targets(b);
            let cites: BTreeSet<&ForeignId> = b
                .foreign
                .pins
                .keys()
                .chain(b.foreign.links.values().flatten().map(|l| &l.target))
                .filter(|t| t.repo == *repo && changed.contains(&t.id) && !baseline.contains(*t))
                .collect();
            if cites.is_empty() {
                continue;
            }
            let entry = policy.as_ref().and_then(|p| owner_entry(&m, p, &b.id));
            out.push(BlastBlock {
                block: b.id.clone(),
                cites: cites.into_iter().cloned().collect(),
                owners: entry.map(|e| e.owners.clone()).unwrap_or_default(),
                owner_path: entry.map(|e| e.path.clone()),
            });
        }
        out.sort_by(|a, b| a.block.cmp(&b.block));
        Ok(out)
    }

    /// The signals across projects of canon (hub rows 72 and 73): the suspects that follow a
    /// foreign edge, and one fact per foreign side of each open tension of canon that has one.
    /// A suspect names the entry that delivered the version in force now, else the entry that
    /// delivered the version its citer pins. A dismissed tension, and one that a live deviation
    /// resolves, lists no fact. A project with no delivery has none.
    pub fn foreign_signals(&self) -> Result<ForeignSignals, RdyError> {
        let st = self.read_state();
        let foreign = st.foreign()?;
        if foreign.is_empty() {
            return Ok(ForeignSignals {
                suspects: vec![],
                tensions: vec![],
            });
        }
        let m = st.materialized(&Scope::Canon)?;
        let mut suspects = vec![];
        for s in st.foreign_suspects(&Scope::Canon)? {
            let entry = match foreign.current_version(&s.target) {
                Some(h) => Some(h.entry),
                None => {
                    let pin = m
                        .get(&s.block)
                        .and_then(|b| b.foreign.pins.get(&s.target).cloned());
                    let held = match &pin {
                        Some(p) => st.foreign_held_pin(&s.target, p)?,
                        None => None,
                    };
                    match held {
                        Some(v) => st.foreign_carrier(&s.target, &v)?.map(|(_, e)| e),
                        None => None,
                    }
                }
            };
            suspects.push(ForeignSuspectFact {
                block: s.block,
                target: s.target,
                reason: s.reason,
                successor: s.successor,
                entry,
            });
        }
        let rows: Vec<_> = st
            .canon_tensions()?
            .into_iter()
            .filter(|t| t.has_foreign())
            .map(|t| {
                let status = st.tension_status(&t);
                (t, status)
            })
            .collect();
        Ok(ForeignSignals {
            suspects,
            tensions: rdy_engine::foreign_facts::foreign_tension_gate(&rows, &BTreeMap::new()),
        })
    }

    /// The open tensions with a foreign side that the landing request version `landing` has with
    /// its written blocks (hub row 80): one fact per foreign side, with the tension's kind and
    /// severity, the cited repo, the entry that delivered the side, the cited block and the
    /// version it holds, the local blocks of the tension and the author's answer to its plan
    /// item. The answer is read from the version's stored assessment; a version with none, or an
    /// item the author has not answered, has none. A dismissed tension, and one that a live
    /// deviation resolves, gives no fact. Sorted by tension, repo, entry and cited block.
    pub fn foreign_tension_gate(&self, landing: &Hash) -> Result<Vec<ForeignGateFact>, RdyError> {
        let st = self.read_state();
        let changes = self.landing_changes(landing)?;
        let reader = st.reader();
        let mut answers = BTreeMap::new();
        if let Some(h) = reader.assessment_of_proposal(landing, st.seq())? {
            for t in reader.assessment(&h)?.tensions {
                if let Some(a) = t.answer {
                    answers.insert(t.id, a);
                }
            }
        }
        let rows: Vec<_> = st
            .tensions_of_changes(&changes)?
            .into_iter()
            .filter(|t| t.has_foreign())
            .map(|t| {
                let status = st.tension_status(&t);
                (t, status)
            })
            .collect();
        Ok(rdy_engine::foreign_facts::foreign_tension_gate(
            &rows, &answers,
        ))
    }

    /// The new deviations from locked cited instructions in force that the landing request
    /// version `landing` writes (hub row 82): for each decision a change creates or updates, each
    /// `deviates_from` target that its base did not depart from and that is locked in force. A
    /// lock has force only through an adopted organization or team set. In change order, then
    /// decision and target order.
    pub fn lock_facts(&self, landing: &Hash) -> Result<Vec<LockFact>, RdyError> {
        self.read_state()
            .lock_facts(&self.landing_changes(landing)?)
    }

    /// The changes that the landing request version `landing` makes to locks or to locked
    /// instructions, as the set owner's project records them (hub row 82): each instruction a
    /// change creates, updates or deletes, read against its base. In change order, then op order.
    pub fn lock_changes(&self, landing: &Hash) -> Result<Vec<LockChange>, RdyError> {
        self.read_state()
            .lock_changes(&self.landing_changes(landing)?)
    }

    /// The delivered lock changes that the adopted baseline does not hold, with their delivery
    /// times (hub row 83): for each live, unrevoked repo link, what its newest delivery changes
    /// against the instructions its last adopt answer put in force. A first delivery's locked
    /// instructions count as changes against the empty baseline, with that delivery's time
    /// (D-MEMMQ). In alias order, then instruction order.
    pub fn unadopted_locked(&self) -> Result<Vec<UnadoptedLock>, RdyError> {
        self.read_state().unadopted_locked()
    }

    /// One row per repo link block of canon: where its deliveries stand against its adoptions
    /// (D-0DNKT, hub rdyhub TASK-1914). The hub reads facts here and applies its own rule.
    ///
    /// - `state` is `None` before the first delivery, `Delivered` while a live source delivers
    ///   and `Revoked` after a revocation.
    /// - `last` is the newest delivery: its entry, its time, its set and how many instructions
    ///   it selects. It is absent before the first delivery, and a revoked link keeps it. A first
    ///   delivery that selects no instruction counts: `instructions` is 0.
    /// - `adopted` is the delivery that the link's last landed adopt answer named, with its time
    ///   and set. It is absent when no adopt answer has landed. A keep answer leaves it as it
    ///   was, and so does a delivery that changes only derived facts. A delivery that removes
    ///   every instruction, once adopted, is still the adopted one: its baseline is empty.
    ///
    /// The rows are in alias order, then block id order. It is a pure read of canon and the
    /// foreign cut.
    pub fn link_adoption(&self) -> Result<Vec<LinkAdoption>, RdyError> {
        let st = self.read_state();
        let foreign = st.foreign()?;
        let m = st.materialized(&Scope::Canon)?;
        let reader = st.reader();
        let mut out = vec![];
        for b in m.iter() {
            let Some((alias, repo)) = link_of(b) else {
                continue;
            };
            let mut row = LinkAdoption {
                link: b.id.clone(),
                alias,
                repo,
                state: LinkState::None,
                last: None,
                adopted: None,
            };
            if let Some(view) = foreign.link_source(&b.id) {
                // a revocation ends a chain: the last delivery is the one it follows
                let last = match reader.foreign_entry(&view.head)?.event {
                    ForeignEvent::Revoked { prev } => prev,
                    _ => view.head,
                };
                row.state = if view.revoked {
                    LinkState::Revoked
                } else {
                    LinkState::Delivered
                };
                row.last = Some(LastDelivery {
                    entry: last,
                    at_ms: self.entry_at(&last)?,
                    set: view.set.clone(),
                    instructions: view.members.keys().filter(|id| is_instruction(id)).count(),
                });
                if let Some((entry, set)) = st.adopted_delivery(&repo, view.head)? {
                    row.adopted = Some(AdoptedDelivery {
                        entry,
                        at_ms: self.entry_at(&entry)?,
                        set,
                    });
                }
            }
            out.push(row);
        }
        out.sort_by(|a, b| (&a.alias, &a.link).cmp(&(&b.alias, &b.link)));
        Ok(out)
    }
}
