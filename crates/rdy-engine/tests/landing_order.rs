//! ok6: a landing stops order-key ties by re-keying its own blocks (D-TX3KH). Views that append
//! under one parent from one base give their blocks one key; the acceptance change of each later
//! landing gives its own placed blocks (the ones it creates, moves or re-parents) a key of their
//! own, between the tie and the next key. Blocks the landing does not place keep their versions.
//! A landing that cannot do this refuses before anything is written: no room under the key cap,
//! a block that changed in the view after the proposal, or a frozen decision.
mod common;

use rdy_engine::tensions::{TensionKind, TensionStatus, tension_label};
use rdy_engine::{
    LandInfo, ProposalInfo, Rdy, RdyError, Scope, Session, WriteRequest, WriteResult, parse_ops,
};
use rdy_model::{Block, BlockId, Change, FieldValue, Hash, Op, Role};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn agent(view: &str) -> Session {
    Session::agent(format!("agent:{view}"), Some(view.into()), Role::Writer).unwrap()
}

fn person() -> Session {
    Session::human("human:qa".into(), Role::Approver).unwrap()
}

fn view(v: &str) -> Scope {
    Scope::View(v.into())
}

/// Writes `ops` to view `v` as `agent:<v>`.
fn put(r: &mut Rdy, v: &str, ops: Value) -> WriteResult {
    r.write(
        &agent(v),
        WriteRequest {
            view: v.into(),
            ops: parse_ops(ops.as_array().unwrap().clone()).unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap_or_else(|e| panic!("write to {v}: {e}"))
}

/// Proposes view `v` (title `v`, with a summary) and confirms its exceptions as the person.
fn propose(r: &mut Rdy, v: &str) -> ProposalInfo {
    let p = r
        .propose(&agent(v), v, v, Some(common::TEST_SUMMARY))
        .unwrap_or_else(|e| panic!("propose {v}: {e}"));
    common::confirm_exceptions(r, &person(), &p);
    p
}

fn approve(r: &mut Rdy, p: &ProposalInfo) -> Result<LandInfo, RdyError> {
    r.land_report(&person(), &p.id)
}

fn land(r: &mut Rdy, v: &str) -> LandInfo {
    let p = propose(r, v);
    approve(r, &p).unwrap_or_else(|e| panic!("land {v}: {e}"))
}

/// The children of `parent` in `scope`, in order: (id, order key).
fn kids(r: &Rdy, scope: &Scope, parent: &str) -> Vec<(String, String)> {
    r.read()
        .materialized(scope)
        .unwrap()
        .children(Some(&id(parent)))
        .iter()
        .map(|b| (b.id.to_string(), b.order.clone()))
        .collect()
}

fn ids_of(kids: &[(String, String)]) -> Vec<String> {
    kids.iter().map(|(i, _)| i.clone()).collect()
}

/// The pairs of siblings that share an order key.
fn tied_pairs(kids: &[(String, String)]) -> usize {
    let mut by_key: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, k) in kids {
        *by_key.entry(k.as_str()).or_default() += 1;
    }
    by_key.values().map(|n| n * (n - 1) / 2).sum()
}

fn block(r: &Rdy, scope: &Scope, b: &str) -> Block {
    r.read().block(scope, &id(b)).unwrap()
}

/// The acceptance change of landing `l`: the one change the approver wrote.
fn accept_change(r: &Rdy, l: &LandInfo) -> Option<Change> {
    let st = r.read();
    let reader = st.reader();
    let rec = reader.landing(&l.landing.parse::<Hash>().unwrap()).unwrap();
    let mine: Vec<Change> = rec
        .changes
        .iter()
        .map(|h| reader.change(h).unwrap())
        .filter(|c| c.provenance.role == Role::Approver)
        .collect();
    assert!(mine.len() <= 1, "one acceptance change at most: {mine:?}");
    mine.into_iter().next()
}

/// Every canon block, by id, with its version.
fn canon_versions(r: &Rdy) -> BTreeMap<BlockId, Hash> {
    r.read()
        .materialized(&Scope::Canon)
        .unwrap()
        .versions()
        .map(|(i, h)| (i.clone(), *h))
        .collect()
}

/// A canon page `spec` with section `Rules` and its claim "base". Returns the section and claim.
fn seeded() -> (tempfile::TempDir, Rdy, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = put(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$doc", "fields": {"name": "spec", "title": "Spec", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "base"}
        ]),
    );
    land(&mut r, "seed");
    (dir, r, w.created["$s"].clone(), w.created["$c"].clone())
}

/// Appends a claim with `body` under `section` in view `v`; returns its id.
fn append(r: &mut Rdy, v: &str, section: &str, body: &str) -> String {
    put(
        r,
        v,
        json!([{"op": "create", "type": "claim", "id": "$n", "parent": section, "body": body}]),
    )
    .created["$n"]
        .clone()
}

