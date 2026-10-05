//! The perf-regression tier: a fixed workload, per-machine baselines, regression verdicts.
use crate::fixtures::{approver, land, write};
use rdy_engine::{Rdy, Scope, testkit};
use rdy_model::{Block, BlockId, Op};
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
        "context" | "impact" | "version_position" | "resolve_path" | "tree_full" => {
            "< 5 ms p99 (spec 11)"
        }
        "version_position_cached" => "< 10 us p99 on the isolated Linux bench (vpos1)",
        "tree_full_memo" => {
            "< 5 ms p99 at 1k folders / 10k docs, memoised per canon head (M2 spec 15.1)"
        }
        "view_materialize_cold" => "< 50 ms cold (M2 spec 15.1)",
        "view_materialize_incremental" => "< 1 ms incremental (M2 spec 15.1)",
        "render" => "< 2 s full render at 100k (spec 11)",
        "cold_open" => "< 2 s at 100k (spec 11)",
        "cold_open_lagging" => "< 2 s at 100k, index catch-up included (M2 spec 15.1)",
        _ => return None,
    };
    Some(if large {
        format!("{target}{LARGE_TIER_NOTE}")
    } else {
        target.to_string()
    })
}

pub fn compare(baseline: Option<&Baseline>, measured: &[Measure]) -> Vec<Verdict> {
    measured
        .iter()
        .map(|m| {
            let base = baseline.and_then(|b| b.ops.get(&m.op)).map(|b| b.median_ms);
            let margin = if m.op.starts_with("write") || m.op.starts_with("propose") {
                0.50
            } else {
                0.25
            };
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
}

const TYPICAL_RUNS: Runs = Runs {
    write: 30,
    read: 20,
    tree: 20,
    materialize_cold: 10,
    incremental: 20,
};

const LARGE_RUNS: Runs = Runs {
    write: 20,
    read: 10,
    tree: 20,
    materialize_cold: 5,
    incremental: 20,
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
    pending_writes(&mut r, &fx.section);

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
    tier_reads(&mut out, &mut r, &fx, "", &TYPICAL_RUNS);
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
    if crate::fixtures::deep() {
        out.extend(run_large_tier(dir.path()));
    }
    rdy_model::unseed_ids_for_tests();
    out
}

/// The large tier (`RDY_QA_DEEP=1`): a ~50k-block project, ids seeded as for the typical tier,
/// landed, with twenty pending writes; the shared ops under the `_50k` names. `version_position`
/// (its fixture is landing depth, not size), `cold_open_lagging`, `write_batch_200`,
/// `propose_land_500_flat` and `export_import` stay typical-only.
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
    tier_render(&mut out, &r, parent, sfx, 3);
    drop(r);
    tier_cold_open(&mut out, &proj, sfx, 3);
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
}
