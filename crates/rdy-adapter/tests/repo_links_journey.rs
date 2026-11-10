//! rpl36 (repo links; D-DAPR8, D-8VJAZ, D-MEMMQ, D-0DNKT, D-CN4QJ): the whole repo-link journey
//! through the adapter, against two real project stores.
//!
//! The cited project is a real one: it lands pages, a decision to deviate from, a decision to
//! adopt, and a rule that a later release locks; it tags each release, exports its `site` profile
//! as a link delivery stream and, last, removes the text of a page with a person's approval. The
//! citing project takes each delivery through the store's one writer, cites the delivered content,
//! answers the adopt cards as the person, records a deviation, adopts an upstream decision, has
//! the link revoked and takes the removal. Every foreign act leaves canon where it was: only the
//! person's approved local acts move it. Each step's facts come from one captured snapshot, and
//! they hold again after a restart of the store, a reindex, a dump and load, and a follower that
//! follows the project's increments.
//!
//! Every project and every file lives under a temporary directory, and the person layer of the
//! instruction test comes from a meta project under its own temporary directory.
use rdy_adapter::foreign_facts::{
    AdoptedDelivery, CitedRepoRefs, ForeignSignals, LinkAdoption, LinkState, UnadoptedLock,
};
use rdy_adapter::{
    BlockId, DeliverySource, Follower, ForeignSet, Hash, LinkSource, RdyError, ReadRequest, RepoId,
    Snapshot, StoreHandle, maintenance,
};
use rdy_engine::answer::AnswerRequest;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::instructions::{
    self, BOUNDS, Caller, Contribution, Layer, LoadRequest, PersonHost, Reply,
};
use rdy_engine::instructions_config::{PersonPath, PersonSource};
use rdy_engine::pending::{CARD_MAX_TOKENS, Item, ItemKind, PendingQuery, PlanCardBinding};
use rdy_engine::plan::api::{ItemState, PlanAnswerInput, PlanRow};
use rdy_engine::plan::compute::allowed_acts;
use rdy_engine::readiness::{Candidate, assess};
use rdy_engine::redaction::RedactInput;
use rdy_engine::tensions::TensionKind;
use rdy_engine::{DecisionInput, Rdy, Session, WriteRequest, WriteResult};
use rdy_model::Role;
use rdy_model::plan::{MAX_PLAN_ANSWERS, PlanAct, PlanKind};
use rdy_store::foreign::SourceView;
use rdy_store::{Store, StoreError, kinds};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs::File;
use std::path::{Path, PathBuf};

const AT_MS: i64 = 1_760_000_000_000;
const SUMMARY: &str = "test summary";
const PERSON: &str = "human:founder";
/// The text of the page the cited project removes.
const LEAKED: &str = "A leaked code 1234";

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn writer() -> Session {
    Session::human(PERSON.into(), Role::Writer).unwrap()
}

fn founder() -> Session {
    Session::human(PERSON.into(), Role::Approver).unwrap()
}

fn put(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: rdy_engine::parse_ops(ops.as_array().unwrap().clone()).unwrap(),
            justification: None,
            message: format!("write to {view}"),
        },
    )
    .unwrap_or_else(|e| panic!("write to {view}: {e:?}"))
}

fn bid(w: &WriteResult, name: &str) -> BlockId {
    id(&w.created[name])
}

/// Answers each open tension item of view `view` with `confirm`, and each open item on stable
/// content by its first allowed act, as the person.
fn answer_open_items(r: &mut Rdy, view: &str) {
    let Ok(reply) = r.read().view_plan(view) else {
        return;
    };
    let inputs: Vec<PlanAnswerInput> = reply
        .items
        .iter()
        .filter_map(|row| {
            let PlanRow::Live(i) = row else { return None };
            if i.state != ItemState::Open {
                return None;
            }
            let act = match i.kind {
                PlanKind::Tension => PlanAct::Confirm,
                PlanKind::UpdateImplementation => return None,
                kind if i.stable => *allowed_acts(kind).first()?,
                _ => return None,
            };
            Some(PlanAnswerInput {
                item: i.id.clone(),
                act,
                reason: Some("test: checked".into()),
            })
        })
        .collect();
    for chunk in inputs.chunks(MAX_PLAN_ANSWERS) {
        r.answer_plan(&writer(), view, chunk).unwrap();
    }
}

/// Proposes view `view` with its open items answered.
fn propose(r: &mut Rdy, view: &str) -> (Hash, rdy_engine::ProposalInfo) {
    answer_open_items(r, view);
    let p = r
        .propose(&writer(), view, view, Some(SUMMARY))
        .unwrap_or_else(|e| panic!("propose {view}: {e} {:?}", e.issues));
    (p.id.parse().unwrap(), p)
}

/// Confirms each exception beside version `p`, then lands it as the person.
fn land_version(r: &mut Rdy, p: &rdy_engine::ProposalInfo) {
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
        r.answer(
            &founder(),
            PERSON,
            AnswerRequest {
                r#ref: item,
                option: "confirm".into(),
                ..AnswerRequest::default()
            },
        )
        .unwrap();
    }
    r.land(&founder(), &p.id)
        .unwrap_or_else(|e| panic!("land {}: {e:?}", p.id));
}

fn land(r: &mut Rdy, view: &str) {
    let (_, p) = propose(r, view);
    land_version(r, &p);
}

