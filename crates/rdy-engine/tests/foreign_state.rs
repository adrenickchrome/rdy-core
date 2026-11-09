//! rpl8 (repo links, D-FK58A): foreign state beside canon in a read state. A delivery is held in
//! the foreign log and in the state's foreign cut; it never moves canon, never enters a scope's
//! materialization, key index, lexicon, coverage or mention scans, and a state read keeps the one
//! cut it was published with.
mod common;

use rdy_engine::*;
use rdy_model::foreign::{
    ForeignDerived, ForeignEntry, ForeignEvent, ForeignId, ForeignPin, ForeignPublication,
    ForeignShell, ForeignSource, ForeignVersion, delivery_selection, selection_identity,
};
use rdy_model::*;
use rdy_store::{codec, kinds};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

fn writer() -> Session {
    Session::human("human:founder".into(), Role::Writer).unwrap()
}

fn founder() -> Session {
    Session::human("human:founder".into(), Role::Approver).unwrap()
}

fn write(r: &mut Rdy, ops: Value) {
    r.write(
        &writer(),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "write to work".into(),
        },
    )
    .unwrap();
}

fn land(r: &mut Rdy, title: &str) {
    let p = r
        .propose(&writer(), "work", title, Some(common::TEST_SUMMARY))
        .unwrap();
    common::confirm_exceptions(r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap();
}

fn cited() -> RepoId {
    RepoId([7; 16])
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// A block as the cited side delivers it.
fn block(id_: &str, block_type: &str, fields: &[(&str, &str)], body: &str) -> Block {
    Block {
        id: id(id_),
        block_type: block_type.into(),
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

fn fv(b: &Block) -> ForeignVersion {
    ForeignVersion {
        id: b.id.clone(),
        version: version_of(b),
        content: Hash([b.body.len() as u8; 32]),
        encoding: 0,
        derived: ForeignDerived::default(),
    }
}

fn link(l: &str) -> ForeignSource {
    ForeignSource::Link {
        link: l.into(),
        site: "hub.example".into(),
        at_ms: 1_760_000_000_000,
    }
}

/// One source's chain as the citing side builds it.
struct Source {
    source: ForeignSource,
    profile: &'static str,
    head: Option<Hash>,
    sel: BTreeMap<BlockId, Hash>,
}

impl Source {
    fn new(l: &str, profile: &'static str) -> Source {
        Source {
            source: link(l),
            profile,
            head: None,
            sel: BTreeMap::new(),
        }
    }

    /// Writes a delivery of `added`, `changed` and `removed` straight to the store and publishes
    /// the state after it.
    fn deliver(&mut self, r: &mut Rdy, added: &[&Block], changed: &[&Block], removed: &[&str]) {
        self.deliver_shelled(r, added, changed, removed, vec![]);
    }

    /// As [`Source::deliver`], with the delivery's `shells` counted in its selection.
    fn deliver_shelled(
        &mut self,
        r: &mut Rdy,
        added: &[&Block],
        changed: &[&Block],
        removed: &[&str],
        shells: Vec<ForeignShell>,
    ) {
        for b in added.iter().chain(changed) {
            self.sel.insert(b.id.clone(), version_of(b));
        }
        for x in removed {
            self.sel.remove(&id(x));
        }
        let entry = ForeignEntry {
            repo: cited(),
            source: self.source.clone(),
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
                selection: delivery_selection(self.sel.iter(), &shells).unwrap(),
                shells,
            },
        };
        let blocks: Vec<Block> = added.iter().chain(changed).map(|b| (*b).clone()).collect();
        self.head = Some(r.store_mut().put_foreign(&entry, &blocks).unwrap());
        r.publish_after_foreign_for_tests().unwrap();
    }
}

/// A project with a landed doc, a local entity and a note that names a foreign term.
fn project(base: &Path) -> Rdy {
    let mut r = Rdy::init(&base.join("p"), "rdy-standard").unwrap();
    write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
            {"op": "create", "type": "entity", "id": "$e", "parent": "$s", "fields": {"name": "Gateway"}},
            {"op": "create", "type": "note", "id": "$n", "parent": "$s",
             "body": "The Foreign Widget feeds the Gateway."}
        ]),
    );
    land(&mut r, "Guide");
    r
}

