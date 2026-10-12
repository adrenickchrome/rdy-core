//! ib10: the MCP resources (spec §10.3). Six resources, each the JSON reply of one tool read with
//! `max_tokens: 4000`:
//!
//! | URI | Returns |
//! |---|---|
//! | `rdy://project` | `rdy_overview {}` |
//! | `rdy://pack` | `rdy_schema {}` |
//! | `rdy://doc/{id}` | `rdy_read {id}` |
//! | `rdy://view/{name}` | `rdy_view {op: "show", view: name}` |
//! | `rdy://proposal/{id}` | `rdy_read {id}` with `id` a `P-<id>@<n>` or `P-<id>` |
//! | `rdy://inbox` | `rdy_inbox {}` (never acks) |
//!
//! The set is closed. A read needs a session (`rdy_session` first). `subscribe` and the
//! `subscriptions/listen` filter accept the same URIs and nothing else.
use crate::notify::Sink;
use crate::server::RdyServer;
use crate::tools::{self, McpState, session};
use rdy_engine::{E_INTERNAL, E_NOT_FOUND, RdyError};
use rmcp::{
    ErrorData as McpError,
    model::{
        ListResourceTemplatesResult, ListResourcesResult, ReadResourceResult, Resource,
        ResourceContents, ResourceTemplate, SubscriptionFilter,
    },
    service::SubscriptionContext,
};
use serde_json::{Value, json};

/// The resources with a fixed URI: `(uri, name)`.
pub const RESOURCES: &[(&str, &str)] = &[
    ("rdy://project", "project"),
    ("rdy://pack", "pack"),
    ("rdy://inbox", "inbox"),
];

/// The resources whose URI takes a name or an id: `(uri template, name)`.
pub const TEMPLATES: &[(&str, &str)] = &[
    ("rdy://doc/{id}", "doc"),
    ("rdy://view/{name}", "view"),
    ("rdy://proposal/{id}", "proposal"),
];

/// The mime type of every resource: the tool's JSON reply.
const MIME: &str = "application/json";

/// The token budget every resource read passes to its tool.
const BUDGET: usize = 4000;

fn describe(name: &str) -> &'static str {
    match name {
        "project" => {
            "The project overview: pack, views, landings, open report, coverage and the tree."
        }
        "pack" => "The schema: block types, fields and the write reference.",
        "inbox" => "Your inbox: the events that concern this session. Reading it never acks.",
        "doc" => "One block or doc by id or path, in compact form.",
        "view" => "One view: its owner, changes and state.",
        "proposal" => "One proposal version, P-<id>@<n>, or its latest as P-<id>.",
        _ => "",
    }
}

/// One of the six resources, parsed from its URI.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Res {
    Project,
    Pack,
    Inbox,
    Doc(String),
    View(String),
    Proposal(String),
}

impl Res {
    /// The resource a URI names, or `None` for any URI outside the closed set.
    pub(crate) fn parse(uri: &str) -> Option<Res> {
        match uri {
            "rdy://project" => return Some(Res::Project),
            "rdy://pack" => return Some(Res::Pack),
            "rdy://inbox" => return Some(Res::Inbox),
            _ => {}
        }
        let named = |prefix: &str| uri.strip_prefix(prefix).filter(|rest| !rest.is_empty());
        if let Some(id) = named("rdy://doc/") {
            return Some(Res::Doc(id.to_string()));
        }
        if let Some(name) = named("rdy://view/") {
            return Some(Res::View(name.to_string()));
        }
        named("rdy://proposal/").map(|id| Res::Proposal(id.to_string()))
    }

    /// The tool call behind the resource, run for the session in `st`.
    fn read(self, st: &mut McpState) -> Result<Value, RdyError> {
        // the schema tool needs no session; a resource read always does
        session(st)?;
        match self {
            Res::Project => tools::rdy_overview(st, params(json!({"max_tokens": BUDGET}))?),
            Res::Pack => tools::rdy_schema(st, params(json!({}))?),
            Res::Inbox => tools::rdy_inbox(st, params(json!({"max_tokens": BUDGET}))?),
            Res::Doc(id) | Res::Proposal(id) => {
                tools::rdy_read(st, params(json!({"id": id, "max_tokens": BUDGET}))?)
            }
            Res::View(name) => crate::view_tools::rdy_view(
                st,
                params(json!({"op": "show", "view": name, "max_tokens": BUDGET}))?,
            ),
        }
    }
}

/// A tool's parameters from JSON. The values here are built by this file, so a failure is a bug.
fn params<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, RdyError> {
    serde_json::from_value(v).map_err(|e| {
        RdyError::new(
            E_INTERNAL,
            format!("a resource read built bad tool parameters: {e}"),
            "report this as a bug in rdy",
        )
    })
}

/// The refusal for a URI outside the closed set.
pub(crate) fn unknown(uri: &str) -> McpError {
    McpError::resource_not_found(
        format!(
            "unknown resource {uri}: rdy serves rdy://project, rdy://pack, rdy://doc/{{id}}, rdy://view/{{name}}, rdy://proposal/{{id}} and rdy://inbox"
        ),
        None,
    )
}

