//! The local approval pages' requests (D-6M0AF, D-94BT2; TASK-982 O7, C5): which requests are UI
//! routes, the checks they pass before any handler, and the one answer form.
//!
//! UI routes exist only on a full listener whose bound local address is loopback, while
//! `serve.ui` is on. The listener comes from the connection's local address, never from a Host
//! header. Anywhere else, [`classify`] returns `None`, and the ordinary guard answers the path:
//! its themed not-found page for `GET` and `HEAD`, its `405` for `POST`, and `Bad request.` for a
//! UI event request. The checks of an eligible route run in this order: one allowed Host, the
//! route's method, the body framing and its limit, the `POST` Origin, then the form's content
//! type. The body is read and decoded after them ([`parse_form`]).
//!
//! The types here know nothing of the handler that answers a route.

use crate::site::guard::{
    Refusal, clean_segment, is_rev, no_body, one_host, own_origin, own_origins, percent_decode,
    query_decode, split_target,
};
use crate::site::http::{Method, Request, Response, text};
use rdy_engine::did_you_mean::nearest;
use rdy_engine::{E_BAD_INPUT, RdyError};
use std::net::SocketAddr;

/// The most bytes of an answer form's body (TASK-982 O7: 4 MiB).
pub const MAX_UI_BODY: usize = 4_194_304;

/// The `Allow` of a UI page route: `GET` and `HEAD`.
pub const UI_GET: &str = "GET, HEAD";
/// The `Allow` of the answer route: `POST` only.
pub const UI_POST: &str = "POST";

/// The keys of the answer form, in bytewise order (the order the fix lists them in).
pub const FORM_KEYS: [&str; 5] = ["brief", "card_hash", "note", "option", "ref"];

/// The one media type of the answer form, in lower case, with and without its charset.
const FORM_TYPES: [&str; 2] = [
    "application/x-www-form-urlencoded",
    "application/x-www-form-urlencoded; charset=utf-8",
];

/// A UI route.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    /// `/~ui/`: the person's pending cards.
    Pending,
    /// `/~ui/item/<ref>`: one card, by its ref (one path segment, decoded once).
    Item(String),
    /// `POST /~ui/answer`: the answer form.
    Answer,
    /// `/_rdy/events?ui=1&rev=<16 lowercase hex>`: the pending pages' event stream.
    Events { rev: String },
}

/// The answer form: the item's ref, the card hash shown, the option chosen, and the optional
/// note and briefing token. An empty note or brief is absent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Form {
    pub reference: String,
    pub card_hash: String,
    pub option: String,
    pub note: Option<String>,
    pub brief: Option<String>,
}

/// What a UI route path is, before any check.
enum Path {
    Page(Route),
    Answer,
    /// An event request that names `ui`, with its query.
    Events(String),
}

/// The UI route of `req`, after its checks, on a listener bound to `local`: `full` when the
/// listener serves the full site, `ui` when `serve.ui` is on. `None` when the path is no UI
/// route, or the listener has none (not full, not loopback, or `serve.ui` off): the ordinary
/// guard then answers the request.
pub fn classify(
    req: &Request,
    local: SocketAddr,
    full: bool,
    ui: bool,
) -> Result<Option<Route>, Refusal> {
    if !(full && ui && local.ip().is_loopback()) {
        return Ok(None);
    }
    let Some(path) = ui_path(&req.target) else {
        return Ok(None);
    };
    // exactly one Host, a loopback spelling of this listener's port
    let host = one_host(req, local.port(), None)?;
    match path {
        Path::Answer => {
            if !matches!(&req.method, Method::Other(m) if m == "POST") {
                return Err(Refusal::UiMethod { allow: UI_POST });
            }
            content_length(req)?;
            match req.all("origin").as_slice() {
                [one] if own_origin(one, host) => {}
                _ => return Err(Refusal::Origin),
            }
            form_type(req)?;
            Ok(Some(Route::Answer))
        }
        Path::Page(route) => {
            get_route(req, host)?;
            Ok(Some(route))
        }
        Path::Events(query) => {
            get_route(req, host)?;
            Ok(Some(Route::Events {
                rev: ui_events(&query)?,
            }))
        }
    }
}

/// The method, body and Origin rules of a UI page route: `GET` or `HEAD`, no body, and no Origin
/// or this site's own.
fn get_route(req: &Request, host: &str) -> Result<(), Refusal> {
    if !matches!(req.method, Method::Get | Method::Head) {
        return Err(Refusal::UiMethod { allow: UI_GET });
    }
    no_body(req)?;
    own_origins(req, host)
}

