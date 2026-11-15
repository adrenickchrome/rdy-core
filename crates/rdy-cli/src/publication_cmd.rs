//! `rdy publication check`: the reference check and disclosure comparison of a publication
//! profile (D-0VCQ5, D-KDFZ3, D-KQ4CA). It builds the request for the ctl op `publication_check`,
//! which calls the engine function `rdy_publication_check` calls; this file only prints the reply.
//! `--json` prints the reply as it is. The check authorizes nothing: no approval or publish
//! prompt follows it.
use crate::conn::{Env, call};
use crate::output::{num, text};
use clap::Subcommand;
use rdy_daemon::ErrorReply;
use serde_json::{Value, json};

#[derive(Subcommand)]
pub(crate) enum PublicationCmd {
    /// Check a publication profile: the docs it selects, the references that leave the selection, and with --against what the output discloses that a baseline did not. It checks local output; it does not authorize publication or find the last publication. The chat path is rdy_publication_check.
    Check {
        /// The profile: its id or key.
        #[arg(long)]
        profile: String,
        // the help is given in full: a doc comment would lose its closing period
        #[arg(
            long,
            help = "canon (default), tag:<name>, landing:<hash>, a view name, view:<name>, or proposal:P-x@n."
        )]
        scope: Option<String>,
        /// A baseline to compare with: canon, tag:<name> or landing:<hash>. Same profile id and audience.
        #[arg(long)]
        against: Option<String>,
        /// The product version this candidate ships as, such as 1.2.0. Shown content and decisions that ship in a later version make the result blocked, which still exits 0. Omit it for no release check.
        #[arg(long)]
        release: Option<String>,
    },
}

/// The sum of the counters `names` over the rows of the array `rows`.
fn row_omissions(rows: &Value, names: &[&str]) -> u64 {
    rows.as_array().map_or(0, |a| {
        a.iter()
            .map(|r| names.iter().map(|n| num(&r[*n])).sum::<u64>())
            .sum()
    })
}

/// The omission counters of a doc summary's inner lists (D-RDH1C).
const SUMMARY_OMISSIONS: [&str; 3] = [
    "omitted_outline",
    "omitted_decision_titles",
    "omitted_figures",
];

/// The arrays of the report that a row cut can leave rows out of, link rows too, and the inner
/// lists of the review's rows: the count of rows left out, summed.
fn omitted_any(v: &Value) -> bool {
    let refs = &v["references"];
    let d = &v["disclosure"];
    let top = num(&v["selection"]["omitted_entries"])
        + num(&refs["omitted_rows"])
        + num(&refs["links_outside"]["omitted_rows"])
        + num(&refs["dropped_decisions"]["omitted_rows"])
        + num(&v["release"]["omitted_rows"])
        + num(&v["hidden_planned"]["omitted_rows"])
        + num(&d["newly_hidden_planned"]["omitted_rows"])
        + num(&v["included"]["omitted_rows"]);
    let diff: u64 = d["omitted"]
        .as_object()
        .map_or(0, |o| o.values().map(num).sum());
    let inner = row_omissions(
        &d["changed_docs"],
        &["omitted_changes", "omitted_decisions"],
    ) + row_omissions(&d["added_docs"], &SUMMARY_OMISSIONS)
        + row_omissions(&v["included"]["rows"], &SUMMARY_OMISSIONS);
    top + diff + inner > 0
}

