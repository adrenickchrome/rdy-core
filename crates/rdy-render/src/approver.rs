//! The approval pages of the local UI (D-6M0AF, D-X61DT, D-WPE4R): the person's pending cards, one
//! item with its briefing, and the page of a refused answer. Each page is built in memory in the
//! site's own layout and theme, from state and objects the caller already computed. The renderer
//! never recomputes a card fact, never fetches a briefing, and writes no file: a page is not a
//! documentation page, so no site output, search entry, llms file, page count or navigation lists
//! it, and no site page links to it. Every text is escaped by the template, never written as
//! markup, and a page needs no script: its forms and its error details work without one.
use crate::refusal::{self, Detail};
use crate::{RdyError, assets, escape, render_page};
use rdy_engine::pending::{Item, ItemKind, Pending};
use rdy_engine::site_scope::percent_encode;
use rdy_engine::{Briefing, E_IO, ReadState};
use serde::{Deserialize, Serialize};

/// A built approval page: its bytes. The caller sends them; nothing else holds them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiPage {
    pub html: Vec<u8>,
}

/// The heading of the pending page, and the first part of its browser title.
const PENDING_HEADING: &str = "Needs you";
/// The heading of a refusal page, and the first part of its browser title.
const REFUSAL_HEADING: &str = "Could not answer";
/// What the pending page says for an empty queue.
const EMPTY: &str = "Nothing needs your answer.";
/// The heading of the group of versions that are not ready.
const NOT_READY: &str = "Not ready";
/// The heading of a question's briefing.
const BRIEFING: &str = "Briefing";
/// What a page with no live stream says in `noscript`.
const NO_STREAM: &str = "Reload this page to see the current queue.";
/// What a page with a live stream says in `noscript`.
const NEEDS_SCRIPT: &str =
    "Live updates need JavaScript. Reload this page to see the current queue.";
/// The route of one item page, before its percent-encoded ref.
const ITEM_ROUTE: &str = "/~ui/item/";

/// The text after a pending page's cards when the queue left items out.
fn omitted_text(n: usize) -> String {
    match n {
        1 => "1 more item waits. Answer these first, then reload this page.".to_string(),
        n => format!("{n} more items wait. Answer these first, then reload this page."),
    }
}

/// An item as the template reads it: the engine's object, and the address of its item page.
#[derive(Serialize)]
struct ItemView<'a> {
    item: &'a Item,
    href: String,
    /// A question is put to the person on its item page, where its briefing stands beside it.
    question: bool,
}

impl<'a> ItemView<'a> {
    fn of(item: &'a Item) -> Self {
        ItemView {
            item,
            href: format!("{ITEM_ROUTE}{}", percent_encode(&item.r#ref)),
            question: item.kind == ItemKind::Answer,
        }
    }
}

/// A version that is not ready: its ref, and one refusal block (message with code and fix) per
/// issue, already rendered by the shared partial.
#[derive(Serialize)]
struct NotReadyView<'a> {
    r#ref: &'a str,
    issues: Vec<minijinja::Value>,
}

/// The refusal block of `message` with `code` and `fix`, as the shared partial writes it.
fn block(message: &str, code: &str, fix: &str) -> Result<minijinja::Value, RdyError> {
    let detail = Detail {
        code: code.to_string(),
        fix: fix.to_string(),
    };
    refusal::html(message, &detail).map(minijinja::Value::from_safe_string)
}

/// The refusal block of an engine refusal: its message, and its code and fix in the details.
fn block_of(e: &RdyError) -> Result<minijinja::Value, RdyError> {
    block(&e.message, e.code, &e.fix)
}

/// The page every approval page shares: the layout's variables, and the page's own under `ui`.
fn render(
    st: &ReadState,
    title: &str,
    events: Option<&str>,
    ui: minijinja::Value,
) -> Result<UiPage, RdyError> {
    let env = crate::env()?;
    let ctx = minijinja::context! {
        site_name => st.project_name(),
        lang => "en",
        title => title,
        kind => "ui",
        root => "/",
        init_js => assets::INIT_JS,
        crumbs => vec![minijinja::context! { label => title, href => minijinja::Value::from(()) }],
        events => events,
        no_stream => NO_STREAM,
        needs_script => NEEDS_SCRIPT,
        ui => ui,
    };
    let (html, slots) = render_page(&env, "approver.html", ctx)?;
    // only the layout's own slots exist in a page: a slot here is a template bug, and an error
    // in every build, as on the not-found page
    if !slots.is_empty() {
        return Err(RdyError::new(
            E_IO,
            "template error: an approval page wrote a label or header slot",
            "this is a bug in rdy-render templates",
        ));
    }
    Ok(UiPage { html })
}

/// The pending page: `pending`'s items in the engine's order, each with its complete card, then
/// the versions that are not ready and the count of items the queue left out. A card that is
/// not a question has its answer form here; a question links to its item page. `notice`, when
/// there is one, stands above the cards. `events`, when there is one, is the data-events URL of
/// the page's live stream (the reload-only subscription); it is escaped.
pub fn pending_page(
    st: &ReadState,
    pending: &Pending,
    notice: Option<&str>,
    events: Option<&str>,
) -> Result<UiPage, RdyError> {
    let items: Vec<ItemView> = pending.items.iter().map(ItemView::of).collect();
    let mut not_ready = vec![];
    for n in &pending.not_ready {
        let mut issues = vec![];
        for i in &n.issues {
            issues.push(block(&i.message, i.code, &i.fix)?);
        }
        not_ready.push(NotReadyView {
            r#ref: &n.r#ref,
            issues,
        });
    }
    let ui = minijinja::context! {
        page => "pending",
        heading => PENDING_HEADING,
        empty => EMPTY,
        not_ready_heading => NOT_READY,
        notice => notice,
        items => minijinja::Value::from_serialize(&items),
        not_ready => minijinja::Value::from_serialize(&not_ready),
        omitted => (pending.omitted > 0).then(|| omitted_text(pending.omitted)),
    };
    render(st, PENDING_HEADING, events, ui)
}

/// The page of one item. Without a refusal it shows the complete card and, below it, the
/// question's `briefing` when there is one (the caller fetched it; this page only shows it and
/// sends its token), then the answer form. With a `refusal` it shows the refusal's message with
/// its code and fix in details above the card: a corrected card of a question has no form and
/// no briefing, so a new request fetches a fresh briefing first; a corrected card of any other
/// kind has its fresh form.
pub fn item_page(
    st: &ReadState,
    item: &Item,
    briefing: Option<&Briefing>,
    refusal: Option<&RdyError>,
    events: Option<&str>,
) -> Result<UiPage, RdyError> {
    let view = ItemView::of(item);
    let refused = refusal.map(block_of).transpose()?;
    // a briefing belongs to a question that is put to the person now
    let shown = briefing.filter(|_| view.question && refusal.is_none());
    let (brief, brief_html) = match shown {
        Some(b) => (Some(b.base.token.clone()), Some(briefing_html(b)?)),
        None => (None, None),
    };
    let form = !(view.question && refusal.is_some());
    let ui = minijinja::context! {
        page => "item",
        item => minijinja::Value::from_serialize(&view),
        refusal => refused,
        form => form,
        briefing_heading => BRIEFING,
        brief => brief,
        brief_html => brief_html.map(minijinja::Value::from_safe_string),
    };
    render(st, &item.title, events, ui)
}

/// The page of a refusal that has no item to show: its message with code and fix in details,
/// and, when the refusal carries a corrected card, that card's text. It never prints a request
/// id, a path or a form.
pub fn refusal_page(st: &ReadState, refusal: &RdyError) -> Result<UiPage, RdyError> {
    let card = refusal
        .corrected
        .as_ref()
        .and_then(|c| c.get("card"))
        .and_then(|c| c.as_str());
    let ui = minijinja::context! {
        page => "refusal",
        heading => REFUSAL_HEADING,
        refusal => block_of(refusal)?,
        card => card,
    };
    render(st, REFUSAL_HEADING, None, ui)
}

/// A JSON value with its objects in the order they were written (serde_json's own map sorts
/// its keys), so the briefing shows its fields in their serialized order.
enum Node {
    Null,
    Bool(bool),
    Number(String),
    Text(String),
    List(Vec<Node>),
    Map(Vec<(String, Node)>),
}

impl<'de> Deserialize<'de> for Node {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Node, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = Node;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("any JSON value")
            }

