//! plx4: rdy_overview and rdy_read agree on which folders and docs a landing scope holds
//! (D-KQ4CA: the chat path is primary; p-283QD: a snapshot maps block ids to versions at one
//! landing; tb-79ZXY: `landing:<hash>` reads). Five landings give membership before creation,
//! after creation, after a rename, after a deletion and after a name is reused by a new id.
//! The expected membership is written down as each landing is made; it is never read back from
//! `tree` or `rdy_read`.
use rdy_engine::engine::Local;
use rdy_engine::{Rdy, ReadState, Scope, TreeNode};
use rdy_mcp::tools::*;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

/// A tree row: id, type, name, path.
type Row = (String, String, String, String);

/// One folder or doc a landing holds.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Member {
    id: String,
    kind: &'static str,
    name: String,
    path: String,
    /// The text a markdown or YAML read must show: a folder's name, a doc's title.
    text: String,
}

struct Fixture {
    /// The landing hash of L1..L5.
    landings: Vec<String>,
    /// What each landing holds, written when the landing was made.
    expected: Vec<Vec<Member>>,
    /// Every folder and doc id of the fixture.
    all_ids: Vec<String>,
}

impl Fixture {
    fn scope(&self, i: usize) -> String {
        format!("landing:{}", self.landings[i])
    }

    fn absent(&self, i: usize) -> Vec<String> {
        self.all_ids
            .iter()
            .filter(|id| !self.expected[i].iter().any(|m| &m.id == *id))
            .cloned()
            .collect()
    }
}

fn member(id: &str, kind: &'static str, name: &str, path: &str, text: &str) -> Member {
    Member {
        id: id.into(),
        kind,
        name: name.into(),
        path: path.into(),
        text: text.into(),
    }
}

/// Builds L1..L5 in `r`. `after_landing` sees the project after each landing is made.
fn build(r: &mut Rdy, mut after_landing: impl FnMut(&Rdy, usize)) -> Fixture {
    let mut landings = vec![];
    let mut expected: Vec<Vec<Member>> = vec![];
    let mut all_ids: Vec<String> = vec![];

    // L1: folder `early` with one doc
    let res = write(
        r,
        "v1",
        json!([
            {"op": "create", "type": "folder", "id": "$e", "fields": {"name": "early"}},
            {"op": "create", "type": "doc", "id": "$d1", "parent": "$e", "fields": {"title": "Alpha", "doc_type": "page"}}
        ]),
    )
    .unwrap();
    let (e, d1) = (res.created["$e"].clone(), res.created["$d1"].clone());
    let d1_name = res.names[&d1].clone();
    all_ids.extend([e.clone(), d1.clone()]);
    let l1 = vec![
        member(&e, "folder", "early", "early", "early"),
        member(&d1, "doc", &d1_name, &format!("early/{d1_name}"), "Alpha"),
    ];
    landings.push(land(r, "v1").unwrap());
    expected.push(l1.clone());
    after_landing(r, 0);

    // L2: folder `later` with its doc
    let res = write(
        r,
        "v2",
        json!([
            {"op": "create", "type": "folder", "id": "$a", "fields": {"name": "later"}},
            {"op": "create", "type": "doc", "id": "$d2", "parent": "$a", "fields": {"title": "Beta", "doc_type": "page"}}
        ]),
    )
    .unwrap();
    let (a, d2) = (res.created["$a"].clone(), res.created["$d2"].clone());
    let d2_name = res.names[&d2].clone();
    all_ids.extend([a.clone(), d2.clone()]);
    let mut l2 = l1.clone();
    l2.push(member(&a, "folder", "later", "later", "later"));
    l2.push(member(
        &d2,
        "doc",
        &d2_name,
        &format!("later/{d2_name}"),
        "Beta",
    ));
    landings.push(land(r, "v2").unwrap());
    expected.push(l2.clone());
    after_landing(r, 1);

    // L3: `later` is renamed `renamed`
    write(
        r,
        "v3",
        json!([{"op": "update", "id": a, "fields": {"name": "renamed"}}]),
    )
    .unwrap();
    let mut l3 = l1.clone();
    l3.push(member(&a, "folder", "renamed", "renamed", "renamed"));
    l3.push(member(
        &d2,
        "doc",
        &d2_name,
        &format!("renamed/{d2_name}"),
        "Beta",
    ));
    landings.push(land(r, "v3").unwrap());
    expected.push(l3);
    after_landing(r, 2);

    // L4: the doc moves into `early`; the empty folder is deleted
    write(
        r,
        "v4",
        json!([
            {"op": "move", "id": d2, "parent": e},
            {"op": "delete", "id": a}
        ]),
    )
    .unwrap();
    let mut l4 = l1.clone();
    l4.push(member(
        &d2,
        "doc",
        &d2_name,
        &format!("early/{d2_name}"),
        "Beta",
    ));
    landings.push(land(r, "v4").unwrap());
    expected.push(l4.clone());
    after_landing(r, 3);

    // L5: a new folder named `later`, a different id, with a doc
    let res = write(
        r,
        "v5",
        json!([
            {"op": "create", "type": "folder", "id": "$b", "fields": {"name": "later"}},
            {"op": "create", "type": "doc", "id": "$d3", "parent": "$b", "fields": {"title": "Gamma", "doc_type": "page"}}
        ]),
    )
    .unwrap();
    let (b, d3) = (res.created["$b"].clone(), res.created["$d3"].clone());
    let d3_name = res.names[&d3].clone();
    all_ids.extend([b.clone(), d3.clone()]);
    assert_ne!(a, b, "the recreated name is a different block");
    let mut l5 = l4.clone();
    l5.push(member(&b, "folder", "later", "later", "later"));
    l5.push(member(
        &d3,
        "doc",
        &d3_name,
        &format!("later/{d3_name}"),
        "Gamma",
    ));
    landings.push(land(r, "v5").unwrap());
    expected.push(l5);
    after_landing(r, 4);

    for m in &mut expected {
        m.sort();
    }
    Fixture {
        landings,
        expected,
        all_ids,
    }
}

