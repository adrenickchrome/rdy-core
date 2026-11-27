use crate::conn::{Env, call};
use crate::output::{num, text};
use rdy_daemon::ErrorReply;
use serde_json::{Value, json};

#[derive(clap::Args)]
pub(crate) struct SearchArgs {
    /// The text to find (a pattern with --regex). Leave it out with --like or --like-text.
    #[arg(required_unless_present_any = ["like", "like_text"])]
    query: Option<String>,
    /// Find the blocks most like this block, in score order.
    #[arg(long, value_name = "ID")]
    like: Option<String>,
    /// Find the blocks most like this text, in score order.
    #[arg(long = "like-text", value_name = "TEXT")]
    like_text: Option<String>,
    /// Treat the query as a regular expression.
    #[arg(long)]
    regex: bool,
    /// Match regardless of case.
    #[arg(short = 'i', long)]
    ignore_case: bool,
    /// canon (the default), an existing view's name, tag:<version>, landing:<id>, proposal:P-<id>@<n> or view:<name>.
    #[arg(long)]
    scope: Option<String>,
    /// The budget for this page of matches.
    #[arg(long = "max-tokens")]
    max_tokens: Option<usize>,
    /// Continue a cut search: the cursor its last page printed.
    #[arg(long)]
    page: Option<String>,
    /// Keep this block type (repeat for more).
    #[arg(long = "type")]
    types: Vec<String>,
    /// Keep blocks with this trait (repeat for more).
    #[arg(long = "trait")]
    traits: Vec<String>,
    /// Keep blocks under this doc or folder path.
    #[arg(long)]
    path: Option<String>,
    /// draft, stable or deprecated, as inherited.
    #[arg(long)]
    lifecycle: Option<String>,
    /// A version, tag:<version> or landing:<id>: keep blocks changed since.
    #[arg(long = "changed-since")]
    changed_since: Option<String>,
    /// Leave out matches inside superseded, rejected or revoked decisions.
    #[arg(long = "exclude-history")]
    exclude_history: bool,
    /// Keep blocks this decision governs.
    #[arg(long = "governed-by")]
    governed_by: Option<String>,
    /// Keep blocks that describe this block.
    #[arg(long)]
    describes: Option<String>,
    /// Keep blocks linked to or from this block.
    #[arg(long = "linked-to")]
    linked_to: Option<String>,
    /// linked or unlinked: keep blocks that name an entity and are (or are not yet) linked to it.
    #[arg(long)]
    mentions: Option<String>,
    /// Keep only blocks with an open suspect.
    #[arg(long)]
    suspect: bool,
    /// Keep only blocks involved in an open tension (rdy tensions lists them).
    #[arg(long)]
    tension: bool,
    /// Keep hits whose block coverage score is under this (0 to 100).
    #[arg(long = "coverage-below")]
    coverage_below: Option<u32>,
    /// coverage (lowest coverage first), relevance (best score first) or reading (reading order).
    #[arg(long)]
    sort: Option<String>,
}

pub(crate) fn search(env: &Env, a: SearchArgs) -> Result<(Value, String), ErrorReply> {
    let SearchArgs {
        query,
        like,
        like_text,
        regex,
        ignore_case,
        scope,
        max_tokens,
        page,
        types,
        traits,
        path,
        lifecycle,
        changed_since,
        exclude_history,
        governed_by,
        describes,
        linked_to,
        mentions,
        suspect,
        tension,
        coverage_below,
        sort,
    } = a;
    // Empty lists and false flags are sent as absent.
    let list = |v: Vec<String>| (!v.is_empty()).then_some(v);
    let flag = |b: bool| b.then_some(true);
    let v = call(
        env,
        "search",
        json!({"query": query, "like": like, "like_text": like_text, "regex": regex, "ignore_case": ignore_case, "scope": scope, "max_tokens": max_tokens, "page": page,
                "types": list(types), "traits": list(traits), "path": path, "lifecycle": lifecycle, "changed_since": changed_since,
                "exclude_history": flag(exclude_history), "governed_by": governed_by, "describes": describes, "linked_to": linked_to,
                "mentions": mentions, "suspect": flag(suspect), "tension": flag(tension),
                "coverage_below": coverage_below, "sort": sort}),
    )?;
    let t = search_text(&v);
    Ok((v, t))
}

