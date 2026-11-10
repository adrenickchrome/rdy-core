//! rpl25 (repo links, D-CN4QJ): a person in the citing project redacts a cited version with an
//! ordinary redaction request. The selector `<alias>:<id>@<version>` (or its stored form
//! `<repo>:<id>@<version>`) names exactly one delivered version: the `foreign_version` target.
//! The request takes the existing report, approval and freshness flow; an agent alone cannot
//! apply it (`E_REDACTION_HUMAN`). The scan and the byte removal reach the foreign copy's
//! occurrences only: local content, local references and every foreign pin keep their bytes and
//! their identity, and the removed version keeps its version and content hashes through its
//! residue.
use rdy_engine::answer::AnswerRequest;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::pending::{CARD_MAX_TOKENS, ItemKind, PendingQuery};
use rdy_engine::redaction::{RedactInput, RedactResult};
use rdy_engine::*;
use rdy_model::foreign::{
    ForeignDerived, ForeignEntry, ForeignEvent, ForeignId, ForeignPin, ForeignPublication,
    ForeignSource, ForeignVersion, selection_identity,
};
use rdy_model::redaction::{CONTENT_REMOVED, RedactTarget};
use rdy_model::*;
use rdy_qa::intake::TEST_SUMMARY;
use rdy_store::{codec, kinds};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

const PERSON: &str = "human:founder";
const AGENTS: &str = "pf-AGNT1";
/// The text only the first delivered version of X holds.
const X1_TEXT: &str = "Use the X1 rule for every cited build.";
/// The local claim's own text: local content that no removal of a cited version reaches.
const LOCAL_TEXT: &str = "Our claim keeps its own words.";

fn writer() -> Session {
    Session::human(PERSON.into(), Role::Writer).unwrap()
}

fn approver() -> Session {
    Session::human(PERSON.into(), Role::Approver).unwrap()
}

fn agent() -> Session {
    Session::agent("agent:a".into(), Some("run-1".into()), Role::Writer).unwrap()
}

/// An agent session that claims the approver role, which `Session::agent` never builds.
fn forged_agent_approver() -> Session {
    Session {
        actor: "agent:a".into(),
        run: Some("run-1".into()),
        role: Role::Approver,
        kind: SessionKind::Agent,
    }
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
        answer(r, &approver(), &item, "confirm").unwrap();
    }
}

fn answer(
    r: &mut Rdy,
    s: &Session,
    r#ref: &str,
    option: &str,
) -> Result<rdy_engine::answer::AnswerInfo, RdyError> {
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
}

fn land(r: &mut Rdy, view: &str) {
    let p = r
        .propose(&writer(), view, view, Some(TEST_SUMMARY))
        .unwrap();
    confirm_exceptions(r, &p);
    r.land(&approver(), &p.id)
        .unwrap_or_else(|e| panic!("land {view}: {e:?}"));
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

/// A landed project with a doc, a section and a repo link `core` to the cited repo. The first
/// delivery holds X (version 1) and Y; a landed local claim cites X, so its pin holds X1's
/// content; a second delivery changes X to version 2.
struct World {
    dir: tempfile::TempDir,
    r: Rdy,
    claim: BlockId,
    x1: Block,
    x2: Block,
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
                "body": format!("{LOCAL_TEXT} Follow {{{{core:d-FRGNX}}}}."),
                "links": {"decided_by": [{"target": "core:d-FRGNX"}]}}]),
    );
    land(&mut r, "cite");
    src.deliver(&mut r, &[], &[&x2]);
    World {
        dir,
        r,
        claim: id(&c.created["$c"]),
        x1,
        x2,
        y,
    }
}

fn input(targets: &[String], dry_run: bool) -> RedactInput {
    RedactInput {
        targets: targets.to_vec(),
        reason_class: "personal".into(),
        reason: "a name in a cited decision".into(),
        dry_run,
    }
}

fn dry(r: &Rdy, targets: &[String]) -> Result<RedactResult, RdyError> {
    r.read()
        .redaction_report(&input(targets, true))
        .map(|report| RedactResult {
            dry_run: true,
            reason_class: rdy_model::redaction::ReasonClass::Personal,
            reason: String::new(),
            change: None,
            redaction: None,
            warnings: report.warnings.clone(),
            report,
        })
}

fn aliased(b: &Block) -> String {
    format!("core:{}@{}", b.id, version_of(b))
}

