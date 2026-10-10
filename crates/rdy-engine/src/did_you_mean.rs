//! FW-HD1: the did-you-mean for an unknown field. One closed rule (`nearest`) over a closed list
//! of known names, and the parse of serde's unknown-field errors into rdy's refusal.
use crate::error::{E_BAD_INPUT, RdyError};

/// The Levenshtein distance between `a` and `b`, over chars.
pub fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != *cb)).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

/// The closed rule: the entry of `known` within 2 edits of `word` (smallest distance first, then
/// the first in `known`); else the first entry where one string is a prefix of the other and the
/// shorter has at least 3 characters; else nothing.
pub fn nearest<'a>(word: &str, known: &[&'a str]) -> Option<&'a str> {
    let close = known
        .iter()
        .enumerate()
        .map(|(i, k)| (edit_distance(word, k), i, *k))
        .filter(|(d, _, _)| *d <= 2)
        .min()
        .map(|(_, _, k)| k);
    close.or_else(|| {
        known.iter().copied().find(|k| {
            let (short, long) = if k.chars().count() <= word.chars().count() {
                (*k, word)
            } else {
                (word, *k)
            };
            short.chars().count() >= 3 && long.starts_with(short)
        })
    })
}

/// The names between backticks in `s`, in order.
fn ticked(s: &str) -> Option<Vec<String>> {
    let parts: Vec<&str> = s.split('`').collect();
    if parts.len() < 3 || parts.len().is_multiple_of(2) {
        return None;
    }
    Some(
        parts
            .iter()
            .skip(1)
            .step_by(2)
            .map(|p| p.to_string())
            .collect(),
    )
}

/// The unknown field and serde's expected list, from serde's unknown-field error text:
/// ``unknown field `x`, expected one of `a`, `b` ``, `` … expected `a` or `b` ``,
/// `` … expected `a` ``, or `` … there are no fields ``, with a trailing
/// ` at line L column C` removed. `None` for any other text.
pub fn unknown_field(serde_msg: &str) -> Option<(String, Vec<String>)> {
    let msg = strip_position(serde_msg);
    let rest = msg.strip_prefix("unknown field `")?;
    let (field, rest) = rest.split_once("`, ")?;
    if rest == "there are no fields" {
        return Some((field.to_string(), vec![]));
    }
    let expected = rest
        .strip_prefix("expected one of ")
        .or_else(|| rest.strip_prefix("expected "))?;
    let names = ticked(expected)?;
    // the separators between the names are ", " (one of) or " or " (two)
    let shape: String = expected.split('`').step_by(2).collect::<Vec<_>>().join("|");
    let ok = match names.len() {
        1 => shape == "|",
        2 => shape == "| or |" || shape == "|, |",
        n => shape == format!("|{}|", vec![", "; n - 1].join("|")),
    };
    ok.then(|| (field.to_string(), names))
}

