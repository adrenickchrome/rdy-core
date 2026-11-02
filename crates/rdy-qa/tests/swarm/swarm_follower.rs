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

fn segments(root: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(root.join(".rdy/store/segments"))
        .unwrap()
        .map(|e| {
            let p = e.unwrap().path();
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
    let o = run::run(&Config::for_run(1));
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
