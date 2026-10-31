//! The swarm script: what each scripted agent and the person do, step by step.
//!
//! `script` is a pure function of its `Config`. It uses SplitMix64 (below) and a fixed table
//! of weights. The interleaving of the agents at run time is real and not deterministic.
use std::collections::BTreeSet;

/// The areas of the `rdy-qa` pack that `Create` and `Intent` name.
pub const AREAS: [&str; 3] = ["payments", "identity", "glossary"];
/// How many seed blocks a project holds for `Pick::Seed` to index.
pub const SEED_BLOCKS: usize = 6;
/// The fewest steps a script can hold: the required acts need free places.
pub const MIN_STEPS: usize = 24;

/// How a swarm run is set up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub seed: u64,
    pub agents: usize,
    pub steps: usize,
    /// The step at which the daemon is killed, if any.
    pub kill_at: Option<usize>,
}

impl Config {
    /// The last two agents review, when there are at least four agents.
    pub fn is_reviewer(&self, i: usize) -> bool {
        self.agents >= 4 && i + 2 >= self.agents && i < self.agents
    }

    /// The fast tier: 8 agents, 40 steps, a kill at a seeded step in 10..30.
    pub fn fast(seed: u64) -> Config {
        Config {
            seed,
            agents: 8,
            steps: 40,
            kill_at: Some(kill_step(seed, 10, 20)),
        }
    }

    /// The deep tier: 8 agents, 200 steps, a kill at a seeded step in 50..150. (400 steps until
    /// fix round 1: the drain's reviewers then met more than 2 000 open review requests, the hang
    /// guard of ruling task-5.5, which reduces the tier instead.)
    pub fn deep(seed: u64) -> Config {
        Config {
            seed,
            agents: 8,
            steps: 200,
            kill_at: Some(kill_step(seed, 50, 100)),
        }
    }

    /// `deep` when `RDY_QA_DEEP=1`, else `fast`.
    pub fn for_run(seed: u64) -> Config {
        if crate::fixtures::deep() {
            Config::deep(seed)
        } else {
            Config::fast(seed)
        }
    }
}

fn kill_step(seed: u64, from: usize, span: u64) -> usize {
    from + (Rng::new(seed ^ 0x6b69_6c6c).next() % span) as usize
}

/// A block an act aims at. Indices are resolved at run time against what exists. A pick that
/// resolves to nothing becomes a `Read` of a seed block and is journaled as `skipped`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// The nth seed block of the project.
    Seed(usize),
    /// The nth block this agent created.
    Own(usize),
    /// The nth block another agent created.
    Other { agent: usize, nth: usize },
}

