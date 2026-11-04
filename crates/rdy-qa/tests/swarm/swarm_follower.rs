//! A follower of a frozen swarm result (tb-A4DAV; D-AA4FD): a full dump and increments made from
//! copies of a real swarm's store, through the QA command, reads as its source reads. No follower
//! agent and no cadence joins the swarm.
use rdy_engine::follower::FollowerCore;
use rdy_engine::{Rdy, Scope};
use rdy_qa::swarm::oracle::copy_of;
use rdy_qa::swarm::run;
use rdy_qa::swarm::script::Config;
use serde_json::Value;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

/// Seed 1's run, made once and shared by the tests of this file.
fn seed_one() -> &'static run::Outcome {
    static RUN: OnceLock<run::Outcome> = OnceLock::new();
    RUN.get_or_init(|| run::run(&Config::for_run(1)))
}

/// The bytes of each `.log` segment. A quarantined `.torn-` copy is local to the store that a
/// swarm kill left it in: it holds no source frame and no dump carries it.
fn segments(root: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(root.join(".rdy/store/segments"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "log"))
        .map(|p| {
            (
                p.file_name().unwrap().to_string_lossy().to_string(),
                std::fs::read(&p).unwrap(),
            )
        })
        .collect()
}

/// Each block of a materialized scope, by id: its version and body.
fn blocks(m: &rdy_engine::Materialized) -> Vec<(String, String, String)> {
    let mut rows: Vec<_> = m
        .iter()
        .map(|b| {
            (
                b.id.to_string(),
                m.version_of(&b.id)
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
                b.body.clone(),
            )
        })
        .collect();
    rows.sort();
    rows
}

#[test]
fn mh8_follower_matches_a_frozen_swarm_chain() {
    let o = seed_one();
    let (_keep, frozen) = copy_of(&o.frozen);
    let (_keep2, drained) = copy_of(&o.dir);
    let d = tempfile::tempdir().unwrap();
    let full = d.path().join("frozen.rdydump");
    let r = Rdy::open(&frozen).unwrap();
    r.read().dump(&full).unwrap();
    r.close().unwrap();
    let inc = d.path().join("drained.rdydump");
    let r = Rdy::open(&drained).unwrap();
    // the drain continues the frozen copy's log
    let s = r.read().dump_after_file(&full, &inc).unwrap();
    assert!(s.batches > 0);
    let to = d.path().join("f");
    let out = Command::new(env!("CARGO_BIN_EXE_qa-follower"))
        .arg(&full)
        .arg(&to)
        .arg("--then")
        .arg(&inc)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["digest"], s.digest.to_string());
    assert_eq!(v["batches"], s.batches);
    assert_eq!(segments(&to), segments(&drained), "the source frames");
    let core = FollowerCore::open(&to).unwrap();
    core.fsck().unwrap();
    let got = core.read().unwrap();
    let want = r.read();
    assert_eq!(got.seq(), want.seq());
    assert_eq!(got.canon_head(), want.canon_head());
    let (a, b) = (
        got.materialized(&Scope::Canon).unwrap(),
        want.materialized(&Scope::Canon).unwrap(),
    );
    assert_eq!(blocks(&a), blocks(&b));
    for (name, _, _) in want.reader().tags().unwrap() {
        let scope = format!("tag:{}", name.trim_start_matches('v'));
        let (a, b) = (
            got.materialized(&got.resolve_scope(&scope).unwrap())
                .unwrap(),
            want.materialized(&want.resolve_scope(&scope).unwrap())
                .unwrap(),
        );
        assert_eq!(blocks(&a), blocks(&b), "{name}");
    }
    drop((got, want, core));
    r.close().unwrap();
}

