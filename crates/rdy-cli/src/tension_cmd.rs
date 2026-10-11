//! mten1/cten1: `rdy tensions` and `rdy resolve`. Each builds the request for the ctl op of the
//! same name; the renderer turns the reply into text. `--json` prints the reply as it is.
use crate::conn::{Env, call};
use crate::output::{hash_short, num, text};
use rdy_daemon::ErrorReply;
use serde_json::{Value, json};

#[derive(clap::Args)]
pub(crate) struct TensionsArgs {
    /// inflight (default: every tension), canon (tensions whose sides are all canon) or a view.
    #[arg(long)]
    scope: Option<String>,
    /// Only this kind, e.g. premise_shift.
    #[arg(long)]
    kind: Option<String>,
    /// Only this severity: blocking or advisory.
    #[arg(long)]
    severity: Option<String>,
    /// open (default) or dismissed.
    #[arg(long)]
    status: Option<String>,
    /// Only tensions with a change side in this view.
    #[arg(long)]
    view: Option<String>,
    /// Only tensions that involve a block in this pack area.
    #[arg(long)]
    area: Option<String>,
    /// Only tensions that involve this block (id, key or path) or this change (hex prefix).
    #[arg(long)]
    involving: Option<String>,
    /// Also list the dismissal reviews.
    #[arg(long)]
    reviews: bool,
    /// The budget for the listing; tensions that do not fit are left out from the end.
    #[arg(long = "max-tokens")]
    max_tokens: Option<usize>,
}

#[derive(clap::Args)]
pub(crate) struct ResolveArgs {
    /// The tension: tn-<hex>, or a unique prefix of at least 6 hex digits.
    tension: Option<String>,
    /// Accept the reviewers' advice to block on this exact proposal version (P-x@n): every advised semantic tension of it, or those --tensions names.
    #[arg(long, value_name = "PROPOSAL", requires = "digest", conflicts_with_all = ["tension", "mention", "dismiss", "restore", "scope"])]
    accept: Option<String>,
    /// With --accept: the advised digest rdy propose or rdy show printed with the tensions the person was shown.
    #[arg(long, value_name = "DIGEST", requires = "accept")]
    digest: Option<String>,
    /// With --accept: only these tensions.
    #[arg(
        long,
        value_name = "TN,...",
        requires = "accept",
        value_delimiter = ','
    )]
    tensions: Option<Vec<String>>,
    /// The person's words, quoted, when an agent relays them. Requires --relayed-by.
    #[arg(long, value_name = "WORDS", requires_all = ["accept", "relayed_by"])]
    evidence: Option<String>,
    /// The agent that relays the person's answer. Requires --evidence.
    #[arg(long, value_name = "ACTOR[/RUN]", requires_all = ["accept", "evidence"])]
    relayed_by: Option<String>,
    /// The question the person was asked.
    #[arg(long, value_name = "TEXT", requires = "evidence")]
    question: Option<String>,
    /// Where the person said it (default: cli).
    #[arg(long = "session", value_name = "REF", requires = "evidence")]
    session_ref: Option<String>,
    /// A passing mention instead of a tension: the block and the entity it names.
    #[arg(long, num_args = 2, value_names = ["BLOCK", "ENTITY"], conflicts_with = "tension")]
    mention: Option<Vec<String>>,
    /// Dismiss the tension or the mention, and why.
    #[arg(long, conflicts_with = "restore")]
    dismiss: Option<String>,
    /// Restore a dismissed mention, and why.
    #[arg(long)]
    restore: Option<String>,
    /// Where the mention is checked (default: your view), e.g. view:work.
    #[arg(long, requires = "mention")]
    scope: Option<String>,
}

/// `<id>  <severity>  <kind>  <status>  <summary>`, then the sides, the proposal of a landing
/// form, and the dismissal.
fn tension_text(t: &Value) -> String {
    let mut out = format!(
        "{}  {}  {}  {}  {}",
        text(&t["id"]),
        text(&t["severity"]),
        text(&t["kind"]),
        text(&t["status"]),
        text(&t["summary"])
    );
    let sides: Vec<String> = t["sides"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| {
            let label = text(&s["side"]);
            let label = if s["change"].is_string() {
                hash_short(label)
            } else {
                label
            };
            let mut one = label.to_string();
            if let Some(a) = s["actor"].as_str() {
                one.push_str(&format!(" ({a})"));
            }
            one
        })
        .collect();
    if !sides.is_empty() {
        out.push_str(&format!("\n  sides: {}", sides.join("; ")));
    }
    if let Some(p) = t["proposal"].as_str() {
        out.push_str(&format!(
            "\n  landing form of {p} (not in flight; the gate judges it)"
        ));
    }
    if t["dismissal"].is_object() {
        out.push_str(&format!(
            "\n  dismissed by {}: {}",
            text(&t["dismissal"]["actor"]),
            text(&t["dismissal"]["reason"])
        ));
    }
    out.push_str(&judged_text(&t["judged"]));
    out
}

