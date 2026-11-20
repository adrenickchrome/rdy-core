//! crb3: the rebase test of a chain of landings onto a landing's state (D-2QX9W, D-N0NVF), in a
//! holder, from the snapshot's captured refs only, with no write.

use crate::hub_world::{
    AT, World, check, copy_dir, doc, export, land, moved, project, repo_of, seed, world, write_in,
};
use rdy_adapter::stream::StreamPurpose;
use rdy_adapter::sync::CanonMove;
use rdy_adapter::{ConflictKind, Hash, MergeConflict, RebaseTest, StoreHandle};
use rdy_engine::Rdy;
use serde_json::json;
use std::path::Path;

fn landing(h: &Hash) -> String {
    format!("landing:{h}")
}

/// Moves the history file `path` onto the hub with `how`, canon's tip expected at `from`.
fn move_file(h: &StoreHandle, path: &Path, how: CanonMove, from: Option<Hash>, to: Hash) {
    let s = h.snapshot().unwrap();
    let c = check(&s, &[path]).unwrap();
    assert_eq!(h.move_canon(&c, how, from).unwrap(), moved(to));
}

/// The end of the holder's log and every ref it holds, read with the holder closed.
fn log_and_refs(root: &Path) -> (rdy_store::segment::Location, Vec<(String, Vec<u8>)>) {
    let r = Rdy::open_holder(root).unwrap();
    let end = r.store().log_end();
    let refs = r.store().refs_with_prefix("").unwrap();
    r.close().unwrap();
    (end, refs)
}

#[test]
fn crb3_a_held_chain_rebases_clean_onto_canon() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = world();
    let h = w.holder("hub");
    seed(&h, &w);
    move_file(&h, &w.e2, CanonMove::Extend, Some(w.p1), w.p2);
    move_file(&h, &w.eq, CanonMove::Hold, Some(w.p2), w.q2);
    let snap = h.snapshot().unwrap();
    assert_eq!(snap.canon(), Some(w.p2));
    assert_eq!(
        snap.rebase_test(&[w.q2], "canon").unwrap(),
        RebaseTest::Clean
    );
    drop(snap);
    h.close().unwrap();
}

/// Q edits the titles of the doc and the section of "Canon" and lands `q3` on `p1`; P edits both
/// differently and lands `p3` on `p1`. The hub's canon is `p3`, and it holds `q3`.
#[test]
fn crb3_conflict_names_every_block_of_the_stopping_landing() {
    let _clock = rdy_engine::pin_clock(AT);
    let dir = tempfile::tempdir().unwrap();
    let (pd, qd) = (dir.path().join("p"), dir.path().join("q"));
    let mut p = project(&pd);
    let repo = repo_of(&p);
    let ids = doc(&mut p, "w0", "Canon", "The hub keeps one canon.").created;
    let p1 = land(&mut p, "w0");
    p.close().unwrap();
    copy_dir(&pd, &qd);
    let titles = |r: &mut Rdy, view: &str, word: &str| {
        write_in(
            r,
            view,
            json!([
                {"op": "update", "id": ids["$d"], "fields": {"title": format!("Canon by {word}")}},
                {"op": "update", "id": ids["$s"], "fields": {"title": format!("Terms by {word}")}}
            ]),
        );
        land(r, view)
    };
    let mut q = Rdy::open(&qd).unwrap();
    let q3 = titles(&mut q, "wq", "Q");
    q.close().unwrap();
    let mut p = Rdy::open(&pd).unwrap();
    let p3 = titles(&mut p, "wp", "P");
    p.close().unwrap();
    let at = |n: &str| dir.path().join(format!("{n}.rdysync"));
    let canon = StreamPurpose::CanonHistory;
    export(&pd, canon, p1, None, &at("e1"));
    export(&pd, canon, p3, Some(p1), &at("ep"));
    export(&qd, canon, q3, Some(p1), &at("eq"));
    let h = StoreHandle::create_holder(&dir.path().join("hub"), repo).unwrap();
    move_file(&h, &at("e1"), CanonMove::Seed, None, p1);
    move_file(&h, &at("ep"), CanonMove::Extend, Some(p1), p3);
    move_file(&h, &at("eq"), CanonMove::Hold, Some(p3), q3);
    let snap = h.snapshot().unwrap();
    let mut want: Vec<MergeConflict> = ["$d", "$s"]
        .iter()
        .map(|k| MergeConflict {
            block: ids[*k].parse().unwrap(),
            kind: ConflictKind::StaleBase,
        })
        .collect();
    want.sort_by(|a, b| a.block.cmp(&b.block));
    let RebaseTest::Conflict {
        landing: stop,
        mut conflicts,
    } = snap.rebase_test(&[q3], "canon").unwrap()
    else {
        panic!("a clean rebase of q3 onto p3");
    };
    assert_eq!(stop, q3);
    conflicts.sort_by(|a, b| a.block.cmp(&b.block));
    assert_eq!(conflicts, want);
    // onto `p1`, the landing Q wrote on, the same chain is clean
    assert_eq!(
        snap.rebase_test(&[q3], &landing(&p1)).unwrap(),
        RebaseTest::Clean
    );
    drop(snap);
    h.close().unwrap();
}

