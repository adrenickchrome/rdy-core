//! rpl15 (repo links, D-Y49PX, D-8VJAZ): the five deterministic kinds across projects. A side
//! cited from a linked project is `SideRef::Foreign {repo, entry}`: the cited repo and the
//! FOREIGN entry that delivered the involved version, shown `foreign:<alias>@<12 hex>`. Each
//! such tension is advisory unless the citing pack's `tensions_across` raises its kind. A
//! canon-or-foreign tension stays in canon scope. A live deviation resolves only the tensions
//! between the blocks it governs and its target, shown `deviated: <decision>`.
use rdy_engine::foreign_delivery::{DeliveryTaken, take_delivery};
use rdy_engine::tensions::api::{TensionsRequest, run_tensions};
use rdy_engine::tensions::*;
use rdy_engine::{DecisionInput, Local, Rdy, WriteResult};
use rdy_model::foreign::{
    ForeignDerived, ForeignPublication, ForeignSet, ForeignSource, ForeignVersion,
    selection_identity,
};
use rdy_model::{Block, BlockId, FieldValue, Hash, RepoId};
use rdy_qa::fixtures::*;
use rdy_store::codec;
use rdy_store::format::baseline_features;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_stream::{DeliveryRecord, StreamHeader, StreamRecord, StreamWriter};
use rdy_store::transition::{Cause, SideV1, TransitionsV1};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

const PROFILE: &str = "pf-AGNT1";

fn cited() -> RepoId {
    RepoId([7; 16])
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn created(w: &WriteResult, name: &str) -> BlockId {
    BlockId::parse(&w.created[name]).unwrap()
}

fn put(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    write(r, view, ops).unwrap_or_else(|e| panic!("write to {view}: {e:?}"))
}

/// A cited block, as the cited side delivers it.
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

fn live() -> ForeignDerived {
    ForeignDerived {
        status: None,
        lifecycle: None,
        superseded_by: vec![],
    }
}

fn deprecated() -> ForeignDerived {
    ForeignDerived {
        status: None,
        lifecycle: Some("deprecated".into()),
        superseded_by: vec![],
    }
}

/// A link delivery stream of `repo` holding `selected` (any order), written to `dir/name`.
fn stream(dir: &Path, name: &str, repo: RepoId, selected: &[(&Block, ForeignDerived)]) -> File {
    let mut selected = selected.to_vec();
    selected.sort_by(|a, b| a.0.id.cmp(&b.0.id));
    let versions: Vec<ForeignVersion> = selected
        .iter()
        .map(|(b, d)| ForeignVersion {
            id: b.id.clone(),
            version: b.version_hash(),
            content: b.content_hash(),
            encoding: 0,
            derived: d.clone(),
        })
        .collect();
    let delivery = DeliveryRecord {
        publication: ForeignPublication {
            tag: name.into(),
            profile: PROFILE.into(),
            profile_version: None,
        },
        selection: selection_identity(versions.iter().map(|v| (&v.id, &v.version))).unwrap(),
        versions,
        shells: vec![],
    };
    let header = StreamHeader::link_delivery(baseline_features(), repo, Hash::of(name.as_bytes()));
    let mut w = StreamWriter::new_delivery(Vec::new(), &header, &delivery).unwrap();
    for (b, _) in &selected {
        let o = codec::encode_new(kinds::BLOCK, *b);
        w.record(&StreamRecord::Present {
            key: ObjectKey {
                kind: kinds::BLOCK,
                hash: Hash::of(&o.payload),
            },
            encoding: o.version,
            payload: o.payload,
        })
        .unwrap();
    }
    let path = dir.join(name);
    std::fs::write(&path, w.finish().unwrap().0).unwrap();
    File::open(path).unwrap()
}

/// The Link source of repo link `link`.
fn source(link: &BlockId) -> ForeignSource {
    ForeignSource::Link {
        link: link.to_string(),
        site: "hub.example".into(),
        at_ms: 1_760_000_000_000,
    }
}

/// Takes the delivery of `selected` from `repo` for link `link` (set `set`).
fn deliver(
    r: &mut Rdy,
    dir: &Path,
    name: &str,
    link: &BlockId,
    repo: RepoId,
    set: Option<ForeignSet>,
    selected: &[(&Block, ForeignDerived)],
) -> DeliveryTaken {
    take_delivery(r, &stream(dir, name, repo, selected), &source(link), set)
        .unwrap_or_else(|e| panic!("take {name}: {e:?}"))
}

/// A landed doc and section with repo links `links` (alias, repo); returns the section and the
/// links by alias.
fn linked(r: &mut Rdy, links: &[(&str, RepoId)]) -> (BlockId, BTreeMap<String, BlockId>) {
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d",
               "fields": {"title": "Guide", "doc_type": "overview"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$d",
               "fields": {"title": "Rules"}}),
    ];
    for (n, (alias, repo)) in links.iter().enumerate() {
        ops.push(
            json!({"op": "create", "type": "repo_link", "id": format!("$l{n}"),
            "parent": "$d",
            "fields": {"alias": alias, "repo": repo.to_string(), "profile": PROFILE}}),
        );
    }
    let w = put(r, "seed", Value::Array(ops));
    land(r, "seed").unwrap();
    let by_alias = links
        .iter()
        .enumerate()
        .map(|(n, (alias, _))| (alias.to_string(), created(&w, &format!("$l{n}"))))
        .collect();
    (created(&w, "$s"), by_alias)
}

