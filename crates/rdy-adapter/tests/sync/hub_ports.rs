//! hpt3/hpr3/hpc3 (D-A3H7S, D-DZDB7, D-HX6BX, D-3JN9N, D-7AT4S): a hub carries landed changes of
//! one line into a new pushed view of another line of the same family, through the adapter.
//! `Snapshot::port_bundle` reads one holder; `StoreHandle::port_into_view` writes another. The
//! bundle travels as bytes between them, and no call opens both holders.
//!
//! The world: project P lands the seed `p1` (tagged v1.0.0) and later work; the main hub M takes
//! P's canon. The line L is M's real fork at v1.0.0. Project C, a copy of P at `p1` with L's repo
//! id, is L's client: its landings move L's canon. Projects are built with core calls and closed
//! before an adapter handle reads them.

use crate::hub_world::{AT, check, copy_dir, export, land, moved, project, repo_of, tag, write_in};
use rdy_adapter::port::{PortBundle, PortOutcome, PortedFrom, PortedView};
use rdy_adapter::stream::{ObjectKey, StreamPurpose};
use rdy_adapter::sync::CanonMove;
use rdy_adapter::{Hash, Origin, RdyError, RepoId, Snapshot, StoreHandle};
use rdy_engine::Rdy;
use rdy_engine::redaction::RedactInput;
use rdy_model::{Provenance, Role};
use rdy_store::LandRequest;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The person a carry names.
const PERSON: &str = "human:carrier";

// ---------------------------------------------------------------------------------------------
// the world

/// Main M, its line L (a fork at v1.0.0), and the ids of the seed. `m_tip` and `l_tip` follow
/// the canon moves this world makes.
struct Lines {
    dir: tempfile::TempDir,
    m: StoreHandle,
    l: StoreHandle,
    m_repo: RepoId,
    l_repo: RepoId,
    p1: Hash,
    m_tip: Hash,
    l_tip: Hash,
    /// P's canon history at `p1`.
    e1: PathBuf,
    /// The seed doc, its section `terms`, the prose `intro` and `secret` in it, and the sections
    /// `a` and `b` of the doc.
    doc: String,
    terms: String,
    intro: String,
    secret: String,
    a: String,
    b: String,
    /// The seed's link to the cited repo (alias `core`).
    link: String,
    exports: usize,
}

impl Lines {
    fn at(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn next_export(&mut self) -> PathBuf {
        self.exports += 1;
        self.at(&format!("x{}.rdysync", self.exports))
    }

    /// P runs `f` (writes and one landing, which it returns); M takes the landing.
    fn main_lands(&mut self, f: impl FnOnce(&mut Rdy) -> Hash) -> Hash {
        let out = self.next_export();
        let l = land_into(&self.at("p"), &self.m, self.m_tip, f, &out);
        self.m_tip = l;
        l
    }

    /// C runs `f` (writes and one landing, which it returns); L takes the landing.
    fn line_lands(&mut self, f: impl FnOnce(&mut Rdy) -> Hash) -> Hash {
        let out = self.next_export();
        let l = land_into(&self.at("c"), &self.l, self.l_tip, f, &out);
        self.l_tip = l;
        l
    }
}

/// Opens the project at `dir`, runs `f`, closes it, and moves `hub` from `tip` to the landing
/// `f` returns, through the export at `out`.
fn land_into(
    dir: &Path,
    hub: &StoreHandle,
    tip: Hash,
    f: impl FnOnce(&mut Rdy) -> Hash,
    out: &Path,
) -> Hash {
    let mut r = Rdy::open(dir).unwrap();
    let l = f(&mut r);
    r.close().unwrap();
    export(dir, StreamPurpose::CanonHistory, l, Some(tip), out);
    let s = hub.snapshot().unwrap();
    let c = check(&s, &[out]).unwrap();
    assert_eq!(
        hub.move_canon(&c, CanonMove::Extend, Some(tip)).unwrap(),
        moved(l)
    );
    l
}

/// Writes `ops` in view `view` of `r` and lands it.
fn write_land(r: &mut Rdy, view: &str, ops: Value) -> Hash {
    write_in(r, view, ops);
    land(r, view)
}

fn lines() -> Lines {
    let _clock = rdy_engine::pin_clock(AT);
    let dir = tempfile::tempdir().unwrap();
    let pd = dir.path().join("p");
    let mut p = project(&pd);
    let m_repo = repo_of(&p);
    let w = write_in(
        &mut p,
        "w0",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Intro", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": "Public words."},
            {"op": "create", "type": "prose", "id": "$x", "parent": "$s", "body": "The launch code is ZEBRA-7731."},
            {"op": "create", "type": "section", "id": "$a", "parent": "$d", "fields": {"title": "Section A"}},
            {"op": "create", "type": "section", "id": "$b", "parent": "$d", "fields": {"title": "Section B"}},
            {"op": "create", "type": "repo_link", "id": "$k", "parent": "$d",
                "fields": {"alias": "core", "repo": cited().to_string(), "profile": AGENTS}}
        ]),
    );
    let id = |k: &str| w.created[k].clone();
    let p1 = land(&mut p, "w0");
    tag(&mut p, "v1.0.0");
    p.close().unwrap();
    let cd = dir.path().join("c");
    copy_dir(&pd, &cd);
    let e1 = dir.path().join("e1.rdysync");
    export(&pd, StreamPurpose::CanonHistory, p1, None, &e1);
    let m = StoreHandle::create_holder(&dir.path().join("m"), m_repo).unwrap();
    let s = m.snapshot().unwrap();
    let c = check(&s, &[&e1]).unwrap();
    assert_eq!(m.move_canon(&c, CanonMove::Seed, None).unwrap(), moved(p1));
    drop((s, c));
    let ms = m.snapshot().unwrap();
    let l = ms.fork_at("v1.0.0", &dir.path().join("l")).unwrap();
    drop(ms);
    let l_repo = l.snapshot().unwrap().identity().repo_id;
    // C: L's client, a copy of P at p1 under L's repo id
    let mut c = Rdy::open(&cd).unwrap();
    c.store_mut()
        .put_origin(
            &Origin {
                fork: true,
                repo_id: m_repo,
                tag: Some("v1.0.0".into()),
                landing: Some(p1),
            },
            Some(l_repo),
        )
        .unwrap();
    c.close().unwrap();
    Lines {
        m,
        l,
        m_repo,
        l_repo,
        p1,
        m_tip: p1,
        l_tip: p1,
        e1,
        doc: id("$d"),
        terms: id("$s"),
        intro: id("$p"),
        secret: id("$x"),
        a: id("$a"),
        b: id("$b"),
        link: id("$k"),
        exports: 0,
        dir,
    }
}

