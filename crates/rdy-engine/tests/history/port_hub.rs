//! hpt3/hpr3 the take of a hub port (D-A3H7S, D-DZDB7, D-3JN9N, D-7AT4S): `Rdy::port_into_view`
//! writes one new pushed view of a holder in one batch, through the checked canon move. The
//! source here is a real fork of the main hub; the main hub takes the source project's removals
//! by tag uploads, so it holds removal records the fork's bundle may name.
use crate::port_bundle::*;

use rdy_engine::fork::fork_holder_at;
use rdy_engine::port::{self, PortBundle};
use rdy_engine::port_hub::{PortOutcome, PortedView};
use rdy_engine::*;
use rdy_model::*;
use rdy_store::kinds;
use rdy_store::segment::Location;
use rdy_store::{ViewOpKind, format};
use std::path::{Path, PathBuf};

const PERSON: &str = "human:qa";

/// The main hub `hs` of project `s` and its fork `hf` at v1.0.0. The seed holds the World
/// blocks; `lf` lands a figure of one chunk (`blob`). Both are under v1.0.0. `hs` then takes
/// the removals of `removed` through a tag upload of v2.0.0.
struct Hubs {
    root: tempfile::TempDir,
    s: Rdy,
    t: Rdy,
    hs: Rdy,
    hs_dir: PathBuf,
    hf: Rdy,
    seed: Hash,
    lf: Hash,
    blob: Hash,
    dc: String,
}

fn has_tag(r: &Rdy, name: &str) -> bool {
    r.store()
        .tags()
        .unwrap()
        .into_iter()
        .any(|(n, _, _)| n == name)
}

fn hubs(removed: &[&str]) -> Hubs {
    let World {
        root,
        mut s,
        t,
        notes,
        dc,
        seed,
        ..
    } = world();
    let (lf, blob, _) = land_figure(&mut s, "f", &notes, &[5u8; 64]);
    s.tag(&approver(), "v1.0.0", None, None).unwrap();
    let hs_dir = root.path().join("hs");
    let mut hs = hub(&s, &hs_dir);
    if !has_tag(&hs, "v1.0.0") {
        take_tag(&mut hs, &s, root.path(), "v1.0.0");
    }
    let hf = fork_holder_at(&hs.read(), "v1.0.0", &root.path().join("hf")).unwrap();
    for (i, target) in removed.iter().enumerate() {
        let target = match *target {
            "dc" => dc.clone(),
            "blob" => format!("blob:{blob}"),
            other => other.to_string(),
        };
        remove(&mut s, &format!("rm{i}"), &target);
    }
    if !removed.is_empty() {
        s.tag(&approver(), "v2.0.0", None, None).unwrap();
        take_tag(&mut hs, &s, root.path(), "v2.0.0");
    }
    Hubs {
        root,
        s,
        t,
        hs,
        hs_dir,
        hf,
        seed,
        lf,
        blob,
        dc,
    }
}

/// What a write would change: the log end, every ref (canon, views, tags, held, origin and the
/// features ref among them), the feature names and the batch count.
type Trace = (Location, Vec<(String, Vec<u8>)>, Vec<String>, u64);

fn trace(r: &Rdy) -> Trace {
    let st = r.store();
    (
        st.log_end(),
        st.reader().refs_with_prefix("").unwrap(),
        st.features()
            .features
            .iter()
            .map(|f| f.name.clone())
            .collect(),
        st.write_count(),
    )
}

fn take(r: &mut Rdy, b: &PortBundle, view: &str, author: &str) -> Result<PortOutcome, RdyError> {
    r.port_into_view(b, view, author, T0)
}

fn created(out: PortOutcome) -> PortedView {
    match out {
        PortOutcome::Created(v) => v,
        other => panic!("not created: {other:?}"),
    }
}

/// `e` is the refusal `want`: its code, message and fix.
fn is(e: &RdyError, want: RdyError) {
    assert_eq!(refusal(e), refusal(&want), "{e}");
}

fn reopen(r: Rdy, dir: &Path) -> Rdy {
    r.close().unwrap();
    Rdy::open_holder(dir).unwrap()
}

// ---------------------------------------------------------------------------------------------
// the write

