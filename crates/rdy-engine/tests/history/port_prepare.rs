//! hpt2/hpr2/hpc2 preparation (D-1QT23, D-08M5W, D-3JN9N, D-7AT4S): a port copies each source
//! change's ops, justification and message into a new change of the target. A held base stays;
//! a create or update whose base the target lacks becomes a create; such a delete is dropped
//! when the target lacks the block and refused when it holds it. Ports merge as replays do, so a
//! resolution must cover every port side. The briefing is the target's own, made at canon's tip.
use crate::port_bundle::*;

use rdy_engine::history_facts;
use rdy_engine::port::{self, PortBrief};
use rdy_engine::stream_export;
use rdy_engine::sync_fetch::{self, HubState};
use rdy_engine::sync_replay::{self, PULLED, PULLED_WITH_CONFLICTS, PullRequest, StopReason};
use rdy_engine::*;
use rdy_model::foreign::{
    ForeignDerived, ForeignEntry, ForeignEvent, ForeignPublication, ForeignSource, ForeignVersion,
    selection_identity,
};
use rdy_model::*;
use rdy_store::kinds;
use rdy_store::sync_state::{SyncCanon, SyncStep};
use rdy_store::sync_stream::StreamPurpose;
use rdy_store::take_batch::FetchTarget;
use rdy_store::{ViewOpKind, codec};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::Path;

/// The changes of landings `ls` in `r`, in landing order.
fn source_changes(r: &Rdy, ls: &[Hash]) -> Vec<(Hash, Change)> {
    let reader = r.store().reader();
    ls.iter()
        .flat_map(|l| reader.landing(l).unwrap().changes)
        .map(|c| (c, reader.change(&c).unwrap()))
        .collect()
}

fn repo(r: &Rdy) -> RepoId {
    r.read().repo_id().unwrap()
}

fn conflict_blocks(r: &Rdy, view: &str) -> Vec<BlockId> {
    let mut out: Vec<BlockId> = r
        .read()
        .conflicts(&Scope::View(view.into()))
        .unwrap()
        .iter()
        .map(|c| c.block.clone())
        .collect();
    out.sort();
    out
}

fn stored(r: &Rdy, c: &Hash) -> Change {
    r.store().reader().change(c).unwrap()
}

fn log_end(r: &Rdy) -> rdy_store::segment::Location {
    r.store().log_end()
}

fn delete_refusal() -> (&'static str, &'static str, &'static str) {
    (
        E_BAD_INPUT,
        "the target line lacks the base of a ported delete",
        "carry the landing that wrote it first",
    )
}

// ---------------------------------------------------------------------------------------------
// copies

#[test]
fn hpt2_port_preserves_ops_message_and_justification() {
    let World {
        root,
        mut s,
        mut t,
        notes,
        dc,
        ..
    } = world();
    // l2: a new claim, then an edit of the draft note; l3: two more edits, then the claim goes
    let w = write_as(
        &mut s,
        "a",
        json!([{"op": "create", "type": "claim", "id": "$n", "parent": notes, "body": "New."}]),
        &[],
        "add a claim",
        editorial("add it"),
    );
    let n = w.created["$n"].clone();
    write_as(
        &mut s,
        "a",
        json!([{"op": "update", "id": dc, "body": "dc two"}]),
        &[],
        "edit the note",
        None,
    );
    let l2 = land(&mut s, "a");
    write_as(
        &mut s,
        "b",
        json!([{"op": "update", "id": dc, "body": "dc three"}]),
        &[],
        "edit it again",
        editorial("again"),
    );
    write_as(
        &mut s,
        "b",
        json!([{"op": "update", "id": dc, "body": "dc four"}]),
        &[],
        "and again",
        None,
    );
    write_as(
        &mut s,
        "b",
        json!([{"op": "delete", "id": n}]),
        &[],
        "drop the claim",
        None,
    );
    let l3 = land(&mut s, "b");
    let h = hub(&s, &root.path().join("hub"));
    let b = make(&h, &[l2, l3]).unwrap();
    let src = source_changes(&s, &[l2, l3]);
    let tip = t.read().canon_head();

    // a clean port: every op as the source wrote it
    let p = prepare(&t, &b, "carry").unwrap();
    assert_eq!(p.view, "carry");
    assert_eq!(p.base, tip);
    assert!(p.conflicts.is_empty(), "{:?}", p.conflicts);
    assert_eq!(p.changes.len(), src.len());
    for ((hash, from), (new, mapped)) in src.iter().zip(p.changes.iter().zip(&p.ported)) {
        assert_eq!(new.ops, from.ops);
        assert_eq!(new.message, from.message);
        assert_eq!(new.justification, from.justification);
        assert_eq!(new.view, "carry");
        assert_eq!(new.provenance, person());
        let origin = PortedFrom {
            repo_id: repo(&s),
            change: *hash,
        };
        assert_eq!(new.links.ported_from, Some(origin));
        assert_eq!(new.links.replayed_from, None);
        assert_eq!(new.links.intent, None);
        assert_eq!(new.links.alternative, None);
        assert!(p.briefing.is_some(), "a hub port makes its briefing");
        assert_eq!(new.links.briefing, p.briefing);
        assert!(!new.deps.contains(hash), "a source is never a dep");
        assert_eq!((mapped.source, mapped.change), (origin, new.id()));
    }
    // sequential writes to one block follow each other
    assert!(p.changes[3].deps.contains(&p.changes[2].id()));
    assert!(p.changes[2].deps.contains(&p.changes[1].id()));
    // a missing source base: l3 alone. Its first edit of the note becomes a create, which the
    // held note conflicts with; the delete of a claim the target never held keeps its change
    let p3 = prepare(&t, &make(&h, &[l3]).unwrap(), "only-l3").unwrap();
    let Op::Create { block } = &p3.changes[0].ops[0] else {
        panic!("{:?}", p3.changes[0].ops)
    };
    let Op::Update { block: from, .. } = &src[2].1.ops[0] else {
        panic!()
    };
    assert_eq!(block, from);
    assert!(
        matches!(&p3.changes[1].ops[0], Op::Update { .. }),
        "held: kept"
    );
    assert!(p3.changes[2].ops.is_empty(), "absent target block: dropped");
    assert_eq!(p3.conflicts, vec![bid(&dc)]);

    apply(&mut t, &p);
    for c in &p.changes {
        assert_eq!(&stored(&t, &c.id()), c);
    }
    let view = Scope::View("carry".into());
    assert_eq!(
        version_in(&t, &view, &dc),
        version_in(&s, &Scope::Canon, &dc)
    );
    assert_eq!(version_in(&t, &view, &n), None);
    let summary = t.read().view_show("carry").unwrap();
    assert_eq!(
        summary
            .changes
            .iter()
            .map(|c| c.ported_from)
            .collect::<Vec<_>>(),
        p.ported.iter().map(|x| Some(x.source)).collect::<Vec<_>>()
    );

    // an equal result agrees; a divergent target edit on a held base conflicts
    edit(&mut t, "eq", &dc, "dc two");
    let b2 = make(&h, &[l2]).unwrap();
    let p = prepare(&t, &b2, "carry-eq").unwrap();
    assert!(p.conflicts.is_empty(), "{:?}", p.conflicts);
    assert_eq!(p.changes[1].ops, src[1].1.ops);
    edit(&mut t, "div", &dc, "a target edit");
    let p = prepare(&t, &b2, "carry-div").unwrap();
    assert_eq!(p.conflicts, vec![bid(&dc)]);
    assert_eq!(p.changes[1].ops, src[1].1.ops, "a held base stays");
}

