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
use std::collections::{BTreeMap, BTreeSet};
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

/// The absolute floor of an fsync-bound op: its median moves with the disk, so a rise of less
/// than this is noise, whatever its percentage (ruling R5).
pub const FSYNC_FLOOR_MS: f64 = 2.0;
/// The absolute floor of every other op: small enough that a sub-millisecond read that grows
/// many-fold fails, large enough to absorb timer jitter.
pub const READ_FLOOR_MS: f64 = 0.05;

/// The rise in median (ms) below which `op` never counts as regressed.
pub fn floor_ms(op: &str) -> f64 {
    if fsync_bound(op) {
        FSYNC_FLOOR_MS
    } else {
        READ_FLOOR_MS
    }
}

/// The spec's ceiling for `op` in ms, enforced on its p95 with or without a baseline. Exact op
/// names only: a `_50k` op has none, and an op whose target is reported (not enforced) has none.
pub fn spec_ceiling_ms(op: &str) -> Option<f64> {
    Some(match op {
        "tree_full" | "tree_full_memo" | "brief_doc" | "version_position" => 5.0,
        "view_materialize_cold" | "view_cold" => 50.0,
        "view_materialize_incremental" | "view_incremental" => 1.0,
        "render"
        | "cold_open"
        | "cold_open_lagging"
        | "daemon_cold_start"
        | "open_chosen_alts_100" => 2000.0,
        _ => return None,
    })
}

/// The suffix of a read target that is measured cold, so its spec figure (a warm read) is
/// reported but not enforced.
const COLD_READ_NOTE: &str = " (reported, not enforced: measured cold)";

/// The note a `_50k` op carries: its budget is the typical tier's, applied at 50k blocks.
const LARGE_TIER_NOTE: &str = " (large tier: budget at 50k, DECISIONS-rdy-m2 §7)";

