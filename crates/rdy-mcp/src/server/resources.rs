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
        // a proposal ref is what `rdy_read` routes as one: it is read only as a proposal, and a proposal URI takes nothing else
        if let Some(id) = named("rdy://doc/") {
            return (!rdy_engine::is_proposal_ref(id)).then(|| Res::Doc(id.to_string()));
        }
        if let Some(name) = named("rdy://view/") {
            return Some(Res::View(name.to_string()));
        }
        named("rdy://proposal/")
            .filter(|id| rdy_engine::is_proposal_ref(id))
            .map(|id| Res::Proposal(id.to_string()))
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
            Res::View(name) => {
                // compiled only for this crate's own tests: a release binary has no hook
                #[cfg(test)]
                if name == "__rdy_test_panic__" {
                    panic!("test hook: deliberate panic in a read")
                }
                crate::view_tools::rdy_view(
                    st,
                    params(json!({"op": "show", "view": name, "max_tokens": BUDGET}))?,
                )
            }
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

/// A tool error as a protocol error. A missing doc, view or proposal is a missing resource; a
/// fault in rdy is an internal error; any other error (a missing session, bad input) is an
/// invalid request.
fn refuse(e: RdyError) -> McpError {
    let data = serde_json::to_value(&e).ok();
    match e.code {
        E_NOT_FOUND => McpError::resource_not_found(e.to_string(), data),
        E_INTERNAL => McpError::internal_error(e.to_string(), data),
        _ => McpError::invalid_request(e.to_string(), data),
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
        let value = self
            .blocking("resources/read", false, move |st| res.read(st))
            .await
            .map_err(refuse)?;
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
            Res::parse("rdy://proposal/P-1a2b@2"),
            Some(Res::Proposal("P-1a2b@2".into()))
        );
        // a doc path may start with `P-`: only a real proposal ref (`P-`, 4 to 64 hex digits) is a proposal
        assert_eq!(
            Res::parse("rdy://doc/P-plan"),
            Some(Res::Doc("P-plan".into()))
        );
        assert_eq!(Res::parse("rdy://doc/P-1"), Some(Res::Doc("P-1".into())));
        for no in [
            "rdy://doc/",
            "rdy://view/",
            "rdy://proposal/",
            "rdy://nope",
            "rdy://inbox/x",
            // a proposal URI takes a proposal id only, a doc URI never takes one
            "rdy://proposal/d-XXXX",
            "rdy://proposal/p-1",
            "rdy://proposal/P-plan",
            "rdy://proposal/P-1",
            "rdy://doc/P-1a2b",
            "rdy://doc/P-1a2b@2",
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

    fn server() -> (tempfile::TempDir, RdyServer) {
        let dir = tempfile::tempdir().unwrap();
        let rdy = rdy_engine::Rdy::init(dir.path(), "rdy-standard").unwrap();
        let engine = std::sync::Arc::new(rdy_engine::engine::Local::new(rdy));
        (
            dir,
            RdyServer::new(engine, None, crate::notify::NotifyHub::new()),
        )
    }

    async fn open_session(server: &RdyServer) {
        let p = crate::tools::SessionParams {
            actor: "agent:a".into(),
            run: Some("r".into()),
            role: "writer".into(),
            view: None,
            areas: None,
        };
        let out = server.run("rdy_session", Ok(p), tools::rdy_session).await;
        assert_ne!(out.is_error, Some(true), "{out:?}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ib10_a_panic_in_a_read_is_an_internal_error_and_the_server_lives() {
        let (_d, server) = server();
        open_session(&server).await;
        let e = server
            .resources_read("rdy://view/__rdy_test_panic__".into())
            .await
            .unwrap_err();
        assert_eq!(e.code, rmcp::model::ErrorCode::INTERNAL_ERROR, "{e:?}");
        assert!(
            e.message.contains("E_INTERNAL") && e.message.contains("deliberate panic"),
            "{e:?}"
        );
        // the state lock is usable and the next read answers
        let ok = server.resources_read("rdy://project".into()).await;
        assert!(ok.is_ok(), "{ok:?}");
    }

    #[test]
    fn ib10_an_internal_error_is_an_internal_error_not_an_invalid_request() {
        use rmcp::model::ErrorCode;
        let e = |code: &'static str| RdyError::new(code, "m", "f");
        assert_eq!(refuse(e(E_INTERNAL)).code, ErrorCode::INTERNAL_ERROR);
        assert_eq!(refuse(e(E_NOT_FOUND)).code, ErrorCode::RESOURCE_NOT_FOUND);
        assert_eq!(refuse(e("E_NO_SESSION")).code, ErrorCode::INVALID_REQUEST);
        assert_eq!(refuse(e("E_BAD_INPUT")).code, ErrorCode::INVALID_REQUEST);
    }
}