// ---------------------------------------------------------------------------------------------
// missing bases

#[test]
fn hpr2_missing_base_matrix() {
    let World {
        root,
        mut s,
        mut t,
        notes,
        dc,
        ..
    } = world();
    let w = write(
        &mut s,
        "a",
        json!([
            {"op": "update", "id": dc, "body": "one"},
            {"op": "create", "type": "claim", "id": "$m", "parent": notes, "body": "m one"}
        ]),
    );
    let m = w.created["$m"].clone();
    let l2 = land(&mut s, "a");
    write(
        &mut s,
        "b",
        json!([
            {"op": "update", "id": dc, "body": "two"},
            {"op": "update", "id": m, "body": "m two"}
        ]),
    );
    let l3 = land(&mut s, "b");
    let h = hub(&s, &root.path().join("hub"));
    let both = make(&h, &[l2, l3]).unwrap();
    let late = make(&h, &[l3]).unwrap();
    let src = source_changes(&s, &[l3]);
    let v2 = version_in(&s, &Scope::Canon, &dc).unwrap();
    let m2 = version_in(&s, &Scope::Canon, &m).unwrap();
    let as_create = |c: &Change| -> Vec<Op> {
        c.ops
            .iter()
            .map(|op| match op {
                Op::Update { block, .. } => Op::Create {
                    block: block.clone(),
                },
                other => other.clone(),
            })
            .collect()
    };

    // held bases stay as they are
    let p = prepare(&t, &both, "held").unwrap();
    let all = source_changes(&s, &[l2, l3]);
    for (new, (_, from)) in p.changes.iter().zip(&all) {
        assert_eq!(new.ops, from.ops);
    }
    assert!(p.conflicts.is_empty());

    // missing bases against the held note (a create conflicts) and the never-held claim (a
    // create applies)
    let p = prepare(&t, &late, "held-target").unwrap();
    assert_eq!(p.changes[0].ops, as_create(&src[0].1));
    assert_eq!(p.conflicts, vec![bid(&dc)]);
    apply(&mut t, &p);
    let c = stored(&t, &p.changes[0].id());
    assert_eq!(c.ops, as_create(&src[0].1), "stored as a create");
    for op in &c.ops {
        assert!(op.base().is_none(), "no stored base the target lacks");
    }
    let view = Scope::View("held-target".into());
    assert_eq!(version_in(&t, &view, &m), Some(m2), "never held: applies");
    assert_eq!(conflict_blocks(&t, "held-target"), vec![bid(&dc)]);

    // an equal target result agrees
    edit(&mut t, "eq", &dc, "two");
    assert_eq!(version_in(&t, &Scope::Canon, &dc), Some(v2));
    let p = prepare(&t, &late, "equal-target").unwrap();
    assert!(p.conflicts.is_empty(), "{:?}", p.conflicts);
    apply(&mut t, &p);
    assert_eq!(
        version_in(&t, &Scope::View("equal-target".into()), &dc),
        Some(v2)
    );

    // a deleted target conflicts
    write(&mut t, "del", json!([{"op": "delete", "id": dc}]));
    land(&mut t, "del");
    let p = prepare(&t, &late, "deleted-target").unwrap();
    assert_eq!(p.changes[0].ops, as_create(&src[0].1));
    assert_eq!(p.conflicts, vec![bid(&dc)]);
    apply(&mut t, &p);
    assert_eq!(conflict_blocks(&t, "deleted-target"), vec![bid(&dc)]);
}