/// One act of a scripted agent. The closed list; each variant names its MCP tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    /// `rdy_brief` on the block's section.
    Brief { target: Pick },
    /// `rdy_write` with a briefed update of the block.
    Write { target: Pick, stable: bool },
    /// `rdy_write` of a stable block with no brief token.
    WriteNoBrief { target: Pick },
    /// `rdy_write` of the block, then `rdy_write` again with the first token.
    WriteStale { target: Pick },
    /// `rdy_write` creating a claim under the seed section of that area. `contra` is the
    /// index `k` into `Script::contra` when this is the act that carries the `CONTRA-<k>`
    /// marker (the executor writes the marker into this act and no other); else `None`.
    /// `source`: the forced include sequence's claim (`include_plan`), created in the agent's
    /// source view `a<i>-in` under the seed section of the agent's own area (`area` is unused).
    Create {
        area: &'static str,
        contra: Option<usize>,
        source: bool,
    },
    /// `rdy_write` updating a block that agent `with` also updates, from the same base.
    Collide { with: usize, target: Pick },
    /// `rdy_write` creating a claim that references a stable block, pinning its version.
    Cite { target: Pick },
    /// `rdy_write` with the `repin` op.
    Repin,
    /// `rdy_view` fork.
    Fork,
    /// `rdy_view` include of the view of agent `from`. `forced`: the forced include sequence
    /// (`include_plan`) includes the claim agent `from` created in its source view `a<from>-in`
    /// into the agent's include view `a<i>-ex`.
    Include { from: usize, forced: bool },
    /// `rdy_view` exclude. `forced`: the forced include sequence excludes that claim from the
    /// agent's include view `a<i>-ex`.
    Exclude { forced: bool },
    /// `rdy_view {op: "undo"}` on the agent's own view.
    Undo,
    /// `rdy_intent` in that area (`payments`, `identity` or `glossary`).
    Intent { area: &'static str },
    /// `rdy_alternatives {op: "create", title, members: [{label, view}]}` with two members:
    /// the agent's own view and the view of agent `with`.
    AltCreate { with: usize },
    /// `rdy_propose`.
    Propose,
    /// `rdy_review_next {kind: "tension"}`, then `rdy_review_submit` on the case it leased.
    ReviewNext,
    /// `rdy_tensions`.
    Tensions,
    /// `rdy_inbox {since: <the agent's last cursor>, ack, max_tokens: 100000}`.
    Inbox { ack: bool },
    /// `rdy_read` of the block.
    Read { target: Pick },
    /// An authoring check of the agent's own work (D-JQD5X). With no `rule` step: `rdy_review
    /// {op: "run"}` of `decision_review` on the agent's newest proposed decision, else of
    /// `meaning_preservation` on its newest editorial change, then the author's own `pass`
    /// (`rdy_review_submit` with no reason and no citations). With a `rule` step: the forced
    /// rule fixture (`RuleStep`).
    CheckRun { rule: Option<RuleStep> },
    /// An independent review request (D-JQD5X). With no `hold` step: `rdy_review {op:
    /// "request"}` of `meaning_preservation` on the agent's newest editorial change. With a
    /// `hold` step: the forced request and new-version sequence (`HoldStep`).
    CheckRequest { hold: Option<HoldStep> },
    /// A reviewer claims the next independent check request (`rdy_review_next {kind:
    /// "meaning_preservation"}`, then `decision_review`) and submits a deterministic `concern`
    /// with one citation and the reason `CHECK_REASON`. No model is called.
    CheckSubmit,
}

/// The reason of every scripted check concern.
pub const CHECK_REASON: &str = "the case needs a person to review it";

/// A step of the forced rule fixture: the child pack's blocking rule `check-fixture-key` on a
/// stable claim keyed `check-fixture`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleStep {
    /// The agent creates the claim in its rule view `a<i>-rule`, under a stable seed section,
    /// with no `checked` field, and proposes the view: the version is not ready (`E_RULE_CHECK`).
    Fail,
    /// The agent sets `checked: true` on the claim with a real write and proposes the view again:
    /// the new version is ready.
    Repair,
}

/// A step of the forced hold sequence: request, concern, a person's acceptance, a new version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldStep {
    /// The agent creates an editorial claim in its check view `a<i>-ck`, under a stable seed
    /// section, proposes the view, and requests an independent `meaning_preservation` review of
    /// that version.
    Request,
    /// The agent proposes its check view again: a new version, which inherits no hold.
    NewVersion,
}

impl Act {
    /// Every act name, in the order of the variants.
    pub const ALL_NAMES: [&'static str; 22] = [
        "brief",
        "write",
        "write_no_brief",
        "write_stale",
        "create",
        "collide",
        "cite",
        "repin",
        "fork",
        "include",
        "exclude",
        "undo",
        "intent",
        "alt_create",
        "propose",
        "review_next",
        "tensions",
        "inbox",
        "read",
        "check_run",
        "check_request",
        "check_submit",
    ];

    /// The snake_case name of the variant.
    pub fn name(&self) -> &'static str {
        match self {
            Act::Brief { .. } => "brief",
            Act::Write { .. } => "write",
            Act::WriteNoBrief { .. } => "write_no_brief",
            Act::WriteStale { .. } => "write_stale",
            Act::Create { .. } => "create",
            Act::Collide { .. } => "collide",
            Act::Cite { .. } => "cite",
            Act::Repin => "repin",
            Act::Fork => "fork",
            Act::Include { .. } => "include",
            Act::Exclude { .. } => "exclude",
            Act::Undo => "undo",
            Act::Intent { .. } => "intent",
            Act::AltCreate { .. } => "alt_create",
            Act::Propose => "propose",
            Act::ReviewNext => "review_next",
            Act::Tensions => "tensions",
            Act::Inbox { .. } => "inbox",
            Act::Read { .. } => "read",
            Act::CheckRun { .. } => "check_run",
            Act::CheckRequest { .. } => "check_request",
            Act::CheckSubmit => "check_submit",
        }
    }
}

