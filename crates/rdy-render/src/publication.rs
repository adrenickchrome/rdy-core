//! A named profile's output (D-0VCQ5, D-48NYM, D-KDFZ3): every page, link, permalink, trail,
//! glossary, question, search entry, figure and agent file reads one filtered view of canon, the
//! profile's selected state and its induced graph. [`prepare`] reads that view before the output
//! boundary, as `prepare::source` reads the full source; the site builder and its placement are
//! the same for both. Nothing of the source's history, other tags, tensions' raw text, other
//! profiles or local configuration reaches a [`Prepared`] made here.
use crate::inline;
use crate::page::lede_of;
use crate::prepare::{FigureBlobs, History, Prepared, ProfilePresentation, Purpose};
use crate::render_input::{capture_inputs, disagreements};
use crate::site::{Site, Url};
use crate::{disagree, scope_warnings};
use rdy_engine::publication::{self, PublicationSource, ReadPublicationSource, Resolved};
use rdy_engine::render_inputs::{RenderInputs, validate_inputs};
use rdy_engine::{
    E_BAD_INPUT, E_NOT_FOUND, RdyError, ReadState, Scope, VersionPosition, parse_version,
};
use rdy_model::Hash;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

/// The refusal of a scope that is not canon, a tag or a landing (Task 4's wording).
fn bad_scope() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "A publication profile requires canon, a tag, or a landing.",
        "Use scope canon, tag:<name>, or landing:<hash>.",
    )
}

/// What `profile` (an id or key) selects in the scope `scope_text` (`canon`, `tag:<name>` or
/// `landing:<hash>`), read for `purpose`: the selected state with each removed figure read as
/// the notice, the induced graph, the selected figures' bytes, the profile's title and
/// description, the chosen tag's name and `redacted` mark (RULING F3), the disagreements whose
/// every side is selected and has a page, and the org warning first among the warnings. A public
/// profile whose check is blocked refuses before any asset is read or output staged. Both
/// purposes read the same view, which [`prepare_resolved`] builds from the source's captured
/// inputs ([`capture_inputs`]); this local output keeps its scope's frozen mark and the project's
/// `.rdy` directory for its placement guard.
pub(crate) fn prepare(
    read: &ReadState,
    scope_text: &str,
    profile: &str,
    _purpose: Purpose,
) -> Result<Prepared, RdyError> {
    // the explicit text stays here: `Scope::At` alone cannot tell a tag from a bare landing
    let tag = if scope_text == "canon" || scope_text.starts_with("landing:") {
        None
    } else if let Some(name) = scope_text.strip_prefix("tag:") {
        Some(name)
    } else {
        return Err(bad_scope());
    };
    let scope = read.resolve_scope(scope_text)?;
    let resolved = publication::resolve(read, &scope, profile)?;
    resolved.require_output()?;
    let (tag_name, tag_redacted) = match tag {
        Some(name) => chosen_tag(read, name)?,
        None => (None, false),
    };
    let inputs = capture_inputs(read, &scope)?;
    let source = ReadPublicationSource::with_settings(
        read,
        &inputs.pack,
        &inputs.title,
        inputs.description.as_deref(),
    );
    let mut prepared = prepare_resolved(
        &source,
        resolved,
        tag_name.as_deref().unwrap_or_default(),
        tag_redacted,
        &inputs,
    )?;
    prepared.position.frozen = matches!(scope, Scope::At(_));
    prepared.rdy_dir = read.root().join(".rdy");
    Ok(prepared)
}

/// `E_BAD_INPUT`: a source whose effective pack is not the one the inputs captured. No other
/// pack (a holder's built-in one, say) is read in its place.
fn other_pack() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "the publication source's pack differs from the render inputs' pack",
        "capture the render inputs from the source again",
    )
}

/// Checks `inputs` (`validate_inputs`) and that `source` reads with the inputs' effective pack.
pub(crate) fn check_inputs(
    source: &dyn PublicationSource,
    inputs: &RenderInputs,
) -> Result<(), RdyError> {
    validate_inputs(inputs)?;
    if source.pack() != &inputs.pack {
        return Err(other_pack());
    }
    Ok(())
}

