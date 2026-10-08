//! The token budget: counting, cutting text and fitting a reply into `max_tokens`.

use serde_json::{Value, json};

/// The tokens `v` costs as JSON (4 bytes a token).
pub(super) fn tokens_of(v: &Value) -> usize {
    serde_json::to_string(v).map(|s| s.len() / 4).unwrap_or(0)
}

/// Cuts the string at `k` until `obj` fits `max` tokens (or the string is empty), and marks it
/// `truncated`; `mark` records how many bytes were cut (it runs before each fit check).
pub(super) fn cut_text(
    obj: &mut serde_json::Map<String, Value>,
    k: &str,
    max: usize,
    mark: impl Fn(&mut serde_json::Map<String, Value>, usize),
) {
    let text = obj[k].as_str().unwrap_or_default().to_string();
    let mut keep = (max * 4).saturating_sub(200).min(text.len());
    loop {
        let cut = (0..=keep)
            .rev()
            .find(|i| text.is_char_boundary(*i))
            .unwrap_or(0);
        obj.insert(k.to_string(), json!(text[..cut]));
        obj.insert("truncated".into(), json!(true));
        mark(obj, text.len() - cut);
        if tokens_of(&Value::Object(obj.clone())) <= max || cut == 0 {
            break;
        }
        keep = cut.saturating_sub((cut / 8).max(1));
    }
}

pub fn budget(v: Value, max_tokens: Option<usize>) -> Value {
    let max = max_tokens.unwrap_or(4000);
    if tokens_of(&v) <= max {
        return v;
    }
    let mut v = match v {
        Value::Array(a) => json!({ "items": a }),
        other => other,
    };
    let Some(obj) = v.as_object_mut() else {
        return v;
    };
    let key = obj
        .iter()
        .filter_map(|(k, x)| {
            x.as_array().map(|a| {
                (
                    serde_json::to_string(a).map(|s| s.len()).unwrap_or(0),
                    k.clone(),
                )
            })
        })
        .max()
        .map(|(_, k)| k);
    let Some(key) = key else {
        // no list to shorten: cut the longest string instead (e.g. rdy_read's text)
        let longest = obj
            .iter()
            .filter_map(|(k, x)| x.as_str().map(|s| (s.len(), k.clone())))
            .max()
            .map(|(_, k)| k);
        if let Some(k) = longest {
            cut_text(obj, &k, max, |o, n| {
                o.insert("omitted".into(), json!(n));
            });
        }
        return v;
    };
    let full = obj[&key].as_array().cloned().unwrap_or_default();
    let (mut lo, mut hi) = (0usize, full.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        obj.insert(key.clone(), Value::Array(full[..mid].to_vec()));
        obj.insert("truncated".into(), json!(true));
        obj.insert("omitted".into(), json!(full.len() - mid));
        if tokens_of(&Value::Object(obj.clone())) <= max {
            lo = mid
        } else {
            hi = mid - 1
        }
    }
    obj.insert(key, Value::Array(full[..lo].to_vec()));
    obj.insert("truncated".into(), json!(true));
    obj.insert("omitted".into(), json!(full.len() - lo));
    v
}
