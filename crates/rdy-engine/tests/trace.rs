//! rf11 (D-1876T, D-VPATZ, D-X4RYX; TASK-507 (1), (2a)-(2c), (4)): a trace explains a block from
//! its recorded decisions and landings. It walks lineage both ways for at most `depth` steps,
//! adds one-step premises and answered questions, keeps sources apart from governing decisions,
//! finds the first landing that created the block and the oldest landing that accepted each
//! decision in the scope's own landing chain, counts only that chain's seals, cuts whole decision
//! rows to its token budget, bounds every count, and writes nothing.
mod common;
#[path = "support/plan_frames.rs"]
mod plan_frames;

use plan_frames::put_plan_frames;
use rdy_engine::redaction::RedactInput;
use rdy_engine::trace::TraceOptions;
use rdy_engine::*;
use rdy_model::engine_fields::{Against, Reaffirmation, encode_reaffirmed};
use rdy_model::plan::{Plan, PlanKind, PlanOutcome, PlanSeal, SealedItem, item_hash};
use rdy_model::*;
use rdy_store::kinds;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

// ---- sessions, writes and landings ----

const WRITER: &str = "agent:designer";
const FOUNDER: &str = "human:founder";

fn writer() -> Session {
    Session {
        actor: WRITER.into(),
        run: Some("run-1".into()),
        role: Role::Writer,
        kind: SessionKind::Human,
    }
}

fn founder() -> Session {
    Session {
        actor: FOUNDER.into(),
        run: None,
        role: Role::Approver,
        kind: SessionKind::Human,
    }
}

fn bid(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    (dir, r)
}

fn put(r: &mut Rdy, view: &str, ops: Value) -> BTreeMap<String, String> {
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: parse_ops(ops.as_array().unwrap().clone()).unwrap(),
            justification: None,
            message: format!("write to {view}"),
        },
    )
    .unwrap_or_else(|e| panic!("write to {view}: {e}"))
    .created
    .into_iter()
    .collect()
}

/// Proposes `view` with a summary, confirms its exceptions and lands it as the founder: the
/// version's `P-x@n` and the landing.
fn land(r: &mut Rdy, view: &str, title: &str) -> (String, Hash) {
    let p = r
        .propose(&writer(), view, title, Some(common::TEST_SUMMARY))
        .unwrap_or_else(|e| panic!("propose {view}: {e}"));
    common::confirm_exceptions(r, &founder(), &p);
    let l = r
        .land(&founder(), &p.id)
        .unwrap_or_else(|e| panic!("land {view}: {e}"));
    (p.r#ref, l.parse().unwrap())
}

/// A version of `view` through the raw store writer (no plan, no acceptance).
fn raw_propose(r: &mut Rdy, view: &str) -> Hash {
    let v = r.store_mut().propose(view, view, prov()).unwrap();
    r.invalidate();
    v
}

/// Lands `version` through the raw store writer: no seal and no acceptance change.
fn raw_land(r: &mut Rdy, version: &Hash) -> Hash {
    let l = r.store_mut().land(version, founder().provenance()).unwrap();
    r.invalidate();
    l
}

fn prov() -> Provenance {
    Provenance {
        actor: "agent:fixture".into(),
        run: Some("fixture".into()),
        role: Role::Writer,
        at_ms: 1,
    }
}

/// Commits `ops` to `view` as one typed change, with no engine check.
fn commit(r: &mut Rdy, view: &str, ops: Vec<Op>) {
    let deps = r.read().included(view).to_vec();
    r.store_mut()
        .commit_to_view(
            view,
            Change {
                view: view.into(),
                ops,
                deps,
                provenance: prov(),
                justification: None,
                message: String::new(),
                links: Default::default(),
                read_identity: None,
            },
        )
        .unwrap();
    r.invalidate();
}

/// The `n`th id of `prefix`, in Crockford digits.
fn nid(prefix: &str, mut n: usize) -> BlockId {
    const C: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut body = [b'0'; 5];
    for i in (0..5).rev() {
        body[i] = C[n % 32];
        n /= 32;
    }
    bid(&format!("{prefix}-{}", std::str::from_utf8(&body).unwrap()))
}

fn block(id: &BlockId, ty: &str, parent: &BlockId, fields: Value, body: &str) -> Block {
    let fields = match fields {
        Value::Object(m) => m
            .into_iter()
            .map(|(k, v)| (k, json_to_field(&v).unwrap()))
            .collect(),
        _ => BTreeMap::new(),
    };
    Block {
        id: id.clone(),
        block_type: ty.into(),
        parent: Some(parent.clone()),
        order: format!("a{}x", id.as_str().split_once('-').unwrap().1),
        fields,
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn link(target: &BlockId, part: Option<&str>) -> Link {
    Link {
        target: target.clone(),
        part: part.map(str::to_string),
    }
}

// ---- reads ----

fn opts(depth: u8, detail: bool, max_tokens: usize) -> TraceOptions {
    TraceOptions {
        depth,
        detail,
        max_tokens,
    }
}

/// The whole trace: a budget no fixture reaches.
fn wide(depth: u8) -> TraceOptions {
    opts(depth, false, 100_000)
}

fn trace_in(st: &ReadState, scope: &Scope, id: &str, o: TraceOptions) -> Value {
    let reply = st
        .trace(scope, &bid(id), o)
        .unwrap_or_else(|e| panic!("trace {id}: {e}"));
    serde_json::to_value(&reply).unwrap()
}

fn trace(r: &Rdy, scope: &Scope, id: &str, o: TraceOptions) -> Value {
    trace_in(&r.read(), scope, id, o)
}

fn refused(e: RdyError, code: &str, message: &str, fix: &str) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (code, message, fix),
        "{e}"
    );
}

/// (depth, id) of each row of `list`.
fn rows(v: &Value, list: &str) -> Vec<(u64, String)> {
    v[list]
        .as_array()
        .unwrap_or_else(|| panic!("{list} is a list: {v}"))
        .iter()
        .map(|r| {
            (
                r["depth"].as_u64().unwrap(),
                r["id"].as_str().unwrap().into(),
            )
        })
        .collect()
}

fn ids(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect()
}

fn sorted(mut v: Vec<(u64, String)>) -> Vec<(u64, String)> {
    v.sort();
    v
}

fn decision_row<'v>(v: &'v Value, id: &str) -> &'v Value {
    v["decisions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == id)
        .unwrap_or_else(|| panic!("no decision {id} in {v}"))
}

fn dec(id: &str, statement: &str, links: Value) -> Value {
    json!({"op": "create", "type": "decision", "id": id, "parent": "$s",
           "fields": {"statement": statement, "status": "proposed"}, "links": links})
}

// ---- the lineage fixture ----

/// Canon: claim K is decided by B. B refines A, depends on finding F and on decision E, and
/// answers Q; C refines B and A (a cycle) and is conditional on prose P; E depends on claim Z.
/// A second landing adds D, which supersedes C, G, which refines D, and H, which refines G.
struct Lineage {
    _dir: tempfile::TempDir,
    r: Rdy,
    id: BTreeMap<&'static str, String>,
}

