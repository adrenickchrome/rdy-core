//! cbrf1: the text `rdy edit` shows. The briefing becomes `#` comment lines above the YAML
//! (the YAML parser drops them on save), and a write's consequences become a short list.
use serde_json::Value;

/// A string field of `v`, empty when absent.
fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or_default()
}

/// One section: its title line, then its entry lines (or `(none)`).
fn section(out: &mut Vec<String>, title: &str, entries: impl Iterator<Item = String>) {
    out.push(format!("# {title}:"));
    let before = out.len();
    out.extend(entries.map(|e| format!("#   {e}")));
    if out.len() == before {
        out.push("#   (none)".into());
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
    let mut out = vec![
        format!("# rdy briefing for {} (token {token})", covered.join(", ")),
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

/// A write reply's consequences as text to append to its one-line summary: empty when there are
/// none, else `\nconsequences:` and one line per entry, with a line for what a cut left out.
pub fn consequences_text(reply: &Value) -> String {
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
            "rdy briefing for c-A (token 0123456789ab)\ngoverns:\n  D-1 [live] Keep A\ndepends on this:\n  (none)\nwill go stale if this changes:\n  (none)\nopen suspects:\n  (none)\n"
        );
    }
}
