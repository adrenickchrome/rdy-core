//! The version_position cache (plan M2a1, vpos1) and change links on writes (clink1).
use rdy_engine::*;
use rdy_model::{ChangeLinks, Hash};
use rdy_qa::fixtures::*;
use serde_json::json;

fn doc(title: &str) -> serde_json::Value {
    json!([{"op": "create", "type": "doc", "fields": {"title": title, "doc_type": "page"}}])
}

#[test]
fn vpos1_a_cached_position_moves_with_canon_tags_and_the_view() {
    let (_d, mut r) = project();
    write(&mut r, "work", doc("One")).unwrap();
    land(&mut r, "work").unwrap();
    let v = Scope::View("work".into());
    let first = r.read().version_position(&v).unwrap();
    assert_eq!(r.read().version_position(&v).unwrap(), first);
    assert!(r.read().version_position_cache_len() >= 1);
    write(&mut r, "work", doc("Two")).unwrap();
    assert_eq!(
        r.read()
            .version_position(&v)
            .unwrap()
            .view
            .unwrap()
            .unlanded_changes,
        1,
        "a write moves the view head"
    );
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    assert_eq!(
        r.read()
            .version_position(&Scope::Canon)
            .unwrap()
            .version
            .unwrap()
            .name,
        "v1.0.0",
        "a tag moves the position"
    );
    land(&mut r, "work").unwrap();
    assert_eq!(
        r.read()
            .version_position(&Scope::Canon)
            .unwrap()
            .landings_since,
        1,
        "a landing moves canon"
    );
}

#[test]
// Orchestrator (2026-10-05): a microsecond timing budget is not a correctness check. On a machine shared
// by two build tracks it fails at random, so it runs only on request (`--ignored`). The perf tier measures
// the cached position as `version_position_cached`, judged on the isolated Linux bench (M2a2 Task 14).
#[ignore = "timing: run with --release --ignored on an idle machine; the perf tier covers it"]
fn vpos1_a_cached_position_costs_under_10_us_p99() {
    let (_d, mut r) = project();
    for i in 0..20 {
        write(&mut r, "work", doc(&format!("Doc {i}"))).unwrap();
        land(&mut r, "work").unwrap();
    }
    r.tag(&approver(), "v0.1.0", None, None).unwrap();
    write(&mut r, "work", doc("Pending")).unwrap();
    let v = Scope::View("work".into());
    r.read().version_position(&v).unwrap();
    let mut t: Vec<std::time::Duration> = (0..10_000)
        .map(|_| {
            let s = std::time::Instant::now();
            r.read().version_position(&v).unwrap();
            s.elapsed()
        })
        .collect();
    t.sort();
    let p99 = t[t.len() * 99 / 100];
    assert!(p99 < std::time::Duration::from_micros(10), "p99 {p99:?}");
}

#[test]
fn clink1_links_are_stored_on_the_change_and_part_of_its_id() {
    let (_d, mut r) = project();
    // a real intent to link: on a doc written first
    let w = write(&mut r, "work", doc("Target")).unwrap();
    let target = w.created.values().next().unwrap().clone();
    let intent: Hash = r
        .intent_post(&writer(), Some("work"), None, &[target], "link me", None)
        .unwrap()
        .intent
        .parse()
        .unwrap();
    let links = ChangeLinks {
        intent: Some(intent),
        alternative: None,
        client_nonce: Some("nonce-7".into()),
        imported_by: None,
        replayed_from: None,
        briefing: None,
    };
    let req = |title: &str| WriteRequest {
        view: "work".into(),
        ops: serde_json::from_value(doc(title)).unwrap(),
        justification: None,
        message: String::new(),
    };
    let a = r
        .write_linked(&writer(), req("A"), links.clone(), None)
        .unwrap();
    let id: Hash = a.change.parse().unwrap();
    assert_eq!(r.store().change(&id).unwrap().links, links);
    let b = r.write(&writer(), req("B")).unwrap();
    assert_eq!(
        r.store().change(&b.change.parse().unwrap()).unwrap().links,
        ChangeLinks::default()
    );
    // a link names a record that exists (cfmt1's fsck checks it)
    r.fsck().unwrap();
}