fn lineage() -> Lineage {
    let (dir, mut r) = project();
    let w = put(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "refunds", "title": "Refunds", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "finding", "id": "$f", "parent": "$s",
                "fields": {"statement": "Refunds over 100 were disputed.", "evidence": [{"quote": "Two disputes last month."}]}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": "Only when the ledger is open."},
            {"op": "create", "type": "claim", "id": "$z", "parent": "$s", "body": "Ledgers close nightly."},
            {"op": "create", "type": "question", "id": "$q", "parent": "$s", "fields": {"prompt": "Who signs large refunds?"}},
            dec("$a", "Approvers sign refunds", json!({})),
            dec("$e", "The ledger is the record", json!({"depends_on": [{"target": "$z"}]})),
            dec("$b", "Approvers sign refunds over 100", json!({
                "refines": [{"target": "$a"}],
                "depends_on": [{"target": "$f"}, {"target": "$e"}],
                "answers": [{"target": "$q"}]})),
            dec("$c", "Two approvers sign refunds over 500", json!({
                "refines": [{"target": "$b"}, {"target": "$a"}],
                "conditional_on": [{"target": "$p"}]})),
            {"op": "create", "type": "claim", "id": "$k", "parent": "$s", "body": "A refund of 200 needs an approver.",
                "links": {"decided_by": [{"target": "$b"}]}}
        ]),
    );
    land(&mut r, "work", "Refund rules");
    let g = put(
        &mut r,
        "more",
        json!([
            {"op": "create", "type": "decision", "id": "$dd", "parent": w["$s"], "fields": {"statement": "Two approvers sign refunds over 400", "status": "proposed"},
                "links": {"supersedes": [{"target": w["$c"]}]}},
            {"op": "create", "type": "decision", "id": "$g", "parent": w["$s"], "fields": {"statement": "Refunds over 400 need a reason", "status": "proposed"},
                "links": {"refines": [{"target": "$dd"}]}},
            {"op": "create", "type": "decision", "id": "$h", "parent": w["$s"], "fields": {"statement": "The reason names the order", "status": "proposed"},
                "links": {"refines": [{"target": "$g"}]}}
        ]),
    );
    land(&mut r, "more", "Refund limits");
    let mut id = BTreeMap::new();
    for (k, v) in [
        ("A", &w["$a"]),
        ("B", &w["$b"]),
        ("C", &w["$c"]),
        ("E", &w["$e"]),
        ("F", &w["$f"]),
        ("P", &w["$p"]),
        ("Q", &w["$q"]),
        ("Z", &w["$z"]),
        ("K", &w["$k"]),
        ("D", &g["$dd"]),
        ("G", &g["$g"]),
        ("H", &g["$h"]),
    ] {
        id.insert(k, v.clone());
    }
    Lineage { _dir: dir, r, id }
}

impl Lineage {
    fn at(&self, pairs: &[(u64, &str)]) -> Vec<(u64, String)> {
        sorted(
            pairs
                .iter()
                .map(|(d, k)| (*d, self.id[k].clone()))
                .collect(),
        )
    }
}

/// View `o`: R refines A, A refines B, B refines C; R also depends on B (a shorter route to B)
/// and on prose P, and answers question Q. X, apart, depends on P and answers Q.
fn overlap() -> (tempfile::TempDir, Rdy, BTreeMap<String, String>) {
    let (dir, mut r) = project();
    let w = put(
        &mut r,
        "o",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "o", "title": "O", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": "Only on weekdays."},
            {"op": "create", "type": "question", "id": "$q", "parent": "$s", "fields": {"prompt": "Who signs?"}},
            dec("$c", "Refunds are signed", json!({})),
            dec("$b", "Approvers sign refunds", json!({"refines": [{"target": "$c"}]})),
            dec("$a", "Approvers sign refunds over 100", json!({"refines": [{"target": "$b"}]})),
            dec("$r", "Two approvers sign refunds over 500", json!({
                "refines": [{"target": "$a"}],
                "depends_on": [{"target": "$b"}, {"target": "$p"}],
                "answers": [{"target": "$q"}]})),
            dec("$x", "Refunds are logged", json!({
                "depends_on": [{"target": "$p"}],
                "answers": [{"target": "$q"}]}))
        ]),
    );
    (dir, r, w)
}

