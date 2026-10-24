//! plz4 (TASK-524; system/graph p-29ZQF, p-ECCYB): [`Graph::with_changes`] carries a graph across
//! a write with edge work linear in the changed edges and the edges of the lists they touch,
//! and its result equals a rebuild. The work is the test-only counter in this crate: edge
//! comparisons, set lookups, edges copied, slots visited and slots moved. No timing is read.
use super::*;
use rdy_model::engine_fields::{Against, Reaffirmation};
use rdy_model::{BlockResidue, CROCKFORD, FieldValue, Link};

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// The id `prefix-XXXXX` whose five Crockford digits spell `n`, so id order is number order.
fn nid(prefix: &str, n: usize) -> BlockId {
    let mut body = [0u8; 5];
    let mut v = n;
    for d in body.iter_mut().rev() {
        *d = CROCKFORD[v % 32];
        v /= 32;
    }
    assert_eq!(v, 0, "{n} needs more than five digits");
    id(&format!("{prefix}-{}", std::str::from_utf8(&body).unwrap()))
}

fn blk(i: BlockId, ty: &str, parent: Option<&BlockId>) -> Block {
    Block {
        id: i,
        block_type: ty.into(),
        parent: parent.cloned(),
        order: "V".into(),
        fields: BTreeMap::new(),
        body: String::new(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn link(block: &mut Block, name: &str, target: &BlockId, part: Option<&str>) {
    block.links.entry(name.into()).or_default().push(Link {
        target: target.clone(),
        part: part.map(Into::into),
    });
}

/// The graph of `blocks` rebuilt in id order with `tracking`.
fn rebuild(blocks: &BTreeMap<BlockId, Block>, tracking: &Arc<Tracking>) -> Graph {
    Graph::build_with(blocks.values(), tracking.clone())
}

fn by_id(blocks: impl IntoIterator<Item = Block>) -> BTreeMap<BlockId, Block> {
    blocks.into_iter().map(|b| (b.id.clone(), b)).collect()
}

/// How many edge occurrences `blocks` generate, `Contains` included.
fn generated(blocks: &[&Block]) -> usize {
    blocks.iter().map(|b| Graph::edges_of(b).len()).sum()
}

/// U: the unchanged edge occurrences in a list the write touches, in either direction, each
/// once. The touched lists are the outgoing lists of every changed edge's `from` and the
/// incoming lists of every changed edge's `to`.
fn touched_unchanged(state: &BTreeMap<BlockId, Block>, old: &[&Block], new: &[&Block]) -> usize {
    let changed: BTreeSet<&BlockId> = old.iter().chain(new).map(|b| &b.id).collect();
    let edges: Vec<Edge> = old
        .iter()
        .chain(new)
        .flat_map(|b| Graph::edges_of(b))
        .collect();
    let outs: BTreeSet<&BlockId> = edges.iter().map(|e| &e.from).collect();
    let incs: BTreeSet<&BlockId> = edges.iter().map(|e| &e.to).collect();
    state
        .values()
        .filter(|b| !changed.contains(&b.id))
        .flat_map(Graph::edges_of)
        .filter(|e| outs.contains(&e.from) || incs.contains(&e.to))
        .count()
}

/// The carried graph of one write, with the work it counted.
fn carry(g: &Graph, old: &[&Block], new: &[&Block]) -> (Graph, u64) {
    reset_work();
    let next = g.with_changes(old.iter().copied(), new.iter().copied());
    (next, work())
}

fn assert_linear(what: &str, work: u64, e_old: usize, e_new: usize, u: usize) {
    let bound = 24 * (e_old + e_new + u + 1) + 128;
    assert!(
        work <= bound as u64,
        "{what}: counted work {work} > bound {bound} (E_old {e_old}, E_new {e_new}, U {u})"
    );
}

/// One rewritten generator with `n` links to one target, between an earlier generator with one
/// link and a later, unchanged generator with `n` links, all children of one parent.
fn repeated_target(n: usize) {
    let tracking = Arc::<Tracking>::default();
    let parent = id("s-PPPPP");
    let target = id("t-TTTTT");
    let mut early = blk(id("c-1AAAA"), "claim", Some(&parent));
    link(&mut early, "depends_on", &target, None);
    let mut old = blk(id("c-2BBBB"), "claim", Some(&parent));
    let mut new = old.clone();
    let mut late = blk(id("c-3CCCC"), "claim", Some(&parent));
    for i in 0..n {
        link(&mut old, "depends_on", &target, Some(&format!("p{i}")));
        link(&mut new, "depends_on", &target, Some(&format!("q{i}")));
        link(&mut late, "depends_on", &target, None);
    }
    new.pins.insert(target.clone(), Hash::of(b"the new pin"));
    let base = [
        blk(parent.clone(), "section", None),
        blk(target.clone(), "term", None),
        early,
        late,
    ];
    let before = by_id(base.iter().cloned().chain([old.clone()]));
    let after = by_id(base.iter().cloned().chain([new.clone()]));

    let g = rebuild(&before, &tracking);
    let (next, work) = carry(&g, &[&old], &[&new]);

    let (e_old, e_new) = (generated(&[&old]), generated(&[&new]));
    let u = touched_unchanged(&before, &[&old], &[&new]);
    assert_eq!(
        (e_old, e_new),
        (n + 1, n + 1),
        "the links and the Contains edge"
    );
    assert_eq!(
        u,
        n + 3,
        "the later and earlier links, two sibling Contains edges"
    );
    assert_linear(&format!("n = {n}"), work, e_old, e_new, u);
    assert!(
        next == rebuild(&after, &tracking),
        "n = {n}: carried != rebuild"
    );
    assert!(
        g == rebuild(&before, &tracking),
        "n = {n}: the original changed"
    );
}

#[test]
fn plz4_relink_work_is_linear_for_repeated_targets() {
    for n in [128, 20000] {
        repeated_target(n);
    }
}

/// `k` changed blocks with one link each to one target, and a later, unchanged generator with
/// `k` links to it. All `k` are rewritten in one call; the new versions come in reverse id order.
fn many_blocks(k: usize) {
    let tracking = Arc::<Tracking>::default();
    let target = id("t-TTTTT");
    let mut late = blk(id("c-ZZZZZ"), "claim", None);
    let mut olds = vec![];
    let mut news = vec![];
    for i in 0..k {
        link(&mut late, "depends_on", &target, None);
        let mut o = blk(nid("c", i + 1), "claim", None);
        let mut n = o.clone();
        link(&mut o, "depends_on", &target, None);
        link(&mut n, "depends_on", &target, Some("rewritten"));
        olds.push(o);
        news.push(n);
    }
    news.reverse();
    let base = [blk(target.clone(), "term", None), late];
    let before = by_id(base.iter().cloned().chain(olds.iter().cloned()));
    let after = by_id(base.iter().cloned().chain(news.iter().cloned()));
    let old: Vec<&Block> = olds.iter().collect();
    let new: Vec<&Block> = news.iter().collect();

    let g = rebuild(&before, &tracking);
    let (next, work) = carry(&g, &old, &new);

    let (e_old, e_new) = (generated(&old), generated(&new));
    let u = touched_unchanged(&before, &old, &new);
    assert_eq!((e_old, e_new, u), (k, k, k));
    if k == 128 {
        assert_eq!(24 * (e_old + e_new + u + 1) + 128, 9368);
    }
    assert_linear(&format!("k = {k}"), work, e_old, e_new, u);
    assert!(
        next == rebuild(&after, &tracking),
        "k = {k}: carried != rebuild"
    );
    assert!(
        g == rebuild(&before, &tracking),
        "k = {k}: the original changed"
    );
}

#[test]
fn plz4_relink_work_is_linear_for_many_blocks_to_one_target() {
    for k in [128, 4000] {
        many_blocks(k);
    }
}

/// The generator of each edge in `list`, in order.
fn generators(list: &[Edge]) -> Vec<&str> {
    list.iter().map(|e| generator(e).as_str()).collect()
}

#[test]
fn plz4_relink_preserves_generators_duplicates_pins_and_order() {
    let tracking = Arc::<Tracking>::default();
    let (p1, p2) = (id("s-PAAAA"), id("s-PBBBB"));
    let (t1, t2, t3) = (id("D-T1111"), id("D-T2222"), id("D-T3333"));
    let mut a = blk(id("c-1AAAA"), "claim", Some(&p1));
    link(&mut a, "depends_on", &t1, None);
    link(&mut a, "implements", &t1, Some("a"));
    let mut z = blk(id("c-3ZZZZ"), "claim", Some(&p1));
    link(&mut z, "depends_on", &t1, None);
    link(&mut z, "depends_on", &t1, None);
    link(&mut z, "implements", &t1, Some("z"));

    // the changed generator: the only child of p2 and the only source into t2
    let mut x = blk(id("c-2XXXX"), "claim", Some(&p2));
    x.body = "This cites {{D-T1111}}.".into();
    x.pins.insert(t1.clone(), Hash::of(b"old t1 pin"));
    link(&mut x, "depends_on", &t2, None);
    link(&mut x, "depends_on", &t2, None);
    link(&mut x, "depends_on", &t1, Some("x"));
    link(&mut x, "implements", &t2, Some("y"));
    link(&mut x, "implements", &t2, Some("z"));
    // reparented to p1 between its siblings, and moved from t2 to t3
    let mut x2 = blk(x.id.clone(), "claim", Some(&p1));
    x2.body = x.body.clone();
    x2.pins.insert(t1.clone(), Hash::of(b"new t1 pin"));
    x2.pins.insert(t3.clone(), Hash::of(b"t3 pin"));
    link(&mut x2, "depends_on", &t3, None);
    link(&mut x2, "depends_on", &t3, None);
    link(&mut x2, "depends_on", &t1, Some("x2"));
    link(&mut x2, "implements", &t3, Some("y"));
    link(&mut x2, "implements", &t3, Some("z"));

    let base: Vec<Block> = [&p1, &p2]
        .into_iter()
        .map(|p| blk(p.clone(), "section", None))
        .chain(
            [&t1, &t2, &t3]
                .into_iter()
                .map(|t| blk(t.clone(), "decision", None)),
        )
        .chain([a, z])
        .collect();
    let before = by_id(base.iter().cloned().chain([x.clone()]));
    let after = by_id(base.iter().cloned().chain([x2.clone()]));
    let g = rebuild(&before, &tracking);
    assert_eq!(
        generators(g.incoming(&t1)),
        [
            "c-1AAAA", "c-1AAAA", "c-2XXXX", "c-2XXXX", "c-3ZZZZ", "c-3ZZZZ", "c-3ZZZZ"
        ]
    );

    let (next, _) = carry(&g, &[&x], &[&x2]);
    let expected = rebuild(&after, &tracking);
    for key in [
        &p1,
        &p2,
        &t1,
        &t2,
        &t3,
        &x.id,
        &id("c-1AAAA"),
        &id("c-3ZZZZ"),
    ] {
        assert_eq!(next.outgoing(key), expected.outgoing(key), "out[{key}]");
        assert_eq!(next.incoming(key), expected.incoming(key), "inc[{key}]");
    }
    assert!(next == expected, "carried != rebuild");

    // the exact lists: generator runs in id order, duplicates kept, parts and pins carried
    assert_eq!(
        generators(next.outgoing(&p1)),
        ["c-1AAAA", "c-2XXXX", "c-3ZZZZ"]
    );
    let into_t1 = next.incoming(&t1);
    assert_eq!(
        generators(into_t1),
        [
            "c-1AAAA", "c-1AAAA", "c-2XXXX", "c-2XXXX", "c-3ZZZZ", "c-3ZZZZ", "c-3ZZZZ"
        ]
    );
    let new_pin = Some(Hash::of(b"new t1 pin"));
    assert_eq!(
        (
            into_t1[2].kind.clone(),
            into_t1[2].part.clone(),
            into_t1[2].pin
        ),
        (EdgeKind::References, None, new_pin)
    );
    assert_eq!(
        (
            into_t1[3].kind.clone(),
            into_t1[3].part.as_deref(),
            into_t1[3].pin
        ),
        (EdgeKind::Link("depends_on".into()), Some("x2"), new_pin)
    );
    assert_eq!(into_t1[4], into_t1[5], "a duplicate link is kept twice");
    let into_t3: Vec<(String, Option<&str>)> = next
        .incoming(&t3)
        .iter()
        .map(|e| (e.kind.name(), e.part.as_deref()))
        .collect();
    assert_eq!(
        into_t3,
        [
            ("depends_on".to_string(), None),
            ("depends_on".to_string(), None),
            ("implements".to_string(), Some("y")),
            ("implements".to_string(), Some("z")),
        ]
    );
    assert!(
        next.incoming(&t3)
            .iter()
            .all(|e| e.pin == Some(Hash::of(b"t3 pin")))
    );

    // abandoned lists are dropped, not left empty
    assert!(!next.out.contains_key(&p2), "p2 has no child left");
    assert!(!next.inc.contains_key(&t2), "nothing links to t2");
    assert!(g == rebuild(&before, &tracking), "the original changed");
}

fn tracking() -> Arc<Tracking> {
    Arc::new(Tracking::new(BTreeMap::from([(
        "decision".to_string(),
        BTreeSet::from(["review_by".to_string()]),
    )])))
}

fn dec(i: &str, status: &str, section: &BlockId) -> Block {
    let mut d = blk(id(i), "decision", Some(section));
    d.fields
        .insert("status".into(), FieldValue::Str(status.into()));
    d.fields.insert(
        "statement".into(),
        FieldValue::Str(format!("statement of {i}")),
    );
    d.fields
        .insert("review_by".into(), FieldValue::Str("2027-01-01".into()));
    d
}

/// `d` with a `reaffirmed` record holding `against` for `target`.
fn reaffirm(d: &mut Block, target: &str, against: Against) {
    let map = BTreeMap::from([(
        id(target),
        Reaffirmation {
            against,
            reason: format!("{target} still holds"),
        },
    )]);
    d.fields.insert(
        engine_fields::REAFFIRMED.into(),
        engine_fields::encode_reaffirmed(&map),
    );
}

#[test]
fn plz4_relink_matches_rebuild_for_mixed_changes() {
    let t = tracking();
    let s = id("s-SSSSS");
    let section = blk(s.clone(), "section", None);
    let d1 = dec("D-AAAAA", "accepted", &s);
    let mut d2 = dec("D-BBBBB", "accepted", &s);
    link(&mut d2, "depends_on", &d1.id, None);
    d2.pins.insert(d1.id.clone(), d1.content_hash_with(&t));
    reaffirm(&mut d2, "D-GGGGG", Against::Deleted);
    let gone = dec("D-CCCCC", "proposed", &s);
    let stub = BlockResidue::of(&dec("D-RRRRR", "accepted", &s)).stub(Hash::of(b"a request"));
    let mut claim = blk(id("c-CCCCC"), "claim", Some(&s));
    claim.body = "Cites {{D-AAAAA}} and {{D-RRRRR}}.".into();
    link(&mut claim, "depends_on", &id("D-CCCCC"), None);

    // the batch: delete D-CCCCC, update the claim, add a superseder of D-AAAAA
    let mut claim2 = claim.clone();
    claim2.links.clear();
    link(&mut claim2, "depends_on", &d2.id, Some("its premise"));
    claim2.pins.insert(d2.id.clone(), d2.content_hash_with(&t));
    let mut d3 = dec("D-DDDDD", "accepted", &s);
    link(&mut d3, "supersedes", &d1.id, None);
    reaffirm(&mut d3, "D-AAAAA", Against::Retiring(vec![id("D-DDDDD")]));

    let shared = vec![section, d1, d2, stub];
    let before = by_id(shared.iter().cloned().chain([gone.clone(), claim.clone()]));
    let after = by_id(shared.iter().cloned().chain([claim2.clone(), d3.clone()]));
    let g = rebuild(&before, &t);
    assert!(!g.redacted.is_empty() && !g.reaffirmed.is_empty() && !g.contents.is_empty());

    let (next, _) = carry(&g, &[&gone, &claim], &[&claim2, &d3]);
    let expected = rebuild(&after, &t);
    assert!(Arc::ptr_eq(next.tracking(), &t), "the same tracking Arc");
    assert_eq!(next.decision_status, expected.decision_status);
    assert_eq!(next.contents, expected.contents);
    assert_eq!(next.redacted, expected.redacted);
    assert_eq!(next.reaffirmed, expected.reaffirmed);
    assert!(next == expected, "carried != rebuild");
    assert_eq!(expected.reaffirmed.len(), 2);
    assert!(g == rebuild(&before, &t), "the original changed");

    let (empty, _) = carry(&next, &[], &[]);
    assert!(empty == expected, "an empty batch changes nothing");
}