/// Every event-derived tension of the published state with a foreign side.
fn foreign_tensions(r: &Rdy) -> Vec<Arc<Tension>> {
    r.read()
        .tensions()
        .unwrap()
        .iter()
        .filter(|t| t.sides.iter().any(SideRef::is_foreign))
        .cloned()
        .collect()
}

/// The one tension of `kind` with a foreign side that involves local block `b`.
fn across(r: &Rdy, kind: TensionKind, b: &BlockId) -> Arc<Tension> {
    let mut v: Vec<_> = foreign_tensions(r)
        .into_iter()
        .filter(|t| t.kind == kind && t.blocks().contains(b))
        .collect();
    assert_eq!(v.len(), 1, "{kind} on {b}: {:#?}", foreign_tensions(r));
    v.remove(0)
}

/// The foreign side of `t` (exactly one) and the cited block it involves.
fn foreign_side(t: &Tension) -> (RepoId, Hash, BlockId) {
    let items: Vec<_> = t.involved.iter().filter(|i| i.side.is_foreign()).collect();
    assert_eq!(items.len(), 1, "{t:#?}");
    match &items[0].side {
        SideRef::Foreign { repo, entry } => (*repo, *entry, items[0].block.clone()),
        _ => unreachable!(),
    }
}

/// The TRANSITION record of the batch the store sealed last, if any.
fn last_record(r: &Rdy) -> Option<TransitionsV1> {
    let h = r
        .store()
        .last_batch()
        .iter()
        .find(|(k, _)| *k == kinds::TRANSITION)
        .map(|(_, h)| *h)?;
    Some(r.read().reader().get_typed(kinds::TRANSITION, &h).unwrap())
}

