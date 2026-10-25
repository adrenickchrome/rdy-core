//! ok2: a block's content hash leaves out its order key (D-TX3KH). The judgments bound to a
//! block's content (pins, tension dismissals, pair reviews and plan answers, D-H73DB) hold
//! through a move within its parent, and still lapse on a real edit. A store written before
//! this encoding keeps its bytes and objects; only its analysis changes.
mod common;

use rdy_engine::plan::{PlanAnswerInput, PlanQuery, W_PLAN_ANSWER_LAPSED};
use rdy_engine::semantic::candidates::request_label;
use rdy_engine::semantic::case::case_hash;
use rdy_engine::semantic::submit::ReviewSubmitRequest;
use rdy_engine::tensions::{Involved, TensionKind, TensionStatus, tension_label, tension_vkey};
use rdy_engine::{Location, Rdy, Scope, Session, WriteRequest, WriteResult, parse_ops};
use rdy_model::plan::{PlanAct, PlanKind, item_display, item_hash};
use rdy_model::{
    Block, BlockId, Change, FieldValue, Hash, Link, Op, PairItem, Provenance, ReviewSubject, Role,
    Tracking, pair_key,
};
use rdy_store::kinds;
use rdy_store::segment::seg_path;
use rdy_store::transition::Cause;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn agent(view: &str) -> Session {
    Session::agent(format!("agent:{view}"), Some(view.into()), Role::Writer).unwrap()
}

fn person() -> Session {
    Session::human("human:qa".into(), Role::Approver).unwrap()
}

fn reviewer() -> Session {
    Session::agent("agent:r".into(), Some("r".into()), Role::Reviewer).unwrap()
}

fn view(v: &str) -> Scope {
    Scope::View(v.into())
}

/// Writes `ops` to view `v` as `agent:<v>`.
fn put(r: &mut Rdy, v: &str, ops: Value) -> WriteResult {
    r.write(
        &agent(v),
        WriteRequest {
            view: v.into(),
            ops: parse_ops(ops.as_array().unwrap().clone()).unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap_or_else(|e| panic!("write to {v}: {e}"))
}

fn land(r: &mut Rdy, v: &str) {
    let p = r
        .propose(&agent(v), v, v, Some(common::TEST_SUMMARY))
        .unwrap_or_else(|e| panic!("propose {v}: {e}"));
    common::confirm_exceptions(r, &person(), &p);
    r.land(&person(), &p.id)
        .unwrap_or_else(|e| panic!("land {v}: {e}"));
}

/// Canon: section `Rules` with claims u ("U holds."), t, which cites and pins u, d, which cites
/// and pins t, and e, in that order. t is the target every case moves or edits.
struct Seed {
    sec: String,
    u: String,
    t: String,
    d: String,
    e: String,
}

fn seeded() -> (tempfile::TempDir, Rdy, Seed) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = put(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$doc", "fields": {"name": "spec", "title": "Spec", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$u", "parent": "$s", "body": "U holds."},
            {"op": "create", "type": "claim", "id": "$t", "parent": "$s", "after": "$u", "body": "T holds per {{$u}}."},
            {"op": "create", "type": "claim", "id": "$d", "parent": "$s", "after": "$t", "body": "D cites {{$t}}."},
            {"op": "create", "type": "claim", "id": "$e", "parent": "$s", "after": "$d", "body": "E stands."}
        ]),
    );
    land(&mut r, "seed");
    let get = |k: &str| w.created[k].clone();
    let s = Seed {
        sec: get("$s"),
        u: get("$u"),
        t: get("$t"),
        d: get("$d"),
        e: get("$e"),
    };
    (dir, r, s)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Act {
    /// A move of the target within its parent, after its last sibling.
    Move,
    /// A body edit of the target that keeps its reference and pin.
    Edit,
}