/// Removes every version of the blocks `targets` of `r` in a landing of its own; returns it.
fn remove(r: &mut Rdy, targets: &[&str]) -> Hash {
    let s = crate::hub_world::session(Role::Writer);
    let req = r
        .redact(
            &s,
            RedactInput {
                targets: targets.iter().map(|t| t.to_string()).collect(),
                reason_class: "secret".into(),
                reason: "a leaked code".into(),
                dry_run: false,
            },
        )
        .unwrap();
    assert!(req.redaction.is_some());
    let p = r.propose(&s, "work", "Remove the code", None).unwrap();
    let version: Hash = p.id.parse().unwrap();
    r.seal_open().unwrap();
    r.store_mut().set_held(false).unwrap();
    r.store_mut()
        .land_atomic(LandRequest {
            proposal: version,
            accept: None,
            approver: Provenance {
                actor: "human:t".into(),
                run: None,
                role: Role::Approver,
                at_ms: 1_000_000,
            },
            alt: None,
            relay: None,
            evidence: None,
            assessment: None,
        })
        .unwrap();
    r.store_mut().set_held(true).unwrap();
    r.invalidate();
    r.store().reader().canon_head().unwrap().unwrap()
}

/// M takes P's removal of the secret (the move carries the removal).
fn main_removes_secret(w: &mut Lines) -> Hash {
    let secret = w.secret.clone();
    w.main_lands(|r| remove(r, &[&secret]))
}

/// What a write would change, read through a fresh snapshot: canon, every view head, the tags,
/// the feature names and the presence of `keys`.
type State = (
    Option<Hash>,
    Vec<(String, Hash)>,
    Vec<Hash>,
    Vec<String>,
    Vec<bool>,
);

fn state(h: &StoreHandle, keys: &[ObjectKey]) -> State {
    let s = h.snapshot().unwrap();
    (
        s.canon(),
        s.view_heads()
            .unwrap()
            .into_iter()
            .map(|v| (v.name, v.head))
            .collect(),
        s.tags().unwrap().into_iter().map(|(h, _)| h).collect(),
        s.features()
            .features
            .iter()
            .map(|f| f.name.clone())
            .collect(),
        s.has_objects(keys)
            .unwrap()
            .into_iter()
            .map(|p| p.present)
            .collect(),
    )
}

fn change_keys(changes: &[Hash]) -> Vec<ObjectKey> {
    changes
        .iter()
        .map(|h| ObjectKey {
            kind: rdy_store::kinds::CHANGE,
            hash: *h,
        })
        .collect()
}

fn bundle(snap: &Snapshot, landings: &[Hash]) -> PortBundle {
    snap.port_bundle(landings).unwrap()
}

/// The bundle of `landings` from a fresh snapshot of `h`.
fn bundle_of(h: &StoreHandle, landings: &[Hash]) -> PortBundle {
    bundle(&h.snapshot().unwrap(), landings)
}

fn created(out: PortOutcome) -> PortedView {
    match out {
        PortOutcome::Created(v) => v,
        other => panic!("not created: {other:?}"),
    }
}

fn refusal(e: &RdyError) -> (&str, &str, &str) {
    (e.code, e.message.as_str(), e.fix.as_str())
}

fn removed_refusal() -> (&'static str, &'static str, &'static str) {
    (
        "E_REDACTED_STATE",
        "the port bundle holds a version or blob removed in this line",
        "carry the change after the line's removal is replaced",
    )
}

fn family_refusal() -> (&'static str, &'static str, &'static str) {
    (
        "E_BAD_INPUT",
        "the port bundle comes from outside this line's family",
        "carry from a line of the same family",
    )
}

/// The ported sources of every change of `b`, in bundle order.
fn sources(b: &PortBundle) -> Vec<PortedFrom> {
    b.facts()
        .landings
        .iter()
        .flat_map(|l| &l.changes)
        .map(|c| PortedFrom {
            repo_id: b.facts().source,
            change: *c,
        })
        .collect()
}

/// The conflicted block ids of scope `scope` in a fresh snapshot of `h`.
fn conflicted(h: &StoreHandle, scope: &str) -> Vec<String> {
    let s = h.snapshot().unwrap();
    s.compare(None, scope)
        .unwrap()
        .conflicted
        .into_iter()
        .map(|e| e.id)
        .collect()
}

// ---------------------------------------------------------------------------------------------
// the carry

