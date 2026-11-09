//! rpl12 (repo links, D-DAPR8, D-FK58A, D-8VJAZ): a write cites another project's delivered
//! blocks as `alias:id` or `alias:KEY`, in bodies, string fields and link targets. It stores
//! `{{<repo>:<id>}}` (keeping the `@key` mark of a key typed bare) and a foreign pin of the
//! content the selected alias's current profile selection delivers. An update keeps the pin a
//! citation already holds; repin, plan repin and reaffirm never move a foreign pin. One held-pin
//! check refuses a pin whose version no delivery holds, with `E_FOREIGN_VERSION`.
mod common;

use rdy_engine::plan::api::PlanAnswerSpec;
use rdy_engine::plan::group::PlanGroup;
use rdy_engine::*;
use rdy_model::foreign::{
    ForeignDerived, ForeignEntry, ForeignEvent, ForeignId, ForeignLink, ForeignPin,
    ForeignPublication, ForeignSource, ForeignVersion, selection_identity,
};
use rdy_model::plan::{PlanAct, PlanKind};
use rdy_model::*;
use rdy_store::{codec, kinds};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

const AGENTS: &str = "pf-AGNT1";
const PEOPLE: &str = "pf-HMN22";

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

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// A block as the cited side delivers it.
fn block(id_: &str, fields: &[(&str, &str)], body: &str) -> Block {
    Block {
        id: id(id_),
        block_type: "decision".into(),
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

/// The content identity the cited side gives `b` (any hash distinct per content does here).
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

/// One source's chain as the citing side builds it.
struct Source {
    link: String,
    profile: &'static str,
    head: Option<Hash>,
    sel: BTreeMap<BlockId, Hash>,
}

impl Source {
    fn new(link: &str, profile: &'static str) -> Source {
        Source {
            link: link.into(),
            profile,
            head: None,
            sel: BTreeMap::new(),
        }
    }

    /// Writes a delivery straight to the store and publishes the state after it.
    fn deliver(&mut self, r: &mut Rdy, added: &[&Block], changed: &[&Block], removed: &[&str]) {
        for b in added.iter().chain(changed) {
            self.sel.insert(b.id.clone(), version_of(b));
        }
        for x in removed {
            self.sel.remove(&id(x));
        }
        let entry = ForeignEntry {
            repo: cited(),
            source: ForeignSource::Link {
                link: self.link.clone(),
                site: "hub.example".into(),
                at_ms: 1_760_000_000_000,
            },
            event: ForeignEvent::Delivery {
                prev: self.head,
                publication: ForeignPublication {
                    tag: "v1".into(),
                    profile: self.profile.into(),
                    profile_version: None,
                },
                set: None,
                added: added.iter().map(|b| fv(b)).collect(),
                changed: changed.iter().map(|b| fv(b)).collect(),
                removed: removed.iter().map(|x| id(x)).collect(),
                selection: selection_identity(self.sel.iter()).unwrap(),
            },
        };
        let blocks: Vec<Block> = added.iter().chain(changed).map(|b| (*b).clone()).collect();
        self.head = Some(r.store_mut().put_foreign(&entry, &blocks).unwrap());
        r.publish_after_foreign_for_tests().unwrap();
    }
}

/// X and Z (key CORE-RULE) as the agents profile delivers them, Y as the people profile does.
struct Cited {
    x: Block,
    y: Block,
    z: Block,
}

fn cited_blocks() -> Cited {
    Cited {
        x: block("d-FRGNX", &[("title", "X")], "Use X."),
        y: block("d-FRGNY", &[("title", "Y")], "Use Y."),
        z: block("d-FRGNZ", &[("title", "Z"), ("key", "CORE-RULE")], "Use Z."),
    }
}

/// A landed project with a doc and section, and two links to one repo: `core` under the agents
/// profile and `core-people` under the people profile, each with its first delivery.
struct World {
    r: Rdy,
    sec: String,
    agents: Source,
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
            {"op": "create", "type": "repo_link", "id": "$a", "parent": "$d",
             "fields": {"alias": "core", "repo": cited().to_string(), "profile": AGENTS}},
            {"op": "create", "type": "repo_link", "id": "$p", "parent": "$d",
             "fields": {"alias": "core-people", "repo": cited().to_string(), "profile": PEOPLE}}
        ]),
    );
    land(&mut r, "seed");
    let c = cited_blocks();
    let mut agents = Source::new(&w.created["$a"], AGENTS);
    agents.deliver(&mut r, &[&c.x, &c.z], &[], &[]);
    let mut people = Source::new(&w.created["$p"], PEOPLE);
    people.deliver(&mut r, &[&c.y], &[], &[]);
    World {
        r,
        sec: w.created["$s"].clone(),
        agents,
        c,
    }
}