fn open_state(dir: &Path) -> McpState {
    let mut st = McpState::new(Arc::new(Local::open(dir).unwrap()));
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:qa", "role": "reader", "view": "canon"}))
            .unwrap(),
    )
    .unwrap();
    st
}

fn flatten(nodes: &[Value], out: &mut Vec<Row>) {
    for n in nodes {
        assert!(n.get("collapsed").is_none(), "a node was collapsed: {n}");
        out.push((
            n["id"].as_str().unwrap().into(),
            n["type"].as_str().unwrap().into(),
            n["name"].as_str().unwrap().into(),
            n["path"].as_str().unwrap().into(),
        ));
        flatten(n["children"].as_array().map_or(&[][..], |c| &c[..]), out);
    }
}

fn expected_rows(want: &[Member]) -> Vec<Row> {
    let mut rows: Vec<_> = want
        .iter()
        .map(|m| {
            (
                m.id.clone(),
                m.kind.to_string(),
                m.name.clone(),
                m.path.clone(),
            )
        })
        .collect();
    rows.sort();
    rows
}

fn read_in(st: &mut McpState, args: Value) -> Result<Value, rdy_engine::RdyError> {
    rdy_read(st, serde_json::from_value(args).unwrap())
}

/// Warms canon: the overview and a read of the current tree, as a session that has worked.
fn warm_canon(st: &mut McpState) {
    let o = rdy_overview(st, ScopeParams::default()).unwrap();
    let mut rows = vec![];
    flatten(o["tree"].as_array().unwrap(), &mut rows);
    for (id, ..) in rows {
        read_in(st, json!({"id": id, "format": "markdown"})).unwrap();
    }
}