/// The publication view of `resolved` (D-0VCQ5, D-KDFZ3), read through `source` and the captured
/// `inputs` alone: no store, scope, canon, tension or project path. A public profile whose check
/// is blocked is refused first. The selected state with each removed figure read as the notice,
/// the induced graph, the selected figures' bytes (bounded by the source's limits), the pack of
/// the inputs, the title and description (an explicit profile's own, the implicit profile's from
/// the inputs) read through the selection, the captured disagreements whose every side is
/// selected and has a page, the org warning first and then the scope's warnings. The chosen tag's
/// name `tag_name` (empty for none) is the only history the output shows: no tag message, version
/// list, canon label or in-flight link. `rdy_dir` is empty: an in-memory output has no placement.
pub(crate) fn prepare_resolved(
    source: &dyn PublicationSource,
    resolved: Resolved,
    tag_name: &str,
    tag_redacted: bool,
    inputs: &RenderInputs,
) -> Result<Prepared, RdyError> {
    crate::probe::hit();
    check_inputs(source, inputs)?;
    resolved.require_output()?;
    let blobs = selected_blobs(source, &resolved)?;
    let warning = resolved.references.warning();
    let Resolved {
        profile: chosen,
        m,
        graph,
        ..
    } = resolved;
    // a figure whose blob lost content reads as removed from here on, as in source preparation
    let m = blobs.project(Arc::new(m));
    let pack = inputs.pack.clone();
    // the implicit profile shows the captured source title; an explicit one its own
    let (title, description) = match chosen.id {
        Some(_) => (chosen.title.as_str(), chosen.description.as_deref()),
        None => (inputs.title.as_str(), inputs.description.as_deref()),
    };
    // the title and description read their references through the selection, as every other
    // text does: display words kept, else the target's label, else `unavailable reference` (the
    // profile's own doc need not be selected)
    let (site_name, description) = {
        let words = Site::with_unavailable(&m, &pack, "", Some(inline::UNAVAILABLE));
        (
            words.as_written(title),
            description.map(|d| words.as_written(d)),
        )
    };
    let site = Site::with_unavailable(&m, &pack, &site_name, Some(inline::UNAVAILABLE));
    // an entry stays only when every side is selected and has a page: an excluded side's text,
    // count or existence never reaches the page
    let disagreements: Vec<disagree::Disagreement> = disagreements(inputs)?
        .into_iter()
        .filter(|d| {
            d.blocks
                .iter()
                .all(|b| m.get(b).is_some() && site.url_of(b).is_some())
        })
        .collect();
    let mut warnings: Vec<String> = warning
        .map(|w| format!("{} {}", w.message, w.fix))
        .into_iter()
        .collect();
    warnings.extend(scope_warnings(&m, &site, false, None));
    drop(site);
    let tag_name = (!tag_name.is_empty()).then(|| tag_name.to_string());
    let position = VersionPosition {
        version: None,
        landings_since: 0,
        view: None,
        frozen: tag_name.is_some(),
        label: tag_name.clone().unwrap_or_default(),
    };
    Ok(Prepared {
        m,
        g: Arc::new(graph),
        pack,
        blobs,
        site_name,
        description,
        position,
        versions: vec![],
        landed_ms: BTreeMap::new(),
        disagreements,
        scope_text: String::new(),
        rdy_dir: PathBuf::new(),
        history: History {
            warnings,
            ..History::empty()
        },
        profile: Some(ProfilePresentation {
            trail: chosen.trail,
            tag_name,
            tag_redacted,
        }),
    })
}

