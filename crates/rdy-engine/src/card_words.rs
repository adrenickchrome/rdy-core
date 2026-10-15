//! The authored words of a decision or question card (FW-I1, D-JV6Z8): a plain title, a one-line
//! why, the option labels and the recommendation's reason. They are block fields, written when
//! the item is raised. This lint checks that they are present and short. Every issue is a
//! warning: it never refuses a write.

use rdy_model::{BlockId, FieldValue, decision};
use rdy_schema::{Issue, Severity};
use std::collections::BTreeMap;

/// The fields of one block.
pub type Fields = BTreeMap<String, FieldValue>;

/// The most characters a card title holds.
pub const TITLE_MAX: usize = 80;
/// The most characters a card's one-line why holds.
pub const WHY_MAX: usize = 160;
/// The most characters one option label holds.
pub const LABEL_MAX: usize = 60;
/// The most characters a recommendation's reason holds.
pub const REASON_MAX: usize = 200;

/// The code of every card-words issue.
pub const W_CARD_WORDS: &str = "W_CARD_WORDS";

fn warn(id: &BlockId, message: String, fix: String) -> Issue {
    Issue {
        severity: Severity::Warning,
        code: W_CARD_WORDS,
        block: Some(id.clone()),
        message,
        fix,
    }
}

fn text<'a>(fields: &'a Fields, name: &str) -> Option<&'a str> {
    fields
        .get(name)
        .and_then(FieldValue::as_str)
        .filter(|s| !s.trim().is_empty())
}

/// The card-words issues of block `id`, a `block_type` with `fields`; empty for any type but a
/// decision or a question. The list is closed: no other case is an issue.
pub fn card_words_issues(id: &BlockId, block_type: &str, fields: &Fields) -> Vec<Issue> {
    let question = block_type == decision::QUESTION;
    if !question && block_type != decision::DECISION {
        return vec![];
    }
    let mut out = vec![];
    match text(fields, "title") {
        None => out.push(warn(
            id,
            format!("{id} has no card title"),
            format!("add a plain-words title of {TITLE_MAX} characters or fewer"),
        )),
        Some(t) => too_long(id, "title", t, TITLE_MAX, &mut out),
    }
    match text(fields, "why") {
        None => out.push(warn(
            id,
            format!("{id} has no card why"),
            "add one line on why a person must answer it".to_string(),
        )),
        Some(t) => too_long(id, "why", t, WHY_MAX, &mut out),
    }
    let recommended;
    if question {
        let options: Vec<&str> = match fields.get("options") {
            Some(FieldValue::List(items)) => items.iter().filter_map(FieldValue::as_str).collect(),
            _ => vec![],
        };
        if fields.contains_key("options") && !(2..=4).contains(&options.len()) {
            out.push(warn(
                id,
                format!("{id} has {} options", options.len()),
                "give two to four options".to_string(),
            ));
        }
        for o in &options {
            too_long(id, "options", o, LABEL_MAX, &mut out);
        }
        recommended = text(fields, "recommended");
        if let Some(r) = recommended
            && !options.contains(&r)
        {
            out.push(warn(
                id,
                format!("{id} recommends an option it does not list"),
                "set recommended to one of the options".to_string(),
            ));
        }
    } else {
        recommended = text(fields, "recommend");
    }
    match text(fields, "recommend_reason") {
        None if recommended.is_some() => out.push(warn(
            id,
            format!("{id} recommends without a reason"),
            "add recommend_reason in one line".to_string(),
        )),
        None => {}
        Some(t) => too_long(id, "recommend_reason", t, REASON_MAX, &mut out),
    }
    out
}

fn too_long(id: &BlockId, field: &str, value: &str, max: usize, out: &mut Vec<Issue>) {
    if value.chars().count() > max {
        out.push(warn(
            id,
            format!("{id}: {field} is longer than {max} characters"),
            format!("shorten {field} to {max} characters or fewer"),
        ));
    }
}
