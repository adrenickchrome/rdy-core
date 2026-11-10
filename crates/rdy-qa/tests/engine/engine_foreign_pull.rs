//! rpl32 (repo links; D-NXHX3, D-FK58A, D-8ZEHP): the core foreign-pin check on the paths that
//! carry changes today.
//!
//! The Sync client's pull does not exist yet. D-NXHX3 moves the real-pull test (a clone, its
//! catch-up and a local replay through a public pull) to the Sync plan's acceptance. The test ids
//! keep the word "pull"; the paths they drive are view replay, dump and load, and the follower:
//!
//! - A carried change is a change that another path left in this store, unchecked by any write:
//!   here it is committed straight to the store, as a transport leaves it. View replay takes it
//!   only when every foreign pin it holds names content that a delivery of this store carried
//!   (`ReadState::check_carried_pins`, the shared held-pin check). The first pin that fails is
//!   `E_FOREIGN_VERSION`, before anything is written.
//! - A cited version that a removal erased stays held: its residue keeps its version and content
//!   identity (D-FK58A), so a pin of it still passes, and the replay restores no removed byte.
//! - A landing that cites keeps its signature statement through dump and load, the follower and a
//!   view replay of its change. The re-attachment of signatures to a replayed landing (D-8ZEHP) is
//!   the Sync client's: no core path replays a landing.
use rdy_engine::answer::AnswerRequest;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::follower::{self, FollowerCore};
use rdy_engine::pending::{CARD_MAX_TOKENS, ItemKind, PendingQuery};
use rdy_engine::redaction::RedactInput;
use rdy_engine::signature_check::{SIGNED_VALID, STATEMENT_MATCH, signature_checks};
use rdy_engine::*;
use rdy_model::foreign::{
    ForeignDerived, ForeignEntry, ForeignEvent, ForeignId, ForeignPin, ForeignPublication,
    ForeignSource, ForeignVersion, selection_identity,
};
use rdy_model::signature::{Signature, StatementSource};
use rdy_model::*;
use rdy_qa::intake::TEST_SUMMARY;
use rdy_qa::signing::{TestKey, sign_record};
use rdy_store::{codec, kinds};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

const PERSON: &str = "human:founder";
const AGENTS: &str = "pf-AGNT1";
/// The text only the first delivered version of X holds.
const X1_TEXT: &str = "Use the X1 rule for every pulled build.";

fn writer() -> Session {
    Session::human(PERSON.into(), Role::Writer).unwrap()
}

fn approver() -> Session {
    Session::human(PERSON.into(), Role::Approver).unwrap()
}

fn write(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: parse_ops(ops.as_array().unwrap().clone()).unwrap(),
            justification: None,
            message: format!("write to {view}"),
        },
    )
    .unwrap_or_else(|e| panic!("write to {view}: {e:?}"))
}

fn answer(r: &mut Rdy, s: &Session, r#ref: &str, option: &str) {
    r.answer(
        s,
        PERSON,
        AnswerRequest {
            r#ref: r#ref.into(),
            option: option.into(),
            card_hash: None,
            note: None,
            relay: RelayInput::default(),
            brief: None,
        },
    )
    .unwrap_or_else(|e| panic!("{option} {ref}: {e:?}"));
}

