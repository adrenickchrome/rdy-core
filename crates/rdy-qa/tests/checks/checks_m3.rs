//! mg9 (FW-G7 D-2GT0Z; D-JQD5X, D-MQR17, D-VPATZ, D-X4RYX, D-6CYFV, D-YXBXH, D-DTWA8, D-HZYK0,
//! D-1876T, D-KVQ7F): the M3 gate's conformance of the check contract. These tests read built
//! behaviour through the product's own calls; they change no product behaviour. Checks are
//! deterministic stages; the eight agent checks are cases a reviewer reads, never a gate input;
//! source existence is a read fact; governing and prose drift use the four routes; neighbor
//! consequences and group answers use the whole displayed set; the read tools and the CLI agree;
//! Grounded method 2 scores normative mentions and keeps every stored method-1 record.
use rdy_engine::check::api::{ReviewRequest, run_review};
use rdy_engine::check::{AGENT_CHECKS, AgentCheck, STAGE_CHECKS, Stage};
use rdy_engine::consequence::NEIGHBOR_EDGE;
use rdy_engine::engine::{Engine, Local, with_writer_result};
use rdy_engine::pending::{CARD_MAX_TOKENS, Item, ItemKind, PendingQuery};
use rdy_engine::plan::{ItemState, PlanRow};
use rdy_engine::readiness::{Candidate, assess, readiness};
use rdy_engine::semantic::api::run_review_submit;
use rdy_engine::semantic::submit::{CitationRequest, ReviewSubmitRequest};
use rdy_engine::{ProposalInfo, Rdy, RdyError, Scope, Session};
use rdy_model::check_request::CheckMode;
use rdy_model::plan::PlanKind;
use rdy_model::{BlockId, Hash, Role};
use rdy_qa::daemon::Served;
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::{approver, land, project, write, writer};
use rdy_qa::intake::{PERSON, TEST_SUMMARY, answer_open_items};
use rdy_qa::mcp::McpClient;
use rdy_qa::tensions::{ag, cite, created, j_implements, j_typo, put, put_j, term, upd};
use rdy_schema::{PackSet, SchemaError};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

const REASON: &str = "it reads otherwise";
const NOTE: &str = "the meaning moved";

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn on<T: Send + 'static>(
    e: &dyn Engine,
    f: impl FnOnce(&mut Rdy) -> Result<T, RdyError> + Send + 'static,
) -> Result<T, RdyError> {
    with_writer_result(e, f)
}

fn review(e: &dyn Engine, s: &Session, v: Value) -> Result<Value, RdyError> {
    run_review(e, s, PERSON, true, None, ReviewRequest::from_value(v)?)
}

fn reviewer() -> Session {
    Session::agent("agent:r".into(), Some("r".into()), Role::Reviewer).unwrap()
}