/// Moves or edits t in view `v`, and checks the act did what it says: the version changes in
/// both; the parent and the content hash stay for a move, and the order key for an edit.
fn act(r: &mut Rdy, v: &str, s: &Seed, a: Act) {
    let before = r.read().block(&view(v), &id(&s.t)).unwrap();
    let op = match a {
        Act::Move => json!([{"op": "move", "id": s.t, "parent": s.sec, "after": s.e}]),
        Act::Edit => json!([{"op": "update", "id": s.t,
            "body": format!("T holds for good per {{{{{}}}}}.", s.u)}]),
    };
    put(r, v, op);
    let st = r.read();
    let after = st.block(&view(v), &id(&s.t)).unwrap();
    let t = st.reader().tracking();
    assert_ne!(after.version_hash(), before.version_hash(), "{a:?}");
    assert_eq!(after.parent, before.parent, "{a:?}");
    assert_eq!(after.pins, before.pins, "{a:?}");
    match a {
        Act::Move => {
            assert_ne!(after.order, before.order, "the move re-keys t");
            assert_eq!(after.content_hash_with(t), before.content_hash_with(t));
            let m = st.materialized(&view(v)).unwrap();
            let kids: Vec<BlockId> = m
                .children(before.parent.as_ref())
                .into_iter()
                .map(|b| b.id.clone())
                .collect();
            assert_eq!(kids.last(), Some(&id(&s.t)), "t is now last");
        }
        Act::Edit => {
            assert_eq!(after.order, before.order);
            assert_ne!(after.content_hash_with(t), before.content_hash_with(t));
        }
    }
}

fn suspects(r: &Rdy, v: &str) -> Vec<(String, String)> {
    r.read()
        .suspects(&view(v))
        .unwrap()
        .into_iter()
        .map(|s| (s.block.to_string(), s.target.to_string()))
        .collect()
}

/// A pin: d pins t in canon; view m moves or edits t. Returns m's suspects after the act.
fn pin_case(a: Act) -> (Seed, Vec<(String, String)>) {
    let (_d, mut r, s) = seeded();
    assert_eq!(suspects(&r, "m"), vec![]);
    act(&mut r, "m", &s, a);
    let got = suspects(&r, "m");
    (s, got)
}

/// The edit collisions that involve `b`, each with whether it is dismissed.
fn collisions(r: &Rdy, b: &str) -> Vec<(Hash, bool)> {
    let st = r.read();
    let set = st.tensions().unwrap();
    set.involving_block(&id(b))
        .into_iter()
        .filter(|t| t.kind == TensionKind::EditCollision)
        .map(|t| {
            let dismissed = matches!(st.tension_status(&t), TensionStatus::Dismissed(_));
            (t.vkey, dismissed)
        })
        .collect()
}

/// A dismissal: views x and y both edit t, the person dismisses the collision, then x moves or
/// edits t. Returns the collision's vkey and the collisions that involve t after the act.
fn dismissal_case(a: Act) -> (Hash, Vec<(Hash, bool)>) {
    let (_d, mut r, s) = seeded();
    for v in ["x", "y"] {
        put(
            &mut r,
            v,
            json!([{"op": "update", "id": s.t,
                "body": format!("T holds per {{{{{}}}}}, as {v} sees it.", s.u)}]),
        );
    }
    let open = collisions(&r, &s.t);
    assert_eq!(open.len(), 1, "{open:?}");
    let vkey = open[0].0;
    let t = r.read().tensions().unwrap().involving_block(&id(&s.t));
    let t = t
        .iter()
        .find(|t| t.kind == TensionKind::EditCollision)
        .unwrap();
    r.dismiss_tension(&person(), &tension_label(&t.id), "both readings stand")
        .unwrap();
    assert_eq!(collisions(&r, &s.t), vec![(vkey, true)]);
    act(&mut r, "x", &s, a);
    (vkey, collisions(&r, &s.t))
}

/// The keys of the review requests whose two blocks are `x` and `y`.
fn requests_of(r: &Rdy, x: &str, y: &str) -> Vec<Hash> {
    let want: BTreeSet<BlockId> = [id(x), id(y)].into();
    r.read()
        .review_requests()
        .unwrap()
        .into_iter()
        .filter(|q| {
            q.items
                .iter()
                .map(|i| i.block.clone())
                .collect::<BTreeSet<_>>()
                == want
        })
        .map(|q| q.key)
        .collect()
}