/// One act of the simulated person. The closed list; Task 4 maps each to its relay tool or
/// its CLI command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersonAct {
    /// Land a proposal. `relayed`: through the relaying agent, else direct.
    Land { relayed: bool },
    /// Dismiss a tension.
    Dismiss,
    /// Accept a proposal's advice.
    AcceptAdvice,
    /// Edit the working copy and commit.
    WcEdit,
    /// Read the person's inbox and acknowledge it.
    InboxAck,
    /// Read the tensions.
    Tensions,
    /// Accept a check verdict as blocking on a version's approval card, through the relay
    /// (`rdy_pending`, then `rdy_answer` with the card's `accept_verdict:` option).
    AcceptCheckVerdict,
}

impl PersonAct {
    /// The snake_case name of the variant.
    pub fn name(&self) -> &'static str {
        match self {
            PersonAct::Land { .. } => "land",
            PersonAct::Dismiss => "dismiss",
            PersonAct::AcceptAdvice => "accept_advice",
            PersonAct::WcEdit => "wc_edit",
            PersonAct::InboxAck => "inbox_ack",
            PersonAct::Tensions => "tensions",
            PersonAct::AcceptCheckVerdict => "accept_check_verdict",
        }
    }
}

/// The scripts of one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    /// One list of acts per agent; `steps` long.
    pub agents: Vec<Vec<Act>>,
    /// The person's acts: `steps / 2`, with a landing cadence (see `script`).
    pub person: Vec<PersonAct>,
    /// Pairs of agents whose `Create` texts carry one shared `CONTRA-<k>` marker, where `k`
    /// is the index in this list (the scripted reviewer rule, ledger O4). Each member holds
    /// one `Create` with `contra: Some(k)`; the two share one area, so one seed section.
    /// Ruling task-3.1.
    ///
    /// A reviewer is offered the pair only when the second claim is written against canon:
    /// at BASE `pair_now` (`semantic/candidates.rs`) pairs two in-flight blocks only inside
    /// one view, and whether it should pair them across views is placed with M3 (ruling
    /// task-4.2). So the pair goes through the real flow, as explicit scripted acts, in this
    /// order: the first member (`.0`) writes its marked `Create` in the first quarter of its
    /// steps and holds a `Propose` between the first quarter and the first half; the person
    /// lands that proposal (the person's `Land`, through the relay); the second member (`.1`)
    /// writes its marked `Create` between five eighths and three quarters of its steps, and
    /// holds a `Propose` in the last quarter, before the last two steps, so the reviewers judge
    /// the pair and the person is shown their advice before the run ends (ruling task-5.2). The positions order the acts of one
    /// script; the landing between them is up to the run, which paces the person (Task 5). The
    /// journal shows whether the landing came first.
    pub contra: Vec<(usize, usize)>,
}

/// SplitMix64: a small seeded generator. No new crate.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A number in `0..n`. `n` must not be 0.
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

/// The kinds a random draw can give. `Brief` is also the first act of every script.
#[derive(Debug, Clone, Copy)]
enum Kind {
    Brief,
    Write,
    WriteNoBrief,
    WriteStale,
    Create,
    Collide,
    Cite,
    Repin,
    Fork,
    Include,
    Exclude,
    Undo,
    Intent,
    AltCreate,
    Propose,
    ReviewNext,
    Tensions,
    Inbox,
    Read,
    CheckRun,
    CheckRequest,
    CheckSubmit,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Brief => "brief",
            Kind::Write => "write",
            Kind::WriteNoBrief => "write_no_brief",
            Kind::WriteStale => "write_stale",
            Kind::Create => "create",
            Kind::Collide => "collide",
            Kind::Cite => "cite",
            Kind::Repin => "repin",
            Kind::Fork => "fork",
            Kind::Include => "include",
            Kind::Exclude => "exclude",
            Kind::Undo => "undo",
            Kind::Intent => "intent",
            Kind::AltCreate => "alt_create",
            Kind::Propose => "propose",
            Kind::ReviewNext => "review_next",
            Kind::Tensions => "tensions",
            Kind::Inbox => "inbox",
            Kind::Read => "read",
            Kind::CheckRun => "check_run",
            Kind::CheckRequest => "check_request",
            Kind::CheckSubmit => "check_submit",
        }
    }
}

