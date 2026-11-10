//! Adopt cards (repo links, D-8VJAZ, D-MEMMQ): what a delivery from a linked project changes in
//! what this project cites or follows, and the one question it asks: adopt it, keep the approved
//! pins, or later.
//!
//! The cards are internal for now. [`ReadState::adopt_cards`] builds them, but the queue never
//! lists one, and an answer to one is refused with the unsupported-answer refusal and no write,
//! until the answer path enables both together.
//!
//! # Membership
//!
//! One card for each live `repo_link` of canon whose source is not revoked, bound to the link's
//! newest delivery (its head entry). The card asks about everything delivered since the link's
//! last answer: the newest entry of the source's chain that has an `adopt` or `keep_pin` answer
//! (the item `adopt:<repo>@<entry>`), or the first entry when none has. A delivery before that
//! answer is settled. So a keep takes the card out until a later delivery changes something
//! again, and an adopt moves the pins, which takes the rows out of the card.
//!
//! The card exists when the delivery since that answer:
//! - changes or removes a block that canon pins (a pin row: the held version and the delivered
//!   one, or none when the block is no longer delivered); or
//! - gives a superseded cited block a successor that the repo's view holds (a rebind row: the
//!   citing block, the cited block and its successor). A delivery that changes only such derived
//!   facts moves no pin and offers only the rebind; or
//! - touches an instruction of the link or changes the delivery's set, and the link's
//!   instructions or set differ from the adopted baseline (an instruction row for each
//!   instruction that is added, changed or removed, and the set). The baseline instructions
//!   are the versions the link block pins, and none when it pins none; the baseline set is the
//!   set at the entry of the last `adopt` answer (D-MEMMQ: a keep leaves none of the rules in
//!   force, and an adopt answer does not put every delivered instruction in force).
//!
//! # The card
//!
//! Ref `adopt:<repo>@<entry short>`: the repo's 32 hex digits and the first 12 of the entry's.
//! Title `Adopt the delivery from <alias>`. Facts, in order: `Delivery`, `Link`, then one fact for
//! each pin row (old and new version and the word diff of the cited text), `Clears suspects`,
//! one for each rebind, one for each instruction row, `Set` and `Open plans` (each open proposal
//! version that pins a block of a pin row). The members are the citing blocks. Options: `adopt`,
//! `keep` and `later`, with no recommendation. The card never blocks.
//!
//! # The binding
//!
//! [`PlanCardBinding::Adopt`] binds the repo, the link, the full entry and every row, also those
//! the text cuts, so the card hash binds the whole delivery and diff.
use super::item::{Fact, Item, ItemKind, age};
use super::plan::PlanCardBinding;
use super::queue::{PendingQuery, answered, finish, item, later, option, text};
use super::snooze::hide_snoozed;
use crate::error::{E_INTERNAL, E_LIMIT, RdyError};
use crate::foreign_refs::RepoLinks;
use crate::session::{now_ms, rfc3339};
use crate::worddiff::{self, Op as PieceOp, Unit};
use crate::{ReadState, Scope};
use rdy_graph::foreign::ForeignSuspect;
use rdy_model::foreign::{ForeignEvent, ForeignId, ForeignPin, ForeignSet, ForeignSource};
use rdy_model::{AnswerAct, Block, BlockId, Hash, Op, RepoId};
use rdy_schema::instruction::INSTRUCTION_PREFIX;
use rdy_store::foreign::{HeldVersion, MAX_CHAIN, MAX_LIST};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// The prefix of an adopt card's ref.
pub const ADOPT_REF_PREFIX: &str = "adopt:";
/// The why line of every adopt card.
pub const ADOPT_WHY: &str = "a linked project delivered a change that this project has not adopted";
/// The label of the option that adopts the delivery (`adopt`).
pub const ADOPT_LABEL: &str = "Adopt the delivery";
/// The label of the option that keeps the approved pins (`keep`).
pub const KEEP_PIN_LABEL: &str = "Keep the approved pins";
/// The longest word diff one fact shows, in characters.
const DIFF_MAX: usize = 400;
/// The hex digits of an entry in a ref.
const ENTRY_SHORT: usize = 12;

/// The ref of the adopt card of delivery `entry` from `repo`: `adopt:<repo>@<entry short>`.
pub fn adopt_ref(repo: &RepoId, entry: &Hash) -> String {
    format!(
        "{ADOPT_REF_PREFIX}{repo}@{}",
        &entry.to_string()[..ENTRY_SHORT]
    )
}

