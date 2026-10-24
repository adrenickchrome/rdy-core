//! The live layer of the renderer (FW-LS1, D-N5NSN): what the site server needs to know about a
//! scope's build. It tells when the inputs of a build changed (so an unrelated commit costs no
//! build), which pages changed between two builds, and the bytes to send for a page: its version
//! label and header filled in and the live script added. Nothing here touches the file system.
use crate::{Mode, SiteFiles, Slot, SlotKind, Slots, build_site_from, disagree, escape, prepare};
use rdy_engine::site_scope::ScopeRef;
use rdy_engine::site_target::SiteTarget;
use rdy_engine::{E_BAD_INPUT, RdyError, ReadState, Scope};
use rdy_model::Hash;
use std::collections::{BTreeMap, BTreeSet};

/// The script a live page loads. The server sends it at `/_rdy/live.js`.
pub const LIVE_JS: &str = include_str!("../theme/rdy-standard/js/live.js");

/// A site built for the live server, with the revision of each page.
pub struct LiveBuild {
    /// Every file of the site, as `build_site` wrote it.
    pub files: BTreeMap<String, Vec<u8>>,
    /// One entry per `.html` file but the permalink pages: the revision of its bytes without the
    /// version label and the header. A permalink page sends the reader on at once, so nothing
    /// stays open on it and it has no revision: its text follows the target's label, which would
    /// reload it for every edit of that label.
    pub revs: BTreeMap<String, String>,
    /// The version label's text (the colophon and the llms files), as written in the files.
    pub label: String,
    /// The header's version markup (the pill and its chips), the same on every page.
    pub header: String,
    /// The revision of the label and the header together.
    pub label_rev: String,
    /// Where the build wrote the label and the header in each file (see `SiteFiles::label_at`).
    pub label_at: BTreeMap<String, Vec<Slot>>,
    /// The render's warnings for this build, as `rdy render` gives them (`RenderSummary`), in
    /// the same order.
    pub warnings: Vec<String>,
    /// The redaction epoch the build's state captured (D-CC90E): a build serves only while the
    /// store's epoch is still this one.
    pub epoch: u64,
}

/// The first 16 lowercase hex digits of the hash of `bytes`.
pub fn rev(bytes: &[u8]) -> String {
    Hash::of(bytes).to_string()[..16].to_string()
}

/// The revision of a page: the bytes between the slot ranges (the label's and the header's), each
/// piece after its length, so a change of the label or the header alone does not change it.
fn page_rev(bytes: &[u8], label_at: &[Slot]) -> String {
    let mut cut: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut from = 0;
    for (_, r) in label_at {
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

/// The revision of the label and the header: each after its length.
fn label_rev(label: &str, header: &str) -> String {
    let mut k = Key(Vec::with_capacity(label.len() + header.len() + 16));
    k.text(label).text(header);
    rev(&k.0)
}

/// Builds the site of `scope` (the scope `target` names, resolved) as the live server serves it,
/// with the revision of each page. `target` is a source target: its root (`/`, `/~view/<enc>/`,
/// `/~proposal/<ref>/` or `/~tag/<name>/`) is where the header's links lead, and its menu link
/// (`SiteTarget::menu_url`) is where the pill opens the menu, so an unfiltered proposal's page
/// opens that proposal's lists. Any other target is refused with the invalid-target error.
pub fn build_live(
    st: &ReadState,
    scope: &Scope,
    target: &SiteTarget,
) -> Result<LiveBuild, RdyError> {
    rdy_engine::refuse_inflight_export(scope)?;
    target.validate()?;
    if !matches!(target, SiteTarget::Source(s) if *s != ScopeRef::Versions) {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "invalid live site target",
            "use a source scope and a publication profile key",
        ));
    }
    let mut prepared = prepare::source(st, scope, prepare::Purpose::Site)?;
    prepared.menu_url = target.menu_url();
    let root = target.root();
    let built = build_site_from(prepared, Mode::Live { root: &root }, Slots::Source, None)?;
    Ok(of_site(built.site, st.redaction_epoch()))
}

/// A live build of a publication profile (D-CWC91, D-DK03K): the site of the profile's selected
/// state as the live server serves it, the profile the target resolved to, and the generated
/// change summary's page when the build wrote one: `changes.html` for a review route,
/// `changes-since.html` for canon.
pub struct ProfileBuild {
    pub live: LiveBuild,
    pub profile: rdy_engine::publication::ResolvedProfile,
    pub changes: Option<&'static str>,
}

pub use crate::profile_live::{ProfileHistoryMemo, build_profile_live};

