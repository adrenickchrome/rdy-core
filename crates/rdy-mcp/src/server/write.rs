//! The `write` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, RedactParams, WriteParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = write_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "Atomic batch write of ops (create, update, move, delete, rebalance, repin, reaffirm; rdy_schema lists them under \"ops\"). Place a block with after or before a sibling, or neither to append. Use id \"$name\" placeholders to refer to blocks created earlier in the same batch, including {{$name}} inside text. Reference blocks in text as {{id}}, or {{id|your words}} to choose the link text; a block's key (fields.key, e.g. REQ-12) works wherever an id does; a bare {{KEY}} is stored as {{id|@key}} and {{KEY|words}} as {{id|words}}. Bodies take a small Markdown subset (rdy_schema \"text\"). links: {\"<kind>\": [{\"target\": \"<id or path>\"}]}. An update replaces only the link kinds it names; \"<kind>\": [] removes that kind. Cite brief: <token> from rdy_brief for stable content; the response lists the write's consequences. Use reaffirm for a suspect accepted decision that still holds. Use repin with code: true after checking a reflects block against its code. Pass one of implements, editorial or reflects. reflects is {sources?: [source id, key or path], code?: true}. A source created in this write counts if the final state holds it. code: true records the code state of each written block with reflects globs."
    )]
    async fn rdy_write(&self, Parameters(p): Parameters<WriteParams>) -> CallToolResult {
        self.run("rdy_write", p, tools::rdy_write).await
    }

    #[tool(
        description = "Content removal (redaction). With dry_run true (any role) it reports the blast radius and records nothing: the targets, every version, blob and payload the removal drops, each record that embeds, references or quotes the content, the copies outside the store, and warnings. Show that report and its warnings to the person. Without dry_run it records the request in your view (default: your session's) and erases nothing; propose the view with rdy_propose, and only a person can approve it (rdy_pending shows the card). targets: a block id (every stored version), id@<full version hash>, or blob:<full manifest hash>. reason_class: secret, personal, legal or retraction. max_tokens (256 to 20000, default 4000) budgets text; the structured report stays complete."
    )]
    async fn rdy_redact(&self, Parameters(p): Parameters<RedactParams>) -> CallToolResult {
        self.run("rdy_redact", p, tools::rdy_redact).await
    }
}