fn get(r: &Rdy, view: &str, block: &str) -> Block {
    let st = r.read();
    let m = st.materialized(&Scope::View(view.into())).unwrap();
    m.get(&id(block)).unwrap().clone()
}

fn fid(b: &Block) -> ForeignId {
    ForeignId::new(cited(), b.id.clone())
}

fn stored(b: &Block) -> String {
    format!("{{{{{}:{}}}}}", cited(), b.id)
}

fn expect_err(e: RdyError, code: &str, message: &str, fix: &str) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (code, message, fix),
        "{e:?}"
    );
}

fn claim(r: &mut Rdy, view: &str, sec: &str, body: &str) -> Result<WriteResult, RdyError> {
    try_write(
        r,
        view,
        json!([{"op": "create", "type": "claim", "id": "$c", "parent": sec, "body": body}]),
    )
}

#[test]
fn rpl12_alias_resolution_uses_selected_profile() {
    let d = tempfile::tempdir().unwrap();
    let World { mut r, sec, c, .. } = world(d.path());

    // an id, a key typed bare and a key with display text, in the body, a field and a link
    let w = write(
        &mut r,
        "w",
        json!([
            {"op": "create", "type": "claim", "id": "$c", "parent": sec,
             "body": "Follow {{core:d-FRGNX}}, {{core:CORE-RULE}} and {{core:core-rule|the rule}}.",
             "links": {"decided_by": [{"target": "core:d-FRGNX"}]}},
            {"op": "create", "type": "section", "id": "$t", "parent": sec,
             "fields": {"title": "Cites {{core:d-FRGNX}}"}}
        ]),
    );
    let t = get(&r, "w", &w.created["$t"]);
    assert_eq!(
        t.field_str("title"),
        Some(format!("Cites {}", stored(&c.x)).as_str())
    );
    assert_eq!(t.foreign.pins, BTreeMap::from([(fid(&c.x), pin_of(&c.x))]));
    assert!(t.foreign.links.is_empty());
    let b = get(&r, "w", &w.created["$c"]);
    let hex = cited().to_string();
    assert_eq!(
        b.body,
        format!(
            "Follow {}, {{{{{hex}:d-FRGNZ|@key}}}} and {{{{{hex}:d-FRGNZ|the rule}}}}.",
            stored(&c.x)
        )
    );
    assert!(!b.links.contains_key("decided_by"), "no local link");
    assert_eq!(
        b.foreign.links,
        BTreeMap::from([(
            "decided_by".to_string(),
            vec![ForeignLink {
                target: fid(&c.x),
                part: None
            }]
        )])
    );
    assert_eq!(
        b.foreign.pins,
        BTreeMap::from([(fid(&c.x), pin_of(&c.x)), (fid(&c.z), pin_of(&c.z))])
    );
    assert!(b.pins.is_empty(), "no local pin");

    // the other alias of the repo reads its own profile's selection
    let w = claim(&mut r, "w", &sec, "Also {{core-people:d-FRGNY}}.").unwrap();
    let b = get(&r, "w", &w.created["$c"]);
    assert_eq!(b.body, format!("Also {}.", stored(&c.y)));
    assert_eq!(b.foreign.pins, BTreeMap::from([(fid(&c.y), pin_of(&c.y))]));

    // never widened by another profile's delivered blocks, in a body or a link
    let not_in = |alias: &str, x: &str| {
        (
            format!("{alias}:{x} is not in the content delivered from {alias}"),
            format!(
                "cite a block that {alias} delivers: rdy_links {{op: \"show\", alias}} / rdy links show {alias}"
            ),
        )
    };
    let (m, f) = not_in("core", "d-FRGNY");
    expect_err(
        claim(&mut r, "w", &sec, "{{core:d-FRGNY}}").unwrap_err(),
        E_UNKNOWN_REF,
        &m,
        &f,
    );
    let e = try_write(
        &mut r,
        "w",
        json!([{"op": "create", "type": "claim", "parent": sec, "body": "B.",
                "links": {"decided_by": [{"target": "core:d-FRGNY"}]}}]),
    )
    .unwrap_err();
    expect_err(e, E_UNKNOWN_REF, &m, &f);
    let (m, f) = not_in("core-people", "CORE-RULE");
    expect_err(
        claim(&mut r, "w", &sec, "{{core-people:CORE-RULE}}").unwrap_err(),
        E_UNKNOWN_REF,
        &m,
        &f,
    );

    // an alias no live link has
    expect_err(
        claim(&mut r, "w", &sec, "{{nope:d-FRGNX}}").unwrap_err(),
        E_UNKNOWN_REF,
        "no repo link has the alias nope",
        "use the alias of a linked project: rdy_links / rdy links",
    );

    // unique live aliases, and never this project's own repo
    let doc = get(&r, "w", &sec).parent.unwrap().to_string();
    let link = |alias: &str, repo: String| {
        json!([{"op": "create", "type": "repo_link", "parent": doc,
                "fields": {"alias": alias, "repo": repo, "profile": AGENTS}}])
    };
    expect_err(
        try_write(&mut r, "w", link("core", RepoId([9; 16]).to_string())).unwrap_err(),
        E_VALIDATION,
        "repo link core: alias is already in use",
        "correct the repo_link block: rdy_write / rdy edit",
    );
    let own = r.read().repo_id().unwrap().to_string();
    expect_err(
        try_write(&mut r, "w", link("self", own)).unwrap_err(),
        E_VALIDATION,
        "repo link self: repo is this project",
        "correct the repo_link block: rdy_write / rdy edit",
    );
    // a refused write wrote nothing; a third alias for the repo is fine
    write(&mut r, "w", link("core-too", cited().to_string()));
    r.close().unwrap();
}

