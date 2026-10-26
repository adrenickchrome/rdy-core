//! ok7: replay. Selected changes become new changes in an owned open view, one per distinct
//! source, in dependency order, with one `Include` for the whole call. A replay brings no
//! unselected dependency, keeps each source base, and records a conflict wherever the target's
//! version differs from that base and from the replayed result. A result equal to the target's
//! version is already satisfied, and records none.
mod common;

#[path = "../../rdy-store/tests/support/redaction.rs"]
mod redaction_fixture;

use rdy_engine::views::{ReplayResult, replay_order_for_tests};
use rdy_engine::*;
use rdy_model::redaction::RedactTarget;
use rdy_model::*;
use rdy_store::closure::{END_OF_LOG, StoreSource, closure, write_batch};
use rdy_store::kinds;
use rdy_store::merge::{Side, conflict_id};
use rdy_store::segment::Location;
use rdy_store::stream_error::StreamError;
use rdy_store::sync_proof::ObjectSource;
use rdy_store::sync_stream::StreamHeader;
use rdy_store::take_batch::{TagChoice, UploadFacts, UploadTarget};
use rdy_store::view::ViewOpKind;
use redaction_fixture::{FixtureOptions, Proof, RequestSpec, authorized_batch_fixture};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;

const HUMAN: &str = "human:founder";

fn human() -> Session {
    Session::human(HUMAN.into(), Role::Writer).unwrap()
}

fn founder() -> Session {
    Session::human(HUMAN.into(), Role::Approver).unwrap()
}

fn agent(run: &str) -> Session {
    Session::agent("agent:a".into(), Some(run.into()), Role::Writer).unwrap()
}

fn write(r: &mut Rdy, s: &Session, view: &str, ops: Value) -> WriteResult {
    r.write(
        s,
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: format!("write to {view}"),
        },
    )
    .unwrap_or_else(|e| panic!("write to {view}: {e}"))
}

