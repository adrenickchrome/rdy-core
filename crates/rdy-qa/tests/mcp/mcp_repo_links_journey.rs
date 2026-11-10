//! rpl36 (repo links; D-DAPR8, D-8VJAZ, D-MEMMQ, D-0DNKT, D-CN4QJ): the whole repo-link journey
//! over the MCP tools, against two real project stores.
//!
//! The cited project is a real one: it lands pages, a decision to deviate from, a decision to
//! adopt and a rule that a later release locks, tags each release, writes its `site` profile as a
//! link delivery and, last, removes the text of a page with a person's approval. The citing
//! project takes each delivery as the hub's act. Everything else goes through the tools: the
//! agent reads the links, cites, records a deviation and an upstream adoption, and relays the
//! person's answers to the adopt cards (keep, later, adopt). A foreign act (a delivery, a
//! revocation, a removal) leaves canon where it was, and only the person's approved local acts
//! move it. The same facts hold in a second server process after a restart, after a reindex, in a
//! project loaded from a dump and in a follower that follows the increments. The meta project
//! of the instruction test lives under its own temporary home.
use rdy_engine::engine::{Engine, Local, with_writer_result};
use rdy_engine::follower::{self, FollowerCore};
use rdy_engine::foreign_delivery::{revoke_link, take_delivery};
use rdy_engine::foreign_removal::take_foreign_removal;
use rdy_engine::redaction::RedactInput;
use rdy_engine::tensions::TensionKind;
use rdy_engine::{DecisionInput, Rdy, RdyError, Scope};
use rdy_graph::DerivedStatus;
use rdy_mcp::tools::*;
use rdy_model::foreign::{ForeignSet, ForeignSource};
use rdy_model::{BlockId, Hash, RepoId};
use rdy_qa::fixtures::{approver, land, project, write, writer};
use rdy_qa::instructions::{Set, World, author};
use rdy_qa::mcp::{McpClient, Reply};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const SITE: &str = "hub.example";
const AT_MS: i64 = 1_760_000_000_000;
/// The text of the page the cited project removes.
const LEAKED: &str = "A leaked code 1234";

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn p<T: serde::de::DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v).unwrap()
}

fn bid(w: &rdy_engine::WriteResult, name: &str) -> BlockId {
    id(&w.created[name])
}

fn put(r: &mut Rdy, view: &str, ops: Value) -> rdy_engine::WriteResult {
    write(r, view, ops).unwrap_or_else(|e| panic!("write to {view}: {e:?}"))
}

fn landed(r: &mut Rdy, view: &str) {
    land(r, view).unwrap_or_else(|e| panic!("land {view}: {e:?}"));
}

fn refused(e: &RdyError, code: &str, message: &str, fix: &str) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (code, message, fix),
        "{e:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// the cited project

/// The cited project, open: the ids it delivers and the profile it keeps.
struct Cited {
    _d: tempfile::TempDir,
    r: Rdy,
    repo: RepoId,
    /// The page the citing project cites, sees changed and sees removed.
    ap: BlockId,
    /// The decision the citing project adopts as its own copy.
    up: BlockId,
    /// The decision the citing project deviates from.
    dv: BlockId,
    /// The rule that release 1.2.0 locks.
    lk: BlockId,
    /// The `site` profile, which the citing project's link names.
    site: BlockId,
    /// A doc the profile does not select.
    admin: BlockId,
    files: tempfile::TempDir,
}

fn tag_of(r: &Rdy, name: &str) -> (Hash, Hash) {
    let (_, h, t) = r
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, ..)| n == name)
        .unwrap_or_else(|| panic!("tag {name}"));
    (h, t.landing)
}

/// v1.0.0: doc `a` holds the page `ap` and the decisions `up` and `dv`; the doc `rules` is empty;
/// the profile `site` selects both docs. v1.1.0 changes `ap`. v1.2.0 adds the locked rule `lk`.
/// v1.3.0 changes `ap` again.
fn cited() -> Cited {
    let (d, mut r) = project();
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
    let dec = put(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "decision", "id": "$up", "parent": a.to_string(),
             "fields": {"statement": "Every job logs in UTC.", "status": "proposed"}},
            {"op": "create", "type": "decision", "id": "$dv", "parent": a.to_string(),
             "fields": {"statement": "Every job retries three times.", "status": "proposed"}},
            {"op": "create", "type": "publication_profile", "id": "$site", "parent": admin.to_string(),
             "fields": {"key": "site", "title": "Site", "audience": "org",
                        "select": [a.to_string(), rules.to_string()]}}
        ]),
    );
    landed(&mut r, "work");
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    put(
        &mut r,
        "work",
        json!([{"op": "update", "id": w.created["$ap"], "body": format!("{LEAKED}, later.")}]),
    );
    landed(&mut r, "work");
    r.tag(&approver(), "v1.1.0", None, None).unwrap();
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
    let lk = put(
        &mut r,
        "work",
        json!([{"op": "create", "type": "instruction", "id": "$lk", "parent": rules.to_string(),
                "fields": {"applies_to": ["all"], "locked": true},
                "body": "Every claim names its owner.",
                "links": {"decided_by": [{"target": reason}]}}]),
    );
    landed(&mut r, "work");
    r.tag(&approver(), "v1.2.0", None, None).unwrap();
    put(
        &mut r,
        "work",
        json!([{"op": "update", "id": w.created["$ap"], "body": format!("{LEAKED}, once more.")}]),
    );
    landed(&mut r, "work");
    r.tag(&approver(), "v1.3.0", None, None).unwrap();
    let repo = r.store().reader().repo_id().unwrap().unwrap();
    Cited {
        _d: d,
        r,
        repo,
        ap: bid(&w, "$ap"),
        up: bid(&dec, "$up"),
        dv: bid(&dec, "$dv"),
        lk: bid(&lk, "$lk"),
        site: bid(&dec, "$site"),
        admin,
        files: tempfile::tempdir().unwrap(),
    }
}