/// (least endpoint depth, from, to, kind) of each edge of `v`, as the reply orders them.
fn edge_rows(v: &Value) -> Vec<(u64, String, String, String)> {
    let depth: BTreeMap<String, u64> = rows(v, "decisions")
        .into_iter()
        .chain(rows(v, "premises"))
        .chain(rows(v, "questions"))
        .map(|(d, id)| (id, d))
        .collect();
    v["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            let (f, t) = (e["from"].as_str().unwrap(), e["to"].as_str().unwrap());
            (
                depth[f].min(depth[t]),
                f.to_string(),
                t.to_string(),
                e["kind"]["Link"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
fn rf11_trace_walks_lineage_premises_and_answers_once() {
    let l = lineage();
    let v = trace(&l.r, &Scope::Canon, &l.id["K"], wide(3));
    let i = |k: &str| l.id[k].clone();
    assert_eq!(v["id"], i("K"));
    assert_eq!(v["block_type"], "claim");
    assert_eq!(v["summary"], "A refund of 200 needs an approver.");
    assert_eq!(
        v["governing"],
        json!([{"decision": i("B"), "status": "live", "via": "decided_by"}])
    );
    // lineage both ways (A outgoing, C incoming), the cycle C-A once, D through C's supersede, G
    // at the default depth 3; E is a decision premise at owner depth + 1
    assert_eq!(
        rows(&v, "decisions"),
        l.at(&[(0, "B"), (1, "A"), (1, "C"), (1, "E"), (2, "D"), (3, "G")])
    );
    let status: BTreeMap<String, String> = v["decisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["id"].as_str().unwrap().into(),
                d["status"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(status[&i("C")], "superseded");
    assert_eq!(status[&i("B")], "live");
    assert_eq!(
        decision_row(&v, &i("B"))["statement"],
        "Approvers sign refunds over 100"
    );
    // the non-decision premises, with the finding's summary; E's own premise Z is not expanded
    assert_eq!(rows(&v, "premises"), l.at(&[(1, "F"), (2, "P")]));
    let f = v["premises"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == i("F"))
        .unwrap();
    assert_eq!(
        f,
        &json!({"id": i("F"), "block_type": "finding", "summary": "Refunds over 100 were disputed.", "depth": 1})
    );
    assert_eq!(
        v["questions"],
        json!([{"id": i("Q"), "prompt": "Who signs large refunds?", "depth": 1}])
    );
    // E is unexpanded and G is at the depth boundary with H beyond it
    let frontier: Vec<String> = l
        .at(&[(1, "E"), (3, "G")])
        .into_iter()
        .map(|p| p.1)
        .collect();
    assert_eq!(ids(&v["frontier"]), frontier);
    assert!(
        v.get("truncated").is_none() && v.get("omitted").is_none(),
        "{v}"
    );
    // every edge once, sorted by (least endpoint depth, from, to, kind, part)
    let depth: BTreeMap<String, u64> = rows(&v, "decisions")
        .into_iter()
        .chain(rows(&v, "premises"))
        .chain(rows(&v, "questions"))
        .map(|(d, id)| (id, d))
        .collect();
    let e = |from: &str, to: &str, kind: &str| {
        (
            depth[&i(from)].min(depth[&i(to)]),
            i(from),
            i(to),
            kind.to_string(),
        )
    };
    let mut want = vec![
        e("B", "A", "refines"),
        e("C", "B", "refines"),
        e("C", "A", "refines"),
        e("D", "C", "supersedes"),
        e("G", "D", "refines"),
        e("B", "F", "depends_on"),
        e("B", "E", "depends_on"),
        e("C", "P", "conditional_on"),
        e("B", "Q", "answers"),
    ];
    want.sort();
    let got: Vec<Value> = want
        .iter()
        .map(|(_, f, t, k)| json!({"from": f, "to": t, "kind": {"Link": k}, "part": null}))
        .collect();
    assert_eq!(v["edges"], json!(got));
    // nothing appears twice, and nothing past the walk appears at all
    let text = v.to_string();
    for k in ["H", "Z"] {
        assert!(!text.contains(&i(k)), "{k} is outside the trace: {v}");
    }
    let shown: BTreeSet<String> = rows(&v, "decisions").into_iter().map(|p| p.1).collect();
    assert_eq!(shown.len(), v["decisions"].as_array().unwrap().len());
    // a decision traced directly starts at itself, with no governing rows
    let a = trace(&l.r, &Scope::Canon, &i("A"), wide(1));
    assert_eq!(a["governing"], json!([]));
    assert_eq!(
        rows(&a, "decisions"),
        l.at(&[(0, "A"), (1, "B"), (1, "C"), (2, "E")])
    );

    // overlapping routes: lineage reaches B at 2 (R, A, B), but R's premise reaches it at 1, and
    // B's lineage neighbour C follows at 2, not 3; a shorter route changes no lineage expansion
    let (_dir, r, o) = overlap();
    let i = |k: &str| o[k].clone();
    let v = trace(&r, &Scope::View("o".into()), &i("$r"), wide(3));
    let want = sorted(vec![(0, i("$r")), (1, i("$a")), (1, i("$b")), (2, i("$c"))]);
    assert_eq!(rows(&v, "decisions"), want);
    assert_eq!(rows(&v, "premises"), vec![(1, i("$p"))]);
    assert_eq!(rows(&v, "questions"), vec![(1, i("$q"))]);
    // B was reached by lineage, so it is no unexpanded premise; C has no unvisited neighbour
    assert_eq!(v["frontier"], json!([]));
    let mut want = vec![
        (0, i("$r"), i("$a"), "refines".to_string()),
        (0, i("$r"), i("$b"), "depends_on".to_string()),
        (0, i("$r"), i("$p"), "depends_on".to_string()),
        (0, i("$r"), i("$q"), "answers".to_string()),
        (1, i("$a"), i("$b"), "refines".to_string()),
        (1, i("$b"), i("$c"), "refines".to_string()),
    ];
    want.sort();
    assert_eq!(edge_rows(&v), want);
}

#[test]
fn rf11_trace_uses_governing_routes_and_keeps_sources_separate() {
    let (_dir, mut r) = project();
    let w = put(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "rules", "title": "Rules", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Stable"}},
            {"op": "create", "type": "section", "id": "$draft", "parent": "$d", "fields": {"title": "Draft", "lifecycle": "draft"}},
            {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "The record of payments."},
            {"op": "create", "type": "source", "id": "$src", "parent": "$s",
                "fields": {"title": "Release 2", "kind": "release", "locator": "v2"}},
            {"op": "create", "type": "finding", "id": "$f", "parent": "$s",
                "fields": {"statement": "Payments drift.", "evidence": [{"quote": "It drifted."}]}},
            dec("$x", "The ledger is append-only", json!({"affects": [{"target": "$t"}]})),
            dec("$w", "Refunds are written to the ledger", json!({})),
            {"op": "create", "type": "claim", "id": "$kd", "parent": "$s", "body": "Entries are never edited.",
                "links": {"describes": [{"target": "$t"}]}},
            dec("$y", "Every refund is recorded", json!({})),
            {"op": "create", "type": "claim", "id": "$ky", "parent": "$s", "body": "Each refund has an entry.",
                "links": {"implements": [{"target": "$y"}], "decided_by": [{"target": "$w"}]}},
            {"op": "create", "type": "claim", "id": "$kg", "parent": "$s", "body": "The importer reads the ledger.",
                "fields": {"reflects": ["src/**"]}},
            {"op": "create", "type": "claim", "id": "$kn", "parent": "$s", "body": "Nothing grounds this."},
            {"op": "create", "type": "claim", "id": "$kdr", "parent": "$draft", "body": "Draft words."}
        ]),
    );
    // Y also affects K_y: one row, by the better route (implements)
    put(
        &mut r,
        "work",
        json!([{"op": "update", "id": w["$y"], "links": {"affects": [{"target": w["$ky"]}]}}]),
    );
    let ks = r
        .write_reflecting(
            &writer(),
            WriteRequest {
                view: "work".into(),
                ops: parse_ops(vec![json!({"op": "create", "type": "claim", "id": "$ks", "parent": w["$s"], "body": "Release 2 ships the ledger."})]).unwrap(),
                justification: None,
                message: String::new(),
            },
            ReflectsInput {
                sources: vec![w["$src"].clone()],
                code: false,
            },
            ChangeLinks::default(),
            None,
        )
        .unwrap()
        .created["$ks"]
        .clone();
    // the section turns stable (a typed write: a stable write needs a briefing)
    let sec = bid(&w["$s"]);
    let mut stable = r.read().block(&Scope::View("work".into()), &sec).unwrap();
    let base = stable.version_hash();
    stable
        .fields
        .insert("lifecycle".into(), FieldValue::Str("stable".into()));
    commit(
        &mut r,
        "work",
        vec![Op::Update {
            base,
            block: stable,
        }],
    );
    land(&mut r, "work", "Ledger rules");
    // a proposed decision and a stable claim it decides, in an open view
    let (pp, kp) = (nid("D", 900), nid("c", 900));
    let mut claim = block(&kp, "claim", &sec, json!({}), "Each entry has a reason.");
    claim
        .links
        .insert("decided_by".into(), vec![link(&pp, None)]);
    commit(
        &mut r,
        "next",
        vec![
            Op::Create {
                block: block(
                    &pp,
                    "decision",
                    &sec,
                    json!({"statement": "Entries carry a reason", "status": "proposed"}),
                    "",
                ),
            },
            Op::Create { block: claim },
        ],
    );
    let p: BTreeMap<&str, String> =
        BTreeMap::from([("$pp", pp.to_string()), ("$kp", kp.to_string())]);
    let next = Scope::View("next".into());
    let at = |k: &str| w[k].clone();

    // a proposed decision is shown but does not ground the block
    let v = trace(&r, &next, &p["$kp"], wide(3));
    assert_eq!(
        v["governing"],
        json!([{"decision": p["$pp"], "status": "proposed", "via": "decided_by"}])
    );
    assert_eq!(rows(&v, "decisions"), vec![(0, p["$pp"].clone())]);
    assert_eq!(v["ungrounded"], true);
    assert_eq!(v["sources"], json!([]));
    // an entity route names its entity
    let v = trace(&r, &Scope::Canon, &at("$kd"), wide(3));
    assert_eq!(
        v["governing"],
        json!([{"decision": at("$x"), "status": "live", "via": "describes/affects", "entity": at("$t")}])
    );
    assert_eq!(v["ungrounded"], false);
    // one row per decision, by the best route, in id order
    let v = trace(&r, &Scope::Canon, &at("$ky"), wide(3));
    let mut want = vec![
        json!({"decision": at("$y"), "status": "live", "via": "implements"}),
        json!({"decision": at("$w"), "status": "live", "via": "decided_by"}),
    ];
    want.sort_by_key(|g| g["decision"].as_str().unwrap().to_string());
    assert_eq!(v["governing"], json!(want));
    assert_eq!(
        sorted(rows(&v, "decisions")),
        sorted(vec![(0, at("$y")), (0, at("$w"))])
    );
    // a held source grounds, and is listed apart from the governing decisions
    let v = trace(&r, &Scope::Canon, &ks, wide(3));
    assert_eq!(v["governing"], json!([]));
    assert_eq!(v["decisions"], json!([]));
    assert_eq!(
        v["sources"],
        json!([{"source": at("$src"), "kind": "release", "title": "Release 2"}])
    );
    assert_eq!(v["ungrounded"], false);
    // reflects globs ground; nothing at all does not; a draft block is never ungrounded
    for (k, want) in [("$kg", false), ("$kn", true), ("$kdr", false)] {
        let v = trace(&r, &Scope::Canon, &at(k), wide(3));
        assert_eq!(v["ungrounded"], want, "{k}: {v}");
    }
    // a source, a finding and a decision are never ungrounded
    for k in ["$src", "$f", "$x"] {
        let v = trace(&r, &Scope::Canon, &at(k), wide(3));
        assert_eq!(v["ungrounded"], false, "{k}: {v}");
    }
    // the entity itself is governed directly
    let v = trace(&r, &Scope::Canon, &at("$t"), wide(3));
    assert_eq!(
        v["governing"],
        json!([{"decision": at("$x"), "status": "live", "via": "affects"}])
    );
}

#[test]
fn rf11_introduction_and_acceptance_are_older_than_history_page() {
    let (_dir, mut r) = project();
    let w = put(
        &mut r,
        "w",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "plan", "title": "Plan", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$k", "parent": "$s", "body": "Version 0."},
            dec("$d1", "The plan is written down", json!({}))
        ]),
    );
    let (ref1, l1) = land(&mut r, "w", "First");
    let (k, d1, sec) = (w["$k"].clone(), w["$d1"].clone(), bid(&w["$s"]));
    // D2 lands proposed through the raw writer, and is accepted by a later engine landing
    let d2 = nid("D", 77);
    commit(
        &mut r,
        "p",
        vec![Op::Create {
            block: block(
                &d2,
                "decision",
                &sec,
                json!({"statement": "Plans are reviewed", "status": "proposed"}),
                "",
            ),
        }],
    );
    let v2 = raw_propose(&mut r, "p");
    let l2 = raw_land(&mut r, &v2);
    // more than a history page of later versions of K, as typed changes (one version each)
    let mut kb = r.read().block(&Scope::Canon, &bid(&k)).unwrap();
    let mut deps = r.read().included("w2").to_vec();
    for n in 1..=1001 {
        let base = kb.version_hash();
        kb.body = format!("Version {n}.");
        let c = r
            .store_mut()
            .commit_to_view(
                "w2",
                Change {
                    view: "w2".into(),
                    ops: vec![Op::Update {
                        base,
                        block: kb.clone(),
                    }],
                    deps: deps.clone(),
                    provenance: prov(),
                    justification: None,
                    message: String::new(),
                    links: Default::default(),
                    read_identity: None,
                },
            )
            .unwrap();
        deps = vec![c];
    }
    r.invalidate();
    put(
        &mut r,
        "w2",
        json!([{"op": "update", "id": d2.to_string(), "fields": {"statement": "Plans are reviewed weekly"}}]),
    );
    let (_, l3) = land(&mut r, "w2", "Third");
    // more than a history page of later accepted versions of D1 (a re-affirmation is the one
    // change a frozen decision takes), carried by two later raw landings: 500, then 501
    let mut lands = Vec::new();
    for (view, count) in [("w3", 500), ("w4", 501)] {
        let mut later = r.read().block(&Scope::Canon, &bid(&d1)).unwrap();
        let mut deps = r.read().included(view).to_vec();
        for n in 0..count {
            let base = later.version_hash();
            later.fields.insert(
                "reaffirmed".into(),
                encode_reaffirmed(&BTreeMap::from([(
                    nid("c", 5),
                    Reaffirmation {
                        against: Against::Deleted,
                        reason: format!("Still holds ({view} {n})."),
                    },
                )])),
            );
            let c = r
                .store_mut()
                .commit_to_view(
                    view,
                    Change {
                        view: view.into(),
                        ops: vec![Op::Update {
                            base,
                            block: later.clone(),
                        }],
                        deps: deps.clone(),
                        provenance: prov(),
                        justification: None,
                        message: String::new(),
                        links: Default::default(),
                        read_identity: None,
                    },
                )
                .unwrap();
            deps = vec![c];
        }
        r.invalidate();
        let v = raw_propose(&mut r, view);
        lands.push(raw_land(&mut r, &v));
    }
    assert_ne!(l3, lands[0]);
    assert_ne!(lands[0], lands[1]);
    let d1_versions = r
        .read()
        .history(&Scope::Canon, &bid(&d1), HISTORY_MAX)
        .unwrap();
    assert!(d1_versions.more, "D1 has more than a history page");
    assert!(
        d1_versions
            .entries
            .iter()
            .all(|e| e.landing.as_ref().is_none_or(|l| l.id != l1.to_string())),
        "the page does not reach D1's acceptance"
    );

    // the history page cannot reach the create; the trace can
    let st = r.read();
    let page = st.history(&Scope::Canon, &bid(&k), HISTORY_MAX).unwrap();
    assert!(page.more);
    assert!(page.entries.iter().all(|e| e.op != "create"));
    let v = trace_in(&st, &Scope::Canon, &k, wide(3));
    assert_eq!(
        v["introduced"],
        json!({"id": l1.to_string(), "proposal": ref1, "title": "First", "approved_by": FOUNDER,
               "assurance": "local",
               "author": {"actor": WRITER, "run": "run-1", "role": "writer", "at_ms": v["introduced"]["author"]["at_ms"]}})
    );
    // the oldest accepted version: D1's acceptance change in the first landing
    let v = trace_in(&st, &Scope::Canon, &d1, wide(3));
    let acc = &decision_row(&v, &d1)["accepted_in"];
    assert_eq!(acc["id"], l1.to_string());
    assert_eq!(acc["proposal"], ref1);
    let accept = st.reader().landing(&l1).unwrap().changes;
    let accept = st.reader().change(accept.last().unwrap()).unwrap();
    assert_eq!(acc["author"]["actor"], accept.provenance.actor.as_str());
    assert_eq!(v["introduced"]["id"], l1.to_string());
    // D2: introduced proposed by the raw landing, accepted by the third
    let v = trace_in(&st, &Scope::Canon, d2.as_str(), wide(3));
    assert_eq!(v["introduced"]["id"], l2.to_string());
    assert_eq!(v["introduced"]["proposal"], format!("P-{}@1", v2.short()));
    assert_eq!(
        decision_row(&v, d2.as_str())["accepted_in"]["id"],
        l3.to_string()
    );
    assert_eq!(
        decision_row(&v, d2.as_str())["accepted_in"]["title"],
        "Third"
    );
    // a historical scope reads its own chain: at L2, D2 was never accepted
    let v = trace_in(&st, &Scope::At(l2), d2.as_str(), wide(3));
    assert_eq!(v["introduced"]["id"], l2.to_string());
    assert!(
        decision_row(&v, d2.as_str()).get("accepted_in").is_none(),
        "{v}"
    );

    // never landed: a view's own blocks have no landing; its base chain still counts
    let n = put(
        &mut r,
        "draft",
        json!([
            {"op": "create", "type": "claim", "id": "$n", "parent": sec.to_string(), "body": "Not yet."},
            {"op": "create", "type": "decision", "id": "$dn", "parent": sec.to_string(), "fields": {"statement": "Maybe", "status": "proposed"}}
        ]),
    );
    let draft = Scope::View("draft".into());
    let v = trace(&r, &draft, &n["$n"], wide(3));
    assert_eq!(v["introduced"], Value::Null);
    let v = trace(&r, &draft, &n["$dn"], wide(3));
    assert_eq!(v["introduced"], Value::Null);
    assert!(
        decision_row(&v, &n["$dn"]).get("accepted_in").is_none(),
        "{v}"
    );
    assert_eq!(
        trace(&r, &draft, &k, wide(3))["introduced"]["id"],
        l1.to_string()
    );

    // imported: the first import landing, with the importer
    let out = tempfile::tempdir().unwrap();
    r.read().export(&Scope::Canon, out.path()).unwrap();
    let (_dir2, mut b) = project();
    b.import(&founder(), "imp", out.path()).unwrap();
    let p = b
        .propose(&founder(), "imp", "Import", Some(common::TEST_SUMMARY))
        .unwrap();
    common::confirm_exceptions(&mut b, &founder(), &p);
    let lb = b.land(&founder(), &p.id).unwrap();
    let v = trace(&b, &Scope::Canon, &d1, wide(3));
    let acc = &decision_row(&v, &d1)["accepted_in"];
    assert_eq!(acc["id"], lb);
    assert_eq!(acc["imported_by"], json!({"actor": FOUNDER, "run": null}));
    assert_eq!(v["introduced"]["id"], lb);
    assert_eq!(
        v["introduced"]["imported_by"],
        json!({"actor": FOUNDER, "run": null})
    );
}

/// One seal of `d` at `landing` with one item per outcome in `outcomes`, recorded for `view`.
fn seal(
    r: &mut Rdy,
    view: &str,
    d: &BlockId,
    landing: Hash,
    outcomes: &[(PlanKind, PlanOutcome)],
) -> Hash {
    let mut items: Vec<SealedItem> = outcomes
        .iter()
        .enumerate()
        .map(|(n, (kind, outcome))| {
            let subject = format!("subject-{n}");
            SealedItem {
                id: item_hash(d, &subject, *kind),
                kind: *kind,
                subject,
                cause: d.clone(),
                edge: "references".into(),
                outcome: *outcome,
                answer: None,
            }
        })
        .collect();
    items.sort_by_key(|i| i.id);
    let version = Hash([3; 32]);
    let h = put_plan_frames(
        r.store_mut(),
        &Plan {
            stable: None,
            prev: None,
            view: view.into(),
            proposal: Some(version),
            answers: vec![],
            heads: vec![],
            provenance: prov(),
            seal: Some(PlanSeal {
                landing,
                version,
                decisions: vec![d.clone()],
                items,
            }),
        },
    );
    r.invalidate();
    h
}

fn outcomes(pairs: &[(&str, u64)]) -> Value {
    let mut m = serde_json::Map::new();
    for o in [
        "committed",
        "confirmed",
        "deferred",
        "done",
        "open",
        "reaffirmed",
        "relinked",
        "repinned",
        "retired",
        "updated",
    ] {
        m.insert(o.into(), json!(0));
    }
    for (k, n) in pairs {
        m.insert((*k).into(), json!(n));
    }
    Value::Object(m)
}

#[test]
fn rf11_historical_scope_excludes_future_seals() {
    use PlanKind::*;
    use PlanOutcome::*;
    let (_dir, mut r) = project();
    let w = put(
        &mut r,
        "w",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "p", "title": "P", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            dec("$dd", "Seals are counted per landing chain", json!({}))
        ]),
    );
    let d = bid(&w["$dd"]);
    let v1 = raw_propose(&mut r, "w");
    let l1 = raw_land(&mut r, &v1);
    r.tag(&founder(), "v1.0.0", None, None).unwrap();
    // a view and a proposal version based on L1
    put(
        &mut r,
        "early",
        json!([{"op": "create", "type": "prose", "id": "$e", "parent": w["$s"], "body": "Early."}]),
    );
    let pv = raw_propose(&mut r, "early");
    put(
        &mut r,
        "w2",
        json!([{"op": "create", "type": "prose", "id": "$l", "parent": w["$s"], "body": "Later."}]),
    );
    let v2 = raw_propose(&mut r, "w2");
    let l2 = raw_land(&mut r, &v2);
    // a landing on another chain, never canon's
    let mut off: LandingRecord = r.read().reader().landing(&l2).unwrap();
    off.prev = Some(l1);
    off.proposal = Hash([8; 32]);
    let lx = r.store_mut().put(kinds::LANDING, &off).unwrap();
    // seals: two on L1, one on L2, one on the other chain
    let s1 = seal(
        &mut r,
        "s1",
        &d,
        l1,
        &[(Update, Updated), (Update, Updated), (Update, Open)],
    );
    let s2 = seal(
        &mut r,
        "s2",
        &d,
        l2,
        &[(Confirm, Confirmed), (Update, Updated)],
    );
    seal(&mut r, "sx", &d, lx, &[(Reaffirm, Retired)]);

    let st = r.read();
    // each seal counts once per outcome it holds: S1's two updated items count once
    let canon = outcomes(&[("updated", 2), ("open", 1), ("confirmed", 1)]);
    let first = outcomes(&[("updated", 1), ("open", 1)]);
    let tag = st.resolve_scope("tag:v1.0.0").unwrap();
    // a view reads its base landing's chain, wherever the landing left it
    let view_want = match st.effective_base("early") {
        Some(b) if b == l1 => &first,
        Some(b) if b == l2 => &canon,
        b => panic!("the view's base is {b:?}"),
    };
    let cases = [
        ("canon", Scope::Canon, &canon),
        ("inflight", Scope::Inflight, &canon),
        ("landing L2", Scope::At(l2), &canon),
        ("landing L1", Scope::At(l1), &first),
        ("tag", tag, &first),
        ("view", Scope::View("early".into()), view_want),
        ("proposal", Scope::Proposal(pv), &first),
    ];
    for (what, scope, want) in &cases {
        let v = trace_in(&st, scope, d.as_str(), wide(3));
        assert_eq!(&decision_row(&v, d.as_str())["plans"], *want, "{what}");
    }
    // detail: one row per seal of the chain, oldest landing first, with its own item counts
    let row = |h: Hash| {
        let (landing, items, o) = if h == s1 {
            (l1, 3, outcomes(&[("updated", 2), ("open", 1)]))
        } else {
            (l2, 2, outcomes(&[("confirmed", 1), ("updated", 1)]))
        };
        json!({"landing": landing.to_string(), "version": Hash([3; 32]).to_string(), "items": items, "outcomes": o})
    };
    let v = trace_in(&st, &Scope::Canon, d.as_str(), opts(3, true, 100_000));
    assert_eq!(
        decision_row(&v, d.as_str())["sealed_plans"],
        json!([row(s1), row(s2)])
    );
    let v = trace_in(&st, &Scope::At(l1), d.as_str(), opts(3, true, 100_000));
    assert_eq!(
        decision_row(&v, d.as_str())["sealed_plans"],
        json!([row(s1)])
    );
    // a normal read carries counts only
    let v = trace_in(&st, &Scope::Canon, d.as_str(), wide(3));
    assert!(decision_row(&v, d.as_str()).get("sealed_plans").is_none());
}

