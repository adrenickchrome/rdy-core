//! Publication rendering for a host that embeds rdy core (D-WDKWQ, D-KDFZ3, D-48NYM, D-0VCQ5,
//! D-QYCRB, D-5X0ZK; hub i-FGX7D, i-N0G2N, r-B0ZYE, r-7GS51).
//!
//! Every fact here is core's: the manifest recipe and hash, the render inputs, the render
//! process and the one-page render are re-exported from the crates that own them, and a
//! consumer names them from this crate alone. The holder keeps no second renderer, worker or
//! output holder. It writes a render request for a child process the hub creates and limits
//! ([`Snapshot::write_render_request`]), or renders one page of a captured tag in the call
//! ([`Snapshot::render_page`]). A scope render request names a landing the holder reaches
//! (D-Y32MN), alone or as a review pair against a second landing that the hub picks
//! ([`Snapshot::write_scope_render_request`], D-CST6K); core builds no one-page call for it. A project captures explicit [`RenderInputs`] for a requested tag
//! with [`Snapshot::capture_render_inputs`] (core's `rdy_render::render_input::capture_inputs`,
//! with the tag's disagreement list); the holder adds no default and captures none.
//! [`check_publication_media_type`] is core's rule for the media type of a manifest entry; a
//! host checks each entry of an uploaded manifest with it and keeps no table of its own (hub
//! D-X0F6P).

use crate::store::Snapshot;
use rdy_engine::publication::ReadPublicationSource;
use rdy_engine::render_source::{ReadTagHistorySource, tag_is_redacted};
use rdy_engine::stream_export;
use rdy_engine::{E_BAD_INPUT, E_INTERNAL, RdyError, Scope};
use rdy_model::Hash;
use rdy_render::process::{
    write_request_prelude, write_scope_request_prelude, write_stream_length,
};
use rdy_store::sync_stream::StreamPurpose;
use std::collections::BTreeSet;
use std::io::{self, Write};

pub use crate::stream::BatchReport;
pub use rdy_engine::publication_manifest::{Manifest, ManifestEntry, manifest_hash};
pub use rdy_engine::render_inputs::{DisagreementInput, RenderInputs};
pub use rdy_render::PublicationArtifact;
pub use rdy_render::RenderedPage;
pub use rdy_render::check_publication_media_type;
pub use rdy_render::process::read_artifact as read_render_artifact;
pub use rdy_render::process::run as run_render_process;
pub use rdy_render::process::{ProcessSummary, RenderFormat};

/// The target of a scope render request (D-CST6K): `scope` is the state to render, and
/// `against`, when given, the state its review marks are against. Each is `landing:` and the
/// landing's 64 lowercase hex digits. The hub picks the pair: a version against the next is
/// `against` = the head of version n and `scope` = the head of version n+1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeRender {
    pub scope: String,
    pub against: Option<String>,
}

/// What [`Snapshot::write_scope_render_request`] wrote: the report of each stream, `scope`'s and
/// `against`'s.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeRenderReport {
    pub scope: BatchReport,
    pub against: Option<BatchReport>,
}

/// `E_BAD_INPUT`: a scope render target `text` that is not `landing:<64 hex digits>`.
fn not_renderable(text: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("scope {text:?} cannot be rendered: use landing:<64 hex digits>"),
        "name the landing by its full hash, or render a tag with write_render_request",
    )
}

/// `E_INTERNAL`: stream `n` wrote another length than its count found.
fn stream_changed(n: u8) -> RdyError {
    RdyError::new(
        E_INTERNAL,
        format!("render request stream {n} changed while it was written"),
        "report this as a bug in rdy",
    )
}

/// A writer that counts the bytes it is given and passes them on to `out`.
struct Counted<W> {
    out: W,
    bytes: u64,
}

impl<W: Write> Write for Counted<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.out.write(buf)?;
        self.bytes += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

impl Snapshot {
    /// The landing a scope render target `text` names: `landing:<64 hex digits>` that D-Y32MN's
    /// reach finds from the captured refs. Another text is `E_BAD_INPUT`; an unreached landing is
    /// the reads' `E_NOT_FOUND`.
    fn render_landing(&self, text: &str) -> Result<Hash, RdyError> {
        // a hash parses from exactly 64 lowercase hex digits
        let Some(Ok(hash)) = text.strip_prefix("landing:").map(str::parse::<Hash>) else {
            return Err(not_renderable(text));
        };
        if !self.reached_landing(hash)? {
            return Err(crate::reads::unreached(hash));
        }
        Ok(hash)
    }

