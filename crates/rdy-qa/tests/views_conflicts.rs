//! cres1: a conflicted block reads as its base with both sides listed; a write from the base or a
//! side resolves it; stale writes to clean blocks keep E_STALE_BASE.
use rdy_engine::*;
use rdy_qa::fixtures::*;
use rdy_qa::invariants::check;
use rdy_store::view::ViewOpKind;
use serde_json::json;

fn update(id: &str, base: Option<&str>, body: &str) -> serde_json::Value {
    let mut op = json!({"op": "update", "id": id, "body": body});
    if let Some(b) = base {
        op["base"] = json!(b);
    }
    json!([op])
}
fn view(v: &str) -> Scope {
    Scope::View(v.into())
}

/// a and b edit the seed claim from canon; a includes b's change (through the store, since
/// include is built beside this task).
fn conflicted() -> (tempfile::TempDir, Rdy, String, String, String, String) {
    let (d, mut r) = project();
    let (_doc, _section, c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let v = version_of(&r, &Scope::Canon, &c);
    let a = as_agent("agent:a", "r1");
    let ca = write_as(&mut r, &a, "a", update(&c, Some(&v), "a's"))
        .unwrap()
        .change;
    let cb = write_as(
        &mut r,
        &as_agent("agent:b", "r1"),
        "b",
        update(&c, Some(&v), "b's"),
    )
    .unwrap()
    .change;
    r.store_mut()
        .view_append(
            "a",
            vec![ViewOpKind::Include {
                changes: vec![cb.parse().unwrap()],
            }],
            &a.provenance(),
        )
        .unwrap();
    r.invalidate();
    (d, r, c, v, ca, cb)
}

#[test]
fn cres1_a_conflicted_block_reads_as_its_base_with_both_sides() {
    let (_d, r, c, v, ca, cb) = conflicted();
    assert_eq!(body_of(&r, &view("a"), &c), "one");
    assert_eq!(version_of(&r, &view("a"), &c), v);
    let id = rdy_model::BlockId::parse(&c).unwrap();
    let info = r
        .read()
        .conflict_info(&view("a"), &id)
        .unwrap()
        .expect("conflicted");
    assert_eq!(info.sides.len(), 2);
    let by_change: std::collections::BTreeMap<String, &ConflictSide> = info
        .sides
        .iter()
        .map(|s| (s.change.clone().unwrap(), s))
        .collect();
    assert!(
        by_change[&ca].summary.starts_with("agent:a:"),
        "{:?}",
        by_change[&ca]
    );
    assert!(
        by_change[&cb].summary.contains("b's"),
        "{:?}",
        by_change[&cb]
    );
    assert!(
        r.read().conflict_info(&view("b"), &id).unwrap().is_none(),
        "b has only its own change"
    );
    assert!(
        r.read()
            .conflict_info(&Scope::Canon, &id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn cres1_a_write_from_the_conflict_base_resolves_it_and_depends_on_both_sides() {
    let (_d, mut r, c, v, ca, cb) = conflicted();
    let a = as_agent("agent:a", "r1");
    let res = write_as(&mut r, &a, "a", update(&c, Some(&v), "merged")).unwrap();
    let deps: Vec<String> = r
        .read()
        .reader()
        .change(&res.change.parse().unwrap())
        .unwrap()
        .deps
        .iter()
        .map(|h| h.to_string())
        .collect();
    assert!(deps.contains(&ca) && deps.contains(&cb), "{deps:?}");
    assert!(r.read().conflicts(&view("a")).unwrap().is_empty());
    assert_eq!(body_of(&r, &view("a"), &c), "merged");
    check(&r, &view("a")).unwrap();
    let l = land(&mut r, "a").unwrap();
    assert!(!l.is_empty());
    assert_eq!(
        body_of(&r, &Scope::Canon, &c),
        "merged",
        "a resolved view lands"
    );
}

#[test]
fn cres1_a_write_from_a_sides_version_resolves_it_too() {
    let (_d, mut r, c, _v, _ca, _cb) = conflicted();
    let b_version = version_of(&r, &view("b"), &c);
    let a = as_agent("agent:a", "r1");
    write_as(&mut r, &a, "a", update(&c, Some(&b_version), "b's, kept")).unwrap();
    assert!(r.read().conflicts(&view("a")).unwrap().is_empty());
    assert_eq!(body_of(&r, &view("a"), &c), "b's, kept");
}

#[test]
fn cres1_a_delete_resolves_a_conflict() {
    let (_d, mut r, c, v, _ca, _cb) = conflicted();
    let a = as_agent("agent:a", "r1");
    write_as(
        &mut r,
        &a,
        "a",
        json!([{"op": "delete", "id": c, "base": v}]),
    )
    .unwrap();
    assert!(r.read().conflicts(&view("a")).unwrap().is_empty());
    assert!(
        r.read()
            .materialized(&view("a"))
            .unwrap()
            .get(&rdy_model::BlockId::parse(&c).unwrap())
            .is_none()
    );
}

#[test]
fn cres1_a_write_without_a_base_or_with_a_foreign_base_is_refused_naming_the_conflict() {
    let (_d, mut r, c, _v, _ca, _cb) = conflicted();
    let a = as_agent("agent:a", "r1");
    let e = write_as(&mut r, &a, "a", update(&c, None, "x")).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert!(
        e.message.contains("conflicted") && e.fix.contains("base="),
        "{e}"
    );
    let e = write_as(
        &mut r,
        &a,
        "a",
        update(&c, Some(&rdy_model::Hash::of(b"nope").to_string()), "x"),
    )
    .unwrap_err();
    assert_eq!(e.code, E_STALE_BASE, "{e}");
    assert!(
        e.message.contains("is neither its base") && e.message.contains("nor a side"),
        "{e}"
    );
    assert_eq!(
        r.read().conflicts(&view("a")).unwrap().len(),
        1,
        "nothing written"
    );
}

#[test]
fn cres1_own_view_stale_writes_on_clean_blocks_keep_e_stale_base() {
    let (_d, mut r) = project();
    let (_doc, _section, c) = seed_doc(&mut r, "seed");
    let v = version_of(&r, &view("seed"), &c);
    write(&mut r, "seed", update(&c, Some(&v), "two")).unwrap();
    let e = write(&mut r, "seed", update(&c, Some(&v), "three")).unwrap_err();
    assert_eq!(e.code, E_STALE_BASE, "{e}");
    assert!(
        e.message.contains("changed since you read it"),
        "the M1 text is unchanged: {e}"
    );
    assert!(!e.message.contains("conflicted"));
}