/// A semantic tension's verdict: who judged it and the first finding, the advice, and who
/// accepted it. Empty for a tension with no verdict.
fn judged_text(j: &Value) -> String {
    if !j.is_object() {
        return String::new();
    }
    let findings = j["findings"].as_array().map_or(&[][..], Vec::as_slice);
    let mut out = format!("\n  judged by {}", text(&j["actor"]));
    if let Some(f) = findings.first() {
        out.push_str(&format!(": {}", text(&f["note"])));
    }
    if findings.len() > 1 {
        out.push_str(&format!(" (+{} more)", findings.len() - 1));
    }
    if let Some(advice) = j["advice"].as_str() {
        out.push_str(&format!("\n  advises blocking: {advice}"));
    }
    let a = &j["accepted"];
    if a.is_object() {
        out.push_str(&format!("\n  accepted by {}", text(&a["actor"])));
        if let Some(by) = a["relayed_by"].as_str() {
            out.push_str(&format!(" (relayed by {by})"));
        }
        if let Some(words) = a["evidence"].as_str() {
            out.push_str(&format!(": \"{words}\""));
        }
    }
    out
}

/// `<review 12>  <tension 12>  <kind>  <actor>  holds|lapsed: <reason>`.
fn review_text(r: &Value) -> String {
    format!(
        "review {}  {}  {}  {}  {}: {}",
        hash_short(text(&r["review"])),
        hash_short(text(&r["tension"])),
        text(&r["kind"]),
        text(&r["actor"]),
        if r["holds"].as_bool() == Some(true) {
            "holds"
        } else {
            "lapsed"
        },
        text(&r["reason"])
    )
}

/// The text of a `tensions` reply: one entry per tension, the reviews, then the counts.
fn tensions_render(v: &Value) -> String {
    let rows = v["tensions"].as_array().map_or(&[][..], Vec::as_slice);
    let omitted = v["omitted"].as_u64().unwrap_or(0);
    let mut lines: Vec<String> = if rows.is_empty() && omitted == 0 {
        vec!["no tensions".to_string()]
    } else {
        rows.iter().map(tension_text).collect()
    };
    if omitted > 0 {
        lines.push(format!(
            "{omitted} more not listed (pass --max-tokens for more)"
        ));
    }
    for r in v["reviews"].as_array().into_iter().flatten() {
        lines.push(review_text(r));
    }
    if let Some(n) = v["reviews_omitted"].as_u64().filter(|n| *n > 0) {
        lines.push(format!(
            "{n} reviews not listed (pass --max-tokens for more)"
        ));
    }
    let c = &v["counts"];
    lines.push(format!(
        "{} open ({} blocking), {} dismissed",
        num(&c["open"]),
        num(&c["blocking_open"]),
        num(&c["dismissed"])
    ));
    lines.join("\n")
}

pub(crate) fn tensions(env: &Env, a: TensionsArgs) -> Result<(Value, String), ErrorReply> {
    let TensionsArgs {
        scope,
        kind,
        severity,
        status,
        view,
        area,
        involving,
        reviews,
        max_tokens,
    } = a;
    let v = call(
        env,
        "tensions",
        json!({
            "scope": scope,
            "kind": kind,
            "severity": severity,
            "status": status,
            "view": view,
            "area": area,
            "involving": involving,
            "reviews": reviews.then_some(true),
            "max_tokens": max_tokens,
        }),
    )?;
    let t = tensions_render(&v);
    Ok((v, t))
}

/// The text of an acceptance: one line per accepted tension, then one per tension that now
/// blocks in a view (other views' forms of the same two texts included).
fn accept_text(v: &Value) -> String {
    let accepted = v["accepted"].as_array().into_iter().flatten().map(|r| {
        format!(
            "accepted {} (was {}); it blocks until a side's content changes",
            text(&r["id"]),
            text(&r["was"])
        )
    });
    let blocking = v["now_blocking"].as_array().into_iter().flatten().map(|r| {
        let proposal = r["proposal"]
            .as_str()
            .map(|p| format!(" ({p})"))
            .unwrap_or_default();
        format!(
            "now blocking in view {}{proposal}: {}",
            text(&r["view"]),
            text(&r["tension"])
        )
    });
    accepted.chain(blocking).collect::<Vec<_>>().join("\n")
}