fn target(b: &Block) -> ForeignId {
    ForeignId::new(cited(), b.id.clone())
}

#[test]
fn rpl8_delivery_never_advances_canon() {
    let d = tempfile::tempdir().unwrap();
    let mut r = project(d.path());
    let before = r.read();
    assert!(before.foreign().unwrap().is_empty());
    let canon = before.materialized(&Scope::Canon).unwrap();
    let (head, snapshot, count) = (before.canon_head(), canon.snapshot_id(), canon.len());
    let views = before.view_names();

    let dec = block("d-FRGN1", "decision", &[("title", "Cited rule")], "Use it.");
    let mut s = Source::new("rl-00001", "agents");
    s.deliver(&mut r, &[&dec], &[], &[]);

    let after = r.read();
    assert_eq!(after.canon_head(), head, "canon head");
    let canon_after = after.materialized(&Scope::Canon).unwrap();
    assert_eq!(canon_after.snapshot_id(), snapshot, "canon state");
    assert_eq!(canon_after.len(), count);
    assert_eq!(after.view_names(), views);
    assert!(canon_after.get(&dec.id).is_none(), "not in the tree");
    assert_eq!(
        after.block(&Scope::Canon, &dec.id).unwrap_err().code,
        E_UNKNOWN_BLOCK
    );
    // it is in the foreign cut, with the version the delivery named
    let f = after.foreign().unwrap();
    let held = f.current_version(&target(&dec)).expect("current");
    assert_eq!(held.version.version, version_of(&dec));
    assert!(after.seq() > before.seq());
    assert!(f.watermark() > before.foreign().unwrap().watermark());
    // the state before still shows no delivery
    assert!(before.foreign().unwrap().is_empty());
    r.close().unwrap();
}