fn queue(e: &dyn Engine) -> Vec<Item> {
    e.read()
        .pending(&PendingQuery {
            person: PERSON.into(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .unwrap()
        .items
}

fn approve_card(e: &dyn Engine, pref: &str) -> Option<Item> {
    queue(e)
        .into_iter()
        .find(|i| i.r#ref == pref && i.kind == ItemKind::Approve)
}

fn lines(i: &Item) -> Vec<String> {
    i.facts
        .iter()
        .map(|f| format!("{}: {}", f.name, f.value))
        .collect()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=qa",
            "-c",
            "user.email=qa",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit(dir: &Path, path: &str, text: &str, message: &str) -> String {
    std::fs::write(dir.join(path), text).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "--no-verify", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
}

/// Names checkout `app` in the project config and points the machine at `checkout`.
fn link_repo(project: &Path, checkout: &Path) {
    let mut yaml = std::fs::read_to_string(project.join(".rdy.yaml")).unwrap();
    yaml.push_str("code_repos: [app]\n");
    std::fs::write(project.join(".rdy.yaml"), yaml).unwrap();
    std::fs::create_dir_all(project.join(".rdy")).unwrap();
    std::fs::write(
        project.join(".rdy/local.yaml"),
        format!("code_paths:\n  app: {}\n", checkout.display()),
    )
    .unwrap();
}

// ---------------------------------------------------------------- 1. stages

/// The pack of the stage test: claims with an `ok` field, and `rules`.
fn ok_pack(rules: &str) -> String {
    format!(
        "\
name: mg9
version: \"1\"
extends: rdy-standard
block_types:
  claim:
    prefix: c
    traits: [normative, describes]
    body: required
    fields:
      ok: {{ kind: bool }}
rules:
{rules}"
    )
}

fn yaml_rule(name: &str, select: &str, require: &str, severity: &str) -> String {
    format!(
        "  - name: \"{name}\"\n    select: \"{select}\"\n    require: \"{require}\"\n    severity: {severity}\n    message: {name} message\n    fix: {name} fix\n"
    )
}

fn refused_pack(rules: &str) -> String {
    let mut set = PackSet::with_builtin();
    match set.add_yaml(&ok_pack(rules)) {
        Err(SchemaError::Invalid(d)) => return d,
        Err(e) => return format!("{e}"),
        Ok(_) => {}
    }
    match set.resolve("mg9") {
        Err(SchemaError::Invalid(d)) => d,
        other => panic!("expected an invalid pack, got {other:?}"),
    }
}

#[test]
fn mg9_deterministic_stages_never_run_an_agent_check() {
    use Stage::{Change, Land, Propose};
    // the catalogue of stages: deterministic entries only, in their order
    let rows: Vec<(&str, Vec<Stage>)> = STAGE_CHECKS
        .iter()
        .map(|c| (c.name, c.stages.to_vec()))
        .collect();
    assert_eq!(
        rows,
        vec![
            ("tension_delta", vec![Change]),
            ("pack_rules", vec![Change, Propose]),
            ("readiness", vec![Propose]),
            ("suspects", vec![Land]),
            ("events", vec![Land]),
            ("rebases", vec![Land]),
        ]
    );
    let staged: BTreeSet<&str> = STAGE_CHECKS.iter().map(|c| c.name).collect();
    assert_eq!(AGENT_CHECKS.len(), 8);
    for c in AgentCheck::ALL {
        assert!(
            !staged.contains(c.spec().name),
            "{} is staged",
            c.spec().name
        );
    }

    // a blocking rule refuses only at readiness, and only on stable content
    let rules = yaml_rule("ok-claims", "type = claim", "fields.ok = true", "blocking");
    let (d, r) = project();
    drop(r);
    rdy_qa::packs::use_pack(d.path(), "mg9", &ok_pack(&rules), "");
    let mut r = Rdy::open(d.path()).unwrap();
    let seeded = put_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$st", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview", "lifecycle": "stable"}},
            {"op": "create", "type": "section", "id": "$ss", "parent": "$st", "fields": {"title": "Rules"}},
            {"op": "create", "type": "doc", "id": "$dr", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$ds", "parent": "$dr", "fields": {"title": "Ideas"}}
        ]),
        j_typo(),
    );
    land(&mut r, "seed").unwrap();
    let (ss, ds) = (seeded.created["$ss"].clone(), seeded.created["$ds"].clone());
    let e = Arc::new(Local::new(r));
    // change: the write stands, and its reply reports the stable failure
    let v = rdy_engine::consequence::write_and_report(&*e, "st", None, {
        let ss = ss.clone();
        move |r| {
            rdy_qa::tensions::try_put_j(
                r,
                "st",
                json!([{"op": "create", "type": "claim", "parent": ss, "body": "C.", "fields": {"ok": false}}]),
                j_typo(),
            )
        }
    })
    .unwrap();
    assert!(v["change"].is_string(), "{v:#}");
    let codes: Vec<&str> = v["on_change"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, ["E_RULE_CHECK"], "{v:#}");
    // propose: stable is not ready (item 6), draft is advice and ready
    let calls = rdy_engine::readiness::readiness_calls_for_tests();
    let p = on(&*e, |r| {
        answer_open_items(r, &ag("st"), "st");
        r.propose(&ag("st"), "st", "st", Some(TEST_SUMMARY))
    })
    .unwrap();
    assert_eq!(
        rdy_engine::readiness::readiness_calls_for_tests() - calls,
        1
    );
    assert!(!p.ready);
    assert!(
        p.issues.iter().any(|i| i.code == rdy_engine::E_RULE_CHECK),
        "{:#?}",
        p.issues
    );
    assert!(
        approve_card(&*e, &p.r#ref).is_none(),
        "no card while not ready"
    );
    let p = on(&*e, move |r| {
        rdy_qa::tensions::put_j(
            r,
            "dr",
            json!([{"op": "create", "type": "claim", "parent": ds, "body": "D.", "fields": {"ok": false}}]),
            j_typo(),
        );
        r.propose(&ag("dr"), "dr", "dr", Some(TEST_SUMMARY))
    })
    .unwrap();
    assert!(p.ready, "{:#?}", p.issues);
    assert!(
        p.warnings
            .iter()
            .any(|w| w.code == rdy_engine::W_RULE_CHECK)
    );
    // no stage runs at the landing: the evaluator refuses that stage
    let st = e.read();
    let m = st.materialized(&Scope::Canon).unwrap();
    let g = st.graph(&Scope::Canon).unwrap();
    let (empty, none) = (BTreeSet::new(), BTreeMap::new());
    let inputs = rdy_engine::filter::FilterInputs {
        pack: st.pack(),
        m: &m,
        graph: &g,
        areas: st.area_matcher(),
        suspects: &empty,
        tensions: &empty,
        coverage: &none,
    };
    let err =
        rdy_engine::check::evaluate_rules(st.pack(), inputs, std::iter::empty(), Land).unwrap_err();
    assert_eq!(err.code, rdy_engine::E_INTERNAL);
    drop(st);

    // strict pack input and the existing limits: 64 rules, 64-character names, filter bounds
    let any = |n: usize| -> String {
        (0..n)
            .map(|i| {
                yaml_rule(
                    &format!("r{i:03}"),
                    "type = claim",
                    "type = claim",
                    "advisory",
                )
            })
            .collect()
    };
    let mut set = PackSet::with_builtin();
    set.add_yaml(&ok_pack(&any(64))).unwrap();
    assert_eq!(set.resolve("mg9").unwrap().rules.len(), 64);
    assert_eq!(refused_pack(&any(65)), "a pack takes at most 64 rules");
    assert_eq!(
        refused_pack(&yaml_rule(
            &"n".repeat(65),
            "type = claim",
            "type = claim",
            "advisory"
        )),
        "rule name must have 1 to 64 characters"
    );
    let typo = "  - name: t\n    select: type = claim\n    require: type = claim\n    severty: blocking\n    message: m\n    fix: f\n";
    assert!(
        refused_pack(typo).contains("unknown field `severty`"),
        "{}",
        refused_pack(typo)
    );
    let long = format!("fields.ok = true{}", " or fields.ok = true".repeat(110));
    assert!(
        refused_pack(&yaml_rule("long", "type = claim", &long, "advisory"))
            .contains("the filter is longer than 2000 characters")
    );
    let deep = format!("{}type = claim{}", "(".repeat(17), ")".repeat(17));
    assert!(
        refused_pack(&yaml_rule("deep", "type = claim", &deep, "advisory"))
            .contains("nesting deeper than 16")
    );
}

// ---------------------------------------------------------------- 2. the eight cases

#[test]
fn mg9_all_eight_cases_keep_their_subject_and_review_contract() {
    let (dir, mut r) = project();
    let s = spec_doc(&mut r);
    put_j(
        &mut r,
        "prose",
        json!([{"op": "create", "type": "prose", "parent": s.section.to_string(),
                "body": "Readers keep A in mind.",
                "links": {"decided_by": [{"target": s.decision.to_string()}]}}]),
        j_typo(),
    );
    land(&mut r, "prose").unwrap();
    // view a: an editorial edit, an implementing claim, a refining decision with confirmation
    // text, a term and a claim that names it
    put_j(&mut r, "a", upd(&s.c, "C stands alone!"), j_typo());
    let x = created(
        &put_j(
            &mut r,
            "a",
            json!([{"op": "create", "type": "claim", "id": "$x", "parent": s.section.to_string(),
                    "body": "X follows the decision."}]),
            j_implements(&s.decision),
        ),
        "$x",
    );
    let d2 = rdy_qa::tensions::try_decide_in(&mut r, "a", &s.section, "Cache the reads").unwrap();
    put_j(
        &mut r,
        "a",
        json!([{"op": "update", "id": d2.to_string(),
                "fields": {"confirmation": "A test reads the cache."},
                "links": {"refines": [{"target": s.decision.to_string()}]}}]),
        None,
    );
    let t = created(&put(&mut r, "a", term(&s.section, "Cache")), "$t");
    put(&mut r, "a", cite(&s.section, &t));
    let checkout = tempfile::tempdir().unwrap();
    let repo = std::fs::canonicalize(checkout.path()).unwrap();
    git(&repo, &["init", "-q"]);
    let from = commit(
        &repo,
        "cache.rs",
        "fn a() {}\n",
        &format!("start\n\nImplements: {d2}"),
    );
    let to = commit(
        &repo,
        "cache.rs",
        "fn a() { cache(); }\n",
        &format!("cache\n\nImplements: {d2}"),
    );
    link_repo(dir.path(), &repo);
    let p = r.propose(&ag("a"), "a", "a", Some(TEST_SUMMARY)).unwrap();
    assert!(p.ready, "{:#?}", p.issues);
    let version: Hash = p.id.parse().unwrap();
    let e = Local::new(r);
    let gate = |e: &Local| -> String {
        let st = e.read();
        let pref = e
            .inspect(|r| r.resolve_proposal(&version.to_string()))
            .unwrap();
        let c = Candidate::stored(&st, pref, st.reader().proposal(&version).unwrap()).unwrap();
        format!(
            "{:?} {:?}",
            readiness(&st, &c).unwrap().issues,
            assess(&st, &c, &st.gate_config()).unwrap().assessment
        )
    };
    let before_gate = gate(&e);
    let stored_facts = lines(&approve_card(&e, &p.r#ref).expect("a card"));
    let a = ag("a");
    let range = format!("app@{}..{}", &from[..12], &to[..12]);
    // (check, subject, author-run by agent:a, the stored subject, verdict, cited block)
    let pair = {
        let mut v = vec![s.a.clone(), s.c.clone()];
        v.sort();
        v
    };
    let blocks = |b: Vec<BlockId>| (b, None, false);
    let of_version = (vec![], Some(version), false);
    let cases: Vec<(
        &str,
        Value,
        bool,
        (Vec<BlockId>, Option<Hash>, bool),
        &str,
        BlockId,
    )> = vec![
        (
            "contradiction",
            json!([s.c.to_string(), s.a.to_string()]),
            false,
            blocks(pair.clone()),
            "no_tension",
            s.a.clone(),
        ),
        (
            "meaning_preservation",
            json!(p.r#ref),
            true,
            of_version.clone(),
            "pass",
            s.c.clone(),
        ),
        (
            "implementation_fidelity",
            json!(p.r#ref),
            false,
            of_version.clone(),
            "concern",
            x.clone(),
        ),
        (
            "decision_review",
            json!(d2.to_string()),
            true,
            blocks(vec![d2.clone()]),
            "fail",
            d2.clone(),
        ),
        (
            "entity_divergence",
            json!(t.to_string()),
            false,
            blocks(vec![t.clone()]),
            "pass",
            t.clone(),
        ),
        (
            "prose_drift",
            json!(p.r#ref),
            false,
            of_version.clone(),
            "fail",
            d2.clone(),
        ),
        (
            "diff_contradicts",
            json!({"decision": d2.to_string(), "code": range}),
            true,
            (vec![d2.clone()], None, true),
            "concern",
            d2.clone(),
        ),
        (
            "confirmation",
            json!(d2.to_string()),
            false,
            blocks(vec![d2.clone()]),
            "fail",
            d2.clone(),
        ),
    ];
    assert_eq!(
        cases.iter().map(|c| c.0).collect::<Vec<_>>(),
        AGENT_CHECKS.iter().map(|c| c.name).collect::<Vec<_>>(),
        "one row per check, in catalogue order"
    );
    let mut shown = vec![];
    for (check, subject, author_run, (want_blocks, want_version, want_code), verdict, cited) in
        &cases
    {
        let op = if *author_run { "run" } else { "request" };
        let asker = if *check == "contradiction" {
            writer()
        } else {
            a.clone()
        };
        let q = review(
            &e,
            &asker,
            json!({"op": op, "check": check, "subject": subject}),
        )
        .unwrap_or_else(|err| panic!("{check}: {err:#?}"));
        let label = q["request"].as_str().unwrap();
        assert!(
            label.starts_with("rq-") && label.len() == 67,
            "{check}: {label}"
        );
        // the contradiction case is the semantic tension case; the others carry their name
        let kind = if *check == "contradiction" {
            "tension"
        } else {
            check
        };
        assert_eq!(q["case"]["kind"], kind, "{check}: {q:#}");
        let rec = e
            .read()
            .reader()
            .check_request(&label[3..].parse().unwrap())
            .unwrap();
        // the exact subject, ownership and case hash
        assert_eq!(rec.check, *check);
        // a version subject stores the case's subject blocks beside the version
        let subject_rows: Vec<BlockId> = q["case"]["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|b| b["role"] == "subject")
            .map(|b| id(b["id"].as_str().unwrap()))
            .collect();
        if want_version.is_some() {
            assert_eq!(rec.subject.blocks, subject_rows, "{check}: {q:#}");
        } else {
            assert_eq!(&rec.subject.blocks, want_blocks, "{check}");
        }
        assert_eq!(&rec.subject.version, want_version, "{check}");
        assert_eq!(rec.subject.change, None, "{check}");
        assert_eq!(rec.subject.code.is_some(), *want_code, "{check}");
        if let Some(c) = &rec.subject.code {
            assert_eq!(
                (c.repo.as_str(), c.from.as_str(), c.to.as_str()),
                ("app", from.as_str(), to.as_str())
            );
        }
        assert_eq!(rec.case_hash.to_string(), q["case_hash"], "{check}");
        let (mode, actor, run) = if *author_run {
            (CheckMode::Author, "agent:a", Some("a"))
        } else if *check == "contradiction" {
            (
                CheckMode::Independent,
                PERSON,
                rec.requested_by.run.as_deref(),
            )
        } else {
            (CheckMode::Independent, "agent:a", Some("a"))
        };
        assert_eq!(rec.mode, mode, "{check}");
        assert_eq!(
            (
                rec.requested_by.actor.as_str(),
                rec.requested_by.run.as_deref()
            ),
            (actor, run),
            "{check}"
        );
        if *author_run {
            assert_eq!(q["omitted"], 0, "{check}: {q:#}");
        }
        // the reviewer answers the exact case; a self review is refused for an author-run case
        let by = if *author_run { a.clone() } else { reviewer() };
        let pass = matches!(*verdict, "pass" | "no_tension");
        let v = run_review_submit(
            &e,
            &by,
            ReviewSubmitRequest {
                request: label.to_string(),
                case_hash: q["case_hash"].as_str().unwrap().to_string(),
                verdict: verdict.to_string(),
                citations: if pass {
                    vec![]
                } else {
                    vec![CitationRequest {
                        block: cited.to_string(),
                        note: NOTE.into(),
                    }]
                },
                severity: None,
                reason: (!pass).then(|| REASON.to_string()),
            },
        )
        .unwrap_or_else(|err| panic!("{check} submit: {err:#?}"));
        assert!(v["review"].is_string(), "{check}: {v:#}");
        if *check != "contradiction" {
            let mode = if *author_run {
                "author-run"
            } else {
                "independent"
            };
            let who = if *author_run { "agent:a" } else { "agent:r" };
            let name = check.replace('_', " ");
            shown.push(if pass {
                format!("{name}: {verdict} ({who}, {mode})")
            } else {
                format!("{name}: {verdict} ({who}, {mode}): {REASON}")
            });
        }
    }
    // a verdict nobody accepted is a card fact after the stored facts, never a gate input
    let card = approve_card(&e, &p.r#ref).expect("still a card: no verdict is a requirement");
    let n = stored_facts.len();
    assert_eq!(lines(&card)[..n], stored_facts[..]);
    assert_eq!(lines(&card)[n..], shown[..]);
    assert_eq!(
        gate(&e),
        before_gate,
        "readiness and assessment are unchanged"
    );
    drop(checkout);
}

// ---------------------------------------------------------------- 3. sources

#[test]
fn mg9_sources_are_read_facts_not_gate_inputs() {
    let (dir, mut r) = project();
    let checkout = tempfile::tempdir().unwrap();
    let repo = std::fs::canonicalize(checkout.path()).unwrap();
    git(&repo, &["init", "-q"]);
    let sha = commit(&repo, "a.txt", "a\n", "c");
    git(&repo, &["tag", "v1"]);
    drop(r);
    link_repo(dir.path(), &repo);
    r = Rdy::open(dir.path()).unwrap();
    let w = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$sec", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$sec", "body": "X holds."},
            {"op": "create", "type": "source", "id": "$r1", "parent": "$sec", "fields": {"title": "Release 1", "kind": "release", "locator": "v1"}, "body": "R1."},
            {"op": "create", "type": "source", "id": "$r9", "parent": "$sec", "fields": {"title": "Release 9", "kind": "release", "locator": "v9"}, "body": "R9."},
            {"op": "create", "type": "source", "id": "$c1", "parent": "$sec", "fields": {"title": "Commit", "kind": "commit", "locator": format!("app@{sha}")}, "body": "C1."},
            {"op": "create", "type": "source", "id": "$c0", "parent": "$sec", "fields": {"title": "Lost commit", "kind": "commit", "locator": "app@0123456789abcdef0123456789abcdef01234567"}, "body": "C0."},
            {"op": "create", "type": "source", "id": "$sp", "parent": "$sec", "fields": {"title": "The spec", "kind": "spec", "locator": "spec/v1.md"}, "body": "S."}
        ]),
    )
    .unwrap();
    land(&mut r, "seed").unwrap();
    let g = |k: &str| w.created[k].clone();
    let claim = g("$x");
    let sources = ["$r1", "$r9", "$c1", "$c0", "$sp"].map(g);
    write(
        &mut r,
        "linked",
        json!([{"op": "update", "id": claim,
                "links": {"reflects": sources.iter().map(|t| json!({"target": t})).collect::<Vec<_>>()}}]),
    )
    .unwrap();
    land(&mut r, "linked").unwrap();
    // a version to judge: an edit of the claim
    write(&mut r, "v", upd(&id(&claim), "X holds, now.")).unwrap();
    let p = r.propose(&writer(), "v", "v", Some(TEST_SUMMARY)).unwrap();
    let version: Hash = p.id.parse().unwrap();
    let e = Local::new(r);
    let existence = |e: &Local| -> BTreeMap<String, String> {
        let b = e
            .read()
            .brief(
                &Scope::Canon,
                &rdy_engine::Coverage {
                    ids: vec![claim.clone()],
                    path: None,
                },
                None,
            )
            .unwrap();
        let v = serde_json::to_value(&b).unwrap();
        v["sources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| {
                // the closed row: a found, missing or unchecked state, never a truth value
                assert_eq!(
                    s.as_object().unwrap().keys().cloned().collect::<Vec<_>>(),
                    ["blocks", "existence", "kind", "source", "title"],
                    "{s}"
                );
                (
                    s["source"].as_str().unwrap().to_string(),
                    s["existence"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    };
    let gate = |e: &Local| -> String {
        let st = e.read();
        let pref = e
            .inspect(|r| r.resolve_proposal(&version.to_string()))
            .unwrap();
        let c = Candidate::stored(&st, pref, st.reader().proposal(&version).unwrap()).unwrap();
        let a = assess(&st, &c, &st.gate_config()).unwrap().assessment;
        format!("{:?} {a:?}", readiness(&st, &c).unwrap().issues)
    };
    let want = |states: [&str; 5]| -> BTreeMap<String, String> {
        sources
            .iter()
            .cloned()
            .zip(states.map(String::from))
            .collect()
    };
    // the checkout holds v1 and the commit; v9 and the lost commit are missing; a spec is never
    // checked
    assert_eq!(
        existence(&e),
        want(["found", "missing", "found", "missing", "unchecked"])
    );
    let linked = gate(&e);
    // another machine state: the tag is gone, so v1 is missing; the gate reads the same
    git(&repo, &["tag", "-d", "v1"]);
    assert_eq!(
        existence(&e),
        want(["missing", "missing", "found", "missing", "unchecked"])
    );
    assert_eq!(
        gate(&e),
        linked,
        "no machine fact in readiness or the assessment"
    );
    git(&repo, &["tag", "v1"]);
    // an absent checkout: every state is unchecked (the read never fails)
    let moved = repo.with_extension("moved");
    std::fs::rename(&repo, &moved).unwrap();
    assert_eq!(
        existence(&e),
        want([
            "unchecked",
            "unchecked",
            "unchecked",
            "unchecked",
            "unchecked"
        ])
    );
    std::fs::rename(&moved, &repo).unwrap();
    assert_eq!(gate(&e), linked);
    // the stored assessment names no source state
    let st = e.read();
    let h = st
        .reader()
        .assessment_of_proposal(&version, st.seq())
        .unwrap()
        .unwrap();
    let stored = format!("{:?}", st.reader().assessment(&h).unwrap());
    for word in [
        "existence",
        "unchecked",
        "source_not_found",
        "found\"",
        "missing\"",
    ] {
        assert!(
            !stored.contains(word),
            "the assessment holds {word}: {stored}"
        );
    }
    drop(st);
    drop(checkout);
}

// ---------------------------------------------------------------- 4. governing routes

#[test]
fn mg9_governing_and_prose_drift_use_the_four_routes() {
    let (_d, mut r) = project();
    // a stable doc: decision D0, a term, prose governed by each route, prose and a claim that
    // only name D0 in text or link it as related, and claims for the ungrounded fallback
    let w = put_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview", "lifecycle": "stable"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Cache"}, "body": "Holds reads."},
            {"op": "create", "type": "prose", "id": "$paff", "parent": "$s", "body": "Affected prose."},
            {"op": "create", "type": "decision", "id": "$d0", "parent": "$s", "fields": {"statement": "Keep reads", "status": "proposed", "needs_content": false}, "links": {"affects": [{"target": "$paff"}, {"target": "$t"}]}},
            {"op": "create", "type": "prose", "id": "$pdec", "parent": "$s", "body": "Decided prose.", "links": {"decided_by": [{"target": "$d0"}]}},
            {"op": "create", "type": "prose", "id": "$pimp", "parent": "$s", "body": "Implementing prose.", "links": {"implements": [{"target": "$d0"}]}},
            {"op": "create", "type": "prose", "id": "$pent", "parent": "$s", "body": "Entity prose.", "links": {"describes": [{"target": "$t"}]}},
            {"op": "create", "type": "prose", "id": "$ptxt", "parent": "$s", "body": "Text names {{$d0}}."},
            {"op": "create", "type": "claim", "id": "$cgov", "parent": "$s", "body": "Governed claim.", "links": {"decided_by": [{"target": "$d0"}]}},
            {"op": "create", "type": "claim", "id": "$ctxt", "parent": "$s", "body": "Claim names {{$d0}}."}
        ]),
        j_typo(),
    );
    land(&mut r, "seed").unwrap();
    let g = |k: &str| w.created[k].clone();
    let d0 = g("$d0");
    // governing reads: the four routes give D0; text and plain references do not
    let governing = |r: &Rdy, b: &str| -> Vec<String> {
        r.read().context(&Scope::Canon, &id(b)).unwrap().governing
    };
    for k in ["$pdec", "$pimp", "$paff", "$pent", "$cgov"] {
        assert_eq!(governing(&r, &g(k)), vec![d0.clone()], "{k}");
    }
    for k in ["$ptxt", "$ctxt"] {
        assert_eq!(governing(&r, &g(k)), Vec::<String>::new(), "{k}");
    }
    // the fallback: a stable normative block that no route governs is ungrounded
    let issues = r.read().coverage_report(&Scope::Canon, None).unwrap();
    let v = serde_json::to_value(&issues).unwrap();
    let ungrounded: BTreeSet<String> = v["issues"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["code"] == "ungrounded")
        .map(|i| i["block"].as_str().unwrap().to_string())
        .collect();
    assert!(ungrounded.contains(&g("$ctxt")), "{ungrounded:?}");
    assert!(!ungrounded.contains(&g("$cgov")), "{ungrounded:?}");
    // trace's ungrounded fallback follows the same rule (D-X4RYX)
    let traced = |r: &Rdy, b: &str| {
        r.read()
            .trace(
                &Scope::Canon,
                &id(b),
                rdy_engine::trace::TraceOptions::default(),
            )
            .unwrap()
    };
    let t = traced(&r, &g("$ctxt"));
    assert!(t.ungrounded && t.governing.is_empty(), "{t:#?}");
    let t = traced(&r, &g("$cgov"));
    assert!(!t.ungrounded, "{t:#?}");
    // prose drift: a version whose decision supersedes D0 reads exactly the four routes' prose
    let d1 =
        rdy_qa::tensions::try_decide_in(&mut r, "v", &id(&g("$s")), "Keep fewer reads").unwrap();
    put_j(
        &mut r,
        "v",
        json!([{"op": "update", "id": d1.to_string(), "links": {"supersedes": [{"target": d0}]}}]),
        None,
    );
    answer_open_items(&mut r, &ag("v"), "v");
    let p = r.propose(&ag("v"), "v", "v", Some(TEST_SUMMARY)).unwrap();
    let e = Local::new(r);
    let q = review(
        &e,
        &ag("v"),
        json!({"op": "run", "check": "prose_drift", "subject": p.r#ref}),
    )
    .unwrap_or_else(|err| panic!("prose_drift: {err:#?}"));
    let rows: Vec<(String, String)> = q["case"]["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| {
            (
                b["id"].as_str().unwrap().to_string(),
                b["role"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(rows[0], (d1.to_string(), "subject".into()), "{q:#}");
    assert_eq!(rows[1], (d0.clone(), "decision".into()), "{q:#}");
    let governed: BTreeSet<String> = rows[2..]
        .iter()
        .filter(|r| r.1 == "governed")
        .map(|r| r.0.clone())
        .collect();
    let want: BTreeSet<String> = ["$pdec", "$pimp", "$paff", "$pent"].map(g).into();
    assert_eq!(
        governed, want,
        "the four routes, informative blocks only: {q:#}"
    );
    assert_eq!(rows.len(), 2 + want.len(), "{q:#}");
}

// ---------------------------------------------------------------- 5. neighbors

fn neighbor_items(r: &Rdy, view: &str) -> Vec<(String, String, PlanKind, ItemState)> {
    let mut v: Vec<_> = r
        .read()
        .view_plan(view)
        .unwrap()
        .items
        .iter()
        .filter_map(|row| match row {
            PlanRow::Live(i) if i.edge == NEIGHBOR_EDGE => {
                Some((i.cause.clone(), i.subject.clone(), i.kind, i.state))
            }
            _ => None,
        })
        .collect();
    v.sort_by(|x, y| (&x.0, &x.1, x.2).cmp(&(&y.0, &y.1, y.2)));
    v
}

#[test]
fn mg9_neighbours_and_group_answers_use_the_complete_displayed_set() {
    let (_d, mut r) = project();
    let w = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$nb", "fields": {"name": "nb", "title": "nb", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$nb", "fields": {"title": "S"}},
            {"op": "create", "type": "entity", "id": "$e", "parent": "$s", "fields": {"name": "Ledger"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A keeps 30 days.", "links": {"describes": [{"target": "$e"}]}},
            {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "B stands."},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "C stands."},
            {"op": "create", "type": "section", "id": "$s2", "parent": "$nb", "fields": {"title": "S2"}},
            {"op": "create", "type": "claim", "id": "$p", "parent": "$s2", "body": "P keeps 30 days.", "links": {"describes": [{"target": "$e"}]}},
            {"op": "create", "type": "claim", "id": "$q", "parent": "$s2", "body": "Q is fine.", "links": {"describes": [{"target": "$e"}]}}
        ]),
    )
    .unwrap();
    land(&mut r, "seed").unwrap();
    let g = |k: &str| w.created[k].clone();
    let (a, b, c, e, p, s2) = (g("$a"), g("$b"), g("$c"), g("$e"), g("$p"), g("$s2"));
    // an edit: each unchanged content neighbor of the section confirms (the entity block is no
    // text neighbor); the entity neighbor in the doc that states the removed literal updates; q,
    // which states none, has no item
    write(&mut r, "edit", upd(&id(&a), "A keeps 60 days.")).unwrap();
    let got = neighbor_items(&r, "edit");
    let mut want = vec![
        (a.clone(), b.clone(), PlanKind::Confirm, ItemState::Open),
        (a.clone(), c.clone(), PlanKind::Confirm, ItemState::Open),
        (a.clone(), p.clone(), PlanKind::Update, ItemState::Open),
    ];
    want.sort_by(|x, y| (&x.0, &x.1, x.2).cmp(&(&y.0, &y.1, y.2)));
    assert_eq!(got, want);
    // the grouped answer closes exactly the displayed neighbor items
    let spec: Vec<rdy_engine::plan::api::PlanAnswerSpec> = vec![
        serde_json::from_value(
            json!({"group": {"edge": NEIGHBOR_EDGE}, "act": "confirm", "reason": "they hold"}),
        )
        .unwrap(),
    ];
    // the group holds an update item that confirm cannot answer: the answer refuses whole
    let update_item = r
        .read()
        .view_plan("edit")
        .unwrap()
        .items
        .iter()
        .find_map(|row| match row {
            PlanRow::Live(i) if i.edge == NEIGHBOR_EDGE && i.kind == PlanKind::Update => {
                Some(i.id.clone())
            }
            _ => None,
        })
        .unwrap();
    let err = r.answer_plan_specs(&writer(), "edit", &spec).unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str(), err.fix.as_str()),
        (
            "E_PLAN_ITEM",
            format!("confirm does not answer 1 items of the group: {update_item}").as_str(),
            "narrow the group, or answer those items one by one"
        )
    );
    assert_eq!(neighbor_items(&r, "edit"), want, "nothing was answered");
    // a create and a delete confirm the unchanged section neighbors
    write(
        &mut r,
        "create",
        json!([{"op": "create", "type": "claim", "id": "$n", "parent": s2, "body": "N is new."}]),
    )
    .unwrap();
    let made: BTreeSet<String> = neighbor_items(&r, "create")
        .into_iter()
        .map(|i| i.1)
        .collect();
    assert_eq!(
        made,
        BTreeSet::from([p.clone(), g("$q")]),
        "the baseline section neighbors"
    );
    // a group of confirm items: the answer closes exactly the displayed members
    let shown = neighbor_items(&r, "create");
    r.answer_plan_specs(&writer(), "create", &spec).unwrap();
    let after = neighbor_items(&r, "create");
    assert_eq!(after.len(), shown.len(), "no member added or lost");
    for ((c0, s0, k0, _), (c1, s1, k1, st)) in shown.iter().zip(&after) {
        assert_eq!((c0, s0, k0), (c1, s1, k1));
        assert!(matches!(st, ItemState::Closed), "{s1}: {st:?}");
    }
    write(&mut r, "delete", json!([{"op": "delete", "id": c}])).unwrap();
    let gone: BTreeSet<String> = neighbor_items(&r, "delete")
        .into_iter()
        .map(|i| i.1)
        .collect();
    assert_eq!(gone, BTreeSet::from([a.clone(), b.clone()]));
    // a move, a link change and a re-order are no text change: no neighbor item
    write(
        &mut r,
        "move",
        json!([{"op": "move", "id": b, "parent": s2}]),
    )
    .unwrap();
    assert_eq!(neighbor_items(&r, "move"), vec![]);
    write(
        &mut r,
        "links",
        json!([{"op": "update", "id": b, "links": {"describes": [{"target": e}]}}]),
    )
    .unwrap();
    assert_eq!(neighbor_items(&r, "links"), vec![]);
    // complete paging: every page of the plan, read whole, is the unpaged plan
    let whole = r.read().view_plan("edit").unwrap();
    assert!(whole.next.is_none(), "{whole:#?}");
    assert_eq!(
        whole.items.len(),
        r.read().view_plan("edit").unwrap().items.len()
    );
    // the bound: past it a read refuses whole (pinned at full size by nb8_neighbor_bounds_*)
    assert_eq!(rdy_engine::consequence::MAX_NEIGHBOR_PAIRS, 100_000);
}

// ---------------------------------------------------------------- 6. read tools

fn served() -> (Served, McpClient, String, String) {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Steps"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds."},
            {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "B builds on {{$a}}."}
        ]),
    )
    .unwrap();
    let l1 = land(&mut r, "work").unwrap();
    r.tag(&approver(), "v0.1.0", None, None).unwrap();
    let ops: Vec<Value> = (0..12)
        .map(|i| json!({"op": "create", "type": "claim", "parent": w.created["$s"], "body": format!("More {i} holds.")}))
        .collect();
    write(&mut r, "more", Value::Array(ops)).unwrap();
    land(&mut r, "more").unwrap();
    r.close().unwrap();
    let mut c = McpClient::start_proxy(&s);
    c.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "a1", "role": "writer"}),
    )
    .ok();
    (s, c, w.created["$a"].clone(), l1)
}