/// A typed store commit of `ops` to view `v`, past every engine rule: the way a block with a
/// chosen order key gets into a view. Every change `v` includes is a dep.
fn raw_commit(r: &mut Rdy, v: &str, ops: Vec<Op>) {
    let deps = r.read().included(v).to_vec();
    r.store_mut()
        .commit_to_view(
            v,
            Change {
                view: v.into(),
                ops,
                deps,
                provenance: agent(v).provenance(),
                justification: None,
                message: "raw".into(),
                links: Default::default(),
                read_identity: None,
            },
        )
        .unwrap();
    r.invalidate();
}

/// A block of `ty` under `parent` with order key `order`, a fresh id and `body`.
fn raw_block(ty: &str, parent: &str, order: &str, body: &str) -> Block {
    let prefix = match ty {
        "decision" => "D",
        _ => "c",
    };
    Block {
        id: BlockId::new(prefix),
        block_type: ty.into(),
        parent: Some(id(parent)),
        order: order.into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: Default::default(),
        pins: Default::default(),
        read_identity: None,
    }
}

/// Lands view `v` straight through the store, with no acceptance change: the way an older store
/// or an imported tree holds a tie in canon.
fn land_raw(r: &mut Rdy, v: &str) {
    let by = person().provenance();
    let store = r.store_mut();
    let p = store.propose(v, v, by.clone()).unwrap();
    store
        .land_atomic(rdy_store::LandRequest {
            proposal: p,
            accept: None,
            approver: by,
            alt: None,
            relay: None,
            evidence: None,
        })
        .unwrap();
    r.invalidate();
}

#[test]
fn ok6_parallel_appends_land_with_distinct_keys() {
    for n in [2usize, 3, 8] {
        let (_d, mut r, sec, base) = seeded();
        let views: Vec<String> = (0..n).map(|i| format!("v{i}")).collect();
        let ids: Vec<String> = views
            .iter()
            .map(|v| append(&mut r, v, &sec, &format!("from {v}")))
            .collect();
        let keys: BTreeSet<String> = views
            .iter()
            .zip(&ids)
            .map(|(v, b)| block(&r, &view(v), b).order)
            .collect();
        assert_eq!(keys.len(), 1, "{n}: every view appended with one key");
        for (i, v) in views.iter().enumerate() {
            let l = land(&mut r, v);
            let acc = accept_change(&r, &l);
            if i == 0 {
                assert!(acc.is_none(), "{n}: the first landing has no tie: {acc:?}");
                continue;
            }
            let acc = acc.unwrap_or_else(|| panic!("{n}: {v} re-keys its claim"));
            assert_eq!(acc.message, format!("give new order keys in {v}"));
            let touched: Vec<String> = acc.ops.iter().map(|o| o.block_id().to_string()).collect();
            assert_eq!(touched, vec![ids[i].clone()], "{n}: only {v}'s own claim");
            let k = kids(&r, &Scope::Canon, &sec);
            assert_eq!(tied_pairs(&k), 0, "{n}: no tie after {v}: {k:?}");
        }
        let k = kids(&r, &Scope::Canon, &sec);
        let distinct: BTreeSet<&String> = k.iter().map(|(_, o)| o).collect();
        assert_eq!(distinct.len(), n + 1, "{n}: distinct keys: {k:?}");
        // each later claim goes directly after the tie it made with the first append: the
        // first landed claim keeps its key, so later landings stack between it and the others
        let mut want = vec![base.clone(), ids[0].clone()];
        want.extend(ids[1..].iter().rev().cloned());
        assert_eq!(ids_of(&k), want, "{n}: {k:?}");
        for v in &views {
            let vk = kids(&r, &view(v), &sec);
            assert_eq!(vk, k, "{n}: view {v} reads the landed order");
        }
    }
}

