//! The live layer of the renderer (FW-LS1, D-N5NSN): what the site server needs to know about a
//! scope's build. It tells when the inputs of a build changed (so an unrelated commit costs no
//! build), which pages changed between two builds, and the bytes to send for a page: its version
//! label filled in and the live script added. Nothing here touches the file system.
use crate::{Mode, SiteFiles, build_site, disagree, escape};
use rdy_engine::{RdyError, ReadState, Scope};
use rdy_model::Hash;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

/// The script a live page loads. The server sends it at `/_rdy/live.js`.
pub const LIVE_JS: &str = include_str!("../theme/rdy-standard/js/live.js");

/// A site built for the live server, with the revision of each page.
pub struct LiveBuild {
    /// Every file of the site, as `build_site` wrote it.
    pub files: BTreeMap<String, Vec<u8>>,
    /// One entry per `.html` file but the permalink pages: the revision of its bytes without the
    /// version label. A permalink page sends the reader on at once, so nothing stays open on it
    /// and it has no revision: its text follows the target's label, which would reload it for
    /// every edit of that label.
    pub revs: BTreeMap<String, String>,
    /// The version label's text, as written in the files.
    pub label: String,
    /// The revision of the label.
    pub label_rev: String,
    /// Where the build wrote the label in each file (see `SiteFiles::label_at`).
    pub label_at: BTreeMap<String, Vec<Range<usize>>>,
    /// The render's warnings for this build, as `rdy render` gives them (`RenderSummary`), in
    /// the same order.
    pub warnings: Vec<String>,
}

/// The first 16 lowercase hex digits of the hash of `bytes`.
pub fn rev(bytes: &[u8]) -> String {
    Hash::of(bytes).to_string()[..16].to_string()
}

/// The revision of a page: the bytes between the label ranges, each piece after its length, so
/// a change of the label alone does not change it.
fn page_rev(bytes: &[u8], label_at: &[Range<usize>]) -> String {
    let mut cut: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut from = 0;
    for r in label_at {
        let piece = &bytes[from..r.start];
        cut.extend_from_slice(&(piece.len() as u64).to_le_bytes());
        cut.extend_from_slice(piece);
        from = r.end;
    }
    let piece = &bytes[from..];
    cut.extend_from_slice(&(piece.len() as u64).to_le_bytes());
    cut.extend_from_slice(piece);
    rev(&cut)
}

/// Whether `bytes` is a permalink page (`redirect.html`), which names its kind in the opening tag.
fn is_redirect(bytes: &[u8]) -> bool {
    const KIND: &[u8] = b"data-rdy-kind=\"redirect\"";
    bytes[..bytes.len().min(96)]
        .windows(KIND.len())
        .any(|w| w == KIND)
}

/// Builds the site of `scope` as the live server serves it, with the revision of each page.
pub fn build_live(st: &ReadState, scope: &Scope) -> Result<LiveBuild, RdyError> {
    rdy_engine::refuse_inflight_export(scope)?;
    let SiteFiles {
        files,
        label,
        label_at,
        summary,
    } = build_site(st, scope, Mode::Live)?;
    let revs = files
        .iter()
        .filter(|(path, bytes)| path.ends_with(".html") && !is_redirect(bytes))
        .map(|(path, bytes)| {
            let at = label_at.get(path).map_or(&[][..], Vec::as_slice);
            (path.clone(), page_rev(bytes, at))
        })
        .collect();
    let label_rev = rev(label.as_bytes());
    Ok(LiveBuild {
        files,
        revs,
        label,
        label_rev,
        label_at,
        warnings: summary.warnings,
    })
}

/// A hash input: every field after its length, so no two inputs share bytes.
struct Key(Vec<u8>);

impl Key {
    fn text(&mut self, s: &str) -> &mut Self {
        self.num(s.len() as u64);
        self.0.extend_from_slice(s.as_bytes());
        self
    }
    fn num(&mut self, n: u64) -> &mut Self {
        self.0.extend_from_slice(&n.to_le_bytes());
        self
    }
    fn hash(&mut self, h: Option<Hash>) -> &mut Self {
        match h {
            Some(h) => {
                self.num(1);
                self.0.extend_from_slice(&h.0);
                self
            }
            None => self.num(0),
        }
    }
    fn opt_text(&mut self, s: Option<&str>) -> &mut Self {
        match s {
            Some(s) => self.num(1).text(s),
            None => self.num(0),
        }
    }
}