/// The weights of a writer agent's random draw. `ReviewNext` is never drawn.
const WRITER_WEIGHTS: &[(Kind, u64)] = &[
    (Kind::Brief, 5),
    (Kind::Write, 18),
    (Kind::WriteNoBrief, 3),
    (Kind::WriteStale, 3),
    (Kind::Create, 8),
    (Kind::Collide, 6),
    (Kind::Cite, 6),
    (Kind::Repin, 3),
    (Kind::Fork, 3),
    (Kind::Include, 3),
    (Kind::Exclude, 2),
    (Kind::Undo, 3),
    (Kind::Intent, 3),
    (Kind::AltCreate, 3),
    (Kind::Propose, 8),
    (Kind::Tensions, 5),
    (Kind::Inbox, 5),
    (Kind::Read, 10),
    // D-JQD5X: each check act weighs 1; the weights above keep their values
    (Kind::CheckRun, 1),
    (Kind::CheckRequest, 1),
];

/// The weights of a reviewer's random draw. A reviewer reads and reviews; it never writes.
const REVIEWER_WEIGHTS: &[(Kind, u64)] = &[
    (Kind::Brief, 5),
    (Kind::ReviewNext, 40),
    (Kind::Inbox, 10),
    (Kind::Tensions, 10),
    (Kind::Read, 10),
    (Kind::CheckSubmit, 1),
];

/// A writer's draw weights, by act name, in table order.
pub fn writer_weights() -> Vec<(&'static str, u64)> {
    WRITER_WEIGHTS.iter().map(|(k, w)| (k.name(), *w)).collect()
}

/// A reviewer's draw weights, by act name, in table order.
pub fn reviewer_weights() -> Vec<(&'static str, u64)> {
    REVIEWER_WEIGHTS
        .iter()
        .map(|(k, w)| (k.name(), *w))
        .collect()
}

/// The weights of the person's random draw on an act that does not land (after the required
/// acts): `Dismiss`, `AcceptAdvice`, `WcEdit`, `InboxAck`, `Tensions`.
const OTHER_WEIGHTS: &[(u8, u64)] = &[(1, 20), (2, 20), (3, 15), (4, 15), (5, 10)];

fn weighted<T: Copy>(rng: &mut Rng, table: &[(T, u64)]) -> T {
    let total: u64 = table.iter().map(|(_, w)| w).sum();
    let mut at = rng.next() % total;
    for (item, w) in table {
        if at < *w {
            return *item;
        }
        at -= w;
    }
    table[table.len() - 1].0
}

fn pick(rng: &mut Rng, me: usize, agents: usize) -> Pick {
    let other = |rng: &mut Rng| {
        // Any agent but me.
        let a = rng.below(agents - 1);
        Pick::Other {
            agent: if a >= me { a + 1 } else { a },
            nth: rng.below(4),
        }
    };
    match rng.below(10) {
        0..=4 => Pick::Seed(rng.below(SEED_BLOCKS)),
        5..=7 => Pick::Own(rng.below(4)),
        _ if agents > 1 => other(rng),
        _ => Pick::Seed(rng.below(SEED_BLOCKS)),
    }
}

/// Another writer than `me`, drawn: the partner of a `Collide`, an `Include` or an `AltCreate`.
/// A reviewer never writes, so it is never drawn (final review M8). `me` when no other writer
/// exists.
fn other_agent(rng: &mut Rng, me: usize, cfg: &Config) -> usize {
    let others: Vec<usize> = (0..cfg.agents)
        .filter(|&i| i != me && !cfg.is_reviewer(i))
        .collect();
    if others.is_empty() {
        return me;
    }
    others[rng.below(others.len())]
}

fn area(rng: &mut Rng) -> &'static str {
    AREAS[rng.below(AREAS.len())]
}

