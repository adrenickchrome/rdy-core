//! The one door to a project: reads from the published state, writes through a closure that
//! runs against `&mut Rdy` on a single writer, in arrival order.
use crate::publication::{IMPLICIT_ORG, ResolvedProfile, profile_at};
use crate::semantic::lease::LeaseRegistry;
use crate::site_scope::{ScopeRef, proposal_ref};
use crate::site_target::SiteTarget;
use crate::{BriefRegistry, E_SITE, OpenReport, Rdy, RdyError, ReadState, Scope, W_SITE_LAN};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

/// A write: runs against the project on the writer and returns its result as JSON.
pub type WriteFn = Box<dyn FnOnce(&mut Rdy) -> Result<serde_json::Value, RdyError> + Send>;

pub trait Engine: Send + Sync {
    /// The current published state. After `write` returns, it includes that write.
    fn read(&self) -> Arc<ReadState>;
    /// Runs `f` serialized with every other write and returns after the state is published.
    fn write(&self, f: WriteFn) -> Result<serde_json::Value, RdyError>;
    /// What opening the store did.
    fn open_report(&self) -> OpenReport;
    fn root(&self) -> PathBuf;
    /// The project's briefing registry (one per open project).
    fn briefs(&self) -> Arc<BriefRegistry>;
    /// The project's lease registry for review requests (one per open project).
    fn leases(&self) -> Arc<LeaseRegistry>;
    /// How many times the loaded pack changed (a reload, or a new or fixed pack error); `0` for
    /// an engine that never reloads.
    fn pack_generation(&self) -> u64 {
        0
    }
    /// Why the pack files on disk do not load, while they do not; the previous pack stays loaded.
    fn pack_error(&self) -> Option<String> {
        None
    }
    /// The link to the live site that serves `target`: the full site's listener for a source or
    /// profile target, and the bound profile-only listener for [`SiteTarget::ProfileOnly`]
    /// (opened on the first ask). An engine that runs without a daemon has none.
    fn site(&self, _target: &SiteTarget) -> Result<SiteLink, RdyError> {
        Err(no_daemon_site())
    }
    /// The facts of the build that serves `target`, all from one captured build (the target is
    /// built first when the site has no build of it). An engine with no site has none.
    fn site_facts(&self, _target: &SiteTarget) -> Result<SiteFacts, RdyError> {
        Err(no_daemon_site())
    }
    /// Prints the docs (Task 11). An engine with no renderer refuses.
    fn export(&self, _req: ExportRequest) -> Result<serde_json::Value, RdyError> {
        Err(RdyError::new(
            crate::E_BAD_INPUT,
            "rdy_export runs in the daemon, and this session runs without one",
            "drop --no-daemon, or run rdy export --out <path> --format <pdf|html|md>",
        ))
    }
    /// Renders a static site (an output seam like [`Engine::export`]). An engine with no
    /// renderer refuses.
    fn render(&self, _req: RenderRequest) -> Result<serde_json::Value, RdyError> {
        Err(RdyError::new(
            crate::E_BAD_INPUT,
            "rdy_render runs in the daemon, and this session runs without one",
            "drop --no-daemon, or run rdy render --out <path>",
        ))
    }
    /// Publishes a tag to a publish remote's hub site as a candidate (an output seam like
    /// [`Engine::render`]): the daemon runs the coordinator off its writer. An engine with no
    /// daemon refuses.
    fn publish(
        &self,
        _req: crate::hub_publish::PublishRequest,
        _session: &crate::Session,
    ) -> Result<serde_json::Value, RdyError> {
        Err(RdyError::new(
            crate::E_BAD_INPUT,
            "rdy_publish runs in the daemon, and this session runs without one",
            "drop --no-daemon, or run rdy publish <tag> --to <remote> --site <site>",
        ))
    }
}

/// `E_SITE` for an engine that runs without a daemon: the live site is the daemon's.
fn no_daemon_site() -> RdyError {
    RdyError::new(
        E_SITE,
        "the live site is served by the daemon, and this session runs without one",
        "drop --no-daemon; for a static site, run rdy render --out <dir>",
    )
}

/// What `rdy_render` asks for, as its parameters give it.
#[derive(Clone, Debug)]
pub struct RenderRequest {
    /// The directory to write, resolved by the caller.
    pub out: PathBuf,
    /// The scope's text; canon when none.
    pub scope: Option<String>,
    /// A publication profile (an id, a key, or `implicit:org`); the full source when none.
    pub profile: Option<String>,
    /// The asking session's view, as for [`ExportRequest::view`].
    pub view: Option<String>,
}

/// What `rdy_export` asks for, as its parameters give it; the daemon parses `format` and `layout`.
#[derive(Clone, Debug)]
pub struct ExportRequest {
    /// The path to write, resolved by the caller.
    pub out: PathBuf,
    /// `pdf`, `html` or `md`.
    pub format: String,
    /// `mkdocs` or `techdocs`, with `md` only.
    pub layout: Option<String>,
    /// The scope's text; canon when none.
    pub scope: Option<String>,
    /// A publication profile (an id, a key, or `implicit:org`); the full source when none.
    pub profile: Option<String>,
    /// The asking session's view. A scope naming it needs no record yet (the session's first
    /// write makes one), as for every read (`ReadState::resolve_read_scope`); another view with
    /// no record is not found.
    pub view: Option<String>,
}

