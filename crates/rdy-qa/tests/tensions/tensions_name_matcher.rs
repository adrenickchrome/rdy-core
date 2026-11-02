//! nb1: duplicate definitions read the one name matcher (D-YXBXH, D-JDW8A, D-3AT5X, O4). The
//! blocks whose name and alias keys overlap, directly or through a bridge, are one group and one
//! tension. Normative text duplicates keep their exact normalized text.
use rdy_engine::Scope;
use rdy_engine::tensions::*;
use rdy_model::BlockId;
use rdy_qa::edit_loop::{brief_of, spec_doc, stable_spec, write_cited_as};
use rdy_qa::fixtures::*;
use rdy_qa::tensions::*;
use serde_json::{Value, json};

/// A term `name` with `aliases` under `parent`.
fn named(parent: &BlockId, name: &str, aliases: &[&str]) -> Value {
    json!([{"op": "create", "type": "term", "id": "$t", "parent": parent.to_string(),
            "fields": {"name": name, "aliases": aliases}, "body": format!("{name} is a term.")}])
}

/// The carried set's duplicate tensions equal a rebuild's (ids).
fn assert_carry_matches(r: &rdy_engine::Rdy) {
    let st = r.read();
    let ids = |set: &TensionSet| -> Vec<rdy_model::Hash> {
        set.iter()
            .filter(|t| t.kind == TensionKind::DuplicateDefinition)
            .map(|t| t.id)
            .collect()
    };
    assert_eq!(
        ids(&st.tensions().unwrap()),
        ids(&st.tensions_rebuilt().unwrap())
    );
    assert_eq!(st.tensions_carried(), Some(true));
}

fn sorted(mut v: Vec<BlockId>) -> Vec<BlockId> {
    v.sort();
    v
}

#[test]
fn nb1_duplicate_alias_bridges_form_one_group_and_split() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let a = created(&put(&mut r, "a", named(&s.section, "Conflict", &[])), "$t");
    r.read().tensions().unwrap();
    let b = created(
        &put(&mut r, "b", named(&s.section, "Merge", &["Conflicts"])),
        "$t",
    );
    assert_carry_matches(&r);
    let two = one(&r, TensionKind::DuplicateDefinition);
    assert_eq!(two.blocks(), sorted(vec![a.clone(), b.clone()]));
    assert!(
        two.summary.contains("both define \"conflict\""),
        "{}",
        two.summary
    );
    assert_eq!(two.severity, TensionSeverity::Advisory, "draft content");

    // Merges shares only `merge` with B: B bridges it to A, one group, no pair tensions
    let c = created(&put(&mut r, "c", named(&s.section, "Merges", &[])), "$t");
    assert_carry_matches(&r);
    let three = one(&r, TensionKind::DuplicateDefinition);
    assert_eq!(
        three.blocks(),
        sorted(vec![a.clone(), b.clone(), c.clone()])
    );
    assert!(
        three.summary.contains("all define \"conflict\""),
        "the least key two members share: {}",
        three.summary
    );
    for x in [&a, &b, &c] {
        assert!(three.summary.contains(&x.to_string()), "{}", three.summary);
    }
    assert_ne!(three.id, two.id, "a new member gives a new id");
    assert_eq!(
        three.id,
        tension_id(three.kind, three.severity, &three.involved),
        "the id hashes the kind and the members, not the key"
    );
    r.dismiss_tension(&ag("a"), &tension_label(&three.id), "fine")
        .unwrap();
    assert!(matches!(
        r.read().tension_status(&three),
        TensionStatus::Dismissed(_)
    ));

    // removing the bridge alias splits the group: A stands alone, B and C share `merge`
    put(
        &mut r,
        "b",
        json!([{"op": "update", "id": b.to_string(), "fields": {"name": "Merge", "aliases": []}}]),
    );
    assert_carry_matches(&r);
    let split = one(&r, TensionKind::DuplicateDefinition);
    assert_eq!(split.blocks(), sorted(vec![b.clone(), c.clone()]));
    assert!(
        split.summary.contains("both define \"merge\""),
        "{}",
        split.summary
    );
    assert_ne!(split.id, three.id);
    assert_eq!(
        r.read().tension_status(&split),
        TensionStatus::Open,
        "the dismissal lapsed with the membership"
    );

    // in stable content the same bridged group blocks
    let (_d2, mut r) = project();
    let s = stable_spec(&mut r);
    let tok = brief_of(&r, &Scope::Canon, &[&s.section]).base.token;
    write_cited_as(
        &mut r,
        &ag("a"),
        "a",
        named(&s.section, "Conflict", &[]),
        Some(&tok),
    )
    .unwrap();
    write_cited_as(
        &mut r,
        &ag("b"),
        "b",
        named(&s.section, "Merge", &["Conflicts"]),
        Some(&tok),
    )
    .unwrap();
    assert_eq!(
        one(&r, TensionKind::DuplicateDefinition).severity,
        TensionSeverity::Blocking
    );
}

#[test]
fn nb1_normative_duplicate_text_does_not_plural_fold() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let claim = |body: &str| {
        json!([{"op": "create", "type": "claim", "id": "$c",
                "parent": s.section.to_string(), "body": body}])
    };
    let c1 = created(
        &put(&mut r, "a", claim("Every payment retries the request.")),
        "$c",
    );
    put(&mut r, "b", claim("Every payment retries the requests."));
    assert!(
        of_kind(&r, TensionKind::DuplicateDefinition).is_empty(),
        "two different normative texts: {:#?}",
        of_kind(&r, TensionKind::DuplicateDefinition)
    );
    r.read().tensions().unwrap();
    let c3 = created(
        &put(&mut r, "c", claim("every payment  retries the REQUEST.")),
        "$c",
    );
    assert_carry_matches(&r);
    let t = one(&r, TensionKind::DuplicateDefinition);
    assert_eq!(t.blocks(), sorted(vec![c1, c3]));
    assert!(
        t.summary
            .contains("have the same text \"every payment retries the request.\""),
        "{}",
        t.summary
    );
    assert_eq!(t.id, tension_id(t.kind, t.severity, &t.involved));

    // a plural name pair with one text is that name group's tension, reported once, as the name
    let (_d2, mut r) = project();
    let s = spec_doc(&mut r);
    let iface = |name: &str| {
        json!([{"op": "create", "type": "interface", "id": "$i", "parent": s.section.to_string(),
                "fields": {"name": name}, "body": "Charges a card once per order."}])
    };
    put(&mut r, "a", iface("Pay API"));
    put(&mut r, "b", iface("Pay APIs"));
    let all = of_kind(&r, TensionKind::DuplicateDefinition);
    assert_eq!(all.len(), 1, "{all:#?}");
    assert!(
        all[0].summary.contains("both define \"pay api\""),
        "{}",
        all[0].summary
    );
}
