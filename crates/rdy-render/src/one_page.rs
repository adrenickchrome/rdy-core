//! One page of a publication, without the rest of the site (hub r-B0ZYE, D-KDFZ3, D-0VCQ5,
//! D-48NYM): [`render_publication_page`] reads the same filtered view and shared setup as
//! [`crate::build_publication`], builds the one page context a request names and renders it
//! once. The other pages' bodies, the search index and the agent files are never made.
use crate::page::{self, Kind};
use crate::site::{Url, is_profile};
use crate::{
    Budget, Mode, Slots, assets, bad_notice_slot, build_page, copy_blobs, kind_page, manifest_of,
    page_kinds, publication, put_file, render_ctx, used_media, with_shared,
};
use rdy_engine::publication::PublicationSource;
use rdy_engine::publication_manifest::{
    MAX_PATH_BYTES, Manifest, ManifestEntry, NOTICE_SLOT, validate_manifest,
};
use rdy_engine::render_inputs::RenderInputs;
use rdy_engine::{E_NOT_FOUND, Materialized, RdyError};
use rdy_model::{BlockId, Hash};
use std::collections::BTreeMap;

/// One rendered page of a publication.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedPage {
    /// The page's site-relative path, as requested.
    pub path: String,
    pub html: Vec<u8>,
    /// Where the page's empty notice slot starts, in bytes.
    pub notice_offset: u64,
    /// The files this page needs: the theme, a `assets/app.js` made for this page, and the
    /// figure files the page links, by path.
    pub assets: BTreeMap<String, Vec<u8>>,
    /// The page and its assets alone, as the site's manifest lists them. Its entries equal the
    /// full site's for those paths, except `assets/app.js`, which depends on the page.
    pub manifest: Manifest,
    /// The build's warnings: an org profile's reference warning first.
    pub warnings: Vec<String>,
}

/// `E_NOT_FOUND`: a page this publication has no page for. The same refusal answers a page that
/// never existed and one the profile leaves out, so it names neither.
fn not_found() -> RdyError {
    RdyError::new(
        E_NOT_FOUND,
        "publication page was not found",
        "choose a page from this publication",
    )
}

/// Checks `path` by the manifest's rules (1 to 4096 bytes of safe segments), as the manifest
/// does. A path over the byte limit is checked as the shortest one over it, so nothing the size
/// of the request is copied.
fn check_path(path: &str) -> Result<(), RdyError> {
    let path = if path.len() > MAX_PATH_BYTES {
        "a".repeat(MAX_PATH_BYTES + 1)
    } else {
        path.to_string()
    };
    validate_manifest(&Manifest {
        files: vec![ManifestEntry {
            path,
            hash: Hash::of(b""),
            media_type: "text/html".into(),
            size: NOTICE_SLOT.len() as u64,
            notice_offset: None,
        }],
    })
}

/// What a path names in a build.
enum Target<'a> {
    Page(Kind<'a>),
    Permalink(BlockId, Url),
}

/// The page the full site writes at `path`: a permalink page, which replaces a page at its path,
/// else the last page of the build with that path. None for a path the build has no page at.
fn resolve<'a>(sh: &page::Shared<'a>, path: &str) -> Option<Target<'a>> {
    let site = sh.site;
    if let Some(name) = path
        .strip_prefix("go/")
        .and_then(|p| p.strip_suffix(".html"))
        && let Ok(id) = BlockId::parse(name)
        && site.go_page(&id) == path
        && sh.m.get(&id).is_some_and(|b| !is_profile(b))
        && let Some(url) = site.url_of(&id)
    {
        return Some(Target::Permalink(id, url));
    }
    page_kinds(site, sh.m)
        .into_iter()
        .rev()
        .find(|k| kind_page(site, k).as_deref() == Some(path))
        .map(Target::Page)
}

/// Renders the page of the publication of `profile` (a profile id or key, or `implicit:org`) in
/// the tagged snapshot `m` that the site writes at `path`: the home page, a folder or doc page,
/// the glossary, the open questions, the disagreements or a block's permalink page. It reads
/// `source` and the captured `inputs` alone, as [`crate::build_publication`] does, and builds
/// and renders that one page only. No path of a file system is opened.
///
/// The page links address the whole publication, and so do its search index URL and its notice
/// slot's offset in its own bytes. The assets are the theme files, `assets/app.js` as this page
/// needs it (the diagram library only when the page has a diagram) and the figure files the page
/// links. The manifest lists the page and those assets. A path the publication has no page at,
/// or whose page the profile leaves out, is `E_NOT_FOUND`.
pub fn render_publication_page(
    source: &dyn PublicationSource,
    m: &Materialized,
    tag_name: &str,
    tag_redacted: bool,
    profile: &str,
    inputs: &RenderInputs,
    path: &str,
) -> Result<RenderedPage, RdyError> {
    check_path(path)?;
    publication::check_inputs(source, inputs)?;
    let resolved = rdy_engine::publication::resolve_source(source, m, profile)?;
    let prepared = publication::prepare_resolved(source, resolved, tag_name, tag_redacted, inputs)?;
    let budget = Budget::publication();
    let (built, mut blobs) = with_shared(prepared, Mode::Static, |sh, env, extras| {
        let ctx = match resolve(sh, path).ok_or_else(not_found)? {
            Target::Page(kind) => build_page(sh, kind),
            Target::Permalink(id, url) => build_page(sh, Kind::Redirect(&id, &url)),
        };
        let has_mermaid = ctx.has_mermaid;
        let (page_path, html, _, notice) = render_ctx(env, Slots::Notices, Some(&budget), &ctx)?;
        Ok(Built {
            page_path,
            html,
            notice: notice.ok_or_else(bad_notice_slot)?,
            has_mermaid,
            used: sh.used_blobs.take(),
            warnings: extras.warnings,
        })
    })?;
    let Built {
        page_path,
        html,
        notice,
        has_mermaid,
        used,
        warnings,
    } = built;
    // the page's bytes were admitted as it was written
    let mut files = BTreeMap::new();
    budget.count(1)?;
    files.insert(page_path.clone(), html);
    for (name, bytes) in assets::files(has_mermaid) {
        put_file(&mut files, Some(&budget), name.into(), bytes)?;
    }
    copy_blobs(&mut files, Some(&budget), &used, &mut blobs)?;
    drop(blobs);
    let notices = BTreeMap::from([(page_path.clone(), notice)]);
    let manifest = manifest_of(&files, &notices, &used_media(&used))?;
    validate_manifest(&manifest)?;
    let html = files.remove(&page_path).ok_or_else(bad_notice_slot)?;
    Ok(RenderedPage {
        path: page_path,
        html,
        notice_offset: notice,
        assets: files,
        manifest,
        warnings,
    })
}

/// What the closure of [`render_publication_page`] brings out of the shared setup.
struct Built {
    page_path: String,
    html: Vec<u8>,
    notice: u64,
    has_mermaid: bool,
    used: BTreeMap<String, (Hash, String)>,
    warnings: Vec<String>,
}
