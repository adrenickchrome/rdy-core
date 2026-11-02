//! nb6 (D-YXBXH, O7): related decisions by text. `same_entity` also covers live, partly
//! superseded and proposed decisions whose statement or body names an entity through the name
//! matcher, and each row says whether a `link` or the `text` relates it. The reader is pure over
//! one state and its lexicon, and refuses a call past its work guards with `E_LIMIT`.
use rdy_engine::lexicon::text_scans;
use rdy_engine::related::{RELATED_CANDIDATE_VISITS, RELATED_CAP, RELATED_TEXT_CANDIDATES};
use rdy_engine::*;
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};

const VIEW: &str = "work";

struct World {
    s: String,
    /// Entity "Ledger service" and entity "Snapshot".
    e: String,
    e2: String,
}

fn world(r: &mut Rdy) -> World {
    let w = write(r, VIEW, json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Summary"}},
        {"op": "create", "type": "entity", "id": "$e", "parent": "$s", "fields": {"name": "Ledger service"}},
        {"op": "create", "type": "entity", "id": "$e2", "parent": "$s", "fields": {"name": "Snapshot"}}
    ]))
    .unwrap();
    land(r, VIEW).unwrap();
    World {
        s: w.created["$s"].clone(),
        e: w.created["$e"].clone(),
        e2: w.created["$e2"].clone(),
    }
}

/// A decision in the view (proposed, not landed).
fn decide(r: &mut Rdy, w: &World, statement: &str, affects: &[&str], refines: &[&str]) -> String {
    decide_with(
        r,
        DecisionInput {
            parent: w.s.clone(),
            statement: statement.into(),
            affects: affects.iter().map(|s| s.to_string()).collect(),
            refines: refines.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        },
    )
}

fn decide_with(r: &mut Rdy, d: DecisionInput) -> String {
    r.decide(&writer(), VIEW, d).unwrap().created["$decision"].clone()
}

fn scope() -> Scope {
    Scope::View(VIEW.into())
}

fn related_in_scope(r: &Rdy, scope: &Scope, id: &str) -> (Vec<Value>, usize) {
    let (rows, omitted) = r
        .read()
        .related_decisions(scope, &[BlockId::parse(id).unwrap()])
        .unwrap();
    (
        rows.into_iter()
            .map(|x| serde_json::to_value(x).unwrap())
            .collect(),
        omitted,
    )
}

/// The rows of decision `id` in the view, with no omitted count.
fn related(r: &Rdy, id: &str) -> Vec<Value> {
    let (rows, omitted) = related_in_scope(r, &scope(), id);
    assert_eq!(omitted, 0);
    rows
}