/// The stored tag `name` names by semantic-version precedence (as `resolve_scope` matches it):
/// its canonical name and whether its history holds removed content. Nothing else of it.
fn chosen_tag(read: &ReadState, name: &str) -> Result<(Option<String>, bool), RdyError> {
    let want = parse_version(name)?;
    read.versions()?
        .into_iter()
        .find(|v| parse_version(&v.name).is_ok_and(|p| p.cmp_precedence(&want) == Ordering::Equal))
        .map(|v| (Some(v.name), v.redacted))
        .ok_or_else(|| {
            RdyError::new(
                E_NOT_FOUND,
                format!("no version {name}"),
                "list versions with rdy tags",
            )
        })
}

/// The selected figures' blobs, read through `source`: the bytes of each available one through
/// the bounded asset reader, and each unavailable one the source holds, by the request that
/// removed its content (`PublicationSource::removed_blob`). Any other error refuses.
fn selected_blobs(
    source: &dyn PublicationSource,
    resolved: &Resolved,
) -> Result<FigureBlobs, RdyError> {
    let bytes = publication::load_source_assets(source, resolved)?;
    let mut removed: BTreeMap<Hash, Hash> = BTreeMap::new();
    for a in resolved.attachments.iter().filter(|a| !a.available) {
        if removed.contains_key(&a.blob) {
            continue;
        }
        if let Some(request) = source.removed_blob(&a.blob)? {
            removed.insert(a.blob, request);
        }
    }
    Ok(FigureBlobs::selected(bytes, removed))
}

/// The pages a named profile's site writes and the anchors each emits: an authored local link
/// may lead only there (D-0VCQ5). Built from the selected site before any page body is written;
/// it never looks at the file system or the source tree.
#[derive(Clone, Debug, Default)]
pub(crate) struct LocalTargets {
    pages: BTreeMap<String, BTreeSet<String>>,
}

impl LocalTargets {
    /// The pages of `site` (its folders and docs, the home page and the collected pages it
    /// writes) with their emitted anchors: each block's anchor and id on its doc's page (a lede
    /// keeps only its anchor), the glossary's groups and terms, the open questions.
    pub(crate) fn of(site: &Site) -> LocalTargets {
        let mut pages: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        pages.insert(
            "index.html".into(),
            BTreeSet::from(["contents".to_string()]),
        );
        for id in site.tree_pages() {
            if let Some(p) = site.page(id) {
                pages.entry(p).or_default();
            }
        }
        for (id, anchor) in &site.anchors {
            let Some(doc) = site.doc_of(id) else { continue };
            let Some(set) = site.page(&doc.id).and_then(|p| pages.get_mut(&p)) else {
                continue;
            };
            set.insert(anchor.clone());
            if lede_of(site.m, doc).is_none_or(|l| l.id != *id) {
                set.insert(id.to_string());
            }
        }
        if !site.terms.is_empty() {
            let set = pages.entry("glossary.html".into()).or_default();
            set.extend(site.term_groups.iter().map(|g| g.anchor.clone()));
            set.extend(
                site.terms
                    .iter()
                    .filter_map(|t| site.term_anchors.get(t).cloned()),
            );
        }
        if !site.open_questions.is_empty() {
            pages
                .entry("questions.html".into())
                .or_default()
                .extend(site.question_anchors.values().cloned());
        }
        if !site.disagreements.is_empty() {
            pages.entry("disagreements.html".into()).or_default();
        }
        LocalTargets { pages }
    }