fn refused(e: RdyError, code: &str, message: &str, fix: &str) {
    assert_eq!(
        (e.code, &e.message[..], &e.fix[..]),
        (code, message, fix),
        "{e:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// the cited project

/// The cited project: its directory, its repo, the ids it delivers and the profiles it keeps.
struct Source {
    dir: tempfile::TempDir,
    repo: RepoId,
    /// The page the citing project cites, then sees changed, then sees removed.
    ap: BlockId,
    /// The decision the citing project adopts as its own.
    up: BlockId,
    /// The decision the citing project deviates from.
    dv: BlockId,
    /// The rule that release 1.2.0 locks.
    lk: BlockId,
    /// The `site` profile: the pages, both decisions and the rules.
    site: BlockId,
    /// The doc that holds the profiles.
    admin: BlockId,
}

/// v1.0.0: doc `a` holds the page `ap` and the decisions `up` and `dv`; the doc `rules` is empty.
/// The `site` profile selects both docs. v1.1.0 changes `ap`. v1.2.0 adds the locked rule `lk`
/// to `rules`. v1.3.0 changes `ap` again.
fn source() -> Source {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = put(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$a",
             "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$ap", "parent": "$a",
             "body": format!("{LEAKED}.")},
            {"op": "create", "type": "doc", "id": "$rules",
             "fields": {"name": "rules", "title": "Rules", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin",
             "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let (a, rules, admin) = (bid(&w, "$a"), bid(&w, "$rules"), bid(&w, "$admin"));
    let d = put(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "decision", "id": "$up", "parent": a.to_string(),
             "fields": {"statement": "Every job logs in UTC.", "status": "proposed"}},
            {"op": "create", "type": "decision", "id": "$dv", "parent": a.to_string(),
             "fields": {"statement": "Every job retries three times.", "status": "proposed"}},
            {"op": "create", "type": "publication_profile", "id": "$site",
             "parent": admin.to_string(),
             "fields": {"key": "site", "title": "Site", "audience": "org",
                        "select": [a.to_string(), rules.to_string()]}}
        ]),
    );
    land(&mut r, "work");
    r.tag(&founder(), "v1.0.0", None, None).unwrap();
    put(
        &mut r,
        "work",
        json!([{"op": "update", "id": w.created["$ap"],
                "body": format!("{LEAKED}, later.")}]),
    );
    land(&mut r, "work");
    r.tag(&founder(), "v1.1.0", None, None).unwrap();
    let reason = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent: rules.to_string(),
                statement: "Every claim names its owner.".into(),
                title: Some("Owners".into()),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    let l = put(
        &mut r,
        "work",
        json!([{"op": "create", "type": "instruction", "id": "$lk", "parent": rules.to_string(),
                "fields": {"applies_to": ["all"], "locked": true},
                "body": "Every claim names its owner.",
                "links": {"decided_by": [{"target": reason}]}}]),
    );
    land(&mut r, "work");
    r.tag(&founder(), "v1.2.0", None, None).unwrap();
    put(
        &mut r,
        "work",
        json!([{"op": "update", "id": w.created["$ap"],
                "body": format!("{LEAKED}, once more.")}]),
    );
    land(&mut r, "work");
    r.tag(&founder(), "v1.3.0", None, None).unwrap();
    r.close().unwrap();
    let handle = StoreHandle::open(dir.path()).unwrap();
    let repo = handle.snapshot().unwrap().identity().repo_id;
    handle.close().unwrap();
    Source {
        dir,
        repo,
        ap: bid(&w, "$ap"),
        up: bid(&d, "$up"),
        dv: bid(&d, "$dv"),
        lk: bid(&l, "$lk"),
        site: bid(&d, "$site"),
        admin,
    }
}

impl Source {
    /// The `site` delivery at `tag`, written to `files/name`.
    fn delivery(&self, files: &Path, tag: &str, name: &str) -> File {
        self.export(files, tag, "site", name)
    }

    /// The delivery of `profile` at `tag`, written to `files/name`.
    fn export(&self, files: &Path, tag: &str, profile: &str, name: &str) -> File {
        let handle = StoreHandle::open(self.dir.path()).unwrap();
        let snap = handle.snapshot().unwrap();
        let mut out = Vec::new();
        snap.link_export(tag, profile, &mut out)
            .unwrap_or_else(|e| panic!("export {profile} at {tag}: {e:?}"));
        drop(snap);
        handle.close().unwrap();
        let path = files.join(name);
        std::fs::write(&path, out).unwrap();
        File::open(path).unwrap()
    }

    /// Removes the text of `ap` (every version of it) with a person's approval.
    fn remove(&self) {
        let mut r = Rdy::open(self.dir.path()).unwrap();
        let s = writer();
        r.redact_in(
            &s,
            "redact",
            RedactInput {
                targets: vec![self.ap.to_string()],
                reason_class: "secret".into(),
                reason: "a leaked code".into(),
                dry_run: false,
            },
        )
        .unwrap();
        let p = r.propose(&s, "redact", "Remove", Some(SUMMARY)).unwrap();
        r.land(&founder(), &p.id).unwrap();
        r.close().unwrap();
    }

    /// The complete history of v1.0.0, written to `files/name`: after a removal it carries the
    /// removal's portable proof.
    fn history(&self, files: &Path, name: &str) -> File {
        let handle = StoreHandle::open(self.dir.path()).unwrap();
        let snap = handle.snapshot().unwrap();
        let mut out = Vec::new();
        snap.write_stream("v1.0.0", &BTreeSet::new(), &mut out)
            .unwrap();
        drop(snap);
        handle.close().unwrap();
        let path = files.join(name);
        std::fs::write(&path, out).unwrap();
        File::open(path).unwrap()
    }
}

// ---------------------------------------------------------------------------------------------
// the citing project

/// What one snapshot holds of the cited repo and of this project's side of the link. Every part
/// is a read of one captured snapshot.
#[derive(Debug, PartialEq)]
struct Facts {
    canon: Option<Hash>,
    refs: Vec<CitedRepoRefs>,
    view: Option<SourceView>,
    bytes: u64,
    adoption: Vec<LinkAdoption>,
    unadopted: Vec<UnadoptedLock>,
    signals: ForeignSignals,
    /// The read of the cited page and of the cited decision, as `rdy_read` gives them.
    page: Result<Value, String>,
    decision: Result<Value, String>,
}

fn facts_of(snap: &Snapshot, link: &BlockId, s: &Source) -> Facts {
    let read = |what: &BlockId| {
        snap.read(ReadRequest {
            id: format!("core:{what}"),
            scope: Some("canon".into()),
            format: Some("yaml".into()),
            ..ReadRequest::default()
        })
        .map_err(|e| format!("{}: {}", e.code, e.message))
    };
    Facts {
        canon: snap.canon(),
        refs: snap.foreign_refs().unwrap(),
        view: snap.foreign_source_for_tests(link).unwrap(),
        bytes: snap.foreign_bytes(&s.repo).unwrap(),
        adoption: snap.link_adoption().unwrap(),
        unadopted: snap.unadopted_locked().unwrap(),
        signals: snap.foreign_signals().unwrap(),
        page: read(&s.ap),
        decision: read(&s.up),
    }
}

/// A citing project, driven step by step. The engine and the store handle cannot hold the
/// directory together, so each helper opens what it needs and closes it again.
struct Citing {
    dir: tempfile::TempDir,
    files: tempfile::TempDir,
    section: BlockId,
    link: BlockId,
    /// The local decision that the project later replaces by its upstream copy.
    old: BlockId,
    /// The claim that cites the page.
    claim: Option<BlockId>,
    at_ms: i64,
}

impl Citing {
    /// A project with a doc, a section, the repo link `core` to `s`'s `site` profile and the
    /// local accepted decision `old`, all landed.
    fn new(s: &Source) -> Citing {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
        let w = put(
            &mut r,
            "seed",
            json!([
                {"op": "create", "type": "doc", "id": "$d",
                 "fields": {"name": "d", "title": "Guide", "doc_type": "overview"}},
                {"op": "create", "type": "section", "id": "$s", "parent": "$d",
                 "fields": {"title": "Rules"}},
                {"op": "create", "type": "repo_link", "id": "$l", "parent": "$d",
                 "fields": {"alias": "core", "repo": s.repo.to_string(),
                            "profile": s.site.to_string()}}
            ]),
        );
        land(&mut r, "seed");
        let section = bid(&w, "$s");
        let old = r
            .decide(
                &writer(),
                "old",
                DecisionInput {
                    parent: section.to_string(),
                    statement: "Every job logs in UTC.".into(),
                    title: Some("Log time".into()),
                    ..Default::default()
                },
            )
            .unwrap()
            .created["$decision"]
            .clone();
        land(&mut r, "old");
        r.close().unwrap();
        Citing {
            dir,
            files: tempfile::tempdir().unwrap(),
            section,
            link: bid(&w, "$l"),
            old: id(&old),
            claim: None,
            at_ms: AT_MS,
        }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    /// Runs `f` on the engine, closing it again.
    fn rdy<T>(&self, f: impl FnOnce(&mut Rdy) -> T) -> T {
        let mut r = Rdy::open(self.root()).unwrap();
        let out = f(&mut r);
        r.close().unwrap();
        out
    }

    /// Runs `f` on the store handle, closing it again.
    fn handle<T>(&self, f: impl FnOnce(&StoreHandle) -> T) -> T {
        let h = StoreHandle::open(self.root()).unwrap();
        let out = f(&h);
        h.close().unwrap();
        out
    }

    fn facts(&self, s: &Source) -> Facts {
        self.handle(|h| facts_of(&h.snapshot().unwrap(), &self.link, s))
    }

    fn canon(&self) -> Option<Hash> {
        self.handle(|h| h.snapshot().unwrap().canon())
    }

    fn source_of(&mut self, set: Option<ForeignSet>) -> DeliverySource {
        self.at_ms += 1;
        DeliverySource {
            link: self.link.clone(),
            site: "hub.example".into(),
            at_ms: self.at_ms,
            set,
        }
    }

    /// Takes `s`'s `site` delivery at `tag` through the store's one writer.
    fn deliver(
        &mut self,
        s: &Source,
        tag: &str,
        set: Option<ForeignSet>,
    ) -> rdy_adapter::DeliveryTaken {
        let file = s.delivery(self.files.path(), tag, &format!("delivery-{tag}"));
        let source = self.source_of(set);
        self.handle(|h| {
            h.take_delivery(&file, source)
                .unwrap_or_else(|e| panic!("take {tag}: {e:?}"))
        })
    }

    fn revoke(&mut self) -> rdy_adapter::LinkRevoked {
        self.at_ms += 1;
        let source = LinkSource {
            link: self.link.clone(),
            site: "hub.example".into(),
            at_ms: self.at_ms,
        };
        self.handle(|h| h.revoke_link(source).unwrap())
    }

    /// The open adopt card of the link.
    fn card(&self) -> Item {
        self.card_of(&self.link)
    }

    /// The open adopt card of `link`.
    fn card_of(&self, link: &BlockId) -> Item {
        self.rdy(|r| {
            let q = r
                .read()
                .pending(&PendingQuery {
                    person: PERSON.into(),
                    session_ref: None,
                    max_tokens: CARD_MAX_TOKENS,
                })
                .unwrap();
            let mut all: Vec<Item> = q
                .items
                .into_iter()
                .filter(|i| i.kind == ItemKind::Adopt)
                .filter(|i| {
                    matches!(&i.plan_binding,
                        Some(PlanCardBinding::Adopt { link: l, .. }) if l == link)
                })
                .collect();
            assert_eq!(all.len(), 1, "{all:#?}");
            all.remove(0)
        })
    }

    /// The person answers the open adopt card with `option`.
    fn answer(&self, option: &str) -> Result<(), RdyError> {
        self.answer_of(&self.link, option)
    }

    /// The person answers the open adopt card of `link` with `option`.
    fn answer_of(&self, link: &BlockId, option: &str) -> Result<(), RdyError> {
        let card = self.card_of(link);
        self.rdy(|r| {
            r.answer(
                &founder(),
                PERSON,
                AnswerRequest {
                    r#ref: card.r#ref.clone(),
                    option: option.into(),
                    card_hash: Some(card.card_hash.clone()),
                    note: (option == "keep").then(|| "Billing stays on the first text.".into()),
                    relay: RelayInput::default(),
                    ..AnswerRequest::default()
                },
            )
            .map(|_| ())
        })
    }

    /// Cites the page `ap` in a landed claim.
    fn cite(&mut self, s: &Source) {
        let section = self.section.to_string();
        let w = self.rdy(|r| {
            let w = put(
                r,
                "cite",
                json!([{"op": "create", "type": "claim", "id": "$c", "parent": section,
                        "body": format!("Follow {{{{core:{}}}}}.", s.ap)}]),
            );
            land(r, "cite");
            w
        });
        self.claim = Some(bid(&w, "$c"));
    }

    /// Records, proposes and lands a decision that deviates from `target`.
    fn deviate(&self, target: &str, statement: &str) -> Result<BlockId, RdyError> {
        self.rdy(|r| {
            let d = r.decide(
                &writer(),
                "dev",
                DecisionInput {
                    parent: self.section.to_string(),
                    statement: statement.into(),
                    title: Some("Depart".into()),
                    deviates_from: vec![target.into()],
                    ..Default::default()
                },
            )?;
            land(r, "dev");
            Ok(id(&d.created["$decision"]))
        })
    }
}

fn read_text(snap: &Snapshot, what: &str) -> String {
    let v = snap
        .read(ReadRequest {
            id: what.into(),
            scope: Some("canon".into()),
            format: Some("yaml".into()),
            ..ReadRequest::default()
        })
        .unwrap_or_else(|e| panic!("read {what}: {e:?}"));
    v["text"].as_str().unwrap().to_string()
}

fn store_dir(project: &Path) -> PathBuf {
    project.join(".rdy").join(rdy_store::layout::STORE_DIR)
}

/// Opens the store at `dir` directly (no handle holds it) and tells whether the BLOCK `v` reads
/// as removed.
fn block_removed(dir: &Path, v: Hash) -> bool {
    let s = Store::open(&store_dir(dir)).unwrap();
    let removed = matches!(
        s.reader().get_kind_raw(kinds::BLOCK, &v),
        Err(StoreError::Redacted { .. })
    );
    s.close().unwrap();
    removed
}

/// Whether any segment file of the store at `project` holds `needle`'s bytes.
fn on_disk(project: &Path, needle: &str) -> bool {
    std::fs::read_dir(store_dir(project).join("segments"))
        .unwrap()
        .any(|e| {
            let bytes = std::fs::read(e.unwrap().path()).unwrap();
            bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
        })
}

fn org() -> Option<ForeignSet> {
    Some(ForeignSet::Organization)
}

/// The versions of `ap` that v1.0.0 and v1.1.0 hold, read from the citing project's own refs
/// right after each is delivered.
fn held_of(c: &Citing, s: &Source) -> (Option<Hash>, Option<Hash>) {
    let refs = c.handle(|h| h.snapshot().unwrap().foreign_refs().unwrap());
    let row = refs
        .iter()
        .flat_map(|r| r.refs.iter())
        .find(|r| r.target.id == s.ap)
        .expect("a citation of the page");
    (row.held, row.current)
}

/// An open proposal that cites the page, and so pins its first delivered version.
fn open_premise(c: &Citing, s: &Source) -> Hash {
    let section = c.section.to_string();
    c.rdy(|r| {
        put(
            r,
            "open",
            json!([{"op": "create", "type": "claim", "id": "$y", "parent": section,
                    "body": format!("Plan on {{{{core:{}}}}}.", s.ap)}]),
        );
        propose(r, "open").0
    })
}

/// The engine's own foreign gate facts and readiness for the proposal `version`, read from the
/// engine on the same log the adapter's snapshot reads.
fn engine_gate(
    c: &Citing,
    version: &Hash,
) -> (Vec<rdy_adapter::foreign_facts::ForeignGateFact>, String) {
    c.rdy(|r| {
        let st = r.read();
        let pref = r.resolve_proposal(&version.to_string()).unwrap();
        let rec = st.reader().proposal(version).unwrap();
        let cand = Candidate::stored(&st, pref, rec.clone()).unwrap();
        let assessed = assess(&st, &cand, &st.gate_config()).unwrap();
        let rows: Vec<_> = st
            .tensions_of_changes(&rec.changes)
            .unwrap()
            .into_iter()
            .map(|t| {
                let status = st.tension_status(&t);
                (t, status)
            })
            .collect();
        (
            rdy_engine::foreign_facts::foreign_tension_gate(&rows, &Default::default()),
            format!("{:?}", assessed.readiness),
        )
    })
}

// ---------------------------------------------------------------------------------------------

/// What the journey leaves for a caller to check afterwards.
struct Ends {
    /// The versions of the page the three deliveries hold.
    pages: [Hash; 3],
}

/// The journey: a first delivery, a citation, a changed source, a keep, a new lock, an adoption,
/// a deviation, an upstream adoption, a revocation and a removal. Canon moves only with the
/// person's approved local acts. `after` runs when each step is done, with the step's name.
fn journey(s: &Source, c: &mut Citing, after: &mut dyn FnMut(&Citing, &str)) -> Ends {
    let (s, c) = (s, &mut *c);
    let mut canon = c.canon();
    assert!(canon.is_some());
    let moved = |c: &Citing, canon: &mut Option<Hash>, what: &str| {
        let now = c.canon();
        assert_ne!(now, *canon, "{what} lands: canon moves");
        *canon = now;
    };
    let stays = |c: &Citing, canon: &Option<Hash>, what: &str| {
        assert_eq!(&c.canon(), canon, "{what} is no local act: canon stays");
    };

    // before any delivery: one link, no state
    let rows = c.handle(|h| h.snapshot().unwrap().link_adoption().unwrap());
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].state, &rows[0].last, &rows[0].adopted),
        (LinkState::None, &None, &None)
    );

    // 1. the first delivery: the hub's act, no local one
    let first = c.deliver(s, "v1.0.0", None);
    assert!(!first.unchanged && first.added > 0, "{first:?}");
    stays(&c, &canon, "a delivery");
    let again = c.deliver(s, "v1.0.0", None);
    assert!(again.unchanged, "the same selection writes nothing");
    stays(&c, &canon, "a repeated delivery");
    let rows = c.handle(|h| h.snapshot().unwrap().link_adoption().unwrap());
    assert_eq!(rows[0].state, LinkState::Delivered);
    assert_eq!(rows[0].last.as_ref().map(|l| l.entry), Some(first.entry));
    assert_eq!(rows[0].adopted, None, "a delivery alone adopts nothing");
    assert!(c.facts(s).bytes > 0);
    after(c, "first delivery");

    // 2. the citation: the person lands a claim that pins the delivered page
    c.cite(&s);
    moved(&c, &mut canon, "the citation");
    let claim = c.claim.clone().unwrap();
    let (held, current) = held_of(&c, &s);
    assert!(held.is_some() && held == current, "pinned at the delivery");
    let v1 = held.unwrap();
    let refs = c.handle(|h| h.snapshot().unwrap().foreign_refs().unwrap());
    assert_eq!(refs.len(), 1);
    assert_eq!(
        (refs[0].repo, refs[0].aliases.clone()),
        (s.repo, vec!["core".into()])
    );
    assert_eq!(refs[0].refs.len(), 1);
    assert_eq!(refs[0].refs[0].citer, claim);
    assert_eq!(refs[0].refs[0].entry, Some(first.entry));
    after(c, "citation");

    // an open proposal pins the same page: the next delivery shifts its premise
    let open = open_premise(&c, &s);
    stays(&c, &canon, "an open proposal");
    let (gate, _) = engine_gate(&c, &open);
    assert_eq!(gate, vec![], "nothing shifted yet");

    // 3. a changed source: v1.1.0 changes the page. The pin stays; a suspect and an adopt card
    // appear; canon stays.
    let second = c.deliver(s, "v1.1.0", None);
    assert!(!second.unchanged && second.changed == 1, "{second:?}");
    stays(&c, &canon, "a delivery that changes a cited page");
    let (held, current) = held_of(&c, &s);
    assert_eq!(held, Some(v1), "the pin stays behind");
    assert!(current.is_some() && current != held);
    let v2 = current.unwrap();
    let signals = c.handle(|h| h.snapshot().unwrap().foreign_signals().unwrap());
    let suspect: Vec<_> = signals
        .suspects
        .iter()
        .filter(|x| x.target.id == s.ap)
        .collect();
    assert_eq!(suspect.len(), 1, "{signals:#?}");
    assert_eq!(
        (
            suspect[0].block.clone(),
            suspect[0].reason.as_str(),
            suspect[0].entry
        ),
        (claim.clone(), "changed", Some(second.entry))
    );
    let card = c.card();
    assert_eq!(
        card.options
            .iter()
            .map(|o| o.id.as_str())
            .collect::<Vec<_>>(),
        ["adopt", "keep", "later"]
    );
    assert!(card.r#ref.starts_with("adopt:"), "{}", card.r#ref);
    after(c, "changed source");

    // the adapter's facts for the open proposal are the engine's own, from the same log, and
    // the delivery shifted its premise
    let (gate, readiness) = engine_gate(&c, &open);
    let seen = c.handle(|h| h.snapshot().unwrap().foreign_tension_gate(&open).unwrap());
    assert_eq!(seen, gate, "{readiness}");
    assert_eq!(seen.len(), 1, "{seen:#?} {readiness}");
    assert_eq!(seen[0].kind, TensionKind::PremiseShift);
    assert_eq!((seen[0].repo, seen[0].entry), (s.repo, second.entry));
    // readiness holds the same tension as one open plan item, named by the tension's id
    let item = format!("tn-{}", &seen[0].tension.to_string()[..12]);
    assert!(readiness.contains(&item), "{item} in {readiness}");
    assert!(readiness.contains("E_PLAN_OPEN"), "{readiness}");

    // 4. keep: the reason is recorded, the pin is retained and canon stays
    c.answer("keep").unwrap();
    stays(&c, &canon, "a keep answer");
    assert_eq!(held_of(&c, &s), (Some(v1), Some(v2)));
    let rows = c.handle(|h| h.snapshot().unwrap().link_adoption().unwrap());
    assert_eq!(rows[0].adopted, None, "a keep answer adopts nothing");
    after(c, "keep");

    // 5. a new lock: v1.2.0 adds a locked rule of the organization's set. It is delivered, not in
    // force: the unadopted lock names it and nothing moves.
    let third = c.deliver(s, "v1.2.0", org());
    assert!(!third.unchanged && third.added >= 1, "{third:?}");
    stays(&c, &canon, "a delivery that locks a rule");
    let unadopted = c.handle(|h| h.snapshot().unwrap().unadopted_locked().unwrap());
    assert_eq!(unadopted.len(), 1, "{unadopted:#?}");
    assert_eq!(
        (unadopted[0].entry, unadopted[0].at_ms),
        (third.entry, c.at_ms)
    );
    after(c, "new lock");

    // 6. a deviation: the person lands a decision that departs from the cited decision `dv`. It
    // moves canon, and the citation list shows the departure as a link of the deviating decision.
    let dv_ref = format!("core:{}", s.dv);
    let dev = c
        .deviate(&dv_ref, "Billing retries five times.")
        .unwrap_or_else(|e| panic!("deviate: {e:?}"));
    moved(&c, &mut canon, "the deviation");
    let refs = c.handle(|h| h.snapshot().unwrap().foreign_refs().unwrap());
    let departs: Vec<_> = refs[0].refs.iter().filter(|r| r.citer == dev).collect();
    assert_eq!(departs.len(), 1, "{refs:#?}");
    assert_eq!(departs[0].target.id, s.dv);
    assert_eq!(departs[0].links, ["deviates_from"]);
    // a decision that departs from a block no delivery holds is refused, and writes nothing
    let e = c
        .deviate(
            &format!("core:{}", s.up).replace(&s.up.to_string(), "D-NONEX"),
            "Nothing.",
        )
        .unwrap_err();
    assert_eq!(e.code, "E_UNKNOWN_REF", "{e:?}");
    stays(&c, &canon, "a refused deviation");
    after(c, "deviation");

    // 7. adoption: the person adopts the delivery of v1.2.0. The pin moves to the page's current
    // version, the organization's lock has force, and the unadopted lock is gone.
    c.answer("adopt").unwrap();
    moved(&c, &mut canon, "the adoption");
    // the delivery added a rule and changed no cited page: the page's pin stays behind
    assert_eq!(held_of(&c, &s), (Some(v1), Some(v2)));
    let rows = c.handle(|h| h.snapshot().unwrap().link_adoption().unwrap());
    assert_eq!(
        rows[0].adopted,
        Some(AdoptedDelivery {
            entry: third.entry,
            at_ms: c.at_ms,
            set: org()
        })
    );
    assert_eq!(
        rows[0].last.as_ref().map(|l| (l.entry, l.instructions)),
        Some((third.entry, 1))
    );
    assert!(
        c.handle(|h| h.snapshot().unwrap().unadopted_locked().unwrap())
            .is_empty()
    );
    // the adoption took the one adopt item: none stands for the link now
    let q = c.rdy(|r| {
        r.read()
            .pending(&PendingQuery {
                person: PERSON.into(),
                session_ref: None,
                max_tokens: CARD_MAX_TOKENS,
            })
            .unwrap()
    });
    assert!(q.items.iter().all(|i| i.kind != ItemKind::Adopt), "{q:#?}");
    // a deviation from the adopted lock is refused, and writes nothing
    let e = c
        .deviate(&format!("core:{}", s.lk), "We skip the owner rule.")
        .unwrap_err();
    refused(
        e,
        "E_INSTRUCTION_LOCKED",
        &format!("core:{} is locked by its organization set", s.lk),
        "add an instruction on top of it, or ask the set's owners to change it there: rdy_write a new instruction / rdy new instruction",
    );
    stays(&c, &canon, "a refused deviation from a lock");
    after(c, "adoption");

    // a later delivery changes the page again: its adopt answer moves the pin to the page's
    // current version, in one landing
    let fourth = c.deliver(s, "v1.3.0", org());
    assert!(!fourth.unchanged && fourth.changed == 1, "{fourth:?}");
    stays(&c, &canon, "a delivery that changes a cited page again");
    let (held, current) = held_of(&c, &s);
    assert_eq!(held, Some(v1));
    let v3 = current.unwrap();
    assert!(v3 != v1 && v3 != v2);
    c.answer("adopt").unwrap();
    moved(&c, &mut canon, "the second adoption");
    assert_eq!(held_of(&c, &s), (Some(v3), Some(v3)), "the pin moved");
    let rows = c.handle(|h| h.snapshot().unwrap().link_adoption().unwrap());
    assert_eq!(
        rows[0].adopted.as_ref().map(|a| a.entry),
        Some(fourth.entry)
    );
    after(c, "second adoption");

    // 8. an upstream adoption: the local decision `old` is replaced by the cited decision
    let up_ref = format!("core:{}", s.up);
    let old = c.old.to_string();
    c.rdy(|r| {
        r.supersede_adopted_as(&writer(), "upstream", &old, &up_ref, None, None)
            .unwrap_or_else(|e| panic!("adopt upstream: {e:?}"));
        land(r, "upstream");
    });
    moved(&c, &mut canon, "the upstream adoption");
    let status = c.rdy(|r| {
        format!(
            "{:?}",
            r.read()
                .graph(&rdy_engine::Scope::Canon)
                .unwrap()
                .status(&c.old)
        )
    });
    assert!(status.contains("Superseded"), "{status}");
    after(c, "upstream adoption");

    // 9. a revocation: the hub's act. The link's last view stays readable, a later delivery is
    // refused, and canon stays.
    let before = c.facts(&s);
    let revoked = c.revoke();
    assert!(!revoked.unchanged);
    stays(&c, &canon, "a revocation");
    assert!(c.revoke().unchanged, "a revoked source writes nothing more");
    let gone = c.facts(&s);
    assert!(gone.view.as_ref().unwrap().revoked);
    // the pins and the held versions stay; a revoked source offers no current selection
    let held = |f: &Facts| -> Vec<_> {
        f.refs
            .iter()
            .flat_map(|r| r.refs.iter())
            .map(|r| {
                (
                    r.citer.clone(),
                    r.target.clone(),
                    r.pin.clone(),
                    r.held,
                    r.entry,
                )
            })
            .collect()
    };
    assert_eq!(held(&gone), held(&before));
    assert!(before.refs[0].refs.iter().all(|r| r.current.is_some()));
    assert!(gone.refs[0].refs.iter().all(|r| r.current.is_none()));
    // a held page stays readable at its pinned version; it reads as no longer delivered
    let (was, is) = (
        before.page.as_ref().unwrap()["text"].as_str().unwrap(),
        gone.page.as_ref().unwrap()["text"].as_str().unwrap(),
    );
    assert!(
        was.contains("in v1.3.0") && !was.contains("no longer delivered"),
        "{was}"
    );
    assert!(is.contains("delivered: no longer delivered"), "{is}");
    assert!(is.contains(&format!("body: {LEAKED}, once more.")), "{is}");
    let rows = &gone.adoption;
    assert_eq!(rows[0].state, LinkState::Revoked);
    assert_eq!(rows[0].adopted, before.adoption[0].adopted);
    let file = s.delivery(c.files.path(), "v1.3.0", "late");
    let source = c.source_of(org());
    let e = c.handle(|h| h.take_delivery(&file, source).unwrap_err());
    refused(
        e,
        "E_BAD_INPUT",
        "the repo link is revoked",
        "use a live repo link",
    );
    stays(&c, &canon, "a refused delivery");
    after(c, "revocation");

    // 10. a removal: the cited project removes the page's text. The history carries the proof,
    // and the take erases every delivered copy and nothing local.
    s.remove();
    let history = s.history(c.files.path(), "history");
    assert!(on_disk(c.root(), LEAKED));
    let taken = c.handle(|h| h.take_foreign_removal(&history, s.repo).unwrap());
    assert!(!taken.unchanged);
    assert_eq!(taken.removals.len(), 1, "{taken:?}");
    let mut applied = taken.removals[0].applied.clone();
    applied.sort();
    let mut want = vec![(s.ap.clone(), v1), (s.ap.clone(), v2), (s.ap.clone(), v3)];
    want.sort();
    assert_eq!(applied, want);
    stays(&c, &canon, "a removal");
    assert!([v1, v2, v3].iter().all(|v| block_removed(c.root(), *v)));
    assert!(
        !on_disk(c.root(), LEAKED),
        "no byte of the removed text stays"
    );
    let end = c.facts(&s);
    // the count keeps the removal entry and the residue of the erased copies
    assert!(end.bytes > 0);
    assert_eq!(
        end.refs[0]
            .refs
            .iter()
            .find(|r| r.target.id == s.ap)
            .unwrap()
            .held,
        Some(v3)
    );
    assert!(!format!("{:?}", end.page).contains(LEAKED));
    c.handle(|h| h.fsck().unwrap());
    // the removal proof again writes nothing
    let again = c.handle(|h| h.take_foreign_removal(&history, s.repo).unwrap());
    assert!(again.unchanged);
    stays(&c, &canon, "a repeated removal");
    after(c, "removal");
    Ends {
        pages: [v1, v2, v3],
    }
}

