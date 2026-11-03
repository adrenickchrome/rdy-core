//! mg5 (M3 gate, Task 5; D-2GT0Z, D-ME2HD, D-HAG73, D-ZWP7Q, D-QBNAQ, D-JQD5X, D-6M0AF,
//! D-WPE4R, D-TF78H): authority through every built approval route. Each route gets a fresh
//! project from one recipe. Engine calls only seed the fixture; every act that approves, answers
//! or lands is a real exchange: a relayed `rdy_answer` and an elicited answer over the `rdy mcp`
//! daemon proxy, the simulated person's form on the local approval page, and the person's own
//! `rdy answer` and `rdy approve`. The stored effects are read back through `rdy log` and
//! compared after the generated ids and times, and the route's own evidence fields, are set
//! aside. Local, elicited, relayed and policy provenance keep their real differences.
use rdy_engine::pending::queue::PENDING_ITEMS_MAX;
use rdy_engine::pending::{CARD_MAX_TOKENS, Pending, PendingQuery};
use rdy_engine::{DecisionInput, ProposalInfo, Rdy, WriteRequest};
use rdy_model::assessment::PolicyVerdict;
use rdy_model::{BlockId, Justification};
use rdy_qa::daemon::Served;
use rdy_qa::edit_loop::{Spec, spec_doc};
use rdy_qa::fixtures::{approver, land, parse_test_ops, write, writer};
use rdy_qa::http::{self, UiForm};
use rdy_qa::intake::{PERSON, TEST_SUMMARY, answer_open_items};
use rdy_qa::mcp::{McpClient, McpFailure, PersonAct, Reply, Revision};
use serde_json::{Value, json};
use std::path::Path;

// ---------------------------------------------------------------- the routes

/// One built way for the person's approval to reach the store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    /// An agent relays the person's words over MCP: `rdy_answer` with evidence.
    Relayed,
    /// The agent's MCP client shows rdy's card and gives back the person's choice.
    Elicited,
    /// The simulated person answers the form of the local approval page.
    Ui,
    /// The person's `rdy answer <ref> <option> --card <hash>`.
    CliAnswer,
    /// The person's `rdy approve <P-x@n>`: an approval of the version, with no card.
    CliApprove,
}

const ROUTES: [Route; 5] = [
    Route::Relayed,
    Route::Elicited,
    Route::Ui,
    Route::CliAnswer,
    Route::CliApprove,
];

/// The routes that answer a card (every route but `rdy approve`).
const CARD_ROUTES: [Route; 4] = [Route::Relayed, Route::Elicited, Route::Ui, Route::CliAnswer];

/// The relaying agent's session.
const RELAY: &str = "agent:relay";
/// The person's words an agent relays.
const WORDS: &str = "Yes, go ahead.";

/// The card the person was shown: its item ref, its hash and its text.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Shown {
    r#ref: String,
    card_hash: String,
    options: Vec<String>,
}

/// What a route's act gave back.
#[derive(Debug)]
enum Outcome {
    /// It was recorded; the reply (null for the UI, which redirects).
    Done(Value),
    /// It was refused: the code, and the reply (the UI: its refusal page as text).
    Refused { code: String, reply: Value },
}

impl Outcome {
    #[track_caller]
    fn done(self) -> Value {
        match self {
            Outcome::Done(v) => v,
            Outcome::Refused { code, reply } => panic!("refused {code}: {reply:#}"),
        }
    }

    #[track_caller]
    fn refused(self) -> (String, Value) {
        match self {
            Outcome::Refused { code, reply } => (code, reply),
            Outcome::Done(v) => panic!("expected a refusal, got {v:#}"),
        }
    }
}

/// A served project for one route: the person is [`PERSON`] (the user config `Served` writes).
struct Project {
    s: Served,
}

impl Project {
    /// A fresh project, seeded by `recipe` with the daemon stopped.
    fn new<T>(recipe: impl FnOnce(&mut Rdy) -> T) -> (Project, T) {
        let s = Served::new();
        let out = with_rdy(&s, recipe);
        (Project { s }, out)
    }

    /// A fresh project in the pack `rdy-qa` (areas `payments`, `identity`, `glossary`).
    fn qa_pack<T>(extra: &str, recipe: impl FnOnce(&mut Rdy) -> T) -> (Project, T) {
        let s = Served::new();
        let dir = s.dir.path();
        std::fs::write(
            dir.join(".rdy/packs/rdy-qa.yaml"),
            rdy_qa::packs::QA_PACK_YAML,
        )
        .expect("pack");
        std::fs::write(
            dir.join(".rdy.yaml"),
            format!("pack: rdy-qa\n{extra}serve:\n  idle_timeout: 60\n"),
        )
        .expect("config");
        let out = with_rdy(&s, recipe);
        (Project { s }, out)
    }

    fn dir(&self) -> &Path {
        self.s.dir.path()
    }

    /// Runs `f` on the project's engine with the daemon stopped: fixture seeding only.
    fn seed<T>(&self, f: impl FnOnce(&mut Rdy) -> T) -> T {
        with_rdy(&self.s, f)
    }

    /// `rdy --json <args>` as the person: whether it succeeded, and its JSON.
    fn cli(&self, args: &[&str]) -> (bool, Value) {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        self.s.json(&all)
    }

    #[track_caller]
    fn cli_ok(&self, args: &[&str]) -> Value {
        let (ok, v) = self.cli(args);
        assert!(ok, "rdy {args:?}: {v:#}");
        v
    }

    /// The log, newest first, as `rdy log` reads it.
    fn log(&self) -> Vec<Value> {
        let v = self.cli_ok(&["log"]);
        rows_of(&v)
    }

    /// A relaying MCP session over the daemon proxy: `agent:relay`, run `r1`, writer.
    fn agent(&self) -> McpClient {
        let mut c = McpClient::start_proxy(&self.s);
        let ok = c.call(
            "rdy_session",
            json!({"actor": RELAY, "run": "r1", "role": "writer"}),
        );
        assert!(ok.is_ok(), "{ok:?}");
        c
    }

    /// An eliciting MCP session (the 2025-11-25 in-call flow) over the daemon proxy.
    fn eliciting(&self) -> McpClient {
        let mut c = McpClient::try_launch_eliciting(McpClient::proxy(&self.s), in_call())
            .unwrap_or_else(|f| panic!("launch the proxy: {f:?}"));
        let ok = c
            .try_call(
                "rdy_session",
                json!({"actor": RELAY, "run": "r1", "role": "writer"}),
            )
            .expect("rdy_session");
        assert!(ok.is_ok(), "{ok:?}");
        c
    }

    /// The person's queue over MCP, read whole.
    fn mcp_queue(&self) -> Value {
        let mut c = self.agent();
        let q = c.call("rdy_pending", json!({})).ok();
        whole(&q);
        q
    }

    /// The person's queue through `rdy pending`, read whole.
    fn cli_queue(&self) -> Value {
        let q = self.cli_ok(&["pending"]);
        whole(&q);
        q
    }

    /// The base URL of the project's live site (it starts the daemon).
    fn site(&self) -> String {
        let v = self.cli_ok(&["site"]);
        v["url"].as_str().expect("the site's url").to_string()
    }

    /// The form of item `r` on its local approval page.
    fn ui_form(&self, r: &str) -> Option<UiForm> {
        let base = self.site();
        let page = http::get(&format!("{}{}", base.trim_end_matches('/'), item_path(r)));
        if page.status != 200 {
            return None;
        }
        let forms = http::ui_forms(&String::from_utf8_lossy(&page.body));
        forms.into_iter().next()
    }

    /// The card of `r` as `route` shows it to the person.
    fn shown(&self, route: Route, r: &str) -> Option<Shown> {
        match route {
            Route::Relayed | Route::Elicited => shown_in(&self.mcp_queue(), r),
            Route::CliAnswer | Route::CliApprove => shown_in(&self.cli_queue(), r),
            Route::Ui => self.ui_form(r).map(|f| Shown {
                r#ref: f.reference,
                card_hash: f.card_hash,
                options: f.options,
            }),
        }
    }