/// What the build of `scope` reads, as one hash. The same key means the same pages, so the
/// server builds only when it changes. The key holds the scope and its head (canon's head; a
/// view's name, head op and effective base; a version's landing or a proposal version's hash),
/// canon's head (a landing rebases a view and moves every label), the versions, the pack
/// generation, the project's name and description, and the disagreements list (a dismissal or a
/// judged tension changes canon's page and moves no head). The list is empty for a view and a
/// proposal version, so only canon (and a version at canon's head) holds entries.
pub fn inputs_key(st: &ReadState, scope: &Scope, pack_generation: u64) -> Result<Hash, RdyError> {
    rdy_engine::refuse_inflight_export(scope)?;
    let mut k = Key(Vec::new());
    match scope {
        Scope::Canon => {
            k.num(0).hash(st.canon_head());
        }
        Scope::View(name) => {
            k.num(1)
                .text(name)
                .hash(st.view_head(name).map(|h| h.head))
                .hash(st.effective_base(name));
        }
        Scope::At(h) => {
            k.num(2).hash(Some(*h));
        }
        Scope::Proposal(h) => {
            k.num(3).hash(Some(*h));
        }
        // refused above: an analysis scope is never served
        Scope::Inflight => {
            k.num(4);
        }
    }
    k.hash(st.canon_head());
    let versions = st.versions()?;
    k.num(versions.len() as u64);
    for v in &versions {
        k.text(&v.name)
            .text(&v.landing)
            .opt_text(v.message.as_deref())
            .num(v.at_ms as u64);
    }
    k.num(pack_generation)
        .text(&st.project_name())
        .opt_text(st.project_description().as_deref());
    let list = disagree::list(st, scope)?;
    k.num(list.len() as u64);
    for d in &list {
        k.text(d.sentence).num(d.blocks.len() as u64);
        for b in &d.blocks {
            k.text(b.as_str());
        }
        k.opt_text(d.lead.as_ref().map(|b| b.as_str()));
    }
    Ok(Hash::of(&k.0))
}

/// What changed between two builds of one scope.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Change {
    /// Each `.html` path whose revision changed or that is gone.
    pub reload: BTreeSet<String>,
    /// The new label, when the label changed.
    pub label: Option<String>,
}

/// The pages that changed from `old` to `new`, and the new label when it changed.
pub fn diff(old: &LiveBuild, new: &LiveBuild) -> Change {
    let reload = old
        .revs
        .iter()
        .filter(|(path, r)| new.revs.get(*path) != Some(*r))
        .map(|(path, _)| path.clone())
        .collect();
    Change {
        reload,
        label: (old.label_rev != new.label_rev).then(|| new.label.clone()),
    }
}

/// `s` with every byte outside `A-Z a-z 0-9 - . _ ~ : @` as `%XX`.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~:@".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The bytes the server sends for `path`, or none for a path the site does not have. The
/// version label goes into exactly the ranges the build recorded (escaped in an HTML page, on
/// one line in `llms.txt` and `llms-full.txt`, as the static build writes them), last range first, and nothing else is searched or
/// changed. An HTML page also gets the live script tag right before its first `</head>`. Any
/// other file is sent as it is. `scope_text` is the scope as the events route reads it
/// (`canon`, `view:<name>`, `proposal:P-<x>@<n>`).
pub fn page(build: &LiveBuild, path: &str, scope_text: &str) -> Option<Vec<u8>> {
    let mut bytes = build.files.get(path)?.clone();
    let html = path.ends_with(".html");
    if let Some(ranges) = build.label_at.get(path) {
        // as the static build writes it: escaped in a page, on one line in the llms files
        let text = if html {
            escape(&build.label)
        } else {
            crate::llms::one_line(&build.label)
        };
        for r in ranges.iter().rev() {
            bytes.splice(r.clone(), text.bytes());
        }
    }
    // a permalink page has no revision and takes no script
    if let (true, Some(rev)) = (html, build.revs.get(path)) {
        let tag = format!(
            "<script src=\"/_rdy/live.js\" data-events=\"/_rdy/events?scope={}&amp;page={}&amp;rev={}&amp;label={}\" defer></script>",
            percent_encode(scope_text),
            path,
            rev,
            build.label_rev
        );
        // the head comes before any content, and content is escaped: the first one is the page's
        if let Some(at) = bytes.windows(7).position(|w| w == b"</head>") {
            bytes.splice(at..at, tag.bytes());
        }
    }
    Some(bytes)
}
