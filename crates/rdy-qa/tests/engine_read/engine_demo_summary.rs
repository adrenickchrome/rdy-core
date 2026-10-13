//! df5: a one-line summary shows a reference's link words and is cut at a word with an ellipsis
//! (the demo's F3: the briefing's `dependents` summary cut mid-word with no ellipsis).
use rdy_engine::*;
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use rdy_store::view::ViewOpKind;
use serde_json::json;

const CUT: &str = "A member who has not passed the identity check may hold at most 150 EUR in a…";

struct World {
    r: String,
    m: String,
    c: String,
}

/// Doc and section holding a term r (name "Identity check"), a component m (name "Wallet") and a
/// claim c that cites r with link words and describes m. Landed.
fn world(rdy: &mut Rdy) -> World {
    let w = write(rdy, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "kyc", "title": "KYC", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
        {"op": "create", "type": "term", "id": "$r", "parent": "$s", "fields": {"name": "Identity check"},
         "body": "An account is verified when the member passes an identity check with a government ID."},
        {"op": "create", "type": "component", "id": "$m", "parent": "$s", "fields": {"name": "Wallet"}},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s",
         "body": "A member who has not passed {{$r|the identity check}} may hold at most 150 EUR in a wallet.",
         "links": {"describes": [{"target": "$m"}]}}
    ]))
    .unwrap();
    land(rdy, "work").unwrap();
    World {
        r: w.created["$r"].clone(),
        m: w.created["$m"].clone(),
        c: w.created["$c"].clone(),
    }
}

#[test]
fn df5_search_summary_inlines_ref() {
    let (_d, mut rdy) = project();
    let w = world(&mut rdy);
    let res = rdy
        .read()
        .search(
            &Scope::Canon,
            &SearchQuery {
                text: "Wallet".into(),
                ..Default::default()
            },
            None,
            Some(4000),
        )
        .unwrap();
    let sheet = res.entities.iter().find(|e| e.id == w.m).expect("entity m");
    let fact = sheet.facts.iter().find(|f| f.id == w.c).expect("fact c");
    assert_eq!(fact.summary, CUT);
}

#[test]
fn df5_brief_dependents_show_link_words() {
    let (_d, mut rdy) = project();
    let w = world(&mut rdy);
    let b = rdy
        .read()
        .brief(
            &Scope::Canon,
            &Coverage {
                ids: vec![w.r.clone()],
                path: None,
            },
            None,
        )
        .unwrap();
    let dep = b
        .dependents
        .iter()
        .find(|d| d.block == w.c)
        .unwrap_or_else(|| panic!("c not under dependents: {:?}", b.dependents));
    assert_eq!(dep.summary, CUT);
}

#[test]
fn df5_a_summary_cuts_at_a_word_with_an_ellipsis() {
    let full =
        "A member who has not passed the identity check may hold at most 150 EUR in a wallet.";
    assert_eq!(full.chars().count(), 84);
    // (a) the 84-character text at 80
    assert_eq!(cut_line(full, 80), CUT);
    assert!(cut_line(full, 80).chars().count() <= 80);
    // (b) one word longer than the limit
    assert_eq!(
        cut_line(&"x".repeat(100), 80),
        format!("{}…", "x".repeat(79))
    );
    // (f) exactly 80 characters stays whole
    let exact = format!("{} {}", "a".repeat(39), "b".repeat(40));
    assert_eq!(exact.chars().count(), 80);
    assert_eq!(cut_line(&exact, 80), exact);
    // whitespace collapses, and a trailing space before the cut is dropped
    assert_eq!(cut_line("one  two\n three", 80), "one two three");
    assert_eq!(cut_line("one two three", 8), "one two…");
    assert_eq!(cut_line("one two three", 7), "one…");

    let (_d, mut rdy) = project();
    let w = world(&mut rdy);
    let canon = rdy.read().materialized(&Scope::Canon).unwrap();
    let id = |t: &str| BlockId::parse(t).unwrap();
    // (c) the 80th character is inside link words: cut before that word
    let pad = "p".repeat(60);
    let cites = write(&mut rdy, "more", json!([
        {"op": "create", "type": "claim", "id": "$x", "parent": canon.get(&id(&w.c)).unwrap().parent.as_ref().unwrap().to_string(),
         "body": format!("{pad} {{{{{}|link words that run past the cut}}}} tail", w.r)},
        {"op": "create", "type": "claim", "id": "$k", "parent": canon.get(&id(&w.c)).unwrap().parent.as_ref().unwrap().to_string(),
         "body": format!("Check {{{{{}|@key}}}} first.", w.r)},
        {"op": "create", "type": "claim", "id": "$e", "parent": canon.get(&id(&w.c)).unwrap().parent.as_ref().unwrap().to_string(),
         "body": r"Write \{{not a ref}} as text."}
    ]))
    .unwrap();
    let view = Scope::View("more".into());
    let m = rdy.read().materialized(&view).unwrap();
    let x = summary_in(m.get(&id(&cites.created["$x"])).unwrap(), &m);
    assert_eq!(x, format!("{pad} link words that…"));
    // (d) {{r|@key}} shows the target's name
    let k = summary_in(m.get(&id(&cites.created["$k"])).unwrap(), &m);
    assert_eq!(k, "Check Identity check first.");
    // (e) an escaped opener shows as text
    let e = summary_in(m.get(&id(&cites.created["$e"])).unwrap(), &m);
    assert_eq!(e, "Write {{not a ref}} as text.");
    // without state: link words, else the bare id
    let bare = summary(m.get(&id(&cites.created["$k"])).unwrap());
    assert_eq!(bare, format!("Check {} first.", w.r));
}

#[test]
fn df5_a_conflict_side_shows_link_words() {
    let (_d, mut rdy) = project();
    let w = world(&mut rdy);
    let v = version_of(&rdy, &Scope::Canon, &w.c);
    let a = as_agent("agent:a", "r1");
    let body = |t: &str| format!("{t} {{{{{}|the identity check}}}} applies.", w.r);
    write_as(
        &mut rdy,
        &a,
        "a",
        json!([{"op": "update", "id": w.c, "base": v, "body": body("First")}]),
    )
    .unwrap();
    let cb = write_as(
        &mut rdy,
        &as_agent("agent:b", "r1"),
        "b",
        json!([{"op": "update", "id": w.c, "base": v, "body": body("Second")}]),
    )
    .unwrap()
    .change;
    rdy.store_mut()
        .view_append(
            "a",
            vec![ViewOpKind::Include {
                changes: vec![cb.parse().unwrap()],
            }],
            &a.provenance(),
        )
        .unwrap();
    rdy.invalidate();
    let info = rdy
        .read()
        .conflict_info(&Scope::View("a".into()), &BlockId::parse(&w.c).unwrap())
        .unwrap()
        .expect("conflicted");
    assert_eq!(info.sides.len(), 2);
    for side in &info.sides {
        assert!(!side.summary.contains("{{"), "{}", side.summary);
        assert!(
            side.summary.contains("the identity check applies."),
            "{}",
            side.summary
        );
    }
}