/// Seeds a follower in `to` from the full dump `full`, then follows each increment of `then`,
/// through the QA command. Its JSON report.
fn follow(full: &Path, to: &Path, then: &[&Path]) -> Value {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_qa-follower"));
    cmd.arg(full).arg(to);
    for inc in then {
        cmd.arg("--then").arg(inc);
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

/// The bytes of a full dump of `st`, written under `d` as `name`.
fn dump_bytes(st: &rdy_engine::ReadState, d: &Path, name: &str) -> Vec<u8> {
    let at = d.join(name);
    st.dump(&at).unwrap();
    std::fs::read(&at).unwrap()
}

/// The landings an acknowledged M3 route attempt names.
fn route_landings(o: &run::Outcome) -> Vec<String> {
    o.before_freeze()
        .iter()
        .filter(|e| e.outcome == rdy_qa::swarm::journal::Outcome::Ok)
        .filter_map(|e| match e.route.as_ref().map(|a| &a.reply) {
            Some(rdy_qa::swarm::journal::Reply::Acknowledged { records, .. }) => {
                Some(records.clone())
            }
            _ => None,
        })
        .flatten()
        .collect()
}

/// Plan Task 10 (D-AA4FD): the existing `qa-follower` follows a frozen, completed M3 swarm chain.
/// A follower seeded from the full dump of the frozen copy (the M3 routes ran: its chain holds
/// each landing an acknowledged route named, and the real compact's removal) reads at the
/// source's position, holds the source's frames and gives a full dump byte for byte the
/// source's, with the same M3 projection. Following the drain's increment brings it to the
/// drained project the same way.
#[test]
fn mg10_follower_matches_the_completed_m3_swarm_prefix() {
    use rdy_qa::m3_audit::M3Facts;
    let o = seed_one();
    let routed = route_landings(o);
    assert!(
        !routed.is_empty(),
        "seed 1 acknowledged no M3 route landing"
    );
    assert!(
        o.before_freeze().iter().any(|e| e.act == "m3_compact_real"
            && e.outcome == rdy_qa::swarm::journal::Outcome::Ok),
        "seed 1 ran no real compact before the freeze"
    );
    let (_keep, frozen) = copy_of(&o.frozen);
    let (_keep2, drained) = copy_of(&o.dir);
    let d = tempfile::tempdir().unwrap();
    let full = d.path().join("frozen.rdydump");
    let src = Rdy::open(&frozen).unwrap();
    src.read().dump(&full).unwrap();
    let inc = d.path().join("drained.rdydump");
    let done = Rdy::open(&drained).unwrap();
    let s = done.read().dump_after_file(&full, &inc).unwrap();
    assert!(s.batches > 0);
    for (what, source, then, to) in [
        ("the frozen prefix", &src, vec![], d.path().join("prefix")),
        (
            "the completed chain",
            &done,
            vec![inc.as_path()],
            d.path().join("done"),
        ),
    ] {
        let report = follow(&full, &to, &then);
        let core = FollowerCore::open(&to).unwrap();
        core.fsck().unwrap();
        let got = core.read().unwrap();
        let want = source.read();
        assert_eq!(got.seq(), want.seq(), "{what}: the read position");
        assert_eq!(
            core.end().unwrap(),
            want.seq(),
            "{what}: the follower's end"
        );
        assert_eq!(got.canon_head(), want.canon_head(), "{what}: canon's head");
        assert_eq!(
            segments(&to),
            segments(source.root()),
            "{what}: the source frames"
        );
        assert_eq!(
            dump_bytes(&got, d.path(), &format!("{what}-follower.rdydump")),
            dump_bytes(&want, d.path(), &format!("{what}-source.rdydump")),
            "{what}: the full dump bytes"
        );
        let (a, b) = (M3Facts::read(&got).unwrap(), M3Facts::read(&want).unwrap());
        assert_eq!(a, b, "{what}: the M3 projection");
        let held: Vec<String> = a.landings.iter().map(|l| l.landing.to_string()).collect();
        for l in &routed {
            assert!(held.contains(l), "{what}: route landing {l} is not held");
        }
        eprintln!("{what}: {report}, {} M3 landings", a.landings.len());
        drop((got, want, core));
    }
    src.close().unwrap();
    done.close().unwrap();
}