/// Whether `r` is an adopt item ref: `adopt:` and 32 lowercase hex digits, `@`, and 12.
pub fn is_adopt_ref(r: &str) -> bool {
    let hex = |s: &str, n: usize| {
        s.len() == n && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    };
    r.strip_prefix(ADOPT_REF_PREFIX)
        .and_then(|p| p.split_once('@'))
        .is_some_and(|(repo, entry)| hex(repo, 32) && hex(entry, ENTRY_SHORT))
}

/// What a delivery did to one block of a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdoptChange {
    Added,
    Changed,
    Removed,
}

impl AdoptChange {
    fn word(self) -> &'static str {
        match self {
            AdoptChange::Added => "added",
            AdoptChange::Changed => "changed",
            AdoptChange::Removed => "removed",
        }
    }
}

/// One cited block whose delivered content differs from what canon pins, or that no delivery
/// selects now: the held version, the delivered one (`None` when the block is no longer
/// delivered) and the blocks that cite it with that pin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AdoptPin {
    pub target: ForeignId,
    #[serde(with = "rdy_store::transition::hex::opt")]
    pub old: Option<Hash>,
    #[serde(with = "rdy_store::transition::hex::opt")]
    pub new: Option<Hash>,
    pub citers: Vec<BlockId>,
}

/// One block that cites a superseded cited block whose successor the repo's view holds.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct AdoptRebind {
    pub citer: BlockId,
    pub target: ForeignId,
    pub successor: BlockId,
}

/// One suspect that adopting the pin rows clears: a citing block and the cited block whose pin
/// moves.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct AdoptCleared {
    pub citer: BlockId,
    pub target: ForeignId,
}

/// One instruction of the link that differs from the adopted baseline: the baseline version
/// (`None` when it is not in force) and the delivered one (`None` when removed).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AdoptInstruction {
    pub id: BlockId,
    pub change: AdoptChange,
    #[serde(with = "rdy_store::transition::hex::opt")]
    pub old: Option<Hash>,
    #[serde(with = "rdy_store::transition::hex::opt")]
    pub new: Option<Hash>,
}

/// The set the baseline served and the set the delivery serves.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AdoptSet {
    pub from: Option<ForeignSet>,
    pub to: Option<ForeignSet>,
}

/// One open proposal version whose changes pin a cited block that a pin row moves or removes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct AdoptPlan {
    pub proposal: String,
    #[serde(with = "rdy_store::transition::hex::one")]
    pub version: Hash,
    pub targets: Vec<ForeignId>,
}

/// One delivery after the link's last answer, as the card reads it.
struct Visit {
    set: Option<ForeignSet>,
    touched: Vec<BlockId>,
}

/// What a walk of one source's chain found.
#[derive(Default)]
struct Walk {
    /// The deliveries after the last answer, newest first.
    after: Vec<Visit>,
    /// The entry of the last `adopt` or `keep_pin` answer, and its set.
    answered: Option<(Hash, Option<ForeignSet>)>,
    /// The entry of the last `adopt` answer, with the set it served.
    baseline: Option<(Hash, Option<ForeignSet>)>,
}

fn is_instruction(id: &BlockId) -> bool {
    id.prefix() == INSTRUCTION_PREFIX
}

fn limit() -> RdyError {
    RdyError::new(
        E_LIMIT,
        "adopt card exceeds its record limit",
        "ask the source for a smaller selection",
    )
}

/// The act of the answer to the item of delivery `entry` when it is an adopt or a keep.
fn settled(st: &ReadState, repo: &RepoId, entry: &Hash) -> Result<Option<AnswerAct>, RdyError> {
    let r = adopt_ref(repo, entry);
    for act in [AnswerAct::Adopt, AnswerAct::KeepPin] {
        if answered(st, &r, act)? {
            return Ok(Some(act));
        }
    }
    Ok(None)
}