/// D-A3H7S, D-DZDB7: a failed append and a refusal leave the log, the refs, the objects, the
/// features and the index as they were. A success is one batch: the objects, one Create on
/// canon's tip and one Commit per change, by the person with run `hub-carry`, and the view
/// head. Canon, tags, held refs and `meta/origin` stay; no landing is made.
#[test]
fn hpt3_failed_writes_leave_the_target_unchanged() {
    let Hubs {
        root: _root,
        mut hs,
        hs_dir,
        hf,
        seed,
        lf,
        ..
    } = hubs(&[]);
    let b = make(&hf, &[seed, lf]).unwrap();
    let before = trace(&hs);
    assert!(
        !before
            .2
            .contains(&format::FEATURE_PORTED_CHANGE.to_string())
    );

    // a failed append, then a refusal
    hs.store_mut().fail_next_append_after(40);
    take(&mut hs, &b, "carry", PERSON).unwrap_err();
    assert_eq!(trace(&hs), before);
    let e = take(&mut hs, &b, "carry", "agent:qa").unwrap_err();
    is(&e, port::not_a_person());
    assert_eq!(trace(&hs), before);
    let mut hs = reopen(hs, &hs_dir);
    assert_eq!(trace(&hs).1, before.1, "nothing durable either");
    assert!(hs.read().view_head("carry").is_none());

    // the write: one batch
    let tip = hs.read().canon_head().unwrap();
    let before = trace(&hs);
    let v = created(take(&mut hs, &b, "carry", PERSON).unwrap());
    let after = trace(&hs);
    assert_eq!(after.3, before.3 + 1, "one batch");
    assert!(after.2.contains(&format::FEATURE_PORTED_CHANGE.to_string()));
    let moved: Vec<&String> = after
        .1
        .iter()
        .filter(|r| !before.1.contains(r))
        .map(|(n, _)| n)
        .collect();
    assert_eq!(
        moved,
        [
            &format::REF_STORE_FEATURES.to_string(),
            &"view/carry".to_string()
        ],
        "canon, tags, held refs and the origin stay"
    );
    assert_eq!(hs.read().canon_head(), Some(tip));
    assert_eq!(v.base, tip);

    // the chain: a Create on the tip, then one Commit per change, oldest first
    let reader = hs.store().reader();
    let mut ops = vec![];
    let mut at = Some(v.head);
    while let Some(h) = at {
        let op = reader.view_op(&h).unwrap();
        at = op.prev;
        ops.push((h, op));
    }
    ops.reverse();
    assert_eq!(ops[0].0, v.root);
    assert_eq!(
        ops[0].1.op,
        ViewOpKind::Create {
            base: Some(tip),
            owner: None,
            pinned: false
        }
    );
    assert_eq!(ops.len(), v.ported.len() + 1);
    for ((_, op), p) in ops[1..].iter().zip(&v.ported) {
        assert_eq!(op.op, ViewOpKind::Commit { change: p.change });
    }
    for (_, op) in &ops {
        assert_eq!(
            (op.actor.as_str(), op.run.as_deref(), op.at_ms),
            (PERSON, Some("hub-carry"), T0)
        );
    }
    for p in &v.ported {
        let c = reader.change(&p.change).unwrap();
        assert_eq!(c.view, "carry");
        assert_eq!(
            c.provenance,
            Provenance {
                actor: PERSON.into(),
                run: Some("hub-carry".into()),
                role: Role::Writer,
                at_ms: T0,
            }
        );
        assert_eq!(c.links.ported_from, Some(p.source));
        let token = c.links.briefing.expect("the target briefing");
        assert!(hs.briefs().get(&token.to_string()).is_some());
    }
    let head = hs.read().view_head("carry").unwrap().clone();
    assert_eq!(
        (head.head, head.base, head.owner, head.pinned),
        (v.head, Some(tip), None, false)
    );
    drop(reader);
    // durable: a reopen reads the same view
    let hs = reopen(hs, &hs_dir);
    assert_eq!(hs.read().view_head("carry").unwrap().head, v.head);
}

// ---------------------------------------------------------------------------------------------
// removals

