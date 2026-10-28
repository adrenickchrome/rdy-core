//! rdx15: the boundaries of content removal (D-600CW: "Content may leave the store; the record
//! that it left never does."; D-CC90E: removal lands only with a person's approval; D-3M81W:
//! hashes stay verifiable; D-33JR0: snapshot history is in the blast radius; D-EPK1Z: retained
//! issue text is report-only).
//!
//! Unapproved requests, every refusal path and forged approvals never remove a byte; an approved
//! removal keeps its logical history and masks through a restart, a reindex, a dump and load and
//! a clone; a missing payload opens only under its exact authorization; state snapshots stay
//! fixed references and no assessment is invented. The generated sequences are seeded and hold
//! at most 128 operations; a failure prints the seed, the operation kinds and ids, never a
//! payload or a selected canary.
#[path = "../../../rdy-store/tests/support/redaction.rs"]
mod redaction_fixture;

use rdy_engine::answer::AnswerRequest;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::clone::{CloneSource, clone};
use rdy_engine::load::load;
use rdy_engine::pending::{CARD_MAX_TOKENS, Item, PendingQuery};
use rdy_engine::redaction::RedactInput;
use rdy_engine::{
    DecisionInput, E_REDACTION_CORRUPT, ProposalInfo, Rdy, Scope, Session, SessionKind,
    WriteRequest,
};
use rdy_model::redaction::{
    AssessmentSupport, CONTENT_REMOVED, DropSpan, ImpactKind, ImpactRelation, RedactTarget,
};
use rdy_model::{BlockId, Hash, LandingRecord, Provenance, Role, StateSnapshot};
use rdy_qa::every_kind;
use rdy_qa::fixtures::{approver, land, write, writer};
use rdy_qa::invariants::{check, check_all, frozen_report};
use rdy_store::dump::DUMP_FORMAT;
use rdy_store::format::{DumpStore, baseline_features};
use rdy_store::kinds;
use rdy_store::redaction::StoredFrame;
use rdy_store::segment::Location;
use rdy_store::{LandRequest, Store};
use redaction_fixture::{
    FixtureOptions, Proof, RequestSpec, authorized_batch_fixture, build_request,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The selected canary: the prose block's body.
const SECRET: &str = "Kestrel secret canary.";
/// The person the queue and the approvals name.
const PERSON: &str = "human:qa";
/// The bound on a generated sequence.
const MAX_OPS: usize = 128;
const ORIGIN: Location = Location {
    segment: 0,
    offset: 0,
};

/// A small deterministic generator (xorshift64*): the same seed gives the same sequence.
struct Gen(u64);

impl Gen {
    fn new(seed: u64) -> Gen {
        Gen(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn agent(n: usize) -> Session {
    Session::agent(format!("agent:a{n}"), Some(format!("a{n}")), Role::Writer).unwrap()
}

fn relaying_agent() -> Session {
    Session::agent("agent:relay".into(), Some("chat".into()), Role::Writer).unwrap()
}

/// An agent session that claims the approver role, which `Session::agent` never builds.
fn forged_approver() -> Session {
    Session {
        actor: "agent:forger".into(),
        run: Some("f1".into()),
        role: Role::Approver,
        kind: SessionKind::Agent,
    }
}

fn input(targets: Vec<String>) -> RedactInput {
    RedactInput {
        targets,
        reason_class: "secret".into(),
        reason: "a leaked code".into(),
        dry_run: false,
    }
}

/// Whether any segment file of the project at `dir` holds `needle`'s bytes.
fn on_disk(dir: &Path, needle: &str) -> bool {
    let segs = dir.join(".rdy/store/segments");
    std::fs::read_dir(segs).unwrap().any(|e| {
        let bytes = std::fs::read(e.unwrap().path()).unwrap();
        bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
    })
}

/// A project whose canon holds a doc with a prose block holding `SECRET`, a readable sibling and
/// a term, landed by the person as `landing` and tagged v1.0.0.
struct P {
    dir: tempfile::TempDir,
    r: Rdy,
    prose: BlockId,
    sibling: BlockId,
    landing: Hash,
}

fn landed() -> P {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "plan", "title": "Plan", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Part"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": SECRET},
            {"op": "create", "type": "prose", "id": "$b", "parent": "$s", "body": "A readable sibling."},
            {"op": "create", "type": "term", "parent": "$s", "fields": {"name": "Ledger"}, "body": "A ledger."}
        ]),
    )
    .unwrap();
    let landing: Hash = land(&mut r, "work").unwrap().parse().unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    let id = |k: &str| BlockId::parse(&w.created[k]).unwrap();
    P {
        prose: id("$p"),
        sibling: id("$b"),
        landing,
        dir,
        r,
    }
}

/// The canon version of `id`.
fn canon_version(r: &Rdy, id: &BlockId) -> Hash {
    r.read()
        .materialized(&Scope::Canon)
        .unwrap()
        .version_of(id)
        .unwrap()
}