#[test]
fn hpr2_missing_delete_refuses_or_keeps_empty_change() {
    let World {
        root,
        mut s,
        mut t,
        notes,
        dc,
        ..
    } = world();
    let w = write(
        &mut s,
        "a",
        json!([
            {"op": "create", "type": "claim", "id": "$m", "parent": notes, "body": "m"},
            {"op": "update", "id": dc, "body": "one"}
        ]),
    );
    let m = w.created["$m"].clone();
    let l2 = land(&mut s, "a");
    write(&mut s, "b", json!([{"op": "delete", "id": m}]));
    write(&mut s, "b", json!([{"op": "delete", "id": dc}]));
    let l3 = land(&mut s, "b");
    let h = hub(&s, &root.path().join("hub"));
    let late = make(&h, &[l3]).unwrap();
    let _ = l2;

    // the target holds the note: the second change's delete refuses the whole port, though the
    // first change's delete was satisfied
    let before = log_end(&t);
    let briefs = t.briefs().len();
    let e = prepare(&t, &late, "carry").unwrap_err();
    assert_eq!(refusal(&e), delete_refusal());
    assert_eq!(log_end(&t), before);
    assert_eq!(t.briefs().len(), briefs, "no briefing registered");
    assert!(t.read().view_head("carry").is_none());

    // once the target deletes the note, both deletes are satisfied: two changes, no ops
    write(&mut t, "del", json!([{"op": "delete", "id": dc}]));
    land(&mut t, "del");
    let p = prepare(&t, &late, "carry").unwrap();
    assert_eq!(p.changes.len(), 2);
    assert!(p.changes.iter().all(|c| c.ops.is_empty()));
    let src = source_changes(&s, &[l3]);
    for (mapped, (hash, _)) in p.ported.iter().zip(&src) {
        assert_eq!(mapped.source.change, *hash);
    }
    apply(&mut t, &p);
    let included = t.read().included("carry").to_vec();
    assert_eq!(
        included,
        p.changes.iter().map(Change::id).collect::<Vec<_>>()
    );
    let head = t.read().view_head("carry").unwrap().head;
    let op = t.store().reader().view_op(&head).unwrap();
    assert!(matches!(op.op, ViewOpKind::Include { .. }));
    for c in &included {
        let c = stored(&t, c);
        assert_eq!(c.provenance, person());
        assert!(c.links.ported_from.is_some());
    }
}

// ---------------------------------------------------------------------------------------------
// conflicts, proposals and resolutions

/// A target that edits the note on canon (change `ct`) and a port of two source edits of it:
/// the first conflicts, the second follows it.
struct Conflicted {
    root: tempfile::TempDir,
    t: Rdy,
    dc: String,
    ct: Hash,
    p: PreparedPort,
}

fn conflicted() -> Conflicted {
    let World {
        root,
        mut s,
        mut t,
        dc,
        ..
    } = world();
    let wt = write(
        &mut t,
        "mine",
        json!([{"op": "update", "id": dc, "body": "target"}]),
    );
    land(&mut t, "mine");
    let l2 = edit(&mut s, "a", &dc, "source one");
    let l3 = edit(&mut s, "b", &dc, "source two");
    let h = hub(&s, &root.path().join("hub"));
    let p = prepare(&t, &make(&h, &[l2, l3]).unwrap(), "carry").unwrap();
    apply(&mut t, &p);
    Conflicted {
        root,
        t,
        dc,
        ct: h_of(&wt.change),
        p,
    }
}

fn h_of(s: &str) -> Hash {
    s.parse().unwrap()
}

/// Commits an ordinary change to `view` of `t` that writes `body` on `dc` and depends on `deps`.
/// Its base is the view's version of `dc`, or canon's when a conflict leaves the view none.
fn resolve_raw(t: &mut Rdy, view: &str, dc: &str, body: &str, deps: Vec<Hash>) -> Hash {
    let st = t.read();
    let cur = [Scope::View(view.into()), Scope::Canon]
        .iter()
        .find_map(|scope| st.materialized(scope).unwrap().get(&bid(dc)).cloned())
        .unwrap();
    let mut next = cur.clone();
    next.body = body.into();
    let c = Change {
        view: view.into(),
        ops: vec![Op::Update {
            base: cur.version_hash(),
            block: next.clone(),
        }],
        deps,
        provenance: Provenance {
            actor: "human:qa".into(),
            run: None,
            role: Role::Writer,
            at_ms: T0 + 1,
        },
        justification: None,
        message: "resolve".into(),
        links: ChangeLinks::default(),
        read_identity: None,
    };
    let id = c.id();
    let s = t.store_mut();
    s.write(vec![
        codec::item_new(kinds::BLOCK, &next),
        codec::item(kinds::CHANGE, &c).unwrap(),
    ])
    .unwrap();
    s.view_append(
        view,
        vec![ViewOpKind::Include { changes: vec![id] }],
        &person(),
    )
    .unwrap();
    t.seal_open().unwrap();
    t.invalidate();
    id
}

fn propose(t: &mut Rdy, view: &str) -> Result<ProposalInfo, RdyError> {
    t.propose(&writer(), view, view, Some(crate::common::TEST_SUMMARY))
}