#[test]
fn ok6_only_placed_blocks_are_rekeyed() {
    let (_d, mut r, sec, base) = seeded();
    let a = append(&mut r, "a", &sec, "from a");
    let b = append(&mut r, "b", &sec, "from b");
    // a body-only edit of the base claim is not placed
    put(
        &mut r,
        "b",
        json!([{"op": "update", "id": base, "body": "base, as b reads it"}]),
    );
    land(&mut r, "a");
    let before = canon_versions(&r);
    let in_view: BTreeMap<String, Block> = [&base, &b]
        .into_iter()
        .map(|x| (x.clone(), block(&r, &view("b"), x)))
        .collect();
    let l = land(&mut r, "b");
    let after = canon_versions(&r);
    // every canon block b does not write keeps its version
    for (i, h) in &before {
        let s = i.to_string();
        if s == base || s == b {
            continue;
        }
        assert_eq!(after.get(i), Some(h), "{i} kept its version");
    }
    assert_eq!(
        after[&id(&base)],
        in_view[&base].version_hash(),
        "the base claim lands as b wrote it, with its key"
    );
    let landed = block(&r, &Scope::Canon, &b);
    assert_ne!(
        landed.order, in_view[&b].order,
        "b's claim has a key of its own"
    );
    let mut only_order = in_view[&b].clone();
    only_order.order = landed.order.clone();
    assert_eq!(landed, only_order, "the re-key changes the order key only");
    let acc = accept_change(&r, &l).expect("b re-keys its claim");
    assert_eq!(acc.ops.len(), 1, "{:?}", acc.ops);
    match &acc.ops[0] {
        Op::Update { base: h, block } => {
            assert_eq!(*h, in_view[&b].version_hash(), "from the proposed version");
            assert_eq!(block.id.to_string(), b);
        }
        op => panic!("an update of b's claim: {op:?}"),
    }
    assert_eq!(
        ids_of(&kids(&r, &Scope::Canon, &sec)),
        vec![base.clone(), a.clone(), b.clone()]
    );

    // a tied run of the landing's own blocks keeps its lowest id and re-keys the others
    let last = kids(&r, &Scope::Canon, &sec).last().unwrap().1.clone();
    let key = rdy_model::order_between(Some(&last), None).unwrap();
    let mut own: Vec<Block> = (0..3)
        .map(|i| raw_block("claim", &sec, &key, &format!("own {i}")))
        .collect();
    own.sort_by(|x, y| x.id.cmp(&y.id));
    raw_commit(
        &mut r,
        "c",
        own.iter()
            .cloned()
            .map(|block| Op::Create { block })
            .collect(),
    );
    let before = canon_versions(&r);
    let l = land(&mut r, "c");
    let acc = accept_change(&r, &l).expect("c re-keys two claims");
    let touched: Vec<BlockId> = acc.ops.iter().map(|o| o.block_id().clone()).collect();
    assert_eq!(touched, vec![own[1].id.clone(), own[2].id.clone()]);
    assert_eq!(block(&r, &Scope::Canon, &own[0].id.to_string()).order, key);
    let k = kids(&r, &Scope::Canon, &sec);
    assert_eq!(tied_pairs(&k), 0, "{k:?}");
    let tail: Vec<String> = own.iter().map(|b| b.id.to_string()).collect();
    assert_eq!(ids_of(&k)[3..], tail[..], "in id order after the run's key");
    let after = canon_versions(&r);
    for (i, h) in &before {
        assert_eq!(after.get(i), Some(h), "{i} kept its version");
    }
}

#[test]
fn ok6_order_rekey_keeps_citations_and_dismissals() {
    let (_d, mut r, sec, _base) = seeded();
    let rule = "Order keys never tie among siblings.";
    append(&mut r, "p", &sec, rule);
    land(&mut r, "p");
    append(&mut r, "a", &sec, "from a");
    // b restates p's rule (a duplicate definition) and cites and pins its restatement
    let w = put(
        &mut r,
        "b",
        json!([
            {"op": "create", "type": "claim", "id": "$b", "parent": sec, "body": rule},
            {"op": "create", "type": "claim", "id": "$c", "parent": sec, "after": "$b", "body": "C relies on {{$b}}."}
        ]),
    );
    let (b, c) = (w.created["$b"].clone(), w.created["$c"].clone());
    let cited = block(&r, &view("b"), &c);
    assert!(cited.pins.contains_key(&id(&b)), "c pins b: {cited:?}");
    let duplicates = |r: &Rdy| -> Vec<(Hash, bool)> {
        let st = r.read();
        let set = st.tensions().unwrap();
        set.involving_block(&id(&b))
            .into_iter()
            .filter(|t| t.kind == TensionKind::DuplicateDefinition)
            .map(|t| {
                let dismissed = matches!(st.tension_status(&t), TensionStatus::Dismissed(_));
                (t.vkey, dismissed)
            })
            .collect()
    };
    let open = duplicates(&r);
    assert_eq!(open.len(), 1, "{open:?}");
    let t = r
        .read()
        .tensions()
        .unwrap()
        .involving_block(&id(&b))
        .into_iter()
        .find(|t| t.kind == TensionKind::DuplicateDefinition)
        .unwrap();
    r.dismiss_tension(&person(), &tension_label(&t.id), "b restates p on purpose")
        .unwrap();
    assert_eq!(duplicates(&r), vec![(open[0].0, true)]);
    land(&mut r, "a");
    let proposed = block(&r, &view("b"), &b);
    let l = land(&mut r, "b");
    let acc = accept_change(&r, &l).expect("b re-keys its restatement");
    let touched: Vec<String> = acc.ops.iter().map(|o| o.block_id().to_string()).collect();
    assert_eq!(touched, vec![b.clone()]);
    let landed = block(&r, &Scope::Canon, &b);
    assert_ne!(landed.order, proposed.order);
    let st = r.read();
    let t = st.reader().tracking();
    assert_eq!(
        landed.content_hash_with(t),
        proposed.content_hash_with(t),
        "the re-key keeps b's content"
    );
    drop(st);
    for scope in [Scope::Canon, view("b")] {
        let s = r.read().suspects(&scope).unwrap();
        assert!(
            s.is_empty(),
            "c's pin on b stays current in {scope:?}: {s:?}"
        );
    }
    assert_eq!(
        duplicates(&r),
        vec![(open[0].0, true)],
        "the dismissal binds b's content, which the re-key keeps"
    );
}