/// A held chain on `p1`, written in Q: `q2` writes the "Held" doc, `q4` updates its prose, `q5`
/// deletes the prose of "Canon" and `q6` re-creates that prose under its own id.
struct Chain {
    w: World,
    hub: StoreHandle,
    q2: Hash,
    q4: Hash,
    q5: Hash,
    q6: Hash,
    held_prose: String,
    canon_prose: String,
}

/// The id and the parent of the block of canon in `r` whose body is `body`.
fn block_of(r: &Rdy, body: &str) -> (String, String) {
    let reader = r.store().reader();
    for v in reader.canon_state().unwrap().blocks.values() {
        let b = reader.get_block(v).unwrap();
        if b.body == body {
            return (b.id.to_string(), b.parent.unwrap().to_string());
        }
    }
    panic!("no block of canon reads {body:?}");
}

fn chain() -> Chain {
    let w = world();
    let qd = w.dir.path().join("q");
    let mut q = Rdy::open(&qd).unwrap();
    let (canon_prose, section) = block_of(&q, "The hub keeps one canon.");
    let (held_prose, _) = block_of(&q, "A chain the hub holds beside canon.");
    write_in(
        &mut q,
        "w4",
        json!([{"op": "update", "id": held_prose, "body": "The held chain moves on."}]),
    );
    let q4 = land(&mut q, "w4");
    write_in(&mut q, "w5", json!([{"op": "delete", "id": canon_prose}]));
    let q5 = land(&mut q, "w5");
    write_in(
        &mut q,
        "w6",
        json!([{"op": "create", "type": "prose", "id": canon_prose, "parent": section,
                "body": "The hub keeps one canon, again."}]),
    );
    let q6 = land(&mut q, "w6");
    q.close().unwrap();
    let eq = w.dir.path().join("eq6.rdysync");
    export(&qd, StreamPurpose::CanonHistory, q6, Some(w.p1), &eq);
    let hub = w.holder("hub");
    seed(&hub, &w);
    move_file(&hub, &eq, CanonMove::Hold, Some(w.p1), q6);
    let q2 = w.q2;
    Chain {
        w,
        hub,
        q2,
        q4,
        q5,
        q6,
        held_prose,
        canon_prose,
    }
}

/// `q4` merges onto the state `q2` gives, not onto `p1` alone; `q6` re-creates what `q5`
/// deleted, so it merges after `q5` and not onto `p1` alone.
#[test]
fn crb3_a_later_landing_merges_onto_the_earlier_ones() {
    let _clock = rdy_engine::pin_clock(AT);
    let c = chain();
    let snap = c.hub.snapshot().unwrap();
    let onto = landing(&c.w.p1);
    assert_eq!(
        snap.rebase_test(&[c.q2, c.q4], &onto).unwrap(),
        RebaseTest::Clean
    );
    assert_eq!(
        snap.rebase_test(&[c.q4], &onto).unwrap(),
        RebaseTest::Conflict {
            landing: c.q4,
            conflicts: vec![MergeConflict {
                block: c.held_prose.parse().unwrap(),
                kind: ConflictKind::StaleBase,
            }],
        }
    );
    // a delete and then a re-create of the same id
    assert_eq!(
        snap.rebase_test(&[c.q5, c.q6], &onto).unwrap(),
        RebaseTest::Clean
    );
    assert_eq!(
        snap.rebase_test(&[c.q2, c.q4, c.q5, c.q6], &onto).unwrap(),
        RebaseTest::Clean
    );
    assert_eq!(
        snap.rebase_test(&[c.q6], &onto).unwrap(),
        RebaseTest::Conflict {
            landing: c.q6,
            conflicts: vec![MergeConflict {
                block: c.canon_prose.parse().unwrap(),
                kind: ConflictKind::AlreadyExists,
            }],
        }
    );
    drop(snap);
    c.hub.close().unwrap();
}