/// The whole check of landing `i` through one MCP session.
fn check_landing(st: &mut McpState, fx: &Fixture, i: usize) {
    let scope = fx.scope(i);
    let want = &fx.expected[i];
    let o = rdy_overview(
        st,
        serde_json::from_value(json!({"scope": scope, "max_tokens": 20000})).unwrap(),
    )
    .unwrap_or_else(|e| panic!("overview at L{}: {e}", i + 1));
    let mut got = vec![];
    flatten(o["tree"].as_array().unwrap(), &mut got);
    got.sort();
    assert_eq!(
        got,
        expected_rows(want),
        "overview tree at L{} ({scope}) differs from the membership recorded at its creation",
        i + 1
    );
    for m in want {
        for format in ["compact", "markdown", "yaml"] {
            let v = read_in(
                st,
                json!({"id": m.id, "scope": scope, "format": format, "max_tokens": 20000}),
            )
            .unwrap_or_else(|e| panic!("read {} ({format}) at L{} is refused: {e}", m.id, i + 1));
            let text = v["text"].as_str().unwrap();
            assert!(
                text.contains(&m.id),
                "{format} read of {} at L{}: {text}",
                m.id,
                i + 1
            );
            assert!(
                text.contains(&m.text),
                "{format} read of {} at L{} lacks {:?}: {text}",
                m.id,
                i + 1,
                m.text
            );
            if format == "compact" {
                // the requested block's own line carries its recorded title or name
                let first = text.lines().next().unwrap();
                assert!(
                    first.starts_with(&format!("{} ", m.id)) && first.contains(&m.text),
                    "compact read of {} at L{} has first line {first:?}, not one with {:?}",
                    m.id,
                    i + 1,
                    m.text
                );
            }
        }
        // a path read resolves to this landing's id, not to a later or earlier block of that name
        let v = read_in(
            st,
            json!({"id": m.path, "scope": scope, "format": "compact", "max_tokens": 20000}),
        )
        .unwrap_or_else(|e| panic!("read path {:?} at L{} is refused: {e}", m.path, i + 1));
        let first = v["text"].as_str().unwrap().lines().next().unwrap();
        assert!(
            first.starts_with(&format!("{} ", m.id)),
            "path {:?} at L{} reads {first:?}, not {}",
            m.path,
            i + 1,
            m.id
        );
    }
    for id in fx.absent(i) {
        for format in ["compact", "markdown", "yaml"] {
            let e = read_in(st, json!({"id": id, "scope": scope, "format": format}))
                .expect_err(&format!("{id} is not in L{} but {format} read it", i + 1));
            assert_eq!(e.code, "E_UNKNOWN_BLOCK", "{id} at L{}: {e}", i + 1);
            assert_eq!(e.message, format!("no block {id}"), "{id} at L{}", i + 1);
            assert_eq!(e.fix, "list docs with rdy_overview", "{id} at L{}", i + 1);
        }
        assert!(
            !got_ids(&o).contains(&id),
            "the overview at L{} lists {id}",
            i + 1
        );
    }
}

fn got_ids(o: &Value) -> Vec<String> {
    let mut rows = vec![];
    flatten(o["tree"].as_array().unwrap(), &mut rows);
    rows.into_iter().map(|r| r.0).collect()
}

fn run(st: &mut McpState, fx: &Fixture, order: &[usize]) {
    warm_canon(st);
    for &i in order {
        check_landing(st, fx, i);
    }
}

/// The engine's own answer for landing `i`: its tree rows and its folder and doc ids.
fn engine_rows(rs: &ReadState, fx: &Fixture, i: usize) -> (Vec<Row>, Vec<String>) {
    let scope = rs.resolve_scope(&fx.scope(i)).unwrap();
    assert!(matches!(scope, Scope::At(_)));
    fn walk(ns: &[TreeNode], out: &mut Vec<Row>) {
        for n in ns {
            assert_eq!(n.collapsed, 0);
            out.push((
                n.id.clone(),
                n.block_type.clone(),
                n.name.clone(),
                n.path.clone(),
            ));
            walk(&n.children, out);
        }
    }
    let mut rows = vec![];
    walk(&rs.tree(&scope, None, None).unwrap(), &mut rows);
    rows.sort();
    let m = rs.materialized(&scope).unwrap();
    let mut ids: Vec<String> = m
        .ids()
        .filter(|id| {
            m.get(id)
                .is_some_and(|b| b.block_type == "folder" || b.block_type == "doc")
        })
        .map(|id| id.to_string())
        .collect();
    ids.sort();
    (rows, ids)
}