/// Walks the chain of the source whose newest entry is `head`, newest first: the deliveries
/// after the last answer, then the last answer, and the set of the last adopt.
fn walk(st: &ReadState, repo: &RepoId, head: Hash) -> Result<Walk, RdyError> {
    let reader = st.reader();
    let mut w = Walk::default();
    let mut next = Some(head);
    let mut steps = 0usize;
    while let Some(h) = next {
        steps += 1;
        if steps > MAX_CHAIN {
            return Err(limit());
        }
        let e = reader.foreign_entry(&h)?;
        let ForeignEvent::Delivery {
            prev,
            set,
            added,
            changed,
            removed,
            ..
        } = e.event
        else {
            break;
        };
        next = prev;
        if w.baseline.is_none() {
            let act = settled(st, repo, &h)?;
            if w.answered.is_none() {
                match act {
                    Some(_) => w.answered = Some((h, set.clone())),
                    None => w.after.push(Visit {
                        set: set.clone(),
                        touched: added
                            .iter()
                            .chain(&changed)
                            .map(|v| v.id.clone())
                            .chain(removed.iter().cloned())
                            .collect(),
                    }),
                }
            }
            if act == Some(AnswerAct::Adopt) {
                w.baseline = Some((h, set.clone()));
            }
        }
    }
    Ok(w)
}

/// The citations of canon: per repo, each cited block with the blocks that pin it and the pin.
type Citations = BTreeMap<RepoId, Vec<(ForeignId, BlockId, ForeignPin)>>;

fn citations(m: &crate::Materialized) -> Citations {
    let mut out: Citations = BTreeMap::new();
    for b in m
        .iter()
        .filter(|b| !b.foreign.pins.is_empty() && !b.is_redacted())
    {
        for (target, pin) in &b.foreign.pins {
            out.entry(target.repo)
                .or_default()
                .push((target.clone(), b.id.clone(), pin.clone()));
        }
    }
    out
}

/// The word diff of `old` and `new` as the card shows it: `[-removed-]` and `{+added+}`, on one
/// line, cut to [`DIFF_MAX`] characters.
fn diff_text(old: &str, new: &str) -> String {
    let mut out = String::new();
    for p in worddiff::diff(old, new, Unit::Words) {
        match p.op {
            PieceOp::Same => out.push_str(&p.text),
            PieceOp::Del => out.push_str(&format!("[-{}-]", p.text)),
            PieceOp::Add => out.push_str(&format!("{{+{}+}}", p.text)),
        }
    }
    let line: String = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= DIFF_MAX {
        return line;
    }
    let kept: String = line.chars().take(DIFF_MAX - 1).collect();
    format!("{}…", kept.trim_end())
}

/// The text of a delivered block that its card diffs: its body, else its statement, title or
/// name.
fn shown_text(b: &Block) -> String {
    if !b.body.trim().is_empty() {
        return b.body.clone();
    }
    ["statement", "title", "name"]
        .iter()
        .find_map(|f| text(b, f))
        .unwrap_or_default()
        .to_string()
}

/// The text of version `version` of `target`, when a delivery held it.
fn held_text(
    st: &ReadState,
    target: &ForeignId,
    version: Option<Hash>,
) -> Result<String, RdyError> {
    let Some(v) = version else {
        return Ok(String::new());
    };
    Ok(st
        .foreign_held(target, &v)?
        .map(|b| shown_text(&b))
        .unwrap_or_default())
}

fn short(h: &Hash) -> String {
    h.to_string()[..ENTRY_SHORT].to_string()
}

fn set_word(s: &Option<ForeignSet>) -> String {
    match s {
        None => "none".to_string(),
        Some(ForeignSet::Organization) => "organization".to_string(),
        Some(ForeignSet::Team(n)) => format!("team {n}"),
    }
}

/// The open proposal versions that pin a block of `targets`, and which.
fn open_plans(st: &ReadState, targets: &BTreeSet<ForeignId>) -> Result<Vec<AdoptPlan>, RdyError> {
    let mut out = vec![];
    if targets.is_empty() {
        return Ok(out);
    }
    for (pref, p) in st.open_proposals()? {
        if super::queue::answered(st, &pref.to_string(), AnswerAct::RejectProposal)? {
            continue;
        }
        let mut hit: BTreeSet<ForeignId> = BTreeSet::new();
        for h in &p.changes {
            for op in &st.reader().change(h)?.ops {
                if let Op::Create { block } | Op::Update { block, .. } = op {
                    hit.extend(
                        block
                            .foreign
                            .pins
                            .keys()
                            .filter(|t| targets.contains(*t))
                            .cloned(),
                    );
                }
            }
        }
        if !hit.is_empty() {
            out.push(AdoptPlan {
                proposal: pref.to_string(),
                version: pref.version,
                targets: hit.into_iter().collect(),
            });
        }
    }
    out.sort();
    Ok(out)
}