/// A tool error as a protocol error. A missing session is an invalid request; a missing doc,
/// view or proposal is a missing resource.
fn refuse(e: RdyError) -> McpError {
    let data = serde_json::to_value(&e).ok();
    if e.code == E_NOT_FOUND {
        McpError::resource_not_found(e.to_string(), data)
    } else {
        McpError::invalid_request(e.to_string(), data)
    }
}

impl RdyServer {
    pub(crate) fn resources_list() -> ListResourcesResult {
        ListResourcesResult::with_all_items(
            RESOURCES
                .iter()
                .map(|(uri, name)| {
                    Resource::new(*uri, *name)
                        .with_description(describe(name))
                        .with_mime_type(MIME)
                })
                .collect(),
        )
    }

    pub(crate) fn resource_templates_list() -> ListResourceTemplatesResult {
        ListResourceTemplatesResult::with_all_items(
            TEMPLATES
                .iter()
                .map(|(uri, name)| {
                    ResourceTemplate::new(*uri, *name)
                        .with_description(describe(name))
                        .with_mime_type(MIME)
                })
                .collect(),
        )
    }

    /// `resources/read`: the tool behind `uri`, on the blocking pool with the state lock.
    pub(crate) async fn resources_read(&self, uri: String) -> Result<ReadResourceResult, McpError> {
        let res = Res::parse(&uri).ok_or_else(|| unknown(&uri))?;
        let state = std::sync::Arc::clone(&self.state);
        let joined = tokio::task::spawn_blocking(move || {
            let mut st = state.lock().unwrap_or_else(|p| p.into_inner());
            res.read(&mut st)
        })
        .await;
        let value = match joined {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => return Err(refuse(e)),
            Err(_) => {
                return Err(McpError::internal_error(
                    format!("internal error reading {uri}: the read was cancelled"),
                    None,
                ));
            }
        };
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(value.to_string(), uri).with_mime_type(MIME),
        ]))
    }

    /// Legacy `resources/subscribe`: the session's peer becomes its sink.
    pub(crate) fn resources_subscribe(
        &self,
        uri: &str,
        peer: rmcp::Peer<rmcp::RoleServer>,
    ) -> Result<(), McpError> {
        Res::parse(uri).ok_or_else(|| unknown(uri))?;
        self.hub.connect(self.hub_sid, Sink::Legacy(peer));
        self.hub.subscribe(self.hub_sid, uri);
        Ok(())
    }

    pub(crate) fn resources_unsubscribe(&self, uri: &str) -> Result<(), McpError> {
        Res::parse(uri).ok_or_else(|| unknown(uri))?;
        self.hub.unsubscribe(self.hub_sid, uri);
        Ok(())
    }

    /// The `subscriptions/listen` filter rdy accepts: the requested resource URIs that are known.
    pub(crate) fn resources_filter(requested: &SubscriptionFilter) -> SubscriptionFilter {
        let known: Vec<String> = requested
            .resource_subscriptions
            .iter()
            .flatten()
            .filter(|uri| Res::parse(uri).is_some())
            .cloned()
            .collect();
        SubscriptionFilter::builder()
            .resource_subscriptions(known)
            .build()
    }

    /// `subscriptions/listen`: records the accepted URIs and the stream's sink, waits for the
    /// client to cancel, then forgets both.
    pub(crate) async fn resources_listen(&self, context: SubscriptionContext) {
        let uris = context
            .accepted()
            .resource_subscriptions
            .clone()
            .unwrap_or_default();
        for uri in &uris {
            self.hub.subscribe(self.hub_sid, uri);
        }
        self.hub
            .connect(self.hub_sid, Sink::Listen(context.sink().clone()));
        context.cancelled().await;
        for uri in &uris {
            self.hub.unsubscribe(self.hub_sid, uri);
        }
        self.hub.connect(self.hub_sid, Sink::None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ib10_the_closed_set_of_uris() {
        assert_eq!(Res::parse("rdy://project"), Some(Res::Project));
        assert_eq!(Res::parse("rdy://pack"), Some(Res::Pack));
        assert_eq!(Res::parse("rdy://inbox"), Some(Res::Inbox));
        assert_eq!(Res::parse("rdy://doc/d-1"), Some(Res::Doc("d-1".into())));
        assert_eq!(
            Res::parse("rdy://view/work"),
            Some(Res::View("work".into()))
        );
        assert_eq!(
            Res::parse("rdy://proposal/P-1@2"),
            Some(Res::Proposal("P-1@2".into()))
        );
        for no in [
            "rdy://doc/",
            "rdy://view/",
            "rdy://proposal/",
            "rdy://nope",
            "rdy://inbox/x",
            "file:///etc",
            "",
        ] {
            assert_eq!(Res::parse(no), None, "{no}");
        }
    }

    #[test]
    fn ib10_the_listen_filter_keeps_known_uris_only() {
        let requested = SubscriptionFilter::builder()
            .tools_list_changed()
            .resource_subscriptions(["rdy://inbox", "file:///etc", "rdy://doc/d-1"])
            .build();
        let accepted = RdyServer::resources_filter(&requested);
        assert_eq!(
            accepted.resource_subscriptions,
            Some(vec!["rdy://inbox".to_string(), "rdy://doc/d-1".to_string()])
        );
        assert_eq!(accepted.tools_list_changed, None);
    }
}
