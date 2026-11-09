//! rpl13 (repo links, D-8VJAZ): cited content is read-only here. A write that edits, moves,
//! deletes, supersedes, refines or revokes a cited block or an external decision is refused with
//! `E_FOREIGN_READ_ONLY`, before local-id parsing loses its foreign identity. A project departs
//! from one with a decision that links `deviates_from`: an engine link to a cited decision, a
//! cited instruction or an external decision, stored as a foreign link. The deviation keeps every
//! local decision rule, and it governs the local blocks that cite its target; its decision id is
//! the waiver key.
mod common;

use rdy_engine::*;
use rdy_model::foreign::{
    ForeignDerived, ForeignEntry, ForeignEvent, ForeignId, ForeignLink, ForeignPin,
    ForeignPublication, ForeignSource, ForeignVersion, selection_identity,
};
use rdy_model::*;
use rdy_store::{codec, kinds};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

const AGENTS: &str = "pf-AGNT1";

const READ_ONLY_FIX: &str = "change it in its home project; to depart from it here, record a deviation: rdy_decide with deviates_from / a decision with deviates_from in rdy edit";
const ADR_FIX: &str = "change it in its home repo; to depart from it here, record a deviation: rdy_decide with deviates_from / a decision with deviates_from in rdy edit";
const DEVIATION_FIX: &str = "name a cited decision, a cited instruction or an external decision";

fn writer() -> Session {
    Session::human("human:founder".into(), Role::Writer).unwrap()
}

fn founder() -> Session {
    Session::human("human:founder".into(), Role::Approver).unwrap()
}

fn try_write(r: &mut Rdy, view: &str, ops: Value) -> Result<WriteResult, RdyError> {
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: parse_ops(ops.as_array().unwrap().clone()).unwrap(),
            justification: None,
            message: format!("write to {view}"),
        },
    )
}

fn write(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    try_write(r, view, ops).unwrap_or_else(|e| panic!("write to {view}: {e:?}"))
}

fn land(r: &mut Rdy, view: &str) {
    let p = r
        .propose(&writer(), view, view, Some(common::TEST_SUMMARY))
        .unwrap();
    common::confirm_exceptions(r, &founder(), &p);
    r.land(&founder(), &p.id)
        .unwrap_or_else(|e| panic!("land {view}: {e:?}"));
}

fn cited() -> RepoId {
    RepoId([7; 16])
}