/// The pin rows of one source: each pinned block of `repo` that a delivery of `delta` touched
/// and whose delivered content (in `members`) differs from its pin or is gone.
fn pin_rows(
    st: &ReadState,
    cited: &[(ForeignId, BlockId, ForeignPin)],
    delta: &BTreeSet<&BlockId>,
    members: &BTreeMap<BlockId, HeldVersion>,
) -> Result<Vec<AdoptPin>, RdyError> {
    let mut rows: BTreeMap<(ForeignId, Option<Hash>), AdoptPin> = BTreeMap::new();
    for (target, citer, pin) in cited {
        if !delta.contains(&target.id) {
            continue;
        }
        let new = match members.get(&target.id) {
            Some(h) if (pin.content, pin.encoding) == (h.version.content, h.version.encoding) => {
                continue;
            }
            Some(h) => Some(h.version.version),
            None => None,
        };
        let old = st.foreign_held_pin(target, pin)?;
        let row = rows
            .entry((target.clone(), old))
            .or_insert_with(|| AdoptPin {
                target: target.clone(),
                old,
                new,
                citers: vec![],
            });
        row.citers.push(citer.clone());
    }
    let mut rows: Vec<AdoptPin> = rows.into_values().collect();
    for r in &mut rows {
        r.citers.sort();
        r.citers.dedup();
    }
    Ok(rows)
}

/// The instructions in force for a link: the versions its `repo_link` block pins (D-MEMMQ). An
/// adopt answer settles a delivery, but it is no evidence that every instruction in it was
/// adopted, so the pins alone say what is in force.
fn adopted_instructions(
    st: &ReadState,
    m: &crate::Materialized,
    link: &BlockId,
    repo: &RepoId,
) -> Result<BTreeMap<BlockId, Hash>, RdyError> {
    let mut out = BTreeMap::new();
    let Some(b) = m.get(link) else {
        return Ok(out);
    };
    for (target, pin) in &b.foreign.pins {
        if target.repo == *repo
            && is_instruction(&target.id)
            && let Some(v) = st.foreign_held_pin(target, pin)?
        {
            out.insert(target.id.clone(), v);
        }
    }
    Ok(out)
}

/// The instruction rows of a source against its adopted baseline.
fn instruction_rows(
    members: &BTreeMap<BlockId, HeldVersion>,
    baseline: &BTreeMap<BlockId, Hash>,
) -> Vec<AdoptInstruction> {
    let mut out = vec![];
    for (id, h) in members.iter().filter(|(id, _)| is_instruction(id)) {
        let new = h.version.version;
        match baseline.get(id) {
            None => out.push(AdoptInstruction {
                id: id.clone(),
                change: AdoptChange::Added,
                old: None,
                new: Some(new),
            }),
            Some(old) if *old != new => out.push(AdoptInstruction {
                id: id.clone(),
                change: AdoptChange::Changed,
                old: Some(*old),
                new: Some(new),
            }),
            Some(_) => {}
        }
    }
    for (id, old) in baseline {
        if !members.contains_key(id) {
            out.push(AdoptInstruction {
                id: id.clone(),
                change: AdoptChange::Removed,
                old: Some(*old),
                new: None,
            });
        }
    }
    out
}

