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
            &format!(
                "core.hooksPath={}",
                rdy_qa::git_repo::git_hooks_dir().display()
            ),
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

/// One row of the eight-case table: the check, its subject, whether its author runs it, the
/// stored subject (blocks, version, whether it names code), the verdict and the cited block.
type Case<'a> = (
    &'a str,
    Value,
    bool,
    (Vec<BlockId>, Option<Hash>, bool),
    &'a str,
    BlockId,
);

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
    // a version subject stores the blocks its recipe selects, known here from the writes: the
    // one editorial edit, the one implementing claim, the one decision that refines another
    let of_version = |b: Vec<BlockId>| (b, Some(version), false);
    let cases: Vec<Case> = vec![
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
            of_version(vec![s.c.clone()]),
            "pass",
            s.c.clone(),
        ),
        (
            "implementation_fidelity",
            json!(p.r#ref),
            false,
            of_version(vec![x.clone()]),
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
            of_version(vec![d2.clone()]),
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
        assert_eq!(&rec.subject.blocks, want_blocks, "{check}: {q:#}");
        if *check != "contradiction" {
            assert_eq!(
                &subject_rows, want_blocks,
                "{check}: the case's subject rows: {q:#}"
            );
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
        if *check != "contradiction" {
            // the case hash, version 2: the domain, then the case's JSON with sorted keys
            let mut bytes = b"rdy-case-2\0".to_vec();
            bytes.extend(serde_json::to_vec(&q["case"]).unwrap());
            assert_eq!(rec.case_hash, Hash::of(&bytes), "{check}: the case hash");
            assert_eq!(q["case"]["kind"], *check);
        }
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
        let pass = matches!(*verdict, "pass" | "no_tension");
        let submit = |by: &Session, hash: &str| {
            run_review_submit(
                &e,
                by,
                ReviewSubmitRequest {
                    request: label.to_string(),
                    case_hash: hash.to_string(),
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
        };
        let refused = |by: &Session, hash: &str| -> (&'static str, String) {
            let err = submit(by, hash).expect_err(check);
            (err.code, err.message)
        };
        let hash = q["case_hash"].as_str().unwrap();
        // ownership: an author-run case is its author's alone; an independent case needs the
        // reviewer role and a reviewer who wrote no subject block
        if *author_run {
            assert_eq!(
                refused(&reviewer(), hash),
                (
                    "E_PERMISSION",
                    format!("{label} is an author-run check of agent:a")
                ),
                "{check}"
            );
        } else {
            assert_eq!(
                refused(&ag("a"), hash),
                (
                    "E_PERMISSION",
                    "rdy_review_submit needs the reviewer role".to_string()
                ),
                "{check}"
            );
            if *check != "contradiction" {
                let author =
                    Session::agent("agent:a".into(), Some("a2".into()), Role::Reviewer).unwrap();
                assert_eq!(
                    refused(&author, hash),
                    (
                        "E_SELF_REVIEW",
                        format!(
                            "actor agent:a wrote block {} of {label}; another run of the same actor is not independent",
                            want_blocks[0]
                        )
                    ),
                    "{check}"
                );
            }
        }
        // a hash other than the case's is refused
        let other = Hash::of(b"mg9 other case").to_string();
        assert_eq!(
            refused(&if *author_run { a.clone() } else { reviewer() }, &other),
            (
                "E_REVIEW_STALE",
                format!("the case of {label} changed since you read it")
            ),
            "{check}"
        );
        // the owner answers the exact case
        let by = if *author_run { a.clone() } else { reviewer() };
        let v = submit(&by, hash).unwrap_or_else(|err| panic!("{check} submit: {err:#?}"));
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

    // a case past its cap: an entity case shows its subject and at most 20 tied blocks, in id
    // order within the doc, and counts the rest as omitted
    let (_cap, mut r) = project();
    let w = put(
        &mut r,
        "k",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "cap", "title": "Cap", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}}
        ]),
    );
    let sec = created(&w, "$s");
    let t = created(&put(&mut r, "k", term(&sec, "Ledger")), "$t");
    let mut tied: Vec<BlockId> = (0..25)
        .map(|_| created(&put(&mut r, "k", cite(&sec, &t)), "$y"))
        .collect();
    tied.sort();
    let e = Local::new(r);
    let q = review(
        &e,
        &ag("k"),
        json!({"op": "run", "check": "entity_divergence", "subject": t.to_string()}),
    )
    .unwrap();
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
    let mut want = vec![(t.to_string(), "subject".to_string())];
    want.extend(
        tied[..20]
            .iter()
            .map(|b| (b.to_string(), "entity".to_string())),
    );
    assert_eq!(
        rows, want,
        "the subject and the first 20 tied blocks: {q:#}"
    );
    // each tied block shows its shown text whole (a mention reads as its id); the five left out
    // count their bytes
    let body = format!("Builds on {t}.");
    for b in &q["case"]["blocks"].as_array().unwrap()[1..] {
        assert_eq!(
            b["after"]["text"],
            json!([{"field": "body", "text": body}]),
            "{b:#}"
        );
    }
    assert_eq!(
        q["case"]["omitted"],
        json!({"blocks": 5, "text_bytes": 5 * body.len(), "code_files": 0, "code_bytes": 0}),
        "{q:#}"
    );
    assert_eq!(q["omitted"], 0, "no case is left out: {q:#}");
    let rec = e
        .read()
        .reader()
        .check_request(&q["request"].as_str().unwrap()[3..].parse().unwrap())
        .unwrap();
    assert_eq!(rec.subject.blocks, vec![t.clone()]);
}