fn land(r: &mut Rdy, view: &str) -> Hash {
    let p = r
        .propose(&human(), view, "ok7", Some(common::TEST_SUMMARY))
        .unwrap();
    common::confirm_exceptions(r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap().parse().unwrap()
}

fn h(s: &str) -> Hash {
    s.parse().unwrap()
}

/// A landed canon: doc `one` holding prose x, y, z and w.
struct P {
    dir: tempfile::TempDir,
    r: Rdy,
    seed: Hash,
    one: String,
    x: String,
    y: String,
    z: String,
    w: String,
}

fn project() -> P {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        &human(),
        "seed",
        json!([
            {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "area"}},
            {"op": "create", "type": "doc", "id": "$one", "parent": "$f", "fields": {"name": "one", "title": "One", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$x", "parent": "$one", "body": "Text x."},
            {"op": "create", "type": "prose", "id": "$y", "parent": "$one", "body": "Text y."},
            {"op": "create", "type": "prose", "id": "$z", "parent": "$one", "body": "Text z."},
            {"op": "create", "type": "prose", "id": "$w", "parent": "$one", "body": "Text w."}
        ]),
    );
    land(&mut r, "seed");
    let id = |k: &str| w.created[k].clone();
    P {
        seed: h(&w.change),
        one: id("$one"),
        x: id("$x"),
        y: id("$y"),
        z: id("$z"),
        w: id("$w"),
        dir,
        r,
    }
}

fn update(r: &mut Rdy, s: &Session, view: &str, id: &str, body: &str) -> Hash {
    h(&write(
        r,
        s,
        view,
        json!([{"op": "update", "id": id, "body": body}]),
    )
    .change)
}

fn new_view(r: &mut Rdy, s: &Session, name: &str) {
    r.view_create(s, name, None, false).unwrap();
}

fn replay(r: &mut Rdy, s: &Session, view: &str, sources: &[Hash]) -> ReplayResult {
    let input: Vec<String> = sources.iter().map(|c| c.to_string()).collect();
    r.view_replay(s, view, &input)
        .unwrap_or_else(|e| panic!("replay into {view}: {e}"))
}

fn replay_err(r: &mut Rdy, s: &Session, view: &str, sources: &[String]) -> RdyError {
    r.view_replay(s, view, sources).unwrap_err()
}

fn change(r: &Rdy, c: &Hash) -> Change {
    r.read().reader().change(c).unwrap()
}

fn version(r: &Rdy, view: &str, id: &str) -> Option<Hash> {
    r.read()
        .materialized(&Scope::View(view.into()))
        .unwrap()
        .version_of(&BlockId::parse(id).unwrap())
}

fn canon_version(r: &Rdy, id: &str) -> Option<Hash> {
    r.read()
        .materialized(&Scope::Canon)
        .unwrap()
        .version_of(&BlockId::parse(id).unwrap())
}

fn included(r: &Rdy, view: &str) -> Vec<Hash> {
    r.read().included(view).as_ref().clone()
}

/// The new change replaying `source` in `res`.
fn new_of(res: &ReplayResult, source: &Hash) -> Hash {
    h(&res
        .replayed
        .iter()
        .find(|x| x.source == source.to_string())
        .unwrap_or_else(|| panic!("{source} was not replayed"))
        .change)
}

/// The log end and every ref: what a refused act must leave as it was.
fn log_state(r: &Rdy) -> (Location, Vec<(String, Vec<u8>)>) {
    (
        r.store().log_end(),
        r.read().reader().refs_with_prefix("").unwrap(),
    )
}

/// A conflict as (block, base, sorted sides of (change, version)).
type Shape = (
    String,
    Option<String>,
    Vec<(Option<String>, Option<String>)>,
);

fn shapes(cs: &[ConflictInfo]) -> BTreeMap<String, Shape> {
    cs.iter()
        .map(|c| {
            let mut sides: Vec<_> = c
                .sides
                .iter()
                .map(|s| (s.change.clone(), s.version.clone()))
                .collect();
            sides.sort();
            (c.block.clone(), (c.block.clone(), c.base.clone(), sides))
        })
        .collect()
}

fn shape(block: &str, base: Option<Hash>, sides: &[(Option<Hash>, Option<Hash>)]) -> Shape {
    let mut sides: Vec<_> = sides
        .iter()
        .map(|(c, v)| (c.map(|c| c.to_string()), v.map(|v| v.to_string())))
        .collect();
    sides.sort();
    (block.to_string(), base.map(|b| b.to_string()), sides)
}

/// The conflict id the existing recipe gives `c`.
fn recipe_id(c: &ConflictInfo) -> String {
    let mut sides: Vec<Side> = c
        .sides
        .iter()
        .map(|s| Side {
            change: s.change.as_deref().map(h),
            version: s.version.as_deref().map(h),
        })
        .collect();
    sides.sort();
    conflict_id(
        &BlockId::parse(&c.block).unwrap(),
        c.base.as_deref().map(h),
        &sides,
    )
    .to_string()
}

fn sorted(mut hs: Vec<Hash>) -> Vec<Hash> {
    hs.sort();
    hs
}

#[test]
fn ok7_replay_orders_sources_without_importing_deps() {
    let mut p = project();
    let r = &mut p.r;
    let c1 = update(r, &human(), "src", &p.x, "x one");
    let c2 = update(r, &human(), "src", &p.x, "x two");
    let c3 = update(r, &human(), "src", &p.y, "y one");
    assert!(change(r, &c2).deps.contains(&c1));
    let olds: BTreeSet<Hash> = [c1, c2, c3].into();
    // shuffled inputs with duplicates give one order: no edge outside the selection, so the
    // ready order is ascending hash
    let mut orders = vec![];
    for (n, input) in [vec![c3, c2, c2], vec![c2, c3, c3, c2]].iter().enumerate() {
        let view = format!("t{n}");
        new_view(r, &human(), &view);
        let res = replay(r, &human(), &view, input);
        let sources: Vec<Hash> = res.replayed.iter().map(|x| h(&x.source)).collect();
        assert_eq!(sources, sorted(vec![c2, c3]), "{view}");
        let news: Vec<String> = res.replayed.iter().map(|x| x.change.clone()).collect();
        assert_eq!(res.added, news);
        assert!(res.added_deps.is_empty() && res.removed.is_empty() && res.cascaded.is_empty());
        assert_eq!(res.changes, 2);
        // the unselected dependency is absent, and no source enters a dep list
        let inc = included(r, &view);
        assert!(!inc.contains(&c1), "{view}");
        assert_eq!(inc, news.iter().map(|c| h(c)).collect::<Vec<_>>());
        for c in &inc {
            let ch = change(r, c);
            assert!(ch.deps.iter().all(|d| !olds.contains(d)), "{view}: {ch:?}");
            assert!(olds.contains(&ch.links.replayed_from.unwrap()));
        }
        orders.push(sources);
    }
    assert_eq!(orders[0], orders[1]);
    // a selected chain replays in dependency order, whatever its hashes; the later replay
    // depends on the earlier one, never on its source
    new_view(r, &human(), "chain");
    let res = replay(r, &human(), "chain", &[c2, c1]);
    let sources: Vec<Hash> = res.replayed.iter().map(|x| h(&x.source)).collect();
    assert_eq!(sources, vec![c1, c2]);
    let (r1, r2) = (new_of(&res, &c1), new_of(&res, &c2));
    assert_eq!(change(r, &r2).deps, vec![r1]);
    // the chain applies on the target's own base: x reads as c2 wrote it
    assert_eq!(version(r, "chain", &p.x), version(r, "src", &p.x));
    assert!(res.conflicts.is_empty(), "{:?}", res.conflicts);
}

#[test]
fn ok7_excluding_one_independent_replay_keeps_the_other() {
    let mut p = project();
    let r = &mut p.r;
    let a = update(r, &human(), "src", &p.x, "x a");
    let b = update(r, &human(), "src", &p.y, "y b");
    for (n, gone) in [a, b].iter().enumerate() {
        let view = format!("t{n}");
        new_view(r, &human(), &view);
        let res = replay(r, &human(), &view, &[a, b]);
        let (ra, rb) = (new_of(&res, &a), new_of(&res, &b));
        // independent replays gain no dependency on each other
        assert!(!change(r, &rb).deps.contains(&ra));
        assert!(!change(r, &ra).deps.contains(&rb));
        let (out, kept) = if *gone == a { (ra, rb) } else { (rb, ra) };
        let ex = r
            .view_exclude(&human(), &view, &[out.to_string()], true)
            .unwrap();
        assert!(ex.cascaded.is_empty(), "{view}: {:?}", ex.cascaded);
        assert_eq!(included(r, &view), vec![kept], "{view}");
    }
    // a shared block and a selected source dependency still cascade
    let c = update(r, &human(), "src", &p.z, "z c");
    let d = update(r, &human(), "src", &p.z, "z d");
    new_view(r, &human(), "tc");
    let res = replay(r, &human(), "tc", &[d, c]);
    let (rc, rd) = (new_of(&res, &c), new_of(&res, &d));
    assert_eq!(change(r, &rd).deps, vec![rc]);
    let e = r
        .view_exclude(&human(), "tc", &[rc.to_string()], false)
        .unwrap_err();
    assert_eq!(e.code, E_HAS_DEPENDENTS);
    let ex = r
        .view_exclude(&human(), "tc", &[rc.to_string()], true)
        .unwrap();
    assert_eq!(ex.cascaded, vec![rd.to_string()]);
    assert!(included(r, "tc").is_empty());
}

#[test]
fn ok7_replay_copies_authored_fields_and_alternative() {
    let p = project();
    let P { dir, mut r, x, .. } = p;
    // a source with every authored field set, written by another actor in another view
    let xid = BlockId::parse(&x).unwrap();
    let base = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .get(&xid)
        .unwrap()
        .clone();
    let mut edited = base.clone();
    edited.body = "x, as the source wrote it".into();
    let source = Change {
        view: "elsewhere".into(),
        ops: vec![Op::Update {
            base: base.version_hash(),
            block: edited,
        }],
        deps: vec![],
        provenance: Provenance {
            actor: "agent:source".into(),
            run: Some("run-src".into()),
            role: Role::Writer,
            at_ms: 42,
        },
        justification: Some(Justification::Editorial {
            class: EditorialClass::Clarify,
            reason: "say it plainly".into(),
        }),
        message: "the source's message".into(),
        links: ChangeLinks {
            intent: Some(Hash::of(b"the source session's intent")),
            alternative: Some(AltChoice {
                set: Hash::of(b"an alternative set"),
                member: "elsewhere".into(),
            }),
            client_nonce: Some("source retry nonce".into()),
            imported_by: Some(ActorRun {
                actor: "agent:importer".into(),
                run: None,
            }),
            replayed_from: None,
            briefing: Some(Hash::of(b"the briefing token")),
        },
        read_identity: None,
    };
    let src = r
        .store_mut()
        .commit_to_view("elsewhere", source.clone())
        .unwrap();
    r.close().unwrap();
    let mut r = Rdy::open(dir.path()).unwrap();
    let source = change(&r, &src);
    // the caller: an agent in its own view
    let caller = agent("run-c");
    new_view(&mut r, &caller, "run-c");
    let pre = r.read().view_head("run-c").unwrap().head;
    let res = replay(&mut r, &caller, "run-c", &[src]);
    let new = new_of(&res, &src);
    let c = change(&r, &new);
    // copied exactly
    assert_eq!(c.ops, source.ops);
    assert_eq!(c.justification, source.justification);
    assert_eq!(c.message, source.message);
    assert_eq!(c.links.alternative, source.links.alternative);
    assert_eq!(c.links.briefing, source.links.briefing);
    // set for the new act
    assert_eq!(c.view, "run-c");
    assert_eq!(c.links.replayed_from, Some(src));
    assert_eq!(c.links.client_nonce, Some(format!("replay:{pre}:0")));
    assert_eq!(c.links.intent, None);
    assert_eq!(c.links.imported_by, None);
    // the caller's provenance, the same as the one Include's
    let include = r.read().reader().view_op(&h(&res.head)).unwrap();
    assert_eq!(include.op, ViewOpKind::Include { changes: vec![new] });
    assert_eq!(
        (c.provenance.actor.as_str(), c.provenance.run.as_deref()),
        ("agent:a", Some("run-c"))
    );
    assert_eq!(c.provenance.role, Role::Writer);
    assert_eq!(c.provenance.at_ms, include.at_ms);
    assert_eq!(
        (include.actor.as_str(), include.run.as_deref()),
        ("agent:a", Some("run-c"))
    );
    // the current encoding, and no source dependency: x applies on canon's base, so the change
    // depends on what an ordinary write in the view would (the landed producer of x)
    assert_eq!(
        c.stored_identity().unwrap().version,
        rdy_model::encoding::CHANGE_ENCODING
    );
    assert_eq!(c.deps, vec![p_seed(&r, &x)]);
    // a second replay of the same source is a distinct act through the changed head
    let res2 = replay(&mut r, &caller, "run-c", &[src]);
    let again = change(&r, &new_of(&res2, &src));
    assert_ne!(new_of(&res2, &src), new);
    assert_eq!(
        again.links.client_nonce,
        Some(format!("replay:{}:0", res.head))
    );
}

/// The landed change that produced canon's version of `id`.
fn p_seed(r: &Rdy, id: &str) -> Hash {
    let v = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .version_of(&BlockId::parse(id).unwrap())
        .unwrap();
    r.read().reader().producer(&v).unwrap().unwrap()
}

#[test]
fn ok7_replay_divergence_records_every_op_case() {
    let mut p = project();
    let r = &mut p.r;
    // a landed create: the create-collision source
    let k = write(
        r,
        &human(),
        "kview",
        json!([{"op": "create", "type": "prose", "id": "$k", "parent": p.one, "body": "Text k."}]),
    );
    let s_new = h(&k.change);
    let kid = k.created["$k"].clone();
    land(r, "kview");
    let canon = canon_version;
    let (cx, cy, cz, cw, ck) = (
        canon(r, &p.x),
        canon(r, &p.y),
        canon(r, &p.z),
        canon(r, &p.w),
        canon(r, &kid),
    );
    // the sources, each on canon's base
    let s_upd = update(r, &human(), "src", &p.x, "x src");
    let s_del = h(&write(r, &human(), "src", json!([{"op": "delete", "id": p.y}])).change);
    let s_miss = update(r, &human(), "src", &p.z, "z src");
    let s_eq = update(r, &human(), "src", &p.w, "w same");
    // a replay of the landed creator into a view that still holds its version: the result is
    // the target's version, so no conflict
    new_view(r, &human(), "same");
    let res = replay(r, &human(), "same", &[s_new]);
    assert!(res.conflicts.is_empty(), "{:?}", res.conflicts);
    assert_eq!(version(r, "same", &kid), ck);
    // the target diverges on every block
    new_view(r, &human(), "t");
    let t_upd = update(r, &human(), "t", &p.x, "x t");
    let t_y = update(r, &human(), "t", &p.y, "y t");
    let t_miss = h(&write(r, &human(), "t", json!([{"op": "delete", "id": p.z}])).change);
    let t_eq = update(r, &human(), "t", &p.w, "w same");
    let t_k = update(r, &human(), "t", &kid, "k t");
    assert_eq!(version(r, "t", &p.w), version(r, "src", &p.w));
    let res = replay(r, &human(), "t", &[s_upd, s_del, s_miss, s_eq, s_new]);
    let rn = |s: &Hash| Some(new_of(&res, s));
    let src_v = |r: &Rdy, id: &str| version(r, "src", id);
    let want: BTreeMap<String, Shape> = [
        // an update on another base
        shape(
            &p.x,
            cx,
            &[
                (Some(t_upd), Some(change_version(r, &t_upd))),
                (rn(&s_upd), src_v(r, &p.x)),
            ],
        ),
        // a delete on another base: a null replay side
        shape(
            &p.y,
            cy,
            &[
                (Some(t_y), Some(change_version(r, &t_y))),
                (rn(&s_del), None),
            ],
        ),
        // an update of a block the target deleted: a null target side
        shape(
            &p.z,
            cz,
            &[(Some(t_miss), None), (rn(&s_miss), src_v(r, &p.z))],
        ),
        // a create of an id the target holds (at another version)
        shape(
            &kid,
            None,
            &[(Some(t_k), Some(change_version(r, &t_k))), (rn(&s_new), ck)],
        ),
    ]
    .into_iter()
    .map(|s| (s.0.clone(), s))
    .collect();
    assert_eq!(shapes(&res.conflicts), want);
    for c in &res.conflicts {
        assert_eq!(c.id, recipe_id(c), "{}", c.block);
    }
    // w: another base (canon's) but the result the target holds (t_eq's): no conflict, and w
    // keeps that version (ruling task-7-round1); x above is the same base with another result
    assert_ne!(cw, version(r, "t", &p.w));
    assert_eq!(version(r, "t", &p.w), src_v(r, &p.w));
    // the conflicts are the view's: a read agrees, and a conflicted block reads as its base
    let read = r.read().conflicts(&Scope::View("t".into())).unwrap();
    assert_eq!(read.len(), 4);
    assert_eq!(version(r, "t", &p.x), cx);
    // the target's own changes are untouched
    for c in [t_upd, t_y, t_miss, t_eq, t_k] {
        assert!(included(r, "t").contains(&c));
    }
}

/// The version the single op of change `c` writes.
fn change_version(r: &Rdy, c: &Hash) -> Hash {
    match &change(r, c).ops[..] {
        [Op::Update { block, .. }] | [Op::Create { block }] => block.version_hash(),
        ops => panic!("{ops:?}"),
    }
}

#[test]
fn ok7_replay_chain_does_not_resolve_target_conflicts() {
    let mut p = project();
    let r = &mut p.r;
    let cx = canon_version(r, &p.x);
    let cy = canon_version(r, &p.y);
    // a selected chain on x, and two independent sources on y
    let c1 = update(r, &human(), "src", &p.x, "x one");
    let c2 = update(r, &human(), "src", &p.x, "x two");
    let a = update(r, &human(), "sa", &p.y, "y a");
    let b = update(r, &human(), "sb", &p.y, "y b");
    // the target diverges on x and already holds a conflict on y
    new_view(r, &human(), "t");
    let t_x = update(r, &human(), "t", &p.x, "x t");
    let v1 = update(r, &human(), "v1", &p.y, "y v1");
    let v2 = update(r, &human(), "v2", &p.y, "y v2");
    r.view_include(&human(), "t", &[v1.to_string(), v2.to_string()])
        .unwrap();
    assert_eq!(
        r.read().conflicts(&Scope::View("t".into())).unwrap().len(),
        1
    );
    let res = replay(r, &human(), "t", &[c2, b, c1, a]);
    let (r1, r2, ra, rb) = (
        new_of(&res, &c1),
        new_of(&res, &c2),
        new_of(&res, &a),
        new_of(&res, &b),
    );
    // each replay on y depends on the latest earlier one there, never on the target's sides
    let (first, second) = if res.replayed.iter().position(|x| h(&x.change) == ra)
        < res.replayed.iter().position(|x| h(&x.change) == rb)
    {
        (ra, rb)
    } else {
        (rb, ra)
    };
    assert_eq!(change(r, &second).deps, vec![first]);
    assert!(change(r, &first).deps.is_empty());
    assert_eq!(change(r, &r2).deps, vec![r1]);
    // every side stays: the target's and each replay's
    let got = shapes(&res.conflicts);
    let xv = |c: &Hash| Some(change_version(r, c));
    assert_eq!(
        got[&p.x],
        shape(
            &p.x,
            cx,
            &[
                (Some(t_x), xv(&t_x)),
                (Some(r1), xv(&r1)),
                (Some(r2), xv(&r2))
            ]
        )
    );
    assert_eq!(
        got[&p.y],
        shape(
            &p.y,
            cy,
            &[
                (Some(v1), xv(&v1)),
                (Some(v2), xv(&v2)),
                (Some(ra), xv(&ra)),
                (Some(rb), xv(&rb))
            ]
        )
    );
    // an explicit write that covers every side resolves each, one block at a time
    let resolve = |r: &mut Rdy, id: &str, base: Option<Hash>, body: &str| {
        write(
            r,
            &human(),
            "t",
            json!([{"op": "update", "id": id, "base": base.unwrap().to_string(), "body": body}]),
        )
    };
    resolve(r, &p.x, cx, "x resolved");
    let left = r.read().conflicts(&Scope::View("t".into())).unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].block.to_string(), p.y);
    resolve(r, &p.y, cy, "y resolved");
    assert!(
        r.read()
            .conflicts(&Scope::View("t".into()))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn ok7_replay_missing_parent_is_atomic() {
    let mut p = project();
    let r = &mut p.r;
    // a doc only view src has, then a source that writes x and puts a prose under that doc
    let s0 = write(
        r,
        &human(),
        "src",
        json!([{"op": "create", "type": "doc", "id": "$d", "fields": {"name": "three", "title": "Three", "doc_type": "page"}}]),
    );
    let three = s0.created["$d"].clone();
    let s1 = update(r, &human(), "src", &p.x, "x one");
    let s2 = h(&write(
        r,
        &human(),
        "src",
        json!([
            {"op": "update", "id": p.x, "body": "x two"},
            {"op": "create", "type": "prose", "id": "$n", "parent": three, "body": "Under three."}
        ]),
    )
    .change);
    assert!(change(r, &s2).deps.contains(&s1));
    new_view(r, &human(), "t");
    update(r, &human(), "t", &p.y, "y t");
    let before = log_state(r);
    let inc = included(r, "t");
    let head = r.read().view_head("t").unwrap().clone();
    let e = replay_err(r, &human(), "t", &[s1.to_string(), s2.to_string()]);
    assert_eq!(e.code, E_DEPS_MISSING);
    assert_eq!(
        e.message,
        format!("replay needs parent {three} in view \"t\"")
    );
    assert_eq!(
        e.fix,
        "create the parent in the target view, then replay again"
    );
    assert_eq!(log_state(r), before);
    assert_eq!(included(r, "t"), inc);
    assert_eq!(r.read().view_head("t").unwrap(), &head);
    // a parent on a conflict side does not count: one source diverges on doc one and puts a
    // prose under it
    update_fields(r, "t", &p.one, "One, in t");
    let s3 = h(&write(
        r,
        &human(),
        "src2",
        json!([
            {"op": "update", "id": p.one, "fields": {"title": "One, in src2"}},
            {"op": "create", "type": "prose", "id": "$m", "parent": p.one, "body": "Under one."}
        ]),
    )
    .change);
    let before = log_state(r);
    let e = replay_err(r, &human(), "t", &[s3.to_string()]);
    assert_eq!(e.code, E_DEPS_MISSING);
    assert_eq!(
        e.message,
        format!("replay needs parent {} in view \"t\"", p.one)
    );
    assert_eq!(log_state(r), before);
    // with the parent in the target, the same replay goes through
    r.view_include(&human(), "t", std::slice::from_ref(&s0.change))
        .unwrap();
    let res = replay(r, &human(), "t", &[s1, s2]);
    assert_eq!(res.replayed.len(), 2);
}

#[test]
fn ok7_replay_parent_created_by_the_same_call() {
    // ruling task-7 item 1: one write creates doc d, the next puts a prose under it, and the
    // second does not depend on the first. A replay of both succeeds whichever sorts first by
    // hash: the parent rule reads the whole call, as include's does
    let mut p = project();
    let r = &mut p.r;
    let mut orders = BTreeSet::new();
    for n in 0..64 {
        if orders.len() == 2 {
            break;
        }
        let src = format!("src{n}");
        let d = write(
            r,
            &human(),
            &src,
            json!([{"op": "create", "type": "doc", "id": "$d", "fields": {"name": format!("d{n}"), "title": "D", "doc_type": "page"}}]),
        );
        let did = d.created["$d"].clone();
        let w1 = h(&d.change);
        let c = write(
            r,
            &human(),
            &src,
            json!([{"op": "create", "type": "prose", "id": "$n", "parent": did, "body": format!("Under d, {n}.")}]),
        );
        let w2 = h(&c.change);
        assert!(!change(r, &w2).deps.contains(&w1));
        let t = format!("t{n}");
        new_view(r, &human(), &t);
        let res = replay(r, &human(), &t, &[w1, w2]);
        assert!(res.conflicts.is_empty(), "{:?}", res.conflicts);
        let prose = r
            .read()
            .block(
                &Scope::View(t.clone()),
                &BlockId::parse(&c.created["$n"]).unwrap(),
            )
            .unwrap();
        assert_eq!(prose.parent, Some(BlockId::parse(&did).unwrap()));
        orders.insert(w2 < w1);
        // a parent that an unselected change creates is still refused, with the exact text
        let u = format!("u{n}");
        new_view(r, &human(), &u);
        let before = log_state(r);
        let e = replay_err(r, &human(), &u, &[w2.to_string()]);
        assert_eq!(e.code, E_DEPS_MISSING);
        assert_eq!(
            e.message,
            format!("replay needs parent {did} in view \"{u}\"")
        );
        assert_eq!(
            e.fix,
            "create the parent in the target view, then replay again"
        );
        assert_eq!(log_state(r), before);
    }
    // both hash orders ran
    assert_eq!(orders.len(), 2);
}

#[test]
fn ok7_replay_and_its_source_never_conflict() {
    // ruling task-7 item 2, as ruling task-7-round1 restates it: source S updates x (u to v) in
    // view a and is replayed into b as R. S and R reach one result, so no later merge records a
    // conflict between them
    let mut p = project();
    let r = &mut p.r;
    let s = update(r, &human(), "a", &p.x, "x in a");
    new_view(r, &human(), "b");
    new_view(r, &human(), "c");
    let inflight = |r: &Rdy| r.read().conflicts(&Scope::Inflight).unwrap().len();
    assert_eq!(inflight(r), 0);
    let res = replay(r, &human(), "b", &[s]);
    assert!(res.conflicts.is_empty(), "{:?}", res.conflicts);
    let res = replay(r, &human(), "c", &[s]);
    assert!(res.conflicts.is_empty(), "{:?}", res.conflicts);
    // (1) the in-flight scope merges S and both replays: no conflict
    assert_eq!(inflight(r), 0);
    // (2) an include of S into c beside its replay: no conflict on x
    r.view_include(&human(), "c", &[s.to_string()]).unwrap();
    let conflicts = |r: &Rdy, v: &str| r.read().conflicts(&Scope::View(v.into())).unwrap().len();
    assert_eq!(conflicts(r, "c"), 0);
    assert_eq!(version(r, "c", &p.x), version(r, "a", &p.x));
    // (3) after view a lands, b and c hold no conflict on x, and b can land
    update(r, &human(), "b", &p.y, "y in b");
    land(r, "a");
    assert_eq!(conflicts(r, "b"), 0);
    assert_eq!(conflicts(r, "c"), 0);
    assert_eq!(inflight(r), 0);
    land(r, "b");
    assert_eq!(canon_version(r, &p.x), version(r, "c", &p.x));
    assert_eq!(canon_version(r, &p.y), version(r, "b", &p.y));
    // a landed doc and, unlanded, a prose under it, replayed into a view that holds the doc:
    // the doc's create reaches the version the view holds and agrees, so the prose finds its
    // parent there
    let d = write(
        r,
        &human(),
        "a3",
        json!([{"op": "create", "type": "doc", "id": "$d", "fields": {"name": "three", "title": "Three", "doc_type": "page"}}]),
    );
    let did = d.created["$d"].clone();
    land(r, "a3");
    let n = write(
        r,
        &human(),
        "a4",
        json!([{"op": "create", "type": "prose", "id": "$n", "parent": did, "body": "Under three."}]),
    );
    new_view(r, &human(), "b3");
    let res = replay(r, &human(), "b3", &[h(&d.change), h(&n.change)]);
    assert!(res.conflicts.is_empty(), "{:?}", res.conflicts);
    assert_eq!(
        r.read()
            .block(
                &Scope::View("b3".into()),
                &BlockId::parse(&n.created["$n"]).unwrap()
            )
            .unwrap()
            .parent,
        Some(BlockId::parse(&did).unwrap())
    );
    // a landed delete, replayed into a view whose base no longer holds the block: it agrees
    let del = h(&write(r, &human(), "a5", json!([{"op": "delete", "id": p.y}])).change);
    land(r, "a5");
    new_view(r, &human(), "b5");
    let res = replay(r, &human(), "b5", &[del]);
    assert!(res.conflicts.is_empty(), "{:?}", res.conflicts);
    assert_eq!(version(r, "b5", &p.y), None);
    // and the other way round: the replay lands first, then its source's view lands
    let s2 = update(r, &human(), "a2", &p.z, "z in a2");
    new_view(r, &human(), "b2");
    let res = replay(r, &human(), "b2", &[s2]);
    assert!(res.conflicts.is_empty(), "{:?}", res.conflicts);
    land(r, "b2");
    assert_eq!(conflicts(r, "a2"), 0);
    update(r, &human(), "a2", &p.w, "w in a2");
    land(r, "a2");
    assert_eq!(canon_version(r, &p.z), version(r, "a2", &p.z));
}

#[test]
fn ok7_two_replays_of_one_source_agree_after_one_lands() {
    // ruling task-7-round1, case a: S updates x in view a and is replayed into b (as r1) and
    // into c (as r2). b lands before S does, so canon's x is r1's. c then holds r2 against r1:
    // one result, so no conflict, and c lands
    let mut p = project();
    let r = &mut p.r;
    let s = update(r, &human(), "a", &p.x, "x in a");
    new_view(r, &human(), "b");
    new_view(r, &human(), "c");
    assert!(replay(r, &human(), "b", &[s]).conflicts.is_empty());
    assert!(replay(r, &human(), "c", &[s]).conflicts.is_empty());
    update(r, &human(), "c", &p.y, "y in c");
    let conflicts = |r: &Rdy, v: &str| r.read().conflicts(&Scope::View(v.into())).unwrap().len();
    let inflight = |r: &Rdy| r.read().conflicts(&Scope::Inflight).unwrap().len();
    land(r, "b");
    assert_eq!(canon_version(r, &p.x), version(r, "a", &p.x));
    assert_eq!(conflicts(r, "c"), 0);
    assert_eq!(conflicts(r, "a"), 0);
    assert_eq!(inflight(r), 0);
    land(r, "c");
    assert_eq!(canon_version(r, &p.x), version(r, "a", &p.x));
    assert_eq!(canon_version(r, &p.y), version(r, "c", &p.y));
    // and S's own view still lands after both
    update(r, &human(), "a", &p.z, "z in a");
    assert_eq!(conflicts(r, "a"), 0);
    land(r, "a");
    assert_eq!(canon_version(r, &p.z), version(r, "a", &p.z));
}

#[test]
fn ok7_replay_equal_result_agrees_in_any_hash_order() {
    // view t holds r0, a replay that moved x from canon's u to v. A second source reached v
    // from another base u2 and is replayed into t: its result is t's version, so it agrees. It
    // merges after r0 (its writer of v) whichever sorts first by hash, so no read of t ever
    // sees v against v
    let mut p = project();
    let r = &mut p.r;
    let mut orders = BTreeSet::new();
    for n in 0..64 {
        if orders.len() == 2 {
            break;
        }
        let body = format!("x v{n}");
        let s0 = update(r, &human(), &format!("a{n}"), &p.x, &body);
        let t = format!("t{n}");
        new_view(r, &human(), &t);
        let r0 = new_of(&replay(r, &human(), &t, &[s0]), &s0);
        let other = format!("s{n}");
        update(r, &human(), &other, &p.x, &format!("x u2 {n}"));
        let s1 = update(r, &human(), &other, &p.x, &body);
        assert_eq!(version(r, &other, &p.x), version(r, &t, &p.x));
        let res = replay(r, &human(), &t, &[s1]);
        assert!(res.conflicts.is_empty(), "{:?}", res.conflicts);
        let r1 = new_of(&res, &s1);
        assert!(change(r, &r1).deps.contains(&r0));
        assert!(!change(r, &r1).deps.contains(&s1));
        assert!(
            r.read()
                .conflicts(&Scope::View(t.clone()))
                .unwrap()
                .is_empty()
        );
        assert_eq!(version(r, &t, &p.x), version(r, &other, &p.x));
        orders.insert(r1 < r0);
    }
    // both hash orders ran
    assert_eq!(orders.len(), 2);
}

fn update_fields(r: &mut Rdy, view: &str, id: &str, title: &str) {
    write(
        r,
        &human(),
        view,
        json!([{"op": "update", "id": id, "fields": {"title": title}}]),
    );
}

#[test]
fn ok7_replay_append_failures_change_nothing() {
    // a failure at each frame of the replay's one batch (its changes, blocks, include, head
    // ref, moves and marker), and at its sync, leaves the log's refs, the sources and canon as
    // they were; the replay then succeeds whole
    fn ready() -> (tempfile::TempDir, Rdy, Hash, Hash) {
        let P {
            dir, mut r, x, y, ..
        } = project();
        let a = update(&mut r, &human(), "src", &x, "x a");
        let b = update(&mut r, &human(), "src", &y, "y b");
        new_view(&mut r, &human(), "t");
        (dir, r, a, b)
    }
    let (_d0, mut r0, a, b) = ready();
    let start = r0.store().log_end();
    replay(&mut r0, &human(), "t", &[a, b]);
    let mut kinds_seen = vec![];
    r0.store()
        .reader()
        .for_each_frame_from(start, r0.store().log_end(), |_, kind, _, _, _| {
            kinds_seen.push(rdy_store::codec::logical_kind(kind));
            Ok(())
        })
        .unwrap();
    // two changes with their blocks, the include, the head ref and the batch marker at least
    assert!(kinds_seen.len() >= 6, "{kinds_seen:?}");
    assert_eq!(
        kinds_seen
            .iter()
            .filter(|k| **k == kinds::BATCH_COMMIT)
            .count(),
        1,
        "one batch: {kinds_seen:?}"
    );
    for k in 0..=kinds_seen.len() {
        let (dir, mut r, a, b) = ready();
        let before = log_state(&r);
        let src = included(&r, "src");
        let canon = r.read().canon_head();
        if k < kinds_seen.len() {
            r.store_mut().fail_append_at_frame_for_tests(Some((k, 3)));
        } else {
            r.store_mut().fail_next_sync();
        }
        let e = r.view_replay(&human(), "t", &[a.to_string(), b.to_string()]);
        assert!(e.is_err(), "frame {k}");
        drop(r);
        let mut r = Rdy::open(dir.path()).unwrap();
        assert_eq!(log_state(&r).1, before.1, "frame {k}: refs");
        assert_eq!(r.read().canon_head(), canon, "frame {k}");
        assert_eq!(included(&r, "src"), src, "frame {k}");
        assert!(included(&r, "t").is_empty(), "frame {k}");
        r.fsck().unwrap();
        let res = replay(&mut r, &human(), "t", &[a, b]);
        assert_eq!(res.replayed.len(), 2, "frame {k}");
    }
}

#[test]
fn ok7_replay_owned_open_view_only() {
    let mut p = project();
    let r = &mut p.r;
    let c = update(r, &human(), "src", &p.x, "x c");
    let cs = vec![c.to_string()];
    let owner_text = "replay needs a view owned by this session";
    // absent
    let e = replay_err(r, &human(), "nope", &cs);
    assert_eq!(
        (e.code, e.message.as_str()),
        (E_NOT_FOUND, "no view \"nope\"")
    );
    // unowned
    new_view(r, &human(), "u");
    r.view_own(&founder(), "u", None).unwrap();
    assert!(r.read().view_head("u").unwrap().owner.is_none());
    let e = replay_err(r, &human(), "u", &cs);
    assert_eq!((e.code, e.message.as_str()), (E_VIEW_OWNER, owner_text));
    assert_eq!(
        e.fix,
        "use your own view, or ask the owner to hand it to you"
    );
    let e = replay_err(r, &agent("run-a"), "u", &cs);
    assert_eq!(e.code, E_VIEW_OWNER);
    // foreign-owned
    new_view(r, &agent("run-a"), "run-a");
    let e = replay_err(r, &human(), "run-a", &cs);
    assert_eq!((e.code, e.message.as_str()), (E_VIEW_OWNER, owner_text));
    let e = replay_err(r, &agent("run-b"), "run-a", &cs);
    assert_eq!(e.code, E_VIEW_OWNER);
    // closed: the closed-view refusal include gives
    new_view(r, &human(), "shut");
    r.view_close(&human(), &["shut".to_string()], "done")
        .unwrap();
    let e = replay_err(r, &human(), "shut", &cs);
    let inc = r.view_include(&human(), "shut", &cs).unwrap_err();
    assert_eq!(
        (e.code, &e.message, &e.fix),
        (inc.code, &inc.message, &inc.fix)
    );
    // a reader
    let reader = Session::human(HUMAN.into(), Role::Reader).unwrap();
    let e = replay_err(r, &reader, "t-none", &cs);
    assert_eq!(e.code, E_PERMISSION);
    assert_eq!(e.message, "role Reader cannot replay");
    // nothing above wrote
    let before = log_state(r);
    let _ = replay_err(r, &human(), "u", &cs);
    assert_eq!(log_state(r), before);
    // the human's work view, owned by the normalized cli owner: a direct and a classified
    // human session (with another valid run) both replay
    update(r, &human(), "work", &p.z, "z work");
    let classified = classify(HUMAN, Some(HUMAN), Some("another-run"), Role::Writer).unwrap();
    assert_eq!(classified.run.as_deref(), Some("cli"));
    let work_owner = r.read().view_head("work").unwrap().owner.clone().unwrap();
    assert_eq!(
        (work_owner.actor.as_str(), work_owner.run.as_deref()),
        (HUMAN, Some("cli"))
    );
    replay(r, &human(), "work", &[c]);
    replay(r, &classified, "work", &[c]);
    // an agent never replays into work
    let e = replay_err(r, &agent("run-a"), "work", &cs);
    assert_eq!(e.code, E_VIEW_OWNER);
    assert_eq!(e.message, "view \"work\" is the human's default view");
    // an agent replays into its own view
    replay(r, &agent("run-a"), "run-a", &[c]);
}

#[test]
fn ok7_one_undo_removes_the_whole_replay() {
    let mut p = project();
    let r = &mut p.r;
    let a = update(r, &human(), "src", &p.x, "x a");
    let b = update(r, &human(), "src", &p.y, "y b");
    new_view(r, &human(), "t");
    let res = replay(r, &human(), "t", &[a, b]);
    let news = included(r, "t");
    assert_eq!(news.len(), 2);
    // one undo of the Include removes both
    let u = r.undo(&human(), "t", Some(&res.head), false).unwrap();
    assert_eq!(u.undone, res.head);
    assert_eq!(u.kind, "include");
    assert_eq!(
        sorted(u.excluded.iter().map(|c| h(c)).collect()),
        sorted(news.clone())
    );
    assert!(included(r, "t").is_empty());
    // the undo of that undo puts both back
    let uu = r.undo(&human(), "t", Some(&u.op), false).unwrap();
    assert_eq!(
        sorted(uu.included.iter().map(|c| h(c)).collect()),
        sorted(news.clone())
    );
    assert_eq!(included(r, "t"), news);
    // the normal cascade rule: a write on a replay's result blocks the undo, or goes with it
    let ra = new_of(&res, &a);
    let w = update(r, &human(), "t", &p.x, "x after the replay");
    assert!(change(r, &w).deps.contains(&ra));
    let e = r.undo(&human(), "t", Some(&res.head), false).unwrap_err();
    assert_eq!(e.code, E_UNDO_BLOCKED);
    let u = r.undo(&human(), "t", Some(&res.head), true).unwrap();
    assert_eq!(u.cascaded, vec![w.to_string()]);
    assert!(included(r, "t").is_empty());
    // the landed-work rule: once the replay lands, its undo is refused
    new_view(r, &human(), "t2");
    let res = replay(r, &human(), "t2", &[a, b]);
    land(r, "t2");
    let e = r.undo(&human(), "t2", Some(&res.head), false).unwrap_err();
    assert_eq!(e.code, E_UNDO_LANDING);
    // the sources and their view are as they were
    assert!(included(r, "src").contains(&a) && included(r, "src").contains(&b));
}

fn incoming(_: &mut dyn ObjectSource, _: &UploadFacts) -> Result<TagChoice, StreamError> {
    Ok(TagChoice::Incoming)
}

#[test]
fn ok7_replay_survives_reindex_dump_and_take() {
    let mut p = project();
    let r = &mut p.r;
    // view t holds a replay conflict on x and a clean replay of y; view c holds a clean replay
    // only, to land and tag
    let sx = update(r, &human(), "src", &p.x, "x src");
    let sy = update(r, &human(), "src", &p.y, "y src");
    new_view(r, &human(), "t");
    update(r, &human(), "t", &p.x, "x t");
    let res = replay(r, &human(), "t", &[sx, sy]);
    assert_eq!(res.conflicts.len(), 1);
    new_view(r, &human(), "c");
    let landed = replay(r, &human(), "c", &[sy]);
    let ry = new_of(&landed, &sy);
    land(r, "c");
    r.tag(&founder(), "v1.0.0", None, None).unwrap();
    // the conflicts as the view now records them (the landing moved canon under it)
    let ids: Vec<String> = r
        .read()
        .conflicts(&Scope::View("t".into()))
        .unwrap()
        .iter()
        .map(|c| c.id.to_string())
        .collect();
    assert!(!ids.is_empty());
    let news: Vec<Hash> = res.replayed.iter().map(|x| h(&x.change)).collect();
    let frames = |r: &Rdy| -> Vec<(u8, Vec<u8>)> {
        news.iter()
            .chain([&ry])
            .map(|c| r.read().reader().get_kind_raw(kinds::CHANGE, c).unwrap())
            .collect()
    };
    let check = |r: &Rdy, what: &str| {
        let got: Vec<String> = r
            .read()
            .conflicts(&Scope::View("t".into()))
            .unwrap()
            .iter()
            .map(|c| c.id.to_string())
            .collect();
        assert_eq!(got, ids, "{what}");
        for x in &res.replayed {
            assert_eq!(
                change(r, &h(&x.change)).links.replayed_from,
                Some(h(&x.source)),
                "{what}"
            );
        }
        assert_eq!(
            sorted(res.replayed.iter().map(|x| h(&x.source)).collect()),
            sorted(vec![sx, sy])
        );
        assert_eq!(change(r, &ry).links.replayed_from, Some(sy), "{what}");
    };
    let before = frames(r);
    check(r, "before");
    // reindex
    r.reindex().unwrap();
    check(r, "reindex");
    assert_eq!(frames(r), before);
    // dump and load
    let file = p.dir.path().join("all.rdydump");
    r.read().dump(&file).unwrap();
    let loaded_dir = p.dir.path().join("loaded");
    load::load(&file, &loaded_dir).unwrap();
    let loaded = Rdy::open(&loaded_dir).unwrap();
    check(&loaded, "load");
    assert_eq!(frames(&loaded), before);
    loaded.fsck().unwrap();
    drop(loaded);
    // take: a holder of the tag's closure keeps the landed replay and its source link
    let src_store = r.store();
    let reader = src_store.reader();
    let tag = reader
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == "v1.0.0")
        .unwrap();
    let cl = closure(&reader, tag.1, &BTreeSet::new()).unwrap();
    let repo = reader.repo_id().unwrap().unwrap();
    let header = StreamHeader::tag_history(
        reader.store_features().unwrap(),
        repo,
        tag.1,
        cl.prerequisites.clone(),
    );
    let sync = p.dir.path().join("v1.rdysync");
    let mut source = StoreSource {
        reader: &reader,
        end: END_OF_LOG,
    };
    write_batch(
        &mut source,
        &header,
        &cl.records,
        File::create(&sync).unwrap(),
    )
    .unwrap();
    let mut holder = rdy_store::Store::create_holder(&p.dir.path().join("holder"), repo).unwrap();
    let target = UploadTarget {
        tag: "v1.0.0".into(),
        landing: tag.2.landing,
    };
    holder
        .take_batches(&[File::open(&sync).unwrap()], &target, incoming)
        .unwrap_or_else(|e| panic!("take: {e:?}"));
    let held = holder.reader().change(&ry).unwrap();
    assert_eq!(held.links.replayed_from, Some(sy));
    assert_eq!(
        holder.reader().get_kind_raw(kinds::CHANGE, &ry).unwrap(),
        reader.get_kind_raw(kinds::CHANGE, &ry).unwrap()
    );
    // the source is a carried reference of the replay, so the holder holds it too
    assert!(holder.reader().has_kind(kinds::CHANGE, &sy).unwrap());
}