/// The journey through the adapter, with nothing more to check than its own steps.
#[test]
fn rpl36_deliver_cite_deviate_adopt_revoke_remove() {
    let s = source();
    let mut c = Citing::new(&s);
    journey(&s, &mut c, &mut |_, _| {});
}

/// What the project itself holds of its own history: the landed claim that cites, the replaced
/// decision and the reads of the local deviation, each as `rdy_read` gives them in canon.
fn local_reads(snap: &Snapshot, c: &Citing) -> Vec<String> {
    [c.claim.clone().unwrap(), c.old.clone()]
        .iter()
        .map(|b| read_text(snap, &b.to_string()))
        .collect()
}

/// The journey's facts, step by step, hold again after the store is reopened, reindexed, dumped
/// and loaded, and followed. The follower is seeded before the first delivery and follows one
/// increment per step; it holds after each what the project held at that step. The clone loaded
/// from the final dump holds the final facts, keeps the revocation and keeps the erasure.
#[test]
fn rpl36_local_and_foreign_histories_survive_roundtrip() {
    let s = source();
    let mut c = Citing::new(&s);
    let files = c.files.path().to_path_buf();
    let d0 = c.handle(|h| h.dump(&files.join("d0")).unwrap());
    let fdir = files.join("follower");
    maintenance::seed_follower(&files.join("d0"), &fdir).unwrap();
    let mut last = d0.chain_end();
    let mut steps: Vec<(String, Facts, PathBuf)> = vec![];
    let ends = journey(&s, &mut c, &mut |c, name| {
        let out = files.join(format!("inc-{}", steps.len() + 1));
        let facts = c.facts(&s);
        last = c.handle(|h| h.dump_after(&last, &out).unwrap().chain_end());
        steps.push((name.to_string(), facts, out));
    });
    assert_eq!(steps.len(), 11, "one increment per step");
    let want = &steps.last().unwrap().1;
    let local = c.handle(|h| local_reads(&h.snapshot().unwrap(), &c));
    assert_eq!(local.len(), 2);
    // the claim stores the cited form `{{<repo-id>:<block-id>}}`
    assert!(
        local[0].contains(&format!("{{{{{}:{}}}}}", s.repo, s.ap)),
        "{}",
        local[0]
    );

    // the store reopened: a fresh handle holds the same
    assert_eq!(&c.facts(&s), want, "after a restart");
    assert_eq!(c.handle(|h| local_reads(&h.snapshot().unwrap(), &c)), local);

    // a reindex replays the log and holds the same
    c.rdy(|r| r.reindex().unwrap());
    assert_eq!(&c.facts(&s), want, "after a reindex");
    assert_eq!(c.handle(|h| local_reads(&h.snapshot().unwrap(), &c)), local);
    c.handle(|h| h.fsck().unwrap());

    // the follower, one increment per step
    let mut f = Follower::open(&fdir).unwrap();
    {
        let snap = f.snapshot().unwrap();
        assert_eq!(snap.foreign_bytes(&s.repo).unwrap(), 0);
        assert_eq!(snap.foreign_source_for_tests(&c.link).unwrap(), None);
    }
    for (name, facts, increment) in &steps {
        f.follow(increment).unwrap();
        let snap = f.snapshot().unwrap();
        assert_eq!(
            &facts_of(&snap, &c.link, &s),
            facts,
            "the follower after {name}"
        );
    }
    {
        let snap = f.snapshot().unwrap();
        assert_eq!(
            [c.claim.clone().unwrap(), c.old.clone()]
                .iter()
                .map(|b| read_text(&snap, &b.to_string()))
                .collect::<Vec<_>>(),
            local,
            "the follower holds the local history"
        );
    }
    drop(f);

    // dump and load: the clone holds the final facts
    let full = files.join("full");
    c.handle(|h| h.dump(&full).unwrap());
    let clone = files.join("clone");
    maintenance::load(&full, &clone).unwrap();
    let h = StoreHandle::open(&clone).unwrap();
    {
        let snap = h.snapshot().unwrap();
        assert_eq!(&facts_of(&snap, &c.link, &s), want, "after dump and load");
        assert_eq!(
            [c.claim.clone().unwrap(), c.old.clone()]
                .iter()
                .map(|b| read_text(&snap, &b.to_string()))
                .collect::<Vec<_>>(),
            local
        );
    }
    // the revocation still refuses a delivery in the clone
    let late = s.delivery(&files, "v1.3.0", "clone-late");
    let source = DeliverySource {
        link: c.link.clone(),
        site: "hub.example".into(),
        at_ms: c.at_ms + 10,
        set: org(),
    };
    refused(
        h.take_delivery(&late, source).unwrap_err(),
        "E_BAD_INPUT",
        "the repo link is revoked",
        "use a live repo link",
    );
    h.fsck().unwrap();
    h.close().unwrap();
    // the erasure survives in the clone, and no byte of the text comes back
    for v in ends.pages {
        assert!(block_removed(&clone, v), "{v}");
    }
    assert!(!on_disk(&clone, LEAKED));
    assert!(!on_disk(c.root(), LEAKED));
}

