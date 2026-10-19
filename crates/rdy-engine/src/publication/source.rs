//! The storage one publication resolves through (D-48NYM, D-KDFZ3; hub n-XGTFE): the pack, the
//! captured source title and description, and the blobs of the selected figures. Selection,
//! digests and the reference check stay in [`super::select`]; a source only answers reads, so a
//! store and a record stream that hold the same snapshot resolve the same selection.

use crate::ReadState;
use crate::error::RdyError;
use rdy_model::Hash;
use rdy_schema::Pack;
use rdy_store::kinds::{self, BlobManifest};
use std::ops::ControlFlow;

/// What a publication reads besides the snapshot it selects from. The manifest, availability
/// and redaction reads allocate no blob payload; [`PublicationSource::blob`] alone reads bytes,
/// and only after every bound is checked on the manifests.
pub trait PublicationSource {
    /// The pack the profile fields and the graph's tracking rules read.
    fn pack(&self) -> &Pack;
    /// The source title the implicit profile shows.
    fn title(&self) -> &str;
    /// The source description the implicit profile shows, if any.
    fn description(&self) -> Option<&str>;
    /// The manifest of blob `blob`, if the source holds it intact: None for a blob it lacks or
    /// whose manifest a removal masked.
    fn manifest(&self, blob: &Hash) -> Result<Option<BlobManifest>, RdyError>;
    /// Whether the source can read blob `blob`: it holds the blob and no approved removal masks
    /// its manifest or a chunk. No blob byte is read.
    fn blob_available(&self, blob: &Hash) -> Result<bool, RdyError>;
    /// The bytes of blob `blob`.
    fn blob(&self, blob: &Hash) -> Result<Vec<u8>, RdyError>;
    /// The approved removal request that masks blob `blob`, the first one
    /// [`PublicationSource::visit_blob_redactions`] reports; None for a blob the source lacks or
    /// one no removal masks.
    fn removed_blob(&self, blob: &Hash) -> Result<Option<Hash>, RdyError>;
    /// Reports to `visit` each approved removal request that masks blob `blob`: its manifest's,
    /// then each chunk's in manifest order (a masked manifest has no chunks to walk). The same
    /// request may be reported more than once; the caller bounds and deduplicates. An error
    /// `visit` returns stops the walk and is returned. No blob payload is read.
    fn visit_blob_redactions(
        &self,
        blob: &Hash,
        visit: &mut dyn FnMut(Hash) -> Result<(), RdyError>,
    ) -> Result<(), RdyError>;
}

/// A [`PublicationSource`] over one published store state, with the rendering settings the
/// caller captured: the pack, the title and the description.
pub struct ReadPublicationSource<'a> {
    read: &'a ReadState,
    pack: &'a Pack,
    title: &'a str,
    description: Option<&'a str>,
}

impl<'a> ReadPublicationSource<'a> {
    /// `read`'s store, read with the explicit `pack`, `title` and `description`.
    pub fn with_settings(
        read: &'a ReadState,
        pack: &'a Pack,
        title: &'a str,
        description: Option<&'a str>,
    ) -> ReadPublicationSource<'a> {
        ReadPublicationSource {
            read,
            pack,
            title,
            description,
        }
    }

    /// The one removal walk of blob `h`: the manifest's mark, then each chunk's, through the
    /// store's existing removal reads. `step` answers each request and may break the walk.
    fn walk(
        &self,
        h: &Hash,
        step: &mut dyn FnMut(Hash) -> Result<ControlFlow<()>, RdyError>,
    ) -> Result<(), RdyError> {
        let reader = self.read.reader();
        if !reader.has_blob(h)? {
            return Ok(());
        }
        // a masked manifest is unreadable: nothing below it to walk
        if let Some(request) = reader.redaction_of(kinds::BLOB_MANIFEST, h)? {
            return step(request).map(|_| ());
        }
        let manifest: BlobManifest = reader.get_typed(kinds::BLOB_MANIFEST, h)?;
        for c in &manifest.chunks {
            if let Some(request) = reader.redaction_of(kinds::BLOB_CHUNK, c)?
                && step(request)?.is_break()
            {
                break;
            }
        }
        Ok(())
    }
}

impl PublicationSource for ReadPublicationSource<'_> {
    fn pack(&self) -> &Pack {
        self.pack
    }

    fn title(&self) -> &str {
        self.title
    }

    fn description(&self) -> Option<&str> {
        self.description
    }

    fn manifest(&self, blob: &Hash) -> Result<Option<BlobManifest>, RdyError> {
        let reader = self.read.reader();
        if !reader.has_blob(blob)? || reader.redaction_of(kinds::BLOB_MANIFEST, blob)?.is_some() {
            return Ok(None);
        }
        Ok(Some(reader.get_typed(kinds::BLOB_MANIFEST, blob)?))
    }

    fn blob_available(&self, blob: &Hash) -> Result<bool, RdyError> {
        if !self.read.reader().has_blob(blob)? {
            return Ok(false);
        }
        Ok(self.removed_blob(blob)?.is_none())
    }

    fn blob(&self, blob: &Hash) -> Result<Vec<u8>, RdyError> {
        Ok(self.read.reader().get_blob(blob)?)
    }

    fn removed_blob(&self, blob: &Hash) -> Result<Option<Hash>, RdyError> {
        let mut first = None;
        self.walk(blob, &mut |request| {
            first = Some(request);
            Ok(ControlFlow::Break(()))
        })?;
        Ok(first)
    }

    fn visit_blob_redactions(
        &self,
        blob: &Hash,
        visit: &mut dyn FnMut(Hash) -> Result<(), RdyError>,
    ) -> Result<(), RdyError> {
        self.walk(blob, &mut |request| {
            visit(request)?;
            Ok(ControlFlow::Continue(()))
        })
    }
}