#[test]
fn rpl8_snapshot_keeps_one_foreign_cut() {
    let d = tempfile::tempdir().unwrap();
    let mut r = project(d.path());
    let a1 = block("d-FRGNA", "decision", &[("title", "A")], "one");
    let mut s = Source::new("rl-00001", "agents");
    s.deliver(&mut r, &[&a1], &[], &[]);
    let st1 = r.read();

    // a second delivery changes A and adds B; a second source adds C under another profile
    let a2 = block("d-FRGNA", "decision", &[("title", "A")], "two");
    let b = block("d-FRGNB", "decision", &[("title", "B")], "bee");
    s.deliver(&mut r, &[&b], &[&a2], &[]);
    let st2 = r.read();
    let c = block("d-FRGNC", "decision", &[("title", "C")], "sea");
    let mut other = Source::new("rl-00002", "people");
    other.deliver(&mut r, &[&c], &[], &[]);
    let st3 = r.read();
    s.deliver(&mut r, &[], &[], &["d-FRGNB"]);
    let st4 = r.read();

    let current = |st: &ReadState| -> BTreeMap<String, Hash> {
        st.foreign()
            .unwrap()
            .current(&cited())
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.to_string(), v.version.version))
                    .collect()
            })
            .unwrap_or_default()
    };
    let (va1, va2, vb, vc) = (
        version_of(&a1),
        version_of(&a2),
        version_of(&b),
        version_of(&c),
    );
    // each state keeps the cut it was published with, whatever the store holds now
    assert_eq!(current(&st1), BTreeMap::from([("d-FRGNA".into(), va1)]));
    assert_eq!(
        current(&st2),
        BTreeMap::from([("d-FRGNA".into(), va2), ("d-FRGNB".into(), vb)])
    );
    assert_eq!(
        current(&st3),
        BTreeMap::from([
            ("d-FRGNA".into(), va2),
            ("d-FRGNB".into(), vb),
            ("d-FRGNC".into(), vc)
        ])
    );
    assert_eq!(
        current(&st4),
        BTreeMap::from([("d-FRGNA".into(), va2), ("d-FRGNC".into(), vc)])
    );
    assert!(st1.foreign().unwrap().watermark() < st2.foreign().unwrap().watermark());

    // a held pin reads a replaced or removed version, but only once an entry before the
    // state's watermark delivered it
    let (ta, tb) = (target(&a1), target(&b));
    assert!(st1.foreign_held(&ta, &va1).unwrap().is_some());
    assert!(st1.foreign_held(&ta, &va2).unwrap().is_none(), "later");
    assert!(st1.foreign_held(&tb, &vb).unwrap().is_none(), "later");
    assert_eq!(st2.foreign_held(&ta, &va1).unwrap().unwrap().body, "one");
    assert_eq!(st2.foreign_held(&ta, &va2).unwrap().unwrap().body, "two");
    assert_eq!(st4.foreign_held(&tb, &vb).unwrap().unwrap().body, "bee");
    // a version is held for its own block and repo only
    assert!(st4.foreign_held(&tb, &va1).unwrap().is_none());
    assert!(
        st4.foreign_held(&ForeignId::new(RepoId([8; 16]), b.id.clone()), &vb)
            .unwrap()
            .is_none()
    );

    // a lookup under one profile never takes another profile's blocks
    let f = st4.foreign().unwrap();
    let agents: Vec<String> = f
        .current_for_profile(&cited(), "agents")
        .keys()
        .map(|k| k.to_string())
        .collect();
    assert_eq!(agents, vec!["d-FRGNA".to_string()]);
    let people: Vec<String> = f
        .current_for_profile(&cited(), "people")
        .keys()
        .map(|k| k.to_string())
        .collect();
    assert_eq!(people, vec!["d-FRGNC".to_string()]);
    assert!(f.current_for_profile(&cited(), "none").is_empty());
    assert!(f.link_source(&id("rl-00001")).is_some());
    assert!(f.link_source(&id("rl-00009")).is_none());
    r.close().unwrap();
}

#[test]
fn rpl8_foreign_entities_do_not_enter_coverage() {
    let d = tempfile::tempdir().unwrap();
    let mut r = project(d.path());
    let before = r.read();
    let report = |st: &ReadState| {
        serde_json::to_value(st.coverage_report(&Scope::Canon, None).unwrap()).unwrap()
    };
    let report_before = report(&before);
    let lex_before = before.lexicon(&Scope::Canon).unwrap();
    let entities = lex_before.entity_count();
    assert!(lex_before.resolve("Foreign Widget").is_empty());

    let widget = block(
        "en-FRGN1",
        "entity",
        &[("name", "Foreign Widget"), ("key", "foreign-widget")],
        "",
    );
    let mut s = Source::new("rl-00001", "agents");
    s.deliver(&mut r, &[&widget], &[], &[]);

    let after = r.read();
    assert!(
        after
            .foreign()
            .unwrap()
            .current_version(&target(&widget))
            .is_some()
    );
    let lex = after.lexicon(&Scope::Canon).unwrap();
    assert_eq!(lex.entity_count(), entities, "no entity joins the lexicon");
    assert!(lex.resolve("Foreign Widget").is_empty(), "no name");
    assert!(lex.resolve("foreign-widget").is_empty(), "no key");
    // the note that names the foreign term gains no mention, and coverage does not move
    assert_eq!(report(&after), report_before);
    let issues = after.coverage_issues(&Scope::Canon).unwrap();
    assert_eq!(
        issues.blocks.len(),
        before.coverage_issues(&Scope::Canon).unwrap().blocks.len()
    );
    assert!(!issues.blocks.contains_key(&widget.id), "no coverage entry");
    // nor does the graph or the key index know it
    let graph = after.graph(&Scope::Canon).unwrap();
    assert!(graph.status(&widget.id).is_none());
    assert!(graph.incoming(&widget.id).is_empty() && graph.outgoing(&widget.id).is_empty());
    assert!(after.resolve_id(&Scope::Canon, "foreign-widget").is_err());
    assert!(after.block(&Scope::Canon, &widget.id).is_err());
    // a view reads canon the same way
    assert!(
        after
            .block(&Scope::View("work".into()), &widget.id)
            .is_err()
    );
    r.close().unwrap();
}