/// The live build of a built site whose state captured redaction epoch `epoch`: its files and
/// slots, with the revision of each page and of the label and the header.
pub(crate) fn of_site(site: SiteFiles, epoch: u64) -> LiveBuild {
    let SiteFiles {
        files,
        label,
        header,
        label_at,
        summary,
    } = site;
    let revs = files
        .iter()
        .filter(|(path, bytes)| path.ends_with(".html") && !is_redirect(bytes))
        .map(|(path, bytes)| {
            let at = label_at.get(path).map_or(&[][..], Vec::as_slice);
            (path.clone(), page_rev(bytes, at))
        })
        .collect();
    let label_rev = label_rev(&label, &header);
    LiveBuild {
        files,
        revs,
        label,
        header,
        label_rev,
        label_at,
        warnings: summary.warnings,
        epoch,
    }
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
/// view's name, head op, effective base and owner, whom the review bar names; a version's
/// landing or a proposal version's hash),
/// canon's head (a landing rebases a view and moves every label), the versions, the pack
/// generation, the project's name and description, and the disagreements list (a dismissal or a
/// judged tension changes canon's page and moves no head). The list is empty for a view and a
/// proposal version, so only canon (and a version at canon's head) holds entries. The key also
/// holds the redaction epoch the state captured: no build is reused across a removal, even where
/// the scope's ids are unchanged.
pub fn inputs_key(st: &ReadState, scope: &Scope, pack_generation: u64) -> Result<Hash, RdyError> {
    rdy_engine::refuse_inflight_export(scope)?;
    let mut k = Key(Vec::new());
    k.num(st.redaction_epoch());
    match scope {
        Scope::Canon => {
            k.num(0).hash(st.canon_head());
        }
        Scope::View(name) => {
            let head = st.view_head(name);
            k.num(1)
                .text(name)
                .hash(head.map(|h| h.head))
                .hash(st.effective_base(name))
                .opt_text(
                    head.and_then(|h| h.owner.as_ref())
                        .map(|o| o.actor.as_str()),
                );
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
    /// The new label, when the label or the header changed (the new header is `new.header`).
    pub label: Option<String>,
}

/// The pages that changed from `old` to `new`, and the new label when it or the header changed.
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

/// The bytes the server sends for `path`, or none for a path the site does not have. The
/// version label and the header go into exactly the ranges the build recorded (the label escaped
/// in an HTML page and on one line in `llms.txt` and `llms-full.txt`, as the static build writes
/// them; the header as it is), last range first, and nothing else is searched or changed. An
/// HTML page also gets the live script tag right before its first `</head>`. Any other file is
/// sent as it is. The script's event query is `target`'s ([`SiteTarget::events_query`]), then
/// the page, its revision and the label's, so the stream it opens is of the target it was
/// served for.
pub fn page(build: &LiveBuild, path: &str, target: &SiteTarget) -> Option<Vec<u8>> {
    let mut bytes = build.files.get(path)?.clone();
    let html = path.ends_with(".html");
    if let Some(ranges) = build.label_at.get(path) {
        // as the static build writes it: escaped in a page, on one line in the llms files
        let text = if html {
            escape(&build.label)
        } else {
            crate::llms::one_line(&build.label)
        };
        for (kind, r) in ranges.iter().rev() {
            let fill = match kind {
                SlotKind::Label => &text,
                SlotKind::Header => &build.header,
            };
            bytes.splice(r.clone(), fill.bytes());
        }
    }
    // a permalink page has no revision and takes no script
    if let (true, Some(rev)) = (html, build.revs.get(path)) {
        // the query's values are encoded: only its separators need the attribute's escape
        let tag = format!(
            "<script src=\"/_rdy/live.js\" data-events=\"/_rdy/events?{}&amp;page={}&amp;rev={}&amp;label={}\" defer></script>",
            target.events_query().replace('&', "&amp;"),
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

#[cfg(test)]
mod sr5 {
    use super::*;
    use rdy_engine::{Rdy, Session, WriteRequest};
    use rdy_model::Role;
    use serde_json::json;

    fn session(role: Role) -> Session {
        Session::human("human:qa".into(), role).unwrap()
    }

    fn write(r: &mut Rdy, view: &str, ops: serde_json::Value) {
        r.write(
            &session(Role::Writer),
            WriteRequest {
                view: view.into(),
                ops: serde_json::from_value(ops).expect("ops"),
                justification: None,
                message: String::new(),
            },
        )
        .expect("write");
    }

    /// Review Focus 3: every input of the header is in the key. A tag moves it (the pill and the
    /// landings chip); a commit to another view does not move canon's; a change of a view's
    /// owner moves the view's (the review bar names the owner, O13).
    #[test]
    fn sr5_the_key_covers_the_header() {
        let dir = tempfile::tempdir().expect("tempdir");
        rdy_engine::testkit::generate_tree(dir.path(), 1, 1, 1);
        let mut r = Rdy::open(dir.path()).expect("open");
        let p = r
            .propose(
                &session(Role::Writer),
                "work",
                "first",
                Some("test summary"),
            )
            .expect("propose");
        r.land(&session(Role::Approver), &p.id).expect("land");
        let key = |r: &Rdy, s: &Scope| inputs_key(&r.read(), s, 0).expect("key");
        let canon = key(&r, &Scope::Canon);
        r.tag(&session(Role::Approver), "v1.0.0", None, None)
            .expect("tag");
        let tagged = key(&r, &Scope::Canon);
        assert_ne!(tagged, canon, "a new tag moves the key");
        write(
            &mut r,
            "a",
            json!([{"op": "create", "type": "doc", "fields": {"name": "elsewhere", "title": "Elsewhere", "doc_type": "overview"}}]),
        );
        assert_eq!(
            key(&r, &Scope::Canon),
            tagged,
            "a commit to view a keeps canon's key"
        );
        let a = Scope::View("a".into());
        let before = key(&r, &a);
        let owner = rdy_engine::view_api::OwnerInput {
            actor: "agent:claude".into(),
            run: None,
        };
        r.view_own(&session(Role::Writer), "a", Some(&owner))
            .expect("own");
        assert_ne!(key(&r, &a), before, "a change of owner moves view a's key");
    }
}
