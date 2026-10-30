//! dq5: what a filter means for one block (D-HZYK0), over inputs the caller supplies (D-JQD5X):
//! each field and op, the missing member, multi-valued `!=`, link tests and their evaluation
//! bound, a candidate that is not stored yet, and the scope adapter's coverage refusal.
use rdy_engine::coverage::rollup;
use rdy_engine::filter::{
    FILTER_FIX, FilterContext, FilterInputs, candidate_for_tests, coverage_scores, filter_error,
};
use rdy_engine::{DecisionInput, E_BAD_INPUT, Materialized, Rdy, ReadState, Scope, now_ms};
use rdy_graph::Graph;
use rdy_model::{BlockId, FieldValue, Hash, Op};
use rdy_qa::fixtures::*;
use rdy_qa::packs::use_pack;
use rdy_qa::tensions::{change_of, put, upd};
use rdy_schema::filter::parse;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const PACK: &str = "\
name: filter-qa
version: \"1\"
extends: rdy-standard
areas:
  a:
    paths: [one/**]
  b:
    paths: [one/**]
  c:
    paths: [elsewhere/**]
block_types:
  claim:
    prefix: c
    traits: [normative, describes]
    body: required
    fields:
      label: { kind: str }
      n: { kind: int }
      tags: { kind: list }
      flag: { kind: bool }
      meta: { kind: map }
  plain:
    prefix: pl
    body: required
    fields:
      n: { kind: str }
  section:
    prefix: s
    traits: [structural]
    children: [section, prose, claim, requirement, constraint, term, component, actor, interface, value, entity, flow, example, note, rationale, table, diagram, figure, code, decision, question, plain]
";

fn work() -> Scope {
    Scope::View("work".into())
}

fn pack_project() -> (tempfile::TempDir, Rdy) {
    let (d, r) = project();
    drop(r);
    use_pack(d.path(), "filter-qa", PACK, "");
    let r = Rdy::open(d.path()).unwrap();
    (d, r)
}

/// The world's blocks by name.
struct World {
    ids: BTreeMap<&'static str, BlockId>,
}

impl World {
    fn id(&self, name: &str) -> &BlockId {
        self.ids
            .get(name)
            .unwrap_or_else(|| panic!("no block {name}"))
    }
    fn set(&self, names: &[&str]) -> BTreeSet<BlockId> {
        names.iter().map(|n| self.id(n).clone()).collect()
    }
    /// The block's name, for a failure message.
    fn names(&self, set: &BTreeSet<BlockId>) -> Vec<String> {
        set.iter()
            .map(|id| {
                self.ids
                    .iter()
                    .find(|(_, v)| *v == id)
                    .map_or_else(|| id.to_string(), |(n, _)| (*n).to_string())
            })
            .collect()
    }
}

/// Folder `one` holds doc `first` (in areas a and b): section `$s` (draft) with claims `$alpha`
/// (key FW-X1), `$beta` (key K2024) and `$gamma`, a block `$plain` of a type with no trait, a
/// prose block, an entity `$hub` two claims describe, and decisions, one `$live`, one `$old`
/// that `$new` superseded, one `$prop` only proposed. Section `$dep` (deprecated) holds `$deep`.
/// Doc `guides` (no area) holds `$cite`, which references `$beta`, and `$g`, which implements
/// `$live`. `work` changes `$beta`, so `$cite` is suspect there. Two agents edit `$gamma`, so it
/// is in an open tension.
fn world() -> (tempfile::TempDir, Rdy, World) {
    let (d, mut r) = pack_project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "folder", "id": "$one", "fields": {"name": "one", "title": "One"}},
        {"op": "create", "type": "doc", "id": "$first", "parent": "$one", "fields": {"name": "first", "title": "First", "doc_type": "page", "key": "FIRST-DOC"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$first", "fields": {"title": "S", "lifecycle": "draft"}},
        {"op": "create", "type": "entity", "id": "$hub", "parent": "$s", "fields": {"name": "Hub"}},
        {"op": "create", "type": "claim", "id": "$alpha", "parent": "$s", "body": "Alpha holds.",
         "fields": {"key": "FW-X1", "label": "alpha one", "n": 5, "tags": ["x", "yes"], "flag": true, "meta": {"a": 1}},
         "links": {"describes": [{"target": "$hub"}]}},
        {"op": "create", "type": "claim", "id": "$beta", "parent": "$s", "body": "Beta holds.",
         "fields": {"key": "K2024", "label": "Beta", "n": i64::MAX, "tags": [], "flag": false},
         "links": {"describes": [{"target": "$hub"}]}},
        {"op": "create", "type": "claim", "id": "$gamma", "parent": "$s", "body": "Gamma holds."},
        {"op": "create", "type": "plain", "id": "$plain", "parent": "$s", "body": "Plain text.", "fields": {"n": "seven"}},
        {"op": "create", "type": "prose", "id": "$prose", "parent": "$s", "body": "Prose text."},
        {"op": "create", "type": "section", "id": "$dep", "parent": "$first", "fields": {"title": "Old", "lifecycle": "deprecated"}},
        {"op": "create", "type": "claim", "id": "$deep", "parent": "$dep", "body": "Deep holds."},
        {"op": "create", "type": "doc", "id": "$guides", "fields": {"name": "guides", "title": "Guides", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$gs", "parent": "$guides", "fields": {"title": "G"}},
        {"op": "create", "type": "claim", "id": "$cite", "parent": "$gs", "body": "Per {{$beta}}."},
        {"op": "create", "type": "prose", "id": "$g", "parent": "$gs", "body": "Guide text."}
    ]))
    .unwrap();
    let s = w.created["$s"].clone();
    let decide = |r: &mut Rdy, statement: &str| {
        r.decide(
            &writer(),
            "work",
            DecisionInput {
                parent: s.clone(),
                statement: statement.into(),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
            .clone()
    };
    let live = decide(&mut r, "Keep the hub");
    let old = decide(&mut r, "Wallets move");
    land(&mut r, "work").unwrap();
    let new = r
        .supersede(
            &writer(),
            "next",
            &old,
            None,
            DecisionInput {
                parent: s.clone(),
                statement: "Wallets stay".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    land(&mut r, "next").unwrap();
    let prop = decide(&mut r, "Maybe later");
    write(
        &mut r,
        "work",
        json!([
            {"op": "update", "id": w.created["$beta"], "body": "Beta changed."},
            {"op": "update", "id": w.created["$g"], "links": {"implements": [{"target": live}]}}
        ]),
    )
    .unwrap();
    let gamma = BlockId::parse(&w.created["$gamma"]).unwrap();
    put(&mut r, "ag-a", upd(&gamma, "Gamma says one."));
    put(&mut r, "ag-b", upd(&gamma, "Gamma says two."));

    let mut ids: BTreeMap<&'static str, BlockId> = BTreeMap::new();
    for name in [
        "one", "first", "s", "hub", "alpha", "beta", "gamma", "plain", "prose", "dep", "deep",
        "guides", "gs", "cite", "g",
    ] {
        ids.insert(
            name,
            BlockId::parse(&w.created[&format!("${name}")]).unwrap(),
        );
    }
    for (name, id) in [
        ("live", &live),
        ("old", &old),
        ("new", &new),
        ("prop", &prop),
    ] {
        ids.insert(name, BlockId::parse(id).unwrap());
    }
    (d, r, World { ids })
}

fn query_ids(r: &Rdy, scope: &Scope, filter: &str) -> BTreeSet<BlockId> {
    r.read()
        .query(scope, filter)
        .unwrap_or_else(|e| panic!("{filter}: {e:?}"))
        .matches
        .iter()
        .map(|m| BlockId::parse(&m.id).unwrap())
        .collect()
}

#[track_caller]
fn assert_finds(r: &Rdy, w: &World, filter: &str, names: &[&str]) {
    let found = query_ids(r, &work(), filter);
    assert_eq!(w.names(&found), w.names(&w.set(names)), "filter: {filter}");
}

/// The sets that a filter needs, empty, for evaluating a state directly.
struct Empty {
    suspects: BTreeSet<BlockId>,
    tensions: BTreeSet<BlockId>,
    coverage: BTreeMap<BlockId, u32>,
}

fn empty() -> Empty {
    Empty {
        suspects: BTreeSet::new(),
        tensions: BTreeSet::new(),
        coverage: BTreeMap::new(),
    }
}

fn inputs<'a>(
    st: &'a ReadState,
    m: &'a Materialized,
    graph: &'a Graph,
    sets: &'a Empty,
) -> FilterInputs<'a> {
    FilterInputs {
        pack: st.pack(),
        m,
        graph,
        areas: st.area_matcher(),
        suspects: &sets.suspects,
        tensions: &sets.tensions,
        coverage: &sets.coverage,
    }
}

#[test]
fn dq5_each_field_takes_its_ops() {
    let (_d, r, w) = world();
    let alpha = w.id("alpha").to_string();
    let first = w.id("first").to_string();
    let rows: Vec<(String, Vec<&str>)> = vec![
        // id
        (format!("id = {alpha}"), vec!["alpha"]),
        (format!("type = claim and id != {alpha} and key ^= K"), vec!["beta"]),
        ("type = plain and id ^= pl-".into(), vec!["plain"]),
        // key
        ("key = FW-X1".into(), vec!["alpha"]),
        ("type = claim and key != FW-X1".into(), vec!["beta"]),
        ("type = claim and key ^= FW".into(), vec!["alpha"]),
        // type
        ("type = entity".into(), vec!["hub"]),
        ("type != claim and type != doc and type != folder and type != section and type != decision and type != prose".into(), vec!["hub", "plain"]),
        // trait
        ("trait = defines".into(), vec!["hub"]),
        ("trait != normative and type = plain".into(), vec![]),
        ("trait != normative and type = entity".into(), vec!["hub"]),
        // path
        ("path = guides and type = claim".into(), vec!["cite"]),
        ("path != guides and type = claim".into(), vec!["alpha", "beta", "gamma", "deep"]),
        ("path ^= one/ and type = plain".into(), vec!["plain"]),
        // doc
        ("doc = guides and type = prose".into(), vec!["g"]),
        ("doc != guides and type = plain".into(), vec!["plain"]),
        // area
        ("area = a and type = plain".into(), vec!["plain"]),
        ("area != a and type = plain".into(), vec![]),
        // lifecycle
        ("lifecycle = deprecated".into(), vec!["dep", "deep"]),
        ("lifecycle != draft and type = claim".into(), vec!["deep"]),
        // status
        ("status = live".into(), vec!["live", "new"]),
        ("status = superseded".into(), vec!["old"]),
        ("status = proposed".into(), vec!["prop"]),
        ("status != live".into(), vec!["old", "prop"]),
        // suspect
        ("suspect = true".into(), vec!["cite"]),
        ("suspect != false".into(), vec!["cite"]),
        ("suspect = false and type = plain".into(), vec!["plain"]),
        ("suspect != true and type = plain".into(), vec!["plain"]),
        // tension
        ("tension = true".into(), vec!["gamma"]),
        ("tension != false".into(), vec!["gamma"]),
        ("tension = false and type = plain".into(), vec!["plain"]),
        ("tension != true and type = plain".into(), vec!["plain"]),
        // fields: a string
        ("fields.label = \"alpha one\"".into(), vec!["alpha"]),
        ("fields.label != \"alpha one\"".into(), vec!["beta"]),
        ("fields.label ^= alpha".into(), vec!["alpha"]),
        // fields: a number
        ("fields.n = 5".into(), vec!["alpha"]),
        ("fields.n != 5".into(), vec!["beta"]),
        ("fields.n < 6".into(), vec!["alpha"]),
        ("fields.n <= 5".into(), vec!["alpha"]),
        ("fields.n > 5".into(), vec!["beta"]),
        ("fields.n >= 5".into(), vec!["alpha", "beta"]),
        // fields: a boolean
        ("fields.flag = true".into(), vec!["alpha"]),
        ("fields.flag != true".into(), vec!["beta"]),
        // fields: a list
        ("fields.tags = x".into(), vec!["alpha"]),
        ("fields.tags ^= ye".into(), vec!["alpha"]),
        ("fields.tags != z".into(), vec!["alpha", "beta"]),
        // fields: a map holds for no test
        ("fields.meta = a".into(), vec![]),
        ("fields.meta != a".into(), vec![]),
        // the doc by id stays in the row set above by its other spellings
        (format!("doc = {first} and type = plain"), vec!["plain"]),
    ];
    for (filter, names) in rows {
        assert_finds(&r, &w, &filter, &names);
    }
}

#[test]
fn dq5_a_test_of_a_missing_member_is_false_for_every_op() {
    let (_d, r, w) = world();
    // not a decision
    assert!(!query_ids(&r, &work(), "status != live").contains(w.id("prose")));
    assert!(!query_ids(&r, &work(), "status = live").contains(w.id("prose")));
    // `not` of the test selects it
    assert!(query_ids(&r, &work(), "not status = live").contains(w.id("prose")));
    // no key
    assert!(!query_ids(&r, &work(), "key = x").contains(w.id("prose")));
    assert!(!query_ids(&r, &work(), "key != x").contains(w.id("prose")));
    assert!(!query_ids(&r, &work(), "key ^= x").contains(w.id("prose")));
    // a string field against a number comparison
    assert!(!query_ids(&r, &work(), "fields.n > 1").contains(w.id("plain")));
    assert!(!query_ids(&r, &work(), "fields.n != 1").contains(w.id("plain")));
    // a number field against a string
    assert!(!query_ids(&r, &work(), "fields.n != seven").contains(w.id("alpha")));
    // no such field
    assert!(!query_ids(&r, &work(), "fields.none != 1").contains(w.id("alpha")));
    // no lifecycle above it, no doc, no path, no area
    assert!(!query_ids(&r, &work(), "lifecycle != draft").contains(w.id("cite")));
    assert!(!query_ids(&r, &work(), "doc != guides").contains(w.id("one")));
    assert!(!query_ids(&r, &work(), "area != a").contains(w.id("cite")));
}

#[test]
fn dq5_key_and_words_match_in_any_case_and_values_keep_theirs() {
    let (_d, r, w) = world();
    assert_finds(&r, &w, "key = fw-x1", &["alpha"]);
    assert_finds(&r, &w, "KEY = FW-x1", &["alpha"]);
    assert_finds(&r, &w, "key ^= fw-", &["alpha"]);
    assert_finds(&r, &w, "type = claim and key != fw-x1", &["beta"]);
    assert_finds(&r, &w, "path = Guides", &[]);
    assert_finds(&r, &w, "path = guides and TYPE = prose", &["g"]);
    // a value keeps its case, so a pack type in another case is no pack type
    let e = r.read().query(&work(), "type = PROSE").unwrap_err();
    assert_eq!(e.message, "filter: no block type PROSE at column 8");
    assert_finds(&r, &w, "fields.label = beta", &[]);
    assert_finds(&r, &w, "fields.label = Beta", &["beta"]);
}

#[test]
fn dq5_a_number_against_a_string_member_compares_as_text() {
    let (_d, mut r, w) = world();
    // A write refuses a key that starts with a digit (`rdy_model::key`), so the block whose key is
    // `2024` is a supplied state: the evaluator's answer depends only on its inputs.
    let st = r.read();
    let m = st.materialized(&work()).unwrap();
    let mut beta = m.get(w.id("beta")).unwrap().clone();
    beta.fields
        .insert("key".into(), FieldValue::Str("2024".into()));
    let base = m.version_of(w.id("beta")).unwrap();
    let numbered = m.with(&[Op::Update { base, block: beta }]);
    let graph = Graph::build_from(numbered.iter());
    let sets = empty();
    let ctx = FilterContext::from_parts(inputs(&st, &numbered, &graph, &sets));
    let holds = |text: &str, name: &str| ctx.matches(&parse(text, st.pack()).unwrap(), w.id(name));
    assert!(holds("key = 2024", "beta"));
    assert!(holds("key ^= 20", "beta"));
    assert!(!holds("key != 2024", "beta"));
    assert!(!holds("key = 2024", "alpha"));
    assert!(holds("key != 2024", "alpha"));
    drop(ctx);

    // in a stored project: a number finds no key and refuses nothing
    assert_finds(&r, &w, "key = 2024", &[]);
    assert_finds(&r, &w, "type = claim and key != 2024", &["alpha", "beta"]);
    // an id never starts with a digit: the test compares text, finds nothing and refuses nothing
    assert_finds(&r, &w, "id ^= 7", &[]);
    assert_finds(&r, &w, "type = plain and id != 7", &["plain"]);
    assert_finds(&r, &w, "path = true", &[]);

    // a path and a doc name a number or a boolean as text; a key names a boolean as text
    let made = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$year", "fields": {"name": "2024", "title": "Year", "doc_type": "page"}},
        {"op": "create", "type": "prose", "id": "$p", "parent": "$year", "body": "A year."},
        {"op": "create", "type": "claim", "id": "$t", "parent": w.id("s").to_string(), "body": "True.", "fields": {"key": "true"}}
    ]))
    .unwrap();
    let p = BlockId::parse(&made.created["$p"]).unwrap();
    let t = BlockId::parse(&made.created["$t"]).unwrap();
    assert!(query_ids(&r, &work(), "path = 2024").contains(&p));
    assert!(query_ids(&r, &work(), "path ^= 20").contains(&p));
    assert!(query_ids(&r, &work(), "doc = 2024").contains(&p));
    assert!(query_ids(&r, &work(), "key = true").contains(&t));
    assert!(query_ids(&r, &work(), "key = TRUE").contains(&t));
}

#[test]
fn dq5_multi_valued_inequality_needs_no_equal_value() {
    let (_d, r, w) = world();
    // areas: a block in a and b, and a block in no area
    let not_a = query_ids(&r, &work(), "area != a");
    let not_c = query_ids(&r, &work(), "area != c");
    assert!(!not_a.contains(w.id("alpha")));
    assert!(not_c.contains(w.id("alpha")));
    assert!(!not_a.contains(w.id("cite")));
    assert!(!not_c.contains(w.id("cite")));
    // traits: a type with two, and a type with none
    let not_normative = query_ids(&r, &work(), "trait != normative");
    let not_asset = query_ids(&r, &work(), "trait != asset");
    assert!(!not_normative.contains(w.id("alpha")));
    assert!(not_asset.contains(w.id("alpha")));
    assert!(!not_normative.contains(w.id("plain")));
    assert!(!not_asset.contains(w.id("plain")));
    // a present, empty list
    assert!(query_ids(&r, &work(), "fields.tags != x").contains(w.id("beta")));
    assert!(!query_ids(&r, &work(), "fields.tags != x").contains(w.id("alpha")));
    assert!(!query_ids(&r, &work(), "fields.tags = x").contains(w.id("beta")));
}

#[test]
fn dq5_fields_compare_the_i64_range() {
    let (_d, r, w) = world();
    assert_finds(&r, &w, "fields.n = 9223372036854775807", &["beta"]);
    assert_finds(&r, &w, "fields.n > 9223372036854775806", &["beta"]);
    assert_finds(&r, &w, "fields.n < -9223372036854775808", &[]);
    assert_finds(
        &r,
        &w,
        "fields.n >= -9223372036854775808",
        &["alpha", "beta"],
    );
}

#[test]
fn dq5_lifecycle_is_inherited() {
    let (_d, r, w) = world();
    let draft = query_ids(&r, &work(), "lifecycle = draft");
    for name in ["s", "alpha", "plain", "hub", "live"] {
        assert!(draft.contains(w.id(name)), "{name}");
    }
    assert!(
        !draft.contains(w.id("deep")),
        "its nearest ancestor is deprecated"
    );
    assert!(
        !draft.contains(w.id("first")),
        "a parent takes nothing from below"
    );
    assert!(query_ids(&r, &work(), "has lifecycle").contains(w.id("deep")));
    assert!(!query_ids(&r, &work(), "has lifecycle").contains(w.id("cite")));
}

#[test]
fn dq5_area_and_doc_match() {
    let (_d, r, w) = world();
    let area = query_ids(&r, &work(), "area = b");
    for name in ["one", "first", "s", "alpha", "deep"] {
        assert!(area.contains(w.id(name)), "{name}");
    }
    assert!(!area.contains(w.id("guides")));
    assert!(query_ids(&r, &work(), "area = c").is_empty());

    let first = w.id("first").to_string();
    let by_id = query_ids(&r, &work(), &format!("doc = {first}"));
    assert_eq!(
        by_id,
        query_ids(&r, &work(), "doc = first-doc"),
        "by key, any case"
    );
    assert_eq!(by_id, query_ids(&r, &work(), "doc = one/first"), "by path");
    assert!(by_id.contains(w.id("alpha")) && by_id.contains(w.id("first")));
    assert!(!by_id.contains(w.id("one")), "a folder is in no doc");
    assert!(!by_id.contains(w.id("cite")));
    let not_first = query_ids(&r, &work(), &format!("doc != {first}"));
    assert!(not_first.contains(w.id("cite")));
    assert!(!not_first.contains(w.id("alpha")));
}

#[test]
fn dq5_list_fields_match_any_item() {
    let (_d, r, w) = world();
    assert_finds(&r, &w, "fields.tags = x", &["alpha"]);
    assert_finds(&r, &w, "fields.tags = yes", &["alpha"]);
    assert_finds(&r, &w, "fields.tags ^= y", &["alpha"]);
    assert_finds(&r, &w, "fields.tags = y", &[]);
    assert_finds(&r, &w, "fields.tags != yes", &["beta"]);
    assert_finds(&r, &w, "fields.tags < 1", &[]);
}

#[test]
fn dq5_has_holds_for_present_members() {
    let (_d, r, w) = world();
    let has = |f: &str| query_ids(&r, &work(), &format!("has {f}"));
    assert!(has("key").contains(w.id("alpha")));
    assert!(!has("key").contains(w.id("prose")));
    assert!(has("path").contains(w.id("prose")));
    assert!(has("doc").contains(w.id("prose")));
    assert!(!has("doc").contains(w.id("one")));
    assert!(has("area").contains(w.id("prose")));
    assert!(!has("area").contains(w.id("g")));
    assert!(has("status").contains(w.id("live")));
    assert!(!has("status").contains(w.id("prose")));
    assert!(
        has("fields.tags").contains(w.id("beta")),
        "an empty list is present"
    );
    assert!(!has("fields.tags").contains(w.id("gamma")));
    assert!(has("fields.meta").contains(w.id("alpha")));
    let scores = coverage_scores(
        r.read().pack(),
        &*r.read().coverage_issues(&work()).unwrap(),
    );
    assert_eq!(has("coverage"), scores.keys().cloned().collect());
}

#[test]
fn dq5_link_tests_follow_one_hop() {
    let (_d, r, w) = world();
    assert_finds(&r, &w, "out describes", &["alpha", "beta"]);
    assert_finds(&r, &w, "in describes", &["hub"]);
    assert_finds(&r, &w, "in describes (key = FW-X1)", &["hub"]);
    assert_finds(&r, &w, "in describes (type = decision)", &[]);
    assert_finds(&r, &w, "out describes (type = entity)", &["alpha", "beta"]);
    assert_finds(
        &r,
        &w,
        "type = section and out contains (type = plain)",
        &["s"],
    );
    assert_finds(
        &r,
        &w,
        "in contains (key = first-doc) and type = section",
        &["s", "dep"],
    );
    assert_finds(&r, &w, "out contains (id = nothing)", &[]);
    // the folder has no parent
    assert_finds(&r, &w, "type = folder and no in contains", &["one"]);
    assert_finds(&r, &w, "out references", &["cite"]);
    assert_finds(&r, &w, "in references", &["beta"]);
    assert_finds(&r, &w, "in references (type = claim)", &["beta"]);
    assert_finds(&r, &w, "in implements", &["live"]);
    assert_finds(
        &r,
        &w,
        "type = decision and no in implements",
        &["old", "new", "prop"],
    );
    assert_finds(&r, &w, "type = entity and no in describes", &[]);
    assert_finds(
        &r,
        &w,
        "type = entity and no in describes (type = plain)",
        &["hub"],
    );
}

#[test]
fn dq5_three_nested_link_tests_evaluate() {
    let (_d, r, w) = world();
    assert_finds(
        &r,
        &w,
        "in describes (out describes (in describes (key = K2024)))",
        &["hub"],
    );
    assert_finds(
        &r,
        &w,
        "in describes (out describes (in describes (key = none)))",
        &[],
    );
}

#[test]
fn dq5_a_nested_link_query_stays_within_its_evaluation_bound() {
    let (_d, mut r) = project();
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"name": "hubs", "title": "Hubs", "doc_type": "page"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}}),
        json!({"op": "create", "type": "entity", "id": "$hub", "parent": "$s", "fields": {"name": "Hub"}}),
    ];
    ops.extend((0..2_000).map(|i| {
        json!({"op": "create", "type": "claim", "parent": "$s", "body": format!("Claim {i}."),
               "links": {"describes": [{"target": "$hub"}]}})
    }));
    let w = write(&mut r, "work", Value::Array(ops)).unwrap();
    let hub = BlockId::parse(&w.created["$hub"]).unwrap();
    let st = r.read();
    let m = st.materialized(&work()).unwrap();
    let graph = st.graph(&work()).unwrap();
    let blocks: Vec<BlockId> = m.ids().cloned().collect();
    let edges: usize = blocks.iter().map(|id| graph.outgoing(id).len()).sum();
    let sets = empty();
    // (the text, its tests, the blocks it matches); the last never matches, so nothing cuts a walk
    for (text, tests, want) in [
        ("in describes (out describes (in describes))", 3, 1),
        (
            "in describes (out describes (in describes (type = decision)))",
            4,
            0,
        ),
    ] {
        let filter = parse(text, st.pack()).unwrap();
        let ctx = FilterContext::from_parts(inputs(&st, &m, &graph, &sets));
        let found: Vec<&BlockId> = blocks
            .iter()
            .filter(|id| ctx.matches(&filter, id))
            .collect();
        assert_eq!(found.len(), want, "{text}");
        if want == 1 {
            assert_eq!(found, [&hub], "{text}");
        }
        assert!(
            ctx.evaluations() <= tests * (blocks.len() + edges),
            "{text}: {} evaluations over {} blocks and {edges} edges",
            ctx.evaluations(),
            blocks.len()
        );
        assert!(ctx.evaluations() > 0);
    }
}

#[test]
fn dq5_coverage_at_a_past_landing_is_refused() {
    let (_d, mut r) = project();
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "doc", "fields": {"name": "one", "title": "One", "doc_type": "page"}}]),
    )
    .unwrap();
    let first: Hash = land(&mut r, "work").unwrap().parse().unwrap();
    write(
        &mut r,
        "more",
        json!([{"op": "create", "type": "doc", "fields": {"name": "two", "title": "Two", "doc_type": "page"}}]),
    )
    .unwrap();
    land(&mut r, "more").unwrap();
    let st = r.read();
    let past = Scope::At(first);
    for filter in [
        "coverage < 50",
        "has coverage",
        "out contains (coverage > 1)",
    ] {
        let e = st.query(&past, filter).unwrap_err();
        assert_eq!(e.code, E_BAD_INPUT, "{filter}");
        assert_eq!(
            e.message,
            format!("coverage is computed for the current state only, not at landing:{first}"),
            "{filter}"
        );
        assert_eq!(
            e.fix,
            "ask for canon or a view; coverage over time is not part of rdy core"
        );
    }
    // a filter that reads no coverage reads the past landing
    assert_eq!(st.query(&past, "type = doc").unwrap().total, 1);
    // canon's head is canon
    let head = Scope::At(st.canon_head().unwrap());
    st.query(&head, "coverage < 50").unwrap();
}

#[test]
fn dq5_coverage_compares_the_block_score() {
    let (_d, r, w) = world();
    let st = r.read();
    let issues = st.coverage_issues(&work()).unwrap();
    let scores = coverage_scores(st.pack(), &issues);
    for (id, entry) in issues.blocks.iter() {
        assert_eq!(scores[id], rollup(st.pack(), std::iter::once(entry)).score);
    }
    assert_eq!(scores.len(), issues.blocks.len());
    let (alpha, score) = (w.id("alpha"), scores[w.id("alpha")]);
    let one =
        |test: String| query_ids(&r, &work(), &format!("id = {alpha} and {test}")).contains(alpha);
    assert!(one(format!("coverage = {score}")));
    assert!(!one(format!("coverage != {score}")));
    assert!(one(format!("coverage <= {score}")));
    assert!(one(format!("coverage >= {score}")));
    assert_eq!(one("coverage < 100".to_string()), score < 100);
    assert_eq!(one("coverage > 0".to_string()), score > 0);

    // the six comparisons, on supplied scores
    let m = st.materialized(&work()).unwrap();
    let graph = st.graph(&work()).unwrap();
    let sets = Empty {
        coverage: [(alpha.clone(), 40)].into_iter().collect(),
        ..empty()
    };
    let ctx = FilterContext::from_parts(inputs(&st, &m, &graph, &sets));
    for (text, want) in [
        ("coverage = 40", true),
        ("coverage != 40", false),
        ("coverage != 41", true),
        ("coverage < 41", true),
        ("coverage < 40", false),
        ("coverage <= 40", true),
        ("coverage > 39", true),
        ("coverage > 40", false),
        ("coverage >= 40", true),
    ] {
        let f = parse(text, st.pack()).unwrap();
        assert_eq!(ctx.matches(&f, alpha), want, "{text}");
        assert!(
            !ctx.matches(&f, w.id("beta")),
            "{text}: a block with no score holds for no test"
        );
    }
}

#[test]
fn dq5_matches_come_in_reading_order() {
    let (_d, r, w) = world();
    let reply = r.read().query(&work(), "type = claim").unwrap();
    let got: Vec<BlockId> = reply
        .matches
        .iter()
        .map(|m| BlockId::parse(&m.id).unwrap())
        .collect();
    let want: Vec<BlockId> = ["alpha", "beta", "gamma", "deep", "cite"]
        .iter()
        .map(|n| w.id(n).clone())
        .collect();
    assert_eq!(got, want);
    assert_eq!(reply.total, 5);
    assert_eq!(reply.next, None);
    let first = &reply.matches[0];
    assert_eq!(first.block_type, "claim");
    assert_eq!(first.path.as_deref(), Some("one/first"));
    assert_eq!(first.summary, "Alpha holds.");
    let one = r.read().query(&work(), "type = folder").unwrap();
    assert_eq!(one.matches[0].path.as_deref(), Some("one"));
}

#[test]
fn dq5_matches_agrees_with_query_for_every_block() {
    let (_d, r, _w) = world();
    let st = r.read();
    let scope = work();
    let m = st.materialized(&scope).unwrap();
    let graph = st.graph(&scope).unwrap();
    let suspects: BTreeSet<BlockId> = st
        .suspects(&scope)
        .unwrap()
        .into_iter()
        .map(|s| s.block)
        .collect();
    let tensions = st.open_tension_blocks(now_ms()).unwrap();
    let coverage = coverage_scores(st.pack(), &*st.coverage_issues(&scope).unwrap());
    let ctx = FilterContext::from_parts(FilterInputs {
        pack: st.pack(),
        m: &m,
        graph: &graph,
        areas: st.area_matcher(),
        suspects: &suspects,
        tensions: &tensions,
        coverage: &coverage,
    });
    for text in [
        "type = claim",
        "not type = claim",
        "suspect = true or tension = true",
        "coverage >= 0",
        "in describes (key = FW-X1) or status = live",
        "lifecycle = draft and no out describes",
        "area = a and has key",
    ] {
        let filter = parse(text, st.pack()).unwrap();
        let by_query: BTreeSet<BlockId> = st
            .query(&scope, text)
            .unwrap()
            .matches
            .iter()
            .map(|h| BlockId::parse(&h.id).unwrap())
            .collect();
        let by_matches: BTreeSet<BlockId> = m
            .ids()
            .filter(|id| ctx.matches(&filter, id))
            .cloned()
            .collect();
        assert_eq!(by_matches, by_query, "{text}");
        assert!(!by_query.is_empty(), "{text}");
    }
    let f = parse("type = claim", st.pack()).unwrap();
    assert!(
        !ctx.matches(&f, &BlockId::parse("c-ZZZZZ").unwrap()),
        "a block the state lacks matches nothing"
    );
}

/// A landed doc with a section, its id.
fn base() -> (tempfile::TempDir, Rdy, String) {
    let (d, mut r) = project();
    let w = write(&mut r, "base", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "base", "title": "Base", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}}
    ]))
    .unwrap();
    land(&mut r, "base").unwrap();
    (d, r, w.created["$s"].clone())
}

fn add_claim(r: &mut Rdy, view: &str, section: &str, body: &str) -> (Hash, BlockId) {
    let w = write(
        r,
        view,
        json!([{"op": "create", "type": "claim", "id": "$c", "parent": section, "body": body}]),
    )
    .unwrap();
    (change_of(&w), BlockId::parse(&w.created["$c"]).unwrap())
}

#[test]
fn dq5_from_parts_evaluates_a_candidate_before_it_is_stored() {
    let (_d, mut r, s) = base();
    let (change, claim) = add_claim(&mut r, "w", &s, "A new claim.");
    let st = r.read();
    let f = parse("type = claim", st.pack()).unwrap();
    let sets = empty();
    let (m, graph) = candidate_for_tests(&st, &[change]).unwrap();
    let ctx = FilterContext::from_parts(inputs(&st, &m, &graph, &sets));
    assert!(ctx.matches(&f, &claim));

    let canon_m = st.materialized(&Scope::Canon).unwrap();
    let canon_g = st.graph(&Scope::Canon).unwrap();
    let canon = FilterContext::from_parts(inputs(&st, &canon_m, &canon_g, &sets));
    assert!(canon_m.get(&claim).is_none());
    assert!(!canon.matches(&f, &claim));
}

#[test]
fn dq5_a_subset_candidate_leaves_out_another_view_edit() {
    let (_d, mut r, s) = base();
    let (one, first) = add_claim(&mut r, "w", &s, "First claim.");
    let (_two, second) = add_claim(&mut r, "w", &s, "Second claim.");
    let st = r.read();
    let f = parse("type = claim", st.pack()).unwrap();
    let sets = empty();
    let (m, graph) = candidate_for_tests(&st, &[one]).unwrap();
    let ctx = FilterContext::from_parts(inputs(&st, &m, &graph, &sets));
    assert!(ctx.matches(&f, &first));
    assert!(!ctx.matches(&f, &second));
    // the view itself holds both
    let in_view = query_ids(&r, &Scope::View("w".into()), "type = claim");
    assert!(in_view.contains(&first) && in_view.contains(&second));
}

#[test]
fn dq5_the_candidate_reads_canon_after_an_unrelated_landing() {
    let (_d, mut r, s) = base();
    let (mine, my_claim) = add_claim(&mut r, "w", &s, "My claim.");
    let (_, landed) = add_claim(&mut r, "other", &s, "A landed claim.");
    land(&mut r, "other").unwrap();
    let st = r.read();
    let f = parse("type = claim", st.pack()).unwrap();
    let sets = empty();
    let (m, graph) = candidate_for_tests(&st, &[mine]).unwrap();
    let ctx = FilterContext::from_parts(inputs(&st, &m, &graph, &sets));
    assert!(ctx.matches(&f, &my_claim));
    assert!(ctx.matches(&f, &landed));
}

#[test]
fn dq5_a_coverage_rule_pair_evaluates_on_supplied_scores() {
    let (_d, mut r, s) = base();
    let (first, low) = add_claim(&mut r, "w", &s, "Low claim.");
    let (second, high) = add_claim(&mut r, "w", &s, "High claim.");
    let st = r.read();
    let select = parse("type = claim", st.pack()).unwrap();
    let require = parse("coverage >= 50", st.pack()).unwrap();
    assert!(require.needs().coverage);
    let (m, graph) = candidate_for_tests(&st, &[first, second]).unwrap();
    let sets = Empty {
        coverage: [(low.clone(), 40), (high.clone(), 60)]
            .into_iter()
            .collect(),
        ..empty()
    };
    let ctx = FilterContext::from_parts(inputs(&st, &m, &graph, &sets));
    let raises = |id: &BlockId| ctx.matches(&select, id) && !ctx.matches(&require, id);
    assert!(raises(&low));
    assert!(!raises(&high));
}

#[test]
fn dq5_a_parse_error_maps_to_e_bad_input() {
    let (_d, r, _) = base();
    let st = r.read();
    let e = st.query(&Scope::Canon, "typ = decision").unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(
        e.message,
        "filter: unknown field typ; did you mean type? at column 1"
    );
    assert_eq!(e.fix, FILTER_FIX);
    assert_eq!(
        e.fix,
        "read the filter guide in rdy_schema, or run rdy q --help"
    );
    let at = st.query(&Scope::Canon, "type = ").unwrap_err();
    assert_eq!(at.message, "filter: expected a value at column 7");
    let paged = st
        .query_page(&Scope::Canon, "canon", "typ = decision", None, 4_000)
        .unwrap_err();
    assert_eq!(paged.message, e.message);
    let mapped = filter_error(parse("typ = decision", st.pack()).unwrap_err());
    assert_eq!(
        (mapped.code, mapped.fix),
        (E_BAD_INPUT, FILTER_FIX.to_string())
    );
}

#[test]
fn dq5_query_in_inflight_and_in_a_proposal_version() {
    let (_d, mut r, s) = base();
    let (_, claim) = add_claim(&mut r, "w", &s, "A claim in flight.");
    assert!(query_ids(&r, &Scope::Canon, "type = claim").is_empty());
    assert_eq!(
        query_ids(&r, &Scope::Inflight, "type = claim"),
        [claim.clone()].into_iter().collect()
    );
    let p = r.propose(&writer(), "w", "Add a claim", None).unwrap();
    let pref = r.resolve_proposal(&p.id).unwrap().to_string();
    let scope = r
        .read()
        .resolve_read_scope(&format!("proposal:{pref}"), None);
    let scope = scope.unwrap();
    assert!(matches!(scope, Scope::Proposal(_)));
    assert_eq!(
        query_ids(&r, &scope, "type = claim"),
        [claim].into_iter().collect()
    );
    // a proposal version has no coverage
    let e = r.read().query(&scope, "coverage < 50").unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert!(
        e.message
            .starts_with("coverage is computed for canon and views, not for proposal:"),
        "{}",
        e.message
    );
    assert_eq!(e.fix, "ask for the proposal's view, or canon");
}