fn check_engine(rs: &ReadState, fx: &Fixture, i: usize, who: &str) {
    let (rows, ids) = engine_rows(rs, fx, i);
    assert_eq!(
        rows,
        expected_rows(&fx.expected[i]),
        "{who}: tree at L{}",
        i + 1
    );
    let mut want: Vec<String> = fx.expected[i].iter().map(|m| m.id.clone()).collect();
    want.sort();
    assert_eq!(ids, want, "{who}: materialized at L{}", i + 1);
}

#[test]
fn plx4_overview_and_read_agree_at_every_landing() {
    let (d, mut r) = project();
    let fx = build(&mut r, |_, _| {});
    assert_eq!(fx.landings.len(), 5);
    let distinct: std::collections::BTreeSet<_> = fx.landings.iter().collect();
    assert_eq!(distinct.len(), 5, "five different landings");
    r.close().unwrap();
    let mut st = open_state(d.path());
    for i in 0..5 {
        check_landing(&mut st, &fx, i);
    }
    // the name `later` belongs to two different ids at two landings
    let ids_named_later: BTreeMap<usize, String> = (0..5)
        .filter_map(|i| {
            fx.expected[i]
                .iter()
                .find(|m| m.path == "later")
                .map(|m| (i, m.id.clone()))
        })
        .collect();
    assert_eq!(ids_named_later.len(), 2, "{ids_named_later:?}");
    assert_ne!(ids_named_later[&1], ids_named_later[&4]);
}

#[test]
fn plx4_scope_membership_survives_cache_order_and_reopen() {
    let (d, mut r) = project();
    let mut at_l2: Option<Arc<ReadState>> = None;
    let fx = build(&mut r, |r, i| {
        if i == 1 {
            at_l2 = Some(r.read());
        }
    });
    let at_l2 = at_l2.expect("the state at L2 was captured");
    // the state captured at L2 holds L1 and L2; a fresh state after L5 holds all five
    let fresh = r.read();
    for i in 0..2 {
        check_engine(&at_l2, &fx, i, "state captured at L2");
        check_engine(&fresh, &fx, i, "fresh state after L5");
    }
    for i in 0..5 {
        check_engine(&fresh, &fx, i, "fresh state after L5");
    }
    drop((at_l2, fresh));
    r.close().unwrap();

    let orders: [&[usize]; 3] = [&[0, 1, 2, 3, 4], &[4, 3, 2, 1, 0], &[2, 0, 4, 1, 3]];
    // each order on a session of its own, then all three one after another on one session
    for order in orders {
        let mut st = open_state(d.path());
        run(&mut st, &fx, order);
    }
    {
        let mut st = open_state(d.path());
        for order in orders {
            run(&mut st, &fx, order);
        }
    }
    // close and open again, then a reindex, then the matrix on the rebuilt index
    let r = Rdy::open(d.path()).unwrap();
    r.close().unwrap();
    for order in orders {
        let mut st = open_state(d.path());
        run(&mut st, &fx, order);
    }
    let mut r = Rdy::open(d.path()).unwrap();
    r.reindex().unwrap();
    r.close().unwrap();
    for order in orders {
        let mut st = open_state(d.path());
        run(&mut st, &fx, order);
    }
    let r = Rdy::open(d.path()).unwrap();
    let fresh = r.read();
    for i in 0..5 {
        check_engine(&fresh, &fx, i, "state after reindex");
    }
}