pub(crate) fn resolve(env: &Env, a: ResolveArgs) -> Result<(Value, String), ErrorReply> {
    let ResolveArgs {
        tension,
        accept,
        digest,
        tensions,
        evidence,
        relayed_by,
        question,
        session_ref,
        mention,
        dismiss,
        restore,
        scope,
    } = a;
    if let Some(proposal) = accept {
        let v = call(
            env,
            "resolve",
            json!({
                "accept": {
                    "proposal": proposal,
                    "advised_digest": digest.unwrap_or_default(),
                    "tensions": tensions.unwrap_or_default(),
                    "evidence": evidence,
                    "question": question,
                    "session_ref": session_ref,
                    "relayed_by": relayed_by,
                },
            }),
        )?;
        let t = accept_text(&v);
        return Ok((v, t));
    }
    let mention = mention.map(|m| json!({"block": m[0], "entity": m[1]}));
    let reason = |r: Option<String>| r.map(|r| json!({"reason": r}));
    let v = call(
        env,
        "resolve",
        json!({
            "tension": tension,
            "mention": mention,
            "dismiss": reason(dismiss),
            "restore": reason(restore),
            "scope": scope,
        }),
    )?;
    let t = if v["entity"].is_string() {
        let did = if text(&v["verdict"]) == "restore" {
            "restored"
        } else {
            "dismissed"
        };
        format!(
            "{did} the mention of {} in {} (review {})",
            text(&v["entity"]),
            text(&v["block"]),
            hash_short(text(&v["review"]))
        )
    } else {
        format!(
            "dismissed {} (review {})",
            text(&v["id"]),
            hash_short(text(&v["review"]))
        )
    };
    Ok((v, t))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// FR1 (ruling 2): the acceptance text names each accepted tension, then every tension
    /// that now blocks, with its view and the view's open proposal, other views' forms included.
    #[test]
    fn se8_accept_text_names_every_tension_that_now_blocks() {
        let v = json!({
            "proposal": "P-1@1", "severity": "blocking",
            "accepted": [{"id": "tn-111111111111", "was": "tn-000000000000", "version": "ab"}],
            "now_blocking": [
                {"tension": "tn-111111111111", "view": "a", "proposal": "P-1@1"},
                {"tension": "tn-222222222222", "view": "b"}
            ]
        });
        assert_eq!(
            accept_text(&v),
            "accepted tn-111111111111 (was tn-000000000000); it blocks until a side's content changes\nnow blocking in view a (P-1@1): tn-111111111111\nnow blocking in view b: tn-222222222222"
        );
    }

    #[test]
    fn cten1_a_landing_form_names_its_proposal() {
        let t = json!({"id": "tn-0123456789ab", "severity": "blocking", "kind": "duplicate_definition",
            "status": "open", "summary": "t-A and t-B both define \"fee\"", "sides": [],
            "proposal": "P-abcdef012345@1"});
        assert_eq!(
            tension_text(&t),
            "tn-0123456789ab  blocking  duplicate_definition  open  t-A and t-B both define \"fee\"\n  landing form of P-abcdef012345@1 (not in flight; the gate judges it)"
        );
    }

    /// D-WMCFW: a judged tension names its reviewer and first finding, the advice, and who
    /// accepted it, relayed by whom, in which words.
    #[test]
    fn se8_a_judged_tension_names_the_verdict_the_advice_and_the_acceptance() {
        let t = json!({"id": "tn-0123456789ab", "severity": "blocking", "kind": "semantic",
            "status": "open", "summary": "a reviewer judged that c-A and c-B disagree", "sides": [],
            "judged": {"review": "ab", "actor": "agent:r", "advice": "it contradicts the gate rule",
                "findings": [{"block": "c-A", "note": "A and B cannot both hold"}, {"block": "c-B", "note": "n"}],
                "accepted": {"actor": "human:qa", "relayed_by": "agent:w/7", "evidence": "yes", "at": "t"}}});
        assert_eq!(
            tension_text(&t),
            "tn-0123456789ab  blocking  semantic  open  a reviewer judged that c-A and c-B disagree\n  judged by agent:r: A and B cannot both hold (+1 more)\n  advises blocking: it contradicts the gate rule\n  accepted by human:qa (relayed by agent:w/7): \"yes\""
        );
    }
}