/// Proposes `view`: the version names `dc`'s conflict as blocking, and its landing is refused
/// as not ready, with the unresolved conflict.
fn assert_blocked(t: &mut Rdy, view: &str, dc: &str) {
    let prop = propose(t, view).unwrap();
    let blocking: Vec<&str> = prop.blocking.iter().map(|c| c.block.as_str()).collect();
    assert_eq!(blocking, [dc], "{prop:?}");
    crate::common::confirm_exceptions(t, &approver(), &prop);
    let e = t.land(&approver(), &prop.id).unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_NOT_READY, "{e:?}");
    assert!(
        e.issues.iter().any(|i| i.code == E_CONFLICT_UNRESOLVED),
        "{e:?}"
    );
}

/// Proposes and lands `view`, which carries no blocking conflict; returns the landing.
fn land_clean(t: &mut Rdy, view: &str) -> Hash {
    let prop = propose(t, view).unwrap();
    assert!(prop.blocking.is_empty(), "{prop:?}");
    crate::common::confirm_exceptions(t, &approver(), &prop);
    t.land(&approver(), &prop.id).unwrap().parse().unwrap()
}

#[test]
fn hpt2_port_conflict_blocks_proposal() {
    let Conflicted {
        root: _root,
        mut t,
        dc,
        p,
        ..
    } = conflicted();
    assert_eq!(p.conflicts, vec![bid(&dc)]);
    assert_eq!(conflict_blocks(&t, "carry"), vec![bid(&dc)]);
    assert_blocked(&mut t, "carry", &dc);
}

#[test]
fn hpt2_resolution_must_cover_port_sides() {
    let Conflicted {
        root: _root,
        mut t,
        dc,
        ct,
        p,
    } = conflicted();
    let (p1, p2) = (p.changes[0].id(), p.changes[1].id());
    assert_eq!(p.changes[1].deps, vec![p1]);
    // every port side stays: the second port change never settles the first
    let st = t.read();
    let cs = st.conflicts(&Scope::View("carry".into())).unwrap();
    let sides: BTreeSet<Option<Hash>> = cs[0].sides.iter().map(|s| s.change).collect();
    assert_eq!(sides, BTreeSet::from([Some(ct), Some(p1), Some(p2)]));
    drop(st);
    // an ordinary change that covers the target side and the later port side only leaves the
    // conflict open, and the proposal refused
    resolve_raw(&mut t, "carry", &dc, "partial", vec![ct, p2]);
    assert_eq!(conflict_blocks(&t, "carry"), vec![bid(&dc)]);
    assert_blocked(&mut t, "carry", &dc);
    // one that covers every side resolves it
    let st = t.read();
    let open: Vec<Hash> = st.conflicts(&Scope::View("carry".into())).unwrap()[0]
        .sides
        .iter()
        .filter_map(|s| s.change)
        .collect();
    drop(st);
    let mut deps = open;
    deps.extend([ct, p1, p2]);
    deps.sort();
    deps.dedup();
    resolve_raw(&mut t, "carry", &dc, "resolved", deps);
    assert!(conflict_blocks(&t, "carry").is_empty());
    let l = land_clean(&mut t, "carry");
    // the landing names each ported change with its source, in landing order
    let facts = history_facts::landing(&t.read(), l).unwrap();
    let ported: Vec<(Hash, PortedFrom)> = facts
        .ported_changes
        .iter()
        .map(|f| (f.change, f.ported_from))
        .collect();
    assert_eq!(
        ported,
        p.ported
            .iter()
            .map(|x| (x.change, x.source))
            .collect::<Vec<_>>()
    );
}

#[test]
fn hpr2_create_conflict_survives_every_reader() {
    let World {
        root,
        mut s,
        mut t,
        dc,
        ..
    } = world();
    edit(&mut s, "a", &dc, "one");
    let l3 = edit(&mut s, "b", &dc, "two");
    let h = hub(&s, &root.path().join("hub"));
    let p = prepare(&t, &make(&h, &[l3]).unwrap(), "carry").unwrap();
    assert!(matches!(p.changes[0].ops[0], Op::Create { .. }));
    apply(&mut t, &p);
    let check = |t: &Rdy, what: &str| {
        assert_eq!(conflict_blocks(t, "carry"), vec![bid(&dc)], "{what}");
        let c = stored(t, &p.changes[0].id());
        assert!(matches!(c.ops[0], Op::Create { .. }), "{what}");
        let show = t.read().view_show("carry").unwrap();
        assert_eq!(show.conflicts.len(), 1, "{what}");
        assert_eq!(
            show.changes[0].ported_from,
            Some(p.ported[0].source),
            "{what}"
        );
    };
    check(&t, "written");
    // the proposal names it as blocking, and its landing is refused
    assert_blocked(&mut t, "carry", &dc);
    // a reopen reads the same conflict from the stored ops alone
    t.close().unwrap();
    let mut t = Rdy::open(&root.path().join("t")).unwrap();
    check(&t, "reopened");
    // a resolution that covers both sides lands
    let st = t.read();
    let deps: Vec<Hash> = st.conflicts(&Scope::View("carry".into())).unwrap()[0]
        .sides
        .iter()
        .filter_map(|s| s.change)
        .collect();
    drop(st);
    resolve_raw(&mut t, "carry", &dc, "resolved", deps);
    assert!(conflict_blocks(&t, "carry").is_empty());
    let l = land_clean(&mut t, "carry");
    let facts = history_facts::landing(&t.read(), l).unwrap();
    assert_eq!(facts.ported_changes.len(), 1);
    assert_eq!(facts.ported_changes[0].ported_from, p.ported[0].source);
    assert_eq!(
        version_in(&t, &Scope::Canon, &dc)
            .map(|v| { t.store().reader().get_block(&v).unwrap().body }),
        Some("resolved".to_string())
    );
}