#[test]
fn ok6_acceptance_and_key_rekey_share_one_update() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = put(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "handbook-for-every-team-in-the-company", "title": "Handbook"}},
            {"op": "create", "type": "doc", "id": "$log", "fields": {"name": "log", "title": "Log", "doc_type": "decisions-log"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$log", "fields": {"title": "Decisions"}}
        ]),
    );
    land(&mut r, "seed");
    let (f, s) = (w.created["$f"].clone(), w.created["$s"].clone());
    // a and b each append under the folder and the section from one base: both docs derive the
    // key `guide` (their long paths keep only the leaf), and b's doc and decision take the order
    // keys a's folder and decision take
    let wa = put(
        &mut r,
        "a",
        json!([
            {"op": "create", "type": "folder", "id": "$x", "parent": f, "fields": {"name": "team-a-notes-for-every-new-member", "title": "A"}},
            {"op": "create", "type": "doc", "id": "$d", "parent": "$x", "fields": {"name": "guide", "title": "G", "doc_type": "overview"}},
            {"op": "create", "type": "decision", "id": "$k", "parent": s, "fields": {"statement": "Use base-62 order keys.", "status": "proposed"}}
        ]),
    );
    let wb = put(
        &mut r,
        "b",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "parent": f, "fields": {"name": "guide", "title": "G", "doc_type": "overview"}},
            {"op": "create", "type": "decision", "id": "$k", "parent": s, "fields": {"statement": "Re-key only the landing's own blocks.", "status": "proposed"}}
        ]),
    );
    let (xa, ka) = (wa.created["$x"].clone(), wa.created["$k"].clone());
    let (db, kb) = (wb.created["$d"].clone(), wb.created["$k"].clone());
    assert_eq!(wa.keys[&wa.created["$d"]], "guide");
    assert_eq!(wb.keys[&db], "guide");
    assert_eq!(
        block(&r, &view("a"), &xa).order,
        block(&r, &view("b"), &db).order
    );
    assert_eq!(
        block(&r, &view("a"), &ka).order,
        block(&r, &view("b"), &kb).order
    );
    // b is proposed before a lands, so its doc's key clashes only at the landing (K1)
    let pb = propose(&mut r, "b");
    land(&mut r, "a");
    let proposed: BTreeMap<String, Block> = [&db, &kb]
        .into_iter()
        .map(|x| (x.clone(), block(&r, &view("b"), x)))
        .collect();
    let l = approve(&mut r, &pb).unwrap();
    assert_eq!(l.keys.len(), 1, "{:?}", l.keys);
    let new_key = l.keys[&db].clone();
    assert_ne!(new_key, "guide");
    let acc = accept_change(&r, &l).expect("an acceptance change");
    assert_eq!(
        acc.message,
        "accept decisions, give new keys and give new order keys in b"
    );
    let by_block: BTreeMap<String, (Hash, Block)> = acc
        .ops
        .iter()
        .map(|o| match o {
            Op::Update { base, block } => (block.id.to_string(), (*base, block.clone())),
            op => panic!("an update: {op:?}"),
        })
        .collect();
    assert_eq!(acc.ops.len(), 2, "one update per block: {:?}", acc.ops);
    assert_eq!(by_block.len(), 2);
    for (b, (base, next)) in &by_block {
        assert_eq!(
            *base,
            proposed[b].version_hash(),
            "{b} from its proposed version"
        );
        assert_ne!(
            next.order, proposed[b].order,
            "{b} has an order key of its own"
        );
    }
    let (_, doc) = &by_block[&db];
    assert_eq!(doc.fields.get("key"), Some(&FieldValue::Str(new_key)));
    let (_, dec) = &by_block[&kb];
    assert_eq!(
        dec.fields.get("status"),
        Some(&FieldValue::Str("accepted".into()))
    );
    // the canon decision a landed is frozen and keeps its key
    assert_eq!(
        block(&r, &Scope::Canon, &ka).order,
        proposed[&kb].order,
        "a's accepted decision keeps the shared key"
    );
    for parent in [&f, &s] {
        let k = kids(&r, &Scope::Canon, parent);
        assert_eq!(tied_pairs(&k), 0, "{k:?}");
    }
    assert_eq!(block(&r, &Scope::Canon, &db), by_block[&db].1);
    assert_eq!(block(&r, &Scope::Canon, &kb), by_block[&kb].1);
}