/// A pair review: view p edits t; the reviewer judges t's pair with e (`no_tension`); then p
/// moves or edits t. Returns the reviewed key and the requests for the pair after the act.
fn pair_case(a: Act) -> (Hash, Vec<Hash>) {
    let (_d, mut r, s) = seeded();
    put(
        &mut r,
        "p",
        json!([{"op": "update", "id": s.t,
            "body": format!("T holds firmly per {{{{{}}}}}.", s.u)}]),
    );
    let keys = requests_of(&r, &s.t, &s.e);
    assert_eq!(keys.len(), 1, "one request for t and e: {keys:?}");
    let key = keys[0];
    let st = r.read();
    let q = st
        .review_requests()
        .unwrap()
        .into_iter()
        .find(|q| q.key == key)
        .unwrap();
    let case = st.case_of(&q.key, &q.items).unwrap();
    drop(st);
    r.submit_verdict(
        &reviewer(),
        ReviewSubmitRequest {
            request: request_label(&key),
            case_hash: case_hash(&case).to_string(),
            verdict: "no_tension".into(),
            citations: vec![],
            severity: None,
            reason: None,
        },
    )
    .unwrap();
    assert_eq!(
        requests_of(&r, &s.t, &s.e),
        vec![],
        "the verdict settles it"
    );
    act(&mut r, "p", &s, a);
    (key, requests_of(&r, &s.t, &s.e))
}

/// The state and answer of plan item `item` of view `v`, and the plan's notes.
fn plan_row(r: &Rdy, v: &str, item: &str) -> (Value, Option<Value>, Value) {
    let plan = r
        .read()
        .change_plan(
            &PlanQuery {
                view: Some(v.into()),
                max_tokens: Some(100_000),
                ..Default::default()
            },
            "work",
        )
        .unwrap();
    let plan = serde_json::to_value(plan).unwrap();
    let row = plan["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == item)
        .unwrap_or_else(|| panic!("no item {item}: {plan:#}"))
        .clone();
    (
        row["state"].clone(),
        row.get("answer").cloned(),
        plan["notes"].clone(),
    )
}

/// A plan answer: view w edits u, so t (which pins u) has a confirm item; w's agent confirms
/// it, then w moves or edits t, the item's subject. Returns the item and its row after the act.
fn plan_case(a: Act) -> (String, Seed, (Value, Option<Value>, Value)) {
    let (_d, mut r, s) = seeded();
    put(
        &mut r,
        "w",
        json!([{"op": "update", "id": s.u, "body": "U holds firmly."}]),
    );
    let item = item_display(&item_hash(&id(&s.u), &s.t, PlanKind::Confirm));
    assert_eq!(plan_row(&r, "w", &item).0, "open");
    r.answer_plan(
        &agent("w"),
        "w",
        &[PlanAnswerInput {
            item: item.clone(),
            act: PlanAct::Confirm,
            reason: Some("t still holds".into()),
        }],
    )
    .unwrap();
    let (state, answer, notes) = plan_row(&r, "w", &item);
    assert_eq!((state, notes), (json!("closed"), json!([])));
    assert!(answer.is_some());
    act(&mut r, "w", &s, a);
    let row = plan_row(&r, "w", &item);
    (item, s, row)
}

#[test]
fn ok2_move_keeps_pins_dismissals_pairs_and_plan_answers() {
    // a pin on the moved block stays current
    let (_s, got) = pin_case(Act::Move);
    assert_eq!(got, vec![], "a move makes no pin on t suspect");
    // the dismissal binds to a vkey the move keeps
    let (vkey, after) = dismissal_case(Act::Move);
    assert_eq!(after, vec![(vkey, true)], "the dismissal holds");
    // the pair keeps its key, so the verdict still settles it
    let (_key, after) = pair_case(Act::Move);
    assert_eq!(after, vec![], "no request returns");
    // the answer's subject keeps its content
    let (item, _s, (state, answer, notes)) = plan_case(Act::Move);
    assert_eq!(notes, json!([]), "no answer lapsed");
    assert_eq!(state, "closed", "{item}");
    assert_eq!(answer.unwrap()["act"], "confirm");
}

#[test]
fn ok2_real_edit_stales_the_same_judgments() {
    let (s, got) = pin_case(Act::Edit);
    assert_eq!(got, vec![(s.d.clone(), s.t.clone())], "d is suspect of t");
    let (vkey, after) = dismissal_case(Act::Edit);
    assert_eq!(after.len(), 1, "{after:?}");
    assert_ne!(after[0].0, vkey, "the new content has a new vkey");
    assert!(!after[0].1, "the dismissal lapsed: the collision is open");
    let (key, after) = pair_case(Act::Edit);
    assert_eq!(after.len(), 1, "the request returns: {after:?}");
    assert_ne!(after[0], key, "under a new pair key");
    let (item, s, (state, answer, notes)) = plan_case(Act::Edit);
    assert_eq!(state, "open", "{item}");
    assert_eq!(answer, None);
    assert_eq!(
        notes,
        json!([{"code": W_PLAN_ANSWER_LAPSED,
                "message": format!("the answer to {item} lapsed: {} changed", s.t),
                "fix": "answer the item again"}])
    );
}

// ---- a store written under content-1 ----

/// The retired content-1 hash of `b` (domain `rdy-content-1\0`, with the order key), as an rdy
/// before content-2 recorded it. Recomputed here only to show what old records hold; no
/// production path accepts it.
fn content_1(b: &Block, t: &Tracking) -> Hash {
    #[derive(serde::Serialize)]
    struct Content1<'a> {
        id: &'a BlockId,
        block_type: &'a String,
        parent: &'a Option<BlockId>,
        order: &'a String,
        fields: Vec<(&'a String, &'a FieldValue)>,
        body: &'a String,
        links: &'a BTreeMap<String, Vec<Link>>,
    }
    let tracked = t.fields(&b.block_type);
    let content = Content1 {
        id: &b.id,
        block_type: &b.block_type,
        parent: &b.parent,
        order: &b.order,
        fields: b
            .fields
            .iter()
            .filter(|(k, _)| !["status", "decided_at", "lifecycle"].contains(&k.as_str()))
            .filter(|(k, _)| !rdy_model::engine_fields::is_engine_field(k))
            .filter(|(k, _)| !tracked.is_some_and(|f| f.contains(k.as_str())))
            .collect(),
        body: &b.body,
        links: &b.links,
    };
    let mut bytes = b"rdy-content-1\0".to_vec();
    bytes.extend(postcard::to_allocvec(&content).unwrap());
    Hash::of(&bytes)
}

