//! sg5: the swarm simulation (spec §15.3; ledger O1-O5, O9). Eight scripted agents and one
//! person act on one daemon, through MCP first. The daemon is killed once and restarted. Then no
//! actor runs (the freeze), the project is copied, and the drain lands every live view.
//!
//! Every assertion reads the final state or the journal, never an order of events. One run per
//! seed is shared by every test of this file: seeds 1 to 6 in the fast gate (each runs every
//! check; together they must exercise every act, ruling task-5.2), seeds 1, 2 and 3 in the deep
//! gate (`RDY_QA_DEEP=1`). Every failure names its seed, so a person can run it again:
//! `RDY_QA_SWARM_SEEDS=<seed>,...` runs only those seeds.
use rdy_engine::inbox::read::parse_cursor;
use rdy_engine::plan::{PlanQuery, allowed_acts};
use rdy_engine::tensions::tension_label;
use rdy_engine::{EventKind, Materialized, Rdy, ReadState, Scope};
use rdy_model::{BlockId, Change, Hash, Op, PlanKind};
use rdy_qa::fixtures::deep;
use rdy_qa::invariants::check_all;
use rdy_qa::landing_audit::{alt_unchosen, chosen_members, conflicts_at, landed_of};
use rdy_qa::reindex_check::reindex_reproduces;
use rdy_qa::swarm::journal::{Entry, Outcome as Met, allowed};
use rdy_qa::swarm::oracle::{self, copy_of};
use rdy_qa::swarm::run::{self, Outcome};
use rdy_qa::swarm::script::{Act, Config, PersonAct, Pick, Script};
use rdy_store::kinds;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::OnceLock;

/// One seed's run, or the text it failed with.
type Run = (u64, Result<Outcome, String>);

fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<String>()
        .cloned()
        .or_else(|| p.downcast_ref::<&str>().map(|s| (*s).to_string()))
        .unwrap_or_else(|| "a panic".to_string())
}

/// `f`, with its panic text starting `swarm seed {seed}: `.
fn seeded<T>(seed: u64, f: impl FnOnce() -> T) -> Result<T, String> {
    catch_unwind(AssertUnwindSafe(f)).map_err(|p| {
        let text = panic_text(&*p);
        if text.starts_with(&format!("swarm seed {seed}: ")) {
            text
        } else {
            format!("swarm seed {seed}: {text}")
        }
    })
}

/// The fast gate's seeds. Each one runs every check, and their union must exercise every act
/// (ruling task-5.2).
const FAST_SEEDS: [u64; 6] = [1, 2, 3, 4, 5, 6];
/// The deep gate's seeds.
const DEEP_SEEDS: [u64; 3] = [1, 2, 3];

/// The seeds of this run: `RDY_QA_SWARM_SEEDS` (a comma list, to run a failing seed again),
/// else the tier's.
fn seeds() -> Vec<u64> {
    if let Ok(list) = std::env::var("RDY_QA_SWARM_SEEDS") {
        return list
            .split(',')
            .map(|s| {
                s.trim()
                    .parse()
                    .unwrap_or_else(|_| panic!("RDY_QA_SWARM_SEEDS: {s:?} is not a seed"))
            })
            .collect();
    }
    if deep() {
        DEEP_SEEDS.to_vec()
    } else {
        FAST_SEEDS.to_vec()
    }
}

/// The runs of this file: one per seed, made once.
fn runs() -> &'static [Run] {
    static RUNS: OnceLock<Vec<Run>> = OnceLock::new();
    RUNS.get_or_init(|| {
        seeds()
            .into_iter()
            .map(|seed| (seed, seeded(seed, || run::run(&Config::for_run(seed)))))
            .collect()
    })
}

/// Runs `check` on every seed's outcome. A run that failed fails here with its own text. Every
/// seed is checked; the failure names each seed that failed.
fn each(check: impl Fn(u64, &Outcome)) {
    let mut failed = vec![];
    for (seed, r) in runs() {
        match r {
            Ok(o) => {
                if let Err(text) = seeded(*seed, || check(*seed, o)) {
                    failed.push(text);
                }
            }
            Err(text) => failed.push(text.clone()),
        }
    }
    if !failed.is_empty() {
        resume_unwind(Box::new(failed.join("\n\n")));
    }
}

/// The acts whose `Ok` reply is an acknowledged write, view op or proposal.
const ACKNOWLEDGED: [&str; 13] = [
    "write",
    "create",
    "cite",
    "collide",
    "repin",
    "write_stale",
    "fork",
    "include",
    "exclude",
    "undo",
    "propose",
    "wc_commit",
    "wc_propose",
];

/// The kind of every tension the frozen log opened, by label.
fn logged_kinds(o: &Outcome) -> BTreeMap<String, String> {
    oracle::logged_moves(&o.frozen)
        .into_iter()
        .filter_map(|(_, k)| match k {
            EventKind::TensionOpened(m) => Some((tension_label(&m.tension), m.kind)),
            _ => None,
        })
        .collect()
}

/// The person's actor: a landing it made is a direct `rdy approve`.
const HUMAN: &str = "human:qa";

/// The acts every seed's run reaches before the freeze (ruling task-5.2).
const CORE: [&str; 6] = [
    "write",
    "brief",
    "propose",
    "land",
    "review_submit",
    "inbox",
];

/// What one seed's run did before the freeze (ruling task-5.2: the drain's acts do not count).
struct Coverage {
    /// The acts acknowledged (`write_no_brief`: refused with `E_BRIEF_REQUIRED`; `inbox`: an
    /// agent's read, resumed or not).
    acts: BTreeSet<&'static str>,
    lost: bool,
    landings: usize,
    chose: bool,
    kinds: BTreeSet<String>,
    opened: bool,
    /// A member that won its set by a landing lands again, choosing nothing (seed 3's pattern;
    /// ruling task-5-round1.2).
    winner_again: bool,
    /// An include of a change that a set binds (msw20), that set chosen by a landing of the run,
    /// and a landing after the choice (ruling task-5-round1.1). Printed as evidence only: the
    /// random seeds reach it rarely (fix round 2 measured 1 seed in 16), so the directed run
    /// `sg5_an_included_change_bound_to_the_winner_lands_after_the_choice` asserts it, and
    /// `sg3_a_fast_seed_includes_before_a_set_and_lands_after_it` asserts the scripts allow it.
    bound_include: bool,
    /// A landing by a direct `rdy approve` (the person, with no relay: the CLI parity path).
    direct: bool,
}

/// Whether the run (before the freeze) included a change that a set binds, chose that set by a
/// landing, and landed again after the choice. The bindings and the choices are read from the
/// drained store; the order from the journal.
fn bound_include(o: &Outcome) -> bool {
    let (_keep, copy) = copy_of(&o.dir);
    let r = Rdy::open(&copy).expect("open the drained copy");
    let st = r.read();
    let chosen_by: BTreeMap<String, Hash> = st
        .reader()
        .landings()
        .expect("read canon's chain")
        .iter()
        .filter_map(|(l, rec)| Some((l.to_string(), rec.chosen.as_ref()?.set)))
        .collect();
    let run = o.before_freeze();
    // the sets that bind a change an include of the run brought into another view
    let mut bound: BTreeSet<Hash> = BTreeSet::new();
    let mut found = false;
    // the sets chosen so far, each with whether a landing came after its choice
    let mut chosen: BTreeSet<Hash> = BTreeSet::new();
    for e in run.iter().filter(|e| e.outcome == Met::Ok) {
        if e.act == "include" {
            for c in e
                .reply
                .as_ref()
                .and_then(|r| r["included"].as_array())
                .into_iter()
                .flatten()
                .filter_map(|c| c.as_str()?.parse::<Hash>().ok())
            {
                bound.extend(bindings_of(&st, &c).into_iter().map(|(set, _)| set));
            }
        }
        let Some(l) = e
            .reply
            .as_ref()
            .filter(|_| e.act == "land" || e.act == run::LAND_FOUND)
            .and_then(|r| r["landing"].as_str())
        else {
            continue;
        };
        found |= chosen.iter().any(|set| bound.contains(set));
        if let Some(set) = chosen_by.get(l) {
            chosen.insert(*set);
        }
    }
    drop(st);
    r.close().expect("close the drained copy");
    found
}

fn coverage(o: &Outcome) -> Coverage {
    let run = o.before_freeze();
    let mut acts: BTreeSet<&'static str> = run
        .iter()
        .filter(|e| e.outcome == Met::Ok)
        // an approve whose reply the kill cut, found landed (`land_found`), was not acknowledged:
        // it is no `land` act (final review M14)
        .map(|e| match e.act {
            "inbox_resume" => "inbox",
            act => act,
        })
        .collect();
    // the daemon never acknowledges an unbriefed stable write: its refusal is the act
    acts.remove("write_no_brief");
    if run
        .iter()
        .any(|e| e.act == "write_no_brief" && e.outcome == Met::Refused("E_BRIEF_REQUIRED".into()))
    {
        acts.insert("write_no_brief");
    }
    let landed: Vec<&Entry> = run
        .iter()
        .filter(|e| {
            (e.act == "land" && e.outcome == Met::Ok)
                || (e.act == run::LAND_FOUND && e.reply.is_some())
        })
        .collect();
    let logged = logged_kinds(o);
    let mut kinds: BTreeSet<String> = o
        .reported
        .values()
        .filter_map(|t| t["kind"].as_str().map(str::to_string))
        .collect();
    kinds.extend(
        run.iter()
            .flat_map(|e| e.seen.iter())
            .filter_map(|l| logged.get(l).cloned()),
    );
    // the views that won a set by a landing of the run, and whether one landed again choosing
    // nothing
    let mut won: BTreeSet<&str> = BTreeSet::new();
    let mut winner_again = false;
    for e in run
        .iter()
        .filter(|e| e.act == "land" && e.outcome == Met::Ok)
    {
        let (Some(view), Some(reply)) = (e.view.as_deref(), e.reply.as_ref()) else {
            continue;
        };
        match reply.get("chosen").filter(|c| !c.is_null()) {
            Some(c) => {
                if c["member"] == view {
                    won.insert(view);
                }
            }
            None => winner_again |= won.contains(view),
        }
    }
    Coverage {
        winner_again,
        bound_include: bound_include(o),
        direct: run
            .iter()
            .any(|e| e.act == "land" && e.outcome == Met::Ok && e.actor == HUMAN),
        acts,
        lost: run.iter().any(|e| e.outcome == Met::Lost),
        landings: landed.len(),
        chose: landed.iter().any(|e| {
            e.reply
                .as_ref()
                .is_some_and(|r| r.get("chosen").is_some_and(|c| !c.is_null()))
        }),
        kinds,
        opened: run.iter().any(|e| {
            e.inbox
                .as_ref()
                .is_some_and(|r| r.entries.iter().any(|x| x["event"] == "tension_opened"))
        }),
    }
}