/// The repo id of the ADR source `adrs` (any id other than this project's does here).
fn adr_repo() -> RepoId {
    RepoId([9; 16])
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// A block as the cited side delivers it.
fn block(id_: &str, ty: &str, fields: &[(&str, &str)], body: &str) -> Block {
    Block {
        id: id(id_),
        block_type: ty.into(),
        parent: None,
        order: "V".into(),
        fields: fields
            .iter()
            .map(|(k, v)| (k.to_string(), FieldValue::Str((*v).into())))
            .collect(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    }
}

fn version_of(b: &Block) -> Hash {
    Hash::of(&codec::item_new(kinds::BLOCK, b).1)
}

fn content_of(b: &Block) -> Hash {
    Hash::of(format!("content of {} {}", b.id, b.body).as_bytes())
}

fn pin_of(b: &Block) -> ForeignPin {
    ForeignPin {
        content: content_of(b),
        encoding: 0,
    }
}

fn fv(b: &Block) -> ForeignVersion {
    ForeignVersion {
        id: b.id.clone(),
        version: version_of(b),
        content: content_of(b),
        encoding: 0,
        derived: ForeignDerived::default(),
    }
}

/// Writes a first delivery of `blocks` from `source` straight to the store and publishes it.
fn deliver(r: &mut Rdy, repo: RepoId, source: ForeignSource, profile: &str, blocks: &[&Block]) {
    let sel: BTreeMap<BlockId, Hash> = blocks
        .iter()
        .map(|b| (b.id.clone(), version_of(b)))
        .collect();
    let entry = ForeignEntry {
        repo,
        source,
        event: ForeignEvent::Delivery {
            prev: None,
            publication: ForeignPublication {
                tag: "v1".into(),
                profile: profile.into(),
                profile_version: None,
            },
            set: None,
            added: blocks.iter().map(|b| fv(b)).collect(),
            changed: vec![],
            removed: vec![],
            selection: selection_identity(sel.iter()).unwrap(),
        },
    };
    let held: Vec<Block> = blocks.iter().map(|b| (*b).clone()).collect();
    r.store_mut().put_foreign(&entry, &held).unwrap();
    r.publish_after_foreign_for_tests().unwrap();
}

/// What the cited project and the ADR source deliver.
struct Cited {
    /// A cited decision, key CORE-RULE.
    x: Block,
    /// A cited instruction.
    i: Block,
    /// A cited claim.
    c: Block,
    /// An external decision, key ADR-0007.
    a: Block,
}

/// A landed project with a doc, a section, a local claim and a local decision, and a repo link
/// `core` under the agents profile with its first delivery, and an ADR source `adrs`.
struct World {
    r: Rdy,
    sec: String,
    claim: String,
    dec: String,
    c: Cited,
}

fn world(base: &Path) -> World {
    let mut r = Rdy::init(&base.join("p"), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Local claim."},
            {"op": "create", "type": "repo_link", "id": "$a", "parent": "$d",
             "fields": {"alias": "core", "repo": cited().to_string(), "profile": AGENTS}}
        ]),
    );
    let sec = w.created["$s"].clone();
    let dec = r
        .decide(
            &writer(),
            "seed",
            DecisionInput {
                parent: sec.clone(),
                statement: "Keep the local rule.".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    land(&mut r, "seed");
    let c = Cited {
        x: block(
            "d-FRGNX",
            "decision",
            &[("statement", "Use X."), ("key", "CORE-RULE")],
            "",
        ),
        i: block("ai-FRGN1", "instruction", &[], "Always X."),
        c: block("c-FRGNC", "claim", &[], "X is fast."),
        a: block(
            "d-ADR01",
            "decision",
            &[("statement", "Use A."), ("key", "ADR-0007")],
            "",
        ),
    };
    let link = ForeignSource::Link {
        link: w.created["$a"].clone(),
        site: "hub.example".into(),
        at_ms: 1_760_000_000_000,
    };
    deliver(&mut r, cited(), link, AGENTS, &[&c.x, &c.i, &c.c]);
    let adr = ForeignSource::Adr {
        source: "adrs".into(),
        revision: "r1".into(),
    };
    deliver(&mut r, adr_repo(), adr, "adr", &[&c.a]);
    World {
        r,
        sec,
        claim: w.created["$c"].clone(),
        dec,
        c,
    }
}

fn get(r: &Rdy, view: &str, block: &str) -> Block {
    let st = r.read();
    let m = st.materialized(&Scope::View(view.into())).unwrap();
    m.get(&id(block)).unwrap().clone()
}

fn fid(repo: RepoId, b: &Block) -> ForeignId {
    ForeignId::new(repo, b.id.clone())
}

fn expect_err(e: RdyError, code: &str, message: &str, fix: &str) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (code, message, fix),
        "{e:?}"
    );
}

fn read_only(e: RdyError, r#ref: &str) {
    expect_err(
        e,
        "E_FOREIGN_READ_ONLY",
        &format!("{ref} is cited from core and is read-only here"),
        READ_ONLY_FIX,
    );
}

fn adr_read_only(e: RdyError, key: &str) {
    expect_err(
        e,
        "E_FOREIGN_READ_ONLY",
        &format!("{key} is an external decision and is read-only here"),
        ADR_FIX,
    );
}

fn deviate(r: &mut Rdy, view: &str, sec: &str, targets: &[&str]) -> Result<WriteResult, RdyError> {
    r.decide(
        &writer(),
        view,
        DecisionInput {
            parent: sec.into(),
            statement: "Depart from the cited rule here.".into(),
            deviates_from: targets.iter().map(|t| t.to_string()).collect(),
            ..Default::default()
        },
    )
}