/// Where the live site listens (D-N5NSN, D-FV6G9).
#[derive(Clone, Debug)]
pub struct SiteLink {
    /// The loopback address, `http://127.0.0.1:<port>/`.
    pub url: String,
    /// The LAN address, while `serve.site_lan` is on and bound.
    pub lan_url: Option<String>,
    /// The LAN warning, in plain words.
    pub warning: Option<SiteWarning>,
}

/// A `W_SITE_LAN` warning: what is true of the site's LAN address, and what to do about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SiteWarning {
    pub message: String,
    pub fix: String,
}

/// The fix of the `W_SITE_LAN` warning while the site is open to the network.
pub const SITE_LAN_FIX: &str = "set serve.site_lan to false to keep the site on this machine";

/// The facts of the build behind a live site link, from the one build the daemon captured: the
/// profile it resolved to (a profile target only), the change summary page it generated, and
/// the render's warnings with the reference warning first.
#[derive(Clone, Debug)]
pub struct SiteFacts {
    pub profile: Option<ResolvedProfile>,
    /// The summary page's file name (`changes.html` or `changes-since.html`), if the build has
    /// one and no top-level doc owns it.
    pub changes: Option<&'static str>,
    /// The warnings of the build, as `rdy render` gives them: the text and its fix in one string.
    pub warnings: Vec<String>,
}

/// The caveat of every profile-only site, for the profile titled `title`.
pub fn profile_only_caveat(title: &str) -> SiteWarning {
    SiteWarning {
        message: format!(
            "this site shows only the profile \"{title}\"; anyone with the project still reads everything"
        ),
        fix: "give the full site's link from rdy site when the person needs everything".to_string(),
    }
}

/// The LAN warning of a profile-only listener whose own diagnostic is `listener` (and whose LAN
/// address is `lan_url`, while bound), for the profile titled `title`: a bound address names
/// the profile, and a failed setup prefixes its message with the profile.
pub fn profile_only_lan_warning(
    listener: &SiteWarning,
    lan_url: Option<&str>,
    title: &str,
) -> SiteWarning {
    match lan_url {
        Some(url) => SiteWarning {
            message: format!("anyone on your network can read the profile \"{title}\" at {url}"),
            fix: SITE_LAN_FIX.to_string(),
        },
        None => SiteWarning {
            message: format!("profile \"{title}\": {}", listener.message),
            fix: listener.fix.clone(),
        },
    }
}

/// The reply of `rdy_site` and the `site_url` op: the link to the live site at `scope` (`canon`
/// when none), for the whole source or for one publication `profile` of that scope (an id, a key
/// or `implicit:org`), or, with `only`, for the profile-only site of canon.
///
/// The reply is `{url, scope}`, plus `profile` (`{key, title, audience}`, the folded key in the
/// URL) for a profile, `profile_only: true` in the profile-only mode, `lan_url` and a
/// `W_SITE_LAN` `warning` while the site is open to the network (or could not be), the build's
/// `warnings` (as `rdy render` gives them; the scope is built first when it has no build), and
/// `changes` (and `lan_changes` beside `lan_url`): the link to the page that summarizes what the
/// scope changes. A view or proposal version opens in review mode. The link and the scope are
/// spelled as the site reads them ([`ScopeRef`]): one spelling per scope. A tag
/// (`tag:<version>`) opens its frozen site.
///
/// `only` needs a profile and the canon scope, checked before anything is resolved or bound. The
/// profile is resolved in the requested scope before its route is made, and the reply's profile
/// facts, warnings and LAN wording come from the build the daemon captured for the call.
pub fn site_reply(
    engine: &dyn Engine,
    scope: Option<&str>,
    profile: Option<&str>,
    only: bool,
) -> Result<serde_json::Value, RdyError> {
    if only && (profile.is_none() || scope.is_some_and(|s| s != "canon")) {
        return Err(RdyError::new(
            crate::E_BAD_INPUT,
            "a profile-only site needs a profile and shows canon only",
            "pass profile with scope canon, or leave out only",
        ));
    }
    let text = scope.unwrap_or("canon");
    let st = engine.read();
    let resolved = st.resolve_read_scope(text, None)?;
    let scope = match resolved.clone() {
        Scope::Canon => ScopeRef::Canon,
        Scope::View(name) if name == "." || name == ".." => {
            return Err(RdyError::new(
                crate::E_BAD_INPUT,
                format!(
                    "a view named {name:?} has no link on the live site: a browser reads that name as a step in the path"
                ),
                format!("render the view with rdy render --scope view:{name} --out <dir>"),
            ));
        }
        Scope::View(name) => ScopeRef::View(name),
        Scope::Proposal(_) => {
            // `P-x` follows the proposal's latest version; any other form names one version
            let given = text.strip_prefix("proposal:").unwrap_or(text);
            let r = crate::gate::resolve_in(&st, given)?;
            let spelled = if given.starts_with("P-") && !given.contains('@') {
                format!("P-{}", r.stable.short())
            } else {
                r.to_string()
            };
            ScopeRef::Proposal(proposal_ref(&spelled).unwrap_or(spelled))
        }
        // `landing:` reads as `Scope::At` too: tell a tag from it by the scope text
        Scope::At(_) if text.starts_with("tag:") => {
            let want = crate::version::parse_version(&text["tag:".len()..])?;
            // the stored name, so that `tag:1.0.0` and `tag:v1.0.0` give one link
            let stored = st
                .version_tags()?
                .into_iter()
                .find(|(v, _)| v.cmp_precedence(&want) == std::cmp::Ordering::Equal)
                .map(|(_, t)| t.name);
            // every stored version is a version name, so it has a link (ruling task-2); the scope
            // resolved, so the tag exists unless it was removed since
            let Some(stored) = stored else {
                return Err(RdyError::new(
                    crate::E_BAD_INPUT,
                    format!("no version matches {text:?}"),
                    "pass a version that exists as tag:<version>",
                ));
            };
            // a stored name over the site's bound (only an older release could store one) has
            // no page (ruling final (3))
            if !crate::site_scope::tag_name_ok(&stored) {
                return Err(RdyError::new(
                    crate::E_BAD_INPUT,
                    format!(
                        "this version's name is longer than the site serves ({} bytes)",
                        crate::MAX_VERSION_BYTES
                    ),
                    "open it with rdy show, or read it in the versions list",
                ));
            }
            ScopeRef::Tag(stored)
        }
        Scope::At(_) | Scope::Inflight => {
            return Err(RdyError::new(
                crate::E_BAD_INPUT,
                format!("the site shows canon, a view or a proposal version, not {text:?}"),
                "pass canon, a version as tag:<version>, a view name, view:<name> or proposal:P-x@n",
            ));
        }
    };
    let target = match profile {
        None => SiteTarget::Source(scope),
        Some(name) => {
            // in the requested scope, before any route is made or listener is bound; an id
            // becomes the folded key of the profile, which the URL and the cache read
            let found = profile_at(&st, &resolved, name)?;
            let key = found
                .key
                .as_deref()
                .map_or_else(|| IMPLICIT_ORG.to_string(), rdy_model::key::fold);
            if only {
                SiteTarget::ProfileOnly { key }
            } else {
                SiteTarget::Profile { scope, key }
            }
        }
    };
    let link = engine.site(&target)?;
    let facts = engine.site_facts(&target)?;
    site_reply_value(&target, link, facts)
}

