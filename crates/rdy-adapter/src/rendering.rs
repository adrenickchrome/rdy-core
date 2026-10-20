//! Publication rendering for a host that embeds rdy core (D-WDKWQ, D-KDFZ3, D-48NYM, D-0VCQ5,
//! D-QYCRB, D-5X0ZK; hub i-FGX7D, i-N0G2N, r-B0ZYE, r-7GS51).
//!
//! Every fact here is core's: the manifest recipe and hash, the render inputs, the render
//! process and the one-page render are re-exported from the crates that own them, and a
//! consumer names them from this crate alone. The holder keeps no second renderer, worker or
//! output holder. It writes a render request for a child process the hub creates and limits
//! ([`Snapshot::write_render_request`]), or renders one page of a captured tag in the call
//! ([`Snapshot::render_page`]). A project caller obtains explicit [`RenderInputs`] through
//! `rdy_render::render_input::capture_inputs`; the holder adds no default.

use crate::store::Snapshot;
use rdy_engine::publication::ReadPublicationSource;
use rdy_engine::render_source::{ReadTagHistorySource, tag_is_redacted};
use rdy_engine::{RdyError, Scope};
use rdy_render::process::write_request_prelude;
use std::io::Write;

pub use crate::stream::BatchReport;
pub use rdy_engine::publication_manifest::{Manifest, ManifestEntry, manifest_hash};
pub use rdy_engine::render_inputs::RenderInputs;
pub use rdy_render::PublicationArtifact;
pub use rdy_render::RenderedPage;
pub use rdy_render::process::read_artifact as read_render_artifact;
pub use rdy_render::process::run as run_render_process;
pub use rdy_render::process::{ProcessSummary, RenderFormat};

impl Snapshot {
    /// Writes the render request of the captured tag `tag` (`1.2.0` finds `v1.2.0`) to `out`:
    /// the process prelude (`profile`, `format` and `inputs`), then the tag's complete history
    /// stream with nothing known ([`Snapshot::write_stream`]). `profile` is a full profile id
    /// or `implicit:org`; a profile key is refused with the publication take's own text. The
    /// inputs, the selector and the tag are checked before any byte is written. It returns no
    /// success until the stream and the epoch check have finished; on an error the caller
    /// discards what it was given. No renderer runs here: the host owns the child process, its
    /// limits and its standard streams.
    pub fn write_render_request<W: Write>(
        &self,
        tag: &str,
        profile: &str,
        inputs: &RenderInputs,
        format: RenderFormat,
        mut out: W,
    ) -> Result<BatchReport, RdyError> {
        self.read_state().ensure_current_epoch()?;
        self.captured_tag(tag)?;
        write_request_prelude(profile, format, inputs, &mut out)?;
        self.write_stream(tag, &Default::default(), out)
    }

    /// Renders the page at `path` of the publication of `profile` (a full profile id or
    /// `implicit:org`) in the captured tag `tag`, as the full site writes it. The page reads the
    /// captured tag's state and the supplied `inputs` alone: their effective pack and title,
    /// never the holder's built-in pack or directory name. The tag's redaction mark is read from
    /// the tag's own history with `inputs.pack`, never from a canon.
    pub fn render_page(
        &self,
        tag: &str,
        profile: &str,
        inputs: &RenderInputs,
        path: &str,
    ) -> Result<RenderedPage, RdyError> {
        let read = self.read_state();
        read.ensure_current_epoch()?;
        let (_, captured) = self.captured_tag(tag)?;
        let m = read.materialized(&Scope::At(captured.landing))?;
        let source = ReadPublicationSource::with_settings(
            read,
            &inputs.pack,
            &inputs.title,
            inputs.description.as_deref(),
        );
        let redacted = tag_is_redacted(
            &mut ReadTagHistorySource::new(read),
            captured.landing,
            &inputs.pack,
        )?;
        let page = rdy_render::render_publication_page(
            &source,
            &m,
            &captured.name,
            redacted,
            profile,
            inputs,
            path,
        )?;
        read.ensure_current_epoch()?;
        Ok(page)
    }
}