/// Block id `zq-<n in Crockford base32>`.
fn numbered(mut n: usize) -> BlockId {
    let mut body = [b'0'; 5];
    for at in (0..5).rev() {
        body[at] = rdy_model::CROCKFORD[n % 32];
        n /= 32;
    }
    BlockId::parse(&format!("zq-{}", std::str::from_utf8(&body).unwrap())).unwrap()
}

/// A project with `n` sources in view `big`, each of `ops` creates of prose bodies of `body`
/// bytes, written straight to the store; the project is reopened.
fn big_sources(n: usize, ops: usize, body: usize) -> (P, Vec<Hash>) {
    let P {
        dir,
        mut r,
        seed,
        one,
        x,
        y,
        z,
        w,
    } = project();
    let parent = BlockId::parse(&one).unwrap();
    let template = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .get(&BlockId::parse(&x).unwrap())
        .unwrap()
        .clone();
    let mut out = vec![];
    for i in 0..n {
        let ops: Vec<Op> = (0..ops)
            .map(|j| {
                let mut b = template.clone();
                b.id = numbered(i * 10_000 + j);
                b.parent = Some(parent.clone());
                b.body = "b".repeat(body);
                b.read_identity = None;
                Op::Create { block: b }
            })
            .collect();
        let c = Change {
            view: "big".into(),
            ops,
            deps: vec![],
            provenance: human().provenance(),
            justification: None,
            message: format!("big {i}"),
            links: ChangeLinks::default(),
            read_identity: None,
        };
        out.push(r.store_mut().commit_to_view("big", c).unwrap());
    }
    r.close().unwrap();
    let r = Rdy::open(dir.path()).unwrap();
    (
        P {
            dir,
            r,
            seed,
            one,
            x,
            y,
            z,
            w,
        },
        out,
    )
}