fn make(rng: &mut Rng, kind: Kind, me: usize, cfg: &Config) -> Act {
    let agents = cfg.agents;
    match kind {
        Kind::Brief => Act::Brief {
            target: pick(rng, me, agents),
        },
        Kind::Write => Act::Write {
            target: pick(rng, me, agents),
            stable: rng.chance(30),
        },
        Kind::WriteNoBrief => Act::WriteNoBrief {
            target: Pick::Seed(rng.below(SEED_BLOCKS)),
        },
        Kind::WriteStale => Act::WriteStale {
            target: pick(rng, me, agents),
        },
        Kind::Create => Act::Create {
            area: area(rng),
            contra: None,
            source: false,
        },
        Kind::Collide => Act::Collide {
            with: other_agent(rng, me, cfg),
            target: Pick::Seed(rng.below(SEED_BLOCKS)),
        },
        Kind::Cite => Act::Cite {
            target: pick(rng, me, agents),
        },
        Kind::Repin => Act::Repin,
        Kind::Fork => Act::Fork,
        Kind::Include => Act::Include {
            from: other_agent(rng, me, cfg),
            forced: false,
        },
        Kind::Exclude => Act::Exclude { forced: false },
        Kind::Undo => Act::Undo,
        Kind::Intent => Act::Intent { area: area(rng) },
        Kind::AltCreate => Act::AltCreate {
            with: other_agent(rng, me, cfg),
        },
        Kind::Propose => Act::Propose,
        Kind::ReviewNext => Act::ReviewNext,
        Kind::Tensions => Act::Tensions,
        Kind::Inbox => Act::Inbox {
            ack: rng.chance(50),
        },
        Kind::Read => Act::Read {
            target: pick(rng, me, agents),
        },
        Kind::CheckRun => Act::CheckRun { rule: None },
        Kind::CheckRequest => Act::CheckRequest { hold: None },
        Kind::CheckSubmit => Act::CheckSubmit,
    }
}

/// An act put on a seeded free step in `lo..hi`.
type Placed = (Act, usize, usize);

/// The fewest steps a script needs to hold the forced check sequences (`check_plan`): the fast
/// tier's length.
pub const CHECK_MIN_STEPS: usize = 40;

/// The forced check sequences of a run (D-JQD5X): who holds each step, and where. Every step is a
/// fixed step index; the run's pacing (each agent's step waits for the agents to finish the step
/// before it, and for the person's acts due before it; each person act waits for its step) orders
/// them with no clock. One writer fails and then repairs the rule fixture. One writer requests an
/// independent review of a version; a reviewer, a different actor, submits a concern; the person
/// accepts it; then the writer makes a new version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckPlan {
    /// The writer of the rule fixture, and its two steps.
    pub rule_agent: usize,
    pub rule_fail: usize,
    pub rule_repair: usize,
    /// The writer of the hold sequence, its request step and its new-version step.
    pub hold_agent: usize,
    pub request: usize,
    pub new_version: usize,
    /// The reviewer that submits the concern, and its step.
    pub reviewer: usize,
    pub submit: usize,
    /// The person's act (an index into `Script::person`) that accepts the verdict.
    pub accept: usize,
}

/// The salt of `check_plan`'s draw.
const CHECK_SALT: u64 = 4;

/// The step the person's act `j` of `len` waits for (the run's pacing, `run::due`).
fn person_due(j: usize, steps: usize, len: usize) -> usize {
    ((j + 1) * steps / len.max(1)).min(steps)
}

/// The forced check sequences of `cfg`: a pure function of it. `None` for a script with no
/// reviewer or no writer, or shorter than `CHECK_MIN_STEPS`: the fast and deep tiers hold them.
pub fn check_plan(cfg: &Config) -> Option<CheckPlan> {
    let writers: Vec<usize> = (0..cfg.agents).filter(|&i| !cfg.is_reviewer(i)).collect();
    let reviewers: Vec<usize> = (0..cfg.agents).filter(|&i| cfg.is_reviewer(i)).collect();
    if cfg.steps < CHECK_MIN_STEPS || writers.is_empty() || reviewers.is_empty() {
        return None;
    }
    // a draw of its own, so the agents' and the person's draws are left as they were
    let mut rng = Rng::new(cfg.seed ^ CHECK_SALT);
    let (steps, len) = (cfg.steps, cfg.steps / 2);
    let request = steps / 4;
    let submit = request + 2;
    // the person's first odd act that waits until the reviewer has finished the submit step
    let accept = (1..len.saturating_sub(2))
        .step_by(2)
        .find(|&j| person_due(j, steps, len) > submit)?;
    let new_version = person_due(accept, steps, len) + 2;
    if new_version >= steps {
        return None;
    }
    Some(CheckPlan {
        rule_agent: writers[rng.below(writers.len())],
        rule_fail: steps / 8,
        rule_repair: steps * 5 / 8,
        hold_agent: writers[rng.below(writers.len())],
        request,
        new_version,
        reviewer: reviewers[rng.below(reviewers.len())],
        submit,
        accept,
    })
}