/// The UI path `target` names, by the guard's one percent decode and segment rules; `None` for
/// any other path, `/~ui` and every malformed or extra UI segment among them.
fn ui_path(target: &str) -> Option<Path> {
    let (segments, query) = split_target(target).ok()?;
    let parts: Vec<&[u8]> = segments.iter().map(Vec::as_slice).collect();
    match parts.as_slice() {
        [b"~ui", b""] => Some(Path::Page(Route::Pending)),
        [b"~ui", b"answer"] => Some(Path::Answer),
        [b"~ui", b"item", item] => {
            let item = clean_segment(item).filter(|s| !s.is_empty())?;
            Some(Path::Page(Route::Item(item.to_string())))
        }
        [b"_rdy", b"events"] => {
            let query = query?;
            let names_ui = query
                .split('&')
                .any(|pair| pair.split_once('=').map_or(pair, |(k, _)| k) == "ui");
            names_ui.then(|| Path::Events(query.to_string()))
        }
        _ => None,
    }
}

/// The revision of a UI event query: exactly the keys `ui` and `rev`, once each, with `ui=1`
/// and a revision of 16 lowercase hex digits. Anything else is malformed.
fn ui_events(query: &str) -> Result<String, Refusal> {
    let (mut ui, mut rev) = (None, None);
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').ok_or(Refusal::Malformed)?;
        let slot = match key {
            "ui" => &mut ui,
            "rev" => &mut rev,
            _ => return Err(Refusal::Malformed),
        };
        let value = query_decode(value).ok_or(Refusal::Malformed)?;
        if slot.replace(value).is_some() {
            return Err(Refusal::Malformed);
        }
    }
    match (ui.as_deref(), rev) {
        (Some("1"), Some(rev)) if is_rev(&rev) => Ok(rev),
        _ => Err(Refusal::Malformed),
    }
}

/// The body length of a UI `POST`: exactly one `Content-Length` of decimal digits only, at most
/// [`MAX_UI_BODY`], and no `Transfer-Encoding`. A length past the limit is refused from its
/// digits, before any allocation; a duplicate length is malformed even when equal.
pub fn content_length(req: &Request) -> Result<usize, Refusal> {
    if !req.all("transfer-encoding").is_empty() {
        return Err(Refusal::Malformed);
    }
    let lengths = req.all("content-length");
    let [value] = lengths.as_slice() else {
        return Err(Refusal::Malformed);
    };
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Refusal::Malformed);
    }
    let digits = value.trim_start_matches('0');
    // 4194304 has 7 digits: a longer number is past the limit, however long it is
    if digits.len() > 7 {
        return Err(Refusal::UiBodyTooLarge);
    }
    let n = if digits.is_empty() {
        0
    } else {
        digits.parse::<usize>().map_err(|_| Refusal::Malformed)?
    };
    if n > MAX_UI_BODY {
        return Err(Refusal::UiBodyTooLarge);
    }
    Ok(n)
}

/// Exactly one `Content-Type`: the form's media type, optionally with `; charset=UTF-8`, in any
/// case.
fn form_type(req: &Request) -> Result<(), Refusal> {
    match req.all("content-type").as_slice() {
        [one] if FORM_TYPES.contains(&one.to_ascii_lowercase().as_str()) => Ok(()),
        _ => Err(Refusal::UiContentType),
    }
}

/// The fix of every form refusal but an unknown key.
const FORM_RETRY: &str = "reload the card and choose an option";

/// `E_BAD_INPUT`: duplicate, malformed or invalid UTF-8 form data.
fn invalid() -> RdyError {
    RdyError::new(E_BAD_INPUT, "the answer form is not valid", FORM_RETRY)
}

/// `E_BAD_INPUT`: a required field missing or empty.
fn needs(field: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("the answer form needs {field}"),
        FORM_RETRY,
    )
}

/// `E_BAD_INPUT`: a key the form does not have, with the nearest key when one is close.
fn unknown(key: &str) -> RdyError {
    let all = FORM_KEYS.join(", ");
    let fix = match nearest(key, &FORM_KEYS) {
        Some(near) => format!("did you mean \"{near}\"? the parameters here are {all}"),
        None => format!("remove \"{key}\"; the parameters here are {all}"),
    };
    RdyError::new(
        E_BAD_INPUT,
        format!("answer form: unknown parameter \"{key}\""),
        fix,
    )
}

