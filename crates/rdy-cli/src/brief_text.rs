//! cbrf1: the text `rdy edit` shows. The briefing becomes `#` comment lines above the YAML
//! (the YAML parser drops them on save), and a write's consequences become a short list.
use rdy_schema::CoverageDimension;
use serde_json::Value;

/// A string field of `v`, empty when absent.
fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or_default()
}

/// `text` on one line: every control character, and the Unicode line and paragraph separators,
/// becomes a space. libyaml breaks lines at `\r`, `\n`, NEL, U+2028 and U+2029, and a break would
/// end a `#` comment, so free text (titles, view names, statements) cannot leak into the YAML.
fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// One section: its title line, then its entry lines (or `(none)`), each on one line.
fn section(out: &mut Vec<String>, title: &str, entries: impl Iterator<Item = String>) {
    out.push(format!("# {title}:"));
    let before = out.len();
    out.extend(entries.map(|e| format!("#   {}", one_line(&e))));
    if out.len() == before {
        out.push("#   (none)".into());
    }
}

/// One related decision: `<decision> [<status>] <relation> (<entities>): <statement>`, without
/// the parentheses when it names no entity.
fn related_line(r: &Value) -> String {
    let entities: Vec<&str> = r["entities"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let on = if entities.is_empty() {
        String::new()
    } else {
        format!(" ({})", entities.join(", "))
    };
    format!(
        "{} [{}] {}{on}: {}",
        s(r, "decision"),
        s(r, "status"),
        s(r, "relation"),
        s(r, "statement")
    )
}

/// The coverage lines of a briefing (FW-CV1, FW-CV2): the score with its dimensions and the
/// confidence level, one line per reason, and the count of reasons left out. Nothing when the
/// briefing covers no scored block.
fn coverage_lines(out: &mut Vec<String>, c: &Value) {
    let Some(score) = c["score"].as_u64() else {
        return;
    };
    let dims: Vec<String> = CoverageDimension::ALL
        .iter()
        .filter_map(|d| {
            c["dimensions"][d.name()]
                .as_u64()
                .map(|n| format!("{} {n}", d.name()))
        })
        .collect();
    if dims.is_empty() {
        return;
    }
    out.push(format!(
        "# coverage: {score} ({}); confidence {}",
        dims.join(", "),
        one_line(s(c, "confidence"))
    ));
    for r in c["reasons"].as_array().into_iter().flatten() {
        out.push(format!(
            "#   {}",
            one_line(&format!(
                "{} {}: {}",
                s(r, "code"),
                s(r, "block"),
                s(r, "fix")
            ))
        ));
    }
    if let Some(n) = c["reasons_omitted"].as_u64().filter(|n| *n > 0) {
        out.push(format!("#   … {n} more"));
    }
}

/// The briefing as `#` comment lines, one section per list, ending in a bare `#` line.
pub fn header(briefing: &Value) -> String {
    let list = |k: &str| briefing[k].as_array().into_iter().flatten();
    let base = &briefing["base"];
    let covered: Vec<&str> = base["blocks"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|b| s(b, "id"))
        .collect();
    let token: String = s(base, "token").chars().take(12).collect();
    // A code briefing for a file nothing cites covers no blocks.
    let covered = if covered.is_empty() {
        "no blocks".to_string()
    } else {
        covered.join(", ")
    };
    let mut out = vec![
        format!("# rdy briefing for {} (token {token})", one_line(&covered)),
        "# Lines starting with # are dropped when you save.".to_string(),
    ];
    section(
        &mut out,
        "governs",
        list("governs").map(|g| {
            format!(
                "{} [{}] {}",
                s(g, "decision"),
                s(g, "status"),
                s(g, "statement")
            )
        }),
    );
    section(
        &mut out,
        "depends on this",
        list("dependents").map(|d| {
            format!(
                "{} ({} {}) {}",
                s(d, "block"),
                s(d, "kind"),
                s(d, "on"),
                s(d, "summary")
            )
        }),
    );
    section(
        &mut out,
        "Lineage",
        list("lineage").map(|l| {
            if s(l, "direction") == "out" {
                format!(
                    "{} (out: {} {} {}) {}",
                    s(l, "block"),
                    s(l, "on"),
                    s(l, "kind"),
                    s(l, "block"),
                    s(l, "summary")
                )
            } else {
                format!(
                    "{} ({} {} {}) {}",
                    s(l, "block"),
                    s(l, "direction"),
                    s(l, "kind"),
                    s(l, "on"),
                    s(l, "summary")
                )
            }
        }),
    );
    // FW-LP6: printed only when there are reaches, so a header without them is unchanged
    let mut reaches: Vec<String> = list("reaches")
        .map(|x| {
            let blocks: Vec<&str> = x["blocks"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            format!(
                "{} {} {}: {}",
                s(x, "decision"),
                s(x, "via"),
                s(x, "target"),
                blocks.join(", ")
            )
        })
        .collect();
    if let Some(n) = briefing["reaches_omitted"].as_u64().filter(|n| *n > 0) {
        reaches.push(format!("and {n} more"));
    }
    if !reaches.is_empty() {
        section(&mut out, "reaches", reaches.into_iter());
    }
    section(
        &mut out,
        "will go stale if this changes",
        list("will_go_stale").map(|w| {
            format!(
                "{} via {} on {}: {}",
                s(w, "block"),
                s(w, "via"),
                s(w, "on"),
                s(w, "follow_up")
            )
        }),
    );
    section(
        &mut out,
        "open suspects",
        list("suspects").map(|x| {
            format!(
                "{} -> {} ({})",
                s(x, "block"),
                s(x, "target"),
                s(x, "reason")
            )
        }),
    );
    let inflight = &briefing["inflight"];
    if inflight.is_object() {
        let at = |k: &str| inflight[k].as_array().into_iter().flatten();
        let blocks = |x: &Value| {
            x["blocks"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        };
        let views = at("views").map(|v| {
            let o = &v["owner"];
            let by = if o.is_object() {
                match o["run"].as_str() {
                    Some(run) => format!(" by {}/{run}", s(o, "actor")),
                    None => format!(" by {}", s(o, "actor")),
                }
            } else {
                ", unowned".to_string()
            };
            format!(
                "view {}{by} (last write {}): {}",
                s(v, "view"),
                s(v, "last_write"),
                blocks(v)
            )
        });
        let proposals = at("proposals").map(|p| {
            format!(
                "proposal {} from {} \"{}\": {}",
                s(p, "ref"),
                s(p, "view"),
                s(p, "title"),
                blocks(p)
            )
        });
        let intents = at("intents").map(|i| {
            let by = match i["run"].as_str() {
                Some(run) => format!("{}/{run}", s(i, "actor")),
                None => s(i, "actor").to_string(),
            };
            let n = i["blocks"].as_array().map_or(0, Vec::len);
            format!(
                "intent {} by {by} in {}: {} (until {}) on {n} block(s)",
                s(i, "id"),
                s(i, "view"),
                s(i, "angle"),
                s(i, "expires_at")
            )
        });
        let tensions = at("tensions").map(|t| {
            let dismissed = if t["status"].as_str() == Some("dismissed") {
                " (dismissed)"
            } else {
                ""
            };
            format!(
                "tension {} {} {}: {}{dismissed}",
                s(t, "id"),
                s(t, "severity"),
                s(t, "kind"),
                s(t, "summary")
            )
        });
        let collapsed = inflight["tensions_collapsed"]
            .as_object()
            .filter(|m| !m.is_empty())
            .map(|m| {
                let kinds: Vec<String> = m
                    .iter()
                    .map(|(k, n)| format!("{} {k}", n.as_u64().unwrap_or(0)))
                    .collect();
                format!("tensions not listed: {}", kinds.join(", "))
            });
        let unread = inflight["tensions_error"]
            .as_str()
            .map(|e| format!("tensions not read: {e}"));
        section(
            &mut out,
            "in flight",
            views
                .chain(proposals)
                .chain(intents)
                .chain(tensions)
                .chain(collapsed)
                .chain(unread),
        );
    }
    let related = list("related_decisions")
        .map(related_line)
        .collect::<Vec<_>>();
    if !related.is_empty() {
        section(&mut out, "related decisions", related.into_iter());
    }
    let mentions = list("mentioned_by")
        .map(|m| {
            format!(
                "{} names {} (\"{}\" in {})",
                s(m, "block"),
                s(m, "doc"),
                s(m, "mention"),
                s(m, "field")
            )
        })
        .collect::<Vec<_>>();
    if !mentions.is_empty() {
        section(&mut out, "mentioned by", mentions.into_iter());
    }
    coverage_lines(&mut out, &briefing["coverage"]);
    if let Some(e) = briefing["coverage_error"].as_str() {
        out.push(format!("# coverage not read: {}", one_line(e)));
    }
    if let Some(m) = briefing["omitted"].as_object().filter(|m| !m.is_empty()) {
        let mut names: Vec<_> = m.iter().collect();
        names.sort_by_key(|(k, _)| k.as_str());
        let list: Vec<String> = names.iter().map(|(k, n)| format!("{k} {n}")).collect();
        out.push(format!("# omitted: {}", list.join(", ")));
    }
    out.push("#".into());
    let mut text = out.join("\n");
    text.push('\n');
    text
}

/// The code section of a code briefing as text: the path and repos, what its commits implement,
/// the blocks whose `reflects` globs match it, and trailer values that named nothing.
pub fn code_text(v: &Value) -> String {
    let c = &v["code"];
    let repos: Vec<&str> = c["repos"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut out = if repos.is_empty() {
        format!(
            "code {} (no code repo linked: set code_repos in .rdy.yaml)\n",
            s(c, "path")
        )
    } else {
        format!("code {} (repos: {})\n", s(c, "path"), repos.join(", "))
    };
    let list = |k: &str| c[k].as_array().into_iter().flatten();
    out.push_str("implements:\n");
    let before = out.len();
    for a in list("implements") {
        if a["status"].is_string() {
            out.push_str(&format!(
                "  {} [{}] {} commit(s), {} depend on it",
                s(a, "block"),
                s(a, "status"),
                a["commits"],
                a["dependent_commits"]
            ));
            if a["provisional"].as_bool() == Some(true) {
                out.push_str(", provisional");
            }
            if a["rework"].as_bool() == Some(true) {
                out.push_str(", rework");
            }
        } else {
            out.push_str(&format!("  {} {} commit(s)", s(a, "block"), a["commits"]));
        }
        out.push('\n');
    }
    if out.len() == before {
        out.push_str("  (none)\n");
    }
    out.push_str("reflects:\n");
    let before = out.len();
    for r in list("reflects") {
        out.push_str(&format!("  {} {}\n", s(r, "block"), s(r, "glob")));
    }
    if out.len() == before {
        out.push_str("  (none)\n");
    }
    let unresolved: Vec<&str> = list("unresolved").filter_map(Value::as_str).collect();
    if !unresolved.is_empty() {
        out.push_str(&format!("unresolved: {}\n", unresolved.join(", ")));
    }
    out
}

/// The briefing as plain lines: `header` without its save note and closing `#`, and without the
/// comment marks.
pub fn plain(v: &Value) -> String {
    let h = header(v);
    let mut lines: Vec<&str> = h.lines().collect();
    lines.retain(|l| *l != "# Lines starting with # are dropped when you save.");
    if lines.last() == Some(&"#") {
        lines.pop();
    }
    let mut out = String::new();
    for l in lines {
        out.push_str(
            l.strip_prefix("# ")
                .or_else(|| l.strip_prefix('#'))
                .unwrap_or(l),
        );
        out.push('\n');
    }
    out
}

/// A write reply's extras as text to append to its one-line summary: empty when there are none,
/// else `\nconsequences:` and one line per entry (with a line for what a cut left out), then the
/// intents on the written blocks, the tension changes, the coverage change, the suggestions and
/// the related decisions in the same way, and last a line for each part rdy could not work out.
pub fn consequences_text(reply: &Value) -> String {
    let mut t = consequences_part(reply);
    t.push_str(&intents_part(reply));
    t.push_str(&on_change_part(reply));
    t.push_str(&coverage_part(reply));
    t.push_str(&suggestions_part(reply));
    t.push_str(&related_part(reply));
    t.push_str(&errors_part(reply));
    t
}

/// The intents of other sessions on the blocks the write touched (M2d1).
fn intents_part(reply: &Value) -> String {
    let items = reply["intents"].as_array().map_or(&[][..], |a| a);
    if items.is_empty() {
        return String::new();
    }
    let mut t = String::from("\nintents on these blocks:");
    for i in items {
        let by = match i["run"].as_str() {
            Some(run) => format!("{}/{run}", s(i, "actor")),
            None => s(i, "actor").to_string(),
        };
        let n = i["blocks"].as_array().map_or(0, Vec::len);
        t.push_str(&format!(
            "\n  intent {} by {by} in {}: {} (until {}) on {n} block(s)",
            s(i, "id"),
            s(i, "view"),
            one_line(s(i, "angle")),
            s(i, "expires_at")
        ));
    }
    t
}

/// The coverage change a write made (FW-CH3): `coverage: <before> → <after>` and, per page that
/// moved, `  <path>: <before> → <after>`. Nothing when every number is unchanged, or when the
/// reply's `coverage` is not a change (an import reply's is the baseline report).
fn coverage_part(reply: &Value) -> String {
    let c = &reply["coverage"];
    let (Some(before), Some(after)) = (c["before"].as_u64(), c["after"].as_u64()) else {
        return String::new();
    };
    let moved: Vec<&Value> = c["pages"]
        .as_array()
        .map_or(&[][..], |a| a)
        .iter()
        .filter(|p| p["before"] != p["after"])
        .collect();
    if before == after && moved.is_empty() {
        return String::new();
    }
    let mut t = format!("\ncoverage: {before} → {after}");
    let all_moved = moved.len() == c["pages"].as_array().map_or(0, Vec::len);
    for p in moved {
        t.push_str(&format!(
            "\n  {}: {} → {}",
            s(p, "path"),
            p["before"],
            p["after"]
        ));
    }
    // pages sort moved first, then the cut: the omitted count can name a moved page only when
    // every page listed moved
    if let Some(n) = c["pages_omitted"].as_u64().filter(|n| *n > 0 && all_moved) {
        t.push_str(&format!("\n  and {n} more pages"));
    }
    t
}

/// The parts of the reply rdy could not work out, each with its error.
fn errors_part(reply: &Value) -> String {
    const PARTS: [(&str, &str); 7] = [
        ("consequences_error", "consequences"),
        ("consequences_tension_error", "the tension consequences"),
        ("on_change_error", "the tension changes"),
        // a propose reply's tensions (gate `ProposalInfo`)
        ("tensions_error", "the tensions"),
        ("coverage_error", "the coverage change"),
        ("suggestions_error", "suggestions"),
        ("related_decisions_error", "related decisions"),
    ];
    let mut t = String::new();
    for (key, what) in PARTS {
        if let Some(e) = reply[key].as_str() {
            t.push_str(&format!("\nnot worked out: {what} ({})", one_line(e)));
        }
    }
    t
}

fn consequences_part(reply: &Value) -> String {
    let items = reply["consequences"].as_array().map_or(&[][..], |a| a);
    if items.is_empty() {
        return String::new();
    }
    let mut t = String::from("\nconsequences:");
    for c in items {
        t.push_str(&format!(
            "\n  {} {}: {} ({} {})",
            s(c, "kind"),
            s(c, "block"),
            s(c, "follow_up"),
            s(&c["because"], "edge"),
            s(&c["because"], "changed")
        ));
        if let Some(tn) = c["tension"].as_str() {
            t.push_str(&format!(" {tn}"));
        }
        if let Some(fix) = c["fix"].as_str() {
            t.push_str(&format!("\n    fix: {fix}"));
        }
    }
    if let Some(m) = reply["consequences_omitted"]
        .as_object()
        .filter(|m| !m.is_empty())
    {
        let n: u64 = m.values().filter_map(Value::as_u64).sum();
        let kinds: Vec<String> = m.iter().map(|(k, v)| format!("{k} {v}")).collect();
        t.push_str(&format!("\n  and {n} more ({})", kinds.join(", ")));
    }
    t
}

/// The tensions the write opened and resolved (`on_change`), and the advisory ones a cut left
/// out; nothing when there are none. Printed even when there are no consequences.
fn on_change_part(reply: &Value) -> String {
    let change = &reply["on_change"];
    let opened = change["tensions"].as_array().map_or(&[][..], |a| a);
    let resolved = change["resolved"].as_array().map_or(&[][..], |a| a);
    let mut t = String::new();
    if !opened.is_empty() {
        t.push_str("\ntensions opened:");
        for x in opened {
            t.push_str(&format!(
                "\n  {} {} {}: {}",
                s(x, "id"),
                s(x, "severity"),
                s(x, "kind"),
                one_line(s(x, "summary"))
            ));
        }
    }
    if let Some(m) = change["collapsed"].as_object().filter(|m| !m.is_empty()) {
        let kinds: Vec<String> = m.iter().map(|(k, v)| format!("{v} {k}")).collect();
        t.push_str(&format!("\ntensions not listed: {}", kinds.join(", ")));
    }
    if !resolved.is_empty() {
        let list: Vec<String> = resolved
            .iter()
            .map(|x| format!("{} ({})", s(x, "id"), s(x, "kind")))
            .collect();
        t.push_str(&format!("\ntensions resolved: {}", list.join(", ")));
    }
    t
}

fn suggestions_part(reply: &Value) -> String {
    let items = reply["suggestions"].as_array().map_or(&[][..], |a| a);
    if items.is_empty() {
        return String::new();
    }
    let mut t = String::from("\nsuggestions:");
    for x in items {
        if s(x, "kind") == "entity" {
            t.push_str(&format!(
                "\n  entity `{}` ({} blocks)",
                one_line(s(x, "literal")),
                x["count"]
            ));
        } else {
            t.push_str(&format!(
                "\n  link {} {} {} (\"{}\" in {})",
                s(x, "block"),
                s(x, "link"),
                s(x, "target"),
                one_line(s(x, "mention")),
                s(x, "field")
            ));
        }
    }
    if let Some(n) = reply["suggestions_omitted"].as_u64().filter(|n| *n > 0) {
        t.push_str(&format!("\n  and {n} more"));
    }
    t
}

fn related_part(reply: &Value) -> String {
    let items = reply["related_decisions"].as_array().map_or(&[][..], |a| a);
    if items.is_empty() {
        return String::new();
    }
    let mut t = String::from("\nrelated decisions:");
    for r in items {
        t.push_str(&format!("\n  {}", one_line(&related_line(r))));
    }
    if let Some(n) = reply["related_decisions_omitted"]
        .as_u64()
        .filter(|n| *n > 0)
    {
        t.push_str(&format!("\n  and {n} more"));
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cbrf1_the_header_is_comment_lines_in_section_order() {
        let b = json!({
            "covers": 1,
            "governs": [{"decision": "D-1", "status": "live", "statement": "Keep A", "blocks": ["c-A"]}],
            "dependents": [{"block": "c-B", "kind": "references", "on": "c-A", "summary": "B builds on A."}],
            "lineage": [{"block": "D-2", "kind": "refines", "direction": "in", "on": "c-A", "summary": "D2 refines A."}],
            "will_go_stale": [{"block": "c-B", "via": "references", "on": "c-A", "follow_up": "confirm"}],
            "suspects": [],
            "base": {"token": "0123456789abcdef0123", "blocks": [{"id": "c-A", "version": "ff"}], "depends": 1}
        });
        let h = header(&b);
        assert!(h.lines().all(|l| l.starts_with('#')), "{h}");
        let want = [
            "# rdy briefing for c-A (token 0123456789ab)",
            "# Lines starting with # are dropped when you save.",
            "# governs:",
            "#   D-1 [live] Keep A",
            "# depends on this:",
            "#   c-B (references c-A) B builds on A.",
            "# Lineage:",
            "#   D-2 (in refines c-A) D2 refines A.",
            "# will go stale if this changes:",
            "#   c-B via references on c-A: confirm",
            "# open suspects:",
            "#   (none)",
            "#",
        ];
        assert_eq!(h.lines().collect::<Vec<_>>(), want);
    }

    #[test]
    fn cbrf1_consequences_print_one_line_each_and_nothing_when_empty() {
        assert_eq!(
            consequences_text(&json!({"change": "x", "consequences": []})),
            ""
        );
        let v = json!({"consequences": [
            {"id": "cq-1", "block": "c-B", "kind": "suspect", "because": {"edge": "references", "changed": "c-A"}, "follow_up": "confirm"},
            {"id": "cq-2", "block": "D-2", "kind": "suspect", "because": {"edge": "depends_on", "changed": "c-A"}, "follow_up": "reaffirm",
             "fix": "supersede it, or leave it suspect; re-affirmation arrives with FW-R6"}
        ], "consequences_omitted": {"dependent": 3}});
        assert_eq!(
            consequences_text(&v),
            "\nconsequences:\n  suspect c-B: confirm (references c-A)\n  suspect D-2: reaffirm (depends_on c-A)\n    fix: supersede it, or leave it suspect; re-affirmation arrives with FW-R6\n  and 3 more (dependent 3)"
        );
    }

    #[test]
    fn tonc1_consequences_text_names_the_tension_and_the_on_change_sections() {
        let reply = serde_json::json!({
            "consequences": [{"id": "cq-1", "block": "c-AAAAA", "kind": "tension",
                "because": {"edge": "premise_shift", "changed": "c-BBBBB"},
                "follow_up": "resolve", "tension": "tn-0123456789ab", "fix": "resolve it"}],
            "on_change": {
                "tensions": [{"id": "tn-0123456789ab", "kind": "premise_shift", "severity": "blocking",
                    "summary": "c-BBBBB changed; c-AAAAA still cites it"}],
                "resolved": [{"id": "tn-ffffffffffff", "kind": "edit_collision"}]
            }
        });
        let t = consequences_text(&reply);
        assert!(
            t.contains("tension c-AAAAA: resolve (premise_shift c-BBBBB) tn-0123456789ab"),
            "{t}"
        );
        assert!(t.contains("tensions opened:\n  tn-0123456789ab blocking premise_shift: c-BBBBB changed; c-AAAAA still cites it"), "{t}");
        assert!(
            t.contains("tensions resolved: tn-ffffffffffff (edit_collision)"),
            "{t}"
        );
        let quiet =
            serde_json::json!({"consequences": [], "on_change": {"tensions": [], "resolved": []}});
        assert_eq!(consequences_text(&quiet), "");
        let collapsed = serde_json::json!({"consequences": [], "on_change": {"tensions": [],
            "resolved": [], "collapsed": {"duplicate_definition": 10, "area_overlap": 2}}});
        assert!(
            consequences_text(&collapsed)
                .contains("tensions not listed: 2 area_overlap, 10 duplicate_definition")
        );
    }

    #[test]
    fn ccli1_code_text_lists_implements_reflects_and_unresolved() {
        let v = json!({"code": {
            "path": "src/a.rs", "repos": ["../app"], "provisional": true, "rework": false,
            "implements": [
                {"block": "D-1", "commits": 1, "status": "proposed", "dependent_commits": 2, "provisional": true, "rework": false},
                {"block": "c-A", "commits": 3, "provisional": false, "rework": false}
            ],
            "reflects": [{"block": "c-E", "glob": "src/**/*.rs"}],
            "unresolved": ["§4.2"]
        }});
        assert_eq!(
            code_text(&v),
            "code src/a.rs (repos: ../app)\nimplements:\n  D-1 [proposed] 1 commit(s), 2 depend on it, provisional\n  c-A 3 commit(s)\nreflects:\n  c-E src/**/*.rs\nunresolved: §4.2\n"
        );
        let none = json!({"code": {"path": "x.rs", "repos": [], "implements": [], "reflects": [], "unresolved": [],
            "provisional": false, "rework": false}});
        assert_eq!(
            code_text(&none),
            "code x.rs (no code repo linked: set code_repos in .rdy.yaml)\nimplements:\n  (none)\nreflects:\n  (none)\n"
        );
    }

    #[test]
    fn ccli1_plain_drops_the_comment_marks_and_the_save_note() {
        let b = json!({
            "covers": 1,
            "governs": [{"decision": "D-1", "status": "live", "statement": "Keep A", "blocks": ["c-A"]}],
            "dependents": [], "will_go_stale": [], "suspects": [],
            "base": {"token": "0123456789abcdef0123", "blocks": [{"id": "c-A", "version": "ff"}], "depends": 0}
        });
        assert_eq!(
            plain(&b),
            "rdy briefing for c-A (token 0123456789ab)\ngoverns:\n  D-1 [live] Keep A\ndepends on this:\n  (none)\nLineage:\n  (none)\nwill go stale if this changes:\n  (none)\nopen suspects:\n  (none)\n"
        );
    }

    #[test]
    fn ccli1_a_briefing_that_covers_no_blocks_says_so() {
        let b = json!({
            "covers": 0, "governs": [], "dependents": [], "will_go_stale": [], "suspects": [],
            "base": {"token": "0123456789abcdef0123", "blocks": [], "depends": 0}
        });
        assert_eq!(
            plain(&b).lines().next(),
            Some("rdy briefing for no blocks (token 0123456789ab)")
        );
    }

    #[test]
    fn infl1_the_header_lists_work_in_flight_when_the_briefing_has_it() {
        let b = json!({
            "governs": [], "dependents": [], "will_go_stale": [], "suspects": [],
            "inflight": {
                "views": [
                    {"view": "va", "owner": {"actor": "agent:a", "run": "r1"}, "last_write": "2026-10-06T10:00:00Z", "last_write_ms": 1, "blocks": ["c-A"]},
                    {"view": "vu", "owner": null, "last_write": "2026-10-06T11:00:00Z", "last_write_ms": 2, "blocks": ["c-A", "c-B"]}
                ],
                "proposals": [{"ref": "P-abcdef012345@2", "view": "pa", "title": "Change A", "blocks": ["c-A"]}]
            },
            "base": {"token": "0123456789abcdef0123", "blocks": [{"id": "c-A", "version": "ff"}], "depends": 0}
        });
        let h = header(&b);
        let lines: Vec<&str> = h.lines().collect();
        let at = lines
            .iter()
            .position(|l| *l == "# in flight:")
            .expect("the section");
        assert_eq!(lines[at - 2], "# open suspects:");
        assert_eq!(
            &lines[at + 1..at + 4],
            &[
                "#   view va by agent:a/r1 (last write 2026-10-06T10:00:00Z): c-A",
                "#   view vu, unowned (last write 2026-10-06T11:00:00Z): c-A, c-B",
                "#   proposal P-abcdef012345@2 from pa \"Change A\": c-A",
            ]
        );
        assert_eq!(lines[at + 4], "#");
    }

    #[test]
    fn tbrf1_the_edit_header_lists_tensions() {
        let b = json!({
            "governs": [], "dependents": [], "will_go_stale": [], "suspects": [],
            "inflight": {
                "views": [], "proposals": [], "intents": [],
                "tensions": [
                    {"id": "tn-0123456789ab", "kind": "fork", "severity": "blocking", "status": "dismissed", "summary": "D-1 and D-2 both supersede D-0"},
                    {"id": "tn-abcdef012345", "kind": "area_overlap", "severity": "advisory", "status": "open", "summary": "two intents\nshare an area"}
                ]
            },
            "base": {"token": "0123456789abcdef0123", "blocks": [{"id": "c-A", "version": "ff"}], "depends": 0}
        });
        let h = header(&b);
        assert!(h.lines().all(|l| l.starts_with('#')), "{h}");
        assert!(
            h.contains("  tension tn-0123456789ab blocking fork: D-1 and D-2 both supersede D-0 (dismissed)\n"),
            "{h}"
        );
        assert!(
            h.contains(
                "  tension tn-abcdef012345 advisory area_overlap: two intents share an area\n"
            ),
            "{h}"
        );
    }

    #[test]
    fn tbrf1_the_edit_header_counts_collapsed_tensions_and_a_read_error() {
        let b = json!({
            "governs": [], "dependents": [], "will_go_stale": [], "suspects": [],
            "inflight": {
                "views": [], "proposals": [], "intents": [], "tensions": [],
                "tensions_collapsed": {"area_overlap": 2, "duplicate_definition": 10},
                "tensions_error": "E_INTERNAL: boom"
            },
            "base": {"token": "0123456789abcdef0123", "blocks": [{"id": "c-A", "version": "ff"}], "depends": 0}
        });
        let h = header(&b);
        assert!(
            h.contains("  tensions not listed: 2 area_overlap, 10 duplicate_definition\n"),
            "{h}"
        );
        assert!(h.contains("  tensions not read: E_INTERNAL: boom\n"), "{h}");
    }

    #[test]
    fn cc16_the_consequences_text_prints_the_change() {
        let reply = json!({
            "coverage": {"before": 80, "after": 85, "pages": [
                {"doc": "d-1", "path": "ops", "before": 70, "after": 90},
                {"doc": "d-2", "path": "fees", "before": 100, "after": 100}
            ]}
        });
        assert_eq!(
            consequences_text(&reply),
            "\ncoverage: 80 → 85\n  ops: 70 → 90"
        );
        // nothing when every number is unchanged
        let same = json!({"coverage": {"before": 80, "after": 80, "pages": [
            {"doc": "d-1", "path": "ops", "before": 70, "after": 70}
        ]}});
        assert_eq!(consequences_text(&same), "");
        // an import reply's coverage is the baseline report: no before or after, nothing printed
        let baseline = json!({"coverage": {"score": 60, "dimensions": {}}});
        assert_eq!(consequences_text(&baseline), "");
        // a page that moved under an unchanged corpus score still prints
        let page = json!({"coverage": {"before": 80, "after": 80, "pages": [
            {"doc": "d-1", "path": "ops", "before": 70, "after": 71}
        ]}});
        assert_eq!(
            consequences_text(&page),
            "\ncoverage: 80 → 80\n  ops: 70 → 71"
        );
        // a cut page list says how many pages it left out
        let cut = json!({"coverage": {"before": 80, "after": 85, "pages_omitted": 3, "pages": [
            {"doc": "d-1", "path": "ops", "before": 70, "after": 90}
        ]}});
        assert_eq!(
            consequences_text(&cut),
            "\ncoverage: 80 → 85\n  ops: 70 → 90\n  and 3 more pages"
        );
        // two moved pages and omitted unchanged pages: the count would read as moved pages, so
        // it is not printed (the cut sorts moved pages first)
        let unchanged_cut = json!({"coverage": {"before": 80, "after": 85, "pages_omitted": 10, "pages": [
            {"doc": "d-1", "path": "ops", "before": 70, "after": 90},
            {"doc": "d-3", "path": "audit", "before": 50, "after": 60},
            {"doc": "d-2", "path": "fees", "before": 100, "after": 100}
        ]}});
        assert_eq!(
            consequences_text(&unchanged_cut),
            "\ncoverage: 80 → 85\n  ops: 70 → 90\n  audit: 50 → 60"
        );
        let err = json!({"coverage_error": "E_INTERNAL: boom"});
        assert_eq!(
            consequences_text(&err),
            "\nnot worked out: the coverage change (E_INTERNAL: boom)"
        );
    }

    #[test]
    fn cc15_the_edit_header_says_when_the_coverage_was_not_read() {
        let h = header(&json!({
            "governs": [], "dependents": [], "will_go_stale": [], "suspects": [],
            "base": {"token": "t", "blocks": []},
            "coverage_error": "E_INTERNAL: boom"
        }));
        assert!(h.contains("# coverage not read: E_INTERNAL: boom\n"), "{h}");
    }

    #[test]
    fn infl1_free_text_with_line_breaks_stays_inside_the_comment() {
        // libyaml (under serde_yaml_ng) also breaks lines at NEL, U+2028 and U+2029.
        let b = json!({
            "governs": [{"decision": "D-1", "status": "live", "statement": "Keep A\nbody: X", "blocks": ["c-A"]}],
            "dependents": [], "will_go_stale": [], "suspects": [],
            "inflight": {
                "views": [{"view": "vb\n[", "owner": null, "last_write": "t", "last_write_ms": 1, "blocks": ["c-A"]}],
                "proposals": [{"ref": "P-abcdef012345@1", "view": "va\r\n- x",
                    "title": "Change A\nbody: INJECTED\n#\tz\u{7}\u{85}a\u{2028}b\u{2029}c", "blocks": ["c-A"]}]
            },
            "base": {"token": "0123456789abcdef0123", "blocks": [{"id": "c-A", "version": "ff"}], "depends": 0}
        });
        let h = header(&b);
        assert!(h.lines().all(|l| l.starts_with('#')), "{h}");
        let breaks =
            |c: char| (c.is_control() && c != '\n') || matches!(c, '\u{2028}' | '\u{2029}');
        assert!(!h.chars().any(breaks), "{h:?}");
        let lines: Vec<&str> = h.lines().collect();
        assert!(lines.contains(&"#   D-1 [live] Keep A body: X"), "{h}");
        assert!(
            lines.contains(&"#   view vb [, unowned (last write t): c-A"),
            "{h}"
        );
        assert!(
            lines.contains(
                &"#   proposal P-abcdef012345@1 from va  - x \"Change A body: INJECTED # z  a b c\": c-A"
            ),
            "{h}"
        );
    }

    #[test]
    fn bent1_the_header_shows_related_decisions_mentions_and_coverage() {
        let b = json!({
            "governs": [], "dependents": [], "will_go_stale": [], "suspects": [],
            "base": {"token": "abcdef0123456789", "blocks": [{"id": "D-2", "version": "x"}]},
            "related_decisions": [{"for": "D-2", "decision": "D-1", "status": "live", "statement": "Ledger first", "relation": "same_entity", "entities": ["en-1"]}],
            "mentioned_by": [{"doc": "d-1", "block": "c-1", "field": "body", "mention": "Archive"}],
            "coverage": {"score": 70, "dimensions": {"grounded": 50, "complete": 100}, "pages": [], "issues": [], "confidence": "high", "reasons": []}
        });
        let h = header(&b);
        assert!(
            h.contains("# related decisions:\n#   D-1 [live] same_entity (en-1): Ledger first\n"),
            "{h}"
        );
        assert!(
            h.contains("# mentioned by:\n#   c-1 names d-1 (\"Archive\" in body)\n"),
            "{h}"
        );
        assert!(
            h.contains("# coverage: 70 (grounded 50, complete 100); confidence high\n"),
            "{h}"
        );
        assert!(h.ends_with("\n#\n"), "the closing # stays last: {h}");
        let quiet = header(
            &json!({"governs": [], "dependents": [], "will_go_stale": [], "suspects": [], "base": {"token": "t", "blocks": []}}),
        );
        assert!(
            !quiet.contains("related decisions")
                && !quiet.contains("mentioned by")
                && !quiet.contains("coverage"),
            "{quiet}"
        );
    }

    #[test]
    fn cc13_the_coverage_line_and_reasons_print() {
        let b = json!({
            "governs": [], "dependents": [], "will_go_stale": [], "suspects": [],
            "base": {"token": "abcdef0123456789", "blocks": [{"id": "c-1", "version": "x"}]},
            "coverage": {
                "score": 62,
                "dimensions": {"complete": 75, "grounded": 50, "governed": 100},
                "pages": [{"doc": "d-1", "path": "ops", "score": 62}],
                "issues": [],
                "confidence": "low",
                "reasons": [
                    {"block": "c-1", "dimension": "current", "code": "open_suspect", "subject": "c-9", "fix": "Review c-1 against c-9."},
                    {"block": "c-2", "dimension": "complete", "code": "untied_content", "subject": null, "fix": "Link c-2 to an entity."}
                ],
                "reasons_omitted": 3
            }
        });
        let h = header(&b);
        let lines: Vec<&str> = h.lines().collect();
        let at = lines
            .iter()
            .position(|l| l.starts_with("# coverage:"))
            .expect(&h);
        assert_eq!(
            &lines[at..at + 4],
            [
                "# coverage: 62 (grounded 50, complete 75, governed 100); confidence low",
                "#   open_suspect c-1: Review c-1 against c-9.",
                "#   untied_content c-2: Link c-2 to an entity.",
                "#   … 3 more",
            ]
        );
        // a briefing that covers no scored block prints nothing
        let none = header(&json!({
            "governs": [], "dependents": [], "will_go_stale": [], "suspects": [],
            "base": {"token": "t", "blocks": []},
            "coverage": {"score": 100, "dimensions": {}, "pages": [], "issues": [], "confidence": "high", "reasons": []}
        }));
        assert!(!none.contains("coverage"), "{none}");
    }

    #[test]
    fn bent1_write_text_lists_suggestions_and_related_decisions() {
        let v = json!({
            "consequences": [],
            "suggestions": [
                {"kind": "link", "id": "sg-1", "block": "c-1", "link": "describes", "target": "en-1", "mention": "ledger", "field": "body"},
                {"kind": "entity", "id": "sg-2", "literal": "max_conn", "blocks": ["c-1", "c-2", "c-3"], "count": 3}
            ],
            "suggestions_omitted": 2,
            "related_decisions": [{"for": "D-2", "decision": "D-1", "status": "live", "statement": "Ledger first", "relation": "same_entity", "entities": ["en-1"]}]
        });
        assert_eq!(
            consequences_text(&v),
            "\nsuggestions:\n  link c-1 describes en-1 (\"ledger\" in body)\n  entity `max_conn` (3 blocks)\n  and 2 more\nrelated decisions:\n  D-1 [live] same_entity (en-1): Ledger first"
        );
        assert_eq!(
            consequences_text(&json!({"consequences": [], "suggestions": []})),
            ""
        );
    }

    #[test]
    fn msw14_an_outgoing_lineage_line_reads_subject_verb_object() {
        let b = json!({
            "covers": 1, "governs": [], "dependents": [],
            "lineage": [{"block": "c-A", "kind": "refines", "direction": "out", "on": "c-B", "summary": "A holds."}],
            "will_go_stale": [], "suspects": [],
            "base": {"token": "0123456789abcdef0123", "blocks": [{"id": "c-B", "version": "ff"}], "depends": 0}
        });
        let h = header(&b);
        assert!(
            h.lines()
                .any(|l| l == "#   c-A (out: c-B refines c-A) A holds."),
            "{h}"
        );
        assert!(!h.contains("(out refines c-B)"), "{h}");
    }

    #[test]
    fn mfb2_the_header_lists_reaches_only_when_there_are_some() {
        let base = json!({"token": "0123456789abcdef", "blocks": [{"id": "D-2", "version": "ff"}], "depends": 0});
        let none = header(&json!({"base": base}));
        assert!(!none.contains("reaches"), "{none}");
        let h = header(&json!({
            "base": base,
            "reaches": [{"decision": "D-2", "via": "refines", "target": "D-1", "blocks": ["c-A", "c-C"]}],
            "reaches_omitted": 3
        }));
        let lines: Vec<&str> = h.lines().collect();
        let at = lines
            .iter()
            .position(|l| *l == "# reaches:")
            .expect("a reaches section");
        assert_eq!(lines[at + 1], "#   D-2 refines D-1: c-A, c-C");
        assert_eq!(lines[at + 2], "#   and 3 more");
        let lineage = lines.iter().position(|l| *l == "# Lineage:").unwrap();
        let stale = lines
            .iter()
            .position(|l| *l == "# will go stale if this changes:")
            .unwrap();
        assert!(lineage < at && at < stale, "{h}");
    }

    #[test]
    fn mfb6_write_text_lists_intents_after_the_consequences_and_errors_last() {
        let v = json!({
            "consequences": [{"id": "cq-1", "block": "c-B", "kind": "dependent",
                "because": {"edge": "references", "changed": "c-A"}, "follow_up": "confirm"}],
            "intents": [{"id": "in-1", "actor": "agent:a", "run": "ra", "view": "va",
                "angle": "rewording", "expires_at": "2026-10-10T00:00:00Z", "blocks": ["c-A", "c-B"]}],
            "suggestions": [],
            "suggestions_error": "E_IO: index busy",
            "on_change_error": "E_IO: tensions busy"
        });
        assert_eq!(
            consequences_text(&v),
            "\nconsequences:\n  dependent c-B: confirm (references c-A)\nintents on these blocks:\n  intent in-1 by agent:a/ra in va: rewording (until 2026-10-10T00:00:00Z) on 2 block(s)\nnot worked out: the tension changes (E_IO: tensions busy)\nnot worked out: suggestions (E_IO: index busy)"
        );
    }

    #[test]
    fn mfb6_no_intents_and_no_errors_print_nothing_new() {
        assert_eq!(consequences_text(&json!({"consequences": []})), "");
        let v = json!({"consequences": [], "intents": [{"id": "in-2", "actor": "agent:b", "view": "v",
            "angle": "x", "expires_at": "t", "blocks": ["c-A"]}]});
        assert_eq!(
            consequences_text(&v),
            "\nintents on these blocks:\n  intent in-2 by agent:b in v: x (until t) on 1 block(s)"
        );
    }
}