#[test]
fn rpl12_new_ref_pins_but_update_keeps_pin() {
    let d = tempfile::tempdir().unwrap();
    let World {
        mut r,
        sec,
        mut agents,
        c,
    } = world(d.path());
    let c1 = claim(&mut r, "w", &sec, "Per {{core:d-FRGNX}}.")
        .unwrap()
        .created["$c"]
        .clone();
    let first = get(&r, "w", &c1);
    assert_eq!(
        first.foreign.pins,
        BTreeMap::from([(fid(&c.x), pin_of(&c.x))])
    );

    // the cited side changes X
    let x2 = block("d-FRGNX", &[("title", "X")], "Use X, twice.");
    agents.deliver(&mut r, &[], &[&x2], &[]);
    assert_ne!(pin_of(&x2), pin_of(&c.x));

    // an update keeps the pin: the stored text back, the alias form again, a field, a link
    let pins = |r: &Rdy, b: &str| get(r, "w", b).foreign.pins;
    let held = BTreeMap::from([(fid(&c.x), pin_of(&c.x))]);
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": c1, "body": format!("{} More.", first.body)}]),
    );
    assert_eq!(pins(&r, &c1), held);
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": c1, "body": "Per {{core:d-FRGNX}}, again."}]),
    );
    assert_eq!(pins(&r, &c1), held);
    assert_eq!(
        get(&r, "w", &c1).body,
        format!("Per {}, again.", stored(&c.x))
    );
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": c1,
                "links": {"decided_by": [{"target": "core:d-FRGNX"}]}}]),
    );
    assert_eq!(pins(&r, &c1), held, "one pin per target");

    // a new citation pins the current delivery
    let c2 = claim(&mut r, "w", &sec, "Per {{core:d-FRGNX}}.")
        .unwrap()
        .created["$c"]
        .clone();
    assert_eq!(pins(&r, &c2), BTreeMap::from([(fid(&c.x), pin_of(&x2))]));
    // so does a citation that was dropped and written again
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": c1, "body": "Nothing cited.", "links": {"decided_by": []}}]),
    );
    assert!(pins(&r, &c1).is_empty());
    assert!(get(&r, "w", &c1).foreign.is_empty());
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": c1, "body": "Per {{core:d-FRGNX}}."}]),
    );
    assert_eq!(pins(&r, &c1), BTreeMap::from([(fid(&c.x), pin_of(&x2))]));

    // once X leaves the selection, a held pin still reads, but a new citation is refused
    agents.deliver(&mut r, &[], &[], &["d-FRGNX"]);
    let body2 = get(&r, "w", &c2).body;
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": c2, "body": format!("{body2} Still.")}]),
    );
    assert_eq!(pins(&r, &c2), BTreeMap::from([(fid(&c.x), pin_of(&x2))]));
    expect_err(
        claim(&mut r, "w", &sec, "Per {{core:d-FRGNX}}.").unwrap_err(),
        E_UNKNOWN_REF,
        "core:d-FRGNX is not in the content delivered from core",
        "cite a block that core delivers: rdy_links {op: \"show\", alias} / rdy links show core",
    );
    // the stored form is a new citation too when the block holds no pin for it
    expect_err(
        claim(&mut r, "w", &sec, &format!("Per {}.", stored(&c.x))).unwrap_err(),
        E_UNKNOWN_REF,
        "core:d-FRGNX is not in the content delivered from core",
        "cite a block that core delivers: rdy_links {op: \"show\", alias} / rdy links show core",
    );
    r.close().unwrap();
}