/// `n` with its noun: the singular for a count of 1 (D-MCH70).
fn count(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// A doc row's title; a null title prints as the notice for removed content (D-MCH70).
fn title(row: &Value) -> &str {
    row["title"].as_str().unwrap_or("Content removed.")
}

/// The review's lines (D-MCH70): with a comparison, one line for each changed, added and removed
/// doc, in the order of its list; without one, the included totals.
fn review_lines(v: &Value) -> Vec<String> {
    let d = &v["disclosure"];
    let rows = |name: &str| d[name].as_array().cloned().unwrap_or_default();
    let mut lines = vec![];
    if d.is_object() {
        for r in rows("changed_docs") {
            let c = &r["counts"];
            lines.push(if r["record_only"].as_bool().unwrap_or(false) {
                format!("Edited {} (no change a reader sees)", title(&r))
            } else {
                format!(
                    "Edited {} (blocks: {} added, {} removed, {} updated, {} moved)",
                    title(&r),
                    num(&c["added"]),
                    num(&c["removed"]),
                    num(&c["updated"]),
                    num(&c["moved"])
                )
            });
        }
        for (word, name) in [("New", "added_docs"), ("Removed", "removed_docs")] {
            for r in rows(name) {
                let blocks = count(num(&r["size"]["blocks"]), "block", "blocks");
                lines.push(format!("{word} {} ({blocks})", title(&r)));
            }
        }
    }
    let t = &v["included"]["totals"];
    if t.is_object() {
        lines.push(format!(
            "Included: {}, {}, {}, {}, {}, {}.",
            count(num(&t["docs"]), "doc", "docs"),
            count(num(&t["folders"]), "folder", "folders"),
            count(num(&t["sections"]), "section", "sections"),
            count(num(&t["blocks"]), "block", "blocks"),
            count(num(&t["decisions"]), "decision", "decisions"),
            count(num(&t["figures"]), "figure", "figures"),
        ));
    }
    lines
}

/// The listed rows of a disclosure array plus the rows the report left out: its full count.
fn full_count(d: &Value, name: &str) -> u64 {
    d[name].as_array().map_or(0, |a| a.len() as u64) + num(&d["omitted"][name])
}

/// The line for one hidden list, or nothing for an empty one. A doc row with no hidden block still
/// counts as a doc.
fn hidden_line(label: &str, list: &Value) -> Option<String> {
    (num(&list["docs"]) > 0).then(|| {
        format!(
            "{label}: {} blocks in {} docs.",
            num(&list["blocks"]),
            num(&list["docs"])
        )
    })
}

/// The text of a `publication_check` reply (the lines are in the order the docs list), then the
/// review's lines. The implicit profile (an `id` that is null) is named by its audience; a
/// profile block by its key.
fn check_text(v: &Value, asked: &str) -> String {
    let p = &v["profile"];
    let status = text(&v["status"]);
    let first = if p.get("id").is_some_and(Value::is_null) {
        format!(
            "Implicit publication profile ({}): {status}.",
            text(&p["audience"])
        )
    } else {
        let key = p["key"].as_str().unwrap_or(asked);
        format!("Publication profile {key}: {status}.")
    };
    let s = &v["selection"];
    let mut lines = vec![
        first,
        format!(
            "{} blocks in {} docs and {} folders.",
            num(&s["blocks"]),
            num(&s["docs"]),
            num(&s["folders"])
        ),
        format!(
            "References outside selection: {}.",
            num(&v["references"]["total"])
        ),
        format!(
            "Links outside selection: {}.",
            num(&v["references"]["links_outside"]["count"])
        ),
    ];
    let rel = &v["release"];
    if rel.is_object() {
        lines.push(format!("Release: {}.", text(&rel["version"])));
        lines.push(format!(
            "Blocks after release: {}.",
            num(&rel["blocks_later"])
        ));
        lines.push(format!(
            "Decisions after release: {}.",
            num(&rel["decisions_later"])
        ));
    }
    lines.extend(hidden_line("Hidden as planned", &v["hidden_planned"]));
    let d = &v["disclosure"];
    if d.is_object() {
        let changed = if d["profile_changed"].as_bool().unwrap_or(false) {
            "yes"
        } else {
            "no"
        };
        lines.push(format!("Profile revision changed: {changed}."));
        let attachments = [
            "added_attachments",
            "removed_attachments",
            "changed_attachments",
        ]
        .iter()
        .map(|n| full_count(d, n))
        .sum::<u64>();
        lines.push(format!(
            "Disclosure changes: {} added docs, {} removed docs, {} changed docs, {} changed folders, {} changed blocks outside docs, {attachments} attachment changes, {} new references.",
            full_count(d, "added_docs"),
            full_count(d, "removed_docs"),
            full_count(d, "changed_docs"),
            full_count(d, "changed_folders"),
            full_count(d, "changed_outside_docs"),
            num(&d["new_reference_count"]),
        ));
        lines.extend(hidden_line(
            "Newly hidden as planned",
            &d["newly_hidden_planned"],
        ));
    }
    lines.extend(review_lines(v));
    if omitted_any(v) {
        lines.push("Some rows are omitted. Counts include every row.".to_string());
    }
    lines.join("\n")
}

pub(crate) fn publication(env: &Env, cmd: PublicationCmd) -> Result<(Value, String), ErrorReply> {
    let PublicationCmd::Check {
        profile,
        scope,
        against,
        release,
    } = cmd;
    let v = call(
        env,
        "publication_check",
        json!({"profile": profile, "scope": scope, "against": against, "release": release}),
    )?;
    let t = check_text(&v, &profile);
    Ok((v, t))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(against: bool, omitted: u64) -> Value {
        let counts = |a: u64, r: u64, u: u64, m: u64| json!({"added": a, "removed": r, "updated": u, "moved": m});
        json!({
            "profile": {"key": "site"},
            "selection": {"blocks": 7, "docs": 2, "folders": 1, "omitted_entries": 0},
            "status": "warning",
            "references": {"status": "warning", "total": 3, "omitted_rows": omitted},
            "disclosure": against.then(|| json!({
                "profile_changed": true,
                "added_docs": [{"title": "Setup", "size": {"blocks": 6}}], "removed_docs": [],
                "changed_docs": [
                    {"title": "Guide", "record_only": false, "counts": counts(2, 1, 3, 0)},
                    {"title": "Notes", "record_only": true, "counts": counts(0, 0, 0, 0)}
                ],
                "changed_folders": [], "changed_outside_docs": [{}, {}],
                "added_attachments": [{}], "removed_attachments": [],
                "changed_attachments": [{}],
                "new_references": [], "new_reference_count": 5,
                "omitted": {"added_docs": 0, "removed_docs": 4, "changed_docs": 0, "changed_folders": 0,
                    "changed_outside_docs": 1,
                    "added_attachments": 0, "removed_attachments": 0, "changed_attachments": 0, "new_references": 0}
            })),
        })
    }

    /// [`report`] of a first check, with the included totals the review adds.
    fn first_report() -> Value {
        let mut v = report(false, 0);
        v["included"] = json!({
            "docs": 2, "folders": 1, "rows": [], "omitted_rows": 0,
            "totals": {"docs": 2, "folders": 1, "sections": 3, "blocks": 4, "decisions": 0, "figures": 2}
        });
        v
    }

    #[test]
    fn lo1_the_links_outside_line_follows_the_references_line() {
        let mut v = report(false, 0);
        v["references"]["links_outside"] = json!({"count": 844, "rows": [], "omitted_rows": 0});
        assert_eq!(
            check_text(&v, "site"),
            "Publication profile site: warning.\n7 blocks in 2 docs and 1 folders.\nReferences outside selection: 3.\nLinks outside selection: 844."
        );
        // a link row that the report leaves out is an omitted row
        v["references"]["links_outside"]["omitted_rows"] = json!(644);
        assert!(check_text(&v, "site").ends_with(
            "\nLinks outside selection: 844.\nSome rows are omitted. Counts include every row."
        ));
    }

    #[test]
    fn pub12_the_implicit_profile_is_named_by_its_audience() {
        let mut v = report(true, 0);
        v["profile"] = json!({"id": null, "version": null, "key": null, "audience": "org"});
        let t = check_text(&v, "implicit:org");
        assert!(
            t.starts_with("Implicit publication profile (org): warning.\n7 blocks in 2 docs"),
            "{t}"
        );
        assert_eq!(
            t.lines().skip(1).collect::<Vec<_>>(),
            check_text(&report(true, 0), "site")
                .lines()
                .skip(1)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn pub9_the_text_has_four_lines_and_a_comparison_with_full_counts() {
        assert_eq!(
            check_text(&first_report(), "site"),
            "Publication profile site: warning.\n7 blocks in 2 docs and 1 folders.\nReferences outside selection: 3.\nLinks outside selection: 0.\nIncluded: 2 docs, 1 folder, 3 sections, 4 blocks, 0 decisions, 2 figures."
        );
        assert_eq!(
            check_text(&report(true, 0), "site"),
            "Publication profile site: warning.\n7 blocks in 2 docs and 1 folders.\nReferences outside selection: 3.\nLinks outside selection: 0.\nProfile revision changed: yes.\nDisclosure changes: 1 added docs, 4 removed docs, 2 changed docs, 0 changed folders, 3 changed blocks outside docs, 2 attachment changes, 5 new references.\nEdited Guide (blocks: 2 added, 1 removed, 3 updated, 0 moved)\nEdited Notes (no change a reader sees)\nNew Setup (6 blocks)\nSome rows are omitted. Counts include every row."
        );
        assert!(
            check_text(&report(false, 1), "site")
                .ends_with("\nSome rows are omitted. Counts include every row.")
        );
    }

    #[test]
    fn relc4_the_hidden_lines_follow_the_other_lines_and_skip_empty_lists() {
        let mut v = report(true, 0);
        v["hidden_planned"] = json!({"docs": 0, "blocks": 0, "rows": [], "omitted_rows": 0});
        v["disclosure"]["newly_hidden_planned"] =
            json!({"docs": 0, "blocks": 0, "rows": [], "omitted_rows": 0});
        let plain = check_text(&v, "site");
        assert!(!plain.contains("planned"), "{plain}");
        // a zero-block doc row is nonempty
        v["hidden_planned"] = json!({"docs": 2, "blocks": 0, "rows": [{}, {}], "omitted_rows": 0});
        v["disclosure"]["newly_hidden_planned"] =
            json!({"docs": 1, "blocks": 3, "rows": [{}], "omitted_rows": 0});
        let t = check_text(&v, "site");
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(lines[4], "Hidden as planned: 0 blocks in 2 docs.");
        assert_eq!(lines[7], "Newly hidden as planned: 3 blocks in 1 docs.");
        // either list's omitted rows reach the existing notice
        let mut w = report(false, 0);
        w["hidden_planned"] = json!({"docs": 300, "blocks": 9, "rows": [], "omitted_rows": 100});
        assert!(check_text(&w, "site").ends_with("Counts include every row."));
        let mut w = report(true, 0);
        w["disclosure"]["omitted"] = json!({});
        assert!(!check_text(&w, "site").contains("omitted"));
        w["disclosure"]["newly_hidden_planned"] =
            json!({"docs": 300, "blocks": 9, "rows": [], "omitted_rows": 100});
        assert!(check_text(&w, "site").ends_with("Counts include every row."));
    }

    /// drv6 (D-MCH70): the doc lines follow the summary in each list's order, then the included
    /// totals of a first check. A count of 1 takes the singular noun; a null title prints as
    /// "Content removed.".
    #[test]
    fn drv6_text_lists_doc_titles_and_first_totals() {
        let mut v = report(true, 0);
        v["disclosure"]["omitted"] = json!({});
        v["disclosure"]["changed_docs"] = json!([
            {"title": "Guide", "record_only": false,
             "counts": {"added": 1, "removed": 0, "updated": 2, "moved": 1}},
            {"title": null, "record_only": false,
             "counts": {"added": 0, "removed": 1, "updated": 0, "moved": 0}},
            {"title": "Quiet", "record_only": true,
             "counts": {"added": 0, "removed": 0, "updated": 0, "moved": 0}}
        ]);
        v["disclosure"]["added_docs"] = json!([
            {"title": "One", "size": {"blocks": 1}},
            {"title": null, "size": {"blocks": 0}}
        ]);
        v["disclosure"]["removed_docs"] = json!([
            {"title": "Old", "size": {"blocks": 12}},
            {"title": "Small", "size": {"blocks": 1}}
        ]);
        let t = check_text(&v, "site");
        let lines: Vec<&str> = t.lines().skip(6).collect();
        assert_eq!(
            lines,
            [
                "Edited Guide (blocks: 1 added, 0 removed, 2 updated, 1 moved)",
                "Edited Content removed. (blocks: 0 added, 1 removed, 0 updated, 0 moved)",
                "Edited Quiet (no change a reader sees)",
                "New One (1 block)",
                "New Content removed. (0 blocks)",
                "Removed Old (12 blocks)",
                "Removed Small (1 block)",
            ],
            "{t}"
        );
        // no list holds rows past a cap, so no omission line follows
        assert!(!t.contains("omitted"), "{t}");

        // a first check prints the included totals, with singular nouns for a count of 1
        let mut v = first_report();
        v["included"]["totals"] = json!({"docs": 1, "folders": 0, "sections": 1, "blocks": 1, "decisions": 1, "figures": 1});
        assert!(
            check_text(&v, "site").ends_with(
                "\nIncluded: 1 doc, 0 folders, 1 section, 1 block, 1 decision, 1 figure."
            ),
            "{}",
            check_text(&v, "site")
        );
        v["included"]["totals"] = json!({"docs": 3, "folders": 2, "sections": 0, "blocks": 9, "decisions": 4, "figures": 0});
        assert!(check_text(&v, "site").ends_with(
            "\nIncluded: 3 docs, 2 folders, 0 sections, 9 blocks, 4 decisions, 0 figures."
        ));
        // a comparison prints no included line, and a first check no doc line
        assert!(!check_text(&report(true, 0), "site").contains("Included:"));
        let first = check_text(&first_report(), "site");
        assert!(
            !first.contains("Edited") && !first.contains("New "),
            "{first}"
        );
    }

    /// drv6: every omission counter of the review reaches the one omission line, which does not
    /// change.
    #[test]
    fn drv6_text_reports_nested_omissions() {
        let notice = "\nSome rows are omitted. Counts include every row.";
        let mut clean = report(true, 0);
        clean["disclosure"]["omitted"] = json!({});
        assert!(!check_text(&clean, "site").contains("omitted"));
        let first = first_report();
        assert!(!check_text(&first, "site").contains("omitted"));
        let set = |v: &Value, path: &[&str], n: u64| {
            let mut v = v.clone();
            let mut at = &mut v;
            for p in path {
                at = match p.parse::<usize>() {
                    Ok(i) => &mut at[i],
                    Err(_) => &mut at[*p],
                };
            }
            *at = json!(n);
            v
        };
        let mut with_rows = clean.clone();
        with_rows["disclosure"]["added_docs"][0]["omitted_outline"] = json!(0);
        for path in [
            &["disclosure", "changed_docs", "0", "omitted_changes"][..],
            &["disclosure", "changed_docs", "1", "omitted_decisions"],
            &["disclosure", "added_docs", "0", "omitted_outline"],
            &["disclosure", "added_docs", "0", "omitted_decision_titles"],
            &["disclosure", "added_docs", "0", "omitted_figures"],
            &["references", "dropped_decisions", "omitted_rows"],
        ] {
            let v = set(&with_rows, path, 2);
            assert!(check_text(&v, "site").ends_with(notice), "{path:?}");
            assert!(!check_text(&set(&with_rows, path, 0), "site").contains("omitted"));
        }
        let mut first = first_report();
        first["included"]["rows"] =
            json!([{"omitted_outline": 0, "omitted_decision_titles": 0, "omitted_figures": 0}]);
        for path in [
            &["included", "omitted_rows"][..],
            &["included", "rows", "0", "omitted_outline"],
            &["included", "rows", "0", "omitted_decision_titles"],
            &["included", "rows", "0", "omitted_figures"],
            &["references", "dropped_decisions", "omitted_rows"],
        ] {
            let v = set(&first, path, 1);
            assert!(check_text(&v, "site").ends_with(notice), "{path:?}");
        }
        assert!(!check_text(&first, "site").contains("omitted"));
    }
}