/// The forced acts of agent `me` under `plan`, each with its fixed step.
fn forced_acts(plan: Option<&CheckPlan>, me: usize) -> Vec<(Act, usize)> {
    let Some(p) = plan else { return vec![] };
    let mut out = vec![];
    if p.rule_agent == me {
        out.push((
            Act::CheckRun {
                rule: Some(RuleStep::Fail),
            },
            p.rule_fail,
        ));
        out.push((
            Act::CheckRun {
                rule: Some(RuleStep::Repair),
            },
            p.rule_repair,
        ));
    }
    if p.hold_agent == me {
        out.push((
            Act::CheckRequest {
                hold: Some(HoldStep::Request),
            },
            p.request,
        ));
        out.push((
            Act::CheckRequest {
                hold: Some(HoldStep::NewVersion),
            },
            p.new_version,
        ));
    }
    if p.reviewer == me {
        out.push((Act::CheckSubmit, p.submit));
    }
    out
}

/// The salt of `include_plan`'s draw.
const INCLUDE_SALT: u64 = 0x696e_636c;

/// The step of the forced include sequence's source claim; its include comes two steps later
/// (the pacing then waits until the source's whole step is done) and its exclude one step after
/// that. Every step is before the first forced check step (`steps / 8`) and the kill window.
pub const INCLUDE_SOURCE_STEP: usize = 1;
/// The step of the forced include.
pub const INCLUDE_STEP: usize = INCLUDE_SOURCE_STEP + 2;
/// The step of the forced exclude.
pub const EXCLUDE_STEP: usize = INCLUDE_STEP + 1;

/// The forced include sequence of a run (deep swarm ruling 1, item 2): writer `source` creates a
/// claim in its source view `a<source>-in`; writer `writer`, another writer, includes that change
/// into its include view `a<writer>-ex` and then excludes it. Neither is agent 0 or 1 (the
/// members of the scripted alternative set).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IncludePlan {
    pub source: usize,
    pub writer: usize,
}

/// The forced include sequence of `cfg`: a pure function of it, with a draw of its own. `None`
/// where `check_plan` is `None`, or with fewer than two writers outside agents 0 and 1.
pub fn include_plan(cfg: &Config) -> Option<IncludePlan> {
    check_plan(cfg)?;
    let writers: Vec<usize> = (2..cfg.agents).filter(|&i| !cfg.is_reviewer(i)).collect();
    if writers.len() < 2 {
        return None;
    }
    let mut rng = Rng::new(cfg.seed ^ INCLUDE_SALT);
    let source = writers[rng.below(writers.len())];
    let others: Vec<usize> = writers.into_iter().filter(|&w| w != source).collect();
    let writer = others[rng.below(others.len())];
    Some(IncludePlan { source, writer })
}

/// The forced include acts of agent `me` under `plan`, each with its fixed step.
fn include_acts(plan: Option<&IncludePlan>, me: usize) -> Vec<(Act, usize)> {
    let Some(p) = plan else { return vec![] };
    let mut out = vec![];
    if p.source == me {
        out.push((
            Act::Create {
                area: AREAS[me % AREAS.len()],
                contra: None,
                source: true,
            },
            INCLUDE_SOURCE_STEP,
        ));
    }
    if p.writer == me {
        out.push((
            Act::Include {
                from: p.source,
                forced: true,
            },
            INCLUDE_STEP,
        ));
        out.push((Act::Exclude { forced: true }, EXCLUDE_STEP));
    }
    out
}