/// Confirms every exception item of proposal `p`.
fn confirm_exceptions(r: &mut Rdy, p: &ProposalInfo) {
    let q = r
        .read()
        .pending(&PendingQuery {
            person: PERSON.into(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .unwrap();
    let refs: Vec<String> = q
        .items
        .iter()
        .filter(|i| i.kind == ItemKind::Confirm && i.batch.as_deref() == Some(p.r#ref.as_str()))
        .map(|i| i.r#ref.clone())
        .collect();
    for item in refs {
        answer(r, &approver(), &item, "confirm");
    }
}

/// Lands view `view` and returns the landing.
fn land(r: &mut Rdy, view: &str) -> Hash {
    let p = r
        .propose(&writer(), view, view, Some(TEST_SUMMARY))
        .unwrap();
    confirm_exceptions(r, &p);
    r.land(&approver(), &p.id)
        .unwrap_or_else(|e| panic!("land {view}: {e:?}"))
        .parse()
        .unwrap()
}

fn cited() -> RepoId {
    RepoId([7; 16])
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// A block as the cited project delivers it.
fn block(id_: &str, title: &str, body: &str) -> Block {
    Block {
        id: id(id_),
        block_type: "decision".into(),
        parent: None,
        order: "V".into(),
        fields: [("title".to_string(), FieldValue::Str(title.into()))].into(),
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

/// The content identity the cited project gives `b` (any hash distinct per content does here).
fn content_of(b: &Block) -> Hash {
    Hash::of(format!("content of {} {}", b.id, b.body).as_bytes())
}

fn fid(b: &Block) -> ForeignId {
    ForeignId::new(cited(), b.id.clone())
}

fn pin(b: &Block) -> ForeignPin {
    ForeignPin {
        content: content_of(b),
        encoding: 0,
    }
}

/// A pin of content that no delivery carried.
fn ghost() -> ForeignPin {
    ForeignPin {
        content: Hash::of(b"content no delivery carried"),
        encoding: 0,
    }
}

fn short(h: &Hash) -> String {
    h.to_string()[..12].to_string()
}

fn fv(b: &Block) -> ForeignVersion {
    ForeignVersion {
        id: b.id.clone(),
        version: version_of(b),
        content: content_of(b),
        encoding: 0,
        derived: ForeignDerived::default(),
    }
}

/// One source's chain, as the citing side builds it.
struct Source {
    link: String,
    head: Option<Hash>,
    sel: BTreeMap<BlockId, Hash>,
}

impl Source {
    /// Writes a delivery straight to the store and publishes the state after it.
    fn deliver(&mut self, r: &mut Rdy, added: &[&Block], changed: &[&Block]) {
        for b in added.iter().chain(changed) {
            self.sel.insert(b.id.clone(), version_of(b));
        }
        let entry = ForeignEntry {
            repo: cited(),
            source: ForeignSource::Link {
                link: self.link.clone(),
                site: "hub.example".into(),
                at_ms: 1_760_000_000_000,
            },
            event: ForeignEvent::Delivery {
                prev: self.head,
                publication: ForeignPublication {
                    tag: "v1".into(),
                    profile: AGENTS.into(),
                    profile_version: None,
                },
                set: None,
                added: added.iter().map(|b| fv(b)).collect(),
                changed: changed.iter().map(|b| fv(b)).collect(),
                removed: vec![],
                shells: vec![],
                selection: selection_identity(self.sel.iter()).unwrap(),
            },
        };
        let blocks: Vec<Block> = added.iter().chain(changed).map(|b| (*b).clone()).collect();
        self.head = Some(r.store_mut().put_foreign(&entry, &blocks).unwrap());
        r.publish_after_foreign_for_tests().unwrap();
    }
}

/// A project with a doc, a section and a repo link `core` to the cited repo. The first delivery
/// holds X (version 1) and Y. The local claim cites both and lands, so its pins hold X1's and Y's
/// content. A second delivery then changes X to version 2: X1 is held, no longer current.
struct World {
    dir: tempfile::TempDir,
    r: Rdy,
    claim: BlockId,
    /// The change that created the claim.
    cite: Hash,
    /// The landing that holds it.
    landing: Hash,
    x1: Block,
    y: Block,
}

fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(&dir.path().join("p"), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "repo_link", "id": "$a", "parent": "$d",
             "fields": {"alias": "core", "repo": cited().to_string(), "profile": AGENTS}}
        ]),
    );
    land(&mut r, "seed");
    let x1 = block("d-FRGNX", "X", X1_TEXT);
    let x2 = block("d-FRGNX", "X", "Use the X2 rule.");
    let y = block("d-FRGNY", "Y", "Use Y.");
    let mut src = Source {
        link: w.created["$a"].clone(),
        head: None,
        sel: BTreeMap::new(),
    };
    src.deliver(&mut r, &[&x1, &y], &[]);
    let c = write(
        &mut r,
        "cite",
        json!([{"op": "create", "type": "claim", "id": "$c", "parent": w.created["$s"],
                "body": "Follow {{core:d-FRGNX}} and {{core:d-FRGNY}}."}]),
    );
    let landing = land(&mut r, "cite");
    src.deliver(&mut r, &[], &[&x2]);
    let claim = id(&c.created["$c"]);
    let cited_here = r.read().block(&Scope::Canon, &claim).unwrap();
    assert_eq!(
        cited_here.foreign.pins,
        BTreeMap::from([(fid(&x1), pin(&x1)), (fid(&y), pin(&y))]),
        "the landed claim pins X1 and Y"
    );
    World {
        dir,
        r,
        claim,
        cite: c.change.parse().unwrap(),
        landing,
        x1,
        y,
    }
}

/// A carried change: an update of the canon claim `claim` whose body is `body` and whose foreign
/// pins are `pins`, committed straight to the store in view `view` (no write checks it), sealed
/// and published. Returns its hash.
fn carry(
    r: &mut Rdy,
    view: &str,
    claim: &BlockId,
    body: &str,
    pins: &[(ForeignId, ForeignPin)],
) -> Hash {
    let st = r.read();
    let base = st
        .materialized(&Scope::Canon)
        .unwrap()
        .version_of(claim)
        .unwrap();
    let mut b = st.block(&Scope::Canon, claim).unwrap();
    drop(st);
    b.read_identity = None;
    b.body = body.into();
    b.foreign.pins = pins.iter().cloned().collect();
    let change = Change {
        view: view.into(),
        ops: vec![Op::Update { base, block: b }],
        deps: vec![],
        provenance: writer().provenance(),
        justification: None,
        message: format!("carried into {view}"),
        links: ChangeLinks::default(),
        read_identity: None,
    };
    let h = r.store_mut().commit_to_view(view, change).unwrap();
    r.seal_open().unwrap();
    r.invalidate();
    h
}

/// Replays `changes` into a new view `view` owned by the writer.
fn replay(r: &mut Rdy, view: &str, changes: &[Hash]) -> Result<views::ReplayResult, RdyError> {
    r.view_create(&writer(), view, None, false).unwrap();
    let input: Vec<String> = changes.iter().map(ToString::to_string).collect();
    r.view_replay(&writer(), view, &input)
}

fn expect_err(e: &RdyError, code: &str, message: &str, fix: &str) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (code, message, fix),
        "{e:?}"
    );
}

const MISSING_FIX: &str = "wait for the delivery that holds it, or pin a delivered version: rdy_links {op: \"show\", alias} / rdy links show core";

/// Whether any segment file of the project at `project` holds `needle`'s bytes.
fn on_disk(project: &Path, needle: &str) -> bool {
    let segs = project.join(".rdy/store/segments");
    std::fs::read_dir(segs).unwrap().any(|e| {
        let bytes = std::fs::read(e.unwrap().path()).unwrap();
        bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
    })
}

/// Erases the foreign copy of `b` (one delivered version) with a person's approval (rpl25).
fn erase(r: &mut Rdy, b: &Block) {
    let target = format!("core:{}@{}", b.id, version_of(b));
    let res = r
        .redact_in(
            &writer(),
            "redact",
            RedactInput {
                targets: vec![target],
                reason_class: "personal".into(),
                reason: "a name in a cited decision".into(),
                dry_run: false,
            },
        )
        .unwrap();
    assert!(res.redaction.is_some(), "{res:#?}");
    let p = r
        .propose(
            &writer(),
            "redact",
            "Remove the name",
            Some("Removes a cited name."),
        )
        .unwrap();
    answer(r, &approver(), &p.r#ref, "approve");
}

/// View replay takes a carried change only when each foreign pin it holds is held here: a pin of
/// content that a delivery carried passes, whether that version is still current, superseded by
/// a newer delivery, or erased by a removal (its residue keeps its identity, and the replay
/// restores no removed byte). Any other pin is `E_FOREIGN_VERSION` for its target, naming the
/// pinned content, and the replay writes nothing: not the view op, not the changes with held
/// pins that the same call selected.
#[test]
fn rpl32_pull_replay_requires_each_held_foreign_pin() {
    let _clock = pin_clock(1_000_000);
    let mut w = world();
    let r = &mut w.r;
    let (x, y) = (fid(&w.x1), fid(&w.y));

    // both pins held: X1 is superseded by X2 and still held; Y is current
    let held_pins = [(x.clone(), pin(&w.x1)), (y.clone(), pin(&w.y))];
    let held = carry(r, "carried-held", &w.claim, "Held.", &held_pins);
    let got = replay(r, "target-held", &[held]).unwrap();
    assert_eq!(got.replayed.len(), 1);
    assert!(got.conflicts.is_empty(), "{got:?}");
    let replayed = r
        .read()
        .block(&Scope::View("target-held".into()), &w.claim)
        .unwrap();
    assert_eq!(replayed.body, "Held.");
    assert_eq!(
        replayed.foreign.pins,
        held_pins.iter().cloned().collect::<BTreeMap<_, _>>(),
        "the replayed block keeps its pins byte for byte"
    );

    // one pin of content no delivery carried: refused for that target, nothing written
    let bad = carry(
        r,
        "carried-ghost",
        &w.claim,
        "Ghost.",
        &[(x.clone(), pin(&w.x1)), (y.clone(), ghost())],
    );
    r.view_create(&writer(), "target-ghost", None, false)
        .unwrap();
    let seq = r.read().seq();
    let e = r
        .view_replay(&writer(), "target-ghost", &[bad.to_string()])
        .unwrap_err();
    expect_err(
        &e,
        E_FOREIGN_VERSION,
        &format!(
            "core:d-FRGNY has no delivered version {}",
            short(&ghost().content)
        ),
        MISSING_FIX,
    );
    assert_eq!(r.read().seq(), seq, "the refused replay writes nothing");
    assert!(r.read().included("target-ghost").is_empty());

    // each source is checked: a held change selected with a refused one is not written either
    let e = r
        .view_replay(
            &writer(),
            "target-ghost",
            &[held.to_string(), bad.to_string()],
        )
        .unwrap_err();
    assert_eq!(e.code, E_FOREIGN_VERSION, "{e:?}");
    assert_eq!(r.read().seq(), seq);
    assert!(r.read().included("target-ghost").is_empty());

    // the right content under another encoding, or another block's delivered content, is no
    // held version
    for (i, (target, wrong)) in [
        (
            x.clone(),
            ForeignPin {
                content: content_of(&w.x1),
                encoding: 1,
            },
        ),
        (x.clone(), pin(&w.y)),
    ]
    .into_iter()
    .enumerate()
    {
        let short_content = short(&wrong.content);
        let c = carry(
            r,
            &format!("carried-wrong-{i}"),
            &w.claim,
            "Wrong.",
            &[(target, wrong)],
        );
        let e = r
            .view_replay(&writer(), "target-ghost", &[c.to_string()])
            .unwrap_err();
        expect_err(
            &e,
            E_FOREIGN_VERSION,
            &format!("core:d-FRGNX has no delivered version {short_content}"),
            MISSING_FIX,
        );
    }

    // an erased version stays held through its residue: the pin passes, no byte comes back
    let project = w.dir.path().join("p");
    assert!(on_disk(&project, X1_TEXT));
    erase(r, &w.x1);
    assert!(!on_disk(&project, X1_TEXT), "the foreign copy is erased");
    let erased = carry(
        r,
        "carried-erased",
        &w.claim,
        "Erased.",
        &[(x.clone(), pin(&w.x1))],
    );
    let got = replay(r, "target-erased", &[erased]).unwrap();
    assert_eq!(got.replayed.len(), 1);
    let replayed = r
        .read()
        .block(&Scope::View("target-erased".into()), &w.claim)
        .unwrap();
    assert_eq!(replayed.foreign.pins[&x], pin(&w.x1));
    assert!(!on_disk(&project, X1_TEXT), "the replay restores no byte");
    // and the landed change that cites replays as well
    let got = replay(r, "target-cite", &[w.cite]).unwrap();
    assert_eq!(got.replayed.len(), 1);
    assert!(!on_disk(&project, X1_TEXT));
}

/// The person's approval signature of the landing `landing` in `r`.
fn approval_of(r: &Rdy, landing: Hash) -> Signature {
    let st = r.read();
    let record = st.reader().landing(&landing).unwrap();
    sign_record(
        landing,
        StatementSource::Approval(&record),
        st.repo_id().unwrap(),
        Provenance {
            actor: PERSON.into(),
            run: None,
            role: Role::Approver,
            at_ms: 7,
        },
        TestKey::Person,
    )
}

/// The stored records of the signatures of `landing` in `st`.
fn stored_signatures(st: &ReadState, landing: &Hash) -> Vec<Signature> {
    let reader = st.reader();
    reader
        .signatures_before(landing, st.seq())
        .unwrap()
        .iter()
        .map(|h| reader.signature(h).unwrap())
        .collect()
}

/// A landing that cites keeps its signature record and its check through dump and load and
/// through the follower: the same statement, the same SSHSIG, valid and matching. A view replay
/// of its change in the clone keeps the claim's pins and leaves the landing's signature as it
/// was. A carried change whose pin the clone does not hold is refused before anything is
/// written, so the landing's signatures and the log stay as they were.
#[test]
fn rpl32_pull_replay_preserves_signature_statement() {
    let _clock = pin_clock(2_000_000);
    let mut w = world();
    let files = tempfile::tempdir().unwrap();
    let before = files.path().join("before.dump");
    w.r.read().dump(&before).unwrap();

    let sig = approval_of(&w.r, w.landing);
    let frames = rdy_store::signature::frames(std::slice::from_ref(&sig)).unwrap();
    w.r.store_mut().write(frames).unwrap();
    w.r.seal_open().unwrap();
    let source = w.r.read();
    let checks = signature_checks(&source, &w.landing, None).unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(
        (
            checks[0].signed.as_str(),
            checks[0].statement.as_str(),
            &checks[0].code
        ),
        (SIGNED_VALID, STATEMENT_MATCH, &None)
    );
    assert_eq!(stored_signatures(&source, &w.landing), vec![sig.clone()]);
    let claim = source.block(&Scope::Canon, &w.claim).unwrap();
    drop(source);

    // dump and load: the clone holds the same record, with the same check
    let full = files.path().join("full.dump");
    w.r.read().dump(&full).unwrap();
    let clone_dir = files.path().join("clone");
    rdy_engine::load::load(&full, &clone_dir).unwrap();
    let mut c = Rdy::open(&clone_dir).unwrap();
    let st = c.read();
    assert_eq!(signature_checks(&st, &w.landing, None).unwrap(), checks);
    assert_eq!(stored_signatures(&st, &w.landing), vec![sig.clone()]);
    st.check_held_pins(&Scope::Canon, [&claim]).unwrap();
    drop(st);

    // the follower: seeded before the signature, it follows the increment that carries it
    let inc = files.path().join("inc.dump");
    w.r.read().dump_after_file(&before, &inc).unwrap();
    let fdir = files.path().join("follower");
    follower::seed(&before, &fdir).unwrap();
    let mut f = FollowerCore::open(&fdir).unwrap();
    assert!(
        signature_checks(&f.read().unwrap(), &w.landing, None)
            .unwrap()
            .is_empty()
    );
    f.follow(&inc).unwrap();
    let st = f.read().unwrap();
    assert_eq!(signature_checks(&st, &w.landing, None).unwrap(), checks);
    assert_eq!(stored_signatures(&st, &w.landing), vec![sig.clone()]);
    st.check_held_pins(&Scope::Canon, [&claim]).unwrap();
    drop((st, f));

    // a view replay of the cited change in the clone: the claim keeps its pins, and the landing's
    // statement and check stay
    let got = replay(&mut c, "again", &[w.cite]).unwrap();
    assert_eq!(got.replayed.len(), 1);
    let replayed = c.store().change(&got.added[0].parse().unwrap()).unwrap();
    assert_eq!(replayed.links.replayed_from, Some(w.cite));
    let again = c
        .read()
        .block(&Scope::View("again".into()), &w.claim)
        .unwrap();
    assert_eq!(again.foreign, claim.foreign);
    assert_eq!(again.version_hash(), claim.version_hash());
    assert_eq!(
        signature_checks(&c.read(), &w.landing, None).unwrap(),
        checks
    );

    // a carried change whose pin the clone does not hold: refused, nothing written
    let bad = carry(
        &mut c,
        "carried",
        &w.claim,
        "Ghost.",
        &[(fid(&w.x1), ghost())],
    );
    c.view_create(&writer(), "again-2", None, false).unwrap();
    let seq = c.read().seq();
    let e = c
        .view_replay(&writer(), "again-2", &[bad.to_string()])
        .unwrap_err();
    expect_err(
        &e,
        E_FOREIGN_VERSION,
        &format!(
            "core:d-FRGNX has no delivered version {}",
            short(&ghost().content)
        ),
        MISSING_FIX,
    );
    assert_eq!(c.read().seq(), seq);
    let st = c.read();
    assert_eq!(signature_checks(&st, &w.landing, None).unwrap(), checks);
    assert_eq!(stored_signatures(&st, &w.landing), vec![sig]);
    drop(st);
    c.close().unwrap();
    w.r.close().unwrap();
}

/// rpl32 fix round 1 (D-FK58A): a tag whose history holds an approved foreign tension exports
/// and a holder takes it. The landing's assessment names the FOREIGN entry that delivered the
/// tension's cited side and the held version of each pin; those are foreign-only edges, which
/// the source store's own checks keep and the portable closure leaves out. The batch carries the
/// assessment byte for byte and nothing of the foreign log: no FOREIGN entry and no delivered
/// BLOCK version. The holder then passes its check, and the source still passes fsck.
#[test]
fn rpl32_tag_with_approved_foreign_tension_exports_and_takes() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(&dir.path().join("p"), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "repo_link", "id": "$a", "parent": "$d",
             "fields": {"alias": "core", "repo": cited().to_string(), "profile": AGENTS}}
        ]),
    );
    land(&mut r, "seed");
    // one delivery: X, deprecated where it comes from
    let x = block("d-FRGNX", "X", X1_TEXT);
    let mut v = fv(&x);
    v.derived.lifecycle = Some("deprecated".into());
    let entry = ForeignEntry {
        repo: cited(),
        source: ForeignSource::Link {
            link: w.created["$a"].clone(),
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
            added: vec![v],
            changed: vec![],
            removed: vec![],
            shells: vec![],
            selection: selection_identity([(&x.id, &version_of(&x))].into_iter()).unwrap(),
        },
    };
    let delivered = r
        .store_mut()
        .put_foreign(&entry, std::slice::from_ref(&x))
        .unwrap();
    r.publish_after_foreign_for_tests().unwrap();
    // a claim that follows the deprecated X: its foreign tension is answered and the view lands
    write(
        &mut r,
        "p",
        json!([{"op": "create", "type": "claim", "id": "$c", "parent": w.created["$s"],
                "body": "Follow {{core:d-FRGNX}}."}]),
    );
    rdy_qa::intake::answer_open_items(&mut r, &writer(), "p");
    let landing = land(&mut r, "p");
    let assessment = r
        .store()
        .reader()
        .landing(&landing)
        .unwrap()
        .assessment
        .expect("the landing has an assessment");
    let wire = r.store().reader().assessment_wire(&assessment).unwrap();
    assert_eq!(wire.foreign.tensions.len(), 1, "{:#?}", wire.foreign);
    assert_eq!(wire.foreign.tensions[0].entry, delivered);
    let held: Vec<Hash> = wire.foreign.reach.iter().map(|r| r.version).collect();
    assert!(held.contains(&version_of(&x)), "{:#?}", wire.foreign);
    let tag: Hash = r
        .tag(&approver(), "v1.0.0", None, None)
        .unwrap()
        .tag
        .parse()
        .unwrap();
    let repo = r.read().repo_id().unwrap();

    // the export: the assessment travels, the foreign log does not
    let mut out = Vec::new();
    let report = rdy_engine::stream_export::write(
        &r.read(),
        tag,
        &std::collections::BTreeSet::new(),
        &mut out,
    )
    .unwrap_or_else(|e| panic!("export v1.0.0: {e:?}"));
    let key = |kind: u8, hash: Hash| rdy_store::object_refs::ObjectKey { kind, hash };
    assert!(report.keys.contains(&key(kinds::ASSESSMENT, assessment)));
    assert!(
        report.keys.iter().all(|k| k.kind != kinds::FOREIGN),
        "no FOREIGN entry travels: {:?}",
        report.keys
    );
    for v in &held {
        assert!(
            !report.keys.contains(&key(kinds::BLOCK, *v)),
            "no delivered version travels: {v}"
        );
    }

    // a holder takes it: the assessment keeps its identity, and no foreign record arrives
    let path = dir.path().join("history.rdysync");
    std::fs::write(&path, &out).unwrap();
    let mut h = Rdy::create_holder(&dir.path().join("h"), repo).unwrap();
    rdy_engine::stream_take::take(
        &mut h,
        &[std::fs::File::open(&path).unwrap()],
        rdy_store::take_batch::UploadTarget {
            tag: "v1.0.0".into(),
            landing,
        },
    )
    .unwrap_or_else(|e| panic!("take v1.0.0: {e:?}"));
    let hr = h.store().reader();
    assert_eq!(hr.assessment_wire(&assessment).unwrap(), wire);
    assert!(!hr.has_kind(kinds::FOREIGN, &delivered).unwrap());
    for v in &held {
        assert!(!hr.has_kind(kinds::BLOCK, v).unwrap(), "{v}");
    }
    drop(hr);
    h.fsck().unwrap();
    r.fsck().unwrap();
    h.close().unwrap();
    r.close().unwrap();
}