/// csrch1: the matching blocks, one header per block and its matching lines under it.
/// One entity's fact sheet in `rdy search` text (FW-EN1, FW-EN1a).
fn sheet_lines(sheet: &Value, out: &mut Vec<String>) {
    let mut head = format!(
        "entity {} ({}, {})",
        text(&sheet["name"]),
        text(&sheet["type"]),
        text(&sheet["id"])
    );
    if let Some(value) = sheet["value"].as_str() {
        head.push_str(&format!(" = {value}"));
    }
    if let Some(kind) = sheet["kind"].as_str() {
        head.push_str(&format!(" [{kind}]"));
    }
    head.push_str(&format!(", matched by {}", text(&sheet["matched_by"])));
    if let Some(level) = sheet["confidence"].as_str() {
        head.push_str(&format!("  confidence {level}"));
    }
    out.push(head);
    let first = text(&sheet["definition"]).lines().next().unwrap_or("");
    if !first.is_empty() {
        out.push(format!("  definition: {first}"));
    }
    for (key, title) in [
        ("facts", "facts"),
        ("also_described_in", "also described in"),
    ] {
        let list = sheet[key].as_array().cloned().unwrap_or_default();
        if !list.is_empty() {
            out.push(format!("  {title}:"));
            for b in &list {
                out.push(format!(
                    "    {} ({}) {}",
                    text(&b["id"]),
                    text(&b["via"]),
                    text(&b["summary"])
                ));
            }
        }
    }
    let decisions = sheet["decisions"].as_array().cloned().unwrap_or_default();
    if !decisions.is_empty() {
        out.push("  decisions:".to_string());
        for d in &decisions {
            out.push(format!(
                "    {} [{}] ({}) {}",
                text(&d["id"]),
                text(&d["status"]),
                text(&d["via"]),
                text(&d["statement"])
            ));
        }
    }
    let dependents = sheet["dependents"].as_array().cloned().unwrap_or_default();
    if !dependents.is_empty() {
        out.push("  depends on it:".to_string());
        for b in &dependents {
            out.push(format!(
                "    {} ({}) {}",
                text(&b["id"]),
                text(&b["via"]),
                text(&b["summary"])
            ));
        }
    }
    if let Some(omitted) = sheet["omitted"].as_object().filter(|o| !o.is_empty()) {
        out.push(format!(
            "  omitted: {}",
            omitted
                .iter()
                .map(|(k, n)| format!("{} {}", k.replace('_', " "), num(n)))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
}

/// vxs8 (D-76Y9H): what a like hit shares with its source, `words a, b; links X`; `None` for a
/// hit of the other modes, which carries neither list.
fn shared(m: &Value) -> Option<String> {
    let list = |k: &str| {
        m[k].as_array()
            .map(|a| a.iter().map(text).collect::<Vec<_>>().join(", "))
    };
    let (words, links) = (list("words"), list("links"));
    if words.is_none() && links.is_none() {
        return None;
    }
    let parts: Vec<String> = [("words", words), ("links", links)]
        .into_iter()
        .filter_map(|(k, l)| l.filter(|l| !l.is_empty()).map(|l| format!("{k} {l}")))
        .collect();
    Some(parts.join("; "))
}

/// csrch1: the hits, one header per block and its matching lines under it; vxs8: a ranked hit's
/// header ends with its score, and a like hit's next line names what it shares.
fn search_text(v: &Value) -> String {
    let mut out = vec![format!("{} matching blocks", num(&v["total"]))];
    let docs = v["totals"]["docs"].as_array().cloned().unwrap_or_default();
    if !docs.is_empty() {
        let mut line = format!(
            "by doc: {}",
            docs.iter()
                .map(|d| format!("{} {}", d["path"].as_str().unwrap_or("-"), num(&d["hits"])))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let more = num(&v["totals"]["more_docs"]);
        if more > 0 {
            line.push_str(&format!(", … +{more} more docs"));
        }
        out.push(line);
    }
    if let Some(types) = v["totals"]["types"].as_object().filter(|t| !t.is_empty()) {
        out.push(format!(
            "by type: {}",
            types
                .iter()
                .map(|(t, n)| format!("{t} {}", num(n)))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for sheet in v["entities"].as_array().into_iter().flatten() {
        sheet_lines(sheet, &mut out);
    }
    let more = num(&v["entities_omitted"]);
    if more > 0 {
        out.push(format!("and {more} more entities"));
    }
    for m in v["matches"].as_array().into_iter().flatten() {
        let path = m["path"].as_str().unwrap_or("-");
        let mut head = format!("{path}  {} ({})", text(&m["id"]), text(&m["type"]));
        if m["all_words"].as_bool() == Some(true) {
            head.push_str(" [all words]");
        }
        if m["history"].as_bool() == Some(true) {
            head.push_str(" [history]");
        }
        if let (Some(coverage), Some(level)) = (m["coverage"].as_u64(), m["confidence"].as_str()) {
            head.push_str(&format!(
                "  coverage {coverage}  issues {}  {level}",
                num(&m["issues"])
            ));
        }
        if let Some(score) = m["score"].as_f64() {
            head.push_str(&format!("  score={score:.3}"));
        }
        out.push(head);
        if let Some(shared) = shared(m) {
            out.push(format!("  shares: {shared}"));
        }
        for l in m["lines"].as_array().into_iter().flatten() {
            out.push(format!(
                "  {}:{}: {}",
                text(&l["field"]),
                num(&l["line"]),
                text(&l["text"])
            ));
        }
        let more = num(&m["more_lines"]);
        if more > 0 {
            out.push(format!("  … +{more} more lines"));
        }
        let also = m["also"].as_array().cloned().unwrap_or_default();
        if !also.is_empty() {
            out.push(format!(
                "  also: {}",
                also.iter()
                    .map(|a| {
                        let mut member =
                            format!("{} {}", a["path"].as_str().unwrap_or("-"), text(&a["id"]));
                        if let Some(score) = a["score"].as_f64() {
                            member.push_str(&format!(" score={score:.3}"));
                        }
                        if let Some(shared) = shared(a) {
                            member.push_str(&format!(" ({shared})"));
                        }
                        member
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        for s in m["suggestions"].as_array().into_iter().flatten() {
            out.push(format!(
                "  suggest: {} {}",
                text(&s["link"]),
                text(&s["target"])
            ));
        }
    }
    if let Some(next) = v["next"].as_str() {
        out.push(format!("next page: --page {next}"));
    }
    out.join("\n")
}
