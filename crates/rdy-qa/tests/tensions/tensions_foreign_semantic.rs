//! rpl16 (repo links, D-Y49PX): reviewer pairs across the project boundary. A pair side is a
//! local block at its content or a cited block (repo and block id) at its delivered content
//! identity. A pair of local sides keeps its released key; a pair with a cited side hashes the
//! repo, so one block id in two repos is two pairs. The review of such a pair is a
//! `ReviewSubject::ForeignPair` (REVIEW encoding 1) and its tension a `SemanticTensionV1`
//! (TENSION encoding 1). The semantic kind never takes the citing pack's `tensions_across`
//! severity: only a person's accepted blocking advice raises it.
use rdy_engine::foreign_delivery::{DeliveryTaken, take_delivery};
use rdy_engine::semantic::candidates::{Rule, request_label};
use rdy_engine::semantic::case::case_hash;
use rdy_engine::semantic::submit::{CitationRequest, ReviewSubmitRequest};
use rdy_engine::tensions::*;
use rdy_engine::{Rdy, Session, WriteResult};
use rdy_model::foreign::{
    ForeignDerived, ForeignId, ForeignPublication, ForeignSet, ForeignSource, ForeignVersion,
    selection_identity,
};
use rdy_model::review::ReviewSubjectV1;
use rdy_model::semantic::{ForeignPairItem, PairSide, SemanticTensionV1, local_sides};
use rdy_model::{
    Block, BlockId, FieldValue, Hash, PairItem, RelayEvidence, RepoId, Role, pair_key,
    pair_key_sides,
};
use rdy_qa::fixtures::*;
use rdy_qa::tensions::{of_kind, one};
use rdy_store::codec;
use rdy_store::format::baseline_features;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_stream::{DeliveryRecord, StreamHeader, StreamRecord, StreamWriter};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;

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

fn side(repo: RepoId, id: &BlockId, content: Hash) -> PairSide {
    PairSide::Foreign(ForeignPairItem {
        target: ForeignId::new(repo, id.clone()),
        content,
    })
}

fn local(id: &BlockId, content: Hash) -> PairSide {
    PairSide::Local(PairItem {
        block: id.clone(),
        content,
    })
}

fn reviewer(actor: &str) -> Session {
    Session::agent(actor.into(), Some("r".into()), Role::Reviewer).unwrap()
}

/// The first rule of a world with a landed section that links `core`, one delivered claim, and
/// an in-flight claim (view `a`) that cites it.
struct World {
    _dir: tempfile::TempDir,
    _files: tempfile::TempDir,
    r: Rdy,
    link: BlockId,
    citer: BlockId,
    cited: Block,
}

fn world() -> World {
    let (dir, mut r) = project();
    let files = tempfile::tempdir().unwrap();
    let (section, links) = linked(&mut r, &[("core", cited())]);
    let link = links["core"].clone();
    let cited = block(
        "c-FRGNX",
        "claim",
        &[],
        "every request carries a trace id that the gateway sets",
    );
    deliver(
        &mut r,
        files.path(),
        "v1",
        &link,
        cited_repo(),
        None,
        &[(&cited, live())],
    );
    let w = put(
        &mut r,
        "a",
        json!([{"op": "create", "type": "claim", "id": "$x", "parent": section.to_string(),
                "body": "Requests carry a span id. See {{core:c-FRGNX}}."}]),
    );
    let citer = created(&w, "$x");
    World {
        _dir: dir,
        _files: files,
        r,
        link,
        citer,
        cited,
    }
}

fn cited_repo() -> RepoId {
    cited()
}

/// The one request that pairs a local block with a cited one.
fn foreign_request(r: &Rdy) -> rdy_engine::semantic::candidates::Request {
    let mut v: Vec<_> = r
        .read()
        .review_requests()
        .unwrap()
        .into_iter()
        .filter(|q| q.items.iter().any(|i| i.side.is_foreign()))
        .collect();
    assert_eq!(v.len(), 1, "{v:#?}");
    v.remove(0)
}

