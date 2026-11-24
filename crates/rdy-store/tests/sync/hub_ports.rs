//! hpt3 (D-A3H7S, D-DZDB7): a hub port's records and its new view head move in one checked
//! canon move. The ported change (`links.ported_from` set) turns the feature `ported_change` on
//! in the same batch. A failed append writes nothing; a second carry of the view is behind.
use rdy_model::*;
use rdy_store::format::FEATURE_PORTED_CHANGE;
use rdy_store::hub_canon::{CanonMove, Moved};
use rdy_store::segment::Location;
use rdy_store::view::{ViewOp, ViewOpKind};
use rdy_store::{Store, codec, kinds};
use std::collections::BTreeMap;
use std::path::Path;

const ORIGIN: Location = Location {
    segment: 0,
    offset: 0,
};

/// The writer's clock of every move here (injected).
const AT: i64 = 1_700_000_000_123;

type Records = Vec<(u8, Vec<u8>)>;

/// A store's log end, refs and feature names, and its batch count.
type State = (Location, Vec<(String, Vec<u8>)>, Vec<String>, u64);

fn state(s: &Store) -> State {
    (
        s.log_end(),
        s.reader().refs_with_prefix("").unwrap(),
        s.features()
            .features
            .iter()
            .map(|f| f.name.clone())
            .collect(),
        s.write_count(),
    )
}

fn prov(at: i64) -> Provenance {
    Provenance {
        actor: "human:a".into(),
        run: Some("hub-carry".into()),
        role: Role::Writer,
        at_ms: at,
    }
}

fn block(id: &str, body: &str) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: "claim".into(),
        parent: None,
        order: "a".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    }
}

/// The sync frames of `s` from `from` to its end, in log order.
fn frames(s: &Store, from: Location) -> Records {
    let mut out = vec![];
    s.reader()
        .for_each_frame_from(from, s.log_end(), |_, raw, _, payload, _| {
            if rdy_store::sync_stream::is_sync_kind(codec::logical_kind(raw)) {
                out.push((raw, payload.to_vec()));
            }
            Ok(())
        })
        .unwrap();
    out
}

/// A seeded canon of one landing that creates `b-00001`, built in the scratch holder `src`;
/// returns the landing and its frames.
fn seed(src: &mut Store) -> (Hash, Records) {
    let from = src.log_end();
    let b = block("b-00001", "one");
    src.put(kinds::BLOCK, &b).unwrap();
    let c = src
        .put(
            kinds::CHANGE,
            &Change {
                view: "work".into(),
                ops: vec![Op::Create { block: b.clone() }],
                deps: vec![],
                provenance: prov(1),
                justification: None,
                message: "one".into(),
                links: ChangeLinks::default(),
                read_identity: None,
            },
        )
        .unwrap();
    let state = src
        .put(
            kinds::SNAPSHOT,
            &StateSnapshot {
                blocks: [(b.id.clone(), b.version_hash())].into_iter().collect(),
            },
        )
        .unwrap();
    let proposal = src
        .put(
            kinds::PROPOSAL,
            &Proposal {
                stable: None,
                prev: None,
                title: "one".into(),
                view: "work".into(),
                changes: vec![c],
                alternative: None,
                provenance: prov(2),
                canon_at: None,
                summary: None,
            },
        )
        .unwrap();
    let landing = src
        .put_landing_for_tests(&LandingRecord {
            proposal,
            prev: None,
            state,
            approved_by: prov(3),
            changes: vec![c],
            chosen: None,
            checks: vec![],
            assurance: Assurance::Local,
            relayed_by: None,
            relay_evidence: None,
            approved_version: None,
            rebased_from: vec![],
            evidence: None,
            assessment: None,
            policy: None,
            read_identity: None,
        })
        .unwrap();
    (landing, frames(src, from))
}