// ---------------------------------------------------------------------------------------------
// briefing

#[test]
fn hpt2_port_uses_target_briefing_without_exemption() {
    let World {
        root, mut s, t, c, ..
    } = world();
    // the source edits stable content under its own briefing
    write_as(
        &mut s,
        "a",
        json!([{"op": "update", "id": c, "body": "Refunds take three days."}]),
        &[&c],
        "faster refunds",
        editorial("say it as it is"),
    );
    let l2 = land(&mut s, "a");
    // the target's own claim moves on its canon, so its briefing differs from the source's
    let mut t = t;
    write_as(
        &mut t,
        "ctx",
        json!([{"op": "update", "id": c, "body": "Refunds take four days."}]),
        &[&c],
        "a target edit",
        editorial("the target's own words"),
    );
    land(&mut t, "ctx");
    let source_token = source_changes(&s, &[l2])[0].1.links.briefing;
    assert!(source_token.is_some());
    let h = hub(&s, &root.path().join("hub"));
    let b = make(&h, &[l2]).unwrap();
    let p = prepare(&t, &b, "carry").unwrap();
    // the token is the target's normal root briefing of the block, governance included
    let tip = t.read().canon_head().unwrap();
    let normal = t
        .read()
        .brief_ids(&Scope::At(tip), &BTreeSet::from([bid(&c)]), None)
        .unwrap();
    assert_eq!(p.briefing, Some(normal.record.token));
    assert_ne!(p.briefing, source_token, "never the source's token");
    assert!(p.changes.iter().all(|x| x.links.briefing == p.briefing));
    assert!(
        t.briefs().get(&p.briefing.unwrap().to_string()).is_some(),
        "registered"
    );

    // no exemption: a cited port briefing is checked as a write's is, drafts too
    let st = t.read();
    let scope = Scope::At(tip);
    let m = st.materialized(&scope).unwrap();
    let ops: Vec<&Op> = b.content().landings[0].changes[0].ops.iter().collect();
    let base = |v: &Hash| -> Result<Block, RdyError> { Ok(st.reader().get_block(v)?) };
    let reg = t.briefs();
    let e =
        port::port_brief(&st, &scope, &m, &ops, &base, PortBrief::Cite(None), &reg).unwrap_err();
    assert_eq!(
        refusal(&e),
        (
            E_BRIEF_REQUIRED,
            "a port needs a target briefing",
            "brief the target blocks and retry with the token"
        )
    );
    let unknown = "ab".repeat(32);
    let e = port::port_brief(
        &st,
        &scope,
        &m,
        &ops,
        &base,
        PortBrief::Cite(Some(&unknown)),
        &reg,
    )
    .unwrap_err();
    assert_eq!(
        refusal(&e),
        (
            E_BRIEF_STALE,
            "the cited briefing abababab is unknown: the daemon restarted, or the briefing was evicted",
            "brief these blocks (rdy_brief) and retry with the token"
        )
    );
    // the source's token is unknown here too
    let e = port::port_brief(
        &st,
        &scope,
        &m,
        &ops,
        &base,
        PortBrief::Cite(Some(&source_token.unwrap().to_string())),
        &reg,
    )
    .unwrap_err();
    assert_eq!(e.code, E_BRIEF_STALE);
    // the made token, cited, passes
    let token = p.briefing.unwrap().to_string();
    let (got, rec) = port::port_brief(
        &st,
        &scope,
        &m,
        &ops,
        &base,
        PortBrief::Cite(Some(&token)),
        &reg,
    )
    .unwrap();
    assert_eq!((got, rec.is_none()), (p.briefing, true));
}

// ---------------------------------------------------------------------------------------------
// pins, links and provenance

const AGENTS: &str = "pf-AGNT1";

fn cited() -> RepoId {
    RepoId([7; 16])
}