#[test]
fn rpl13_every_foreign_mutation_is_read_only() {
    let d = tempfile::tempdir().unwrap();
    let World {
        mut r, sec, dec, c, ..
    } = world(d.path());
    let stored = format!("{}:d-FRGNX", cited());
    let adr_stored = format!("{}:d-ADR01", adr_repo());

    // edit, move, delete, and the repin and reaffirm edits, by alias id, alias key or stored form
    for target in ["core:d-FRGNX", stored.as_str()] {
        for op in [
            json!({"op": "update", "id": target, "body": "Changed."}),
            json!({"op": "move", "id": target, "parent": sec}),
            json!({"op": "delete", "id": target}),
            json!({"op": "repin", "id": target}),
            json!({"op": "reaffirm", "id": target, "reason": "Still holds."}),
        ] {
            read_only(
                try_write(&mut r, "w", json!([op])).unwrap_err(),
                "core:d-FRGNX",
            );
        }
    }
    read_only(
        try_write(
            &mut r,
            "w",
            json!([{"op": "update", "id": "core:CORE-RULE", "body": "Changed."}]),
        )
        .unwrap_err(),
        "core:CORE-RULE",
    );

    // supersede, refine and revoke, as raw links of a new decision or an update
    for kind in ["supersedes", "refines", "revokes"] {
        let e = try_write(
            &mut r,
            "w",
            json!([{"op": "create", "type": "decision", "parent": sec,
                    "fields": {"statement": "S.", "status": "proposed"},
                    "links": {kind: ["core:d-FRGNX"]}}]),
        )
        .unwrap_err();
        read_only(e, "core:d-FRGNX");
        let e = try_write(
            &mut r,
            "w",
            json!([{"op": "update", "id": dec, "links": {kind: [stored]}}]),
        )
        .unwrap_err();
        read_only(e, "core:d-FRGNX");
    }

    // and through the decision paths
    let input = |refines: Vec<String>| DecisionInput {
        parent: sec.clone(),
        statement: "Replace it.".into(),
        refines,
        ..Default::default()
    };
    read_only(
        r.supersede(&writer(), "w", "core:d-FRGNX", None, input(vec![]))
            .unwrap_err(),
        "core:d-FRGNX",
    );
    read_only(
        r.revoke(&writer(), "w", &stored, "No longer.", &sec, None)
            .unwrap_err(),
        "core:d-FRGNX",
    );
    read_only(
        r.decide(&writer(), "w", input(vec!["core:CORE-RULE".into()]))
            .unwrap_err(),
        "core:CORE-RULE",
    );

    // an external decision: by its stored form, by its source name and key
    for op in [
        json!({"op": "update", "id": adr_stored, "body": "Changed."}),
        json!({"op": "delete", "id": adr_stored}),
    ] {
        adr_read_only(try_write(&mut r, "w", json!([op])).unwrap_err(), "ADR-0007");
    }
    adr_read_only(
        try_write(
            &mut r,
            "w",
            json!([{"op": "update", "id": "adrs:ADR-0007", "body": "Changed."}]),
        )
        .unwrap_err(),
        "ADR-0007",
    );
    adr_read_only(
        r.supersede(&writer(), "w", &adr_stored, None, input(vec![]))
            .unwrap_err(),
        "ADR-0007",
    );
    adr_read_only(
        r.revoke(&writer(), "w", &adr_stored, "No longer.", &sec, None)
            .unwrap_err(),
        "ADR-0007",
    );

    // an alias no live link or ADR source has is no cited block
    expect_err(
        try_write(&mut r, "w", json!([{"op": "delete", "id": "nope:d-FRGNX"}])).unwrap_err(),
        E_UNKNOWN_REF,
        "no repo link has the alias nope",
        "use the alias of a linked project: rdy_links / rdy links",
    );
    // every refusal wrote nothing
    assert!(r.read().included("w").is_empty());

    // citing is no mutation: a reference and a premise link stay allowed
    let w = write(
        &mut r,
        "w",
        json!([{"op": "create", "type": "claim", "id": "$c", "parent": sec,
                "body": "Per {{core:d-FRGNX}}.",
                "links": {"decided_by": ["core:d-FRGNX"]}}]),
    );
    let got = get(&r, "w", &w.created["$c"]);
    assert_eq!(
        got.foreign.pins,
        BTreeMap::from([(fid(cited(), &c.x), pin_of(&c.x))])
    );
    let dep = r
        .decide(
            &writer(),
            "w",
            DecisionInput {
                parent: sec.clone(),
                statement: "Build on it.".into(),
                depends_on: vec!["core:d-FRGNX".into()],
                ..Default::default()
            },
        )
        .unwrap();
    assert!(dep.created.contains_key("$decision"));
    r.close().unwrap();
}

