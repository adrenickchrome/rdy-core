//! sg5: the swarm simulation (spec §15.3; ledger O1-O5, O9). Eight scripted agents and one
//! person act on one daemon, through MCP first. The daemon is killed once and restarted. Then no
//! actor runs (the freeze), the project is copied, and the drain lands every live view.
//!
//! Every assertion reads the final state or the journal, never an order of events. One run per
//! seed is shared by every test of this file: seeds 1 to 6 in the fast gate (each runs every
//! check; together they must exercise every act, ruling task-5.2), seeds 1, 2 and 3 in the deep
//! gate (`RDY_QA_DEEP=1`). Every failure names its seed, so a person can run it again:
//! `RDY_QA_SWARM_SEEDS=<seed>,...` runs only those seeds.
//!
//! rg15 (M3b, D-VF4RH, D-ME2HD): the run goes through the readiness gate. Each agent answers its
//! view's change plan before it proposes, the person accepts a version's deferral card before the
//! approve, every landing's version had no open item that gates or warns, and every stable change
//! that lands records the briefing its writer read.
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
use rdy_qa::swarm::people::landing_fields;
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
const CORE: [&str; 7] = [
    "write",
    "brief",
    "propose",
    "land",
    "review_submit",
    "inbox",
    // the forced advice sequence reaches it in every seed (`script::advice_steps`)
    "accept_advice",
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
            .and_then(|r| landing_fields(r)["landing"].as_str())
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
        match landing_fields(reply).get("chosen").filter(|c| !c.is_null()) {
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
            e.reply.as_ref().is_some_and(|r| {
                landing_fields(r)
                    .get("chosen")
                    .is_some_and(|c| !c.is_null())
            })
        }),
        kinds,
        opened: run.iter().any(|e| {
            e.inbox
                .as_ref()
                .is_some_and(|r| r.entries.iter().any(|x| x["event"] == "tension_opened"))
        }),
    }
}

/// The M3 route acts each fast tier acknowledges before the freeze (plan Task 10). The role
/// route's acts are visible gaps, proven by the authority area (ruling task-8 finding 1).
const M3_ACKNOWLEDGED: [&str; 10] = [
    "m3_relay_answer",
    "m3_elicit_first_rev",
    "m3_elicit_second_rev",
    "m3_elicit_decline",
    "m3_relay_after_decline",
    "m3_moved_card",
    "m3_publication_check",
    "m3_publication_output",
    "m3_blob_remove",
    "m3_compact_real",
];

/// Ruling task-5.2: the run, not the drain, exercises the acts. Every seed reaches the core acts
/// and lands at least once before the freeze; the union of the seeds reaches every act of the
/// closed list, a lost call, a seed with 4 landings, a landing that chose a member of a set, an
/// `edit_collision` and a `premise_shift`, and a `tension_opened` inbox entry. Ruling
/// task-5-round1.2: the union also reaches a set's winner landing again with no choice. Final
/// review M4: the union also lands by a direct `rdy approve`. Ruling task-10.4: the union also
/// accepts a check verdict (`accept_check_verdict`). Plan Task 10: the union also acknowledges
/// every M3 route act of `M3_ACKNOWLEDGED`. Only acknowledged `land` calls count
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
            "swarm seed {seed}: {} acts, lost {}, {} landings, chose {}, kinds {:?}, opened {}, accept_advice {}, accept_check_verdict {}, winner lands again {}, bound include then choice {}, direct landing {}",
            c.acts.len(),
            c.lost,
            c.landings,
            c.chose,
            c.kinds,
            c.opened,
            c.acts.contains("accept_advice"),
            c.acts.contains("accept_check_verdict"),
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
            "accept_check_verdict",
            "ui_answer",
        ])
        // plan Task 10: the acknowledged M3 route acts (the role route is the authority area's)
        .chain(M3_ACKNOWLEDGED)
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
            if let Some(l) = e
                .reply
                .as_ref()
                .and_then(|r| landing_fields(r)["landing"].as_str())
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
            let l = landing_fields(x.reply.as_ref()?)["landing"].as_str()?;
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
                // (judged Loser when another of its sets already chose against it: the gate reads open sets first)
                ("E_ALT_MEMBER", "propose", Some(AtRefusal::OpenSet | AtRefusal::Loser)) => {
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
                    .and_then(|r| landing_fields(r)
                        .get("chosen")
                        .or_else(|| r.get("alternatives")))
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
    a0[3] = Act::Include {
        from: 2,
        forced: false,
    };
    a0[4] = Act::AltCreate { with: 1 };
    a0[5] = Act::Exclude { forced: false };
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
        source: false,
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
            .and_then(|r| landing_fields(r).get("chosen"))
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
        source: false,
    };
    a2[2] = write(2);
    let mut a0 = vec![read(0); 10];
    a0[0] = brief(0);
    a0[3] = write(0);
    a0[4] = Act::Include {
        from: 2,
        forced: false,
    };
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
            .and_then(|r| landing_fields(r).get("chosen"))
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
    /// Landings an acknowledged M3 route attempt names (plan Task 10).
    m3_landings: usize,
    /// Landings with the card the person saw journaled, for the M3 audit's card rule.
    m3_cards: usize,
}

/// What the oracles of `runs` lacked: an assertion over an empty set passes whatever the product
/// does. Each seed's run has reference tensions, logged moves and seen labels to check; the union
/// of the seeds has journal collisions, inbox tension entries, both briefing refusals, an M3 route
/// landing and a landing with a journaled card (plan Task 10).
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
        ("M3 route landing", sum(|w| w.m3_landings)),
        ("landing with a journaled card", sum(|w| w.m3_cards)),
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
        m3_landings: 1,
        m3_cards: 1,
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
    assert_eq!(vacuous(&[(2, empty)]).len(), 9);
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
            m3_landings: o
                .before_freeze()
                .iter()
                .map(|e| route_records(e).len())
                .sum(),
            m3_cards: seen_cards(&o.journal).len(),
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