#[test]
fn rpl12_repin_plan_and_reaffirm_leave_foreign_pins() {
    let d = tempfile::tempdir().unwrap();
    let World {
        mut r,
        sec,
        mut agents,
        c,
    } = world(d.path());
    let w = write(
        &mut r,
        "seed2",
        json!([
            {"op": "create", "type": "claim", "id": "$a", "parent": sec, "body": "A holds 30 days."},
            {"op": "create", "type": "claim", "id": "$b", "parent": sec,
             "body": "B builds on {{$a}} and {{core:d-FRGNX}}."}
        ]),
    );
    let (a, b) = (w.created["$a"].clone(), w.created["$b"].clone());
    let dec = r
        .decide(
            &writer(),
            "seed2",
            DecisionInput {
                parent: sec.clone(),
                statement: "Keep A, as {{core:d-FRGNX}} says.".into(),
                depends_on: vec![a.clone()],
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    land(&mut r, "seed2");
    let held = BTreeMap::from([(fid(&c.x), pin_of(&c.x))]);
    let canon = |r: &Rdy, x: &str| {
        let st = r.read();
        let m = st.materialized(&Scope::Canon).unwrap();
        m.get(&id(x)).unwrap().clone()
    };
    assert_eq!(canon(&r, &b).foreign.pins, held);
    assert_eq!(canon(&r, &dec).foreign.pins, held);

    // X changes upstream, and A changes here
    let x2 = block("d-FRGNX", &[("title", "X")], "Use X, twice.");
    agents.deliver(&mut r, &[], &[&x2], &[]);
    let tracking = r.store().tracking().clone();
    let edit_a = |r: &mut Rdy, view: &str| {
        write(
            r,
            view,
            json!([{"op": "update", "id": a, "body": "A holds 60 days."}]),
        );
        get(r, view, &a).content_hash_with(&tracking)
    };

    // a repin moves the local pin only
    let now = edit_a(&mut r, "repin");
    write(&mut r, "repin", json!([{"op": "repin", "id": b}]));
    let got = get(&r, "repin", &b);
    assert_eq!(got.pins.get(&id(&a)), Some(&now));
    assert_eq!(got.foreign.pins, held, "repin");

    // a plan repin, as a confirm group with repin, moves the local pin only
    let now = edit_a(&mut r, "plan");
    r.answer_plan_specs(
        &writer(),
        "plan",
        &[PlanAnswerSpec {
            item: None,
            group: Some(PlanGroup {
                kind: Some(PlanKind::Confirm),
                ..Default::default()
            }),
            act: PlanAct::Confirm,
            reason: Some("repinned in bulk".into()),
            repin: true,
            implements: None,
            editorial: None,
            reflects: None,
        }],
    )
    .unwrap();
    let got = get(&r, "plan", &b);
    assert_eq!(got.pins.get(&id(&a)), Some(&now), "plan repinned b");
    assert_eq!(got.foreign.pins, held, "plan repin");

    // a reaffirm moves the cause's pin only
    let now = edit_a(&mut r, "again");
    write(
        &mut r,
        "again",
        json!([{"op": "reaffirm", "id": dec, "targets": [a], "reason": "Still holds."}]),
    );
    let got = get(&r, "again", &dec);
    assert_eq!(got.pins.get(&id(&a)), Some(&now));
    assert_eq!(got.foreign.pins, held, "reaffirm");
    r.close().unwrap();
}

#[test]
fn rpl12_missing_held_version_refuses_replay() {
    let d = tempfile::tempdir().unwrap();
    let World { mut r, sec, c, .. } = world(d.path());
    let w = claim(&mut r, "w", &sec, "Per {{core:d-FRGNX}}.").unwrap();
    let cited_here = get(&r, "w", &w.created["$c"]);
    let change = r
        .store()
        .change(r.read().included("w").last().unwrap())
        .unwrap();
    let replayed: Vec<Block> = change
        .ops
        .iter()
        .filter_map(|op| match op {
            Op::Create { block } | Op::Update { block, .. } => Some(block.clone()),
            Op::Delete { .. } => None,
        })
        .collect();
    assert_eq!(replayed, vec![cited_here.clone()]);
    // where the delivery is held, the pins pass
    let view = Scope::View("w".into());
    r.read().check_held_pins(&view, replayed.iter()).unwrap();

    // a project that links the same repo but holds no delivery of it refuses them, naming the
    // pinned content
    let mut other = Rdy::init(&d.path().join("q"), "rdy-standard").unwrap();
    let short = &content_of(&c.x).to_string()[..12];
    let e = other
        .read()
        .check_held_pins(&Scope::Canon, replayed.iter())
        .unwrap_err();
    // with no live link, the repo shows as its first 12 hex digits
    let repo12 = &cited().to_string()[..12];
    expect_err(
        e,
        E_FOREIGN_VERSION,
        &format!("{repo12}:d-FRGNX has no delivered version {short}"),
        &format!(
            "wait for the delivery that holds it, or pin a delivered version: rdy_links {{op: \"show\", alias}} / rdy links show {repo12}"
        ),
    );
    write(
        &mut other,
        "w",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "repo_link", "parent": "$d",
             "fields": {"alias": "core", "repo": cited().to_string(), "profile": AGENTS}}
        ]),
    );
    expect_err(
        other
            .read()
            .check_held_pins(&view, replayed.iter())
            .unwrap_err(),
        E_FOREIGN_VERSION,
        &format!("core:d-FRGNX has no delivered version {short}"),
        "wait for the delivery that holds it, or pin a delivered version: rdy_links {op: \"show\", alias} / rdy links show core",
    );
    // a version held for another block, or a content no delivery carried, is no held version
    let mut wrong = cited_here.clone();
    wrong.foreign.pins.insert(fid(&c.x), pin_of(&c.z));
    let e = r.read().check_held_pins(&view, [&wrong]).unwrap_err();
    assert_eq!(e.code, E_FOREIGN_VERSION, "{e:?}");
    assert_eq!(
        e.message,
        format!(
            "core:d-FRGNX has no delivered version {}",
            &content_of(&c.z).to_string()[..12]
        )
    );
    other.close().unwrap();
    r.close().unwrap();
}