/// A released dump fixture (read only).
fn released(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../rdy-qa/fixtures/dump")
        .join(name)
}

/// The blake3 of each segment file under `project` up to `end`, as the expect files pin them.
fn segment_digests(project: &Path, end: Location) -> Value {
    let dir = project.join(".rdy/store/segments");
    let mut out = serde_json::Map::new();
    for seg in 0..=end.segment {
        let p = seg_path(&dir, seg);
        let bytes = std::fs::read(&p).unwrap();
        let bytes = if seg == end.segment {
            &bytes[..usize::try_from(end.offset).unwrap()]
        } else {
            &bytes[..]
        };
        out.insert(
            p.file_name().unwrap().to_string_lossy().into_owned(),
            json!(Hash::of(bytes).to_string()),
        );
    }
    Value::Object(out)
}

/// The edge kind of `block`'s pin on `target`: the link kind that names it, else `references`.
fn edge_kind(b: &Block, target: &BlockId) -> String {
    b.links
        .iter()
        .find(|(_, ls)| ls.iter().any(|l| &l.target == target))
        .map_or_else(|| "references".to_string(), |(k, _)| k.clone())
}

#[test]
fn ok2_old_dump_analysis_changes_without_object_changes() {
    // The (3, 18) standalone fixture, written by an rdy before content-2: its member bytes as
    // released.
    let file = released("v3-s18.rdydump");
    assert_eq!(
        Hash::of(&std::fs::read(&file).unwrap()).to_string(),
        "b69978453eaf0bc06b5e8040cd152836c1a290a913603cd53a5a2e5ece53c615"
    );
    let expect: Value =
        serde_json::from_slice(&std::fs::read(released("v3-s18.expect.json")).unwrap()).unwrap();
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("p");
    let summary = rdy_engine::load::load(&file, &root).unwrap();

    // the objects and bytes stay exact: the source log byte for byte, every canon version
    let end = Location {
        segment: u32::try_from(expect["whole"]["end"][0].as_u64().unwrap()).unwrap(),
        offset: expect["whole"]["end"][1].as_u64().unwrap(),
    };
    assert_eq!(summary.chain[0].end, end);
    assert_eq!(segment_digests(&root, end), expect["whole"]["segments"]);
    let mut r = Rdy::open(&root).unwrap();
    let st = r.read();
    let canon = st.materialized(&Scope::Canon).unwrap();
    let want = expect["whole"]["store"]["canon_blocks"]
        .as_object()
        .unwrap();
    assert_eq!(canon.len(), want.len());
    for (b, v) in want {
        let blk = canon.get(&id(b)).unwrap();
        assert_eq!(blk.version_hash().to_string(), v["version"], "{b}");
        assert_eq!(json!(blk.is_redacted()), v["redacted"], "{b}");
    }
    let t = Arc::clone(st.reader().tracking());

    // every canon pin is now suspect, for each old edge kind; those that were current under
    // content-1 are suspect only because the encoding changed
    let mut pins: Vec<(String, String, String)> = vec![];
    let mut current_1: Vec<(String, String)> = vec![];
    for b in canon.iter() {
        for (target, pin) in &b.pins {
            let Some(at) = canon.get(target) else {
                continue;
            };
            assert_ne!(*pin, at.content_hash_with(&t));
            if *pin == content_1(at, &t) {
                current_1.push((b.id.to_string(), target.to_string()));
            }
            pins.push((b.id.to_string(), target.to_string(), edge_kind(b, target)));
        }
    }
    pins.sort();
    current_1.sort();
    let own = |b: &str, t: &str, k: &str| (b.to_string(), t.to_string(), k.to_string());
    assert_eq!(
        pins,
        vec![
            own("D-YAHHY", "F-QWZ70", "depends_on"),
            own("F-QWZ70", "S-E4324", "references"),
            own("c-A3RNE", "D-YAHHY", "decided_by"),
            own("c-A3RNE", "S-E4324", "reflects"),
        ]
    );
    // the source's later change had already made the pins on it stale
    assert_eq!(
        current_1,
        vec![
            ("D-YAHHY".to_string(), "F-QWZ70".to_string()),
            ("c-A3RNE".to_string(), "D-YAHHY".to_string()),
        ]
    );
    let mut suspect: Vec<(String, String, String)> = st
        .suspects(&Scope::Canon)
        .unwrap()
        .into_iter()
        .map(|s| {
            let b = canon.get(&s.block).unwrap();
            (
                s.block.to_string(),
                s.target.to_string(),
                edge_kind(b, &s.target),
            )
        })
        .collect();
    suspect.sort();
    assert_eq!(suspect, pins, "every old pin is suspect");

    // the one dismissal was of the k2 collision's content-1 vkey; it no longer binds
    let dismissed: Vec<String> = st
        .reader()
        .refs_with_prefix("dismiss/")
        .unwrap()
        .into_iter()
        .map(|(n, _)| n["dismiss/".len()..].to_string())
        .collect();
    assert_eq!(dismissed.len(), 1);
    let set = st.tensions().unwrap();
    let mut lapsed = vec![];
    for x in set.iter() {
        assert_eq!(st.tension_status(x), TensionStatus::Open, "{x:?}");
        let old: Vec<Involved> = x
            .involved
            .iter()
            .map(|i| Involved {
                content: i
                    .version
                    .map(|v| content_1(&st.reader().get_block(&v).unwrap(), &t)),
                ..i.clone()
            })
            .collect();
        if tension_vkey(x.kind, x.severity, &old).to_string() == dismissed[0] {
            assert_ne!(x.vkey.to_string(), dismissed[0]);
            lapsed.push((x.kind, x.blocks()));
        }
    }
    assert_eq!(
        lapsed,
        vec![(TensionKind::EditCollision, vec![id("c-MN4R2")])],
        "the dismissed collision is still there, now open"
    );

    // the pair review of k3 and k4 judged content-1 items: the pair now has a new key, and its
    // request returns under it
    let pair_ref = "pair/6318a39727b58bede6db482798f2dccfb1d97a460f039cb2f93bf0597c0fc50a";
    let review = st.reader().get_ref(pair_ref).unwrap().unwrap();
    let review = st
        .reader()
        .review(&Hash(review.as_slice().try_into().unwrap()))
        .unwrap();
    let ReviewSubject::Pair { key, items } = review.subject else {
        panic!("a pair review");
    };
    let blocks: Vec<String> = items.iter().map(|i| i.block.to_string()).collect();
    assert_eq!(blocks, ["c-2MWV6", "c-F52HE"]);
    let back: Vec<_> = st
        .review_requests()
        .unwrap()
        .into_iter()
        .filter(|q| {
            q.items
                .iter()
                .map(|i| i.block.to_string())
                .eq(blocks.iter().cloned())
        })
        .collect();
    assert_eq!(back.len(), 1, "the request returns");
    assert_ne!(back[0].key, key);
    for (now, old) in back[0].items.iter().zip(&items) {
        let b = st.reader().get_block(&now.version).unwrap();
        assert_eq!(
            content_1(&b, &t),
            old.content,
            "the review judged this version"
        );
        assert_eq!(now.content, b.content_hash_with(&t));
    }
    let now: Vec<PairItem> = back[0]
        .items
        .iter()
        .map(|i| PairItem {
            block: i.block.clone(),
            content: i.content,
        })
        .collect();
    assert_eq!(back[0].key, pair_key(&now));
    assert!(st.semantic().settled(&back[0].key).is_none());
    drop(st);

    // the first writable start logs one reopened batch; a second start logs nothing
    assert_eq!(r.fsck().unwrap().unlogged_moves, 5);
    let at = r.store().log_end();
    assert_eq!(r.reconcile_transitions().unwrap(), 5);
    let batch = r.store().last_batch().to_vec();
    assert_eq!(batch.len(), 1, "one record in one batch");
    assert_eq!(batch[0].0, kinds::TRANSITION);
    assert!(r.store().log_end() > at);
    let rec = r.read().reader().transitions(&batch[0].1).unwrap();
    assert!(rec.gone.is_empty(), "{rec:?}");
    let moved = |ms: &[rdy_store::transition::TensionMove]| -> Vec<(String, String, Cause)> {
        ms.iter()
            .map(|m| (m.kind.clone(), m.severity.clone(), m.why.cause))
            .collect()
    };
    // the lapsed dismissal opens its collision again
    assert_eq!(
        moved(&rec.opened),
        [("edit_collision".into(), "blocking".into(), Cause::Reopened)]
    );
    // the semantic tensions the pair reviews opened bind to the old pair keys: they resolve
    assert_eq!(
        moved(&rec.resolved),
        [
            ("semantic".into(), "advisory".into(), Cause::Reopened),
            ("semantic".into(), "blocking".into(), Cause::Reopened),
        ]
    );
    let mut new: Vec<(String, String, Cause)> = rec
        .new
        .iter()
        .map(|m| (m.block.to_string(), m.target.to_string(), m.why.cause))
        .collect();
    new.sort();
    let want: Vec<(String, String, Cause)> = current_1
        .iter()
        .map(|(b, t)| (b.clone(), t.clone(), Cause::Reopened))
        .collect();
    assert_eq!(
        new, want,
        "each pin current under content-1 turns suspect, as reopened"
    );
    r.close().unwrap();
    let mut r = Rdy::open(&root).unwrap();
    let after = r.store().log_end();
    assert_eq!(r.reconcile_transitions().unwrap(), 0);
    assert_eq!(r.store().log_end(), after);
    assert_eq!(r.fsck().unwrap().unlogged_moves, 0);
    r.close().unwrap();

    // A chain whose removals were applied before content-2 still loads: a block residue keeps
    // the content-1 hash it recorded, and its proof check does not derive it again.
    let chain: Vec<PathBuf> = ["v3-s18-chain-full", "v3-s18-chain-1", "v3-s18-chain-2"]
        .iter()
        .map(|n| released(&format!("{n}.rdydump")))
        .collect();
    let to = d.path().join("chain");
    let loaded = rdy_engine::load::load_chain(&chain, &to, None)
        .unwrap_or_else(|e| panic!("the released chain loads: {e}"));
    assert!(
        loaded.fsck.redaction.is_some(),
        "the chain holds applied removals"
    );
    let r = Rdy::open(&to).unwrap();
    let removed: Vec<Block> = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .iter()
        .filter(|b| b.is_redacted())
        .cloned()
        .collect();
    assert!(!removed.is_empty());
    for b in &removed {
        assert_eq!(b.content_hash_with(&t), b.content_hash(), "{}", b.id);
    }
    r.close().unwrap();

    // The released fixtures hold no live plan answer and no open-view pin with a real content-1
    // hash, so these two are the records an rdy before content-2 wrote, with the hashes it
    // recorded: an open-view premise shift and a lapsed answer.
    let (_d2, mut r, s) = seeded();
    let tr = Arc::clone(r.read().reader().tracking());
    // view o: d's edit, pinning t at t's content-1 hash
    put(
        &mut r,
        "o",
        json!([{"op": "update", "id": s.d, "body": format!("D still cites {{{{{}}}}}.", s.t)}]),
    );
    let t_canon = r.read().block(&Scope::Canon, &id(&s.t)).unwrap();
    assert_eq!(
        t_canon.order,
        r.read().block(&view("o"), &id(&s.t)).unwrap().order
    );
    let mut next = r.read().block(&view("o"), &id(&s.d)).unwrap();
    let base = next.version_hash();
    next.body = format!("D cites {{{{{}}}}} as before.", s.t);
    next.pins.insert(id(&s.t), content_1(&t_canon, &tr));
    commit(&mut r, "o", base, next);
    assert!(
        suspects(&r, "o").contains(&(s.d.clone(), s.t.clone())),
        "the old pin is stale"
    );
    let shifts: Vec<Vec<BlockId>> = r
        .read()
        .tensions()
        .unwrap()
        .iter()
        .filter(|x| x.kind == TensionKind::PremiseShift)
        .map(|x| x.blocks())
        .collect();
    let mut pair = vec![id(&s.d), id(&s.t)];
    pair.sort();
    assert_eq!(shifts, vec![pair], "an open-view premise shift");
    // view w: a confirm answer on t, recorded with content-1 subject and cause hashes
    put(
        &mut r,
        "w",
        json!([{"op": "update", "id": s.u, "body": "U holds firmly."}]),
    );
    let item = item_display(&item_hash(&id(&s.u), &s.t, PlanKind::Confirm));
    r.answer_plan(
        &agent("w"),
        "w",
        &[PlanAnswerInput {
            item: item.clone(),
            act: PlanAct::Confirm,
            reason: Some("t still holds".into()),
        }],
    )
    .unwrap();
    assert_eq!(plan_row(&r, "w", &item).0, "closed");
    let st = r.read();
    let cur_id = st
        .reader()
        .get_ref(&rdy_store::plan::plan_ref("w"))
        .unwrap()
        .map(|v| Hash(v.as_slice().try_into().unwrap()))
        .unwrap();
    let cur = st.reader().plan(&cur_id).unwrap();
    let at = |b: &str| content_1(&st.block(&view("w"), &id(b)).unwrap(), &tr);
    let mut answers = cur.answers.clone();
    assert_eq!(answers.len(), 1);
    assert_eq!(
        answers[0].subject,
        Some(
            st.block(&view("w"), &id(&s.t))
                .unwrap()
                .content_hash_with(&tr)
        )
    );
    answers[0].subject = Some(at(&s.t));
    answers[0].cause = at(&s.u);
    let old = rdy_model::plan::Plan {
        stable: Some(cur.stable.unwrap_or(cur_id)),
        prev: Some(cur_id),
        proposal: None,
        answers,
        ..cur
    };
    drop(st);
    r.store_mut().put_plan(&old).unwrap();
    r.invalidate();
    let (state, answer, notes) = plan_row(&r, "w", &item);
    assert_eq!((state, answer), (json!("open"), None));
    assert_eq!(
        notes,
        json!([{"code": W_PLAN_ANSWER_LAPSED,
                "message": format!("the answer to {item} lapsed: {} changed", s.t),
                "fix": "answer the item again"}])
    );
}

/// A typed store commit of `next` (from version `base`) to view `v`: every change `v` includes
/// is a dep. It stands in for a write an rdy before content-2 made.
fn commit(r: &mut Rdy, v: &str, base: Hash, next: Block) {
    let deps = r.read().included(v).to_vec();
    r.store_mut()
        .commit_to_view(
            v,
            Change {
                view: v.into(),
                ops: vec![Op::Update { base, block: next }],
                deps,
                provenance: Provenance {
                    actor: format!("agent:{v}"),
                    run: Some(v.into()),
                    role: Role::Writer,
                    at_ms: 5,
                },
                justification: None,
                message: String::new(),
                links: Default::default(),
                read_identity: None,
            },
        )
        .unwrap();
    r.invalidate();
}
