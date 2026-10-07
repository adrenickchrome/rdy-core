//! The perf-regression tier: a fixed workload, per-machine baselines, regression verdicts.
use crate::edit_loop::{brief_of, write_cited};
use crate::fixtures::{approver, land, write, writer};
use rdy_daemon::{Attempt, Client, Hello, PROTO_VERSION, Proto, RunDir, ServeOptions, ServeRun};
use rdy_engine::brief::Coverage;
use rdy_engine::consequence::ConsequenceKind;
use rdy_engine::{Materialized, Rdy, Scope, now_ms, testkit};
use rdy_model::{Block, BlockId, Hash, Op};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Measure {
    pub op: String,
    pub median_ms: f64,
    pub p95_ms: f64,
    pub runs: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Baseline {
    pub machine: String,
    pub ops: BTreeMap<String, Measure>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Verdict {
    pub op: String,
    pub median_ms: f64,
    pub p95_ms: f64,
    pub baseline_ms: Option<f64>,
    pub delta_pct: Option<f64>,
    pub pass: bool,
    pub spec_target: Option<String>,
}

pub const FLOOR_MS: f64 = 2.0;

/// The note a `_50k` op carries: its budget is the typical tier's, applied at 50k blocks.
const LARGE_TIER_NOTE: &str = " (large tier: budget at 50k, DECISIONS-rdy-m2 §7)";

fn spec_target(op: &str) -> Option<String> {
    let (base, large) = match op.strip_suffix("_50k") {
        Some(b) => (b, true),
        None => (op, false),
    };
    let target = match base {
        o if o.starts_with("write") || o.starts_with("propose") => {
            "< 5 ms p99 on Linux NVMe; one durable flush + < 1 ms on macOS (spec 11, restated per hardware class)"
        }
        "brief_doc" => "< 5 ms p99 at 10k with the view's graph carried (the read budget, spec 11)",
        "context" | "impact" | "version_position" | "resolve_path" | "tree_full" => {
            "< 5 ms p99 (spec 11)"
        }
        "version_position_cached" => "< 10 us p99 on the isolated Linux bench (vpos1)",
        "tree_full_memo" => {
            "< 5 ms p99 at 1k folders / 10k docs, memoised per canon head (M2 spec 15.1)"
        }
        "view_materialize_cold" => "< 50 ms cold (M2 spec 15.1)",
        "view_materialize_incremental" => "< 1 ms incremental (M2 spec 15.1)",
        "view_materialize_after_rebase" => {
            "< 50 ms cold, an unpinned view after the tier's landings rebased it (M2 spec 15.1)"
        }
        "include_200" => {
            "< 50 ms: 200 changes included into a fresh view, store write included (M2 spec 15.1 gives no figure for a bulk include; read against the cold materialization figure)"
        }
        "exclude_cold" => {
            "< 50 ms: one exclude from a 200-change view, then the read that rebuilds it (M2 spec 15.1: exclude counts as cold)"
        }
        "view_cold" => "< 50 ms cold, a fresh 200-change view (M2 spec 15.1)",
        "view_incremental" => {
            "< 1 ms incremental: the read of a 200-change view right after one write seeded it (M2 spec 15.1)"
        }
        "land_rebase_50" => {
            "< 100 ms p99, one landing with 50 non-empty unpinned views rebasing, 5 of them on the landed blocks (M2 spec 15.1)"
        }
        "brief_inflight_50" => {
            "< 5 ms p99 at 10k with 50 live views (the read budget, spec 11; amendment §4.5 item 1)"
        }
        "view_consequences_200" => {
            "no spec target: the consequences of a 200-change include, on the read pool (recorded for M2c3; read against the < 50 ms cold figure)"
        }
        "landing_consequences" => {
            "no spec target: a proposal's consequences for canon, on the read pool (recorded for M2c3)"
        }
        "view_ls_50" => {
            "no spec target: view_list(all) over 50 open views plus the closed ones, cold (recorded for M2c)"
        }
        "render" => "< 2 s full render at 100k (spec 11)",
        "cold_open" => "< 2 s at 100k (spec 11)",
        "cold_open_lagging" => "< 2 s at 100k, index catch-up included (M2 spec 15.1)",
        "new_doc_single" => "in-process comparator for daemon_write_single (no spec target)",
        "daemon_write_single" => {
            "< 5 ms p99 on Linux NVMe; one durable flush plus < 1 ms on macOS (DECISIONS-rdy-m2 §1 Q1)"
        }
        "daemon_cold_start" => "< 2 s at 100k (M2 spec 15.1)",
        "code_index_build" => {
            "a full rebuild in seconds (FW-C6): 2,000 commits, 400 files; recorded, no spec budget"
        }
        "brief_code" => {
            "recorded, no spec budget: one git rev-parse per call plus a briefing over the file's anchors (warm index)"
        }
        "history_100" | "last_changes_doc" | "log_page_20" => "< 5 ms p99 (reads, spec 11)",
        "log_full" => "recorded: 2000 landings, no spec target",
        _ => return None,
    };
    Some(if large {
        format!("{target}{LARGE_TIER_NOTE}")
    } else {
        target.to_string()
    })
}

/// The ops that end in a durable flush, so their medians move with the disk: they get the wider margin.
fn fsync_bound(op: &str) -> bool {
    op.starts_with("write")
        || op.starts_with("propose")
        || op.starts_with("new_doc")
        || op.starts_with("daemon_write")
        || op.starts_with("include_")
        || op.starts_with("exclude_")
        || op.starts_with("land_rebase")
}

pub fn compare(baseline: Option<&Baseline>, measured: &[Measure]) -> Vec<Verdict> {
    measured
        .iter()
        .map(|m| {
            let base = baseline.and_then(|b| b.ops.get(&m.op)).map(|b| b.median_ms);
            let margin = if fsync_bound(&m.op) { 0.50 } else { 0.25 };
            let pass = match base {
                None => true,
                Some(b) => !(m.median_ms > b * (1.0 + margin) && m.median_ms - b > FLOOR_MS),
            };
            Verdict {
                op: m.op.clone(),
                median_ms: m.median_ms,
                p95_ms: m.p95_ms,
                baseline_ms: base,
                delta_pct: base
                    .filter(|b| *b > 0.0)
                    .map(|b| (m.median_ms - b) / b * 100.0),
                pass,
                spec_target: spec_target(&m.op),
            }
        })
        .collect()
}

/// Suffix of the large tier's ops (about 50k blocks, `RDY_QA_DEEP=1` only).
pub const LARGE_SUFFIX: &str = "_50k";

/// The baseline without its large-tier ops: what a typical run is compared against, so the
/// absence of the `_50k` ops is not reported as drift.
pub fn typical_only(base: &Baseline) -> Baseline {
    Baseline {
        machine: base.machine.clone(),
        ops: base
            .ops
            .iter()
            .filter(|(op, _)| !op.ends_with(LARGE_SUFFIX))
            .map(|(op, m)| (op.clone(), m.clone()))
            .collect(),
    }
}

/// The class baseline after a record or rebaseline: the measured ops replace their entries and
/// every other op is kept, so a typical-tier rebaseline never deletes the `_50k` baseline.
pub fn merge_baseline(
    existing: Option<&Baseline>,
    machine: &str,
    measured: &[Measure],
) -> Baseline {
    let mut ops = existing.map(|b| b.ops.clone()).unwrap_or_default();
    for m in measured {
        ops.insert(m.op.clone(), m.clone());
    }
    Baseline {
        machine: machine.to_string(),
        ops,
    }
}

fn cmd(program: &str, args: &[&str]) -> Option<String> {
    let o = std::process::Command::new(program)
        .args(args)
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// `"<os> / <cpu> / <storage>"`; never a hostname. Anything unreadable is `unknown`.
pub fn machine_class() -> String {
    let (os, cpu, storage) = if cfg!(target_os = "macos") {
        (
            cmd("sw_vers", &["-productVersion"]).map(|v| format!("macOS {v}")),
            cmd("sysctl", &["-n", "machdep.cpu.brand_string"]),
            cmd("diskutil", &["info", "/"]).and_then(|s| {
                s.lines()
                    .find(|l| l.contains("File System Personality"))
                    .and_then(|l| l.split(':').nth(1))
                    .map(|v| v.trim().to_string())
            }),
        )
    } else {
        (
            std::fs::read_to_string("/etc/os-release")
                .ok()
                .and_then(|s| {
                    s.lines()
                        .find(|l| l.starts_with("PRETTY_NAME="))
                        .map(|l| l[12..].trim_matches('"').to_string())
                }),
            std::fs::read_to_string("/proc/cpuinfo").ok().and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("model name"))
                    .and_then(|l| l.split(':').nth(1))
                    .map(|v| v.trim().to_string())
            }),
            cmd("findmnt", &["-n", "-o", "FSTYPE", "-T", "."]),
        )
    };
    let u = |x: Option<String>| {
        x.map(|s| s.replace(" / ", " "))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".into())
    };
    format!("{} / {} / {}", u(os), u(cpu), u(storage))
}

