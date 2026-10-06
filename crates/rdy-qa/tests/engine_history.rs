//! hist1: a block's versions with their changes, authors, justifications and landings.
use rdy_engine::*;
use rdy_model::{BlockId, EditorialClass, Justification};
use rdy_qa::fixtures::*;
use serde_json::{from_value, json};

fn req(
    view: &str,
    ops: serde_json::Value,
    j: Option<Justification>,
    message: &str,
) -> WriteRequest {
    WriteRequest {
        view: view.into(),
        ops: from_value(ops).unwrap(),
        justification: j,
        message: message.into(),
    }
}

/// A landed glossary doc with a section, a claim and a decision; returns (doc, section, claim, decision).
fn seeded(r: &mut Rdy) -> (String, String, String, String) {
    let res = write(r, "seed", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Tx", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "One."}
    ]))
    .unwrap();
    let section = res.created["$s"].clone();
    let dec = r
        .decide(
            &writer(),
            "seed",
            DecisionInput {
                parent: section.clone(),
                statement: "Claims are short".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    land(r, "seed").unwrap();
    (
        res.created["$d"].clone(),
        section,
        res.created["$c"].clone(),
        dec,
    )
}

#[test]
fn hist1_versions_list_newest_first_with_author_justification_and_landing() {
    let (_d, mut r) = project();
    let (_doc, _s, claim, dec) = seeded(&mut r);
    let id = BlockId::parse(&claim).unwrap();
    let a = agent("agent:a", Some("ra"));
    let w2 = r
        .write(
            &a,
            req(
                "ra",
                json!([{"op": "update", "id": claim, "body": "Two."}]),
                Some(Justification::Implements(BlockId::parse(&dec).unwrap())),
                "tighten",
            ),
        )
        .unwrap();
    land(&mut r, "ra").unwrap();
    let head = r.read().canon_head().unwrap().to_string();
    let w3 = r
        .write(
            &writer(),
            req(
                "work",
                json!([{"op": "update", "id": claim, "body": "Three."}]),
                Some(Justification::Editorial {
                    class: EditorialClass::Typo,
                    reason: "spelling".into(),
                }),
                "",
            ),
        )
        .unwrap();
    let work = Scope::View("work".into());
    let h = r.read().history(&work, &id, 10).unwrap();
    assert!(!h.more);
    let e = &h.entries;
    assert_eq!(e.len(), 3, "{e:?}");
    assert_eq!(
        e[0].version,
        r.read()
            .block(&work, &id)
            .unwrap()
            .version_hash()
            .to_string()
    );
    assert_eq!(
        (e[0].op.as_str(), e[0].change.as_str()),
        ("update", w3.change.as_str())
    );
    assert_eq!(
        e[0].justification,
        Some(JustificationInfo::Editorial {
            class: "typo".into(),
            reason: "spelling".into()
        })
    );
    assert!(e[0].landing.is_none(), "an unlanded change has no landing");
    assert_eq!(e[0].author.actor, "human:qa");
    assert_eq!(e[1].change, w2.change);
    assert_eq!(
        (
            e[1].author.actor.as_str(),
            e[1].author.run.as_deref(),
            e[1].author.role.as_str()
        ),
        ("agent:a", Some("ra"), "writer")
    );
    assert_eq!(e[1].message, "tighten");
    assert_eq!(
        e[1].justification,
        Some(JustificationInfo::Implements(dec.clone()))
    );
    let l = e[1].landing.as_ref().expect("landed");
    assert_eq!(
        (l.id.as_str(), l.approved_by.as_str(), l.assurance.as_str()),
        (head.as_str(), "human:qa", "local")
    );
    assert_eq!(e[2].op, "create");
    assert!(e[2].landing.is_some() && e[2].justification.is_none());
    // canon has not seen the third version
    let c = r.read().history(&Scope::Canon, &id, 10).unwrap();
    assert_eq!(c.entries.len(), 2);
    assert_eq!(c.entries[0].change, w2.change);
    // the JSON shape every surface carries
    let v = serde_json::to_value(&e[1]).unwrap();
    assert_eq!(v["justification"], json!({"implements": dec}));
    assert_eq!(v["landing"]["id"], head);
    assert_eq!(
        v["author"],
        json!({"actor": "agent:a", "run": "ra", "role": "writer", "at_ms": e[1].author.at_ms})
    );
    assert_eq!(
        serde_json::to_value(&e[0]).unwrap()["justification"],
        json!({"editorial": {"class": "typo", "reason": "spelling"}})
    );
    assert_eq!(
        e[1].justification.as_ref().unwrap().label(),
        format!("implements {dec}")
    );
    assert_eq!(
        e[0].justification.as_ref().unwrap().label(),
        "editorial typo"
    );
}

#[test]
fn hist1_the_same_version_written_in_two_views_names_each_views_own_change() {
    let (_d, mut r) = project();
    let (_doc, _s, claim, _dec) = seeded(&mut r);
    let id = BlockId::parse(&claim).unwrap();
    let edit = json!([{"op": "update", "id": claim, "body": "Same."}]);
    let wa = r
        .write(
            &agent("agent:a", Some("ra")),
            req("ra", edit.clone(), None, ""),
        )
        .unwrap();
    let wb = r
        .write(&agent("agent:b", Some("rb")), req("rb", edit, None, ""))
        .unwrap();
    assert_ne!(wa.change, wb.change, "two changes: different provenance");
    {
        let st = r.read();
        let ha = st.history(&Scope::View("ra".into()), &id, 1).unwrap();
        let hb = st.history(&Scope::View("rb".into()), &id, 1).unwrap();
        assert_eq!(
            ha.entries[0].version, hb.entries[0].version,
            "one version hash"
        );
        assert_eq!(
            ha.entries[0].change, wa.change,
            "view ra names its own change"
        );
        assert_eq!(
            hb.entries[0].change, wb.change,
            "view rb names its own change"
        );
        assert!(hb.entries[0].landing.is_none());
        assert!(ha.more && hb.more, "the create is older");
    }
    land(&mut r, "rb").unwrap();
    let head = r.read().canon_head().unwrap().to_string();
    let hc = r.read().history(&Scope::Canon, &id, 1).unwrap();
    assert_eq!(
        hc.entries[0].change, wb.change,
        "canon names the landed producer"
    );
    assert_eq!(
        hc.entries[0].landing.as_ref().map(|l| l.id.clone()),
        Some(head),
        "the landing shows on the first read after it"
    );
}

#[test]
fn hist1_limit_marks_more_an_as_of_scope_starts_there_and_bad_input_is_refused() {
    let (_d, mut r) = project();
    let (_doc, _s, claim, _dec) = seeded(&mut r);
    let first = r.read().canon_head().unwrap();
    write(
        &mut r,
        "w2",
        json!([{"op": "update", "id": claim, "body": "Two."}]),
    )
    .unwrap();
    land(&mut r, "w2").unwrap();
    let id = BlockId::parse(&claim).unwrap();
    let st = r.read();
    let h = st.history(&Scope::Canon, &id, 1).unwrap();
    assert_eq!((h.entries.len(), h.more), (1, true));
    let h = st.history(&Scope::Canon, &id, 2).unwrap();
    assert_eq!((h.entries.len(), h.more), (2, false));
    let old = st.history(&Scope::At(first), &id, 10).unwrap();
    assert_eq!(old.entries.len(), 1);
    assert_eq!(old.entries[0].op, "create");
    for bad in [0, HISTORY_MAX + 1] {
        let e = st.history(&Scope::Canon, &id, bad).unwrap_err();
        assert_eq!(e.code, E_BAD_INPUT, "{e}");
        assert!(e.message.contains("1..=1000"), "{e}");
    }
    let e = st
        .history(&Scope::Canon, &BlockId::parse("c-ZZZZZ").unwrap(), 1)
        .unwrap_err();
    assert_eq!(e.code, E_UNKNOWN_BLOCK);
}

#[test]
fn hist1_last_changes_match_each_blocks_newest_entry() {
    let (_d, mut r) = project();
    let (doc, section, claim, dec) = seeded(&mut r);
    r.write(
        &agent("agent:a", Some("ra")),
        req(
            "ra",
            json!([{"op": "update", "id": claim, "body": "Two."}]),
            Some(Justification::Implements(BlockId::parse(&dec).unwrap())),
            "",
        ),
    )
    .unwrap();
    land(&mut r, "ra").unwrap();
    let st = r.read();
    let ids: Vec<BlockId> = st
        .materialized(&Scope::Canon)
        .unwrap()
        .ids()
        .cloned()
        .collect();
    let last = st.last_changes(&Scope::Canon, &ids).unwrap();
    assert_eq!(last.len(), ids.len());
    for id in &ids {
        assert_eq!(
            last[id],
            st.history(&Scope::Canon, id, 1).unwrap().entries[0],
            "{id}"
        );
    }
    let bid = |s: &str| BlockId::parse(s).unwrap();
    assert_eq!(
        last[&bid(&claim)].justification,
        Some(JustificationInfo::Implements(dec))
    );
    assert_eq!(
        last[&bid(&doc)].change,
        last[&bid(&section)].change,
        "one change wrote both"
    );
    let e = st
        .last_changes(&Scope::Canon, &[bid("c-ZZZZZ")])
        .unwrap_err();
    assert_eq!(e.code, E_UNKNOWN_BLOCK);
}

#[test]
fn hist1_times_format_as_rfc3339_utc_to_the_second() {
    assert_eq!(rfc3339(1_791_190_800_000), "2026-10-05T09:00:00Z");
    assert_eq!(rfc3339(1_791_190_800_999), "2026-10-05T09:00:00Z");
    assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
    assert_eq!(rfc3339(-1), "1969-12-31T23:59:59Z");
}