impl Cited {
    /// The `site` link delivery at `tag`.
    fn delivery(&self, tag: &str) -> File {
        self.export(tag, "site")
    }

    /// The link delivery of `profile` at `tag`.
    fn export(&self, tag: &str, profile: &str) -> File {
        let (h, landing) = tag_of(&self.r, tag);
        let mut out = Vec::new();
        rdy_engine::link_export::write(&self.r.read(), tag, h, landing, profile, &mut out)
            .unwrap_or_else(|e| panic!("delivery of {profile} at {tag}: {e:?}"));
        self.file(&format!("delivery-{profile}-{tag}"), &out)
    }

    fn file(&self, name: &str, bytes: &[u8]) -> File {
        let path = self.files.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        File::open(path).unwrap()
    }

    /// Removes the text of `ap` (every version of it) with a person's approval.
    fn remove(&mut self) {
        let s = writer();
        self.r
            .redact_in(
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
        let prop = self
            .r
            .propose(&s, "redact", "Remove", Some("test summary"))
            .unwrap();
        self.r.land(&approver(), &prop.id).unwrap();
    }

    /// The history at v1.0.0: after a removal it carries the removal's portable proof.
    fn history(&self) -> File {
        let (h, _) = tag_of(&self.r, "v1.0.0");
        let mut out = Vec::new();
        rdy_engine::stream_export::write(&self.r.read(), h, &BTreeSet::new(), &mut out).unwrap();
        self.file("history", &out)
    }
}

// ---------------------------------------------------------------------------------------------
// the citing project, driven through the tools

/// The citing project under an MCP state: the person is `human:qa`, the agent `agent:qa`.
struct Citing {
    dir: PathBuf,
    _d: tempfile::TempDir,
    st: McpState,
    section: BlockId,
    link: BlockId,
    /// The local decision that the project later replaces by its upstream copy.
    old: BlockId,
    claim: Option<BlockId>,
    at_ms: i64,
}

impl Citing {
    /// A project with a doc, a section, the repo link `core` to `s`'s `site` profile and the
    /// local accepted decision `old`, all landed; then an MCP state over it.
    fn new(s: &Cited) -> Citing {
        let (d, mut r) = project();
        let w = put(
            &mut r,
            "seed",
            json!([
                {"op": "create", "type": "doc", "id": "$d",
                 "fields": {"title": "Guide", "doc_type": "overview"}},
                {"op": "create", "type": "section", "id": "$s", "parent": "$d",
                 "fields": {"title": "Rules"}},
                {"op": "create", "type": "repo_link", "id": "$l", "parent": "$d",
                 "fields": {"alias": "core", "repo": s.repo.to_string(),
                            "profile": s.site.to_string()}}
            ]),
        );
        landed(&mut r, "seed");
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
        landed(&mut r, "old");
        let dir = d.path().to_path_buf();
        // the person of the project: the one an answer is recorded for
        std::fs::write(dir.join(".rdy/local.yaml"), "actor: human:qa\n").unwrap();
        let mut st = McpState::new(Arc::new(Local::new(r)));
        rdy_session(
            &mut st,
            p(json!({"actor": "agent:qa", "run": "qa", "role": "writer"})),
        )
        .unwrap();
        Citing {
            dir,
            _d: d,
            st,
            section,
            link: bid(&w, "$l"),
            old: id(&old),
            claim: None,
            at_ms: AT_MS,
        }
    }

    fn engine(&self) -> &dyn Engine {
        &*self.st.engine
    }

    fn canon(&self) -> Option<Hash> {
        self.st.engine.read().canon_head()
    }

    fn source(&mut self) -> ForeignSource {
        self.at_ms += 1;
        ForeignSource::Link {
            link: self.link.to_string(),
            site: SITE.into(),
            at_ms: self.at_ms,
        }
    }

    /// The hub's act: takes `s`'s delivery at `tag` through the engine's one writer.
    fn deliver(&mut self, s: &Cited, tag: &str, set: Option<ForeignSet>) -> Hash {
        let file = s.delivery(tag);
        let source = self.source();
        with_writer_result(self.engine(), move |r| {
            take_delivery(r, &file, &source, set)
        })
        .unwrap_or_else(|e| panic!("take {tag}: {e:?}"))
        .entry
    }

    fn revoke(&mut self) -> bool {
        let source = self.source();
        with_writer_result(self.engine(), move |r| revoke_link(r, &source))
            .unwrap()
            .unchanged
    }

    /// The person's approval of the view `view`: its answers, its landing.
    fn land_view(&self, view: &str) {
        let view = view.to_string();
        with_writer_result(self.engine(), move |r| land(r, &view).map(|_| ()))
            .unwrap_or_else(|e| panic!("land: {e:?}"));
    }

    fn links(&mut self, args: Value) -> Result<Value, RdyError> {
        rdy_links(&mut self.st, p(args))
    }

    fn read(&mut self, what: &str) -> Result<Value, RdyError> {
        rdy_read(
            &mut self.st,
            p(json!({"id": what, "scope": "canon", "format": "yaml"})),
        )
    }

    fn pending(&mut self, session: Option<&str>) -> Value {
        rdy_pending(
            &mut self.st,
            p(match session {
                Some(s) => json!({"session_ref": s}),
                None => json!({}),
            }),
        )
        .unwrap()
    }