#[test]
fn ok6_stale_or_full_interval_refuses_atomically() {
    let unchanged = |r: &Rdy| {
        (
            r.store().log_end(),
            r.read().canon_head(),
            canon_versions(r),
        )
    };

    // stale: b's claim needs a new key, but b edited it after proposing
    let (_d, mut r, sec, _base) = seeded();
    append(&mut r, "a", &sec, "from a");
    let b = append(&mut r, "b", &sec, "from b");
    let pb = propose(&mut r, "b");
    put(
        &mut r,
        "b",
        json!([{"op": "update", "id": b, "body": "from b, edited"}]),
    );
    land(&mut r, "a");
    let before = unchanged(&r);
    let e = approve(&mut r, &pb).unwrap_err();
    assert_eq!(e.code, "E_STALE_BASE", "{e}");
    assert_eq!(
        e.message,
        format!("{b} needs a new order key, but it changed in view \"b\" after the proposal")
    );
    assert_eq!(
        e.fix,
        format!("propose again to include the later edits to {b}")
    );
    assert_eq!(unchanged(&r), before, "nothing is written");

    // full: the only key after the tie's key is past the cap
    let (_d, mut r, sec, base) = seeded();
    let k = block(&r, &Scope::Canon, &base).order;
    let full = format!(
        "{k}{}1",
        "0".repeat(rdy_model::MAX_ORDER_KEY_LEN - k.len() - 1)
    );
    assert_eq!(full.len(), rdy_model::MAX_ORDER_KEY_LEN);
    assert!(rdy_model::order_run(Some(&k), Some(&full), 1).is_err());
    let y = raw_block("claim", &sec, &full, "at the cap");
    raw_commit(&mut r, "y", vec![Op::Create { block: y }]);
    land(&mut r, "y");
    let z = raw_block("claim", &sec, &k, "tied with base");
    let zid = z.id.to_string();
    raw_commit(&mut r, "z", vec![Op::Create { block: z }]);
    let pz = propose(&mut r, "z");
    let before = unchanged(&r);
    let e = approve(&mut r, &pz).unwrap_err();
    assert_eq!(e.code, "E_ORDER_DEPTH", "{e}");
    assert_eq!(
        e.message,
        format!("cannot give {zid} a distinct order key without changing another block")
    );
    assert_eq!(
        e.fix,
        format!("move {zid} to a place with room, then propose again")
    );
    assert_eq!(unchanged(&r), before, "nothing is written");

    // frozen: a placed accepted decision would need a new key
    let (_d, mut r, sec, base) = seeded();
    let k = block(&r, &Scope::Canon, &base).order;
    let mut fz = raw_block("decision", &sec, &k, "");
    for (f, v) in [
        ("statement", "Frozen on arrival."),
        ("status", "accepted"),
        ("decided_at", "2026-10-26T00:00:00Z"),
    ] {
        fz.fields.insert(f.into(), FieldValue::Str(v.into()));
    }
    let fid = fz.id.to_string();
    raw_commit(&mut r, "f", vec![Op::Create { block: fz }]);
    let pf = propose(&mut r, "f");
    let before = unchanged(&r);
    let e = approve(&mut r, &pf).unwrap_err();
    assert_eq!(e.code, "E_FROZEN", "{e}");
    assert_eq!(
        e.message,
        format!("{fid} is frozen and needs a new order key")
    );
    assert_eq!(
        e.fix,
        "choose another place for the block, then propose again"
    );
    assert_eq!(unchanged(&r), before, "nothing is written");
}

#[test]
fn ok6_historical_ties_do_not_trigger_unrelated_edits() {
    let (_d, mut r, sec, base) = seeded();
    let t0 = append(&mut r, "t0", &sec, "tied 0");
    let t1 = append(&mut r, "t1", &sec, "tied 1");
    land_raw(&mut r, "t0");
    land_raw(&mut r, "t1");
    let m = append(&mut r, "m", &sec, "after the tie");
    let l = land(&mut r, "m");
    assert!(accept_change(&r, &l).is_none(), "an append after a tie");
    let k = kids(&r, &Scope::Canon, &sec);
    assert_eq!(tied_pairs(&k), 1, "the historical tie: {k:?}");
    let key = block(&r, &Scope::Canon, &t0).order;

    // a body edit of a tied block is not placed: no acceptance change, no other block written
    put(
        &mut r,
        "e",
        json!([{"op": "update", "id": t0, "body": "tied 0, edited"}]),
    );
    let before = canon_versions(&r);
    let l = land(&mut r, "e");
    assert!(
        accept_change(&r, &l).is_none(),
        "a body edit re-keys nothing"
    );
    let after = canon_versions(&r);
    for (i, h) in &before {
        if i.to_string() != t0 {
            assert_eq!(after.get(i), Some(h), "{i} kept its version");
        }
    }
    assert_eq!(block(&r, &Scope::Canon, &t0).order, key);
    assert_eq!(tied_pairs(&kids(&r, &Scope::Canon, &sec)), 1);

    // a block placed into the historical run goes after it, before the next key; the old
    // tie stays as it is
    let n = raw_block("claim", &sec, &key, "into the tie");
    let nid = n.id.to_string();
    raw_commit(&mut r, "n", vec![Op::Create { block: n }]);
    let before = canon_versions(&r);
    let l = land(&mut r, "n");
    let acc = accept_change(&r, &l).expect("n re-keys its claim");
    assert_eq!(acc.message, "give new order keys in n");
    let touched: Vec<String> = acc.ops.iter().map(|o| o.block_id().to_string()).collect();
    assert_eq!(touched, vec![nid.clone()]);
    let after = canon_versions(&r);
    for (i, h) in &before {
        assert_eq!(after.get(i), Some(h), "{i} kept its version");
    }
    let k = kids(&r, &Scope::Canon, &sec);
    assert_eq!(tied_pairs(&k), 1, "no new tie: {k:?}");
    let (lo, hi) = if t0 < t1 { (&t0, &t1) } else { (&t1, &t0) };
    assert_eq!(
        ids_of(&k),
        vec![base.clone(), lo.clone(), hi.clone(), nid, m.clone()]
    );

    // a landing elsewhere writes nothing in the run
    let doc = block(&r, &Scope::Canon, &sec).parent.unwrap().to_string();
    let w = put(
        &mut r,
        "o",
        json!([{"op": "create", "type": "section", "id": "$s", "parent": doc, "fields": {"title": "Other"}}]),
    );
    let other = w.created["$s"].clone();
    append(&mut r, "o", &other, "elsewhere");
    let before = canon_versions(&r);
    let l = land(&mut r, "o");
    assert!(accept_change(&r, &l).is_none());
    let after = canon_versions(&r);
    for (i, h) in &before {
        assert_eq!(after.get(i), Some(h), "{i} kept its version");
    }
    assert_eq!(tied_pairs(&kids(&r, &Scope::Canon, &sec)), 1);
}