/// (decision, relation, via) of each row.
fn shape(rows: &[Value]) -> Vec<(String, String, String)> {
    rows.iter()
        .map(|x| {
            (
                x["decision"].as_str().unwrap().to_string(),
                x["relation"].as_str().unwrap().to_string(),
                x["via"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// The `same_entity` rows `(decision, via)` of `rows`, by decision id.
fn same_entity(rows: &[Value]) -> Vec<(String, String)> {
    shape(rows)
        .into_iter()
        .filter(|(_, rel, _)| rel == "same_entity")
        .map(|(d, _, via)| (d, via))
        .collect()
}

/// `(decision, via)` pairs in decision id order.
fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = list
        .iter()
        .map(|(d, v)| (d.to_string(), v.to_string()))
        .collect();
    out.sort();
    out
}

fn row_of<'a>(rows: &'a [Value], decision: &str, relation: &str) -> &'a Value {
    rows.iter()
        .find(|x| x["decision"] == decision && x["relation"] == relation)
        .unwrap_or_else(|| panic!("no {relation} row for {decision} in {rows:#?}"))
}

#[test]
fn nb6_related_text_is_symmetric_and_plural_aware() {
    let (_d, mut r) = project();
    let w = world(&mut r);
    // a is tied to the entity; b names it in its statement; c names it in the plural, with
    // other case; g names it only in its body
    let a = decide(&mut r, &w, "Entries are append-only", &[&w.e], &[]);
    let b = decide(&mut r, &w, "The ledger service keeps every entry", &[], &[]);
    let c = decide(&mut r, &w, "LEDGER SERVICES are replicated", &[], &[]);
    let g = decide_with(
        &mut r,
        DecisionInput {
            parent: w.s.clone(),
            statement: "Entries are kept forever".into(),
            body: "The Ledger service holds them.".into(),
            ..Default::default()
        },
    );
    // the entity named in the title, the card words and the options only: not the statement or
    // the body
    let n = decide_with(
        &mut r,
        DecisionInput {
            parent: w.s.clone(),
            statement: "Entries are kept for a year".into(),
            title: Some("Ledger service".into()),
            why: Some("The ledger service owners decide it".into()),
            options: vec!["Ledger service A".into(), "Ledger service B".into()],
            ..Default::default()
        },
    );
    let a2 = decide(&mut r, &w, "Entries are signed", &[&w.e], &[]);

    // link / text: a's end is a tie, the other end names the entity
    let rows = related(&r, &a);
    assert_eq!(
        same_entity(&rows),
        pairs(&[(&b, "text"), (&c, "text"), (&g, "text"), (&a2, "link")])
    );
    assert!(rows.iter().all(|x| x["entities"] == json!([w.e])));
    // text / link: the same pairs, read from the other end
    assert_eq!(
        same_entity(&related(&r, &b)),
        pairs(&[(&a, "text"), (&c, "text"), (&g, "text"), (&a2, "text")])
    );
    // text / text: b and c, b and g, c and g
    assert_eq!(
        same_entity(&related(&r, &c)),
        pairs(&[(&a, "text"), (&b, "text"), (&g, "text"), (&a2, "text")])
    );
    assert_eq!(
        same_entity(&related(&r, &g)),
        pairs(&[(&a, "text"), (&b, "text"), (&c, "text"), (&a2, "text")])
    );
    // link / link
    assert_eq!(
        same_entity(&related(&r, &a2))
            .into_iter()
            .find(|(d, _)| *d == a),
        Some((a.clone(), "link".to_string()))
    );
    // the title, card words and options name nothing: n has no row and is on no row
    assert_eq!(related(&r, &n), Vec::<Value>::new());
    for d in [&a, &b, &c, &g, &a2] {
        assert!(
            related(&r, d).iter().all(|x| x["decision"] != n.as_str()),
            "{n} is listed for {d}"
        );
    }
    // a full row
    let b_row = row_of(&related(&r, &a), &b, "same_entity").clone();
    assert_eq!(
        b_row,
        json!({"for": a, "decision": b, "status": "proposed",
               "statement": "The ledger service keeps every entry",
               "relation": "same_entity", "via": "text", "entities": [w.e]})
    );
}

#[test]
fn nb6_related_via_prefers_a_complete_link_route() {
    let (_d, mut r) = project();
    let w = world(&mut r);
    // x is tied to the ledger service and names snapshots in its statement
    let x = decide(&mut r, &w, "Snapshots are taken hourly", &[&w.e], &[]);
    // both entities tied at d1 and shared: the ledger service is tied at both ends, so link; the
    // entity ids are the union, sorted
    let d1 = decide(&mut r, &w, "Reads use the index", &[&w.e, &w.e2], &[]);
    // d2 is tied to the snapshot only: x only names it, so the shared entity is text
    let d2 = decide(&mut r, &w, "Entries are signed", &[&w.e2], &[]);
    // d3 names both entities and ties none: text, with the union
    let d3 = decide(
        &mut r,
        &w,
        "The ledger service writes a snapshot nightly",
        &[],
        &[],
    );
    // a decision tied to an unrelated entity of its own only shares nothing
    let rows = related(&r, &x);
    let mut both = vec![w.e.clone(), w.e2.clone()];
    both.sort();
    assert_eq!(
        same_entity(&rows),
        pairs(&[(&d1, "link"), (&d2, "text"), (&d3, "text")])
    );
    assert_eq!(row_of(&rows, &d1, "same_entity")["entities"], json!(both));
    assert_eq!(row_of(&rows, &d2, "same_entity")["entities"], json!([w.e2]));
    assert_eq!(row_of(&rows, &d3, "same_entity")["entities"], json!(both));
    // the other end of the mixed pair: d2 ties the snapshot, x names it: still text
    assert_eq!(
        same_entity(&related(&r, &d2))
            .into_iter()
            .find(|(d, _)| *d == x),
        Some((x.clone(), "text".to_string()))
    );

    // lineage is always link; a pair with lineage and a text same_entity keeps both rows
    let l = decide(
        &mut r,
        &w,
        "Reads of the ledger service use the index",
        &[],
        &[&x],
    );
    let rows = related(&r, &x);
    assert_eq!(
        shape(&rows)
            .into_iter()
            .filter(|(d, _, _)| *d == l)
            .collect::<Vec<_>>(),
        vec![
            (l.clone(), "refined_by".into(), "link".into()),
            (l.clone(), "same_entity".into(), "text".into())
        ],
        "lineage ranks before same_entity, each row with its own via"
    );
    assert_eq!(row_of(&rows, &l, "refined_by").get("entities"), None);
    let rows = related(&r, &l);
    assert_eq!(row_of(&rows, &x, "refines")["via"], "link");
    // the pair that ties the same entity at both ends and refines: both rows are link
    let m = decide(&mut r, &w, "Snapshots keep a checksum", &[&w.e], &[&x]);
    let rows = related(&r, &x);
    assert_eq!(row_of(&rows, &m, "refined_by")["via"], "link");
    assert_eq!(row_of(&rows, &m, "same_entity")["via"], "link");
}

/// A raw decision as the approver writes it (a status the engine's own tools never set).
fn raw_decision(id: &str, parent: &str, statement: &str, status: &str, links: Value) -> Value {
    json!({"op": "create", "type": "decision", "id": id, "parent": parent,
           "fields": {"statement": statement, "status": status,
                      "decided_at": "2021-01-10T09:00:00Z", "recorded_at": "2021-01-10T09:00:00Z",
                      "options": ["yes", "no"]},
           "links": links})
}

#[test]
fn nb6_related_statuses_order_and_caps_stay_exact() {
    let (_d, mut r) = project();
    let w = world(&mut r);
    let land_it = |r: &mut Rdy| {
        land(r, VIEW).unwrap();
    };
    // canon decisions that name the entity in text only, one for each status
    let anchor = decide(&mut r, &w, "Entries are append-only", &[&w.e], &[]);
    let live = decide(&mut r, &w, "The ledger service is replicated", &[], &[]);
    let part = decide(
        &mut r,
        &w,
        "The ledger service is durable and fast",
        &[],
        &[],
    );
    let whole = decide(&mut r, &w, "The ledger service is versioned", &[], &[]);
    let gone = decide(&mut r, &w, "The ledger service is public", &[], &[]);
    land_it(&mut r);
    // a decision that supersedes `part` in part, one that supersedes `whole` entirely and one
    // that revokes `gone`
    let new_part = decide(&mut r, &w, "The ledger service is fast", &[], &[]);
    write(
        &mut r,
        VIEW,
        json!([{"op": "update", "id": new_part,
                "links": {"supersedes": [{"target": part, "part": "the speed"}]}}]),
    )
    .unwrap();
    let new_whole = r
        .supersede(
            &writer(),
            VIEW,
            &whole,
            None,
            DecisionInput {
                parent: w.s.clone(),
                statement: "The ledger service is versioned twice".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    let revoker = r
        .revoke(&writer(), VIEW, &gone, "wrong", &w.s, None)
        .unwrap()
        .created["$decision"]
        .clone();
    // a rejected decision that refines the anchor and names the entity
    let rejected = rdy_qa::golden::write_as(
        &approver(),
        &mut r,
        VIEW,
        json!([raw_decision(
            "$rej",
            &w.s,
            "The ledger service may be eventually consistent",
            "rejected",
            json!({"refines": [{"target": anchor}]})
        )]),
    )
    .unwrap()
    .created["$rej"]
        .clone();
    land_it(&mut r);
    // and a proposed one, left in the view
    let proposed = decide(&mut r, &w, "The ledger service is audited", &[], &[]);

    let st = r.read();
    let g = st.graph(&scope()).unwrap();
    let status = |d: &str| g.status(&BlockId::parse(d).unwrap()).unwrap().tag();
    assert_eq!(
        [
            &live, &part, &whole, &gone, &new_part, &new_whole, &rejected, &proposed
        ]
        .map(|d| status(d)),
        [
            "live",
            "partially_superseded",
            "superseded",
            "revoked",
            "live",
            "live",
            "rejected",
            "proposed"
        ]
    );
    drop(st);

    let rows = related(&r, &anchor);
    // accepted, partly superseded and proposed decisions are listed; the rejected, the revoked
    // and the wholly superseded are not
    let want = pairs(&[
        (&live, "text"),
        (&part, "text"),
        (&new_part, "text"),
        (&new_whole, "text"),
        (&proposed, "text"),
    ]);
    assert_eq!(same_entity(&rows), want);
    // a lineage row keeps its status, whatever it is
    assert_eq!(
        shape(&rows)[0],
        (rejected.clone(), "refined_by".into(), "link".into())
    );
    assert_eq!(rows[0]["status"], "rejected");
    // the lineage neighbours of the revoked and the superseded decisions show their status
    let rows_gone = related(&r, &gone);
    assert_eq!(row_of(&rows_gone, &revoker, "revoked_by")["status"], "live");
    let rows_whole = related(&r, &whole);
    assert_eq!(
        row_of(&rows_whole, &new_whole, "superseded_by")["via"],
        "link"
    );
    assert_eq!(
        rows_whole
            .iter()
            .find(|x| x["decision"] == new_whole.as_str() && x["relation"] == "superseded_by")
            .map(|x| x["status"].as_str().unwrap()),
        Some("live")
    );
    let rows_new_whole = related(&r, &new_whole);
    assert_eq!(
        row_of(&rows_new_whole, &whole, "supersedes")["status"],
        "superseded"
    );
    // the order: relation rank, then decision id
    let order: Vec<(usize, String)> = shape(&rows)
        .into_iter()
        .map(|(d, rel, _)| (usize::from(rel == "same_entity"), d))
        .collect();
    let mut sorted = order.clone();
    sorted.sort();
    assert_eq!(order, sorted);

    // the cap: 20 rows are listed and the rest are counted
    let (_d, mut r) = project();
    let w = world(&mut r);
    let first = decide(&mut r, &w, "Entries are append-only", &[&w.e], &[]);
    let mut others = vec![];
    for n in 0..20 {
        others.push(decide(
            &mut r,
            &w,
            &format!("Entry class {n} is kept"),
            &[&w.e],
            &[],
        ));
    }
    let (rows, omitted) = related_in_scope(&r, &scope(), &first);
    assert_eq!((rows.len(), omitted), (RELATED_CAP, 0), "20 rows fit");
    let extra = decide(&mut r, &w, "Entry class 21 is kept", &[&w.e], &[]);
    let (rows, omitted) = related_in_scope(&r, &scope(), &first);
    assert_eq!(
        (rows.len(), omitted),
        (RELATED_CAP, 1),
        "the 21st is counted"
    );
    // the aggregate: two anchors, each cut to 20 and each counting its own drop
    let (rows, omitted) = r
        .read()
        .related_decisions(
            &scope(),
            &[
                BlockId::parse(&first).unwrap(),
                BlockId::parse(&extra).unwrap(),
            ],
        )
        .unwrap();
    assert_eq!((rows.len(), omitted), (2 * RELATED_CAP, 2));
    let _ = others;
}

#[test]
fn nb6_everyday_related_text_requires_links() {
    let (_d, mut r) = project();
    let w = world(&mut r);
    let x = decide(&mut r, &w, "Entries are append-only", &[&w.e], &[]);
    let linked = decide(&mut r, &w, "Entries are signed", &[&w.e], &[]);
    let text_only = decide(&mut r, &w, "The ledger service is replicated", &[], &[]);
    let other_text = decide(&mut r, &w, "Ledger services are audited", &[], &[]);
    let want = pairs(&[
        (&linked, "link"),
        (&text_only, "text"),
        (&other_text, "text"),
    ]);
    assert_eq!(same_entity(&related(&r, &x)), want);

    write(
        &mut r,
        VIEW,
        json!([{"op": "update", "id": w.e, "fields": {"everyday": true}}]),
    )
    .unwrap();
    // an unlinked end gives no row, whichever end it is
    assert_eq!(
        same_entity(&related(&r, &x)),
        pairs(&[(&linked, "link")]),
        "both linked remain; the text-only decisions drop out"
    );
    assert_eq!(
        same_entity(&related(&r, &linked)),
        pairs(&[(&x, "link")]),
        "the linked decision no longer lists the unlinked ones"
    );
    assert_eq!(
        related(&r, &text_only),
        Vec::<Value>::new(),
        "an unlinked decision lists nothing: its own words do not count"
    );
    assert_eq!(related(&r, &other_text), related(&r, &text_only));
    // the flag is data: false brings the text rows back
    write(
        &mut r,
        VIEW,
        json!([{"op": "update", "id": w.e, "fields": {"everyday": false}}]),
    )
    .unwrap();
    assert_eq!(same_entity(&related(&r, &x)), want);
}

#[test]
fn nb6_related_candidate_index_is_complete_and_bounded() {
    assert_eq!(RELATED_TEXT_CANDIDATES, 100_000);
    assert_eq!(RELATED_CANDIDATE_VISITS, 1_000_000);
    let (_d, mut r) = project();
    let w = world(&mut r);
    let anchor = decide(&mut r, &w, "Entries are append-only", &[&w.e], &[]);
    // every plural and case form of the last word finds its decision
    let forms = [
        "The ledger service is replicated",
        "Ledger Services are replicated",
        "LEDGER SERVICES, again",
        "A ledger service.",
    ];
    let hits: Vec<String> = forms
        .iter()
        .map(|f| decide(&mut r, &w, f, &[], &[]))
        .collect();
    // 150 decisions whose words never complete the name: some hold "ledger", some "service"
    let mut ops = vec![];
    for n in 0..150 {
        let text = match n % 3 {
            0 => format!("Ledger entry {n} is kept"),
            1 => format!("Service call {n} is logged"),
            _ => format!("Plain rule {n} holds"),
        };
        ops.push(raw_decision(
            &format!("$d{n}"),
            &w.s,
            &text,
            "proposed",
            json!({}),
        ));
    }
    rdy_qa::golden::write_as(&approver(), &mut r, VIEW, json!(ops)).unwrap();
    // the lexicon and its word index exist before the read, as a carried one does
    r.read().lexicon(&scope()).unwrap();
    let before = text_scans();
    let rows = related(&r, &anchor);
    let scanned = text_scans() - before;
    assert_eq!(
        same_entity(&rows).len(),
        hits.len(),
        "every form is found: {rows:#?}"
    );
    assert!(
        scanned <= 1 + 2 * hits.len(),
        "the read scanned the text of {scanned} decisions; the scope holds over 150 more"
    );

    // the limits: exact values pass, the first excess is E_LIMIT, never a partial list
    let ids = [BlockId::parse(&anchor).unwrap()];
    let n = hits.len();
    let limited = |texts: usize, visits: usize| {
        r.read()
            .related_decisions_limited(&scope(), &ids, texts, visits)
    };
    let (rows, omitted) = limited(n, n).unwrap();
    assert_eq!((rows.len(), omitted), (n, 0));
    let e = limited(n - 1, n).unwrap_err();
    assert_eq!(e.code, E_LIMIT);
    assert_eq!(
        e.message,
        format!("related decisions exceed {} text candidates", n - 1)
    );
    assert_eq!(
        e.fix,
        "brief fewer decisions, or set everyday: true on an entity that many decisions name"
    );
    let e = limited(n, n - 1).unwrap_err();
    assert_eq!(e.code, E_LIMIT);
    assert_eq!(
        e.message,
        format!("related decisions exceed {} candidate visits", n - 1)
    );
    assert_eq!(
        e.fix,
        "brief fewer decisions, or set everyday: true on an entity that many decisions name"
    );
    // a call that needs no text candidate stays under any limit
    let lone = decide(&mut r, &w, "Nothing else names it", &[], &[]);
    let (rows, _) = r
        .read()
        .related_decisions_limited(&scope(), &[BlockId::parse(&lone).unwrap()], 0, 0)
        .unwrap();
    assert!(rows.is_empty());
}