/// `msg` without serde_json's trailing ` at line L column C`.
fn strip_position(msg: &str) -> &str {
    let Some(at) = msg.rfind(" at line ") else {
        return msg;
    };
    let tail = &msg[at + " at line ".len()..];
    let Some((line, col)) = tail.split_once(" column ") else {
        return msg;
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if digits(line) && digits(col) {
        &msg[..at]
    } else {
        msg
    }
}

/// The fix for unknown field `field` of a struct whose fields are `known`.
pub fn fields_fix(field: &str, known: &[&str]) -> String {
    if known.is_empty() {
        return format!("remove \"{field}\"; it takes no fields");
    }
    let all = known.join(", ");
    match nearest(field, known) {
        Some(y) => format!("did you mean \"{y}\"? the fields are {all}"),
        None => format!("remove \"{field}\"; the fields are {all}"),
    }
}

/// `E_BAD_INPUT` for serde's unknown-field error `serde_msg`, named after `context`; `None` when
/// `serde_msg` is no unknown-field error.
pub fn unknown_field_error(context: &str, serde_msg: &str) -> Option<RdyError> {
    unknown_field_error_in(context, serde_msg, &serde_json::Value::Null)
}

/// Where unknown field `field` sits in `input`, as a path from its top (`parent_id`,
/// `move_to.parent_id`, `members[0].viw`): the first object, outer objects first, that holds it
/// and whose other keys are all among `known` (or the `op` tag); else the first object that
/// holds it. The open maps (`fields`, `links`, `pins`) are not searched. `None` when no object
/// holds it.
pub fn field_path(input: &serde_json::Value, field: &str, known: &[&str]) -> Option<String> {
    use serde_json::Value;
    let fits = |o: &serde_json::Map<String, Value>| {
        o.contains_key(field)
            && o.keys()
                .all(|k| k == field || k == "op" || known.contains(&k.as_str()))
    };
    let mut level: Vec<(String, &Value)> = vec![(String::new(), input)];
    let mut first_holder: Option<String> = None;
    while !level.is_empty() {
        let mut next = vec![];
        for (at, v) in level {
            let join = |k: &str| {
                if at.is_empty() {
                    k.to_string()
                } else {
                    format!("{at}.{k}")
                }
            };
            match v {
                Value::Object(o) => {
                    if o.contains_key(field) {
                        if fits(o) {
                            return Some(join(field));
                        }
                        first_holder.get_or_insert_with(|| join(field));
                    }
                    for (k, child) in o {
                        if !["fields", "links", "pins"].contains(&k.as_str()) {
                            next.push((join(k), child));
                        }
                    }
                }
                Value::Array(items) => {
                    for (i, child) in items.iter().enumerate() {
                        next.push((format!("{at}[{i}]"), child));
                    }
                }
                _ => {}
            }
        }
        level = next;
    }
    first_holder
}

/// `unknown_field_error` with the field named by its path in `input` (see `field_path`): an
/// unknown field in a nested object reads `move_to.parent_id`, not `parent_id`.
pub fn unknown_field_error_in(
    context: &str,
    serde_msg: &str,
    input: &serde_json::Value,
) -> Option<RdyError> {
    let (field, expected) = unknown_field(serde_msg)?;
    let known: Vec<&str> = expected.iter().map(String::as_str).collect();
    let shown = field_path(input, &field, &known).unwrap_or_else(|| field.clone());
    Some(RdyError::new(
        E_BAD_INPUT,
        format!("{context}: unknown field \"{shown}\""),
        fields_fix(&field, &known),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    fn own(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn hd3_field_path_names_the_object_that_holds_the_field() {
        let v = serde_json::json!({"op": "update", "id": "x", "move_to": {"parent_id": "p"}});
        assert_eq!(
            field_path(&v, "parent_id", &["parent", "after", "before"]).as_deref(),
            Some("move_to.parent_id")
        );
        let v = serde_json::json!({"op": "create", "members": [
            {"label": "a", "view": "v"}, {"label": "b", "viw": 1}
        ]});
        assert_eq!(
            field_path(&v, "viw", &["label", "view"]).as_deref(),
            Some("members[1].viw")
        );
        let v = serde_json::json!({"op": "update", "id": "x", "parent_id": "p",
            "fields": {"parent_id": 1}});
        assert_eq!(
            field_path(&v, "parent_id", &["id", "base", "fields"]).as_deref(),
            Some("parent_id")
        );
        assert_eq!(field_path(&v, "nowhere", &["id"]), None);
    }

    #[test]
    fn hd3_the_four_serde_forms_parse() {
        assert_eq!(
            unknown_field("unknown field `x`, expected one of `a`, `b`, `c`"),
            Some(("x".into(), own(&["a", "b", "c"])))
        );
        assert_eq!(
            unknown_field("unknown field `x`, expected `a` or `b`"),
            Some(("x".into(), own(&["a", "b"])))
        );
        assert_eq!(
            unknown_field("unknown field `x`, expected `a`"),
            Some(("x".into(), own(&["a"])))
        );
        assert_eq!(
            unknown_field("unknown field `x`, there are no fields"),
            Some(("x".into(), vec![]))
        );
    }

    #[test]
    fn hd3_the_position_suffix_is_removed() {
        assert_eq!(
            unknown_field(
                "unknown field `arg`, expected one of `id`, `op`, `args` at line 1 column 25"
            ),
            Some(("arg".into(), own(&["id", "op", "args"])))
        );
        assert_eq!(
            unknown_field("unknown field `x`, there are no fields at line 3 column 1"),
            Some(("x".into(), vec![]))
        );
    }

    #[test]
    fn hd3_other_text_is_none() {
        assert_eq!(unknown_field("missing field `id`"), None);
        assert_eq!(
            unknown_field("invalid type: string \"x\", expected a map"),
            None
        );
        assert_eq!(
            unknown_field(
                "refines: unknown field \"prat\" in a link; a link takes target and part"
            ),
            None
        );
        assert_eq!(
            unknown_field_error("op 0", "missing field `id`").map(|e| e.message),
            None
        );
    }

    #[test]
    fn hd3_real_serde_errors_parse() {
        #[derive(Debug, Deserialize)]
        #[serde(deny_unknown_fields)]
        #[allow(dead_code)]
        struct Three {
            a: Option<u8>,
            bb: Option<u8>,
            ccc: Option<u8>,
        }
        #[derive(Debug, Deserialize)]
        #[serde(deny_unknown_fields)]
        #[allow(dead_code)]
        struct Two {
            a: Option<u8>,
            bb: Option<u8>,
        }
        #[derive(Debug, Deserialize)]
        #[serde(deny_unknown_fields)]
        #[allow(dead_code)]
        struct One {
            a: Option<u8>,
        }
        #[derive(Debug, Deserialize)]
        #[serde(deny_unknown_fields)]
        struct None0 {}
        let msg = |r: Result<(), serde_json::Error>| r.unwrap_err().to_string();
        let e = msg(serde_json::from_str::<Three>(r#"{"cc":1}"#).map(|_| ()));
        assert_eq!(
            unknown_field(&e),
            Some(("cc".into(), own(&["a", "bb", "ccc"]))),
            "{e}"
        );
        let e = msg(serde_json::from_str::<Two>(r#"{"x":1}"#).map(|_| ()));
        assert_eq!(
            unknown_field(&e),
            Some(("x".into(), own(&["a", "bb"]))),
            "{e}"
        );
        let e = msg(serde_json::from_str::<One>(r#"{"x":1}"#).map(|_| ()));
        assert_eq!(unknown_field(&e), Some(("x".into(), own(&["a"]))), "{e}");
        let e = msg(serde_json::from_str::<None0>(r#"{"x":1}"#).map(|_| ()));
        assert_eq!(unknown_field(&e), Some(("x".into(), vec![])), "{e}");
    }

    #[test]
    fn hd3_fields_fix_has_three_forms() {
        assert_eq!(
            fields_fix("bse", &["id", "base"]),
            "did you mean \"base\"? the fields are id, base"
        );
        assert_eq!(
            fields_fix("zzz", &["id", "base"]),
            "remove \"zzz\"; the fields are id, base"
        );
        assert_eq!(fields_fix("zzz", &[]), "remove \"zzz\"; it takes no fields");
    }

    #[test]
    fn hd3_nearest_takes_the_smallest_distance_then_the_first() {
        assert_eq!(nearest("ab", &["abcd", "abc"]), Some("abc"));
        assert_eq!(nearest("ax", &["ay", "az"]), Some("ay"));
        assert_eq!(edit_distance("parent_id", "parent"), 3);
    }
}