    /// The canonical href, from page `from`, of the local href `h` as `from` sees it, when it
    /// names a page of the site and, with a fragment, an anchor emitted on that page. None for a
    /// longer URL than [`inline::MAX_LOCAL_URL`], a query string, a backslash, a NUL, a
    /// protocol-relative URL, a malformed percent escape, invalid UTF-8, a path above the output
    /// root, an empty segment inside the path, an asset file or any page the site does not write.
    /// Percent escapes are decoded once, for the comparison only.
    pub(crate) fn href(&self, from: &str, h: &str) -> Option<String> {
        if h.len() > inline::MAX_LOCAL_URL || h.starts_with("//") || h.contains(['?', '\\', '\0']) {
            return None;
        }
        let (path, fragment) = match h.split_once('#') {
            Some((p, f)) => (p, Some(f)),
            None => (h, None),
        };
        let page = if path.is_empty() {
            from.to_string()
        } else {
            let mut segs: Vec<String> = match path.strip_prefix('/') {
                Some(_) => vec![],
                None => {
                    let mut s: Vec<String> = from.split('/').map(String::from).collect();
                    s.pop();
                    s
                }
            };
            let rest = path.strip_prefix('/').unwrap_or(path);
            let parts: Vec<&str> = rest.split('/').collect();
            let mut dir = rest.is_empty();
            for (i, raw) in parts.iter().enumerate() {
                let seg = decode(raw)?;
                if seg.contains(['/', '\\', '\0']) {
                    return None;
                }
                let last = i + 1 == parts.len();
                match seg.as_str() {
                    "" if last => dir = true,
                    "" => return None,
                    "." => dir = last,
                    ".." => {
                        segs.pop()?;
                        dir = last;
                    }
                    _ => segs.push(seg),
                }
            }
            if dir {
                segs.push("index.html".into());
            }
            segs.join("/")
        };
        let anchors = self.pages.get(&page)?;
        let anchor = match fragment.map(decode) {
            None => None,
            Some(None) => return None,
            Some(Some(f)) if f.is_empty() => None,
            Some(Some(f)) if anchors.contains(&f) => Some(f),
            Some(Some(_)) => return None,
        };
        Some(Url { page, anchor }.href_from(from))
    }
}

/// `s` with each percent escape decoded once; none for a malformed escape or bytes that are not
/// UTF-8.
fn decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = |c: u8| (c as char).to_digit(16);
            let hi = hex(*b.get(i + 1)?)?;
            let lo = hex(*b.get(i + 2)?)?;
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn targets() -> LocalTargets {
        let mut pages = BTreeMap::new();
        pages.insert(
            "index.html".to_string(),
            BTreeSet::from(["contents".to_string()]),
        );
        pages.insert(
            "guide/intro.html".to_string(),
            BTreeSet::from(["setup".to_string(), "p-AAAAA".to_string()]),
        );
        pages.insert("guide/index.html".to_string(), BTreeSet::new());
        pages.insert("café.html".to_string(), BTreeSet::new());
        LocalTargets { pages }
    }

    #[test]
    fn pub6_local_hrefs_lead_only_to_written_pages_and_anchors() {
        let t = targets();
        let from = "guide/intro.html";
        for (h, want) in [
            // the renderer's canonical href: by the relative root, `#anchor` on the same page
            ("intro.html", Some("../guide/intro.html")),
            ("./intro.html#setup", Some("#setup")),
            ("#setup", Some("#setup")),
            ("#p-AAAAA", Some("#p-AAAAA")),
            ("../index.html#contents", Some("../index.html#contents")),
            ("/guide/intro.html", Some("../guide/intro.html")),
            ("/", Some("../index.html")),
            ("./", Some("../guide/index.html")),
            ("../caf%C3%A9.html", Some("../café.html")),
            ("intro.html#", Some("../guide/intro.html")),
            ("%69ntro.html", Some("../guide/intro.html")),
            ("intro.html#nope", None),
            ("other.html", None),
            ("../../index.html", None),
            ("intro.html?x=1", None),
            ("intro%2ehtml%", None),
            ("caf%C3.html", None),
            ("a//intro.html", None),
            ("//host/x", None),
            ("..\\index.html", None),
            ("assets/site.css", None),
        ] {
            assert_eq!(t.href(from, h).as_deref(), want, "{h}");
        }
        let long = format!("{}intro.html", "./".repeat(1019));
        assert_eq!(long.len(), 2048);
        assert_eq!(t.href(from, &long).as_deref(), Some("../guide/intro.html"));
        // one byte more (an empty fragment, which alone would change nothing) leads nowhere
        let longer = format!("{long}#");
        assert_eq!(longer.len(), 2049);
        assert_eq!(t.href(from, &longer), None);
    }
}
