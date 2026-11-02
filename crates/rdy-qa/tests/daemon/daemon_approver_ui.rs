//! ui9 (D-6M0AF, D-WPE4R, D-N507H, D-X61DT, D-TF78H, D-Q8NFG, D-QVH4N, D-94BT2, D-JQD5X; TASK-979,
//! TASK-980, TASK-982 I1, O2, O3, O7, O9, O11, C5): the local approval pages, over a real socket.
//! A page reads the engine's complete queue and shows the same items and hashes. A form answers
//! through the answer engine as the configured person, `local`, evidence `card`, with no relay,
//! and every engine refusal keeps its code. A refused card comes back fresh. A listener with no UI
//! route answers the paths as ordinary paths. Nothing here enters a site output.
use rdy_daemon::Shared;
use rdy_daemon::site::ensure_profile_site;
use rdy_daemon::site::guard::{Refusal, refusal};
use rdy_daemon::site::http::{CSP, UI_CSP};
use rdy_engine::check::api::{ReviewRequest, run_review};
use rdy_engine::engine::{Engine, RenderRequest, with_writer_result};
use rdy_engine::pending::{CARD_MAX_TOKENS, Item, ItemKind, Pending, PendingQuery};
use rdy_engine::redaction::RedactInput;
use rdy_engine::semantic::api::run_review_submit;
use rdy_engine::semantic::submit::{CitationRequest, ReviewSubmitRequest};
use rdy_engine::site_scope::percent_encode;
use rdy_engine::{ProposalInfo, Rdy, RdyError, Scope, Session, WriteRequest};
use rdy_model::{
    Answer, AnswerAct, ApprovalEvidence, Assurance, BlockId, EvidenceKind, Hash, Justification,
    LandingRecord, Role,
};
use rdy_qa::daemon::{Hosted, short_tempdir};
use rdy_qa::edit_loop::{brief_of, spec_doc, write_cited};
use rdy_qa::fixtures::{approver, land, parse_test_ops, seed_doc, write, writer};
use rdy_qa::golden::tiny;
use rdy_qa::http::{self, UiForm};
use rdy_qa::intake::{PERSON, TEST_SUMMARY, answer_open_items, batch_project};
use rdy_qa::packs::QA_PACK_YAML;
use rdy_qa::swarm::project::imported_tree;
use rdy_qa::tensions::{ag, created, j_typo, put_j, upd};
use serde_json::json;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const HTML: &str = "text/html; charset=utf-8";
const PLAIN: &str = "text/plain; charset=utf-8";

// --- the site ------------------------------------------------------------------------------------

/// A daemon over a project, with its configured person, and a client of its loopback listener.
pub(crate) struct Site {
    pub h: Hosted,
    pub sh: Arc<Shared>,
    pub port: u16,
}

/// `.rdy/local.yaml` of the project at `dir`: `text`.
pub(crate) fn local(dir: &Path, text: &str) {
    std::fs::write(dir.join(".rdy/local.yaml"), text).expect("local config");
}

fn actor(name: &str) -> String {
    format!("actor: {name}\n")
}

pub(crate) fn port_of(url: &str) -> u16 {
    url.strip_prefix("http://127.0.0.1:")
        .unwrap()
        .trim_end_matches('/')
        .parse()
        .unwrap()
}

impl Site {
    /// The daemon of the project at `dir` (its store closed), with `config` as its local config.
    pub(crate) fn start(dir: tempfile::TempDir, config: &str) -> Site {
        local(dir.path(), config);
        let h = Hosted::start_in(dir);
        let sh = Arc::clone(h.run.as_ref().unwrap().shared());
        let port = port_of(sh.site_info.get().unwrap().url.as_ref().unwrap());
        Site { h, sh, port }
    }

    pub(crate) fn addr(&self) -> SocketAddr {
        SocketAddr::from((Ipv4Addr::LOCALHOST, self.port))
    }

    pub(crate) fn host(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    pub(crate) fn send(&self, request: &str) -> http::Response {
        http::raw(self.addr(), request)
    }

    pub(crate) fn get(&self, path: &str) -> http::Response {
        self.send(&format!(
            "GET {path} HTTP/1.1\r\nHost: {}\r\n\r\n",
            self.host()
        ))
    }

    fn head(&self, path: &str) -> http::Response {
        self.send(&format!(
            "HEAD {path} HTTP/1.1\r\nHost: {}\r\n\r\n",
            self.host()
        ))
    }

    /// The form of item `r` on its item page: the page, and what its form submits.
    fn form(&self, r: &str) -> (http::Response, UiForm) {
        let page = self.get(&item_path(r));
        assert_eq!(page.status, 200, "{r}: {}", page.head);
        let forms = http::ui_forms(&body(&page));
        assert_eq!(forms.len(), 1, "{r}: one form");
        (page, forms[0].clone())
    }

    fn answer(&self, form: &UiForm, option: &str) -> http::Response {
        self.send(&http::ui_answer(&self.host(), form, option))
    }

    fn engine(&self) -> Arc<dyn Engine> {
        self.sh.engine()
    }

    fn on<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Rdy) -> Result<T, RdyError> + Send + 'static,
    ) -> T {
        with_writer_result(&*self.engine(), f).expect("the write")
    }

    fn queue(&self, person: &str) -> Pending {
        self.engine()
            .read()
            .pending(&PendingQuery {
                person: person.into(),
                session_ref: None,
                max_tokens: CARD_MAX_TOKENS,
            })
            .expect("pending")
    }

    fn card(&self, r: &str) -> Item {
        let q = self.queue(PERSON);
        q.items
            .iter()
            .find(|i| i.r#ref == r)
            .cloned()
            .unwrap_or_else(|| panic!("{r} is not queued: {:#?}", q))
    }

    /// The store's position now: a write moves it.
    fn at(&self) -> (rdy_store::segment::Location, Option<Hash>) {
        let st = self.engine().read();
        (st.seq(), st.canon_head())
    }

    /// The landings of canon, newest first.
    fn chain(&self) -> Vec<(Hash, LandingRecord)> {
        self.engine().read().reader().landings().expect("the chain")
    }
}

pub(crate) fn body(r: &http::Response) -> String {
    String::from_utf8_lossy(&r.body).into_owned()
}

fn item_path(r: &str) -> String {
    format!("/~ui/item/{}", percent_encode(r))
}

/// The text `s` as a page's template writes it.
fn esc(s: &str) -> String {
    rdy_render::escape(s).to_string()
}