/// `raw` decoded once (`%XX` escapes, `+` as a space) as UTF-8; `None` for a malformed escape or
/// bytes that are not UTF-8.
fn decode(raw: &[u8]) -> Option<String> {
    String::from_utf8(percent_decode(raw, true)?).ok()
}

/// The answer form of `body` (`application/x-www-form-urlencoded`): `ref`, `card_hash` and
/// `option`, each nonempty, and the optional `note` and `brief`; each key at most once, no other
/// key. At most five entries are inspected before a value is decoded, so a sixth entry is refused
/// without a map of them. The values are kept as they are: the engine validates the ref, the
/// card hash, the option, the note and the briefing token.
pub fn parse_form(body: &[u8]) -> Result<Form, RdyError> {
    // the raw value of each key, by its place in FORM_KEYS
    let mut raw: [Option<&[u8]>; 5] = [None; 5];
    if !body.is_empty() {
        for (i, entry) in body.split(|b| *b == b'&').enumerate() {
            if i == FORM_KEYS.len() {
                return Err(invalid());
            }
            let at = entry.iter().position(|b| *b == b'=').ok_or_else(invalid)?;
            let key = decode(&entry[..at])
                .filter(|k| !k.is_empty())
                .ok_or_else(invalid)?;
            let Some(slot) = FORM_KEYS.iter().position(|k| *k == key) else {
                return Err(unknown(&key));
            };
            if raw[slot].replace(&entry[at + 1..]).is_some() {
                return Err(invalid());
            }
        }
    }
    let mut values: [Option<String>; 5] = Default::default();
    for (slot, value) in raw.iter().enumerate() {
        if let Some(v) = value {
            values[slot] = Some(decode(v).ok_or_else(invalid)?);
        }
    }
    let [brief, card_hash, note, option, reference] = values;
    let required =
        |v: Option<String>, field: &str| v.filter(|s| !s.is_empty()).ok_or_else(|| needs(field));
    let reference = required(reference, "ref")?;
    let card_hash = required(card_hash, "card_hash")?;
    let option = required(option, "option")?;
    Ok(Form {
        reference,
        card_hash,
        option,
        note: note.filter(|s| !s.is_empty()),
        brief: brief.filter(|s| !s.is_empty()),
    })
}