/// The adopt items of the state's canon, in alias order, with no card text and no hash yet.
/// Pure: it reads canon and the foreign cut, writes nothing and asks no clock but `now` for ages.
pub(crate) fn adopt_items(st: &ReadState, now: i64) -> Result<Vec<Item>, RdyError> {
    let foreign = st.foreign()?;
    if foreign.is_empty() {
        return Ok(vec![]);
    }
    let m = st.materialized(&Scope::Canon)?;
    let links = RepoLinks::of_view(&m);
    let mut cited: Option<Citations> = None;
    let mut suspects: Option<Vec<ForeignSuspect>> = None;
    let mut out = vec![];
    for (alias, row) in links.rows() {
        let Some(view) = foreign.link_source(&row.link) else {
            continue;
        };
        if view.revoked {
            continue;
        }
        let w = walk(st, &row.repo, view.head)?;
        if w.after.is_empty() {
            continue;
        }
        let delta: BTreeSet<&BlockId> = w.after.iter().flat_map(|v| &v.touched).collect();
        // a set change after the answer: a delivery whose set differs from the one before it
        let mut set_moved = false;
        for (i, v) in w.after.iter().enumerate() {
            let before = match w.after.get(i + 1) {
                Some(older) => Some(&older.set),
                None => w.answered.as_ref().map(|(_, s)| s),
            };
            // the oldest delivery of a chain follows no set at all
            if v.set != before.cloned().unwrap_or(None) {
                set_moved = true;
            }
        }
        let touches_instruction = delta.iter().any(|id| is_instruction(id));
        let citations = cited.get_or_insert_with(|| citations(&m));
        let own = citations.get(&row.repo).map(Vec::as_slice).unwrap_or(&[]);
        let pins = pin_rows(st, own, &delta, &view.members)?;
        if suspects.is_none() {
            suspects = Some(st.foreign_suspects(&Scope::Canon)?);
        }
        let suspects = suspects.as_deref().unwrap_or_default();
        let mut rebinds: Vec<AdoptRebind> = suspects
            .iter()
            .filter(|s| s.target.repo == row.repo && delta.contains(&s.target.id))
            .filter(|s| s.reason == "superseded")
            .filter_map(|s| {
                Some(AdoptRebind {
                    citer: s.block.clone(),
                    target: s.target.clone(),
                    successor: s.successor.clone()?,
                })
            })
            .collect();
        rebinds.sort();
        rebinds.dedup();
        let baseline_set = w.baseline.as_ref().and_then(|(_, s)| s.clone());
        let in_force = adopted_instructions(st, &m, &row.link, &row.repo)?;
        let rows = instruction_rows(&view.members, &in_force);
        let set = AdoptSet {
            from: baseline_set,
            to: view.set.clone(),
        };
        let instruction_diff = !rows.is_empty() || set.from != set.to;
        let rules = instruction_diff && (touches_instruction || set_moved);
        if pins.is_empty() && rebinds.is_empty() && !rules {
            continue;
        }
        let rows = if rules { rows } else { vec![] };
        let mut cleared: Vec<AdoptCleared> = vec![];
        for p in pins.iter().filter(|p| p.new.is_some()) {
            for s in suspects
                .iter()
                .filter(|s| s.target == p.target && s.reason == "changed")
            {
                if p.citers.contains(&s.block) {
                    cleared.push(AdoptCleared {
                        citer: s.block.clone(),
                        target: s.target.clone(),
                    });
                }
            }
        }
        cleared.sort();
        cleared.dedup();
        let moved: BTreeSet<ForeignId> = pins.iter().map(|p| p.target.clone()).collect();
        let plans = open_plans(st, &moved)?;
        if pins
            .len()
            .max(rebinds.len())
            .max(rows.len())
            .max(plans.len())
            > MAX_LIST
        {
            return Err(limit());
        }
        let set = if rules {
            set
        } else {
            AdoptSet {
                from: set.from.clone(),
                to: set.from,
            }
        };
        let binding = PlanCardBinding::Adopt {
            repo: row.repo,
            link: row.link.clone(),
            entry: view.head,
            after: w.answered.as_ref().map(|(h, _)| *h),
            pins,
            rebinds,
            cleared,
            instructions: rows,
            set,
            plans,
        };
        out.push(card(st, &m, alias, &links, binding, now)?);
    }
    Ok(out)
}

