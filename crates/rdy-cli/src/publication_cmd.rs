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
    },
}

/// The ten array names of the report that a row cut can leave rows out of: the count of rows
/// left out, summed.
fn omitted_any(v: &Value) -> bool {
    let top = num(&v["selection"]["omitted_entries"]) + num(&v["references"]["omitted_rows"]);
    let diff: u64 = v["disclosure"]["omitted"]
        .as_object()
        .map_or(0, |o| o.values().map(num).sum());
    top + diff > 0
}

/// The listed rows of a disclosure array plus the rows the report left out: its full count.
fn full_count(d: &Value, name: &str) -> u64 {
    d[name].as_array().map_or(0, |a| a.len() as u64) + num(&d["omitted"][name])
}

/// The text of a `publication_check` reply (the lines are in the order the docs list). The
/// implicit profile (an `id` that is null) is named by its audience; a profile block by its key.
fn check_text(v: &Value, asked: &str) -> String {
    let p = &v["profile"];
    let status = text(&v["references"]["status"]);
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
    ];
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
            "Disclosure changes: {} added docs, {} removed docs, {} changed docs, {} changed folders, {attachments} attachment changes, {} new references.",
            full_count(d, "added_docs"),
            full_count(d, "removed_docs"),
            full_count(d, "changed_docs"),
            full_count(d, "changed_folders"),
            num(&d["new_reference_count"]),
        ));
    }
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
    } = cmd;
    let v = call(
        env,
        "publication_check",
        json!({"profile": profile, "scope": scope, "against": against}),
    )?;
    let t = check_text(&v, &profile);
    Ok((v, t))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(against: bool, omitted: u64) -> Value {
        json!({
            "profile": {"key": "site"},
            "selection": {"blocks": 7, "docs": 2, "folders": 1, "omitted_entries": 0},
            "references": {"status": "warning", "total": 3, "omitted_rows": omitted},
            "disclosure": against.then(|| json!({
                "profile_changed": true,
                "added_docs": [{}], "removed_docs": [], "changed_docs": [{}, {}],
                "changed_folders": [], "added_attachments": [{}], "removed_attachments": [],
                "changed_attachments": [{}],
                "new_references": [], "new_reference_count": 5,
                "omitted": {"added_docs": 0, "removed_docs": 4, "changed_docs": 0, "changed_folders": 0,
                    "added_attachments": 0, "removed_attachments": 0, "changed_attachments": 0, "new_references": 0}
            })),
        })
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
    fn pub9_the_text_has_three_lines_and_a_comparison_with_full_counts() {
        assert_eq!(
            check_text(&report(false, 0), "site"),
            "Publication profile site: warning.\n7 blocks in 2 docs and 1 folders.\nReferences outside selection: 3."
        );
        assert_eq!(
            check_text(&report(true, 0), "site"),
            "Publication profile site: warning.\n7 blocks in 2 docs and 1 folders.\nReferences outside selection: 3.\nProfile revision changed: yes.\nDisclosure changes: 1 added docs, 4 removed docs, 2 changed docs, 0 changed folders, 2 attachment changes, 5 new references.\nSome rows are omitted. Counts include every row."
        );
        assert!(
            check_text(&report(false, 1), "site")
                .ends_with("\nSome rows are omitted. Counts include every row.")
        );
    }
}