fn submit(
    r: &mut Rdy,
    q: &rdy_engine::semantic::candidates::Request,
    verdict: &str,
    severity: Option<&str>,
) -> rdy_engine::semantic::submit::SubmitInfo {
    let case = r.read().case_of(&q.key, &q.items).unwrap();
    let hash = case_hash(&case);
    r.submit_verdict(
        &reviewer("agent:r"),
        ReviewSubmitRequest {
            request: request_label(&q.key),
            case_hash: hash.to_string(),
            verdict: verdict.into(),
            citations: vec![CitationRequest {
                block: q.items[0].block.to_string(),
                note: "these two cannot both hold".into(),
            }],
            severity: severity.map(str::to_string),
            reason: severity.map(|_| "it contradicts the gateway rule".to_string()),
        },
    )
    .unwrap_or_else(|e| panic!("submit: {e:?}"))
}

#[test]
fn rpl16_pair_identity_distinguishes_repos() {
    let b = BlockId::parse("c-FRGNX").unwrap();
    let l = BlockId::parse("c-QCQAA").unwrap();
    let content = Hash([4; 32]);
    let other = Hash([5; 32]);
    let (r7, r8) = (RepoId([7; 16]), RepoId([8; 16]));
    let key = |sides: &[PairSide]| pair_key_sides(sides);
    // one block id and content in two repos are two pairs
    let in7 = key(&[local(&l, other), side(r7, &b, content)]);
    let in8 = key(&[local(&l, other), side(r8, &b, content)]);
    assert_ne!(in7, in8);
    // the same pair in any order is one pair
    assert_eq!(in7, key(&[side(r7, &b, content), local(&l, other)]));
    // a cited block is never the local block of that id
    assert_ne!(
        key(&[local(&l, other), side(r7, &b, content)]),
        key(&[local(&l, other), local(&b, content)])
    );
    // a content change is a new pair
    assert_ne!(in7, key(&[local(&l, other), side(r7, &b, other)]));
    // two cited sides distinguish their repos too
    let c = BlockId::parse("c-FRGNY").unwrap();
    assert_ne!(
        key(&[side(r7, &b, content), side(r7, &c, content)]),
        key(&[side(r7, &b, content), side(r8, &c, content)])
    );
    // every released local key stays: a pair of local sides is `pair_key`
    let items = vec![
        PairItem {
            block: l.clone(),
            content: other,
        },
        PairItem {
            block: b.clone(),
            content,
        },
    ];
    assert_eq!(key(&local_sides(&items)), pair_key(&items));
}