/// D-A3H7S, D-DZDB7: a carry writes one new pushed view on hub canon's tip, with one change per
/// source change, and makes no landing. The bundle is made at main and its handle closed before
/// the line takes the bytes. Canon moving before the call is the tip the view starts on. A clean
/// carry still waits for the person's land and push; a conflicted one reads its conflicts
/// through the ordinary view scopes, at the root and at the head.
#[test]
fn hpt3_carry_creates_a_view_without_landing() {
    let _clock = rdy_engine::pin_clock(AT);
    let mut w = lines();
    let (intro, terms) = (w.intro.clone(), w.terms.clone());
    let p2 = w.main_lands(|r| {
        write_in(
            r,
            "w1",
            json!([{"op": "update", "id": intro, "body": "Main words."}]),
        );
        write_land(
            r,
            "w1",
            json!([{"op": "create", "type": "prose", "parent": terms, "body": "A main note."}]),
        )
    });
    let p3 = w.main_lands(|r| {
        write_land(
            r,
            "w2",
            json!([
                {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Main two", "doc_type": "page"}},
                {"op": "create", "type": "prose", "parent": "$d", "body": "A second page."}
            ]),
        )
    });
    let intro = w.intro.clone();
    let l2 = w.line_lands(|r| {
        write_land(
            r,
            "l2",
            json!([{"op": "update", "id": intro, "body": "Line words."}]),
        )
    });

    // make at main, close it, take the bytes on the line
    let (clean, conflicting) = {
        let s = w.m.snapshot().unwrap();
        (
            bundle(&s, &[p3]).to_bytes().to_vec(),
            bundle(&s, &[p2]).to_bytes().to_vec(),
        )
    };
    let m = w.m;
    m.close().unwrap();
    let clean = PortBundle::from_bytes(&clean).unwrap();
    let conflicting = PortBundle::from_bytes(&conflicting).unwrap();
    assert_eq!(clean.facts().source, w.m_repo);

    // the line's canon moves after the bundle: the view starts on the tip at the writer's turn
    let l3 = {
        let out = w.dir.path().join("l3.rdysync");
        land_into(
            &w.dir.path().join("c"),
            &w.l,
            l2,
            |r| {
                write_land(
                    r,
                    "l3",
                    json!([{"op": "create", "type": "doc", "fields": {"title": "Line page", "doc_type": "page"}}]),
                )
            },
            &out,
        )
    };
    let before = w.l.snapshot().unwrap();
    let tags_before = before.tags().unwrap();

    let v = created(w.l.port_into_view(&clean, "carry", PERSON).unwrap());
    assert_eq!(v.view, "carry");
    assert_eq!(v.base, l3);
    assert!(v.conflicts.is_empty(), "{:?}", v.conflicts);
    assert_eq!(
        v.ported.iter().map(|p| p.source).collect::<Vec<_>>(),
        sources(&clean)
    );
    let news: Vec<Hash> = v.ported.iter().map(|p| p.change).collect();
    for (p, src) in v.ported.iter().zip(sources(&clean)) {
        assert_ne!(p.change, src.change, "a new change, never the source's");
    }

    // no landing, no tag, no hold; an old snapshot does not see the view
    let after = w.l.snapshot().unwrap();
    assert_eq!(after.canon(), Some(l3));
    assert_eq!(after.tags().unwrap(), tags_before);
    assert!(before.view_head("carry").unwrap().is_none());
    let head = after.view_head("carry").unwrap().unwrap();
    assert_eq!(
        (head.root, head.head, head.base, head.len),
        (v.root, v.head, Some(l3), news.len() as u64)
    );
    assert_eq!((head.owner, head.pinned, head.closed), (None, false, false));
    assert!(after.features().has("ported_change"));
    assert!(!before.features().has("ported_change"));
    assert!(
        after
            .has_objects(&change_keys(&news))
            .unwrap()
            .iter()
            .all(|p| p.present)
    );
    // the view reads through the ordinary scopes: the new page is added at the head
    let at_head = format!("view:carry@{}", v.head);
    let cmp = after.compare(Some("canon"), &at_head).unwrap();
    assert_eq!(cmp.added.len(), 2, "{cmp:?}");
    assert!(cmp.conflicted.is_empty());
    let at_root = format!("view:carry@{}", v.root);
    let cmp = after.compare(Some("canon"), &at_root).unwrap();
    assert!(cmp.added.is_empty() && cmp.changed.is_empty(), "{cmp:?}");
    // the landed facts of canon carry no port
    let facts = after.landing_facts(l3).unwrap();
    assert!(facts.ported_changes.is_empty());

    // a conflicting carry: the line edited the same prose
    let c = created(w.l.port_into_view(&conflicting, "carry-c", PERSON).unwrap());
    assert_eq!(c.base, l3);
    assert_eq!(c.conflicts.len(), 1);
    assert_eq!(c.conflicts[0].to_string(), w.intro);
    assert!(conflicted(&w.l, &format!("view:carry-c@{}", c.root)).is_empty());
    assert_eq!(
        conflicted(&w.l, &format!("view:carry-c@{}", c.head)),
        [w.intro.clone()]
    );
    let after = w.l.snapshot().unwrap();
    assert_eq!(after.canon(), Some(l3), "a conflict lands nothing either");
}

/// D-DZDB7: an existing view is `Behind` with its head and no write; of two racing creations
/// exactly one succeeds and the other is `Behind`.
#[test]
fn hpt3_existing_view_and_racing_writers_change_nothing() {
    let _clock = rdy_engine::pin_clock(AT);
    let mut w = lines();
    let p2 = w.main_lands(|r| {
        write_land(
            r,
            "w1",
            json!([{"op": "create", "type": "doc", "fields": {"title": "Main", "doc_type": "page"}}]),
        )
    });
    let b = bundle_of(&w.m, &[p2]);
    let first = created(w.l.port_into_view(&b, "carry", PERSON).unwrap());
    let keys = change_keys(&[first.ported[0].change]);
    let before = state(&w.l, &keys);
    assert_eq!(
        w.l.port_into_view(&b, "carry", PERSON).unwrap(),
        PortOutcome::Behind {
            head: Some(first.head)
        }
    );
    // another bundle under the same name is behind too
    let p3 = w.main_lands(|r| {
        write_land(
            r,
            "w2",
            json!([{"op": "create", "type": "doc", "fields": {"title": "Later", "doc_type": "page"}}]),
        )
    });
    let other = bundle_of(&w.m, &[p3]);
    assert_eq!(
        w.l.port_into_view(&other, "carry", PERSON).unwrap(),
        PortOutcome::Behind {
            head: Some(first.head)
        }
    );
    assert_eq!(state(&w.l, &keys), before);

    // two writers race for one new name
    let barrier = std::sync::Barrier::new(2);
    let outs: Vec<PortOutcome> = std::thread::scope(|s| {
        let runs: Vec<_> = [&b, &other]
            .into_iter()
            .map(|bundle| {
                let (l, barrier) = (&w.l, &barrier);
                s.spawn(move || {
                    barrier.wait();
                    l.port_into_view(bundle, "race", PERSON).unwrap()
                })
            })
            .collect();
        runs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let won: Vec<&PortedView> = outs
        .iter()
        .filter_map(|o| match o {
            PortOutcome::Created(v) => Some(v),
            PortOutcome::Behind { .. } => None,
        })
        .collect();
    assert_eq!(won.len(), 1, "{outs:?}");
    let lost: Vec<&PortOutcome> = outs
        .iter()
        .filter(|o| matches!(o, PortOutcome::Behind { .. }))
        .collect();
    assert_eq!(
        lost,
        [&PortOutcome::Behind {
            head: Some(won[0].head)
        }]
    );
    let s = w.l.snapshot().unwrap();
    assert_eq!(s.view_head("race").unwrap().unwrap().head, won[0].head);
}

/// D-HX6BX, D-CDQF7: a carry needs a counted creation origin in common. Main, a line, a sibling
/// line and a grandchild carry each way. An unrelated hub is refused both ways, also after it
/// records a late origin that names main. A store of main's own repo is the same store.
#[test]
fn hpt3_lineage_counts_only_creation_origins() {
    let _clock = rdy_engine::pin_clock(AT);
    let mut w = lines();
    let p2 = w.main_lands(|r| {
        write_land(
            r,
            "w1",
            json!([{"op": "create", "type": "doc", "fields": {"title": "Main", "doc_type": "page"}}]),
        )
    });
    let l2 = w.line_lands(|r| {
        write_land(
            r,
            "l2",
            json!([{"op": "create", "type": "doc", "fields": {"title": "Line", "doc_type": "page"}}]),
        )
    });
    let ms = w.m.snapshot().unwrap();
    let sibling = ms.fork_at("v1.0.0", &w.at("l-sibling")).unwrap();
    drop(ms);
    let ls = w.l.snapshot().unwrap();
    let grandchild = ls.fork_at("v1.0.0", &w.at("ll")).unwrap();
    drop(ls);
    assert_eq!(
        grandchild
            .snapshot()
            .unwrap()
            .identity()
            .origin
            .as_ref()
            .unwrap()
            .repo_id,
        w.l_repo
    );

    // an unrelated hub: P's words under another repo whose own origin names a third repo
    let ud = w.at("u");
    copy_dir(&w.at("p"), &ud);
    let urepo = RepoId([0x66; 16]);
    {
        let mut r = Rdy::open(&ud).unwrap();
        let elsewhere = Origin {
            fork: true,
            repo_id: RepoId([0x55; 16]),
            tag: None,
            landing: None,
        };
        r.store_mut().put_origin(&elsewhere, Some(urepo)).unwrap();
        r.close().unwrap();
    }
    let ue = w.at("u.rdysync");
    export(&ud, StreamPurpose::CanonHistory, w.p1, None, &ue);
    let udir = w.at("uh");
    let u = StoreHandle::create_holder(&udir, urepo).unwrap();
    let s = u.snapshot().unwrap();
    let c = check(&s, &[&ue]).unwrap();
    assert_eq!(
        u.move_canon(&c, CanonMove::Seed, None).unwrap(),
        moved(w.p1)
    );
    drop((s, c));
    // a second holder of main's own repo
    let twin = StoreHandle::create_holder(&w.at("m-twin"), w.m_repo).unwrap();
    let s = twin.snapshot().unwrap();
    let c = check(&s, &[&w.e1]).unwrap();
    assert_eq!(
        twin.move_canon(&c, CanonMove::Seed, None).unwrap(),
        moved(w.p1)
    );
    drop((s, c));

    // within the family, each way
    let from_m = bundle_of(&w.m, &[p2]);
    let from_l = bundle_of(&w.l, &[l2]);
    let from_ll = bundle_of(&grandchild, &[w.p1]);
    for (name, target, b) in [
        ("main-to-line", &w.l, &from_m),
        ("line-to-main", &w.m, &from_l),
        ("grandchild-to-main", &w.m, &from_ll),
        ("main-to-grandchild", &grandchild, &from_m),
        ("line-to-sibling", &sibling, &from_l),
        ("sibling-to-line", &w.l, &bundle_of(&sibling, &[w.p1])),
    ] {
        let v = created(target.port_into_view(b, name, PERSON).unwrap());
        assert_eq!(v.ported.len(), b.facts().changes, "{name}");
    }

    // outside the family, both ways, before and after a late origin
    let from_u = bundle_of(&u, &[w.p1]);
    let check_outside = |u: &StoreHandle, from_u: &PortBundle| {
        for (target, b) in [(&w.m, from_u), (u, &from_m), (&w.l, from_u), (u, &from_l)] {
            let before = state(target, &[]);
            let e = target.port_into_view(b, "outside", PERSON).unwrap_err();
            assert_eq!(refusal(&e), family_refusal());
            assert_eq!(state(target, &[]), before);
        }
    };
    check_outside(&u, &from_u);
    u.close().unwrap();
    {
        let mut r = Rdy::open_holder(&udir).unwrap();
        r.store_mut()
            .put_origin(
                &Origin {
                    fork: true,
                    repo_id: w.m_repo,
                    tag: Some("v1.0.0".into()),
                    landing: Some(w.p1),
                },
                None,
            )
            .unwrap();
        r.close().unwrap();
    }
    let u = StoreHandle::open_holder(&udir).unwrap();
    let from_u = bundle_of(&u, &[w.p1]);
    check_outside(&u, &from_u);

    // the same repo is the same store
    let before = state(&w.m, &[]);
    let e =
        w.m.port_into_view(&bundle_of(&twin, &[w.p1]), "twin", PERSON)
            .unwrap_err();
    assert_eq!(
        refusal(&e),
        (
            "E_BAD_INPUT",
            "the port bundle comes from this store",
            "replay the change inside this store"
        )
    );
    assert_eq!(state(&w.m, &[]), before);
}

/// D-A3H7S, D-08M5W: a carried view travels as any pushed view. A client pulls it, resolves its
/// conflict, lands it and pushes. The landing's facts keep each source repo and change, the
/// change with no op included, before and after the hub's move.
#[test]
fn hpt3_pull_resolve_land_push_preserves_port_origin() {
    use rdy_engine::sync_fetch::{self, HubState};
    use rdy_engine::sync_views::{self, ViewPullRequest};
    use rdy_store::sync_state::{SyncCanon, SyncStep};
    use rdy_store::take_batch::FetchTarget;
    const URL: &str = "https://127.0.0.1:9/r/line";
    let _clock = rdy_engine::pin_clock(AT);
    let mut w = lines();
    let (intro, terms) = (w.intro.clone(), w.terms.clone());
    let p2 = w.main_lands(|r| {
        write_land(
            r,
            "w1",
            json!([{"op": "update", "id": intro, "body": "Main words."}]),
        )
    });
    // p3 makes a note that p4 deletes: carried alone, p4's change keeps its place with no op
    let mut note = String::new();
    w.main_lands(|r| {
        note = write_in(
            r,
            "w2",
            json!([{"op": "create", "type": "prose", "id": "$n", "parent": terms, "body": "Soon gone."}]),
        )
        .created["$n"]
            .clone();
        land(r, "w2")
    });
    let p4 = w.main_lands(|r| write_land(r, "w3", json!([{"op": "delete", "id": note}])));
    let intro = w.intro.clone();
    let l2 = w.line_lands(|r| {
        write_land(
            r,
            "l2",
            json!([{"op": "update", "id": intro, "body": "Line words."}]),
        )
    });
    let b = bundle_of(&w.m, &[p2, p4]);
    let v = created(w.l.port_into_view(&b, "carry", PERSON).unwrap());
    assert_eq!(v.conflicts.len(), 1);
    assert_eq!(v.ported.len(), 2);

    // C pulls the view from L
    let cd = w.at("c");
    let file = w.at("view.rdysync");
    let snap = w.l.snapshot().unwrap();
    snap.write_history(
        StreamPurpose::ViewHistory,
        v.head,
        None,
        &BTreeSet::<ObjectKey>::new(),
        std::fs::File::create(&file).unwrap(),
    )
    .unwrap();
    drop(snap);
    let mut c = Rdy::open(&cd).unwrap();
    c.store_mut()
        .write_sync(
            "origin",
            vec![SyncStep::Canon(SyncCanon {
                repo_id: w.l_repo,
                url: URL.into(),
                fetched: Some(l2),
                applied: Some(l2),
                held: None,
            })],
        )
        .unwrap();
    sync_fetch::fetch(
        &mut c,
        &crate::hub_world::files(&[&file]),
        &FetchTarget {
            remote: "origin".into(),
            url: URL.into(),
            purpose: StreamPurpose::ViewHistory,
            root: v.head,
            view: Some("carry".into()),
        },
    )
    .unwrap();
    let person = rdy_engine::Session {
        actor: PERSON.into(),
        run: None,
        role: Role::Writer,
        kind: rdy_engine::SessionKind::Human,
    };
    sync_views::pull_views(
        &mut c,
        &person,
        &ViewPullRequest {
            remote: "origin".into(),
            hub: HubState {
                repo_id: Some(w.l_repo),
                canon: Some(l2),
            },
            views: vec![("carry".into(), Some(v.head))],
            take: false,
        },
    )
    .unwrap();
    let reader = c.store().reader();
    let empty = reader.change(&v.ported[1].change).unwrap();
    assert!(empty.ops.is_empty(), "{empty:?}");
    assert_eq!(empty.links.ported_from, Some(v.ported[1].source));
    drop(reader);

    // resolve, land, push
    let st = c.read();
    let canon_side = st
        .materialized(&rdy_engine::Scope::Canon)
        .unwrap()
        .get(&rdy_model::BlockId::parse(&w.intro).unwrap())
        .unwrap()
        .version_hash();
    drop(st);
    write_in(
        &mut c,
        "carry",
        json!([{"op": "update", "id": w.intro, "base": canon_side.to_string(), "body": "Agreed words."}]),
    );
    let lx = land(&mut c, "carry");
    c.close().unwrap();
    let push = w.at("push.rdysync");
    export(&cd, StreamPurpose::CanonHistory, lx, Some(l2), &push);
    let s = w.l.snapshot().unwrap();
    let checked = check(&s, &[&push]).unwrap();
    let want: Vec<(Hash, PortedFrom)> = v.ported.iter().map(|p| (p.change, p.source)).collect();
    let pairs = |f: &rdy_adapter::history::LandingFacts| -> Vec<(Hash, PortedFrom)> {
        f.ported_changes
            .iter()
            .map(|p| (p.change, p.ported_from))
            .collect()
    };
    let pushed = s.pushed_landings(&checked, &[lx]).unwrap();
    assert_eq!(pairs(&pushed[0]), want);
    assert_eq!(
        w.l.move_canon(&checked, CanonMove::Extend, Some(l2))
            .unwrap(),
        moved(lx)
    );
    drop((s, checked));
    let landed = w.l.snapshot().unwrap().landings(&[lx]).unwrap();
    assert_eq!(pairs(&landed[0]), want);
    assert_eq!(landed[0].ported_changes[1].ported_from.repo_id, w.m_repo);
}

// ---------------------------------------------------------------------------------------------
// removals

/// D-3JN9N: the target checks every bundled version and base against its own removals: a
/// result version it removed, an update's base and a delete's base. Already-held content and
/// clean changes in the same bundle do not save it, and a clean bundle still carries.
#[test]
fn hpr3_target_removals_cover_every_bundle_member() {
    let _clock = rdy_engine::pin_clock(AT);
    let mut w = lines();
    let (secret, intro) = (w.secret.clone(), w.intro.clone());
    let l2 = w.line_lands(|r| {
        write_land(
            r,
            "l2",
            json!([{"op": "update", "id": intro, "body": "Line words."}]),
        )
    });
    let l3 = w.line_lands(|r| write_land(r, "l3", json!([{"op": "delete", "id": secret}])));
    let l4 = w.line_lands(|r| {
        write_land(
            r,
            "l4",
            json!([{"op": "create", "type": "doc", "fields": {"title": "Line page", "doc_type": "page"}}]),
        )
    });
    let ls = w.l.snapshot().unwrap();
    let held_and_removed = bundle(&ls, &[w.p1]);
    let base_removed = bundle(&ls, &[l2]);
    let delete_base = bundle(&ls, &[l3]);
    let mixed = bundle(&ls, &[l2, l4]);
    let clean = bundle(&ls, &[l4]);
    drop(ls);
    // main removes the secret and the intro
    let (secret, intro) = (w.secret.clone(), w.intro.clone());
    w.main_lands(|r| remove(r, &[&secret, &intro]));
    let before = state(&w.m, &[]);
    for (what, b) in [
        ("a result version, beside held ones", &held_and_removed),
        ("an update's base", &base_removed),
        ("a delete's base", &delete_base),
        ("a removed base beside a clean change", &mixed),
    ] {
        let e = w.m.port_into_view(b, "carry", PERSON).unwrap_err();
        assert_eq!(refusal(&e), removed_refusal(), "{what}");
        assert_eq!(state(&w.m, &[]), before, "{what}");
    }
    created(w.m.port_into_view(&clean, "carry", PERSON).unwrap());
}

/// D-3DG3R: a snapshot read before a removal is refused before it reads the input.
#[test]
fn hpt3_snapshot_epoch_refuses_removed_source() {
    let _clock = rdy_engine::pin_clock(AT);
    let mut w = lines();
    let p2 = w.main_lands(|r| {
        write_land(
            r,
            "w1",
            json!([{"op": "create", "type": "doc", "fields": {"title": "Main", "doc_type": "page"}}]),
        )
    });
    let old = w.m.snapshot().unwrap();
    main_removes_secret(&mut w);
    for input in [vec![p2], vec![]] {
        let e = old.port_bundle(&input).unwrap_err();
        assert_eq!(
            refusal(&e),
            (
                "E_REDACTED_STATE",
                "This read state predates content removal.",
                "Read the project again, or run the dump again."
            )
        );
    }
    drop(old);
    let b = bundle_of(&w.m, &[p2]);
    created(w.l.port_into_view(&b, "carry", PERSON).unwrap());
}

/// D-3JN9N: one removed member refuses the whole batch: no view, change, blob or feature.
#[test]
fn hpt3_removed_target_refuses_the_whole_batch() {
    let _clock = rdy_engine::pin_clock(AT);
    let mut w = lines();
    let intro = w.intro.clone();
    let l2 = w.line_lands(|r| {
        write_land(
            r,
            "l2",
            json!([{"op": "update", "id": intro, "body": "Line words."}]),
        )
    });
    let b = bundle_of(&w.l, &[l2, w.p1]);
    assert_eq!(
        b.facts()
            .landings
            .iter()
            .map(|l| l.landing)
            .collect::<Vec<_>>(),
        [w.p1, l2],
        "oldest first"
    );
    main_removes_secret(&mut w);
    let before = state(&w.m, &[]);
    assert!(!before.3.contains(&"ported_change".to_string()));
    let e = w.m.port_into_view(&b, "carry", PERSON).unwrap_err();
    assert_eq!(refusal(&e), removed_refusal());
    assert_eq!(state(&w.m, &[]), before);
    assert!(
        w.m.snapshot()
            .unwrap()
            .view_head("carry")
            .unwrap()
            .is_none()
    );
}

// ---------------------------------------------------------------------------------------------
// the take order

/// D-3DG3R, D-7AT4S: the first fault wins. Length and form; then the family before the same
/// store and the author; the author and the view name before the target; the target before the
/// compare; the compare before the removals; the removals before the op checks.
#[test]
fn hpr3_take_first_fault_wins() {
    let _clock = rdy_engine::pin_clock(AT);
    let mut w = lines();
    let unverified = (
        "E_BAD_INPUT",
        "the port bundle does not verify",
        "make the bundle again in the source line",
    );
    // length, then form and hash
    let e = PortBundle::from_bytes(&vec![0u8; rdy_adapter::port::MAX_PORT_BUNDLE_BYTES + 1])
        .unwrap_err();
    assert_eq!(refusal(&e), unverified);
    let e = PortBundle::from_bytes(b"rdy-port-bundle-0").unwrap_err();
    assert_eq!(refusal(&e), unverified);
    let mut bytes = bundle_of(&w.m, &[w.p1]).to_bytes().to_vec();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    assert_eq!(
        refusal(&PortBundle::from_bytes(&bytes).unwrap_err()),
        unverified
    );

    let (secret, a) = (w.secret.clone(), w.a.clone());
    let l2 = w.line_lands(|r| {
        write_land(
            r,
            "l2",
            json!([{"op": "create", "type": "section", "id": "$n", "parent": a, "fields": {"title": "New"}}]),
        )
    });
    let section = w.l.snapshot().unwrap().landing_facts(l2).unwrap().blocks[0]
        .id
        .to_string();
    // l3: a claim under l2's section (a missing parent in main) and an edit of the secret
    let l3 = w.line_lands(|r| {
        write_land(
            r,
            "l3",
            json!([
                {"op": "create", "type": "prose", "parent": section, "body": "Under the new section."},
                {"op": "update", "id": secret, "body": "Rewritten."}
            ]),
        )
    });
    let from_l = bundle_of(&w.l, &[l3]);
    let parent_only = bundle_of(&w.l, &[l2]);

    // the family before the author
    let urepo = RepoId([0x44; 16]);
    let ud = w.at("u");
    copy_dir(&w.at("p"), &ud);
    {
        let mut r = Rdy::open(&ud).unwrap();
        r.store_mut()
            .put_origin(
                &Origin {
                    fork: true,
                    repo_id: RepoId([0x45; 16]),
                    tag: None,
                    landing: None,
                },
                Some(urepo),
            )
            .unwrap();
        r.close().unwrap();
    }
    let ue = w.at("u.rdysync");
    export(&ud, StreamPurpose::CanonHistory, w.p1, None, &ue);
    let u = StoreHandle::create_holder(&w.at("uh"), urepo).unwrap();
    let s = u.snapshot().unwrap();
    let c = check(&s, &[&ue]).unwrap();
    u.move_canon(&c, CanonMove::Seed, None).unwrap();
    drop((s, c));
    let e =
        w.m.port_into_view(&bundle_of(&u, &[w.p1]), "canon", "agent:a")
            .unwrap_err();
    assert_eq!(refusal(&e), family_refusal());
    // the same store before the author
    let e =
        w.m.port_into_view(&bundle_of(&w.m, &[w.p1]), "canon", "agent:a")
            .unwrap_err();
    assert_eq!(e.message, "the port bundle comes from this store");
    // the author before the view name and the target
    let empty = StoreHandle::create_holder(&w.at("empty"), w.m_repo).unwrap();
    let person = (
        "E_BAD_INPUT",
        "a hub carry needs a person's actor",
        "pass the person who asked for the carry",
    );
    for author in ["agent:a", "human:", "carrier", "human:Bad Name"] {
        let e = empty.port_into_view(&from_l, "canon", author).unwrap_err();
        assert_eq!(refusal(&e), person, "{author}");
    }
    // the view name before the target
    let e = empty.port_into_view(&from_l, "canon", PERSON).unwrap_err();
    assert_eq!(e.code, "E_RESERVED_VIEW");
    // the target: a project, then a holder with no hub canon
    let project = StoreHandle::open(&w.at("p")).unwrap();
    let e = project
        .port_into_view(&from_l, "carry", PERSON)
        .unwrap_err();
    assert_eq!(
        refusal(&e),
        (
            "E_BAD_INPUT",
            "a hub port needs a holder",
            "open the line's hub store"
        )
    );
    project.close().unwrap();
    let e = empty.port_into_view(&from_l, "carry", PERSON).unwrap_err();
    assert_eq!(
        refusal(&e),
        (
            "E_BAD_INPUT",
            "the target line has no hub canon",
            "seed the line before carrying a landing"
        )
    );
    // the compare before the removals: main removes the secret and holds a view
    main_removes_secret(&mut w);
    let held = created(w.m.port_into_view(&parent_only, "taken", PERSON).unwrap());
    assert_eq!(
        w.m.port_into_view(&from_l, "taken", PERSON).unwrap(),
        PortOutcome::Behind {
            head: Some(held.head)
        }
    );
    // the removals before the op checks: l3's missing parent is never reached
    let before = state(&w.m, &[]);
    let e = w.m.port_into_view(&from_l, "carry", PERSON).unwrap_err();
    assert_eq!(refusal(&e), removed_refusal());
    assert_eq!(state(&w.m, &[]), before);
}

// ---------------------------------------------------------------------------------------------
// no freshness claim

/// D-3JN9N: the bundle claims nothing about its source's freshness. The same landings make the
/// same bytes before and after the source moves on and takes a removal of other content, and
/// the take reads no source: it runs with the source closed.
#[test]
fn hpr3_bundle_has_no_source_freshness_claim() {
    let _clock = rdy_engine::pin_clock(AT);
    let mut w = lines();
    let p2 = w.main_lands(|r| {
        write_land(
            r,
            "w1",
            json!([{"op": "create", "type": "doc", "fields": {"title": "Main", "doc_type": "page"}}]),
        )
    });
    let early = bundle_of(&w.m, &[p2]);
    let facts = early.facts().clone();
    assert_eq!(facts.bytes, early.to_bytes().len());
    // the source moves on, holds a view and takes a removal of other content
    let p3 = w.main_lands(|r| {
        write_land(
            r,
            "w2",
            json!([{"op": "create", "type": "doc", "fields": {"title": "Later", "doc_type": "page"}}]),
        )
    });
    let l2 = w.line_lands(|r| {
        write_land(
            r,
            "l2",
            json!([{"op": "create", "type": "doc", "fields": {"title": "Line page", "doc_type": "page"}}]),
        )
    });
    created(
        w.m.port_into_view(&bundle_of(&w.l, &[l2]), "kept", PERSON)
            .unwrap(),
    );
    main_removes_secret(&mut w);
    let late = bundle_of(&w.m, &[p2]);
    assert_eq!(late.to_bytes(), early.to_bytes(), "no epoch, end or expiry");
    assert_eq!(late.facts(), &facts);
    // the wire holds no source position: neither canon tip nor the later landings
    for h in [w.m_tip, p3] {
        assert!(
            !early.to_bytes().windows(32).any(|win| win == h.0),
            "{h} in the bundle"
        );
    }
    // the take runs with the source closed
    let m = w.m;
    m.close().unwrap();
    let bytes = early.to_bytes().to_vec();
    drop(early);
    let b = PortBundle::from_bytes(&bytes).unwrap();
    created(w.l.port_into_view(&b, "carry", PERSON).unwrap());
}

// ---------------------------------------------------------------------------------------------
// parents

/// D-7AT4S (Task 2's four parent cases, through the take): a missing parent; a parent made
/// cleanly earlier in the same carry; a parent that an earlier change of the same carry leaves
/// conflicted; a move that makes a loop only against the target. A refusal writes nothing.
#[test]
fn hpc3_hub_parent_checks() {
    let _clock = rdy_engine::pin_clock(AT);
    let mut w = lines();
    let (doc, a, b, intro) = (w.doc.clone(), w.a.clone(), w.b.clone(), w.intro.clone());
    let mut section = String::new();
    let p2 = w.main_lands(|r| {
        section = write_in(
            r,
            "p",
            json!([{"op": "create", "type": "section", "id": "$p", "parent": doc, "fields": {"title": "New section"}}]),
        )
        .created["$p"]
            .clone();
        land(r, "p")
    });
    let sec = section.clone();
    let p3 = w.main_lands(|r| {
        write_land(
            r,
            "q",
            json!([{"op": "create", "type": "prose", "parent": sec, "body": "Under it."}]),
        )
    });
    // p4 moves section a under section b
    let (a4, b4) = (a.clone(), b.clone());
    let p4 = w.main_lands(|r| write_land(r, "m", json!([{"op": "move", "id": a4, "parent": b4}])));
    // p5 retitles section a; p6 puts prose under it. Carried without p4, p5's base is missing
    // in the line, so p5 becomes a create that conflicts with the line's a: p6's parent is
    // conflicted by an earlier change of the same carry
    let a2 = a.clone();
    let p5 = w.main_lands(|r| {
        write_land(
            r,
            "r",
            json!([{"op": "update", "id": a2, "fields": {"title": "A, in main"}}]),
        )
    });
    let a3 = a.clone();
    let p6 = w.main_lands(|r| {
        write_land(
            r,
            "s",
            json!([{"op": "create", "type": "prose", "parent": a3, "body": "Under a."}]),
        )
    });
    // the line puts b under a
    let (a6, b6) = (a.clone(), b.clone());
    w.line_lands(|r| write_land(r, "mv", json!([{"op": "move", "id": b6, "parent": a6}])));
    let _ = intro;
    let ms = w.m.snapshot().unwrap();
    let missing = bundle(&ms, &[p3]);
    let same_carry = bundle(&ms, &[p2, p3]);
    let conflicted_parent = bundle(&ms, &[p5, p6]);
    let looped = bundle(&ms, &[p4]);
    drop(ms);
    let unchanged = |before: &State| {
        assert_eq!(&state(&w.l, &[]), before);
        assert!(
            w.l.snapshot()
                .unwrap()
                .view_head("carry")
                .unwrap()
                .is_none()
        );
    };

    let before = state(&w.l, &[]);
    let e = w.l.port_into_view(&missing, "carry", PERSON).unwrap_err();
    assert_eq!(
        refusal(&e),
        (
            "E_DEPS_MISSING",
            format!("the port needs parent {section} present and clean in the target").as_str(),
            "carry the change that creates the parent first, or resolve its conflict"
        )
    );
    unchanged(&before);
    let e =
        w.l.port_into_view(&conflicted_parent, "carry", PERSON)
            .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            "E_DEPS_MISSING",
            format!("the port needs parent {a} present and clean in the target").as_str()
        )
    );
    unchanged(&before);
    let e = w.l.port_into_view(&looped, "carry", PERSON).unwrap_err();
    let one = format!("the port would make {a} and {b} each other's ancestors");
    let other = format!("the port would make {b} and {a} each other's ancestors");
    assert_eq!(
        (e.code, e.fix.as_str()),
        (
            "E_BAD_INPUT",
            "move one block out of the other in the target, then carry again"
        )
    );
    assert!(e.message == one || e.message == other, "{e:?}");
    unchanged(&before);
    // the parent made cleanly earlier in the same carry
    let v = created(w.l.port_into_view(&same_carry, "carry", PERSON).unwrap());
    assert!(v.conflicts.is_empty());
    assert_eq!(v.ported.len(), 2);
}

// ---------------------------------------------------------------------------------------------
// foreign parts

const AGENTS: &str = "pf-AGNT1";

fn cited() -> RepoId {
    RepoId([7; 16])
}

fn cited_x() -> rdy_model::Block {
    use rdy_model::{Block, BlockId, FieldValue};
    Block {
        id: BlockId::parse("d-FRGNX").unwrap(),
        block_type: "decision".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::from([("title".to_string(), FieldValue::Str("X".into()))]),
        body: "Use X.".into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    }
}

fn version_of(b: &rdy_model::Block) -> Hash {
    Hash::of(&rdy_store::codec::item_new(rdy_store::kinds::BLOCK, b).1)
}

fn content_of(b: &rdy_model::Block) -> Hash {
    Hash::of(format!("content of {} {}", b.id, b.body).as_bytes())
}

/// The delivery of `x` through the link `link`.
fn delivery(link: &str, x: &rdy_model::Block) -> rdy_model::foreign::ForeignEntry {
    use rdy_model::foreign::{
        ForeignDerived, ForeignEntry, ForeignEvent, ForeignPublication, ForeignSource,
        ForeignVersion, selection_identity,
    };
    ForeignEntry {
        repo: cited(),
        source: ForeignSource::Link {
            link: link.into(),
            site: "hub.example".into(),
            at_ms: 1_760_000_000_000,
        },
        event: ForeignEvent::Delivery {
            prev: None,
            publication: ForeignPublication {
                tag: "v1".into(),
                profile: AGENTS.into(),
                profile_version: None,
            },
            set: None,
            added: vec![ForeignVersion {
                id: x.id.clone(),
                version: version_of(x),
                content: content_of(x),
                encoding: 0,
                derived: ForeignDerived::default(),
            }],
            changed: vec![],
            removed: vec![],
            shells: vec![],
            selection: selection_identity(BTreeMap::from([(x.id.clone(), version_of(x))]).iter())
                .unwrap(),
        },
    }
}

/// Delivers `x` to `r` through its link `link`, straight to the store.
fn deliver(r: &mut Rdy, link: &str, x: &rdy_model::Block) {
    r.store_mut()
        .put_foreign(&delivery(link, x), std::slice::from_ref(x))
        .unwrap();
    r.publish_after_foreign_for_tests().unwrap();
}

/// The number of foreign log entries of the holder at `dir`, and whether it stores the cited
/// version as a block.
fn foreign_trace(dir: &Path) -> (usize, bool) {
    let r = Rdy::open_holder(dir).unwrap();
    let reader = r.store().reader();
    let n = reader
        .refs_with_prefix("foreign/")
        .unwrap()
        .into_iter()
        .count();
    let held = reader
        .has_kind(rdy_store::kinds::BLOCK, &version_of(&cited_x()))
        .unwrap();
    drop(reader);
    r.close().unwrap();
    (n, held)
}

/// D-08M5W, D-A3H7S: foreign links and pins travel through the bytes in a create and an
/// update. The bytes round-trip exactly. The target writes the parts as the source wrote them
/// when it holds the delivery, and refuses with the existing foreign-version triple when it does
/// not. A refusal writes nothing; a success imports no delivery and no pinned version.
#[test]
fn hpc3_foreign_bundle_bytes_roundtrip() {
    let _clock = rdy_engine::pin_clock(AT);
    let mut w = lines();
    let x = cited_x();
    {
        let mut r = Rdy::open(&w.at("p")).unwrap();
        deliver(&mut r, &w.link, &x);
        r.close().unwrap();
    }
    let doc = w.doc.clone();
    let mut claim = String::new();
    let lc = w.main_lands(|r| {
        claim = write_in(
            r,
            "c",
            json!([{"op": "create", "type": "claim", "id": "$f", "parent": doc,
                "body": "Per {{core:d-FRGNX}}.",
                "links": {"describes": [{"target": "core:d-FRGNX"}]}}]),
        )
        .created["$f"]
            .clone();
        land(r, "c")
    });
    let cl = claim.clone();
    let lu = w.main_lands(|r| {
        write_land(
            r,
            "u",
            json!([{"op": "update", "id": cl, "body": "Still per {{core:d-FRGNX}}, edited."}]),
        )
    });
    // the bytes round-trip exactly
    let made = bundle_of(&w.m, &[lc, lu]);
    let bytes = made.to_bytes().to_vec();
    let back = PortBundle::from_bytes(&bytes).unwrap();
    assert_eq!(back.to_bytes(), &bytes[..]);
    assert_eq!(back.facts(), made.facts());
    let source_block = {
        let r = Rdy::open(&w.at("p")).unwrap();
        let b = r
            .read()
            .materialized(&rdy_engine::Scope::Canon)
            .unwrap()
            .get(&rdy_model::BlockId::parse(&claim).unwrap())
            .unwrap()
            .clone();
        r.close().unwrap();
        b
    };
    assert!(!source_block.foreign.pins.is_empty(), "{source_block:?}");
    assert!(!source_block.foreign.links.is_empty(), "{source_block:?}");
    let ld = w.at("l");
    let m = w.m;
    m.close().unwrap();

    // the line holds no delivery: the existing triple, and no write
    let before = state(&w.l, &[]);
    let e = w.l.port_into_view(&back, "carry", PERSON).unwrap_err();
    let short = &content_of(&x).to_string()[..12];
    assert_eq!(
        refusal(&e),
        (
            "E_FOREIGN_VERSION",
            format!("core:d-FRGNX has no delivered version {short}").as_str(),
            "wait for the delivery that holds it, or pin a delivered version: rdy_links {op: \"show\", alias} / rdy links show core"
        )
    );
    assert_eq!(state(&w.l, &[]), before);
    // a holder takes no delivery, so no hub line can hold the pinned version
    w.l.close().unwrap();
    let trace = foreign_trace(&ld);
    assert_eq!(trace, (0, false), "no foreign entry, no pinned version");
    {
        let mut r = Rdy::open_holder(&ld).unwrap();
        let e = r
            .store_mut()
            .put_foreign(&delivery(&w.link, &x), std::slice::from_ref(&x))
            .unwrap_err();
        assert_eq!(e.to_string(), "a holder store holds no foreign entry");
        r.close().unwrap();
    }
    assert_eq!(foreign_trace(&ld), trace, "nothing imported");
    // the same bytes again, decoded once more, refuse the same way
    let l = StoreHandle::open_holder(&ld).unwrap();
    let again = PortBundle::from_bytes(back.to_bytes()).unwrap();
    assert_eq!(
        l.port_into_view(&again, "carry", PERSON).unwrap_err().code,
        "E_FOREIGN_VERSION"
    );
    let _ = source_block;
}