            fn visit_unit<E>(self) -> Result<Node, E> {
                Ok(Node::Null)
            }

            fn visit_none<E>(self) -> Result<Node, E> {
                Ok(Node::Null)
            }

            fn visit_bool<E>(self, v: bool) -> Result<Node, E> {
                Ok(Node::Bool(v))
            }

            fn visit_i64<E>(self, v: i64) -> Result<Node, E> {
                Ok(Node::Number(v.to_string()))
            }

            fn visit_u64<E>(self, v: u64) -> Result<Node, E> {
                Ok(Node::Number(v.to_string()))
            }

            fn visit_f64<E>(self, v: f64) -> Result<Node, E> {
                Ok(Node::Number(v.to_string()))
            }

            fn visit_str<E>(self, v: &str) -> Result<Node, E> {
                Ok(Node::Text(v.to_string()))
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut a: A) -> Result<Node, A::Error> {
                let mut out = vec![];
                while let Some(n) = a.next_element()? {
                    out.push(n);
                }
                Ok(Node::List(out))
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut a: A) -> Result<Node, A::Error> {
                let mut out = vec![];
                while let Some((k, v)) = a.next_entry()? {
                    out.push((k, v));
                }
                Ok(Node::Map(out))
            }
        }
        d.deserialize_any(V)
    }
}

impl Node {
    /// `self` as markup: a map as a list of names and values, a list as an ordered list, a
    /// scalar as its text. Every name and text is escaped.
    fn write(&self, out: &mut String) {
        match self {
            Node::Null => out.push_str("null"),
            Node::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Node::Number(n) => out.push_str(n),
            Node::Text(t) => out.push_str(&escape(t)),
            Node::List(items) => {
                out.push_str("<ol class=\"briefing-list\">");
                for n in items {
                    out.push_str("<li>");
                    n.write(out);
                    out.push_str("</li>");
                }
                out.push_str("</ol>");
            }
            Node::Map(fields) => {
                out.push_str("<dl class=\"briefing-fields\">");
                for (name, n) in fields {
                    out.push_str("<dt>");
                    out.push_str(&escape(name));
                    out.push_str("</dt><dd>");
                    n.write(out);
                    out.push_str("</dd>");
                }
                out.push_str("</dl>");
            }
        }
    }
}

/// The fields of `briefing` as it serializes them, in that order, as markup with every name and
/// value escaped. Fields the engine omits stay omitted, and its omitted counts and next cursor
/// show as the fields they are.
fn briefing_html(briefing: &Briefing) -> Result<String, RdyError> {
    let bad = |e: serde_json::Error| {
        RdyError::new(
            E_IO,
            format!("the briefing cannot be shown: {e}"),
            "this is a bug in rdy-render",
        )
    };
    let text = serde_json::to_string(briefing).map_err(bad)?;
    let node: Node = serde_json::from_str(&text).map_err(bad)?;
    let mut out = String::with_capacity(text.len() * 2);
    node.write(&mut out);
    Ok(out)
}