/// mh7 (D-4YRQ4): writer 0's one `compact` runs through the real MCP link on every seed, and the
/// journal keeps the whole report: six fields, rows that add up to the log bytes. The act adds
/// no frame: the log the frozen copy holds is at least as long as the report counted, and its
/// `rdy fsck` (the final oracle) reports a byte total and removed bytes of its own. Only the
/// seeded kill may cut the call; no refusal is allowed.
#[test]
fn mh7_swarm_runs_compact_through_mcp() {
    each(|seed, o| {
        let acts: Vec<&Entry> = o.journal.iter().filter(|e| e.act == "compact").collect();
        assert_eq!(acts.len(), 1, "swarm seed {seed}: {acts:#?}");
        let e = acts[0];
        assert_eq!(e.actor, "agent:a0", "swarm seed {seed}");
        assert!(
            e.change.is_none() && e.view.is_none(),
            "swarm seed {seed}: {e:?}"
        );
        match &e.outcome {
            Met::Ok => {}
            Met::Lost => return,
            other => panic!("swarm seed {seed}: compact met {other:?}"),
        }
        let r = e.reply.as_ref().expect("the journal keeps the report");
        let mut keys: Vec<&str> = r.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "by_kind",
                "log_bytes",
                "notes",
                "on_disk_bytes",
                "punched_bytes",
                "removed_bytes"
            ],
            "swarm seed {seed}: {r}"
        );
        let rows = r["by_kind"].as_array().unwrap();
        assert!(!rows.is_empty(), "swarm seed {seed}");
        assert_eq!(
            rows.iter()
                .map(|x| x["bytes"].as_u64().unwrap())
                .sum::<u64>(),
            r["log_bytes"].as_u64().unwrap(),
            "swarm seed {seed}"
        );
        // the frozen copy holds the log the report counted, and more
        let (ok, fsck) = oracle::fsck(&o.frozen, &o.served.env_pairs());
        assert!(ok, "swarm seed {seed}: rdy fsck failed: {fsck}");
        assert!(
            fsck["log_bytes"].as_u64().unwrap() >= r["log_bytes"].as_u64().unwrap(),
            "swarm seed {seed}: the act shortened the log: {fsck} against {r}"
        );
        assert!(fsck["removed_bytes"].is_u64(), "swarm seed {seed}: {fsck}");
        assert!(fsck["by_kind"].is_array(), "swarm seed {seed}: {fsck}");
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
/// needs a new order key for a claim its view changed after the proposal. Both lanes refuse it
/// (ruling task-8, finding 2): the chat lane's `rdy_answer` with `E_PROPOSAL_NOT_READY`, whose
/// issue names the stale order key, and the CLI parity lane's direct approve with the order-key
/// E_STALE_BASE. a1 proposes again and that version lands through the chat path.
#[test]
fn sg5_a_landing_whose_rekeyed_claim_changed_after_the_proposal_is_refused_then_lands() {
    const SEED: u64 = 23;
    let cfg = Config {
        seed: SEED,
        agents: 2,
        steps: 10,
        kill_at: None,
    };
    let read = |n| Act::Read {
        target: Pick::Seed(n),
    };
    let create = || Act::Create {
        area: "payments",
        contra: None,
        source: false,
    };
    let mut a0 = vec![read(0); 10];
    a0[0] = create();
    a0[1] = Act::Propose;
    let mut a1 = vec![read(1); 10];
    a1[0] = create();
    a1[2] = Act::Propose;
    a1[3] = Act::Write {
        target: Pick::Own(0),
        stable: true,
    };
    a1[9] = Act::Propose;
    // the person's act j runs after step 2(j + 1): a0 lands after step 4; a1's first version
    // meets the chat lane after step 6 and the CLI parity lane after step 8; a1 proposes again
    // at step 9, and that version lands through the chat path after step 10
    let person = vec![
        PersonAct::Tensions,
        PersonAct::Land { relayed: true },
        PersonAct::Land { relayed: true },
        PersonAct::Land { relayed: false },
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
    let a1_lands = |actor: &str| -> Vec<&Entry> {
        run.iter()
            .filter(|e| e.act == "land" && e.view.as_deref() == Some("a1") && e.actor == actor)
            .collect()
    };
    // the CLI parity lane: the order-key E_STALE_BASE
    let cli = a1_lands("human:qa");
    let stale = cli
        .iter()
        .find(|e| e.outcome == Met::Refused("E_STALE_BASE".into()))
        .unwrap_or_else(|| {
            panic!(
                "swarm seed {SEED}: a1's first version met no stale order key on the CLI lane:\n{}",
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
    // the chat lane, on the same version: not ready, and its issue is the stale order key
    let chat = a1_lands("agent:relay");
    let not_ready = chat
        .iter()
        .find(|e| e.outcome == Met::Refused("E_PROPOSAL_NOT_READY".into()))
        .unwrap_or_else(|| {
            panic!(
                "swarm seed {SEED}: a1's first version met no chat refusal:\n{}",
                lines(run)
            )
        });
    assert_eq!(not_ready.block, stale.block, "one version, both lanes");
    // the journal keeps the whole tool error: its issues are the version's readiness issues
    let issues = not_ready
        .reply
        .as_ref()
        .and_then(|r| r["issues"].as_array())
        .unwrap_or_else(|| panic!("the refusal's issues are journaled: {not_ready:#?}"));
    assert!(
        issues.iter().any(|i| i["message"]
            .as_str()
            .is_some_and(|m| m.contains("needs a new order key"))),
        "the chat refusal's issue is not the stale order key: {not_ready:#?}"
    );
    // the later version lands through the chat path
    assert!(
        chat.iter()
            .any(|e| e.outcome == Met::Ok && e.block != stale.block),
        "swarm seed {SEED}: a1's later version did not land through the chat path:\n{}",
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
/// readiness gate, and the gate never meets an open item of the person's `work` plan (ruling
/// task-15 F1: the person answers it before it proposes). After the drain each agent view is
/// closed or holds no change. Before each
/// approve of the run and the drain, the relay reads the version's change plan (a
/// `version_plan` entry naming it), and that read shows no open item that gates or warns. From
/// the drained store, each landing's seal holds no open item but a code item, and each item it
/// seals as deferred names the answer that accepted the deferral. Some seed lands an item an
/// agent answered. Some seed reads a work version's plan before an approve: the run puts the
/// version of the person's forced working-copy edit (`script::EARLY_EDIT`) ahead of the queue.
#[test]
fn rg15_every_live_view_drains_through_the_gate() {
    let answered = std::cell::Cell::new(0usize);
    let work_reads = std::cell::Cell::new(0usize);
    let (deferred, accepted) = (std::cell::Cell::new(0usize), std::cell::Cell::new(0usize));
    each(|seed, o| {
        let mut bad = vec![];
        // the plan read before each landing of the run and the drain
        let mut landed: BTreeMap<String, String> = BTreeMap::new();
        // the landings with open items, for a person to look at
        let mut look: Vec<(String, String)> = vec![];
        for (k, e) in o.journal.iter().enumerate() {
            let landing = match (e.act, &e.outcome, e.reply.as_ref()) {
                ("land", Met::Ok, Some(r)) => landing_fields(r)["landing"].as_str(),
                (run::LAND_FOUND, _, Some(r)) => landing_fields(r)["landing"].as_str(),
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
        if !live.is_empty() {
            bad.push(format!(
                "views neither landed nor closed after the drain: {live:?}"
            ));
        }
        // the person's `work` (ruling task-15 F1): the gate never meets an open item of its
        // plan. Each read of a work version's plan before an approve shows no open item that
        // gates or warns, and no approve of one is refused for an open item (E_PLAN_OPEN).
        // Whether every work version lands by the end of the run is not checked here (see the
        // fix-round report: the queue and moved refusals, not the plan, keep it out).
        let work: BTreeSet<&str> = o
            .journal
            .iter()
            .filter(|e| e.act == "wc_propose" && e.outcome == Met::Ok)
            .filter_map(|e| e.block.as_deref())
            .collect();
        let mut stopped = false;
        for e in &o.journal {
            let Some(pref) = e.block.as_deref().filter(|b| work.contains(b)) else {
                continue;
            };
            match (e.act, &e.outcome, e.reply.as_ref()) {
                ("version_plan", Met::Ok, Some(r)) => {
                    work_reads.set(work_reads.get() + 1);
                    let open = gating_open(&r["counts"]);
                    if !open.is_empty() {
                        stopped = true;
                        bad.push(format!(
                            "{} step {}: the plan of work version {pref} holds open items {open:?}",
                            e.actor, e.step
                        ));
                    }
                }
                ("land", Met::Refused(code), _)
                    if code == "E_PROPOSAL_NOT_READY"
                        && e.message
                            .as_deref()
                            .is_some_and(|m| m.contains("E_PLAN_OPEN")) =>
                {
                    stopped = true;
                    bad.push(format!(
                        "{} step {}: work version {pref} is not ready for open plan items: {}",
                        e.actor,
                        e.step,
                        e.message.as_deref().unwrap_or("")
                    ));
                }
                _ => {}
            }
        }
        if stopped {
            bad.push(format!("the person's queue at the end: {:?}", o.queued));
        }
        assert!(bad.is_empty(), "swarm seed {seed}: {bad:#?}");
    });
    eprintln!(
        "rg15: {} answered items sealed, {} of them deferred; {} deferral cards accepted; {} plan reads of a work version before an approve",
        answered.get(),
        deferred.get(),
        accepted.get(),
        work_reads.get()
    );
    assert!(
        work_reads.get() > 0,
        "no seed approved a version of the person's work: its check is vacuous"
    );
    assert!(
        answered.get() > 0,
        "no landing of any seed sealed an item an agent answered: the gate checked nothing"
    );
}

/// rg15 (D-AAQ9Z), directed: no seed's agents defer an item (the swarm meets confirm and tension
/// items only), so this pins the person's deferral path. Agent a0 lands a claim that cites seed
/// claim c0, then edits c0: its claim gets a confirm item, which a0 defers. Before the relayed
/// approve the relay reads the queue (`pending`, through `rdy_pending`), relays the person's
/// acceptance of the version's deferral card (`accept_deferrals`, through `rdy_answer`), and the
/// version lands with the item sealed deferred under that answer.
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
    // the relay reads the queue and relays the answer through the tools (FW-UX1, D-VF4RH)
    assert_eq!(
        read[0].reply,
        Some(json!({"via": "rdy_pending"})),
        "{:?}",
        read[0]
    );
    assert_eq!(accepted.len(), 1, "{entries:#?}");
    assert_eq!(accepted[0].outcome, Met::Ok, "{:?}", accepted[0]);
    assert_eq!(accepted[0].actor, "agent:relay");
    let answer = accepted[0].reply.as_ref().expect("the answer's reply");
    assert_eq!(answer["via"], "rdy_answer", "{answer}");
    assert_eq!(answer["answer"]["act"], "accept_deferrals", "{answer}");
    assert_eq!(landed.len(), 1, "{entries:#?}");
    assert_eq!(landed[0].outcome, Met::Ok, "{:?}", landed[0]);
    assert!(
        queue.lock().unwrap().is_empty(),
        "the landed ref left the queue"
    );
    // the seal holds the deferred item under the accepting answer
    let l = landing_fields(landed[0].reply.as_ref().unwrap())["landing"]
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

/// rg15 (ruling task-15 F1, D-ME2HD), directed: the person answers its own view's change plan
/// through the CLI before it proposes `work`. Agent a0 lands a claim that cites seed claim c0;
/// the person's working-copy edit at step 0 rewords c0, so `work` gets a confirm item on a0's
/// claim. The person reads the plan (`rdy plan --view work`), confirms the item
/// (`rdy plan confirm`), proposes, and its next act lands the version with the item sealed
/// confirmed under that answer; nothing is left in its queue.
#[test]
fn rg15_the_person_answers_the_work_plan_before_it_proposes() {
    use rdy_qa::mcp::McpClient;
    use rdy_qa::swarm::agent::KillGate;
    use rdy_qa::swarm::journal::Journal;
    use rdy_qa::swarm::people;
    use rdy_qa::swarm::project;
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    let cfg = Config {
        seed: 32,
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
    // a0's claim cites c0, and the person lands it
    let b = c.call("rdy_brief", json!({"ids": [section]})).ok();
    let token = b["base"]["token"].as_str().unwrap().to_string();
    let body = format!("Agent zero builds on {{{{{c0}}}}} in pass one.");
    c.call(
        "rdy_write",
        json!({"ops": [{"op": "create", "type": "claim", "id": "$y", "parent": section,
            "body": body}], "brief": token, "implements": [decision]}),
    )
    .ok();
    // nb8 (TASK-978 X2, D-YXBXH): the new claim owes its stable section's other claims a
    // neighbor confirm, which readiness needs answered; a0 answers them with one group, as the
    // swarm agent does (nb10)
    c.call(
        "rdy_plan",
        json!({"view": "a0", "answers": [{"group": {"edge": "neighbor", "follow_up": "confirm"},
            "act": "confirm", "reason": "swarm: checked"}]}),
    )
    .ok();
    let v = c
        .call(
            "rdy_propose",
            json!({"view": "a0", "title": "a0 cites c0", "summary": "a0 cites c0"}),
        )
        .ok();
    let first = v["ref"].as_str().unwrap().to_string();
    let queue = Arc::new(Mutex::new(vec![first.clone()]));
    let person = |acts: Vec<PersonAct>| {
        people::spawn(&p, acts, j.clone(), queue.clone(), g.clone())
            .join()
            .unwrap_or_else(|e| panic!("the person panicked: {e:?}"));
    };
    person(vec![PersonAct::Land { relayed: true }]);
    assert!(
        queue.lock().unwrap().is_empty(),
        "{first}: {:#?}",
        j.entries()
    );
    // the person rewords c0 in `work` (its edit at step 0), answers the plan, proposes and lands
    person(vec![PersonAct::WcEdit, PersonAct::Land { relayed: true }]);
    let entries = j.entries();
    let work = |act: &str| -> Vec<&Entry> {
        entries
            .iter()
            .filter(|e| e.act == act && e.view.as_deref() == Some("work"))
            .collect()
    };
    let (read, answered, proposed) = (work("plan"), work("plan_answer"), work("wc_propose"));
    assert!(!read.is_empty(), "{entries:#?}");
    assert!(
        read.iter()
            .all(|e| e.actor == "human:qa" && e.outcome == Met::Ok),
        "{read:#?}"
    );
    assert_eq!(answered.len(), 1, "{entries:#?}");
    assert_eq!(answered[0].actor, "human:qa");
    assert_eq!(answered[0].outcome, Met::Ok, "{:?}", answered[0]);
    let sent = answered[0].reply.as_ref().unwrap()["answers"]
        .as_array()
        .unwrap()
        .clone();
    assert!(
        sent.iter().any(|a| a["kind"] == "confirm"
            && a["act"] == "confirm"
            && a["reason"] == "swarm: checked"),
        "{sent:#?}"
    );
    assert_eq!(proposed.len(), 1, "{entries:#?}");
    assert_eq!(proposed[0].outcome, Met::Ok, "{:?}", proposed[0]);
    // the answers come before the propose
    let at = |e: &Entry| entries.iter().position(|x| x == e).unwrap();
    assert!(at(answered[0]) < at(proposed[0]), "{entries:#?}");
    let pref = proposed[0].block.clone().expect("the work version");
    let landed: Vec<&Entry> = entries
        .iter()
        .filter(|e| e.act == "land" && e.block.as_deref() == Some(pref.as_str()))
        .collect();
    assert_eq!(landed.len(), 1, "{entries:#?}");
    assert_eq!(landed[0].outcome, Met::Ok, "{:?}", landed[0]);
    assert!(queue.lock().unwrap().is_empty(), "{:?}", queue.lock());
    // the seal holds each answered item confirmed
    let l = landing_fields(landed[0].reply.as_ref().unwrap())["landing"]
        .as_str()
        .unwrap()
        .to_string();
    let (ok, seal) = p.served.json(&["--json", "plan", "--landing", &l]);
    assert!(ok, "{seal}");
    for a in &sent {
        let item = seal["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == a["item"])
            .unwrap_or_else(|| panic!("{} is not in the seal: {seal}", a["item"]));
        assert_eq!(item["outcome"], "confirmed", "{seal}");
    }
}

/// rg12 (D-ME2HD, D-SDSV2; C7): from the store after each seed's run, every landing whose
/// proposal is a proposed version names an assessment of the version it approved; a
/// re-affirmation card's landing (its proposal is in a `reaffirm-` view) names none.
#[test]
fn rg12_every_landing_of_a_proposed_version_names_an_assessment() {
    each(|seed, o| {
        let (_keep, copy) = copy_of(&o.dir);
        let r = Rdy::open(&copy).expect("open the drained copy");
        let st = r.read();
        let reader = st.reader();
        let mut bad = vec![];
        let (mut named, mut reaffirms) = (0, 0);
        for (landing, rec) in reader.landings().expect("read canon's chain") {
            let view = reader
                .proposal(&rec.proposal)
                .unwrap_or_else(|e| panic!("landing {landing}: its proposal: {e}"))
                .view;
            if view.starts_with(rdy_store::REAFFIRM_VIEW_PREFIX) {
                reaffirms += 1;
                if let Some(a) = rec.assessment {
                    bad.push(format!(
                        "re-affirmation landing {} names assessment {}",
                        landing.short(),
                        a.short()
                    ));
                }
                continue;
            }
            let approved = rec.approved_version.unwrap_or(rec.proposal);
            match rec.assessment {
                None => bad.push(format!(
                    "landing {} of view {view} names no assessment",
                    landing.short()
                )),
                Some(a) => match reader.assessment(&a) {
                    Ok(x) if x.proposal == approved => named += 1,
                    Ok(x) => bad.push(format!(
                        "landing {} names assessment {} of {}, not of {}",
                        landing.short(),
                        a.short(),
                        x.proposal.short(),
                        approved.short()
                    )),
                    Err(e) => bad.push(format!(
                        "landing {}: assessment {}: {e}",
                        landing.short(),
                        a.short()
                    )),
                },
            }
        }
        drop(st);
        r.close().expect("close the drained copy");
        assert!(
            named > 0,
            "swarm seed {seed}: no landing named an assessment"
        );
        assert!(
            bad.is_empty(),
            "swarm seed {seed} ({reaffirms} re-affirmation landings): {bad:#?}"
        );
    });
}

/// ap13 (D-ZWP7Q, D-TF78H, D-DDSBJ): the authority policy of a swarm project, as its store holds
/// it now: the policy block and its versions, each with the landing that put it into canon.
fn policy_versions(st: &ReadState) -> (BlockId, BTreeMap<String, Hash>) {
    let canon = st.materialized(&Scope::Canon).expect("canon");
    let live = rdy_engine::policy::live_policy(st, &canon).expect("the swarm project's policy");
    let history = st
        .history(&Scope::Canon, &live.block, 100)
        .expect("the policy's history");
    let by_landing = history
        .entries
        .iter()
        .filter_map(|e| {
            let landing = e.landing.as_ref()?.id.clone();
            Some((landing, e.version.parse().ok()?))
        })
        .collect();
    (live.block, by_landing)
}

/// What a landing of a seed's run is: by the authority policy, or by a person (local or relayed).
fn landing_counts(o: &Outcome) -> (usize, usize) {
    let (_keep, copy) = copy_of(&o.dir);
    let r = Rdy::open(&copy).expect("open the drained copy");
    let st = r.read();
    let (mut policy, mut person) = (0, 0);
    for (landing, rec) in st.reader().landings().expect("read canon's chain") {
        if o.seed_landings.contains(&landing.to_string()) {
            continue;
        }
        match rec.assurance {
            rdy_model::Assurance::Policy => policy += 1,
            rdy_model::Assurance::Local | rdy_model::Assurance::Relayed => person += 1,
        }
    }
    drop(st);
    r.close().expect("close the drained copy");
    (policy, person)
}

/// ap13: one agent, one scratch write and one stable write, and a propose that the seeded draw
/// makes scratch-only (`rdy_propose {changes}`): the version holds the scratch change alone, the
/// propose reply's verdict accepts it under rule `swarm-drafts`, and the agent lands it with
/// `rdy_land` (`assurance: policy`).
#[test]
fn ap13_a_scratch_only_version_is_accepted_under_swarm_drafts() {
    use rdy_qa::swarm::{agent, journal::Journal, project};
    let seed = (1..1_000)
        .find(|s| agent::scratch_only_draw(*s, 0, 0))
        .expect("a seed whose first propose of a0 is scratch-only");
    let cfg = Config {
        seed,
        agents: 2,
        steps: 12,
        kill_at: None,
    };
    let p = project::build(&cfg);
    let j = Journal::new();
    let acts = vec![
        Act::Write {
            target: Pick::Seed(0),
            stable: false,
        },
        Act::Write {
            target: Pick::Seed(0),
            stable: true,
        },
        Act::Propose,
    ];
    agent::spawn(
        &p,
        &cfg,
        0,
        acts,
        j.clone(),
        std::sync::Arc::new(agent::KillGate::new()),
    )
    .join()
    .unwrap_or_else(|e| panic!("agent a0 panicked: {e:?}"));
    let entries = j.entries();
    let all = || format!("{entries:#?}");
    let change_of = |stable: bool| {
        entries
            .iter()
            .find(|e| e.act == "write" && e.stable == stable && e.outcome == Met::Ok)
            .and_then(|e| e.change.clone())
            .unwrap_or_else(|| panic!("a write with stable {stable}: {}", all()))
    };
    let (scratch, stable) = (change_of(false), change_of(true));
    let propose = entries
        .iter()
        .find(|e| e.act == "propose" && e.outcome == Met::Ok)
        .unwrap_or_else(|| panic!("an acknowledged propose: {}", all()));
    let reply = propose.reply.clone().expect("a propose reply");
    assert_eq!(reply["changes"], serde_json::json!([scratch]), "{reply:#}");
    assert!(!reply["changes"].to_string().contains(&stable), "{reply:#}");
    assert_eq!(reply["policy"]["verdict"], "accepts", "{reply:#}");
    assert_eq!(reply["policy"]["rule"], project::SWARM_RULE, "{reply:#}");
    let landed = entries
        .iter()
        .find(|e| e.act == "land" && e.actor == "agent:a0")
        .unwrap_or_else(|| panic!("a0 lands its version: {}", all()));
    assert_eq!(landed.outcome, Met::Ok, "{}", all());
    assert_eq!(landed.block, propose.block);
    let land = landed.reply.as_ref().expect("the rdy_land reply");
    assert_eq!(land["assurance"], "policy", "{land:#}");
}

/// ap13 (D-ZWP7Q "The agent's landing act"): from the drained store, every policy landing of a
/// seed's run names the policy version that was live when it landed (the last one a landing
/// before it put into canon) and an assessment of its version whose verdict accepts under that
/// policy; it records no relay; and its changes write no decision and no block in stable content.
/// The seeds together land at least one (ruling task-10.1).
#[test]
fn ap13_every_policy_landing_is_in_policy() {
    let total = std::cell::Cell::new(0usize);
    let counts = std::cell::RefCell::new(vec![]);
    each(|seed, o| {
        let (_keep, copy) = copy_of(&o.dir);
        let r = Rdy::open(&copy).expect("open the drained copy");
        let st = r.read();
        let reader = st.reader();
        let canon = st.materialized(&Scope::Canon).expect("canon");
        let (block, versions) = policy_versions(&st);
        let mut chain = reader.landings().expect("read canon's chain");
        chain.reverse();
        let mut live: Option<Hash> = None;
        let (mut count, mut bad) = (0, vec![]);
        for (landing, rec) in &chain {
            let at = landing.short();
            if rec.assurance == rdy_model::Assurance::Policy {
                count += 1;
                let want = live.map(|version| rdy_model::assessment::PolicyRef {
                    block: block.clone(),
                    version,
                });
                if rec.policy != want {
                    bad.push(format!(
                        "landing {at} names policy {:?}, not the live {want:?}",
                        rec.policy
                    ));
                }
                if rec.relayed_by.is_some() || rec.relay_evidence.is_some() {
                    bad.push(format!("landing {at} records a relay"));
                }
                let approved = rec.approved_version.unwrap_or(rec.proposal);
                match rec.assessment.map(|a| reader.assessment(&a)) {
                    Some(Ok(a)) => {
                        if a.proposal != approved {
                            bad.push(format!(
                                "landing {at}: its assessment is of another version"
                            ));
                        }
                        let accepts = a.policy.as_ref().is_some_and(|p| {
                            p.policy == rec.policy
                                && matches!(
                                    p.verdict,
                                    rdy_model::assessment::PolicyVerdict::Accepts { .. }
                                )
                        });
                        if !accepts {
                            bad.push(format!(
                                "landing {at}: its assessment's verdict is {:?}",
                                a.policy
                            ));
                        }
                    }
                    Some(Err(e)) => bad.push(format!("landing {at}: its assessment: {e}")),
                    None => bad.push(format!("landing {at} names no assessment")),
                }
                for c in &rec.changes {
                    let change = reader.change(c).expect("a landed change");
                    for op in &change.ops {
                        let decision = match op {
                            Op::Create { block } | Op::Update { block, .. } => {
                                block.block_type == "decision"
                            }
                            Op::Delete { id, .. } => {
                                canon.get(id).is_some_and(|b| b.block_type == "decision")
                            }
                        };
                        if decision {
                            bad.push(format!(
                                "landing {at}: change {} writes decision {}",
                                c.short(),
                                op.block_id()
                            ));
                        }
                    }
                    let stable = stable_subjects(&st, &canon, &change);
                    if !stable.is_empty() {
                        bad.push(format!(
                            "landing {at}: change {} writes stable content {stable:?}",
                            c.short()
                        ));
                    }
                }
            }
            if let Some(v) = versions.get(&landing.to_string()) {
                live = Some(*v);
            }
        }
        drop(st);
        r.close().expect("close the drained copy");
        eprintln!("ap13 swarm seed {seed}: {count} policy landings");
        total.set(total.get() + count);
        counts.borrow_mut().push(format!("seed {seed}: {count}"));
        assert!(bad.is_empty(), "swarm seed {seed}: {bad:#?}");
    });
    assert!(
        total.get() >= 1,
        "no seed landed under the policy: {:?}",
        counts.borrow()
    );
}

/// ap13 (D-ZWP7Q): the seeds together land under the policy (an agent's `rdy_land`), and each
/// seed lands by a person's approval (local or relayed) (ruling task-10.1). The counts a person reads with --nocapture: the landings of each
/// kind, the disputes opened and resolved, and each `E_POLICY_HUMAN` and `E_PERMISSION` the
/// journal saw.
#[test]
fn ap13_seeds_land_under_the_policy_and_by_a_person() {
    let total = std::cell::Cell::new(0usize);
    let counts = std::cell::RefCell::new(vec![]);
    each(|seed, o| {
        let (policy, person) = landing_counts(o);
        let ok = |act: &str| {
            o.journal
                .iter()
                .filter(|e| e.act == act && e.outcome == Met::Ok)
                .count()
        };
        let met = |code: &str| {
            o.journal
                .iter()
                .filter(|e| e.outcome == Met::Refused(code.to_string()))
                .map(|e| format!("{} step {} {}", e.actor, e.step, e.act))
                .collect::<Vec<_>>()
        };
        let scratch_only = o
            .before_freeze()
            .iter()
            .filter(|e| e.act == "propose" && e.outcome == Met::Ok)
            .filter(|e| e.reply.as_ref().is_some_and(|r| r["changes"].is_array()))
            .count();
        let in_the_run = o
            .before_freeze()
            .iter()
            .filter(|e| e.act == "land" && e.outcome == Met::Ok && e.actor.starts_with("agent:a"))
            .count();
        eprintln!(
            "ap13 swarm seed {seed}: {policy} policy landings ({in_the_run} before the freeze), {person} approvals, {scratch_only} scratch-only proposes, {} disputes opened, {} resolved, E_POLICY_HUMAN {:?}, E_PERMISSION {:?}",
            ok("dispute_open"),
            ok("dispute_resolve"),
            met("E_POLICY_HUMAN"),
            met("E_PERMISSION")
        );
        total.set(total.get() + policy);
        counts.borrow_mut().push(format!("seed {seed}: {policy}"));
        assert!(person >= 1, "swarm seed {seed}: no person's approval");
    });
    assert!(
        total.get() >= 1,
        "no seed landed under the policy: {:?}",
        counts.borrow()
    );
}

/// ap13 (D-ZWP7Q "Disputes": a dispute shows and never blocks): the person opens disputes in the
/// run, a landing follows an open dispute, and every live agent view still drains. The seeds
/// together open at least one dispute.
#[test]
fn ap13_disputes_never_block_the_drain() {
    let opened = std::cell::Cell::new(0usize);
    each(|seed, o| {
        let (_keep, copy) = copy_of(&o.dir);
        let r = Rdy::open(&copy).expect("open the drained copy");
        let st = r.read();
        let (disputes, _) = st
            .reader()
            .all_disputes(st.seq(), usize::MAX)
            .expect("read the disputes");
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
        opened.set(opened.get() + disputes.len());
        assert!(
            live.is_empty(),
            "swarm seed {seed}: views neither landed nor closed after the drain: {live:?}"
        );
        // an open the kill cut may or may not have reached the log
        let opens = |met: Met| {
            o.journal
                .iter()
                .filter(|e| e.act == "dispute_open" && e.outcome == met)
                .count()
        };
        let (acknowledged, cut) = (opens(Met::Ok), opens(Met::Lost));
        assert!(
            (acknowledged..=acknowledged + cut).contains(&disputes.len()),
            "swarm seed {seed}: {acknowledged} acknowledged opens, {cut} cut by the kill, {} disputes in the store",
            disputes.len()
        );
        // a landing after the first open dispute: the dispute blocked nothing
        if let Some(first) = o
            .journal
            .iter()
            .position(|e| e.act == "dispute_open" && e.outcome == Met::Ok)
        {
            let after = o.journal[first..]
                .iter()
                .any(|e| e.act == "land" && e.outcome == Met::Ok);
            assert!(
                after,
                "swarm seed {seed}: no landing after the first dispute opened"
            );
        }
    });
    assert!(opened.get() > 0, "no seed opened a dispute");
}

/// ap13 (guard): `rdy fsck` passes on each drained project, and every landing the journal
/// acknowledged (an `Ok` `land` entry, or a `land_found` entry that names one) is in canon's
/// chain, the policy landings of agents among them.
#[test]
fn ap13_every_acknowledged_landing_passes_fsck() {
    each(|seed, o| {
        let (ok, fsck) = oracle::fsck(&o.dir, &o.served.env_pairs());
        assert!(ok, "swarm seed {seed}: rdy fsck failed: {fsck}");
        let (_keep, copy) = copy_of(&o.dir);
        let r = Rdy::open(&copy).expect("open the drained copy");
        let chain: BTreeSet<String> = r
            .read()
            .reader()
            .landings()
            .expect("read canon's chain")
            .into_iter()
            .map(|(h, _)| h.to_string())
            .collect();
        r.close().expect("close the drained copy");
        let missing: Vec<String> = o
            .landings
            .iter()
            .filter(|l| !chain.contains(*l))
            .cloned()
            .collect();
        assert!(!o.landings.is_empty(), "swarm seed {seed}: no landing");
        assert!(
            missing.is_empty(),
            "swarm seed {seed}: acknowledged landings not in canon: {missing:?}"
        );
    });
}

// ---- ui11: real HTTP answers in the swarm (D-6M0AF, D-WPE4R; system/quality-gate tb-A4DAV) ----

/// The `ui_answer` entries before the freeze.
fn ui_entries(o: &Outcome) -> Vec<&Entry> {
    o.before_freeze()
        .iter()
        .filter(|e| e.act == "ui_answer")
        .collect()
}

/// ui11: the union of the forced sequences comes in every seed: at least the forced landing, the
/// moved card's refusal and its fresh landing, and the imported decision's landing.
fn ui_counts(o: &Outcome) -> (usize, usize) {
    let (mut landed, mut moved) = (0, 0);
    for e in ui_entries(o) {
        match &e.outcome {
            Met::Ok if e.ui.as_ref().is_some_and(|u| u.landing.is_some()) => landed += 1,
            Met::Refused(code) if code == "E_PROPOSAL_MOVED" => moved += 1,
            _ => {}
        }
    }
    (landed, moved)
}

/// ui11: each seed's frozen store holds what each UI answer's journal entry says the page showed:
/// its item, hash, labels and choice, as the person's local card, with no relay.
#[test]
fn ui11_swarm_ui_records_match_the_shown_card() {
    each(|seed, o| {
        let (landed, _) = ui_counts(o);
        assert!(landed >= 3, "swarm seed {seed}: {landed} UI landings");
        for e in ui_entries(o) {
            let Some(ui) = &e.ui else {
                continue;
            };
            assert_eq!(ui.source, "ui", "swarm seed {seed}");
            assert!(!ui.card_hash.is_empty(), "swarm seed {seed}: {ui:?}");
            assert_eq!(ui.option, "approve", "swarm seed {seed}: {ui:?}");
            assert!(ui.labels.contains(&ui.label), "swarm seed {seed}: {ui:?}");
        }
        let bad = oracle::ui_violations(o.before_freeze(), &o.frozen, false);
        assert!(bad.is_empty(), "swarm seed {seed}: {bad:#?}");
        let bad = oracle::ui_violations(&o.journal, &o.dir, false);
        assert!(
            bad.is_empty(),
            "swarm seed {seed} after the drain: {bad:#?}"
        );
    });
}

/// ui11: the barrier sequence's old hash meets `E_PROPOSAL_MOVED` and 409, and never lands; only
/// the fresh form posted apart lands, and no version lands twice.
#[test]
fn ui11_moved_ui_answer_never_lands() {
    each(|seed, o| {
        let all = ui_entries(o);
        let moved: Vec<&&Entry> = all
            .iter()
            .filter(|e| e.outcome == Met::Refused("E_PROPOSAL_MOVED".into()))
            .collect();
        assert_eq!(moved.len(), 1, "swarm seed {seed}: {all:#?}");
        let old = moved[0].ui.as_ref().expect("the facts");
        assert_eq!(old.status, 409, "swarm seed {seed}: {old:?}");
        assert_eq!(old.landing, None);
        assert!(old.item.ends_with("@1"), "swarm seed {seed}: {old:?}");
        let (stable, _) = old.item.rsplit_once('@').expect("a version ref");
        let fresh = format!("{stable}@2");
        let landed: Vec<&&Entry> = all
            .iter()
            .filter(|e| e.block.as_deref() == Some(fresh.as_str()) && e.outcome == Met::Ok)
            .collect();
        assert_eq!(landed.len(), 1, "swarm seed {seed}: {all:#?}");
        assert!(
            landed[0]
                .ui
                .as_ref()
                .is_some_and(|u| u.status == 303 && u.landing.is_some()),
            "swarm seed {seed}: {:?}",
            landed[0]
        );
        // the versions the journal names are the store's: the oracle checks the chain for each
        let bad = oracle::ui_violations(&o.journal, &o.dir, false);
        assert!(bad.is_empty(), "swarm seed {seed}: {bad:#?}");
    });
}

/// ui11 (D-Y9DNZ): the imported accepted decision: its importer's `rdy_land` is refused with the
/// never-list reason; the person's UI approval lands it; the store keeps its source author and its
/// importer, and the landing is a person's, never a policy act.
#[test]
fn ui11_imported_status_never_lands_under_policy_in_the_swarm() {
    use rdy_qa::swarm::people::UI_IMPORTED;
    each(|seed, o| {
        let run = o.before_freeze();
        let refused = run
            .iter()
            .find(|e| e.act == "ui_policy_land")
            .unwrap_or_else(|| panic!("swarm seed {seed}: no policy landing tried"));
        assert_eq!(
            refused.outcome,
            Met::Refused("E_POLICY_HUMAN".into()),
            "swarm seed {seed}: {refused:?}"
        );
        assert!(
            refused
                .message
                .as_deref()
                .is_some_and(|m| m.contains(rdy_engine::policy_verdict::IMPORTED_STATUS)),
            "swarm seed {seed}: {refused:?}"
        );
        let pref = refused.block.as_deref().expect("the version");
        let at = run
            .iter()
            .position(|e| std::ptr::eq(e, refused))
            .expect("the entry");
        let answer = run[at..]
            .iter()
            .find(|e| e.act == "ui_answer" && e.block.as_deref() == Some(pref))
            .unwrap_or_else(|| panic!("swarm seed {seed}: no person's answer to {pref}"));
        assert_eq!(answer.outcome, Met::Ok, "swarm seed {seed}: {answer:?}");
        let landing: Hash = answer
            .ui
            .as_ref()
            .and_then(|u| u.landing.as_deref())
            .and_then(|l| l.parse().ok())
            .unwrap_or_else(|| panic!("swarm seed {seed}: {answer:?}"));
        let (_keep, copy) = copy_of(&o.frozen);
        let r = Rdy::open(&copy).expect("open the frozen copy");
        let st = r.read();
        let rec = st.reader().landing(&landing).expect("the landing");
        assert_eq!(rec.assurance, rdy_model::Assurance::Local);
        assert_eq!(rec.policy, None, "swarm seed {seed}: no policy act");
        assert_eq!(rec.approved_by.actor, "human:qa");
        let (mut source, mut importer) = (false, false);
        for h in &rec.changes {
            let c = st.reader().change(h).expect("the change");
            source |= c.provenance.actor == "agent:src";
            importer |= c
                .links
                .imported_by
                .as_ref()
                .is_some_and(|i| i.actor == "human:qa");
        }
        assert!(source && importer, "swarm seed {seed}: {rec:#?}");
        // the imported decision is in canon, accepted, by its source
        let decision = rdy_model::BlockId::parse(UI_IMPORTED.3).expect("a decision id");
        let m = st.materialized(&Scope::Canon).expect("canon");
        assert!(m.get(&decision).is_some(), "swarm seed {seed}");
        drop(st);
        r.close().expect("close the frozen copy");
    });
}

/// ui11: what a UI answer acknowledged before the kill stands in the frozen store and after a
/// reindex: the same records, none lost, none doubled, and the run did restart its daemon.
#[test]
fn ui11_acknowledged_ui_acts_survive_restart_and_reindex() {
    each(|seed, o| {
        let run = o.before_freeze();
        let last = run.iter().map(|e| e.epoch).max().unwrap_or(0);
        assert_eq!(last, 1, "swarm seed {seed}: the run restarted its daemon");
        let before_kill = ui_entries(o)
            .iter()
            .filter(|e| e.epoch == 0 && e.outcome == Met::Ok)
            .count();
        assert!(
            before_kill >= 3,
            "swarm seed {seed}: {before_kill} answers before the kill"
        );
        let bad = oracle::ui_violations(run, &o.frozen, false);
        assert!(bad.is_empty(), "swarm seed {seed}: {bad:#?}");
        let bad = oracle::ui_violations(run, &o.frozen, true);
        assert!(
            bad.is_empty(),
            "swarm seed {seed} after a reindex: {bad:#?}"
        );
    });
}

// ---- ck10: check acts in the swarm (D-JQD5X, D-ME2HD; system/quality-gate tb-A4DAV) ----

/// A directed rig for the check tests: a project with two writers (agents 0 and 1) and two
/// reviewers (agents 2 and 3), one journal, one queue for the person, and a gate. Each step runs
/// one actor's acts to their end before the next starts: a join is the barrier, never a sleep.
struct CheckRig {
    p: rdy_qa::swarm::project::SwarmProject,
    cfg: Config,
    j: rdy_qa::swarm::journal::Journal,
    g: std::sync::Arc<rdy_qa::swarm::agent::KillGate>,
    queue: rdy_qa::swarm::people::Queue,
}

impl CheckRig {
    fn new(seed: u64, gate: rdy_qa::swarm::agent::KillGate) -> CheckRig {
        let cfg = Config {
            seed,
            agents: 4,
            steps: 2,
            kill_at: None,
        };
        CheckRig {
            p: rdy_qa::swarm::project::build(&cfg),
            cfg,
            j: rdy_qa::swarm::journal::Journal::new(),
            g: std::sync::Arc::new(gate),
            queue: rdy_qa::swarm::people::Queue::default(),
        }
    }

    /// Agent `i` runs `acts` to the end; returns the proposals it made.
    fn agent(&self, i: usize, acts: Vec<Act>) -> Vec<String> {
        rdy_qa::swarm::agent::spawn(&self.p, &self.cfg, i, acts, self.j.clone(), self.g.clone())
            .join()
            .unwrap_or_else(|e| panic!("agent {i} panicked: {}", panic_text(&*e)))
            .proposals
    }

    /// The person runs `acts` to the end, with `queued` as its queue.
    fn person(&self, acts: Vec<PersonAct>, queued: &[String]) {
        *self.queue.lock().unwrap() = queued.to_vec();
        rdy_qa::swarm::people::spawn(
            &self.p,
            acts,
            self.j.clone(),
            self.queue.clone(),
            self.g.clone(),
        )
        .join()
        .unwrap_or_else(|e| panic!("the person panicked: {}", panic_text(&*e)));
    }

    /// The last entry of `act`.
    fn last(&self, act: &str) -> Entry {
        self.j
            .entries()
            .into_iter()
            .rev()
            .find(|e| e.act == act)
            .unwrap_or_else(|| panic!("no {act} entry: {}", lines(&self.j.entries())))
    }

    /// Stops the daemon and waits for it to exit (a hang guard, not a speed check).
    fn stop(&self) {
        self.p.served.stop();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while self.p.served.daemon_pid().is_some() {
            assert!(std::time::Instant::now() < end, "the daemon did not stop");
            std::thread::yield_now();
        }
    }

    fn dir(&self) -> &std::path::Path {
        self.p.served.dir.path()
    }
}

/// The request, the concern on it and the version of a directed hold sequence: agent 0 makes
/// and proposes its check view and requests an independent review of the version; reviewer 3
/// submits the deterministic concern.
fn hold_request(rig: &CheckRig) -> (String, String) {
    use rdy_qa::swarm::script::HoldStep;
    let made = rig.agent(
        0,
        vec![Act::CheckRequest {
            hold: Some(HoldStep::Request),
        }],
    );
    let v1 = made.first().cloned().expect("the check view's version");
    let asked = rig.last("check_request");
    assert_eq!(asked.outcome, Met::Ok, "{asked:?}");
    let facts = asked.check.clone().expect("the request's facts");
    assert_eq!(facts.version.as_deref(), Some(v1.as_str()), "{facts:?}");
    assert!(
        facts
            .request
            .as_deref()
            .is_some_and(|r| r.starts_with("rq-"))
    );
    assert!(facts.case_hash.is_some(), "{facts:?}");
    rig.agent(3, vec![Act::CheckSubmit]);
    let concern = rig.last("check_submit");
    assert_eq!(concern.outcome, Met::Ok, "{concern:?}");
    assert_eq!(concern.actor, "agent:a3", "another actor reviews");
    let c = concern.check.clone().expect("the verdict's facts");
    assert_eq!(c.request, facts.request, "the reviewer claimed the request");
    assert_eq!(c.verdict.as_deref(), Some("concern"));
    (v1, c.review.expect("the review"))
}

/// The versions `rig`'s journal names, and the stored check records of its project.
fn rig_checks(rig: &CheckRig) -> (BTreeSet<String>, rdy_qa::swarm::oracle::StoredChecks) {
    let versions = oracle::proposed_versions(&rig.j.entries());
    let stored = oracle::stored_checks(rig.dir(), &versions);
    (versions, stored)
}

/// The full hash of version `pref`, as the project at `dir` resolves it.
fn version_hash(dir: &std::path::Path, pref: &str) -> String {
    let (_keep, copy) = copy_of(dir);
    let r = Rdy::open(&copy).expect("open a copy");
    let h = r
        .resolve_proposal(pref)
        .expect("resolve")
        .version
        .to_string();
    r.close().expect("close the copy");
    h
}

/// ck10: a person's accepted verdict holds its version and never lands with it, in every order
/// of approval, acceptance and a new version; the journal and the stored oracle agree, with no
/// sleep (a join orders each step). Then the same oracle holds at the end of every seed's run.
///
/// - accept, then approve: the approval is refused (`E_PROPOSAL_NOT_READY`); a new version holds
///   nothing and lands;
/// - approve, then accept: the version lands, and no card is left to accept;
/// - a new version, then accept: the new version holds the same case, so its card shows the
///   verdict and the acceptance binds the new version; a further version inherits no hold and
///   lands.
#[test]
fn ck10_swarm_never_lands_an_accepted_current_verdict() {
    use rdy_qa::swarm::journal::Skip;
    use rdy_qa::swarm::script::HoldStep;
    let new_version = || Act::CheckRequest {
        hold: Some(HoldStep::NewVersion),
    };
    let land = PersonAct::Land { relayed: true };

    // accept, then approve, then a new version
    let rig = CheckRig::new(101, rdy_qa::swarm::agent::KillGate::new());
    let (v1, review) = hold_request(&rig);
    rig.person(vec![PersonAct::AcceptCheckVerdict], &[]);
    let accepted = rig.last("accept_check_verdict");
    assert_eq!(accepted.outcome, Met::Ok, "{accepted:?}");
    let facts = accepted.check.clone().unwrap();
    let h1 = version_hash(rig.dir(), &v1);
    assert_eq!(
        facts.answer.as_deref(),
        Some(format!("accept_verdict:{review}:{h1}").as_str())
    );
    assert_eq!(facts.review.as_deref(), Some(review.as_str()));
    assert_eq!(facts.version.as_deref(), Some(v1.as_str()));
    rig.person(vec![land.clone()], std::slice::from_ref(&v1));
    let refused = rig.last("land");
    assert_eq!(
        refused.outcome,
        Met::Refused("E_PROPOSAL_NOT_READY".into()),
        "{refused:?}"
    );
    let v2 = rig.agent(0, vec![new_version()])[0].clone();
    assert_ne!(v2, v1);
    rig.person(vec![land.clone()], std::slice::from_ref(&v2));
    assert_eq!(rig.last("land").outcome, Met::Ok, "{:?}", rig.last("land"));
    rig.stop();
    let (_, stored) = rig_checks(&rig);
    let h2 = version_hash(rig.dir(), &v2);
    assert_eq!(
        stored.accepted,
        BTreeSet::from([format!("accept_verdict:{review}:{h1}")])
    );
    assert_eq!(stored.holds[&h1], BTreeSet::from([review.clone()]));
    assert!(
        stored.holds[&h2].is_empty(),
        "the new version inherits no hold"
    );
    assert!(!stored.landed.contains(&h1) && stored.landed.contains(&h2));
    let v = oracle::check_violations_of(&rig.j.entries(), &stored);
    assert!(v.is_empty(), "{v:#?}");

    // approve, then accept
    let rig = CheckRig::new(102, rdy_qa::swarm::agent::KillGate::new());
    let (v1, _) = hold_request(&rig);
    rig.person(vec![land.clone()], std::slice::from_ref(&v1));
    assert_eq!(rig.last("land").outcome, Met::Ok, "{:?}", rig.last("land"));
    rig.person(vec![PersonAct::AcceptCheckVerdict], &[]);
    let none = rig.last("accept_check_verdict");
    assert_eq!(
        (none.outcome.clone(), none.skip),
        (Met::Skipped, Some(Skip::NoVerdictToAccept)),
        "{none:?}"
    );
    rig.stop();
    let (_, stored) = rig_checks(&rig);
    assert!(stored.accepted.is_empty(), "{stored:?}");
    assert!(stored.landed.contains(&version_hash(rig.dir(), &v1)));
    let v = oracle::check_violations_of(&rig.j.entries(), &stored);
    assert!(v.is_empty(), "{v:#?}");

    // a new version, then accept, then approve: the new version holds the same case, so its card
    // shows the verdict, and the acceptance binds that version; it holds that version only, and a
    // further version lands
    let rig = CheckRig::new(103, rdy_qa::swarm::agent::KillGate::new());
    let (v1, review) = hold_request(&rig);
    let v2 = rig.agent(0, vec![new_version()])[0].clone();
    assert_ne!(v2, v1);
    rig.person(vec![PersonAct::AcceptCheckVerdict], &[]);
    let accepted = rig.last("accept_check_verdict");
    assert_eq!(accepted.outcome, Met::Ok, "{accepted:?}");
    let (h1, h2) = (version_hash(rig.dir(), &v1), version_hash(rig.dir(), &v2));
    assert_eq!(
        accepted.check.as_ref().unwrap().answer.as_deref(),
        Some(format!("accept_verdict:{review}:{h2}").as_str())
    );
    rig.person(vec![land.clone()], std::slice::from_ref(&v2));
    assert_eq!(
        rig.last("land").outcome,
        Met::Refused("E_PROPOSAL_NOT_READY".into())
    );
    let v3 = rig.agent(0, vec![new_version()])[0].clone();
    rig.person(vec![land.clone()], std::slice::from_ref(&v3));
    assert_eq!(rig.last("land").outcome, Met::Ok, "{:?}", rig.last("land"));
    rig.stop();
    let (_, stored) = rig_checks(&rig);
    let h3 = version_hash(rig.dir(), &v3);
    assert_eq!(
        stored.accepted,
        BTreeSet::from([format!("accept_verdict:{review}:{h2}")])
    );
    assert!(
        stored.holds[&h1].is_empty(),
        "an older version gains no hold"
    );
    assert_eq!(stored.holds[&h2], BTreeSet::from([review.clone()]));
    assert!(
        stored.holds[&h3].is_empty(),
        "the new version inherits no hold"
    );
    assert!(!stored.landed.contains(&h2) && stored.landed.contains(&h3));
    let v = oracle::check_violations_of(&rig.j.entries(), &stored);
    assert!(v.is_empty(), "{v:#?}");

    // every seed's run: the stored oracle at the drained end agrees with the journal
    each(|seed, o| {
        let stored = oracle::stored_checks(&o.dir, &oracle::proposed_versions(&o.journal));
        let held = stored.holds.values().filter(|h| !h.is_empty()).count();
        // the evidence a person reads with --nocapture
        eprintln!(
            "swarm seed {seed}: {} check requests, {} check verdicts, {} accepted, {held} held versions",
            stored.requests.len(),
            stored.reviews.len(),
            stored.accepted.len()
        );
        let v = oracle::check_violations_of(&o.journal, &stored);
        assert!(v.is_empty(), "swarm seed {seed}: {v:#?}");
    });
}

/// The `issues` (code, message) of a `propose` entry's reply.
fn propose_issues(e: &Entry, key: &str) -> Vec<(String, String)> {
    e.reply.as_ref().map_or(vec![], |r| {
        r[key]
            .as_array()
            .into_iter()
            .flatten()
            .map(|i| {
                (
                    i["code"].as_str().unwrap_or_default().to_string(),
                    i["message"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect()
    })
}

/// ck10: the child pack's blocking rule `check-fixture-key` on a stable claim refuses its version
/// until a real write repairs the claim; the repaired version lands; the same claim in a draft
/// doc is only a warning and blocks nothing. Then, at every landing of every seed's run, canon
/// holds each rule fixture checked, and each forced failing version was not ready with
/// `E_RULE_CHECK`.
#[test]
fn ck10_swarm_repairs_blocking_rules_before_landing() {
    use rdy_qa::swarm::project::{FIXTURE_KEY, FIXTURE_MESSAGE};
    use rdy_qa::swarm::script::RuleStep;
    let land = PersonAct::Land { relayed: true };
    let rig = CheckRig::new(104, rdy_qa::swarm::agent::KillGate::new());
    // the fixture's write is in the rule view, not the agent's own: an undo after it finds
    // nothing of its own to undo (it read no view; before the fix it undid view a0, which no
    // write had made, and met E_NOT_FOUND)
    let v1 = rig.agent(
        0,
        vec![
            Act::CheckRun {
                rule: Some(RuleStep::Fail),
            },
            Act::Undo,
        ],
    )[0]
    .clone();
    assert_eq!(
        rig.last("undo").outcome,
        Met::Skipped,
        "{:?}",
        rig.last("undo")
    );
    let made = rig.last("check_run");
    let c = made.block.clone().expect("the fixture claim");
    let proposed = rig.last("propose");
    assert_eq!(
        proposed.reply.as_ref().unwrap()["ready"],
        false,
        "{proposed:?}"
    );
    let issues = propose_issues(&proposed, "issues");
    let want = (
        "E_RULE_CHECK".to_string(),
        format!("rule check-fixture-key fails on {c}: {FIXTURE_MESSAGE}"),
    );
    assert!(issues.contains(&want), "{issues:#?}");
    rig.person(vec![land.clone()], std::slice::from_ref(&v1));
    assert_eq!(
        rig.last("land").outcome,
        Met::Refused("E_PROPOSAL_NOT_READY".into())
    );
    let v2 = rig.agent(
        0,
        vec![Act::CheckRun {
            rule: Some(RuleStep::Repair),
        }],
    )[0]
    .clone();
    assert_ne!(v2, v1);
    let repaired = rig.last("propose");
    assert_eq!(
        repaired.reply.as_ref().unwrap()["ready"],
        true,
        "{repaired:?}"
    );
    assert!(propose_issues(&repaired, "issues").is_empty());
    rig.person(vec![land.clone()], std::slice::from_ref(&v2));
    assert_eq!(rig.last("land").outcome, Met::Ok, "{:?}", rig.last("land"));
    rig.stop();
    let (_, stored) = rig_checks(&rig);
    assert!(stored.unchecked.is_empty(), "{:?}", stored.unchecked);
    {
        let (_keep, copy) = copy_of(rig.dir());
        let r = Rdy::open(&copy).unwrap();
        let st = r.read();
        let b = st.block(&rdy_engine::Scope::Canon, &BlockId::parse(&c).unwrap());
        let b = b.expect("the fixture landed");
        assert_eq!(b.field_str("key"), Some(FIXTURE_KEY));
        assert_eq!(
            b.fields.get("checked"),
            Some(&rdy_model::FieldValue::Bool(true))
        );
    }

    // a draft doc's fixture: a warning, never a block
    let rig = CheckRig::new(105, rdy_qa::swarm::agent::KillGate::new());
    let mut a = rdy_qa::mcp::McpClient::start_proxy(&rig.p.served);
    a.call(
        "rdy_session",
        serde_json::json!({"actor": "agent:a0", "run": "a0", "role": "writer"}),
    )
    .ok();
    let wrote = a
        .call(
            "rdy_write",
            serde_json::json!({"view": "a0", "editorial": {"class": "clarify", "reason": "a draft"},
            "ops": [
                {"op": "create", "type": "doc", "id": "$d",
                 "fields": {"name": "draft-fixture", "title": "Draft", "doc_type": "overview"}},
                {"op": "create", "type": "claim", "id": "$c", "parent": "$d",
                 "body": "A draft keeps the check fixture.", "fields": {"key": FIXTURE_KEY}}
            ]}),
        )
        .ok();
    let dc = wrote["created"]["$c"].as_str().unwrap().to_string();
    let warned: Vec<&str> = wrote["on_change"]["checks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r["code"].as_str())
        .collect();
    assert_eq!(warned, ["W_RULE_CHECK"], "{wrote:#}");
    let p = a
        .call(
            "rdy_propose",
            serde_json::json!({"view": "a0", "title": "draft", "summary": "A draft fixture."}),
        )
        .ok();
    assert_eq!(p["ready"], true, "{p:#}");
    let warnings: Vec<(String, String)> = p["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|w| w["code"] == "W_RULE_CHECK")
        .map(|w| {
            (
                w["code"].as_str().unwrap().to_string(),
                w["message"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        warnings,
        [(
            "W_RULE_CHECK".to_string(),
            format!("rule check-fixture-key fails on {dc}: {FIXTURE_MESSAGE}")
        )]
    );
    drop(a);
    rig.stop();

    // every seed's run
    each(|seed, o| {
        let stored = oracle::stored_checks(&o.dir, &BTreeSet::new());
        assert!(
            stored.unchecked.is_empty(),
            "swarm seed {seed}: {:?}",
            stored.unchecked
        );
        // the run's own proposes of a rule view record their readiness (the drain's do not)
        let before = o.before_freeze();
        for e in before.iter().filter(|e| {
            e.act == "propose"
                && e.outcome == Met::Ok
                && e.view.as_deref().is_some_and(|v| v.ends_with("-rule"))
        }) {
            let ready = e.reply.as_ref().is_some_and(|r| r["ready"] == true);
            let codes: Vec<String> = propose_issues(e, "issues")
                .into_iter()
                .map(|(code, _)| code)
                .collect();
            let blocked = codes.iter().any(|code| code == "E_RULE_CHECK");
            // the act's own plan answer (`answer_plan`), cut by the run's kill: the fixture's
            // claim owes its stable section's claims neighbor confirms (nb8, TASK-978 X2), and the
            // version proposed after the restart finds them unanswered (`E_PLAN_OPEN`), as
            // `answer_plan` says; the kill races the act at the seed's kill step
            let cut = before.iter().any(|x| {
                x.actor == e.actor
                    && x.step == e.step
                    && x.view == e.view
                    && matches!(x.act, "plan" | "plan_answer")
                    && x.outcome == Met::Lost
            });
            let step: Vec<String> = before
                .iter()
                .filter(|x| x.actor == e.actor && x.step == e.step)
                .map(|x| format!("{} {:?} epoch {}", x.act, x.outcome, x.epoch))
                .collect();
            // a version of the rule view is ready exactly when no blocking rule fails on it; when
            // the kill cut its plan answer, the open plan items are the only other issue
            let want = if cut {
                assert!(
                    codes
                        .iter()
                        .all(|code| code == "E_RULE_CHECK" || code == "E_PLAN_OPEN"),
                    "swarm seed {seed}: {} step {}: {:?}",
                    e.actor,
                    e.step,
                    e.reply
                );
                codes.is_empty()
            } else {
                !blocked
            };
            assert_eq!(
                ready, want,
                "swarm seed {seed}: {} step {}: {:?}; the step's entries: {step:?}",
                e.actor, e.step, e.reply
            );
        }
    });
}

/// ck10: a kill and a restart drop the leases only. A reviewer leases a check request; the daemon
/// is killed and restarted; another reviewer claims the same request and submits; the person
/// accepts the verdict, which holds its version. fsck passes, and a reindex rebuilds the same
/// requests, verdicts, acceptances and holds. Then, for every seed's run (which kills its daemon
/// once): every acknowledged request and verdict is stored, fsck passes, and the frozen and
/// drained projects rebuild the same check facts after a reindex.
#[test]
fn ck10_restart_keeps_requests_and_holds() {
    use rdy_qa::swarm::script::HoldStep;
    let rig = CheckRig::new(106, rdy_qa::swarm::agent::KillGate::with_kill());
    let made = rig.agent(
        0,
        vec![Act::CheckRequest {
            hold: Some(HoldStep::Request),
        }],
    );
    let v1 = made[0].clone();
    let rq = rig.last("check_request").check.unwrap().request.unwrap();
    // reviewer 2 leases the request and never answers it
    let mut r2 = rdy_qa::mcp::McpClient::start_proxy(&rig.p.served);
    r2.call(
        "rdy_session",
        serde_json::json!({"actor": "agent:a2", "run": "a2", "role": "reviewer"}),
    )
    .ok();
    let leased = r2
        .call(
            "rdy_review_next",
            serde_json::json!({"kind": "meaning_preservation"}),
        )
        .ok();
    assert_eq!(leased["request"], rq.as_str(), "{leased:#}");
    // while the lease holds, nobody else may claim it
    rig.agent(3, vec![Act::CheckSubmit]);
    assert_eq!(
        rig.last("check_submit").skip,
        Some(rdy_qa::swarm::journal::Skip::NoCheckRequest)
    );
    // the kill and the restart: the lease is gone, the request stays
    rig.g.killing();
    rig.p.served.kill9();
    rig.g.restarted();
    drop(r2);
    rig.agent(3, vec![Act::CheckSubmit]);
    let submitted = rig.last("check_submit");
    assert_eq!(submitted.outcome, Met::Ok, "{submitted:?}");
    assert_eq!(
        submitted.check.as_ref().unwrap().request.as_deref(),
        Some(rq.as_str())
    );
    rig.person(vec![PersonAct::AcceptCheckVerdict], &[]);
    assert_eq!(rig.last("accept_check_verdict").outcome, Met::Ok);
    rig.stop();
    let (versions, stored) = rig_checks(&rig);
    let h1 = version_hash(rig.dir(), &v1);
    assert!(stored.requests.contains_key(&rq), "{stored:?}");
    assert_eq!(stored.holds[&h1].len(), 1, "{stored:?}");
    let v = oracle::check_violations_of(&rig.j.entries(), &stored);
    assert!(v.is_empty(), "{v:#?}");
    assert_eq!(
        oracle::stored_checks_reindexed(rig.dir(), &versions),
        stored,
        "a reindex rebuilds the same check facts"
    );
    let (ok, fsck) = oracle::fsck(rig.dir(), &rig.p.served.env_pairs());
    assert!(ok, "rdy fsck failed: {fsck}");

    each(|seed, o| {
        let versions = oracle::proposed_versions(&o.journal);
        for (what, dir) in [("frozen", &o.frozen), ("drained", &o.dir)] {
            let stored = oracle::stored_checks(dir, &versions);
            assert_eq!(
                oracle::stored_checks_reindexed(dir, &versions),
                stored,
                "swarm seed {seed}: the {what} project's check facts after a reindex"
            );
        }
        // the frozen copy holds every request and verdict acknowledged before the freeze, those
        // before the kill included
        let stored = oracle::stored_checks(&o.frozen, &versions);
        let lost: Vec<String> = oracle::check_violations_of(o.before_freeze(), &stored)
            .into_iter()
            .filter(|v| v.contains("is not stored"))
            .collect();
        assert!(lost.is_empty(), "swarm seed {seed}: {lost:#?}");
        let (ok, fsck) = oracle::fsck(&o.frozen, &o.served.env_pairs());
        assert!(
            ok,
            "swarm seed {seed}: rdy fsck of the frozen copy failed: {fsck}"
        );
    });
}

// ---- nb10 (M3e, D-YXBXH, D-ME2HD, D-NJ2SP): one grouped answer for the displayed neighbor items ----

/// The ids a journal value lists, in order.
fn nb10_ids(v: &Value) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_str().map(str::to_string))
        .collect()
}

/// The open neighbor items with a confirm follow-up (`edge` neighbor, kind confirm or rejustify)
/// that the plan reads of the `plan_answer` entry at `at` showed. Those are the `plan` entries of
/// the same actor and view right before it, one per page; the actor makes no other call between
/// the read and the answers (`KillGate::plan_turn`).
fn nb10_displayed(entries: &[Entry], at: usize) -> Vec<String> {
    let me = &entries[at];
    let mut shown: Vec<Vec<String>> = vec![];
    for e in entries[..at].iter().rev().filter(|x| x.actor == me.actor) {
        if e.act != "plan" || e.view != me.view {
            break;
        }
        let rows = e.reply.as_ref().and_then(|r| r["items"].as_array());
        shown.push(
            rows.into_iter()
                .flatten()
                .filter(|i| {
                    i["state"] == "open"
                        && i["edge"] == "neighbor"
                        && matches!(i["kind"].as_str(), Some("confirm" | "rejustify"))
                })
                .filter_map(|i| i["id"].as_str().map(str::to_string))
                .collect(),
        );
    }
    shown.into_iter().rev().flatten().collect()
}

/// The checks of one grouped `plan_answer` entry (the one at `at`): it names the neighbor group,
/// its items are exactly the displayed ones, none twice, the reply returned exactly them, and
/// the answers it lists are those items once each with the group's act and reason, and no id
/// twice. Returns the group's items.
fn nb10_group_call(entries: &[Entry], at: usize) -> Vec<String> {
    let e = &entries[at];
    let who = format!("{} step {} {:?}", e.actor, e.step, e.view);
    let reply = e
        .reply
        .as_ref()
        .expect("a plan_answer entry names its answers");
    let group = &reply["group"];
    assert_eq!(group["edge"], "neighbor", "{who}: {reply}");
    assert_eq!(group["follow_up"], "confirm", "{who}: {reply}");
    let items = nb10_ids(&group["items"]);
    let shown = nb10_displayed(entries, at);
    assert!(!items.is_empty(), "{who}: an empty group is never sent");
    let unique: BTreeSet<&String> = items.iter().collect();
    assert_eq!(unique.len(), items.len(), "{who}: a repeated id: {items:?}");
    assert_eq!(
        unique,
        shown.iter().collect::<BTreeSet<_>>(),
        "{who}: the group is not the displayed neighbor items"
    );
    assert_eq!(
        nb10_ids(&group["returned"]).iter().collect::<BTreeSet<_>>(),
        unique,
        "{who}: the reply returned other answers than the group"
    );
    let answers = reply["answers"].as_array().expect("answers");
    let named: Vec<&str> = answers.iter().filter_map(|a| a["item"].as_str()).collect();
    assert_eq!(
        named.iter().collect::<BTreeSet<_>>().len(),
        named.len(),
        "{who}: an item answered twice: {named:?}"
    );
    for id in &items {
        let a = answers
            .iter()
            .find(|a| a["item"] == id.as_str())
            .unwrap_or_else(|| panic!("{who}: {id} is not among the answers"));
        assert_eq!(a["act"], "confirm", "{who}: {a}");
        assert_eq!(a["reason"], "swarm: checked", "{who}: {a}");
    }
    items
}

/// nb10: an agent's one `rdy_plan` call answers the neighbor items its plan read displayed with
/// one group `{edge: neighbor, follow_up: confirm}`. Agent a0 briefs, writes a stable claim and
/// proposes: its plan holds the claim's section neighbors. The group is exactly the displayed
/// neighbor items, no id twice, and the reply returned exactly them.
#[test]
fn nb10_swarm_agent_groups_only_displayed_neighbor_confirms() {
    const SEED: u64 = 41;
    let cfg = Config {
        seed: SEED,
        agents: 2,
        steps: 8,
        kill_at: None,
    };
    let read = |n| Act::Read {
        target: Pick::Seed(n),
    };
    let mut a0 = vec![read(0); 8];
    a0[0] = Act::Brief {
        target: Pick::Seed(0),
    };
    a0[1] = Act::Write {
        target: Pick::Seed(0),
        stable: true,
    };
    a0[2] = Act::Propose;
    let s = Script {
        agents: vec![a0, vec![read(1); 8]],
        person: vec![
            PersonAct::Tensions,
            PersonAct::Land { relayed: true },
            PersonAct::Land { relayed: true },
            PersonAct::Land { relayed: true },
        ],
        contra: vec![],
    };
    let o = match seeded(SEED, || run::run_script(&cfg, s)) {
        Ok(o) => o,
        Err(text) => panic!("{text}"),
    };
    let run = o.before_freeze();
    let calls: Vec<usize> = (0..run.len())
        .filter(|&k| run[k].act == "plan_answer" && run[k].actor == "agent:a0")
        .collect();
    assert_eq!(calls.len(), 1, "swarm seed {SEED}:\n{}", lines(run));
    assert_eq!(run[calls[0]].outcome, Met::Ok, "{:?}", run[calls[0]]);
    let items = nb10_group_call(run, calls[0]);
    assert!(
        items.len() >= 3,
        "swarm seed {SEED}: the claim's three section neighbors: {items:?}"
    );
    // every item of the call is a neighbor item or an item of an existing kind: the group is
    // the only way a neighbor item is answered
    let reply = run[calls[0]].reply.as_ref().unwrap();
    assert!(
        reply["answers"]
            .as_array()
            .unwrap()
            .iter()
            .all(|a| items.contains(&a["item"].as_str().unwrap().to_string())
                || a["kind"] != "confirm"
                || a["reason"] == "swarm: checked"),
        "{reply}"
    );
    assert!(
        o.violations.is_empty(),
        "swarm seed {SEED}: {:?}",
        o.violations
    );
}

/// nb10: the person's CLI fallback sends the same group, `rdy plan confirm --edge neighbor
/// --follow-up confirm --reason "swarm: checked" --view work`, and receives the same membership.
/// The person rewords a seed claim in `work` and answers its plan before it proposes.
#[test]
fn nb10_swarm_person_uses_the_same_neighbor_group() {
    use rdy_qa::swarm::agent::KillGate;
    use rdy_qa::swarm::journal::Journal;
    use rdy_qa::swarm::people;
    use rdy_qa::swarm::project;
    use std::sync::{Arc, Mutex};
    let cfg = Config {
        seed: 42,
        agents: 1,
        steps: 4,
        kill_at: None,
    };
    let p = project::build(&cfg);
    let (j, g) = (Journal::new(), Arc::new(KillGate::new()));
    let queue = Arc::new(Mutex::new(vec![]));
    people::spawn(&p, vec![PersonAct::WcEdit], j.clone(), queue, g)
        .join()
        .unwrap_or_else(|e| panic!("the person panicked: {e:?}"));
    let entries = j.entries();
    let calls: Vec<usize> = (0..entries.len())
        .filter(|&k| entries[k].act == "plan_answer" && entries[k].view.as_deref() == Some("work"))
        .collect();
    assert!(!calls.is_empty(), "{}", lines(&entries));
    let grouped: Vec<usize> = calls
        .iter()
        .copied()
        .filter(|&k| {
            entries[k]
                .reply
                .as_ref()
                .is_some_and(|r| r.get("group").is_some())
        })
        .collect();
    assert_eq!(
        grouped.len(),
        1,
        "one command carries the group:\n{}",
        lines(&entries)
    );
    let k = grouped[0];
    assert_eq!(entries[k].actor, "human:qa");
    assert_eq!(entries[k].outcome, Met::Ok, "{:?}", entries[k]);
    let items = nb10_group_call(&entries, k);
    assert!(items.len() >= 3, "{items:?}");
    // the person answers the neighbor items by the group alone, in the same command as its other
    // confirm items of that reason, and no id twice across the commands
    let mut all: Vec<String> = vec![];
    for &c in &calls {
        let reply = entries[c].reply.as_ref().unwrap();
        all.extend(nb10_ids(&Value::Array(
            reply["answers"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a["item"].clone())
                .collect(),
        )));
    }
    assert_eq!(
        all.iter().collect::<BTreeSet<_>>().len(),
        all.len(),
        "an item answered twice: {all:?}"
    );
}

/// nb10: a neighbor update item (an entity neighbor that states a removed literal) still defers
/// with `swarm: later`; a declared item and a tension item keep the acts of their kinds; a code
/// item stays open; a neighbor confirm item joins the group and is not answered by id.
#[test]
fn nb10_entity_update_and_declared_items_keep_their_acts() {
    use rdy_qa::swarm::agent::{PlanRow, split_plan_answers};
    let row = |id: &str, kind: &str, edge: &str| PlanRow {
        id: id.into(),
        kind: kind.into(),
        edge: edge.into(),
    };
    let open = [
        row("pi-1", "confirm", "neighbor"),
        row("pi-2", "update", "neighbor"),
        row("pi-3", "confirm", "references"),
        row("pi-4", "update", "references"),
        row("pi-5", "reaffirm", "decision"),
        row("tn-6", "tension", "tension"),
        row("pi-7", "update_implementation", "describes"),
        row("pi-8", "confirm", "neighbor"),
        row("pi-9", "rejustify", "neighbor"),
        row("pi-a", "rejustify", "implements"),
    ];
    let split = split_plan_answers(&open);
    // the group follows the engine's: edge neighbor and follow_up confirm (kind confirm or
    // rejustify)
    assert_eq!(split.group, ["pi-1", "pi-8", "pi-9"]);
    let by_id: Vec<(&str, &str, &str)> = split
        .explicit
        .iter()
        .map(|a| (a.item.as_str(), a.act, a.reason))
        .collect();
    assert_eq!(
        by_id,
        [
            ("pi-2", "defer", "swarm: later"),
            ("pi-3", "confirm", "swarm: checked"),
            ("pi-4", "defer", "swarm: later"),
            ("pi-5", "defer", "swarm: later"),
            ("tn-6", "confirm", "swarm: accepted"),
            ("pi-a", "defer", "swarm: later"),
        ]
    );
    // nothing displayed in the group: no group is sent
    assert!(
        split_plan_answers(&[row("pi-2", "update", "neighbor")])
            .group
            .is_empty()
    );
}

/// nb10 (fix round 1): a group answer whose reply is cut to pages is read whole under the plan
/// turn and compared on every page. A reply the cursor cuts is followed to its last page; ids on
/// later pages count; a missing id, an extra id and a repeated id are each refused, whichever
/// page they are on; a page that cannot be read leaves no check; a cursor that comes round again
/// is refused.
#[test]
fn nb10_paginated_group_reply_is_compared_whole() {
    use rdy_qa::swarm::agent::{follow_pages, group_returned};
    use serde_json::json;
    let item = |id: &str| {
        json!({"id": id, "kind": "confirm", "edge": "neighbor", "state": "closed",
               "answer": {"act": "confirm", "reason": "swarm: checked"}})
    };
    let page = |ids: &[&str], next: Option<&str>| {
        let mut p = json!({"items": ids.iter().map(|i| item(i)).collect::<Vec<_>>()});
        if let Some(n) = next {
            p["next"] = json!(n);
        }
        p
    };
    let group: Vec<String> = ["pi-1", "pi-2", "pi-3"].map(String::from).to_vec();
    let before = BTreeSet::new();
    // the pages of the reply: the cursor on each leads to the next
    let pages = [
        page(&["pi-1"], Some("c1")),
        page(&["pi-2"], Some("c2")),
        page(&["pi-3"], None),
    ];
    let fetch_from = |pages: Vec<Value>| {
        let asked = std::cell::RefCell::new(vec![]);
        let read = follow_pages(&pages[0], |c| {
            asked.borrow_mut().push(c.to_string());
            let n: usize = c[1..].parse().unwrap();
            pages.get(n).cloned()
        });
        (read, asked.into_inner())
    };
    let (read, asked) = fetch_from(pages.to_vec());
    assert_eq!(asked, ["c1", "c2"], "every cursor is followed once");
    let whole = read.unwrap().expect("every page was read");
    assert_eq!(whole.len(), 3);
    assert_eq!(group_returned(&whole, &group, &before).unwrap(), group);
    // a reply that holds only the first page and still has a cursor is not the whole group
    assert!(group_returned(&whole[..1], &group, &before).is_err());
    // an id missing from the last page, an id outside the group on a later page, and a repeat
    for (label, last) in [
        ("missing", page(&[], None)),
        ("extra", page(&["pi-3", "pi-9"], None)),
        ("repeat", page(&["pi-3", "pi-1"], None)),
    ] {
        let (read, _) = fetch_from(vec![pages[0].clone(), page(&["pi-2"], Some("c2")), last]);
        let whole = read.unwrap().unwrap();
        let err = group_returned(&whole, &group, &before).expect_err(label);
        assert!(err.contains("3 page(s)"), "{label}: {err}");
    }
    // an id the swarm held before the call is not an answer of this call
    let held: BTreeSet<String> = ["pi-0".to_string()].into();
    let with_held = [
        page(&["pi-0", "pi-1"], Some("c1")),
        page(&["pi-2", "pi-3"], None),
    ];
    assert_eq!(
        group_returned(&with_held, &group, &held).unwrap(),
        group,
        "ids held before are left out"
    );
    // a page that cannot be read: no pages, so no check
    let first = page(&["pi-1"], Some("c1"));
    assert_eq!(follow_pages(&first, |_| None).unwrap(), None);
    // a cursor that comes round again would never end
    let looped = page(&["pi-1"], Some("c1"));
    assert!(follow_pages(&looped, |_| Some(page(&["pi-2"], Some("c1")))).is_err());
}

/// nb10: a kill and a reconnect keep the neighbor answers. Two agents each write a stable claim
/// and propose; the run kills the daemon once, at the existing seam (`Config::kill_at`), and the
/// drain lands what is left. Each acknowledged grouped answer is a group of the displayed items,
/// and the seal of the landing of its view names each of those items as confirmed, and no sealed
/// neighbor item is open. No timer is asserted.
#[test]
fn nb10_kill_and_reconnect_preserve_neighbor_answers() {
    const SEED: u64 = 43;
    let cfg = Config {
        seed: SEED,
        agents: 2,
        steps: 10,
        kill_at: Some(4),
    };
    let read = |n| Act::Read {
        target: Pick::Seed(n),
    };
    let acts = |n| {
        let mut a = vec![read(n); 10];
        a[0] = Act::Brief {
            target: Pick::Seed(n),
        };
        a[1] = Act::Write {
            target: Pick::Seed(n),
            stable: true,
        };
        a[2] = Act::Propose;
        a
    };
    let s = Script {
        agents: vec![acts(0), acts(1)],
        person: vec![
            PersonAct::Tensions,
            PersonAct::Land { relayed: true },
            PersonAct::Land { relayed: true },
            PersonAct::Land { relayed: true },
            PersonAct::Land { relayed: true },
        ],
        contra: vec![],
    };
    let o = match seeded(SEED, || run::run_script(&cfg, s)) {
        Ok(o) => o,
        Err(text) => panic!("{text}"),
    };
    assert!(
        o.journal.iter().any(|e| e.epoch == 1),
        "swarm seed {SEED}: the daemon never restarted"
    );
    let (_keep, copy) = copy_of(&o.dir);
    let r = Rdy::open(&copy).expect("open the drained copy");
    let st = r.read();
    let mut checked = 0;
    for (at, e) in o.journal.iter().enumerate() {
        if e.act != "plan_answer"
            || e.outcome != Met::Ok
            || e.reply.as_ref().is_none_or(|r| r.get("group").is_none())
        {
            continue;
        }
        let items = nb10_group_call(&o.journal, at);
        // the first proposal of the view after the answers, and its landing
        let pref = o.journal[at..]
            .iter()
            .find(|x| x.act == "propose" && x.outcome == Met::Ok && x.view == e.view)
            .and_then(|x| x.block.clone())
            .unwrap_or_else(|| panic!("swarm seed {SEED}: no proposal after {e:?}"));
        let landing = o
            .journal
            .iter()
            .filter(|x| (x.act == "land" && x.outcome == Met::Ok) || x.act == run::LAND_FOUND)
            .find(|x| x.block.as_deref() == Some(pref.as_str()))
            .and_then(|x| {
                landing_fields(x.reply.as_ref()?)["landing"]
                    .as_str()
                    .map(str::to_string)
            })
            .unwrap_or_else(|| {
                panic!(
                    "swarm seed {SEED}: {pref} never landed:\n{}",
                    lines(&o.journal)
                )
            });
        let q = PlanQuery {
            landing: Some(landing),
            max_tokens: Some(100_000),
            ..Default::default()
        };
        let seal = st
            .change_plan(&q, "work")
            .map(|p| serde_json::to_value(p).expect("a plan reply serializes"))
            .unwrap_or_else(|err| panic!("swarm seed {SEED}: the seal of {pref}: {err}"));
        let rows = seal["items"].as_array().cloned().unwrap_or_default();
        for id in &items {
            let row = rows
                .iter()
                .find(|i| i["id"] == id.as_str())
                .unwrap_or_else(|| panic!("swarm seed {SEED}: the seal of {pref} lacks {id}"));
            assert_eq!(row["outcome"], "confirmed", "swarm seed {SEED}: {row}");
        }
        assert!(
            rows.iter()
                .filter(|i| i["edge"] == "neighbor")
                .all(|i| i["outcome"] != "open"),
            "swarm seed {SEED}: an open neighbor item sealed in {pref}"
        );
        checked += 1;
    }
    drop(st);
    r.close().expect("close the drained copy");
    assert!(
        checked > 0,
        "swarm seed {SEED}: no grouped answer was acknowledged: the check is vacuous:\n{}",
        lines(&o.journal)
    );
}

// ---- mg10 (M3 gate, Task 10): the M3 routes ran, every landing passes the M3 audit, and the
// M3 records and queue items survive the kill, the reindex, the dump and the follower ----

use rdy_engine::check::verdicts::{VERDICT_ITEM, parse_accept_option};
use rdy_engine::pending::CARD_MAX_TOKENS;
use rdy_engine::pending::queue::{Pending, PendingQuery};
use rdy_model::LandingRecord;
use rdy_model::assessment::{AssessmentSeverity, AssessmentTensionStatus};
use rdy_model::{Answer, AnswerAct, SemanticTension, decision};
use rdy_qa::invariants::Violation;
use rdy_qa::m3_audit::{
    Hold, HoldKind, LandingFact, M3Facts, ObservedLanding, Shown as SeenCard, TemporalFacts,
    assessment_id, audit_m3, check_m3_in, check_temporal, holds_of,
};
use rdy_qa::publication_scenario::{scan_local_links, tree};
use rdy_qa::report::AREA_PROVEN_WITNESSES;
use rdy_qa::swarm::journal::Reply;
use rdy_qa::swarm::m3::{COVERED_BY_AREA, LAND_CARD};
use rdy_qa::swarm::script::{Route, m3_scenarios};
use rdy_store::redaction::StoredFrame;
use rdy_store::segment::Location;

/// The code an entry met, as a replay line prints it.
fn code_of(e: &Entry) -> String {
    match &e.outcome {
        Met::Ok => "ok".into(),
        Met::Refused(c) => c.clone(),
        Met::Lost => "lost (the kill cut the call)".into(),
        Met::Skipped => format!("skipped ({})", e.message.as_deref().unwrap_or("-")),
    }
}

/// One line that replays a failure: the seed (as the variable that runs it again), the
/// scenario, the act, the actor, the step, the ref and card hash shown, and the code the daemon
/// gave.
fn replay(seed: u64, e: &Entry) -> String {
    let a = e.route.as_ref();
    let scenario = a.map_or("-", |a| a.route.name());
    let shown = a
        .and_then(|a| a.observed_ref.as_deref())
        .or(e.block.as_deref())
        .unwrap_or("-");
    let card = a.and_then(|a| a.card_hash.as_deref()).unwrap_or("-");
    format!(
        "RDY_QA_SWARM_SEEDS={seed}: scenario {scenario}, act {}, actor {}, step {}, shown {shown} card {card}, code {}",
        e.act,
        e.actor,
        e.step,
        code_of(e)
    )
}

/// The M3 route acts (`Route::acts`).
fn is_route_act(act: &str) -> bool {
    Route::ALL.iter().any(|r| r.acts().contains(&act))
}

/// The landing ids an acknowledged route attempt names.
fn route_records(e: &Entry) -> Vec<String> {
    match e.route.as_ref().map(|a| &a.reply) {
        Some(Reply::Acknowledged { records, .. }) if e.outcome == Met::Ok => records.clone(),
        _ => vec![],
    }
}

/// The witnesses the swarm itself must give, across the fixed seed set (plan Task 10). A witness
/// another area proves is in `report::AREA_PROVEN_WITNESSES`, never here.
const SWARM_WITNESSES: [&str; 13] = [
    "relayed landing",
    "policy landing",
    "refused stale card then new-card answer",
    "accepted elicited answer (2025-11-25, in-call)",
    "accepted elicited answer (2026-07-28, inline)",
    "declined elicitation",
    "local UI answer",
    "explicit CLI fallback",
    "accepted blocking verdict that does not land",
    "answered plan or deferral",
    "compact with removed bytes",
    "org profile check of a held landing",
    "output link scan",
];

/// How many times one seed's run (before the freeze, except the store's own policy landings and
/// verdict holds) gave each witness of `SWARM_WITNESSES`.
fn swarm_witnesses(o: &Outcome) -> BTreeMap<&'static str, usize> {
    let run = o.before_freeze();
    let ok = |act: &str| {
        run.iter()
            .filter(|e| e.act == act && e.outcome == Met::Ok)
            .count()
    };
    let mut w: BTreeMap<&'static str, usize> = SWARM_WITNESSES.iter().map(|n| (*n, 0)).collect();
    let mut set = |name: &'static str, n: usize| {
        *w.get_mut(name).expect("a listed witness") += n;
    };
    set(
        "relayed landing",
        run.iter()
            .filter(|e| {
                matches!(e.act, "m3_relay_answer" | "m3_relay_after_decline")
                    && !route_records(e).is_empty()
            })
            .count(),
    );
    set("policy landing", landing_counts(o).0);
    // a refusal of the old card, then an acknowledged answer on a newer card, in one scenario
    let mut moved = 0;
    for (i, e) in run.iter().enumerate() {
        if e.act != "m3_moved_card" || e.outcome != Met::Refused("E_PROPOSAL_MOVED".into()) {
            continue;
        }
        let old = e.route.as_ref().and_then(|a| a.card_hash.clone());
        moved += usize::from(run[i + 1..].iter().any(|n| {
            n.act == "m3_moved_card"
                && n.actor == e.actor
                && n.step == e.step
                && !route_records(n).is_empty()
                && n.route.as_ref().and_then(|a| a.card_hash.clone()) != old
        }));
    }
    set("refused stale card then new-card answer", moved);
    set(
        "accepted elicited answer (2025-11-25, in-call)",
        run.iter()
            .filter(|e| e.act == "m3_elicit_first_rev" && !route_records(e).is_empty())
            .count(),
    );
    set(
        "accepted elicited answer (2026-07-28, inline)",
        run.iter()
            .filter(|e| e.act == "m3_elicit_second_rev" && !route_records(e).is_empty())
            .count(),
    );
    set(
        "declined elicitation",
        run.iter()
            .filter(|e| e.act == "m3_elicit_decline" && e.outcome == Met::Ok)
            .filter(|e| e.reply.as_ref().is_some_and(|r| r["state"] == "declined"))
            .count(),
    );
    set(
        "local UI answer",
        run.iter()
            .filter(|e| e.act == "ui_answer" && e.outcome == Met::Ok)
            .filter(|e| e.ui.as_ref().is_some_and(|u| u.landing.is_some()))
            .count(),
    );
    set(
        "explicit CLI fallback",
        run.iter()
            .filter(|e| e.act == "land" && e.outcome == Met::Ok && e.actor == HUMAN)
            .count(),
    );
    let stored = oracle::stored_checks(&o.dir, &oracle::proposed_versions(&o.journal));
    set(
        "accepted blocking verdict that does not land",
        stored
            .holds
            .iter()
            .filter(|(v, h)| !h.is_empty() && !stored.landed.contains(*v))
            .count(),
    );
    // the random seeds answer plans; a deferral card comes in the directed rg15 run
    // (`rg15_the_person_accepts_a_deferral_card_before_the_approve`)
    set(
        "answered plan or deferral",
        ok("plan_answer") + ok("accept_deferrals"),
    );
    set(
        "compact with removed bytes",
        run.iter()
            .filter(|e| e.act == "m3_compact_real" && e.outcome == Met::Ok)
            .filter(|e| {
                e.reply
                    .as_ref()
                    .and_then(|r| r["removed_bytes"].as_u64())
                    .is_some_and(|n| n > 0)
            })
            .count(),
    );
    set(
        "org profile check of a held landing",
        run.iter()
            .filter(|e| e.act == "m3_publication_check" && e.outcome == Met::Ok)
            .filter(|e| {
                e.route
                    .as_ref()
                    .and_then(|a| a.observed_ref.as_deref())
                    .is_some_and(|s| s.starts_with("landing:"))
            })
            .count(),
    );
    set(
        "output link scan",
        run.iter()
            .filter(|e| e.act == "m3_publication_output" && e.outcome == Met::Ok)
            .filter(|e| e.route.as_ref().is_some_and(|a| a.counts.links_checked > 0))
            .count(),
    );
    w
}

/// How many org profile checks of a held landing read `warning`.
fn org_warnings(o: &Outcome) -> usize {
    o.before_freeze()
        .iter()
        .filter(|e| e.act == "m3_publication_check" && e.outcome == Met::Ok)
        .filter(|e| {
            e.reply
                .as_ref()
                .is_some_and(|v| v["references"]["status"] == "warning")
        })
        .count()
}

/// The source of the test file `file` under `tests/`.
fn test_source(file: &str) -> String {
    let at = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(file);
    std::fs::read_to_string(&at).unwrap_or_else(|e| panic!("read {}: {e}", at.display()))
}

/// Plan Task 10 (as rulings task-8 and task-10 finding 1 amend it): across the fixed seed set the
/// swarm gave every witness of `SWARM_WITNESSES`, and each area-proven witness still names an
/// existing area test (a guard only; the area run proves it). Every M3 route attempt met an
/// outcome of its act's closed set, and each failure names the line that replays it.
#[test]
fn mg10_swarm_exercised_every_required_m3_scenario() {
    let mut all: BTreeMap<&'static str, usize> = SWARM_WITNESSES.iter().map(|n| (*n, 0)).collect();
    let mut failed = vec![];
    for (seed, r) in runs() {
        let o = match r {
            Ok(o) => o,
            Err(text) => {
                failed.push(text.clone());
                continue;
            }
        };
        let w = swarm_witnesses(o);
        // the queue items the person worked down before an M3 card past the cap (deep-drain
        // ruling, finding 2)
        let worked = o
            .journal
            .iter()
            .filter(|e| e.act == rdy_qa::swarm::people::WORK_DOWN)
            .count();
        eprintln!(
            "swarm seed {seed}: M3 witnesses {w:?}, org warnings {}, work-down items {worked}",
            org_warnings(o)
        );
        for (k, n) in w {
            *all.get_mut(k).expect("a listed witness") += n;
        }
        // every scheduled route left a trace: an attempt, or a visible gap with its reason
        let scheduled = m3_scenarios(&Config::for_run(*seed));
        for sc in &scheduled {
            let trace = o.journal.iter().any(|e| {
                sc.route.acts().contains(&e.act) && e.actor == format!("agent:a{}", sc.agent)
            });
            if !trace {
                failed.push(format!(
                    "RDY_QA_SWARM_SEEDS={seed}: scenario {} (agent a{}, step {}) left no entry",
                    sc.route.name(),
                    sc.agent,
                    sc.step
                ));
            }
        }
        for e in o.journal.iter().filter(|e| is_route_act(e.act)) {
            // the role route is proven by the authority area: its entries are visible gaps that
            // carry no attempt, never a witness (ruling task-8 finding 1)
            if matches!(e.act, "m3_wrong_role" | "m3_right_role") {
                if e.outcome != Met::Skipped
                    || e.route.is_some()
                    || e.message.as_deref() != Some(COVERED_BY_AREA)
                {
                    failed.push(format!(
                        "the role route ran in the swarm: {}",
                        replay(*seed, e)
                    ));
                }
                continue;
            }
            let fine = match (&e.outcome, &e.route) {
                (Met::Skipped, None) => true,
                (_, Some(a)) => a.is_allowed() && !a.is_pending(),
                (_, None) => false,
            };
            if !fine {
                failed.push(format!(
                    "an outcome outside the closed set: {}",
                    replay(*seed, e)
                ));
            }
        }
    }
    eprintln!("swarm seeds: M3 witnesses {all:?}");
    if std::env::var("RDY_QA_SWARM_SEEDS").is_err() {
        let missing: Vec<&str> = all
            .iter()
            .filter(|(_, n)| **n == 0)
            .map(|(k, _)| *k)
            .collect();
        if !missing.is_empty() {
            let seeds: Vec<u64> = runs().iter().map(|(s, _)| *s).collect();
            failed.push(format!(
                "swarm seeds {seeds:?}: no run gave the M3 witness {missing:?}"
            ));
        }
    }
    // A guard against a rename or a deletion of the area test each area-proven witness names
    // (rulings task-8 and task-10 finding 1). It is not swarm evidence and proves no execution:
    // the gate's area run executes that test, and the gate report lists each such witness as
    // area-proven, not exercised under swarm concurrency.
    for (witness, file, test) in AREA_PROVEN_WITNESSES {
        if !test_source(file).contains(&format!("fn {test}()")) {
            failed.push(format!(
                "the witness {witness:?} names {test} in tests/{file}, which has no such test"
            ));
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

/// The person's queue in `st`, every card whole, as the swarm's person sees it with no session:
/// each item's ref and card hash, and the refs that are not ready.
fn queue_of(st: &ReadState) -> (Vec<(String, String)>, Vec<String>, usize) {
    let p: Pending = st
        .pending(&PendingQuery {
            person: HUMAN.into(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .expect("read the person's queue");
    (
        p.items
            .iter()
            .map(|i| (i.r#ref.clone(), i.card_hash.clone()))
            .collect(),
        p.not_ready.iter().map(|n| n.r#ref.clone()).collect(),
        p.omitted,
    )
}

/// The landings of `st`'s canon chain, oldest first.
fn chain(st: &ReadState) -> Vec<(Hash, LandingRecord)> {
    let mut out = vec![];
    let mut cur = st.canon_head();
    while let Some(h) = cur {
        let rec = st.reader().landing(&h).expect("read a landing");
        cur = rec.prev;
        out.push((h, rec));
    }
    out.reverse();
    out
}

/// Canon's landings in the order the log's `canon` ref updates name them (`LogAt::canon`), each
/// with its record and its location: the committed history, read without following any record's
/// `prev`. Also every way that history disagrees with the log: a `canon` value that is no landing
/// id, a landing canon names twice, a landing whose batch did not set canon to it, a landing
/// frame canon never named, and a chain of `prev`s (`chain`) that is not this history.
fn canon_history(
    st: &ReadState,
    log: &LogAt,
) -> (Vec<(Hash, LandingRecord, Location)>, Vec<String>) {
    let reader = st.reader();
    let mut out: Vec<(Hash, LandingRecord, Location)> = vec![];
    let mut bad = vec![];
    for (moved, v) in &log.canon {
        let Some(h) = v else {
            bad.push(format!(
                "the canon ref update at {moved:?} names no landing"
            ));
            continue;
        };
        if out.iter().any(|(l, _, _)| l == h) {
            bad.push(format!("canon is set to landing {} twice", h.short()));
            continue;
        }
        let (rec, at) = match (reader.landing(h), reader.landing_location(h)) {
            (Ok(rec), Ok(at)) => (rec, at),
            (a, b) => {
                bad.push(format!(
                    "canon names landing {}, which cannot be read: {:?} {:?}",
                    h.short(),
                    a.err(),
                    b.err()
                ));
                continue;
            }
        };
        if !(at < *moved
            && log.batch_start(at).is_some()
            && log.batch_start(at) == log.batch_start(*moved))
        {
            bad.push(format!(
                "landing {} at {at:?}: canon was set to it at {moved:?}, not by its own batch",
                h.short()
            ));
        }
        out.push((*h, rec, at));
    }
    for (at, h) in &log.landing_frames {
        if !out.iter().any(|(l, _, _)| l == h) {
            bad.push(format!(
                "the landing frame {} at {at:?} is never canon",
                h.short()
            ));
        }
    }
    let by_prev: Vec<Hash> = chain(st).into_iter().map(|(h, _)| h).collect();
    let by_ref: Vec<Hash> = out.iter().map(|(h, _, _)| *h).collect();
    if by_prev != by_ref {
        bad.push(format!(
            "the chain of prevs ({} landings) is not canon's ref history ({} landings)",
            by_prev.len(),
            by_ref.len()
        ));
    }
    (out, bad)
}

/// What the person saw before each journaled landing it approved through chat: the version
/// (`P-x@n`) and the card hash, by landing id. A route answer keeps them in its attempt; a
/// relayed `land` keeps them in the `land_card` read just before it.
fn seen_cards(journal: &[Entry]) -> BTreeMap<String, (String, String)> {
    let mut out = BTreeMap::new();
    for (i, e) in journal.iter().enumerate() {
        if let Some(a) = &e.route {
            for l in route_records(e) {
                if let (Some(r), Some(c)) = (&a.observed_ref, &a.card_hash) {
                    out.insert(l, (r.clone(), c.clone()));
                }
            }
            continue;
        }
        if e.act != "land" || e.outcome != Met::Ok || e.actor != "agent:relay" {
            continue;
        }
        let (Some(pref), Some(l)) = (
            e.block.as_deref(),
            e.reply
                .as_ref()
                .and_then(|r| landing_fields(r)["landing"].as_str()),
        ) else {
            continue;
        };
        let card = journal[..i]
            .iter()
            .rev()
            .find(|c| c.act == LAND_CARD && c.block.as_deref() == Some(pref))
            .and_then(|c| c.reply.as_ref())
            .and_then(|r| r["shown"]["card_hash"].as_str());
        if let Some(c) = card {
            out.insert(l.to_string(), (pref.to_string(), c.to_string()));
        }
    }
    out
}

/// The answers and semantic tension versions of a closed copy's log, each at its log location,
/// in log order. With them the person acts recorded before any log position are read at that
/// position, never from the final state: a later answer to an item replaces the earlier one in
/// the store's ref (`answer_of`), so a hold the person later answered over would be lost.
///
/// The same walk reads canon's head history from the log's own records, never from a landing
/// record's `prev`: every `REF_UPDATE` frame that sets the ref `canon` (`canon`), every landing
/// frame (`landing_frames`), every batch marker with its batch's first frame (`batches`), and
/// every frame a removal zeroed with the location of the apply that authorizes it (`zeroed`).
struct LogAt {
    answers: Vec<(Location, Answer)>,
    tensions: Vec<(Location, SemanticTension)>,
    /// Each value the ref `canon` was set to, at its `REF_UPDATE` frame, in log order. `None`
    /// is a value that is not a landing id.
    canon: Vec<(Location, Option<Hash>)>,
    landing_frames: Vec<(Location, Hash)>,
    /// `(batch start, marker)` of every batch, in log order.
    batches: Vec<(Location, Location)>,
    /// `(frame, apply)`: a frame a removal zeroed, and where the log holds the apply.
    zeroed: Vec<(Location, Option<Location>)>,
}

impl LogAt {
    fn read(st: &ReadState) -> LogAt {
        let reader = st.reader();
        let mut found = vec![];
        let mut log = LogAt {
            answers: vec![],
            tensions: vec![],
            canon: vec![],
            landing_frames: vec![],
            batches: vec![],
            zeroed: vec![],
        };
        let mut dropped = vec![];
        let mut applies = BTreeMap::new();
        let origin = Location {
            segment: 0,
            offset: 0,
        };
        reader
            .for_each_record_from(origin, st.seq(), |loc, f, _| {
                match f {
                    StoredFrame::Present {
                        kind,
                        hash,
                        payload,
                    } => {
                        let k = rdy_store::codec::logical_kind(kind);
                        if k == kinds::ANSWER || k == kinds::TENSION {
                            found.push((loc, k, hash));
                        } else if k == kinds::LANDING {
                            log.landing_frames.push((loc, hash));
                        } else if k == kinds::REDACTION_APPLY {
                            applies.insert(hash, loc);
                        } else if k == kinds::REF_UPDATE {
                            let u: kinds::RefUpdate =
                                rdy_store::codec::decode_record(kind, payload)?;
                            if u.name == "canon" {
                                let v = <[u8; 32]>::try_from(&u.value[..]).ok().map(Hash);
                                log.canon.push((loc, v));
                            }
                        } else if k == kinds::BATCH_COMMIT {
                            let m = kinds::BatchCommit::decode(payload)
                                .expect("a batch marker decodes");
                            log.batches.push((m.start, loc));
                        }
                    }
                    // a frame a redaction removed holds no record to read (`visit_before` skips it)
                    StoredFrame::Dropped { apply, .. } => dropped.push((loc, apply)),
                }
                Ok(())
            })
            .expect("walk the log");
        log.zeroed = dropped
            .into_iter()
            .map(|(loc, apply)| (loc, applies.get(&apply).copied()))
            .collect();
        for (loc, k, h) in found {
            if k == kinds::ANSWER {
                let a: Answer = reader.get_typed(k, &h).expect("read an answer");
                // a snooze is the person's own pending state (`pending/<person>/<item>`)
                if a.act != AnswerAct::Later {
                    log.answers.push((loc, a));
                }
            } else {
                log.tensions
                    .push((loc, reader.get_typed(k, &h).expect("read a tension")));
            }
        }
        log
    }

    /// Canon's head at log position `at`: the value of the last `canon` ref update before it.
    fn head_before(&self, at: Location) -> Option<Hash> {
        self.canon
            .iter()
            .rev()
            .find(|(l, _)| *l < at)
            .and_then(|(_, v)| *v)
    }

    /// The landings canon named before log position `at`, oldest first, by its ref updates.
    fn heads_before(&self, at: Location) -> Vec<Hash> {
        self.canon
            .iter()
            .filter(|(l, _)| *l < at)
            .filter_map(|(_, v)| *v)
            .collect()
    }

    /// The first frame of the batch that holds the frame at `at`.
    fn batch_start(&self, at: Location) -> Option<Location> {
        self.batches
            .iter()
            .find(|(start, marker)| *start <= at && at < *marker)
            .map(|(start, _)| *start)
    }

    /// Whether the log cut at `cut` can be opened: no frame before the cut was zeroed by a
    /// removal whose apply the cut leaves out (that frame would be an unauthorized hole).
    fn prefix_readable(&self, cut: Location) -> bool {
        self.zeroed
            .iter()
            .all(|(l, apply)| *l >= cut || apply.is_some_and(|a| a < cut))
    }

    /// The latest answer to `item` before `at`: what the ref `answer/<item>` named there.
    fn answer_before(&self, item: &str, at: Location) -> Option<&Answer> {
        self.answers
            .iter()
            .rev()
            .find(|(l, a)| *l < at && a.item == item)
            .map(|(_, a)| a)
    }

    /// How many answers a later answer to the same item replaced.
    fn replaced(&self) -> usize {
        let mut seen = BTreeSet::new();
        self.answers
            .iter()
            .rev()
            .filter(|(_, a)| !seen.insert(a.item.clone()))
            .count()
    }

    /// Each pair's latest semantic tension version before `at`.
    fn tensions_before(&self, at: Location) -> BTreeMap<Hash, &SemanticTension> {
        let mut out = BTreeMap::new();
        for (l, t) in &self.tensions {
            if *l < at {
                out.insert(t.key, t);
            }
        }
        out
    }
}

/// `P-<stable short>@<n>` of version `version` (`m3_audit`'s `version_ref`).
fn version_ref_of(st: &ReadState, version: &Hash) -> String {
    let reader = st.reader();
    let p = reader.proposal(version).expect("read a proposal");
    let mut n = 1;
    let mut cur = p.prev;
    while let Some(prev) = cur {
        n += 1;
        cur = reader.proposal(&prev).expect("read a proposal").prev;
    }
    format!("P-{}@{n}", p.stable.unwrap_or(*version).short())
}

/// The person acts recorded before log position `at` that hold version `version`: Task 1's
/// `holds_of`, read at `at` (canon at `head`, canon's head there) instead of on the final
/// state. The five later refusals: a rejected decision, a rejected one_way re-affirmation and
/// refused deferrals (each the latest answer to its item before `at`), an accepted advice (the
/// latest version before `at` of a semantic tension the version's assessment lists as open
/// advisory is blocking) and an accepted check verdict before `at`.
fn holds_at(
    st: &ReadState,
    log: &LogAt,
    version: &Hash,
    at: Location,
    head: Option<Hash>,
) -> Result<Vec<Hold>, Violation> {
    let fail = |what: String| Violation {
        rule: "mg10_rebuild",
        block: None,
        detail: what,
    };
    let reader = st.reader();
    let p = reader
        .proposal(version)
        .map_err(|e| fail(format!("proposal {}: {e}", version.short())))?;
    let pref = version_ref_of(st, version);
    let answered =
        |item: &str, act: AnswerAct| log.answer_before(item, at).is_some_and(|a| a.act == act);
    let mut out = vec![];
    let mut last: BTreeMap<BlockId, Option<rdy_model::Block>> = BTreeMap::new();
    for c in &p.changes {
        let ch = reader
            .change(c)
            .map_err(|e| fail(format!("change {}: {e}", c.short())))?;
        for op in ch.ops {
            match op {
                Op::Create { block } | Op::Update { block, .. }
                    if block.block_type == decision::DECISION =>
                {
                    last.insert(block.id.clone(), Some(block));
                }
                op => {
                    if let Some(b) = last.get_mut(op.block_id()) {
                        *b = None;
                    }
                }
            }
        }
    }
    let canon = match head {
        Some(h) => Some(
            st.materialized(&Scope::At(h))
                .map_err(|e| fail(format!("canon at {}: {e}", h.short())))?,
        ),
        None => None,
    };
    for (id, b) in last {
        let Some(b) = b else { continue };
        let item = format!("{id}@{}", b.version_hash().short());
        if decision::status(&b) == Some(decision::DecisionStatus::Proposed) {
            if answered(&item, AnswerAct::RejectDecision) {
                out.push(Hold {
                    kind: HoldKind::RejectedDecision,
                    item,
                });
            }
        } else if !rdy_model::engine_fields::reaffirmed_causes(
            canon.as_ref().and_then(|c| c.get(&id)),
            &b,
        )
        .is_empty()
            && b.field_str("reversibility") == Some("one_way")
            && answered(&item, AnswerAct::RejectReaffirm)
        {
            out.push(Hold {
                kind: HoldKind::RejectedReaffirm,
                item,
            });
        }
    }
    let defer = format!("defer:{pref}");
    if answered(&defer, AnswerAct::RefuseDeferrals) {
        out.push(Hold {
            kind: HoldKind::RefusedDeferrals,
            item: defer,
        });
    }
    if let Some(a) = reader
        .assessment_of_proposal(version, at)
        .map_err(|e| fail(format!("the assessment of {}: {e}", version.short())))?
    {
        let a = reader
            .assessment(&a)
            .map_err(|e| fail(format!("assessment {}: {e}", a.short())))?;
        let advisory: Vec<BTreeSet<&BlockId>> = a
            .tensions
            .iter()
            .filter(|t| {
                t.kind == "semantic"
                    && t.severity == AssessmentSeverity::Advisory
                    && t.status == AssessmentTensionStatus::Open
            })
            .map(|t| t.blocks.iter().collect())
            .collect();
        let accepted = log.tensions_before(at).values().any(|t| {
            let blocks: BTreeSet<&BlockId> = t.items.iter().map(|i| &i.block).collect();
            t.blocking && advisory.contains(&blocks)
        });
        if accepted {
            out.push(Hold {
                kind: HoldKind::AcceptedAdvice,
                item: format!("advice:{pref}"),
            });
        }
    }
    let mut verdicts = vec![];
    reader
        .for_each_verdict_answer(at, |_, a| {
            if let Some((review, v)) = a.option.as_deref().and_then(parse_accept_option)
                && v == *version
                && a.item == format!("{VERDICT_ITEM}{review}")
            {
                verdicts.push(a.item);
            }
            Ok(())
        })
        .map_err(|e| fail(format!("the verdict answers: {e}")))?;
    verdicts.sort();
    verdicts.dedup();
    out.extend(verdicts.into_iter().map(|item| Hold {
        kind: HoldKind::AcceptedVerdict,
        item,
    }));
    Ok(out)
}

/// The temporal facts of landing `h` (record `rec`) of the closed copy `r`, every input read at
/// log position `at` (the landing's own location, for the audit): canon's head there (the value
/// of the last `canon` ref update before `at`, `LogAt::head_before`: the log's own history,
/// never a landing record's `prev`), the landings canon named since the assessment's base, the
/// policy canon held at that head, and the person acts recorded before `at` (`holds_at`). The
/// pack and the gate config are the run's own (the swarm changes neither). `m3_snapshot`
/// compares that head with the record's `prev`, so it fails when the landing did not land on the
/// canon of its own log position, a `prev` that skips a head included.
fn rebuilt_temporal(
    r: &Rdy,
    st: &ReadState,
    log: &LogAt,
    h: Hash,
    rec: LandingRecord,
    at: Location,
    seen: Option<&(String, String)>,
) -> Result<TemporalFacts, Violation> {
    let fact = LandingFact::read_whole(st, h, rec.clone())?;
    let fail = |what: String| Violation {
        rule: "mg10_rebuild",
        block: None,
        detail: what,
    };
    let reader = st.reader();
    let first_briefed = reader
        .first_briefed_change()
        .map_err(|e| fail(format!("the first briefed change: {e}")))?;
    let before_head = log.head_before(at);
    let mut since = vec![];
    if let Some(a) = &fact.assessment {
        for l in log.heads_before(at).iter().rev() {
            if Some(*l) == a.base {
                break;
            }
            let lr = reader
                .landing(l)
                .map_err(|e| fail(format!("landing {}: {e}", l.short())))?;
            let mut written = BTreeSet::new();
            for c in &lr.changes {
                let ch = reader
                    .change(c)
                    .map_err(|e| fail(format!("change {}: {e}", c.short())))?;
                written.extend(ch.ops.iter().map(|op| op.block_id().clone()));
            }
            since.push((*l, written));
        }
        since.reverse();
    }
    let live_policy = match before_head {
        Some(head) => {
            let canon = st
                .materialized(&Scope::At(head))
                .map_err(|e| fail(format!("canon at {}: {e}", head.short())))?;
            rdy_engine::policy::live_policy(st, &canon).map(|p| p.reference())
        }
        None => None,
    };
    let holds = match rec.approved_version {
        Some(v) => holds_at(st, log, &v, at, before_head)?,
        None => vec![],
    };
    let shown = shown_of(r, seen)?;
    Ok(TemporalFacts {
        landing: fact,
        first_briefed,
        before_head,
        since,
        pack_digest: rdy_engine::readiness::assessment::pack_digest(st.pack()),
        config_digest: rdy_engine::readiness::assessment::config_digest(&st.gate_config()),
        live_policy,
        holds,
        shown,
    })
}

/// The card a journal entry shows the person saw (`seen_cards`), as Task 1's audit takes it.
fn shown_of(r: &Rdy, seen: Option<&(String, String)>) -> Result<Option<SeenCard>, Violation> {
    let fail = |what: String| Violation {
        rule: "mg10_rebuild",
        block: None,
        detail: what,
    };
    let Some((pref, card)) = seen else {
        return Ok(None);
    };
    Ok(Some(SeenCard {
        version: r
            .resolve_proposal(pref)
            .map_err(|e| fail(format!("resolve {pref}: {e}")))?
            .version,
        card_hash: Some(
            card.parse()
                .map_err(|_| fail(format!("card hash {card} is not a hash")))?,
        ),
    }))
}

/// Cuts the log of the closed project `project` at `cut`, the first frame of a batch: the
/// segments after `cut.segment` go, segment `cut.segment` ends at `cut.offset`, and the index
/// goes, so the next open rebuilds it from the cut log. The project is then the state the log
/// held just before that batch.
fn cut_log(project: &std::path::Path, cut: Location) {
    let store = project.join(".rdy").join(rdy_store::layout::STORE_DIR);
    let segments = store.join("segments");
    for e in std::fs::read_dir(&segments).expect("list the segments") {
        let p = e.expect("a segment entry").path();
        let Some(id) = p
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_prefix("seg-")?.strip_suffix(".log"))
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        if id > cut.segment {
            std::fs::remove_file(&p).expect("remove a segment after the cut");
        } else if id == cut.segment {
            std::fs::OpenOptions::new()
                .write(true)
                .open(&p)
                .and_then(|f| f.set_len(cut.offset))
                .expect("cut the segment");
        }
    }
    match std::fs::remove_file(store.join("index.redb")) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => panic!("remove the index: {e}"),
        _ => {}
    }
}

/// Writes back into the cut project `project` the payload of every frame before `cut` that a
/// removal whose apply the cut leaves out zeroed (`LogAt::zeroed`), from `kept`: the copy of the
/// log the real compact took before its removal request (`Outcome::pre_removal`). A frame never
/// moves and its header stays (D-4YRQ4), so each payload goes back at its own place; the
/// project's log is then, byte for byte, the log as it was just before `cut`. The count of
/// frames restored.
fn restore_removed(
    project: &std::path::Path,
    kept: &std::path::Path,
    log: &LogAt,
    cut: Location,
) -> usize {
    use std::io::{Read, Seek, SeekFrom, Write};
    let segments = project
        .join(".rdy")
        .join(rdy_store::layout::STORE_DIR)
        .join("segments");
    // a frame header: length, kind and hash (the store's `segment::HEADER`)
    let header: u64 = 4 + 1 + 32;
    let mut n = 0;
    for (loc, _) in log
        .zeroed
        .iter()
        .filter(|(l, apply)| *l < cut && !apply.is_some_and(|a| a < cut))
    {
        let at = rdy_store::segment::seg_path(&segments, loc.segment);
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&at)
            .expect("open a cut segment");
        let mut len = [0u8; 4];
        f.seek(SeekFrom::Start(loc.offset))
            .and_then(|_| f.read_exact(&mut len))
            .expect("read a removed frame's header");
        let mut payload = vec![0u8; u32::from_le_bytes(len) as usize];
        let mut k = std::fs::File::open(rdy_store::segment::seg_path(kept, loc.segment))
            .expect("open a kept segment");
        k.seek(SeekFrom::Start(loc.offset + header))
            .and_then(|_| k.read_exact(&mut payload))
            .expect("read a removed payload from the kept log");
        assert!(
            payload.iter().any(|b| *b != 0),
            "the kept log holds the frame at {loc:?} zeroed: it was copied after the removal"
        );
        f.seek(SeekFrom::Start(loc.offset + header))
            .and_then(|_| f.write_all(&payload))
            .expect("restore a removed payload");
        n += 1;
    }
    n
}

/// What `prefix_audits` found for one seed.
#[derive(Default)]
struct PrefixAudit {
    /// The drained project's landings after the seed: every one is to be audited.
    landings: usize,
    /// Landings audited on the state their log held just before them.
    audited: usize,
    /// Of those, the landings whose facts rebuilt from the log alone (`rebuilt_temporal`) equal
    /// Task 1's `TemporalFacts::read` on the held state.
    matched: usize,
    /// Landings whose log prefix holds a frame a later removal zeroed, audited on the cut log
    /// with those payloads written back from the kept copy (`restore_removed`).
    restored: usize,
    /// Landings whose log prefix holds a frame a later removal zeroed and that no kept copy
    /// restores (it cannot be opened).
    unreadable: Vec<String>,
    /// The landing tensions (`landing_tensions` of the approved version's changes) the blocking
    /// rule read on those states.
    tensions: usize,
    bad: Vec<String>,
}

/// Task 1's whole audit, `audit_m3`, of each landing of the drained project `dir` after the
/// seed, on a held `before`: the project with its log cut at the first frame of the landing's
/// batch (`cut_log`), which is the state the gate read, since a landing is one batch. `after` is
/// the drained project. A cut that keeps a frame a later removal zeroed gets that frame's payload
/// back from `kept`, the log the real compact copied before its removal request
/// (`restore_removed`). So the temporal rules read canon, the assessments, the answers and the
/// tension records of the landing's own time, and the landing audit applies its three rules:
/// `blocking_landed` (every landing tension open and blocking with every side in canon or the
/// landing), `conflict_landed` on the view's effective base then, and `alt_unchosen`. The cut
/// state's canon must be the ref history's head there (`LogAt::head_before`) and must not hold
/// the landing. The landings are cut latest first, on one copy. Without `kept`, a cut that keeps
/// a frame a later removal zeroed is not readable (the frame is an unauthorized hole there,
/// `LogAt::prefix_readable`); such a landing is listed in `unreadable`.
fn prefix_audits(
    dir: &std::path::Path,
    kept: Option<&std::path::Path>,
    seed_landings: &[String],
    journal: &[Entry],
) -> PrefixAudit {
    let seen = seen_cards(journal);
    let mut out = PrefixAudit::default();
    let (_keep_after, after_dir) = copy_of(dir);
    let after = Rdy::open(&after_dir).expect("open the drained project");
    let ast = after.read();
    let log = LogAt::read(&ast);
    let (landings, _) = canon_history(&ast, &log);
    let (_keep, cut) = copy_of(dir);
    for (h, rec, at) in landings.iter().rev() {
        let id = h.to_string();
        if seed_landings.contains(&id) {
            continue;
        }
        out.landings += 1;
        let Some(start) = log.batch_start(*at) else {
            out.bad
                .push(format!("landing {}: no batch holds it", h.short()));
            continue;
        };
        cut_log(&cut, start);
        if !log.prefix_readable(start) {
            let Some(kept) = kept else {
                out.unreadable.push(h.short());
                continue;
            };
            if restore_removed(&cut, kept, &log, start) == 0 {
                out.bad.push(format!(
                    "landing {}: its cut is not readable and restores no payload",
                    h.short()
                ));
            }
            out.restored += 1;
        }
        let before = match Rdy::open(&cut) {
            Ok(b) => b,
            Err(e) => {
                out.bad.push(format!(
                    "landing {}: the log cut before it does not open: {e}",
                    h.short()
                ));
                continue;
            }
        };
        let bst = before.read();
        if bst.canon_head() != log.head_before(*at) {
            out.bad.push(format!(
                "landing {}: the log before it holds canon {:?}, its ref history {:?}",
                h.short(),
                bst.canon_head().map(|c| c.short()),
                log.head_before(*at).map(|c| c.short())
            ));
        }
        if bst.reader().landing(h).is_ok() {
            out.bad
                .push(format!("landing {}: the log before it holds it", h.short()));
        }
        match shown_of(&after, seen.get(&id)) {
            Ok(shown) => {
                let obs = ObservedLanding { landing: *h, shown };
                out.bad.extend(
                    audit_m3(&bst, &ast, &obs)
                        .into_iter()
                        .map(|v| format!("landing {}: {}: {}", h.short(), v.rule, v.detail)),
                );
                // the reconstruction from the log alone reads what the held state holds
                let held = TemporalFacts::read(&bst, &ast, &obs);
                let rebuilt =
                    rebuilt_temporal(&after, &ast, &log, *h, rec.clone(), *at, seen.get(&id));
                match (held, rebuilt) {
                    (Ok(a), Ok(b)) if a == b => out.matched += 1,
                    (a, b) => out.bad.push(format!(
                        "landing {}: the facts rebuilt from the log are not the held state's: {b:#?} {a:#?}",
                        h.short()
                    )),
                }
            }
            Err(v) => out.bad.push(format!("{}: {}", v.rule, v.detail)),
        }
        // the version the person approved: its changes are what the gate judged (the landing's
        // own proposal record and its last change are written in the landing's batch)
        match rec.approved_version.map(|v| ast.reader().proposal(&v)) {
            Some(Ok(p)) => match bst.landing_tensions(&p.view, &p.changes) {
                Ok(ts) => out.tensions += ts.len(),
                Err(e) => out.bad.push(format!(
                    "landing {}: its landing tensions before it: {e}",
                    h.short()
                )),
            },
            Some(Err(e)) => out
                .bad
                .push(format!("landing {}: its approved version: {e}", h.short())),
            None => out.bad.push(format!(
                "landing {}: it names no approved version",
                h.short()
            )),
        }
        out.audited += 1;
        drop(bst);
        before.close().expect("close the cut copy");
    }
    drop(ast);
    after.close().expect("close the drained project");
    out
}

/// The landing each journaled `land` or `land_found` entry first names, with the base its view
/// read on just before it landed (the run reads it through the relay, `run::before_land`).
fn journaled_bases(journal: &[Entry]) -> BTreeMap<Hash, Option<Hash>> {
    let mut out = BTreeMap::new();
    for e in journal
        .iter()
        .filter(|e| (e.act == "land" && e.outcome == Met::Ok) || e.act == run::LAND_FOUND)
    {
        if let Some(l) = e
            .reply
            .as_ref()
            .and_then(|r| landing_fields(r)["landing"].as_str())
            && let Ok(h) = l.parse::<Hash>()
        {
            out.entry(h)
                .or_insert_with(|| e.base.as_deref().and_then(|b| b.parse().ok()));
        }
    }
    out
}

/// Task 1's audit for every landing the swarm made, of the run and of the drain:
/// - the durable M3 rules on the frozen copy and the drained project (`check_m3_in`), and each
///   landing after the seed an M3 landing;
/// - canon's history from the log's own `canon` ref updates (`canon_history`), never from a
///   landing record's `prev`: each landing set canon in its own batch, every landing frame was
///   canon once, and the chain of `prev`s is that history;
/// - the temporal rules (`check_temporal`) on the facts of each landing, every input read at the
///   landing's own log position (`rebuilt_temporal`, `holds_at`), never from the final state;
/// - Task 1's whole audit, `audit_m3`, on a held before state: the log cut just before the
///   landing's batch (`prefix_audits`). This applies the landing audit's complete blocking rule,
///   its conflict rule and its alternative rule to the state the gate read. There, the facts
///   rebuilt from the log alone must equal `TemporalFacts::read` on the held state. For a
///   landing before the run's removal the cut keeps a frame the removal zeroed later; its payload
///   comes back from the log the real compact copied before its removal request
///   (`Outcome::pre_removal`). Every drained landing after the seed is audited so;
/// - for every landing, the conflict rule on the base its view read just before it
///   (`journaled_bases`), the alternative rule on the choices of the landings before it, and the
///   run's own check of canon's blocking tensions just before and just after it (`run::after_land`,
///   the run's `violations`).
///
/// Each journaled card a person saw before a landing is the card and version that landing
/// records (`m3_card`). Negative witnesses: the facts read at the next landing's position fail
/// `m3_snapshot`, a record whose `prev` skips the canon head before it fails `m3_snapshot`, and
/// another card fails `m3_card`. The holds read at the log's end equal Task 1's `holds_of` on the
/// final state.
#[test]
fn mg10_every_landing_passes_the_m3_audit() {
    each(|seed, o| {
        assert!(
            o.violations.is_empty(),
            "RDY_QA_SWARM_SEEDS={seed}: the blocking rule at a landing: {:#?}",
            o.violations
        );
        let seen = seen_cards(&o.journal);
        let bases = journaled_bases(&o.journal);
        let mut audited = 0;
        let mut carded = 0;
        let mut snapshots = 0;
        let mut skips = 0;
        let mut replaced = 0;
        for (what, dir) in [
            ("the frozen copy", &o.frozen),
            ("the drained project", &o.dir),
        ] {
            let (_keep, copy) = copy_of(dir);
            let r = Rdy::open(&copy).unwrap_or_else(|e| panic!("open {what}: {e}"));
            let st = r.read();
            let durable = check_m3_in(&st);
            assert!(
                durable.is_empty(),
                "swarm seed {seed}: the M3 rules on {what}: {durable:#?}"
            );
            let facts = M3Facts::read(&st).unwrap_or_else(|e| panic!("{what}: {e:?}"));
            let m3: BTreeSet<String> = facts
                .landings
                .iter()
                .map(|l| l.landing.to_string())
                .collect();
            let log = LogAt::read(&st);
            replaced = replaced.max(log.replaced());
            let (landings, mut bad) = canon_history(&st, &log);
            let mut settled: BTreeMap<Hash, String> = BTreeMap::new();
            for (i, (h, rec, at)) in landings.iter().enumerate() {
                let (h, at) = (*h, *at);
                let id = h.to_string();
                let seeded = o.seed_landings.contains(&id);
                if !seeded {
                    if !m3.contains(&id) {
                        bad.push(format!("landing {} records no M3 field", h.short()));
                    }
                    let card = seen.get(&id);
                    carded += usize::from(card.is_some());
                    let rebuild =
                        |at, card| rebuilt_temporal(&r, &st, &log, h, rec.clone(), at, card);
                    match rebuild(at, card) {
                        Ok(t) => {
                            bad.extend(
                                check_temporal(&t)
                                    .into_iter()
                                    .map(|v| format!("{}: {}", v.rule, v.detail)),
                            );
                            // the card rule is not vacuous: another card than the one seen fails
                            if let Some(s) = &t.shown {
                                let mut other = t.clone();
                                other.shown = Some(SeenCard {
                                    version: s.version,
                                    card_hash: Some(Hash::of(b"another card")),
                                });
                                if !check_temporal(&other).iter().any(|v| v.rule == "m3_card") {
                                    bad.push(format!(
                                        "landing {}: a card the person did not see passes",
                                        h.short()
                                    ));
                                }
                            }
                        }
                        Err(v) => bad.push(format!("{}: {}", v.rule, v.detail)),
                    }
                    // the snapshot rule is not vacuous: the facts read at the next landing's log
                    // position are not the canon this landing read
                    if let Some((next, _, next_at)) = landings.get(i + 1) {
                        match rebuild(*next_at, None) {
                            Ok(t) if check_temporal(&t).iter().any(|v| v.rule == "m3_snapshot") => {
                                snapshots += 1
                            }
                            Ok(_) => bad.push(format!(
                                "landing {}: the facts read after landing {} still pass m3_snapshot",
                                h.short(),
                                next.short()
                            )),
                            Err(v) => bad.push(format!("{}: {}", v.rule, v.detail)),
                        }
                    }
                    // a landing whose prev skips the canon head before it fails m3_snapshot: the
                    // chain its records' prevs give leaves the skipped head out, and the log's
                    // canon history still names it
                    if i >= 2 {
                        let skipped = landings[i - 1].0;
                        let mut forged = rec.clone();
                        forged.prev = Some(landings[i - 2].0);
                        match rebuilt_temporal(&r, &st, &log, h, forged, at, None) {
                            Ok(t) if check_temporal(&t).iter().any(|v| v.rule == "m3_snapshot") => {
                                skips += 1
                            }
                            Ok(_) => bad.push(format!(
                                "landing {}: a record whose prev skips canon head {} passes m3_snapshot",
                                h.short(),
                                skipped.short()
                            )),
                            Err(v) => bad.push(format!("{}: {}", v.rule, v.detail)),
                        }
                    }
                    // the reading at a position agrees with Task 1's reader at the log's end
                    if let Some(v) = rec.approved_version {
                        let head = landings.last().map(|(l, _, _)| *l);
                        match (holds_at(&st, &log, &v, st.seq(), head), holds_of(&st, &v)) {
                            (Ok(a), Ok(b)) if a == b => {}
                            (a, b) => bad.push(format!(
                                "landing {}: the holds read at the log's end {a:?} are not holds_of's {b:?}",
                                h.short()
                            )),
                        }
                    }
                    // the landing audit's conflict and alternative rules, on the evidence of the
                    // landing's own time
                    let landed =
                        landed_of(&r, &h).unwrap_or_else(|err| panic!("landing {h}: {err}"));
                    bad.extend(
                        alt_unchosen(&st, &landed, &settled)
                            .into_iter()
                            .map(|v| format!("{}: {}", v.rule, v.detail)),
                    );
                    match bases.get(&h) {
                        Some(Some(base)) => bad.extend(
                            conflicts_at(&st, &landed, Some(*base))
                                .into_iter()
                                .map(|v| format!("{}: {}", v.rule, v.detail)),
                        ),
                        _ => bad.push(format!(
                            "landing {}: no journaled land entry names it with the base its view read on",
                            h.short()
                        )),
                    }
                    audited += 1;
                }
                if let Some(c) = &rec.chosen {
                    settled.insert(c.set, c.member.clone());
                }
            }
            drop(st);
            r.close().expect("close the copy");
            assert!(
                bad.is_empty(),
                "RDY_QA_SWARM_SEEDS={seed}: the M3 audit on {what}: {bad:#?}"
            );
        }
        let held = prefix_audits(
            &o.dir,
            o.pre_removal.as_deref(),
            &o.seed_landings,
            &o.journal,
        );
        assert!(
            held.bad.is_empty(),
            "RDY_QA_SWARM_SEEDS={seed}: Task 1's audit on the log before each landing: {:#?}",
            held.bad
        );
        eprintln!(
            "swarm seed {seed}: {audited} landings audited, {carded} with a journaled card, {snapshots} snapshot negatives, {skips} skipped-prev negatives, {replaced} answers replaced by a later answer; audit_m3 on the log before the landing: {} of {} drained landings ({} before the removal, on the kept log), {} with equal rebuilt facts, {} landing tensions read, {} not readable: {:?}",
            held.audited,
            held.landings,
            held.restored,
            held.matched,
            held.tensions,
            held.unreadable.len(),
            held.unreadable
        );
        assert!(audited > 0, "swarm seed {seed}: no landing was audited");
        assert!(
            snapshots > 0,
            "swarm seed {seed}: no landing's later facts failed m3_snapshot"
        );
        // complete coverage: every landing after the seed, the ones before the removal included
        assert_eq!(
            held.audited, held.landings,
            "swarm seed {seed}: {} of {} drained landings audited on the log before them (not readable: {:?})",
            held.audited, held.landings, held.unreadable
        );
        assert!(
            held.audited > 0,
            "swarm seed {seed}: no landing was audited on the log before it"
        );
        assert!(
            skips > 0,
            "swarm seed {seed}: no record whose prev skips a canon head failed m3_snapshot"
        );
        assert!(
            carded > 0,
            "swarm seed {seed}: no landing had a journaled card"
        );
    });
}

/// Each version a card showed before the kill (epoch 0) as `(ref, card hash, entry)`.
fn shown_before_kill(o: &Outcome) -> Vec<(String, String, &Entry)> {
    o.before_freeze()
        .iter()
        .filter(|e| e.act == LAND_CARD && e.epoch == 0 && e.outcome == Met::Ok)
        .filter_map(|e| {
            let s = &e.reply.as_ref()?["shown"];
            Some((
                s["ref"].as_str()?.to_string(),
                s["card_hash"].as_str()?.to_string(),
                e,
            ))
        })
        .collect()
}

/// Each landing the run acknowledged before the freeze, with the entry that acknowledged it: an
/// M3 route attempt's records, a person's or an agent's acknowledged `land` and an acknowledged
/// UI answer. Every one of them records an M3 assessment or policy (`mg10_every_landing_...`).
fn acknowledged_landings(o: &Outcome) -> Vec<(&Entry, String)> {
    let mut out = vec![];
    for e in o.before_freeze() {
        out.extend(route_records(e).into_iter().map(|l| (e, l)));
        if e.outcome != Met::Ok {
            continue;
        }
        let landing = match e.act {
            "land" => e
                .reply
                .as_ref()
                .and_then(|r| landing_fields(r)["landing"].as_str())
                .map(str::to_string),
            "ui_answer" => e.ui.as_ref().and_then(|u| u.landing.clone()),
            _ => None,
        };
        out.extend(landing.map(|l| (e, l)));
    }
    out
}

/// Plan Task 10: what an M3 route acknowledged survives the kill. Each landing an acknowledged
/// attempt, approve or UI answer names (`acknowledged_landings`; the M3 routes run after the
/// kill, so the landings acknowledged before it are the person's and the agents') is in canon's
/// chain of the frozen copy (the state after the reconnect), the
/// drained project and a reindexed copy of each. Each version a card showed before the kill
/// still resolves after it, and one that did not land, has no newer version and whose view is
/// open is still an item of the person's queue (or a not-ready version) after the reconnect.
#[test]
fn mg10_acknowledged_m3_records_survive_the_kill() {
    let before_kill = std::cell::Cell::new(0);
    let shown = std::cell::Cell::new(0);
    each(|seed, o| {
        let acked = acknowledged_landings(o);
        assert!(
            acked.iter().any(|(e, _)| e.route.is_some()),
            "swarm seed {seed}: no M3 route acknowledged a record"
        );
        assert!(
            acked.iter().any(|(e, _)| e.epoch == 0),
            "swarm seed {seed}: no landing was acknowledged before the kill"
        );
        for (what, dir, reindex) in [
            ("the frozen copy", &o.frozen, false),
            ("the frozen copy, reindexed", &o.frozen, true),
            ("the drained project", &o.dir, false),
            ("the drained project, reindexed", &o.dir, true),
        ] {
            let (_keep, copy) = copy_of(dir);
            let mut r = Rdy::open(&copy).unwrap_or_else(|e| panic!("open {what}: {e}"));
            if reindex {
                r.reindex()
                    .unwrap_or_else(|e| panic!("reindex {what}: {e}"));
            }
            let st = r.read();
            let held: BTreeSet<String> = chain(&st).iter().map(|(h, _)| h.to_string()).collect();
            let lost: Vec<String> = acked
                .iter()
                .filter(|(_, l)| !held.contains(l))
                .map(|(e, l)| format!("landing {l}: {}", replay(seed, e)))
                .collect();
            assert!(
                lost.is_empty(),
                "swarm seed {seed}: {what} lost acknowledged M3 landings: {lost:#?}"
            );
            if what != "the frozen copy" {
                drop(st);
                r.close().expect("close the copy");
                continue;
            }
            // the queue after the reconnect keeps each card shown before the kill that is
            // still open
            let (items, not_ready, omitted) = queue_of(&st);
            let landed: BTreeSet<Hash> = chain(&st)
                .iter()
                .filter_map(|(_, rec)| rec.approved_version)
                .collect();
            let mut missing = vec![];
            for (pref, _card, e) in shown_before_kill(o) {
                shown.set(shown.get() + 1);
                let Ok(p) = r.resolve_proposal(&pref) else {
                    missing.push(format!("{pref} no longer resolves: {}", replay(seed, e)));
                    continue;
                };
                let stable = pref.split('@').next().unwrap_or(&pref);
                let latest = r.resolve_proposal(stable).expect("the latest version");
                let view = st.reader().proposal(&p.version).expect("the version").view;
                let closed = st.view_show(&view).map_or(true, |v| v.head.closed);
                if landed.contains(&p.version) || latest.n != p.n || closed || omitted > 0 {
                    continue;
                }
                let queued = items.iter().any(|(r, _)| *r == pref) || not_ready.contains(&pref);
                if !queued {
                    missing.push(format!(
                        "{pref} is open and unlanded but not in the queue after the reconnect: {}",
                        replay(seed, e)
                    ));
                }
            }
            drop(st);
            r.close().expect("close the copy");
            assert!(missing.is_empty(), "swarm seed {seed}: {missing:#?}");
        }
        before_kill.set(before_kill.get() + acked.iter().filter(|(e, _)| e.epoch == 0).count());
    });
    eprintln!(
        "swarm seeds: {} landings acknowledged before the kill, {} cards shown before it",
        before_kill.get(),
        shown.get()
    );
}

/// What the reindex and the dump must keep of a closed store: the M3 projection of canon's
/// landings and the person's queue.
#[derive(Debug, PartialEq)]
struct Kept {
    facts: M3Facts,
    queue: (Vec<(String, String)>, Vec<String>, usize),
}

fn kept(r: &Rdy) -> Kept {
    let st = r.read();
    Kept {
        facts: M3Facts::read(&st).unwrap_or_else(|e| panic!("the M3 facts: {e:?}")),
        queue: queue_of(&st),
    }
}

/// Plan Task 10: a reindex keeps the M3 records and the pending items. On a copy of the frozen
/// copy and of the drained project, the M3 projection of every landing (records, assessments,
/// changes, seals) and the person's queue (each item's ref and card hash, the not-ready
/// versions) are the same before the reindex, after it, after a reopen, and in a project loaded
/// from a full dump; the reindex audit passes.
#[test]
fn mg10_reindex_preserves_m3_records_and_pending_items() {
    each(|seed, o| {
        for (what, dir) in [
            ("the frozen copy", &o.frozen),
            ("the drained project", &o.dir),
        ] {
            let (_keep, copy) = copy_of(dir);
            let mut r = Rdy::open(&copy).unwrap_or_else(|e| panic!("open {what}: {e}"));
            let first = kept(&r);
            assert!(
                !first.facts.landings.is_empty(),
                "swarm seed {seed}: {what} holds no M3 landing"
            );
            r.reindex()
                .unwrap_or_else(|e| panic!("reindex {what}: {e}"));
            assert_eq!(
                kept(&r),
                first,
                "swarm seed {seed}: {what} after the reindex"
            );
            let d = tempfile::tempdir().expect("a dump dir");
            let file = d.path().join("all.rdydump");
            r.read()
                .dump(&file)
                .unwrap_or_else(|e| panic!("dump {what}: {e}"));
            r.close().expect("close the copy");
            let r = Rdy::open(&copy).unwrap_or_else(|e| panic!("reopen {what}: {e}"));
            assert_eq!(kept(&r), first, "swarm seed {seed}: {what} after a reopen");
            r.close().expect("close the copy");
            let loaded = d.path().join("loaded");
            rdy_engine::load::load(&file, &loaded)
                .unwrap_or_else(|e| panic!("load the dump of {what}: {e}"));
            let r = Rdy::open(&loaded).unwrap_or_else(|e| panic!("open the load of {what}: {e}"));
            assert_eq!(
                kept(&r),
                first,
                "swarm seed {seed}: {what} loaded from its dump"
            );
            r.close().expect("close the load");
            let reindexed = reindex_reproduces(&copy);
            assert!(
                reindexed.is_ok(),
                "swarm seed {seed}: the reindex audit on {what}: {reindexed:#?}"
            );
        }
    });
}

/// Each assessment a landing names, by landing id: its id and its method.
fn assessments(st: &ReadState) -> BTreeMap<String, (Hash, u32)> {
    chain(st)
        .into_iter()
        .filter_map(|(h, rec)| {
            let id = rec.assessment?;
            let a = st.reader().assessment(&id).ok()?;
            Some((h.to_string(), (id, a.method)))
        })
        .collect()
}

/// Plan Task 10 (D-33JR0, D-KVQ7F): a stored assessment is the original measurement with its
/// original method. Each assessment a landing names decodes to the bytes of its own id
/// (`assessment_id`), after the swarm's real compact and in every later state; the drain's new
/// versions and their new assessments replace none of them: every landing of the frozen copy
/// names the same assessment, with the same method, in the drained project, after its reindex
/// and in a project loaded from its dump.
#[test]
fn mg10_original_assessments_survive_new_methods_and_compact() {
    let compacted = std::cell::Cell::new(0);
    each(|seed, o| {
        let compact = o
            .before_freeze()
            .iter()
            .any(|e| e.act == "m3_compact_real" && e.outcome == Met::Ok);
        compacted.set(compacted.get() + usize::from(compact));
        let read = |dir: &std::path::Path, reindex: bool| {
            let (_keep, copy) = copy_of(dir);
            let mut r = Rdy::open(&copy).expect("open a copy");
            if reindex {
                r.reindex().expect("reindex a copy");
            }
            let st = r.read();
            let facts = M3Facts::read(&st).unwrap_or_else(|e| panic!("the M3 facts: {e:?}"));
            for l in &facts.landings {
                if let (Some(id), Some(a)) = (l.record.assessment, &l.assessment) {
                    assert_eq!(
                        assessment_id(a),
                        id,
                        "swarm seed {seed}: landing {}'s assessment {} reads other bytes",
                        l.landing.short(),
                        id.short()
                    );
                }
            }
            let got = assessments(&st);
            drop(st);
            r.close().expect("close a copy");
            got
        };
        let frozen = read(&o.frozen, false);
        assert!(
            !frozen.is_empty(),
            "swarm seed {seed}: no landing of the run names an assessment"
        );
        for (what, got) in [
            ("the drained project", read(&o.dir, false)),
            ("the drained project, reindexed", read(&o.dir, true)),
        ] {
            for (l, want) in &frozen {
                assert_eq!(
                    got.get(l),
                    Some(want),
                    "swarm seed {seed}: landing {l}'s assessment in {what}"
                );
            }
        }
        let methods: BTreeSet<u32> = frozen.values().map(|(_, m)| *m).collect();
        eprintln!(
            "swarm seed {seed}: {} landed assessments, methods {methods:?}, compacted {compact}",
            frozen.len()
        );
    });
    if std::env::var("RDY_QA_SWARM_SEEDS").is_err() {
        assert!(
            compacted.get() > 0,
            "no seed ran the real compact before the freeze"
        );
    }
}

/// The text of the subject claim the real compact's removal names in `seed`'s run (the claim of
/// its slot: `project` writes "The blob-notes attachment number <n> stays as written.").
fn removed_text(seed: u64) -> Option<String> {
    let sc = m3_scenarios(&Config::for_run(seed))
        .into_iter()
        .find(|s| s.route == Route::RealCompact)?;
    Some(format!(
        "The blob-notes attachment number {} stays as written.",
        sc.slot + 1
    ))
}

/// Plan Task 10: the profile outputs the swarm produced pass the disclosure and link checks. Each
/// seed's publication route checked canon and a held landing under the org profile and wrote
/// the Markdown, HTML and site outputs of that landing; each output is there and not empty,
/// every local link and anchor of it resolves (scanned again here, from the files), and an
/// output of a landing at or after the removal's landing holds no byte of the removed text.
#[test]
fn mg10_profile_outputs_pass_disclosure_and_link_checks() {
    let after_removal = std::cell::Cell::new(0);
    each(|seed, o| {
        let run = o.before_freeze();
        let outputs: Vec<(usize, &Entry)> = run
            .iter()
            .enumerate()
            .filter(|(_, e)| e.act == "m3_publication_output")
            .collect();
        let checks: Vec<&Entry> = run
            .iter()
            .filter(|e| e.act == "m3_publication_check")
            .collect();
        if outputs.is_empty() && checks.is_empty() {
            // the kill cut the route before its first call
            return;
        }
        for e in &checks {
            if let (Met::Ok, Some(v)) = (&e.outcome, &e.reply) {
                let status = &v["references"]["status"];
                assert!(
                    status == "clear" || status == "warning",
                    "an org check reads {status}: {}",
                    replay(seed, e)
                );
            }
        }
        // the removal's landing, if the run made it before the outputs
        let removal = run
            .iter()
            .position(|e| e.act == "m3_blob_remove" && !route_records(e).is_empty());
        let canary = removed_text(seed);
        for (at, e) in outputs {
            let Some(a) = &e.route else { continue };
            let Reply::Acknowledged { ack, .. } = &a.reply else {
                continue;
            };
            let dir = std::path::Path::new(ack);
            assert!(
                dir.exists(),
                "{} is gone: {}",
                dir.display(),
                replay(seed, e)
            );
            let files = if dir.is_dir() {
                tree(dir)
            } else {
                let name = dir.file_name().unwrap().to_string_lossy().into_owned();
                BTreeMap::from([(name, std::fs::read(dir).unwrap())])
            };
            assert!(
                !files.is_empty(),
                "{} is empty: {}",
                dir.display(),
                replay(seed, e)
            );
            let scan = scan_local_links(&files);
            assert!(
                scan.broken.is_empty(),
                "broken local links {:?}: {}",
                scan.broken,
                replay(seed, e)
            );
            assert_eq!(
                scan.checked,
                a.counts.links_checked,
                "the scan differs from the run's: {}",
                replay(seed, e)
            );
            if let (Some(r), Some(text)) = (removal, &canary)
                && r < at
            {
                after_removal.set(after_removal.get() + 1);
                let held: Vec<&String> = files
                    .iter()
                    .filter(|(_, b)| b.windows(text.len()).any(|w| w == text.as_bytes()))
                    .map(|(n, _)| n)
                    .collect();
                assert!(
                    held.is_empty(),
                    "outputs {held:?} hold the removed text {text:?}: {}",
                    replay(seed, e)
                );
            }
        }
        // the dirs the run kept are the ones the journal names
        for d in &o.m3_outputs {
            assert!(d.is_dir(), "swarm seed {seed}: {} is gone", d.display());
        }
    });
    eprintln!(
        "swarm seeds: {} outputs scanned for the removed text after its removal",
        after_removal.get()
    );
}