    /// The open adopt card, as the queue shows it to `session`; none when it is hidden.
    fn adopt_card(&mut self, session: Option<&str>) -> Option<Value> {
        let q = self.pending(session);
        let mut all: Vec<Value> = q["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["kind"] == "adopt")
            .cloned()
            .collect();
        assert!(all.len() <= 1, "{all:#?}");
        all.pop()
    }

    /// The agent relays the person's answer to the open adopt card.
    fn answer(&mut self, option: &str, session: &str) -> Result<Value, RdyError> {
        let card = self.adopt_card(None).expect("an open adopt card");
        let mut args = json!({"ref": card["ref"], "option": option,
            "card_hash": card["card_hash"],
            "evidence": format!("{option} it."), "question": "Adopt the change from core?",
            "session_ref": session});
        if option == "keep" {
            args["note"] = json!("Billing stays on the first text.");
        }
        rdy_answer(&mut self.st, p(args))
    }

    /// Writes `ops` into the agent's view `view` and lands it as the person.
    fn write_and_land(&mut self, view: &str, ops: Value) -> Value {
        let v = rdy_write(&mut self.st, p(json!({"view": view, "ops": ops}))).unwrap();
        self.land_view(view);
        v
    }

    /// Records a decision that deviates from `target` in `view`, without landing it.
    fn decide_deviation(
        &mut self,
        view: &str,
        target: &str,
        statement: &str,
    ) -> Result<Value, RdyError> {
        rdy_decide(
            &mut self.st,
            p(
                json!({"view": view, "statement": statement, "title": "Depart",
                     "parent": self.section.to_string(), "deviates_from": [target]}),
            ),
        )
    }
}

/// The pins of a refs reply: the cited block, the held version, the delivered one and whether
/// the pin is behind.
fn pin_rows(refs: &Value) -> Vec<(String, String, String, bool)> {
    refs["repos"][0]["pins"]
        .as_array()
        .unwrap_or_else(|| panic!("pins in {refs}"))
        .iter()
        .map(|r| {
            (
                r["target"].as_str().unwrap().to_string(),
                r["held"].to_string(),
                r["delivered"].to_string(),
                r["behind"].as_bool().unwrap(),
            )
        })
        .collect()
}

fn text_of(v: &Value) -> String {
    v["text"].as_str().unwrap_or_default().to_string()
}

/// The engine's own foreign gate facts for the proposal `version`, read from the state the tools
/// read: the full ids of the tensions they name.
fn engine_gate(c: &Citing, version: &Hash) -> Vec<String> {
    let st = c.st.engine.read();
    let rec = st.reader().proposal(version).unwrap();
    let rows: Vec<_> = st
        .tensions_of_changes(&rec.changes)
        .unwrap()
        .into_iter()
        .map(|t| {
            let status = st.tension_status(&t);
            (t, status)
        })
        .collect();
    let facts = rdy_engine::foreign_facts::foreign_tension_gate(&rows, &Default::default());
    assert!(
        facts.iter().all(|f| f.kind == TensionKind::PremiseShift),
        "{facts:#?}"
    );
    facts.iter().map(|f| f.tension.to_string()).collect()
}

/// The tension items of a plan reply, by the full tension id each names.
fn plan_tensions(plan: &Value) -> Vec<String> {
    plan["items"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|i| i["kind"] == "tension")
                .map(|i| i["subject"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// What the journey leaves for a caller to check afterwards.
struct Ends {
    /// The full ids of the claim and the replaced decision.
    claim: BlockId,
}

/// The journey: a first delivery, a citation, a changed source, a keep, a later, an adoption, a
/// deviation, an upstream adoption, a revocation and a removal. Canon moves only with the
/// person's approved local acts. `after` runs when each step is done, with the step's name.
fn journey(s: &mut Cited, c: &mut Citing, after: &mut dyn FnMut(&mut Citing, &str)) -> Ends {
    let mut canon = c.canon();
    let moved = |c: &Citing, canon: &mut Option<Hash>, what: &str| {
        let now = c.canon();
        assert_ne!(now, *canon, "{what} lands: canon moves");
        *canon = now;
    };
    let stays = |c: &Citing, canon: &Option<Hash>, what: &str| {
        assert_eq!(&c.canon(), canon, "{what} is no local act: canon stays");
    };
    let org = || Some(ForeignSet::Organization);

    // before any delivery: the link is listed with no state
    let v = c.links(json!({})).unwrap();
    assert_eq!(v["links"][0]["alias"], "core", "{v}");
    assert_eq!(v["links"][0]["state"], "none", "{v}");

    // 1. the first delivery: the hub's act, no local one
    let first = c.deliver(s, "v1.0.0", None);
    stays(c, &canon, "a delivery");
    let v = c.links(json!({})).unwrap();
    assert_eq!(v["links"][0]["state"], "delivered", "{v}");
    let shown = c.links(json!({"op": "show", "alias": "core"})).unwrap();
    let ids: Vec<&str> = shown["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["id"].as_str().unwrap())
        .collect();
    for want in [&s.ap, &s.up, &s.dv] {
        assert!(
            ids.contains(&want.to_string().as_str()),
            "{want} in {ids:?}"
        );
    }
    assert!(
        !ids.contains(&s.lk.to_string().as_str()),
        "the lock comes with v1.2.0"
    );
    after(c, "first delivery");

    // 2. the citation: the agent writes a claim, the person approves it
    let ap = s.ap.to_string();
    let w = c.write_and_land(
        "cite",
        json!([{"op": "create", "type": "claim", "id": "$c", "parent": c.section.to_string(),
                "body": format!("Follow {{{{core:{ap}}}}}.")}]),
    );
    c.claim = Some(id(w["created"]["$c"].as_str().unwrap()));
    moved(c, &mut canon, "the citation");
    let refs = c.links(json!({"op": "refs", "alias": "core"})).unwrap();
    let rows = pin_rows(&refs);
    assert_eq!(rows.len(), 1, "{refs}");
    assert_eq!(rows[0].0, ap, "{refs}");
    assert!(
        !rows[0].3 && rows[0].1 == rows[0].2,
        "pinned at the delivery: {refs}"
    );
    let read = text_of(&c.read(c.claim.clone().unwrap().as_str()).unwrap());
    assert!(read.contains(&format!("{{{{{}:{ap}}}}}", s.repo)), "{read}");
    // a citation of a block that no delivery holds is refused, and writes nothing
    let e = rdy_write(
        &mut c.st,
        p(json!({"view": "bad", "ops": [{"op": "create", "type": "claim",
                 "parent": c.section.to_string(), "body": format!("Follow {{{{core:{}}}}}.", s.admin)}]})),
    )
    .unwrap_err();
    refused(
        &e,
        "E_UNKNOWN_REF",
        &format!("core:{} is not in the content delivered from core", s.admin),
        "cite a block that core delivers: rdy_links {op: \"show\", alias} / rdy links show core",
    );
    stays(c, &canon, "a refused write");
    after(c, "citation");

    // an open proposal pins the same page: the next delivery shifts its premise
    rdy_write(
        &mut c.st,
        p(
            json!({"view": "open", "ops": [{"op": "create", "type": "claim",
                 "parent": c.section.to_string(), "body": format!("Plan on {{{{core:{ap}}}}}.")}]}),
        ),
    )
    .unwrap();
    let prop = rdy_propose(
        &mut c.st,
        p(json!({"view": "open", "title": "Plan", "summary": "test summary"})),
    )
    .unwrap();
    let pref = prop["ref"]
        .as_str()
        .unwrap_or_else(|| panic!("{prop}"))
        .to_string();
    let version: Hash = prop["id"].as_str().unwrap().parse().unwrap();
    stays(c, &canon, "an open proposal");
    let plan = rdy_plan(&mut c.st, p(json!({"proposal": pref}))).unwrap();
    assert_eq!(plan_tensions(&plan), Vec::<String>::new(), "{plan}");
    assert!(engine_gate(c, &version).is_empty());

    // 3. a changed source: v1.1.0 changes the page. The pin stays behind, canon stays and an
    // adopt card stands; the open proposal's readiness holds the engine's own facts.
    let second = c.deliver(s, "v1.1.0", None);
    stays(c, &canon, "a delivery that changes a cited page");
    let refs = c.links(json!({"op": "refs", "alias": "core"})).unwrap();
    let rows = pin_rows(&refs);
    assert!(
        rows[0].3 && rows[0].1 != rows[0].2,
        "the pin is behind: {refs}"
    );
    let card = c.adopt_card(None).expect("an adopt card");
    let options: Vec<&str> = card["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["id"].as_str().unwrap())
        .collect();
    assert_eq!(options, ["adopt", "keep", "later"], "{card}");
    assert!(
        card["ref"].as_str().unwrap().starts_with("adopt:"),
        "{card}"
    );
    let facts = engine_gate(c, &version);
    assert_eq!(facts.len(), 1, "{facts:?}");
    let plan = rdy_plan(&mut c.st, p(json!({"proposal": pref}))).unwrap();
    assert_eq!(
        plan_tensions(&plan),
        facts,
        "readiness names the engine's tension: {plan}"
    );
    after(c, "changed source");

    // 4. keep: the reason is recorded, the pin is retained, canon stays
    let a = c.answer("keep", "chat-1").unwrap();
    assert_eq!(a["act"], "keep_pin", "{a}");
    stays(c, &canon, "a keep answer");
    assert_eq!(
        pin_rows(&c.links(json!({"op": "refs", "alias": "core"})).unwrap()),
        rows
    );
    assert!(c.adopt_card(None).is_none(), "the card is settled");
    after(c, "keep");

    // 5. a new lock and a later: v1.2.0 adds a locked rule of the organization's set. The
    // person's later hides the card for that chat session only; nothing moves.
    let third = c.deliver(s, "v1.2.0", org());
    stays(c, &canon, "a delivery that locks a rule");
    let shown = c.links(json!({"op": "show", "alias": "core"})).unwrap();
    assert!(
        shown.to_string().contains(&s.lk.to_string()),
        "the new rule is shown: {shown}"
    );
    assert_eq!(shown["pending"][0]["id"], s.lk.to_string(), "{shown}");
    assert_eq!(shown["pending"][0]["change"], "added", "{shown}");
    let a = c.answer("later", "chat-1").unwrap();
    assert_eq!(a["act"], "later", "{a}");
    stays(c, &canon, "a later answer");
    assert!(
        c.adopt_card(Some("chat-1")).is_none(),
        "hidden in the session that said later"
    );
    assert!(
        c.adopt_card(Some("chat-2")).is_some(),
        "shown in another session"
    );
    assert!(c.adopt_card(None).is_some());
    after(c, "new lock");

    // 6. adoption: the person adopts the delivery with the lock
    let a = c.answer("adopt", "chat-2").unwrap();
    assert_eq!(a["act"], "adopt", "{a}");
    moved(c, &mut canon, "the adoption");
    assert!(c.adopt_card(None).is_none(), "the card is settled");
    let shown = c.links(json!({"op": "show", "alias": "core"})).unwrap();
    assert!(
        shown["pending"].as_array().is_none_or(|a| a.is_empty()),
        "nothing waits for adoption: {shown}"
    );
    after(c, "adoption");

    // 7. a deviation: the agent records a decision that departs from the cited decision, and the
    // person approves it; a departure from the adopted lock is refused, so is a local target
    let dev_ref = format!("core:{}", s.dv);
    let d = c
        .decide_deviation("dev", &dev_ref, "Billing retries five times.")
        .unwrap_or_else(|e| panic!("deviate: {e:?}"));
    assert!(d["created"]["$decision"].is_string(), "{d}");
    stays(c, &canon, "a decision in a view");
    c.land_view("dev");
    moved(c, &mut canon, "the deviation");
    let lock_ref = format!("core:{}", s.lk);
    let e = c
        .decide_deviation("dev-lock", &lock_ref, "We skip the owner rule.")
        .unwrap_err();
    refused(
        &e,
        "E_INSTRUCTION_LOCKED",
        &format!("{lock_ref} is locked by its organization set"),
        "add an instruction on top of it, or ask the set's owners to change it there: rdy_write a new instruction / rdy new instruction",
    );
    let e = c
        .decide_deviation("dev-local", &c.old.to_string(), "We skip the old rule.")
        .unwrap_err();
    assert_eq!(e.code, "E_VALIDATION", "{e:?}");
    assert_eq!(
        e.message,
        format!(
            "deviates_from names {}, which is not cited or external",
            c.old
        )
    );
    stays(c, &canon, "a refused deviation");
    after(c, "deviation");

    // a later delivery changes the page again; its adopt answer moves the pin in one landing
    let fourth = c.deliver(s, "v1.3.0", org());
    stays(c, &canon, "a delivery that changes a cited page again");
    let behind = pin_rows(&c.links(json!({"op": "refs", "alias": "core"})).unwrap());
    assert!(behind.iter().any(|r| r.0 == ap && r.3), "{behind:?}");
    let a = c.answer("adopt", "chat-2").unwrap();
    assert_eq!(a["act"], "adopt", "{a}");
    moved(c, &mut canon, "the second adoption");
    let now = pin_rows(&c.links(json!({"op": "refs", "alias": "core"})).unwrap());
    assert!(
        now.iter().all(|r| !r.3 && r.1 == r.2),
        "every pin is current: {now:?}"
    );
    after(c, "second adoption");

    // 8. an upstream adoption: the local decision is replaced by the cited one. A raw edit of the
    // field is refused.
    let old = c.old.to_string();
    let up_ref = format!("core:{}", s.up);
    let e = rdy_write(
        &mut c.st,
        p(json!({"view": "raw", "ops": [{"op": "update", "id": old,
                 "fields": {"adopted_as": up_ref}}]})),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_VALIDATION", "{e:?}");
    assert_eq!(e.issues.len(), 1, "{e:?}");
    assert_eq!(e.issues[0].code, "E_FIELD_UNKNOWN", "{e:?}");
    assert_eq!(
        e.fix,
        "remove adopted_as; use rdy_supersede with adopted_as"
    );
    rdy_supersede(
        &mut c.st,
        p(json!({"view": "up", "old": old, "adopted_as": up_ref})),
    )
    .unwrap_or_else(|e| panic!("adopt upstream: {e:?}"));
    stays(c, &canon, "a supersede in a view");
    c.land_view("up");
    moved(c, &mut canon, "the upstream adoption");
    let status =
        c.st.engine
            .read()
            .graph(&Scope::Canon)
            .unwrap()
            .status(&c.old);
    assert_eq!(
        status,
        Some(DerivedStatus::Superseded {
            by: vec![s.up.clone()]
        })
    );
    after(c, "upstream adoption");

    // 9. a revocation: the hub's act. The link reads revoked, the held page stays readable, a
    // later delivery is refused and canon stays.
    assert!(!c.revoke());
    stays(c, &canon, "a revocation");
    assert!(c.revoke(), "a revoked source writes nothing more");
    let v = c.links(json!({})).unwrap();
    assert_eq!(v["links"][0]["state"], "revoked", "{v}");
    let page = text_of(&c.read(&format!("core:{ap}")).unwrap());
    assert!(
        page.contains(&format!("body: {LEAKED}, once more.")),
        "{page}"
    );
    assert!(page.contains("no longer delivered"), "{page}");
    let file = s.delivery("v1.3.0");
    let source = c.source();
    let e = with_writer_result(c.engine(), move |r| take_delivery(r, &file, &source, None))
        .unwrap_err();
    refused(
        &e,
        "E_BAD_INPUT",
        "the repo link is revoked",
        "use a live repo link",
    );
    stays(c, &canon, "a refused delivery");
    after(c, "revocation");

    // 10. a removal: the cited project removes the text, and the take erases every copy
    s.remove();
    let history = s.history();
    let repo = s.repo;
    let taken = with_writer_result(c.engine(), move |r| {
        take_foreign_removal(r, &history, &repo)
    })
    .unwrap();
    assert!(!taken.unchanged);
    assert_eq!(taken.removals[0].applied.len(), 3, "{taken:?}");
    stays(c, &canon, "a removal");
    let page = text_of(&c.read(&format!("core:{ap}")).unwrap());
    assert!(!page.contains(LEAKED), "{page}");
    assert!(
        !on_disk(&c.dir, LEAKED),
        "no byte of the removed text stays"
    );
    let entries: BTreeSet<Hash> = [first, second, third, fourth].into_iter().collect();
    assert_eq!(entries.len(), 4, "four deliveries, four entries");
    after(c, "removal");
    Ends {
        claim: c.claim.clone().unwrap(),
    }
}

/// The store directory of the project at `project`.
fn store_dir(project: &Path) -> PathBuf {
    project.join(".rdy").join(rdy_store::layout::STORE_DIR)
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

/// The journey over the tools, with nothing more to check than its own steps.
#[test]
fn rpl36_deliver_cite_deviate_adopt_revoke_remove() {
    let mut s = cited();
    let mut c = Citing::new(&s);
    journey(&mut s, &mut c, &mut |_, _| {});
}

/// Keeps from `v` the keys the surface compares: the replies of the tools carry a reminder and
/// warnings that depend on the session.
fn pick(v: &Value, keys: &[&str]) -> Value {
    let mut out = serde_json::Map::new();
    for k in keys {
        if let Some(x) = v.get(*k) {
            out.insert((*k).to_string(), x.clone());
        }
    }
    Value::Object(out)
}

const LIST_KEYS: [&str; 2] = ["links", "unlinked"];
const SHOW_KEYS: [&str; 3] = ["blocks", "link", "pending"];
const REFS_KEYS: [&str; 1] = ["repos"];

/// What the engine itself reads of the links in `st`, in the shape of the tools' replies.
fn engine_surface(st: &rdy_engine::ReadState) -> Value {
    json!({
        "list": pick(&serde_json::to_value(st.links().unwrap()).unwrap(), &LIST_KEYS),
        "show": pick(&serde_json::to_value(st.link_show("core").unwrap()).unwrap(), &SHOW_KEYS),
        "refs": pick(&serde_json::to_value(st.link_refs(None).unwrap()).unwrap(), &REFS_KEYS),
        "canon": st.canon_head().map(|h| h.to_string()),
    })
}

/// What the tools read of the links through the in-process state: the same shape, and the cited
/// page as `rdy_read` gives it.
fn tool_surface(c: &mut Citing) -> Value {
    let list = c.links(json!({})).unwrap();
    let show = c.links(json!({"op": "show", "alias": "core"})).unwrap();
    let refs = c.links(json!({"op": "refs"})).unwrap();
    json!({
        "list": pick(&list, &LIST_KEYS),
        "show": pick(&show, &SHOW_KEYS),
        "refs": pick(&refs, &REFS_KEYS),
        "canon": c.canon().map(|h| h.to_string()),
    })
}

/// The same reads through a server process.
fn client_surface(c: &mut McpClient) -> Value {
    let call = |c: &mut McpClient, tool: &str, args: Value| -> Value {
        match c.call(tool, args) {
            Reply::Ok(v) => v,
            other => panic!("{tool}: {other:?}"),
        }
    };
    let list = call(c, "rdy_links", json!({}));
    let show = call(c, "rdy_links", json!({"op": "show", "alias": "core"}));
    let refs = call(c, "rdy_links", json!({"op": "refs"}));
    json!({
        "list": pick(&list, &LIST_KEYS),
        "show": pick(&show, &SHOW_KEYS),
        "refs": pick(&refs, &REFS_KEYS),
    })
}

fn start_client(dir: &Path, home: &Path) -> McpClient {
    let mut c = McpClient::start_isolated(dir, home);
    let r = c.call(
        "rdy_session",
        json!({"actor": "agent:k", "run": "k1", "role": "reader"}),
    );
    assert!(r.is_ok(), "{r:?}");
    c
}

/// A surface without its canon head: a project loaded from a dump holds the same facts.
fn without_canon(v: &Value) -> Value {
    let mut v = v.clone();
    v.as_object_mut().unwrap().remove("canon");
    v
}

fn page_text(c: &mut McpClient, what: &str) -> String {
    match c.call(
        "rdy_read",
        json!({"id": what, "scope": "canon", "format": "yaml"}),
    ) {
        Reply::Ok(v) => text_of(&v),
        other => panic!("read {what}: {other:?}"),
    }
}

/// The journey's facts, step by step, hold again in a restarted server, after a reindex, in a
/// project loaded from a dump and in a follower that follows the increments. The follower is
/// seeded before the first delivery and follows one increment per step. The tools read what the
/// engine reads at every step.
#[test]
fn rpl36_local_and_foreign_histories_survive_roundtrip() {
    let mut s = cited();
    let mut c = Citing::new(&s);
    let files = tempfile::tempdir().unwrap();
    let d0 = files.path().join("d0");
    c.st.engine.read().dump(&d0).unwrap();
    let fdir = files.path().join("follower");
    follower::seed(&d0, &fdir).unwrap();
    let mut last = d0.clone();
    let mut steps: Vec<(String, Value, PathBuf)> = vec![];
    let ends = journey(&mut s, &mut c, &mut |c, name| {
        let out = files.path().join(format!("inc-{}", steps.len() + 1));
        let tools = tool_surface(c);
        assert_eq!(
            tools,
            engine_surface(&c.st.engine.read()),
            "tools and engine after {name}"
        );
        c.st.engine.read().dump_after_file(&last, &out).unwrap();
        last = out.clone();
        steps.push((name.to_string(), tools, out));
    });
    assert_eq!(steps.len(), 11, "one increment per step");
    let want = steps.last().unwrap().1.clone();
    let page = text_of(&c.read(&format!("core:{}", s.ap)).unwrap());
    let claim = text_of(&c.read(&ends.claim.to_string()).unwrap());
    assert!(!page.contains(LEAKED) && claim.contains(&format!("{{{{{}:{}}}}}", s.repo, s.ap)));
    let full = files.path().join("full");
    c.st.engine.read().dump(&full).unwrap();

    // the follower, one increment per step
    {
        let mut core = FollowerCore::open(&fdir).unwrap();
        for (name, surface, increment) in &steps {
            core.follow(increment).unwrap();
            assert_eq!(
                &engine_surface(&core.read().unwrap()),
                surface,
                "the follower after {name}"
            );
        }
    }

    // the server restarted: the in-process state is dropped, and a new process reads the project
    let Citing { dir, _d, st, .. } = c;
    drop(st);
    let home = tempfile::tempdir().unwrap();
    let mut client = start_client(&dir, home.path());
    let restarted = client_surface(&mut client);
    assert_eq!(restarted, without_canon(&want), "after a restart");
    let page_after = page_text(&mut client, &format!("core:{}", s.ap));
    assert_eq!(page_after, page);
    let claim_after = page_text(&mut client, &ends.claim.to_string());
    assert_eq!(claim_after, claim);
    drop(client);

    // a reindex replays the log and reads the same
    let mut r = Rdy::open(&dir).unwrap();
    r.reindex().unwrap();
    assert_eq!(engine_surface(&r.read()), want, "after a reindex");
    r.close().unwrap();
    let mut client = start_client(&dir, home.path());
    assert_eq!(client_surface(&mut client), without_canon(&want));
    drop(client);

    // the project loaded from the full dump reads the same
    let clone = files.path().join("clone");
    rdy_engine::load::load(&full, &clone).unwrap();
    let mut client = start_client(&clone, home.path());
    assert_eq!(
        client_surface(&mut client),
        without_canon(&want),
        "after dump and load"
    );
    assert_eq!(page_text(&mut client, &format!("core:{}", s.ap)), page);
    assert_eq!(page_text(&mut client, &ends.claim.to_string()), claim);
    drop(client);
    let r = Rdy::open(&clone).unwrap();
    assert_eq!(
        engine_surface(&r.read()),
        want,
        "the clone's canon is the project's"
    );
    r.close().unwrap();
    assert!(!on_disk(&clone, LEAKED) && !on_disk(&dir, LEAKED));
}

const META_SETS: &[Set] = &[Set {
    name: "personal",
    title: "Personal",
    required: true,
    lead: None,
    rules: &[(&["all"], "Answer in British English.")],
}];

const PROJECT_SETS: &[Set] = &[Set {
    name: "baseline",
    title: "Baseline",
    required: true,
    lead: None,
    rules: &[(&["all"], "Write small changes.")],
}];

/// The layers of a reply of `rdy_instructions`, each rule once, the built-in rows left out.
fn layers_of(v: &Value) -> Vec<(String, String)> {
    v["instructions"]
        .as_array()
        .unwrap_or_else(|| panic!("instructions in {v}"))
        .iter()
        .filter(|r| r["layer"] != "rdy")
        .map(|r| {
            (
                r["layer"].as_str().unwrap().to_string(),
                r["id"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// Two profiles of one cited project under two aliases, delivered to two teams, and the person's
/// own layer in an isolated home. Each alias resolves against its own profile: the page only
/// `core`'s profile delivers is no citation for `ops`. A delivery is in no layer until the person
/// adopts it; then each team's rule is in its own layer, between rdy and the project, with the
/// person's rule last, and a locked team rule cannot be departed from.
#[test]
fn rpl36_two_profiles_two_teams_and_person_layer() {
    let mut s = cited();
    // a second ruleset under a second profile: `ops` delivers it and no page
    let (lk2, ops_profile) = {
        let w = put(
            &mut s.r,
            "work",
            json!([{"op": "create", "type": "doc", "id": "$r2",
                    "fields": {"name": "rules2", "title": "Rules 2", "doc_type": "page"}}]),
        );
        let rules2 = bid(&w, "$r2");
        let reason =
            s.r.decide(
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
            &mut s.r,
            "work",
            json!([
                {"op": "create", "type": "instruction", "id": "$lk2", "parent": rules2.to_string(),
                 "fields": {"applies_to": ["all"], "locked": true},
                 "body": "Keep the owner in every claim.",
                 "links": {"decided_by": [{"target": reason}]}},
                {"op": "create", "type": "publication_profile", "id": "$ops",
                 "parent": s.admin.to_string(),
                 "fields": {"key": "ops", "title": "Ops", "audience": "org",
                            "select": [rules2.to_string()]}}
            ]),
        );
        landed(&mut s.r, "work");
        s.r.tag(&approver(), "v2.0.0", None, None).unwrap();
        (bid(&l, "$lk2"), bid(&l, "$ops"))
    };

    // the person's isolated world: a meta project at a path of its own, a project rule and the
    // citing project with two links to the one repo
    let world = World::new();
    let (pd, md) = (world.project(), world.project());
    author(md.path(), "One spelling in every reply", META_SETS);
    author(pd.path(), "Small changes keep review cheap", PROJECT_SETS);
    world.person(md.path());
    let (section, core_entry, ops_entry) = {
        let mut r = Rdy::open(pd.path()).unwrap();
        let w = put(
            &mut r,
            "links",
            json!([
                {"op": "create", "type": "doc", "id": "$d",
                 "fields": {"name": "guide", "title": "Guide", "doc_type": "overview"}},
                {"op": "create", "type": "section", "id": "$s", "parent": "$d",
                 "fields": {"title": "Rules"}},
                {"op": "create", "type": "repo_link", "id": "$core", "parent": "$d",
                 "fields": {"alias": "core", "repo": s.repo.to_string(),
                            "profile": s.site.to_string()}},
                {"op": "create", "type": "repo_link", "id": "$ops", "parent": "$d",
                 "fields": {"alias": "ops", "repo": s.repo.to_string(),
                            "profile": ops_profile.to_string()}}
            ]),
        );
        landed(&mut r, "links");
        let (core, ops) = (bid(&w, "$core"), bid(&w, "$ops"));
        let take = |r: &mut Rdy, link: &BlockId, profile: &str, at: i64, team: &str| {
            let file = s.export("v2.0.0", profile);
            let source = ForeignSource::Link {
                link: link.to_string(),
                site: SITE.into(),
                at_ms: AT_MS + at,
            };
            take_delivery(r, &file, &source, Some(ForeignSet::Team(team.into())))
                .unwrap_or_else(|e| panic!("take {profile}: {e:?}"))
        };
        let a = take(&mut r, &core, "site", 1, "docs");
        let b = take(&mut r, &ops, "ops", 2, "ops");
        assert_ne!(a.selection, b.selection, "two profiles, two selections");
        assert!(a.added > b.added, "the site profile delivers more");
        let out = (bid(&w, "$s"), a.entry, b.entry);
        r.close().unwrap();
        out
    };
    let mut c = world.mcp(pd.path());
    let r = c.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "a1", "role": "writer"}),
    );
    assert!(r.is_ok(), "{r:?}");
    let call = |c: &mut McpClient, tool: &str, args: Value| -> Reply { c.call(tool, args) };
    let ok = |r: Reply| match r {
        Reply::Ok(v) => v,
        other => panic!("{other:?}"),
    };
    let refusal = |r: Reply| match r {
        Reply::ToolError(v) => v,
        other => panic!("expected an rdy tool error, got {other:?}"),
    };

    // each link's selection is its own: the page is delivered through `core` only
    let v = ok(call(&mut c, "rdy_links", json!({})));
    let aliases: Vec<&str> = v["links"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["alias"].as_str().unwrap())
        .collect();
    assert_eq!(aliases, ["core", "ops"], "{v}");
    assert!(
        v["links"]
            .as_array()
            .unwrap()
            .iter()
            .all(|l| l["state"] == "delivered")
    );
    let cite = |c: &mut McpClient, alias: &str, view: &str| {
        call(
            c,
            "rdy_write",
            json!({"view": view, "ops": [{"op": "create", "type": "claim",
                   "parent": section.to_string(),
                   "body": format!("Follow {{{{{alias}:{}}}}}.", s.ap)}]}),
        )
    };
    let e = refusal(cite(&mut c, "ops", "cite-ops"));
    assert_eq!(e["code"], "E_UNKNOWN_REF", "{e}");
    assert_eq!(
        e["message"],
        format!("ops:{} is not in the content delivered from ops", s.ap)
    );
    assert_eq!(
        e["fix"],
        "cite a block that ops delivers: rdy_links {op: \"show\", alias} / rdy links show ops"
    );
    assert!(cite(&mut c, "core", "cite-core").is_ok());

    // before any adopt answer the rules are delivered and in no layer
    let before = ok(call(&mut c, "rdy_instructions", json!({})));
    let seen: Vec<(String, String)> = layers_of(&before);
    assert!(
        seen.iter().all(|(l, _)| !l.starts_with("team:")),
        "an unadopted delivery is in no layer: {seen:?}"
    );

    // the person answers each adopt card; the cards name their deliveries
    let q = ok(call(&mut c, "rdy_pending", json!({})));
    let cards: Vec<Value> = q["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == "adopt")
        .cloned()
        .collect();
    assert_eq!(cards.len(), 2, "one adopt card for each link: {q}");
    for entry in [core_entry, ops_entry] {
        let want = format!("adopt:{}@{}", s.repo, entry.short());
        let card = cards
            .iter()
            .find(|i| i["ref"] == want)
            .unwrap_or_else(|| panic!("{want} in {cards:?}"));
        let a = ok(call(
            &mut c,
            "rdy_answer",
            json!({"ref": card["ref"], "option": "adopt", "card_hash": card["card_hash"],
                   "evidence": "Adopt it.", "question": "Adopt the delivery?",
                   "session_ref": "chat-1"}),
        ));
        assert_eq!(a["act"], "adopt", "{a}");
    }
    let q = ok(call(&mut c, "rdy_pending", json!({})));
    assert!(
        q["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["kind"] != "adopt"),
        "{q}"
    );

    // the layers: the teams (sorted by name), the project, then the person
    let v = ok(call(&mut c, "rdy_instructions", json!({})));
    let seen = layers_of(&v);
    let layers: Vec<&str> = seen.iter().map(|(l, _)| l.as_str()).collect();
    assert_eq!(
        layers,
        ["team:docs", "team:ops", "project", "person"],
        "{v}"
    );
    assert_eq!(seen[0].1, s.lk.to_string());
    assert_eq!(seen[1].1, lk2.to_string());
    let rows = v["instructions"].as_array().unwrap();
    let row = |i: &BlockId| rows.iter().find(|r| r["id"] == i.to_string()).unwrap();
    assert_eq!(row(&s.lk)["text"], "Every claim names its owner.");
    assert_eq!(row(&lk2)["text"], "Keep the owner in every claim.");
    assert_eq!(row(&s.lk)["locked"], true, "{v}");
    assert_eq!(row(&lk2)["locked"], true, "{v}");
    // the first row of the person layer comes from the isolated meta project
    assert_eq!(rows.last().unwrap()["text"], "Answer in British English.");

    // a locked team rule cannot be departed from, in either team
    for (alias, rule, team) in [("core", &s.lk, "docs"), ("ops", &lk2, "ops")] {
        let e = refusal(call(
            &mut c,
            "rdy_decide",
            json!({"view": format!("dev-{alias}"), "statement": "We skip it.", "title": "Depart",
                   "parent": section.to_string(), "deviates_from": [format!("{alias}:{rule}")]}),
        ));
        assert_eq!(e["code"], "E_INSTRUCTION_LOCKED", "{e}");
        assert_eq!(
            e["message"],
            format!("{alias}:{rule} is locked by its team {team} set")
        );
        assert_eq!(
            e["fix"],
            "add an instruction on top of it, or ask the set's owners to change it there: rdy_write a new instruction / rdy new instruction"
        );
    }
}