fn measure(op: &str, runs: usize, mut f: impl FnMut(usize)) -> Measure {
    measure_timed(op, runs, |i| {
        let s = Instant::now();
        f(i);
        s.elapsed()
    })
}

/// Like [`measure`], but `f` times its own run (so setup can stay outside the timed region).
fn measure_timed(op: &str, runs: usize, mut f: impl FnMut(usize) -> Duration) -> Measure {
    let mut t: Vec<f64> = (0..runs).map(|i| f(i).as_secs_f64() * 1000.0).collect();
    t.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Measure {
        op: op.into(),
        median_ms: t[t.len() / 2],
        p95_ms: t[((t.len() * 95) / 100).min(t.len() - 1)],
        runs,
    }
}

/// Runs `f` with the engine's canon and view caches dropped first, outside the timed region:
/// the materialization and graph rebuild that `f` triggers is what gets timed. This is the
/// agent's real pattern (every write invalidates, so the next read rebuilds); warm reads are
/// cache hits that no regression could move past the floor.
fn cold<T>(r: &mut Rdy, f: impl FnOnce(&Rdy) -> T) -> (T, Duration) {
    r.invalidate();
    let s = Instant::now();
    let out = f(r);
    (out, s.elapsed())
}

/// Landings in the `version_position` fixture. With no version tag, `version_position` walks
/// every one of them, so this is what makes the op long enough to measure.
pub const DEEP_LANDINGS: usize = 2000;

/// A project whose canon is [`DEEP_LANDINGS`] landings deep and carries no version tag.
fn deep_history(parent: &std::path::Path) -> Rdy {
    let proj = parent.join("deep");
    std::fs::create_dir_all(&proj).expect("mkdir");
    let mut r = Rdy::init(&proj, "rdy-standard").expect("init");
    for i in 0..DEEP_LANDINGS {
        let view = format!("d{i}");
        write(
            &mut r,
            &view,
            json!([{"op": "create", "type": "doc", "fields": {"name": format!("deep-{i}"), "title": format!("Deep {i}"), "doc_type": "constraints"}}]),
        )
        .expect("deep write");
        land(&mut r, &view).expect("deep land");
    }
    r
}

/// Versions a claim of [`deep_block`] has been updated to (it holds one more: its create).
pub const DEEP_VERSIONS: usize = 100;

/// A project with one claim written [`DEEP_VERSIONS`] + 1 times, each version in its own view and
/// landed. Returns the project and the claim.
fn deep_block(parent: &std::path::Path) -> (Rdy, BlockId) {
    let proj = parent.join("deep-block");
    std::fs::create_dir_all(&proj).expect("mkdir");
    let mut r = Rdy::init(&proj, "rdy-standard").expect("init");
    let res = write(
        &mut r,
        "b0",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "glossary", "title": "Glossary", "doc_type": "glossary"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Version 0."}
        ]),
    )
    .expect("deep block write");
    let claim = BlockId::parse(&res.created["$c"]).expect("claim id");
    land(&mut r, "b0").expect("deep block land");
    for i in 1..=DEEP_VERSIONS {
        let view = format!("b{i}");
        write(
            &mut r,
            &view,
            json!([{"op": "update", "id": claim.to_string(), "body": format!("Version {i}.")}]),
        )
        .expect("deep block update");
        land(&mut r, &view).expect("deep block land");
    }
    (r, claim)
}

/// `root` and every block under it.
fn subtree(m: &Materialized, root: &BlockId) -> Vec<BlockId> {
    let mut out = vec![];
    let mut stack = vec![root.clone()];
    while let Some(id) = stack.pop() {
        stack.extend(m.children(Some(&id)).iter().map(|b| b.id.clone()));
        out.push(id);
    }
    out
}

/// What the read ops need from a built project: a doc, its section and a claim under it.
struct Fixture {
    doc_path: String,
    section: BlockId,
    claim: BlockId,
}

/// Looks up the first doc of a `generate_tree` project, its first section and that section's
/// first claim in canon.
fn fixture(r: &Rdy, docs: &[String]) -> Fixture {
    let doc_path = "area-0/doc-0";
    assert_eq!(docs[0], doc_path);
    let doc = r.read().resolve_path(&Scope::Canon, doc_path).expect("doc");
    let m = r.read().materialized(&Scope::Canon).expect("canon");
    let section = m.children(Some(&doc))[0].id.clone();
    let claim = m.children(Some(&section))[0].id.clone();
    Fixture {
        doc_path: doc_path.into(),
        section,
        claim,
    }
}

/// Twenty unlanded changes spread over views `va` and `vb`.
fn pending_writes(r: &mut Rdy, section: &BlockId) {
    for i in 0..20 {
        let view = if i % 2 == 0 { "va" } else { "vb" };
        write(
            r,
            view,
            json!([{"op": "create", "type": "claim", "parent": section.to_string(), "body": format!("Pending {i}.")}]),
        )
        .expect("pending write");
    }
}

/// A `Create` of a claim under `section`: the work a commit's publish does to seed the next state.
fn claim_op(section: &BlockId) -> Op {
    Op::Create {
        block: Block {
            id: BlockId::new("c"),
            block_type: "claim".into(),
            parent: Some(section.clone()),
            order: "V".into(),
            fields: Default::default(),
            body: "Incremental.".into(),
            links: Default::default(),
            pins: Default::default(),
        },
    }
}

/// How many runs each op of one tier takes.
struct Runs {
    write: usize,
    read: usize,
    tree: usize,
    materialize_cold: usize,
    incremental: usize,
    /// Runs of `include_200` and `exclude_cold` (each makes a fresh 200-change view).
    view_ops: usize,
    /// Runs of `land_rebase_50` (at most [`LANDING_CLAIMS`]: each lands its own claim edit).
    landing: usize,
    /// Changes in `vr`, in the include source and in every view `tier_views` includes it into.
    view_changes: usize,
    /// Non-empty unpinned views present when `land_rebase_50` lands.
    landing_views: usize,
}