#[test]
fn rpl13_deviation_targets_only_cited_decision_instruction_or_adr() {
    let d = tempfile::tempdir().unwrap();
    let World {
        mut r,
        sec,
        claim,
        dec,
        c,
    } = world(d.path());

    // a cited decision, a cited instruction and an external decision are deviation targets
    let adr_stored = format!("{}:d-ADR01", adr_repo());
    let w = deviate(
        &mut r,
        "w",
        &sec,
        &["core:CORE-RULE", "core:ai-FRGN1", &adr_stored],
    )
    .unwrap();
    let got = get(&r, "w", &w.created["$decision"]);
    assert!(!got.links.contains_key("deviates_from"), "no local link");
    let link = |repo, b| ForeignLink {
        target: fid(repo, b),
        part: None,
    };
    assert_eq!(
        got.foreign.links,
        BTreeMap::from([(
            "deviates_from".to_string(),
            vec![
                link(cited(), &c.x),
                link(cited(), &c.i),
                link(adr_repo(), &c.a)
            ]
        )])
    );
    assert_eq!(
        got.foreign.pins,
        BTreeMap::from([
            (fid(cited(), &c.x), pin_of(&c.x)),
            (fid(cited(), &c.i), pin_of(&c.i)),
            (fid(adr_repo(), &c.a), pin_of(&c.a)),
        ])
    );

    // the structured write takes the same link, on a create and on an edit of a proposed one
    let w = write(
        &mut r,
        "w",
        json!([{"op": "create", "type": "decision", "id": "$d", "parent": sec,
                "fields": {"statement": "Depart.", "status": "proposed"},
                "links": {"deviates_from": ["core:d-FRGNX"]}}]),
    );
    let raw = w.created["$d"].clone();
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": raw, "links": {"deviates_from": ["core:ai-FRGN1"]}}]),
    );
    assert_eq!(
        get(&r, "w", &raw).foreign.links["deviates_from"],
        vec![link(cited(), &c.i)]
    );

    // a local block, cited or not, is no deviation target
    let not_cited =
        |r#ref: &str| format!("deviates_from names {ref}, which is not cited or external");
    for local in [claim.as_str(), dec.as_str()] {
        expect_err(
            deviate(&mut r, "x", &sec, &[local]).unwrap_err(),
            E_VALIDATION,
            &not_cited(local),
            DEVIATION_FIX,
        );
    }
    let e = try_write(
        &mut r,
        "x",
        json!([{"op": "update", "id": raw, "links": {"deviates_from": [dec]}}]),
    )
    .unwrap_err();
    expect_err(e, E_VALIDATION, &not_cited(&dec), DEVIATION_FIX);
    // nor is a cited block that is neither a decision nor an instruction
    expect_err(
        deviate(&mut r, "x", &sec, &["core:c-FRGNC"]).unwrap_err(),
        E_VALIDATION,
        &not_cited("core:c-FRGNC"),
        DEVIATION_FIX,
    );
    let stored_claim = format!("{}:c-FRGNC", cited());
    expect_err(
        deviate(&mut r, "x", &sec, &[&stored_claim]).unwrap_err(),
        E_VALIDATION,
        &not_cited("core:c-FRGNC"),
        DEVIATION_FIX,
    );
    // a block the link does not deliver keeps the alias refusal
    expect_err(
        deviate(&mut r, "x", &sec, &["core:d-NNNNN"]).unwrap_err(),
        E_UNKNOWN_REF,
        "core:d-NNNNN is not in the content delivered from core",
        "cite a block that core delivers: rdy_links {op: \"show\", alias} / rdy links show core",
    );
    assert!(r.read().included("x").is_empty(), "refusals wrote nothing");
    r.close().unwrap();
}