/// A full publish (an open, an invalidate, and the end of a follow) reads the foreign state from
/// the log whatever canon's tip is: a follower's foreign-only increment shows the same way.
#[test]
fn rpl8_full_publish_reads_the_foreign_log_beside_an_unmoved_canon() {
    let d = tempfile::tempdir().unwrap();
    let mut r = project(d.path());
    let canon_head = r.read().canon_head();
    let dec = block("d-FRGN1", "decision", &[("title", "Cited rule")], "Use it.");
    let mut s = Source::new("rl-00001", "agents");
    s.deliver(&mut r, &[&dec], &[], &[]);
    let (version, watermark) = (version_of(&dec), r.read().foreign().unwrap().watermark());

    // a republish of everything
    r.invalidate();
    let st = r.read();
    assert_eq!(st.canon_head(), canon_head);
    let held = st
        .foreign()
        .unwrap()
        .current_version(&target(&dec))
        .unwrap();
    assert_eq!(held.version.version, version);
    assert_eq!(st.foreign().unwrap().watermark(), st.seq());
    assert!(st.seq() >= watermark);
    assert_eq!(
        st.foreign_held(&target(&dec), &version)
            .unwrap()
            .unwrap()
            .body,
        "Use it."
    );

    // a reopen reads the same cut from the log
    drop(st);
    r.close().unwrap();
    let r = Rdy::open(&d.path().join("p")).unwrap();
    let st = r.read();
    assert_eq!(st.canon_head(), canon_head);
    let held = st
        .foreign()
        .unwrap()
        .current_version(&target(&dec))
        .unwrap();
    assert_eq!(held.version.version, version);
    assert!(st.foreign().unwrap().link_source(&id("rl-00001")).is_some());
    r.close().unwrap();
}

#[test]
fn rpl9_a_shell_is_not_a_delivered_version() {
    let d = tempfile::tempdir().unwrap();
    let mut r = project(d.path());
    let dec = block("d-FRGN1", "decision", &[("title", "Cited rule")], "Use it.");
    let shell = ForeignShell {
        id: id("d-FRGN9"),
        version: Hash([9; 32]),
    };
    let mut s = Source::new("rl-00001", "agents");
    s.deliver_shelled(&mut r, &[&dec], &[], &[], vec![shell.clone()]);

    let state = r.read();
    let f = state.foreign().unwrap();
    let ghost = ForeignId::new(cited(), shell.id.clone());
    // the source's selection identity counts the shell, but no member, current view, held
    // lookup, pin or read resolves to it
    let view = f.sources().next().expect("the source").1;
    assert_eq!(
        view.selection,
        delivery_selection(s.sel.iter(), std::slice::from_ref(&shell)).unwrap()
    );
    assert_ne!(view.selection, selection_identity(s.sel.iter()).unwrap());
    assert!(!view.members.contains_key(&shell.id));
    assert!(f.current_version(&ghost).is_none());
    assert!(
        state
            .foreign_held(&ghost, &shell.version)
            .unwrap()
            .is_none()
    );
    let pin = ForeignPin {
        content: shell.version,
        encoding: 0,
    };
    assert!(state.foreign_held_pin(&ghost, &pin).unwrap().is_none());
    // the delivered block still resolves
    assert!(f.current_version(&target(&dec)).is_some());
}
