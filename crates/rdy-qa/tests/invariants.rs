use rdy_engine::Scope;
use rdy_qa::fixtures::*;
use rdy_qa::invariants::check;
use serde_json::json;

fn rules(r: &mut rdy_engine::Rdy, scope: &Scope) -> Vec<&'static str> {
    match check(r, scope) {
        Ok(()) => vec![],
        Err(v) => {
            let mut x: Vec<&'static str> = v.iter().map(|v| v.rule).collect();
            x.sort();
            x.dedup();
            x
        }
    }
}

#[test]
fn a_clean_project_passes_in_view_and_canon() {
    let (_d, mut r) = project();
    write(&mut r, "work", json!([
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "wallets"}},
        {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"title": "Tx", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Balances."},
        {"op": "create", "type": "claim", "parent": "$s", "body": "Uses the {{$t}}."}
    ])).unwrap();
    assert_eq!(
        rules(&mut r, &Scope::View("work".into())),
        Vec::<&str>::new()
    );
    land(&mut r, "work").unwrap();
    assert_eq!(rules(&mut r, &Scope::Canon), Vec::<&str>::new());
}

#[test]
fn each_rule_catches_its_violation() {
    let (_d, mut r) = project();
    let doc = |id: &str, name: &str| {
        raw(
            id,
            "doc",
            None,
            "V",
            &[("name", name), ("title", "T"), ("doc_type", "glossary")],
            "",
        )
    };
    // cycle: two sections that are each other's parent
    raw_commit(
        &mut r,
        "cyc",
        vec![
            raw(
                "s-AAAAA",
                "section",
                Some("s-BBBBB"),
                "V",
                &[("title", "A")],
                "",
            ),
            raw(
                "s-BBBBB",
                "section",
                Some("s-AAAAA"),
                "V",
                &[("title", "B")],
                "",
            ),
        ],
    );
    assert!(rules(&mut r, &Scope::View("cyc".into())).contains(&"cycle"));
    // orphan + missing parent
    raw_commit(
        &mut r,
        "orph",
        vec![raw("c-AAAAA", "claim", Some("s-ZZZZZ"), "V", &[], "x")],
    );
    assert!(rules(&mut r, &Scope::View("orph".into())).contains(&"parent"));
    // a doc under a section
    raw_commit(
        &mut r,
        "place",
        vec![
            doc("d-AAAAA", "a"),
            raw(
                "s-AAAAA",
                "section",
                Some("d-AAAAA"),
                "V",
                &[("title", "S")],
                "",
            ),
            raw(
                "d-BBBBB",
                "doc",
                Some("s-AAAAA"),
                "V",
                &[("name", "b"), ("title", "T"), ("doc_type", "glossary")],
                "",
            ),
        ],
    );
    assert!(rules(&mut r, &Scope::View("place".into())).contains(&"placement"));
    // malformed order key
    raw_commit(
        &mut r,
        "ord",
        vec![
            doc("d-AAAAA", "a"),
            raw(
                "s-AAAAA",
                "section",
                Some("d-AAAAA"),
                "a0",
                &[("title", "S")],
                "",
            ),
        ],
    );
    assert!(rules(&mut r, &Scope::View("ord".into())).contains(&"order-key"));
    // name clash ignoring case
    raw_commit(
        &mut r,
        "name",
        vec![doc("d-AAAAA", "Notes"), doc("d-BBBBB", "notes")],
    );
    assert!(rules(&mut r, &Scope::View("name".into())).contains(&"name"));
    // dangling reference
    raw_commit(
        &mut r,
        "ref",
        vec![
            doc("d-AAAAA", "a"),
            raw(
                "s-AAAAA",
                "section",
                Some("d-AAAAA"),
                "V",
                &[("title", "S")],
                "",
            ),
            raw(
                "c-AAAAA",
                "claim",
                Some("s-AAAAA"),
                "V",
                &[],
                "See {{t-ZZZZZ}}.",
            ),
        ],
    );
    assert!(rules(&mut r, &Scope::View("ref".into())).contains(&"ref"));
    // schema: a claim without a body
    raw_commit(
        &mut r,
        "schema",
        vec![
            doc("d-AAAAA", "a"),
            raw(
                "s-AAAAA",
                "section",
                Some("d-AAAAA"),
                "V",
                &[("title", "S")],
                "",
            ),
            raw("c-AAAAA", "claim", Some("s-AAAAA"), "V", &[], ""),
        ],
    );
    assert!(rules(&mut r, &Scope::View("schema".into())).contains(&"schema"));
    // folder depth over the cap
    let mut chain: Vec<rdy_model::Block> = vec![];
    for i in 0..=rdy_model::tree::MAX_FOLDER_DEPTH {
        let c = rdy_model::CROCKFORD;
        let id = format!("fd-{}{}AAA", c[i / 32] as char, c[i % 32] as char);
        let parent = chain.last().map(|b| b.id.to_string());
        chain.push(raw(
            &id,
            "folder",
            parent.as_deref(),
            "V",
            &[("name", &format!("f{i}"))],
            "",
        ));
    }
    raw_commit(&mut r, "deep", chain);
    assert!(rules(&mut r, &Scope::View("deep".into())).contains(&"depth"));
}