#[test]
fn rpl16_reviewer_can_pair_local_and_foreign() {
    let mut w = world();
    let q = foreign_request(&w.r);
    // the local item first (a live write), then the cited one at its delivered content
    assert_eq!(q.rule, Rule::Premise);
    assert!(matches!(q.items[0].side, SideRef::Change(_)), "{q:#?}");
    assert_eq!(q.items[0].block, w.citer);
    let SideRef::Foreign { repo, .. } = q.items[1].side else {
        panic!("{q:#?}");
    };
    assert_eq!((repo, &q.items[1].block), (cited(), &w.cited.id));
    assert_eq!(q.items[1].content, w.cited.content_hash());
    let sides: Vec<PairSide> = q.items.iter().map(|i| i.pair_side()).collect();
    assert_eq!(q.key, pair_key_sides(&sides));
    // the case reads the cited block from the foreign state: no doc, no local governing
    let case = w.r.read().case_of(&q.key, &q.items).unwrap();
    assert_eq!(case.rule, Some(Rule::Premise));
    assert!(case.blocks[1].side.starts_with("foreign:"), "{case:#?}");
    assert!(case.blocks[1].doc.is_empty() && case.blocks[1].governing.is_empty());
    assert_eq!(
        case.blocks[1].text[0].text,
        "every request carries a trace id that the gateway sets"
    );
    // a reviewer whose actor wrote the local block still judges it: the cited block has no
    // author here, and the local block was written by `agent:a`
    let info = submit(&mut w.r, &q, "tension", None);
    assert_eq!(info.severity, Some(TensionSeverity::Advisory));
    // the verdict is a review in REVIEW encoding 1: the local review type holds no cited
    // side, the wire type reads its pair of sides
    let h: Hash = info.review.parse().unwrap();
    assert!(w.r.read().reader().review(&h).is_err());
    let wire = w.r.read().reader().review_v1(&h).unwrap();
    assert_eq!(
        wire.subject,
        ReviewSubjectV1::Pair {
            key: q.key,
            items: sides.clone()
        }
    );
    assert_eq!(wire.citations.len(), 1);
    assert!(w.r.read().semantic().settled(&q.key).is_some());
    assert!(
        w.r.read()
            .review_requests()
            .unwrap()
            .iter()
            .all(|r| r.key != q.key),
        "a settled pair leaves the queue"
    );
    // the tension has the change side and the foreign side, advisory
    let t = one(&w.r, TensionKind::Semantic);
    assert_eq!(t.severity, TensionSeverity::Advisory);
    assert!(t.sides.iter().any(SideRef::is_foreign));
    assert!(t.sides.iter().any(|s| matches!(s, SideRef::Change(_))));
    assert_eq!(
        rdy_engine::tensions::semantic::pair_of(&t),
        Some(q.key),
        "the tension names its pair by the foreign key"
    );
    let rec: SemanticTensionV1 =
        w.r.read()
            .reader()
            .semantic_tension_v1(&w.r.read().semantic().tension(&q.key).unwrap().version)
            .unwrap();
    assert_eq!((rec.key, rec.items.clone()), (q.key, sides));
    // it survives a reopen and a reindex
    let dir = w._dir.path().to_path_buf();
    let (key, id) = (q.key, t.id);
    w.r.close().unwrap();
    let mut r = Rdy::open(&dir).unwrap();
    assert_eq!(one(&r, TensionKind::Semantic).id, id);
    r.reindex().unwrap();
    assert!(r.read().semantic().tension(&key).is_some());
    assert_eq!(one(&r, TensionKind::Semantic).id, id);
}

#[test]
fn rpl16_only_person_accepted_semantic_advice_blocks() {
    let mut w = world();
    let q = foreign_request(&w.r);
    // advice to block gates nothing: the tension is advisory, whatever the citing pack's
    // tensions_across says (it cannot name the semantic kind)
    let info = submit(&mut w.r, &q, "tension", Some("blocking"));
    assert_eq!(info.severity, Some(TensionSeverity::Advisory));
    assert_eq!(
        info.advice.as_deref(),
        Some("it contradicts the gateway rule")
    );
    assert_eq!(
        one(&w.r, TensionKind::Semantic).severity,
        TensionSeverity::Advisory
    );
    // a person's acceptance is the later version that blocks
    let open = w.r.read().semantic().tension(&q.key).unwrap().clone();
    let mut next = open.record.clone();
    next.prev = Some(open.version);
    next.blocking = true;
    next.relay = Some(RelayEvidence {
        quoted: "accept the advice".into(),
        session_ref: "chat".into(),
        question: None,
    });
    w.r.record_tension_versions(&[next]).unwrap();
    let t = one(&w.r, TensionKind::Semantic);
    assert_eq!(t.severity, TensionSeverity::Blocking);
    assert!(t.sides.iter().any(SideRef::is_foreign));
    // the accepted tension blocks only while both texts stand: a delivery that changes the
    // cited block's content resolves it
    let files = tempfile::tempdir().unwrap();
    let moved = block(
        "c-FRGNX",
        "claim",
        &[],
        "every request carries a span id that the edge sets",
    );
    deliver(
        &mut w.r,
        files.path(),
        "v2",
        &w.link,
        cited(),
        None,
        &[(&moved, live())],
    );
    assert!(
        of_kind(&w.r, TensionKind::Semantic).is_empty(),
        "{:#?}",
        of_kind(&w.r, TensionKind::Semantic)
    );
}