const TYPICAL_RUNS: Runs = Runs {
    write: 30,
    read: 20,
    tree: 20,
    materialize_cold: 10,
    incremental: 20,
    view_ops: 10,
    landing: 5,
    view_changes: 200,
    landing_views: 50,
};

const LARGE_RUNS: Runs = Runs {
    write: 20,
    read: 10,
    tree: 20,
    materialize_cold: 5,
    incremental: 20,
    view_ops: 5,
    landing: 3,
    view_changes: 200,
    landing_views: 50,
};

/// `write_single` and the reads both tiers share, in the order the typical tier has always run
/// them (the reads are timed cold: each run rebuilds what a write would have invalidated).
/// `sfx` is empty for the typical tier and [`LARGE_SUFFIX`] for the large one.
fn tier_reads(out: &mut Vec<Measure>, r: &mut Rdy, fx: &Fixture, sfx: &str, runs: &Runs) {
    let va = Scope::View("va".into());
    let (section, claim, doc_path) = (&fx.section, &fx.claim, fx.doc_path.as_str());
    out.push(measure_timed(&format!("context{sfx}"), runs.read, |_| {
        cold(r, |r| r.read().context(&va, claim).expect("context")).1
    }));
    out.push(measure_timed(&format!("impact{sfx}"), runs.read, |_| {
        cold(r, |r| r.read().impact(&va, claim, 3).expect("impact")).1
    }));
    out.push(measure_timed(
        &format!("resolve_path{sfx}"),
        runs.read,
        |_| {
            cold(r, |r| {
                r.read()
                    .resolve_path(&Scope::Canon, doc_path)
                    .expect("resolve")
            })
            .1
        },
    ));
    out.push(measure(&format!("tree_full{sfx}"), runs.tree, |_| {
        r.read().tree(&Scope::Canon, None, None).expect("tree");
    }));
    // the memoised tree: built once on a warm state (outside the timer), then an Arc clone
    r.read().tree_full(&Scope::Canon).expect("warm the memo");
    out.push(measure(&format!("tree_full_memo{sfx}"), runs.tree, |_| {
        r.read().tree_full(&Scope::Canon).expect("tree_full");
    }));
    // a view's first materialization after a write invalidated the state
    out.push(measure_timed(
        &format!("view_materialize_cold{sfx}"),
        runs.materialize_cold,
        |_| cold(r, |r| r.read().materialized(&va).expect("view")).1,
    ));
    // The incremental step: the view is built (outside the timer), then one Create is applied
    // with `with`, which is exactly the work a commit's publish does to seed the next state.
    let built = r.read().materialized(&va).expect("view");
    let op = [claim_op(section)];
    out.push(measure_timed(
        &format!("view_materialize_incremental{sfx}"),
        runs.incremental,
        |_| {
            let s = Instant::now();
            let next = built.with(&op);
            let took = s.elapsed();
            assert_eq!(next.len(), built.len() + 1);
            took
        },
    ));
    // The last change of every block in the fixture doc, warm (the state is built outside the
    // timer): the cost is one memoised walk per distinct change in the subtree.
    let doc = r
        .read()
        .resolve_path(&Scope::Canon, doc_path)
        .expect("fixture doc");
    let st = r.read();
    let ids = subtree(&st.materialized(&Scope::Canon).expect("canon"), &doc);
    out.push(measure_timed(
        &format!("last_changes_doc{sfx}"),
        runs.read,
        |_| {
            let s = Instant::now();
            let got = st.last_changes(&Scope::Canon, &ids).expect("last changes");
            let took = s.elapsed();
            assert_eq!(got.len(), ids.len());
            took
        },
    ));
}

/// Claims a run of `land_rebase_50` can land an edit of: the touching views edit all of them, so
/// each run's landing intersects exactly the same five views.
const LANDING_CLAIMS: usize = 5;
/// Views that edit the landed claims (the "5 of them touching the landed blocks").
const TOUCHING_VIEWS: usize = 5;
/// Changes (separate writes) in each of the other landing views.
const LANDING_VIEW_CHANGES: usize = 4;

/// `n` one-claim changes in `view`, each its own write, so each is its own change.
fn load_view(r: &mut Rdy, view: &str, section: &BlockId, n: usize) {
    for i in 0..n {
        write(
            r,
            view,
            json!([{"op": "create", "type": "claim", "parent": section.to_string(), "body": format!("Load {view} {i}.")}]),
        )
        .expect("load write");
    }
}

/// A write that creates one new doc, which touches no block any other view writes.
fn doc_write(r: &mut Rdy, view: &str, name: &str) {
    write(
        r,
        view,
        json!([{"op": "create", "type": "doc", "fields": {"name": name, "title": name, "doc_type": "constraints"}}]),
    )
    .expect("doc write");
}

/// The included changes of `view`, as the strings `view_include` takes.
fn change_ids(r: &Rdy, view: &str) -> Vec<String> {
    r.read()
        .included(view)
        .iter()
        .map(|h| h.to_string())
        .collect()
}

/// Open views that are unpinned and non-empty: the ones a landing rebases (M2c reb1).
fn live_unpinned(r: &Rdy) -> usize {
    let st = r.read();
    st.view_names()
        .iter()
        .filter(|n| {
            st.view_head(n).is_some_and(|h| !h.pinned && !h.closed) && !st.included(n).is_empty()
        })
        .count()
}

/// The unpinned view `vr` with its changes. The typical tier makes it before its landings
/// (`propose_land_500_flat`), which rebase it; [`prepare_rebased_view`] does both for the others.
fn make_vr(r: &mut Rdy, fx: &Fixture, runs: &Runs) {
    load_view(r, "vr", &fx.section, runs.view_changes);
}

/// `vr`, then one landing of another view, so `vr` is an unpinned view a landing has rebased.
fn prepare_rebased_view(r: &mut Rdy, fx: &Fixture, runs: &Runs) {
    make_vr(r, fx, runs);
    doc_write(r, "lr", "rebase-vr");
    land(r, "lr").expect("land lr");
}

