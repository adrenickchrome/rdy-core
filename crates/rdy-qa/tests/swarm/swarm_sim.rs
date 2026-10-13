//! sg5: the swarm simulation (spec §15.3; ledger O1-O5, O9). Eight scripted agents and one
//! person act on one daemon, through MCP first. The daemon is killed once and restarted. Then no
//! actor runs (the freeze), the project is copied, and the drain lands every live view.
//!
//! Every assertion reads the final state or the journal, never an order of events. One run per
//! seed is shared by every test of this file: seed 1 in the fast gate, seeds 1, 2 and 3 in the
//! deep gate (`RDY_QA_DEEP=1`). Every failure names its seed, so a person can run it again.
use rdy_engine::inbox::read::parse_cursor;
use rdy_engine::tensions::tension_label;
use rdy_engine::{EventKind, Rdy};
use rdy_model::Hash;
use rdy_qa::fixtures::deep;
use rdy_qa::invariants::check_all;
use rdy_qa::landing_audit::{conflicts_at, landed_of};
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

/// The runs of this file: one per seed, made once.
fn runs() -> &'static [Run] {
    static RUNS: OnceLock<Vec<Run>> = OnceLock::new();
    RUNS.get_or_init(|| {
        let seeds: Vec<u64> = if deep() { vec![1, 2, 3] } else { vec![1] };
        seeds
            .into_iter()
            .map(|seed| (seed, seeded(seed, || run::run(&Config::for_run(seed)))))
            .collect()
    })
}

/// Runs `check` on every seed's outcome. A run that failed fails here with its own text.
fn each(check: impl Fn(u64, &Outcome)) {
    for (seed, r) in runs() {
        let o = match r {
            Ok(o) => o,
            Err(text) => panic!("{text}"),
        };
        if let Err(text) = seeded(*seed, || check(*seed, o)) {
            resume_unwind(Box::new(text));
        }
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

fn ok_lands(o: &Outcome) -> impl Iterator<Item = &Entry> {
    o.journal
        .iter()
        .filter(|e| e.act == "land" && e.outcome == Met::Ok)
}

#[test]
fn sg5_the_run_exercised_every_act() {
    each(|seed, o| {
        let ok = |act: &str| {
            o.journal
                .iter()
                .any(|e| e.act == act && e.outcome == Met::Ok)
        };
        let mut missing: Vec<&str> = Act::ALL_NAMES
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
            // the daemon never acknowledges an unbriefed stable write: its refusal is the act
            .filter(|a| *a != "write_no_brief" && !ok(a))
            .collect();
        let refused_unbriefed = o.journal.iter().any(|e| {
            e.act == "write_no_brief" && e.outcome == Met::Refused("E_BRIEF_REQUIRED".into())
        });
        if !refused_unbriefed {
            missing.push("write_no_brief");
        }
        assert!(
            missing.is_empty(),
            "swarm seed {seed}: no acknowledged act of {missing:?}"
        );
        assert!(
            o.journal.iter().any(|e| e.outcome == Met::Lost),
            "swarm seed {seed}: the kill cut no call"
        );
        assert!(
            o.landings.len() >= 4,
            "swarm seed {seed}: {} landings, fewer than 4",
            o.landings.len()
        );
        let chose = ok_lands(o).any(|e| {
            e.reply
                .as_ref()
                .is_some_and(|r| r.get("chosen").is_some_and(|c| !c.is_null()))
        });
        assert!(
            chose,
            "swarm seed {seed}: no landing chose a member of an alternative set"
        );
        let logged = logged_kinds(o);
        let mut kinds: BTreeSet<String> = o
            .reported
            .values()
            .filter_map(|t| t["kind"].as_str().map(str::to_string))
            .collect();
        kinds.extend(
            o.journal
                .iter()
                .flat_map(|e| e.seen.iter())
                .filter_map(|l| logged.get(l).cloned()),
        );
        for kind in ["edit_collision", "premise_shift"] {
            assert!(
                kinds.contains(kind),
                "swarm seed {seed}: no {kind} was reported or seen: {kinds:?}"
            );
        }
        let opened = o.journal.iter().any(|e| {
            e.inbox
                .as_ref()
                .is_some_and(|r| r.entries.iter().any(|x| x["event"] == "tension_opened"))
        });
        assert!(
            opened,
            "swarm seed {seed}: no inbox read held a tension_opened entry"
        );
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
                unreported.push(format!("{a} and {b} on {block}"));
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

#[test]
fn sg5_no_landing_chose_no_alternative_or_crossed_a_conflict() {
    each(|seed, o| {
        let (_keep, copy) = copy_of(&o.dir);
        let r = Rdy::open(&copy).expect("open the drained copy");
        let st = r.read();
        let mut bad = vec![];
        for e in ok_lands(o) {
            let reply = e.reply.as_ref().expect("a land reply");
            let landing: Hash = reply["landing"]
                .as_str()
                .and_then(|l| l.parse().ok())
                .unwrap_or_else(|| panic!("a landing id in {reply}"));
            let landed =
                landed_of(&r, &landing).unwrap_or_else(|err| panic!("landing {landing}: {err}"));
            for c in &landed.changes {
                let ch = st
                    .reader()
                    .change(c)
                    .unwrap_or_else(|err| panic!("change {c}: {err}"));
                if let Some(choice) = &ch.links.alternative
                    && landed.alternative != Some(choice.set)
                {
                    bad.push(format!(
                        "landing {landing}: change {} is bound to set {}, and the landing chose {:?}",
                        c.short(),
                        choice.set.short(),
                        landed.alternative.map(|s| s.short())
                    ));
                }
            }
            // the conflict rule on the base the view read on when it landed (ruling task-1.1)
            let base = e.base.as_deref().map(|b| {
                b.parse::<Hash>()
                    .unwrap_or_else(|err| panic!("base {b}: {err}"))
            });
            bad.extend(
                conflicts_at(&st, &landed, base)
                    .into_iter()
                    .map(|v| v.detail),
            );
        }
        drop(st);
        r.close().expect("close the drained copy");
        assert!(bad.is_empty(), "swarm seed {seed}: {bad:#?}");
    });
}

#[test]
fn sg5_every_live_view_drains_without_a_dismissal() {
    each(|seed, o| {
        let (_keep, copy) = copy_of(&o.dir);
        let r = Rdy::open(&copy).expect("open the drained copy");
        let st = r.read();
        let mut live = vec![];
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
            // landed: the landing rebased it and dropped every change it landed
            if !head.closed && !st.included(&name).is_empty() {
                live.push(format!("{name} ({} changes)", st.included(&name).len()));
            }
        }
        drop(st);
        r.close().expect("close the drained copy");
        assert!(agent_views > 0, "swarm seed {seed}: no agent view");
        assert!(
            live.is_empty(),
            "swarm seed {seed}: views neither landed nor closed after the drain: {live:?}"
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
    assert!(
        matches!(&refused.outcome, Met::Refused(c) if c == "E_CONFLICT_UNRESOLVED" || c == "E_BLOCKING_TENSION"),
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

/// Deep seed 1: an agent's view that includes a change whose member later lost its set is refused
/// at propose (`E_ALT_CHOSEN`), and a queued proposal whose change joined a set after it was
/// proposed is refused at land (`E_ALT_MEMBER`). Both are the product's answers to the run, not
/// faults, so the closed table holds them.
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