#[test]
fn rf11_detail_copies_evidence_without_reconstructing_it() {
    let (_dir, mut r) = project();
    let w = put(
        &mut r,
        "w",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "p", "title": "P", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "decision", "id": "$dd", "parent": "$s",
                "fields": {"statement": "Keep the archive", "status": "proposed", "options": ["Keep", "Drop"],
                           "authority_basis": "direct_ruling", "reversibility": "two_way"}}
        ]),
    );
    let d = w["$dd"].clone();
    let p = r
        .propose(&writer(), "w", "Archive", Some(common::TEST_SUMMARY))
        .unwrap();
    common::confirm_exceptions(&mut r, &founder(), &p);
    let card = Hash([5; 32]);
    let landed = r
        .land_with(
            &founder(),
            &p.r#ref,
            LandOptions {
                strict: false,
                relay: Some(Relay {
                    by: ActorRun {
                        actor: "agent:relay".into(),
                        run: Some("r-9".into()),
                    },
                    evidence: RelayEvidence {
                        quoted: "Yes, keep it.".into(),
                        session_ref: "chat-1".into(),
                        question: Some("Keep the archive?".into()),
                    },
                }),
                evidence: Some(ApprovalEvidence {
                    kind: EvidenceKind::Card,
                    options: vec!["approve".into(), "reject".into()],
                    choice: Some("approve".into()),
                    card_hash: Some(card),
                }),
            },
        )
        .unwrap();
    let l = landed.landing.clone();
    // a re-affirmation record, written as the engine field (a typed write)
    let t1 = nid("c", 1);
    let t2 = nid("c", 2);
    let record = encode_reaffirmed(&BTreeMap::from([
        (
            t2.clone(),
            Reaffirmation {
                against: Against::Deleted,
                reason: "It was removed on purpose.".into(),
            },
        ),
        (
            t1.clone(),
            Reaffirmation {
                against: Against::Retiring(vec![bid(&d)]),
                reason: "It still holds.".into(),
            },
        ),
    ]));
    let mut next = r.read().block(&Scope::Canon, &bid(&d)).unwrap();
    let base = next.version_hash();
    next.fields.insert("reaffirmed".into(), record.clone());
    commit(&mut r, "w2", vec![Op::Update { base, block: next }]);
    let w2 = Scope::View("w2".into());

    let st = r.read();
    let log = st.log_rows().unwrap();
    let row = serde_json::to_value(log.iter().find(|x| x.landing == l).unwrap()).unwrap();
    // normal: the relay's actor, the target ids; no evidence, options or seals
    let v = trace_in(&st, &w2, &d, wide(3));
    let dv = decision_row(&v, &d);
    assert_eq!(dv["authority_basis"], "direct_ruling");
    assert_eq!(dv["reversibility"], "two_way");
    assert!(
        dv.get("attested_by").is_none() && dv.get("key").is_none(),
        "{dv}"
    );
    assert_eq!(dv["reaffirmed"], json!([t1.to_string(), t2.to_string()]));
    for k in ["options", "sealed_plans"] {
        assert!(dv.get(k).is_none(), "{k}: {dv}");
    }
    let acc = &dv["accepted_in"];
    assert_eq!(acc["id"], l);
    assert_eq!(
        acc["relayed_by"],
        json!({"actor": "agent:relay", "run": "r-9"})
    );
    assert_eq!(acc["assurance"], row["assurance"]);
    for k in ["evidence", "card_evidence"] {
        assert!(acc.get(k).is_none(), "{k}: {acc}");
    }
    assert!(v["introduced"].get("evidence").is_none(), "{v}");
    // detail: the log's own values, the authored options and the whole engine field
    let v = trace_in(&st, &w2, &d, opts(3, true, 100_000));
    let dv = decision_row(&v, &d);
    assert_eq!(dv["options"], json!(["Keep", "Drop"]));
    assert_eq!(dv["reaffirmed"], field_json(&record));
    assert!(dv["sealed_plans"].is_array(), "{dv}");
    let acc = &dv["accepted_in"];
    assert_eq!(acc["evidence"], row["relay_evidence"]);
    assert_eq!(acc["card_evidence"], row["evidence"]);
    assert_eq!(
        acc["card_evidence"],
        json!({"kind": "card", "options": ["approve", "reject"], "choice": "approve", "card_hash": card.to_string()})
    );
    assert_eq!(v["introduced"]["evidence"], row["relay_evidence"]);
}