/// A UI page: status, content type, one CSP with `form-action 'self'`, and no-store.
#[track_caller]
fn assert_ui_page(r: &http::Response, status: u16, what: &str) {
    assert_eq!(r.status, status, "{what}: {}\n{}", r.head, body(r));
    assert_eq!(r.header("content-type"), Some(HTML), "{what}: {}", r.head);
    assert_eq!(r.header("cache-control"), Some("no-store"), "{what}");
    let csps: Vec<&str> = r
        .head
        .split("\r\n")
        .filter(|l| {
            l.to_ascii_lowercase()
                .starts_with("content-security-policy:")
        })
        .collect();
    assert_eq!(
        csps,
        [format!("Content-Security-Policy: {UI_CSP}").as_str()],
        "{what}"
    );
}

/// A refusal page: its status, code and message, and that no write happened since `before`.
#[track_caller]
fn assert_refused(r: &http::Response, status: u16, code: &str, what: &str) {
    assert_ui_page(r, status, what);
    let page = body(r);
    assert!(
        page.contains(&format!("<code>{code}</code>")),
        "{what}: no {code} in {page}"
    );
    assert!(page.contains("class=\"refusal\""), "{what}: details");
}

/// The listener's themed 404, which names no UI.
#[track_caller]
fn assert_themed_404(r: &http::Response, what: &str) {
    assert_eq!(r.status, 404, "{what}: {}", r.head);
    assert_eq!(r.header("content-type"), Some(HTML), "{what}");
    assert_eq!(
        r.header("content-security-policy"),
        Some(CSP),
        "{what}: the ordinary CSP"
    );
    let page = body(r);
    assert!(page.contains("<h1>Page not found</h1>"), "{what}: {page}");
    for needle in ["~ui", "approval", "ui-card"] {
        assert!(!page.contains(needle), "{what}: names {needle:?}");
    }
}

#[track_caller]
fn assert_plain(r: &http::Response, f: Refusal, what: &str) {
    let want = refusal(f);
    assert_eq!(r.status, want.status, "{what}: {}", r.head);
    assert_eq!(r.body, want.body, "{what}");
    assert_eq!(r.header("content-type"), Some(PLAIN), "{what}");
}

fn ref_of_page(r: &http::Response) -> Vec<String> {
    http::ui_forms(&body(r))
        .into_iter()
        .map(|f| f.reference)
        .collect()
}

// --- the worlds ----------------------------------------------------------------------------------

/// A landed seed doc; the section to add claims to.
fn simple() -> (tempfile::TempDir, String) {
    let dir = short_tempdir();
    let mut r = Rdy::init(dir.path(), "rdy-standard").expect("init");
    let (_, section, _) = seed_doc(&mut r, "seed");
    let p = r
        .propose(&writer(), "seed", "Seed", Some("Seeds."))
        .expect("propose seed");
    r.land(&approver(), &p.id).expect("land seed");
    r.close().expect("close");
    (dir, section)
}

/// A claim in view `view` under `section`, proposed with a summary, its plan items answered.
fn proposal_in(r: &mut Rdy, section: &str, view: &str) -> Result<ProposalInfo, RdyError> {
    write(
        r,
        view,
        json!([{"op": "create", "type": "claim", "parent": section, "body": format!("A claim of {view}.")}]),
    )?;
    answer_open_items(r, &writer(), view);
    r.propose(&writer(), view, view, Some("Adds one claim."))
}

/// [`proposal_in`] on the daemon's writer.
fn proposal(s: &Site, section: &str, view: &str) -> ProposalInfo {
    let (section, view) = (section.to_string(), view.to_string());
    s.on(move |r| proposal_in(r, &section, &view))
}

/// The spec doc, stable in canon, and a question with authored options under its section: the
/// question's item ref and the section.
fn question_world() -> (tempfile::TempDir, String, BlockId) {
    let dir = short_tempdir();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let spec = spec_doc(&mut r);
    let t = brief_of(&r, &Scope::Canon, &[&spec.doc]).base.token;
    write_cited(
        &mut r,
        "st",
        json!([{"op": "update", "id": spec.doc.to_string(), "fields": {"lifecycle": "stable"}}]),
        Some(&t),
    )
    .unwrap();
    land(&mut r, "st").unwrap();
    let t = brief_of(&r, &Scope::Canon, &[&spec.section]).base.token;
    let w = write_cited(
        &mut r,
        "qs",
        json!([{"op": "create", "type": "question", "id": "$q",
                "parent": spec.section.to_string(), "fields": {
            "prompt": "Who approves refunds?",
            "title": "Who approves refunds?",
            "why": "Support waits on this",
            "options": ["Finance", "Support lead"],
            "recommended": "Finance",
            "recommend_reason": "Finance owns the ledger"
        }}]),
        Some(&t),
    )
    .unwrap();
    land(&mut r, "qs").unwrap();
    r.close().unwrap();
    let r = Rdy::open(dir.path()).unwrap();
    let q = created(&w, "$q");
    let v = rdy_qa::fixtures::version_of(&r, &Scope::Canon, &q.to_string());
    r.close().unwrap();
    (dir, format!("{q}@{}", &v[..12]), spec.section)
}

/// An `rdy-qa` project whose policy role `approvers` holds only `PERSON` and is required for each
/// version that writes a decision; then agent:a1's version that writes one, and the policy's id
/// and decision.
fn policed_world() -> (tempfile::TempDir, ProposalInfo, BlockId, BlockId) {
    let dir = short_tempdir();
    drop(Rdy::init(dir.path(), "rdy-standard").expect("init"));
    std::fs::write(dir.path().join(".rdy/packs/rdy-qa.yaml"), QA_PACK_YAML).unwrap();
    std::fs::write(dir.path().join(".rdy.yaml"), "pack: rdy-qa\n").unwrap();
    let mut r = Rdy::open(dir.path()).unwrap();
    let seed = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$n", "fields": {"title": "Ideas"}}
        ]),
    )
    .unwrap();
    land(&mut r, "seed").unwrap();
    let s = created(&seed, "$s");
    let rules = write(
        &mut r,
        "pol",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "rules", "title": "Rules", "doc_type": "overview"}},
            {"op": "create", "type": "decision", "id": "$dec", "parent": "$d", "fields": {"statement": "Adopt the policy", "status": "proposed", "needs_content": false}}
        ]),
    )
    .unwrap();
    let d = created(&rules, "$dec");
    let w = r
        .write(
            &writer(),
            WriteRequest {
                view: "pol".into(),
                ops: parse_test_ops(json!([{"op": "create", "type": "authority_policy", "id": "$p", "parent": rules.created["$d"], "fields": {
                    "key": "authority-policy",
                    "agent_may_accept": [],
                    "roles": {"approvers": [PERSON]},
                    "require": [{"when": "decision", "role": "approvers"}],
                    "owners": []
                }}]))
                .unwrap(),
                justification: Some(Justification::Implements(vec![d.clone()])),
                message: String::new(),
            },
        )
        .unwrap();
    land(&mut r, "pol").unwrap();
    let policy = created(&w, "$p");
    put_j(
        &mut r,
        "a1",
        json!([{"op": "create", "type": "decision", "id": "$dec", "parent": s.to_string(),
                "fields": {"statement": "Ideas stay short", "status": "proposed", "needs_content": false,
                           "reversibility": "two_way", "authority_basis": "precedent"}}]),
        j_typo(),
    );
    let p = r
        .propose(&ag("a1"), "a1", "a1", Some(TEST_SUMMARY))
        .unwrap();
    r.close().unwrap();
    (dir, p, policy, d)
}