/// The script of one actor: random acts, then the forced acts on their fixed steps, then the
/// placed acts on seeded free steps inside their windows, then the required acts on seeded free
/// steps.
fn actor_script(
    rng: &mut Rng,
    cfg: &Config,
    me: usize,
    forced: Vec<(Act, usize)>,
    placed: Vec<Placed>,
    required: Vec<Act>,
) -> Vec<Act> {
    let weights = if cfg.is_reviewer(me) {
        REVIEWER_WEIGHTS
    } else {
        WRITER_WEIGHTS
    };
    let mut acts = vec![Act::Brief {
        target: Pick::Seed(rng.below(SEED_BLOCKS)),
    }];
    for _ in 1..cfg.steps {
        let kind = weighted(rng, weights);
        acts.push(make(rng, kind, me, cfg));
    }
    let mut free: Vec<usize> = (1..cfg.steps).collect();
    for (act, at) in forced {
        assert!(
            free.contains(&at),
            "a script of {} steps forces two acts on step {at}",
            cfg.steps
        );
        free.retain(|f| *f != at);
        acts[at] = act;
    }
    for (act, lo, hi) in placed {
        let spots: Vec<usize> = free
            .iter()
            .copied()
            .filter(|at| (lo..hi).contains(at))
            .collect();
        assert!(
            !spots.is_empty(),
            "a script of {} steps has no free step in {lo}..{hi}",
            cfg.steps
        );
        let at = spots[rng.below(spots.len())];
        free.retain(|f| *f != at);
        acts[at] = act;
    }
    assert!(
        required.len() <= free.len(),
        "a script of {} steps cannot hold {} required acts",
        cfg.steps,
        required.len()
    );
    for act in required {
        let at = free.swap_remove(rng.below(free.len()));
        acts[at] = act;
    }
    acts
}

/// The scripts of a run: a pure function of `cfg`.
///
/// Every actor's first act is `Brief`. Only the reviewers hold `ReviewNext`, at least three
/// each. Every writer holds at least one `Collide`, `Cite`, `Propose` and `Inbox`; every
/// reviewer at least one `Inbox`; agents 0 and 1 hold an `AltCreate` with each other. A
/// `Collide`, `Include` or `AltCreate` names another writer. The person's script holds
/// `steps / 2` acts: every other act lands, one landing at a fixed act is direct, the run ends
/// with an `AcceptAdvice` and a `Land`, and every `PersonAct` comes at least once (ruling
/// task-5.2). The
/// members of a contradiction pair hold their marked `Create` acts in the order `Script::contra`
/// states. A fast or deep script also holds the forced check sequences of `check_plan` (D-JQD5X):
/// their acts sit on fixed steps, placed before every other placed or required act, and the
/// person's acceptance on a fixed odd act. It also holds the forced include sequence of
/// `include_plan` (a source claim, an include of it, then an exclude of it), on fixed steps
/// placed with the forced check steps.
///
/// Panics when `cfg.steps` is below `MIN_STEPS`.
pub fn script(cfg: &Config) -> Script {
    assert!(
        cfg.steps >= MIN_STEPS,
        "a swarm script needs at least {MIN_STEPS} steps, got {}",
        cfg.steps
    );
    let mut master = Rng::new(cfg.seed);
    // One or two pairs of writers whose Create texts contradict each other.
    let writers: Vec<usize> = (0..cfg.agents).filter(|&i| !cfg.is_reviewer(i)).collect();
    let mut contra: Vec<(usize, usize)> = Vec::new();
    if writers.len() >= 2 {
        let want = 1 + master.below(2);
        let mut seen = BTreeSet::new();
        for _ in 0..8 {
            if contra.len() == want {
                break;
            }
            let a = writers[master.below(writers.len())];
            let b = writers[master.below(writers.len())];
            let pair = (a.min(b), a.max(b));
            // agents 0 and 1 are the members of one alternative set: landing one closes the
            // other's view, so a pair of them could not be written (they pair only when no
            // other writers exist)
            let alternatives = writers.len() > 2 && pair == (0, 1);
            if a != b && !alternatives && seen.insert(pair) {
                contra.push(pair);
            }
        }
        if contra.is_empty() {
            contra.push((writers[0], writers[writers.len().min(3) - 1]));
        }
    }
    // One area per pair, drawn once: both members' marked `Create` acts use it.
    let contra_areas: Vec<&'static str> = contra.iter().map(|_| area(&mut master)).collect();
    let plan = check_plan(cfg);
    let include = include_plan(cfg);
    let mut agents = Vec::with_capacity(cfg.agents);
    for me in 0..cfg.agents {
        let mut rng = Rng::new(master.next());
        let mut required = Vec::new();
        let mut placed: Vec<Placed> = Vec::new();
        if cfg.is_reviewer(me) {
            for _ in 0..3 {
                required.push(Act::ReviewNext);
            }
            required.push(Act::Inbox { ack: true });
        } else {
            required.push(make(&mut rng, Kind::Collide, me, cfg));
            required.push(make(&mut rng, Kind::Cite, me, cfg));
            required.push(Act::Propose);
            required.push(Act::Inbox { ack: true });
            if cfg.agents >= 2 && me < 2 {
                required.push(Act::AltCreate { with: 1 - me });
            }
            let quarter = cfg.steps / 4;
            // the second claim comes after the first member's landing (its proposal is due by
            // the half, the person lands on a cadence) and early enough to be judged in the run
            let late = cfg.steps / 2 + cfg.steps / 8;
            let mut second_of_a_pair = false;
            for (k, (first, second)) in contra.iter().enumerate() {
                let create = Act::Create {
                    area: contra_areas[k],
                    contra: Some(k),
                    source: false,
                };
                if *first == me {
                    placed.push((create.clone(), 1, quarter));
                    placed.push((Act::Propose, quarter, cfg.steps / 2));
                }
                if *second == me {
                    placed.push((create, late, cfg.steps - quarter));
                    second_of_a_pair = true;
                }
            }
            // the second member proposes its marked claim before the person's tail
            if second_of_a_pair {
                placed.push((Act::Propose, cfg.steps - quarter, cfg.steps - 2));
            }
        }
        let mut forced = forced_acts(plan.as_ref(), me);
        forced.extend(include_acts(include.as_ref(), me));
        agents.push(actor_script(&mut rng, cfg, me, forced, placed, required));
    }
    Script {
        agents,
        person: person_script(
            &mut Rng::new(master.next()),
            cfg.steps / 2,
            plan.map(|p| p.accept),
        ),
        contra,
    }
}