/// Ruling task-5.2: the run, not the drain, exercises the acts. Every seed reaches the core acts
/// and lands at least once before the freeze; the union of the seeds reaches every act of the
/// closed list, a lost call, a seed with 4 landings, a landing that chose a member of a set, an
/// `edit_collision` and a `premise_shift`, and a `tension_opened` inbox entry. Ruling
/// task-5-round1.2: the union also reaches a set's winner landing again with no choice. Final
/// review M4: the union also lands by a direct `rdy approve`. Only acknowledged `land` calls count
/// as `land` (final review M14). With `RDY_QA_SWARM_SEEDS` set, only each seed's core acts are
/// checked: the union is a property of the tier's seeds.
#[test]
fn sg5_the_run_exercised_every_act() {
    let mut all = Coverage {
        acts: BTreeSet::new(),
        lost: false,
        landings: 0,
        chose: false,
        kinds: BTreeSet::new(),
        opened: false,
        winner_again: false,
        bound_include: false,
        direct: false,
    };
    let mut failed = vec![];
    for (seed, r) in runs() {
        let o = match r {
            Ok(o) => o,
            Err(text) => {
                failed.push(text.clone());
                continue;
            }
        };
        let c = coverage(o);
        // the evidence a person reads with --nocapture: what each seed's run reached
        eprintln!(
            "swarm seed {seed}: {} acts, lost {}, {} landings, chose {}, kinds {:?}, opened {}, accept_advice {}, winner lands again {}, bound include then choice {}, direct landing {}",
            c.acts.len(),
            c.lost,
            c.landings,
            c.chose,
            c.kinds,
            c.opened,
            c.acts.contains("accept_advice"),
            c.winner_again,
            c.bound_include,
            c.direct
        );
        let missing: Vec<&str> = CORE
            .iter()
            .copied()
            .filter(|a| !c.acts.contains(a))
            .collect();
        if !missing.is_empty() {
            failed.push(format!(
                "swarm seed {seed}: before the freeze, no acknowledged act of {missing:?}"
            ));
        }
        all.acts.extend(c.acts);
        all.lost |= c.lost;
        all.landings = all.landings.max(c.landings);
        all.chose |= c.chose;
        all.kinds.extend(c.kinds);
        all.opened |= c.opened;
        all.winner_again |= c.winner_again;
        all.direct |= c.direct;
    }
    // the union is a property of the tier's seeds: a run of listed seeds (to run a failing
    // seed again) checks each seed's core acts only (final review M8, minors item 46)
    if std::env::var("RDY_QA_SWARM_SEEDS").is_ok() {
        eprintln!("RDY_QA_SWARM_SEEDS is set: the union of the seeds is not checked");
        assert!(failed.is_empty(), "{}", failed.join("\n\n"));
        return;
    }
    let seeds: Vec<u64> = runs().iter().map(|(s, _)| *s).collect();
    let missing: Vec<&str> = Act::ALL_NAMES
        .iter()
        .copied()
        .chain([
            "review_submit",
            "land",
            "dismiss",
            "accept_advice",
            "wc_commit",
            "inbox_ack",
        ])
        .filter(|a| !all.acts.contains(a))
        .collect();
    if !missing.is_empty() {
        failed.push(format!(
            "swarm seeds {seeds:?}: no run reached an acknowledged act of {missing:?} before the freeze"
        ));
    }
    let mut lacks = vec![];
    if !all.lost {
        lacks.push("a call the kill cut".to_string());
    }
    if all.landings < 4 {
        lacks.push(format!("4 landings in one run (at most {})", all.landings));
    }
    if !all.chose {
        lacks.push("a landing that chose a member of an alternative set".to_string());
    }
    for kind in ["edit_collision", "premise_shift"] {
        if !all.kinds.contains(kind) {
            lacks.push(format!("a reported or seen {kind}"));
        }
    }
    if !all.opened {
        lacks.push("an inbox read with a tension_opened entry".to_string());
    }
    if !all.winner_again {
        lacks.push("a set's winner that landed again, choosing nothing".to_string());
    }
    if !all.direct {
        lacks.push("a landing by a direct rdy approve (no relay)".to_string());
    }
    if !lacks.is_empty() {
        failed.push(format!(
            "swarm seeds {seeds:?}: before the freeze, no run had {lacks:?}"
        ));
    }
    assert!(failed.is_empty(), "{}", failed.join("\n\n"));
}

/// Final review I1: each run's project and frozen copy live under this process's directory,
/// which a later swarm process removes once this one has exited (`project::sweep`).
#[test]
fn sg5_a_run_keeps_its_dirs_under_the_process_directory() {
    let parent = rdy_qa::swarm::project::run_parent();
    each(|seed, o| {
        for (what, dir) in [("project", &o.dir), ("frozen copy", &o.frozen)] {
            assert!(
                dir.starts_with(parent),
                "swarm seed {seed}: the {what} is at {}, outside {}",
                dir.display(),
                parent.display()
            );
        }
    });
}

#[test]
fn sg5_no_acknowledged_write_is_lost_across_a_kill() {
    each(|seed, o| {
        let (_keep, copy) = copy_of(&o.frozen);
        let r = Rdy::open(&copy).expect("open the frozen copy");
        let st = r.read();
        let mut missing = vec![];
        let mut checked = 0;
        for e in o.before_freeze() {
            let (Met::Ok, Some(c)) = (&e.outcome, &e.change) else {
                continue;
            };
            checked += 1;
            let stored = c
                .parse::<Hash>()
                .ok()
                .is_some_and(|h| st.reader().has_kind(kinds::CHANGE, &h).unwrap_or(false));
            if !stored {
                missing.push(format!("{c} ({} step {})", e.actor, e.step));
            }
        }
        drop(st);
        r.close().expect("close the frozen copy");
        assert!(checked > 0, "swarm seed {seed}: no write was acknowledged");
        assert!(
            missing.is_empty(),
            "swarm seed {seed}: acknowledged changes the store does not hold: {missing:?}"
        );
    });
}

#[test]
fn sg5_every_reference_tension_is_reported() {
    each(|seed, o| {
        let reference = oracle::reference(&o.frozen);
        let missing: Vec<String> = reference
            .iter()
            .filter(|(label, _)| !o.reported.contains_key(*label))
            .map(|(label, kind)| format!("{label} ({kind})"))
            .collect();
        assert!(
            missing.is_empty(),
            "swarm seed {seed}: reference tensions the relay did not report: {missing:?}"
        );
        // a read-time intent overlap, and the landing form of an open proposal (a row with
        // `proposal`: the gate judges it, and the event-derived set does not hold it), are listed
        // without being in the set
        let extra: Vec<String> = o
            .reported
            .iter()
            .filter(|(label, row)| {
                !reference.contains_key(*label)
                    && row["read_time"] != true
                    && row.get("proposal").is_none()
            })
            .map(|(label, row)| format!("{label} ({})", row["kind"]))
            .collect();
        assert!(
            extra.is_empty(),
            "swarm seed {seed}: reported tensions that are not in the reference, read-time or a landing form: {extra:?}"
        );
    });
}

#[test]
fn sg5_every_journal_collision_is_a_reported_tension() {
    each(|seed, o| {
        let pairs = oracle::collisions(o.before_freeze(), &o.frozen);
        let holders = oracle::holders(&o.frozen);
        let block_of: BTreeMap<&str, &str> = o
            .before_freeze()
            .iter()
            .filter(|e| e.act == "collide" && e.outcome == Met::Ok)
            .filter_map(|e| Some((e.change.as_deref()?, e.block.as_deref()?)))
            .collect();
        // a collision the person dismissed is reported too, as dismissed
        let rows: Vec<&Value> = o.reported.values().chain(o.dismissed.values()).collect();
        let empty = BTreeSet::new();
        let mut unreported = vec![];
        for (a, b) in &pairs {
            let block = block_of[a.as_str()];
            let (va, vb) = (
                holders.get(a).unwrap_or(&empty),
                holders.get(b).unwrap_or(&empty),
            );
            let in_views = |s: &Value, vs: &BTreeSet<String>| {
                s["change"].is_string()
                    && s["views"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|v| v.as_str().is_some_and(|v| vs.contains(v)))
            };
            let found = rows.iter().any(|t| {
                let sides = t["sides"].as_array().cloned().unwrap_or_default();
                t["kind"] == "edit_collision"
                    && t["blocks"]
                        .as_array()
                        .is_some_and(|bs| bs.iter().any(|x| x == block))
                    && sides.iter().enumerate().any(|(i, s)| {
                        in_views(s, va)
                            && sides
                                .iter()
                                .enumerate()
                                .any(|(j, s2)| j != i && in_views(s2, vb))
                    })
            });
            if !found {
                // what a person needs to look at it: each side's entry and views, and the edit
                // collisions reported on the block
                let entry = |c: &str| {
                    o.before_freeze()
                        .iter()
                        .find(|e| e.change.as_deref() == Some(c))
                        .map(|e| {
                            format!(
                                "{} step {} view {:?} base {:?}",
                                e.actor, e.step, e.view, e.base
                            )
                        })
                };
                let on_block: Vec<String> = rows
                    .iter()
                    .filter(|t| {
                        t["kind"] == "edit_collision"
                            && t["blocks"]
                                .as_array()
                                .is_some_and(|bs| bs.iter().any(|x| x == block))
                    })
                    .map(|t| t["sides"].to_string())
                    .collect();
                unreported.push(format!(
                    "{a} ({:?}, in {va:?}) and {b} ({:?}, in {vb:?}) on {block}; reported on the block: {on_block:?}",
                    entry(a),
                    entry(b)
                ));
            }
        }
        assert!(
            unreported.is_empty(),
            "swarm seed {seed}: journal collisions with no edit_collision reported: {unreported:?}"
        );
    });
}

#[test]
fn sg5_no_landing_put_a_blocking_tension_into_canon() {
    each(|seed, o| {
        assert!(
            o.violations.is_empty(),
            "swarm seed {seed}: {:#?}",
            o.violations
        );
    });
}

/// Every landing of canon's chain after the project's seed landings is audited (fix round 1,
/// finding 4): each needs a journaled `land` or `land_found` entry that names it with the base its
/// view read on, so a landing whose reply the kill cut is audited too. The alternative rule mirrors
/// the gate (ruling task-5.1): a change linked to a set needs the landing to choose the set, or an
/// earlier landing of the chain to have chosen the member it is linked to.
#[test]
fn sg5_no_landing_chose_no_alternative_or_crossed_a_conflict() {
    each(|seed, o| {
        let (_keep, copy) = copy_of(&o.dir);
        let r = Rdy::open(&copy).expect("open the drained copy");
        let st = r.read();
        let mut chain = st.reader().landings().expect("read canon's chain");
        chain.reverse();
        // the first entry that names each landing: the one whose base was read before it
        let mut named: BTreeMap<Hash, &Entry> = BTreeMap::new();
        for e in o
            .journal
            .iter()
            .filter(|e| (e.act == "land" && e.outcome == Met::Ok) || e.act == run::LAND_FOUND)
        {
            if let Some(l) = e.reply.as_ref().and_then(|r| r["landing"].as_str())
                && let Ok(h) = l.parse::<Hash>()
            {
                named.entry(h).or_insert(e);
            }
        }
        let mut settled: BTreeMap<Hash, String> = BTreeMap::new();
        let mut bad = vec![];
        let mut audited = 0;
        for (landing, rec) in &chain {
            let seeded = o.seed_landings.contains(&landing.to_string());
            let entry = named.get(landing);
            match (seeded, entry.and_then(|e| e.base.as_deref())) {
                (true, _) => {}
                (false, None) => bad.push(format!(
                    "landing {}: no journaled land entry names it with a base ({:?})",
                    landing.short(),
                    entry.map(|e| (&e.actor, e.step, e.act))
                )),
                (false, Some(base)) => {
                    let e = entry.expect("an entry with a base");
                    let ctx = format!(
                        " (landing {}, {} step {} {}, base {})",
                        landing.short(),
                        e.actor,
                        e.step,
                        e.act,
                        &base[..12.min(base.len())]
                    );
                    let landed = landed_of(&r, landing)
                        .unwrap_or_else(|err| panic!("landing {landing}: {err}"));
                    bad.extend(
                        alt_unchosen(&st, &landed, &settled)
                            .into_iter()
                            .map(|v| v.detail),
                    );
                    // every binding of each landed change, the sets' fixed ones (msw20) too
                    let chose = rec.chosen.as_ref().map(|c| (&c.set, c.member.as_str()));
                    for c in &landed.changes {
                        for (set, member) in broken_bindings(&bindings_of(&st, c), chose, &settled)
                        {
                            bad.push(format!(
                                "change {} is bound to member {member} of set {}, and the landing broke the binding (it chose {chose:?}; chosen before it {:?}){ctx}",
                                c.short(),
                                set.short(),
                                settled.get(&set)
                            ));
                        }
                    }
                    // the conflict rule on the base the view read on when it landed (ruling task-1.1)
                    let base: Hash = base
                        .parse()
                        .unwrap_or_else(|err| panic!("base {base}: {err}"));
                    bad.extend(
                        conflicts_at(&st, &landed, Some(base))
                            .into_iter()
                            .map(|v| format!("{}{ctx}", v.detail)),
                    );
                    audited += 1;
                }
            }
            if let Some(c) = &rec.chosen {
                settled.insert(c.set, c.member.clone());
            }
        }
        drop(st);
        r.close().expect("close the drained copy");
        assert!(audited > 0, "swarm seed {seed}: no landing was audited");
        assert!(bad.is_empty(), "swarm seed {seed}: {bad:#?}");
    });
}

/// The bindings (set, member) of one landed change that its landing broke, mirroring the gate's
/// `linked_alts` from the landing records alone (ruling task-5.1; msw20 bindings too, which
/// `alt_unchosen` does not read). A binding holds when the landing chose its set for its member,
/// or when an earlier landing (`settled`) chose its set for its member (gate rule 2). A binding
/// to a member that lost, to another member of the set this landing chose, or to a set still open
/// that this landing did not choose, is broken.
fn broken_bindings(
    bindings: &[(Hash, String)],
    chose: Option<(&Hash, &str)>,
    settled: &BTreeMap<Hash, String>,
) -> Vec<(Hash, String)> {
    bindings
        .iter()
        .filter(|(set, member)| match chose {
            Some((cs, cm)) if cs == set => cm != member,
            _ => settled.get(set) != Some(member),
        })
        .cloned()
        .collect()
}

