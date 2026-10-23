//! rf12 (D-1876T, D-KQ4CA; TASK-507 (1), (2a)-(2c), (4)): `rdy trace` and `rdy_trace` give one
//! answer, and the command prints the decided lines. The fixture is imported, tagged, accepted
//! by a relayed landing, sourced and sealed, so every part of a trace has something to show.
use rdy_engine::{LandOptions, Rdy, Relay, Scope};
use rdy_model::{ActorRun, ApprovalEvidence, EvidenceKind, Hash, RelayEvidence};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};

/// The ids of the fixture's blocks and the two landings that matter.
pub struct Seed {
    pub k: String,
    pub d1: String,
    pub d2: String,
    pub src: String,
    pub finding: String,
    pub premise: String,
    pub question: String,
    /// The import's landing (D1 and K were introduced and accepted there).
    pub imported: String,
    /// The relayed landing that accepted D2 and wrote K's source link.
    pub edited: String,
}

/// A project in the served directory: K (a claim) is decided by D1, which an import carried in;
/// a tag names that landing; a second, relayed landing adds the source K reflects, a finding, a
/// claim and a question, and D2, which refines D1 and depends on them. That landing seals a
/// plan for D1: D2's refinement reaches what D1 governs.
pub fn seed(s: &Served) -> Seed {
    let (_d0, mut a) = project();
    let w = write(&mut a, "w", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "refunds", "title": "Refunds", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
        {"op": "create", "type": "decision", "id": "$d1", "parent": "$s",
            "fields": {"statement": "Refunds need an approver", "status": "proposed", "authority_basis": "direct_ruling"}},
        {"op": "create", "type": "claim", "id": "$k", "parent": "$s", "body": "A refund of 200 needs an approver.",
            "links": {"decided_by": [{"target": "$d1"}]}}
    ]))
    .unwrap();
    land(&mut a, "w").unwrap();
    let out = tempfile::tempdir().unwrap();
    a.read().export(&Scope::Canon, out.path()).unwrap();
    a.close().unwrap();

    let mut r = Rdy::open(s.dir.path()).unwrap();
    r.import(&approver(), "imp", out.path()).unwrap();
    let imported = land(&mut r, "imp").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    let (sec, k, d1) = (
        w.created["$s"].clone(),
        w.created["$k"].clone(),
        w.created["$d1"].clone(),
    );
    let e = write(&mut r, "edit", json!([
        {"op": "create", "type": "source", "id": "$src", "parent": sec,
            "fields": {"title": "Release 2", "kind": "release", "locator": "v2"}},
        {"op": "create", "type": "finding", "id": "$f", "parent": sec,
            "fields": {"statement": "Refunds over 100 were disputed.", "evidence": [{"quote": "Two disputes last month."}]}},
        {"op": "create", "type": "claim", "id": "$z", "parent": sec, "body": "Ledgers close nightly."},
        {"op": "create", "type": "question", "id": "$q", "parent": sec, "fields": {"prompt": "Who signs large refunds?"}},
        {"op": "create", "type": "decision", "id": "$d2", "parent": sec,
            "fields": {"statement": "Approvers sign refunds over 100", "status": "proposed", "authority_basis": "direct_ruling"},
            "links": {"refines": [{"target": d1}], "depends_on": [{"target": "$f"}, {"target": "$z"}],
                      "answers": [{"target": "$q"}]}},
        {"op": "update", "id": k, "links": {"reflects": ["$src"]}}
    ]))
    .unwrap();
    let summary = rdy_qa::intake::summary_for(&r, "edit");
    let p = r.propose(&writer(), "edit", "Edit", summary).unwrap();
    rdy_qa::intake::confirm_before_landing(&mut r, &approver(), &p);
    let edited = r
        .land_with(
            &approver(),
            &p.r#ref,
            LandOptions {
                strict: false,
                relay: Some(Relay {
                    by: ActorRun {
                        actor: "agent:relay".into(),
                        run: Some("r-9".into()),
                    },
                    evidence: RelayEvidence {
                        quoted: "Yes, sign them.".into(),
                        session_ref: "chat-1".into(),
                        question: Some("Approvers sign refunds?".into()),
                    },
                }),
                evidence: Some(ApprovalEvidence {
                    kind: EvidenceKind::Card,
                    options: vec!["approve".into(), "reject".into()],
                    choice: Some("approve".into()),
                    card_hash: Some(Hash([5; 32])),
                }),
            },
        )
        .unwrap()
        .landing;

    r.close().unwrap();
    Seed {
        k,
        d1,
        d2: e.created["$d2"].clone(),
        src: e.created["$src"].clone(),
        finding: e.created["$f"].clone(),
        premise: e.created["$z"].clone(),
        question: e.created["$q"].clone(),
        imported,
        edited,
    }
}