#[test]
fn ok6_reindex_reproduces_final_order() {
    let (d, mut r, sec, _base) = seeded();
    for v in ["a", "b", "c"] {
        append(&mut r, v, &sec, &format!("from {v}"));
    }
    for v in ["a", "b", "c"] {
        land(&mut r, v);
    }
    let state = |r: &Rdy| {
        (
            r.read().canon_head(),
            kids(r, &Scope::Canon, &sec),
            canon_versions(r),
        )
    };
    let landed = state(&r);
    assert_eq!(tied_pairs(&landed.1), 0, "{:?}", landed.1);
    r.reindex().unwrap();
    assert_eq!(state(&r), landed, "a reindex reads the same canon");
    r.fsck().unwrap();
    drop(r);
    std::fs::remove_file(d.path().join(".rdy/store/index.redb")).unwrap();
    let r = Rdy::open(d.path()).unwrap();
    assert_eq!(state(&r), landed, "a rebuilt index reads the same canon");
    r.fsck().unwrap();
}

/// Inserts a claim with `body` directly after `after` under `section` in view `v`; returns its id.
fn insert_after(r: &mut Rdy, v: &str, section: &str, after: &str, body: &str) -> String {
    put(
        r,
        v,
        json!([{"op": "create", "type": "claim", "id": "$n", "parent": section, "after": after, "body": body}]),
    )
    .created["$n"]
        .clone()
}

/// The run of each landing pin below, over fresh ids: a pin over one id draw could pass on a
/// hash order by chance.
const ID_RUNS: usize = 6;

#[test]
fn ok6_a_landing_rekey_keeps_the_views_later_block_after_it() {
    for run in 0..ID_RUNS {
        let (_d, mut r, sec, base) = seeded();
        let pa = append(&mut r, "a", &sec, "from a");
        let p = append(&mut r, "b", &sec, "from b");
        let pb = propose(&mut r, "b");
        // b writes on after it proposed: Q goes after P, outside the version
        let q = append(&mut r, "b", &sec, "after P");
        land(&mut r, "a");
        approve(&mut r, &pb).unwrap_or_else(|e| panic!("{run}: land b: {e}"));
        let vk = kids(&r, &view("b"), &sec);
        assert_eq!(tied_pairs(&vk), 0, "{run}: no tie in b's view: {vk:?}");
        assert_eq!(
            ids_of(&vk),
            vec![base.clone(), pa.clone(), p.clone(), q.clone()],
            "{run}: b's view keeps Q after P"
        );
        land(&mut r, "b");
        let k = kids(&r, &Scope::Canon, &sec);
        assert_eq!(tied_pairs(&k), 0, "{run}: no tie in canon: {k:?}");
        assert_eq!(
            ids_of(&k),
            vec![base.clone(), pa.clone(), p.clone(), q.clone()],
            "{run}: P before Q in canon"
        );
    }
}