    /// The person's answer `option` to the card `shown`, through `route`.
    fn answer(&self, route: Route, shown: &Shown, option: &str) -> Outcome {
        match route {
            Route::Relayed => {
                let mut c = self.agent();
                mcp_outcome(c.call(
                    "rdy_answer",
                    json!({
                        "ref": shown.r#ref, "option": option, "card_hash": shown.card_hash,
                        "evidence": WORDS, "question": format!("Shall I answer {}?", shown.r#ref),
                        "session_ref": "chat-1",
                    }),
                ))
            }
            Route::Elicited => {
                let mut c = self.eliciting();
                let choice = option.to_string();
                let mut person = move |req: &Value| {
                    assert_eq!(req["method"], "elicitation/create", "{req}");
                    PersonAct::Answer(json!({"action": "accept", "content": {"choice": choice}}))
                };
                let r = c.try_call_elicited(
                    "rdy_answer",
                    json!({"ref": shown.r#ref, "card_hash": shown.card_hash, "elicit": true}),
                    &mut person,
                );
                match r {
                    Ok(reply) => mcp_outcome(reply),
                    Err(f) => panic!("the elicited exchange failed: {f:?}"),
                }
            }
            Route::Ui => {
                let base = self.site();
                let authority = base
                    .trim_start_matches("http://")
                    .trim_end_matches('/')
                    .to_string();
                let form = UiForm {
                    reference: shown.r#ref.clone(),
                    card_hash: shown.card_hash.clone(),
                    ..UiForm::default()
                };
                let addr = authority
                    .parse::<std::net::SocketAddr>()
                    .expect("a loopback address");
                let r = http::raw(addr, &http::ui_answer(&authority, &form, option));
                let page = String::from_utf8_lossy(&r.body).into_owned();
                if r.status == 303 {
                    Outcome::Done(Value::Null)
                } else {
                    Outcome::Refused {
                        code: ui_code(&page).unwrap_or_else(|| format!("HTTP {}", r.status)),
                        reply: json!({"status": r.status, "page": page}),
                    }
                }
            }
            Route::CliAnswer => {
                cli_outcome(self.cli(&["answer", &shown.r#ref, option, "--card", &shown.card_hash]))
            }
            Route::CliApprove => {
                assert_eq!(option, "approve", "rdy approve only approves");
                cli_outcome(self.cli(&["approve", &shown.r#ref]))
            }
        }
    }

    /// Writes the user config so that the person is `who`, and stops the daemon so the next
    /// command's daemon takes it up.
    fn be(&self, who: &str) {
        self.s.stop();
        std::fs::write(self.s.user_config(), format!("actor: {who}\n")).expect("user config");
    }

    /// The person approves `pref` through `route`: first each confirm item beside it (through
    /// `rdy answer` for the `rdy approve` route), then the version. The landing row.
    #[track_caller]
    fn land_via(&self, route: Route, pref: &str) -> Value {
        self.answer_holds(route, pref);
        let before = self.log().len();
        let (_, out) = self.show_and_answer(route, pref, "approve");
        out.done();
        let log = self.log();
        assert_eq!(log.len(), before + 1, "{route:?}: {pref} landed");
        assert_eq!(log[0]["approved"], pref, "{route:?}");
        log[0].clone()
    }

    /// The person answers, through `route` (`rdy answer` for the `rdy approve` route), each
    /// exception card beside `pref` (confirm) and its deferral card (accept). How many.
    #[track_caller]
    fn answer_holds(&self, route: Route, pref: &str) -> usize {
        let route = if route == Route::CliApprove {
            Route::CliAnswer
        } else {
            route
        };
        let q = self.mcp_queue();
        let mut n = 0;
        for i in q["items"].as_array().unwrap() {
            let option = if i["kind"] == "confirm" && i["batch"] == pref {
                "confirm"
            } else if i["kind"] == "defer" && i["ref"] == format!("defer:{pref}") {
                "o1"
            } else {
                continue;
            };
            let (_, out) = self.show_and_answer(route, i["ref"].as_str().unwrap(), option);
            out.done();
            n += 1;
        }
        n
    }

    /// The card of `r` as `route` shows it, then the person's `option` through `route`.
    #[track_caller]
    fn show_and_answer(&self, route: Route, r: &str, option: &str) -> (Shown, Outcome) {
        let shown = self
            .shown(route, r)
            .unwrap_or_else(|| panic!("{route:?}: {r} is not shown"));
        let out = self.answer(route, &shown, option);
        (shown, out)
    }
}

/// Runs `f` on the served project with the daemon stopped (it holds the store while it runs).
fn with_rdy<T>(s: &Served, f: impl FnOnce(&mut Rdy) -> T) -> T {
    s.stop();
    let mut r = Rdy::open(s.dir.path()).expect("open");
    let out = f(&mut r);
    r.close().expect("close");
    out
}

/// The 2025-11-25 in-call elicitation flow with the form capability.
fn in_call() -> Revision {
    Revision::Initialize {
        version: "2025-11-25".into(),
        capabilities: json!({"elicitation": {"form": {}}}),
    }
}

fn item_path(r: &str) -> String {
    format!("/~ui/item/{}", rdy_engine::site_scope::percent_encode(r))
}

/// The refusal code a UI refusal page names: `<code>E_X</code>`.
fn ui_code(page: &str) -> Option<String> {
    let from = page.find("<code>E_")? + "<code>".len();
    let len = page[from..].find("</code>")?;
    Some(page[from..from + len].to_string())
}

/// The rows of a `rdy log` reply.
fn rows_of(v: &Value) -> Vec<Value> {
    v.as_array()
        .or_else(|| v["landings"].as_array())
        .or_else(|| v["rows"].as_array())
        .unwrap_or_else(|| panic!("a log: {v:#}"))
        .clone()
}

/// Panics unless the queue `q` is whole: nothing left out past its cap.
#[track_caller]
fn whole(q: &Value) {
    assert_eq!(q["omitted"], 0, "the queue is read whole: {q:#}");
}

/// The card of `r` in the queue `q`.
fn shown_in(q: &Value, r: &str) -> Option<Shown> {
    q["items"]
        .as_array()?
        .iter()
        .find(|i| i["ref"] == r)
        .map(|i| Shown {
            r#ref: i["ref"].as_str().unwrap().to_string(),
            card_hash: i["card_hash"].as_str().unwrap().to_string(),
            options: i["options"]
                .as_array()
                .map(|o| {
                    o.iter()
                        .map(|x| x["id"].as_str().unwrap_or_default().to_string())
                        .collect()
                })
                .unwrap_or_default(),
        })
}

fn mcp_outcome(r: Reply) -> Outcome {
    match r {
        Reply::Ok(v) => Outcome::Done(v),
        Reply::ToolError(v) => Outcome::Refused {
            code: v["code"].as_str().unwrap_or_default().to_string(),
            reply: v,
        },
        other => panic!("an MCP reply: {other:?}"),
    }
}

fn cli_outcome((ok, v): (bool, Value)) -> Outcome {
    if ok {
        Outcome::Done(v)
    } else {
        Outcome::Refused {
            code: v["code"].as_str().unwrap_or_default().to_string(),
            reply: v,
        }
    }
}

/// The stored effect of a landing row, with its generated ids and times and its route's
/// evidence fields set aside: what every route must store alike.
fn common(row: &Value) -> Value {
    let mut v = row.clone();
    for k in [
        "landing",
        "prev",
        "proposal",
        "approved_version",
        "approved",
        "assessment",
        "at",
        "at_ms",
        "assurance",
        "relayed_by",
        "relay_evidence",
        "evidence",
    ] {
        v.as_object_mut().unwrap().remove(k);
    }
    v
}

/// The route's own evidence fields of a landing row: what differs by route.
fn provenance(row: &Value) -> Value {
    json!({
        "approved_by": row["approved_by"],
        "assurance": row["assurance"],
        "relayed_by": row["relayed_by"],
        "relay_evidence": row["relay_evidence"],
        "evidence": row["evidence"],
        "policy": row["policy"],
    })
}

/// The provenance `route` stores for the person's approval of `pref` on the card hashed `hash`
/// (the built differences, D-6M0AF, D-WPE4R): a relay names its agent and the person's words;
/// an elicited answer and a UI form are local with their evidence kind and the card's hash; the
/// person's own `rdy answer` and `rdy approve` are local and store no evidence.
fn expected_provenance(route: Route, pref: &str, hash: &str) -> Value {
    let card = |kind: &str| {
        json!({"kind": kind, "options": [format!("Approve {pref}"), "Reject", "Later"],
               "choice": format!("Approve {pref}"), "card_hash": hash})
    };
    let local = |evidence: Value| {
        json!({"approved_by": PERSON, "assurance": "local", "relayed_by": null,
               "relay_evidence": null, "evidence": evidence, "policy": null})
    };
    match route {
        Route::Relayed => json!({
            "approved_by": PERSON, "assurance": "relayed",
            "relayed_by": {"actor": RELAY, "run": "r1"},
            "relay_evidence": {"quoted": WORDS, "session_ref": "chat-1",
                               "question": format!("Shall I answer {pref}?")},
            "evidence": card("card"), "policy": null,
        }),
        Route::Elicited => local(card("elicited")),
        Route::Ui => local(card("card")),
        Route::CliAnswer | Route::CliApprove => local(Value::Null),
    }
}

// ---------------------------------------------------------------- the recipes

/// The ready recipe: canon holds the spec doc; view `one` proposes a new claim (with a
/// summary) for the person, and view `two` an unrelated one. The two refs.
fn ready(r: &mut Rdy) -> (Spec, [ProposalInfo; 2]) {
    let spec = spec_doc(r);
    let mut refs = Vec::new();
    for view in ["one", "two"] {
        write(
            r,
            view,
            json!([{"op": "create", "type": "claim", "parent": spec.section.to_string(),
                    "body": format!("A claim of {view}.")}]),
        )
        .unwrap();
        answer_open_items(r, &writer(), view);
        let p = r
            .propose(&writer(), view, view, Some(TEST_SUMMARY))
            .unwrap();
        assert!(p.ready, "{view}: {:#?}", p.issues);
        refs.push(p);
    }
    let two = refs.pop().unwrap();
    (spec, [refs.pop().unwrap(), two])
}

/// `n` ready versions, each a new claim in its own view and section (no two share a reach).
fn many(r: &mut Rdy, n: usize) -> Vec<ProposalInfo> {
    let spec = spec_doc(r);
    let mut ops = Vec::new();
    for k in 0..n {
        ops.push(
            json!({"op": "create", "type": "section", "id": format!("$s{k}"),
                        "parent": spec.doc.to_string(), "fields": {"title": format!("Part {k}")}}),
        );
    }
    let w = write(r, "parts", json!(ops)).unwrap();
    land(r, "parts").unwrap();
    (0..n)
        .map(|k| {
            let view = format!("v{k}");
            write(
                r,
                &view,
                json!([{"op": "create", "type": "claim", "parent": w.created[&format!("$s{k}")],
                        "body": format!("Claim {k}.")}]),
            )
            .unwrap();
            answer_open_items(r, &writer(), &view);
            let p = r
                .propose(&writer(), &view, &view, Some(TEST_SUMMARY))
                .unwrap();
            assert!(p.ready, "{view}: {:#?}", p.issues);
            p
        })
        .collect()
}

// ---------------------------------------------------------------- mg5 pins

#[test]
fn mg5_all_person_routes_land_only_the_card_the_person_saw() {
    // each route lands exactly the version, assessment and card hash the person saw
    let mut commons = Vec::new();
    for route in ROUTES {
        let (p, (_, [v1, v2])) = Project::new(ready);
        let before = p.log();
        let shown = p.shown(route, &v1.r#ref).expect("shown");
        assert_eq!(
            Some(&shown.card_hash),
            p.shown(Route::Relayed, &v1.r#ref)
                .map(|s| s.card_hash)
                .as_ref(),
            "{route:?} shows the queue's card"
        );
        p.answer(route, &shown, "approve").done();
        let log = p.log();
        assert_eq!(log.len(), before.len() + 1, "{route:?}: one landing");
        let row = &log[0];
        assert_eq!(row["approved"], v1.r#ref, "{route:?}: the version shown");
        assert_eq!(
            row["assessment"],
            json!(v1.assessment),
            "{route:?}: the version's stored assessment"
        );
        assert_eq!(
            provenance(row),
            expected_provenance(route, &v1.r#ref, &shown.card_hash),
            "{route:?}: {row:#}"
        );
        commons.push((route, common(row)));
        // the other version still waits, with its card unmoved
        assert!(p.shown(Route::CliAnswer, &v2.r#ref).is_some(), "{route:?}");
    }
    for (route, c) in &commons[1..] {
        assert_eq!(
            c, &commons[0].1,
            "{route:?} stores what {:?} does",
            commons[0].0
        );
    }

    // a card that changed after it was shown lands nothing, and the route gives the fresh card
    for route in ROUTES {
        let (p, (spec, [v1, _])) = Project::new(ready);
        let shown = p.shown(route, &v1.r#ref).expect("shown");
        let v1b = p.seed(|r| {
            write(
                r,
                "one",
                json!([{"op": "create", "type": "claim", "parent": spec.section.to_string(),
                        "body": "Another claim of one."}]),
            )
            .unwrap();
            answer_open_items(r, &writer(), "one");
            r.propose(&writer(), "one", "one", Some(TEST_SUMMARY))
                .unwrap()
        });
        assert_ne!(v1b.r#ref, v1.r#ref);
        let before = p.log();
        let (code, reply) = p.answer(route, &shown, "approve").refused();
        assert_eq!(code, "E_PROPOSAL_MOVED", "{route:?}: {reply:#}");
        assert_eq!(p.log(), before, "{route:?}: a moved card lands nothing");
        let queued = p.shown(Route::Relayed, &v1b.r#ref).expect("the fresh card");
        let fresh = corrected_of(route, &reply);
        match route {
            Route::CliApprove => {
                assert_eq!(
                    reply["message"],
                    format!(
                        "proposal {} moved: the latest version is {}",
                        v1.r#ref, v1b.r#ref
                    )
                );
                assert!(fresh.is_none(), "rdy approve shows no card: {reply:#}");
            }
            _ => {
                let fresh = fresh.unwrap_or_else(|| panic!("{route:?}: a fresh card: {reply:#}"));
                assert_eq!(
                    (&fresh.r#ref, &fresh.card_hash),
                    (&queued.r#ref, &queued.card_hash),
                    "{route:?}: the fresh card is the queue's"
                );
            }
        }
        let fresh = p.shown(route, &v1b.r#ref).expect("the fresh card");
        p.answer(route, &fresh, "approve").done();
        let log = p.log();
        assert_eq!(log.len(), before.len() + 1);
        assert_eq!(log[0]["approved"], v1b.r#ref, "{route:?}");
    }

    // the same version, with a card that moved under it: an unrelated landing moved canon
    for route in CARD_ROUTES {
        let (p, (_, [v1, v2])) = Project::new(ready);
        let shown = p.shown(route, &v1.r#ref).expect("shown");
        p.seed(|r| r.land(&approver(), &v2.id).map(|_| ()).unwrap());
        let before = p.log();
        let now = p.shown(Route::Relayed, &v1.r#ref).expect("still queued");
        assert_ne!(now.card_hash, shown.card_hash, "{route:?}: the card moved");
        let (code, reply) = p.answer(route, &shown, "approve").refused();
        assert_eq!(code, "E_PROPOSAL_MOVED", "{route:?}: {reply:#}");
        assert_eq!(p.log(), before, "{route:?}: a moved card lands nothing");
        let fresh = corrected_of(route, &reply).expect("a fresh card");
        assert_eq!(fresh.r#ref, now.r#ref, "{route:?}");
        assert_eq!(fresh.card_hash, now.card_hash, "{route:?}");
        p.answer(route, &fresh, "approve").done();
        let log = p.log();
        assert_eq!(log[0]["approved"], v1.r#ref, "{route:?}");
        assert_eq!(
            log[0]["evidence"]["card_hash"].as_str().map(|_| ()),
            match route {
                Route::CliAnswer => None,
                _ => Some(()),
            }
        );
        // the unrelated landing left the version's reach alone: its stored assessment still
        // holds, and only the card the person saw moved
        assert_eq!(log[0]["assessment"], json!(v1.assessment), "{route:?}");
    }

    // the whole queue: past its cap, the person reads and answers every page
    let n = PENDING_ITEMS_MAX + 2;
    let (p, versions) = Project::new(|r| many(r, n));
    let mut c = p.agent();
    let mut landed = Vec::new();
    loop {
        let q = c.call("rdy_pending", json!({})).ok();
        let items: Vec<Value> = q["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["kind"] == "approve")
            .cloned()
            .collect();
        let left = versions.len() - landed.len();
        assert_eq!(items.len(), left.min(PENDING_ITEMS_MAX), "{q:#}");
        assert_eq!(
            q["omitted"],
            left.saturating_sub(PENDING_ITEMS_MAX),
            "{q:#}"
        );
        let Some(i) = items.first() else { break };
        let v = c
            .call(
                "rdy_answer",
                json!({"ref": i["ref"], "option": "approve", "card_hash": i["card_hash"],
                       "evidence": WORDS, "session_ref": "chat-1"}),
            )
            .ok();
        assert_eq!(v["landing"]["approved"], i["ref"], "{v:#}");
        landed.push(i["ref"].as_str().unwrap().to_string());
    }
    drop(c);
    let mut want: Vec<String> = versions.into_iter().map(|v| v.r#ref).collect();
    want.sort();
    landed.sort();
    assert_eq!(landed, want, "every page was answered");
    let log = p.log();
    assert!(
        log.iter()
            .take(n)
            .all(|row| row["assurance"] == "relayed" && row["approved_by"] == PERSON),
        "{log:#?}"
    );
}

/// The fresh card a refusal of `route` supplies: `corrected` of a reply, or the form of the UI's
/// refusal page.
fn corrected_of(route: Route, reply: &Value) -> Option<Shown> {
    match route {
        Route::Ui => http::ui_forms(reply["page"].as_str()?)
            .into_iter()
            .next()
            .map(|f| Shown {
                r#ref: f.reference,
                card_hash: f.card_hash,
                options: f.options,
            }),
        _ => {
            let c = reply.get("corrected")?;
            let c = if c["ref"].is_string() { c } else { &c["card"] };
            Some(Shown {
                r#ref: c["ref"].as_str()?.to_string(),
                card_hash: c["card_hash"].as_str()?.to_string(),
                options: Vec::new(),
            })
        }
    }
}

/// The person a policy role names; [`PERSON`] is in none of its roles of humans.
const BOSS: &str = "human:boss";

/// The queue as `who` sees it, read from the engine (fixture reads only).
fn queue_for(r: &Rdy, who: &str) -> Pending {
    r.read()
        .pending(&PendingQuery {
            person: who.into(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .expect("pending")
}

/// The card of `r#ref` in `who`'s queue, as the engine renders it.
fn card_for(r: &Rdy, who: &str, r#ref: &str) -> Shown {
    let i = queue_for(r, who)
        .items
        .into_iter()
        .find(|i| i.r#ref == r#ref)
        .unwrap_or_else(|| panic!("no item {ref} for {who}"));
    Shown {
        r#ref: i.r#ref,
        card_hash: i.card_hash,
        options: i.options.iter().map(|o| o.id.clone()).collect(),
    }
}

/// Lands a doc `rules` holding a decision that adopts the policy and the authority policy
/// `fields`, whose change implements the decision, as the person (no policy is live yet). The
/// policy block's id and the rules doc's id.
fn land_policy(r: &mut Rdy, fields: Value) -> (String, String) {
    let w = write(
        r,
        "pol",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "rules", "title": "Rules", "doc_type": "page"}},
            {"op": "create", "type": "decision", "id": "$dec", "parent": "$d", "fields": {"statement": "Adopt the policy", "status": "proposed", "needs_content": false}}
        ]),
    )
    .unwrap_or_else(|e| panic!("write the rules: {e}"));
    let pol = r
        .write(
            &writer(),
            WriteRequest {
                view: "pol".into(),
                ops: parse_test_ops(json!([
                    {"op": "create", "type": "authority_policy", "id": "$p", "parent": w.created["$d"], "fields": fields}
                ]))
                .unwrap(),
                justification: Some(Justification::Implements(vec![
                    BlockId::parse(&w.created["$dec"]).unwrap(),
                ])),
                message: String::new(),
            },
        )
        .unwrap_or_else(|e| panic!("write the policy: {e}"));
    land(r, "pol").unwrap_or_else(|e| panic!("land the policy: {e}"));
    (pol.created["$p"].clone(), w.created["$d"].clone())
}

/// Lands folder `name` with doc `<name>-doc`, a section and a claim: (section, claim).
fn folder_doc(r: &mut Rdy, name: &str) -> (String, String) {
    let w = write(
        r,
        name,
        json!([
            {"op": "create", "type": "folder", "id": "$f", "fields": {"name": name}},
            {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"name": format!("{name}-doc"), "title": name, "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Body"}},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "It stands."}
        ]),
    )
    .unwrap_or_else(|e| panic!("write {name}: {e}"));
    land(r, name).unwrap_or_else(|e| panic!("land {name}: {e}"));
    (w.created["$s"].clone(), w.created["$c"].clone())
}

/// Proposes `view` as the writer with a summary; it must be ready.
fn propose_ready(r: &mut Rdy, view: &str) -> ProposalInfo {
    answer_open_items(r, &writer(), view);
    let p = r
        .propose(&writer(), view, view, Some(TEST_SUMMARY))
        .unwrap_or_else(|e| panic!("propose {view}: {e}"));
    assert!(p.ready, "{view}: {:#?}", p.issues);
    p
}

/// The role recipe: a policy whose decisions need `approvers` ([`BOSS`]), whose owned blocks
/// need their owners (folder `owned` is [`BOSS`]'s; its doc `owned-doc` is [`PERSON`]'s, the
/// more specific entry), and whose other items need `core`, a local team. One version per case.
struct Roles {
    /// A decision: role approvers.
    decision: ProposalInfo,
    /// An edit inside `owned/owned-doc`: the doc's owner entry wins over the folder's.
    specific: ProposalInfo,
    /// An edit inside `owned/side-doc`: the folder's owners.
    folder: ProposalInfo,
    /// A move from `owned` to `free`: the old path's owners, read in canon.
    move_out: ProposalInfo,
    /// A move from `free` into `owned`: the new path's owners, read in the prospective canon.
    move_in: ProposalInfo,
    /// The version adds [`PERSON`] to `approvers` in its own edit of the policy: canon routes it.
    widen: ProposalInfo,
    /// An edit inside `free`: no owner, so the `core` team, which holds the person locally.
    team: ProposalInfo,
}

fn roles_policy() -> Value {
    json!({
        "key": "authority-policy",
        "agent_may_accept": [],
        "roles": {"approvers": [BOSS], "core": ["team:core"]},
        "require": [
            {"when": "decision", "role": "approvers"},
            {"when": "owner"},
            {"when": "any", "role": "core"}
        ],
        "owners": [
            {"path": "owned", "owners": [BOSS]},
            {"path": "owned/owned-doc", "owners": [PERSON]}
        ]
    })
}

fn roles_world(r: &mut Rdy) -> Roles {
    let (free_s, free_c) = folder_doc(r, "free");
    let (owned_s, owned_c) = folder_doc(r, "owned");
    let w = write(
        r,
        "side",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "parent": owned_s_folder(r, &owned_s), "fields": {"name": "side-doc", "title": "Side", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Body"}},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Side stands."},
            {"op": "create", "type": "claim", "id": "$m", "parent": "$s", "body": "Side moves."}
        ]),
    )
    .unwrap();
    land(r, "side").unwrap();
    let (side_s, side_c, side_m) = (
        w.created["$s"].clone(),
        w.created["$c"].clone(),
        w.created["$m"].clone(),
    );
    let (block, rules) = land_policy(r, roles_policy());
    let decision = {
        r.decide(
            &writer(),
            "decision",
            DecisionInput {
                parent: free_s.clone(),
                statement: "Adopt the free plan".into(),
                reversibility: Some("two_way".into()),
                ..Default::default()
            },
        )
        .unwrap();
        propose_ready(r, "decision")
    };
    let edit = |r: &mut Rdy, view: &str, id: &str| {
        write(
            r,
            view,
            json!([{"op": "update", "id": id, "body": format!("Edited in {view}.")}]),
        )
        .unwrap();
        propose_ready(r, view)
    };
    let mv = |r: &mut Rdy, view: &str, id: &str, parent: &str| {
        write(r, view, json!([{"op": "move", "id": id, "parent": parent}])).unwrap();
        propose_ready(r, view)
    };
    let specific = edit(r, "specific", &owned_c);
    let folder = edit(r, "folder", &side_c);
    let move_out = mv(r, "move-out", &side_m, &free_s);
    let move_in = mv(r, "move-in", &free_c, &side_s);
    let team = {
        write(
            r,
            "team",
            json!([{"op": "create", "type": "claim", "parent": free_s, "body": "Team stands."}]),
        )
        .unwrap();
        propose_ready(r, "team")
    };
    let widen = {
        let dec = write(
            r,
            "widen",
            json!([{"op": "create", "type": "decision", "id": "$dec", "parent": rules,
                    "fields": {"statement": "Widen the approvers", "status": "proposed", "needs_content": false}}]),
        )
        .unwrap();
        let mut wider = roles_policy();
        wider["roles"]["approvers"] = json!([BOSS, PERSON]);
        r.write(
            &writer(),
            WriteRequest {
                view: "widen".into(),
                ops: parse_test_ops(json!([{"op": "update", "id": block, "fields": wider}]))
                    .unwrap(),
                justification: Some(Justification::Implements(vec![
                    BlockId::parse(&dec.created["$dec"]).unwrap(),
                ])),
                message: String::new(),
            },
        )
        .unwrap();
        propose_ready(r, "widen")
    };
    Roles {
        decision,
        specific,
        folder,
        move_out,
        move_in,
        widen,
        team,
    }
}

/// The folder above section `s`.
fn owned_s_folder(r: &Rdy, s: &str) -> String {
    let st = r.read();
    let canon = st.materialized(&rdy_engine::Scope::Canon).unwrap();
    let doc = canon
        .get(&BlockId::parse(s).unwrap())
        .unwrap()
        .parent
        .clone()
        .unwrap();
    canon.get(&doc).unwrap().parent.clone().unwrap().to_string()
}

/// The refusal of `who` outside `role` for item `r` (D-TF78H), as each route gives it.
#[track_caller]
fn assert_outside(route: Route, code: &str, reply: &Value, who: &str, role: &str, r: &str) {
    assert_eq!(code, "E_PERMISSION", "{route:?}: {reply:#}");
    let message = format!("{who} is not in the policy role {role} for {r}");
    let fix = format!("ask a person in the policy role {role}, or relay their answer");
    if route == Route::Ui {
        let page = reply["page"].as_str().unwrap();
        for text in [&message, &fix] {
            assert!(
                page.contains(&rdy_render::escape(text).to_string()),
                "{route:?}: {text} in {page}"
            );
        }
    } else {
        assert_eq!(reply["message"], message, "{route:?}: {reply:#}");
        assert_eq!(reply["fix"], fix, "{route:?}: {reply:#}");
    }
}

#[test]
fn mg5_roles_route_and_enforce_the_same_person() {
    for route in ROUTES {
        let (p, w) = Project::new(roles_world);
        // the role each version needs, and whether the person holds it
        let cases: [(&str, &ProposalInfo, &str, bool); 7] = [
            ("decision", &w.decision, "approvers", false),
            ("specific", &w.specific, "owners of owned/owned-doc", true),
            ("folder", &w.folder, "owners of owned", false),
            ("move-out", &w.move_out, "owners of owned", false),
            ("move-in", &w.move_in, "owners of owned", false),
            ("widen", &w.widen, "approvers", false),
            ("team", &w.team, "core", true),
        ];
        for (name, v, role, _) in &cases {
            assert_eq!(
                v.policy.as_ref().and_then(|a| a.role.as_deref()),
                Some(*role),
                "{name}: {:#?}",
                v.policy
            );
        }
        // the person sees only the items whose role holds them, on this route as on the queue
        let boss_cards: Vec<Option<Shown>> = p.seed(|r| {
            cases
                .iter()
                .map(|(_, v, _, holds)| (!holds).then(|| card_for(r, BOSS, &v.r#ref)))
                .collect()
        });
        for ((name, v, role, holds), card) in cases.iter().zip(&boss_cards) {
            let shown = p.shown(route, &v.r#ref);
            assert_eq!(shown.is_some(), *holds, "{route:?} {name}: {shown:?}");
            let Some(card) = card else { continue };
            // the card the role's person saw, answered by the person outside it
            let before = p.log();
            let (code, reply) = p.answer(route, card, "approve").refused();
            assert_outside(route, &code, &reply, PERSON, role, &v.r#ref);
            assert_eq!(p.log(), before, "{route:?} {name}: nothing lands");
        }
        // the person lands the items their role holds: owner specificity and the local team
        for (name, v, _, holds) in &cases {
            if *holds {
                let row = p.land_via(route, &v.r#ref);
                assert_eq!(row["approved_by"], PERSON, "{route:?} {name}");
            }
        }
        // the role's person sees and lands the rest through the same route
        p.be(BOSS);
        for (name, v, _, holds) in &cases {
            if !*holds {
                let row = p.land_via(route, &v.r#ref);
                assert_eq!(row["approved_by"], BOSS, "{route:?} {name}");
            }
        }
    }
}

// ---------------------------------------------------------------- the policy route

/// The policy of two rules, each bounded on every axis. `drafts`: agents `agent:a*` may land
/// draft, editorial changes with no decision and at most two blocks. `decides`: the same agents
/// may land draft, editorial changes with `two_way` decisions in area `payments` on basis
/// `precedent`, at most one block. Decisions otherwise need a person from `approvers`.
fn rules_policy() -> Value {
    json!({
        "key": "authority-policy",
        "agent_may_accept": [
            {"name": "drafts", "actors": ["agent:a*"], "lifecycle": "draft",
             "justifications": ["editorial"], "decisions": "none",
             "max_blocks": 2, "max_items": 20},
            {"name": "decides", "actors": ["agent:a*"], "lifecycle": "draft",
             "justifications": ["editorial"], "decisions": "two_way",
             "two_way_areas": ["payments"], "basis": ["precedent"],
             "max_blocks": 1, "max_items": 20}
        ],
        "roles": {"approvers": [PERSON]},
        "require": [{"when": "decision", "role": "approvers"}],
        "owners": []
    })
}

/// The policy recipe's canon ids and versions, one version per axis of the rules.
struct Policed {
    policy: String,
    rules: String,
    /// Accepted by `drafts`: a typo fix of claim x.
    draft: ProposalInfo,
    /// Accepted by `decides`: a payments decision on basis precedent.
    decides: ProposalInfo,
    /// Accepted by `drafts`, for a lander outside the rules' actors.
    actor: ProposalInfo,
    /// A decision outside area payments.
    area: ProposalInfo,
    /// A payments decision on basis elimination.
    basis: ProposalInfo,
    /// A typo fix of a claim in a stable doc.
    lifecycle: ProposalInfo,
    /// An edit that implements a decision: not editorial.
    justification: ProposalInfo,
    /// Three new claims: past both rules' limits.
    limits: ProposalInfo,
    /// A typo fix and a payments decision: two blocks with a decision. Each part fits a rule,
    /// and no one rule fits the whole version.
    split: ProposalInfo,
}

fn decision_op(parent: &str, statement: &str, basis: &str) -> Value {
    json!([{"op": "create", "type": "decision", "parent": parent,
            "fields": {"statement": statement, "status": "proposed", "needs_content": false,
                       "reversibility": "two_way", "authority_basis": basis}}])
}

fn policed(r: &mut Rdy) -> Policed {
    use rdy_qa::tensions::{j_typo, put_j, upd};
    let w = write(
        r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$n", "fields": {"title": "Ideas"}},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$s", "body": "X holds."},
            {"op": "create", "type": "claim", "id": "$y", "parent": "$s", "body": "Y holds."},
            {"op": "create", "type": "doc", "id": "$w", "fields": {"name": "wallets", "title": "Wallets", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$ws", "parent": "$w", "fields": {"title": "Fees"}},
            {"op": "create", "type": "doc", "id": "$od", "fields": {"name": "other", "title": "Other", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$os", "parent": "$od", "fields": {"title": "More"}},
            {"op": "create", "type": "claim", "id": "$o", "parent": "$os", "body": "O holds."},
            {"op": "create", "type": "doc", "id": "$sd", "fields": {"name": "fixed", "title": "Fixed", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$ss", "parent": "$sd", "fields": {"title": "Fixed"}},
            {"op": "create", "type": "claim", "id": "$f", "parent": "$ss", "body": "F holds."},
            {"op": "create", "type": "doc", "id": "$md", "fields": {"name": "many", "title": "Many", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$ms", "parent": "$md", "fields": {"title": "Many"}}
        ]),
    )
    .unwrap();
    land(r, "seed").unwrap();
    let get = |k: &str| w.created[k].clone();
    put_j(
        r,
        "stable",
        json!([{"op": "update", "id": get("$sd"), "fields": {"lifecycle": "stable"}}]),
        j_typo(),
    );
    land(r, "stable").unwrap();
    let (policy, rules) = land_policy(r, rules_policy());
    let rules_decision = {
        let st = r.read();
        let canon = st.materialized(&rdy_engine::Scope::Canon).unwrap();
        let doc = BlockId::parse(&rules).unwrap();
        canon
            .iter()
            .find(|b| b.parent.as_ref() == Some(&doc) && b.block_type == "decision")
            .map(|b| b.id.clone())
            .expect("the rules decision")
    };
    let agent_version = |r: &mut Rdy, name: &str| {
        let p = r
            .propose(&rdy_qa::tensions::ag(name), name, name, Some(TEST_SUMMARY))
            .unwrap_or_else(|e| panic!("propose {name}: {e}"));
        assert!(p.ready, "{name}: {:#?}", p.issues);
        p
    };
    let x = BlockId::parse(&get("$x")).unwrap();
    let y = BlockId::parse(&get("$y")).unwrap();
    let f = BlockId::parse(&get("$f")).unwrap();
    let o = BlockId::parse(&get("$o")).unwrap();
    put_j(r, "a-draft", upd(&x, "X holds, now."), j_typo());
    let draft = agent_version(r, "a-draft");
    put_j(
        r,
        "a-decides",
        decision_op(&get("$ws"), "Fees round up", "precedent"),
        j_typo(),
    );
    let decides = agent_version(r, "a-decides");
    put_j(r, "a-actor", upd(&o, "O holds, now."), j_typo());
    let actor = agent_version(r, "a-actor");
    put_j(
        r,
        "a-area",
        decision_op(&get("$os"), "More stays short", "precedent"),
        j_typo(),
    );
    let area = agent_version(r, "a-area");
    put_j(
        r,
        "a-basis",
        decision_op(&get("$ws"), "Fees round down", "elimination"),
        j_typo(),
    );
    let basis = agent_version(r, "a-basis");
    put_j(r, "a-life", upd(&f, "F holds, now."), j_typo());
    let lifecycle = agent_version(r, "a-life");
    put_j(
        r,
        "a-just",
        upd(&y, "Y holds, as decided."),
        Some(Justification::Implements(vec![rules_decision])),
    );
    let justification = agent_version(r, "a-just");
    let ms = get("$ms");
    put_j(
        r,
        "a-limits",
        json!([
            {"op": "create", "type": "claim", "parent": ms, "body": "One."},
            {"op": "create", "type": "claim", "parent": ms, "body": "Two."},
            {"op": "create", "type": "claim", "parent": ms, "body": "Three."}
        ]),
        j_typo(),
    );
    let limits = agent_version(r, "a-limits");
    put_j(r, "a-split", upd(&y, "Y holds, now."), j_typo());
    put_j(
        r,
        "a-split",
        decision_op(&get("$ws"), "Fees stay low", "precedent"),
        j_typo(),
    );
    let split = agent_version(r, "a-split");
    Policed {
        policy,
        rules,
        draft,
        decides,
        actor,
        area,
        basis,
        lifecycle,
        justification,
        limits,
        split,
    }
}

/// The live policy block's version, read from the engine.
fn live_version(r: &Rdy) -> String {
    let st = r.read();
    let canon = st.materialized(&rdy_engine::Scope::Canon).unwrap();
    rdy_engine::policy::live_policy(&st, &canon)
        .expect("a live policy")
        .version
        .to_string()
}

/// The policy verdict the store names for version `p` now.
fn verdict_of(r: &Rdy, p: &ProposalInfo) -> PolicyVerdict {
    let st = r.read();
    let a = st
        .reader()
        .assessment_of_proposal(&p.id.parse().unwrap(), st.seq())
        .unwrap()
        .unwrap_or_else(|| panic!("{} stores an assessment", p.r#ref));
    st.reader()
        .assessment(&a)
        .unwrap()
        .policy
        .expect("a policy verdict")
        .verdict
}

/// `agent:<name>` lands `pref` through `rdy_land` over the daemon proxy.
fn rdy_land(p: &Project, name: &str, pref: &str) -> Outcome {
    let mut c = McpClient::start_proxy(&p.s);
    let ok = c.call(
        "rdy_session",
        json!({"actor": format!("agent:{name}"), "run": name, "role": "writer"}),
    );
    assert!(ok.is_ok(), "{ok:?}");
    mcp_outcome(c.call("rdy_land", json!({"proposal": pref})))
}

const POLICY_HUMAN_FIX: &str =
    "show the card to the person and relay their answer: rdy_pending, then rdy_answer";

#[test]
fn mg5_policy_uses_one_rule_for_the_whole_version() {
    let (p, w) = Project::qa_pack("", policed);
    let (live, verdicts) = p.seed(|r| {
        let all = [
            &w.draft,
            &w.decides,
            &w.actor,
            &w.area,
            &w.basis,
            &w.lifecycle,
            &w.justification,
            &w.limits,
            &w.split,
        ];
        (
            live_version(r),
            all.map(|v| (v.r#ref.clone(), verdict_of(r, v))),
        )
    });
    let accepts = |rule: &str| PolicyVerdict::Accepts { rule: rule.into() };
    assert_eq!(verdicts[0].1, accepts("drafts"));
    assert_eq!(verdicts[1].1, accepts("decides"));
    assert_eq!(verdicts[2].1, accepts("drafts"));
    // one rule must fit the whole version: each refusal names every rule's first misfit
    let both = |a: &str, b: &str| vec![format!("rule drafts: {a}"), format!("rule decides: {b}")];
    let refused: [(&str, Option<&str>, Vec<String>); 6] = [
        (
            "area",
            Some("approvers"),
            both(
                "it records decision D-<id>",
                "decision D-<id> is outside its two_way_areas",
            ),
        ),
        (
            "basis",
            Some("approvers"),
            both(
                "it records decision D-<id>",
                "decision D-<id> has basis elimination, which it does not accept",
            ),
        ),
        (
            "lifecycle",
            None,
            both(
                "block c-<id> is in stable content",
                "block c-<id> is in stable content",
            ),
        ),
        (
            "justification",
            None,
            both(
                "change <id> is implements, which it does not accept",
                "change <id> is implements, which it does not accept",
            ),
        ),
        (
            "limits",
            None,
            both(
                "it changes 3 blocks; the rule allows 2",
                "it changes 3 blocks; the rule allows 1",
            ),
        ),
        (
            "split",
            Some("approvers"),
            both(
                "it records decision D-<id>",
                "it changes 2 blocks; the rule allows 1",
            ),
        ),
    ];
    let versions = [
        &w.area,
        &w.basis,
        &w.lifecycle,
        &w.justification,
        &w.limits,
        &w.split,
    ];
    for (((name, role, want), (pref, verdict)), v) in
        refused.iter().zip(&verdicts[3..]).zip(versions)
    {
        let PolicyVerdict::NeedsPerson { role: got, reasons } = verdict else {
            panic!("{name}: {verdict:?}");
        };
        assert_eq!(got.as_deref(), *role, "{name}");
        assert_eq!(
            reasons.iter().map(|r| ids_out(r)).collect::<Vec<_>>(),
            *want,
            "{name}"
        );
        // the policy route refuses it with the stored reasons, and lands nothing
        let before = p.log();
        let (code, e) = rdy_land(&p, "a1", pref).refused();
        assert_eq!(code, "E_POLICY_HUMAN", "{name}: {e:#}");
        assert_eq!(
            e["message"],
            format!(
                "the authority policy needs a person for {}: {}",
                v.r#ref,
                reasons.join("; ")
            ),
            "{name}"
        );
        assert_eq!(e["fix"], POLICY_HUMAN_FIX, "{name}");
        assert_eq!(p.log(), before, "{name}: nothing lands");
    }
    // the lander is the actor the rule names
    let before = p.log();
    let (code, e) = rdy_land(&p, "b1", &w.actor.r#ref).refused();
    assert_eq!(code, "E_POLICY_HUMAN", "{e:#}");
    assert_eq!(
        e["message"],
        format!(
            "the authority policy needs a person for {}: actor agent:b1 matches none of rule drafts's actors",
            w.actor.r#ref
        )
    );
    assert_eq!(p.log(), before);

    // an accepted version lands under the exact live policy version, with policy provenance
    let row = {
        rdy_land(&p, "a1", &w.draft.r#ref).done();
        p.log().remove(0)
    };
    let policy_row = |row: &Value, version: &str| {
        assert_eq!(
            provenance(row),
            json!({"approved_by": "agent:a1", "assurance": "policy", "relayed_by": null,
                   "relay_evidence": null, "evidence": null,
                   "policy": {"block": w.policy, "version": version}}),
            "{row:#}"
        );
    };
    assert_eq!(row["approved"], w.draft.r#ref);
    assert_eq!(row["assessment"], json!(w.draft.assessment));
    policy_row(&row, &live);
    // the draft's landing was an unrelated canon landing for `decides`: it still lands as assessed
    rdy_land(&p, "a1", &w.decides.r#ref).done();
    let row = p.log().remove(0);
    assert_eq!(row["approved"], w.decides.r#ref);
    assert_eq!(
        row["assessment"],
        json!(w.decides.assessment),
        "no fresh card"
    );
    policy_row(&row, &live);
    // a person approves a version the policy refused: the person's provenance, no policy
    let row = p.land_via(Route::Relayed, &w.area.r#ref);
    assert_eq!(
        provenance(&row)["assurance"],
        "relayed",
        "the person's relayed approval: {row:#}"
    );
    assert_eq!(row["policy"], Value::Null);

    // the person changes the policy: a version assessed under the old one moves
    let change = p.seed(|r| {
        let dec = write(
            r,
            "chg",
            json!([{"op": "create", "type": "decision", "id": "$dec", "parent": w.rules,
                    "fields": {"statement": "Widen the policy", "status": "proposed", "needs_content": false}}]),
        )
        .unwrap();
        let mut fields = rules_policy();
        fields["agent_may_accept"][0]["max_items"] = json!(30);
        r.write(
            &writer(),
            WriteRequest {
                view: "chg".into(),
                ops: parse_test_ops(json!([{"op": "update", "id": w.policy, "fields": fields}]))
                    .unwrap(),
                justification: Some(Justification::Implements(vec![
                    BlockId::parse(&dec.created["$dec"]).unwrap(),
                ])),
                message: String::new(),
            },
        )
        .unwrap();
        propose_ready(r, "chg")
    });
    p.land_via(Route::CliApprove, &change.r#ref);
    let after = p.seed(|r| live_version(r));
    assert_ne!(after, live, "a new policy version");
    let before = p.log();
    let (code, e) = rdy_land(&p, "a1", &w.actor.r#ref).refused();
    assert_eq!(code, "E_PROPOSAL_MOVED", "{e:#}");
    assert_eq!(
        e["message"],
        format!(
            "proposal {} moved: the authority policy changed after its assessment",
            w.actor.r#ref
        )
    );
    assert_eq!(
        e["corrected"]["card"]["ref"], w.actor.r#ref,
        "a fresh card: {e:#}"
    );
    assert_eq!(p.log(), before, "a changed policy lands nothing");
    // the next landing, through the CLI's policy route, is under the new version
    let (ok, v) =
        p.s.json_as("agent:a1", Some("a1"), &["--json", "land", &w.actor.r#ref]);
    assert!(ok, "{v:#}");
    let row = p.log().remove(0);
    assert_eq!(row["approved"], w.actor.r#ref);
    assert_ne!(
        row["assessment"],
        json!(w.actor.assessment),
        "the fresh assessment"
    );
    policy_row(&row, &after);
}

/// `text` with its generated ids (`D-…`, `c-…` and short change hashes) as `<id>`.
fn ids_out(text: &str) -> String {
    let re = regex::Regex::new(r"\b(?:[0-9a-f]{12}|(?:D|c|d)-[0-9A-Z]{5})\b").unwrap();
    re.replace_all(text, |c: &regex::Captures| {
        let m = &c[0];
        if let Some(p) = m.find('-') {
            format!("{}-<id>", &m[..p])
        } else {
            "<id>".to_string()
        }
    })
    .into_owned()
}

// ---------------------------------------------------------------- the never-list

/// A policy whose one rule would accept any agent's two_way payments decision on any common
/// basis, in any lifecycle, with any justification: only the never-list sends a version to the
/// person.
fn open_policy() -> Value {
    json!({
        "key": "authority-policy",
        "agent_may_accept": [
            {"name": "open", "actors": ["agent:*"], "lifecycle": "any",
             "justifications": ["implements", "editorial", "reflects"], "decisions": "two_way",
             "two_way_areas": ["payments"],
             "basis": ["delegated", "precedent", "elimination", "none"],
             "max_blocks": 50, "max_items": 50}
        ],
        "roles": {"approvers": [PERSON]},
        "require": [{"when": "decision", "role": "approvers"}],
        "owners": []
    })
}

/// An export tree at `out`: doc `name` (ids `doc`, `section`) with one accepted two_way
/// decision `decision` on basis precedent, all by agent:src.
fn export_tree(out: &Path, name: &str, doc: &str, section: &str, decision: &str) {
    std::fs::create_dir_all(out.join("docs")).unwrap();
    std::fs::write(
        out.join("rdy-export.yaml"),
        "format: 3\npack: rdy-standard\nscope: work\n",
    )
    .unwrap();
    let docs = format!(
        "- id: {doc}\n  type: doc\n  order: a1\n  fields:\n    doc_type: overview\n    key: {name}\n    title: Imported\n\
         - id: {section}\n  type: section\n  parent: {doc}\n  order: a1\n  fields:\n    title: Rulings\n\
         - id: {decision}\n  type: decision\n  parent: {section}\n  order: a1\n  fields:\n    authority_basis: precedent\n    needs_content: false\n    reversibility: two_way\n    statement: Imported holds\n    status: accepted\n"
    );
    std::fs::write(out.join("docs").join(format!("{name}.yaml")), docs).unwrap();
    let mut authors = String::new();
    for id in [doc, section, decision] {
        authors.push_str(&format!(
            "- id: {id}\n  actor: agent:src\n  run: s1\n  role: writer\n  at_ms: 100\n"
        ));
    }
    std::fs::write(out.join("authors.yaml"), authors).unwrap();
}

/// One never-list case: its name, the version, the code `rdy_land` gives and the reasons.
struct Never {
    name: &'static str,
    version: ProposalInfo,
    /// The latest version, when the case moved it.
    latest: Option<ProposalInfo>,
    code: &'static str,
}

fn never_world(r: &mut Rdy) -> Vec<Never> {
    use rdy_engine::plan::{PlanAnswerInput, ProposePlanRequest};
    use rdy_model::plan::{PlanAct, PlanKind, item_display, item_hash};
    use rdy_qa::tensions::{ag, j_typo, put_j, upd};
    let w = write(
        r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$n", "fields": {"title": "Ideas"}},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$s", "body": "X holds."},
            {"op": "create", "type": "claim", "id": "$y", "parent": "$s", "body": "Y builds on {{$x}}."},
            {"op": "create", "type": "doc", "id": "$w", "fields": {"name": "wallets", "title": "Wallets", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$ws", "parent": "$w", "fields": {"title": "Fees"}},
            {"op": "create", "type": "doc", "id": "$od", "fields": {"name": "other", "title": "Other", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$os", "parent": "$od", "fields": {"title": "More"}},
            {"op": "create", "type": "claim", "id": "$o", "parent": "$os", "body": "O holds."},
            {"op": "create", "type": "claim", "id": "$o2", "parent": "$os", "body": "O2 holds."},
            {"op": "create", "type": "doc", "id": "$kd", "fields": {"name": "keys", "title": "Keys", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$ks", "parent": "$kd", "fields": {"title": "Keys"}},
            {"op": "create", "type": "prose", "id": "$secret", "parent": "$ks", "body": "The code is 1234."},
            {"op": "create", "type": "doc", "id": "$pd", "fields": {"name": "plain", "title": "Plain", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$ps", "parent": "$pd", "fields": {"title": "Plain"}},
            {"op": "create", "type": "claim", "id": "$pc", "parent": "$ps", "body": "P holds."}
        ]),
    )
    .unwrap();
    land(r, "seed").unwrap();
    let get = |k: &str| w.created[k].clone();
    let bid = |k: &str| BlockId::parse(&get(k)).unwrap();
    let canon_decision = {
        put_j(
            r,
            "dd",
            decision_op(&get("$ws"), "Fees are flat", "precedent"),
            j_typo(),
        );
        let d = r.read().included("dd").to_vec();
        let _ = d;
        land(r, "dd").unwrap();
        let st = r.read();
        let canon = st.materialized(&rdy_engine::Scope::Canon).unwrap();
        canon
            .iter()
            .find(|b| {
                b.block_type == "decision" && b.field_str("statement") == Some("Fees are flat")
            })
            .map(|b| b.id.clone())
            .unwrap()
    };
    let (policy, rules) = land_policy(r, open_policy());
    let rules_decision = |r: &mut Rdy, view: &str, statement: &str| {
        let d = write(
            r,
            view,
            json!([{"op": "create", "type": "decision", "id": "$dec", "parent": rules,
                    "fields": {"statement": statement, "status": "proposed", "needs_content": false}}]),
        )
        .unwrap();
        BlockId::parse(&d.created["$dec"]).unwrap()
    };
    let agent_version = |r: &mut Rdy, name: &str, summary: Option<&str>| {
        answer_open_items(r, &ag(name), name);
        r.propose(&ag(name), name, name, summary)
            .unwrap_or_else(|e| panic!("propose {name}: {e}"))
    };
    let mut out = Vec::new();
    let mut case = |name: &'static str, version: ProposalInfo, code: &'static str| {
        out.push(Never {
            name,
            version,
            latest: None,
            code,
        });
    };
    // not ready: no summary
    put_j(r, "n-bare", upd(&bid("$o"), "O holds, now."), j_typo());
    let bare = agent_version(r, "n-bare", None);
    assert!(!bare.ready);
    case("not_ready", bare, "E_PROPOSAL_NOT_READY");
    // a one_way decision, a decision against a live one, and one that no content applies
    put_j(
        r,
        "n-oneway",
        json!([{"op": "create", "type": "decision", "parent": get("$ws"),
                "fields": {"statement": "Fees never fall", "status": "proposed", "needs_content": false,
                           "reversibility": "one_way", "authority_basis": "precedent"}}]),
        j_typo(),
    );
    let v = agent_version(r, "n-oneway", Some(TEST_SUMMARY));
    case("one_way", v, "E_POLICY_HUMAN");
    rdy_qa::intake::supersede_in(
        r,
        "n-against",
        &canon_decision,
        DecisionInput {
            parent: get("$ws"),
            statement: "Fees are tiered".into(),
            reversibility: Some("two_way".into()),
            authority_basis: Some("precedent".into()),
            ..Default::default()
        },
    );
    let v = agent_version(r, "n-against", Some(TEST_SUMMARY));
    case("against", v, "E_POLICY_HUMAN");
    put_j(
        r,
        "n-unapplied",
        json!([{"op": "create", "type": "decision", "parent": get("$ws"),
                "fields": {"statement": "Fees are shown", "status": "proposed",
                           "reversibility": "two_way", "authority_basis": "precedent"}}]),
        j_typo(),
    );
    let v = agent_version(r, "n-unapplied", Some(TEST_SUMMARY));
    case("unapplied", v, "E_POLICY_HUMAN");
    // a deferral of an item that is not a code item
    put_j(r, "n-defer", upd(&bid("$x"), "X holds 60 days."), j_typo());
    let item = item_display(&item_hash(&bid("$x"), &get("$y"), PlanKind::Confirm));
    let answers = [PlanAnswerInput {
        item,
        act: PlanAct::Defer,
        reason: Some("Y waits for the review".into()),
    }];
    let v = r
        .propose_plan(
            &ag("n-defer"),
            ProposePlanRequest {
                summary: Some(TEST_SUMMARY),
                plan_answers: &answers,
                ..ProposePlanRequest::new("n-defer", "n-defer")
            },
        )
        .unwrap();
    case("deferral", v, "E_POLICY_HUMAN");
    // a person's ruling, direct and recorded
    for (name, basis) in [
        ("n-direct", "direct_ruling"),
        ("n-recorded", "recorded_ruling"),
    ] {
        put_j(
            r,
            name,
            json!([{"op": "create", "type": "decision", "parent": get("$ws"),
                    "fields": {"statement": format!("Fees follow the {basis}"), "status": "proposed",
                               "needs_content": false, "reversibility": "two_way",
                               "authority_basis": basis, "attested_by": PERSON}}]),
            j_typo(),
        );
        let v = agent_version(r, name, Some(TEST_SUMMARY));
        case(
            if name == "n-direct" {
                "direct_ruling"
            } else {
                "recorded_ruling"
            },
            v,
            "E_POLICY_HUMAN",
        );
    }
    // making content stable
    put_j(
        r,
        "n-stable",
        json!([{"op": "update", "id": get("$pd"), "fields": {"lifecycle": "stable"}}]),
        j_typo(),
    );
    let v = agent_version(r, "n-stable", Some(TEST_SUMMARY));
    case("stable", v, "E_POLICY_HUMAN");
    // an imported decision status
    let t = tempfile::tempdir().unwrap();
    export_tree(
        &t.path().join("imported"),
        "imported",
        "d-MP001",
        "s-MP001",
        "D-MP001",
    );
    r.import(&ag("n-import"), "n-import", &t.path().join("imported"))
        .unwrap();
    let v = agent_version(r, "n-import", Some(TEST_SUMMARY));
    case("imported", v, "E_POLICY_HUMAN");
    // a change of the policy's roles, and of its owners
    for (name, field, value) in [
        ("n-roles", "roles", json!({"approvers": [PERSON, BOSS]})),
        (
            "n-owners",
            "owners",
            json!([{"path": "plain", "owners": [BOSS]}]),
        ),
    ] {
        let d = rules_decision(r, name, &format!("Change the policy's {field}"));
        let mut fields = open_policy();
        fields[field] = value;
        r.write(
            &writer(),
            WriteRequest {
                view: name.into(),
                ops: parse_test_ops(json!([{"op": "update", "id": policy, "fields": fields}]))
                    .unwrap(),
                justification: Some(Justification::Implements(vec![d])),
                message: String::new(),
            },
        )
        .unwrap();
        let v = propose_ready(r, name);
        case(
            if field == "roles" { "policy" } else { "owners" },
            v,
            "E_POLICY_HUMAN",
        );
    }
    // a stale version: a newer one replaced it
    put_j(r, "n-stale", upd(&bid("$o2"), "O2 holds, now."), j_typo());
    let first = agent_version(r, "n-stale", Some(TEST_SUMMARY));
    put_j(r, "n-stale", upd(&bid("$o2"), "O2 holds, still."), j_typo());
    let latest = agent_version(r, "n-stale", Some(TEST_SUMMARY));
    case("stale", first, "E_PROPOSAL_MOVED");
    // a redaction request
    r.redact_in(
        &writer(),
        "n-redact",
        rdy_engine::redaction::RedactInput {
            targets: vec![get("$secret")],
            reason_class: "secret".into(),
            reason: "a leaked code".into(),
            dry_run: false,
        },
    )
    .unwrap();
    let redact = r
        .propose(&writer(), "n-redact", "n-redact", Some(TEST_SUMMARY))
        .unwrap();
    case("redaction", redact, "E_REDACTION_HUMAN");
    let _ = &t;
    out.iter_mut()
        .find(|c| c.name == "stale")
        .expect("the stale case")
        .latest = Some(latest);
    out
}

/// The never-list reason each case's version carries, ids as `<id>`.
fn never_reason(name: &str) -> &'static str {
    match name {
        "one_way" => "it has the exception one_way on D-<id>@<id>",
        // the superseding decision names no content, so it is unapplied too: one card, two kinds
        "against" => {
            "it has the exception against on D-<id>@<id>; it has the exception unapplied on D-<id>@<id>"
        }
        "unapplied" => "it has the exception unapplied on D-<id>@<id>",
        "deferral" => "it defers 1 item that is not a code item",
        "direct_ruling" => "decision D-<id> has basis direct_ruling",
        "recorded_ruling" => "decision D-<id> has basis recorded_ruling",
        "stable" => "it makes doc d-<id> stable",
        "imported" => "it carries an imported decision status",
        "policy" | "owners" => "it changes the authority policy",
        other => panic!("no reason for {other}"),
    }
}

#[test]
fn mg5_never_under_policy_items_reach_the_person() {
    let (p, mut cases) = Project::qa_pack("", never_world);
    // the removal first: any later write moves its report (see the ignored pin below)
    cases.sort_by_key(|c| match c.name {
        "redaction" => 0,
        "owners" => 2,
        "policy" => 3,
        _ => 1,
    });
    let names: Vec<&str> = cases.iter().map(|c| c.name).collect();
    assert_eq!(names.len(), 13, "{names:?}");
    for (k, c) in cases.iter().enumerate() {
        let route = ROUTES[k % ROUTES.len()];
        let pref = &c.version.r#ref;
        let before = p.log();
        let queued = p.shown(Route::Relayed, pref);
        let (code, e) = rdy_land(&p, "a1", pref).refused();
        // no fallback: the refusal lands and records nothing; the person's card is unmoved
        assert_eq!(p.log(), before, "{}: rdy_land lands nothing", c.name);
        assert_eq!(p.shown(Route::Relayed, pref), queued, "{}", c.name);
        match c.name {
            "not_ready" => {
                assert_eq!(code, c.code, "{e:#}");
                // it reaches the person as not ready, with no card to approve
                assert!(queued.is_none());
                let q = p.mcp_queue();
                assert!(
                    q["not_ready"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|n| n["ref"] == *pref),
                    "{q:#}"
                );
                continue;
            }
            "stale" => {
                assert_eq!(code, c.code, "{e:#}");
                let latest = c.latest.as_ref().unwrap();
                assert_eq!(
                    e["message"],
                    format!(
                        "proposal {pref} moved: the latest version is {}",
                        latest.r#ref
                    )
                );
                p.land_via(route, &latest.r#ref);
                continue;
            }
            "redaction" => {
                assert_eq!(code, c.code, "{e:#}");
                assert_eq!(e["message"], "Content removal needs a person's approval.");
            }
            "one_way" | "against" | "unapplied" | "deferral" => {
                // the ordinary hold comes first; once the person answers it, the never-list
                let hold = if c.name == "deferral" {
                    "E_DEFERRAL_OPEN"
                } else {
                    "E_EXCEPTION_OPEN"
                };
                assert_eq!(code, hold, "{}: {e:#}", c.name);
                assert_eq!(p.answer_holds(route, pref), 1, "{}", c.name);
                let (code, e) = rdy_land(&p, "a1", pref).refused();
                assert_eq!(code, c.code, "{}: {e:#}", c.name);
                assert_never(&e, pref, c.name);
                assert_eq!(p.log(), before, "{}", c.name);
            }
            _ => {
                assert_eq!(code, c.code, "{}: {e:#}", c.name);
                assert_never(&e, pref, c.name);
            }
        }
        // the person sees the card and lands it; the owners change would make the policy change
        // that comes after it clash, so it only reaches the person
        assert!(
            p.shown(route, pref).is_some(),
            "{} reaches the person",
            c.name
        );
        if c.name != "owners" {
            let row = p.land_via(route, pref);
            assert_ne!(row["assurance"], "policy", "{}: {row:#}", c.name);
            assert_eq!(row["policy"], Value::Null, "{}", c.name);
        }
    }
}

/// `e` is the policy route's refusal of `pref` for the never-list reason of case `name`.
#[track_caller]
fn assert_never(e: &Value, pref: &str, name: &str) {
    assert_eq!(
        ids_out(e["message"].as_str().unwrap()),
        ids_out(&format!(
            "the authority policy needs a person for {pref}: {}",
            never_reason(name)
        )),
        "{name}: {e:#}"
    );
    assert_eq!(e["fix"], POLICY_HUMAN_FIX, "{name}");
}

// ---------------------------------------------------------------- later refusals

/// A person who wrote no side of the advised tension (the spec's claims are [`PERSON`]'s).
const FOUNDER: &str = "human:founder";

/// The versions the five later refusals hold, and what the person answers to make each hold.
struct Holds {
    spec: Spec,
    /// A decision that no content applies: the person rejects its exception card.
    rejected: ProposalInfo,
    /// An edit of claim m that defers n's confirm item: the person refuses the deferral.
    refused: ProposalInfo,
    /// A re-affirmation of a one_way decision: the person rejects it.
    reaffirm: ProposalInfo,
    /// A claim in an advised semantic tension with claim C: the founder accepts the advice.
    advised: ProposalInfo,
    /// A typo fix of claim x with a `fail` meaning verdict: the person accepts the verdict.
    verdict: ProposalInfo,
    x: String,
}

fn holds_world(r: &mut Rdy) -> Holds {
    use rdy_engine::plan::{PlanAnswerInput, ProposePlanRequest};
    use rdy_model::plan::{PlanAct, PlanKind, item_display, item_hash};
    use rdy_qa::intake::{decide_in, decision, verdict_on};
    use rdy_qa::tensions::{ag, created, j_typo, put, put_j, upd};
    let spec = spec_doc(r);
    let w = write(
        r,
        "rules",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "rules", "title": "Rules", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
            {"op": "create", "type": "claim", "id": "$m", "parent": "$s", "body": "M holds 30 days."},
            {"op": "create", "type": "claim", "id": "$n", "parent": "$s", "body": "N builds on {{$m}}."},
            {"op": "create", "type": "claim", "id": "$z", "parent": "$s", "body": "Z stands alone."},
            {"op": "create", "type": "doc", "id": "$nd", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$ns", "parent": "$nd", "fields": {"title": "Ideas"}},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$ns", "body": "X holds."}
        ]),
    )
    .unwrap();
    land(r, "rules").unwrap();
    let get = |k: &str| w.created[k].clone();
    let agent_version = |r: &mut Rdy, name: &str| {
        answer_open_items(r, &ag(name), name);
        let p = r
            .propose(&ag(name), name, name, Some(TEST_SUMMARY))
            .unwrap_or_else(|e| panic!("propose {name}: {e}"));
        assert!(p.ready, "{name}: {:#?}", p.issues);
        p
    };
    // a one_way decision on Z lands, then Z changes: its re-affirmation is an exception
    let one = r
        .decide(
            &writer(),
            "one-way",
            DecisionInput {
                parent: get("$s"),
                statement: "Never drop Z".into(),
                depends_on: vec![get("$z")],
                reversibility: Some("one_way".into()),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    land(r, "one-way").unwrap();
    write(
        r,
        "edit-z",
        json!([{"op": "update", "id": get("$z"), "body": "Z stands still."}]),
    )
    .unwrap();
    land(r, "edit-z").unwrap();
    decide_in(
        r,
        "rejected",
        DecisionInput {
            affects: vec![],
            ..decision(&spec, "Guests see a banner", "Guests see a banner")
        },
    );
    let rejected = agent_version(r, "rejected");
    write(
        r,
        "reaffirm",
        json!([{"op": "reaffirm", "id": one, "targets": [get("$z")], "reason": "Z still fits."}]),
    )
    .unwrap();
    let reaffirm = r
        .propose(&writer(), "reaffirm", "reaffirm", Some(TEST_SUMMARY))
        .unwrap();
    write(
        r,
        "refused",
        json!([{"op": "update", "id": get("$m"), "body": "M holds 60 days."}]),
    )
    .unwrap();
    let answers = [PlanAnswerInput {
        item: item_display(&item_hash(
            &BlockId::parse(&get("$m")).unwrap(),
            &get("$n"),
            PlanKind::Confirm,
        )),
        act: PlanAct::Defer,
        reason: Some("N waits for the review".into()),
    }];
    let refused = r
        .propose_plan(
            &writer(),
            ProposePlanRequest {
                summary: Some(TEST_SUMMARY),
                plan_answers: &answers,
                ..ProposePlanRequest::new("refused", "refused")
            },
        )
        .unwrap();
    let q = created(
        &put(
            r,
            "advised",
            json!([{"op": "create", "type": "claim", "id": "$q", "parent": spec.section.to_string(),
                    "body": "Guests read every rule of the spec."}]),
        ),
        "$q",
    );
    verdict_on(
        r,
        &rdy_engine::Scope::Inflight,
        &q,
        &spec.c,
        "agent:r",
        true,
    );
    let advised = agent_version(r, "advised");
    let x = BlockId::parse(&get("$x")).unwrap();
    put_j(r, "verdict", upd(&x, "X holds, now."), j_typo());
    let verdict = agent_version(r, "verdict");
    Holds {
        spec,
        rejected,
        refused,
        reaffirm,
        advised,
        verdict,
        x: get("$x"),
    }
}

/// An independent `fail` meaning verdict on version `pref` (agent:verdict's) by agent:r, citing
/// `cite`: the review's hash. A reviewer agent's act, seeded through the engine.
fn fail_verdict(p: &Project, pref: &str, cite: &str) -> String {
    use rdy_engine::check::api::{ReviewRequest, run_review};
    use rdy_engine::semantic::api::run_review_submit;
    use rdy_engine::semantic::submit::{CitationRequest, ReviewSubmitRequest};
    p.s.stop();
    let e = rdy_engine::engine::Local::open(p.dir()).unwrap();
    let q = run_review(
        &e,
        &rdy_qa::tensions::ag("verdict"),
        PERSON,
        true,
        None,
        ReviewRequest::from_value(
            json!({"op": "request", "check": "meaning_preservation", "subject": pref}),
        )
        .unwrap(),
    )
    .expect("request");
    let reviewer = rdy_engine::Session::agent(
        "agent:r".into(),
        Some("r".into()),
        rdy_model::Role::Reviewer,
    )
    .unwrap();
    let v = run_review_submit(
        &e,
        &reviewer,
        ReviewSubmitRequest {
            request: q["request"].as_str().unwrap().into(),
            case_hash: q["case_hash"].as_str().unwrap().into(),
            verdict: "fail".into(),
            citations: vec![CitationRequest {
                block: cite.to_string(),
                note: "the meaning moved".into(),
            }],
            severity: None,
            reason: Some("it reads otherwise".into()),
        },
    )
    .expect("submit");
    e.close().unwrap();
    v["review"].as_str().unwrap().to_string()
}

#[test]
fn mg5_later_refusals_block_every_approval_route() {
    for route in ROUTES {
        let (p, w) = Project::new(holds_world);
        let review = fail_verdict(&p, &w.verdict.r#ref, &w.x);
        let answer_route = if route == Route::CliApprove {
            Route::CliAnswer
        } else {
            route
        };
        // the approve cards the person saw before each later refusal
        let shown: Vec<Shown> = [&w.rejected, &w.refused, &w.reaffirm, &w.advised, &w.verdict]
            .iter()
            .map(|v| {
                p.shown(route, &v.r#ref)
                    .unwrap_or_else(|| panic!("{route:?} {}", v.r#ref))
            })
            .collect();
        // each refusal, as the person gives it through the route
        let q = p.mcp_queue();
        let item_of = |kind: &str, batch: &str| -> String {
            q["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|i| {
                    i["kind"] == kind
                        && (i["batch"] == batch || i["ref"] == format!("defer:{batch}"))
                })
                .unwrap_or_else(|| panic!("no {kind} beside {batch}: {q:#}"))["ref"]
                .as_str()
                .unwrap()
                .to_string()
        };
        let ex = item_of("confirm", &w.rejected.r#ref);
        p.show_and_answer(answer_route, &ex, "reject").1.done();
        let dr = format!("defer:{}", w.refused.r#ref);
        p.show_and_answer(answer_route, &dr, "o2").1.done();
        let ra = item_of("confirm", &w.reaffirm.r#ref);
        p.show_and_answer(answer_route, &ra, "reject").1.done();
        let accept = format!("accept_verdict:{review}:{}", w.verdict.id);
        let vshown = p.shown(answer_route, &w.verdict.r#ref).unwrap();
        p.answer(answer_route, &vshown, &accept).done();
        p.be(FOUNDER);
        let adv = format!("advice:{}", w.advised.r#ref);
        p.show_and_answer(answer_route, &adv, "accept").1.done();
        p.be(PERSON);
        let before = p.log();
        // the issue each hold puts on its version, ids as `<id>`
        let want: [(&str, &str); 5] = [
            (
                "E_VALIDATION",
                "D-<id> was rejected; drop or revise it, then propose again",
            ),
            ("E_VALIDATION", "the deferrals of P-<id>@1 were refused"),
            ("E_VALIDATION", "D-<id>'s re-affirmation was rejected"),
            ("E_BLOCKING_TENSION", ""),
            (
                "E_VALIDATION",
                "meaning_preservation verdict <hash> is accepted as blocking for P-<id>@1",
            ),
        ];
        for ((v, card), (issue, message)) in
            [&w.rejected, &w.refused, &w.reaffirm, &w.advised, &w.verdict]
                .iter()
                .zip(&shown)
                .zip(want)
        {
            let (code, e) = p.answer(route, card, "approve").refused();
            // the elicited route binds the card before it asks the person: a version that is
            // no longer ready has no card, so it is refused as a moved item, with the same
            // issues (an envelope difference, like KNOWN-ISSUES row 37; nothing lands)
            let want_code = if route == Route::Elicited {
                "E_ITEM_MOVED"
            } else {
                "E_PROPOSAL_NOT_READY"
            };
            assert_eq!(code, want_code, "{route:?} {}: {e:#}", v.r#ref);
            if route == Route::Ui {
                continue;
            }
            let issues = e["issues"].as_array().unwrap();
            assert_eq!(issues.len(), 1, "{route:?} {}: {e:#}", v.r#ref);
            assert_eq!(issues[0]["code"], issue, "{route:?} {}", v.r#ref);
            // `rdy approve` names the blocking tension itself; the answer paths count them
            if !message.is_empty() {
                assert_eq!(
                    hash_out(&ids_out(issues[0]["message"].as_str().unwrap())),
                    message,
                    "{route:?} {}",
                    v.r#ref
                );
            }
        }
        assert_eq!(p.log(), before, "{route:?}: no later refusal lands");
        // a new version of each: a refused deferral and a verdict whose case changed clear; a
        // rejected decision, a rejected re-affirmation and accepted advice stay with the content
        let views = ["rejected", "refused", "reaffirm", "advised", "verdict"];
        let news: Vec<ProposalInfo> = p.seed(|r| {
            views
                .iter()
                .map(|v| {
                    write(
                        r,
                        v,
                        json!([{"op": "create", "type": "claim", "parent": w.spec.section.to_string(),
                                "body": format!("More of {v}.")}]),
                    )
                    .unwrap();
                    answer_open_items(r, &writer(), v);
                    r.propose(&writer(), v, v, Some(TEST_SUMMARY)).unwrap()
                })
                .collect()
        });
        let ready: Vec<bool> = news.iter().map(|n| n.ready).collect();
        assert_eq!(ready, [false, true, false, false, true], "{route:?}");
        for n in [&news[1], &news[4]] {
            let row = p.land_via(route, &n.r#ref);
            assert_eq!(row["approved_by"], PERSON, "{route:?}");
        }
        for n in [&news[0], &news[2], &news[3]] {
            assert!(p.shown(route, &n.r#ref).is_none(), "{route:?}: still held");
        }
    }
}

/// `text` with each 64-hex hash as `<hash>`.
fn hash_out(text: &str) -> String {
    regex::Regex::new(r"\b[0-9a-f]{64}\b")
        .unwrap()
        .replace_all(text, "<hash>")
        .into_owned()
}

// ---------------------------------------------------------------- the coverage gate

/// Sets the project's `.rdy.yaml` to pack rdy-standard with `coverage.gate: gate` (the daemon
/// stops first, so the next command reads it).
fn set_gate(p: &Project, gate: &str, extra: &str) {
    p.s.stop();
    std::fs::write(
        p.dir().join(".rdy.yaml"),
        format!(
            "pack: rdy-standard\ncoverage:\n  gate: {gate}\n{extra}serve:\n  idle_timeout: 60\n"
        ),
    )
    .unwrap();
}

/// Six draft docs `p0`..`p5`, each with a section `s<k>` and a claim, landed with the gate off.
/// The sections by index.
fn gate_seed(r: &mut Rdy) -> Vec<String> {
    let mut ops = Vec::new();
    for k in 0..6 {
        ops.push(json!({"op": "create", "type": "doc", "id": format!("$d{k}"),
                        "fields": {"name": format!("p{k}"), "title": format!("P{k}"), "doc_type": "overview"}}));
        ops.push(json!({"op": "create", "type": "section", "id": format!("$s{k}"), "parent": format!("$d{k}"),
                        "fields": {"title": "Ideas"}}));
        ops.push(json!({"op": "create", "type": "claim", "parent": format!("$s{k}"), "body": format!("X{k} holds.")}));
    }
    let w = write(r, "seed", json!(ops)).unwrap();
    land(r, "seed").unwrap();
    (0..6)
        .map(|k| w.created[&format!("$s{k}")].clone())
        .collect()
}

/// The coverage cases: a name, whether `no_lower` refuses it, and the ops agent:<name> writes
/// (editorial) in its view, given the sections. The source case reflects a source at a tag the
/// linked checkout does not hold.
fn gate_cases(s: &[String]) -> Vec<(&'static str, bool, Value)> {
    vec![
        (
            "question",
            true,
            json!([{"op": "create", "type": "question", "parent": s[0], "fields": {"prompt": "Is X enough?"}}]),
        ),
        (
            "unapplied",
            true,
            json!([{"op": "create", "type": "decision", "parent": s[1],
            "fields": {"statement": "Ideas stay short", "status": "proposed"}}]),
        ),
        (
            "new_page",
            true,
            json!([
                {"op": "create", "type": "doc", "id": "$m", "fields": {"name": "more", "title": "More", "doc_type": "overview"}},
                {"op": "create", "type": "section", "id": "$t", "parent": "$m", "fields": {"title": "Open"}},
                {"op": "create", "type": "question", "parent": "$t", "fields": {"prompt": "Is more enough?"}}
            ]),
        ),
        (
            "no_content",
            false,
            json!([{"op": "create", "type": "decision", "parent": s[2],
            "fields": {"statement": "Ideas stay long", "status": "proposed", "needs_content": false}}]),
        ),
        (
            "claim",
            false,
            json!([{"op": "create", "type": "claim", "parent": s[3], "body": "A bare claim."}]),
        ),
        (
            "source",
            false,
            json!([
                {"op": "create", "type": "source", "id": "$x", "parent": s[4],
                 "fields": {"title": "Release nine", "kind": "release", "locator": "v9"}},
                {"op": "create", "type": "claim", "parent": s[4], "body": "Release nine ships it.",
                 "links": {"reflects": ["$x"]}}
            ]),
        ),
    ]
}

#[test]
fn mg5_coverage_gate_excludes_machine_facts_and_ungrounded_content() {
    use rdy_qa::tensions::{ag, j_typo, put_j};
    for gate in ["off", "no_lower"] {
        let (p, sections) = Project::new(gate_seed);
        let app = rdy_qa::git_repo::GitRepo::new();
        app.write("a.txt", "a\n");
        app.commit("c");
        app.run(&["tag", "v1"]);
        set_gate(&p, gate, "");
        app.link(p.dir());
        let cases = gate_cases(&sections);
        let versions: Vec<ProposalInfo> = p.seed(|r| {
            let mut out = Vec::new();
            for (name, _, ops) in &cases {
                put_j(r, name, ops.clone(), j_typo());
                answer_open_items(r, &ag(name), name);
                out.push(r.propose(&ag(name), name, name, Some(TEST_SUMMARY)).unwrap());
            }
            // making a section stable leaves its claim ungrounded: the whole score falls, the
            // gate score does not
            let s5 = BlockId::parse(&sections[5]).unwrap();
            let t = rdy_qa::edit_loop::brief_of(r, &rdy_engine::Scope::Canon, &[&s5])
                .base
                .token;
            rdy_qa::edit_loop::write_cited_as(
                r,
                &ag("ungrounded"),
                "ungrounded",
                json!([{"op": "update", "id": sections[5], "fields": {"title": "Ideas", "lifecycle": "stable"}}]),
                Some(&t),
            )
            .unwrap();
            answer_open_items(r, &ag("ungrounded"), "ungrounded");
            out.push(
                r.propose(&ag("ungrounded"), "ungrounded", "ungrounded", Some(TEST_SUMMARY))
                    .unwrap(),
            );
            out
        });
        // the source case: this machine shows the missing tag; the intrinsic coverage the gate
        // and the stored card read does not hold it
        let (shown_codes, intrinsic_codes, card) = p.seed(|r| {
            use rdy_engine::coverage::IssueCode;
            let st = r.read();
            let v = rdy_engine::Scope::View("source".into());
            let codes = |c: &rdy_engine::coverage::CoverageIssues| -> usize {
                c.blocks
                    .values()
                    .flat_map(|e| e.issues.iter())
                    .filter(|i| i.code == IssueCode::SourceNotFound)
                    .count()
            };
            let shown = codes(&st.display_coverage_issues(&v).unwrap());
            let intrinsic = codes(&st.coverage_issues(&v).unwrap());
            drop(st);
            (shown, intrinsic, card_text(r, &versions[5].r#ref))
        });
        assert_eq!((shown_codes, intrinsic_codes), (1, 0), "{gate}");
        assert!(!card.contains("source_not_found"), "{card}");
        let mut names: Vec<(&str, bool)> = cases.iter().map(|(n, l, _)| (*n, *l)).collect();
        names.push(("ungrounded", false));
        for (k, ((name, lowers), v)) in names.iter().zip(&versions).enumerate() {
            let refused = gate == "no_lower" && *lowers;
            let gate_issues: Vec<String> = v
                .issues
                .iter()
                .filter(|i| i.code == "E_COVERAGE_LOWERED")
                .map(|i| {
                    i.message
                        .split(" lowers coverage of ")
                        .nth(1)
                        .unwrap_or("")
                        .split(" from ")
                        .next()
                        .unwrap()
                        .to_string()
                })
                .collect();
            if refused {
                assert!(!v.ready, "{gate} {name}");
                // a new page is compared in the corpus only; others name their page too
                let want: Vec<String> = match *name {
                    "new_page" => vec!["the corpus".into()],
                    "question" => vec!["p0".into(), "the corpus".into()],
                    _ => vec!["p1".into(), "the corpus".into()],
                };
                assert_eq!(gate_issues, want, "{gate} {name}: {:#?}", v.issues);
                // no route shows a card for it, and every route refuses its approval
                for route in ROUTES {
                    assert!(p.shown(route, &v.r#ref).is_none(), "{route:?} {name}");
                    let blind = Shown {
                        r#ref: v.r#ref.clone(),
                        card_hash: "0".repeat(64),
                        options: Vec::new(),
                    };
                    let before = p.log();
                    let (code, e) = p.answer(route, &blind, "approve").refused();
                    let want = if route == Route::Elicited {
                        "E_ITEM_MOVED"
                    } else {
                        "E_PROPOSAL_NOT_READY"
                    };
                    assert_eq!(code, want, "{route:?} {name}: {e:#}");
                    assert_eq!(p.log(), before, "{route:?} {name}");
                }
            } else {
                assert!(v.ready, "{gate} {name}: {:#?}", v.issues);
                assert!(gate_issues.is_empty(), "{gate} {name}");
                p.land_via(ROUTES[k % ROUTES.len()], &v.r#ref);
            }
        }
    }
}

/// The text of the card of `r#ref` in the person's queue.
fn card_text(r: &Rdy, r#ref: &str) -> String {
    queue_for(r, PERSON)
        .items
        .into_iter()
        .find(|i| i.r#ref == r#ref)
        .unwrap_or_else(|| panic!("{ref} is queued"))
        .card
}

// ---------------------------------------------------------------- elicitation and the local UI

/// The 2026-07-28 flow: no `initialize`, the client context in each request.
fn inline() -> Revision {
    Revision::Inline {
        version: "2026-07-28".into(),
        capabilities: json!({"elicitation": {"form": {}}}),
    }
}

/// `.rdy/local.yaml` of the project: the person, then `extra`; the daemon restarts on the next
/// command.
fn local_config(p: &Project, extra: &str) {
    p.s.stop();
    std::fs::write(
        p.dir().join(".rdy/local.yaml"),
        format!("actor: {PERSON}\n{extra}"),
    )
    .unwrap();
}

/// A GET or POST of the approval UI's paths that the listener at `base` answers as ordinary
/// paths: 404 pages naming no UI, and the plain 405 for the answer POST.
#[track_caller]
fn assert_no_ui(url: &str, what: &str) {
    let authority = url
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap()
        .to_string();
    let base = format!("http://{authority}");
    for path in ["/~ui/", "/~ui/item/P-abc@1"] {
        let r = http::get(&format!("{base}{path}"));
        assert_eq!(r.status, 404, "{what}: GET {path}");
        let page = String::from_utf8_lossy(&r.body);
        for needle in ["~ui", "approval", "ui-card", "<form"] {
            assert!(!page.contains(needle), "{what}: {path} names {needle:?}");
        }
    }
    let addr: std::net::SocketAddr = authority.parse().unwrap();
    let post = http::raw(addr, &http::ui_answer_request(&authority, &[("ref", "r")]));
    assert_eq!(post.status, 405, "{what}: POST {}", post.head);
}

#[test]
fn mg5_elicitation_fallback_and_local_ui_keep_their_boundaries() {
    use rdy_qa::mcp::{CONTINUATION_BOUND, Clock};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    let (p, (_, [v1, v2])) = Project::new(|r| {
        let made = ready(r);
        // a publication profile of the spec doc, for a profile-only listener
        let ids = write(
            r,
            "admin",
            json!([{"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}]),
        )
        .unwrap()
        .created;
        write(
            r,
            "admin",
            json!([{"op": "create", "type": "publication_profile", "parent": ids["$admin"],
                    "fields": {"key": "guide", "title": "Guide book", "description": "About it.",
                               "audience": "org", "select": [made.0.doc.to_string()]}}]),
        )
        .unwrap();
        land(r, "admin").unwrap();
        made
    });
    let card = p.shown(Route::Relayed, &v1.r#ref).unwrap();
    let ask = json!({"ref": card.r#ref, "card_hash": card.card_hash, "elicit": true});
    let before = p.log();

    // a client with no form capability: elicitation is unavailable, and the relay path stays
    let mut c = p.agent();
    let e = c.call("rdy_answer", ask.clone());
    assert_eq!(e.code(), Some("E_ELICITATION_UNAVAILABLE"), "{e:?}");
    drop(c);
    assert_eq!(p.log(), before);
    assert_eq!(
        p.shown(Route::Relayed, &v1.r#ref),
        Some(card.clone()),
        "unmoved"
    );

    // declined and cancelled forms, in both flows, write nothing
    for (revision, action) in [
        (in_call(), "decline"),
        (in_call(), "cancel"),
        (inline(), "decline"),
        (inline(), "cancel"),
    ] {
        let mut c = McpClient::try_launch_eliciting(McpClient::proxy(&p.s), revision).unwrap();
        assert!(
            c.try_call(
                "rdy_session",
                json!({"actor": RELAY, "run": "r1", "role": "writer"})
            )
            .unwrap()
            .is_ok()
        );
        let mut person = |_: &Value| PersonAct::Answer(json!({"action": action}));
        let v = c
            .try_call_elicited("rdy_answer", ask.clone(), &mut person)
            .unwrap()
            .ok();
        let state = if action == "decline" {
            "declined"
        } else {
            "cancelled"
        };
        assert_eq!(v, json!({"state": state}), "{action}");
        drop(c);
        assert_eq!(p.log(), before, "{action}");
        assert_eq!(
            p.shown(Route::Relayed, &v1.r#ref),
            Some(card.clone()),
            "{action}"
        );
    }
    // a continuation past its bound is never answered: the client sends nothing
    let mut c = McpClient::try_launch_eliciting(McpClient::proxy(&p.s), inline()).unwrap();
    assert!(
        c.try_call(
            "rdy_session",
            json!({"actor": RELAY, "run": "r1", "role": "writer"})
        )
        .unwrap()
        .is_ok()
    );
    let base = Instant::now();
    let at = Arc::new(Mutex::new(base));
    let clock = Arc::clone(&at);
    let clock: Clock = Arc::new(move || *clock.lock().unwrap());
    c.set_clock(clock);
    let mut late = |_: &Value| {
        *at.lock().unwrap() = base + CONTINUATION_BOUND + Duration::from_secs(1);
        PersonAct::Answer(json!({"action": "accept", "content": {"choice": "approve"}}))
    };
    let got = c.try_call_elicited("rdy_answer", ask.clone(), &mut late);
    assert!(matches!(got, Err(McpFailure::Deadline)), "{got:?}");
    drop(c);
    assert_eq!(p.log(), before, "an expired continuation");
    assert_eq!(p.shown(Route::Relayed, &v1.r#ref), Some(card.clone()));
    // the binding holds before any form: a card that is not the one shown is refused, fresh
    let mut c = p.eliciting();
    let mut never = |req: &Value| panic!("no form for a moved card: {req}");
    let wrong = json!({"ref": card.r#ref, "card_hash": "0".repeat(64), "elicit": true});
    let r = c
        .try_call_elicited("rdy_answer", wrong, &mut never)
        .unwrap();
    let (code, e) = mcp_outcome(r).refused();
    assert_eq!(code, "E_PROPOSAL_MOVED", "{e:#}");
    assert_eq!(
        e["message"],
        format!("{} changed since it was shown", card.r#ref)
    );
    assert_eq!(e["fix"], "show the person the fresh card and ask again");
    let fresh = corrected_of(Route::Elicited, &e).expect("the fresh card");
    assert_eq!(
        (&fresh.r#ref, &fresh.card_hash),
        (&card.r#ref, &card.card_hash),
        "{e:#}"
    );
    drop(c);
    assert_eq!(p.log(), before);
    // (the role holds on the elicited route as on every route: mg5_roles_route_and_enforce_...)

    // the local UI never asks an agent to sign: with signing always it refuses, E_NO_AGENT
    local_config(
        &p,
        "signing:\n  key: null\n  agent_key: null\n  sign: always\n  allowed_signers: null\n",
    );
    let (code, e) = p
        .show_and_answer(Route::Ui, &v1.r#ref, "approve")
        .1
        .refused();
    assert_eq!(code, "E_NO_AGENT", "{e:#}");
    let page = e["page"].as_str().unwrap();
    for text in [
        "the SSH agent could not sign: the local UI has no agent socket".to_string(),
        format!(
            "run rdy approve {} in a terminal to sign this approval",
            v1.r#ref
        ),
    ] {
        assert!(
            page.contains(&rdy_render::escape(&text).to_string()),
            "{text}: {page}"
        );
    }
    assert_eq!(e["status"], 403);
    assert_eq!(p.log(), before, "nothing written");
    local_config(&p, "");

    // with serve.ui off the listener has no approval UI, and a form post writes nothing
    local_config(&p, "serve:\n  ui: false\n");
    assert_no_ui(&p.site(), "serve.ui off");
    assert_eq!(p.log(), before);
    local_config(&p, "");
    // a profile's pages, on the full listener, link and hold no approval UI
    let guide = p.cli_ok(&["site", "--profile", "guide"]);
    let url = guide["url"].as_str().unwrap();
    assert!(url.contains("/~profile/guide/"), "{url}");
    let page = http::get(url);
    assert_eq!(page.status, 200, "{url}");
    let text = String::from_utf8_lossy(&page.body);
    for needle in ["/~ui/", "<form", "card_hash"] {
        assert!(!text.contains(needle), "the profile page names {needle:?}");
    }
    // a static render holds no approval UI
    let out = tempfile::tempdir().unwrap();
    let site = out.path().join("site");
    p.cli_ok(&["render", "--out", site.to_str().unwrap()]);
    for f in walk(&site) {
        let bytes = std::fs::read(&f).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("/~ui/"), "{} names the UI", f.display());
    }
    // the UI is back on the full listener, and the person's form lands the version
    let row = p.land_via(Route::Ui, &v1.r#ref);
    assert_eq!(provenance(&row)["evidence"]["kind"], "card");
    let _ = v2;
}

/// Every file under `dir`.
fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let path = e.unwrap().path();
            if path.is_dir() {
                todo.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out
}

// ---------------------------------------------------------------- disputes

#[test]
fn mg5_disputes_record_the_route_without_blocking_the_next_landing() {
    let (p, (_, [v1, v2])) = Project::new(ready);
    let landed = p.land_via(Route::Relayed, &v1.r#ref);
    let landing = landed["landing"].as_str().unwrap().to_string();
    let count = p.log().len();

    // an agent alone cannot open one: the person's words are required
    let mut c = p.agent();
    let e = c.call(
        "rdy_dispute",
        json!({"op": "open", "landing": landing, "reason": "It dropped a rule."}),
    );
    assert_eq!(e.code(), Some("E_PERMISSION"), "{e:?}");
    // relayed: the person's dispute, with the agent as relay and an ANSWER of the evidence
    let relayed = c
        .call(
            "rdy_dispute",
            json!({"op": "open", "landing": landing, "reason": "It dropped a rule.",
                   "evidence": "Dispute that landing.", "question": "Dispute it?",
                   "session_ref": "chat-1"}),
        )
        .ok();
    drop(c);
    assert_eq!(relayed["dispute"]["state"], "open", "{relayed:#}");
    assert_eq!(
        relayed["dispute"]["opened_by"]["actor"], PERSON,
        "{relayed:#}"
    );
    assert_eq!(
        relayed["relayed_by"],
        json!({"actor": RELAY, "run": "r1"}),
        "{relayed:#}"
    );
    assert_eq!(
        relayed["answer"].as_str().map(str::len),
        Some(64),
        "the relay's ANSWER record: {relayed:#}"
    );
    // local: the person's own dispute, through the CLI's record path, with no ANSWER
    let local = p.cli_ok(&["dispute", "open", &landing, "--reason", "It reads wrong."]);
    assert_eq!(
        local["dispute"]["opened_by"],
        json!({"actor": PERSON, "run": "cli"}),
        "{local:#}"
    );
    assert!(local.get("answer").is_none(), "{local:#}");
    assert!(
        local.get("relayed_by").is_none_or(Value::is_null),
        "{local:#}"
    );

    // the disputes write no landing, and the disputed landing stays as it landed
    let log = p.log();
    assert_eq!(log.len(), count, "a dispute is no landing");
    let row = log
        .iter()
        .find(|r| r["landing"] == landing.as_str())
        .unwrap();
    assert_eq!(row["dispute"]["state"], "open", "{row:#}");
    assert_eq!(
        row["dispute"]["disputes"].as_array().map(Vec::len),
        Some(2),
        "{row:#}"
    );
    assert_eq!(common(row), {
        let mut c = common(&landed);
        c["dispute"] = row["dispute"].clone();
        c
    });
    assert_eq!(
        provenance(row),
        provenance(&landed),
        "no ratification changes it"
    );

    // the next landing is not blocked, and carries no dispute
    let next = p.land_via(Route::CliApprove, &v2.r#ref);
    assert_eq!(next["dispute"], json!({"state": "none", "disputes": []}));
    // the person upholds the relayed dispute with that landing as its correction
    let id = relayed["dispute"]["id"].as_str().unwrap();
    let mut c = p.agent();
    let v = c
        .call(
            "rdy_dispute",
            json!({"op": "resolve", "dispute": id, "outcome": "upheld",
                   "correction": next["landing"], "evidence": "Yes, that fixes it.",
                   "session_ref": "chat-1"}),
        )
        .ok();
    assert_eq!(v["dispute"]["state"], "upheld", "{v:#}");
    assert_eq!(
        v["dispute"]["resolution"]["correction"], next["landing"],
        "{v:#}"
    );
    assert_eq!(v["answer"].as_str().map(str::len), Some(64), "{v:#}");
    drop(c);
    assert_eq!(p.log().len(), count + 1, "only the correction landed");
}

/// Found by this task (mg5): an open removal request's card goes stale when any unrelated view
/// writes, and the person's whole queue then fails with `E_REDACTION_STALE`, so no card of any
/// item can be read or answered. The decided behavior is open (a stale removal card could drop,
/// be marked, or refuse only itself); this pin states the queue staying readable and is ignored
/// until the bounded fix and its ruling land.
#[test]
#[ignore = "defect: an unrelated write makes the whole queue E_REDACTION_STALE (mg5 report)"]
fn mg5_an_unrelated_write_keeps_the_queue_with_a_removal_card_readable() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$kd", "fields": {"name": "keys", "title": "Keys", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$ks", "parent": "$kd", "fields": {"title": "Keys"}},
            {"op": "create", "type": "prose", "id": "$secret", "parent": "$ks", "body": "The code is 1234."},
            {"op": "create", "type": "doc", "id": "$od", "fields": {"name": "other", "title": "Other", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$os", "parent": "$od", "fields": {"title": "More"}},
            {"op": "create", "type": "claim", "id": "$o", "parent": "$os", "body": "O holds."}
        ]),
    )
    .unwrap();
    land(&mut r, "seed").unwrap();
    r.redact_in(
        &writer(),
        "redact",
        rdy_engine::redaction::RedactInput {
            targets: vec![w.created["$secret"].clone()],
            reason_class: "secret".into(),
            reason: "a leaked code".into(),
            dry_run: false,
        },
    )
    .unwrap();
    r.propose(&writer(), "redact", "redact", Some(TEST_SUMMARY))
        .unwrap();
    assert_eq!(queue_for(&r, PERSON).items.len(), 1);
    // a view in another doc writes; nothing touches the removal's target
    write(
        &mut r,
        "x",
        json!([{"op": "update", "id": w.created["$o"], "body": "O holds, now."}]),
    )
    .unwrap();
    let q = r.read().pending(&PendingQuery {
        person: PERSON.into(),
        session_ref: None,
        max_tokens: CARD_MAX_TOKENS,
    });
    assert!(q.is_ok(), "the queue stays readable: {:?}", q.err());
}