/// One accepted decision imported by agent:a1 from another author, proposed: the version.
fn imported_version(r: &mut Rdy, dir: &Path) -> ProposalInfo {
    let out = dir.join("tree");
    imported_tree(
        &out,
        "rdy-standard",
        "wallets",
        "d-WA001",
        "s-WA001",
        "D-AC001",
    );
    let a1 = ag("a1");
    r.import(&a1, "a1", &out).expect("the import");
    std::fs::remove_dir_all(&out).expect("remove the tree");
    answer_open_items(r, &a1, "a1");
    r.propose(&a1, "a1", "a1", Some(TEST_SUMMARY))
        .expect("propose the import")
}

/// A version that edits a seed claim, with a stored independent verdict on it: its ref, id and
/// review.
fn verdict_version(s: &Site, x: &BlockId, verdict: &str) -> (ProposalInfo, String) {
    let id = x.clone();
    let p = s.on(move |r| {
        put_j(r, "a9", upd(&id, "X holds, now."), j_typo());
        r.propose(&ag("a9"), "a9", "a9", Some(TEST_SUMMARY))
    });
    let e = s.engine();
    let q = run_review(
        &*e,
        &ag("a9"),
        PERSON,
        true,
        None,
        ReviewRequest::from_value(
            json!({"op": "request", "check": "meaning_preservation", "subject": p.r#ref}),
        )
        .unwrap(),
    )
    .expect("the request");
    let reviewer = Session::agent("agent:r".into(), Some("r".into()), Role::Reviewer).unwrap();
    let v = run_review_submit(
        &*e,
        &reviewer,
        ReviewSubmitRequest {
            request: q["request"].as_str().unwrap().into(),
            case_hash: q["case_hash"].as_str().unwrap().into(),
            verdict: verdict.into(),
            citations: vec![CitationRequest {
                block: x.to_string(),
                note: "the meaning moved".into(),
            }],
            severity: None,
            reason: Some("it reads otherwise".into()),
        },
    )
    .expect("the verdict");
    (p, v["review"].as_str().unwrap().to_string())
}

fn hash(text: &str) -> Hash {
    text.parse().expect("hash")
}

fn labels(i: &Item) -> Vec<String> {
    i.options.iter().map(|o| o.label.clone()).collect()
}

fn label_of(i: &Item, option: &str) -> String {
    i.options
        .iter()
        .find(|o| o.id == option)
        .unwrap_or_else(|| panic!("{option} on {}", i.r#ref))
        .label
        .clone()
}

/// The landing record of the newest landing is the person's local card of `shown`.
#[track_caller]
fn check_landing(rec: &LandingRecord, shown: &Item, option: &str) {
    assert_eq!(rec.assurance, Assurance::Local, "{rec:#?}");
    assert_eq!(rec.approved_by.actor, PERSON, "{rec:#?}");
    assert!(
        rec.relayed_by.is_none() && rec.relay_evidence.is_none(),
        "{rec:#?}"
    );
    assert_eq!(
        rec.evidence,
        Some(ApprovalEvidence {
            kind: EvidenceKind::Card,
            options: labels(shown),
            choice: Some(label_of(shown, option)),
            card_hash: Some(hash(&shown.card_hash)),
        }),
        "{rec:#?}"
    );
}

/// The answers of the store.
fn answers_of(st: &rdy_engine::ReadState) -> Vec<Answer> {
    st.reader()
        .answers_with_prefix("answer/")
        .expect("the answers")
        .into_iter()
        .map(|(_, _, a)| a)
        .collect()
}

#[track_caller]
fn check_answer(a: &Answer, act: AnswerAct, shown: &Item, option: &str) {
    assert_eq!(a.act, act, "{a:#?}");
    assert_eq!(a.option.as_deref(), Some(option), "{a:#?}");
    assert_eq!(a.options, labels(shown), "{a:#?}");
    assert_eq!(a.card_hash, Some(hash(&shown.card_hash)), "{a:#?}");
    assert_eq!(a.person.actor, PERSON, "{a:#?}");
    assert_eq!(a.person.role, Role::Approver, "{a:#?}");
    assert!(
        a.relayed_by.is_none() && a.relay_evidence.is_none(),
        "{a:#?}"
    );
    assert_eq!(a.evidence_kind, Some(EvidenceKind::Card), "{a:#?}");
}

// --- the pins ------------------------------------------------------------------------------------

/// Pending, list, item, GET and HEAD read the same complete engine objects, with the Imported
/// fact and the check verdict options among them; a role keeps an item off a person's page and
/// refuses its direct ref; a forged ref gets the themed 404.
#[test]
fn ui9_ui_and_pending_share_items_roles_and_hashes() {
    let (dir, r, b) = batch_project();
    r.close().unwrap();
    let s = Site::start(dir, &actor(PERSON));
    let q = s.queue(PERSON);
    assert!(q.items.len() > 2, "{q:#?}");
    let page = s.get("/~ui/");
    assert_ui_page(&page, 200, "pending");
    let text = body(&page);
    let head = s.head("/~ui/");
    assert_eq!(head.status, 200);
    assert!(head.body.is_empty());
    assert_eq!(
        head.header("content-length"),
        page.header("content-length"),
        "HEAD keeps the GET length"
    );
    assert!(
        text.contains("/_rdy/events?ui=1&amp;rev="),
        "the reload stream: {text}"
    );
    for i in &q.items {
        assert!(
            text.contains(&format!("data-ref=\"{}\"", esc(&i.r#ref))),
            "{}",
            i.r#ref
        );
        assert!(text.contains(&i.card_hash), "{}", i.r#ref);
        assert!(text.contains(&esc(&i.card)), "{}: the whole card", i.r#ref);
        let one = s.get(&item_path(&i.r#ref));
        assert_ui_page(&one, 200, &i.r#ref);
        let t = body(&one);
        assert!(t.contains(&i.card_hash), "{}", i.r#ref);
        assert!(t.contains(&esc(&i.card)), "{}", i.r#ref);
        let h = s.head(&item_path(&i.r#ref));
        assert_eq!(h.status, 200);
        assert!(h.body.is_empty());
        assert_eq!(h.header("content-length"), one.header("content-length"));
    }
    assert!(q.items.iter().any(|i| i.r#ref == b.r#ref));
    // a ref that was never an item, in and out of the grammar: the themed 404
    for forged in ["P-AAAAAAAAAAAA@1", "garbage", "q-ZZZZZ@abcdef012345"] {
        assert_themed_404(&s.get(&item_path(forged)), forged);
        let h = s.head(&item_path(forged));
        assert_eq!(h.status, 404, "{forged}");
        assert!(h.body.is_empty(), "{forged}");
    }
    drop(s);

    // the Imported fact and the verdict options, from the same queue
    let (dir, section) = simple();
    let s = Site::start(dir, &actor(PERSON));
    let root = s.h.dir.path().to_path_buf();
    let imp = s.on(move |r| Ok(imported_version(r, &root)));
    let card = s.card(&imp.r#ref);
    assert!(
        card.facts.iter().any(|f| f.name == "Imported"),
        "{:#?}",
        card.facts
    );
    let (page, form) = s.form(&imp.r#ref);
    let fact = card
        .facts
        .iter()
        .find(|f| f.name == "Imported")
        .expect("the Imported fact");
    assert!(
        fact.value
            .starts_with("1 decision recorded as accepted or rejected elsewhere"),
        "{}",
        fact.value
    );
    assert!(body(&page).contains(&esc(&fact.value)), "the computed fact");
    assert!(body(&page).contains(&esc(&card.card)), "the whole card");
    assert_eq!(form.card_hash, card.card_hash);
    let x = s.on(move |r| {
        let w = write(
            r,
            "xs",
            json!([{"op": "create", "type": "claim", "id": "$x", "parent": section, "body": "X holds."}]),
        )?;
        answer_open_items(r, &writer(), "xs");
        land(r, "xs")?;
        Ok(created(&w, "$x"))
    });
    let (p, review) = verdict_version(&s, &x, "concern");
    let card = s.card(&p.r#ref);
    let accept = format!("accept_verdict:{review}:{}", p.id);
    assert!(card.options.iter().any(|o| o.id == accept), "{card:#?}");
    let (_, form) = s.form(&p.r#ref);
    assert_eq!(form.card_hash, card.card_hash);
    assert!(form.options.contains(&accept), "{form:#?}");
    drop(s);

    // a person outside the policy role: no item on the page, and the direct ref refused
    let (dir, p, _, _) = policed_world();
    let s = Site::start(dir, &actor("human:other"));
    let theirs = s.queue("human:other");
    assert!(
        !theirs.items.iter().any(|i| i.r#ref == p.r#ref),
        "routed to the role: {theirs:#?}"
    );
    assert!(
        !body(&s.get("/~ui/")).contains(&format!("data-ref=\"{}\"", p.r#ref)),
        "the page lists what the queue lists"
    );
    let r = s.get(&item_path(&p.r#ref));
    assert_refused(&r, 403, "E_PERMISSION", "outside the role");
    assert!(
        body(&r).contains(&esc(&format!(
            "human:other is not in the policy role approvers for {}",
            p.r#ref
        ))),
        "{}",
        body(&r)
    );
    assert!(
        body(&r).contains("ask a person in the policy role approvers, or relay their answer"),
        "{}",
        body(&r)
    );
}

/// A form's approval and a question's answer record exact local evidence, the person, no relay,
/// and the normal published state, also after a reopen and a reindex. A question under stable
/// content shows the destination's briefing with a registered token; with no token it is
/// `E_BRIEF_REQUIRED`, with a moved destination `E_BRIEF_STALE`, and a new GET gets a fresh one.
#[test]
fn ui9_post_records_a_local_card_through_the_gate() {
    let (dir, section) = simple();
    let s = Site::start(dir, &actor(PERSON));
    let p = proposal(&s, &section, "a");
    let shown = s.card(&p.r#ref);
    let (_, form) = s.form(&p.r#ref);
    assert_eq!(form.card_hash, shown.card_hash);
    let done = s.answer(&form, "approve");
    assert_eq!(done.status, 303, "{}\n{}", done.head, body(&done));
    assert_eq!(done.header("location"), Some("/~ui/"));
    assert!(done.body.is_empty());
    assert_eq!(done.header("content-security-policy"), Some(UI_CSP));
    assert!(done.header("set-cookie").is_none(), "no cookie");
    let (landing, rec) = s.chain().remove(0);
    check_landing(&rec, &shown, "approve");
    // the published state: the page no longer lists it
    assert!(!body(&s.get("/~ui/")).contains(&format!("data-ref=\"{}\"", p.r#ref)));
    let Site { mut h, sh, .. } = s;
    drop(sh);
    h.stop();
    let mut r = Rdy::open(h.dir.path()).unwrap();
    assert_eq!(r.read().reader().landing(&landing).unwrap(), rec);
    r.reindex().unwrap();
    assert_eq!(r.read().reader().landing(&landing).unwrap(), rec);
    r.close().unwrap();

    // the question under a stable destination
    let (dir, it, section) = question_world();
    let s = Site::start(dir, &actor(PERSON));
    let shown = s.card(&it);
    assert_eq!(shown.kind, ItemKind::Answer);
    // the pending page links it and shows no form for it
    let page = body(&s.get("/~ui/"));
    assert!(
        page.contains(&format!("href=\"{}\"", esc(&item_path(&it)))),
        "{page}"
    );
    assert!(!ref_of_page(&s.get("/~ui/")).contains(&it));
    let (page, form) = s.form(&it);
    let text = body(&page);
    assert!(text.contains("<h2>Briefing</h2>"), "{text}");
    assert!(text.contains(&section.to_string()), "the destination");
    let token = form.brief.clone().expect("a hidden briefing token");
    assert_eq!(
        form.card_hash, shown.card_hash,
        "the briefing moves no hash"
    );
    assert_eq!(s.card(&it).card_hash, shown.card_hash);
    // no token: refused, nothing written, and no new token offered
    let at = s.at();
    let bare = UiForm {
        brief: None,
        ..form.clone()
    };
    let r = s.answer(&bare, "o1");
    assert_refused(&r, 422, "E_BRIEF_REQUIRED", "no token");
    assert!(http::ui_forms(&body(&r)).is_empty(), "no form");
    assert_eq!(s.at(), at);
    assert!(answers_of(&s.engine().read()).is_empty());
    // the destination moves: refused, nothing written, no substitute token, no new form
    let id = section.clone();
    s.on(move |r| {
        let t = brief_of(r, &Scope::Canon, &[&id]).base.token;
        write_cited(
            r,
            "moved",
            json!([{"op": "update", "id": id.to_string(), "fields": {"title": "Moved"}}]),
            Some(&t),
        )?;
        land(r, "moved").map(|_| ())
    });
    let at = s.at();
    let r = s.answer(&form, "o1");
    assert_refused(&r, 422, "E_BRIEF_STALE", "a moved destination");
    assert!(http::ui_forms(&body(&r)).is_empty(), "no form");
    assert!(!body(&r).contains(&token), "the token is not shown again");
    assert_eq!(s.at(), at);
    assert!(answers_of(&s.engine().read()).is_empty());
    // the next GET fetches a fresh briefing and token; that answer lands in the person's view
    let shown = s.card(&it);
    let (_, form) = s.form(&it);
    let fresh = form.brief.clone().expect("a fresh token");
    assert_ne!(fresh, token);
    let done = s.answer(&form, "o1");
    assert_eq!(done.status, 303, "{}", body(&done));
    let st = s.engine().read();
    let all = answers_of(&st);
    assert_eq!(all.len(), 1, "{all:#?}");
    check_answer(&all[0], AnswerAct::AnswerQuestion, &shown, "o1");
    assert_eq!(
        all[0].label.as_deref(),
        Some("Finance"),
        "the authored label"
    );
    drop(st);
    let Site { mut h, sh, .. } = s;
    drop(sh);
    h.stop();
    let mut r = Rdy::open(h.dir.path()).unwrap();
    r.reindex().unwrap();
    let stored = answers_of(&r.read());
    assert_eq!(stored, all);
    r.close().unwrap();
}

/// A moved proposal or item, a changed policy role, exceptions, deferrals, accepted tension and
/// check holds and a redaction keep their engine refusals: nothing lands, and a moved card comes
/// back fresh.
#[test]
fn ui9_moved_and_held_cards_cannot_land() {
    // a moved proposal: 409, and the fresh card's form
    let (dir, section) = simple();
    let s = Site::start(dir, &actor(PERSON));
    let old = proposal(&s, &section, "m");
    let (_, stale) = s.form(&old.r#ref);
    let newer = proposal(&s, &section, "m");
    assert_ne!(newer.r#ref, old.r#ref);
    let chain = s.chain().len();
    let r = s.answer(&stale, "approve");
    assert_refused(&r, 409, "E_PROPOSAL_MOVED", "a moved proposal");
    assert_eq!(ref_of_page(&r), [newer.r#ref.clone()], "the fresh form");
    let fresh = http::ui_forms(&body(&r)).remove(0);
    assert_eq!(fresh.card_hash, s.card(&newer.r#ref).card_hash);
    assert_eq!(s.chain().len(), chain, "no landing");
    // only the fresh form lands
    let done = s.answer(&fresh, "approve");
    assert_eq!(done.status, 303, "{}", body(&done));
    assert_eq!(s.chain().len(), chain + 1);
    drop(s);

    // a moved item, an open exception, and an accepted tension hold
    let (dir, r, b) = batch_project();
    r.close().unwrap();
    let s = Site::start(dir, &actor(PERSON));
    let exception = s
        .queue(PERSON)
        .items
        .into_iter()
        .find(|i| i.kind == ItemKind::Confirm && i.batch.as_deref() == Some(b.r#ref.as_str()))
        .expect("an exception");
    let (_, form) = s.form(&exception.r#ref);
    let wrong = UiForm {
        card_hash: Hash::of(b"another card").to_string(),
        ..form.clone()
    };
    let r = s.answer(&wrong, "confirm");
    assert_refused(&r, 409, "E_ITEM_MOVED", "a moved item");
    assert_eq!(
        http::ui_forms(&body(&r))[0].card_hash,
        exception.card_hash,
        "the fresh card"
    );
    let (_, form) = s.form(&b.r#ref);
    let chain = s.chain().len();
    let r = s.answer(&form, "approve");
    assert_refused(&r, 422, "E_EXCEPTION_OPEN", "an open exception");
    assert_eq!(s.chain().len(), chain);
    // the person accepts the advice beside it: the accepted tension holds the batch
    let advice = format!("advice:{}", b.r#ref);
    local(s.h.dir.path(), &actor("human:founder"));
    let (_, a) = s.form(&advice);
    let done = s.answer(&a, "accept");
    assert_eq!(done.status, 303, "{}", body(&done));
    local(s.h.dir.path(), &actor(PERSON));
    let r = s.answer(&form, "approve");
    assert_refused(&r, 422, "E_PROPOSAL_NOT_READY", "an accepted tension");
    assert!(body(&r).contains("is not ready"), "{}", body(&r));
    assert_eq!(s.chain().len(), chain);
    drop(s);

    // an open deferral holds the version
    let dir = short_tempdir();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds 30 days."},
            {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "B builds on {{$a}}."}
        ]),
    )
    .unwrap();
    land(&mut r, "seed").unwrap();
    let (a, b) = (w.created["$a"].clone(), w.created["$b"].clone());
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": a, "body": "A holds 60 days."}]),
    )
    .unwrap();
    let item = rdy_model::plan::item_display(&rdy_model::plan::item_hash(
        &BlockId::parse(&a).unwrap(),
        &b,
        rdy_model::plan::PlanKind::Confirm,
    ));
    let answers = [rdy_engine::plan::PlanAnswerInput {
        item,
        act: rdy_model::plan::PlanAct::Defer,
        reason: Some("b waits".into()),
    }];
    let p = r
        .propose_plan(
            &writer(),
            rdy_engine::plan::ProposePlanRequest {
                summary: Some(TEST_SUMMARY),
                plan_answers: &answers,
                ..rdy_engine::plan::ProposePlanRequest::new("w", "w")
            },
        )
        .unwrap();
    r.close().unwrap();
    let s = Site::start(dir, &actor(PERSON));
    let (_, form) = s.form(&p.r#ref);
    let chain = s.chain().len();
    let r = s.answer(&form, "approve");
    assert_refused(&r, 422, "E_DEFERRAL_OPEN", "an open deferral");
    assert_eq!(s.chain().len(), chain);
    drop(s);

    // an accepted check hold
    let (dir, section) = simple();
    let s = Site::start(dir, &actor(PERSON));
    let x = s.on(move |r| {
        let w = write(
            r,
            "xs",
            json!([{"op": "create", "type": "claim", "id": "$x", "parent": section, "body": "X holds."}]),
        )?;
        answer_open_items(r, &writer(), "xs");
        land(r, "xs")?;
        Ok(created(&w, "$x"))
    });
    let (p, review) = verdict_version(&s, &x, "fail");
    let (_, form) = s.form(&p.r#ref);
    let done = s.answer(&form, &format!("accept_verdict:{review}:{}", p.id));
    assert_eq!(done.status, 303, "{}", body(&done));
    let chain = s.chain().len();
    let r = s.answer(&form, "approve");
    assert_eq!(s.chain().len(), chain, "held: {}", body(&r));
    assert_refused(&r, 422, "E_PROPOSAL_NOT_READY", "an accepted check hold");
    drop(s);

    // a changed policy role: the person's old form lands nothing
    let (dir, p, policy, decision) = policed_world();
    let s = Site::start(dir, &actor(PERSON));
    let (_, form) = s.form(&p.r#ref);
    let chain = s.chain().len();
    s.on(move |r| {
        r.write(
            &writer(),
            WriteRequest {
                view: "role".into(),
                ops: parse_test_ops(json!([{"op": "update", "id": policy.to_string(),
                    "fields": {"roles": {"approvers": ["human:other"]}}}]))?,
                justification: Some(Justification::Implements(vec![decision])),
                message: String::new(),
            },
        )?;
        answer_open_items(r, &writer(), "role");
        let v = r.propose(&writer(), "role", "role", Some("Moves the role."))?;
        r.land(&approver(), &v.id).map(|_| ())
    });
    let r = s.answer(&form, "approve");
    let page = body(&r);
    assert_eq!(
        s.chain().len(),
        chain + 1,
        "only the role's landing: {page}"
    );
    // the policy moved the card, and the fresh card refuses this person: the original refusal
    // and the corrected card's text, with no form
    assert_refused(&r, 409, "E_PROPOSAL_MOVED", "a changed role");
    assert!(http::ui_forms(&page).is_empty(), "no form: {page}");
    assert!(page.contains("ui-card-text"), "the corrected card: {page}");
    drop(s);

    // a removal between the form and the answer: nothing lands, and the page shows no removed text
    let (dir, section) = simple();
    let s = Site::start(dir, &actor(PERSON));
    let target = s.on(move |r| {
        let w = write(
            r,
            "secret",
            json!([{"op": "create", "type": "claim", "id": "$c", "parent": section, "body": "SECRET-CODE."}]),
        )?;
        answer_open_items(r, &writer(), "secret");
        land(r, "secret")?;
        Ok(created(&w, "$c"))
    });
    let id = target.clone();
    let p = s.on(move |r| {
        write(
            r,
            "edit",
            json!([{"op": "update", "id": id.to_string(), "body": "SECRET-CODE, edited."}]),
        )?;
        answer_open_items(r, &writer(), "edit");
        r.propose(&writer(), "edit", "edit", Some("Edits it."))
    });
    let (_, form) = s.form(&p.r#ref);
    let id = target.to_string();
    s.on(move |r| {
        r.redact_in(
            &writer(),
            "rm",
            RedactInput {
                targets: vec![id],
                reason_class: "secret".into(),
                reason: "a leaked code".into(),
                dry_run: false,
            },
        )?;
        let v = r.propose(&writer(), "rm", "Remove", Some("Removes the code."))?;
        r.land(&approver(), &v.id).map(|_| ())
    });
    let chain = s.chain().len();
    let r = s.answer(&form, "approve");
    assert_eq!(s.chain().len(), chain, "{}\n{}", r.head, body(&r));
    assert!(!body(&r).contains("SECRET-CODE."), "{}", body(&r));
    assert_refused(&r, 409, "E_PROPOSAL_MOVED", "a removal moved the card");
}

/// `signing.sign: always` refuses a landing with the exact UI text, and auto and never land
/// unsigned with no warning; an answer that lands nothing is allowed under each; a UI Later
/// returns the pending page with its notice and stores nothing.
#[test]
fn ui9_ui_signing_and_later_follow_the_ruled_table() {
    let (dir, section) = simple();
    let s = Site::start(dir, &actor(PERSON));
    let root = s.h.dir.path().to_path_buf();
    let signing = |mode: &str| {
        local(
            &root,
            &format!(
                "actor: {PERSON}\nsigning:\n  key: null\n  agent_key: null\n  sign: {mode}\n  allowed_signers: null\n"
            ),
        );
    };
    for mode in ["never", "auto"] {
        signing(mode);
        let p = proposal(&s, &section, &format!("u-{mode}"));
        let shown = s.card(&p.r#ref);
        let (_, form) = s.form(&p.r#ref);
        let done = s.answer(&form, "approve");
        assert_eq!(done.status, 303, "{mode}: {}", body(&done));
        let (landing, rec) = s.chain().remove(0);
        check_landing(&rec, &shown, "approve");
        let st = s.engine().read();
        assert!(
            st.reader()
                .signatures_of(&landing)
                .expect("the signatures")
                .is_empty(),
            "{mode}: unsigned"
        );
    }
    signing("always");
    let p = proposal(&s, &section, "u-always");
    let (_, form) = s.form(&p.r#ref);
    let at = s.at();
    let r = s.answer(&form, "approve");
    assert_refused(&r, 403, "E_NO_AGENT", "always");
    let page = body(&r);
    assert!(
        page.contains(&esc(
            "the SSH agent could not sign: the local UI has no agent socket"
        )),
        "{page}"
    );
    assert!(
        page.contains(&esc(&format!(
            "run rdy approve {} in a terminal to sign this approval",
            p.r#ref
        ))),
        "{page}"
    );
    assert_eq!(s.at(), at, "nothing written");
    // reject lands nothing: it needs no signature
    let shown = s.card(&p.r#ref);
    let done = s.answer(&form, "reject");
    assert_eq!(done.status, 303, "{}", body(&done));
    let all = answers_of(&s.engine().read());
    let a = all
        .iter()
        .find(|a| a.item == p.r#ref)
        .expect("the rejection");
    check_answer(a, AnswerAct::RejectProposal, &shown, "reject");
    signing("never");

    // Later: the pending page, its notice, and no store write
    let p = proposal(&s, &section, "l");
    let (_, form) = s.form(&p.r#ref);
    assert!(form.options.iter().any(|o| o == "later"), "{form:#?}");
    let at = s.at();
    let r = s.answer(&form, "later");
    assert_ui_page(&r, 200, "later");
    let page = body(&r);
    assert!(
        page.contains(&esc(&format!(
            "Later keeps {} open. It stays on this page until you answer it.",
            p.r#ref
        ))),
        "{page}"
    );
    assert!(
        page.contains(&format!("data-ref=\"{}\"", p.r#ref)),
        "{page}"
    );
    assert_eq!(s.at(), at, "a UI Later stores nothing");
    assert!(
        s.queue(PERSON).items.iter().any(|i| i.r#ref == p.r#ref),
        "still open"
    );
    assert!(r.header("location").is_none());
    // Later with a moved hash is bound first
    let stale = UiForm {
        card_hash: Hash::of(b"another card").to_string(),
        ..form
    };
    assert_refused(
        &s.answer(&stale, "later"),
        409,
        "E_PROPOSAL_MOVED",
        "a moved Later",
    );
}

/// Pin id kept from Draft 1; the corrected rule (TASK-982 C5): a listener with no UI route answers
/// `GET` and `HEAD` with its themed 404, `POST` with the existing plain 405, and a UI event request
/// with `Bad request.`, after the ordinary Host check, with no pending lookup or UI hint. A config
/// edit takes effect on the next request. An unknown item on an eligible listener is the same 404.
#[test]
fn ui9_lan_profile_and_disabled_ui_are_plain_404() {
    let ordinary = |port: u16, what: &str| {
        let host = format!("127.0.0.1:{port}");
        let at = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        for path in ["/~ui/", "/~ui/item/P-abc@1", "/~ui/answer"] {
            let get = http::raw(at, &format!("GET {path} HTTP/1.1\r\nHost: {host}\r\n\r\n"));
            assert_themed_404(&get, &format!("{what}: GET {path}"));
            let head = http::raw(at, &format!("HEAD {path} HTTP/1.1\r\nHost: {host}\r\n\r\n"));
            assert_eq!(head.status, 404, "{what}: HEAD {path}");
            assert!(head.body.is_empty(), "{what}: HEAD {path}");
            assert_eq!(
                head.header("content-length"),
                get.header("content-length"),
                "{what}: HEAD {path}"
            );
            // the ordinary guard's precedence: a forged Host is refused first
            let forged = http::raw(
                at,
                &format!("GET {path} HTTP/1.1\r\nHost: rebound.invalid:{port}\r\n\r\n"),
            );
            assert_plain(&forged, Refusal::Host, &format!("{what}: forged {path}"));
        }
        let post = http::raw(at, &http::ui_answer_request(&host, &[("ref", "r")]));
        assert_plain(&post, Refusal::Method, &format!("{what}: POST"));
        assert_eq!(post.header("allow"), Some("GET, HEAD"), "{what}");
        let events = http::raw(
            at,
            &format!("GET /_rdy/events?ui=1&rev=0011223344556677 HTTP/1.1\r\nHost: {host}\r\n\r\n"),
        );
        assert_plain(&events, Refusal::Malformed, &format!("{what}: events"));
    };
    let (dir, section) = simple();
    // serve.ui off, read fresh for each request
    let s = Site::start(dir, &format!("actor: {PERSON}\nserve:\n  ui: false\n"));
    let p = proposal(&s, &section, "c");
    ordinary(s.port, "off");
    local(s.h.dir.path(), &actor(PERSON));
    assert_ui_page(&s.get("/~ui/"), 200, "on again");
    assert!(body(&s.get("/~ui/")).contains(&p.r#ref));
    local(
        s.h.dir.path(),
        &format!("actor: {PERSON}\nserve:\n  ui: false\n"),
    );
    ordinary(s.port, "off again");
    local(s.h.dir.path(), &actor(PERSON));
    // an unknown item of an eligible listener: the same 404, by its path alone
    let miss = s.get("/~ui/item/P-AAAAAAAAAAAA@1");
    assert_themed_404(&miss, "unknown item");
    assert_eq!(miss.body, s.get("/nosuch.html").body, "one page, one text");
    // a UI path with a parser, Host, Origin or method refusal stays plain on an eligible listener
    let host = s.host();
    assert_plain(
        &s.send(&format!(
            "GET /~ui/ HTTP/1.1\r\nHost: rebound.invalid:{}\r\n\r\n",
            s.port
        )),
        Refusal::Host,
        "a forged Host",
    );
    assert_plain(
        &s.send(&format!(
            "GET /~ui/ HTTP/1.1\r\nHost: {host}\r\nOrigin: http://rebound.invalid\r\n\r\n"
        )),
        Refusal::Origin,
        "a foreign Origin",
    );
    assert_plain(
        &s.send(&format!(
            "POST /~ui/ HTTP/1.1\r\nHost: {host}\r\nContent-Length: 0\r\n\r\n"
        )),
        Refusal::UiMethod { allow: "GET, HEAD" },
        "a POST to a page",
    );
    assert_plain(
        &s.send(&format!("GET /~ui/answer HTTP/1.1\r\nHost: {host}\r\n\r\n")),
        Refusal::UiMethod { allow: "POST" },
        "a GET of the answer route",
    );
    drop(s);

    // a profile-only listener: its own layout, whatever serve.ui says
    let (dir, mut r) = tiny();
    let ids = write(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "doc", "id": "$intro", "fields": {"name": "intro", "title": "Intro page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$intro", "body": "INTRO"},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
        ]),
    )
    .unwrap()
    .created;
    land(&mut r, "w1").unwrap();
    write(
        &mut r,
        "admin",
        json!([{"op": "create", "type": "publication_profile", "parent": ids["$admin"].to_string(),
            "fields": {"key": "guide", "title": "Guide book", "description": "About it.",
                "audience": "org", "select": [ids["$intro"].to_string()]}}]),
    )
    .unwrap();
    land(&mut r, "admin").unwrap();
    r.close().unwrap();
    let s = Site::start(dir, &actor(PERSON));
    let only = port_of(
        &ensure_profile_site(Arc::clone(&s.sh), "guide")
            .unwrap()
            .url
            .unwrap(),
    );
    ordinary(only, "profile");
    let host = format!("127.0.0.1:{only}");
    let miss = http::raw(
        SocketAddr::from((Ipv4Addr::LOCALHOST, only)),
        &format!("GET /~ui/ HTTP/1.1\r\nHost: {host}\r\n\r\n"),
    );
    assert!(
        body(&miss).contains("Guide book"),
        "the profile's own layout: {}",
        body(&miss)
    );
    assert_ui_page(&s.get("/~ui/"), 200, "the full listener still has it");
}

/// Using the pages changes nothing a site serves or writes: pages, search, llms and the versions
/// menu keep their bytes and name no UI, no build runs for a UI page, and a static or profile
/// build holds none of it.
#[test]
fn ui9_ui_never_enters_a_static_or_profile_build() {
    let (dir, mut r) = tiny();
    let ids = write(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "doc", "id": "$intro", "fields": {"name": "intro", "title": "Intro page", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$intro", "fields": {"title": "Part"}},
            {"op": "create", "type": "prose", "parent": "$s", "body": "INTRO"},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
        ]),
    )
    .unwrap()
    .created;
    land(&mut r, "w1").unwrap();
    write(
        &mut r,
        "admin",
        json!([{"op": "create", "type": "publication_profile", "parent": ids["$admin"].to_string(),
            "fields": {"key": "guide", "title": "Guide book", "description": "About it.",
                "audience": "org", "select": [ids["$intro"].to_string()]}}]),
    )
    .unwrap();
    land(&mut r, "admin").unwrap();
    let section = ids["$s"].to_string();
    write(
        &mut r,
        "c",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "A claim of c."}]),
    )
    .unwrap();
    answer_open_items(&mut r, &writer(), "c");
    let p = r
        .propose(&writer(), "c", "c", Some("Adds a claim."))
        .unwrap();
    r.close().unwrap();
    let s = Site::start(dir, &actor(PERSON));
    let pages = [
        "/index.html",
        "/intro.html",
        "/search.html",
        "/search-index.json",
        "/llms.txt",
        "/llms-full.txt",
        "/~versions/",
        "/~profile/guide/index.html",
    ];
    let read = |s: &Site| -> Vec<(u16, Vec<u8>)> {
        pages
            .iter()
            .map(|path| {
                let r = s.get(path);
                (r.status, r.body)
            })
            .collect()
    };
    let before = read(&s);
    let render = |s: &Site, out: PathBuf, profile: Option<&str>| {
        s.engine()
            .render(RenderRequest {
                out,
                scope: None,
                profile: profile.map(str::to_string),
                view: None,
            })
            .expect("the static render");
    };
    let t = tempfile::tempdir().unwrap();
    render(&s, t.path().join("a"), None);
    render(&s, t.path().join("pa"), Some("guide"));
    let (builds, scopes) = (s.sh.site.builds(), s.sh.site.scopes());
    // the pages, in use
    assert_ui_page(&s.get("/~ui/"), 200, "pending");
    let (_, form) = s.form(&p.r#ref);
    let r = s.answer(&form, "later");
    assert_ui_page(&r, 200, "later");
    assert_eq!(
        (s.sh.site.builds(), s.sh.site.scopes()),
        (builds, scopes),
        "a UI page builds nothing and keeps no site target"
    );
    let after = read(&s);
    assert_eq!(before, after, "the bytes of the pages");
    for (path, (status, bytes)) in pages.iter().zip(&after) {
        let text = String::from_utf8_lossy(bytes);
        assert!(!text.contains("~ui"), "{path} ({status}) names the UI");
        assert!(!text.contains("ui-card"), "{path} ({status})");
    }
    render(&s, t.path().join("b"), None);
    render(&s, t.path().join("pb"), Some("guide"));
    let tree = |root: &Path| -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = vec![];
        let mut todo = vec![root.to_path_buf()];
        while let Some(d) = todo.pop() {
            for e in std::fs::read_dir(&d).unwrap() {
                let e = e.unwrap();
                if e.file_type().unwrap().is_dir() {
                    todo.push(e.path());
                } else {
                    out.push((
                        e.path().strip_prefix(root).unwrap().to_path_buf(),
                        std::fs::read(e.path()).unwrap(),
                    ));
                }
            }
        }
        out.sort();
        out
    };
    for (a, b) in [("a", "b"), ("pa", "pb")] {
        let (x, y) = (tree(&t.path().join(a)), tree(&t.path().join(b)));
        assert_eq!(x, y, "{a}: the render's files");
        for (path, bytes) in &y {
            let text = String::from_utf8_lossy(bytes);
            assert!(!text.contains("/~ui/"), "{}", path.display());
        }
    }
    // a miss under the UI's name stays scoped: the themed 404 of canon
    assert_themed_404(&s.get("/~ui/x"), "/~ui/x");
    assert_themed_404(&s.get("/~ui"), "/~ui");
}

// --- the goldens ---------------------------------------------------------------------------------

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/site_http")
}

/// Compares `text` (line ends as `\n`) with the golden `name`; `RDY_QA_BLESS=1` writes it.
fn check(name: &str, text: &str) {
    let text = text.replace("\r\n", "\n");
    let file = golden_dir().join(format!("{name}.txt"));
    if std::env::var_os("RDY_QA_BLESS").is_some() {
        std::fs::write(&file, &text).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&file)
        .unwrap_or_else(|_| panic!("{name}: no golden at {}", file.display()));
    assert_eq!(text, want, "{name}: the response changed from the golden");
}

/// The five UI responses, exact: status, one CSP, content type and length. `ui_disabled` is the
/// listener's themed 404 with the ordinary CSP. The ordinary goldens stay in `daemon_site_http`.
#[test]
fn ui9_ui_http_goldens_have_exact_headers_and_lengths() {
    // the fixture clock and a named project give one length for every run
    let fixed = |dir: &Path| {
        std::fs::write(
            dir.join(".rdy.yaml"),
            "pack: rdy-standard\nname: \"Tiny\"\n",
        )
        .unwrap();
    };
    let dir = short_tempdir();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    fixed(dir.path());
    let mut r = Rdy::open(dir.path()).unwrap();
    let (_, section, _) = seed_doc(&mut r, "seed");
    let p = r
        .propose(&writer(), "seed", "Seed", Some("Seeds."))
        .unwrap();
    r.land(&approver(), &p.id).unwrap();
    let old = proposal_in(&mut r, &section, "golden").unwrap();
    r.close().unwrap();
    let s = Site::start(dir, &actor(PERSON));
    let port = s.port;
    let own = format!("Host: 127.0.0.1:{port}");
    let head_of = |r: &http::Response| http::normalize(&r.head, port);
    let pending = s.send(&format!("HEAD /~ui/ HTTP/1.1\r\n{own}\r\n\r\n"));
    assert_ui_page(&pending, 200, "pending");
    check("ui_pending_head", &head_of(&pending));
    let card = s.send(&format!(
        "HEAD {} HTTP/1.1\r\n{own}\r\n\r\n",
        item_path(&old.r#ref)
    ));
    assert_ui_page(&card, 200, "card");
    check("ui_card_head", &head_of(&card));
    let (_, form) = s.form(&old.r#ref);
    let sec = section.clone();
    let newer = s.on(move |r| proposal_in(r, &sec, "golden"));
    let moved = s.answer(&form, "approve");
    assert_refused(&moved, 409, "E_PROPOSAL_MOVED", "moved");
    check("ui_moved", &head_of(&moved));
    drop(newer);
    let (dir2, p, _, _) = policed_world();
    let t = Site::start(dir2, &actor("human:other"));
    let permission = t.send(&format!(
        "HEAD {} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
        item_path(&p.r#ref),
        t.port
    ));
    assert_eq!(permission.status, 403, "{}", permission.head);
    assert_eq!(permission.header("content-security-policy"), Some(UI_CSP));
    check("ui_permission", &http::normalize(&permission.head, t.port));
    drop(t);
    local(
        s.h.dir.path(),
        &format!("actor: {PERSON}\nserve:\n  ui: false\n"),
    );
    let off = s.get("/~ui/");
    assert_themed_404(&off, "off");
    check(
        "ui_disabled",
        &http::normalize(&format!("{}\r\n\r\n{}", off.head, body(&off)), port),
    );
}
