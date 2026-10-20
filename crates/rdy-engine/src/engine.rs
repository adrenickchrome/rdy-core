//! The one door to a project: reads from the published state, writes through a closure that
//! runs against `&mut Rdy` on a single writer, in arrival order.
use crate::semantic::lease::LeaseRegistry;
use crate::site_scope::{CHANGES_PAGE, SINCE_PAGE, ScopeRef, proposal_ref, summary_page_taken};
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
    /// The link to the project's live site. An engine that runs without a daemon has none.
    fn site(&self) -> Result<SiteLink, RdyError> {
        Err(RdyError::new(
            E_SITE,
            "the live site is served by the daemon, and this session runs without one",
            "drop --no-daemon; for a static site, run rdy render --out <dir>",
        ))
    }
    /// The render's warnings for the live site's pages of `scope`, as `rdy render` gives them
    /// for the same scope. The scope is built first when the site has no build of it. An engine
    /// with no site has none.
    fn site_warnings(&self, _scope: &ScopeRef) -> Result<Vec<String>, RdyError> {
        Ok(Vec::new())
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

/// The reply of `rdy_site` and the `site_url` op: the link to the live site at `scope` (`canon`
/// when none), as `{url, scope}`, plus `lan_url` and a `W_SITE_LAN` `warning` while the site is
/// open to the network (or could not be), and the render's `warnings` for the scope's pages (as
/// `rdy render` gives them; the scope is built first when it has no build). A view or proposal
/// version opens in review mode. The link and the scope are spelled as the site reads them
/// ([`ScopeRef`]): one spelling per scope. A tag (`tag:<version>`) opens its frozen site. A view,
/// a proposal version, or canon with landings since its tag also gets `changes` (and
/// `lan_changes` beside `lan_url`): the link to the scope's diff summary.
pub fn site_reply(engine: &dyn Engine, scope: Option<&str>) -> Result<serde_json::Value, RdyError> {
    let link = engine.site()?;
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
    let path = scope.path_prefix();
    let mut v = serde_json::json!({"url": format!("{}{path}", link.url), "scope": scope.text()});
    if let Some(lan) = &link.lan_url {
        v["lan_url"] = serde_json::json!(format!("{lan}{path}"));
    }
    // the link to the scope's diff summary: a view or proposal changes some blocks; canon
    // changed some since its tag (F7). Only when the site has the summary: a top-level doc that
    // owns its path keeps its page, and the render warns (`summary_page_taken`, the renderer's
    // rule too)
    let owned = |page: &str| -> Result<bool, RdyError> {
        Ok(summary_page_taken(&*st.materialized(&resolved)?, page))
    };
    let changes_page = match &scope {
        ScopeRef::View(_) | ScopeRef::Proposal(_) => {
            (!owned(CHANGES_PAGE)?).then(|| format!("{path}{CHANGES_PAGE}"))
        }
        ScopeRef::Canon => {
            let pos = st.version_position(&Scope::Canon)?;
            (pos.version.is_some() && pos.landings_since > 0 && !owned(SINCE_PAGE)?)
                .then(|| SINCE_PAGE.to_string())
        }
        ScopeRef::Tag(_) | ScopeRef::Versions => None,
    };
    if let Some(page) = changes_page {
        v["changes"] = serde_json::json!(format!("{}{page}", link.url));
        if let Some(lan) = &link.lan_url {
            v["lan_changes"] = serde_json::json!(format!("{lan}{page}"));
        }
    }
    if let Some(w) = &link.warning {
        v["warning"] = serde_json::json!({"code": W_SITE_LAN, "message": w.message, "fix": w.fix});
    }
    let warnings = engine.site_warnings(&scope)?;
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