/// The reply of [`site_reply`] for a link and the facts of the build behind it: the one
/// serialization, so a foreground start prints what a tool call returns. It resolves nothing,
/// reads no state and opens no listener. `target` is a source or a profile target.
pub fn site_reply_value(
    target: &SiteTarget,
    link: SiteLink,
    facts: SiteFacts,
) -> Result<serde_json::Value, RdyError> {
    target.validate()?;
    let (scope, key, only) = match target {
        SiteTarget::Source(scope) if *scope != ScopeRef::Versions => (scope.clone(), None, false),
        SiteTarget::Profile { scope, key } => (scope.clone(), Some(key), false),
        SiteTarget::ProfileOnly { key } => (ScopeRef::Canon, Some(key), true),
        SiteTarget::Source(_) | SiteTarget::Menu { .. } => {
            return Err(RdyError::new(
                crate::E_BAD_INPUT,
                "invalid live site target",
                "use a source scope and a publication profile key",
            ));
        }
    };
    let root = target.root();
    let path = root.trim_start_matches('/');
    let url = format!("{}{path}", link.url);
    let mut v = serde_json::json!({"url": url, "scope": scope.text()});
    if let Some(lan) = &link.lan_url {
        v["lan_url"] = serde_json::json!(format!("{lan}{path}"));
    }
    let mut title = None;
    if let Some(key) = key {
        let p = facts.profile.as_ref().ok_or_else(|| {
            RdyError::new(
                crate::E_INTERNAL,
                "a profile site was built with no profile",
                "report this as a bug in rdy",
            )
        })?;
        v["profile"] =
            serde_json::json!({"key": key, "title": p.title, "audience": p.audience.as_str()});
        if only {
            v["profile_only"] = serde_json::json!(true);
        }
        title = Some(p.title.as_str());
    }
    // the link to the scope's diff summary, when the build generated the page; a profile-only
    // site shows canon alone and has none
    if let Some(page) = facts.changes.filter(|_| !only) {
        v["changes"] = serde_json::json!(format!("{url}{page}"));
        if let Some(lan) = &link.lan_url {
            v["lan_changes"] = serde_json::json!(format!("{lan}{path}{page}"));
        }
    }
    let mut warnings = facts.warnings;
    if let Some(w) = &link.warning {
        let w = match title.filter(|_| only) {
            Some(title) => profile_only_lan_warning(w, link.lan_url.as_deref(), title),
            None => w.clone(),
        };
        v["warning"] = serde_json::json!({"code": W_SITE_LAN, "message": w.message, "fix": w.fix});
    }
    if let Some(title) = title.filter(|_| only) {
        let c = profile_only_caveat(title);
        warnings.push(format!("{} {}", c.message, c.fix));
    }
    if !warnings.is_empty() {
        v["warnings"] = serde_json::json!(warnings);
    }
    Ok(v)
}