/// The plain `400` of a form refusal: `E_BAD_INPUT: <message>`, a newline, then `fix: <fix>`.
pub fn form_refusal(e: &RdyError) -> Response {
    text(400, &format!("{}: {}\nfix: {}", e.code, e.message, e.fix))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::site::http::parse_head;

    const HASH: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

    fn form(reference: &str, card_hash: &str, option: &str) -> Form {
        Form {
            reference: reference.into(),
            card_hash: card_hash.into(),
            option: option.into(),
            note: None,
            brief: None,
        }
    }

    #[track_caller]
    fn refused(body: &str, message: &str, fix: &str) {
        let e = parse_form(body.as_bytes()).unwrap_err();
        assert_eq!(e.code, E_BAD_INPUT, "{body}");
        assert_eq!(e.message, message, "{body}");
        assert_eq!(e.fix, fix, "{body}");
    }

    #[track_caller]
    fn invalid_form(body: &[u8]) {
        let e = parse_form(body).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                E_BAD_INPUT,
                "the answer form is not valid",
                "reload the card and choose an option"
            ),
            "{}",
            String::from_utf8_lossy(body)
        );
    }

    #[test]
    fn ui8_form_reads_five_fields_once_each() {
        let body = format!(
            "ref=P-abc%401&card_hash={HASH}&option=approve&note=caf%C3%A9+%2B+ok&brief=brief%3AX"
        );
        assert_eq!(
            parse_form(body.as_bytes()).unwrap(),
            Form {
                note: Some("café + ok".into()),
                brief: Some("brief:X".into()),
                ..form("P-abc@1", HASH, "approve")
            }
        );
        // any order; an empty note or brief is absent; a nonempty brief is kept as it is
        assert_eq!(
            parse_form(b"brief=&note=&option=o1&card_hash=h&ref=r").unwrap(),
            form("r", "h", "o1")
        );
        assert_eq!(
            parse_form(b"ref=r&card_hash=h&option=o1&brief=+x+")
                .unwrap()
                .brief,
            Some(" x ".into())
        );
        // the values are not validated here: the engine checks them
        assert_eq!(
            parse_form(b"ref=nonsense&card_hash=not-a-hash&option=zz").unwrap(),
            form("nonsense", "not-a-hash", "zz")
        );
    }

    #[test]
    fn ui8_form_refuses_missing_duplicate_and_unknown_keys() {
        let fix = "reload the card and choose an option";
        refused("", "the answer form needs ref", fix);
        refused("card_hash=h&option=o", "the answer form needs ref", fix);
        refused(
            "ref=&card_hash=h&option=o",
            "the answer form needs ref",
            fix,
        );
        refused("ref=r&option=o", "the answer form needs card_hash", fix);
        refused("ref=r&card_hash=h", "the answer form needs option", fix);
        refused(
            "ref=r&card_hash=h&option=",
            "the answer form needs option",
            fix,
        );
        let keys = "the parameters here are brief, card_hash, note, option, ref";
        refused(
            "ref=r&card_hash=h&option=o&notes=x",
            "answer form: unknown parameter \"notes\"",
            &format!("did you mean \"note\"? {keys}"),
        );
        refused(
            "refs=r",
            "answer form: unknown parameter \"refs\"",
            &format!("did you mean \"ref\"? {keys}"),
        );
        refused(
            "ref=r&scope=canon",
            "answer form: unknown parameter \"scope\"",
            &format!("remove \"scope\"; {keys}"),
        );
        // an encoded key is decoded once before the check
        refused(
            "ref=r&x%79z=1",
            "answer form: unknown parameter \"xyz\"",
            &format!("remove \"xyz\"; {keys}"),
        );
        for bad in [
            &b"ref=r&ref=r&card_hash=h&option=o"[..],
            b"ref=r&card_hash=h&option=o&note=a&note=b",
            b"ref=r&card_hash=h&option=o&note=a&brief=b&brief=c",
            b"ref=r&card_hash=h&option=o&note=a&brief=b&x=1",
            b"ref=r&card_hash=h&option=o&note=a&brief=b&",
            b"ref=r&card_hash",
            b"ref=r&&card_hash=h&option=o",
            b"=r&card_hash=h&option=o",
            b"ref=%zz&card_hash=h&option=o",
            b"ref=%4&card_hash=h&option=o",
            b"ref=r%&card_hash=h&option=o",
            b"ref=%FF&card_hash=h&option=o",
            b"ref=\xff&card_hash=h&option=o",
            b"r%FFef=r",
        ] {
            invalid_form(bad);
        }
    }

    #[test]
    fn ui8_form_refusal_is_plain_text() {
        let e = parse_form(b"refs=r").unwrap_err();
        let r = form_refusal(&e);
        assert_eq!(r.status, 400);
        assert_eq!(
            r.headers,
            vec![("Content-Type", "text/plain; charset=utf-8".to_string())]
        );
        assert_eq!(
            String::from_utf8(r.body).unwrap(),
            "E_BAD_INPUT: answer form: unknown parameter \"refs\"\nfix: did you mean \"ref\"? the parameters here are brief, card_hash, note, option, ref"
        );
    }

    fn req(lines: &str) -> Request {
        let mut head = lines.replace('\n', "\r\n");
        head.push_str("\r\n\r\n");
        parse_head(head.as_bytes()).unwrap()
    }

    #[test]
    fn ui8_content_length_is_one_bounded_decimal() {
        let with =
            |lines: &str| content_length(&req(&format!("POST /~ui/answer HTTP/1.1\n{lines}")));
        assert_eq!(with("Content-Length: 0"), Ok(0));
        assert_eq!(with("Content-Length: 4194304"), Ok(MAX_UI_BODY));
        assert_eq!(with("Content-Length: 0004194304"), Ok(MAX_UI_BODY));
        assert_eq!(
            with("Content-Length: 4194305"),
            Err(Refusal::UiBodyTooLarge)
        );
        assert_eq!(
            with(&format!("Content-Length: {}", "9".repeat(4000))),
            Err(Refusal::UiBodyTooLarge)
        );
        for bad in [
            "Host: x",
            "Content-Length:",
            "Content-Length: +1",
            "Content-Length: -1",
            "Content-Length: 1e3",
            "Content-Length: 0x10",
            "Content-Length: 1 0",
            "Content-Length: 10,10",
            "Content-Length: 10\nContent-Length: 10",
            "Content-Length: 10\nTransfer-Encoding: chunked",
            "Transfer-Encoding: identity",
        ] {
            assert_eq!(with(bad), Err(Refusal::Malformed), "{bad}");
        }
    }
}