/// The view ops on a project whose `vr` is an unpinned view a landing has rebased: the cold
/// materialization of it, include, exclude, a fresh view's cold and incremental reads, one landing
/// with `runs.landing_views` live views, and the view list. Run last in a tier, because the
/// views it leaves behind would change the numbers of the ops before it.
fn tier_views(out: &mut Vec<Measure>, r: &mut Rdy, fx: &Fixture, sfx: &str, runs: &Runs) {
    assert!(runs.landing <= LANDING_CLAIMS, "one claim per landing run");
    let human = writer();
    // `vr` was rebased by the tier's landings: it reads on canon's head, through Rebase ops
    let source = change_ids(r, "vr");
    assert_eq!(source.len(), runs.view_changes, "vr holds its changes");
    assert_eq!(
        r.read().effective_base("vr"),
        r.read().canon_head(),
        "vr was not rebased onto canon's head"
    );
    {
        let st = r.read();
        let ops = st.reader().view_op_ids("vr").expect("vr's ops");
        let rebases = ops
            .iter()
            .filter(|h| {
                matches!(
                    st.reader().view_op(h).expect("op").op,
                    rdy_store::view::ViewOpKind::Rebase { .. }
                )
            })
            .count();
        assert!(rebases > 0, "vr's chain has no Rebase op");
    }
    let vr = Scope::View("vr".into());
    out.push(measure_timed(
        &format!("view_materialize_after_rebase{sfx}"),
        runs.materialize_cold,
        |_| cold(r, |r| r.read().materialized(&vr).expect("vr")).1,
    ));

    let mut scratch: Vec<String> = vec![];
    // include 200 changes from `vr` into a fresh view, deps resolved
    out.push(measure(&format!("include_200{sfx}"), runs.view_ops, |i| {
        let name = format!("inc{i}");
        let res = r.view_include(&human, &name, &source).expect("include");
        assert_eq!(res.added.len(), runs.view_changes);
        scratch.push(name);
    }));
    // exclude one change from a built 200-change view, then read it: the rebuild
    out.push(measure_timed(
        &format!("exclude_cold{sfx}"),
        runs.view_ops,
        |i| {
            let name = format!("ex{i}");
            r.view_include(&human, &name, &source).expect("include");
            scratch.push(name.clone());
            let scope = Scope::View(name.clone());
            r.read().materialized(&scope).expect("build the view");
            let last = source[source.len() - 1].clone();
            let s = Instant::now();
            r.view_exclude(&human, &name, &[last], false)
                .expect("exclude");
            let built = r.read().materialized(&scope).expect("rebuild");
            let took = s.elapsed();
            assert!(!built.is_empty());
            took
        },
    ));
    // a fresh 200-change view (never rebased), cold; then one write into it and the read
    r.view_include(&human, "vc", &source).expect("include vc");
    scratch.push("vc".into());
    let vc = Scope::View("vc".into());
    out.push(measure_timed(
        &format!("view_cold{sfx}"),
        runs.materialize_cold,
        |_| cold(r, |r| r.read().materialized(&vc).expect("vc")).1,
    ));
    let section = fx.section.to_string();
    out.push(measure_timed(
        &format!("view_incremental{sfx}"),
        runs.incremental,
        |i| {
            write(
                r,
                "vc",
                json!([{"op": "create", "type": "claim", "parent": section, "body": format!("Inc {i}.")}]),
            )
            .expect("write into vc");
            let s = Instant::now();
            let built = r.read().materialized(&vc).expect("vc");
            let took = s.elapsed();
            assert!(!built.is_empty());
            took
        },
    ));
    // the scratch views are done: closed views are not rebased, so the landing below sees only
    // the views it is meant to
    r.view_close(&human, &scratch, "perf scratch")
        .expect("close the scratch views");

    // `landing_views` live views: the ones already there, then `lv<k>`. The first
    // TOUCHING_VIEWS edit the claims the runs land; the rest create docs.
    let claims: Vec<BlockId> = {
        let canon = r.read().materialized(&Scope::Canon).expect("canon");
        canon
            .children(Some(&fx.section))
            .iter()
            .take(LANDING_CLAIMS)
            .map(|b| b.id.clone())
            .collect()
    };
    assert_eq!(claims.len(), LANDING_CLAIMS, "claims to edit");
    let others = runs.landing_views - live_unpinned(r);
    for k in 0..others {
        let name = format!("lv{k}");
        let docs = if k < TOUCHING_VIEWS {
            let edits: Vec<_> = claims
                .iter()
                .map(|c| json!({"op": "update", "id": c.to_string(), "body": format!("Mine {k} {c}.")}))
                .collect();
            write(r, &name, json!(edits)).expect("touching write");
            LANDING_VIEW_CHANGES - 1
        } else {
            LANDING_VIEW_CHANGES
        };
        for d in 0..docs {
            doc_write(r, &name, &format!("lv-{k}-{d}"));
        }
    }
    assert_eq!(live_unpinned(r), runs.landing_views, "the views to rebase");
    out.push(measure_timed(
        &format!("land_rebase_50{sfx}"),
        runs.landing,
        |i| {
            let name = format!("ln{i}");
            write(
                r,
                &name,
                json!([{"op": "update", "id": claims[i].to_string(), "body": format!("Landed {i}.")}]),
            )
            .expect("landing view write");
            let s = Instant::now();
            land(r, &name).expect("land");
            s.elapsed()
        },
    ));
    assert!(
        !r.read()
            .conflicts(&Scope::View("lv0".into()))
            .expect("lv0")
            .is_empty(),
        "the touching views carry the landing's conflicts"
    );

    // the list over every view there is, closed ones included
    let now = now_ms();
    out.push(measure_timed(
        &format!("view_ls_50{sfx}"),
        runs.read,
        |_| cold(r, |r| r.read().view_list(true, 14, now).expect("list")).1,
    ));
    let listed = r.read().view_list(true, 14, now).expect("list").len();
    assert!(listed >= runs.landing_views, "{listed} views listed");

    // M2c3: a briefing over the landing claims with the views above live (the touching ones change
    // those claims), the consequences of a fresh include, and a proposal's consequences for canon
    let cov = Coverage {
        ids: claims.iter().map(|c| c.to_string()).collect(),
        path: None,
    };
    out.push(measure_timed(
        &format!("brief_inflight_50{sfx}"),
        runs.read,
        |_| {
            let st = r.read();
            let s = Instant::now();
            let b = st.brief(&Scope::Canon, &cov, None).expect("brief");
            let took = s.elapsed();
            assert!(
                !b.inflight.views.is_empty(),
                "the touching views are in flight"
            );
            took
        },
    ));
    out.push(measure_timed(
        &format!("view_consequences_200{sfx}"),
        runs.view_ops,
        |i| {
            let name = format!("vq{i}");
            let before = r.read();
            let res = r.view_include(&human, &name, &source).expect("include");
            let after = r.read();
            let head: Hash = res.head.parse().expect("head");
            let changes: Vec<Hash> = res
                .added
                .iter()
                .chain(&res.added_deps)
                .map(|h| h.parse().expect("change"))
                .collect();
            let s = Instant::now();
            after
                .view_consequences(&before, &name, &head, Some(&changes), None)
                .expect("view consequences");
            s.elapsed()
        },
    ));
    r.read()
        .graph(&Scope::Canon)
        .expect("canon's graph, built once");
    out.push(measure_timed(
        &format!("landing_consequences{sfx}"),
        runs.view_ops,
        |i| {
            let name = format!("pq{i}");
            write(
                r,
                &name,
                json!([{"op": "update", "id": claims[i % claims.len()].to_string(), "body": format!("Proposed {i}.")}]),
            )
            .expect("proposal write");
            let p = r.propose(&human, &name, &name).expect("propose");
            let version: Hash = p.id.parse().expect("version");
            let st = r.read();
            let s = Instant::now();
            st.landing_consequences(&version, None).expect("consequences");
            s.elapsed()
        },
    ));
}

