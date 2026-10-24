//! rf12 (D-1876T): the text of `rdy trace`, from the reply `rdy_trace` returns. It prints, in
//! this order and leaving out what is empty: the block, whether it is ungrounded, the decisions
//! that govern it, the sources it reflects, each decision with its edges and acceptance, the
//! premises and questions, where the block was introduced, and the frontier. With `detail`, each
//! decision's other fields and each landing's evidence follow as indented JSON.
use crate::history_text::who;
use crate::output::{hash_short, num, text};
use serde_json::{Map, Value};
use std::collections::BTreeSet;

/// The array at `v`, or none.
fn rows(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or_default()
}

/// An edge kind as its name: the `rdy_context` edge shape gives `"Contains"`, `"References"` or
/// `{"Link": "<name>"}`.
fn kind_name(kind: &Value) -> String {
    match kind {
        Value::String(k) => k.to_lowercase(),
        other => text(&other["Link"]).to_string(),
    }
}

/// `value` as pretty JSON, indented four spaces under a two-space heading. A map's keys are in
/// serde_json's order, so the same reply always prints the same lines.
fn json_lines(value: &Map<String, Value>, out: &mut Vec<String>) {
    let pretty = serde_json::to_string_pretty(value).unwrap_or_default();
    out.extend(pretty.lines().map(|l| format!("    {l}")));
}

/// A landing's relay and card evidence (detail only): the `  evidence:` heading and its JSON.
fn evidence_lines(landing: &Value, out: &mut Vec<String>) {
    let mut m = Map::new();
    for key in ["evidence", "card_evidence"] {
        if landing[key].is_object() {
            m.insert(key.to_string(), landing[key].clone());
        }
    }
    if !m.is_empty() {
        out.push("  evidence:".to_string());
        json_lines(&m, out);
    }
}

/// The lines of one decision: its head, its other fields (detail), its edges and its acceptance.
fn decision_lines(d: &Value, edges: &[&Value], detail: bool, out: &mut Vec<String>) {
    let mut head = format!(
        "{} {} {}",
        num(&d["depth"]),
        text(&d["id"]),
        text(&d["status"])
    );
    if let Some(basis) = d["authority_basis"].as_str() {
        head.push_str(&format!(", {basis}"));
    }
    if let Some(at) = d["decided_at"].as_str() {
        head.push_str(&format!(", decided {at}"));
    }
    head.push_str(&format!(": {}", text(&d["statement"])));
    out.push(head);
    if detail && let Some(row) = d.as_object() {
        let mut extra = row.clone();
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
        if !extra.is_empty() {
            out.push("  detail:".to_string());
            json_lines(&extra, out);
        }
    }
    for e in edges {
        let part = e["part"]
            .as_str()
            .map(|p| format!(" ({p})"))
            .unwrap_or_default();
        out.push(format!(
            "  {} {} -> {}{part}",
            kind_name(&e["kind"]),
            text(&e["from"]),
            text(&e["to"])
        ));
    }
    let acc = &d["accepted_in"];
    if acc.is_object() {
        out.push(format!(
            "  accepted in {} {} by {}",
            hash_short(text(&acc["id"])),
            text(&acc["proposal"]),
            text(&acc["approved_by"])
        ));
        if acc["relayed_by"].is_object() {
            out.push(format!("  relayed by {}", who(&acc["relayed_by"])));
        }
        if acc["imported_by"].is_object() {
            out.push(format!("  imported by {}", who(&acc["imported_by"])));
        }
        if detail {
            evidence_lines(acc, out);
        }
    }
}