fn stored(b: &Block) -> String {
    format!("{}:{}@{}", cited(), b.id, version_of(b))
}

fn foreign_target(b: &Block) -> RedactTarget {
    RedactTarget::ForeignVersion {
        repo: cited(),
        id: b.id.clone(),
        version: version_of(b),
    }
}

/// Whether any segment file of the project holds `needle`'s bytes.
fn on_disk(dir: &Path, needle: &str) -> bool {
    let segs = dir.join("p/.rdy/store/segments");
    std::fs::read_dir(segs).unwrap().any(|e| {
        let bytes = std::fs::read(e.unwrap().path()).unwrap();
        bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
    })
}

fn expect_err(e: &RdyError, code: &str, message: &str, fix: &str) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (code, message, fix),
        "{e:?}"
    );
}

fn target_err(e: &RdyError, target: &str, cause: &str) {
    expect_err(
        e,
        E_REDACTION_TARGET,
        &format!("Cannot redact {target}: {cause}."),
        "Use a block id, id@full-version-hash, or blob:full-manifest-hash.",
    );
}

/// fsck passes on the store's index and on a rebuilt one.
fn clean(r: &mut Rdy) {
    r.store().reader().check_objects().unwrap();
    r.store_mut().reindex().unwrap();
    r.store().reader().check_objects().unwrap();
}

/// Records the removal of `targets` in view `redact` as `s` and proposes it.
fn request(r: &mut Rdy, s: &Session, targets: &[String]) -> (RedactResult, ProposalInfo) {
    let res = r.redact_in(s, "redact", input(targets, false)).unwrap();
    assert!(res.redaction.is_some(), "the request is recorded: {res:#?}");
    let p = r
        .propose(
            s,
            "redact",
            "Remove the name",
            Some("Removes a cited name."),
        )
        .unwrap();
    (res, p)
}

#[test]
fn rpl25_foreign_version_target_is_exact() {
    let _clock = pin_clock(2_000_000);
    let w = world();
    let r = &w.r;

    // the alias form names exactly the delivered version X1: one BLOCK frame, no change
    let one = dry(r, &[aliased(&w.x1)]).unwrap();
    assert_eq!(one.report.targets, vec![foreign_target(&w.x1)]);
    assert_eq!(
        one.report.versions,
        vec![(w.x1.id.clone(), version_of(&w.x1))]
    );
    assert!(one.report.already_redacted.is_empty());
    assert_eq!(one.report.drop_frames, 1, "{:#?}", one.report);
    assert!(
        one.report
            .inside
            .iter()
            .all(|row| row.kind != rdy_model::redaction::ImpactKind::Change
                || row.relation != rdy_model::redaction::ImpactRelation::Embeds),
        "no local change embeds a cited version: {:#?}",
        one.report.inside
    );
    // the stored form is the same target; both forms together are one target
    let same = dry(r, &[stored(&w.x1)]).unwrap();
    assert_eq!(same.report.report_hash, one.report.report_hash);
    let both = dry(r, &[aliased(&w.x1), stored(&w.x1)]).unwrap();
    assert_eq!(both.report.targets, vec![foreign_target(&w.x1)]);
    assert_eq!(both.report.report_hash, one.report.report_hash);
    // X2 is another version: another target, another frame
    let two = dry(r, &[aliased(&w.x2)]).unwrap();
    assert_eq!(two.report.targets, vec![foreign_target(&w.x2)]);
    assert_ne!(two.report.report_hash, one.report.report_hash);

    // an alias no live repo link holds
    let e = dry(r, &[format!("nope:{}@{}", w.x1.id, version_of(&w.x1))]).unwrap_err();
    expect_err(
        &e,
        E_UNKNOWN_REF,
        "no repo link has the alias nope",
        "use the alias of a linked project: rdy_links / rdy links",
    );
    // a version no delivery holds: the local claim's own version under the alias
    let claim_v = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .version_of(&w.claim)
        .unwrap();
    let e = dry(r, &[format!("core:{}@{claim_v}", w.claim)]).unwrap_err();
    target_err(
        &e,
        &format!("{}:{}@{claim_v}", cited(), w.claim),
        "unknown target",
    );
    // a delivered version of another block
    let e = dry(r, &[format!("core:{}@{}", w.x1.id, version_of(&w.y))]).unwrap_err();
    target_err(
        &e,
        &format!("{}:{}@{}", cited(), w.x1.id, version_of(&w.y)),
        "version belongs to another block",
    );
    // no version, a key, or a short hash is no selector
    for text in [
        format!("core:{}", w.x1.id),
        format!("core:CORE-RULE@{}", version_of(&w.x1)),
        format!("core:{}@{}", w.x1.id, &version_of(&w.x1).to_string()[..12]),
    ] {
        let e = dry(r, std::slice::from_ref(&text)).unwrap_err();
        target_err(
            &e,
            &serde_json::to_string(&text).unwrap(),
            "invalid selector",
        );
    }
    // a local selector never reaches the foreign copy
    let e = dry(r, &[format!("{}@{}", w.x1.id, version_of(&w.x1))]).unwrap_err();
    target_err(
        &e,
        &format!("{}@{}", w.x1.id, version_of(&w.x1)),
        "unknown target",
    );
    let e = dry(r, &[w.x1.id.to_string()]).unwrap_err();
    target_err(&e, w.x1.id.as_str(), "unknown target");
}