/// The edit loop: a briefing over a doc, a write that cites a briefing (the check on, a hit) and a
/// write followed by its consequences. Run in a fresh view `eb` where the fixture's section is
/// `stable`, so every write there is checked against the registry. `sfx` as for [`tier_reads`].
fn tier_edit_loop(out: &mut Vec<Measure>, r: &mut Rdy, fx: &Fixture, sfx: &str, runs: &Runs) {
    let eb = Scope::View("eb".into());
    let (section, claim) = (fx.section.to_string(), fx.claim.to_string());
    // setup, untimed: the section stable in `eb`, a current briefing of it, a claim that
    // references the fixture's claim (it becomes suspect when that claim changes), the graph built
    let t0 = brief_of(r, &Scope::Canon, &[&fx.section]).base.token;
    write_cited(
        r,
        "eb",
        json!([{"op": "update", "id": section, "fields": {"lifecycle": "stable"}}]),
        Some(&t0),
    )
    .expect("make the section stable in eb");
    let ts = brief_of(r, &eb, &[&fx.section]).base.token;
    write_cited(
        r,
        "eb",
        json!([{"op": "create", "type": "claim", "parent": section, "body": format!("{{{{{claim}}}}}")}]),
        Some(&ts),
    )
    .expect("the referrer");
    r.read().graph(&eb).expect("graph");

    let cov = Coverage {
        ids: vec![],
        path: Some(fx.doc_path.clone()),
    };
    out.push(measure_timed(&format!("brief_doc{sfx}"), runs.read, |i| {
        write_cited(
            r,
            "eb",
            json!([{"op": "create", "type": "claim", "parent": section, "body": format!("Briefed {i}.")}]),
            Some(&ts),
        )
        .expect("write for brief_doc");
        let s = Instant::now();
        let b = r.read().brief(&eb, &cov, None).expect("brief");
        b.register(&r.briefs());
        let took = s.elapsed();
        assert!(b.covers > 0);
        took
    }));
    out.push(measure(&format!("write_briefed{sfx}"), runs.write, |i| {
        write_cited(
            r,
            "eb",
            json!([{"op": "create", "type": "claim", "parent": section, "body": format!("Cited {i}.")}]),
            Some(&ts),
        )
        .expect("write_briefed");
    }));
    out.push(measure_timed(
        &format!("write_consequences{sfx}"),
        runs.write,
        |i| {
            let tc = brief_of(r, &eb, &[&fx.claim]).base.token;
            let s = Instant::now();
            let w = write_cited(
                r,
                "eb",
                json!([{"op": "update", "id": claim, "body": format!("Changed {i}.")}]),
                Some(&tc),
            )
            .expect("write for consequences");
            let change: Hash = w.change.parse().expect("change hash");
            let cq = r
                .read()
                .consequences(&eb, &change, None)
                .expect("consequences");
            let took = s.elapsed();
            let suspects = cq
                .items
                .iter()
                .filter(|c| c.kind == ConsequenceKind::Suspect)
                .count();
            assert_eq!(suspects, 1, "the referrer from the setup: {cq:?}");
            took
        },
    ));
}

/// The code index: a full rebuild over a 2,000-commit, 400-file synthetic history whose commits
/// cite the fixture's claims, and a briefing of one file against the warm index. The project's
/// config is put back afterwards, so later ops see it unchanged. Typical tier only.
fn tier_code(out: &mut Vec<Measure>, r: &Rdy, fx: &Fixture, _dir: &std::path::Path) {
    use rdy_engine::code_index::{build, code_repos};
    // setup, untimed: the history, the link, the warm index
    let cite: Vec<String> = {
        let m = r.read().materialized(&Scope::Canon).expect("canon");
        m.children(Some(&fx.section))
            .iter()
            .take(50)
            .map(|b| b.id.to_string())
            .collect()
    };
    let g = crate::git_repo::GitRepo::synthetic(2000, 400, &cite);
    let config = r.root().join(".rdy/config.yaml");
    let saved = std::fs::read(&config).expect("read the project config");
    g.link(r.root());
    let repo = code_repos(r.root()).expect("code_repos")[0].clone();
    r.read()
        .brief_code(&Scope::Canon, "src/f0.rs", None, None)
        .expect("warm brief_code");
    out.push(measure("code_index_build", 5, |_| {
        build(&repo).expect("build");
    }));
    out.push(measure("brief_code", 30, |_| {
        let b = r
            .read()
            .brief_code(&Scope::Canon, "src/f0.rs", None, None)
            .expect("brief_code");
        assert!(b.briefing.covers > 0);
    }));
    std::fs::write(&config, saved).expect("restore the project config");
}

fn tier_render(out: &mut Vec<Measure>, r: &Rdy, dir: &std::path::Path, sfx: &str, runs: usize) {
    out.push(measure(&format!("render{sfx}"), runs, |i| {
        let site = dir.join(format!("site{sfx}{i}"));
        rdy_render::render_site(&r.read(), &Scope::Canon, &site).expect("render");
    }));
}

/// Cold open: a fresh handle on the project, then the first read of canon.
fn tier_cold_open(out: &mut Vec<Measure>, proj: &std::path::Path, sfx: &str, runs: usize) {
    out.push(measure(&format!("cold_open{sfx}"), runs, |_| {
        let r = Rdy::open(proj).expect("open");
        r.read().materialized(&Scope::Canon).expect("canon");
    }));
}