/// The person's act that lands direct (`rdy approve` with no relay, the CLI parity path; final
/// review M4): the even act at the middle of the script, before the tail.
pub fn direct_land_at(len: usize) -> usize {
    (len / 2) & !1
}

/// The person's acts (`len` of them, at least 12): a landing cadence, so the run lands as it goes
/// (ruling task-5.2). Each even act lands; each odd act is drawn from the other kinds; the last
/// two accept the advice the person is shown on a queued proposal and land; and each other kind
/// comes at least once, on an odd act before those two. The landing at `direct_land_at` is a
/// direct `rdy approve` (the CLI parity path); the last one is relayed.
///
/// With `accept` (the forced hold sequence, `check_plan`), that odd act accepts a check verdict
/// (`AcceptCheckVerdict`), and the other kinds are placed on the remaining odd acts.
fn person_script(rng: &mut Rng, len: usize, accept: Option<usize>) -> Vec<PersonAct> {
    let make = |rng: &mut Rng, id: u8| match id {
        0 => PersonAct::Land {
            relayed: rng.chance(75),
        },
        1 => PersonAct::Dismiss,
        2 => PersonAct::AcceptAdvice,
        3 => PersonAct::WcEdit,
        4 => PersonAct::InboxAck,
        _ => PersonAct::Tensions,
    };
    let mut acts: Vec<PersonAct> = (0..len)
        .map(|i| {
            let id = if i % 2 == 0 {
                0
            } else {
                weighted(rng, OTHER_WEIGHTS)
            };
            make(rng, id)
        })
        .collect();
    acts[len - 2] = PersonAct::AcceptAdvice;
    acts[len - 1] = PersonAct::Land { relayed: true };
    acts[direct_land_at(len)] = PersonAct::Land { relayed: false };
    let mut free: Vec<usize> = (1..len - 2).step_by(2).collect();
    if let Some(at) = accept {
        free.retain(|f| *f != at);
        acts[at] = PersonAct::AcceptCheckVerdict;
    }
    for id in 1..6u8 {
        let at = free.swap_remove(rng.below(free.len()));
        acts[at] = make(rng, id);
    }
    acts
}