fn cited_x() -> Block {
    Block {
        id: bid("d-FRGNX"),
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

fn version_of(b: &Block) -> Hash {
    Hash::of(&codec::item_new(kinds::BLOCK, b).1)
}

fn content_of(b: &Block) -> Hash {
    Hash::of(format!("content of {} {}", b.id, b.body).as_bytes())
}

/// Delivers `x` to `r` through its link `link`, straight to the store.
fn deliver(r: &mut Rdy, link: &str, x: &Block) {
    let entry = ForeignEntry {
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
    };
    r.store_mut()
        .put_foreign(&entry, std::slice::from_ref(x))
        .unwrap();
    r.publish_after_foreign_for_tests().unwrap();
}

#[test]
fn hpr2_pins_are_data_and_foreign_pins_need_delivery() {
    let World {
        root,
        mut s,
        mut t,
        notes,
        ..
    } = world();
    // a local pin to a claim the target never holds
    let w = write(
        &mut s,
        "a",
        json!([{"op": "create", "type": "claim", "id": "$m", "parent": notes, "body": "Only here."}]),
    );
    let m = w.created["$m"].clone();
    land(&mut s, "a");
    let w = write(
        &mut s,
        "b",
        json!([{"op": "create", "type": "claim", "id": "$p", "parent": notes, "body": "Points at it.",
            "links": {"describes": [{"target": m}]}}]),
    );
    let pointer = w.created["$p"].clone();
    let lp = land(&mut s, "b");
    // a foreign pin: the source links the cited repo and cites its block
    let w = write(
        &mut s,
        "c",
        json!([{"op": "create", "type": "repo_link", "id": "$l", "parent": notes,
            "fields": {"alias": "core", "repo": cited().to_string(), "profile": AGENTS}}]),
    );
    let link = w.created["$l"].clone();
    land(&mut s, "c");
    let x = cited_x();
    deliver(&mut s, &link, &x);
    write(
        &mut s,
        "d",
        json!([{"op": "create", "type": "claim", "id": "$f", "parent": notes, "body": "Per {{core:d-FRGNX}}."}]),
    );
    let lf = land(&mut s, "d");
    let h = hub(&s, &root.path().join("hub"));

    // the local pin is data: the port applies and keeps it
    let p = prepare(&t, &make(&h, &[lp]).unwrap(), "local-pin").unwrap();
    apply(&mut t, &p);
    let got = t
        .read()
        .materialized(&Scope::View("local-pin".into()))
        .unwrap()
        .get(&bid(&pointer))
        .unwrap()
        .clone();
    let want = s
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .get(&bid(&pointer))
        .unwrap()
        .clone();
    assert!(got.pins.contains_key(&bid(&m)), "{got:?}");
    assert_eq!(got, want, "pins and links as the source wrote them");
    assert_eq!(version_in(&t, &Scope::View("local-pin".into()), &m), None);
    let m_version = version_in(&s, &Scope::Canon, &m).unwrap();
    assert!(
        !t.store()
            .reader()
            .has_kind(kinds::BLOCK, &m_version)
            .unwrap(),
        "no extra version"
    );

    // an unheld foreign pin: the existing triple, with the repo's short id (no live link here)
    let bf = make(&h, &[lf]).unwrap();
    let before = log_end(&t);
    let e = prepare(&t, &bf, "foreign").unwrap_err();
    let repo12 = &cited().to_string()[..12];
    let short = &content_of(&x).to_string()[..12];
    assert_eq!(
        refusal(&e),
        (
            E_FOREIGN_VERSION,
            format!("{repo12}:d-FRGNX has no delivered version {short}").as_str(),
            format!(
                "wait for the delivery that holds it, or pin a delivered version: rdy_links {{op: \"show\", alias}} / rdy links show {repo12}"
            )
            .as_str()
        )
    );
    assert_eq!(log_end(&t), before);
    // held once the target has its own delivery: the port brings no version or delivery
    deliver(&mut t, &link, &x);
    let p = prepare(&t, &bf, "foreign").unwrap();
    let frames = p.object_frames().unwrap();
    for (raw, payload) in &frames {
        let kind = rdy_store::codec::logical_kind(*raw);
        assert!([kinds::BLOCK, kinds::CHANGE].contains(&kind), "kind {kind}");
        assert_ne!(Hash::of(payload), version_of(&x));
    }
    apply(&mut t, &p);
    assert_eq!(p.changes.len(), 1);
}

#[test]
fn hpr2_port_drops_intent_and_alternative() {
    let World {
        root,
        mut s,
        mut t,
        dc,
        ..
    } = world();
    for v in ["alt-a", "alt-b"] {
        s.view_create(&writer(), v, None, false).unwrap();
    }
    let set = s
        .alt_create(
            &writer(),
            "Which note?",
            None,
            &[
                rdy_engine::alt::AltMemberInput {
                    label: "a".into(),
                    view: "alt-a".into(),
                },
                rdy_engine::alt::AltMemberInput {
                    label: "b".into(),
                    view: "alt-b".into(),
                },
            ],
        )
        .unwrap();
    s.alt_abandon(&writer(), &set.id, "neither").unwrap();
    let intent = s
        .intent_post(
            &writer(),
            Some("w"),
            None,
            std::slice::from_ref(&dc),
            "edit the note",
            None,
        )
        .unwrap();
    let w = s
        .write_linked(
            &writer(),
            WriteRequest {
                view: "w".into(),
                ops: ops(json!([{"op": "update", "id": dc, "body": "with links"}])),
                justification: None,
                message: "with an intent and an alternative".into(),
            },
            ChangeLinks {
                intent: Some(s.read().resolve_intent(&intent.id).unwrap().0),
                alternative: Some(AltChoice {
                    set: h_of(&set.set),
                    member: "alt-a".into(),
                }),
                ..Default::default()
            },
            None,
        )
        .unwrap();
    let src = stored(&s, &h_of(&w.change));
    assert!(src.links.intent.is_some() && src.links.alternative.is_some());
    let l = land(&mut s, "w");
    let hb = hub(&s, &root.path().join("hub"));
    let p = prepare(&t, &make(&hb, &[l]).unwrap(), "carry").unwrap();
    let new = &p.changes[0];
    assert_eq!(new.ops, src.ops);
    assert_eq!(
        (new.links.intent, new.links.alternative.clone()),
        (None, None)
    );
    apply(&mut t, &p);
    // every typed reference the target keeps resolves
    t.store().check_objects().unwrap();
}

// ---------------------------------------------------------------------------------------------
// replays keep the origin

const URL: &str = "https://127.0.0.1:9/r/demo";

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &to.join(e.file_name()));
        } else {
            std::fs::copy(&p, to.join(e.file_name())).unwrap();
        }
    }
}