/// The notice for a session that last saw pack generation `*seen`: `pack_changed` once per new
/// generation (unless the pack is now in error), then `pack_error` on every call while `error`
/// stands. Updates `*seen`.
pub fn notice(seen: &mut u64, generation: u64, error: Option<&str>) -> Option<String> {
    let mut parts = vec![];
    if *seen != generation {
        *seen = generation;
        if error.is_none() {
            parts.push(format!(
                "pack_changed: the pack was reloaded (generation {generation})"
            ));
        }
    }
    if let Some(e) = error {
        parts.push(format!(
            "pack_error: {e}; the previous pack stays loaded until the file is fixed"
        ));
    }
    (!parts.is_empty()).then(|| parts.join("; "))
}

/// `f` as a `WriteFn` whose `T` is serialized to JSON.
pub fn write_typed<T: serde::Serialize + 'static>(
    e: &dyn Engine,
    f: impl FnOnce(&mut Rdy) -> Result<T, RdyError> + Send + 'static,
) -> Result<serde_json::Value, RdyError> {
    e.write(Box::new(move |r| {
        let v = f(r)?;
        serde_json::to_value(v).map_err(|e| {
            RdyError::new(
                crate::E_INTERNAL,
                format!("a write result did not serialize: {e}"),
                "report this as a bug in rdy",
            )
        })
    }))
}

/// Runs `f` on the writer, serialized with every other write, and returns its owned result once
/// the write has succeeded (its batch sealed and the state published). The result travels
/// through a one-shot channel while the write itself returns JSON null: nothing is serialized,
/// and no writer guard outlives the call. A seal that fails after `f` returned `Ok` drops the
/// result and returns the seal's error.
pub fn with_writer_result<T: Send + 'static>(
    engine: &dyn Engine,
    f: impl FnOnce(&mut Rdy) -> Result<T, RdyError> + Send + 'static,
) -> Result<T, RdyError> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<T>(1);
    engine.write(Box::new(move |r| {
        let v = f(r)?;
        // the receiver lives until the write returns: a full or closed channel cannot happen
        let _ = tx.send(v);
        Ok(serde_json::Value::Null)
    }))?;
    rx.try_recv().map_err(|_| {
        RdyError::new(
            crate::E_INTERNAL,
            "a write returned without its result",
            "report this as a bug in rdy",
        )
    })
}

/// The in-process engine: a mutex around the project. Used by `--no-daemon`, `rdy mcp
/// --no-daemon` and tests.
pub struct Local(Mutex<Rdy>);

impl Local {
    pub fn new(rdy: Rdy) -> Local {
        Local(Mutex::new(rdy))
    }

    pub fn open(project: &Path) -> Result<Local, RdyError> {
        Ok(Local::new(Rdy::open(project)?))
    }

    /// A write that panicked may have left the caches half-updated: a poisoned lock is
    /// recovered with an `invalidate` (the state is republished from the store) and the poison
    /// is cleared, so the next lock pays nothing.
    fn lock(&self) -> MutexGuard<'_, Rdy> {
        match self.0.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                let mut g = poisoned.into_inner();
                g.invalidate();
                self.0.clear_poison();
                g
            }
        }
    }

    /// Closes the project's store cleanly.
    pub fn close(self) -> Result<(), RdyError> {
        let r = match self.0.into_inner() {
            Ok(r) => r,
            Err(poisoned) => {
                let mut r = poisoned.into_inner();
                r.invalidate();
                r
            }
        };
        r.close()
    }

    /// Runs `read` on the project under the lock, with an immutable borrow, and returns its
    /// owned result. It neither seals nor publishes: a typed read of what `Engine::read`
    /// (which returns only the published state) cannot reach, such as the store's refs and
    /// `Rdy::fsck`, without the seal a write closure runs after it. A poisoned lock recovers as
    /// every other entry does.
    pub fn inspect<T>(
        &self,
        read: impl FnOnce(&Rdy) -> Result<T, RdyError>,
    ) -> Result<T, RdyError> {
        read(&self.lock())
    }

    /// Checkpoints if writes are waiting and the store has been idle long enough.
    pub fn checkpoint_if_idle(&self) -> Result<bool, RdyError> {
        self.lock().checkpoint_if_idle()
    }
}

impl Engine for Local {
    fn read(&self) -> Arc<ReadState> {
        self.lock().read()
    }

    /// Runs `f`, then seals the batch it left open (`seal_open`), with its moves. If `f`
    /// returned `Ok` and the seal fails, the seal's error is the write's.
    fn write(&self, f: WriteFn) -> Result<serde_json::Value, RdyError> {
        let mut r = self.lock();
        let out = f(&mut r);
        let sealed = r.seal_open();
        match (out, sealed) {
            (Ok(_), Err(e)) => Err(e),
            (out, _) => out,
        }
    }

    fn open_report(&self) -> OpenReport {
        self.lock().open_report()
    }