    /// The bytes of the complete canon history of `landing` with nothing known, counted as it
    /// is written to nowhere.
    fn history_len(&self, landing: Hash) -> Result<u64, RdyError> {
        let mut count = Counted {
            out: io::sink(),
            bytes: 0,
        };
        self.write_rooted(landing, &mut count)?;
        Ok(count.bytes)
    }

    /// Writes the complete canon history of `landing`, with nothing known, to `out`.
    fn write_rooted<W: Write>(&self, landing: Hash, out: W) -> Result<BatchReport, RdyError> {
        stream_export::write_history(
            self.read_state(),
            StreamPurpose::CanonHistory,
            landing,
            None,
            &BTreeSet::new(),
            out,
        )
    }

    /// Writes stream `n` of a scope request: its length `len`, then the canon history of
    /// `landing`, which must take exactly `len` bytes.
    fn write_side<W: Write>(
        &self,
        n: u8,
        landing: Hash,
        len: u64,
        out: &mut W,
    ) -> Result<BatchReport, RdyError> {
        write_stream_length(n, len, out)?;
        let mut counted = Counted {
            out: &mut *out,
            bytes: 0,
        };
        let report = self.write_rooted(landing, &mut counted)?;
        if counted.bytes != len {
            return Err(stream_changed(n));
        }
        Ok(report)
    }

    /// Writes the scope render request of `target` (D-CST6K) to `out`: the process prelude
    /// (`profile`, `format`, `inputs`, `scope` and `against`), then the complete canon history
    /// stream of `scope` with nothing known after its u64 little-endian length, then `against`'s
    /// in the same form. Each target names a landing by its full hash (`landing:<64 hex
    /// digits>`) that the snapshot's captured refs reach (D-Y32MN). A request with `against`
    /// builds `site` only. The targets, the prelude and each stream's length (at most
    /// 2,147,483,648 bytes) are checked before any byte is written: each stream is counted once
    /// before it is written. It returns no success until both streams and the epoch check have
    /// finished; on an error the caller discards what it was given. No renderer runs here.
    pub fn write_scope_render_request<W: Write>(
        &self,
        target: &ScopeRender,
        profile: &str,
        inputs: &RenderInputs,
        format: RenderFormat,
        mut out: W,
    ) -> Result<ScopeRenderReport, RdyError> {
        let read = self.read_state();
        read.ensure_current_epoch()?;
        let scope = self.render_landing(&target.scope)?;
        let against = match &target.against {
            Some(text) => Some(self.render_landing(text)?),
            None => None,
        };
        let against_text = target.against.as_deref();
        // what the process would refuse is never written
        write_scope_request_prelude(
            profile,
            format,
            inputs,
            &target.scope,
            against_text,
            &mut io::sink(),
        )?;
        let scope_len = self.history_len(scope)?;
        write_stream_length(1, scope_len, &mut io::sink())?;
        let against_len = match against {
            Some(landing) => {
                let len = self.history_len(landing)?;
                write_stream_length(2, len, &mut io::sink())?;
                Some((landing, len))
            }
            None => None,
        };
        write_scope_request_prelude(
            profile,
            format,
            inputs,
            &target.scope,
            against_text,
            &mut out,
        )?;
        let scope_report = self.write_side(1, scope, scope_len, &mut out)?;
        let against_report = match against_len {
            Some((landing, len)) => Some(self.write_side(2, landing, len, &mut out)?),
            None => None,
        };
        read.ensure_current_epoch()?;
        Ok(ScopeRenderReport {
            scope: scope_report,
            against: against_report,
        })
    }

    /// The render inputs of the captured tag `tag` (`1.2.0` finds `v1.2.0`) in this project, as
    /// a push carries them with the tag (D-QMZ92, D-H0BZN, D-8RGWW): the project's effective
    /// pack, theme, title and description, and the tag's own disagreement list (a tag at canon's
    /// head lists canon's open disagreements; a tag at a past landing lists none). The capture
    /// is explicit: no historical settings are read. A holder captures none (`E_BAD_INPUT`); a
    /// tag the snapshot lacks is `E_NOT_FOUND`.
    pub fn capture_render_inputs(&self, tag: &str) -> Result<RenderInputs, RdyError> {
        let read = self.read_state();
        if read.reader().mode()? == rdy_store::holder::StoreMode::Holder {
            return Err(crate::landing_check::project_only("render inputs"));
        }
        read.ensure_current_epoch()?;
        let (_, captured) = self.captured_tag(tag)?;
        let inputs = rdy_render::render_input::capture_inputs(read, &Scope::At(captured.landing))?;
        read.ensure_current_epoch()?;
        Ok(inputs)
    }

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
