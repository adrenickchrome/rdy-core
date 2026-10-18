//! The `site` family's tool registration.

use crate::params::Parameters;
use crate::server::RdyServer;
use crate::tools::{self, ExportParams, PublicationCheckParams, SiteParams};
use rmcp::{model::CallToolResult, tool, tool_router};

#[tool_router(router = site_router, vis = "pub(crate)")]
impl RdyServer {
    #[tool(
        description = "The link to this project's live docs site, for the person to open in a browser. Pages update by themselves as the scope changes. Pass scope for a view or proposal version: it renders in review mode with Show changes. Relay warning and each entry of warnings to the person. Read and write through the tools yourself, not the site."
    )]
    async fn rdy_site(&self, Parameters(p): Parameters<SiteParams>) -> CallToolResult {
        self.run("rdy_site", p, tools::rdy_site).await
    }

    #[tool(
        description = "Print the docs for a person to read: one PDF file, one HTML file, or a Markdown tree (layout mkdocs for MkDocs and Backstage TechDocs). It never overwrites. Relay each entry of warnings to the person. Read and write through the tools yourself, not the export."
    )]
    async fn rdy_export(&self, Parameters(p): Parameters<ExportParams>) -> CallToolResult {
        self.run("rdy_export", p, tools::rdy_export).await
    }

    #[tool(
        description = "Check a publication profile's selected docs and references. Pass profile implicit:org for every doc. Pass against to compare with an explicit canon, tag, or landing baseline. This checks local output; it does not authorize publication or find the last publication. Relay warnings to the person."
    )]
    async fn rdy_publication_check(
        &self,
        Parameters(p): Parameters<PublicationCheckParams>,
    ) -> CallToolResult {
        // the report is closed and size-checked: a pack notice must not be added to it
        self.run_closed("rdy_publication_check", p, tools::rdy_publication_check)
            .await
    }
}