#[test]
fn rf11_depth_budget_and_frontier_are_deterministic() {
    let l = lineage();
    let k = l.id["K"].clone();
    // repeated reads, from two states, are byte-identical
    let a = serde_json::to_vec(
        &l.r.read()
            .trace(&Scope::Canon, &bid(&k), TraceOptions::default())
            .unwrap(),
    )
    .unwrap();
    let b = serde_json::to_vec(
        &l.r.read()
            .trace(&Scope::Canon, &bid(&k), TraceOptions::default())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(a, b);
    // depth 1: C is a boundary (D beyond it), A is not (its neighbours are shown); tied depths
    // sort by id
    let v = trace(&l.r, &Scope::Canon, &k, wide(1));
    assert_eq!(
        rows(&v, "decisions"),
        l.at(&[(0, "B"), (1, "A"), (1, "C"), (1, "E")])
    );
    let want: Vec<String> = l
        .at(&[(1, "C"), (1, "E")])
        .into_iter()
        .map(|p| p.1)
        .collect();
    assert_eq!(ids(&v["frontier"]), want);
    assert!(
        v.get("truncated").is_none() && v.get("omitted").is_none(),
        "{v}"
    );
    // depth 8: the whole lineage; only the unexpanded premise is left
    let v = trace(&l.r, &Scope::Canon, &k, wide(8));
    assert_eq!(
        rows(&v, "decisions"),
        l.at(&[
            (0, "B"),
            (1, "A"),
            (1, "C"),
            (1, "E"),
            (2, "D"),
            (3, "G"),
            (4, "H")
        ])
    );
    assert_eq!(ids(&v["frontier"]), vec![l.id["E"].clone()]);
    assert!(v.get("truncated").is_none(), "{v}");
    // the boundary and the unexpanded premise join the frontier with no cut flags
    let full = trace(&l.r, &Scope::Canon, &k, wide(3));
    assert!(full.get("truncated").is_none() && full.get("omitted").is_none());
    let full_len = serde_json::to_vec(&full).unwrap().len();

    // a budget just under the whole reply drops the deepest row only; G is already in the
    // frontier, so it is not listed twice, and its edge to D stays
    let max = (full_len - 1) / 4 - 1;
    let reply =
        l.r.read()
            .trace(&Scope::Canon, &bid(&k), opts(3, false, max))
            .unwrap();
    let bytes = serde_json::to_vec(&reply).unwrap();
    assert!(
        bytes.len() / 4 <= max,
        "{} bytes over {max} tokens",
        bytes.len()
    );
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["truncated"], true);
    assert_eq!(v["omitted"], 1);
    assert_eq!(
        rows(&v, "decisions"),
        l.at(&[(0, "B"), (1, "A"), (1, "C"), (1, "E"), (2, "D")])
    );
    assert_eq!(v["frontier"], full["frontier"]);
    assert!(
        v["edges"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["from"] == l.id["G"] && e["to"] == l.id["D"]),
        "the boundary edge stays: {v}"
    );

    // a zero budget keeps the envelope and the depth-zero decision with the rows it still owns
    // (F, Q) and the edges that join it to a frontier decision; every removed decision is in the
    // frontier once, and omitted counts the removed rows
    let i = |k: &str| l.id[k].clone();
    for max in [0, 1, 10] {
        let v = trace(&l.r, &Scope::Canon, &k, opts(3, false, max));
        assert_eq!(v["truncated"], true, "{max}");
        assert_eq!(v["omitted"], 5, "{max}");
        assert_eq!(rows(&v, "decisions"), l.at(&[(0, "B")]), "{max}");
        assert_eq!(
            decision_row(&v, &l.id["B"])["statement"],
            "Approvers sign refunds over 100"
        );
        assert_eq!(v["governing"], full["governing"]);
        assert_eq!(rows(&v, "premises"), l.at(&[(1, "F")]), "{max}");
        assert_eq!(rows(&v, "questions"), l.at(&[(1, "Q")]), "{max}");
        let mut want = vec![
            (0, i("B"), i("A"), "refines".to_string()),
            (0, i("C"), i("B"), "refines".to_string()),
            (0, i("B"), i("E"), "depends_on".to_string()),
            (0, i("B"), i("F"), "depends_on".to_string()),
            (0, i("B"), i("Q"), "answers".to_string()),
        ];
        want.sort();
        // the frontier rows are gone, so their depths come from the full reply
        let depth: BTreeMap<String, u64> = rows(&full, "decisions")
            .into_iter()
            .chain(rows(&full, "premises"))
            .chain(rows(&full, "questions"))
            .map(|(d, id)| (id, d))
            .collect();
        let got: Vec<_> = v["edges"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                let (f, t) = (e["from"].as_str().unwrap(), e["to"].as_str().unwrap());
                (
                    depth[f].min(depth[t]),
                    f.to_string(),
                    t.to_string(),
                    e["kind"]["Link"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        assert_eq!(got, want, "{max}");
        let want: Vec<String> = l
            .at(&[(1, "A"), (1, "C"), (1, "E"), (2, "D"), (3, "G")])
            .into_iter()
            .map(|p| p.1)
            .collect();
        assert_eq!(ids(&v["frontier"]), want, "{max}");
    }
    // overlapping routes under a zero budget: R stays with its own premise, question and the
    // edges to its frontier decisions A and B; the edges among removed decisions go
    let (_dir, r, o) = overlap();
    let oi = |k: &str| o[k].clone();
    let view = Scope::View("o".into());
    let v = trace(&r, &view, &oi("$r"), opts(3, false, 0));
    assert_eq!(v["truncated"], true);
    assert_eq!(v["omitted"], 3);
    assert_eq!(rows(&v, "decisions"), vec![(0, oi("$r"))]);
    assert_eq!(rows(&v, "premises"), vec![(1, oi("$p"))]);
    assert_eq!(rows(&v, "questions"), vec![(1, oi("$q"))]);
    let mut want = vec![oi("$a"), oi("$b")];
    want.sort();
    want.push(oi("$c"));
    assert_eq!(ids(&v["frontier"]), want);
    let mut want = vec![
        (oi("$r"), oi("$a"), "refines"),
        (oi("$r"), oi("$b"), "depends_on"),
        (oi("$r"), oi("$p"), "depends_on"),
        (oi("$r"), oi("$q"), "answers"),
    ];
    want.sort();
    let got: Vec<_> = v["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["from"].as_str().unwrap().to_string(),
                e["to"].as_str().unwrap().to_string(),
                e["kind"]["Link"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(got, want);
    // a lone depth-zero decision over the budget: nothing is removed, so omitted is 0, and the
    // rows it owns stay
    let v = trace(&r, &view, &oi("$x"), opts(3, false, 0));
    assert_eq!(v["truncated"], true);
    assert_eq!(v["omitted"], 0);
    assert_eq!(rows(&v, "decisions"), vec![(0, oi("$x"))]);
    assert_eq!(rows(&v, "premises"), vec![(1, oi("$p"))]);
    assert_eq!(rows(&v, "questions"), vec![(1, oi("$q"))]);
    assert_eq!(v["frontier"], json!([]));
    assert_eq!(v["edges"].as_array().unwrap().len(), 2, "{v}");
    // the refusals, checked by the engine as at every boundary
    let st = l.r.read();
    for depth in [0, 9] {
        refused(
            st.trace(&Scope::Canon, &bid(&k), opts(depth, false, 1000))
                .unwrap_err(),
            E_BAD_INPUT,
            &format!("depth {depth} is outside 1 to 8"),
            "pass a depth from 1 to 8",
        );
    }
    refused(
        rdy_engine::trace::check_depth(-1).unwrap_err(),
        E_BAD_INPUT,
        "depth -1 is outside 1 to 8",
        "pass a depth from 1 to 8",
    );
    assert_eq!(rdy_engine::trace::check_depth(8).unwrap(), 8);
    refused(
        st.trace(&Scope::Canon, &bid(&k), opts(3, false, 100_001))
            .unwrap_err(),
        E_LIMIT,
        "max_tokens 100001 is over the limit of 100000",
        "ask for at most 100000 tokens and page through the rest",
    );
    refused(
        st.trace(&Scope::Canon, &bid("c-ZZZZZ"), TraceOptions::default())
            .unwrap_err(),
        E_UNKNOWN_BLOCK,
        "unknown block c-ZZZZZ",
        "check the id with rdy_search",
    );
    assert_eq!(
        TraceOptions::default(),
        opts(3, false, 1000),
        "the defaults are depth 3, no detail, 1000 tokens"
    );
}

/// A view `h` holding a doc, a section and decision `root` (proposed), for typed writes.
fn hostile() -> (tempfile::TempDir, Rdy, BlockId, BlockId) {
    let (dir, mut r) = project();
    let w = put(
        &mut r,
        "h",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "h", "title": "H", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            dec("$root", "The root", json!({}))
        ]),
    );
    (dir, r, bid(&w["$s"]), bid(&w["$root"]))
}

/// `root` in view `h` with `links` replacing its links.
fn relink(r: &mut Rdy, root: &BlockId, links: BTreeMap<String, Vec<Link>>) {
    let mut b = r.read().block(&Scope::View("h".into()), root).unwrap();
    let base = b.version_hash();
    b.links = links;
    commit(r, "h", vec![Op::Update { base, block: b }]);
}

#[test]
fn rf11_hostile_trace_is_bounded_and_redaction_safe() {
    let h = Scope::View("h".into());
    let walk_fix = "trace a governing decision with a smaller depth";
    // ids: the root and 4999 premises are 5000 ids; one more is refused
    {
        let (_dir, mut r, sec, root) = hostile();
        let prose: Vec<BlockId> = (0..5000).map(|n| nid("p", n)).collect();
        commit(
            &mut r,
            "h",
            prose
                .iter()
                .map(|p| Op::Create {
                    block: block(p, "prose", &sec, json!({}), "A premise."),
                })
                .collect(),
        );
        let links = |n: usize| {
            BTreeMap::from([(
                "depends_on".to_string(),
                prose[..n].iter().map(|p| link(p, None)).collect(),
            )])
        };
        relink(&mut r, &root, links(4999));
        let v = trace(&r, &h, root.as_str(), wide(3));
        assert_eq!(
            v["truncated"], true,
            "a 1000-token budget cuts the premises"
        );
        relink(&mut r, &root, links(5000));
        refused(
            r.read()
                .trace(&h, &root, TraceOptions::default())
                .unwrap_err(),
            E_LIMIT,
            "trace exceeds 5000 blocks",
            walk_fix,
        );
    }
    // edges: 20000 distinct edges pass, one more is refused (the part makes an edge distinct);
    // each count in its own project, so no graph is carried across the relink
    for (n, ok) in [(20_000, true), (20_001, false)] {
        let (_dir, mut r, sec, root) = hostile();
        let p = nid("p", 1);
        commit(
            &mut r,
            "h",
            vec![Op::Create {
                block: block(&p, "prose", &sec, json!({}), "One premise."),
            }],
        );
        let parts = (0..n)
            .map(|i| link(&p, Some(&format!("part {i}"))))
            .collect();
        relink(
            &mut r,
            &root,
            BTreeMap::from([("depends_on".to_string(), parts)]),
        );
        let got = r.read().trace(&h, &root, TraceOptions::default());
        if ok {
            assert!(got.is_ok(), "{n}");
        } else {
            refused(
                got.unwrap_err(),
                E_LIMIT,
                "trace exceeds 20000 edges",
                walk_fix,
            );
        }
    }
    // bytes: long statements are measured before they are copied; a lineage cycle ends
    {
        let (_dir, mut r, sec, root) = hostile();
        let long = "x".repeat(1_000_000);
        let ds: Vec<BlockId> = (0..17).map(|n| nid("D", 100 + n)).collect();
        for (n, d) in ds.iter().enumerate() {
            let mut b = block(
                d,
                "decision",
                &sec,
                json!({"statement": long, "status": "proposed"}),
                "",
            );
            // each refines the one before, and the first refines the last: a cycle
            let prev = if n == 0 { &ds[16] } else { &ds[n - 1] };
            b.links
                .insert("refines".into(), vec![link(prev, None), link(&root, None)]);
            commit(&mut r, "h", vec![Op::Create { block: b }]);
        }
        refused(
            r.read().trace(&h, &root, wide(8)).unwrap_err(),
            E_LIMIT,
            "trace exceeds 16777216 bytes",
            walk_fix,
        );
        // at depth 1 the 17 rows are there once each; the envelope alone is over the budget
        let v = trace(&r, &h, ds[0].as_str(), opts(1, false, 1000));
        assert_eq!(v["truncated"], true);
        assert_eq!(rows(&v, "decisions"), vec![(0, ds[0].to_string())]);
        assert_eq!(
            v["decisions"][0]["statement"].as_str().unwrap().len(),
            1_000_000
        );
        let mut want: Vec<(u64, String)> = vec![
            (1, ds[1].to_string()),
            (1, ds[16].to_string()),
            (1, root.to_string()),
        ];
        want.sort();
        assert_eq!(v["omitted"], 3);
        assert_eq!(
            ids(&v["frontier"]),
            want.into_iter().map(|p| p.1).collect::<Vec<_>>()
        );
    }
    // the cut's flags count against the byte cap: 17 held sources (the envelope, never cut) make
    // a reply of exactly 16777216 bytes, flags included, which passes; one byte more is refused,
    // though both are under the cap before the cut adds `truncated` and `omitted`
    {
        let (_dir, mut r, sec, _) = hostile();
        let k = nid("c", 1);
        let srcs: Vec<BlockId> = (0..17).map(|n| nid("S", 100 + n)).collect();
        for (n, s) in srcs.iter().enumerate() {
            let len = if n == 16 { 700_000 } else { 1_000_000 };
            let b = block(
                s,
                "source",
                &sec,
                json!({"title": "a".repeat(len), "kind": "release", "locator": "v1"}),
                "",
            );
            commit(&mut r, "h", vec![Op::Create { block: b }]);
        }
        let mut claim = block(&k, "claim", &sec, json!({}), "Reflects seventeen releases.");
        claim.links.insert(
            "reflects".into(),
            srcs.iter().map(|s| link(s, None)).collect(),
        );
        commit(&mut r, "h", vec![Op::Create { block: claim }]);
        let len_of = |r: &Rdy| {
            r.read()
                .trace(&h, &k, TraceOptions::default())
                .map(|t| serde_json::to_vec(&t).unwrap().len())
        };
        let retitle = |r: &mut Rdy, len: usize| {
            let mut b = r.read().block(&h, &srcs[16]).unwrap();
            let base = b.version_hash();
            b.fields
                .insert("title".into(), FieldValue::Str("a".repeat(len)));
            commit(r, "h", vec![Op::Update { base, block: b }]);
        };
        let base = len_of(&r).unwrap();
        assert!(base < 16_777_216, "{base}");
        let fit = 700_000 + (16_777_216 - base);
        retitle(&mut r, fit);
        let reply = r.read().trace(&h, &k, TraceOptions::default()).unwrap();
        assert!(reply.truncated);
        assert_eq!(reply.omitted, Some(0));
        assert_eq!(serde_json::to_vec(&reply).unwrap().len(), 16_777_216);
        retitle(&mut r, fit + 1);
        refused(
            len_of(&r).unwrap_err(),
            E_LIMIT,
            "trace exceeds 16777216 bytes",
            walk_fix,
        );
    }
    // the redaction barrier: an old state refuses, a fresh one never shows the removed text
    {
        const CANARY: &str = "The vault code is ZEBRA-7731.";
        let (_dir, mut r) = project();
        let w = put(
            &mut r,
            "work",
            json!([
                {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "v", "title": "V", "doc_type": "page"}},
                {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
                {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": CANARY},
                dec("$dd", "The vault stays shut", json!({"depends_on": [{"target": "$p"}]}))
            ]),
        );
        land(&mut r, "work", "Vault");
        let old = r.read();
        let before = trace_in(&old, &Scope::Canon, &w["$dd"], wide(3));
        assert!(before.to_string().contains("ZEBRA-7731"), "{before}");
        let req = r
            .redact_in(
                &Session::human(FOUNDER.into(), Role::Writer).unwrap(),
                "redact",
                RedactInput {
                    targets: vec![w["$p"].clone()],
                    reason_class: "secret".into(),
                    reason: "a leaked code".into(),
                    dry_run: false,
                },
            )
            .unwrap();
        assert!(req.redaction.is_some());
        let p = r
            .propose(
                &Session::human(FOUNDER.into(), Role::Writer).unwrap(),
                "redact",
                "Remove the code",
                Some("Removes the leaked code."),
            )
            .unwrap();
        r.land(
            &Session::human(FOUNDER.into(), Role::Approver).unwrap(),
            &p.id,
        )
        .unwrap();
        let e = old
            .trace(&Scope::Canon, &bid(&w["$dd"]), TraceOptions::default())
            .unwrap_err();
        assert_eq!(e.code, E_REDACTED_STATE, "{e}");
        let after = trace(&r, &Scope::Canon, &w["$dd"], opts(3, true, 100_000));
        assert!(!after.to_string().contains("ZEBRA-7731"), "{after}");
        assert_eq!(rows(&after, "premises"), vec![(1, w["$p"].clone())]);
        let pr = trace(&r, &Scope::Canon, &w["$p"], opts(3, true, 100_000));
        assert!(!pr.to_string().contains("ZEBRA-7731"), "{pr}");
    }
}

#[test]
fn rf11_trace_writes_nothing() {
    let l = lineage();
    let state = |r: &Rdy| {
        (
            r.store().log_end(),
            r.read().reader().refs_with_prefix("").unwrap(),
            r.read().view_names(),
        )
    };
    let before = state(&l.r);
    for (scope, detail) in [
        (Scope::Canon, false),
        (Scope::Canon, true),
        (Scope::Inflight, true),
        (Scope::View("work".into()), true),
    ] {
        for k in ["K", "B", "Z"] {
            trace(&l.r, &scope, &l.id[k], opts(8, detail, 0));
            trace(&l.r, &scope, &l.id[k], opts(8, detail, 100_000));
        }
    }
    assert_eq!(state(&l.r), before);
}