#[test]
fn sg5_a_landing_keeps_every_binding_of_its_changes() {
    let (s1, s2) = (Hash::of(b"set one"), Hash::of(b"set two"));
    let b = |set: Hash, m: &str| (set, m.to_string());
    let none = BTreeMap::new();
    let won = BTreeMap::from([(s1, "a0".to_string())]);
    // the landing chooses the bound member
    assert!(broken_bindings(&[b(s1, "a0")], Some((&s1, "a0")), &none).is_empty());
    // the landing chooses another member of the set
    assert_eq!(
        broken_bindings(&[b(s1, "a1")], Some((&s1, "a0")), &none),
        vec![b(s1, "a1")]
    );
    // a set still open that the landing did not choose
    assert_eq!(
        broken_bindings(&[b(s1, "a0")], None, &none),
        vec![b(s1, "a0")]
    );
    assert_eq!(
        broken_bindings(&[b(s1, "a0")], Some((&s2, "a4")), &none),
        vec![b(s1, "a0")]
    );
    // gate rule 2: the winner's change lands later with no choice; a loser's does not
    assert!(broken_bindings(&[b(s1, "a0")], None, &won).is_empty());
    assert_eq!(
        broken_bindings(&[b(s1, "a1")], None, &won),
        vec![b(s1, "a1")]
    );
}

/// The member each alternative set was chosen for, by the landing records of `st`'s canon chain.
fn settled_of(st: &ReadState) -> BTreeMap<Hash, String> {
    chosen_members(&st.reader().landings().expect("read canon's chain"))
}

/// The bindings of change `c` (set, member): by its link (`links.alternative`), and by the
/// binding each set fixed when the change entered it (msw20).
fn bindings_of(st: &ReadState, c: &Hash) -> Vec<(Hash, String)> {
    let ch = st
        .reader()
        .change(c)
        .unwrap_or_else(|err| panic!("change {c}: {err}"));
    let mut bindings: Vec<(Hash, String)> = ch
        .links
        .alternative
        .iter()
        .map(|a| (a.set, a.member.clone()))
        .collect();
    for (set, binds) in &st.coord_heads().alt_binds {
        if let Some(member) = binds.bound.get(c) {
            bindings.push((*set, member.clone()));
        }
    }
    bindings
}

/// Whether change `c` is bound to a member that lost its chosen set.
fn loser_bound(st: &ReadState, settled: &BTreeMap<Hash, String>, c: &Hash) -> bool {
    bindings_of(st, c)
        .iter()
        .any(|(set, member)| settled.get(set).is_some_and(|won| won != member))
}

/// Whether change `c`, or a change it depends on (transitively), is bound to a losing member.
/// A reference is not a dep (`exclude --cascade` follows deps only): a change that cites a block
/// a loser-bound change created is not loser-related (ruling task-5-round2.1).
fn loser_related(st: &ReadState, settled: &BTreeMap<Hash, String>, c: &Hash) -> bool {
    let mut todo = vec![*c];
    let mut seen = BTreeSet::new();
    while let Some(h) = todo.pop() {
        if !seen.insert(h) {
            continue;
        }
        if loser_bound(st, settled, &h) {
            return true;
        }
        let ch = st
            .reader()
            .change(&h)
            .unwrap_or_else(|err| panic!("change {h}: {err}"));
        todo.extend(ch.deps.iter().copied());
    }
    false
}

/// The hex digits after `change ` in a refusal's message.
fn change_named(text: &str) -> Option<String> {
    let at = text.find("change ")? + "change ".len();
    let hex: String = text[at..]
        .chars()
        .take_while(char::is_ascii_hexdigit)
        .collect();
    (hex.len() >= 6).then_some(hex)
}

/// What the bindings of the change an alternative refusal names were when the refusal came, by
/// the sets the landings before it had chosen (`settled`). A losing binding wins over an open
/// one, and an open one over a winning one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AtRefusal {
    /// Bound to a member that lost a set chosen before the refusal.
    Loser,
    /// Bound to a member of a set no landing had chosen yet: the landing must choose.
    OpenSet,
    /// Bound only to members that won a set chosen before the refusal: gate rule 2 lands it.
    Winner,
    /// Bound to no set.
    Unbound,
}

fn at_refusal(bindings: &[(Hash, String)], settled: &BTreeMap<Hash, String>) -> AtRefusal {
    if bindings
        .iter()
        .any(|(set, m)| settled.get(set).is_some_and(|won| won != m))
    {
        AtRefusal::Loser
    } else if bindings.iter().any(|(set, _)| !settled.contains_key(set)) {
        AtRefusal::OpenSet
    } else if bindings.is_empty() {
        AtRefusal::Unbound
    } else {
        AtRefusal::Winner
    }
}

/// Ruling task-5-round1.1: each binding is judged by the set's state when the refusal came.
#[test]
fn sg5_an_alternative_refusal_is_judged_by_the_set_at_that_time() {
    let (s1, s2) = (Hash::of(b"set one"), Hash::of(b"set two"));
    let bound = |set: Hash, m: &str| (set, m.to_string());
    let chose = |pairs: &[(Hash, &str)]| -> BTreeMap<Hash, String> {
        pairs.iter().map(|(s, m)| (*s, (*m).to_string())).collect()
    };
    assert_eq!(at_refusal(&[], &chose(&[])), AtRefusal::Unbound);
    assert_eq!(
        at_refusal(&[bound(s1, "a0")], &chose(&[])),
        AtRefusal::OpenSet
    );
    assert_eq!(
        at_refusal(&[bound(s1, "a0")], &chose(&[(s1, "a1")])),
        AtRefusal::Loser
    );
    assert_eq!(
        at_refusal(&[bound(s1, "a0")], &chose(&[(s1, "a0")])),
        AtRefusal::Winner
    );
    // a set chosen for its winner and a second set still open: the landing must choose there
    assert_eq!(
        at_refusal(&[bound(s1, "a0"), bound(s2, "a4")], &chose(&[(s1, "a0")])),
        AtRefusal::OpenSet
    );
    assert_eq!(
        at_refusal(&[bound(s1, "a0"), bound(s2, "a4")], &chose(&[(s1, "a1")])),
        AtRefusal::Loser
    );
}

/// Ruling task-5.4 and its round-1 ruling: the alternative refusals of the table are ruled rows.
/// `propose`/`E_ALT_CHOSEN` names a change bound to a member that lost a set chosen before it.
/// `land`/`E_ALT_MEMBER` names such a change, or a change bound to a member of a set still open at
/// that landing (the landing must choose), or (naming no change) comes from a view that lost its
/// set before it. A change bound only to a member that an earlier landing chose lands (gate rule
/// 2): a refusal of it is a product defect. The binding is read from the stored change and the
/// sets' fixed bindings; the set's state at each refusal from canon's landing records, in the
/// order the journal names the landings.
/// `land`/`E_VIEW_CLOSED` (D-K0SS1, R1) names no change and comes from a view that lost its set
/// before it: the choosing landing closed the view and withdrew its version.
///
/// The drain's `propose`/`E_ALT_MEMBER` names a change bound to a member of a set still open at
/// that propose. The drain then lands that set's member (the gate's fix: "propose from <member>
/// to choose it"): a landing that chose one of those sets comes before the view proposes again.
/// It never excludes such a change. This is the drain row and settle act of ruling
/// task-5-round3.1; any other drain `propose`/`E_ALT_MEMBER` fails.
#[test]
fn sg5_each_alternative_refusal_is_a_ruled_row() {
    each(|seed, o| {
        let (_keep, copy) = copy_of(&o.dir);
        let r = Rdy::open(&copy).expect("open the drained copy");
        let st = r.read();
        let chain = st.reader().landings().expect("read canon's chain");
        let chosen_by: BTreeMap<String, (Hash, String)> = chain
            .iter()
            .filter_map(|(l, rec)| {
                let c = rec.chosen.as_ref()?;
                Some((l.to_string(), (c.set, c.member.clone())))
            })
            .collect();
        // every change id a refusal may name: the journal's, the drain's exclusions', and every
        // change a view held at the freeze (a write the kill cut names none in the journal)
        let mut known: BTreeSet<String> =
            o.journal.iter().filter_map(|e| e.change.clone()).collect();
        for e in o.after_freeze().iter().filter(|e| e.act == "exclude") {
            for k in ["removed", "cascaded"] {
                if let Some(r) = &e.reply {
                    known.extend(
                        r[k].as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|c| c.as_str().map(str::to_string)),
                    );
                }
            }
        }
        {
            let (_keep, frozen) = copy_of(&o.frozen);
            let f = Rdy::open(&frozen).expect("open the frozen copy");
            let fst = f.read();
            for v in fst.view_names() {
                known.extend(fst.included(&v).iter().map(ToString::to_string));
            }
            drop(fst);
            f.close().expect("close the frozen copy");
        }
        // the sets chosen so far, walking the journal: the seed landings, then each landing as
        // the journal names it (the person is the only one who lands)
        let mut settled: BTreeMap<Hash, String> = o
            .seed_landings
            .iter()
            .filter_map(|l| chosen_by.get(l).cloned())
            .collect();
        let mut bad = vec![];
        let mut rows: BTreeMap<String, usize> = BTreeMap::new();
        let freeze = o.journal.len() - o.after_freeze().len();
        // the set a landing entry chose, by canon's landing records
        let chose = |x: &Entry| -> Option<&(Hash, String)> {
            let landed = (x.act == "land" && x.outcome == Met::Ok) || x.act == run::LAND_FOUND;
            let l = x.reply.as_ref()?["landing"].as_str()?;
            if landed { chosen_by.get(l) } else { None }
        };
        for (k, e) in o.journal.iter().enumerate() {
            if let Some((set, member)) = chose(e) {
                settled.insert(*set, member.clone());
            }
            let drain = k >= freeze;
            let row = match (&e.outcome, e.act) {
                (Met::Refused(c), "propose") if c == "E_ALT_CHOSEN" => c,
                (Met::Refused(c), "land") if c == "E_ALT_MEMBER" => c,
                (Met::Refused(c), "land") if c == "E_VIEW_CLOSED" => c,
                (Met::Refused(c), "propose") if c == "E_ALT_MEMBER" && drain => c,
                _ => continue,
            };
            // the proposal's view and how it was made, for a person to look at
            let made = o.journal.iter().find(|x| {
                x.act == "propose"
                    && x.outcome == Met::Ok
                    && x.block.is_some()
                    && x.block == e.block
            });
            let at = format!(
                "{} step {} {} {row} ({:?} of view {:?}, proposed by {:?} step {:?} with {:?})",
                e.actor,
                e.step,
                e.act,
                e.block,
                e.view,
                made.map(|x| &x.actor),
                made.map(|x| x.step),
                made.and_then(|x| x.reply.as_ref())
            );
            let message = e.message.as_deref().unwrap_or_default();
            let (judged, why) = match change_named(message) {
                Some(prefix) => {
                    let full = known.iter().find(|k| k.starts_with(&prefix));
                    match full.and_then(|f| f.parse::<Hash>().ok()) {
                        Some(c) => {
                            let b = bindings_of(&st, &c);
                            let then = at_refusal(&b, &settled);
                            let won: Vec<_> = b
                                .iter()
                                .map(|(set, _)| (set.short(), settled.get(set)))
                                .collect();
                            (
                                Some(then),
                                format!(" ({then:?}: bindings {b:?}; chosen before it {won:?})"),
                            )
                        }
                        None => (None, " (a change no entry or view names)".to_string()),
                    }
                }
                // the land's view itself lost its set before it (altg1)
                None => {
                    let lost = e.view.as_ref().is_some_and(|v| {
                        st.coord_heads().settled_alts.iter().any(|(set, (_, alt))| {
                            alt.members.iter().any(|m| m.view == *v)
                                && settled.get(set).is_some_and(|won| won != v)
                        })
                    });
                    (lost.then_some(AtRefusal::Loser), String::new())
                }
            };
            let fine = match (row.as_str(), e.act, judged) {
                // the drain's open-set row (ruling task-5-round3.1): a landing chose one of the
                // change's open sets before the view proposed again
                ("E_ALT_MEMBER", "propose", Some(AtRefusal::OpenSet)) => {
                    let open: BTreeSet<Hash> = change_named(message)
                        .and_then(|prefix| known.iter().find(|x| x.starts_with(&prefix)))
                        .and_then(|f| f.parse::<Hash>().ok())
                        .map(|c| bindings_of(&st, &c))
                        .unwrap_or_default()
                        .into_iter()
                        .map(|(set, _)| set)
                        .filter(|set| !settled.contains_key(set))
                        .collect();
                    let next = o.journal[k + 1..]
                        .iter()
                        .position(|x| x.act == "propose" && x.view == e.view)
                        .map_or(o.journal.len(), |p| k + 1 + p);
                    let settles = o.journal[k + 1..next]
                        .iter()
                        .any(|x| chose(x).is_some_and(|(set, _)| open.contains(set)));
                    if !settles {
                        bad.push(format!(
                            "{at}: {message}: no landing chose its open set before the view proposed again"
                        ));
                        continue;
                    }
                    true
                }
                ("E_ALT_MEMBER", "propose", _) => false,
                (_, _, Some(AtRefusal::Loser)) => true,
                ("E_ALT_MEMBER", "land", Some(AtRefusal::OpenSet)) => true,
                _ => false,
            };
            if fine {
                *rows
                    .entry(format!("{} {row} {:?}", e.act, judged))
                    .or_default() += 1;
            } else if judged == Some(AtRefusal::Winner) {
                bad.push(format!(
                    "{at}: {message}{why}: the change is bound only to a member an earlier landing chose, which gate rule 2 lands (a product defect)"
                ));
            } else {
                bad.push(format!("{at}: {message}{why}"));
            }
        }
        drop(st);
        r.close().expect("close the drained copy");
        eprintln!("swarm seed {seed}: alternative refusals by row: {rows:?}");
        assert!(
            bad.is_empty(),
            "swarm seed {seed}: alternative refusals outside the ruled rows: {bad:#?}"
        );
    });
}