/// The fixed workload: a ~10k-block tree project (ids seeded, so it is identical every run),
/// timed op by op; with `RDY_QA_DEEP=1`, then the same ops on a ~50k-block project under the
/// `_50k` names. Takes a few minutes in release (every write fsyncs).
pub fn run_workload() -> Vec<Measure> {
    rdy_model::seed_ids_for_tests(0x7064_6566);
    let dir = tempfile::tempdir().expect("tempdir");
    let proj = dir.path().join("p");
    std::fs::create_dir_all(&proj).expect("mkdir");
    // 10 folders x 10 docs x 97 claims (+ section, doc, folder) ~ 10k blocks, in view `work`.
    let docs = testkit::generate_tree(&proj, 10, 10, 97);
    let mut r = Rdy::open(&proj).expect("open");
    land(&mut r, "work").expect("land work");
    r.tag(&approver(), "v0.1.0", None, None).expect("tag");

    // A fixed doc, its section and its first claim.
    let fx = fixture(&r, &docs);
    let section = fx.section.to_string();
    // `va` is pinned on this canon so that `propose_land_500_flat`'s landings (which rebase every
    // unpinned non-empty view, M2c reb1) leave it where the view reads have always measured it:
    // the 10k canon plus its own changes. Unpinned, it would read 2,500 landed docs more.
    r.view_create(&crate::fixtures::writer(), "va", None, true)
        .expect("pin va");
    pending_writes(&mut r, &fx.section);
    // `vr`: an unpinned view with 200 changes, which `propose_land_500_flat`'s landings rebase;
    // `tier_views` measures it, and the include and exclude ops draw on its changes. Made
    // before the snapshot below, so the lagging open still catches up the same frames.
    make_vr(&mut r, &fx, &TYPICAL_RUNS);

    // The lagging-index snapshot: made after the pending writes and before the timed writes, so
    // the later writes (and the landings) are what a cold open has to catch the index up over.
    // Outside every timer.
    r.checkpoint().expect("checkpoint");
    let index_path = r.store().dir().join("index.redb");
    drop(r);
    let snapshot = dir.path().join("index.snapshot");
    std::fs::copy(&index_path, &snapshot).expect("snapshot the index");
    let mut r = Rdy::open(&proj).expect("reopen");

    let mut out = vec![];
    // built once, outside every timer, and before the heavy ops so it stays small on disk
    let mut deep = deep_history(dir.path());
    // a block 101 versions deep, in its own project: the workload project stays as it was
    let (deep_block_project, deep_claim) = deep_block(dir.path());

    out.push(measure("write_single", TYPICAL_RUNS.write, |i| {
        write(
            &mut r,
            "va",
            json!([{"op": "create", "type": "claim", "parent": section, "body": format!("Single {i}.")}]),
        )
        .expect("write");
    }));
    out.push(measure("write_batch_200", 9, |i| {
        let ops: Vec<_> = (0..200)
            .map(|c| json!({"op": "create", "type": "claim", "parent": section, "body": format!("Batch {i} claim {c}.")}))
            .collect();
        write(&mut r, "va", json!(ops)).expect("batch");
    }));
    out.push(measure("propose_land_500_flat", 5, |i| {
        let view = format!("flat{i}");
        let ops: Vec<_> = (0..500)
            .map(|d| json!({"op": "create", "type": "doc", "fields": {"name": format!("flat-{i}-{d}"), "title": format!("Flat {i} {d}"), "doc_type": "constraints"}}))
            .collect();
        write(&mut r, &view, json!(ops)).expect("flat write");
        land(&mut r, &view).expect("flat land");
    }));
    // no version tag on this chain, so every run walks all of its landings
    out.push(measure_timed("version_position", 20, |_| {
        cold(&mut deep, |r| {
            r.read().version_position(&Scope::Canon).expect("position")
        })
        .1
    }));
    // a cached lookup on the workload project: the first call (outside the timer) fills the cache
    r.read().version_position(&Scope::Canon).expect("warm");
    out.push(measure("version_position_cached", 200, |_| {
        r.read().version_position(&Scope::Canon).expect("position");
    }));
    // history of a 101-version block, the log's first page and the whole log: the state is
    // warmed outside each timer
    {
        let st = deep_block_project.read();
        st.materialized(&Scope::Canon).expect("warm");
        out.push(measure_timed("history_100", 20, |_| {
            let s = Instant::now();
            let h = st
                .history(&Scope::Canon, &deep_claim, DEEP_VERSIONS + 1)
                .expect("history");
            let took = s.elapsed();
            assert_eq!(h.entries.len(), DEEP_VERSIONS + 1);
            took
        }));
    }
    {
        let st = deep.read();
        st.materialized(&Scope::Canon).expect("warm");
        out.push(measure_timed("log_page_20", 20, |_| {
            let s = Instant::now();
            let rows = st.log_page(None, Some(20)).expect("log page");
            let took = s.elapsed();
            assert_eq!(rows.len(), 20);
            took
        }));
        out.push(measure_timed("log_full", 5, |_| {
            let s = Instant::now();
            let rows = st.log_rows().expect("log");
            let took = s.elapsed();
            assert_eq!(rows.len(), DEEP_LANDINGS);
            took
        }));
    }
    // the flat landings moved canon on, and `va` (pinned) stayed on the 10k canon
    assert_ne!(
        r.read().effective_base("va"),
        r.read().canon_head(),
        "va was rebased: the view reads would measure a different view"
    );
    tier_reads(&mut out, &mut r, &fx, "", &TYPICAL_RUNS);
    tier_edit_loop(&mut out, &mut r, &fx, "", &TYPICAL_RUNS);
    tier_code(&mut out, &r, &fx, dir.path());
    tier_render(&mut out, &r, dir.path(), "", 5);
    out.push(measure("export_import", 5, |i| {
        let export = dir.path().join(format!("export{i}"));
        r.read().export(&Scope::Canon, &export).expect("export");
        let fresh = dir.path().join(format!("fresh{i}"));
        std::fs::create_dir_all(&fresh).expect("mkdir");
        let mut r2 = Rdy::init(&fresh, "rdy-standard").expect("init");
        r2.import(&approver(), "work", &export).expect("import");
    }));
    drop(r);
    tier_cold_open(&mut out, &proj, "", 5);
    // Cold open with a lagging index: the project is closed, so put the old index back over the
    // current one (outside the timer); open then catches it up from the log.
    let mut lag = None;
    out.push(measure_timed("cold_open_lagging", 5, |_| {
        std::fs::copy(&snapshot, &index_path).expect("restore the lagging index");
        let s = Instant::now();
        let r = Rdy::open(&proj).expect("open");
        r.read().materialized(&Scope::Canon).expect("canon");
        let took = s.elapsed();
        lag.get_or_insert(r.store().open_report().caught_up_frames);
        took
    }));
    eprintln!(
        "cold_open_lagging: caught up {} frames",
        lag.expect("ran at least once")
    );
    // the in-process comparator for `daemon_write_single`: one `new_doc` (a doc and its
    // skeleton). After the cold opens, so they open the same project as before the daemon ops
    // were added (before them, its 30 docs in `vd` gave the lagging open 271 more frames to
    // catch up: 4,822 instead of 4,551)
    let mut r = Rdy::open(&proj).expect("open");
    out.push(measure("new_doc_single", 30, |i| {
        r.new_doc(&writer(), "vd", &format!("nd-{i}"), None, None)
            .expect("new_doc");
    }));
    drop(r);
    out.extend(daemon_ops(&proj, ""));
    // the view ops last: the views and landings they leave behind would move the ops above
    let mut r = Rdy::open(&proj).expect("open");
    tier_views(&mut out, &mut r, &fx, "", &TYPICAL_RUNS);
    drop(r);
    if crate::fixtures::deep() {
        out.extend(run_large_tier(dir.path()));
    }
    rdy_model::unseed_ids_for_tests();
    out
}

/// Starts a daemon over `proj` (idle timeout 60 s, human actor `human:qa`, the fixture writer).
fn daemon_start(proj: &std::path::Path) -> ServeRun {
    rdy_daemon::start(ServeOptions {
        project: proj.to_path_buf(),
        version: "perf".into(),
        detach: false,
        idle_timeout: Some(Duration::from_secs(60)),
        human_actor: Some("human:qa".into()),
    })
    .unwrap_or_else(|e| panic!("start the daemon: {e}"))
}

/// A `ctl` connection to the daemon at `proj`, past `ready` (waits for it while it is `starting`).
async fn daemon_ready(proj: &std::path::Path) -> Client {
    let hello = Hello {
        rdy: PROTO_VERSION,
        version: "perf".into(),
        proto: Proto::Ctl,
        cwd: proj.to_path_buf(),
    };
    let run = RunDir::of_project(proj);
    match rdy_daemon::connect(&run, &hello).await {
        Attempt::Ready(c) => c,
        Attempt::Starting(p) => p
            .wait(Instant::now() + Duration::from_secs(30), &mut |_| {})
            .await
            .unwrap_or_else(|e| panic!("the daemon did not become ready: {e}")),
        other => panic!("the daemon did not answer the hello: {other:?}"),
    }
}

fn daemon_stop(run: ServeRun) {
    run.handle().stop();
    run.wait()
        .unwrap_or_else(|e| panic!("the daemon failed: {e}"));
}

/// The daemon ops, on the typical project once every in-process handle on it is closed:
/// `daemon_write_single` (one `new_doc` over a `ctl` connection, the same shape as
/// `new_doc_single`) and `daemon_cold_start` (start to `ready` on a project whose index is
/// current: the warm-up plus the bind, not a catch-up). `sfx` names the tier (`""` or `_50k`).
fn daemon_ops(proj: &std::path::Path, sfx: &str) -> Vec<Measure> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("client runtime");
    let mut out = vec![];
    let run = daemon_start(proj);
    let mut c = rt.block_on(daemon_ready(proj));
    // the human session, who may write into `va`
    rt.block_on(c.ctl("session", json!({"actor": null, "run": null})))
        .unwrap_or_else(|e| panic!("the session op failed: {e}"));
    out.push(measure(&format!("daemon_write_single{sfx}"), 30, |i| {
        rt.block_on(c.ctl("new_doc", json!({"path": format!("dw-{i}"), "view": "va"})))
            .unwrap_or_else(|e| panic!("new_doc: {e}"));
    }));
    drop(c);
    daemon_stop(run);
    // one untimed start and stop, so the timed starts open a current index
    let run = daemon_start(proj);
    drop(rt.block_on(daemon_ready(proj)));
    daemon_stop(run);
    out.push(measure_timed(&format!("daemon_cold_start{sfx}"), 3, |_| {
        let s = Instant::now();
        let run = daemon_start(proj);
        let c = rt.block_on(daemon_ready(proj));
        let took = s.elapsed();
        drop(c);
        daemon_stop(run);
        took
    }));
    out
}