#[test]
fn rpl25_foreign_redaction_requires_person() {
    let _clock = pin_clock(3_000_000);
    let mut w = world();
    let r = &mut w.r;
    // an agent may record the request
    let (res, prop) = request(r, &agent(), &[aliased(&w.x1)]);
    assert_eq!(res.report.targets, vec![foreign_target(&w.x1)]);
    let recorded = r
        .read()
        .reader()
        .redaction_request(&res.redaction.unwrap())
        .unwrap();
    assert_eq!(recorded.targets, vec![foreign_target(&w.x1)]);
    assert_eq!(
        recorded.encoding(),
        rdy_model::encoding::REDACTION_FOREIGN_ENCODING
    );
    let end = r.store().log_end();
    // an agent alone, or one that claims the approver role, cannot apply it
    for (who, what) in [
        (agent(), "an agent"),
        (forged_agent_approver(), "a forged agent approver"),
    ] {
        let e = r.land(&who, &prop.id).unwrap_err();
        expect_err(
            &e,
            E_REDACTION_HUMAN,
            "Content removal needs a person's approval.",
            "Show the redaction card and relay the person's answer, or approve from the CLI as a human.",
        );
        assert_eq!(r.store().log_end(), end, "{what}: nothing is written");
        assert!(on_disk(w.dir.path(), X1_TEXT), "{what}: the bytes stay");
    }
    // the card is the ordinary removal card, and the person approves it
    let item = r
        .read()
        .pending(&PendingQuery {
            person: PERSON.into(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .unwrap()
        .items
        .into_iter()
        .find(|i| i.r#ref == prop.r#ref)
        .expect("the removal item");
    assert!(item.card.contains("Content removal"), "{}", item.card);
    let removal = item.removal.expect("a removal card");
    assert_eq!(removal[0].request, res.redaction.unwrap());
    assert_eq!(removal[0].report, res.report.report_hash);
    answer(r, &approver(), &prop.r#ref, "approve").unwrap();

    // the foreign copy's bytes are gone; the held version reads as its residue
    assert!(!on_disk(w.dir.path(), X1_TEXT));
    let st = r.read();
    st.ensure_current_epoch().unwrap();
    let held = st
        .foreign_held(
            &ForeignId::new(cited(), w.x1.id.clone()),
            &version_of(&w.x1),
        )
        .unwrap()
        .expect("the held version still reads");
    assert!(held.is_redacted());
    assert_eq!(held.body, CONTENT_REMOVED);
    assert_eq!(held.version_hash(), version_of(&w.x1));
    // the request is already removed now
    let again = dry(r, &[aliased(&w.x1)]).unwrap();
    assert_eq!(again.report.already_redacted, vec![foreign_target(&w.x1)]);
    // fsck, a rebuilt index and a reopen all accept it
    clean(r);
    let dir = w.dir.path().join("p");
    drop(w.r);
    let mut r = Rdy::open(&dir).unwrap();
    clean(&mut r);
    let held = r
        .read()
        .foreign_held(
            &ForeignId::new(cited(), w.x1.id.clone()),
            &version_of(&w.x1),
        )
        .unwrap()
        .unwrap();
    assert_eq!(held.body, CONTENT_REMOVED);
}

#[test]
fn rpl25_redaction_leaves_local_content_and_pin_identity() {
    let _clock = pin_clock(4_000_000);
    let mut w = world();
    let r = &mut w.r;
    let canon = |r: &Rdy, b: &BlockId| r.read().block(&Scope::Canon, b).unwrap();
    let before = canon(r, &w.claim);
    let target = ForeignId::new(cited(), w.x1.id.clone());
    let pin = before.foreign.pins[&target].clone();
    assert_eq!(
        pin,
        ForeignPin {
            content: content_of(&w.x1),
            encoding: 0
        }
    );
    let (res, prop) = request(r, &writer(), &[aliased(&w.x1)]);
    // the report names the local claim's text nowhere as removed: only the cited version is
    assert_eq!(
        res.report.versions,
        vec![(w.x1.id.clone(), version_of(&w.x1))]
    );
    answer(r, &approver(), &prop.r#ref, "approve").unwrap();

    // local content keeps its bytes, its version and its pin
    assert!(on_disk(w.dir.path(), LOCAL_TEXT), "local bytes stay");
    assert!(!on_disk(w.dir.path(), X1_TEXT));
    let after = canon(r, &w.claim);
    assert!(!after.is_redacted());
    assert_eq!(after.version_hash(), before.version_hash());
    assert_eq!(after.body, before.body);
    assert_eq!(after.foreign, before.foreign);
    // the pin still resolves to the version that carried its content, and the held-pin check
    // still passes
    let st = r.read();
    assert_eq!(
        st.foreign_held_pin(&target, &pin).unwrap(),
        Some(version_of(&w.x1))
    );
    st.check_held_pins(&Scope::Canon, [&after]).unwrap();
    // the residue keeps the cited version's identity: its version and content hashes
    let held = st
        .foreign_held(&target, &version_of(&w.x1))
        .unwrap()
        .unwrap();
    assert!(held.is_redacted());
    assert_eq!(held.version_hash(), version_of(&w.x1));
    assert_eq!(held.content_hash(), w.x1.content_hash());
    // the other delivered versions are intact
    let x2 = st
        .foreign_held(&target, &version_of(&w.x2))
        .unwrap()
        .unwrap();
    assert_eq!(x2.body, w.x2.body);
    let y = st
        .foreign_held(&ForeignId::new(cited(), w.y.id.clone()), &version_of(&w.y))
        .unwrap()
        .unwrap();
    assert_eq!(y.body, w.y.body);
    drop(st);
    // a later write that keeps the citation keeps the pin
    let claim = w.claim.to_string();
    write(
        r,
        "later",
        json!([{"op": "update", "id": claim,
                "body": format!("{LOCAL_TEXT} Still follow {{{{core:d-FRGNX}}}}.")}]),
    );
    let st = r.read();
    let later = st.block(&Scope::View("later".into()), &w.claim).unwrap();
    assert_eq!(later.foreign.pins[&target], pin);
    drop(st);
    clean(r);
}

/// New defect (found by the rpl25 tests): the report read a block with a foreign part through
/// its encoding-0 layout, which it has none of, so every report over a project that cites
/// failed; and the store refused the residue of such a block (BLOCK encoding 1) and of the
/// change that wrote it (CHANGE encoding 2). A local block that cites is local content: its
/// removal is an ordinary local request that leaves the foreign copy as it is.
#[test]
fn rpl25_a_block_that_cites_is_redactable() {
    let _clock = pin_clock(5_000_000);
    let mut w = world();
    let r = &mut w.r;
    let target = [w.claim.to_string()];
    let dry_run = dry(r, &target).unwrap();
    assert_eq!(dry_run.report.drop_frames, 2, "{:#?}", dry_run.report);
    let (_, prop) = request(r, &writer(), &target);
    answer(r, &approver(), &prop.r#ref, "approve").unwrap();
    let claim = r.read().block(&Scope::Canon, &w.claim).unwrap();
    assert!(claim.is_redacted());
    assert_eq!(claim.body, CONTENT_REMOVED);
    assert!(!on_disk(w.dir.path(), LOCAL_TEXT));
    // the foreign copy keeps its bytes
    assert!(on_disk(w.dir.path(), X1_TEXT));
    let held = r
        .read()
        .foreign_held(
            &ForeignId::new(cited(), w.x1.id.clone()),
            &version_of(&w.x1),
        )
        .unwrap()
        .unwrap();
    assert_eq!(held.body, X1_TEXT);
    clean(r);
}