/// Two team sets deliver one instruction each (and an organization set one that is no team's).
struct Teams {
    _dir: tempfile::TempDir,
    files: tempfile::TempDir,
    r: Rdy,
    links: BTreeMap<String, BlockId>,
    red: Block,
    blue: Block,
}

fn teams() -> Teams {
    let (dir, mut r) = project();
    let files = tempfile::tempdir().unwrap();
    let other = RepoId([9; 16]);
    let (_s, links) = linked(&mut r, &[("red", cited()), ("blue", other), ("org", other)]);
    let reason = [("applies_to", "all")];
    let red = block(
        "ai-FRGNR",
        "instruction",
        &reason,
        "Always run the full test suite before you push a branch.",
    );
    let blue = block(
        "ai-FRGNB",
        "instruction",
        &reason,
        "Never run the test suite before a push; the pipeline runs it.",
    );
    let org = block(
        "ai-FRGNZ",
        "instruction",
        &reason,
        "Write every commit message in the imperative mood.",
    );
    deliver(
        &mut r,
        files.path(),
        "red",
        &links["red"],
        cited(),
        Some(ForeignSet::Team("red".into())),
        &[(&red, live())],
    );
    deliver(
        &mut r,
        files.path(),
        "blue",
        &links["blue"],
        other,
        Some(ForeignSet::Team("blue".into())),
        &[(&blue, live())],
    );
    deliver(
        &mut r,
        files.path(),
        "org",
        &links["org"],
        other,
        Some(ForeignSet::Organization),
        &[(&org, live())],
    );
    Teams {
        _dir: dir,
        files,
        r,
        links,
        red,
        blue,
    }
}

#[test]
fn rpl16_two_team_sets_pair_through_review() {
    let mut w = teams();
    // the queue offers the one pair of two cited instructions of two team sets; the
    // organization set's instruction is no team's and pairs with neither
    let all = w.r.read().review_requests().unwrap();
    assert_eq!(all.len(), 1, "{all:#?}");
    let q = all[0].clone();
    assert_eq!((q.rule, q.change), (Rule::Team, None));
    assert!(q.items.iter().all(|i| i.side.is_foreign()), "{q:#?}");
    let mut ids: Vec<&BlockId> = q.items.iter().map(|i| &i.block).collect();
    ids.sort();
    assert_eq!(ids, vec![&w.blue.id, &w.red.id]);
    let sides: Vec<PairSide> = q.items.iter().map(|i| i.pair_side()).collect();
    assert_eq!(q.key, pair_key_sides(&sides));
    // the case reads both cited blocks from the foreign state
    let case = w.r.read().case_of(&q.key, &q.items).unwrap();
    assert_eq!(case.rule, Some(Rule::Team));
    assert!(case.blocks.iter().all(|b| b.side.starts_with("foreign:")));
    assert!(
        case.blocks
            .iter()
            .all(|b| b.doc.is_empty() && b.governing.is_empty())
    );
    // a reviewer judges it and a tension opens, advisory, with two foreign sides
    let info = submit(&mut w.r, &q, "tension", Some("blocking"));
    assert_eq!(info.severity, Some(TensionSeverity::Advisory));
    assert!(info.tension.is_some(), "{info:?}");
    assert!(w.r.read().semantic().settled(&q.key).is_some());
    assert!(w.r.read().review_requests().unwrap().is_empty());
    let t = one(&w.r, TensionKind::Semantic);
    assert_eq!(t.severity, TensionSeverity::Advisory);
    assert!(t.sides.iter().all(SideRef::is_foreign) && t.sides.len() == 2);
    assert_eq!(rdy_engine::tensions::semantic::pair_of(&t), Some(q.key));
    assert!(
        w.r.read()
            .canon_tensions()
            .unwrap()
            .iter()
            .any(|c| c.id == t.id),
        "a pair of two cited sides stands in canon"
    );
    let h: Hash = info.review.parse().unwrap();
    let wire = w.r.read().reader().review_v1(&h).unwrap();
    assert_eq!(
        wire.subject,
        ReviewSubjectV1::Pair {
            key: q.key,
            items: sides.clone()
        }
    );
    // a person's accepted advice is the version that blocks
    let open = w.r.read().semantic().tension(&q.key).unwrap().clone();
    let mut next = open.record.clone();
    next.prev = Some(open.version);
    next.blocking = true;
    next.relay = Some(RelayEvidence {
        quoted: "accept the advice".into(),
        session_ref: "chat".into(),
        question: None,
    });
    w.r.record_tension_versions(&[next]).unwrap();
    let accepted = one(&w.r, TensionKind::Semantic);
    assert_eq!(accepted.severity, TensionSeverity::Blocking);
    // it survives a reopen
    let dir = w._dir.path().to_path_buf();
    let id = accepted.id;
    let (files, links, other) = (w.files, w.links, RepoId([9; 16]));
    w.r.close().unwrap();
    let mut r = Rdy::open(&dir).unwrap();
    assert_eq!(one(&r, TensionKind::Semantic).id, id);
    // a delivery that changes one text resolves it: the pair judged is gone
    let moved = block(
        "ai-FRGNB",
        "instruction",
        &[("applies_to", "all")],
        "Run the unit tests before a push and leave the rest to the pipeline.",
    );
    deliver(
        &mut r,
        files.path(),
        "blue2",
        &links["blue"],
        other,
        Some(ForeignSet::Team("blue".into())),
        &[(&moved, live())],
    );
    assert!(
        of_kind(&r, TensionKind::Semantic).is_empty(),
        "{:#?}",
        of_kind(&r, TensionKind::Semantic)
    );
}

