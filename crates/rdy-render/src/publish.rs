//! The renderer of the publish coordinator (D-KDFZ3, D-0VCQ5, D-QYCRB): the coordinator's
//! [`PublishRenderer`] over this crate's own builders. [`PublicationRenderer::capture`] is
//! [`capture_inputs`]; [`PublicationRenderer::render`] builds the profile's publication in memory
//! with [`build_publication`], read through the captured inputs alone (their pack, title and
//! description) and showing the tag's stored name and its redaction mark, which
//! [`tag_is_redacted`] reads from the tag's history with the inputs' pack. The artifact it returns
//! is that build, unchanged: no output directory is written and no file is read back. It takes no
//! code, command or shell option.
use crate::render_input::capture_inputs;
use crate::{PublicationArtifact, build_publication};
use rdy_engine::hub_publish::{PublishRenderer, RenderedUpload};
use rdy_engine::hub_publish_files::ArtifactSource;
use rdy_engine::publication::ReadPublicationSource;
use rdy_engine::publication_manifest::{MAX_FILES, Manifest, artifact_mismatch};
use rdy_engine::render_inputs::RenderInputs;
use rdy_engine::render_source::{ReadTagHistorySource, tag_is_redacted};
use rdy_engine::{E_BAD_INPUT, RdyError, ReadState, Scope};
use std::io::{Cursor, Read};

/// The publish renderer: the static site of a profile at a tag.
#[derive(Clone, Copy, Debug, Default)]
pub struct PublicationRenderer;

/// A built publication as an upload source: its manifest and each file's bytes, which never
/// change.
struct Built(PublicationArtifact);

impl ArtifactSource for Built {
    fn manifest(&self) -> &Manifest {
        &self.0.manifest
    }

    /// The build already holds at most [`MAX_FILES`] files (its budget refuses more), so the list
    /// is bounded before it is made.
    fn paths(&self) -> Vec<&str> {
        debug_assert!(self.0.files.len() <= MAX_FILES);
        self.0.files.keys().map(String::as_str).collect()
    }

    fn open(&self, path: &str) -> Result<Box<dyn Read + '_>, RdyError> {
        let bytes = self.0.files.get(path).ok_or_else(artifact_mismatch)?;
        Ok(Box::new(Cursor::new(bytes.as_slice())))
    }
}

/// `E_BAD_INPUT`: a publication renders from a tag's landing only.
fn not_a_tag() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "a publication renders from a tag",
        "publish a tag: rdy publish <tag> --to <remote> --site <site>",
    )
}

impl PublishRenderer for PublicationRenderer {
    fn capture(&self, read: &ReadState, scope: &Scope) -> Result<RenderInputs, RdyError> {
        capture_inputs(read, scope)
    }

    fn render(
        &self,
        read: &ReadState,
        scope: &Scope,
        tag: &str,
        profile: &str,
        inputs: &RenderInputs,
    ) -> Result<RenderedUpload, RdyError> {
        let Scope::At(landing) = scope else {
            return Err(not_a_tag());
        };
        let source = ReadPublicationSource::with_settings(
            read,
            &inputs.pack,
            &inputs.title,
            inputs.description.as_deref(),
        );
        let m = read.materialized(scope)?;
        let redacted =
            tag_is_redacted(&mut ReadTagHistorySource::new(read), *landing, &inputs.pack)?;
        let artifact = build_publication(&source, &m, tag, redacted, profile, inputs)?;
        let warnings = artifact.warnings.clone();
        Ok(RenderedUpload {
            artifact: Box::new(Built(artifact)),
            warnings,
        })
    }
}