#[test]
fn rpl15_five_triggers_have_foreign_sides() {
    let (_d, mut r) = project();
    let files = tempfile::tempdir().unwrap();
    let (s, links) = linked(&mut r, &[("core", cited())]);
    let link = links["core"].clone();
    let ledger = block(
        "t-FRGNT",
        "term",
        &[("name", "Ledger")],
        "The book of accounts.",
    );
    let timeout = block(
        "v-FRGNV",
        "value",
        &[("name", "Timeout"), ("value", "30"), ("unit", "s")],
        "",
    );
    let trace1 = block(
        "c-FRGNX",
        "claim",
        &[],
        "every request carries a trace id that the gateway sets and every service keeps it",
    );
    let old = block(
        "c-FRGND",
        "claim",
        &[],
        "Old services answer on port eighty.",
    );
    let retry1 = block("c-FRGNP", "claim", &[], "Retries back off.");
    let v1 = deliver(
        &mut r,
        files.path(),
        "v1",
        &link,
        cited(),
        None,
        &[
            (&ledger, live()),
            (&timeout, live()),
            (&trace1, live()),
            (&old, deprecated()),
            (&retry1, live()),
        ],
    );
    let sec = s.to_string();
    let w = put(
        &mut r,
        "w",
        json!([
            {"op": "create", "type": "term", "id": "$t", "parent": sec,
             "fields": {"name": "ledger"}, "body": "Our ledger."},
            {"op": "create", "type": "claim", "id": "$v", "parent": sec,
             "body": "The timeout is 45 s, as {{core:v-FRGNV}} says."},
            {"op": "create", "type": "claim", "id": "$x", "parent": sec,
             "body": "Every request carries a trace id that the gateway sets and every service keeps it. See {{core:c-FRGNX}}."},
            {"op": "create", "type": "claim", "id": "$o", "parent": sec,
             "body": "Follow {{core:c-FRGND}}."}
        ]),
    );
    land(&mut r, "w").unwrap();
    let (term, val, restates, citer) = (
        created(&w, "$t"),
        created(&w, "$v"),
        created(&w, "$x"),
        created(&w, "$o"),
    );
    // an open proposal pins the retry rule as v1 delivered it
    let p = put(
        &mut r,
        "p",
        json!([{"op": "create", "type": "claim", "id": "$p", "parent": sec,
                "body": "Builds on {{core:c-FRGNP}}."}]),
    );
    let premised = created(&p, "$p");
    let change: Hash = p.change.parse().unwrap();

    // a delivery changes the trace rule and the retry rule
    let trace2 = block(
        "c-FRGNX",
        "claim",
        &[],
        "every request carries a span id that the edge sets",
    );
    let retry2 = block("c-FRGNP", "claim", &[], "Retries back off exponentially.");
    let v2 = deliver(
        &mut r,
        files.path(),
        "v2",
        &link,
        cited(),
        None,
        &[
            (&ledger, live()),
            (&timeout, live()),
            (&trace2, live()),
            (&old, deprecated()),
            (&retry2, live()),
        ],
    );

    let st = r.read();
    let cases = [
        (
            TensionKind::DuplicateDefinition,
            &term,
            &ledger.id,
            v1.entry,
        ),
        (TensionKind::ValueDisagreement, &val, &timeout.id, v1.entry),
        (
            TensionKind::RestatementDrift,
            &restates,
            &trace1.id,
            v2.entry,
        ),
        (TensionKind::DeprecatedReference, &citer, &old.id, v1.entry),
        (TensionKind::PremiseShift, &premised, &retry1.id, v2.entry),
    ];
    for (kind, local, foreign, entry) in cases {
        let t = across(&r, kind, local);
        assert_eq!(
            foreign_side(&t),
            (cited(), entry, foreign.clone()),
            "{kind}: {t:#?}"
        );
        // across projects every kind is advisory unless the citing pack raises it
        assert_eq!(t.severity, TensionSeverity::Advisory, "{kind}");
        assert!(!t.overridden, "{kind}");
        assert_eq!(
            t.blocks(),
            vec![local.clone()],
            "{kind}: no cited block is local"
        );
        // the surfaces show the side by its alias and the entry's 12 hex
        let info = st.tension_info(&t);
        let shown = format!("foreign:core@{}", entry.short());
        assert!(
            info.sides.iter().any(|s| s.side == shown),
            "{kind}: {:#?}",
            info.sides
        );
        assert_eq!(info.status, "open", "{kind}");
        assert!(info.summary.contains("core:"), "{kind}: {}", info.summary);
    }
    // premise shift is the open proposal's: its side is the change, the rest are canon
    let t = across(&r, TensionKind::PremiseShift, &premised);
    assert!(t.sides.contains(&SideRef::Change(change)), "{t:#?}");
    for kind in [
        TensionKind::DuplicateDefinition,
        TensionKind::ValueDisagreement,
        TensionKind::RestatementDrift,
        TensionKind::DeprecatedReference,
    ] {
        let local = cases.iter().find(|c| c.0 == kind).unwrap().1;
        let t = across(&r, kind, local);
        assert!(
            t.sides
                .iter()
                .all(|s| matches!(s, SideRef::Canon { .. } | SideRef::Foreign { .. })),
            "{kind}: {t:#?}"
        );
    }
    // the delivery's own batch logged what it opened, its foreign sides by repo and entry
    let rec = last_record(&r).expect("the delivery logs its moves");
    let drift = across(&r, TensionKind::RestatementDrift, &restates);
    let opened = rec
        .opened
        .iter()
        .find(|m| m.tension == drift.id)
        .expect("the delivery opened the drift");
    assert!(opened.sides.contains(&SideV1::Foreign {
        repo: cited(),
        entry: v2.entry
    }));
    assert_eq!(
        (opened.why.cause, opened.why.id),
        (Cause::Delivered, Some(v2.entry))
    );
    assert_eq!(rec.invalid(), None);

    // a cited block never enters a local tension: the local sets hold no cited id
    for t in r.read().tensions().unwrap().iter() {
        if !t.sides.iter().any(SideRef::is_foreign) {
            for b in [&ledger.id, &timeout.id, &trace1.id, &old.id, &retry1.id] {
                assert!(!t.involved.iter().any(|i| &i.block == b), "{t:#?}");
            }
        }
    }
}