#[test]
fn ok6_a_landing_rekey_respects_a_later_insert_past_a_deleted_sibling() {
    for run in 0..ID_RUNS {
        let (_d, mut r, sec, base) = seeded();
        let e = append(&mut r, "e", &sec, "next sibling");
        land(&mut r, "e");
        let p = insert_after(&mut r, "b", &sec, &base, "from b");
        let pb = propose(&mut r, "b");
        // b writes on after it proposed: Q goes after P, outside the version
        let q = insert_after(&mut r, "b", &sec, &p, "after P");
        let pa = insert_after(&mut r, "a", &sec, &base, "from a");
        put(&mut r, "a", json!([{"op": "delete", "id": e}]));
        land(&mut r, "a");
        approve(&mut r, &pb).unwrap_or_else(|e| panic!("{run}: land b: {e}"));
        let vk = kids(&r, &view("b"), &sec);
        assert_eq!(tied_pairs(&vk), 0, "{run}: no tie in b's view: {vk:?}");
        assert_eq!(
            ids_of(&vk),
            vec![base.clone(), pa.clone(), p.clone(), q.clone()],
            "{run}: b's view keeps Q after P"
        );
        land(&mut r, "b");
        let k = kids(&r, &Scope::Canon, &sec);
        assert_eq!(tied_pairs(&k), 0, "{run}: no tie in canon: {k:?}");
        assert_eq!(
            ids_of(&k),
            vec![base.clone(), pa.clone(), p.clone(), q.clone()],
            "{run}: P before Q in canon"
        );
    }
}

/// Final ruling 2 (round 2): a canon side that differs from an includer's op base only by its
/// order key is no conflict. v1 appends q; w includes v1's change and edits q; v0 lands, then v1
/// lands with a re-key. w has no conflict and keeps its own version of q (its text, with v1's old
/// tied key) until it lands; its landing then gives q canon's key (round 3), so canon never
/// holds the tie.
#[test]
fn ok6_an_includer_of_a_rekeyed_block_has_no_conflict() {
    for run in 0..ID_RUNS {
        let (_d, mut r, sec, base) = seeded();
        let p = append(&mut r, "v0", &sec, "from v0");
        let w1 = put(
            &mut r,
            "v1",
            json!([{"op": "create", "type": "claim", "id": "$n", "parent": sec, "body": "from v1"}]),
        );
        let q = w1.created["$n"].clone();
        let tied = block(&r, &view("v1"), &q).order;
        assert_eq!(block(&r, &view("v0"), &p).order, tied, "{run}: one key");
        r.view_include(&agent("w"), "w", std::slice::from_ref(&w1.change))
            .unwrap_or_else(|e| panic!("{run}: include: {e}"));
        put(
            &mut r,
            "w",
            json!([{"op": "update", "id": q, "body": "q, as w reads it"}]),
        );
        land(&mut r, "v0");
        let l1 = land(&mut r, "v1");
        let acc = accept_change(&r, &l1).expect("v1 re-keys q");
        assert_eq!(acc.message, "give new order keys in v1", "{run}");
        let canon_q = block(&r, &Scope::Canon, &q);
        assert_ne!(canon_q.order, tied, "{run}: canon's q has a key of its own");
        let conflicts = r.read().conflicts(&view("w")).unwrap();
        assert!(
            conflicts.is_empty(),
            "{run}: no order-only conflict in w: {conflicts:?}"
        );
        let wq = block(&r, &view("w"), &q);
        assert_eq!(wq.body, "q, as w reads it", "{run}: w's text");
        assert_eq!(
            wq.order, tied,
            "{run}: w keeps its own version, with the old key"
        );
        let pw = propose(&mut r, "w");
        approve(&mut r, &pw).unwrap_or_else(|e| panic!("{run}: land w: {e} {:?}", e.issues));
        let k = kids(&r, &Scope::Canon, &sec);
        assert_eq!(tied_pairs(&k), 0, "{run}: canon holds no tie: {k:?}");
        assert_eq!(
            ids_of(&k),
            vec![base.clone(), p.clone(), q.clone()],
            "{run}: q after p, where its landings put it"
        );
        assert_eq!(
            block(&r, &Scope::Canon, &q).order,
            canon_q.order,
            "{run}: q keeps canon's key"
        );
        assert_eq!(
            block(&r, &Scope::Canon, &q).body,
            "q, as w reads it",
            "{run}"
        );
    }
}

