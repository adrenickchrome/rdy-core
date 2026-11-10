//! rpl20 (repo links, D-8VJAZ "adopt cards"): the frozen-decision exception of an adoption. An
//! accepted decision's foreign pin moves only through a [`PinAdoption`]: the exact moves one
//! person's adopt answer bound, each to content a delivery holds. Nothing else may change in the
//! same write. A write with no such authority (a raw commit, a replayed change, an ordinary
//! landing) keeps the freeze, and neither a tracking edit nor a re-affirmation can carry a
//! foreign pin move.
use rdy_model::decision::{self, only_foreign_pins_adopted};
use rdy_model::engine_fields::REAFFIRMED;
use rdy_model::foreign::{
    ForeignDerived, ForeignEntry, ForeignEvent, ForeignId, ForeignPin, ForeignPublication,
    ForeignSource, ForeignVersion, selection_identity,
};
use rdy_model::*;
use rdy_store::{AdoptProposeRequest, PinAdoption, Store, StoreError, adopt_view, codec, kinds};
use std::collections::{BTreeMap, BTreeSet};

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn cited() -> RepoId {
    RepoId([7; 16])
}

fn prov(role: Role) -> Provenance {
    Provenance {
        actor: "human:p".into(),
        run: None,
        role,
        at_ms: 1,
    }
}