    fn root(&self) -> PathBuf {
        self.lock().root().to_path_buf()
    }

    fn briefs(&self) -> Arc<BriefRegistry> {
        self.lock().briefs()
    }

    fn leases(&self) -> Arc<LeaseRegistry> {
        self.lock().leases()
    }
}

/// The one JSON shape of an open report, for the MCP overview and the ctl `status` and `fsck`.
pub fn open_report_json(r: &OpenReport) -> serde_json::Value {
    serde_json::json!({
        "quarantined": r.quarantined.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        "unclean_shutdown": r.unclean_shutdown,
        "redb_repaired": r.redb_repaired,
        "index_rebuilt": r.index_rebuilt,
        "caught_up_frames": r.caught_up_frames,
        "catch_up_commits": r.catch_up_commits,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{E_BAD_INPUT, E_NOT_FOUND, Session, WriteRequest};
    use rdy_model::Role;
    use serde_json::{Value, json};
    use std::collections::BTreeMap;

    const ROOT: &str = "http://127.0.0.1:51000/";
    const ONLY_ROOT: &str = "http://127.0.0.1:52000/";
    const CAVEAT: &str = "this site shows only the profile \"Guide book\"; anyone with the project still reads everything give the full site's link from rdy site when the person needs everything";

    fn writer() -> Session {
        Session::human("human:qa".into(), Role::Writer)
    }

    fn approver() -> Session {
        Session::human("human:qa".into(), Role::Approver)
    }

    fn put(r: &mut Rdy, view: &str, ops: Value) -> BTreeMap<String, String> {
        r.write(
            &writer(),
            WriteRequest {
                view: view.into(),
                ops: crate::parse_ops(ops.as_array().unwrap().clone()).unwrap(),
                justification: None,
                message: String::new(),
            },
        )
        .unwrap_or_else(|e| panic!("write to {view}: {e}"))
        .created
    }

    fn land(r: &mut Rdy, view: &str) {
        let p = r.propose(&writer(), view, view, None).unwrap();
        r.land(&approver(), &p.id).unwrap();
    }

    /// Canon holds a doc `intro` and the profiles `Guide` (org, "Guide book") and `pub` (public,
    /// "Public book"); tag `v1.0.0` is canon. View `work` edits the doc; proposal `prop`
    /// (`pref` is its first version) edits it again. `guide` is the id of `Guide`.
    struct World {
        _dir: tempfile::TempDir,
        engine: Local,
        guide: String,
        stable: String,
        pref: String,
    }

    fn world() -> World {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
        let ids = put(
            &mut r,
            "w1",
            json!([
                {"op": "create", "type": "doc", "id": "$intro", "fields": {"name": "intro", "title": "Intro page", "doc_type": "page"}},
                {"op": "create", "type": "prose", "id": "$ip", "parent": "$intro", "body": "INTRO-ONE"},
                {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
            ]),
        );
        land(&mut r, "w1");
        let profile = |key: &str, title: &str, audience: &str| {
            json!({"op": "create", "type": "publication_profile", "id": format!("${key}"),
                "parent": ids["$admin"],
                "fields": {"key": key, "title": title, "description": "About it.",
                    "audience": audience, "select": [ids["$intro"]]}})
        };
        let made = put(
            &mut r,
            "admin",
            json!([
                profile("Guide", "Guide book", "org"),
                profile("pub", "Public book", "public")
            ]),
        );
        land(&mut r, "admin");
        r.tag(&approver(), "v1.0.0", None, None).unwrap();
        put(
            &mut r,
            "work",
            json!([{"op": "update", "id": ids["$ip"], "body": "INTRO-VIEW"}]),
        );
        put(
            &mut r,
            "prop",
            json!([{"op": "update", "id": ids["$ip"], "body": "INTRO-PROP"}]),
        );
        let pref = r.propose(&writer(), "prop", "Prop", None).unwrap().r#ref;
        let stable = pref.split('@').next().unwrap().to_string();
        World {
            _dir: dir,
            engine: Local::new(r),
            guide: made["$Guide"].clone(),
            stable,
            pref,
        }
    }

    type FactsFn =
        Box<dyn Fn(&dyn Engine, &SiteTarget) -> Result<SiteFacts, RdyError> + Send + Sync>;

    /// An engine that serves links of its own: the full site at [`ROOT`], a profile-only
    /// listener at [`ONLY_ROOT`]. It records every target it was asked to open.
    struct Stub {
        inner: Local,
        link: SiteLink,
        only_link: SiteLink,
        facts: FactsFn,
        asked: Mutex<Vec<SiteTarget>>,
    }

    fn plain_link(url: &str) -> SiteLink {
        SiteLink {
            url: url.into(),
            lan_url: None,
            warning: None,
        }
    }

    /// The facts of a plain build: the profile as `target`'s scope holds it, no warnings.
    fn plain_facts(e: &dyn Engine, target: &SiteTarget) -> Result<SiteFacts, RdyError> {
        let st = e.read();
        let scope = match target.scope() {
            ScopeRef::Canon => Scope::Canon,
            ScopeRef::View(v) => Scope::View(v),
            other => st.resolve_read_scope(&other.text(), None)?,
        };
        let profile = match target {
            SiteTarget::Profile { key, .. } | SiteTarget::ProfileOnly { key } => {
                Some(profile_at(&st, &scope, key)?)
            }
            _ => None,
        };
        Ok(SiteFacts {
            profile,
            changes: None,
            warnings: Vec::new(),
        })
    }

    impl Stub {
        fn new(w: World) -> (Stub, tempfile::TempDir) {
            let World { _dir, engine, .. } = w;
            (
                Stub {
                    inner: engine,
                    link: plain_link(ROOT),
                    only_link: plain_link(ONLY_ROOT),
                    facts: Box::new(plain_facts),
                    asked: Mutex::new(Vec::new()),
                },
                _dir,
            )
        }

        fn asked(&self) -> Vec<SiteTarget> {
            self.asked.lock().unwrap().clone()
        }
    }

    impl Engine for Stub {
        fn read(&self) -> Arc<ReadState> {
            self.inner.read()
        }
        fn write(&self, f: WriteFn) -> Result<Value, RdyError> {
            self.inner.write(f)
        }
        fn open_report(&self) -> OpenReport {
            self.inner.open_report()
        }
        fn root(&self) -> PathBuf {
            self.inner.root()
        }
        fn briefs(&self) -> Arc<BriefRegistry> {
            self.inner.briefs()
        }
        fn leases(&self) -> Arc<LeaseRegistry> {
            self.inner.leases()
        }
        fn site(&self, target: &SiteTarget) -> Result<SiteLink, RdyError> {
            self.asked.lock().unwrap().push(target.clone());
            Ok(match target {
                SiteTarget::ProfileOnly { .. } => self.only_link.clone(),
                _ => self.link.clone(),
            })
        }
        fn site_facts(&self, target: &SiteTarget) -> Result<SiteFacts, RdyError> {
            (self.facts)(self, target)
        }
    }

    fn profile_json(key: &str, title: &str, audience: &str) -> Value {
        json!({"key": key, "title": title, "audience": audience})
    }

    /// spv8: a reply has the closed shape of D-CWC91: exactly the keys it carries, each link on
    /// its complete root, the profile as `{key, title, audience}` with the folded key, and
    /// `profile_only: true` for the profile-only mode alone.
    #[test]
    fn spv8_site_reply_has_the_closed_profile_shape() {
        let w = world();
        let (guide, stable, pref) = (w.guide.clone(), w.stable.clone(), w.pref.clone());
        let (e, _dir) = Stub::new(w);
        let book = profile_json("guide", "Guide book", "org");
        // source: no profile key at all
        assert_eq!(
            site_reply(&e, None, None, false).unwrap(),
            json!({"url": ROOT, "scope": "canon"})
        );
        // by key (any case), by id, and the implicit profile, in canon
        for name in ["Guide", "guide", "GUIDE", guide.as_str()] {
            assert_eq!(
                site_reply(&e, None, Some(name), false).unwrap(),
                json!({"url": format!("{ROOT}~profile/guide/"), "scope": "canon", "profile": book}),
                "{name}"
            );
        }
        let v = site_reply(&e, Some("canon"), Some("implicit:org"), false).unwrap();
        assert_eq!(v["url"], format!("{ROOT}~profile/implicit%3Aorg/"), "{v}");
        assert_eq!(v["profile"]["key"], "implicit:org", "{v}");
        assert_eq!(v["profile"]["audience"], "org", "{v}");
        assert_eq!(v.as_object().unwrap().len(), 3, "{v}");
        // a public profile names its audience
        assert_eq!(
            site_reply(&e, None, Some("pub"), false).unwrap(),
            json!({"url": format!("{ROOT}~profile/pub/"), "scope": "canon",
                "profile": profile_json("pub", "Public book", "public")})
        );
        // a view, a tag, and a proposal version: latest and pinned
        assert_eq!(
            site_reply(&e, Some("work"), Some("Guide"), false).unwrap(),
            json!({"url": format!("{ROOT}~view/work/~profile/guide/"), "scope": "view:work",
                "profile": book})
        );
        assert_eq!(
            site_reply(&e, Some("view:work"), Some(&guide), false).unwrap()["url"],
            format!("{ROOT}~view/work/~profile/guide/")
        );
        let tag = site_reply(&e, Some("tag:1.0.0"), Some("guide"), false).unwrap();
        assert_eq!(tag["scope"], "tag:v1.0.0", "{tag}");
        assert_eq!(
            tag["url"],
            format!("{ROOT}~tag/v1.0.0/~profile/guide/"),
            "{tag}"
        );
        assert_eq!(tag["profile"], book, "{tag}");
        let latest = site_reply(
            &e,
            Some(&format!("proposal:{}", stable.to_uppercase())),
            Some("guide"),
            false,
        )
        .unwrap();
        assert_eq!(latest["scope"], format!("proposal:{stable}"), "{latest}");
        assert_eq!(
            latest["url"],
            format!("{ROOT}~proposal/{stable}/~profile/guide/"),
            "{latest}"
        );
        let pinned =
            site_reply(&e, Some(&format!("proposal:{pref}")), Some("guide"), false).unwrap();
        assert_eq!(pinned["scope"], format!("proposal:{pref}"), "{pinned}");
        assert_eq!(
            pinned["url"],
            format!("{ROOT}~proposal/{pref}/~profile/guide/"),
            "{pinned}"
        );
        // profile-only: the listener's own root, canon, the flag, the caveat and nothing else
        let only = site_reply(&e, None, Some("GUIDE"), true).unwrap();
        assert_eq!(
            only,
            json!({"url": ONLY_ROOT, "scope": "canon", "profile": book,
                "profile_only": true, "warnings": [CAVEAT]})
        );
        assert_eq!(
            e.asked().last(),
            Some(&SiteTarget::ProfileOnly {
                key: "guide".into()
            })
        );
        // `only: false` is an ordinary route: no flag, no caveat
        let plain = site_reply(&e, None, Some("guide"), false).unwrap();
        assert!(plain.get("profile_only").is_none() && plain.get("warnings").is_none());
        // a change summary page links beside the url, and a LAN address beside both
        let mut e = e;
        e.facts = Box::new(|e, t| {
            Ok(SiteFacts {
                changes: Some("changes.html"),
                ..plain_facts(e, t)?
            })
        });
        e.link = SiteLink {
            url: ROOT.into(),
            lan_url: Some("http://192.168.1.5:51000/".into()),
            warning: Some(SiteWarning {
                message: "anyone on your network can read these docs at http://192.168.1.5:51000/"
                    .into(),
                fix: SITE_LAN_FIX.into(),
            }),
        };
        let v = site_reply(&e, Some("work"), Some("guide"), false).unwrap();
        assert_eq!(
            v,
            json!({"url": format!("{ROOT}~view/work/~profile/guide/"), "scope": "view:work",
                "profile": book,
                "changes": format!("{ROOT}~view/work/~profile/guide/changes.html"),
                "lan_url": "http://192.168.1.5:51000/~view/work/~profile/guide/",
                "lan_changes": "http://192.168.1.5:51000/~view/work/~profile/guide/changes.html",
                "warning": {"code": "W_SITE_LAN",
                    "message": "anyone on your network can read these docs at http://192.168.1.5:51000/",
                    "fix": SITE_LAN_FIX}})
        );
        // the source route keeps its summary page link
        let v = site_reply(&e, Some("work"), None, false).unwrap();
        assert_eq!(
            v["changes"],
            format!("{ROOT}~view/work/changes.html"),
            "{v}"
        );
        // only-mode never has a change summary, whatever the build holds
        let mut e = e;
        e.only_link = e.link.clone();
        let v = site_reply(&e, None, Some("guide"), true).unwrap();
        assert!(
            v.get("changes").is_none() && v.get("lan_changes").is_none(),
            "{v}"
        );
    }

    /// spv8: `only` is checked before anything is resolved or bound.
    #[test]
    fn spv8_only_validation_precedes_binding() {
        let w = world();
        let (e, _dir) = Stub::new(w);
        let refused = |scope: Option<&str>, profile: Option<&str>| {
            let err = site_reply(&e, scope, profile, true).unwrap_err();
            assert_eq!(err.code, E_BAD_INPUT, "{scope:?} {profile:?}: {err:?}");
            assert_eq!(
                err.message,
                "a profile-only site needs a profile and shows canon only"
            );
            assert_eq!(err.fix, "pass profile with scope canon, or leave out only");
        };
        refused(None, None);
        refused(Some("canon"), None);
        refused(Some("work"), Some("guide"));
        refused(Some("tag:v1.0.0"), Some("guide"));
        refused(Some("proposal:P-x"), Some("guide"));
        refused(Some("nobody"), Some("guide"));
        assert!(e.asked().is_empty(), "nothing was opened");
        // an unknown profile resolves in canon first, and binds nothing
        let err = site_reply(&e, None, Some("nope"), true).unwrap_err();
        assert_eq!(err.code, E_NOT_FOUND, "{err:?}");
        assert_eq!(err.message, "Publication profile \"nope\" was not found.");
        assert_eq!(
            err.fix,
            "Use a publication profile id or key from this scope."
        );
        let err = site_reply(&e, Some("work"), Some("nope"), false).unwrap_err();
        assert_eq!(err.code, E_NOT_FOUND, "{err:?}");
        assert!(e.asked().is_empty(), "nothing was opened");
        // false (or absent) is an ordinary route; `canon` spelled out is canon
        let v = site_reply(&e, Some("work"), Some("guide"), false).unwrap();
        assert!(v.get("profile_only").is_none(), "{v}");
        let v = site_reply(&e, Some("canon"), Some("guide"), true).unwrap();
        assert_eq!(v["profile_only"], true, "{v}");
        assert_eq!(e.asked().len(), 2);
    }

    /// spv8: the warnings come in one order with exact text: the build's own (the reference
    /// warning first, then the render's), then the profile-only caveat, once. The LAN warning
    /// names the profile's title as the build gave it.
    #[test]
    fn spv8_reply_warnings_have_one_order_and_exact_text() {
        let w = world();
        let (mut e, _dir) = Stub::new(w);
        let blocked = "This public profile links to Hidden page, which it does not show. Select the page, or remove the link.";
        let render = "the top-level doc named glossary has no page: glossary.html is the site's generated glossary. Rename the doc to publish it.";
        e.facts = Box::new(move |e, t| {
            Ok(SiteFacts {
                warnings: vec![blocked.to_string(), render.to_string()],
                ..plain_facts(e, t)?
            })
        });
        // a route: the build's warnings as they are, no caveat
        let v = site_reply(&e, None, Some("pub"), false).unwrap();
        assert_eq!(v["warnings"], json!([blocked, render]), "{v}");
        // only-mode: the caveat comes last, once, on every call (the listener already open too)
        for _ in 0..2 {
            let v = site_reply(&e, None, Some("guide"), true).unwrap();
            assert_eq!(v["warnings"], json!([blocked, render, CAVEAT]), "{v}");
        }
        // an org profile's route warns of nothing it was not given
        e.facts = Box::new(plain_facts);
        let v = site_reply(&e, None, Some("guide"), false).unwrap();
        assert!(
            v.get("warnings").is_none() && v.get("warning").is_none(),
            "{v}"
        );
        // LAN: the full site's warning stays as it is; a profile-only listener names the profile
        let lan = "http://192.168.1.5:52000/";
        e.link = SiteLink {
            url: ROOT.into(),
            lan_url: Some("http://192.168.1.5:51000/".into()),
            warning: Some(SiteWarning {
                message: "anyone on your network can read these docs at http://192.168.1.5:51000/"
                    .into(),
                fix: SITE_LAN_FIX.into(),
            }),
        };
        e.only_link = SiteLink {
            url: ONLY_ROOT.into(),
            lan_url: Some(lan.into()),
            warning: Some(SiteWarning {
                message: format!("anyone on your network can read these docs at {lan}"),
                fix: SITE_LAN_FIX.into(),
            }),
        };
        let v = site_reply(&e, None, Some("guide"), false).unwrap();
        assert_eq!(
            v["warning"],
            json!({"code": "W_SITE_LAN", "fix": SITE_LAN_FIX,
                "message": "anyone on your network can read these docs at http://192.168.1.5:51000/"}),
            "{v}"
        );
        let v = site_reply(&e, None, Some("guide"), true).unwrap();
        assert_eq!(
            v["warning"],
            json!({"code": "W_SITE_LAN", "fix": SITE_LAN_FIX,
                "message": format!("anyone on your network can read the profile \"Guide book\" at {lan}")}),
            "{v}"
        );
        assert_eq!(v["lan_url"], lan, "{v}");
        // a failed LAN setup keeps its fix and names the profile
        e.only_link = SiteLink {
            url: ONLY_ROOT.into(),
            lan_url: None,
            warning: Some(SiteWarning {
                message: "serve.site_lan is on, but this machine has no private network address; the site stays on this machine".into(),
                fix: "connect to a network".into(),
            }),
        };
        let v = site_reply(&e, None, Some("guide"), true).unwrap();
        assert_eq!(
            v["warning"],
            json!({"code": "W_SITE_LAN", "fix": "connect to a network",
                "message": "profile \"Guide book\": serve.site_lan is on, but this machine has no private network address; the site stays on this machine"}),
            "{v}"
        );
        assert!(v.get("lan_url").is_none(), "{v}");
        // a title the build gave later is the one the wording uses
        e.facts = Box::new(|e, t| {
            let mut f = plain_facts(e, t)?;
            f.profile.as_mut().unwrap().title = "Renamed book".into();
            Ok(f)
        });
        let v = site_reply(&e, None, Some("guide"), true).unwrap();
        assert_eq!(v["profile"]["title"], "Renamed book", "{v}");
        assert_eq!(
            v["warning"]["message"],
            "profile \"Renamed book\": serve.site_lan is on, but this machine has no private network address; the site stays on this machine",
            "{v}"
        );
        assert_eq!(
            v["warnings"][0],
            CAVEAT.replace("Guide book", "Renamed book"),
            "{v}"
        );
    }

    /// spv8: an engine with no daemon refuses every call with the base `E_SITE`, whatever the
    /// route, and the pure builder opens nothing.
    #[test]
    fn spv8_no_daemon_keeps_the_existing_site_refusal() {
        let w = world();
        let e = w.engine;
        for (scope, profile, only) in [
            (None, None, false),
            (Some("work"), None, false),
            (None, Some("guide"), false),
            (Some("work"), Some("Guide"), false),
            (None, Some("guide"), true),
        ] {
            let err = site_reply(&e, scope, profile, only).unwrap_err();
            assert_eq!(err.code, E_SITE, "{scope:?} {profile:?} {only}");
            assert_eq!(
                err.message,
                "the live site is served by the daemon, and this session runs without one"
            );
            assert_eq!(
                err.fix,
                "drop --no-daemon; for a static site, run rdy render --out <dir>"
            );
        }
        let target = SiteTarget::Source(ScopeRef::Canon);
        assert_eq!(e.site(&target).unwrap_err().code, E_SITE);
        assert_eq!(e.site_facts(&target).unwrap_err().code, E_SITE);
        drop(w._dir);
    }
}