/// Final ruling round 3: a landing must not undo a landed move. Canon holds base, b; view m
/// moves b before base; view w edits b's body from canon. m lands, then w lands: canon holds
/// b, base, with w's body on b. w's op kept its base's order key while canon's version differs
/// from that base only by its key, so w's landing stores b with canon's key, in its acceptance
/// change.
#[test]
fn ok6_a_landing_keeps_a_landed_move() {
    for run in 0..ID_RUNS {
        let (_d, mut r, sec, base) = seeded();
        let b = append(&mut r, "s", &sec, "second");
        land(&mut r, "s");
        put(
            &mut r,
            "m",
            json!([{"op": "move", "id": b, "parent": sec, "before": base}]),
        );
        put(
            &mut r,
            "w",
            json!([{"op": "update", "id": b, "body": "second, as w reads it"}]),
        );
        land(&mut r, "m");
        let moved = block(&r, &Scope::Canon, &b).order;
        assert_eq!(
            ids_of(&kids(&r, &Scope::Canon, &sec)),
            vec![b.clone(), base.clone()],
            "{run}: m's move landed"
        );
        let conflicts = r.read().conflicts(&view("w")).unwrap();
        assert!(
            conflicts.is_empty(),
            "{run}: no conflict in w: {conflicts:?}"
        );
        let pw = propose(&mut r, "w");
        let lw =
            approve(&mut r, &pw).unwrap_or_else(|e| panic!("{run}: land w: {e} {:?}", e.issues));
        let k = kids(&r, &Scope::Canon, &sec);
        assert_eq!(
            ids_of(&k),
            vec![b.clone(), base.clone()],
            "{run}: w's landing keeps m's move: {k:?}"
        );
        assert_eq!(tied_pairs(&k), 0, "{run}: no tie: {k:?}");
        let cb = block(&r, &Scope::Canon, &b);
        assert_eq!(cb.body, "second, as w reads it", "{run}: w's text");
        assert_eq!(cb.order, moved, "{run}: canon's key");
        // the landing's acceptance change stored that version
        let acc = accept_change(&r, &lw).expect("w's landing stores b at canon's key");
        let stored: Vec<&Block> = acc
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Update { block, .. } if block.id.to_string() == b => Some(block),
                _ => None,
            })
            .collect();
        assert_eq!(stored.len(), 1, "{run}: one update of b: {acc:?}");
        assert_eq!(stored[0].order, moved, "{run}");
        assert_eq!(stored[0].body, "second, as w reads it", "{run}");
        let report = r.fsck().unwrap_or_else(|e| panic!("{run}: fsck: {e}"));
        assert!(report.warnings.is_empty(), "{run}: {report:?}");
    }
}

/// Final ruling round 4: the order-only rule is for edits only. Canon holds base, b, c; view m
/// moves b before base; view w moves b after c. m lands. w's move is a choice that differs from
/// m's landed move, so w has a conflict and its landing is refused.
#[test]
fn ok6_a_move_against_a_landed_move_conflicts() {
    for run in 0..ID_RUNS {
        let (_d, mut r, sec, base) = seeded();
        let b = append(&mut r, "s", &sec, "b");
        let c = append(&mut r, "s", &sec, "c");
        land(&mut r, "s");
        put(
            &mut r,
            "m",
            json!([{"op": "move", "id": b, "parent": sec, "before": base}]),
        );
        put(
            &mut r,
            "w",
            json!([{"op": "move", "id": b, "parent": sec, "after": c}]),
        );
        land(&mut r, "m");
        let landed = vec![b.clone(), base.clone(), c.clone()];
        assert_eq!(ids_of(&kids(&r, &Scope::Canon, &sec)), landed, "{run}");
        let conflicts = r.read().conflicts(&view("w")).unwrap();
        assert_eq!(conflicts.len(), 1, "{run}: {conflicts:?}");
        let pw = propose(&mut r, "w");
        let e = approve(&mut r, &pw).expect_err("w's move must not undo m's landed move");
        assert_eq!(e.code, "E_PROPOSAL_NOT_READY", "{run}: {e}");
        let codes: Vec<_> = e.issues.iter().map(|i| i.code).collect();
        assert_eq!(codes, ["E_CONFLICT_UNRESOLVED"], "{run}: {e:?}");
        assert_eq!(ids_of(&kids(&r, &Scope::Canon, &sec)), landed, "{run}");
    }
}

/// Final ruling round 4: canon holds base, b; view m moves b before base; view w deletes b. m
/// lands. w's delete is against m's landed move, so w has a conflict and its landing is refused.
#[test]
fn ok6_a_delete_against_a_landed_move_conflicts() {
    for run in 0..ID_RUNS {
        let (_d, mut r, sec, base) = seeded();
        let b = append(&mut r, "s", &sec, "b");
        land(&mut r, "s");
        put(
            &mut r,
            "m",
            json!([{"op": "move", "id": b, "parent": sec, "before": base}]),
        );
        put(&mut r, "w", json!([{"op": "delete", "id": b}]));
        land(&mut r, "m");
        let landed = vec![b.clone(), base.clone()];
        assert_eq!(ids_of(&kids(&r, &Scope::Canon, &sec)), landed, "{run}");
        let conflicts = r.read().conflicts(&view("w")).unwrap();
        assert_eq!(conflicts.len(), 1, "{run}: {conflicts:?}");
        let pw = propose(&mut r, "w");
        let e = approve(&mut r, &pw).expect_err("w's delete must not undo m's landed move");
        assert_eq!(e.code, "E_PROPOSAL_NOT_READY", "{run}: {e}");
        let codes: Vec<_> = e.issues.iter().map(|i| i.code).collect();
        assert_eq!(codes, ["E_CONFLICT_UNRESOLVED"], "{run}: {e:?}");
        assert_eq!(ids_of(&kids(&r, &Scope::Canon, &sec)), landed, "{run}");
    }
}