fn change(view: &str, ops: Vec<Op>) -> Change {
    Change {
        view: view.into(),
        ops,
        deps: vec![],
        provenance: prov(Role::Writer),
        justification: None,
        message: String::new(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

/// A cited claim, as the cited side delivers it.
fn claim(body: &str) -> Block {
    Block {
        id: id("c-FRGNC"),
        block_type: "claim".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
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

fn pin_of(b: &Block) -> ForeignPin {
    ForeignPin {
        content: b.content_hash(),
        encoding: 0,
    }
}

fn fv(b: &Block) -> ForeignVersion {
    ForeignVersion {
        id: b.id.clone(),
        version: version_of(b),
        content: b.content_hash(),
        encoding: 0,
        derived: ForeignDerived::default(),
    }
}

fn target() -> ForeignId {
    ForeignId::new(cited(), id("c-FRGNC"))
}

/// Two deliveries of the cited claim from one link: `v1`, then `v2` that changes it.
fn deliver(s: &mut Store, v1: &Block, v2: &Block) {
    let source = ForeignSource::Link {
        link: "rl-00001".into(),
        site: "hub.example".into(),
        at_ms: 1_760_000_000_000,
    };
    let mut head = None;
    for (n, (b, first)) in [(v1, true), (v2, false)].into_iter().enumerate() {
        let sel = BTreeMap::from([(b.id.clone(), version_of(b))]);
        let entry = ForeignEntry {
            repo: cited(),
            source: source.clone(),
            event: ForeignEvent::Delivery {
                prev: head,
                publication: ForeignPublication {
                    tag: format!("v{}", n + 1),
                    profile: "pf-AGNT1".into(),
                    profile_version: None,
                },
                set: None,
                added: if first { vec![fv(b)] } else { vec![] },
                changed: if first { vec![] } else { vec![fv(b)] },
                removed: vec![],
                shells: vec![],
                selection: selection_identity(sel.iter()).unwrap(),
            },
        };
        head = Some(s.put_foreign(&entry, std::slice::from_ref(b)).unwrap());
    }
}

/// A decision of `status` that cites the claim with `pin`.
fn decision(did: &str, status: &str, pin: ForeignPin) -> Block {
    let mut d = Block {
        id: id(did),
        block_type: decision::DECISION.into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::from([
            ("status".to_string(), FieldValue::Str(status.into())),
            (
                "statement".to_string(),
                FieldValue::Str(format!("We follow {{{{{}:c-FRGNC}}}}.", cited())),
            ),
            ("owner".to_string(), FieldValue::Str("ops".into())),
        ]),
        body: String::new(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    };
    d.foreign.pins.insert(target(), pin);
    d
}

/// A store with both deliveries and the decisions `D-ACCPT` (accepted) and `D-REJCT`
/// (rejected) landed, each pinning the first delivered version; `owner` is a decision tracking
/// field. Returns the store, the two claim versions and the two decisions.
fn landed() -> (tempfile::TempDir, Store, Block, Block, Block, Block) {
    let d = tempfile::tempdir().unwrap();
    let mut s = Store::create(&d.path().join("s")).unwrap();
    s.set_tracking(Tracking::new(BTreeMap::from([(
        decision::DECISION.to_string(),
        BTreeSet::from(["owner".to_string()]),
    )])));
    let (v1, v2) = (
        claim("Every service logs in UTC."),
        claim("Every service logs in local time."),
    );
    deliver(&mut s, &v1, &v2);
    let acc = decision("D-ACCPT", "accepted", pin_of(&v1));
    let rej = decision("D-REJCT", "rejected", pin_of(&v1));
    s.commit_to_view(
        "seed",
        change(
            "seed",
            vec![
                Op::Create { block: acc.clone() },
                Op::Create { block: rej.clone() },
            ],
        ),
    )
    .unwrap();
    let p = s.propose("seed", "seed", prov(Role::Writer)).unwrap();
    s.land(&p, prov(Role::Approver)).unwrap();
    (d, s, v1, v2, acc, rej)
}

/// `d` with its pin of the claim moved to `pin`.
fn moved(d: &Block, pin: ForeignPin) -> Block {
    let mut n = d.clone();
    n.foreign.pins.insert(target(), pin);
    n
}

fn update(d: &Block, new: Block) -> Op {
    Op::Update {
        base: d.version_hash(),
        block: new,
    }
}

/// The adoption of the claim's move from `v1` to `v2` on `citer`.
fn adoption(citer: &Block, v1: &Block, v2: &Block) -> PinAdoption {
    let mut a = PinAdoption::default();
    a.insert(citer.id.clone(), target(), pin_of(v1), pin_of(v2));
    a
}

fn frozen<T: std::fmt::Debug>(r: Result<T, StoreError>, what: &str) {
    assert!(
        matches!(r, Err(StoreError::Frozen(_))),
        "{what}: expected Frozen, got {r:?}"
    );
}

#[test]
fn rpl20_unapproved_foreign_pin_change_is_frozen() {
    let (_d, mut s, v1, v2, acc, rej) = landed();
    let canon = s.canon_state().unwrap();
    let up = change("x", vec![update(&acc, moved(&acc, pin_of(&v2)))]);
    // no authority: a raw commit, an ordinary landing and a replay of the same change refuse
    frozen(s.commit_to_view("x", up.clone()), "a raw commit");
    frozen(
        s.merge_checked(&canon, std::slice::from_ref(&up)),
        "a landing with no adoption",
    );
    // the adopt card's exact move passes, and only it
    let exact = adoption(&acc, &v1, &v2);
    assert!(
        s.merge_checked_adopting(&canon, std::slice::from_ref(&up), &exact)
            .is_ok(),
        "the exact adopted move lands"
    );
    // a move the adoption does not bind: another pin, another block, a stale from
    let mut other_pin = PinAdoption::default();
    other_pin.insert(
        acc.id.clone(),
        target(),
        pin_of(&v1),
        ForeignPin {
            content: Hash([9; 32]),
            encoding: 0,
        },
    );
    let mut other_block = PinAdoption::default();
    other_block.insert(id("D-ZZZZ2"), target(), pin_of(&v1), pin_of(&v2));
    let mut stale = PinAdoption::default();
    stale.insert(acc.id.clone(), target(), pin_of(&v2), pin_of(&v2));
    for (a, what) in [
        (&other_pin, "another pin"),
        (&other_block, "another block"),
        (&stale, "a stale from"),
        (&PinAdoption::default(), "an empty adoption"),
    ] {
        frozen(
            s.merge_checked_adopting(&canon, std::slice::from_ref(&up), a),
            what,
        );
    }
    // a rejected decision gets no exception, even with its exact move
    let rup = change("x", vec![update(&rej, moved(&rej, pin_of(&v2)))]);
    frozen(
        s.merge_checked_adopting(
            &canon,
            std::slice::from_ref(&rup),
            &adoption(&rej, &v1, &v2),
        ),
        "a rejected decision",
    );
    // the isolated adopt proposal takes the exact move into a reserved view, and writes nothing
    let card = Hash::of(b"an adopt card");
    let view = adopt_view(&card);
    let end = s.log_end();
    let ok = s.prepare_adopt_propose(AdoptProposeRequest {
        view: &view,
        canon_at: s.canon_head().unwrap(),
        change: change(&view, vec![update(&acc, moved(&acc, pin_of(&v2)))]),
        title: "Adopt the delivery from core",
        summary: "Adopt the delivery.",
        by: prov(Role::Approver),
        adoption: exact.clone(),
    });
    assert!(ok.is_ok(), "{ok:?}");
    assert_eq!(s.log_end(), end, "a prepared adoption writes nothing");
    // without the adoption the same isolated proposal is frozen
    frozen(
        s.prepare_adopt_propose(AdoptProposeRequest {
            view: &view,
            canon_at: s.canon_head().unwrap(),
            change: change(&view, vec![update(&acc, moved(&acc, pin_of(&v2)))]),
            title: "Adopt the delivery from core",
            summary: "Adopt the delivery.",
            by: prov(Role::Approver),
            adoption: PinAdoption::default(),
        }),
        "an adopt proposal with no adoption",
    );
    // a move to content no delivery holds is refused, whatever the adoption says
    let unheld = ForeignPin {
        content: Hash([9; 32]),
        encoding: 0,
    };
    let r = s.prepare_adopt_propose(AdoptProposeRequest {
        view: &view,
        canon_at: s.canon_head().unwrap(),
        change: change(&view, vec![update(&acc, moved(&acc, unheld.clone()))]),
        title: "Adopt the delivery from core",
        summary: "Adopt the delivery.",
        by: prov(Role::Approver),
        adoption: other_pin,
    });
    assert!(
        matches!(&r, Err(StoreError::Refused(m)) if m.contains("no delivery holds")),
        "{r:?}"
    );
    // a reserved view name alone is no approval
    frozen(s.commit_to_view(&view, up), "a commit to an adopt- view");
}

#[test]
fn rpl20_tracking_or_reaffirmation_cannot_smuggle_foreign_changes() {
    let (_d, s, v1, v2, acc, _rej) = landed();
    let canon = s.canon_state().unwrap();
    let exact = adoption(&acc, &v1, &v2);
    let moves = BTreeMap::from([(target(), (pin_of(&v1), pin_of(&v2)))]);
    let tracking = s.tracking().clone();
    // a tracking edit and a re-affirmation each pass alone, with no foreign move
    let mut owner = acc.clone();
    owner
        .fields
        .insert("owner".into(), FieldValue::Str("billing".into()));
    assert!(decision::only_tracking_changed(&acc, &owner, &tracking));
    let mut reaffirmed = acc.clone();
    reaffirmed
        .fields
        .insert(REAFFIRMED.into(), FieldValue::Str("D-CAUSE".into()));
    assert!(decision::only_reaffirmation_changed(&acc, &reaffirmed));
    // the pin move alone is the adoption's shape
    assert!(only_foreign_pins_adopted(
        &acc,
        &moved(&acc, pin_of(&v2)),
        &moves
    ));
    // each of these rides a foreign move on another change: no rule takes it
    let with_tracking = moved(&owner, pin_of(&v2));
    let with_reaffirm = moved(&reaffirmed, pin_of(&v2));
    let mut with_ruling = moved(&acc, pin_of(&v2));
    with_ruling.fields.insert(
        "statement".into(),
        FieldValue::Str("We follow nothing.".into()),
    );
    let mut with_local_pin = moved(&acc, pin_of(&v2));
    with_local_pin
        .pins
        .insert(id("c-AAAA9"), Hash::of(b"a local pin"));
    let mut with_link = moved(&acc, pin_of(&v2));
    with_link.foreign.links.insert(
        "depends_on".into(),
        vec![rdy_model::foreign::ForeignLink {
            target: target(),
            part: None,
        }],
    );
    let mut with_new_pin = moved(&acc, pin_of(&v2));
    with_new_pin
        .foreign
        .pins
        .insert(ForeignId::new(cited(), id("c-FRGNE")), pin_of(&v1));
    let mut dropped = acc.clone();
    dropped.foreign.pins.clear();
    for (new, what) in [
        (&with_tracking, "a tracking edit"),
        (&with_reaffirm, "a re-affirmation"),
        (&with_ruling, "a ruling field"),
        (&with_local_pin, "a local pin"),
        (&with_link, "a foreign link"),
        (&with_new_pin, "a new foreign pin"),
        (&dropped, "a dropped foreign pin"),
    ] {
        assert!(
            !decision::only_tracking_changed(&acc, new, &tracking),
            "{what}: not tracking-only"
        );
        assert!(
            !decision::only_reaffirmation_changed(&acc, new),
            "{what}: not a re-affirmation"
        );
        assert!(
            !only_foreign_pins_adopted(&acc, new, &moves),
            "{what}: not the adopted move alone"
        );
        let up = change("x", vec![update(&acc, new.clone())]);
        frozen(
            s.merge_checked_adopting(&canon, std::slice::from_ref(&up), &exact),
            what,
        );
    }
    // a redacted projection is never the adoption's shape
    let mut redacted = moved(&acc, pin_of(&v2));
    redacted.read_identity = Some(rdy_model::redaction::RedactedBlockIdentity {
        version: Hash([1; 32]),
        content_hash: Hash([2; 32]),
        request: Hash([3; 32]),
        frozen: true,
    });
    assert!(!only_foreign_pins_adopted(&acc, &redacted, &moves));
}