/// Ruling task-5-round2.1: the drain meets `propose`/`E_VALIDATION` for a dangling reference only
/// when a change it excluded from that view created the target. The owning agent then rewrites
/// the citing block without the reference (`repair_dangling_ref`, naming the block and the
/// target) and proposes again. Each repair is `Ok`, names a target that a change the drain
/// excluded from that view created (by the journal), and the citing block no longer references
/// the target in canon. Every drain `E_VALIDATION` is followed by a repair in its view.
#[test]
fn sg5_each_dangling_reference_repair_cites_a_discarded_block() {
    each(|seed, o| {
        let drained = o.after_freeze();
        let mut bad = vec![];
        let mut repairs = vec![];
        for (k, e) in drained.iter().enumerate() {
            let v = e.view.as_deref().unwrap_or("");
            if e.act == "propose" && e.outcome == Met::Refused("E_VALIDATION".to_string()) {
                // the next repair in the view rewrites the block and target the refusal names
                let said = e.message.as_deref().unwrap_or("");
                let named = run::dangling_in(said);
                let next = drained[k + 1..]
                    .iter()
                    .find(|x| x.act == "repair_dangling_ref" && x.view.as_deref() == Some(v));
                match (&named, next) {
                    (None, _) => bad.push(format!(
                        "{v}: E_VALIDATION that names no dangling reference: {said}"
                    )),
                    (Some(_), None) => {
                        bad.push(format!("{v}: E_VALIDATION with no repair after it: {said}"))
                    }
                    (Some((block, target)), Some(x)) => {
                        if x.block.as_deref() != Some(block.as_str())
                            || x.target.as_deref() != Some(target.as_str())
                        {
                            bad.push(format!(
                                "{v}: the refusal names {block} -> {target}, the next repair {:?} -> {:?}",
                                x.block, x.target
                            ));
                        }
                    }
                }
            }
            if e.act != "repair_dangling_ref" {
                continue;
            }
            let (Some(block), Some(target)) = (e.block.clone(), e.target.clone()) else {
                bad.push(format!("{v}: a repair names no block or no target: {e:?}"));
                continue;
            };
            if e.outcome != Met::Ok {
                bad.push(format!("{v}: the repair of {block} met {:?}", e.outcome));
            }
            // the changes the drain excluded from this view before the repair
            let excluded: BTreeSet<String> = drained[..k]
                .iter()
                .filter(|x| x.act == "exclude" && x.outcome == Met::Ok)
                .filter(|x| x.view.as_deref() == Some(v))
                .filter_map(|x| x.reply.as_ref())
                .flat_map(|r| {
                    ["removed", "cascaded"].into_iter().flat_map(move |key| {
                        r[key]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|c| c.as_str().map(str::to_string))
                    })
                })
                .collect();
            let discarded = o.journal.iter().any(|x| {
                x.outcome == Met::Ok
                    && x.block.as_deref() == Some(target.as_str())
                    && x.change.as_ref().is_some_and(|c| excluded.contains(c))
            });
            if !discarded {
                bad.push(format!(
                    "{v}: the repair of {block} names {target}, which no change the drain excluded from {v} created"
                ));
            }
            repairs.push((block, target));
        }
        let (_keep, copy) = copy_of(&o.dir);
        let r = Rdy::open(&copy).expect("open the drained copy");
        let st = r.read();
        for (block, target) in &repairs {
            let id = BlockId::parse(block).expect("a block id");
            // the repaired block lands: a citing change is no dependent of the excluded one
            match st.block(&Scope::Canon, &id) {
                Ok(b) if b.ref_tokens().contains(target) => {
                    bad.push(format!("{block} still references {target} in canon"));
                }
                Ok(_) => {}
                Err(e) => bad.push(format!("the repaired block {block} is not in canon: {e}")),
            }
        }
        drop(st);
        r.close().expect("close the drained copy");
        eprintln!("swarm seed {seed}: dangling reference repairs: {repairs:?}");
        assert!(bad.is_empty(), "swarm seed {seed}: {bad:#?}");
    });
}

/// Ruling task-5.4: after the drain no agent view holds a change. A view that landed (a landing
/// record whose proposal's view is it) is told apart from a view the drain emptied: the drain's
/// exclusions are listed by change, and each excluded change is bound to a losing member or
/// depends on one by its deps (a citation is no dep: the drain repairs a citing block instead,
/// ruling task-5-round2.1, so the citing change must land). Every change a view held at the
/// freeze, or that the drain wrote in it, landed or is such an exclusion; an open, empty view
/// never counts as landed by itself.
#[test]
fn sg5_every_live_view_drains_without_a_dismissal() {
    each(|seed, o| {
        // what each agent view held at the freeze
        let held_at_freeze: BTreeMap<String, Vec<Hash>> = {
            let (_keep, copy) = copy_of(&o.frozen);
            let r = Rdy::open(&copy).expect("open the frozen copy");
            let st = r.read();
            let out = st
                .view_names()
                .into_iter()
                .map(|v| {
                    let held = st.included(&v).to_vec();
                    (v, held)
                })
                .collect();
            drop(st);
            r.close().expect("close the frozen copy");
            out
        };
        let (_keep, copy) = copy_of(&o.dir);
        let r = Rdy::open(&copy).expect("open the drained copy");
        let st = r.read();
        let settled = settled_of(&st);
        let chain = st.reader().landings().expect("read canon's chain");
        let canon: BTreeSet<Hash> = chain
            .iter()
            .flat_map(|(_, rec)| rec.changes.clone())
            .collect();
        let landed_from: BTreeSet<String> = chain
            .iter()
            .filter_map(|(_, rec)| st.reader().proposal(&rec.proposal).ok())
            .map(|p| p.view)
            .collect();
        let drained = o.after_freeze();
        let mut live = vec![];
        let mut emptied = vec![];
        let mut bad = vec![];
        let mut agent_views = 0;
        for name in st.view_names() {
            let Some(head) = st.view_head(&name) else {
                continue;
            };
            let by_agent = head
                .owner
                .as_ref()
                .is_some_and(|w| w.actor.starts_with("agent:a"));
            if !by_agent {
                continue;
            }
            agent_views += 1;
            // a member of a chosen or abandoned set is closed: its work is discarded by design
            if head.closed {
                continue;
            }
            let now = st.included(&name);
            if !now.is_empty() {
                live.push(format!("{name} ({} changes)", now.len()));
                continue;
            }
            let mine =
                |e: &&Entry| e.outcome == Met::Ok && e.view.as_deref() == Some(name.as_str());
            let excluded: BTreeSet<Hash> = drained
                .iter()
                .filter(|e| e.act == "exclude")
                .filter(mine)
                .filter_map(|e| e.reply.as_ref())
                .flat_map(|r| {
                    ["removed", "cascaded"].into_iter().flat_map(move |k| {
                        r[k].as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|c| c.as_str()?.parse().ok())
                    })
                })
                .collect();
            let mut held: BTreeSet<Hash> = held_at_freeze
                .get(&name)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .collect();
            held.extend(
                drained
                    .iter()
                    .filter(mine)
                    .filter_map(|e| e.change.as_deref()?.parse::<Hash>().ok()),
            );
            for c in held.iter().chain(&excluded) {
                if canon.contains(c) {
                    continue;
                }
                if !excluded.contains(c) {
                    bad.push(format!(
                        "{name}: change {} left the view without landing or a drain exclusion",
                        c.short()
                    ));
                } else if !loser_related(&st, &settled, c) {
                    bad.push(format!(
                        "{name}: the drain excluded change {}, which is not bound to a losing member and depends on none",
                        c.short()
                    ));
                }
            }
            if !excluded.is_empty() {
                let ids: Vec<String> = excluded.iter().map(|c| c.short()).collect();
                let landed = landed_from.contains(&name);
                // the drain's refusals that led to the exclusions, for a person to look at
                let met: Vec<String> = drained
                    .iter()
                    .filter(|e| e.view.as_deref() == Some(name.as_str()))
                    .filter(|e| matches!(e.outcome, Met::Refused(_)))
                    .map(|e| {
                        format!(
                            "{} {:?}: {}",
                            e.act,
                            e.outcome,
                            e.message.as_deref().unwrap_or("")
                        )
                    })
                    .collect();
                emptied.push(format!(
                    "{name} (landed: {landed}) excluded {ids:?} after {met:?}"
                ));
            }
        }
        drop(st);
        r.close().expect("close the drained copy");
        assert!(agent_views > 0, "swarm seed {seed}: no agent view");
        assert!(
            live.is_empty(),
            "swarm seed {seed}: views neither landed nor closed after the drain: {live:?}"
        );
        assert!(
            bad.is_empty(),
            "swarm seed {seed}: {bad:#?}\nviews the drain emptied: {emptied:#?}"
        );
        let dismissed: Vec<String> = o
            .after_freeze()
            .iter()
            .filter(|e| e.act == "dismiss")
            .map(|e| format!("{} step {}", e.actor, e.step))
            .collect();
        assert!(
            dismissed.is_empty(),
            "swarm seed {seed}: the drain dismissed: {dismissed:?}"
        );
    });
}

#[test]
fn sg5_two_colliding_views_land_one_after_the_other() {
    const SEED: u64 = 21;
    let cfg = Config {
        seed: SEED,
        agents: 2,
        steps: 6,
        kill_at: None,
    };
    let acts = |with| {
        vec![
            Act::Brief {
                target: Pick::Seed(0),
            },
            Act::Collide {
                with,
                target: Pick::Seed(0),
            },
            Act::Propose,
        ]
    };
    let s = Script {
        agents: vec![acts(1), acts(0)],
        person: vec![PersonAct::Land { relayed: true }; 2],
        contra: vec![],
    };
    let o = match seeded(SEED, || run::run_script(&cfg, s)) {
        Ok(o) => o,
        Err(text) => panic!("{text}"),
    };
    // a refused land whose proposal moved is a dead ref, not a landing attempt on the gate
    let lands: Vec<&Entry> = o
        .journal
        .iter()
        .filter(|e| e.act == "land")
        .filter(|e| match &e.outcome {
            Met::Ok => true,
            Met::Refused(code) => code != "E_PROPOSAL_MOVED",
            _ => false,
        })
        .collect();
    let first = lands
        .first()
        .unwrap_or_else(|| panic!("swarm seed {SEED}: no landing: {:#?}", o.journal));
    assert_eq!(
        first.outcome,
        Met::Ok,
        "swarm seed {SEED}: the first landing: {first:#?}"
    );
    let other: Vec<&&Entry> = lands.iter().filter(|e| e.view != first.view).collect();
    let refused = other.first().unwrap_or_else(|| {
        panic!(
            "swarm seed {SEED}: the second view never met the gate: {:#?}",
            o.journal
        )
    });
    // D-ME2HD: the collision is a conflict or a blocking tension, an issue of a version that is
    // not ready; the owner's write or repin below resolves it
    assert!(
        matches!(&refused.outcome, Met::Refused(c) if c == "E_PROPOSAL_NOT_READY"),
        "swarm seed {SEED}: the second landing: {refused:#?}"
    );
    let last = other.last().expect("a land entry");
    assert_eq!(
        last.outcome,
        Met::Ok,
        "swarm seed {SEED}: the second view did not land: {:#?}",
        o.journal
    );
    // between the refusal and the landing, the view's owner resolved or repinned
    let at = |e: &Entry| o.journal.iter().position(|x| std::ptr::eq(x, e)).unwrap();
    let view = refused.view.clone();
    let owner = format!("agent:{}", view.as_deref().unwrap_or_default());
    let fixed = o.journal[at(refused)..at(last)].iter().any(|e| {
        e.actor == owner
            && matches!(e.act, "write" | "repin")
            && e.outcome == Met::Ok
            && e.view == view
    });
    assert!(
        fixed,
        "swarm seed {SEED}: no resolve or repin before the second landing: {:#?}",
        o.journal
    );
    assert_eq!(o.landings.len(), 2, "swarm seed {SEED}: {:?}", o.landings);
    assert!(
        o.journal.iter().all(|e| e.act != "dismiss"),
        "swarm seed {SEED}: a dismissal"
    );
    assert!(
        o.violations.is_empty(),
        "swarm seed {SEED}: {:?}",
        o.violations
    );
}