fn ids_of(v: &Value, key: &str) -> Vec<String> {
    v[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key}: {v}"))
        .iter()
        .map(|m| m["id"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn mg9_query_diff_and_trace_have_chat_cli_parity() {
    let (s, mut c, a, l1) = served();
    // no separate as-of tool
    let names: Vec<String> = c
        .tools()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&"rdy_trace".to_string()), "{names:?}");
    assert!(
        !names
            .iter()
            .any(|n| n.contains("asof") || n.contains("as_of")),
        "{names:?}"
    );
    // query: the grammar, stable order and parity
    let q = c
        .call(
            "rdy_query",
            json!({"filter": "type = claim", "scope": "canon"}),
        )
        .ok();
    let (ok, cli) = s.json(&["--json", "q", "type = claim"]);
    assert!(ok, "{cli}");
    assert_eq!(ids_of(&q, "matches"), ids_of(&cli, "matches"));
    let mut sorted = ids_of(&q, "matches");
    sorted.sort();
    assert_eq!(ids_of(&q, "matches").len(), 14);
    assert_eq!(sorted.len(), 14);
    let bad = c.call("rdy_query", json!({"filter": "type = = claim"}));
    assert!(!bad.is_ok());
    let (ok, cli_bad) = s.json(&["--json", "q", "type = = claim"]);
    assert!(!ok);
    assert_eq!(bad.code(), cli_bad["code"].as_str(), "{cli_bad}");
    let e = c.call(
        "rdy_query",
        json!({"filter": "type = claim", "max_tokens": 100_001}),
    );
    assert_eq!(e.code(), Some("E_LIMIT"));
    // diff from a tag and from a landing: whole counts on every page, and parity
    for from in ["tag:v0.1.0".to_string(), format!("landing:{l1}")] {
        let first = c
            .call(
                "rdy_diff",
                json!({"from": from, "to": "canon", "max_tokens": 600}),
            )
            .ok();
        let (ok, cli) = s.json(&["--json", "diff", &from, "canon"]);
        assert!(ok, "{cli}");
        assert_eq!(first["counts"], cli["counts"], "{from}: {first} / {cli}");
        assert_eq!(
            first["counts"],
            json!({"added": 12, "removed": 0, "updated": 0, "moved": 0}),
            "{from}: {first}"
        );
        let mut seen: Vec<String> = ids_of(&first, "entries");
        let mut v = first.clone();
        let mut pages = 1;
        while let Some(next) = v["next"].as_str().map(String::from) {
            v = c
                .call(
                    "rdy_diff",
                    json!({"from": from, "to": "canon", "max_tokens": 600, "page": next}),
                )
                .ok();
            assert_eq!(
                v["counts"], first["counts"],
                "the whole diff's counts on each page"
            );
            seen.extend(ids_of(&v, "entries"));
            pages += 1;
            assert!(pages < 50);
        }
        assert!(pages > 1, "{from}: the diff pages");
        assert_eq!(seen, ids_of(&cli, "entries"), "{from}: the same order");
    }
    // trace: id@version, and parity
    let t = c.call("rdy_trace", json!({"id": a, "scope": "canon"})).ok();
    let (ok, cli) = s.json(&["--json", "trace", &a, "--scope", "canon"]);
    assert!(ok, "{cli}");
    assert_eq!(t, cli);
    assert_eq!(t["id"], a.as_str());
    assert_eq!(
        t["ungrounded"], false,
        "a draft claim is never ungrounded: {t}"
    );
}

// ---------------------------------------------------------------- 7. Grounded method 2

#[test]
fn mg9_grounded_method_two_preserves_original_cards() {
    use rdy_engine::readiness::assessment::METHOD;
    use rdy_model::assessment::Assessment;
    assert_eq!(METHOD, 2);
    let (_dir, mut r) = project();
    let w = put_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$t", "parent": "$n", "fields": {"title": "Ideas"}},
            {"op": "create", "type": "term", "id": "$l", "parent": "$t", "fields": {"name": "Ledger"}, "body": "Holds balances."},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$t", "body": "X holds."},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$t", "body": "A small note."}
        ]),
        j_typo(),
    );
    land(&mut r, "seed").unwrap();
    let g = |k: &str| w.created[k].clone();
    let (x, prose, ledger) = (g("$x"), g("$p"), g("$l"));
    let e = Arc::new(Local::new(r));
    let mentions = |a: &Assessment| {
        a.issues
            .iter()
            .chain(a.confidence.reasons.iter())
            .filter(|i| i.code == "unlinked_mention")
            .count()
    };
    let stored = |e: &dyn Engine, p: &ProposalInfo| -> (Hash, Assessment) {
        let st = e.read();
        let h = st
            .reader()
            .assessment_of_proposal(&p.id.parse().unwrap(), st.seq())
            .unwrap()
            .unwrap();
        (h, st.reader().assessment(&h).unwrap())
    };
    let propose = |e: &dyn Engine, view: &'static str| -> ProposalInfo {
        on(e, move |r| {
            r.propose(&writer(), view, view, Some(TEST_SUMMARY))
        })
        .unwrap()
    };
    // a normative mention scores
    let mut st = rdy_mcp::tools::McpState::new(e.clone());
    rdy_mcp::tools::rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:a", "run": "a", "role": "writer"})).unwrap(),
    )
    .unwrap();
    on(&*e, move |r| {
        Ok(put_j(
            r,
            "a",
            upd(&id(&x), "X holds, as the Ledger records."),
            j_typo(),
        ))
    })
    .unwrap();
    let pa = propose(&*e, "a");
    let (ha, a) = stored(&*e, &pa);
    assert_eq!(a.method, 2);
    assert!(mentions(&a) >= 1, "a normative mention is a gap: {a:#?}");
    // an informative mention does not score; the write's link suggestion is still readable
    let v = rdy_mcp::tools::rdy_write(
        &mut st,
        serde_json::from_value(json!({"view": "b", "ops": [
            {"op": "update", "id": prose, "body": "A small note about the Ledger."}
        ]}))
        .unwrap(),
    )
    .unwrap();
    let targets: Vec<&str> = v["suggestions"]
        .as_array()
        .unwrap_or_else(|| panic!("suggestions: {v:#}"))
        .iter()
        .map(|s| s["target"].as_str().unwrap())
        .collect();
    assert_eq!(targets, [ledger.as_str()], "{v:#}");
    let pb = propose(&*e, "b");
    let (hb, b) = stored(&*e, &pb);
    assert_eq!(b.method, 2);
    assert_eq!(
        mentions(&b),
        0,
        "an informative mention scores nothing: {b:#?}"
    );
    // the card shows the stored facts; a stored method-1 record stays its own bytes and card
    let card = |e: &dyn Engine, pref: &str| -> Vec<String> {
        approve_card(e, pref).map(|i| lines(&i)).unwrap_or_default()
    };
    assert_eq!(card(&*e, &pa.r#ref), a.card.facts);
    let mut old = a.clone();
    old.method = 1;
    old.card.facts = vec!["Coverage: scored under the first rule".into()];
    old.card.hash = Hash::of(b"mg9 old card");
    assert_eq!(old.validate(), Ok(()));
    let put = old.clone();
    let old_hash = on(&*e, move |r| {
        let h = r.store_mut().put_assessment(&put)?;
        r.invalidate();
        Ok(h)
    })
    .unwrap();
    assert_ne!(old_hash, ha);
    let check = |e: &dyn Engine, step: &str| {
        let (h, got) = stored(e, &pa);
        assert_eq!((h, &got), (old_hash, &old), "{step}: the method-1 bytes");
        assert!(
            card(e, &pa.r#ref).contains(&old.card.facts[0]),
            "{step}: the original card: {:?}",
            card(e, &pa.r#ref)
        );
        assert_eq!(
            stored(e, &pb),
            (hb, b.clone()),
            "{step}: the method-2 record"
        );
    };
    check(&*e, "now");
    on(&*e, |r| r.reindex()).unwrap();
    check(&*e, "reindex");
}