/// Records `applied` as the local project's applied hub canon.
fn applied(local: &mut Rdy, repo: RepoId, applied: Hash) {
    local
        .store_mut()
        .write_sync(
            "origin",
            vec![SyncStep::Canon(SyncCanon {
                repo_id: repo,
                url: URL.into(),
                fetched: Some(applied),
                applied: Some(applied),
                held: None,
            })],
        )
        .unwrap();
}

/// Fetches the hub project's canon since `base` into `local`, then pulls.
fn pull(hub: &Rdy, local: &mut Rdy, base: Hash, dir: &Path) -> sync_replay::PullOutcome {
    let tip = hub.store().reader().canon_head().unwrap().unwrap();
    let mut out = Vec::new();
    stream_export::write_history(
        &hub.read(),
        StreamPurpose::CanonHistory,
        tip,
        Some(base),
        &BTreeSet::new(),
        &mut out,
    )
    .unwrap();
    let path = dir.join(format!("batch-{tip}"));
    std::fs::write(&path, out).unwrap();
    sync_fetch::fetch(
        local,
        &[File::open(&path).unwrap()],
        &FetchTarget {
            remote: "origin".into(),
            url: URL.into(),
            purpose: StreamPurpose::CanonHistory,
            root: tip,
            view: None,
        },
    )
    .unwrap();
    sync_replay::pull(
        local,
        &Session::human("human:puller".into(), Role::Writer).unwrap(),
        &PullRequest {
            remote: "origin".into(),
            hub: HubState {
                repo_id: Some(repo(hub)),
                canon: Some(tip),
            },
            take: false,
            strict: false,
        },
    )
    .unwrap()
}

#[test]
fn hpr2_replays_copy_ported_from() {
    let World {
        root,
        mut s,
        t,
        notes,
        dc,
        seed,
        ..
    } = world();
    // the target line's hub project is `t`; `local` is a working copy of it
    let t_dir = root.path().join("t");
    let local_dir = root.path().join("local");
    t.close().unwrap();
    copy_dir(&t_dir, &local_dir);
    let mut hubp = Rdy::open(&t_dir).unwrap();
    let mut local = Rdy::open(&local_dir).unwrap();
    applied(&mut local, repo(&hubp), seed);
    // the source: a claim, then a landing whose first change deletes it and whose second edits
    // the note
    let w = write(
        &mut s,
        "a",
        json!([{"op": "create", "type": "claim", "id": "$m", "parent": notes, "body": "m"}]),
    );
    let m = w.created["$m"].clone();
    land(&mut s, "a");
    write(&mut s, "b", json!([{"op": "delete", "id": m}]));
    write(
        &mut s,
        "b",
        json!([{"op": "update", "id": dc, "body": "ported"}]),
    );
    let l3 = land(&mut s, "b");
    let h = hub(&s, &root.path().join("hub"));
    let p = prepare(&local, &make(&h, &[l3]).unwrap(), "carry").unwrap();
    assert!(p.changes[0].ops.is_empty(), "an empty-op change");
    apply(&mut local, &p);
    let check = |c: &Change, from: &Change, what: &str| {
        assert_eq!(c.links.ported_from, from.links.ported_from, "{what}");
        assert_eq!(c.links.briefing, from.links.briefing, "{what}");
        assert_eq!(c.ops, from.ops, "{what}");
    };

    // same-store replay of each ported change, the empty one included
    local.view_create(&writer(), "again", None, false).unwrap();
    let res = local
        .view_replay(
            &writer(),
            "again",
            &p.changes
                .iter()
                .map(|c| c.id().to_string())
                .collect::<Vec<_>>(),
        )
        .unwrap();
    assert_eq!(res.replayed.len(), 2);
    for r in &res.replayed {
        let c = stored(&local, &h_of(&r.change));
        let from = stored(&local, &h_of(&r.source));
        assert_eq!(c.links.replayed_from, Some(h_of(&r.source)));
        check(&c, &from, "same store");
    }

    // a clean pull replays the landed port onto the hub's new tip
    let lp = land(&mut local, "carry");
    write(
        &mut hubp,
        "h1",
        json!([{"op": "create", "type": "doc", "fields": {"name": "h1", "title": "H1", "doc_type": "page"}}]),
    );
    let t1 = land(&mut hubp, "h1");
    let out = pull(&hubp, &mut local, seed, root.path());
    assert_eq!(out.status, PULLED, "{out:?}");
    let replayed = h_of(&out.replayed[0].landing);
    let rec = local.store().reader().landing(&replayed).unwrap();
    assert_eq!(rec.rebased_from, [lp]);
    let olds = local.store().reader().landing(&lp).unwrap().changes;
    assert_eq!(rec.changes.len(), olds.len());
    for (new, old) in rec.changes.iter().zip(&olds) {
        let (c, from) = (stored(&local, new), stored(&local, old));
        assert_eq!(c.links.replayed_from, Some(*old));
        check(&c, &from, "clean pull");
    }
    applied(&mut local, repo(&hubp), t1);

    // a stopped pull: the hub edits the note too, so the replay stops and the view replay
    // keeps the rest
    write(
        &mut hubp,
        "h2",
        json!([{"op": "update", "id": dc, "body": "the hub's"}]),
    );
    land(&mut hubp, "h2");
    let out = pull(&hubp, &mut local, t1, root.path());
    assert_eq!(out.status, PULLED_WITH_CONFLICTS, "{out:?}");
    assert_eq!(out.not_replayed[0].reason, StopReason::Conflict);
    let view = out.view.clone().unwrap();
    let held = local.store().included(&view).unwrap();
    assert_eq!(held.len(), olds.len());
    for c in held.iter() {
        let c = stored(&local, c);
        let from = stored(&local, &c.links.replayed_from.unwrap());
        check(&c, &from, "stopped pull");
        assert!(c.links.ported_from.is_some());
    }
}