fn spec_target(op: &str) -> Option<String> {
    let (base, large) = match op.strip_suffix("_50k") {
        Some(b) => (b, true),
        None => (op, false),
    };
    let target = match base {
        "write_batch_200" => {
            "no spec target: one write of 200 creates, one durable flush (recorded; the < 5 ms write budget is for one op)"
        }
        "propose_land_500_flat" => {
            "no spec target: one write of 500 docs and its landing (recorded; the < 5 ms write budget is for one op)"
        }
        o if (o.starts_with("write") && !o.starts_with("write_batch_"))
            || (o.starts_with("propose") && !o.starts_with("propose_land_")) =>
        {
            "< 5 ms p99 on Linux NVMe; one durable flush + < 1 ms on macOS (spec 11, restated per hardware class)"
        }
        "brief_doc" => "< 5 ms p99 at 10k with the view's graph carried (the read budget, spec 11)",
        "open_chosen_alts_100" => {
            "< 2 s (the cold-open budget, spec 11): open with 100 chosen alternative sets whose winner views wrote 50 changes each after the set was created"
        }
        "brief_expired_intents_1k" => {
            "< 5 ms p99 (the read budget, spec 11): a briefing with 1,000 expired intents and 5 open ones in the project"
        }
        "context" | "impact" | "resolve_path" => {
            return Some(format!(
                "< 5 ms p99 (spec 11){COLD_READ_NOTE}{}",
                if large { LARGE_TIER_NOTE } else { "" }
            ));
        }
        "version_position" | "tree_full" => "< 5 ms p99 (spec 11)",
        "version_position_cached" => "< 10 us p99 on the isolated Linux bench (vpos1)",
        "tree_full_memo" => {
            "< 5 ms p99 at 1k folders / 10k docs, memoised per canon head (M2 spec 15.1)"
        }
        "search_literal" => {
            "no spec target: a full scan of block text, first page (recorded for FW-PR1; semantic search is M5)"
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
        "lexicon_build" => {
            "no spec target: a cold build of the entity lexicon over a 10k view (recorded for FW-EN1)"
        }
        "entity_search" => {
            "< 5 ms p99 (a read, spec 11): an entity search with its fact sheet, first page"
        }
        "suggestions_after_write" => {
            "no spec target: the suggestions of a one-claim write over a carried lexicon (recorded for FW-EN3; they ride on every write response)"
        }
        "coverage_all" => {
            "no spec target: cold graph coverage over a 10k view (recorded for FW-EN4 and FW-EN1a; rdy status and rdy_overview)"
        }
        "context_inflight_1k" | "impact_inflight_1k" | "context_view_1k" | "impact_view_1k" => {
            "< 5 ms p99 with 1k in-flight changes (spec 15.1; the overlays must not regress them)"
        }
        "inflight_build_1k" | "coord_build_1k" => {
            "no spec target: the in-flight state's (coordination graph's) cold build, once per published state (recorded for M2d1)"
        }
        "inflight_carry" => "< 1 ms incremental (spec 15.1, view materialization)",
        "tension_carry_1k" | "tension_on_change_1k" => {
            "< 10 ms p99 with 1k in-flight changes (M2 spec 15.1: on_change and tension detection, included in the write response)"
        }
        "tension_on_change_delete_1k" | "tension_on_change_view_op_1k" => {
            "< 10 ms p99 with 1k in-flight changes (M2 spec 15.1: on_change, included in the response); a delete and a view op never carry, so this is the in-flight build plus a diff that skips the shared canon part"
        }
        "tension_settle_1k" => {
            "recorded, no spec target: the writer's post-reply settle (diff and events), compare with tension_carry_1k"
        }
        "tensions_full_1k" => {
            "no spec target: the cold build of every tension at 1k in-flight changes, once per landing or structural update (recorded for M2d2)"
        }
        "tensions_canon" => {
            "no spec target: the canon-scope build of the tensions (FW-CH6), once per landing; recorded for Coverage and confidence"
        }
        "gate_tensions_1k" => {
            "recorded: the land message's tension check on a built set (inside the landing budget, M2 spec 15.1)"
        }
        "mcp_params_9mib" => {
            "no spec target: parse_params::<WriteParams> alone on one ~9 MiB rdy_write call (recorded; the comparator for mcp_write_9mib)"
        }
        "mcp_write_9mib" => {
            "no spec target: one rdy_write of 2,002 ops (~9 MiB) through the MCP parameter path, consequences included (recorded; intake: the &Value parse and the deep clone of ops cost about +30 ms per 9 MiB)"
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
        "wc_project_typical" => {
            "recorded, no spec budget (M2f-a): the working copy: canon projected as files, whole tree"
        }
        "wc_plan_one_doc" => {
            "recorded, no spec budget (M2f-a): the working copy: one staged doc file planned for commit"
        }
        "history_100" | "last_changes_doc" | "log_page_20" => "< 5 ms p99 (reads, spec 11)",
        "log_full" => "recorded: 2000 landings, no spec target",
        // bounds the functional tests used to assert by the clock, moved here (a wall-clock bound
        // in a functional gate fails on a loaded machine, not on a defect); each keeps its figure
        "refuse_import_20mb_scalar" => {
            "< 5 s: an export with a 20 MB scalar refused by import (the old io bound)"
        }
        "refuse_check_20mb_scalar" => {
            "< 5 s: the same export reported by rdy check (the old io bound)"
        }
        "refuse_import_symlink" => {
            "< 5 s: an export with a symlink loop refused by import (the old io_tree bound)"
        }
        "blob_forged_manifest_refuse" => {
            "< 2 s: has_blob and get_blob of a forged 30,000-chunk manifest (the old store blob1 bound)"
        }
        "figure_forged_manifest_refuse" => {
            "< 2 s: a figure write naming a forged 30,000-chunk manifest refused (the old store blob1 bound)"
        }
        "pack_resolve_1000_areas" => {
            "< 2 s: 1,000 areas of 100 globs added and resolved (the old qapack1 bound)"
        }
        "areas_lookup_100k_globs" => {
            "< 500 ms: one path against 100k globs (the old qapack1 bound)"
        }
        "areas_lookup_pathological" => {
            "< 500 ms: 100 lookups of a 64-segment path against 60 `**` and 50 `*?` (the old qapack1 bound)"
        }
        "pack_resolve_2000_types" => {
            "< 2 s: a pack of 2,000 block types added and resolved (the old schema bound)"
        }
        "daemon_read_during_big_write" => {
            "< 50 ms: the worst status read on another connection while a write of the most ops one write takes runs (the old conc1 bound)"
        }
        "daemon_mcp_overview" => {
            "< 5 ms p99 (the read budget, spec 11): rdy_overview over an mcp connection to the daemon"
        }
        "mcp_stdio_overview" => {
            "< 2 s per call: rdy_overview over `rdy mcp --no-daemon` (the old mcp and mcp_pool bound)"
        }
        "mcp_stdio_write_max_ops" => {
            "< 5 s: the largest allowed write (5,000 ops) over `rdy mcp --no-daemon` (the old mcp2 bound)"
        }
        "mcp_stdio_oversized_line" => {
            "< 10 s: a 17 MiB request line sent and refused over `rdy mcp --no-daemon` (the old mcp2 bound)"
        }
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
        || op.starts_with("mcp_write")
}

pub fn compare(baseline: Option<&Baseline>, measured: &[Measure]) -> Vec<Verdict> {
    measured
        .iter()
        .map(|m| {
            let base = baseline.and_then(|b| b.ops.get(&m.op)).map(|b| b.median_ms);
            let margin = if fsync_bound(&m.op) { 0.50 } else { 0.25 };
            let regressed = base.is_some_and(|b| {
                m.median_ms > b * (1.0 + margin) && m.median_ms - b > floor_ms(&m.op)
            });
            let over_ceiling = spec_ceiling_ms(&m.op).is_some_and(|c| m.p95_ms > c);
            let pass = !regressed && !over_ceiling;
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
    // FW-PR1: a full scan of block text (a literal every generated claim holds), first page only
    let needle = rdy_engine::SearchQuery {
        text: "Claim".into(),
        ..Default::default()
    };
    out.push(measure(&format!("search_literal{sfx}"), runs.read, |_| {
        r.read()
            .search(&Scope::Canon, &needle, None, Some(4000))
            .expect("search");
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

/// FW-EN1, FW-EN3, FW-EN4: the entity ops on view `ve`: the fixture's canon plus 50 entities and
/// 200 claims that name them and share ten code literals. Run after `tier_views`: the view it
/// leaves behind would change the numbers of the ops before it. Run before `tier_inflight`,
/// which closes every open view (`ve` too) and adds 1k in-flight changes.
fn tier_entities(out: &mut Vec<Measure>, r: &mut Rdy, fx: &Fixture, sfx: &str, runs: &Runs) {
    let ve = Scope::View("ve".into());
    let section = fx.section.to_string();
    let ents: Vec<serde_json::Value> = (0..50)
        .map(|i| json!({"op": "create", "type": "entity", "parent": section, "fields": {"name": format!("Entity {i}"), "aliases": [format!("ent{i}")], "kind": "service"}}))
        .collect();
    write(r, "ve", serde_json::Value::Array(ents)).expect("entities");
    let claims: Vec<serde_json::Value> = (0..200)
        .map(|i| json!({"op": "create", "type": "claim", "parent": section, "body": format!("Entity {} talks to ent{} through `cfg_{}`.", i % 50, (i + 1) % 50, i % 10)}))
        .collect();
    write(r, "ve", serde_json::Value::Array(claims)).expect("claims");
    out.push(measure_timed(
        &format!("lexicon_build{sfx}"),
        runs.read,
        |_| {
            cold(r, |r| {
                r.read().lexicon(&ve).expect("lexicon");
            })
            .1
        },
    ));
    let q = rdy_engine::SearchQuery {
        text: "Entity 7".into(),
        ..Default::default()
    };
    out.push(measure(&format!("entity_search{sfx}"), runs.read, |_| {
        r.read().search(&ve, &q, None, Some(4000)).expect("search");
    }));
    out.push(measure_timed(
        &format!("suggestions_after_write{sfx}"),
        runs.write,
        |i| {
            let w = write(r, "ve", json!([{"op": "create", "type": "claim", "parent": section, "body": format!("Entity {} and ent{} use `cfg_1` ({i}).", i % 50, (i + 7) % 50)}]))
                .expect("write");
            let change: Hash = w.change.parse().expect("change");
            let s = Instant::now();
            r.read().suggestions(&ve, &change).expect("suggestions");
            s.elapsed()
        },
    ));
    out.push(measure_timed(
        &format!("coverage_all{sfx}"),
        runs.read,
        |_| {
            cold(r, |r| {
                r.read().graph_coverage(&ve, None).expect("coverage");
            })
            .1
        },
    ));
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
            let delta = rdy_engine::tensions::tension_delta(&before, &after, Some(&changes))
                .expect("tension delta");
            after
                .view_consequences(&before, &name, &head, Some(&changes), &delta, None)
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

/// Changes in flight for the `_1k` ops.
const INFLIGHT_N: usize = 1000;

/// Views the in-flight fixture spreads its changes over: `inf0` … `inf19`.
const INFLIGHT_VIEWS: usize = 20;

/// `n` single-op changes, round-robin across the views `inf0` … `inf19`, each an update of a
/// distinct claim of canon (the first `n` claim ids by id, skipping the fixture's claim); `inf0`
/// also updates the fixture's claim, so its impact crosses in-flight edits. Asserts the in-flight
/// set holds at least `n` changes.
fn inflight_views(r: &mut Rdy, fx: &Fixture, n: usize) {
    let mut ids: Vec<BlockId> = {
        let canon = r.read().materialized(&Scope::Canon).expect("canon");
        canon
            .iter()
            .filter(|b| b.block_type == "claim" && b.id != fx.claim)
            .map(|b| b.id.clone())
            .collect()
    };
    ids.sort();
    ids.truncate(n);
    assert_eq!(
        ids.len(),
        n,
        "canon has {n} claims to edit besides the fixture's"
    );
    write(
        r,
        "inf0",
        json!([{"op": "update", "id": fx.claim.to_string(), "body": "in flight claim"}]),
    )
    .expect("update the fixture claim in inf0");
    for (i, id) in ids.iter().enumerate() {
        write(
            r,
            &format!("inf{}", i % INFLIGHT_VIEWS),
            json!([{"op": "update", "id": id.to_string(), "body": format!("in flight {i}")}]),
        )
        .expect("in-flight write");
    }
    let held = r.read().inflight_set().expect("inflight set").changes.len();
    assert!(held >= n, "{held} changes in flight, wanted {n}");
}

/// The in-flight ops (M2d1): the cold build of the in-flight state, the carry of one clean commit,
/// the warm reads of `context` and `impact` in the `inflight` scope and in a view that holds
/// in-flight edits, and the coordination graph's cold build, with `n` changes in flight (the op
/// names say `1k`, as `tier_views` keeps `_200` on a small tree). The reads time a warm state: the
/// graph is built once per published state, and the cold build has its own op. Run after
/// `tier_views`, on the same project.
fn tier_inflight(
    out: &mut Vec<Measure>,
    r: &mut Rdy,
    fx: &Fixture,
    sfx: &str,
    runs: &Runs,
    n: usize,
) {
    // the views the earlier tiers left open hold conflicts (the landing views edit claims a landing
    // changed), and a conflicted in-flight build is never carried: close them, so the in-flight
    // state is the `inf*` views' and the carry is the one a clean state gets
    let open: Vec<String> = {
        let st = r.read();
        st.view_names()
            .into_iter()
            .filter(|v| st.view_head(v).is_some_and(|h| !h.closed))
            .collect()
    };
    r.view_close(
        &writer(),
        &open,
        "perf: the in-flight ops measure their own views",
    )
    .expect("close the views left open");
    inflight_views(r, fx, n);
    let inflight = Scope::Inflight;
    out.push(measure_timed(
        &format!("inflight_build_1k{sfx}"),
        runs.materialize_cold,
        |_| cold(r, |r| r.read().graph(&inflight).expect("inflight graph")).1,
    ));
    // the carry: the state is built (outside the timer), one clean commit into an in-flight view
    // seeds the next state, and the timed part is the read of it
    let section = fx.section.to_string();
    out.push(measure_timed(
        &format!("inflight_carry{sfx}"),
        runs.incremental,
        |i| {
            r.read().materialized(&inflight).expect("warm inflight");
            write(
                r,
                "inf1",
                json!([{"op": "create", "type": "claim", "parent": section, "body": format!("Carried {i}.")}]),
            )
            .expect("carried write");
            assert!(
                r.read().is_built(&inflight).0,
                "the commit did not carry the in-flight build"
            );
            let s = Instant::now();
            let built = r.read().materialized(&inflight).expect("inflight");
            let took = s.elapsed();
            assert!(!built.is_empty());
            took
        },
    ));
    // warm reads: the state and its graph are built once, before the runs
    let st = r.read();
    st.graph(&inflight).expect("inflight graph");
    out.push(measure_timed(
        &format!("context_inflight_1k{sfx}"),
        runs.read,
        |_| {
            let s = Instant::now();
            let c = st.context(&inflight, &fx.claim).expect("context");
            let took = s.elapsed();
            assert!(
                !c.inflight.is_empty(),
                "the fixture claim is edited in flight"
            );
            took
        },
    ));
    out.push(measure_timed(
        &format!("impact_inflight_1k{sfx}"),
        runs.read,
        |_| {
            let s = Instant::now();
            st.impact(&inflight, &fx.claim, 3).expect("impact");
            s.elapsed()
        },
    ));
    let inf0 = Scope::View("inf0".into());
    st.graph(&inf0).expect("inf0's graph");
    out.push(measure_timed(
        &format!("context_view_1k{sfx}"),
        runs.read,
        |_| {
            let s = Instant::now();
            st.context(&inf0, &fx.claim).expect("context");
            s.elapsed()
        },
    ));
    out.push(measure_timed(
        &format!("impact_view_1k{sfx}"),
        runs.read,
        |_| {
            let s = Instant::now();
            st.impact(&inf0, &fx.claim, 3).expect("impact");
            s.elapsed()
        },
    ));
    drop(st);
    out.push(measure_timed(
        &format!("coord_build_1k{sfx}"),
        runs.materialize_cold,
        |_| cold(r, |r| r.read().coord().expect("coord")).1,
    ));
}

/// Citing claims the tension ops add on top of the in-flight fixture.
const TENSION_CITING: usize = 50;

/// The tension ops (M2d2): the carry of the tension set across one commit, `on_change` for one
/// write, the writer's post-reply settle, the cold builds of the set and of its canon part, and
/// the land message's tension check, with `n` changes in flight (`1k` in the names, as in
/// [`tier_inflight`]). Run right after `tier_inflight`, on the same project: it reuses the `n`
/// changes left in `inf0` … `inf19` and adds `citing` claims written round-robin into those views,
/// each citing a claim another view updated, so premise shifts exist (asserts at least `citing`
/// tensions, untimed).
fn tier_tensions(
    out: &mut Vec<Measure>,
    r: &mut Rdy,
    fx: &Fixture,
    sfx: &str,
    runs: &Runs,
    n: usize,
    citing: usize,
) {
    // the claims `inflight_views` updated are the first `n` by id (besides the fixture's): each
    // `ids[i]` is in `inf{i % 20}`. A fresh claim is one past them.
    let ids: Vec<BlockId> = {
        let canon = r.read().materialized(&Scope::Canon).expect("canon");
        let mut v: Vec<BlockId> = canon
            .iter()
            .filter(|b| b.block_type == "claim" && b.id != fx.claim)
            .map(|b| b.id.clone())
            .collect();
        v.sort();
        v
    };
    let (edited, fresh) = ids.split_at(n);
    assert!(
        edited.len() > citing && fresh.len() >= 3 * runs.incremental,
        "canon has claims to cite and to edit besides the in-flight fixture's"
    );
    let section = fx.section.to_string();
    for k in 0..citing {
        // the claim `k + 1` is updated in the next view round-robin: never the citer's own
        write(
            r,
            &format!("inf{}", k % INFLIGHT_VIEWS),
            json!([{"op": "create", "type": "claim", "parent": section,
                "body": format!("Builds on {{{{{}}}}}.", edited[k + 1])}]),
        )
        .expect("a citing claim");
    }
    let held = r.read().tensions().expect("tensions").len();
    assert!(held >= citing, "{held} tensions, wanted {citing}");
    let mut fresh = fresh.iter();
    // the carry: the state's set is built (outside the timer), one update of a fresh canon claim
    // into `inf0` seeds the next state, and the timed part is the read of its set
    out.push(measure_timed(
        &format!("tension_carry_1k{sfx}"),
        runs.incremental,
        |i| {
            r.read().tensions().expect("warm tensions");
            let id = fresh.next().expect("a fresh claim");
            write(
                r,
                "inf0",
                json!([{"op": "update", "id": id.to_string(), "body": format!("Carried {i}.")}]),
            )
            .expect("carried write");
            let st = r.read();
            let s = Instant::now();
            st.tensions().expect("tensions");
            let took = s.elapsed();
            assert_eq!(
                st.tensions_carried(),
                Some(true),
                "the commit did not carry the tension set"
            );
            took
        },
    ));
    // `on_change` of one write, as the write response computes it: the tension delta between the
    // states before and after (their sets are built, the second one by the carry) and the
    // report for the view
    out.push(measure_timed(
        &format!("tension_on_change_1k{sfx}"),
        runs.incremental,
        |i| {
            let before = r.read();
            before.tensions().expect("warm tensions");
            let w = write(
                r,
                "inf1",
                json!([{"op": "create", "type": "claim", "parent": section,
                    "body": format!("Builds on {{{{{}}}}}. Run {i}.", edited[2])}]),
            )
            .expect("a citing write");
            let change: Hash = w.change.parse().expect("change");
            let after = r.read();
            let s = Instant::now();
            let delta = rdy_engine::tensions::tension_delta(&before, &after, Some(&[change]))
                .expect("delta");
            let o = rdy_engine::tensions::delta::on_change(&before, &after, "inf1", None, &delta)
                .expect("on_change");
            let took = s.elapsed();
            assert!(!o.tensions.is_empty(), "the write opened a premise shift");
            took
        },
    ));
    // `on_change` of a delete: a delete never carries (it takes a block out of reads the seeds
    // do not cover), so the response builds the in-flight part of the state after and diffs
    // it; the canon part both sets share is not copied or diffed
    out.push(measure_timed(
        &format!("tension_on_change_delete_1k{sfx}"),
        runs.incremental,
        |_| {
            let before = r.read();
            before.tensions().expect("warm tensions");
            let id = fresh.next().expect("a fresh claim");
            let w = write(r, "inf1", json!([{"op": "delete", "id": id.to_string()}]))
                .expect("a delete");
            let change: Hash = w.change.parse().expect("change");
            let after = r.read();
            let s = Instant::now();
            let delta = rdy_engine::tensions::tension_delta(&before, &after, Some(&[change]))
                .expect("delta");
            rdy_engine::tensions::delta::on_change(&before, &after, "inf1", None, &delta)
                .expect("on_change");
            let took = s.elapsed();
            assert_eq!(after.tensions_carried(), Some(false), "a delete rebuilds");
            took
        },
    ));
    // `on_change` of a view op: one of inf2's changes included into a fresh view (its in-flight
    // set changes, so the state after builds its in-flight part again)
    let human = writer();
    let inf2: Vec<String> = r
        .read()
        .included("inf2")
        .iter()
        .map(ToString::to_string)
        .collect();
    let picked = inf2.last().expect("inf2 holds changes").clone();
    out.push(measure_timed(
        &format!("tension_on_change_view_op_1k{sfx}"),
        runs.incremental,
        |i| {
            let name = format!("tvo{i}");
            let before = r.read();
            before.tensions().expect("warm tensions");
            let res = r
                .view_include(&human, &name, std::slice::from_ref(&picked))
                .expect("include");
            let changes: Vec<Hash> = res
                .added
                .iter()
                .chain(&res.added_deps)
                .map(|h| h.parse().expect("change"))
                .collect();
            let after = r.read();
            let s = Instant::now();
            let delta = rdy_engine::tensions::tension_delta(&before, &after, Some(&changes))
                .expect("delta");
            rdy_engine::tensions::delta::on_change(&before, &after, &name, None, &delta)
                .expect("on_change");
            s.elapsed()
        },
    ));
    // the settle: the writer diffs the last settled set and the published one after the reply
    r.settle_tensions().expect("first settle");
    out.push(measure_timed(
        &format!("tension_settle_1k{sfx}"),
        runs.incremental,
        |_| {
            let id = fresh.next().expect("a fresh claim");
            write(
                r,
                "inf0",
                json!([{"op": "update", "id": id.to_string(), "body": "Settled."}]),
            )
            .expect("settled write");
            let s = Instant::now();
            r.settle_tensions().expect("settle");
            s.elapsed()
        },
    ));
    out.push(measure_timed(
        &format!("tensions_full_1k{sfx}"),
        runs.materialize_cold,
        |_| cold(r, |r| r.read().tensions_rebuilt().expect("tensions").len()).1,
    ));
    out.push(measure_timed(
        &format!("tensions_canon{sfx}"),
        runs.materialize_cold,
        |_| {
            cold(r, |r| {
                r.read()
                    .canon_tensions_rebuilt()
                    .expect("canon tensions")
                    .len()
            })
            .1
        },
    ));
    // the land message's check, on a built set
    let st = r.read();
    st.tensions().expect("tensions");
    let mine: Vec<Hash> = st.included("inf2").to_vec();
    assert!(!mine.is_empty(), "inf2 holds changes");
    out.push(measure_timed(
        &format!("gate_tensions_1k{sfx}"),
        runs.read,
        |_| {
            let s = Instant::now();
            st.proposal_tensions(
                "inf2",
                &mine,
                &BTreeSet::new(),
                rdy_engine::PROPOSAL_TENSIONS_MAX,
            )
            .expect("proposal tensions");
            s.elapsed()
        },
    ));
}

/// Intents and alternative sets ever made in the coordination ops' projects: none is ever
/// deleted (an intent expires at read time; a set is settled, not removed), so a swarm's count
/// grows for the project's lifetime.
const COORD_N: usize = 1000;

/// Open intents of another session on the written claim, beside the expired ones.
const COORD_OPEN_INTENTS: usize = 5;

/// A fresh small project (2 folders x 3 docs x 12 claims), landed, in `parent/<name>`.
fn coord_project(parent: &std::path::Path, name: &str) -> (std::path::PathBuf, Rdy, Fixture) {
    let proj = parent.join(name);
    std::fs::create_dir_all(&proj).expect("mkdir");
    let docs = testkit::generate_tree(&proj, 2, 3, 12);
    let mut r = Rdy::open(&proj).expect("open");
    land(&mut r, "work").expect("land work");
    let fx = fixture(&r, &docs);
    (proj, r, fx)
}

/// One `write_and_report` (the MCP and CLI write: consequences and intent notes included) of an
/// update of `claim` in view `view` by `s`.
fn reported_write(
    engine: &rdy_engine::engine::Local,
    s: &rdy_engine::Session,
    view: &str,
    claim: &BlockId,
    body: String,
) -> serde_json::Value {
    let (s, v) = (s.clone(), view.to_string());
    let ops =
        serde_json::from_value(json!([{"op": "update", "id": claim.to_string(), "body": body}]))
            .expect("ops");
    rdy_engine::consequence::write_and_report(engine, view, None, move |r| {
        r.write(
            &s,
            rdy_engine::WriteRequest {
                view: v,
                ops,
                justification: None,
                message: String::new(),
            },
        )
    })
    .expect("reported write")
}

/// The coordination records at scale (final-review fix round 1): a write (consequences and
/// intent notes) and a briefing in a project where `n` intents have expired and
/// [`COORD_OPEN_INTENTS`] are open, and a write in a project with `n` abandoned alternative sets.
/// Each runs on a fresh small project, so only the records it adds differ from a plain write or
/// briefing. Spec targets: the write and the read budgets.
fn tier_coord(out: &mut Vec<Measure>, parent: &std::path::Path, sfx: &str, runs: &Runs, n: usize) {
    let (_proj, mut r, fx) = coord_project(parent, "coord-intents");
    let a = crate::fixtures::agent("agent:a", Some("ra"));
    let claim = [fx.claim.to_string()];
    {
        // posted long ago with the shortest ttl: every one has expired at any real `now`
        let _clock = rdy_engine::pin_clock(1_000_000);
        for i in 0..n {
            r.intent_post(&a, None, None, &claim, &format!("old {i}"), Some(60_000))
                .expect("an intent that expires");
        }
    }
    for i in 0..COORD_OPEN_INTENTS {
        r.intent_post(&a, None, None, &claim, &format!("open {i}"), None)
            .expect("an open intent");
    }
    let engine = rdy_engine::engine::Local::new(r);
    let b = crate::fixtures::agent("agent:b", Some("rb"));
    out.push(measure_timed(
        &format!("write_expired_intents_1k{sfx}"),
        runs.write,
        |i| {
            let s = Instant::now();
            let v = reported_write(&engine, &b, "rb", &fx.claim, format!("Noted {i}."));
            let took = s.elapsed();
            assert_eq!(
                v["intents"].as_array().map(Vec::len),
                Some(COORD_OPEN_INTENTS),
                "the open intents only: {v}"
            );
            took
        },
    ));
    let st = rdy_engine::engine::Engine::read(&engine);
    let rb = Scope::View("rb".into());
    let cov = Coverage {
        ids: vec![fx.claim.to_string()],
        path: None,
    };
    st.brief(&rb, &cov, None).expect("warm the briefing");
    out.push(measure_timed(
        &format!("brief_expired_intents_1k{sfx}"),
        runs.read,
        |_| {
            let s = Instant::now();
            let brief = st.brief(&rb, &cov, None).expect("brief");
            let took = s.elapsed();
            assert_eq!(brief.inflight.intents.len(), COORD_OPEN_INTENTS);
            took
        },
    ));
    drop(st);
    drop(engine);

    let (_proj, mut r, fx) = coord_project(parent, "coord-alts");
    let o = crate::fixtures::agent("agent:o", Some("o"));
    for i in 0..n {
        let (p, q) = (format!("p{i}"), format!("q{i}"));
        for v in [&p, &q] {
            r.view_create(&writer(), v, None, false)
                .expect("a member view");
        }
        let members = [
            rdy_engine::alt::AltMemberInput {
                label: "p".into(),
                view: p,
            },
            rdy_engine::alt::AltMemberInput {
                label: "q".into(),
                view: q,
            },
        ];
        let set = r
            .alt_create(&o, &format!("old {i}"), None, &members)
            .expect("a set");
        r.alt_abandon(&o, &set.id, "superseded").expect("abandon");
    }
    assert!(r.read().coord_heads().open_alts.is_empty());
    let engine = rdy_engine::engine::Local::new(r);
    out.push(measure(
        &format!("write_settled_alts_1k{sfx}"),
        runs.write,
        |i| {
            reported_write(&engine, &b, "rb", &fx.claim, format!("Settled {i}."));
        },
    ));
    drop(engine);

    tier_coord_chosen(out, parent, sfx, runs, n);
}

/// msw20 (fix round 1): `n / 10` chosen alternative sets, each {w: w<i>, l: l<i>} chosen for w,
/// whose winner view wrote `n / 20` (at least 5) changes after the set was created, in a fresh
/// small project. `open_chosen_alts_100` reopens it: `CoordHeads::read` derives every chosen
/// set's bindings from history, and each member's walk runs from its head back to the set's
/// creation. `propose_chosen_alts_100` proposes a view of the same number of changes there: the
/// gate checks each change against every settled set.
fn tier_coord_chosen(
    out: &mut Vec<Measure>,
    parent: &std::path::Path,
    sfx: &str,
    runs: &Runs,
    n: usize,
) {
    let (sets, chain) = (n / 10, (n / 20).max(5));
    let (proj, mut r, fx) = coord_project(parent, "coord-chosen");
    let o = crate::fixtures::agent("agent:o", Some("o"));
    let claim = fx.claim.to_string();
    let edit = |r: &mut Rdy, view: &str, body: String| -> String {
        write(
            r,
            view,
            json!([{"op": "update", "id": claim, "body": body}]),
        )
        .expect("a member write")
        .change
    };
    for i in 0..sets {
        let (w, l) = (format!("w{i}"), format!("l{i}"));
        for v in [&w, &l] {
            r.view_create(&writer(), v, None, false)
                .expect("a member view");
        }
        let members = [
            rdy_engine::alt::AltMemberInput {
                label: "w".into(),
                view: w.clone(),
            },
            rdy_engine::alt::AltMemberInput {
                label: "l".into(),
                view: l,
            },
        ];
        let set = r
            .alt_create(&o, &format!("chosen {i}"), None, &members)
            .expect("a set");
        for k in 0..chain {
            edit(&mut r, &w, format!("Winner {i}.{k}."));
        }
        let pick = rdy_engine::alt::AltPick {
            set: set.id.clone(),
            chosen: "w".into(),
        };
        let p = r
            .propose_with(&writer(), &w, &w, None, None, Some(&pick))
            .expect("a choosing proposal");
        r.land(&approver(), &p.id).expect("the choosing landing");
    }
    let source: Vec<String> = (0..chain)
        .map(|k| edit(&mut r, "ps", format!("Proposed {k}.")))
        .collect();
    let chosen = r
        .read()
        .coord_heads()
        .alt_binds
        .values()
        .filter(|b| b.chosen.is_some())
        .count();
    assert_eq!(chosen, sets, "every set is chosen");
    drop(r);
    out.push(measure(
        &format!("open_chosen_alts_100{sfx}"),
        runs.materialize_cold,
        |_| {
            let r = Rdy::open(&proj).expect("open");
            assert_eq!(r.read().coord_heads().alt_binds.len(), sets);
        },
    ));
    let mut r = Rdy::open(&proj).expect("open");
    out.push(measure_timed(
        &format!("propose_chosen_alts_100{sfx}"),
        runs.write,
        |i| {
            let name = format!("pp{i}");
            r.view_include(&writer(), &name, &source)
                .expect("include the proposal's changes");
            let s = Instant::now();
            r.propose(&writer(), &name, &name).expect("propose");
            s.elapsed()
        },
    ));
}

/// Ops in one `mcp_write_args` call: a doc, its section and 2,000 claims.
const MCP_CLAIMS: usize = 2000;

/// The arguments of one large `rdy_write` (about 9.2 MiB serialized): a doc `mw-<i>`, a section
/// and [`MCP_CLAIMS`] claims of 4,700 ASCII characters each, in view `mw`.
fn mcp_write_args(i: usize) -> serde_json::Map<String, serde_json::Value> {
    let body = "lorem ipsum dolor sit amet ".repeat(4700 / 27 + 1)[..4700].to_string();
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"name": format!("mw-{i}"), "title": "MW", "doc_type": "glossary"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}}),
    ];
    // each claim's body is its own (the run index and the claim index lead it): every run
    // writes into the same view `mw`, so a body that depended on `k` alone would duplicate
    // claim `k` of every earlier run. This op measures the ~9 MiB intake and parameter path,
    // not duplicate definitions
    ops.extend((0..MCP_CLAIMS).map(|k| {
        json!({"op": "create", "type": "claim", "parent": "$s", "body": format!("{i}-{k} {body}")})
    }));
    match json!({"view": "mw", "ops": ops}) {
        serde_json::Value::Object(m) => m,
        _ => unreachable!("an object literal"),
    }
}

/// The MCP parameter path on one ~9 MiB `rdy_write` (the intake follow-up: parameters
/// deserialize from a `&Value`, and `rdy_write` clones every op): `parse_params` alone, then
/// parse plus the write, consequences included. Opens the project on its own handle; run last,
/// because its 6,000 blocks would move the ops before it.
fn tier_mcp(out: &mut Vec<Measure>, proj: &std::path::Path) {
    use rdy_mcp::params::parse_params;
    use rdy_mcp::tools::{McpState, WriteParams, rdy_session, rdy_write};
    let r = Rdy::open(proj).expect("open");
    let mut st = McpState::new(std::sync::Arc::new(rdy_engine::engine::Local::new(r)));
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:perf", "run": "mw", "role": "writer"}))
            .expect("session params"),
    )
    .expect("session");
    let size = serde_json::to_vec(&mcp_write_args(0))
        .expect("serialize")
        .len();
    assert!(size >= 9 * 1024 * 1024, "{size} bytes is not ~9 MiB");
    assert!(
        size < rdy_mcp::tools::MAX_REQUEST_BYTES,
        "{size} bytes is over the request limit"
    );
    out.push(measure_timed("mcp_params_9mib", 3, |i| {
        let args = mcp_write_args(i);
        let s = Instant::now();
        let p = parse_params::<WriteParams>("rdy_write", args).expect("parse");
        let took = s.elapsed();
        assert_eq!(p.ops.len(), MCP_CLAIMS + 2);
        took
    }));
    out.push(measure_timed("mcp_write_9mib", 3, |i| {
        let args = mcp_write_args(i);
        let s = Instant::now();
        let p = parse_params::<WriteParams>("rdy_write", args).expect("parse");
        rdy_write(&mut st, p).expect("rdy_write");
        s.elapsed()
    }));
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

/// The working copy: the whole of canon projected as files (`wc_project_typical`), and the plan
/// of a commit that stages one edited doc file (`wc_plan_one_doc`). The checkout is written once
/// into a temporary directory, outside every timer; each plan run edits the first body in one
/// doc file, stages it and plans the commit. Typical tier only.
fn tier_wc(out: &mut Vec<Measure>, r: &Rdy, dir: &std::path::Path) {
    use rdy_engine::wc::files::checkout;
    use rdy_engine::wc::ignore::Ignore;
    use rdy_engine::wc::layout::{FileKind, read_tree};
    use rdy_engine::wc::plan::plan_commit;
    use rdy_engine::wc::state::{Place, WcState, stage_put};
    let st = r.read();
    out.push(measure("wc_project_typical", 10, |_| {
        let p = st.wc_project(&Scope::Canon, None).expect("project canon");
        assert!(!p.files.is_empty());
    }));
    // setup, untimed: a checkout, and the doc file to edit
    let root = dir.join("wc");
    std::fs::create_dir_all(&root).expect("mkdir");
    let place = Place::main(&root);
    let mut state = WcState::load(&place).expect("load the state");
    let pack = st.pack().clone();
    let tree = read_tree(&root, &Ignore::empty(), &pack);
    let proj = st.wc_project(&Scope::Canon, None).expect("project canon");
    checkout(&place, &mut state, &tree, &proj, false).expect("checkout");
    let file = proj
        .files
        .iter()
        .find(|f| f.kind == FileKind::Doc)
        .expect("a doc file");
    let mut entries: Vec<serde_yaml_ng::Value> =
        serde_yaml_ng::from_str(&file.text).expect("a doc file is a list");
    out.push(measure_timed("wc_plan_one_doc", 10, |i| {
        let entry = entries
            .iter_mut()
            .find_map(|e| e.get_mut("body"))
            .expect("an entry with a body");
        *entry = serde_yaml_ng::Value::String(format!("Edited in the working copy, run {i}."));
        let text = serde_yaml_ng::to_string(&entries).expect("yaml");
        let s = Instant::now();
        stage_put(&place, &file.path, text.as_bytes()).expect("stage");
        let plan = plan_commit(&place, &state, &pack).expect("plan the commit");
        let took = s.elapsed();
        assert!(!plan.ops.is_empty());
        took
    }));
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
    tier_wc(&mut out, &r, dir.path());
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
    out.extend(daemon_mcp_on_copy(&proj, dir.path()));
    out.extend(bound_ops(dir.path()));
    out.extend(stdio_mcp_ops(dir.path()));
    // the view ops last: the views and landings they leave behind would move the ops above
    let mut r = Rdy::open(&proj).expect("open");
    tier_views(&mut out, &mut r, &fx, "", &TYPICAL_RUNS);
    // after the view ops (the view it leaves behind would move the ops before it), before the
    // in-flight ops (which close it and add 1k in-flight changes)
    tier_entities(&mut out, &mut r, &fx, "", &TYPICAL_RUNS);
    tier_inflight(&mut out, &mut r, &fx, "", &TYPICAL_RUNS, INFLIGHT_N);
    tier_tensions(
        &mut out,
        &mut r,
        &fx,
        "",
        &TYPICAL_RUNS,
        INFLIGHT_N,
        TENSION_CITING,
    );
    drop(r);
    // on projects of their own: the records they add move nothing above
    tier_coord(&mut out, dir.path(), "", &TYPICAL_RUNS, COORD_N);
    // the large `rdy_write` last, on a reopened project: its 6,000 blocks move nothing above
    tier_mcp(&mut out, &proj);
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

/// Copies the project at `from` to `to` (no handle may be open on it), leaving out `.rdy/run`.
fn copy_project(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).expect("mkdir");
    for e in std::fs::read_dir(from).expect("read_dir") {
        let p = e.expect("entry").path();
        let name = p.file_name().expect("a name");
        if name == "run"
            && p.parent()
                .and_then(|d| d.file_name())
                .is_some_and(|d| d == ".rdy")
        {
            continue;
        }
        let t = to.join(name);
        if p.is_dir() {
            copy_project(&p, &t);
        } else {
            std::fs::copy(&p, &t).expect("copy");
        }
    }
}

/// The daemon's mcp ops ([`daemon_mcp_ops`]) on a copy of the typical project under `parent`:
/// their big writes would otherwise grow the project the later ops (and the cold starts) open.
fn daemon_mcp_on_copy(proj: &std::path::Path, parent: &std::path::Path) -> Vec<Measure> {
    let copy = parent.join("p-mcp");
    copy_project(proj, &copy);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("client runtime");
    let run = daemon_start(&copy);
    let mut c = rt.block_on(daemon_ready(&copy));
    rt.block_on(c.ctl("session", json!({"actor": null, "run": null})))
        .unwrap_or_else(|e| panic!("the session op failed: {e}"));
    let out = rt.block_on(daemon_mcp_ops(&copy, &mut c));
    drop(c);
    daemon_stop(run);
    out
}

/// One JSON-RPC line out, and the reply with `id` back (other lines are skipped).
async fn mcp_call(
    w: &mut tokio::net::unix::OwnedWriteHalf,
    r: &mut tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>,
    id: u64,
    tool: &str,
    args: serde_json::Value,
) -> serde_json::Value {
    mcp_send(w, id, tool, args).await;
    mcp_reply(r, id).await
}

async fn mcp_send(
    w: &mut tokio::net::unix::OwnedWriteHalf,
    id: u64,
    tool: &str,
    args: serde_json::Value,
) {
    use tokio::io::AsyncWriteExt;
    let line = json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": tool, "arguments": args}});
    w.write_all(format!("{line}\n").as_bytes())
        .await
        .expect("mcp write");
}

async fn mcp_reply(
    r: &mut tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>,
    id: u64,
) -> serde_json::Value {
    use tokio::io::AsyncBufReadExt;
    loop {
        let mut line = String::new();
        let n = r.read_line(&mut line).await.expect("mcp read");
        assert!(n > 0, "the daemon closed the mcp connection");
        let v: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
        if v["id"] == json!(id) {
            return v;
        }
    }
}

/// The ops over an `mcp` connection to the daemon at `proj` (typical tier only), with `c` a
/// `ctl` connection that has a session: `daemon_mcp_overview` (one `rdy_overview` round trip)
/// and `daemon_read_during_big_write` (a `status` read on `c` while the mcp connection's write
/// of `MAX_OPS_PER_WRITE` claims runs on the writer; one write per run, its reply read after
/// the timed read).
async fn daemon_mcp_ops(proj: &std::path::Path, c: &mut Client) -> Vec<Measure> {
    use tokio::io::AsyncWriteExt;
    let hello = Hello {
        rdy: PROTO_VERSION,
        version: "perf".into(),
        proto: Proto::Mcp,
        cwd: proj.to_path_buf(),
    };
    let stream = match rdy_daemon::connect(&RunDir::of_project(proj), &hello).await {
        Attempt::Ready(c) => c.into_stream(),
        other => panic!("the daemon did not answer the mcp hello: {other:?}"),
    };
    let (r, mut w) = stream.into_split();
    let mut r = tokio::io::BufReader::new(r);
    let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": crate::mcp::PROTOCOL_VERSION, "capabilities": {}, "clientInfo": {"name": "perf", "version": "0"}}});
    w.write_all(format!("{init}\n").as_bytes())
        .await
        .expect("mcp write");
    mcp_reply(&mut r, 1).await;
    w.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
        .await
        .expect("mcp write");
    let mut id = 1;
    let mut next = || {
        id += 1;
        id
    };
    mcp_call(
        &mut w,
        &mut r,
        next(),
        "rdy_session",
        json!({"actor": "agent:perf", "role": "writer"}),
    )
    .await;
    let made = mcp_call(&mut w, &mut r, next(), "rdy_write", json!({"ops": [
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "perf-big", "title": "B", "doc_type": "constraints"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}}
    ]})).await;
    let text = made["result"]["content"][0]["text"]
        .as_str()
        .expect("the write's result");
    let made: serde_json::Value = serde_json::from_str(text).expect("the write's json");
    let section = made["created"]["$s"]
        .as_str()
        .expect("the section id")
        .to_string();
    let mut out = vec![];
    let mut overview = vec![];
    for _ in 0..30 {
        let n = next();
        let s = Instant::now();
        mcp_call(&mut w, &mut r, n, "rdy_overview", json!({})).await;
        overview.push(s.elapsed());
    }
    let mut overview = overview.into_iter();
    out.push(measure_timed("daemon_mcp_overview", 30, |_| {
        overview.next().expect("a run")
    }));
    // per run: the worst of the status reads made until the write's reply arrives (the reply
    // is awaited on its own task, so every read but possibly the last overlaps the write)
    let mut during = vec![];
    let mut reads = 0usize;
    for run in 0..20 {
        let ops: Vec<_> = (0..rdy_mcp::tools::MAX_OPS_PER_WRITE)
            .map(|i| json!({"op": "create", "type": "claim", "parent": section, "body": format!("Big {run} claim {i}.")}))
            .collect();
        let n = next();
        mcp_send(&mut w, n, "rdy_write", json!({"ops": ops})).await;
        let reply = tokio::spawn(async move {
            let v = mcp_reply(&mut r, n).await;
            (r, v)
        });
        let mut worst = Duration::ZERO;
        loop {
            let s = Instant::now();
            c.ctl("status", json!({})).await.expect("status");
            worst = worst.max(s.elapsed());
            reads += 1;
            if reply.is_finished() {
                break;
            }
            // a pause between reads, outside the timed region: the reads sample the write
            // rather than flood the daemon
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let (back, v) = reply.await.expect("the reply task");
        r = back;
        assert!(v["result"].is_object(), "the big write: {v}");
        during.push(worst);
    }
    eprintln!("daemon_read_during_big_write: {reads} status reads over 20 big writes");
    let mut during = during.into_iter();
    out.push(measure_timed("daemon_read_during_big_write", 20, |_| {
        during.next().expect("a run")
    }));
    out
}

/// The ops whose bounds the functional tests used to assert by the clock: hostile exports
/// refused, forged blob manifests refused, big packs resolved and their area lookups. Each on
/// its own fixture under `parent`, built outside the timed region.
fn bound_ops(parent: &std::path::Path) -> Vec<Measure> {
    use rdy_schema::{MAX_PATH_SEGMENTS, PackSet};
    let mut out = vec![];
    const DOC: &str = "- id: d-AAAAA\n  type: doc\n  order: a1\n  fields:\n    title: T\n    doc_type: glossary\n- id: s-AAAAA\n  type: section\n  parent: d-AAAAA\n  order: a1\n  fields:\n    title: Terms\n";
    let export = |name: &str| {
        let t = parent.join(name);
        std::fs::create_dir_all(t.join("docs")).expect("mkdir");
        std::fs::write(
            t.join("rdy-export.yaml"),
            "format: 2\npack: rdy-standard\nscope: canon\n",
        )
        .expect("manifest");
        t
    };
    // a 20 MB scalar in a doc file that does not validate
    let big = export("bound-big");
    std::fs::write(
        big.join("docs/big.yaml"),
        format!(
            "{DOC}- id: c-AAAAA\n  type: claim\n  parent: s-AAAAA\n  order: a2\n  body: \"{}\"\n  fields:\n    bogus: x\n",
            "x".repeat(20_000_000)
        ),
    )
    .expect("big doc");
    // a symlink loop under docs/
    let link = export("bound-link");
    std::os::unix::fs::symlink(link.join("docs"), link.join("docs/loop")).expect("symlink");
    let pack = PackSet::with_builtin()
        .resolve("rdy-standard")
        .expect("rdy-standard");
    for (op, dir) in [
        ("refuse_import_20mb_scalar", &big),
        ("refuse_import_symlink", &link),
    ] {
        out.push(measure_timed(op, 5, |i| {
            let proj = parent.join(format!("{op}-{i}"));
            std::fs::create_dir_all(&proj).expect("mkdir");
            let mut r = Rdy::init(&proj, "rdy-standard").expect("init");
            let s = Instant::now();
            r.import(&approver(), "import", dir)
                .expect_err("a hostile export is refused");
            s.elapsed()
        }));
    }
    out.push(measure("refuse_check_20mb_scalar", 5, |_| {
        rdy_engine::check_path(&pack, None, &big).expect("check runs");
    }));
    // forged chunk-kind manifests: 30,000 chunks of 1 MiB named by a manifest an agent uploaded
    {
        use rdy_store::kinds::{self, BlobManifest};
        const MIB: usize = 1024 * 1024;
        let proj = parent.join("bound-blob");
        std::fs::create_dir_all(&proj).expect("mkdir");
        let mut r = Rdy::init(&proj, "rdy-standard").expect("init");
        let made = write(&mut r, "work", json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Tx", "doc_type": "glossary"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}}
        ]))
        .expect("doc");
        let section = made.created["$s"].to_string();
        let chunk = r
            .store_mut()
            .write(vec![(kinds::BLOB_CHUNK, vec![9u8; MIB])])
            .expect("chunk")[0];
        let n = 30_000usize;
        let forged = postcard::to_allocvec(&BlobManifest {
            len: (n * MIB) as u64,
            chunks: vec![chunk; n],
        })
        .expect("manifest");
        let h = r
            .store_mut()
            .write(vec![(kinds::BLOB_CHUNK, forged)])
            .expect("forged")[0];
        out.push(measure("blob_forged_manifest_refuse", 5, |_| {
            assert!(!r.store().has_blob(&h).expect("has_blob"));
            r.store().get_blob(&h).expect_err("a forged manifest");
        }));
        out.push(measure("figure_forged_manifest_refuse", 5, |_| {
            let e = write(&mut r, "work", json!([{"op": "create", "type": "figure", "parent": section, "fields": {"blob": h.to_string(), "media_type": "image/png", "alt": "x"}}]))
                .expect_err("a figure naming a forged manifest");
            assert_eq!(e.code, "E_VALIDATION", "{e}");
        }));
    }
    // a thousand areas of a hundred globs each
    let mut areas = String::from("name: big\nversion: \"1\"\nextends: rdy-standard\nareas:\n");
    for a in 0..1000 {
        areas.push_str(&format!("  area{a}:\n    paths: ["));
        for g in 0..100 {
            areas.push_str(&format!("'f{a}/**/d{g}', "));
        }
        areas.push_str("]\n");
    }
    let mut resolved = None;
    out.push(measure("pack_resolve_1000_areas", 5, |_| {
        let mut set = PackSet::with_builtin();
        set.add_yaml(&areas).expect("areas yaml");
        resolved = Some(set.resolve("big").expect("resolve"));
    }));
    let p = resolved.expect("resolved at least once");
    out.push(measure("areas_lookup_100k_globs", 20, |_| {
        assert_eq!(p.areas_for_path("f999/x/y/d99"), vec!["area999"]);
    }));
    let mut set = PackSet::with_builtin();
    let pat = format!("{}/a", vec!["**"; 60].join("/"));
    set.add_yaml(&format!(
        "name: p\nversion: \"1\"\nextends: rdy-standard\nareas:\n  a: {{ paths: ['{pat}', '{}a'] }}\n",
        "*?".repeat(50)
    ))
    .expect("pathological yaml");
    let p = set.resolve("p").expect("resolve");
    let path = vec!["b"; MAX_PATH_SEGMENTS].join("/");
    out.push(measure("areas_lookup_pathological", 5, |_| {
        for _ in 0..100 {
            assert!(p.areas_for_path(&path).is_empty());
        }
    }));
    // two thousand block types
    let letters = b"abcdefghijklmnopqrstuvwxyz";
    let mut types = String::from("name: big\nversion: \"1\"\nblock_types:\n");
    for i in 0..2000 {
        let prefix = format!(
            "{}{}{}",
            letters[i % 26] as char,
            letters[(i / 26) % 26] as char,
            letters[(i / 676) % 26] as char
        );
        types.push_str(&format!("  t{i}: {{ prefix: {prefix} }}\n"));
    }
    out.push(measure("pack_resolve_2000_types", 5, |_| {
        let mut set = PackSet::with_builtin();
        let n = set.add_yaml(&types).expect("the 2,000-type pack is added");
        let p = set.resolve(&n).expect("the 2,000-type pack resolves");
        assert!(p.block_types.len() >= 2000);
    }));
    out
}

/// The ops over `rdy mcp --no-daemon` (the stdio server the mcp and mcp_pool tests drive), on
/// a fresh project under `parent`: `mcp_stdio_overview` (one `rdy_overview` call),
/// `mcp_stdio_write_max_ops` (a write of `MAX_OPS_PER_WRITE` claims) and
/// `mcp_stdio_oversized_line` (a 17 MiB request line, sent and refused). Each run is timed from
/// the send to the reply.
fn stdio_mcp_ops(parent: &std::path::Path) -> Vec<Measure> {
    use crate::mcp::{McpClient, Reply};
    let budget = Duration::from_secs(120);
    let proj = parent.join("stdio");
    std::fs::create_dir_all(&proj).expect("mkdir");
    drop(Rdy::init(&proj, "rdy-standard").expect("init"));
    let mut c = McpClient::start(&proj);
    c.call_within(
        "rdy_session",
        json!({"actor": "agent:perf", "role": "writer"}),
        budget,
    )
    .ok();
    let made = c
        .call_within("rdy_write", json!({"ops": [
            {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Big", "doc_type": "glossary"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}}
        ]}), budget)
        .ok();
    let section = made["created"]["$s"]
        .as_str()
        .expect("the section id")
        .to_string();
    let mut out = vec![];
    out.push(measure("mcp_stdio_overview", 30, |_| {
        assert!(matches!(
            c.call_within("rdy_overview", json!({}), budget),
            Reply::Ok(_)
        ));
    }));
    out.push(measure_timed("mcp_stdio_write_max_ops", 5, |run| {
        let ops: Vec<_> = (0..rdy_mcp::tools::MAX_OPS_PER_WRITE)
            .map(|i| json!({"op": "create", "type": "claim", "parent": section, "body": format!("Max {run} claim {i}.")}))
            .collect();
        let args = json!({"ops": ops});
        let s = Instant::now();
        let r = c.call_within("rdy_write", args, budget);
        let took = s.elapsed();
        assert!(matches!(r, Reply::Ok(_)), "the largest write: {r:?}");
        took
    }));
    let big = "x".repeat(17 * 1024 * 1024);
    out.push(measure_timed("mcp_stdio_oversized_line", 3, |_| {
        let id = c.next_id();
        let line = format!(
            r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"rdy_write","arguments":{{"message":"{big}","ops":[]}}}}}}"#
        );
        let s = Instant::now();
        c.send_raw_within(&line, budget);
        let reply = c.recv(id, budget);
        let took = s.elapsed();
        assert!(
            reply.to_string().contains("rdy_request_too_large"),
            "{reply}"
        );
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
    tier_inflight(&mut out, &mut r, &fx, sfx, &LARGE_RUNS, INFLIGHT_N);
    tier_tensions(
        &mut out,
        &mut r,
        &fx,
        sfx,
        &LARGE_RUNS,
        INFLIGHT_N,
        TENSION_CITING,
    );
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
            compare(None, &[m("log_full", 1e9)])[0].pass,
            "no baseline and no spec ceiling passes"
        );
    }

    #[test]
    fn msw6_a_sub_floor_read_that_regresses_many_fold_fails() {
        let base = Baseline {
            machine: "x".into(),
            ops: [
                ("tree_full_memo".to_string(), m("tree_full_memo", 0.0001)),
                ("brief_doc".to_string(), m("brief_doc", 0.20)),
                ("write_single".to_string(), m("write_single", 0.6)),
            ]
            .into(),
        };
        let v = compare(
            Some(&base),
            &[
                m("tree_full_memo", 1.3),
                m("brief_doc", 0.22),
                m("write_single", 2.4),
            ],
        );
        assert!(
            !v[0].pass,
            "0.0001 -> 1.3 ms is a 13,000x regression: {:?}",
            v[0]
        );
        assert!(v[1].pass, "+10% is inside the margin");
        assert!(
            v[2].pass,
            "an fsync op keeps its 2 ms disk-noise floor (ruling R5)"
        );
    }

    #[test]
    fn msw6_a_spec_ceiling_fails_with_no_baseline_and_inside_the_margin() {
        let over = Measure {
            op: "tree_full".into(),
            median_ms: 4.2,
            p95_ms: 5.5,
            runs: 20,
        };
        assert!(
            !compare(None, std::slice::from_ref(&over))[0].pass,
            "p95 over the 5 ms ceiling, no baseline"
        );
        let base = Baseline {
            machine: "x".into(),
            ops: [(
                "tree_full".to_string(),
                Measure {
                    median_ms: 4.0,
                    ..over.clone()
                },
            )]
            .into(),
        };
        assert!(
            !compare(Some(&base), &[over])[0].pass,
            "+5% is inside the margin but over the ceiling"
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

    /// The in-flight ops run end to end on a small tree (n scaled down to the tree's claims).
    #[test]
    fn d1perf1_the_inflight_ops_run_on_a_small_tree() {
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
        let mut out = vec![];
        tier_inflight(&mut out, &mut r, &fx, "", &runs, 40);
        rdy_model::unseed_ids_for_tests();
        let names: Vec<&str> = out.iter().map(|m| m.op.as_str()).collect();
        assert_eq!(
            names,
            [
                "inflight_build_1k",
                "inflight_carry",
                "context_inflight_1k",
                "impact_inflight_1k",
                "context_view_1k",
                "impact_view_1k",
                "coord_build_1k"
            ]
        );
    }

    /// The tension ops run end to end on a small tree (n and the citing claims scaled down), after
    /// the in-flight ops, as the tiers run them.
    #[test]
    fn d2perf1_the_tension_ops_run_on_a_small_tree() {
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
        let mut inflight = vec![];
        tier_inflight(&mut inflight, &mut r, &fx, "", &runs, 40);
        let mut out = vec![];
        tier_tensions(&mut out, &mut r, &fx, "", &runs, 40, 10);
        rdy_model::unseed_ids_for_tests();
        let names: Vec<&str> = out.iter().map(|m| m.op.as_str()).collect();
        assert_eq!(
            names,
            [
                "tension_carry_1k",
                "tension_on_change_1k",
                "tension_on_change_delete_1k",
                "tension_on_change_view_op_1k",
                "tension_settle_1k",
                "tensions_full_1k",
                "tensions_canon",
                "gate_tensions_1k"
            ]
        );
        assert!(r.read().tensions().expect("tensions").len() >= 10);
    }

    #[test]
    fn d2perf1_the_m2d2_ops_state_their_targets() {
        for op in [
            "tension_carry_1k",
            "tension_on_change_1k",
            "tension_on_change_delete_1k",
            "tension_on_change_view_op_1k",
            "tension_settle_1k",
            "tensions_full_1k",
            "tensions_canon",
            "gate_tensions_1k",
        ] {
            assert!(spec_target(op).is_some(), "{op} has no target");
        }
        assert!(
            spec_target("tension_carry_1k")
                .unwrap()
                .contains("< 10 ms p99")
        );
        assert!(
            spec_target("tension_on_change_1k")
                .unwrap()
                .contains("< 10 ms p99")
        );
        assert!(
            spec_target("tensions_canon_50k")
                .unwrap()
                .contains("no spec target")
        );
    }

    /// After the view ops (which leave conflicted views open), the carry still carries: the tier
    /// closes what the earlier tiers left open.
    #[test]
    fn d1perf1_the_inflight_ops_carry_after_the_view_ops() {
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
        tier_views(&mut vec![], &mut r, &fx, "", &runs);
        assert!(
            !r.read()
                .conflicts(&Scope::Inflight)
                .expect("conflicts")
                .is_empty(),
            "the view ops leave a conflicted in-flight state"
        );
        let mut out = vec![];
        tier_inflight(&mut out, &mut r, &fx, "", &runs, 40);
        rdy_model::unseed_ids_for_tests();
        assert_eq!(out.len(), 7);
        assert!(
            r.read()
                .conflicts(&Scope::Inflight)
                .expect("conflicts")
                .is_empty()
        );
    }

    #[test]
    fn d1perf1_the_m2d1_ops_state_their_targets() {
        for op in [
            "context_inflight_1k",
            "impact_inflight_1k",
            "context_view_1k",
            "impact_view_1k",
            "context_inflight_1k_50k",
        ] {
            assert!(spec_target(op).unwrap().contains("< 5 ms p99"), "{op}");
        }
        for op in [
            "inflight_build_1k",
            "coord_build_1k",
            "mcp_params_9mib",
            "mcp_write_9mib",
        ] {
            assert!(spec_target(op).unwrap().contains("no spec target"), "{op}");
        }
        assert!(spec_target("inflight_carry").unwrap().contains("< 1 ms"));
        assert!(fsync_bound("mcp_write_9mib") && !fsync_bound("mcp_params_9mib"));
    }

    /// Final-review fix round 1: the coordination ops run end to end on small projects (n
    /// scaled down) and state their targets.
    #[test]
    fn d1perf1_the_coord_ops_run_and_state_their_targets() {
        rdy_model::seed_ids_for_tests(0x7064_6566);
        let dir = tempfile::tempdir().expect("tempdir");
        let runs = Runs {
            write: 2,
            read: 2,
            tree: 1,
            materialize_cold: 1,
            incremental: 1,
            view_ops: 1,
            landing: 1,
            view_changes: 1,
            landing_views: 1,
        };
        let mut out = vec![];
        tier_coord(&mut out, dir.path(), "", &runs, 20);
        rdy_model::unseed_ids_for_tests();
        let names: Vec<&str> = out.iter().map(|m| m.op.as_str()).collect();
        assert_eq!(
            names,
            [
                "write_expired_intents_1k",
                "brief_expired_intents_1k",
                "write_settled_alts_1k",
                "open_chosen_alts_100",
                "propose_chosen_alts_100"
            ]
        );
        for op in names {
            let target = spec_target(op).unwrap_or_else(|| panic!("{op} has no target"));
            let budget = if op.starts_with("open_") {
                "< 2 s"
            } else {
                "< 5 ms p99"
            };
            assert!(target.contains(budget), "{op}: {target}");
        }
        assert!(fsync_bound("propose_chosen_alts_100") && !fsync_bound("open_chosen_alts_100"));
        assert!(
            fsync_bound("write_expired_intents_1k") && !fsync_bound("brief_expired_intents_1k")
        );
    }

    #[test]
    #[ignore = "runs the typical perf workload; cargo test -p rdy-qa --release --lib -- --ignored d1perf1"]
    fn d1perf1_the_m2d1_ops_have_spec_targets_and_run() {
        let ops = [
            "inflight_build_1k",
            "inflight_carry",
            "context_inflight_1k",
            "impact_inflight_1k",
            "context_view_1k",
            "impact_view_1k",
            "coord_build_1k",
            "mcp_params_9mib",
            "mcp_write_9mib",
            "write_expired_intents_1k",
            "brief_expired_intents_1k",
            "write_settled_alts_1k",
            "open_chosen_alts_100",
            "propose_chosen_alts_100",
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
    fn enperf1_the_entity_ops_run_on_a_small_tree() {
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
            ..TYPICAL_RUNS
        };
        let mut out = vec![];
        tier_entities(&mut out, &mut r, &fx, "", &runs);
        let ops: Vec<&str> = out.iter().map(|m| m.op.as_str()).collect();
        assert_eq!(
            ops,
            vec![
                "lexicon_build",
                "entity_search",
                "suggestions_after_write",
                "coverage_all"
            ]
        );
        for op in ops {
            assert!(spec_target(op).is_some(), "{op} has no target text");
        }
        rdy_model::unseed_ids_for_tests();
    }

    #[test]
    #[ignore = "runs the typical perf workload; cargo test -p rdy-qa --release --lib -- --ignored enperf1"]
    fn enperf1_entity_ops_have_targets_and_run() {
        let ms = run_workload();
        for op in [
            "lexicon_build",
            "entity_search",
            "suggestions_after_write",
            "coverage_all",
        ] {
            let m = ms
                .iter()
                .find(|m| m.op == op)
                .unwrap_or_else(|| panic!("{op} missing"));
            assert!(m.median_ms > 0.0, "{op}");
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
    #[ignore = "runs the typical perf workload; cargo test -p rdy-qa --release --lib -- --ignored ae2perf1"]
    fn ae2perf1_search_has_a_target_and_runs() {
        assert!(spec_target("search_literal").is_some());
        let ms = run_workload();
        let m = ms
            .iter()
            .find(|m| m.op == "search_literal")
            .expect("search_literal missing");
        assert!(m.median_ms > 0.0);
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
    #[ignore = "runs the typical perf workload; cargo test -p rdy-qa --release --lib -- --ignored wcperf1"]
    fn wcperf1_the_working_copy_ops_have_targets_and_run() {
        for op in ["wc_project_typical", "wc_plan_one_doc"] {
            assert!(spec_target(op).is_some(), "{op} has no target");
        }
        let ms = run_workload();
        for op in ["wc_project_typical", "wc_plan_one_doc"] {
            let m = ms
                .iter()
                .find(|m| m.op == op)
                .unwrap_or_else(|| panic!("{op} missing"));
            assert!(m.median_ms > 0.0);
        }
    }

    #[test]
    fn wcperf1_the_working_copy_tier_measures_both_ops_on_a_small_project() {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("p");
        std::fs::create_dir_all(&proj).unwrap();
        testkit::generate_tree(&proj, 2, 2, 5);
        let mut r = Rdy::open(&proj).unwrap();
        land(&mut r, "work").unwrap();
        let mut out = vec![];
        tier_wc(&mut out, &r, dir.path());
        for op in ["wc_project_typical", "wc_plan_one_doc"] {
            let m = out
                .iter()
                .find(|m| m.op == op)
                .unwrap_or_else(|| panic!("{op} missing"));
            assert!(m.median_ms > 0.0, "{op}: {m:?}");
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
    fn hf1perf_the_bound_ops_have_targets_and_run() {
        let dir = tempfile::tempdir().unwrap();
        let ms = bound_ops(dir.path());
        for op in [
            "refuse_import_20mb_scalar",
            "refuse_check_20mb_scalar",
            "refuse_import_symlink",
            "blob_forged_manifest_refuse",
            "figure_forged_manifest_refuse",
            "pack_resolve_1000_areas",
            "areas_lookup_100k_globs",
            "areas_lookup_pathological",
            "pack_resolve_2000_types",
        ] {
            assert!(spec_target(op).is_some(), "{op}");
            let m = ms
                .iter()
                .find(|m| m.op == op)
                .unwrap_or_else(|| panic!("{op} missing"));
            assert!(m.median_ms > 0.0, "{op}: {m:?}");
        }
    }

    #[test]
    fn hf1perf_the_daemon_mcp_ops_have_targets_and_run_on_a_small_project() {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("p");
        std::fs::create_dir_all(&proj).unwrap();
        testkit::generate_tree(&proj, 1, 2, 3);
        let mut r = Rdy::open(&proj).unwrap();
        land(&mut r, "work").unwrap();
        r.view_create(&writer(), "va", None, true).unwrap();
        drop(r);
        let ms = daemon_mcp_on_copy(&proj, dir.path());
        for op in ["daemon_mcp_overview", "daemon_read_during_big_write"] {
            assert!(spec_target(op).is_some(), "{op}");
            let m = ms
                .iter()
                .find(|m| m.op == op)
                .unwrap_or_else(|| panic!("{op} missing"));
            assert!(m.median_ms > 0.0, "{op}: {m:?}");
        }
    }

    #[test]
    fn hf1perf_the_stdio_mcp_ops_have_targets_and_run() {
        let dir = tempfile::tempdir().unwrap();
        let ms = stdio_mcp_ops(dir.path());
        for op in [
            "mcp_stdio_overview",
            "mcp_stdio_write_max_ops",
            "mcp_stdio_oversized_line",
        ] {
            assert!(spec_target(op).is_some(), "{op}");
            let m = ms
                .iter()
                .find(|m| m.op == op)
                .unwrap_or_else(|| panic!("{op} missing"));
            assert!(m.median_ms > 0.0, "{op}: {m:?}");
        }
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