#[test]
fn ok7_replay_limits_precede_allocation() {
    let limit = (
        E_LIMIT,
        "replay accepts at most 1000 changes, 10000 ops and 16777216 encoded bytes".to_string(),
        "replay a smaller set of changes".to_string(),
    );
    let as_tuple = |e: RdyError| (e.code, e.message, e.fix);
    // the input count, before duplicates collapse
    let mut p = project();
    let c = update(&mut p.r, &human(), "src", &p.x, "x c");
    new_view(&mut p.r, &human(), "t");
    let before = log_state(&p.r);
    let many = vec![c.to_string(); 1001];
    assert_eq!(as_tuple(replay_err(&mut p.r, &human(), "t", &many)), limit);
    assert_eq!(log_state(&p.r), before);
    let e = replay_err(&mut p.r, &human(), "t", &[]);
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_BAD_INPUT,
            "name at least one change to replay",
            "call rdy_view with op replay and changes, or rdy view replay <view> <change>..."
        )
    );
    // an id not in the full-hash form, and a change the store lacks
    let e = replay_err(&mut p.r, &human(), "t", &[c.to_string().to_uppercase()]);
    assert_eq!(e.code, E_BAD_INPUT);
    let e = replay_err(&mut p.r, &human(), "t", &[Hash::of(b"none").to_string()]);
    assert_eq!(e.code, E_NOT_FOUND);
    // exactly 1000 copies of one id are accepted
    let ok = vec![c.to_string(); 1000];
    assert_eq!(
        p.r.view_replay(&human(), "t", &ok).unwrap().replayed.len(),
        1
    );
    // the op count, over all sources
    let (mut p, big) = big_sources(2, 5_001, 1);
    new_view(&mut p.r, &human(), "t");
    let before = log_state(&p.r);
    let input: Vec<String> = big.iter().map(|c| c.to_string()).collect();
    assert_eq!(as_tuple(replay_err(&mut p.r, &human(), "t", &input)), limit);
    assert_eq!(log_state(&p.r), before);
    // exactly one source fits
    assert_eq!(
        p.r.view_replay(&human(), "t", &input[..1])
            .unwrap()
            .replayed
            .len(),
        1
    );
    // the encoded bytes of the new changes, over all of them
    let (mut p, big) = big_sources(2, 9, 1_000_000);
    new_view(&mut p.r, &human(), "t");
    let before = log_state(&p.r);
    let input: Vec<String> = big.iter().map(|c| c.to_string()).collect();
    assert_eq!(as_tuple(replay_err(&mut p.r, &human(), "t", &input)), limit);
    assert_eq!(log_state(&p.r), before);
    // a cycle among the selected sources (a content-addressed log holds none, so the order is
    // checked directly)
    let (a, b, c) = (Hash::of(b"a"), Hash::of(b"b"), Hash::of(b"c"));
    let cycle = (
        E_BAD_INPUT,
        "the selected replay changes contain a dependency cycle".to_string(),
        "choose changes with an acyclic dependency order".to_string(),
    );
    let e = replay_order_for_tests(&[(a, vec![b]), (b, vec![a]), (c, vec![])]).unwrap_err();
    assert_eq!(as_tuple(e), cycle);
    let e = replay_order_for_tests(&[(a, vec![a])]).unwrap_err();
    assert_eq!(as_tuple(e), cycle);
    // the order is Kahn's, ready set ascending, edges only inside the selection
    let order =
        replay_order_for_tests(&[(c, vec![]), (a, vec![c, Hash::of(b"x")]), (b, vec![])]).unwrap();
    // b and c are ready at once; a waits for c
    let want = match (c < b, a < b) {
        (true, true) => vec![c, a, b],
        (true, false) => vec![c, b, a],
        (false, _) => vec![b, c, a],
    };
    assert_eq!(order, want);
    // a removed source: the redaction refusal, nothing written
    let mut p = project();
    let secret = write(
        &mut p.r,
        &human(),
        "src",
        json!([{"op": "create", "type": "prose", "id": "$s", "parent": p.one, "body": "A secret."}]),
    );
    let s = h(&secret.change);
    let sid = BlockId::parse(&secret.created["$s"]).unwrap();
    new_view(&mut p.r, &human(), "t");
    p.r.close().unwrap();
    authorized_batch_fixture(
        &p.dir.path().join(".rdy/store"),
        &[RequestSpec::new(
            "redact",
            vec![RedactTarget::AllVersions(sid)],
        )],
        &FixtureOptions::approved(Proof::local_human()),
    );
    let mut r = Rdy::open(p.dir.path()).unwrap();
    let before = log_state(&r);
    let e = replay_err(&mut r, &human(), "t", &[s.to_string()]);
    assert_eq!(e.code, E_REDACTED, "{e}");
    assert_eq!(log_state(&r), before);
}