/// The text of a trace reply. `detail` says the reply was asked for with detail.
pub(crate) fn trace_text(v: &Value, detail: bool) -> String {
    let mut out = vec![format!(
        "{} {}: {}",
        text(&v["id"]),
        text(&v["block_type"]),
        text(&v["summary"])
    )];
    if v["ungrounded"].as_bool() == Some(true) {
        out.push("ungrounded: no decision, source or reflects globs".to_string());
    }
    let decisions = rows(&v["decisions"]);
    if !rows(&v["governing"]).is_empty() {
        out.push("governed by:".to_string());
        for g in rows(&v["governing"]) {
            // a depth-zero decision row survives the budget cut, so its statement is here
            let statement = decisions
                .iter()
                .find(|d| d["id"] == g["decision"])
                .map_or("", |d| text(&d["statement"]));
            let entity = g["entity"]
                .as_str()
                .map(|e| format!(" {e}"))
                .unwrap_or_default();
            out.push(format!(
                "  {} {} via {}{entity}: {statement}",
                text(&g["decision"]),
                text(&g["status"]),
                text(&g["via"])
            ));
        }
    }
    if !rows(&v["sources"]).is_empty() {
        out.push("reflects:".to_string());
        for s in rows(&v["sources"]) {
            out.push(format!(
                "  {} {}: {}",
                text(&s["source"]),
                text(&s["kind"]),
                text(&s["title"])
            ));
        }
    }
    // an edge sits under the decision it leaves, else the one it enters
    let shown: BTreeSet<&str> = decisions.iter().map(|d| text(&d["id"])).collect();
    for d in decisions {
        let id = text(&d["id"]);
        let edges: Vec<&Value> = rows(&v["edges"])
            .iter()
            .filter(|e| {
                let (from, to) = (text(&e["from"]), text(&e["to"]));
                from == id || (to == id && !shown.contains(from))
            })
            .collect();
        decision_lines(d, &edges, detail, &mut out);
    }
    for p in rows(&v["premises"]) {
        out.push(format!(
            "premise {} {}: {}",
            text(&p["id"]),
            text(&p["block_type"]),
            text(&p["summary"])
        ));
    }
    for q in rows(&v["questions"]) {
        out.push(format!(
            "answers {}: {}",
            text(&q["id"]),
            text(&q["prompt"])
        ));
    }
    let intro = &v["introduced"];
    if intro.is_object() {
        out.push(format!(
            "introduced: {} {} \"{}\" by {}",
            hash_short(text(&intro["id"])),
            text(&intro["proposal"]),
            text(&intro["title"]),
            who(&intro["author"])
        ));
        if detail {
            evidence_lines(intro, &mut out);
        }
    }
    let frontier: Vec<&str> = rows(&v["frontier"])
        .iter()
        .filter_map(Value::as_str)
        .collect();
    if !frontier.is_empty() {
        out.push(format!("more: {}", frontier.join(", ")));
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::trace_text;
    use serde_json::{Value, json};

    fn base() -> Value {
        json!({
            "id": "c-AAAAA", "block_type": "claim", "summary": "A refund needs an approver.",
            "governing": [], "sources": [], "ungrounded": false, "introduced": null,
            "decisions": [], "premises": [], "edges": [], "questions": [], "frontier": []
        })
    }

    fn lines(v: &Value, detail: bool) -> Vec<String> {
        trace_text(v, detail).lines().map(str::to_string).collect()
    }

    #[test]
    fn rf12_trace_text_omits_every_empty_section() {
        assert_eq!(
            lines(&base(), false),
            vec!["c-AAAAA claim: A refund needs an approver."]
        );
    }

    #[test]
    fn rf12_trace_text_prints_ungrounded_only_when_true() {
        let mut v = base();
        v["ungrounded"] = json!(true);
        assert_eq!(
            lines(&v, false),
            vec![
                "c-AAAAA claim: A refund needs an approver.",
                "ungrounded: no decision, source or reflects globs"
            ]
        );
        // a block that follows a source and has no deciding decision is not ungrounded: the
        // reflects section stands alone, with no governed-by section
        let mut v = base();
        v["sources"] = json!([{"source": "S-BBBBB", "kind": "release", "title": "Release 2"}]);
        assert_eq!(
            lines(&v, false),
            vec![
                "c-AAAAA claim: A refund needs an approver.",
                "reflects:",
                "  S-BBBBB release: Release 2"
            ]
        );
    }

    #[test]
    fn rf12_trace_text_governed_by_reads_statements_from_the_decisions() {
        let mut v = base();
        v["governing"] = json!([
            {"decision": "D-CCCCC", "status": "live", "via": "decided_by"},
            {"decision": "D-DDDDD", "status": "proposed", "via": "describes/affects", "entity": "t-EEEEE"}
        ]);
        v["decisions"] = json!([
            {"id": "D-CCCCC", "depth": 0, "status": "live", "statement": "Approvers sign.", "plans": {}},
            {"id": "D-DDDDD", "depth": 0, "status": "proposed", "statement": "Ledgers close.", "plans": {}}
        ]);
        assert_eq!(
            lines(&v, false),
            vec![
                "c-AAAAA claim: A refund needs an approver.",
                "governed by:",
                "  D-CCCCC live via decided_by: Approvers sign.",
                "  D-DDDDD proposed via describes/affects t-EEEEE: Ledgers close.",
                "0 D-CCCCC live: Approvers sign.",
                "0 D-DDDDD proposed: Ledgers close.",
            ]
        );
    }

    #[test]
    fn rf12_trace_text_decision_lines_drop_absent_clauses() {
        let mut v = base();
        v["decisions"] = json!([
            {"id": "D-1", "depth": 0, "status": "live", "statement": "Both.",
                "authority_basis": "direct_ruling", "decided_at": "2026-10-20", "plans": {}},
            {"id": "D-2", "depth": 1, "status": "live", "statement": "Basis only.",
                "authority_basis": "delegated", "plans": {}},
            {"id": "D-3", "depth": 1, "status": "superseded", "statement": "Date only.",
                "decided_at": "2026-10-01T10:00:00Z", "plans": {}},
            {"id": "D-4", "depth": 2, "status": "revoked", "statement": "Neither.", "plans": {}}
        ]);
        assert_eq!(
            lines(&v, false)[1..].to_vec(),
            vec![
                "0 D-1 live, direct_ruling, decided 2026-10-20: Both.",
                "1 D-2 live, delegated: Basis only.",
                "1 D-3 superseded, decided 2026-10-01T10:00:00Z: Date only.",
                "2 D-4 revoked: Neither.",
            ]
        );
    }

    #[test]
    fn rf12_trace_text_edges_and_acceptance_sit_under_their_decision() {
        let mut v = base();
        v["decisions"] = json!([
            {"id": "D-1", "depth": 0, "status": "live", "statement": "One.", "plans": {},
                "accepted_in": {"id": "0123456789abcdef0123", "proposal": "P-abc@2", "title": "T",
                    "approved_by": "human:nick", "assurance": "local",
                    "author": {"actor": "agent:a", "run": "r1", "role": "writer", "at_ms": 1},
                    "relayed_by": {"actor": "agent:relay", "run": "r-9"},
                    "imported_by": {"actor": "human:nick", "run": "cli"}}},
            {"id": "D-2", "depth": 1, "status": "live", "statement": "Two.", "plans": {}}
        ]);
        v["edges"] = json!([
            {"from": "D-2", "to": "D-1", "kind": {"Link": "refines"}, "part": "the words"},
            {"from": "D-1", "to": "c-Z", "kind": {"Link": "depends_on"}, "part": null},
            {"from": "D-1", "to": "c-Y", "kind": "References", "part": null}
        ]);
        assert_eq!(
            lines(&v, false)[1..].to_vec(),
            vec![
                "0 D-1 live: One.",
                "  depends_on D-1 -> c-Z",
                "  references D-1 -> c-Y",
                "  accepted in 0123456789ab P-abc@2 by human:nick",
                "  relayed by agent:relay (run r-9)",
                "  imported by human:nick",
                "1 D-2 live: Two.",
                "  refines D-2 -> D-1 (the words)",
            ]
        );
    }

    #[test]
    fn rf12_trace_text_ends_with_premises_questions_introduction_and_frontier() {
        let mut v = base();
        v["premises"] = json!([
            {"id": "f-1", "block_type": "finding", "summary": "Disputes rose.", "depth": 1}
        ]);
        v["questions"] = json!([{"id": "q-1", "prompt": "Who signs?", "depth": 1}]);
        v["introduced"] = json!({"id": "fedcba9876543210ffff", "proposal": "P-def@1", "title": "First",
            "approved_by": "human:nick", "assurance": "local",
            "author": {"actor": "agent:a", "run": "r1", "role": "writer", "at_ms": 1}});
        v["frontier"] = json!(["D-8", "D-9"]);
        v["truncated"] = json!(true);
        v["omitted"] = json!(2);
        assert_eq!(
            lines(&v, false)[1..].to_vec(),
            vec![
                "premise f-1 finding: Disputes rose.",
                "answers q-1: Who signs?",
                "introduced: fedcba987654 P-def@1 \"First\" by agent:a (run r1)",
                "more: D-8, D-9",
            ]
        );
        // a human run is not shown
        v["introduced"]["author"]["run"] = json!("cli");
        assert!(
            lines(&v, false)
                .contains(&"introduced: fedcba987654 P-def@1 \"First\" by agent:a".to_string())
        );
    }

    #[test]
    fn rf12_trace_text_detail_prints_the_other_fields_and_the_evidence() {
        let mut v = base();
        v["decisions"] = json!([
            {"id": "D-1", "depth": 0, "status": "live", "statement": "One.",
                "authority_basis": "direct_ruling", "reversibility": "two_way",
                "plans": {"updated": 1}, "options": ["Keep", "Drop"],
                "accepted_in": {"id": "0123456789abcdef0123", "proposal": "P-abc@2", "title": "T",
                    "approved_by": "human:nick", "assurance": "local",
                    "author": {"actor": "agent:a", "run": null, "role": "writer", "at_ms": 1},
                    "evidence": {"quoted": "Yes.", "session_ref": "chat-1", "question": null},
                    "card_evidence": {"kind": "card", "options": null, "choice": "approve", "card_hash": null}}}
        ]);
        let got = lines(&v, true);
        assert_eq!(
            got[1..].to_vec(),
            vec![
                "0 D-1 live, direct_ruling: One.",
                "  detail:",
                "    {",
                "      \"options\": [",
                "        \"Keep\",",
                "        \"Drop\"",
                "      ],",
                "      \"plans\": {",
                "        \"updated\": 1",
                "      },",
                "      \"reversibility\": \"two_way\"",
                "    }",
                "  accepted in 0123456789ab P-abc@2 by human:nick",
                "  evidence:",
                "    {",
                "      \"card_evidence\": {",
                "        \"card_hash\": null,",
                "        \"choice\": \"approve\",",
                "        \"kind\": \"card\",",
                "        \"options\": null",
                "      },",
                "      \"evidence\": {",
                "        \"question\": null,",
                "        \"quoted\": \"Yes.\",",
                "        \"session_ref\": \"chat-1\"",
                "      }",
                "    }",
            ]
        );
        // without detail the same reply prints neither block
        assert!(
            !lines(&v, false)
                .iter()
                .any(|l| l.contains("detail:") || l.contains("evidence:"))
        );
    }
}
