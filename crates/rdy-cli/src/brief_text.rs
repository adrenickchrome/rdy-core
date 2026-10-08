//! cbrf1: the text `rdy edit` shows. The briefing becomes `#` comment lines above the YAML
//! (the YAML parser drops them on save), and a write's consequences become a short list.
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
            format!(
                "{} ({} {} {}) {}",
                s(l, "block"),
                s(l, "direction"),
                s(l, "kind"),
                s(l, "on"),
                s(l, "summary")
            )
        }),
    );
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
        section(
            &mut out,
            "in flight",
            views.chain(proposals).chain(intents).chain(tensions),
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
    let g = &briefing["coverage"]["grounded"];
    let n = |k: &str| g[k].as_u64().unwrap_or(0);
    if g.is_object() && n("facts") + n("content") + n("mentions") > 0 {
        out.push(format!(
            "# coverage: {}/{} facts tied, {}/{} content tied, {}/{} mentions linked",
            n("facts_tied"),
            n("facts"),
            n("content_tied"),
            n("content"),
            n("mentions_linked"),
            n("mentions")
        ));
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
            "code {} (no code repo linked: set code_repos in .rdy/config.yaml)\n",
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
/// else `\nconsequences:` and one line per entry (with a line for what a cut left out), then
/// `\nsuggestions:` and `\nrelated decisions:` in the same way.
pub fn consequences_text(reply: &Value) -> String {
    let mut t = consequences_part(reply);
    t.push_str(&suggestions_part(reply));
    t.push_str(&related_part(reply));
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
            "code x.rs (no code repo linked: set code_repos in .rdy/config.yaml)\nimplements:\n  (none)\nreflects:\n  (none)\n"
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
            "coverage": {"scope": "covered", "grounded": {"facts": 2, "facts_tied": 1, "facts_tied_pct": 50, "content": 3, "content_tied": 1, "content_tied_pct": 33, "mentions": 2, "mentions_linked": 1, "mentions_linked_pct": 50}, "connected": {"docs": 0, "unlinked_docs": []}, "entities": 0, "kinds": [], "similar_kinds": []}
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
            h.contains("# coverage: 1/2 facts tied, 1/3 content tied, 1/2 mentions linked\n"),
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
}