/// One line per entry: the actor, step, act, outcome, view, block, message and choice.
fn lines(entries: &[Entry]) -> String {
    entries
        .iter()
        .map(|e| {
            format!(
                "  {} step {} {} {:?} view={} block={} {} {}",
                e.actor,
                e.step,
                e.act,
                e.outcome,
                e.view.as_deref().unwrap_or("-"),
                e.block.as_deref().unwrap_or("-"),
                e.message.as_deref().unwrap_or(""),
                e.reply
                    .as_ref()
                    .and_then(|r| r.get("chosen").or_else(|| r.get("alternatives")))
                    .map(ToString::to_string)
                    .unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Ruling task-5-round1 (the msw20 binding path, directed). Agent a0 includes a2's claim and then
/// makes a set with a1, so the set binds the claim to a0; a0 excludes it again. While the set is
/// open, the landing of a2's proposal (which holds the claim) is refused with `E_ALT_MEMBER`: the
/// landing must choose. a0's proposal lands and chooses a0. Then a2's proposal lands with the
/// claim, which is bound to the winner (gate rule 2), and a0's later work lands with no choice
/// (seed 3's pattern).
#[test]
fn sg5_an_included_change_bound_to_the_winner_lands_after_the_choice() {
    const SEED: u64 = 22;
    let cfg = Config {
        seed: SEED,
        agents: 3,
        steps: 16,
        kill_at: None,
    };
    let read = |n| Act::Read {
        target: Pick::Seed(n),
    };
    let brief = |n| Act::Brief {
        target: Pick::Seed(n),
    };
    let write = |n| Act::Write {
        target: Pick::Seed(n),
        stable: false,
    };
    let mut a0 = vec![read(0); 16];
    a0[0] = brief(0);
    a0[1] = write(0);
    a0[3] = Act::Include { from: 2 };
    a0[4] = Act::AltCreate { with: 1 };
    a0[5] = Act::Exclude;
    a0[7] = Act::Propose;
    a0[11] = write(0);
    a0[13] = Act::Propose;
    let mut a1 = vec![read(1); 16];
    a1[0] = brief(1);
    a1[1] = write(1);
    let mut a2 = vec![read(2); 16];
    a2[0] = brief(2);
    a2[1] = Act::Create {
        area: "payments",
        contra: None,
    };
    // a2 proposes before the set exists: a propose of a set-bound change is refused while the
    // set is open, a landing of a version made before it meets the gate
    a2[2] = Act::Propose;
    // the person's act j runs after step 2(j + 1): it lands after steps 6, 8, 10, 12 and 14
    let mut person = vec![PersonAct::Tensions; 8];
    for act in &mut person[2..7] {
        *act = PersonAct::Land { relayed: true };
    }
    let s = Script {
        agents: vec![a0, a1, a2],
        person,
        contra: vec![],
    };
    let o = match seeded(SEED, || run::run_script(&cfg, s)) {
        Ok(o) => o,
        Err(text) => panic!("{text}"),
    };
    let run = o.before_freeze();
    let ok = |e: &&Entry| e.act == "land" && e.outcome == Met::Ok;
    let chose = |e: &Entry| {
        e.reply
            .as_ref()
            .and_then(|r| r.get("chosen"))
            .filter(|c| !c.is_null())
            .and_then(|c| c["member"].as_str().map(str::to_string))
    };
    let at = |e: &Entry| run.iter().position(|x| std::ptr::eq(x, e)).unwrap();
    let choice = run
        .iter()
        .filter(ok)
        .find(|e| chose(e).is_some())
        .unwrap_or_else(|| panic!("swarm seed {SEED}: no landing chose:\n{}", lines(run)));
    assert_eq!(chose(choice).as_deref(), Some("a0"), "{choice:#?}");
    let open = run.iter().take(at(choice)).find(|e| {
        e.act == "land"
            && e.view.as_deref() == Some("a2")
            && e.outcome == Met::Refused("E_ALT_MEMBER".into())
    });
    assert!(
        open.is_some(),
        "swarm seed {SEED}: a2's proposal met no open set:\n{}",
        lines(run)
    );
    let after: Vec<&Entry> = run[at(choice) + 1..].iter().filter(ok).collect();
    assert!(
        after.iter().any(|e| e.view.as_deref() == Some("a2")),
        "swarm seed {SEED}: a2's proposal did not land after the choice:\n{}",
        lines(run)
    );
    assert!(
        after
            .iter()
            .any(|e| e.view.as_deref() == Some("a0") && chose(e).is_none()),
        "swarm seed {SEED}: the winner did not land again without a choice:\n{}",
        lines(run)
    );
    // the claim a0 included is in canon, by a landing of a2's proposal
    let claim = run
        .iter()
        .find(|e| e.actor == "agent:a2" && e.act == "create" && e.outcome == Met::Ok)
        .and_then(|e| e.change.as_deref()?.parse::<Hash>().ok())
        .expect("a2's claim");
    let (_keep, copy) = copy_of(&o.dir);
    let r = Rdy::open(&copy).expect("open the drained copy");
    let st = r.read();
    let bound = bindings_of(&st, &claim);
    assert_eq!(bound.len(), 1, "swarm seed {SEED}: {bound:?}");
    assert_eq!(bound[0].1, "a0", "swarm seed {SEED}: {bound:?}");
    let by_a2 = st
        .reader()
        .landings()
        .expect("read canon's chain")
        .into_iter()
        .any(|(_, rec)| {
            rec.changes.contains(&claim)
                && st
                    .reader()
                    .proposal(&rec.proposal)
                    .is_ok_and(|p| p.view == "a2")
        });
    drop(st);
    r.close().expect("close the drained copy");
    assert!(by_a2, "swarm seed {SEED}: the claim did not land from a2");
    assert!(
        o.violations.is_empty(),
        "swarm seed {SEED}: {:?}",
        o.violations
    );
}

/// The brief's drain order, oldest view first, and ruling task-5.4's exclusion only of a
/// loser-bound change (fix round 1 of the promoted review). a2 creates a claim first; a0
/// includes it and makes a set with a1, so the set binds the claim to a0; nothing lands in the
/// run. The drain takes a2 first: its propose meets `E_ALT_MEMBER` (the claim is the open set's
/// member a0's alternative). The drain excludes nothing: it lands a0 first, which chooses a0 and
/// lands the claim, and then lands a2's own work.
#[test]
fn sg5_the_drain_lands_an_open_sets_bound_member_before_the_view_that_waits_on_it() {
    const SEED: u64 = 23;
    let cfg = Config {
        seed: SEED,
        agents: 3,
        steps: 10,
        kill_at: None,
    };
    let read = |n| Act::Read {
        target: Pick::Seed(n),
    };
    let brief = |n| Act::Brief {
        target: Pick::Seed(n),
    };
    let write = |n| Act::Write {
        target: Pick::Seed(n),
        stable: false,
    };
    // the pacing keeps the agents within one step: a write two steps later comes later in the
    // journal, so the drain's order is a2, a0, a1
    let mut a2 = vec![read(2); 10];
    a2[0] = brief(2);
    a2[1] = Act::Create {
        area: "payments",
        contra: None,
    };
    a2[2] = write(2);
    let mut a0 = vec![read(0); 10];
    a0[0] = brief(0);
    a0[3] = write(0);
    a0[4] = Act::Include { from: 2 };
    a0[7] = Act::AltCreate { with: 1 };
    let mut a1 = vec![read(1); 10];
    a1[0] = brief(1);
    a1[5] = write(1);
    let s = Script {
        agents: vec![a0, a1, a2],
        person: vec![PersonAct::Tensions; 5],
        contra: vec![],
    };
    let o = match seeded(SEED, || run::run_script(&cfg, s)) {
        Ok(o) => o,
        Err(text) => panic!("{text}"),
    };
    let drain = o.after_freeze();
    let shown = lines(drain);
    assert!(
        !o.before_freeze().iter().any(|e| e.act == "land"),
        "swarm seed {SEED}: the person landed in the run:\n{}",
        lines(o.before_freeze())
    );
    assert!(
        o.before_freeze()
            .iter()
            .any(|e| e.act == "alt_create" && e.outcome == Met::Ok),
        "swarm seed {SEED}: no set was made:\n{}",
        lines(o.before_freeze())
    );
    // oldest first: a2's propose is the drain's first, and the open set refuses it
    let first = drain
        .iter()
        .position(|e| e.act == "propose")
        .unwrap_or_else(|| panic!("swarm seed {SEED}: the drain proposed nothing:\n{shown}"));
    assert_eq!(
        drain[first].view.as_deref(),
        Some("a2"),
        "swarm seed {SEED}: the drain did not take the oldest view first:\n{shown}"
    );
    assert_eq!(
        drain[first].outcome,
        Met::Refused("E_ALT_MEMBER".into()),
        "swarm seed {SEED}:\n{shown}"
    );
    let ok = |e: &&Entry| e.act == "land" && e.outcome == Met::Ok;
    let chosen = |e: &Entry| {
        e.reply
            .as_ref()
            .and_then(|r| r.get("chosen"))
            .and_then(|c| c["member"].as_str().map(str::to_string))
    };
    let choice = drain[first..]
        .iter()
        .position(|e| ok(&e) && chosen(e).is_some())
        .map(|p| first + p)
        .unwrap_or_else(|| panic!("swarm seed {SEED}: no drain landing chose:\n{shown}"));
    assert_eq!(
        (
            drain[choice].view.as_deref(),
            chosen(&drain[choice]).as_deref()
        ),
        (Some("a0"), Some("a0")),
        "swarm seed {SEED}: the drain did not land the bound member first:\n{shown}"
    );
    assert!(
        drain[choice..]
            .iter()
            .any(|e| ok(&e) && e.view.as_deref() == Some("a2")),
        "swarm seed {SEED}: a2's own work did not land after the choice:\n{shown}"
    );
    assert!(
        !drain.iter().any(|e| e.act == "exclude"),
        "swarm seed {SEED}: the drain excluded a change bound to an open set's member:\n{shown}"
    );
    // the claim is in canon
    let claim = o
        .journal
        .iter()
        .find(|e| e.actor == "agent:a2" && e.act == "create" && e.outcome == Met::Ok)
        .and_then(|e| e.change.as_deref()?.parse::<Hash>().ok())
        .expect("a2's claim");
    let (_keep, copy) = copy_of(&o.dir);
    let r = Rdy::open(&copy).expect("open the drained copy");
    let st = r.read();
    let bound = bindings_of(&st, &claim);
    let landed = st
        .reader()
        .landings()
        .expect("read canon's chain")
        .into_iter()
        .any(|(_, rec)| rec.changes.contains(&claim));
    drop(st);
    r.close().expect("close the drained copy");
    assert_eq!(bound.len(), 1, "swarm seed {SEED}: {bound:?}");
    assert_eq!(bound[0].1, "a0", "swarm seed {SEED}: {bound:?}");
    assert!(
        landed,
        "swarm seed {SEED}: the claim is not in canon:\n{shown}"
    );
    assert!(
        o.violations.is_empty(),
        "swarm seed {SEED}: {:?}",
        o.violations
    );
}

/// Ruling task-5.4: the closed table holds `propose`/`E_ALT_CHOSEN` and `land`/`E_ALT_MEMBER`,
/// which the gate answers for a change bound to a member that lost its set (FW-AL1).
/// `sg5_each_alternative_refusal_is_a_ruled_row` holds every such refusal of a run to its ruled
/// rows (and `land`/`E_ALT_MEMBER` also to a set still open at that landing, ruling
/// task-5-round1.1).
#[test]
fn sg5_the_table_holds_the_alternative_refusals_a_run_meets() {
    assert!(allowed("propose", "E_ALT_CHOSEN"));
    assert!(allowed("land", "E_ALT_MEMBER"));
    assert!(!allowed("write", "E_ALT_CHOSEN"));
}

#[test]
fn sg5_every_acknowledged_stable_write_carried_a_brief() {
    each(|seed, o| {
        let unbriefed: Vec<String> = o
            .journal
            .iter()
            .filter(|e| e.outcome == Met::Ok && e.stable && e.brief.is_none())
            .map(|e| format!("{} step {} ({})", e.actor, e.step, e.act))
            .collect();
        assert!(
            unbriefed.is_empty(),
            "swarm seed {seed}: acknowledged stable writes with no briefing: {unbriefed:?}"
        );
        let landed_unbriefed: Vec<String> = o
            .journal
            .iter()
            .filter(|e| e.act == "write_no_brief")
            .filter(|e| !matches!(&e.outcome, Met::Lost | Met::Skipped))
            .filter(|e| e.outcome != Met::Refused("E_BRIEF_REQUIRED".into()))
            .map(|e| format!("{} step {}: {:?}", e.actor, e.step, e.outcome))
            .collect();
        assert!(
            landed_unbriefed.is_empty(),
            "swarm seed {seed}: unbriefed writes the daemon did not refuse with E_BRIEF_REQUIRED: {landed_unbriefed:?}"
        );
        // the second write of a stale-briefing act is the second `write_stale` entry of its step
        let mut by_step: BTreeMap<(&str, usize), Vec<&Entry>> = BTreeMap::new();
        for e in o.journal.iter().filter(|e| e.act == "write_stale") {
            by_step.entry((&e.actor, e.step)).or_default().push(e);
        }
        let stale_landed: Vec<String> = by_step
            .values()
            .filter(|es| es.len() == 2)
            .map(|es| es[1])
            .filter(|e| e.outcome != Met::Lost)
            .filter(|e| e.outcome != Met::Refused("E_BRIEF_STALE".into()))
            .map(|e| format!("{} step {}: {:?}", e.actor, e.step, e.outcome))
            .collect();
        assert!(
            stale_landed.is_empty(),
            "swarm seed {seed}: second writes on a stale briefing the daemon did not refuse with E_BRIEF_STALE: {stale_landed:?}"
        );
    });
}

#[test]
fn sg5_the_log_holds_every_tension_a_reader_saw() {
    each(|seed, o| {
        let opened: BTreeSet<String> = logged_kinds(o).into_keys().collect();
        let entries = o.before_drain();
        let last = entries.iter().map(|e| e.epoch).max().unwrap_or(0);
        let missing: Vec<String> = entries
            .iter()
            // a read before the kill may have seen a batch the kill dropped
            .filter(|e| e.epoch == last || (e.outcome == Met::Ok && ACKNOWLEDGED.contains(&e.act)))
            .flat_map(|e| {
                e.seen
                    .iter()
                    .filter(|l| !opened.contains(*l))
                    .map(move |l| format!("{l} ({} step {} {})", e.actor, e.step, e.act))
            })
            .collect();
        assert!(
            missing.is_empty(),
            "swarm seed {seed}: tensions a reader saw that the log never opened: {missing:?}"
        );
    });
}

/// The tension labels an inbox entry names: the tension its `ids` start with, and the one a
/// `tension_opened` entry says it replaces (a re-key on the reader's change is one entry).
fn entry_labels(x: &Value) -> Vec<String> {
    let mut out = vec![];
    if let Some(id) = x["ids"][0].as_str() {
        out.push(format!("tn-{}", &id[..id.len().min(12)]));
    }
    if x["event"] == "tension_opened"
        && let Some(line) = x["line"].as_str()
        && let Some(at) = line.find(" replaces ")
    {
        let hex: String = line[at + " replaces ".len()..].chars().take(12).collect();
        out.push(format!("tn-{hex}"));
    }
    out
}

fn is_tension_entry(x: &Value) -> bool {
    matches!(
        x["event"].as_str(),
        Some("tension_opened" | "tension_resolved" | "tension_dismissed")
    )
}

#[test]
fn sg5_each_inbox_is_a_function_of_the_log() {
    each(|seed, o| {
        let moves = oracle::logged_moves(&o.frozen);
        let (_keep, copy) = copy_of(&o.frozen);
        let mut r = Rdy::open(&copy).expect("open the frozen copy");
        r.reindex().expect("reindex the frozen copy");
        let st = r.read();
        let subs = st.reader().inboxes().expect("read the inboxes");
        // each agent's reads, by range, in journal order
        let mut ranges: BTreeMap<(String, u32), Vec<&Entry>> = BTreeMap::new();
        for e in o.before_drain() {
            if e.actor.starts_with("agent:a")
                && let Some(read) = &e.inbox
            {
                ranges
                    .entry((e.actor.clone(), read.range))
                    .or_default()
                    .push(e);
            }
        }
        assert!(
            !ranges.is_empty(),
            "swarm seed {seed}: no agent read its inbox"
        );
        let mut wrong = vec![];
        for ((actor, range), reads) in &ranges {
            let run_name = actor.trim_start_matches("agent:").to_string();
            let first = reads[0].inbox.as_ref().expect("a read");
            let lastr = reads[reads.len() - 1].inbox.as_ref().expect("a read");
            // a range that starts where the daemon persisted the agent's cursor, which the agent
            // does not know (an ack the kill cut, or none yet): its start cannot be checked
            if first.since.is_empty() {
                eprintln!("swarm seed {seed}: {actor} range {range} starts at an unknown cursor");
                continue;
            }
            let (Some(since), Some(upto)) =
                (parse_cursor(&first.since), parse_cursor(&lastr.cursor))
            else {
                wrong.push(format!(
                    "{actor} range {range}: cursors {:?} .. {:?} do not parse",
                    first.since, lastr.cursor
                ));
                continue;
            };
            // the logged moves of the range whose sides hold a change of this actor and run
            let want: BTreeSet<String> = moves
                .iter()
                .filter(|(seq, _)| *seq > since.at && *seq <= upto.at)
                .filter_map(|(_, k)| match k {
                    EventKind::TensionOpened(m) | EventKind::TensionResolved(m) => Some(m),
                    _ => None,
                })
                .filter(|m| {
                    m.sides.iter().any(|s| {
                        s.change.is_some()
                            && s.actor.as_deref() == Some(actor.as_str())
                            && s.run.as_deref() == Some(run_name.as_str())
                    })
                })
                .map(|m| tension_label(&m.tension))
                .collect();
            let journaled: Vec<&Value> = reads
                .iter()
                .flat_map(|e| e.inbox.as_ref().expect("a read").entries.iter())
                .filter(|x| is_tension_entry(x))
                .collect();
            let got: BTreeSet<String> = journaled.iter().flat_map(|x| entry_labels(x)).collect();
            if got != want {
                wrong.push(format!(
                    "{actor} range {range}: the log names {:?}, the inbox gave {:?}",
                    want.difference(&got).collect::<Vec<_>>(),
                    got.difference(&want).collect::<Vec<_>>()
                ));
            }
            // a reindexed copy gives the same tension entries in the same order
            let sub = subs
                .iter()
                .find(|s| s.actor == *actor && s.run.as_deref() == Some(run_name.as_str()))
                .unwrap_or_else(|| panic!("no subscription of {actor}/{run_name}"));
            let mut again: Vec<Value> = vec![];
            let mut at = since;
            loop {
                let page = st
                    .inbox_page(sub, Some(at), 1 << 40)
                    .unwrap_or_else(|err| panic!("{actor} range {range}: {err}"));
                for x in &page.entries {
                    if x.seq <= upto.at {
                        again.push(serde_json::to_value(x).expect("an entry serializes"));
                    }
                }
                if !page.more || page.next.at > upto.at || page.next == at {
                    break;
                }
                at = page.next;
            }
            let again: Vec<&Value> = again.iter().filter(|x| is_tension_entry(x)).collect();
            if again != journaled {
                wrong.push(format!(
                    "{actor} range {range}: the reindexed copy gives {} tension entries, the reads gave {}",
                    again.len(),
                    journaled.len()
                ));
            }
        }
        drop(st);
        r.close().expect("close the frozen copy");
        assert!(wrong.is_empty(), "swarm seed {seed}: {wrong:#?}");
    });
}

/// How much each oracle had to check in one seed's run.
#[derive(Debug, Default, Clone, Copy)]
struct Weighed {
    /// Open tensions of the reference set.
    reference: usize,
    /// Tension moves the frozen log holds.
    logged: usize,
    /// Seen labels the log check holds to the log.
    seen: usize,
    /// Journal collision pairs.
    collisions: usize,
    /// Tension entries of the agents' inbox reads.
    inbox: usize,
    /// Second writes on a stale briefing, refused with `E_BRIEF_STALE`.
    stale: usize,
    /// Unbriefed stable writes, refused with `E_BRIEF_REQUIRED`.
    no_brief: usize,
}

/// What the oracles of `runs` lacked: an assertion over an empty set passes whatever the product
/// does. Each seed's run has reference tensions, logged moves and seen labels to check; the union
/// of the seeds has journal collisions, inbox tension entries and both briefing refusals.
fn vacuous(runs: &[(u64, Weighed)]) -> Vec<String> {
    let mut out = vec![];
    for (seed, w) in runs {
        for (what, n) in [
            ("reference tensions", w.reference),
            ("logged tension moves", w.logged),
            ("seen labels held to the log", w.seen),
        ] {
            if n == 0 {
                out.push(format!("swarm seed {seed}: no {what}"));
            }
        }
    }
    let seeds: Vec<u64> = runs.iter().map(|(s, _)| *s).collect();
    let sum = |f: fn(&Weighed) -> usize| runs.iter().map(|(_, w)| f(w)).sum::<usize>();
    for (what, n) in [
        ("journal collision pair", sum(|w| w.collisions)),
        ("inbox tension entry", sum(|w| w.inbox)),
        ("stale briefing refused", sum(|w| w.stale)),
        ("unbriefed write refused", sum(|w| w.no_brief)),
    ] {
        if n == 0 {
            out.push(format!("swarm seeds {seeds:?}: no {what}"));
        }
    }
    out
}

#[test]
fn sg5_an_oracle_with_nothing_to_check_is_reported() {
    let full = Weighed {
        reference: 1,
        logged: 1,
        seen: 1,
        collisions: 1,
        inbox: 1,
        stale: 1,
        no_brief: 1,
    };
    assert!(vacuous(&[(1, full)]).is_empty());
    let empty = Weighed::default();
    assert_eq!(
        vacuous(&[(1, full), (2, empty)]),
        vec![
            "swarm seed 2: no reference tensions".to_string(),
            "swarm seed 2: no logged tension moves".to_string(),
            "swarm seed 2: no seen labels held to the log".to_string(),
        ]
    );
    assert_eq!(vacuous(&[(2, empty)]).len(), 7);
}

/// Adversarial review: every oracle above had something to check (`vacuous`).
#[test]
fn sg5_the_oracles_are_not_vacuous() {
    let mut weighed = vec![];
    let mut failed = vec![];
    for (seed, r) in runs() {
        let o = match r {
            Ok(o) => o,
            Err(text) => {
                failed.push(text.clone());
                continue;
            }
        };
        let entries = o.before_drain();
        let last = entries.iter().map(|e| e.epoch).max().unwrap_or(0);
        let mut by_step: BTreeMap<(&str, usize), Vec<&Entry>> = BTreeMap::new();
        for e in o.journal.iter().filter(|e| e.act == "write_stale") {
            by_step.entry((&e.actor, e.step)).or_default().push(e);
        }
        let w = Weighed {
            reference: oracle::reference(&o.frozen).len(),
            logged: oracle::logged_moves(&o.frozen).len(),
            seen: entries
                .iter()
                .filter(|e| {
                    e.epoch == last || (e.outcome == Met::Ok && ACKNOWLEDGED.contains(&e.act))
                })
                .map(|e| e.seen.len())
                .sum(),
            collisions: oracle::collisions(o.before_freeze(), &o.frozen).len(),
            inbox: entries
                .iter()
                .filter(|e| e.actor.starts_with("agent:a"))
                .filter_map(|e| e.inbox.as_ref())
                .flat_map(|r| r.entries.iter())
                .filter(|x| is_tension_entry(x))
                .count(),
            stale: by_step
                .values()
                .filter(|es| es.len() == 2)
                .filter(|es| es[1].outcome == Met::Refused("E_BRIEF_STALE".into()))
                .count(),
            no_brief: o
                .journal
                .iter()
                .filter(|e| e.act == "write_no_brief")
                .filter(|e| e.outcome == Met::Refused("E_BRIEF_REQUIRED".into()))
                .count(),
        };
        eprintln!("swarm seed {seed}: the oracles weighed {w:?}");
        weighed.push((*seed, w));
    }
    failed.extend(vacuous(&weighed));
    assert!(failed.is_empty(), "{}", failed.join("\n\n"));
}

#[test]
fn sg5_the_checker_and_reindex_pass_at_the_end() {
    each(|seed, o| {
        for (what, dir) in [
            ("the frozen copy", &o.frozen),
            ("the drained project", &o.dir),
        ] {
            let (_keep, copy) = copy_of(dir);
            let r = Rdy::open(&copy).unwrap_or_else(|err| panic!("open {what}: {err}"));
            let checked = check_all(&r);
            r.close()
                .unwrap_or_else(|err| panic!("close {what}: {err}"));
            assert!(
                checked.is_ok(),
                "swarm seed {seed}: the checker on {what}: {checked:#?}"
            );
            let reindexed = reindex_reproduces(&copy);
            assert!(
                reindexed.is_ok(),
                "swarm seed {seed}: the reindex audit on {what}: {reindexed:#?}"
            );
        }
        let (ok, fsck) = oracle::fsck(&o.dir, &o.served.env_pairs());
        assert!(ok, "swarm seed {seed}: rdy fsck failed: {fsck}");
        assert_eq!(
            fsck["unlogged_moves"], 0,
            "swarm seed {seed}: rdy fsck: {fsck}"
        );
    });
}

/// D-K0SS1, R1: a land of a version that a close of its view withdrew (a losing member's, by
/// the landing that chose its set) meets `E_VIEW_CLOSED`, a row of the closed table.
#[test]
fn vw2_the_closed_table_lets_a_land_meet_e_view_closed() {
    assert!(allowed("land", "E_VIEW_CLOSED"));
}

/// Diagnosis pin (M3a Task 6, D-TX3KH; D-ME2HD and O12 keep E_STALE_BASE at landing): a0 and
/// a1 append a claim under one section from one base, so both claims hold one order key. a0
/// proposes, then a1 proposes and edits its own claim again. a0 lands first; a1's version then
/// needs a new order key for a claim its view changed after the proposal, and the landing is
/// refused with the order-key E_STALE_BASE. a1 proposes again and that version lands.
#[test]
fn sg5_a_landing_whose_rekeyed_claim_changed_after_the_proposal_is_refused_then_lands() {
    const SEED: u64 = 23;
    let cfg = Config {
        seed: SEED,
        agents: 2,
        steps: 8,
        kill_at: None,
    };
    let read = |n| Act::Read {
        target: Pick::Seed(n),
    };
    let create = || Act::Create {
        area: "payments",
        contra: None,
    };
    let mut a0 = vec![read(0); 8];
    a0[0] = create();
    a0[1] = Act::Propose;
    let mut a1 = vec![read(1); 8];
    a1[0] = create();
    a1[2] = Act::Propose;
    a1[3] = Act::Write {
        target: Pick::Own(0),
        stable: true,
    };
    a1[7] = Act::Propose;
    // the person's act j runs after step 2(j + 1): it lands after steps 4, 6 and 8, and a1
    // proposes again at step 7, after the landing due at step 6
    let person = vec![
        PersonAct::Tensions,
        PersonAct::Land { relayed: true },
        PersonAct::Land { relayed: true },
        PersonAct::Land { relayed: true },
    ];
    let s = Script {
        agents: vec![a0, a1],
        person,
        contra: vec![],
    };
    let o = match seeded(SEED, || run::run_script(&cfg, s)) {
        Ok(o) => o,
        Err(text) => panic!("{text}"),
    };
    let run = o.before_freeze();
    let stale = run.iter().find(|e| {
        e.act == "land"
            && e.view.as_deref() == Some("a1")
            && e.outcome == Met::Refused("E_STALE_BASE".into())
    });
    let stale = stale.unwrap_or_else(|| {
        panic!(
            "swarm seed {SEED}: a1's first version met no stale order key:\n{}",
            lines(run)
        )
    });
    assert!(
        stale
            .message
            .as_deref()
            .is_some_and(|m| m.contains("needs a new order key")),
        "{stale:#?}"
    );
    assert!(
        run.iter()
            .any(|e| e.act == "land" && e.view.as_deref() == Some("a1") && e.outcome == Met::Ok),
        "swarm seed {SEED}: a1's later version did not land:\n{}",
        lines(run)
    );
    assert!(
        o.violations.is_empty(),
        "swarm seed {SEED}: {:?}",
        o.violations
    );
}

/// The blocks of `c`'s ops that are stable content on `m` (the canon its version recorded), as a
/// write's briefing check names its subjects (O3 (a)): a create's parent, or the nearest block
/// above it that `m` holds when the parent is made in the same change; an update's or a delete's
/// own block. A subject `m` does not hold is skipped, and so is one of a presentational type.
fn stable_subjects(st: &ReadState, m: &Materialized, c: &Change) -> Vec<String> {
    let made: BTreeMap<&BlockId, Option<&BlockId>> = c
        .ops
        .iter()
        .filter_map(|op| match op {
            Op::Create { block } => Some((&block.id, block.parent.as_ref())),
            _ => None,
        })
        .collect();
    let mut out: Vec<String> = c
        .ops
        .iter()
        .filter_map(|op| {
            let id = match op {
                Op::Create { block } => {
                    let mut at = block.parent.as_ref()?;
                    // a parent made in this change hangs from the block above it
                    for _ in 0..made.len() {
                        match made.get(at) {
                            Some(up) => at = (*up)?,
                            None => break,
                        }
                    }
                    at
                }
                Op::Update { block, .. } => &block.id,
                Op::Delete { id, .. } => id,
            };
            let b = m.get(id)?;
            (!st.pack().is_presentational(&b.block_type) && stable_on(m, id))
                .then(|| id.to_string())
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Whether the `lifecycle` of `id` on `m`, or that of its nearest ancestor with one, is `stable`.
fn stable_on(m: &Materialized, id: &BlockId) -> bool {
    let mut cur = m.get(id);
    for _ in 0..=m.len() {
        let Some(b) = cur else { return false };
        if let Some(l) = b.fields.get("lifecycle").and_then(|v| v.as_str()) {
            return l == "stable";
        }
        cur = b.parent.as_ref().and_then(|p| m.get(p));
    }
    false
}

/// rg15 (D-VF4RH, D-ME2HD item 1, O3): from the drained store, each change that a landing of the
/// run or the drain put into canon, and that touches stable content on canon at its version's
/// `canon_at`, records the briefing its writer read. O3's exemptions: (a) a change that touches
/// no stable content (`stable_subjects`); (b) a change `rdy import` wrote; (d) a replay of one.
/// (c), a change before the store's first briefed change, holds only for the seed: its second
/// landing lands the first briefed change, and every landing after the seed's lands changes
/// written later, so the seed's landings are the ones left out.
#[test]
fn rg15_every_landed_stable_change_records_a_briefing() {
    let checked = std::cell::Cell::new(0usize);
    each(|seed, o| {
        let (_keep, copy) = copy_of(&o.dir);
        let r = Rdy::open(&copy).expect("open the drained copy");
        let st = r.read();
        let reader = st.reader();
        let seeded: BTreeSet<&str> = o.seed_landings.iter().map(String::as_str).collect();
        let imported = |c: &Change| c.links.imported_by.is_some();
        let mut canon: BTreeMap<Hash, std::sync::Arc<Materialized>> = BTreeMap::new();
        let mut unbriefed = vec![];
        for (l, rec) in reader.landings().expect("read canon's chain") {
            if seeded.contains(l.to_string().as_str()) {
                continue;
            }
            let p = reader
                .proposal(&rec.proposal)
                .unwrap_or_else(|e| panic!("swarm seed {seed}: the version of {l}: {e}"));
            // the empty canon holds no stable content
            let Some(at) = p.canon_at else { continue };
            let m = canon
                .entry(at)
                .or_insert_with(|| {
                    st.materialized(&Scope::At(at))
                        .unwrap_or_else(|e| panic!("swarm seed {seed}: canon at {at}: {e}"))
                })
                .clone();
            for h in &rec.changes {
                let c = reader
                    .change(h)
                    .unwrap_or_else(|e| panic!("swarm seed {seed}: change {h}: {e}"));
                if imported(&c) {
                    continue;
                }
                if let Some(src) = c.links.replayed_from
                    && reader.change(&src).is_ok_and(|s| imported(&s))
                {
                    continue;
                }
                let stable = stable_subjects(&st, &m, &c);
                if stable.is_empty() {
                    continue;
                }
                checked.set(checked.get() + 1);
                if c.links.briefing.is_none() {
                    unbriefed.push(format!(
                        "landing {} change {} touches {stable:?}",
                        l.short(),
                        h.short()
                    ));
                }
            }
        }
        drop(st);
        r.close().expect("close the drained copy");
        assert!(
            unbriefed.is_empty(),
            "swarm seed {seed}: landed changes on stable content with no briefing: {unbriefed:#?}"
        );
    });
    eprintln!("rg15: {} landed changes on stable content", checked.get());
    assert!(
        checked.get() > 0,
        "no landing of any seed put a change on stable content into canon: the check is vacuous"
    );
}

/// The act and the reason the swarm answers an open plan item of `kind` with (D-ME2HD,
/// `allowed_acts`): `None` for a code item, which never gates and stays open.
fn swarm_answer(kind: &str) -> Option<(&'static str, &'static str)> {
    match kind {
        "confirm" => Some(("confirm", "swarm: checked")),
        "update" | "rejustify" | "reaffirm" => Some(("defer", "swarm: later")),
        "tension" => Some(("confirm", "swarm: accepted")),
        _ => None,
    }
}

/// rg15 (D-ME2HD, D-45RG6): every agent reads its view's change plan before it proposes, and
/// answers each open item that gates or warns, in one `rdy_plan` call, with the act its kind
/// allows (`allowed_acts`): `confirm` for a confirm and a tension item, `defer` for an update, a
/// rejustify and a re-affirm item; a code item stays open. No `rdy_plan` call of the run or the
/// drain is refused with `E_PLAN_ITEM`, and some seed answers an item.
#[test]
fn rg15_agents_answer_each_gating_item_with_its_allowed_act() {
    let answered = std::cell::Cell::new(0usize);
    let by_kind = std::cell::RefCell::new(BTreeMap::<String, usize>::new());
    each(|seed, o| {
        let mut bad = vec![];
        for e in &o.journal {
            if matches!(e.act, "plan" | "plan_answer")
                && e.outcome == Met::Refused("E_PLAN_ITEM".into())
            {
                bad.push(format!(
                    "{} step {} {}: E_PLAN_ITEM: {}",
                    e.actor,
                    e.step,
                    e.act,
                    e.message.as_deref().unwrap_or("")
                ));
            }
            if e.act != "plan_answer" {
                continue;
            }
            let sent = e
                .reply
                .as_ref()
                .and_then(|r| r["answers"].as_array().cloned())
                .unwrap_or_default();
            if sent.is_empty() {
                bad.push(format!(
                    "{} step {}: a plan_answer that names no answer",
                    e.actor, e.step
                ));
            }
            for a in &sent {
                let kind = a["kind"].as_str().unwrap_or_default();
                let act = a["act"].as_str().unwrap_or_default();
                let reason = a["reason"].as_str().unwrap_or_default();
                let allows = serde_json::from_value::<PlanKind>(Value::from(kind))
                    .is_ok_and(|k| allowed_acts(k).iter().any(|x| x.as_str() == act));
                if !allows || swarm_answer(kind) != Some((act, reason)) {
                    bad.push(format!(
                        "{} step {}: item {} of kind {kind} answered {act} ({reason})",
                        e.actor, e.step, a["item"]
                    ));
                }
            }
            if e.outcome == Met::Ok {
                answered.set(answered.get() + sent.len());
                for a in &sent {
                    let kind = a["kind"].as_str().unwrap_or_default().to_string();
                    *by_kind.borrow_mut().entry(kind).or_default() += 1;
                }
            }
        }
        // each proposal an agent made in the run came after its read of the view's plan, in the
        // same step
        for (k, e) in o.before_freeze().iter().enumerate() {
            if e.act != "propose" || e.outcome != Met::Ok || !e.actor.starts_with("agent:a") {
                continue;
            }
            let read = o.before_freeze()[..k].iter().rev().any(|x| {
                x.actor == e.actor && x.step == e.step && x.act == "plan" && x.view == e.view
            });
            if !read {
                bad.push(format!(
                    "{} step {}: proposed {:?} with no read of its plan first",
                    e.actor, e.step, e.view
                ));
            }
        }
        assert!(bad.is_empty(), "swarm seed {seed}: {bad:#?}");
    });
    eprintln!(
        "rg15: {} plan items answered, by kind {:?}",
        answered.get(),
        by_kind.borrow()
    );
    assert!(
        answered.get() > 0,
        "no seed answered a plan item: the check is vacuous"
    );
}

/// The open items of a plan read's `counts.by_kind` that gate or warn: every kind but the code
/// items (`update_implementation`, which never gate).
fn gating_open(counts: &Value) -> Vec<String> {
    counts["by_kind"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(k, c)| k.as_str() != "update_implementation" && c["open"] != 0)
        .map(|(k, c)| format!("{k}: {}", c["open"]))
        .collect()
}

/// rg15 (D-ME2HD, D-3JZP4, D-AAQ9Z), seeds 1 to 6: every live agent view drains through the
/// readiness gate. After the drain each agent view is closed or holds no change. Before each
/// approve of the run and the drain, the relay reads the version's change plan (a
/// `version_plan` entry naming it), and that read shows no open item that gates or warns. From
/// the drained store, each landing's seal holds no open item but a code item, and each item it
/// seals as deferred names the answer that accepted the deferral. Some seed lands an item an
/// agent answered.
#[test]
fn rg15_every_live_view_drains_through_the_gate() {
    let answered = std::cell::Cell::new(0usize);
    let (deferred, accepted) = (std::cell::Cell::new(0usize), std::cell::Cell::new(0usize));
    each(|seed, o| {
        let mut bad = vec![];
        // the plan read before each landing of the run and the drain
        let mut landed: BTreeMap<String, String> = BTreeMap::new();
        // the landings with open items, for a person to look at
        let mut look: Vec<(String, String)> = vec![];
        for (k, e) in o.journal.iter().enumerate() {
            let landing = match (e.act, &e.outcome, e.reply.as_ref()) {
                ("land", Met::Ok, Some(r)) => r["landing"].as_str(),
                (run::LAND_FOUND, _, Some(r)) => r["landing"].as_str(),
                _ => None,
            };
            let (Some(l), Some(pref)) = (landing, e.block.as_deref()) else {
                continue;
            };
            landed.insert(l.to_string(), pref.to_string());
            let read = o.journal[..k].iter().rev().find(|x| {
                x.act == "version_plan" && x.outcome == Met::Ok && x.block.as_deref() == Some(pref)
            });
            match read.and_then(|x| x.reply.as_ref()) {
                None => bad.push(format!(
                    "{} step {}: landed {pref} with no read of its plan first",
                    e.actor, e.step
                )),
                Some(r) => {
                    let open = gating_open(&r["counts"]);
                    if !open.is_empty() {
                        bad.push(format!(
                            "{} step {}: landed {pref} with open plan items {open:?}",
                            e.actor, e.step
                        ));
                        look.push((l.to_string(), pref.to_string()));
                    }
                }
            }
        }
        accepted.set(
            accepted.get()
                + o.journal
                    .iter()
                    .filter(|e| e.act == "accept_deferrals" && e.outcome == Met::Ok)
                    .count(),
        );
        // the seals, from the drained store
        let (_keep, copy) = copy_of(&o.dir);
        let r = Rdy::open(&copy).expect("open the drained copy");
        let st = r.read();
        for (l, pref) in &landed {
            let q = PlanQuery {
                landing: Some(l.clone()),
                max_tokens: Some(100_000),
                ..Default::default()
            };
            let seal = st
                .change_plan(&q, "work")
                .map(|p| serde_json::to_value(p).expect("a plan reply serializes"))
                .unwrap_or_else(|e| panic!("swarm seed {seed}: the seal of {l}: {e}"));
            for i in seal["items"].as_array().into_iter().flatten() {
                let (kind, outcome) = (
                    i["kind"].as_str().unwrap_or_default(),
                    i["outcome"].as_str().unwrap_or_default(),
                );
                if kind == "update_implementation" {
                    continue;
                }
                match outcome {
                    "open" => bad.push(format!("{pref} sealed item {} open", i["id"])),
                    "deferred" if i["answer"].is_null() => bad.push(format!(
                        "{pref} sealed item {} deferred with no accepted deferral",
                        i["id"]
                    )),
                    "confirmed" => answered.set(answered.get() + 1),
                    "deferred" => {
                        answered.set(answered.get() + 1);
                        deferred.set(deferred.get() + 1);
                    }
                    _ => {}
                }
            }
        }
        // what a person reads of each landing with open items: the items its version's plan
        // holds open now, what its agent answered last before it proposed, and whether rdy
        // added a key change to the version
        for (l, pref) in &look {
            let q = PlanQuery {
                proposal: Some(pref.clone()),
                max_tokens: Some(100_000),
                ..Default::default()
            };
            let open: Vec<String> = st
                .change_plan(&q, "work")
                .map(|p| serde_json::to_value(p).expect("a plan reply serializes"))
                .map(|p| {
                    p["items"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|i| i["state"] == "open")
                        .map(|i| format!("{} {}", i["kind"], i["id"]))
                        .collect()
                })
                .unwrap_or_else(|e| vec![format!("plan read: {e}")]);
            let made = o.journal.iter().position(|x| {
                x.act == "propose" && x.outcome == Met::Ok && x.block.as_deref() == Some(pref)
            });
            let asked: Vec<String> = made
                .and_then(|k| {
                    let view = o.journal[k].view.as_deref();
                    o.journal[..k]
                        .iter()
                        .rev()
                        .find(|x| x.act == "plan_answer" && x.view.as_deref() == view)
                })
                .and_then(|x| x.reply.as_ref())
                .and_then(|r| r["answers"].as_array().cloned())
                .unwrap_or_default()
                .iter()
                .map(|a| format!("{} {}", a["kind"], a["item"]))
                .collect();
            let rekeyed = l
                .parse::<Hash>()
                .ok()
                .and_then(|h| st.reader().landing(&h).ok())
                .and_then(|rec| st.reader().proposal(&rec.proposal).ok())
                .is_some_and(|p| {
                    p.changes.iter().any(|c| {
                        st.reader()
                            .change(c)
                            .is_ok_and(|c| c.message.starts_with("new keys rdy derived"))
                    })
                });
            bad.push(format!(
                "{pref}: open now {open:?}; answered before its propose {asked:?}; rdy key change {rekeyed}"
            ));
        }
        // each agent view is closed or holds no change
        let mut live = vec![];
        for name in st.view_names() {
            let Some(head) = st.view_head(&name) else {
                continue;
            };
            let by_agent = head
                .owner
                .as_ref()
                .is_some_and(|w| w.actor.starts_with("agent:a"));
            if by_agent && !head.closed && !st.included(&name).is_empty() {
                live.push(name);
            }
        }
        drop(st);
        r.close().expect("close the drained copy");
        assert!(
            live.is_empty(),
            "swarm seed {seed}: views neither landed nor closed after the drain: {live:?}"
        );
        assert!(bad.is_empty(), "swarm seed {seed}: {bad:#?}");
    });
    eprintln!(
        "rg15: {} answered items sealed, {} of them deferred; {} deferral cards accepted",
        answered.get(),
        deferred.get(),
        accepted.get()
    );
    assert!(
        answered.get() > 0,
        "no landing of any seed sealed an item an agent answered: the gate checked nothing"
    );
}

/// rg15 (D-AAQ9Z), directed: no seed's agents defer an item (the swarm meets confirm and tension
/// items only), so this pins the person's deferral path. Agent a0 lands a claim that cites seed
/// claim c0, then edits c0: its claim gets a confirm item, which a0 defers. Before the relayed
/// approve the person reads the queue (`pending`), accepts the version's deferral card
/// (`accept_deferrals`), and the version lands with the item sealed deferred under that answer.
#[test]
fn rg15_the_person_accepts_a_deferral_card_before_the_approve() {
    use rdy_qa::mcp::McpClient;
    use rdy_qa::swarm::agent::KillGate;
    use rdy_qa::swarm::journal::Journal;
    use rdy_qa::swarm::people;
    use rdy_qa::swarm::project;
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    let cfg = Config {
        seed: 31,
        agents: 1,
        steps: 4,
        kill_at: None,
    };
    let p = project::build(&cfg);
    let (j, g) = (Journal::new(), Arc::new(KillGate::new()));
    let mut c = McpClient::start_proxy(&p.served);
    c.call(
        "rdy_session",
        json!({"actor": "agent:a0", "run": "a0", "role": "writer", "areas": ["payments"]}),
    )
    .ok();
    let (c0, section, decision) = (
        p.seed.claims[0].clone(),
        p.seed.sections[0].clone(),
        p.seed.decisions[0].clone(),
    );
    let brief = |c: &mut McpClient, id: &str| -> (String, Value) {
        let b = c.call("rdy_brief", json!({"ids": [id]})).ok();
        (
            b["base"]["token"].as_str().unwrap().to_string(),
            b["base"]["blocks"][0]["version"].clone(),
        )
    };
    // a0 answers each open item of its plan as an agent does, and defers a confirm item when
    // `defer`; then it proposes, and the person lands the version through the relay
    let land = |c: &mut McpClient, title: &str, defer: bool| {
        let plan = c
            .call("rdy_plan", json!({"view": "a0", "max_tokens": 100_000}))
            .ok();
        let mut deferred = 0;
        let answers: Vec<Value> = plan["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|i| i["state"] == "open")
            .filter_map(|i| {
                let kind = i["kind"].as_str()?;
                let (act, reason) = match swarm_answer(kind)? {
                    _ if defer && kind == "confirm" => ("defer", "swarm: later"),
                    answer => answer,
                };
                deferred += usize::from(act == "defer");
                Some(json!({"item": i["id"], "act": act, "reason": reason}))
            })
            .collect();
        if !answers.is_empty() {
            c.call("rdy_plan", json!({"view": "a0", "answers": answers}))
                .ok();
        }
        let v = c
            .call(
                "rdy_propose",
                json!({"view": "a0", "title": title, "summary": title}),
            )
            .ok();
        let pref = v["ref"].as_str().unwrap().to_string();
        let queue = Arc::new(Mutex::new(vec![pref.clone()]));
        people::spawn(
            &p,
            vec![PersonAct::Land { relayed: true }],
            j.clone(),
            queue.clone(),
            g.clone(),
        )
        .join()
        .unwrap_or_else(|e| panic!("the person panicked: {e:?}"));
        (pref, queue, deferred)
    };
    // a0's claim cites c0, and lands
    let (token, _) = brief(&mut c, &section);
    let body = format!("Agent zero builds on {{{{{c0}}}}} in pass one.");
    c.call(
        "rdy_write",
        json!({"ops": [{"op": "create", "type": "claim", "id": "$y", "parent": section,
            "body": body}], "brief": token, "implements": [decision]}),
    )
    .ok();
    let (first, queue, _) = land(&mut c, "a0 cites c0", false);
    assert!(
        queue.lock().unwrap().is_empty(),
        "{first}: {:#?}",
        j.entries()
    );
    // a0 edits c0: its claim gets a confirm item, which a0 defers
    let (token, version) = brief(&mut c, &c0);
    c.call(
        "rdy_write",
        json!({"ops": [{"op": "update", "id": c0, "base": version,
            "body": "The ledger rule first holds, reworded."}],
            "brief": token, "implements": [decision]}),
    )
    .ok();
    let (pref, queue, deferred) = land(&mut c, "a0 rewords c0", true);
    assert!(
        deferred > 0,
        "a0's edit of c0 gave its claim no confirm item to defer"
    );
    let entries = j.entries();
    let of = |act: &str| -> Vec<&Entry> {
        entries
            .iter()
            .filter(|e| e.act == act && e.block.as_deref() == Some(pref.as_str()))
            .collect()
    };
    let (read, accepted, landed) = (of("pending"), of("accept_deferrals"), of("land"));
    assert_eq!(read.len(), 1, "{entries:#?}");
    assert_eq!(read[0].outcome, Met::Ok, "{:?}", read[0]);
    assert_eq!(accepted.len(), 1, "{entries:#?}");
    assert_eq!(accepted[0].outcome, Met::Ok, "{:?}", accepted[0]);
    assert_eq!(accepted[0].actor, "agent:relay");
    let answer = accepted[0].reply.as_ref().expect("the answer's reply");
    assert_eq!(answer["act"], "accept_deferrals", "{answer}");
    assert_eq!(landed.len(), 1, "{entries:#?}");
    assert_eq!(landed[0].outcome, Met::Ok, "{:?}", landed[0]);
    assert!(
        queue.lock().unwrap().is_empty(),
        "the landed ref left the queue"
    );
    // the seal holds the deferred item under the accepting answer
    let l = landed[0].reply.as_ref().unwrap()["landing"]
        .as_str()
        .unwrap()
        .to_string();
    let (ok, seal) = p.served.json(&["--json", "plan", "--landing", &l]);
    assert!(ok, "{seal}");
    let deferred: Vec<&Value> = seal["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["outcome"] == "deferred")
        .collect();
    assert!(!deferred.is_empty(), "{seal}");
    assert!(deferred.iter().all(|i| i["answer"].is_string()), "{seal}");
}