#[test]
fn rpl16_settled_team_pairs_do_not_consume_the_candidate_limit() {
    // two team sets of five instructions each: 25 pairs, more than one read's limit of 20
    let (_dir, mut r) = project();
    let files = tempfile::tempdir().unwrap();
    let other = RepoId([9; 16]);
    let (_s, links) = linked(&mut r, &[("red", cited()), ("blue", other)]);
    let reason = [("applies_to", "all")];
    let five = |team: &str, letter: char| -> Vec<Block> {
        (1..=5)
            .map(|n| {
                block(
                    &format!("ai-FRG{letter}{n}"),
                    "instruction",
                    &reason,
                    &format!("Rule {n} of the {team} team: do step {n} before you push."),
                )
            })
            .collect()
    };
    let (red, blue) = (five("red", 'R'), five("blue", 'B'));
    let red_sel: Vec<(&Block, ForeignDerived)> = red.iter().map(|b| (b, live())).collect();
    let blue_sel: Vec<(&Block, ForeignDerived)> = blue.iter().map(|b| (b, live())).collect();
    deliver(
        &mut r,
        files.path(),
        "red",
        &links["red"],
        cited(),
        Some(ForeignSet::Team("red".into())),
        &red_sel,
    );
    deliver(
        &mut r,
        files.path(),
        "blue",
        &links["blue"],
        other,
        Some(ForeignSet::Team("blue".into())),
        &blue_sel,
    );
    let mut judged = std::collections::BTreeSet::new();
    let first = r.read().review_requests().unwrap();
    assert_eq!(first.len(), 20, "{first:#?}");
    for q in &first {
        assert_eq!(q.rule, Rule::Team);
        submit(&mut r, q, "no_tension", None);
        assert!(judged.insert(q.key));
    }
    // the settled pairs leave room: the next read offers the five pairs not yet judged
    let rest = r.read().review_requests().unwrap();
    assert_eq!(rest.len(), 5, "{rest:#?}");
    for q in &rest {
        assert!(!judged.contains(&q.key), "a settled pair came back: {q:#?}");
        submit(&mut r, q, "no_tension", None);
        assert!(judged.insert(q.key));
    }
    assert_eq!(judged.len(), 25);
    assert!(r.read().review_requests().unwrap().is_empty());
}