#[test]
fn rpl15_canon_scope_keeps_foreign_sides() {
    let (d, mut r) = project();
    let files = tempfile::tempdir().unwrap();
    let (s, links) = linked(&mut r, &[("core", cited())]);
    let w = put(
        &mut r,
        "w",
        json!([{"op": "create", "type": "term", "id": "$t", "parent": s.to_string(),
                "fields": {"name": "Ledger"}, "body": "Our ledger."}]),
    );
    land(&mut r, "w").unwrap();
    let term = created(&w, "$t");
    let ledger = block(
        "t-FRGNT",
        "term",
        &[("name", "ledger")],
        "The book of accounts.",
    );
    let v1 = deliver(
        &mut r,
        files.path(),
        "v1",
        &links["core"],
        cited(),
        None,
        &[(&ledger, live())],
    );
    let t = across(&r, TensionKind::DuplicateDefinition, &term);
    assert_eq!(foreign_side(&t), (cited(), v1.entry, ledger.id.clone()));
    // a canon-or-foreign tension is a standing condition of canon
    let canon = r.read().canon_tensions().unwrap();
    assert!(canon.iter().any(|c| c.id == t.id), "{canon:#?}");
    assert!(
        t.sides.iter().all(|s| s.is_standing()) && t.sides.iter().any(|s| s.is_foreign()),
        "{t:#?}"
    );
    // the delivery logged the opening, cause delivered, its foreign side by repo and entry
    let rec = last_record(&r).expect("the delivery logs its move");
    let m = rec.opened.iter().find(|m| m.tension == t.id).unwrap();
    assert!(m.sides.contains(&SideV1::Foreign {
        repo: cited(),
        entry: v1.entry
    }));
    assert_eq!(m.why.cause, Cause::Delivered);

    // the citing pack raises the kind: the same condition blocks, re-keyed
    std::fs::write(
        d.path().join(".rdy/packs/across.yaml"),
        "name: across\nversion: \"1\"\nextends: rdy-standard\ntensions_across:\n  duplicate_definition: blocking\n",
    )
    .unwrap();
    std::fs::write(d.path().join(".rdy.yaml"), "pack: across\n").unwrap();
    r.reload_pack().unwrap();
    let raised = across(&r, TensionKind::DuplicateDefinition, &term);
    assert_eq!(raised.severity, TensionSeverity::Blocking);
    assert!(raised.overridden);
    assert_ne!(raised.id, t.id);
    // a local duplicate of two local terms keeps its local severity
    put(
        &mut r,
        "x",
        json!([{"op": "create", "type": "term", "id": "$t", "parent": s.to_string(),
                "fields": {"name": "Fee"}, "body": "A fee."},
               {"op": "create", "type": "term", "id": "$u", "parent": s.to_string(),
                "fields": {"name": "fee"}, "body": "Another fee."}]),
    );
    let local: Vec<_> = r
        .read()
        .tensions()
        .unwrap()
        .iter()
        .filter(|t| t.kind == TensionKind::DuplicateDefinition && !t.has_foreign())
        .cloned()
        .collect();
    assert_eq!(local.len(), 1, "{local:#?}");
    assert_eq!(local[0].severity, TensionSeverity::Advisory);

    // the log is the record: a reopen finds nothing unlogged
    let f = r.fsck().unwrap();
    assert_eq!(f.unlogged_moves, 0, "{f:?}");
    let open = r.read().reader().open_tensions().unwrap();
    r.close().unwrap();
    let r = Rdy::open(d.path()).unwrap();
    assert_eq!(r.read().reader().open_tensions().unwrap(), open);
    assert_eq!(r.fsck().unwrap().unlogged_moves, 0);
    assert!(
        r.read()
            .canon_tensions()
            .unwrap()
            .iter()
            .any(|c| c.id == raised.id)
    );
    // the canon listing keeps it: its sides are canon or foreign
    let engine = Local::new(r);
    let req = TensionsRequest {
        scope: Some("canon".into()),
        ..Default::default()
    };
    let listed = run_tensions(&engine, &writer(), req).unwrap();
    let ids: Vec<&str> = listed["tensions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["tension"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&raised.id.to_string().as_str()), "{listed:#}");
}

#[test]
fn rpl15_team_text_collision_has_two_foreign_sides() {
    let (_d, mut r) = project();
    let files = tempfile::tempdir().unwrap();
    let other = RepoId([9; 16]);
    let (_s, links) = linked(&mut r, &[("red", cited()), ("blue", other), ("org", other)]);
    let text = "Always run the full test suite before you push a branch.";
    let reason = [("applies_to", "all")];
    let red = block("ai-FRGNR", "instruction", &reason, text);
    let blue = block(
        "ai-FRGNB",
        "instruction",
        &reason,
        &format!("  {}", text.to_uppercase()),
    );
    let org = block("ai-FRGNZ", "instruction", &reason, text);
    let tr = deliver(
        &mut r,
        files.path(),
        "red",
        &links["red"],
        cited(),
        Some(ForeignSet::Team("red".into())),
        &[(&red, live())],
    );
    let tb = deliver(
        &mut r,
        files.path(),
        "blue",
        &links["blue"],
        other,
        Some(ForeignSet::Team("blue".into())),
        &[(&blue, live())],
    );
    // the organization set is no team: it collides with neither
    deliver(
        &mut r,
        files.path(),
        "org",
        &links["org"],
        other,
        Some(ForeignSet::Organization),
        &[(&org, live())],
    );
    let ts: Vec<_> = foreign_tensions(&r)
        .into_iter()
        .filter(|t| t.kind == TensionKind::DuplicateDefinition)
        .collect();
    assert_eq!(ts.len(), 1, "{ts:#?}");
    let t = &ts[0];
    let mut sides = t.sides.clone();
    sides.sort();
    let mut want = vec![
        SideRef::Foreign {
            repo: cited(),
            entry: tr.entry,
        },
        SideRef::Foreign {
            repo: other,
            entry: tb.entry,
        },
    ];
    want.sort();
    assert_eq!(sides, want, "{t:#?}");
    assert!(t.blocks().is_empty(), "no local block: {t:#?}");
    let cited_ids: Vec<&BlockId> = t.involved.iter().map(|i| &i.block).collect();
    assert!(cited_ids.contains(&&red.id) && cited_ids.contains(&&blue.id));
    assert_eq!(t.severity, TensionSeverity::Advisory);
    // no local side: a standing condition, in canon scope
    assert!(
        r.read()
            .canon_tensions()
            .unwrap()
            .iter()
            .any(|c| c.id == t.id)
    );
    let info = r.read().tension_info(t);
    let shown: Vec<&str> = info.sides.iter().map(|s| s.side.as_str()).collect();
    assert!(
        shown.contains(&format!("foreign:red@{}", tr.entry.short()).as_str()),
        "{shown:?}"
    );
    assert!(
        shown.contains(&format!("foreign:blue@{}", tb.entry.short()).as_str()),
        "{shown:?}"
    );
}

#[test]
fn rpl15_live_deviation_resolves_only_covered_pairs() {
    let (_d, mut r) = project();
    let files = tempfile::tempdir().unwrap();
    let (s, links) = linked(&mut r, &[("core", cited())]);
    let text = "All services log in UTC with ISO timestamps.";
    let rule = block(
        "d-FRGNA",
        "decision",
        &[("title", "UTC logs"), ("statement", text)],
        "",
    );
    deliver(
        &mut r,
        files.path(),
        "v1",
        &links["core"],
        cited(),
        None,
        &[(&rule, live())],
    );
    // one claim cites the rule (the deviation governs it), one restates it without a citation
    let sec = s.to_string();
    let w = put(
        &mut r,
        "w",
        json!([
            {"op": "create", "type": "claim", "id": "$a", "parent": sec, "body": text,
             "links": {"implements": [{"target": "core:d-FRGNA"}]}},
            {"op": "create", "type": "claim", "id": "$b", "parent": sec, "body": text}
        ]),
    );
    land(&mut r, "w").unwrap();
    let (governed, free) = (created(&w, "$a"), created(&w, "$b"));
    let covered = across(&r, TensionKind::DuplicateDefinition, &governed);
    let uncovered = across(&r, TensionKind::DuplicateDefinition, &free);
    assert_eq!(foreign_side(&covered).2, rule.id);
    assert_eq!(foreign_side(&uncovered).2, rule.id);

    // a proposed deviation is not live: both stay open
    let dw = r
        .decide(
            &writer(),
            "dev",
            DecisionInput {
                parent: sec.clone(),
                statement: "Our services log in local time.".into(),
                deviates_from: vec!["core:d-FRGNA".into()],
                ..Default::default()
            },
        )
        .unwrap();
    let deviation = created(&dw, "$decision");
    let st = r.read();
    for t in [&covered, &uncovered] {
        let now = st.tensions().unwrap().get(&t.id).cloned().unwrap();
        assert_eq!(st.tension_status(&now), TensionStatus::Open);
    }
    drop(st);

    // landed, it resolves the tension of the block it governs and only that one
    land(&mut r, "dev").unwrap();
    let st = r.read();
    let set = st.tensions().unwrap();
    let now = set.get(&covered.id).cloned().expect("still detected");
    assert_eq!(now.deviated, Some(deviation.clone()));
    assert_eq!(
        st.tension_status(&now),
        TensionStatus::Deviated(deviation.clone())
    );
    assert_eq!(
        st.tension_info(&now).status,
        format!("deviated: {deviation}")
    );
    let other = set.get(&uncovered.id).cloned().expect("still detected");
    assert_eq!(other.deviated, None);
    assert_eq!(st.tension_status(&other), TensionStatus::Open);
    // the local duplicate of the two claims is no foreign pair: the deviation leaves it open
    let local: Vec<_> = set
        .iter()
        .filter(|t| t.kind == TensionKind::DuplicateDefinition && !t.has_foreign())
        .cloned()
        .collect();
    assert_eq!(local.len(), 1, "{local:#?}");
    assert_eq!(local[0].deviated, None);
    assert_eq!(st.tension_status(&local[0]), TensionStatus::Open);
    // the landing logged the resolution, and no other move of the uncovered tension
    let rec = last_record(&r).expect("the landing logs its moves");
    assert!(
        rec.resolved.iter().any(|m| m.tension == covered.id),
        "{rec:#?}"
    );
    assert!(!rec.resolved.iter().any(|m| m.tension == uncovered.id));
    assert!(!rec.opened.iter().any(|m| m.tension == covered.id));
    assert_eq!(rec.invalid(), None);
    assert_eq!(r.fsck().unwrap().unlogged_moves, 0);
    drop((st, set));
    // the open listing leaves it out; the uncovered one stays
    let engine = Local::new(r);
    let listed = run_tensions(&engine, &writer(), TensionsRequest::default()).unwrap();
    let ids: Vec<&str> = listed["tensions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["tension"].as_str().unwrap())
        .collect();
    assert!(
        !ids.contains(&covered.id.to_string().as_str()),
        "{listed:#}"
    );
    assert!(
        ids.contains(&uncovered.id.to_string().as_str()),
        "{listed:#}"
    );
}

#[test]
fn rpl15_drift_without_a_pin_reads_earlier_delivered_text() {
    let (_d, mut r) = project();
    let files = tempfile::tempdir().unwrap();
    let (s, links) = linked(&mut r, &[("core", cited())]);
    let link = links["core"].clone();
    let text = "every request carries a trace id that the gateway sets and every service keeps it";
    let trace1 = block("c-FRGNW", "claim", &[], text);
    deliver(
        &mut r,
        files.path(),
        "v1",
        &link,
        cited(),
        None,
        &[(&trace1, live())],
    );
    // a local claim restates the delivered text, with no citation and so no pin
    let w = put(
        &mut r,
        "w",
        json!([{"op": "create", "type": "claim", "id": "$x", "parent": s.to_string(),
                "body": text}]),
    );
    land(&mut r, "w").unwrap();
    let restates = created(&w, "$x");
    let trace2 = block(
        "c-FRGNW",
        "claim",
        &[],
        "every request carries a span id that the edge sets",
    );
    let v2 = deliver(
        &mut r,
        files.path(),
        "v2",
        &link,
        cited(),
        None,
        &[(&trace2, live())],
    );
    let t = across(&r, TensionKind::RestatementDrift, &restates);
    assert_eq!(foreign_side(&t), (cited(), v2.entry, trace2.id.clone()));
    assert_eq!(t.severity, TensionSeverity::Advisory);
}

#[test]
fn rpl15_events_carry_foreign_sides() {
    let (_d, mut r) = project();
    let files = tempfile::tempdir().unwrap();
    let other = RepoId([9; 16]);
    let (_s, links) = linked(&mut r, &[("red", cited()), ("blue", other)]);
    let text = "Always run the full test suite before you push a branch.";
    let reason = [("applies_to", "all")];
    let red = block("ai-FRGNR", "instruction", &reason, text);
    let blue = block("ai-FRGNB", "instruction", &reason, text);
    let from = r.read().seq();
    let rx = r.subscribe();
    let tr = deliver(
        &mut r,
        files.path(),
        "red",
        &links["red"],
        cited(),
        Some(ForeignSet::Team("red".into())),
        &[(&red, live())],
    );
    let tb = deliver(
        &mut r,
        files.path(),
        "blue",
        &links["blue"],
        other,
        Some(ForeignSet::Team("blue".into())),
        &[(&blue, live())],
    );
    let mut want = vec![
        SideV1::Foreign {
            repo: cited(),
            entry: tr.entry,
        },
        SideV1::Foreign {
            repo: other,
            entry: tb.entry,
        },
    ];
    want.sort_by_key(|s| format!("{s:?}"));
    let opened = |events: &[rdy_engine::events::Event]| -> Vec<SideV1> {
        let ms: Vec<_> = events
            .iter()
            .filter_map(|e| match &e.kind {
                rdy_engine::events::EventKind::TensionOpened(m) => Some(m.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(ms.len(), 1, "{ms:#?}");
        let mut sides = ms[0].sides.clone();
        sides.sort_by_key(|s| format!("{s:?}"));
        sides
    };
    // live: the bus hands out both foreign sides
    let live_events: Vec<_> = rx.try_iter().map(|e| (*e).clone()).collect();
    assert_eq!(opened(&live_events), want);
    // replayed: the log gives the same
    let replayed = r.replay_events(from).unwrap();
    assert_eq!(opened(&replayed), want);
    // the JSON names each foreign side by repo and entry
    let e = replayed
        .iter()
        .find(|e| matches!(e.kind, rdy_engine::events::EventKind::TensionOpened(_)))
        .unwrap();
    let v = serde_json::to_value(e).unwrap();
    let sides = v["kind"]["sides"].as_array().unwrap().clone();
    assert_eq!(sides.len(), 2, "{v:#}");
    for (repo, entry) in [(cited(), tr.entry), (other, tb.entry)] {
        assert!(
            sides.contains(&json!({"repo": repo.to_string(), "entry": entry.to_string()})),
            "{v:#}"
        );
    }
}