/// A person host over a meta project at its own path: the person layer reads that project and
/// nothing under any home directory.
struct Host {
    path: PersonPath,
}

impl PersonHost for Host {
    fn locate(&self) -> Result<PersonSource, RdyError> {
        Ok(PersonSource {
            path: self.path.clone(),
            origin: None,
            warnings: vec![],
            config_file: None,
        })
    }

    fn fetch(
        &self,
        meta: &Path,
        load: &LoadRequest,
        _caller: &Caller,
    ) -> Result<Contribution, RdyError> {
        let r = Rdy::open(meta)?;
        let part = instructions::collect(&r.read(), Layer::Person, load);
        r.close()?;
        part
    }
}

/// A landed ruleset `name` with one instruction `body` (selector `all`), its reason a decision
/// that the same view records. Returns the instruction's id.
fn local_rule(r: &mut Rdy, name: &str, body: &str) -> BlockId {
    let w = put(
        r,
        name,
        json!([
            {"op": "create", "type": "doc", "id": "$d",
             "fields": {"name": name, "title": name, "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d",
             "fields": {"title": "Reasons"}}
        ]),
    );
    let d = r
        .decide(
            &writer(),
            name,
            DecisionInput {
                parent: w.created["$s"].clone(),
                statement: format!("{name} is decided"),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    let w = put(
        r,
        name,
        json!([{"op": "create", "type": "instruction", "id": "$i", "parent": w.created["$d"],
                "fields": {"applies_to": ["all"]}, "body": body,
                "links": {"decided_by": [{"target": d}]}}]),
    );
    land(r, name);
    bid(&w, "$i")
}

fn caller() -> Caller {
    Caller {
        actor: "agent".into(),
        run: None,
    }
}

fn layers(reply: &Reply) -> Vec<String> {
    reply
        .instructions
        .iter()
        .map(|i| i.layer.to_string())
        .collect()
}

/// Two profiles of one cited project under two aliases, delivered to two teams, and the person's
/// own layer. Each alias resolves against its own profile: the block that only `core`'s profile
/// delivers is no citation for `ops`. Each team's rule is adopted into its own layer; the layers
/// run rdy, the teams, the project and the person; a locked team rule cannot be departed from.
/// The two teams' rules say the same thing, and the project's signals say so.
#[test]
fn rpl36_two_profiles_two_teams_and_person_layer() {
    let s = source();
    // a second ruleset under a second profile: `ops` delivers it and no page
    let rules2 = {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        let w = put(
            &mut r,
            "work",
            json!([{"op": "create", "type": "doc", "id": "$r2",
                    "fields": {"name": "rules2", "title": "Rules 2", "doc_type": "page"}}]),
        );
        let rules2 = bid(&w, "$r2");
        let reason = r
            .decide(
                &writer(),
                "work",
                DecisionInput {
                    parent: rules2.to_string(),
                    statement: "Owners are named once.".into(),
                    title: Some("Owners again".into()),
                    ..Default::default()
                },
            )
            .unwrap()
            .created["$decision"]
            .clone();
        let l = put(
            &mut r,
            "work",
            json!([
                {"op": "create", "type": "instruction", "id": "$lk2", "parent": rules2.to_string(),
                 "fields": {"applies_to": ["all"], "locked": true},
                 "body": "  every claim NAMES its owner. ",
                 "links": {"decided_by": [{"target": reason}]}},
                {"op": "create", "type": "publication_profile", "id": "$ops",
                 "parent": s.admin.to_string(),
                 "fields": {"key": "ops", "title": "Ops", "audience": "org",
                            "select": [rules2.to_string()]}}
            ]),
        );
        land(&mut r, "work");
        r.tag(&founder(), "v2.0.0", None, None).unwrap();
        r.close().unwrap();
        (bid(&l, "$lk2"), bid(&l, "$ops"))
    };
    let (lk2, ops_profile) = rules2;

    // the citing project: two links to the one repo, a project rule beside them
    let dir = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = put(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d",
             "fields": {"name": "d", "title": "Guide", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d",
             "fields": {"title": "Rules"}},
            {"op": "create", "type": "repo_link", "id": "$core", "parent": "$d",
             "fields": {"alias": "core", "repo": s.repo.to_string(), "profile": s.site.to_string()}},
            {"op": "create", "type": "repo_link", "id": "$ops", "parent": "$d",
             "fields": {"alias": "ops", "repo": s.repo.to_string(), "profile": ops_profile.to_string()}}
        ]),
    );
    land(&mut r, "seed");
    let (section, core, ops) = (bid(&w, "$s"), bid(&w, "$core"), bid(&w, "$ops"));
    local_rule(&mut r, "project-rules", "Project rule.");
    r.close().unwrap();
    let c = Citing {
        dir,
        files,
        section: section.clone(),
        link: core.clone(),
        old: section.clone(),
        claim: None,
        at_ms: AT_MS,
    };

    // each link takes its own profile's delivery, for its own team
    let docs = Some(ForeignSet::Team("docs".into()));
    let ops_team = Some(ForeignSet::Team("ops".into()));
    let take = |link: &BlockId, profile: &str, name: &str, at: i64, set: Option<ForeignSet>| {
        let file = s.export(c.files.path(), "v2.0.0", profile, name);
        c.handle(|h| {
            h.take_delivery(
                &file,
                DeliverySource {
                    link: link.clone(),
                    site: "hub.example".into(),
                    at_ms: AT_MS + at,
                    set,
                },
            )
            .unwrap_or_else(|e| panic!("take {name}: {e:?}"))
        })
    };
    let core_taken = take(&core, "site", "core", 1, docs.clone());
    let ops_taken = take(&ops, "ops", "ops", 2, ops_team.clone());
    assert_ne!(
        core_taken.selection, ops_taken.selection,
        "two profiles, two selections"
    );
    assert_ne!(core_taken.entry, ops_taken.entry);
    assert!(
        core_taken.added > ops_taken.added,
        "the site profile delivers more"
    );

    // each link's selection is its own: the page is delivered through `core` only
    let cite_via = |alias: &str, what: &BlockId| {
        c.rdy(|r| {
            r.write(
                &writer(),
                WriteRequest {
                    view: format!("cite-{alias}"),
                    ops: rdy_engine::parse_ops(
                        json!([{"op": "create", "type": "claim", "parent": section.to_string(),
                                "body": format!("Follow {{{{{alias}:{what}}}}}.")}])
                        .as_array()
                        .unwrap()
                        .clone(),
                    )
                    .unwrap(),
                    justification: None,
                    message: format!("cite via {alias}"),
                },
            )
            .map(|_| ())
        })
    };
    let canon = c.canon();
    refused(
        cite_via("ops", &s.ap).unwrap_err(),
        "E_UNKNOWN_REF",
        &format!("ops:{} is not in the content delivered from ops", s.ap),
        "cite a block that ops delivers: rdy_links {op: \"show\", alias} / rdy links show ops",
    );
    cite_via("core", &s.ap).unwrap();
    assert_eq!(c.canon(), canon, "a write in a view moves no canon");

    // before any adopt answer the rules are delivered and in no layer
    let reply_of = |c: &Citing, meta: &Path| -> Reply {
        let host = Host {
            path: PersonPath::Override(meta.to_path_buf()),
        };
        c.rdy(|r| {
            instructions::deliver_with(
                Some((&r.read(), r.root())),
                &LoadRequest::default(),
                &caller(),
                Some(&host),
                &BOUNDS,
            )
            .unwrap_or_else(|e| panic!("deliver: {e:?}"))
        })
    };
    // the person's meta project: its own temporary path, no home directory
    let meta_dir = tempfile::tempdir().unwrap();
    let meta = meta_dir.path().join("meta");
    let mut mr = Rdy::init(&meta, "rdy-standard").unwrap();
    local_rule(&mut mr, "person-rules", "Person rule.");
    mr.close().unwrap();
    let before = layers(&reply_of(&c, &meta));
    assert!(
        before.iter().all(|l| !l.starts_with("team:")),
        "an unadopted delivery is in no layer: {before:?}"
    );

    // a deviation from the team's rule is allowed while its lock has no force
    let dv = c
        .deviate(&format!("core:{}", s.lk), "We skip the owner rule for now.")
        .unwrap_or_else(|e| panic!("deviate before adoption: {e:?}"));
    // the adopt answer would lock a rule a live deviation departs from: refused, nothing written
    let at_risk = c.canon();
    refused(
        c.answer_of(&core, "adopt").unwrap_err(),
        "E_INSTRUCTION_LOCKED",
        &format!("core:{} is locked by its team docs set", s.lk),
        &format!("revoke the deviation {dv} first, then answer the adopt item again"),
    );
    assert_eq!(c.canon(), at_risk, "a refused adoption writes nothing");
    // the person revokes the deviation, then adopts both deliveries
    c.rdy(|r| {
        r.revoke(
            &writer(),
            "undo",
            &dv.to_string(),
            "The owner rule applies here too.",
            &section.to_string(),
            None,
        )
        .unwrap_or_else(|e| panic!("revoke the deviation: {e:?}"));
        land(r, "undo");
    });
    c.answer_of(&core, "adopt").unwrap();
    c.answer_of(&ops, "adopt").unwrap();

    // the layers: rdy, the teams (sorted by name, one rank), the project, then the person
    let reply = reply_of(&c, &meta);
    let seen = layers(&reply);
    let first = |l: &str| {
        seen.iter()
            .position(|x| x == l)
            .unwrap_or_else(|| panic!("{l} in {seen:?}"))
    };
    let order = [
        first("rdy"),
        first("team:docs"),
        first("team:ops"),
        first("project"),
        first("person"),
    ];
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{seen:?}");
    assert_eq!(
        Layer::Team("docs".into()).precedence(),
        Layer::Team("ops".into()).precedence(),
        "team names sort the display and share one rank"
    );
    let row = |i: &BlockId| {
        reply
            .instructions
            .iter()
            .find(|r| r.id == i.to_string())
            .unwrap_or_else(|| panic!("{i} in {:?}", ids_of(&reply)))
            .clone()
    };
    let (docs_rule, ops_rule) = (row(&s.lk), row(&lk2));
    assert_eq!(docs_rule.layer, Layer::Team("docs".into()));
    assert_eq!(ops_rule.layer, Layer::Team("ops".into()));
    assert!(docs_rule.locked && docs_rule.layer.locks_have_force());
    assert!(ops_rule.locked && ops_rule.layer.locks_have_force());
    assert_eq!(docs_rule.text, "Every claim names its owner.");

    // the adoption left each link adopted at its own delivery, for its own team
    let rows = c.handle(|h| h.snapshot().unwrap().link_adoption().unwrap());
    let by: std::collections::BTreeMap<_, _> =
        rows.iter().map(|r| (r.alias.clone(), r.clone())).collect();
    assert_eq!(
        by["core"]
            .adopted
            .as_ref()
            .map(|a| (a.entry, a.set.clone())),
        Some((core_taken.entry, docs.clone()))
    );
    assert_eq!(
        by["ops"].adopted.as_ref().map(|a| (a.entry, a.set.clone())),
        Some((ops_taken.entry, ops_team.clone()))
    );
    assert!(
        by.values()
            .all(|r| r.repo == s.repo && r.state == LinkState::Delivered)
    );
    let refs = c.handle(|h| h.snapshot().unwrap().foreign_refs().unwrap());
    assert_eq!(refs.len(), 1, "one cited repo");
    assert_eq!(refs[0].aliases, ["core", "ops"], "two aliases of one repo");
    let baselines: Vec<(&BlockId, &BlockId)> = refs[0]
        .refs
        .iter()
        .filter(|r| r.baseline)
        .map(|r| (&r.citer, &r.target.id))
        .collect();
    assert!(
        baselines.contains(&(&core, &s.lk)) && baselines.contains(&(&ops, &lk2)),
        "{baselines:?}"
    );

    // a locked team rule cannot be departed from, in either team
    for (alias, rule, team) in [("core", &s.lk, "docs"), ("ops", &lk2, "ops")] {
        let canon = c.canon();
        refused(
            c.deviate(&format!("{alias}:{rule}"), "We skip it.")
                .unwrap_err(),
            "E_INSTRUCTION_LOCKED",
            &format!("{alias}:{rule} is locked by its team {team} set"),
            "add an instruction on top of it, or ask the set's owners to change it there: rdy_write a new instruction / rdy new instruction",
        );
        assert_eq!(c.canon(), canon);
    }

    // two team sets that say the same thing signal a duplicate definition
    let signals = c.handle(|h| h.snapshot().unwrap().foreign_signals().unwrap());
    let kinds: Vec<TensionKind> = signals.tensions.iter().map(|t| t.kind).collect();
    assert!(
        kinds.contains(&TensionKind::DuplicateDefinition),
        "{signals:#?}"
    );
    assert!(signals.tensions.iter().all(|t| t.repo == s.repo));
}

fn ids_of(reply: &Reply) -> Vec<&str> {
    reply.instructions.iter().map(|i| i.id.as_str()).collect()
}