// ---------------------------------------------------------------------------------------------
// parents

#[test]
fn hpc2_parent_checks() {
    let World {
        root,
        mut s,
        mut t,
        notes,
        a,
        b,
        ..
    } = world();
    // l2 creates a section; l3 puts a claim under it
    let w = write(
        &mut s,
        "p",
        json!([{"op": "create", "type": "section", "id": "$p", "parent": notes, "fields": {"title": "New section"}}]),
    );
    let sec = w.created["$p"].clone();
    let l2 = land(&mut s, "p");
    write(
        &mut s,
        "q",
        json!([{"op": "create", "type": "claim", "parent": sec, "body": "Under it."}]),
    );
    let l3 = land(&mut s, "q");
    // l4 retitles the notes doc and puts a claim under it
    write(
        &mut s,
        "r",
        json!([
            {"op": "update", "id": notes, "fields": {"title": "Notes, in the source"}},
            {"op": "create", "type": "claim", "parent": notes, "body": "Under notes."}
        ]),
    );
    let l4 = land(&mut s, "r");
    // l5 moves section a under section b
    write(&mut s, "m", json!([{"op": "move", "id": a, "parent": b}]));
    let l5 = land(&mut s, "m");
    let h = hub(&s, &root.path().join("hub"));
    let unchanged = |t: &Rdy, before: rdy_store::segment::Location| {
        assert_eq!(log_end(t), before);
        assert!(t.read().view_head("carry").is_none());
    };

    // a missing parent
    let before = log_end(&t);
    let e = prepare(&t, &make(&h, &[l3]).unwrap(), "carry").unwrap_err();
    assert_eq!(
        refusal(&e),
        (
            E_DEPS_MISSING,
            format!("the port needs parent {sec} present and clean in the target").as_str(),
            "carry the change that creates the parent first, or resolve its conflict"
        )
    );
    unchanged(&t, before);
    // the parent created cleanly in the same call
    let p = prepare(&t, &make(&h, &[l2, l3]).unwrap(), "carry").unwrap();
    assert!(p.conflicts.is_empty());

    // a conflicted parent: the target retitled the notes doc its own way
    write(
        &mut t,
        "title",
        json!([{"op": "update", "id": notes, "fields": {"title": "Notes, in the target"}}]),
    );
    land(&mut t, "title");
    let before = log_end(&t);
    let e = prepare(&t, &make(&h, &[l4]).unwrap(), "carry").unwrap_err();
    assert_eq!(e.code, E_DEPS_MISSING, "{e:?}");
    assert_eq!(
        e.message,
        format!("the port needs parent {notes} present and clean in the target")
    );
    unchanged(&t, before);

    // a move that makes a loop only against the target: the target put b under a
    write(&mut t, "mv", json!([{"op": "move", "id": b, "parent": a}]));
    land(&mut t, "mv");
    let before = log_end(&t);
    let e = prepare(&t, &make(&h, &[l5]).unwrap(), "carry").unwrap_err();
    let fix = "move one block out of the other in the target, then carry again";
    let one = format!("the port would make {a} and {b} each other's ancestors");
    let other = format!("the port would make {b} and {a} each other's ancestors");
    assert_eq!((e.code, e.fix.as_str()), (E_BAD_INPUT, fix), "{e:?}");
    assert!(e.message == one || e.message == other, "{e:?}");
    unchanged(&t, before);
}

// ---------------------------------------------------------------------------------------------
// target removals

#[test]
fn hpt2_port_refuses_target_removals() {
    let World {
        root,
        mut s,
        mut t,
        notes,
        dc,
        ..
    } = world();
    let (lf, _, _) = land_figure(&mut s, "f", &notes, &[5u8; 64]);
    let l3 = edit(&mut s, "e", &dc, "An edit.");
    let h = hub(&s, &root.path().join("hub"));
    let bf = make(&h, &[lf]).unwrap();
    let b3 = make(&h, &[l3]).unwrap();
    // the target removes the note's seed version: a bundle whose op bases it fails
    remove(&mut t, "rm", &dc);
    let before = log_end(&t);
    let e = prepare(&t, &b3, "carry").unwrap_err();
    assert_eq!(
        refusal(&e),
        (
            E_REDACTED_STATE,
            "the port bundle holds a version or blob removed in this line",
            "carry the change after the line's removal is replaced"
        )
    );
    assert_eq!(log_end(&t), before);
    // a bundle of content the target never removed still prepares, with its blob once
    let p = prepare(&t, &bf, "carry").unwrap();
    let kinds_of: Vec<u8> = p
        .blobs
        .iter()
        .map(|(raw, _)| rdy_store::codec::logical_kind(*raw))
        .collect();
    assert_eq!(kinds_of, [kinds::BLOB_CHUNK, kinds::BLOB_MANIFEST]);
    apply(&mut t, &p);
}