/// The person's approve item of proposal version `r#ref`, if the queue lists it.
fn item(r: &Rdy, r#ref: &str) -> Option<Item> {
    r.read()
        .pending(&PendingQuery {
            person: PERSON.into(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .ok()?
        .items
        .into_iter()
        .find(|i| i.r#ref == r#ref)
}

fn answer(
    r: &mut Rdy,
    s: &Session,
    it: &Item,
    option: &str,
    relay: RelayInput,
) -> Result<(), String> {
    r.answer(
        s,
        PERSON,
        AnswerRequest {
            r#ref: it.r#ref.clone(),
            option: option.into(),
            card_hash: Some(it.card_hash.clone()),
            note: None,
            relay,
            brief: None,
        },
    )
    .map(|_| ())
    .map_err(|e| e.code.to_string())
}

fn relay_yes() -> RelayInput {
    RelayInput {
        evidence: Some("Yes, remove it.".into()),
        ..RelayInput::default()
    }
}

/// One generated operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// An agent records a request in its own view and proposes it.
    Request,
    /// The person rejects a removal proposal on its card.
    Reject,
    /// The person, relayed, snoozes a removal proposal.
    Later,
    /// The requesting agent closes its view, which withdraws its proposal.
    Withdraw,
    /// Another agent includes a request's carrier in its view and proposes that view.
    Include,
    /// The requesting agent undoes its view's newest op.
    Undo,
    /// The requesting agent approves its own removal.
    AgentApprove,
    /// An agent session that claims the approver role approves a removal.
    ForgedApprover,
    /// A relayed bulk answer of the recommended options names a removal.
    Bulk,
    /// A direct store landing names a non-person approver (a policy or agent proof).
    StorePolicy,
}

const KINDS: [Kind; 10] = [
    Kind::Request,
    Kind::Reject,
    Kind::Later,
    Kind::Withdraw,
    Kind::Include,
    Kind::Undo,
    Kind::AgentApprove,
    Kind::ForgedApprover,
    Kind::Bulk,
    Kind::StorePolicy,
];

/// A removal proposal the sequence made: the session that owns its view, the view and the
/// proposal; the carrier of its request.
struct Open {
    owner: Session,
    view: String,
    proposal: ProposalInfo,
    carrier: Hash,
}

/// The content is intact: its bytes on disk, canon reads it, no mask, no epoch move.
fn intact(p: &P, version: &Hash) -> Result<(), &'static str> {
    if !on_disk(p.dir.path(), SECRET) {
        return Err("the payload left the disk");
    }
    let st = p.r.read();
    if st.redaction_epoch() != 0 {
        return Err("the redaction epoch moved");
    }
    if st
        .reader()
        .redaction_of(kinds::BLOCK, version)
        .unwrap()
        .is_some()
    {
        return Err("a mask is active");
    }
    if st.block(&Scope::Canon, &p.prose).unwrap().body != SECRET {
        return Err("canon no longer reads the content");
    }
    Ok(())
}

/// Runs operation `kind` against a proposal `opens` holds (chosen by `g`); the outcome as text
/// (an error code, never a message), and the id it acted on.
fn run_op(
    p: &mut P,
    g: &mut Gen,
    kind: Kind,
    opens: &mut Vec<Open>,
    n: &mut usize,
) -> (String, String) {
    let pick = |g: &mut Gen, opens: &Vec<Open>| (!opens.is_empty()).then(|| g.below(opens.len()));
    let code = |r: Result<(), rdy_engine::RdyError>| match r {
        Ok(()) => "ok".to_string(),
        Err(e) => e.code.to_string(),
    };
    match kind {
        Kind::Request => {
            *n += 1;
            let s = agent(*n);
            let view = s.default_view();
            let res = p.r.redact(&s, input(vec![p.prose.to_string()]));
            let Ok(res) = res else {
                return (code(res.map(|_| ())), view);
            };
            let Some(carrier) = res.change else {
                return ("nothing recorded".into(), view);
            };
            match p
                .r
                .propose(&s, &view, "Remove the code", Some("Removes the code."))
            {
                Ok(proposal) => {
                    let id = proposal.r#ref.clone();
                    opens.push(Open {
                        owner: s,
                        view,
                        proposal,
                        carrier,
                    });
                    ("ok".into(), id)
                }
                Err(e) => (e.code.to_string(), view),
            }
        }
        Kind::Include => {
            let Some(i) = pick(g, opens) else {
                return ("skip".into(), String::new());
            };
            *n += 1;
            let s = agent(*n);
            let view = s.default_view();
            let carrier = opens[i].carrier.to_string();
            let mut out = code(
                p.r.view_create(&s, &view, None, false)
                    .map(|_| ())
                    .and_then(|_| p.r.view_include(&s, &view, &[carrier]).map(|_| ())),
            );
            if out == "ok" {
                match p
                    .r
                    .propose(&s, &view, "Carry it", Some("Carries the removal."))
                {
                    Ok(proposal) => {
                        let carrier = opens[i].carrier;
                        opens.push(Open {
                            owner: s,
                            view: view.clone(),
                            proposal,
                            carrier,
                        });
                    }
                    Err(e) => out = e.code.to_string(),
                }
            }
            (out, view)
        }
        _ => {
            let Some(i) = pick(g, opens) else {
                return ("skip".into(), String::new());
            };
            let o = &opens[i];
            let id = o.proposal.r#ref.clone();
            let out = match kind {
                Kind::Reject | Kind::Later | Kind::Bulk => match item(&p.r, &id) {
                    None => "not queued".into(),
                    Some(it) => match kind {
                        Kind::Reject => {
                            answer(&mut p.r, &approver(), &it, "reject", RelayInput::default())
                                .map_or_else(|c| c, |_| "ok".into())
                        }
                        Kind::Later => {
                            answer(&mut p.r, &relaying_agent(), &it, "later", relay_yes())
                                .map_or_else(|c| c, |_| "ok".into())
                        }
                        _ => code(
                            p.r.answer_all_recommended(
                                &relaying_agent(),
                                PERSON,
                                std::slice::from_ref(&it.r#ref),
                                std::slice::from_ref(&it.card_hash),
                                relay_yes(),
                            )
                            .map(|_| ()),
                        ),
                    },
                },
                Kind::Withdraw => {
                    let (owner, view) = (o.owner.clone(), o.view.clone());
                    code(p.r.view_close(&owner, &[view], "withdrawn").map(|_| ()))
                }
                Kind::Undo => {
                    let (owner, view) = (o.owner.clone(), o.view.clone());
                    code(p.r.undo(&owner, &view, None, false).map(|_| ()))
                }
                Kind::AgentApprove => {
                    let (owner, pid) = (o.owner.clone(), o.proposal.id.clone());
                    code(p.r.land(&owner, &pid).map(|_| ()))
                }
                Kind::ForgedApprover => {
                    let pid = o.proposal.id.clone();
                    code(p.r.land(&forged_approver(), &pid).map(|_| ()))
                }
                Kind::StorePolicy => {
                    let proposal: Hash = o.proposal.id.parse().unwrap();
                    let role = [Role::Writer, Role::Reviewer, Role::Reader][g.below(3)];
                    let res = p.r.store_mut().land_atomic(LandRequest {
                        proposal,
                        accept: None,
                        approver: Provenance {
                            actor: "agent:policy".into(),
                            run: Some("p1".into()),
                            role,
                            at_ms: 1,
                        },
                        alt: None,
                        relay: None,
                        evidence: None,
                    });
                    match res {
                        Ok(_) => {
                            p.r.invalidate();
                            "ok".into()
                        }
                        Err(e) => rdy_engine::RdyError::from(e).code.to_string(),
                    }
                }
                Kind::Request | Kind::Include => unreachable!(),
            };
            (out, id)
        }
    }
}

#[test]
fn rdx15_unapproved_and_policy_requests_never_remove_bytes() {
    // across the seeds: every kind drawn, and the outcomes each kind reached
    let mut outcomes: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for seed in [1u64, 7, 42, 1515] {
        let mut p = landed();
        let version = canon_version(&p.r, &p.prose);
        let mut g = Gen::new(seed);
        let ops = 1 + g.below(MAX_OPS);
        let mut opens: Vec<Open> = vec![];
        let mut n = 0usize;
        let mut trail: Vec<String> = vec![];
        let mut seen: BTreeSet<&'static str> = BTreeSet::new();
        // the first operation is always a request, so every sequence has one to act on
        for i in 0..ops {
            let kind = if i == 0 {
                Kind::Request
            } else {
                KINDS[g.below(KINDS.len())]
            };
            let (out, id) = run_op(&mut p, &mut g, kind, &mut opens, &mut n);
            trail.push(format!("{kind:?} {id} -> {out}"));
            outcomes
                .entry(format!("{kind:?}"))
                .or_default()
                .insert(out.clone());
            let forged = matches!(
                kind,
                Kind::AgentApprove | Kind::ForgedApprover | Kind::Bulk | Kind::StorePolicy
            );
            assert!(
                !(forged && out == "ok"),
                "seed {seed}: op {i} {kind:?} on {id} approved a removal\n{}",
                trail.join("\n")
            );
            if let Err(what) = intact(&p, &version) {
                panic!(
                    "seed {seed}: after op {i} {kind:?} on {id}: {what}\n{}",
                    trail.join("\n")
                );
            }
            if out == "ok" {
                seen.insert(match kind {
                    Kind::Request => "request",
                    Kind::Reject => "reject",
                    Kind::Later => "later",
                    Kind::Withdraw => "withdraw",
                    Kind::Include => "include",
                    Kind::Undo => "undo",
                    _ => "forged",
                });
            }
        }
        assert!(
            seen.contains("request"),
            "seed {seed}: {}",
            trail.join("\n")
        );
        check_all(&p.r).unwrap_or_else(|v| panic!("seed {seed}: {v:#?}"));
    }
    // each kind was drawn; request, reject, later, withdraw, include and undo each ran through
    // at least once; each forged approval was refused, never skipped every time
    for k in KINDS {
        let got = outcomes
            .get(&format!("{k:?}"))
            .unwrap_or_else(|| panic!("{k:?} is never drawn: {outcomes:?}"));
        match k {
            Kind::AgentApprove | Kind::ForgedApprover | Kind::Bulk | Kind::StorePolicy => assert!(
                got.iter().any(|o| o.starts_with("E_")),
                "{k:?} is never refused: {got:?}"
            ),
            _ => assert!(got.contains("ok"), "{k:?} never runs: {got:?}"),
        }
    }
}

/// What must survive every operation: the landing chain, the tags and their landings, the
/// removed version's stub, every mask of the request's spans, the readable sibling, the blocked
/// blob reads and the redaction counts of fsck.
fn logical(r: &Rdy) -> Value {
    let rm = every_kind::removal(r);
    let st = r.read();
    let reader = st.reader();
    let landings: Vec<String> = st
        .landings()
        .unwrap()
        .into_iter()
        .map(|(h, _)| h.to_string())
        .collect();
    let tags: BTreeMap<String, String> = st
        .versions()
        .unwrap()
        .into_iter()
        .map(|v| (v.name, v.landing))
        .collect();
    let masks: Vec<Value> = rm
        .record
        .manifest
        .spans
        .iter()
        .map(|s| {
            json!([
                s.kind,
                s.hash.to_string(),
                reader
                    .redaction_of(s.kind, &s.hash)
                    .unwrap()
                    .map(|h| h.to_string())
            ])
        })
        .collect();
    let mut stubs = BTreeMap::new();
    let mut history = BTreeMap::new();
    for (id, v) in &rm.record.manifest.versions {
        let b = reader.get_block(v).unwrap();
        let ident = b.read_identity.clone().unwrap();
        stubs.insert(
            format!("{id}@{v}"),
            json!({"body": b.body, "type": b.block_type, "parent": b.parent.map(|p| p.to_string()), "order": b.order,
                   "version": ident.version.to_string(), "request": ident.request.to_string()}),
        );
        let h = st.history(&Scope::Canon, id, 50).unwrap();
        history.insert(id.to_string(), serde_json::to_value(&h).unwrap());
    }
    let blobs: Vec<Value> = rm
        .record
        .manifest
        .blobs
        .iter()
        .chain(&rm.record.manifest.shared_blobs)
        .map(|h| {
            json!(
                reader
                    .get_blob(h)
                    .map_err(|e| rdy_engine::RdyError::from(e).code.to_string())
                    .err()
            )
        })
        .collect();
    let f = serde_json::to_value(r.fsck().unwrap()).unwrap();
    json!({
        "request": rm.request.to_string(),
        "apply": rm.apply.to_string(),
        "landings": landings,
        "canon": st.canon_head().map(|h| h.to_string()),
        "tags": tags,
        "masks": masks,
        "stubs": stubs,
        "history": history,
        "blobs": blobs,
        "redacted": [f["redacted_objects"].clone(), f["redacted_frames"].clone(), f["redacted_bytes"].clone()],
    })
}

/// The five operations an approved removal must survive, after the approval itself.
#[derive(Clone, Copy, Debug)]
enum Move {
    Restart,
    Reindex,
    DumpLoad,
    Clone,
    FileClone,
}

#[test]
fn rdx15_approved_removal_survives_restart_reindex_dump_and_clone() {
    // the approved removal: the every-kind project's last source landing is a person's approval
    let src = every_kind::project();
    let r = Rdy::open(src.path()).unwrap();
    let want = logical(&r);
    assert_eq!(want["blobs"], json!(["E_REDACTED", "E_REDACTED"]), "{want}");
    for m in want["masks"].as_array().unwrap() {
        assert_eq!(m[2], want["request"], "every span is masked: {m}");
    }
    r.close().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    for seed in [3u64, 15, 99] {
        let mut g = Gen::new(seed);
        let mut moves = [
            Move::Restart,
            Move::Reindex,
            Move::DumpLoad,
            Move::Clone,
            Move::FileClone,
        ];
        // a seeded order of the five
        for i in (1..moves.len()).rev() {
            moves.swap(i, g.below(i + 1));
        }
        let mut at = src.path().to_path_buf();
        for (k, m) in moves.iter().enumerate() {
            let next = scratch.path().join(format!("s{seed}-{k}"));
            match m {
                Move::Restart => Rdy::open(&at).unwrap().close().unwrap(),
                Move::Reindex => {
                    let mut r = Rdy::open(&at).unwrap();
                    r.reindex().unwrap();
                    r.close().unwrap();
                }
                Move::DumpLoad => {
                    let file = scratch.path().join(format!("s{seed}-{k}.rdydump"));
                    let r = Rdy::open(&at).unwrap();
                    let d = r.read().dump(&file).unwrap();
                    assert_eq!(
                        (d.dump_format, &d.store_format),
                        (DUMP_FORMAT, &DumpStore::Features(baseline_features()))
                    );
                    r.close().unwrap();
                    load(&file, &next).unwrap();
                    at = next;
                }
                Move::Clone => {
                    let r = Rdy::open(&at).unwrap();
                    {
                        let st = r.read();
                        clone(CloneSource::State(&st), &next, false).unwrap();
                    }
                    r.close().unwrap();
                    at = next;
                }
                Move::FileClone => {
                    let file = scratch.path().join(format!("s{seed}-{k}.rdydump"));
                    let r = Rdy::open(&at).unwrap();
                    r.read().dump(&file).unwrap();
                    r.close().unwrap();
                    clone(CloneSource::File(&file), &next, false).unwrap();
                    at = next;
                }
            }
            let r = Rdy::open(&at).unwrap();
            let got = logical(&r);
            assert!(
                got == want,
                "seed {seed}: after {:?} the logical history or a mask changed",
                &moves[..=k]
            );
            for canary in [every_kind::CANARY_BODY, every_kind::CANARY_FIELD] {
                assert!(
                    !on_disk(&at, canary),
                    "seed {seed}: after {:?} a selected canary is on disk",
                    &moves[..=k]
                );
            }
            r.close().unwrap();
        }
        rdy_qa::reindex_check::reindex_reproduces(&at).unwrap_or_else(|v| panic!("{v:#?}"));
    }
}

/// The segment file of a store segment.
fn seg_name(seg: u32) -> String {
    format!("seg-{seg:06}.log")
}

/// Zeroes the payload of every span in the files of the closed store at `store`, as an erasure
/// would.
fn zero_spans(store: &Path, spans: &[DropSpan]) {
    for s in spans {
        let file = store.join("segments").join(seg_name(s.place.segment));
        let mut bytes = std::fs::read(&file).unwrap();
        let start = s.place.offset as usize + 37;
        bytes[start..start + s.len as usize].fill(0);
        std::fs::write(&file, bytes).unwrap();
    }
}

/// Every segment file of the store at `store`, by name.
fn segments(store: &Path) -> BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(store.join("segments"))
        .unwrap()
        .map(|e| {
            let p = e.unwrap().path();
            (
                p.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read(&p).unwrap(),
            )
        })
        .collect()
}

/// A closed project, written through the engine and landed by the person: a doc whose prose
/// block holds `SECRET` beside a readable sibling. Returns its directory and the removal the
/// raw authorization names: the prose block's canon version.
fn engine_project() -> (tempfile::TempDir, RequestSpec) {
    let p = landed();
    let v = canon_version(&p.r, &p.prose);
    let P { dir, r, prose, .. } = p;
    r.close().unwrap();
    let spec = RequestSpec::new(
        "redact",
        vec![RedactTarget::Version {
            id: prose,
            version: v,
        }],
    );
    (dir, spec)
}

/// Options whose hook changes the request's BLOCK span with `f`.
fn on_block_span(f: impl Fn(&mut DropSpan) + 'static) -> FixtureOptions {
    let mut o = FixtureOptions::approved(Proof::local_human());
    o.on_draft = Some(Box::new(move |_, d| {
        let span = d
            .request
            .manifest
            .spans
            .iter_mut()
            .find(|s| s.kind == kinds::BLOCK)
            .unwrap();
        f(span);
    }));
    o
}

#[test]
fn rdx15_missing_content_needs_exact_authorization() {
    let other = Hash::of(b"rdx15 another object");
    // the exact authorization opens: the payload is gone and the version reads as its stub,
    // also after the index is deleted and rebuilt from the log
    let (dir, spec) = engine_project();
    let store = dir.path().join(".rdy/store");
    authorized_batch_fixture(
        &store,
        std::slice::from_ref(&spec),
        &FixtureOptions::approved(Proof::local_human()),
    );
    for pass in ["open", "deleted index"] {
        if pass == "deleted index" {
            std::fs::remove_file(store.join("index.redb")).unwrap();
        }
        let r = Rdy::open(dir.path()).unwrap();
        let RedactTarget::Version { id, version } = &spec.targets[0] else {
            unreachable!()
        };
        let b = r.read().block(&Scope::Canon, id).unwrap();
        assert_eq!(b.body, CONTENT_REMOVED, "{pass}");
        assert!(
            r.store()
                .redaction_of(kinds::BLOCK, version)
                .unwrap()
                .is_some(),
            "{pass}"
        );
        r.close().unwrap();
        assert!(!on_disk(dir.path(), SECRET), "{pass}");
    }

    type Mutate = Box<dyn Fn() -> FixtureOptions>;
    let cases: Vec<(&str, Mutate, bool)> = vec![
        (
            "hash",
            Box::new(move || on_block_span(move |s| s.hash = other)),
            false,
        ),
        (
            "kind",
            Box::new(|| on_block_span(|s| s.kind = kinds::BLOB_CHUNK)),
            false,
        ),
        ("length", Box::new(|| on_block_span(|s| s.len += 1)), false),
        (
            "location",
            Box::new(|| on_block_span(|s| s.place.offset += 1)),
            false,
        ),
        (
            "marker",
            Box::new(|| FixtureOptions::approved(Proof::local_human())),
            true,
        ),
        (
            "request",
            Box::new(move || {
                let mut o = FixtureOptions::approved(Proof::local_human());
                o.on_applies = Some(Box::new(move |applies| {
                    for a in applies {
                        a.request = other;
                    }
                }));
                o
            }),
            false,
        ),
        (
            "residue",
            Box::new(move || on_block_span(move |s| s.residue = Some(other))),
            false,
        ),
    ];
    for (name, opts, damage_marker) in cases {
        let (dir, spec) = engine_project();
        let store = dir.path().join(".rdy/store");
        let spans = {
            let s = Store::open(&store).unwrap();
            build_request(&s, &spec).request.manifest.spans
        };
        let f = authorized_batch_fixture(&store, std::slice::from_ref(&spec), &opts());
        // the payload is absent, as an erasure would leave it; its authorization is not exact
        zero_spans(&store, &spans);
        if damage_marker {
            let start = f.landing_start.unwrap();
            let file = store.join("segments").join(seg_name(start.segment));
            let mut bytes = std::fs::read(&file).unwrap();
            let n = bytes.len();
            bytes[n - 2] ^= 0xff; // inside the landing batch's marker payload
            std::fs::write(&file, bytes).unwrap();
        }
        let before = segments(&store);
        for pass in ["open", "deleted index"] {
            if pass == "deleted index" {
                let _ = std::fs::remove_file(store.join("index.redb"));
            }
            let e = Rdy::open(dir.path())
                .err()
                .unwrap_or_else(|| panic!("{name} ({pass}): the absence is refused"));
            assert_eq!(e.code, E_REDACTION_CORRUPT, "{name} ({pass}): {}", e.code);
            assert!(
                e.message
                    .starts_with("The redaction record does not authorize the missing payload at "),
                "{name} ({pass})"
            );
            assert_eq!(
                e.fix,
                "Keep the store unchanged and inspect it with rdy fsck."
            );
            assert_eq!(segments(&store), before, "{name} ({pass}): no byte changed");
        }
    }
}

#[test]
fn rdx15_snapshots_are_references_and_assessment_support_is_absent() {
    let mut p = landed();
    let reader = p.r.read().reader().clone();
    let rec: LandingRecord = reader.get_typed(kinds::LANDING, &p.landing).unwrap();
    let snap = rec.state;
    let snap_bytes = reader.get_kind(kinds::SNAPSHOT, &snap).unwrap();
    let version = canon_version(&p.r, &p.prose);
    let snapshots_before = count_kind(&p.r, kinds::SNAPSHOT);
    drop(reader);
    // the report names the state snapshot (and its landing and tag) as references it cannot
    // erase, and says assessment support is not implemented
    let dry =
        p.r.read()
            .redaction_report(&RedactInput {
                dry_run: true,
                ..input(vec![p.prose.to_string()])
            })
            .unwrap();
    let has = |kind: ImpactKind, record: &str| {
        dry.inside.iter().any(|i| {
            i.kind == kind
                && i.record == record
                && i.relation == ImpactRelation::References
                && !i.erasable
        })
    };
    assert!(
        has(ImpactKind::StateSnapshot, &snap.to_string()),
        "{:?}",
        dry.inside
    );
    assert!(has(ImpactKind::Landing, &p.landing.to_string()));
    assert!(has(ImpactKind::Tag, "v1.0.0"));
    assert_eq!(dry.assessment, AssessmentSupport::Carried);
    let j = serde_json::to_value(&dry).unwrap();
    assert_eq!(j["assessment"], "carried");
    assert!(
        !j.to_string().contains("score"),
        "the report holds no score: {j}"
    );
    // the person approves the removal
    let s = agent(1);
    let res = p.r.redact(&s, input(vec![p.prose.to_string()])).unwrap();
    let prop =
        p.r.propose(&s, &s.default_view(), "Remove", Some("Removes the code."))
            .unwrap();
    p.r.land(&approver(), &prop.id).unwrap();
    let st = p.r.read();
    let reader = st.reader();
    assert_eq!(
        reader.redaction_of(kinds::BLOCK, &version).unwrap(),
        res.redaction,
        "the removal is active"
    );
    // the old state snapshot is the same bytes and still names the original version, not a stub
    let after = reader.get_kind(kinds::SNAPSHOT, &snap).unwrap();
    assert_eq!(after, snap_bytes, "the old snapshot's bytes are fixed");
    assert_eq!(Hash::of(&after), snap);
    let s1: StateSnapshot = reader.get_typed(kinds::SNAPSHOT, &snap).unwrap();
    assert_eq!(s1.blocks.get(&p.prose), Some(&version));
    let again: LandingRecord = reader.get_typed(kinds::LANDING, &p.landing).unwrap();
    assert_eq!(again.state, snap, "the landing names the same snapshot");
    assert_eq!(
        st.versions().unwrap()[0].landing,
        p.landing.to_string(),
        "the tag names the same landing"
    );
    // the request records the report-only references and the carried-record scan status
    let req = reader.redaction_request(&res.redaction.unwrap()).unwrap();
    assert_eq!(req.manifest.assessment, AssessmentSupport::Carried);
    assert!(
        req.manifest
            .inside
            .iter()
            .any(|i| i.kind == ImpactKind::StateSnapshot && i.record == snap.to_string())
    );
    // the removal's landing names the same state (its carrier has no op): no snapshot is
    // rewritten or added, and no record of a kind beyond the registry is written (no
    // assessment record)
    let head = p.r.read().canon_head().unwrap();
    let removal: LandingRecord = p.r.store().get_typed(kinds::LANDING, &head).unwrap();
    assert_eq!(
        removal.state, snap,
        "the canon state still references the original version"
    );
    assert_eq!(count_kind(&p.r, kinds::SNAPSHOT), snapshots_before);
    // D-4JMPH: every propose batch writes a plan version, and an item-ful landing writes a
    // sealed one, so the removal's
    // propose and landing write PLAN (kind 24), a registry kind; still no assessment record
    let known: BTreeSet<u8> = (1..=22)
        .filter(|k| *k != 11 && *k != 12)
        .chain([kinds::PLAN])
        .collect();
    let mut seen: BTreeSet<u8> = BTreeSet::new();
    let st = p.r.read();
    st.reader()
        .for_each_record_from(ORIGIN, st.seq(), |_, f, _| {
            // by logical kind: a frame of any encoding (`rdy_store::codec`)
            seen.insert(rdy_store::codec::logical_kind(match f {
                StoredFrame::Present { kind, .. } | StoredFrame::Dropped { kind, .. } => kind,
            }));
            Ok(())
        })
        .unwrap();
    seen.remove(&kinds::BATCH_COMMIT);
    assert!(seen.is_subset(&known), "{seen:?}");
    drop(st);
    check_all(&p.r).unwrap_or_else(|v| panic!("{v:#?}"));
    let _ = p.sibling;
}

/// How many frames of `kind` the log holds.
fn count_kind(r: &Rdy, kind: u8) -> usize {
    let st = r.read();
    let mut n = 0;
    st.reader()
        .for_each_record_from(ORIGIN, st.seq(), |_, f, _| {
            let k = match f {
                StoredFrame::Present { kind, .. } | StoredFrame::Dropped { kind, .. } => kind,
            };
            n += usize::from(k == kind);
            Ok(())
        })
        .unwrap();
    n
}

/// A pack whose decisions track `disposition` (trk1).
const TRK_PACK: &str = "\
name: trk
version: \"1\"
extends: rdy-standard
block_types:
  decision:
    prefix: D
    traits: [normative]
    fields:
      disposition: { kind: str }
    tracking: [disposition]
";

#[test]
fn rdx15_a_removed_frozen_decision_is_covered_not_compared() {
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    std::fs::write(dir.path().join(".rdy/packs/trk.yaml"), TRK_PACK).unwrap();
    std::fs::write(dir.path().join(".rdy.yaml"), "pack: trk\n").unwrap();
    let mut r = Rdy::open(dir.path()).unwrap();
    let (_, section, _) = rdy_qa::fixtures::seed_doc(&mut r, "seed");
    let d = r
        .decide(
            &writer(),
            "seed",
            DecisionInput {
                parent: section,
                statement: "Ship weekly".into(),
                fields: [("disposition".to_string(), json!("M2d1"))]
                    .into_iter()
                    .collect(),
                ..Default::default()
            },
        )
        .unwrap();
    let dec = BlockId::parse(&d.created["$decision"]).unwrap();
    land(&mut r, "seed").unwrap();
    let v1 = canon_version(&r, &dec);
    // a tracking edit lands a second version: the rule compares it with the first
    r.write(
        &writer(),
        WriteRequest {
            view: "reg".into(),
            ops: serde_json::from_value(
                json!([{"op": "update", "id": dec.to_string(), "fields": {"disposition": "M2d2"}}]),
            )
            .unwrap(),
            justification: Some(rdy_model::Justification::Editorial {
                class: rdy_model::EditorialClass::Clarify,
                reason: "re-sliced".into(),
            }),
            message: String::new(),
        },
    )
    .unwrap();
    land(&mut r, "reg").unwrap();
    let v2 = canon_version(&r, &dec);
    let before = frozen_report(&r);
    assert!(
        before.violations.is_empty() && before.unavailable.is_empty(),
        "{before:?}"
    );
    // the person removes the first frozen version
    let s = agent(1);
    let res = r.redact(&s, input(vec![format!("{dec}@{v1}")])).unwrap();
    let p = r
        .propose(
            &s,
            &s.default_view(),
            "Remove",
            Some("Removes the first ruling."),
        )
        .unwrap();
    r.land(&approver(), &p.id).unwrap();
    let request = res.redaction.unwrap();
    // the stub answers with its recorded frozen fact; the comparison needs its erased fields, so
    // it is unavailable, and the removal's exact coverage was verified instead
    let stub = r.store().get_block(&v1).unwrap();
    assert!(stub.is_redacted() && rdy_model::decision::is_frozen(&stub));
    let after = frozen_report(&r);
    assert!(after.violations.is_empty(), "{after:?}");
    assert_eq!(
        after.unavailable,
        vec![rdy_qa::invariants::Unavailable {
            block: dec.clone(),
            first: v1,
            later: v2,
            requests: vec![request],
        }]
    );
    check(&r, &Scope::Canon).unwrap_or_else(|v| panic!("{v:#?}"));
    // an unremoved frozen change still fails: a canon landing whose state holds a rewritten
    // ruling, forged with raw writes (no engine or store writer lands one), is compared with the
    // version that reads in full
    let head = r.read().canon_head().unwrap();
    let rec: LandingRecord = r.store().get_typed(kinds::LANDING, &head).unwrap();
    let mut snap: StateSnapshot = r.store().get_typed(kinds::SNAPSHOT, &rec.state).unwrap();
    let mut ruling = r.store().get_block(&v2).unwrap();
    ruling.fields.insert(
        "statement".into(),
        rdy_model::FieldValue::Str("Ship daily".into()),
    );
    let block = postcard::to_allocvec(&ruling).unwrap();
    snap.blocks.insert(dec.clone(), Hash::of(&block));
    let snap_bytes = postcard::to_allocvec(&snap).unwrap();
    let forged = LandingRecord {
        prev: Some(head),
        state: Hash::of(&snap_bytes),
        changes: vec![],
        read_identity: None,
        ..rec
    };
    let landing = postcard::to_allocvec(&forged).unwrap();
    let forged_id = Hash::of(&landing);
    r.store_mut()
        .write(vec![
            (kinds::BLOCK, block),
            (kinds::SNAPSHOT, snap_bytes),
            (kinds::LANDING, landing),
        ])
        .unwrap();
    r.store_mut()
        .set_refs(vec![("canon".into(), forged_id.0.to_vec())])
        .unwrap();
    r.invalidate();
    let raw = frozen_report(&r);
    assert!(
        raw.violations
            .iter()
            .any(|v| v.rule == "frozen" && v.block.as_ref() == Some(&dec)),
        "{raw:?}"
    );
    let e = check(&r, &Scope::Canon).unwrap_err();
    assert!(e.iter().any(|v| v.rule == "frozen"), "{e:#?}");
}

/// A pack whose section may hold only sections and terms (no prose).
const TERMS_ONLY_PACK: &str = "\
name: terms
version: \"1\"
extends: rdy-standard
block_types:
  section:
    prefix: s
    traits: [structural]
    fields:
      title: { kind: str, required: true }
    children: [section, term]
";

/// Fix round 1, finding 1: an authorized stub keeps the schema's structure rules. Two prose
/// blocks under a section are removed with a person's approval; the project then switches to a
/// pack whose section may hold only terms. The read schema refuses each stub's parent type, and
/// so does the checker, though the removal itself is exactly authorized.
#[test]
fn rdx15_a_removed_block_keeps_its_parent_type_rule() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "plan", "title": "Plan", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Part"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": SECRET},
            {"op": "create", "type": "prose", "id": "$q", "parent": "$s", "body": "A second paragraph."},
            {"op": "create", "type": "term", "parent": "$s", "fields": {"name": "Ledger"}, "body": "A ledger."}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let id = |k: &str| BlockId::parse(&w.created[k]).unwrap();
    let removed = [id("$p"), id("$q")];
    let s = agent(1);
    r.redact(&s, input(removed.iter().map(BlockId::to_string).collect()))
        .unwrap();
    let p = r
        .propose(
            &s,
            &s.default_view(),
            "Remove",
            Some("Removes two paragraphs."),
        )
        .unwrap();
    r.land(&approver(), &p.id).unwrap();
    check(&r, &Scope::Canon).unwrap_or_else(|v| panic!("{v:#?}"));
    r.close().unwrap();
    std::fs::write(dir.path().join(".rdy/packs/terms.yaml"), TERMS_ONLY_PACK).unwrap();
    std::fs::write(dir.path().join(".rdy.yaml"), "pack: terms\n").unwrap();
    let r = Rdy::open(dir.path()).unwrap();
    let st = r.read();
    let m = st.materialized(&Scope::Canon).unwrap();
    for b in &removed {
        let stub = m.get(b).unwrap();
        assert!(stub.is_redacted(), "{b} reads as its stub");
        let parent = stub.parent.as_ref().and_then(|p| m.get(p));
        let codes: Vec<&str> = rdy_schema::validate_read_block(st.pack(), stub, parent)
            .iter()
            .map(|i| i.code)
            .collect();
        assert!(codes.contains(&"E_CHILD_TYPE"), "{b}: {codes:?}");
    }
    drop(st);
    let e = check(&r, &Scope::Canon).expect_err("a stub under a parent that cannot hold it");
    for b in &removed {
        assert!(
            e.iter().any(|v| v.rule == "schema"
                && v.block.as_ref() == Some(b)
                && v.detail.starts_with("E_CHILD_TYPE")),
            "{b}: {e:#?}"
        );
    }
    // the stubs' absence is still exactly authorized: only the parent type is refused
    assert!(!e.iter().any(|v| v.rule == "redaction"), "{e:#?}");
}
