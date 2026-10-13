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
    Create {
        area: &'static str,
        contra: Option<usize>,
    },
    /// `rdy_write` updating a block that agent `with` also updates, from the same base.
    Collide { with: usize, target: Pick },
    /// `rdy_write` creating a claim that references a stable block, pinning its version.
    Cite { target: Pick },
    /// `rdy_write` with the `repin` op.
    Repin,
    /// `rdy_view` fork.
    Fork,
    /// `rdy_view` include of the view of agent `from`.
    Include { from: usize },
    /// `rdy_view` exclude.
    Exclude,
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
}

impl Act {
    /// Every act name, in the order of the variants.
    pub const ALL_NAMES: [&'static str; 19] = [
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
            Act::Exclude => "exclude",
            Act::Undo => "undo",
            Act::Intent { .. } => "intent",
            Act::AltCreate { .. } => "alt_create",
            Act::Propose => "propose",
            Act::ReviewNext => "review_next",
            Act::Tensions => "tensions",
            Act::Inbox { .. } => "inbox",
            Act::Read { .. } => "read",
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
];

/// The weights of a reviewer's random draw. A reviewer reads and reviews; it never writes.
const REVIEWER_WEIGHTS: &[(Kind, u64)] = &[
    (Kind::Brief, 5),
    (Kind::ReviewNext, 40),
    (Kind::Inbox, 10),
    (Kind::Tensions, 10),
    (Kind::Read, 10),
];

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

fn other_agent(rng: &mut Rng, me: usize, agents: usize) -> usize {
    if agents < 2 {
        return me;
    }
    let a = rng.below(agents - 1);
    if a >= me { a + 1 } else { a }
}

fn area(rng: &mut Rng) -> &'static str {
    AREAS[rng.below(AREAS.len())]
}

fn make(rng: &mut Rng, kind: Kind, me: usize, agents: usize) -> Act {
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
        },
        Kind::Collide => Act::Collide {
            with: other_agent(rng, me, agents),
            target: Pick::Seed(rng.below(SEED_BLOCKS)),
        },
        Kind::Cite => Act::Cite {
            target: pick(rng, me, agents),
        },
        Kind::Repin => Act::Repin,
        Kind::Fork => Act::Fork,
        Kind::Include => Act::Include {
            from: other_agent(rng, me, agents),
        },
        Kind::Exclude => Act::Exclude,
        Kind::Undo => Act::Undo,
        Kind::Intent => Act::Intent { area: area(rng) },
        Kind::AltCreate => Act::AltCreate {
            with: other_agent(rng, me, agents),
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
    }
}

/// An act put on a seeded free step in `lo..hi`.
type Placed = (Act, usize, usize);

/// The script of one actor: random acts, then the placed acts on seeded free steps inside their
/// windows, then the required acts on seeded free steps.
fn actor_script(
    rng: &mut Rng,
    cfg: &Config,
    me: usize,
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
        acts.push(make(rng, kind, me, cfg.agents));
    }
    let mut free: Vec<usize> = (1..cfg.steps).collect();
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
/// reviewer at least one `Inbox`; agents 0 and 1 hold an `AltCreate` with each other. The
/// person's script holds `steps / 2` acts: every other act lands, the run ends with an
/// `AcceptAdvice` and a `Land`, and every `PersonAct` comes at least once (ruling task-5.2). The
/// members of a contradiction pair hold their marked `Create` acts in the order `Script::contra`
/// states.
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
            required.push(make(&mut rng, Kind::Collide, me, cfg.agents));
            required.push(make(&mut rng, Kind::Cite, me, cfg.agents));
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
        agents.push(actor_script(&mut rng, cfg, me, placed, required));
    }
    Script {
        agents,
        person: person_script(&mut Rng::new(master.next()), cfg.steps / 2),
        contra,
    }
}

/// The person's acts (`len` of them, at least 12): a landing cadence, so the run lands as it goes
/// (ruling task-5.2). Each even act lands; each odd act is drawn from the other kinds; the last
/// two accept the advice the person is shown on a queued proposal and land; and each other kind
/// comes at least once, on an odd act before those two.
fn person_script(rng: &mut Rng, len: usize) -> Vec<PersonAct> {
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
    let mut free: Vec<usize> = (1..len - 2).step_by(2).collect();
    for id in 1..6u8 {
        let at = free.swap_remove(rng.below(free.len()));
        acts[at] = make(rng, id);
    }
    acts
}