#[test]
fn rpl13_deviation_preserves_local_decision_rules() {
    let d = tempfile::tempdir().unwrap();
    let World {
        mut r,
        sec,
        claim,
        c,
        ..
    } = world(d.path());

    // a deviation is an ordinary proposed decision: its local links pin locally
    let w = r
        .decide(
            &writer(),
            "dev",
            DecisionInput {
                parent: sec.clone(),
                statement: "Depart from X here.".into(),
                depends_on: vec![claim.clone()],
                deviates_from: vec!["core:d-FRGNX".into()],
                ..Default::default()
            },
        )
        .unwrap();
    let dev = w.created["$decision"].clone();
    let got = get(&r, "dev", &dev);
    assert_eq!(got.field_str("status"), Some("proposed"));
    assert!(got.pins.contains_key(&id(&claim)), "local pin");
    assert_eq!(got.links.keys().collect::<Vec<_>>(), vec!["depends_on"]);

    // only a decision departs from a cited block
    let e = try_write(
        &mut r,
        "dev",
        json!([{"op": "create", "type": "claim", "parent": sec, "body": "B.",
                "links": {"deviates_from": ["core:d-FRGNX"]}}]),
    )
    .unwrap_err();
    assert_eq!(e.code, E_VALIDATION, "{e:?}");
    assert_eq!(
        e.message,
        "type claim cannot carry deviates_from: only a decision departs from a cited block"
    );
    assert_eq!(
        e.fix,
        "remove deviates_from; record the departure as a decision: rdy_decide with deviates_from / a decision with deviates_from in rdy edit"
    );
    assert_eq!(e.issues[0].code, "E_LINK_TYPE");
    // a writer cannot accept it; the status stays the landing's
    let e = try_write(
        &mut r,
        "dev",
        json!([{"op": "update", "id": dev, "fields": {"status": "accepted"}}]),
    )
    .unwrap_err();
    assert_eq!(e.code, E_PERMISSION, "{e:?}");

    // the local blocks that cite its target are governed by it; the decision id is the key
    let w = write(
        &mut r,
        "dev",
        json!([
            {"op": "create", "type": "claim", "id": "$x", "parent": sec, "body": "Per {{core:d-FRGNX}}."},
            {"op": "create", "type": "claim", "id": "$i", "parent": sec, "body": "Per {{core:ai-FRGN1}}."}
        ]),
    );
    let (cx, ci) = (w.created["$x"].clone(), w.created["$i"].clone());
    let x = fid(cited(), &c.x);
    let want = |live| {
        vec![Deviation {
            decision: id(&dev),
            target: x.clone(),
            live,
            governs: vec![id(&cx)],
        }]
    };
    assert_eq!(
        r.read().deviations(&Scope::View("dev".into())).unwrap(),
        want(false)
    );
    assert!(r.read().deviations(&Scope::Canon).unwrap().is_empty());

    // it lands as any decision does, and is then frozen
    land(&mut r, "dev");
    assert_eq!(r.read().deviations(&Scope::Canon).unwrap(), want(true));
    let e = try_write(
        &mut r,
        "after",
        json!([{"op": "update", "id": dev, "links": {"deviates_from": ["core:ai-FRGN1"]}}]),
    )
    .unwrap_err();
    assert_eq!(e.code, E_FROZEN, "{e:?}");
    assert!(
        !want(true)[0].governs.contains(&id(&ci)),
        "a block that cites another target is not governed"
    );

    // a local revocation ends it
    r.revoke(&writer(), "end", &dev, "Back to X.", &sec, None)
        .unwrap();
    land(&mut r, "end");
    assert!(r.read().deviations(&Scope::Canon).unwrap().is_empty());
    r.close().unwrap();
}

#[test]
fn rpl13_foreign_link_kinds_follow_the_pack() {
    let d = tempfile::tempdir().unwrap();
    let World { mut r, sec, .. } = world(d.path());
    // a foreign link of a kind the pack does not declare is refused, as a local one is
    let e = try_write(
        &mut r,
        "w",
        json!([{"op": "create", "type": "claim", "parent": sec, "body": "B.",
                "links": {"bogus": ["core:d-FRGNX"]}}]),
    )
    .unwrap_err();
    assert_eq!(e.code, E_VALIDATION, "{e:?}");
    assert_eq!(e.message, "unknown link type \"bogus\"");
    assert_eq!(e.issues[0].code, "E_LINK_TYPE");
    r.close().unwrap();
}