fn session(c: &mut McpClient) {
    let r = c.call(
        "rdy_session",
        json!({"actor": "agent:qa", "run": "qa", "role": "reader"}),
    );
    assert!(r.code().is_none(), "{:?}", r.ok());
}

fn cli(s: &Served, args: &[&str]) -> Value {
    let mut full = vec!["--json", "trace"];
    full.extend_from_slice(args);
    let (ok, v) = s.json(&full);
    assert!(ok, "rdy {args:?}: {v}");
    v
}

fn cli_text(s: &Served, args: &[&str]) -> Vec<String> {
    let mut full = vec!["trace"];
    full.extend_from_slice(args);
    let o = s.rdy(&full);
    assert!(
        o.status.success(),
        "rdy {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout)
        .trim_end()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn rf12_mcp_and_cli_trace_json_match() {
    let s = Served::new();
    let f = seed(&s);
    let mut c = McpClient::start_proxy(&s);
    session(&mut c);
    // every parameter and every default, over the canon scope, a tag pin and a ref line
    let ref_line = format!("[{}@v1.0.0] A refund of 200 needs an approver.", f.k);
    let cases: Vec<(Value, Vec<String>)> = vec![
        (
            json!({"id": f.k, "scope": "canon"}),
            vec![f.k.clone(), "--scope".into(), "canon".into()],
        ),
        (
            json!({"id": f.d2, "scope": "canon", "depth": 1}),
            vec![
                f.d2.clone(),
                "--scope".into(),
                "canon".into(),
                "--depth".into(),
                "1".into(),
            ],
        ),
        (
            json!({"id": f.d1, "scope": "canon", "depth": 8, "detail": true}),
            vec![
                f.d1.clone(),
                "--scope".into(),
                "canon".into(),
                "--depth".into(),
                "8".into(),
                "--detail".into(),
            ],
        ),
        (
            json!({"id": f.d2, "scope": "canon", "detail": true, "max_tokens": 100_000}),
            vec![
                f.d2.clone(),
                "--scope".into(),
                "canon".into(),
                "--detail".into(),
            ],
        ),
        (json!({"id": ref_line}), vec![ref_line.clone()]),
        (
            json!({"id": f.k, "scope": "tag:v1.0.0"}),
            vec![f.k.clone(), "--scope".into(), "tag:v1.0.0".into()],
        ),
        (
            json!({"id": f.d1, "scope": "canon", "max_tokens": 1}),
            vec![f.d1.clone(), "--scope".into(), "canon".into()],
        ),
    ];
    for (n, (args, cmd)) in cases.iter().enumerate() {
        let want = c.call("rdy_trace", args.clone()).ok();
        let cmd: Vec<&str> = cmd.iter().map(String::as_str).collect();
        // a small max_tokens has no CLI flag: the default budget is the MCP default too
        if args.get("max_tokens") == Some(&json!(1)) {
            let got = cli(&s, &cmd);
            assert_ne!(
                got, want,
                "case {n}: a one-token budget cuts, the default does not"
            );
            assert_eq!(want["truncated"], true, "{want}");
            continue;
        }
        let got = cli(&s, &cmd);
        assert_eq!(got, want, "case {n}: {args}");
    }

    // the facts that make the reply worth comparing are all in it
    let v = c
        .call(
            "rdy_trace",
            json!({"id": f.d1, "scope": "canon", "detail": true}),
        )
        .ok();
    assert_eq!(v["introduced"]["id"], f.imported, "{v}");
    assert_eq!(v["introduced"]["imported_by"]["actor"], "human:qa", "{v}");
    let d1 = v["decisions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == f.d1)
        .unwrap();
    let seals = d1["sealed_plans"].as_array().unwrap();
    assert_eq!(seals.len(), 1, "one seal, from the relayed landing: {d1}");
    assert_eq!(seals[0]["landing"], f.edited.as_str(), "{d1}");
    assert_eq!(d1["accepted_in"]["id"], f.imported, "{d1}");
    let d2 = v["decisions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == f.d2)
        .unwrap();
    assert_eq!(d2["accepted_in"]["id"], f.edited, "{d2}");
    assert_eq!(d2["accepted_in"]["relayed_by"]["actor"], "agent:relay");
    assert_eq!(d2["accepted_in"]["evidence"]["quoted"], "Yes, sign them.");
    c.alive();
}

#[test]
fn rf12_trace_text_has_the_decided_lines() {
    let s = Served::new();
    let f = seed(&s);
    let v = cli(&s, &[&f.k, "--scope", "canon"]);
    let short = |h: &Value| h.as_str().unwrap()[..12].to_string();
    let dec = |id: &str| {
        v["decisions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["id"] == id)
            .unwrap()
            .clone()
    };
    let (d1, d2) = (dec(&f.d1), dec(&f.d2));
    let who = |a: &Value| match a["run"].as_str() {
        Some(r) if r != "cli" => format!("{} (run {r})", a["actor"].as_str().unwrap()),
        _ => a["actor"].as_str().unwrap().to_string(),
    };
    let introduced = &v["introduced"];
    let premise = |id: &str| {
        v["premises"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == id)
            .unwrap()
            .clone()
    };
    let (fin, z) = (premise(&f.finding), premise(&f.premise));
    let want = vec![
        format!("{} claim: {}", f.k, v["summary"].as_str().unwrap()),
        "governed by:".to_string(),
        format!(
            "  {} {} via decided_by: Refunds need an approver",
            f.d1,
            d1["status"].as_str().unwrap()
        ),
        "reflects:".to_string(),
        format!("  {} release: Release 2", f.src),
        format!(
            "0 {} {}, direct_ruling, decided {}: Refunds need an approver",
            f.d1,
            d1["status"].as_str().unwrap(),
            d1["decided_at"].as_str().unwrap()
        ),
        format!("  refines {} -> {}", f.d2, f.d1),
        format!(
            "  accepted in {} {} by human:qa",
            short(&d1["accepted_in"]["id"]),
            d1["accepted_in"]["proposal"].as_str().unwrap()
        ),
        "  imported by human:qa".to_string(),
        format!(
            "1 {} {}, direct_ruling, decided {}: Approvers sign refunds over 100",
            f.d2,
            d2["status"].as_str().unwrap(),
            d2["decided_at"].as_str().unwrap()
        ),
        format!("  depends_on {} -> {}", f.d2, f.finding),
        format!("  depends_on {} -> {}", f.d2, f.premise),
        format!("  answers {} -> {}", f.d2, f.question),
        format!(
            "  accepted in {} {} by human:qa",
            short(&d2["accepted_in"]["id"]),
            d2["accepted_in"]["proposal"].as_str().unwrap()
        ),
        "  relayed by agent:relay (run r-9)".to_string(),
        format!(
            "premise {} finding: {}",
            f.finding,
            fin["summary"].as_str().unwrap()
        ),
        format!(
            "premise {} claim: {}",
            f.premise,
            z["summary"].as_str().unwrap()
        ),
        format!("answers {}: Who signs large refunds?", f.question),
        format!(
            "introduced: {} {} \"{}\" by {}",
            short(&introduced["id"]),
            introduced["proposal"].as_str().unwrap(),
            introduced["title"].as_str().unwrap(),
            who(&introduced["author"])
        ),
    ];
    // the edges sit under their decisions in the reply's order; the premise lines follow
    let text = cli_text(&s, &[&f.k, "--scope", "canon"]);
    let mut sorted_want = want.clone();
    let mut sorted_text = text.clone();
    sorted_want.sort();
    sorted_text.sort();
    assert_eq!(sorted_text, sorted_want, "same lines:\n{}", text.join("\n"));
    // and in the decided order of sections
    let at = |prefix: &str| {
        text.iter()
            .position(|l| l.starts_with(prefix))
            .unwrap_or_else(|| panic!("no line {prefix:?} in\n{}", text.join("\n")))
    };
    let order = [
        at(&format!("{} claim: ", f.k)),
        at("governed by:"),
        at("reflects:"),
        at(&format!("0 {} ", f.d1)),
        at(&format!("1 {} ", f.d2)),
        at("premise "),
        at(&format!("answers {}: ", f.question)),
        at("introduced: "),
    ];
    assert!(
        order.windows(2).all(|w| w[0] < w[1]),
        "{order:?}\n{}",
        text.join("\n")
    );

    // detail adds each decision's other JSON fields and the landing evidence
    let detail = cli_text(&s, &[&f.d2, "--scope", "canon", "--detail"]);
    let at = |prefix: &str| {
        detail
            .iter()
            .position(|l| l == prefix)
            .unwrap_or_else(|| panic!("no line {prefix:?} in\n{}", detail.join("\n")))
    };
    let (details, evidences): (Vec<usize>, Vec<usize>) = (
        detail
            .iter()
            .enumerate()
            .filter(|(_, l)| *l == "  detail:")
            .map(|(i, _)| i)
            .collect(),
        detail
            .iter()
            .enumerate()
            .filter(|(_, l)| *l == "  evidence:")
            .map(|(i, _)| i)
            .collect(),
    );
    assert!(
        !details.is_empty() && !evidences.is_empty(),
        "{}",
        detail.join("\n")
    );
    let v = cli(&s, &[&f.d2, "--scope", "canon", "--detail"]);
    let row = v["decisions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == f.d2)
        .unwrap();
    let mut extra = row.as_object().unwrap().clone();
    for k in [
        "id",
        "depth",
        "status",
        "statement",
        "authority_basis",
        "decided_at",
        "accepted_in",
    ] {
        extra.remove(k);
    }
    let pretty = serde_json::to_string_pretty(&Value::Object(extra)).unwrap();
    let from = at("  detail:");
    for (n, line) in pretty.lines().enumerate() {
        assert_eq!(
            detail[from + 1 + n],
            format!("    {line}"),
            "detail line {n}"
        );
    }
    let acc = &row["accepted_in"];
    let ev = json!({"evidence": acc["evidence"], "card_evidence": acc["card_evidence"]});
    let pretty = serde_json::to_string_pretty(&ev).unwrap();
    let from = at("  evidence:");
    for (n, line) in pretty.lines().enumerate() {
        assert_eq!(
            detail[from + 1 + n],
            format!("    {line}"),
            "evidence line {n}"
        );
    }
    assert_eq!(acc["evidence"]["quoted"], "Yes, sign them.");
    // a normal read prints neither
    let plain = cli_text(&s, &[&f.d2, "--scope", "canon"]);
    assert!(!plain.iter().any(|l| l == "  detail:" || l == "  evidence:"));
}