/// A carry's records, built in `src`: a ported change that creates `b-00002`, then the view
/// `carry`'s Create on `tip` and its Commit. Returns the root, the head, the change and the
/// frames.
fn carry(src: &mut Store, tip: Hash) -> (Hash, Hash, Hash, Records) {
    let from = src.log_end();
    let b = block("b-00002", "two");
    src.put(kinds::BLOCK, &b).unwrap();
    let c = src
        .put(
            kinds::CHANGE,
            &Change {
                view: "carry".into(),
                ops: vec![Op::Create { block: b }],
                deps: vec![],
                provenance: prov(4),
                justification: None,
                message: "two".into(),
                links: ChangeLinks {
                    ported_from: Some(PortedFrom {
                        repo_id: RepoId([3; 16]),
                        change: Hash([4; 32]),
                    }),
                    ..Default::default()
                },
                read_identity: None,
            },
        )
        .unwrap();
    let op = |prev, op| ViewOp {
        view: "carry".into(),
        prev,
        op,
        actor: "human:a".into(),
        run: Some("hub-carry".into()),
        at_ms: AT,
    };
    let root = src
        .put(
            kinds::VIEW_OP,
            &op(
                None,
                ViewOpKind::Create {
                    base: Some(tip),
                    owner: None,
                    pinned: false,
                },
            ),
        )
        .unwrap();
    let head = src
        .put(
            kinds::VIEW_OP,
            &op(Some(root), ViewOpKind::Commit { change: c }),
        )
        .unwrap();
    (root, head, c, frames(src, from))
}

fn holder(dir: &Path, name: &str) -> Store {
    Store::create_holder(&dir.join(name), RepoId([7; 16])).unwrap()
}

fn set(root: Hash) -> CanonMove {
    CanonMove::SetViewHead {
        name: "carry".into(),
        root,
    }
}

#[test]
fn hpt3_port_view_head_and_records_move_in_one_batch() {
    let dir = tempfile::tempdir().unwrap();
    let mut src = holder(dir.path(), "src");
    let (l1, r1) = seed(&mut src);
    let (root, head, c, records) = carry(&mut src, l1);
    let mut h = holder(dir.path(), "h");
    let b = h.reader().check_move_batch(r1, l1).unwrap();
    assert_eq!(
        h.move_canon(&b, &CanonMove::Seed, None, AT).unwrap(),
        Moved::Moved { tip: l1 }
    );
    let before = state(&h);
    assert!(!before.2.contains(&FEATURE_PORTED_CHANGE.to_string()));
    assert!(ORIGIN < before.0);

    // a failed append: nothing, the feature included
    let batch = h.reader().check_move_batch(records.clone(), head).unwrap();
    h.fail_next_append_after(40);
    h.move_canon(&batch, &set(root), None, AT).unwrap_err();
    assert_eq!(state(&h), before);
    drop(h);
    let mut h = Store::open(&dir.path().join("h")).unwrap();
    assert_eq!(state(&h).1, before.1, "nothing durable");
    assert_eq!(h.view_head("carry").unwrap(), None);
    let before = state(&h);

    // the move: the records, the feature and the view head in one batch
    let batch = h.reader().check_move_batch(records.clone(), head).unwrap();
    assert_eq!(
        h.move_canon(&batch, &set(root), None, AT).unwrap(),
        Moved::Moved { tip: head }
    );
    let after = state(&h);
    assert_eq!(after.3, before.3 + 1, "one batch");
    assert!(after.2.contains(&FEATURE_PORTED_CHANGE.to_string()));
    let new_refs: Vec<&String> = after
        .1
        .iter()
        .filter(|r| !before.1.contains(r))
        .map(|(n, _)| n)
        .collect();
    assert_eq!(
        new_refs,
        [
            &rdy_store::format::REF_STORE_FEATURES.to_string(),
            &"view/carry".to_string()
        ]
    );
    assert_eq!(h.canon_head().unwrap(), Some(l1), "canon stays");
    let vh = h.view_head("carry").unwrap().unwrap();
    assert_eq!(
        (vh.head, vh.base, vh.len, vh.owner, vh.pinned, vh.closed),
        (head, Some(l1), 1, None, false, false)
    );
    let stored = h.reader().change(&c).unwrap();
    assert_eq!(
        stored.links.ported_from,
        Some(PortedFrom {
            repo_id: RepoId([3; 16]),
            change: Hash([4; 32]),
        })
    );

    // a second carry of the view is behind and writes nothing
    let before = state(&h);
    let batch = h.reader().check_move_batch(records, head).unwrap();
    assert_eq!(
        h.move_canon(&batch, &set(root), None, AT).unwrap(),
        Moved::Behind { tip: Some(head) }
    );
    assert_eq!(state(&h), before);
    h.check_holder().unwrap();
}