/// The large tier (`RDY_QA_DEEP=1`): a ~50k-block project, ids seeded as for the typical tier,
/// landed, with twenty pending writes; the shared ops under the `_50k` names, and the daemon ops
/// (`daemon_write_single_50k`, `daemon_cold_start_50k`) once every in-process handle is closed. `version_position`
/// (its fixture is landing depth, not size), `cold_open_lagging`, `write_batch_200`,
/// `propose_land_500_flat` and `export_import` stay typical-only. The view ops (`tier_views`) run last
/// in both tiers.
fn run_large_tier(parent: &std::path::Path) -> Vec<Measure> {
    rdy_model::seed_ids_for_tests(0x7064_6566);
    let sfx = LARGE_SUFFIX;
    let proj = parent.join("p50");
    std::fs::create_dir_all(&proj).expect("mkdir");
    // 50 folders x 10 docs x 97 claims (+ section, doc, folder) ~ 50k blocks, in view `work`.
    let docs = testkit::generate_tree(&proj, 50, 10, 97);
    let mut r = Rdy::open(&proj).expect("open");
    land(&mut r, "work").expect("land work");
    let fx = fixture(&r, &docs);
    pending_writes(&mut r, &fx.section);
    let section = fx.section.to_string();
    let mut out = vec![];
    out.push(measure(&format!("write_single{sfx}"), LARGE_RUNS.write, |i| {
        write(
            &mut r,
            "va",
            json!([{"op": "create", "type": "claim", "parent": section, "body": format!("Single {i}.")}]),
        )
        .expect("write");
    }));
    tier_reads(&mut out, &mut r, &fx, sfx, &LARGE_RUNS);
    tier_edit_loop(&mut out, &mut r, &fx, sfx, &LARGE_RUNS);
    tier_render(&mut out, &r, parent, sfx, 3);
    drop(r);
    tier_cold_open(&mut out, &proj, sfx, 3);
    out.extend(daemon_ops(&proj, sfx));
    // the view ops last, on a reopened project; no landing has rebased `vr` here, so one does
    let mut r = Rdy::open(&proj).expect("open");
    prepare_rebased_view(&mut r, &fx, &LARGE_RUNS);
    tier_views(&mut out, &mut r, &fx, sfx, &LARGE_RUNS);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(op: &str, median: f64) -> Measure {
        Measure {
            op: op.into(),
            median_ms: median,
            p95_ms: median,
            runs: 1,
        }
    }

    #[test]
    fn hperf1_the_history_ops_state_their_targets() {
        for op in [
            "history_100",
            "last_changes_doc",
            "log_page_20",
            "last_changes_doc_50k",
        ] {
            assert!(spec_target(op).unwrap().contains("< 5 ms p99"), "{op}");
        }
        assert!(
            spec_target("last_changes_doc_50k")
                .unwrap()
                .contains("large tier")
        );
        assert!(spec_target("log_full").unwrap().contains("no spec target"));
    }

    #[test]
    fn regressions_need_both_the_margin_and_the_floor() {
        let base = Baseline {
            machine: "x".into(),
            ops: [
                ("context".to_string(), m("context", 0.01)),
                ("render".to_string(), m("render", 100.0)),
                ("write_single".to_string(), m("write_single", 20.0)),
                (
                    "propose_land_500_flat".to_string(),
                    m("propose_land_500_flat", 20.0),
                ),
            ]
            .into(),
        };
        let v = compare(
            Some(&base),
            &[
                m("context", 0.05),
                m("render", 126.0),
                m("write_single", 29.0),
                m("propose_land_500_flat", 29.0),
            ],
        );
        assert!(v[0].pass, "+400% but under 2 ms absolute");
        assert!(!v[1].pass, "+26% and +26 ms");
        assert!(v[2].pass, "writes get a 50% margin");
        assert!(v[3].pass, "proposals fsync too, so they get it as well");
        let v = compare(Some(&base), &[m("write_single", 31.0)]);
        assert!(!v[0].pass);
        assert!(
            compare(None, &[m("render", 1e9)])[0].pass,
            "no baseline passes"
        );
    }

    #[test]
    fn cold_reads_drop_the_caches_before_the_timed_region_and_rebuild_inside_it() {
        let (_d, mut r) = crate::fixtures::project();
        let res = write(&mut r, "work", json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Tx", "doc_type": "glossary"}}
        ]))
        .unwrap();
        let doc = BlockId::parse(&res.created["$d"]).unwrap();
        let work = Scope::View("work".into());
        r.read().context(&work, &doc).unwrap();
        assert_eq!(r.read().is_built(&work), (true, true), "warm before");
        let mut at_start = None;
        let (_, took) = cold(&mut r, |r| {
            at_start = Some(r.read().is_built(&work));
            r.read().context(&work, &doc).unwrap()
        });
        assert_eq!(at_start, Some((false, false)), "the timed body starts cold");
        assert_eq!(r.read().is_built(&work), (true, true), "the body rebuilt");
        assert!(took > std::time::Duration::ZERO);
    }

    #[test]
    fn the_machine_class_has_no_hostname() {
        let c = machine_class();
        let host = std::process::Command::new("hostname")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        assert!(host.is_empty() || !c.contains(&host), "{c}");
        assert_eq!(c.matches(" / ").count(), 2, "{c}");
    }

    #[test]
    fn the_write_target_is_stated_per_hardware_class() {
        let t = spec_target("write_single").unwrap();
        assert!(
            t.contains("Linux NVMe") && t.contains("one durable flush"),
            "{t}"
        );
        assert!(
            spec_target("cold_open_lagging")
                .unwrap()
                .contains("catch-up")
        );
    }

    #[test]
    fn tier1_large_tier_ops_carry_the_typical_targets() {
        for op in ["context", "render", "write_single"] {
            let typical = spec_target(op).unwrap();
            let large = spec_target(&format!("{op}_50k")).unwrap();
            assert!(!typical.contains("large tier"), "{op}: {typical}");
            assert!(
                large.contains(&typical) && large.contains(LARGE_TIER_NOTE),
                "{op}_50k: {large}"
            );
        }
        assert!(
            spec_target("view_materialize_cold_50k")
                .unwrap()
                .contains("< 50 ms cold")
        );
        assert_eq!(spec_target("nonsense_50k"), None);
    }

    #[test]
    fn tier1_the_new_ops_state_their_targets() {
        assert!(
            spec_target("tree_full_memo")
                .unwrap()
                .contains("memoised per canon head")
        );
        assert!(
            spec_target("view_materialize_cold")
                .unwrap()
                .contains("< 50 ms cold")
        );
        assert!(
            spec_target("view_materialize_incremental")
                .unwrap()
                .contains("< 1 ms incremental")
        );
        assert!(spec_target("version_position_cached").is_some());
    }

    #[test]
    fn vperf1_the_view_ops_state_their_targets() {
        for op in [
            "include_200",
            "exclude_cold",
            "view_cold",
            "view_incremental",
            "land_rebase_50",
            "view_ls_50",
            "view_materialize_after_rebase",
        ] {
            let t = spec_target(op).unwrap_or_else(|| panic!("{op} has no target text"));
            let large = spec_target(&format!("{op}_50k")).unwrap();
            assert!(
                large.contains(&t) && large.contains(LARGE_TIER_NOTE),
                "{op}"
            );
        }
        let t = spec_target("view_materialize_after_rebase").unwrap();
        assert!(
            t.contains("< 50 ms cold") && t.contains("unpinned view") && t.contains("rebased"),
            "{t}"
        );
        assert!(
            spec_target("land_rebase_50")
                .unwrap()
                .contains("< 100 ms p99")
        );
        assert!(
            spec_target("view_incremental")
                .unwrap()
                .contains("< 1 ms incremental")
        );
        for op in ["include_200", "exclude_cold", "land_rebase_50"] {
            assert!(fsync_bound(op), "{op} ends in a durable flush");
        }
        assert!(!fsync_bound("view_cold") && !fsync_bound("view_ls_50"));
    }

    /// The view ops run end to end on a small tree, with every precondition `tier_views` asserts
    /// (the rebased `vr`, exactly the stated number of live views at the landing).
    #[test]
    fn vperf1_the_view_ops_run_on_a_small_tree() {
        rdy_model::seed_ids_for_tests(0x7064_6566);
        let dir = tempfile::tempdir().expect("tempdir");
        let proj = dir.path().join("p");
        std::fs::create_dir_all(&proj).expect("mkdir");
        let docs = testkit::generate_tree(&proj, 2, 3, 12);
        let mut r = Rdy::open(&proj).expect("open");
        land(&mut r, "work").expect("land work");
        let fx = fixture(&r, &docs);
        let runs = Runs {
            write: 1,
            read: 2,
            tree: 1,
            materialize_cold: 2,
            incremental: 2,
            view_ops: 2,
            landing: 2,
            view_changes: 12,
            landing_views: 8,
        };
        prepare_rebased_view(&mut r, &fx, &runs);
        let mut out = vec![];
        tier_views(&mut out, &mut r, &fx, "", &runs);
        rdy_model::unseed_ids_for_tests();
        let names: Vec<&str> = out.iter().map(|m| m.op.as_str()).collect();
        assert_eq!(
            names,
            [
                "view_materialize_after_rebase",
                "include_200",
                "exclude_cold",
                "view_cold",
                "view_incremental",
                "land_rebase_50",
                "view_ls_50",
                "brief_inflight_50",
                "view_consequences_200",
                "landing_consequences"
            ]
        );
        for m in &out {
            assert!(m.median_ms > 0.0 || m.op == "view_incremental", "{m:?}");
        }
    }

    #[test]
    fn tier1_a_typical_rebaseline_keeps_the_large_tier_and_a_typical_run_ignores_it() {
        let base = Baseline {
            machine: "x".into(),
            ops: [
                ("context".to_string(), m("context", 1.0)),
                ("context_50k".to_string(), m("context_50k", 5.0)),
            ]
            .into(),
        };
        let merged = merge_baseline(Some(&base), "x", &[m("context", 2.0), m("render", 9.0)]);
        assert_eq!(
            merged.ops["context"].median_ms, 2.0,
            "measured ops are replaced"
        );
        assert_eq!(merged.ops["context_50k"].median_ms, 5.0, "the rest is kept");
        assert_eq!(merged.ops["render"].median_ms, 9.0, "new ops are added");
        let typical = typical_only(&merged);
        assert!(typical.ops.contains_key("context") && !typical.ops.contains_key("context_50k"));
        let fresh = merge_baseline(None, "y", &[m("context", 3.0)]);
        assert_eq!((fresh.machine.as_str(), fresh.ops.len()), ("y", 1));
    }

    #[test]
    #[ignore = "builds the 50k project; run with --ignored"]
    fn dperf1_the_large_tier_runs_the_daemon_ops_at_50k() {
        assert!(spec_target("daemon_write_single_50k").is_some());
        assert!(spec_target("daemon_cold_start_50k").is_some());
        let dir = tempfile::tempdir().expect("tempdir");
        let ms = run_large_tier(dir.path());
        rdy_model::unseed_ids_for_tests();
        for op in ["daemon_write_single_50k", "daemon_cold_start_50k"] {
            let m = ms
                .iter()
                .find(|m| m.op == op)
                .unwrap_or_else(|| panic!("{op} missing"));
            eprintln!("{op}: median {:.3} ms, p95 {:.3} ms", m.median_ms, m.p95_ms);
            assert!(m.median_ms > 0.0, "{op}: {m:?}");
        }
    }

    #[test]
    #[ignore = "runs the typical perf workload; cargo test -p rdy-qa --release --lib -- --ignored bperf1"]
    fn bperf1_the_edit_loop_ops_have_spec_targets_and_run() {
        for op in ["brief_doc", "write_briefed", "write_consequences"] {
            assert!(spec_target(op).is_some(), "{op} has no target");
        }
        let ms = run_workload();
        for op in ["brief_doc", "write_briefed", "write_consequences"] {
            let m = ms
                .iter()
                .find(|m| m.op == op)
                .unwrap_or_else(|| panic!("{op} missing"));
            assert!(m.median_ms > 0.0);
        }
    }

    #[test]
    #[ignore = "runs the typical perf workload; cargo test -p rdy-qa --release --lib -- --ignored c3perf1"]
    fn c3perf1_the_m2c3_ops_have_spec_targets_and_run() {
        let ops = [
            "brief_inflight_50",
            "view_consequences_200",
            "landing_consequences",
        ];
        for op in ops {
            assert!(spec_target(op).is_some(), "{op} has no target");
        }
        let ms = run_workload();
        for op in ops {
            let m = ms
                .iter()
                .find(|m| m.op == op)
                .unwrap_or_else(|| panic!("{op} missing"));
            assert!(m.median_ms > 0.0);
        }
    }

    #[test]
    #[ignore = "runs the typical perf workload; cargo test -p rdy-qa --release --lib -- --ignored cperf1"]
    fn cperf1_the_code_index_ops_have_targets_and_run() {
        for op in ["code_index_build", "brief_code"] {
            assert!(spec_target(op).is_some(), "{op} has no target");
        }
        let ms = run_workload();
        for op in ["code_index_build", "brief_code"] {
            let m = ms
                .iter()
                .find(|m| m.op == op)
                .unwrap_or_else(|| panic!("{op} missing"));
            assert!(m.median_ms > 0.0);
        }
    }

    #[test]
    fn cperf1_a_synthetic_history_has_every_commit_and_its_citations() {
        let cite = vec!["c-AAAAA".to_string(), "c-BBBBB".to_string()];
        let g = crate::git_repo::GitRepo::synthetic(30, 7, &cite);
        assert_eq!(g.run(&["rev-list", "--count", "HEAD"]), "30");
        let repo = rdy_engine::code_index::CodeRepo {
            configured: g.configured(),
            root: std::fs::canonicalize(g.path()).unwrap(),
        };
        let idx = rdy_engine::code_index::build(&repo).unwrap();
        assert_eq!(idx.commits, 30);
        assert_eq!(idx.cited["c-AAAAA"].len(), 30, "every commit cites both");
        assert!(idx.files.contains_key("src/f0.rs"));
    }

    #[test]
    #[ignore = "runs the full workload; run with --ignored"]
    fn dperf1_the_daemon_ops_have_spec_targets_and_run() {
        assert!(spec_target("daemon_write_single").is_some());
        assert!(spec_target("daemon_cold_start").is_some());
        assert!(spec_target("new_doc_single").is_some());
        let ms = run_workload();
        for op in ["new_doc_single", "daemon_write_single", "daemon_cold_start"] {
            let m = ms
                .iter()
                .find(|m| m.op == op)
                .unwrap_or_else(|| panic!("{op} missing"));
            assert!(m.median_ms > 0.0, "{op}: {m:?}");
        }
    }
}