/// D-3JN9N: the target refuses a bundle that holds a block version it removed, even one it
/// held already, and a blob whose manifest and chunks it removed. The check reads every member
/// before preparation, and a refusal writes nothing.
#[test]
fn hpr3_target_removals_cover_every_bundle_member() {
    // a block version: the seed's version of the draft note, which the main hub holds too
    let Hubs {
        root: _r1,
        mut hs,
        hf,
        seed,
        lf,
        dc,
        ..
    } = hubs(&["dc"]);
    let seed_dc = {
        let st = hf.read();
        st.materialized(&Scope::Canon)
            .unwrap()
            .get(&bid(&dc))
            .unwrap()
            .version_hash()
    };
    let reader = hs.store().reader();
    assert!(
        reader
            .redaction_of(kinds::BLOCK, &seed_dc)
            .unwrap()
            .is_some()
    );
    assert!(reader.has_kind(kinds::BLOCK, &seed_dc).unwrap(), "held");
    drop(reader);
    let before = trace(&hs);
    for (what, ls) in [
        ("the seed", vec![seed]),
        ("seed and figure", vec![seed, lf]),
    ] {
        let e = take(&mut hs, &make(&hf, &ls).unwrap(), "carry", PERSON).unwrap_err();
        is(&e, port::target_removed());
        assert_eq!(trace(&hs), before, "{what}");
    }
    // the figure alone names no removed version: it carries
    created(take(&mut hs, &make(&hf, &[lf]).unwrap(), "fig", PERSON).unwrap());

    // a blob: its manifest and its chunk
    let Hubs {
        root: _r2,
        mut hs,
        hf,
        seed,
        lf,
        blob,
        ..
    } = hubs(&["blob"]);
    let manifest = {
        let reader = hf.store().reader();
        let (raw, payload) = reader.get_kind_raw(kinds::BLOB_MANIFEST, &blob).unwrap();
        let o = rdy_store::codec::split(raw, &payload).unwrap();
        let m: kinds::BlobManifest = rdy_store::codec::decode_split(&o).unwrap();
        m
    };
    let reader = hs.store().reader();
    assert!(
        reader
            .redaction_of(kinds::BLOB_MANIFEST, &blob)
            .unwrap()
            .is_some()
    );
    for k in &manifest.chunks {
        assert!(reader.redaction_of(kinds::BLOB_CHUNK, k).unwrap().is_some());
    }
    drop(reader);
    let bf = make(&hf, &[lf]).unwrap();
    assert_eq!(bf.content().blobs.len(), 1);
    assert_eq!(bf.content().chunks.len(), manifest.chunks.len());
    let before = trace(&hs);
    let e = take(&mut hs, &bf, "carry", PERSON).unwrap_err();
    is(&e, port::target_removed());
    assert_eq!(trace(&hs), before);
    // content with no removed member still carries
    created(take(&mut hs, &make(&hf, &[seed]).unwrap(), "carry", PERSON).unwrap());
}

// ---------------------------------------------------------------------------------------------
// the take order

/// D-3DG3R, D-7AT4S: the first fault wins: the family, the same store, the author, the view
/// name, a holder, a hub canon tip, the view-head compare, then the removals.
#[test]
fn hpr3_take_first_fault_wins() {
    let Hubs {
        root,
        s,
        mut t,
        mut hs,
        hf,
        seed,
        lf,
        ..
    } = hubs(&["dc"]);
    let from_fork = make(&hf, &[seed]).unwrap();
    let from_main = make(&hs, &[lf]).unwrap();
    let repo = s.read().repo_id().unwrap();
    let mut unrelated = Rdy::create_holder(&root.path().join("u"), RepoId([9; 16])).unwrap();
    let mut empty = Rdy::create_holder(&root.path().join("empty"), repo).unwrap();

    // the family before the same store and the author
    let e = take(&mut unrelated, &from_fork, "canon", "agent:a").unwrap_err();
    is(&e, port::outside_family());
    // the same store before the author
    let e = take(&mut hs, &from_main, "canon", "agent:a").unwrap_err();
    is(&e, port::same_store());
    // the author before the view name, the view name before the target
    let e = take(&mut empty, &from_fork, "canon", "agent:a").unwrap_err();
    is(&e, port::not_a_person());
    let e = take(&mut empty, &from_fork, "canon", PERSON).unwrap_err();
    assert_eq!(e.code, E_RESERVED_VIEW);
    // a holder before a canon tip
    let e = take(&mut t, &from_fork, "carry", PERSON).unwrap_err();
    is(&e, port::needs_holder());
    let e = take(&mut empty, &from_fork, "carry", PERSON).unwrap_err();
    is(&e, port::no_hub_canon());
    // the compare before the removals
    let held = created(take(&mut hs, &make(&hf, &[lf]).unwrap(), "taken", PERSON).unwrap());
    let before = trace(&hs);
    assert_eq!(
        take(&mut hs, &from_fork, "taken", PERSON).unwrap(),
        PortOutcome::Behind {
            head: Some(held.head)
        }
    );
    assert_eq!(trace(&hs), before);
    // then the removals
    let e = take(&mut hs, &from_fork, "carry", PERSON).unwrap_err();
    is(&e, port::target_removed());
    assert_eq!(trace(&hs), before);
}