fn refused(e: rdy_adapter::RdyError, code: &str, message: &str, fix: &str) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (code, message, fix),
        "{e:#?}"
    );
}

#[test]
fn crb3_input_refusals() {
    let _clock = rdy_engine::pin_clock(AT);
    let c = chain();
    let snap = c.hub.snapshot().unwrap();
    let onto = landing(&c.w.p1);
    // more than 100,000 hashes, before any read: none of them is a landing
    let many: Vec<Hash> = (0..100_001u32)
        .map(|n| rdy_adapter::hash_bytes(&n.to_be_bytes()))
        .collect();
    refused(
        snap.rebase_test(&many, &onto).unwrap_err(),
        "E_LIMIT",
        "version test chain exceeds 100000 landings",
        "send at most 100000 landings",
    );
    // exactly 100,000 is not over the limit: the first unreached hash is named
    let e = snap.rebase_test(&many[..100_000], &onto).unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND", "{e:#?}");
    // a repeated hash
    refused(
        snap.rebase_test(&[c.q2, c.q4, c.q2], &onto).unwrap_err(),
        "E_BAD_INPUT",
        &format!("landing {} appears twice in the chain", c.q2),
        "name each landing of the chain once",
    );
    // an onto that is not one landing's state
    let fix = "use canon, tag:<version> or landing:<id>";
    for text in [
        "draft",
        "view:draft",
        "inflight",
        &format!("view:draft@{}", c.w.d1),
        "proposal:P-0000000000ab@1",
    ] {
        refused(
            snap.rebase_test(&[c.q2], text).unwrap_err(),
            "E_BAD_INPUT",
            &format!("onto {text:?} is not a landing's state"),
            fix,
        );
    }
    // an unreached hash, with the text of D-Y32MN's reach
    let stray = rdy_adapter::hash_bytes(b"not a landing");
    refused(
        snap.rebase_test(&[c.q2, stray], &onto).unwrap_err(),
        "E_NOT_FOUND",
        &format!(
            "landing {stray} is not reached from canon, a tag, a held chain or a replaced mark"
        ),
        "name a landing this store reaches, or read canon",
    );
    // an onto landing the reach does not find is the read's refusal
    let e = snap.rebase_test(&[c.q2], &landing(&stray)).unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND", "{e:#?}");
    // the empty chain is clean, onto canon and onto a landing
    assert_eq!(snap.rebase_test(&[], "canon").unwrap(), RebaseTest::Clean);
    assert_eq!(snap.rebase_test(&[], &onto).unwrap(), RebaseTest::Clean);
    drop(snap);
    c.hub.close().unwrap();

    // a landing held after the snapshot was taken is not reached from it
    let w = world();
    let h = w.holder("hub");
    seed(&h, &w);
    move_file(&h, &w.e2, CanonMove::Extend, Some(w.p1), w.p2);
    let s1 = h.snapshot().unwrap();
    move_file(&h, &w.eq, CanonMove::Hold, Some(w.p2), w.q2);
    let e = s1.rebase_test(&[w.q2], "canon").unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND", "{e:#?}");
    let s2 = h.snapshot().unwrap();
    assert_eq!(s2.rebase_test(&[w.q2], "canon").unwrap(), RebaseTest::Clean);
    drop((s1, s2));
    h.close().unwrap();
}

#[test]
fn crb3_no_write() {
    let _clock = rdy_engine::pin_clock(AT);
    let c = chain();
    let root = c.w.dir.path().join("hub");
    c.hub.close().unwrap();
    let before = log_and_refs(&root);
    let h = StoreHandle::open_holder(&root).unwrap();
    let snap = h.snapshot().unwrap();
    snap.rebase_test(&[c.q2, c.q4, c.q5, c.q6], "canon")
        .unwrap();
    snap.rebase_test(&[c.q4], &landing(&c.w.p1)).unwrap();
    drop(snap);
    h.close().unwrap();
    assert_eq!(log_and_refs(&root), before);
}