// ---------------------------------------------------------------- 3. sources

/// The gate score (O17) of `scope`: of the blocks whose page is `doc`, or of every scored block.
fn gate_score(st: &rdy_engine::ReadState, scope: &Scope, doc: Option<&BlockId>) -> u32 {
    let m = st.materialized(scope).unwrap();
    let issues = st.coverage_issues(scope).unwrap();
    let entries = issues
        .blocks
        .iter()
        .filter(|(_, e)| doc.is_none_or(|d| e.doc.as_ref() == Some(d)));
    rdy_engine::coverage::gate_rollup(st.pack(), &m, entries).score
}

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
    let (claim, doc, sec) = (g("$x"), id(&g("$d")), g("$sec"));
    let sources = ["$r1", "$r9", "$c1", "$c0", "$sp"].map(g);
    let reflects =
        |list: &[String]| -> Vec<Value> { list.iter().map(|t| json!({"target": t})).collect() };
    // canon cites every source but v1
    write(
        &mut r,
        "linked",
        json!([{"op": "update", "id": claim, "links": {"reflects": reflects(&sources[1..])}}]),
    )
    .unwrap();
    land(&mut r, "linked").unwrap();
    // from here the coverage gate is no_lower, so a lower gate score refuses a version
    r.close().unwrap();
    let mut yaml = std::fs::read_to_string(dir.path().join(".rdy.yaml")).unwrap();
    yaml.push_str("coverage:\n  gate: no_lower\n");
    std::fs::write(dir.path().join(".rdy.yaml"), yaml).unwrap();
    r = Rdy::open(dir.path()).unwrap();
    assert_eq!(r.read().coverage_gate(), rdy_engine::CoverageGate::NoLower);
    // the version to judge: an edit of the claim that also cites v1, the source whose existence
    // the machine changes below
    write(
        &mut r,
        "v",
        json!([{"op": "update", "id": claim, "body": "X holds, now.", "links": {"reflects": reflects(&sources)}}]),
    )
    .unwrap();
    let p = r.propose(&writer(), "v", "v", Some(TEST_SUMMARY)).unwrap();
    assert!(p.ready, "a lowered score would refuse it: {:#?}", p.issues);
    let version: Hash = p.id.parse().unwrap();
    // the control: a version that lowers the gate score is refused, so the gate is live
    write(
        &mut r,
        "low",
        json!([{"op": "create", "type": "question", "parent": sec, "fields": {"prompt": "Is X enough?"}}]),
    )
    .unwrap();
    let low = r
        .propose(&writer(), "low", "low", Some(TEST_SUMMARY))
        .unwrap();
    assert!(
        low.issues
            .iter()
            .any(|i| i.code == rdy_engine::E_COVERAGE_LOWERED),
        "{:#?}",
        low.issues
    );
    let e = Local::new(r);
    let vscope = Scope::View("v".into());
    let existence = |e: &Local| -> BTreeMap<String, String> {
        let b = e
            .read()
            .brief(
                &vscope,
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
    // readiness, the assessment, and the gate scores no_lower compares: of the page and of the
    // corpus, in canon and with the version
    let gate = |e: &Local| -> (String, String, Vec<u32>) {
        let st = e.read();
        let pref = e
            .inspect(|r| r.resolve_proposal(&version.to_string()))
            .unwrap();
        let c = Candidate::stored(&st, pref, st.reader().proposal(&version).unwrap()).unwrap();
        let a = assess(&st, &c, &st.gate_config()).unwrap().assessment;
        let issues = readiness(&st, &c).unwrap().issues;
        assert!(
            !issues
                .iter()
                .any(|i| i.code == rdy_engine::E_COVERAGE_LOWERED),
            "{issues:#?}"
        );
        let scores = [&Scope::Canon, &vscope]
            .into_iter()
            .flat_map(|sc| [gate_score(&st, sc, Some(&doc)), gate_score(&st, sc, None)])
            .collect();
        (format!("{issues:?}"), format!("{a:?}"), scores)
    };
    // the displayed coverage of the version reads the facts: one source_not_found per missing
    // source the claim cites
    let shown_missing = |e: &Local| -> usize {
        let v = serde_json::to_value(e.read().coverage_report(&vscope, None).unwrap()).unwrap();
        v["issues"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["code"] == "source_not_found" && i["block"] == claim.as_str())
            .count()
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
    assert_eq!(shown_missing(&e), 2);
    let linked = gate(&e);
    // another machine state: the tag is gone, so v1, which only the version cites, is missing;
    // the readiness, the assessment and every gate score read the same
    git(&repo, &["tag", "-d", "v1"]);
    assert_eq!(
        existence(&e),
        want(["missing", "missing", "found", "missing", "unchecked"])
    );
    assert_eq!(shown_missing(&e), 3, "the read-time fact changed");
    assert_eq!(
        gate(&e),
        linked,
        "no machine fact in readiness, the assessment or the no_lower scores"
    );
    // and the reverse: v9 is tagged, so a source canon cites is found
    git(&repo, &["tag", "v9"]);
    assert_eq!(
        existence(&e),
        want(["missing", "found", "found", "missing", "unchecked"])
    );
    assert_eq!(shown_missing(&e), 2);
    assert_eq!(gate(&e), linked);
    git(&repo, &["tag", "-d", "v9"]);
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
    // a repin is no text change: the pin follows a changed term, and the repinned block has no
    // neighbor item
    let w = write(
        &mut r,
        "seed2",
        json!([
            {"op": "create", "type": "term", "id": "$t1", "parent": s2, "fields": {"name": "Span"}, "body": "A span."},
            {"op": "create", "type": "claim", "id": "$r", "parent": s2, "body": "R waits one {{$t1}}."}
        ]),
    )
    .unwrap();
    land(&mut r, "seed2").unwrap();
    let (t1, rp) = (w.created["$t1"].clone(), w.created["$r"].clone());
    write(&mut r, "repin", upd(&id(&t1), "A longer span.")).unwrap();
    write(
        &mut r,
        "repin",
        json!([{"op": "repin", "id": rp, "targets": [t1]}]),
    )
    .unwrap();
    assert!(
        neighbor_items(&r, "repin").iter().all(|i| i.0 != rp),
        "{:#?}",
        neighbor_items(&r, "repin")
    );
    // the bound: past it a read refuses whole (pinned at full size by nb8_neighbor_bounds_*)
    assert_eq!(rdy_engine::consequence::MAX_NEIGHBOR_PAIRS, 100_000);

    // a tracking-field edit is no text change
    let (td, tr) = project();
    drop(tr);
    rdy_qa::packs::use_pack(td.path(), "trk", TRK_PACK, "");
    let mut t = Rdy::open(td.path()).unwrap();
    let w = write(
        &mut t,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "trk", "title": "trk", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds.", "fields": {"owner": "ana"}},
            {"op": "create", "type": "claim", "id": "$n", "parent": "$s", "body": "N stands."}
        ]),
    )
    .unwrap();
    land(&mut t, "seed").unwrap();
    let ta = w.created["$a"].clone();
    write(
        &mut t,
        "track",
        json!([{"op": "update", "id": ta, "fields": {"owner": "bo"}}]),
    )
    .unwrap();
    assert_eq!(neighbor_items(&t, "track"), vec![]);
    // and a text edit of the same block is one: the control
    write(&mut t, "text", upd(&id(&ta), "A holds firm.")).unwrap();
    assert_eq!(neighbor_items(&t, "text").len(), 1);
    drop(t);

    // complete paging: an edit with many neighbors, read through rdy_plan in small pages; the
    // grouped confirm answers exactly the displayed confirm items
    let (_pd, mut r) = project();
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$nb", "fields": {"name": "pg", "title": "pg", "doc_type": "page"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$nb", "fields": {"title": "S"}}),
        json!({"op": "create", "type": "entity", "id": "$e", "parent": "$s", "fields": {"name": "Ledger"}}),
        json!({"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A keeps 30 days.", "links": {"describes": [{"target": "$e"}]}}),
        json!({"op": "create", "type": "section", "id": "$s2", "parent": "$nb", "fields": {"title": "S2"}}),
        json!({"op": "create", "type": "claim", "id": "$p", "parent": "$s2", "body": "P keeps 30 days.", "links": {"describes": [{"target": "$e"}]}}),
    ];
    let names: Vec<String> = (0..14).map(|i| format!("$n{i}")).collect();
    ops.extend(names.iter().enumerate().map(|(i, n)| {
        json!({"op": "create", "type": "claim", "id": n, "parent": "$s", "body": format!("N{i} stands.")})
    }));
    let w = write(&mut r, "seed", Value::Array(ops)).unwrap();
    land(&mut r, "seed").unwrap();
    let (a, p) = (w.created["$a"].clone(), w.created["$p"].clone());
    let neighbors: BTreeSet<String> = names.iter().map(|n| w.created[n].clone()).collect();
    put(&mut r, "w", upd(&id(&a), "A keeps 60 days."));
    let engine = Arc::new(Local::new(r));
    let mut st = rdy_mcp::tools::McpState::new(engine.clone());
    rdy_mcp::tools::rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:w", "role": "writer", "run": "w"})).unwrap(),
    )
    .unwrap();
    let mut plan = |args: Value| {
        rdy_mcp::tools::rdy_plan(&mut st, serde_json::from_value(args).unwrap())
            .unwrap_or_else(|e| panic!("rdy_plan: {e}"))
    };
    let mut v = plan(json!({"view": "w", "max_tokens": 150}));
    let total = v["counts"].clone();
    let mut shown: Vec<Value> = vec![];
    let mut pages = 1;
    loop {
        shown.extend(v["items"].as_array().unwrap().iter().cloned());
        let Some(next) = v["next"].as_str().map(String::from) else {
            break;
        };
        v = plan(json!({"page": next, "max_tokens": 150}));
        assert_eq!(v["counts"], total, "each page counts the whole plan");
        pages += 1;
        assert!(pages < 100);
    }
    assert!(pages > 2, "the plan pages: {pages}");
    let item_ids: Vec<&str> = shown.iter().map(|i| i["id"].as_str().unwrap()).collect();
    assert_eq!(
        item_ids.iter().collect::<BTreeSet<_>>().len(),
        item_ids.len(),
        "each item once"
    );
    let rows = |kind: &str| -> BTreeMap<String, String> {
        shown
            .iter()
            .filter(|i| i["edge"] == NEIGHBOR_EDGE && i["kind"] == kind)
            .map(|i| {
                assert_eq!(i["cause"], a.as_str(), "{i:#}");
                assert_eq!(i["state"], "open", "{i:#}");
                (
                    i["id"].as_str().unwrap().to_string(),
                    i["subject"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    };
    let confirms = rows("confirm");
    let updates = rows("update");
    assert_eq!(
        confirms.values().cloned().collect::<BTreeSet<_>>(),
        neighbors,
        "every section neighbor, across the pages"
    );
    assert_eq!(
        updates.values().cloned().collect::<Vec<_>>(),
        vec![p.clone()]
    );
    // the grouped confirm over the displayed kind
    let after = plan(json!({"view": "w", "max_tokens": 100_000, "answers": [
        {"group": {"edge": NEIGHBOR_EDGE, "kind": "confirm"}, "act": "confirm", "reason": "they hold"}]}));
    assert!(after["next"].is_null(), "{after:#}");
    let before: BTreeMap<String, Value> = shown
        .iter()
        .map(|i| (i["id"].as_str().unwrap().to_string(), i.clone()))
        .collect();
    let now: BTreeMap<String, Value> = after["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| (i["id"].as_str().unwrap().to_string(), i.clone()))
        .collect();
    assert_eq!(
        now.keys().collect::<Vec<_>>(),
        before.keys().collect::<Vec<_>>(),
        "the same items"
    );
    let changed: BTreeSet<String> = now
        .iter()
        .filter(|(k, i)| before[*k] != **i)
        .map(|(k, _)| k.clone())
        .collect();
    assert_eq!(
        changed,
        confirms.keys().cloned().collect::<BTreeSet<_>>(),
        "exactly the displayed confirm members changed"
    );
    for k in &changed {
        assert_eq!(now[k]["state"], "closed", "{:#}", now[k]);
        assert_eq!(now[k]["answer"]["act"], "confirm");
        assert_eq!(now[k]["answer"]["reason"], "they hold");
    }
    for k in updates.keys() {
        assert_eq!(now[k]["state"], "open", "the update item stays open");
    }
}

/// A pack whose claims track `owner` (trk1): left out of the content hash and of the text.
const TRK_PACK: &str = "\
name: trk
version: \"1\"
extends: rdy-standard
block_types:
  claim:
    prefix: c
    traits: [normative, describes]
    body: required
    fields:
      owner: { kind: str }
    tracking: [owner]
";

// ---------------------------------------------------------------- 6. read tools

/// The read-tool world, landed: claims `a` and `b` (landing `l1`, tagged v0.1.0), then twelve
/// more claims and an edit of `a` (landing `l2`). Returns the ids in reading order, `a` first,
/// the two landings and the section.
fn read_world(r: &mut Rdy) -> (Vec<String>, String, String, String) {
    let w = write(
        r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Steps"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds."},
            {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "B builds on {{$a}}."}
        ]),
    )
    .unwrap();
    let l1 = land(r, "work").unwrap();
    r.tag(&approver(), "v0.1.0", None, None).unwrap();
    let a = w.created["$a"].clone();
    let mut ops: Vec<Value> = (0..12)
        .map(|i| json!({"op": "create", "type": "claim", "id": format!("$m{i}"), "parent": w.created["$s"], "body": format!("More {i} holds.")}))
        .collect();
    ops.push(json!({"op": "update", "id": a, "body": "A holds firmly."}));
    let more = write(r, "more", Value::Array(ops)).unwrap();
    let l2 = land(r, "more").unwrap();
    let mut order = vec![a, w.created["$b"].clone()];
    order.extend((0..12).map(|i| more.created[&format!("$m{i}")].clone()));
    (order, l1, l2, w.created["$s"].clone())
}

fn served() -> (Served, McpClient, Vec<String>, String) {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let (order, l1, _, _) = read_world(&mut r);
    r.close().unwrap();
    let mut c = McpClient::start_proxy(&s);
    c.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "a1", "role": "writer"}),
    )
    .ok();
    (s, c, order, l1)
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
    let (s, mut c, order, l1) = served();
    let a = order[0].clone();
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
    // query: the grammar, reading order and parity
    let q = c
        .call(
            "rdy_query",
            json!({"filter": "type = claim", "scope": "canon"}),
        )
        .ok();
    let (ok, cli) = s.json(&["--json", "q", "type = claim"]);
    assert!(ok, "{cli}");
    assert_eq!(ids_of(&q, "matches"), order, "the reading order");
    assert_eq!(ids_of(&cli, "matches"), order, "the reading order");
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
    // diff from a tag and from a landing: whole counts on every page, and parity; the entries in
    // the reading order of the newer side: the edited a, then the twelve new claims
    let mut want_entries = vec![a.clone()];
    want_entries.extend(order[2..].iter().cloned());
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
            json!({"added": 12, "removed": 0, "updated": 1, "moved": 0}),
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
        assert_eq!(seen, want_entries, "{from}: the order");
        assert_eq!(
            ids_of(&cli, "entries"),
            want_entries,
            "{from}: the CLI order"
        );
    }
    // trace: canon reads a as it is now; a@v0.1.0 reads it under the tag, through both surfaces,
    // and equals the historical scopes tag:v0.1.0 and landing:<l1>
    let trace = |c: &mut McpClient, args: Value, cli_args: &[&str]| -> Value {
        let t = c.call("rdy_trace", args).ok();
        let mut full = vec!["--json", "trace"];
        full.extend_from_slice(cli_args);
        let (ok, cli) = s.json(&full);
        assert!(ok, "{cli}");
        assert_eq!(t, cli, "{cli_args:?}");
        t
    };
    let now = trace(
        &mut c,
        json!({"id": a, "scope": "canon"}),
        &[&a, "--scope", "canon"],
    );
    assert_eq!(now["id"], a.as_str());
    assert_eq!(now["summary"], "A holds firmly.", "{now}");
    assert_eq!(
        now["ungrounded"], false,
        "a draft claim is never ungrounded: {now}"
    );
    let pinned = format!("{a}@v0.1.0");
    let then = trace(&mut c, json!({"id": pinned}), &[&pinned]);
    assert_eq!(then["id"], a.as_str());
    assert_eq!(then["summary"], "A holds.", "{then}");
    let tagged = trace(
        &mut c,
        json!({"id": a, "scope": "tag:v0.1.0"}),
        &[&a, "--scope", "tag:v0.1.0"],
    );
    assert_eq!(tagged, then);
    let landed = format!("landing:{l1}");
    let at_landing = trace(
        &mut c,
        json!({"id": a, "scope": landed}),
        &[&a, "--scope", &landed],
    );
    assert_eq!(at_landing["summary"], "A holds.", "{at_landing}");
    // a pin and another scope clash, on both surfaces
    let clash = c.call("rdy_trace", json!({"id": pinned, "scope": "canon"}));
    let (ok, cli_clash) = s.json(&["--json", "trace", &pinned, "--scope", "canon"]);
    assert!(!ok);
    assert_eq!(clash.code(), Some("E_BAD_INPUT"));
    assert_eq!(clash.code(), cli_clash["code"].as_str(), "{cli_clash}");
    drop(c);
    drop(s);

    // freeze: a page cursor reads the state its first page read. Over canon, a landing between
    // pages makes the next page stale; between two fixed states, the pages go on across it.
    let (_d, mut r) = project();
    let (order, _, l2, sec) = read_world(&mut r);
    let engine = Arc::new(Local::new(r));
    let mut st = rdy_mcp::tools::McpState::new(engine.clone());
    rdy_mcp::tools::rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:a", "run": "a1", "role": "writer"})).unwrap(),
    )
    .unwrap();
    let q1 = rdy_mcp::tools::rdy_query(
        &mut st,
        serde_json::from_value(
            json!({"filter": "type = claim", "scope": "canon", "max_tokens": 60}),
        )
        .unwrap(),
    )
    .unwrap();
    let canon_diff = |st: &mut rdy_mcp::tools::McpState, page: Option<&str>, to: &str| {
        rdy_mcp::tools::rdy_diff(
            st,
            serde_json::from_value(
                json!({"from": "tag:v0.1.0", "to": to, "max_tokens": 600, "page": page}),
            )
            .unwrap(),
        )
    };
    let d1 = canon_diff(&mut st, None, "canon").unwrap();
    let fixed = format!("landing:{l2}");
    let f1 = canon_diff(&mut st, None, &fixed).unwrap();
    let (qn, dn, fnext) = (
        q1["next"].as_str().expect("the query pages").to_string(),
        d1["next"].as_str().expect("the diff pages").to_string(),
        f1["next"]
            .as_str()
            .expect("the fixed diff pages")
            .to_string(),
    );
    // a competing landing between the pages
    on(&*engine, move |r| {
        write(
            r,
            "late",
            json!([{"op": "create", "type": "claim", "parent": sec, "body": "Late holds."}]),
        )?;
        land(r, "late")
    })
    .unwrap();
    let stale = rdy_mcp::tools::rdy_query(
        &mut st,
        serde_json::from_value(
            json!({"filter": "type = claim", "scope": "canon", "max_tokens": 60, "page": qn}),
        )
        .unwrap(),
    )
    .unwrap_err();
    assert_eq!(stale.code, "E_PAGE_STALE", "{stale:?}");
    let stale = canon_diff(&mut st, Some(&dn), "canon").unwrap_err();
    assert_eq!(stale.code, "E_PAGE_STALE", "{stale:?}");
    let mut seen = ids_of(&f1, "entries");
    let mut next = Some(fnext);
    while let Some(n) = next {
        let v = canon_diff(&mut st, Some(&n), &fixed).unwrap();
        assert_eq!(v["counts"], f1["counts"]);
        seen.extend(ids_of(&v, "entries"));
        next = v["next"].as_str().map(String::from);
    }
    let mut want = vec![order[0].clone()];
    want.extend(order[2..].iter().cloned());
    assert_eq!(
        seen, want,
        "the fixed diff, whole and in order, across the landing"
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
