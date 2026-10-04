//! The perf-regression tier: a fixed workload, per-machine baselines, regression verdicts.
use crate::fixtures::{approver, land, write};
use rdy_engine::{Rdy, Scope, testkit};
use rdy_model::BlockId;
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

fn spec_target(op: &str) -> Option<String> {
    Some(
        match op {
            o if o.starts_with("write") || o.starts_with("propose") => {
                "< 5 ms p99 per write (spec 11)"
            }
            "context" | "impact" | "version_position" | "resolve_path" | "tree_full" => {
                "< 5 ms p99 (spec 11)"
            }
            "render" => "< 2 s full render at 100k (spec 11)",
            "cold_open" => "< 2 s at 100k (spec 11)",
            _ => return None,
        }
        .to_string(),
    )
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
fn cold<T>(r: &mut Rdy, f: impl FnOnce(&mut Rdy) -> T) -> (T, Duration) {
    r.invalidate();
    let s = Instant::now();
    let out = f(r);
    (out, s.elapsed())
}

/// The fixed workload: a ~10k-block tree project (ids seeded, so it is identical every run),
/// timed op by op. Takes a few minutes in release (every write fsyncs).
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
    let doc_path = "area-0/doc-0";
    assert_eq!(docs[0], doc_path);
    let doc = r.resolve_path(&Scope::Canon, doc_path).expect("doc");
    let (section, claim) = {
        let m = r.materialized(&Scope::Canon).expect("canon");
        let section = m.children(Some(&doc))[0].id.clone();
        let claim = m.children(Some(&section))[0].id.clone();
        (section, claim)
    };
    let section = section.to_string();
    let claim: BlockId = claim;

    // Twenty unlanded changes spread over two views.
    for i in 0..20 {
        let view = if i % 2 == 0 { "va" } else { "vb" };
        write(
            &mut r,
            view,
            json!([{"op": "create", "type": "claim", "parent": section, "body": format!("Pending {i}.")}]),
        )
        .expect("pending write");
    }

    let va = Scope::View("va".into());
    let mut out = vec![];

    out.push(measure("write_single", 30, |i| {
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
    // reads timed cold: each run rebuilds what a write would have invalidated
    out.push(measure_timed("context", 20, |_| {
        cold(&mut r, |r| r.context(&va, &claim).expect("context")).1
    }));
    out.push(measure_timed("impact", 20, |_| {
        cold(&mut r, |r| r.impact(&va, &claim, 3).expect("impact")).1
    }));
    out.push(measure_timed("version_position", 20, |_| {
        cold(&mut r, |r| r.version_position(&va).expect("position")).1
    }));
    out.push(measure_timed("resolve_path", 20, |_| {
        cold(&mut r, |r| {
            r.resolve_path(&Scope::Canon, doc_path).expect("resolve")
        })
        .1
    }));
    out.push(measure("tree_full", 20, |_| {
        r.tree(&Scope::Canon, None, None).expect("tree");
    }));
    out.push(measure("render", 5, |i| {
        let site = dir.path().join(format!("site{i}"));
        rdy_render::render_site(&mut r, &Scope::Canon, &site).expect("render");
    }));
    out.push(measure("export_import", 5, |i| {
        let export = dir.path().join(format!("export{i}"));
        r.export(&Scope::Canon, &export).expect("export");
        let fresh = dir.path().join(format!("fresh{i}"));
        std::fs::create_dir_all(&fresh).expect("mkdir");
        let mut r2 = Rdy::init(&fresh, "rdy-standard").expect("init");
        r2.import(&approver(), "work", &export).expect("import");
    }));
    // Cold open: a fresh handle on the project, then the first read of canon.
    drop(r);
    out.push(measure("cold_open", 5, |_| {
        let mut r = Rdy::open(&proj).expect("open");
        r.materialized(&Scope::Canon).expect("canon");
    }));
    rdy_model::unseed_ids_for_tests();
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
        r.context(&work, &doc).unwrap();
        assert_eq!(r.is_cached(&work), (true, true), "warm before");
        let mut at_start = None;
        let (_, took) = cold(&mut r, |r| {
            at_start = Some(r.is_cached(&work));
            r.context(&work, &doc).unwrap()
        });
        assert_eq!(at_start, Some((false, false)), "the timed body starts cold");
        assert_eq!(r.is_cached(&work), (true, true), "the body rebuilt");
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
}