/// The card of `b`: see the module docs.
fn card(
    st: &ReadState,
    m: &crate::Materialized,
    alias: &str,
    links: &RepoLinks,
    binding: PlanCardBinding,
    now: i64,
) -> Result<Item, RdyError> {
    let PlanCardBinding::Adopt {
        repo,
        entry: entry_hash,
        pins,
        rebinds,
        cleared,
        instructions,
        set: moved_set,
        plans,
        ..
    } = &binding
    else {
        return Err(RdyError::new(
            E_INTERNAL,
            "not an adopt binding",
            "report this",
        ));
    };
    let reader = st.reader();
    let entry = reader.foreign_entry(entry_hash)?;
    let ForeignEvent::Delivery {
        publication, set, ..
    } = &entry.event
    else {
        return Err(RdyError::new(
            E_INTERNAL,
            "the newest entry of a live source is not a delivery",
            "report this",
        ));
    };
    let (site, at_ms) = match &entry.source {
        ForeignSource::Link { site, at_ms, .. } => (site.clone(), *at_ms),
        ForeignSource::Adr { source, .. } => (source.clone(), 0),
    };
    let shown = links.shown(repo);
    let mut it = item(
        ItemKind::Adopt,
        &adopt_ref(repo, entry_hash),
        &format!("Adopt the delivery from {shown}"),
    );
    it.why = ADOPT_WHY.to_string();
    it.summary = Some(format!(
        "Adopt delivery {} from {shown}, or keep the approved pins.",
        short(entry_hash)
    ));
    let mut facts = vec![
        Fact {
            name: "Delivery".into(),
            value: format!(
                "{} (tag {}, set {})",
                short(entry_hash),
                publication.tag,
                set_word(set)
            ),
        },
        Fact {
            name: "Link".into(),
            value: format!("{alias} ({})", *repo),
        },
    ];
    for p in pins {
        let old = held_text(st, &p.target, p.old)?;
        let new = held_text(st, &p.target, p.new)?;
        facts.push(Fact {
            name: format!("Pin {shown}:{}", p.target.id).into(),
            value: format!(
                "{} -> {}: {}",
                p.old.map_or("none".to_string(), |h| short(&h)),
                p.new.map_or("not delivered".to_string(), |h| short(&h)),
                diff_text(&old, &new)
            ),
        });
    }
    if !cleared.is_empty() {
        let list: Vec<String> = cleared
            .iter()
            .map(|c| format!("{} on {shown}:{}", c.citer, c.target.id))
            .collect();
        facts.push(Fact {
            name: "Clears suspects".into(),
            value: list.join("; "),
        });
    }
    for r in rebinds {
        facts.push(Fact {
            name: format!("Rebind {shown}:{}", r.target.id).into(),
            value: format!("{} to {shown}:{}", r.citer, r.successor),
        });
    }
    for i in instructions {
        let target = ForeignId::new(*repo, i.id.clone());
        let old = held_text(st, &target, i.old)?;
        let new = held_text(st, &target, i.new)?;
        facts.push(Fact {
            name: format!("Instruction {}", i.id).into(),
            value: format!("{}: {}", i.change.word(), diff_text(&old, &new)),
        });
    }
    if moved_set.from != moved_set.to {
        facts.push(Fact {
            name: "Set".into(),
            value: format!(
                "{} -> {}",
                set_word(&moved_set.from),
                set_word(&moved_set.to)
            ),
        });
    }
    if !plans.is_empty() {
        let list: Vec<String> = plans
            .iter()
            .map(|p| {
                let names: Vec<String> = p
                    .targets
                    .iter()
                    .map(|t| format!("{shown}:{}", t.id))
                    .collect();
                format!("{} pins {}", p.proposal, names.join(", "))
            })
            .collect();
        facts.push(Fact {
            name: "Open plans".into(),
            value: list.join("; "),
        });
    }
    it.facts = facts;
    let citers: BTreeSet<&BlockId> = pins
        .iter()
        .flat_map(|p| &p.citers)
        .chain(rebinds.iter().map(|r| &r.citer))
        .collect();
    it.members = citers
        .into_iter()
        .map(|id| {
            let title = m
                .get(id)
                .and_then(|blk| text(blk, "title"))
                .map_or_else(|| id.to_string(), str::to_string);
            format!("{id}  {title}")
        })
        .collect();
    it.options = vec![
        option(
            "adopt",
            ADOPT_LABEL,
            "Moves the listed pins to the delivered versions and takes the delivered rules into force.",
            None,
        ),
        option(
            "keep",
            KEEP_PIN_LABEL,
            "Keeps every pin and rule as it is, with your reason, until a later delivery changes something.",
            None,
        ),
        later(),
    ];
    it.raised_by = site;
    if at_ms != 0 {
        it.raised_at = rfc3339(at_ms);
        it.age = age(at_ms, now);
    }
    it.plan_binding = Some(binding);
    Ok(it)
}

impl ReadState {
    /// The adopt cards of this state for `q` (see the module docs), cards cut to the query's
    /// budget, less the ones `q.person` snoozed in the session `q.session_ref`. A pure read.
    ///
    /// The queue does not list these cards yet: this is the one way to read them until their
    /// answer path exists.
    #[doc(hidden)]
    pub fn adopt_cards(&self, q: &PendingQuery) -> Result<Vec<Item>, RdyError> {
        let (shown, _) = hide_snoozed(self, q, adopt_items(self, now_ms())?);
        let max = q.max_tokens.min(super::card::CARD_MAX_TOKENS);
        shown.into_iter().map(|i| finish(i, max)).collect()
    }
}
