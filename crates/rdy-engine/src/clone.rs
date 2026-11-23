//! `rdy clone` (plan Export and clone; D-9QV85, D-D2NPN, D-DAGNB, D-C8C70, RULING F3 and F4): a
//! new project from a local project or a dump, in a directory with no store. A clone is a dump
//! and a load through one stream: its log is the source's log byte for byte, so every hash,
//! landing, tag, view and review is kept, then one origin batch. A clone keeps the source's repo
//! id; a fork takes a new one. Both record where they came from in `meta/origin`.
//!
//! The steps (closed):
//! 1. `File`: a dump whose header names a store format before 12 is refused: it holds no repo id
//!    (O10). The check reads the header of the one stream the load then consumes. `State`: refuse
//!    a target in the source's resolved `.rdy` (final review C1), read the project files, then a
//!    spawned thread loads the pipe's reader while this thread writes the dump into the pipe's
//!    writer (O14).
//! 2. The load's steps 2 to 6, unchanged (`crate::load`).
//! 3. After the open under the load's lock: the loaded store's meta refs pass the store's check
//!    first, so a source that `rdy load` refuses is refused in its words, never written over
//!    (ruling task-5). Then read the loaded store's repo id (the source's), its newest version
//!    and canon's head (O8), and write the origin batch (`Store::put_origin`, a fork with a new
//!    repo id). Then the load's `fsck` and counts.
//! 4. Any failure undoes everything, as the load does.
//! 5. The loader thread is always joined. The clone returns the writer's error when the writer
//!    failed and the loader failed reading the stream; in every other case the loader's (O25).
//!
//! A clone from a hub URL ([`clone_url`], D-D5J18, D-7PYYS, D-47HVF) has its own steps, in
//! this order:
//! 1. The target is absent or an empty directory with no store. The user config holds no share
//!    remote (`E_REMOTE_EXISTS`). Then discovery with the install's fence, and the state step.
//! 2. The hub's `min_rdy` stop (`E_STORE_FORMAT`). A hub repo with no canon cannot be cloned.
//! 3. The settings step, read whole: the pack through the strict pack parser in memory, each
//!    setting by its config rule, each member within the render-input bounds. A member this rdy
//!    cannot read refuses in D-7PYYS's words, which never quote it. A pack whose digest
//!    differs from the pack the clone would use refuses the same way.
//! 4. Only then a new project in a temporary directory beside the target, with the hub's repo
//!    id: its pack file, its settings and the one share remote `hub`. Hub canon's history,
//!    rooted at the captured tip with no base, comes in as a pull fetches it, with its tags;
//!    the pull's second batch moves canon to that tip and records it as fetched and applied.
//! 5. A settings load and `fsck` of the new project, then one rename into the target. Any
//!    refusal or failure removes the temporary directory and leaves the target as it was.
//!
//! No token, no `.rdy/local.yaml` and no user config is written. A URL clone takes no view and
//! never forks.
//!
//! Platform (D-1RXEA): `std::io::pipe` and `std::thread::spawn` only.
use crate::Rdy;
use crate::config_levels::PROJECT_FILE;
use crate::dump::project_files;
use crate::engine::Local;
use crate::error::{
    CLONE_NO_CANON_FIX, CLONE_NO_CANON_MESSAGE, CLONE_SETTING_MESSAGE, CLONE_SETTING_NEWER_FIX,
    CLONE_SETTING_OLDER_FIX, CLONE_SETTING_UNNAMED_FIX, CLONE_SETTING_UNNAMED_MESSAGE,
    CLONE_URL_FORK_FIX, CLONE_URL_FORK_MESSAGE, E_ALREADY_EXISTS, E_BAD_INPUT, E_DUMP, E_INTERNAL,
    E_IO, RdyError, UPSTREAM_PACK_MISMATCH_FIX, UPSTREAM_PACK_MISMATCH_MESSAGE,
    W_UPSTREAM_PACK_MISMATCH, upstream_protocol,
};
use crate::fsck::FsckReport;
use crate::hub_config::{HubConfig, RemoteEntry, RemoteMode};
use crate::hub_http::HttpHubTransport;
use crate::hub_publish::{PublishRenderer, RenderedUpload};
use crate::hub_sync::{SyncService, Target};
use crate::hub_sync_wire::{HttpSyncWire, PackReason, SETTINGS_MEMBERS, SettingsBundle};
use crate::hub_types::{RemoteUrl, SystemHubClock};
use crate::load::{LoadSummary, Opened, inside_source_rdy, load_opened, open_dump, open_stream};
use crate::read_state::ReadState;
use crate::readiness::assessment::pack_digest;
use crate::render_inputs::{
    MAX_DESCRIPTION_BYTES, MAX_INPUT_BYTES, MAX_INPUT_DEPTH, MAX_INPUT_SCALAR_BYTES,
    MAX_INPUT_VALUES, MAX_TITLE_BYTES, RDY_VERSION, RenderInputs,
};
use crate::session::{Scope, Session};
use rdy_model::Hash;
use rdy_model::RepoId;
use rdy_schema::{Pack, PackSet, PackWarning};
use rdy_store::holder::StoreMode;
use rdy_store::layout::STORE_DIR;
use rdy_store::meta::{FIRST_REPO_ID_STORE_FORMAT, Origin};
use serde::Serialize;
use std::io::{BufReader, BufWriter, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// What a clone reads: an open project's published state, or a dump file.
pub enum CloneSource<'a> {
    State(&'a ReadState),
    File(&'a Path),
}

/// The clone source a path names (O7, closed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    File,
    Project,
    /// An `https://` hub URL (D-47HVF).
    Url,
}

/// Where a clone came from, as the replies show it: the source's repo id, its newest version and
/// its canon head, each `null` when absent.
#[derive(Clone, Debug, Serialize)]
pub struct OriginReply {
    pub repo_id: String,
    pub tag: Option<String>,
    pub landing: Option<String>,
}

impl From<&Origin> for OriginReply {
    fn from(o: &Origin) -> Self {
        OriginReply {
            repo_id: o.repo_id.to_string(),
            tag: o.tag.clone(),
            landing: o.landing.map(|h| h.to_string()),
        }
    }
}

/// What `rdy clone` did: the load's summary (its `next` names the clone), the clone's repo id,
/// and its origin: `origin` for a clone, `forked_from` for a fork.
#[derive(Clone, Debug, Serialize)]
pub struct CloneSummary {
    #[serde(flatten)]
    pub load: LoadSummary,
    pub repo_id: String,
    pub fork: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<OriginReply>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forked_from: Option<OriginReply>,
}

/// What the person does after a clone: it carries no working copy (O9).
const NEXT: &str = "run rdy checkout to bind a working copy; a clone carries none";

/// The clone source a path names (O7, D-47HVF): a path that contains `://` is a URL, and only
/// an `https://` one is a hub URL (another gets `rdy remote add`'s refusal of a URL that is not
/// HTTPS); a regular file is a dump; a directory with `.rdy.yaml` and `.rdy/store` is a project;
/// anything else is refused.
pub fn source_kind(path: &Path) -> Result<SourceKind, RdyError> {
    let shown = path.display().to_string();
    if shown.contains("://") {
        RemoteUrl::parse(&shown)?;
        return Ok(SourceKind::Url);
    }
    match std::fs::metadata(path) {
        Ok(m) if m.is_file() => return Ok(SourceKind::File),
        Ok(m)
            if m.is_dir()
                && path.join(PROJECT_FILE).is_file()
                && path.join(".rdy").join(STORE_DIR).is_dir() =>
        {
            return Ok(SourceKind::Project);
        }
        _ => {}
    }
    Err(RdyError::new(
        E_BAD_INPUT,
        format!("{shown} is neither a dump file nor an rdy project"),
        "pass a dump file, or a directory with .rdy.yaml and .rdy/store",
    ))
}

/// Clones `source` into `dir`, a directory with no store. A refusal or failure leaves the target
/// as it was.
pub fn clone(source: CloneSource, dir: &Path, fork: bool) -> Result<CloneSummary, RdyError> {
    let (load, origin, repo_id) = match source {
        CloneSource::File(file) => from_file(file, dir, fork)?,
        CloneSource::State(st) => from_state(st, dir, fork)?,
    };
    let reply = OriginReply::from(&origin);
    let (origin, forked_from) = if fork {
        (None, Some(reply))
    } else {
        (Some(reply), None)
    };
    Ok(CloneSummary {
        load: LoadSummary {
            next: NEXT.to_string(),
            ..load
        },
        repo_id: repo_id.to_string(),
        fork,
        origin,
        forked_from,
    })
}

/// The refusal of a holder as a clone source: a clone is a project operation (D-BPYN8).
fn holder_source() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "a holder dump cannot be cloned as a project",
        "load the dump into a new holder directory",
    )
}

/// What the origin step recorded: the origin and the clone's repo id.
type Recorded = Option<(Origin, RepoId)>;

/// Step 3: the origin batch, written into the loaded project `r`. The load runs this step only
/// after the store's meta check passed, which requires a repo id; a store with none would write
/// nothing, and the load's `fsck` would refuse it.
fn record_origin(r: &mut Rdy, fork: bool) -> Result<Recorded, RdyError> {
    // a dump file of a holder: refused before any ref changes, and the load takes the target back
    if r.store().mode()? == StoreMode::Holder {
        return Err(holder_source());
    }
    let Some(source_id) = r.store().reader().repo_id()? else {
        return Ok(None);
    };
    let st = r.read();
    let origin = Origin {
        fork,
        repo_id: source_id,
        tag: st.versions()?.last().map(|v| v.name.clone()),
        landing: st.canon_head(),
    };
    drop(st);
    let new_id = fork.then(RepoId::new);
    let after = r.store_mut().put_origin(&origin, new_id)?;
    Ok(Some((origin, after)))
}

/// Runs the load of the opened stream (named `source` in its texts) into `dir` with the origin
/// step after the open.
fn load_recording<R: Read>(
    source: &Path,
    opened: Opened<R>,
    dir: &Path,
    fork: bool,
) -> Result<(LoadSummary, Origin, RepoId), RdyError> {
    let mut got: Recorded = None;
    let mut step = |r: &mut Rdy| {
        got = record_origin(r, fork)?;
        Ok(())
    };
    let load = load_opened(source, opened, dir, Some(&mut step))?;
    // the load's fsck requires a repo id, so a load that passed recorded the origin
    let (origin, repo_id) = got.ok_or_else(|| {
        RdyError::new(
            E_INTERNAL,
            format!(
                "the clone of {} passed fsck with no origin recorded",
                source.display()
            ),
            "report this as a bug",
        )
    })?;
    Ok((load, origin, repo_id))
}

/// Step 1 for a dump file: opens it once and reads its header, refuses a dump older than store
/// format 12 (O10), then loads that same stream: a file replaced after the check is never read.
/// A file that does not open or a header that does not read is refused in the load's words.
fn from_file(
    file: &Path,
    dir: &Path,
    fork: bool,
) -> Result<(LoadSummary, Origin, RepoId), RdyError> {
    let opened = open_stream(file, BufReader::new(open_dump(file)?))?;
    if let rdy_store::format::DumpStore::Legacy(format) = opened.header.store
        && format < FIRST_REPO_ID_STORE_FORMAT
    {
        let file = file.display();
        return Err(RdyError::new(
            E_DUMP,
            format!(
                "{file} is a dump of store format {format}, which has no repo id, so rdy clone cannot record its origin"
            ),
            format!("load it with rdy load {file} <dir>, then clone that project"),
        ));
    }
    load_recording(file, opened, dir, fork)
}

/// Step 1 for an open project (O14): the loader runs on a spawned thread that owns the pipe's
/// reader; this thread writes the dump into the pipe's writer. The texts name the project's root
/// (O24).
fn from_state(
    st: &ReadState,
    dir: &Path,
    fork: bool,
) -> Result<(LoadSummary, Origin, RepoId), RdyError> {
    // an open holder: refused before the target or the pipe is touched
    if st.reader().mode()? == StoreMode::Holder {
        return Err(holder_source());
    }
    // the target must not be in the source's own .rdy, wherever a symlink puts it (C1)
    inside_source_rdy(&st.root().join(".rdy"), dir)?;
    let files = project_files(st.root())?;
    let (reader, writer) = std::io::pipe().map_err(|e| {
        RdyError::new(
            E_IO,
            format!("rdy clone cannot open a pipe: {e}"),
            "close other programs or raise the open file limit, then clone again",
        )
    })?;
    let ended = Arc::new(AtomicBool::new(false));
    let seen = Seen {
        r: reader,
        ended: ended.clone(),
    };
    let source: PathBuf = st.root().to_path_buf();
    let target: PathBuf = dir.to_path_buf();
    let loader = std::thread::spawn(move || {
        let opened = open_stream(&source, BufReader::new(seen))?;
        load_recording(&source, opened, &target, fork)
    });
    // the writer is dropped here on every path, so the loader always sees the stream end
    let wrote = st.write_dump(BufWriter::new(writer), &files).map(drop);
    let loaded = match loader.join() {
        Ok(loaded) => loaded,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    match (wrote, loaded) {
        // the loader failed because the stream stopped short: the writer's error says why
        (Err(w), Err(_)) if ended.load(Ordering::SeqCst) => Err(w),
        // every other case is the loader's: a loader that passed read the trailer, so the
        // writer had written it
        (_, loaded) => loaded,
    }
}

/// A reader that notes when its stream ended: at its end of file or at an error.
struct Seen<R> {
    r: R,
    ended: Arc<AtomicBool>,
}

impl<R: Read> Read for Seen<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.r.read(buf);
        let ended = match &n {
            Ok(0) => !buf.is_empty(),
            Ok(_) => false,
            Err(e) => e.kind() != std::io::ErrorKind::Interrupted,
        };
        if ended {
            self.ended.store(true, Ordering::SeqCst);
        }
        n
    }
}

// ---------------------------------------------------------------------------------------------
// A clone from a hub URL (D-D5J18, D-7PYYS, D-47HVF)
// ---------------------------------------------------------------------------------------------

/// The name of a URL clone's one remote: a share remote at the hub URL.
pub const HUB_REMOTE: &str = "hub";
/// The pack a URL clone uses when the hub repo's settings hold none.
const DEFAULT_PACK: &str = "rdy-standard";

/// The settings a URL clone took from the hub repo, and those it left at rdy's defaults, by
/// member name, in the order the clone reads them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ClonedSettings {
    pub hub: Vec<String>,
    pub defaults: Vec<String>,
}

/// A warning of a URL clone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CloneWarning {
    pub code: String,
    pub message: String,
    pub fix: String,
}

/// What a URL clone's fetch took.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ClonedFetch {
    pub records_taken: u64,
    pub tags_taken: Vec<String>,
    pub tags_kept: Vec<String>,
}

/// What `rdy clone <url>` did: the new project, the hub URL in canonical form and its remote,
/// the hub repo's id, the origin and the captured tip, the fetch, the counts, the settings from
/// the hub and those at their defaults, the warnings, `fsck`, and what to do next.
#[derive(Clone, Debug, Serialize)]
pub struct UrlCloneSummary {
    pub project: String,
    pub url: String,
    pub remote: String,
    pub repo_id: String,
    pub fork: bool,
    pub origin: OriginReply,
    pub canon: String,
    pub fetched: ClonedFetch,
    pub landings: usize,
    pub tags: usize,
    pub views: usize,
    pub settings: ClonedSettings,
    pub warnings: Vec<CloneWarning>,
    pub fsck: FsckReport,
    pub next: String,
}

/// A step that a test runs on the new project's directory after the fetch and before the
/// clone's checks.
#[doc(hidden)]
pub type BeforeChecks<'a> = &'a mut dyn FnMut(&Path) -> Result<(), RdyError>;

/// The refusal of a URL clone with `fork` (D-47HVF).
pub fn url_fork() -> RdyError {
    RdyError::new(E_BAD_INPUT, CLONE_URL_FORK_MESSAGE, CLONE_URL_FORK_FIX)
}

/// Clones the hub repo at `url` into `dir` for `session`, with the trust and sign-in of the
/// user config `user_file` (see the module docs). A refusal or failure leaves `dir` as it was.
pub fn clone_url(
    session: &Session,
    url: &str,
    dir: &Path,
    user_file: &Path,
) -> Result<UrlCloneSummary, RdyError> {
    clone_url_checked(session, url, dir, user_file, None)
}

/// [`clone_url`] with `before_checks` run on the new project before its checks.
#[doc(hidden)]
pub fn clone_url_checked(
    session: &Session,
    url: &str,
    dir: &Path,
    user_file: &Path,
    before_checks: Option<BeforeChecks>,
) -> Result<UrlCloneSummary, RdyError> {
    let url = RemoteUrl::parse(url)?;
    check_new_dir(dir, "clone a hub URL into a new directory, or an empty one")?;
    let settings = HubConfig::load(None, Some(user_file))?;
    let api = HttpHubTransport::new(settings.fence(url.origin())?)?;
    let wire = HttpSyncWire::new(HttpHubTransport::new(settings.fence(url.origin())?)?);
    let service = SyncService::for_clone(user_file, &api, &SystemHubClock, &wire, &NoRenderer);
    service.clone_target(session, &url, |t| {
        clone_from(&service, session, t, dir, user_file, before_checks)
    })
}

/// The render-input capture of a URL clone: it renders nothing.
pub(crate) struct NoRenderer;

impl PublishRenderer for NoRenderer {
    fn capture(&self, _: &ReadState, _: &Scope) -> Result<RenderInputs, RdyError> {
        Err(no_render())
    }

    fn render(
        &self,
        _: &ReadState,
        _: &Scope,
        _: &str,
        _: &str,
        _: &RenderInputs,
    ) -> Result<RenderedUpload, RdyError> {
        Err(no_render())
    }
}

fn no_render() -> RdyError {
    RdyError::new(
        E_INTERNAL,
        "a clone renders nothing",
        "report this as a bug",
    )
}

/// The target of a URL clone or a hub fork: absent, or an empty directory with no store,
/// outside any `.rdy`, with no daemon. A target that holds anything else is `E_ALREADY_EXISTS`
/// with `fix`.
pub(crate) fn check_new_dir(dir: &Path, fix: &str) -> Result<(), RdyError> {
    crate::load::check_target(dir, &[])?;
    match std::fs::read_dir(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(RdyError::new(
            E_IO,
            format!("{}: {e}", dir.display()),
            "check the path and its permissions",
        )),
        Ok(mut entries) => match entries.next() {
            None => Ok(()),
            Some(_) => Err(RdyError::new(
                E_ALREADY_EXISTS,
                format!("{} is not empty", dir.display()),
                fix,
            )),
        },
    }
}

/// `E_BAD_INPUT`: hub repo `url` has no canon (D-D5J18).
fn no_canon(url: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        CLONE_NO_CANON_MESSAGE.replace("<url>", url),
        CLONE_NO_CANON_FIX,
    )
}

/// This rdy's version.
fn this_rdy() -> semver::Version {
    semver::Version::parse(RDY_VERSION).expect("the crate version is a version")
}

/// `E_BAD_INPUT`: the `member` of hub repo `url`, captured by rdy `captured`, does not fit this
/// rdy (D-7PYYS). The words name the member and the versions only, never what it holds.
fn unfit(member: &str, url: &str, captured: Option<&semver::Version>) -> RdyError {
    let this = this_rdy().to_string();
    match captured {
        Some(c) => {
            let message = CLONE_SETTING_MESSAGE
                .replace("<member>", member)
                .replace("<url>", url)
                .replace("<captured>", &c.to_string())
                .replace("<this>", &this);
            let fix = if *c > this_rdy() {
                CLONE_SETTING_NEWER_FIX.replace("<captured>", &c.to_string())
            } else {
                CLONE_SETTING_OLDER_FIX.replace("<this>", &this)
            };
            RdyError::new(E_BAD_INPUT, message, fix)
        }
        None => RdyError::new(
            E_BAD_INPUT,
            CLONE_SETTING_UNNAMED_MESSAGE
                .replace("<member>", member)
                .replace("<url>", url)
                .replace("<this>", &this),
            CLONE_SETTING_UNNAMED_FIX.replace("<this>", &this),
        ),
    }
}

/// The settings bundle as the clone read it whole: what it writes, the digest the effective
/// pack must have, and the report.
struct ReadBundle {
    /// The `rdy` that captured the bundle, if it names one.
    captured: Option<semver::Version>,
    pack: String,
    /// The pack file under `.rdy/packs` and its bytes; none for rdy's built-in pack.
    pack_file: Option<(String, Vec<u8>)>,
    digest: Hash,
    coverage_gate: Option<String>,
    coverage_target: Option<u64>,
    decisions_home: Option<String>,
    propose_cadence: Option<String>,
    propose_changes: Option<u64>,
    title: Option<String>,
    description: Option<String>,
    settings: ClonedSettings,
    warnings: Vec<CloneWarning>,
}

/// Whether the member `v` keeps the render-input bounds.
fn within_bounds(v: &serde_json::Value) -> bool {
    let Ok(bytes) = serde_json::to_vec(v) else {
        return false;
    };
    crate::hub_http::check_json(
        &bytes,
        crate::hub_http::JsonLimits {
            bytes: MAX_INPUT_BYTES,
            depth: MAX_INPUT_DEPTH,
            values: MAX_INPUT_VALUES,
            scalar_bytes: MAX_INPUT_SCALAR_BYTES,
        },
    )
    .is_ok()
}

/// A text member: a string of at most `max` bytes with no control character; `nonempty` when
/// an empty string does not fit.
fn text_member(v: &serde_json::Value, max: usize, nonempty: bool) -> Option<String> {
    let t = v.as_str()?;
    let fits = t.len() <= max && !(nonempty && t.is_empty()) && !t.chars().any(char::is_control);
    fits.then(|| t.to_string())
}

/// The file name of pack `name` under `.rdy/packs`: the name itself when it is a plain file
/// name, else `hub-pack`.
fn pack_file_name(name: &str) -> String {
    let plain = !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    if plain {
        format!("{name}.yaml")
    } else {
        "hub-pack.yaml".to_string()
    }
}

/// The pack of a bundle as the clone read it.
struct ReadPack {
    name: String,
    /// The pack file under `.rdy/packs` and its bytes; none for a built-in pack.
    file: Option<(String, Vec<u8>)>,
    digest: Hash,
    warnings: Vec<PackWarning>,
}

/// The pack of a bundle, read whole by the strict pack parser in memory, then checked against
/// the pack the clone would use. None when this rdy cannot read it, or when that pack's digest
/// is not the bundle pack's.
///
/// A pack named as a built-in pack must be that pack: the clone writes no file and names it.
/// Any other pack is the effective pack a push captured, so the clone writes its source form
/// ([`rdy_schema::source_form`]) and resolves that file's text beside the built-in packs, as a
/// settings load will. The resolved pack must have the bundle pack's digest, so no member is lost.
fn read_pack(v: &serde_json::Value) -> Option<ReadPack> {
    if !v.is_object() {
        return None;
    }
    let text = serde_json::to_string(v).ok()?;
    // the strict parser first: an unknown key or an unreadable value anywhere refuses
    let mut alone = PackSet::default();
    let name = alone.add_yaml_from(&text, "the hub repo's pack").ok()?;
    let typed: Pack = serde_json::from_value(v.clone()).ok()?;
    let digest = pack_digest(&typed);
    let builtin = PackSet::with_builtin();
    if let Ok(effective) = builtin.resolve(&name) {
        let warnings = alone.warnings(&name).unwrap_or_default();
        return (pack_digest(&effective) == digest).then_some(ReadPack {
            name,
            file: None,
            digest,
            warnings,
        });
    }
    // the bundle's own text beside the built-in packs, for the pack's load warnings
    let mut read = PackSet::with_builtin();
    read.add_yaml_from(&text, "the hub repo's pack").ok()?;
    let warnings = read.warnings(&name).ok()?;
    let source = serde_yaml_ng::to_string(&rdy_schema::source_form(&typed)).ok()?;
    let mut written = PackSet::with_builtin();
    written.add_yaml_from(&source, "the hub repo's pack").ok()?;
    let effective = written.resolve(&name).ok()?;
    (pack_digest(&effective) == digest).then(|| ReadPack {
        file: Some((pack_file_name(&name), source.into_bytes())),
        name,
        digest,
        warnings,
    })
}

/// Step 3: the settings bundle of hub repo `url`, read whole (D-7PYYS). The first member this
/// rdy cannot read refuses, in the order of [`SETTINGS_MEMBERS`].
fn read_bundle(b: &SettingsBundle, url: &str) -> Result<ReadBundle, RdyError> {
    let captured = b.rdy.as_ref();
    let refuse = |member: &str| unfit(member, url, captured);
    for member in SETTINGS_MEMBERS {
        if b.member(member).is_some_and(|v| !within_bounds(v)) {
            return Err(refuse(member));
        }
    }
    let mut out = ReadBundle {
        captured: captured.cloned(),
        pack: DEFAULT_PACK.to_string(),
        pack_file: None,
        digest: pack_digest(
            &PackSet::with_builtin()
                .resolve(DEFAULT_PACK)
                .expect("the built-in pack resolves"),
        ),
        coverage_gate: None,
        coverage_target: None,
        decisions_home: None,
        propose_cadence: None,
        propose_changes: None,
        title: None,
        description: None,
        settings: ClonedSettings::default(),
        warnings: Vec::new(),
    };
    // the pack the clone would use must be the bundle's (D-D5J18), whatever rdy captured it
    if let Some(v) = &b.pack {
        let p = read_pack(v).ok_or_else(|| refuse("pack"))?;
        out.pack = p.name;
        out.pack_file = p.file;
        out.digest = p.digest;
        out.warnings
            .extend(p.warnings.into_iter().map(|w| CloneWarning {
                code: w.code.to_string(),
                message: w.message,
                fix: w.fix,
            }));
    }
    if b.pack_reason == Some(PackReason::Mismatch) {
        out.warnings.push(CloneWarning {
            code: W_UPSTREAM_PACK_MISMATCH.to_string(),
            message: UPSTREAM_PACK_MISMATCH_MESSAGE.replace("<url>", url),
            fix: UPSTREAM_PACK_MISMATCH_FIX.to_string(),
        });
    }
    if let Some(v) = &b.coverage_gate {
        let gate = v
            .as_str()
            .filter(|g| matches!(*g, "off" | "no_lower"))
            .ok_or_else(|| refuse("coverage_gate"))?;
        out.coverage_gate = Some(gate.to_string());
    }
    if let Some(v) = &b.coverage_target {
        let target = v
            .as_u64()
            .filter(|t| *t <= 100)
            .ok_or_else(|| refuse("coverage_target"))?;
        out.coverage_target = Some(target);
    }
    if let Some(v) = &b.decisions_home {
        let home = text_member(v, MAX_INPUT_SCALAR_BYTES, true)
            .filter(|h| h.trim() == h)
            .ok_or_else(|| refuse("decisions_home"))?;
        out.decisions_home = Some(home);
    }
    if let Some(v) = &b.propose {
        let m = v.as_object().ok_or_else(|| refuse("propose"))?;
        if m.keys().any(|k| k != "cadence" && k != "changes") {
            return Err(refuse("propose"));
        }
        if let Some(c) = m.get("cadence") {
            let cadence = c
                .as_str()
                .filter(|c| crate::config_levels::PROPOSE_CADENCES.contains(c))
                .ok_or_else(|| refuse("propose"))?;
            out.propose_cadence = Some(cadence.to_string());
        }
        if let Some(n) = m.get("changes") {
            let changes = n
                .as_u64()
                .filter(|n| (1..=1000).contains(n))
                .ok_or_else(|| refuse("propose"))?;
            out.propose_changes = Some(changes);
        }
    }
    if let Some(v) = &b.title {
        out.title = Some(text_member(v, MAX_TITLE_BYTES, true).ok_or_else(|| refuse("title"))?);
    }
    if let Some(v) = &b.description {
        out.description = Some(
            text_member(v, MAX_DESCRIPTION_BYTES, false).ok_or_else(|| refuse("description"))?,
        );
    }
    for member in SETTINGS_MEMBERS {
        let to = if b.member(member).is_some() {
            &mut out.settings.hub
        } else {
            &mut out.settings.defaults
        };
        to.push(member.to_string());
    }
    Ok(out)
}

/// Steps 2 to 5 on the selected target `t`.
fn clone_from(
    service: &SyncService,
    session: &Session,
    t: &Target,
    dir: &Path,
    user_file: &Path,
    before_checks: Option<BeforeChecks>,
) -> Result<UrlCloneSummary, RdyError> {
    let (base_url, state) = service.clone_open(t)?;
    let Some(tip) = state.canon else {
        return Err(no_canon(&t.url));
    };
    let Some(hub_id) = state.repo_id else {
        return Err(upstream_protocol());
    };
    let bundle = read_bundle(&service.clone_settings(t, &base_url)?, &t.url)?;
    // the first write: a temporary directory beside the target, on its file system
    let (target, tmp) = staging_beside(dir, ".rdy-clone-")?;
    let project = tmp.path().to_path_buf();
    let origin = Origin {
        fork: false,
        repo_id: hub_id,
        tag: None,
        landing: Some(tip),
    };
    write_project(&project, &bundle, &t.url, &origin)?;
    let engine = Local::open(&project)?;
    let fetched = service.clone_fetch(&engine, session, t, &base_url, &state);
    let closed = engine.close();
    let (fetched, _) = fetched?;
    closed?;
    if let Some(step) = before_checks {
        step(&project)?;
    }
    let checked = check_project(&project, user_file, &bundle, origin, t)?;
    install(tmp, &target)?;
    let fetched = fetched.map_or_else(ClonedFetch::default, |f| ClonedFetch {
        records_taken: f.records_taken,
        tags_taken: f.tags_taken,
        tags_kept: f.tags_kept,
    });
    Ok(UrlCloneSummary {
        project: target.display().to_string(),
        url: t.url.clone(),
        remote: t.name.clone(),
        repo_id: hub_id.to_string(),
        fork: false,
        origin: OriginReply::from(&checked.origin),
        canon: tip.to_string(),
        fetched,
        landings: checked.fsck.landings,
        tags: checked.tags,
        views: checked.views,
        settings: bundle.settings,
        warnings: bundle.warnings,
        fsck: checked.fsck,
        next: NEXT.to_string(),
    })
}

/// The absolute target `dir` and a new temporary directory beside it, on its file system, whose
/// name starts with `prefix`. The target's parent is made when it is missing. Dropping the
/// temporary directory removes it.
pub(crate) fn staging_beside(
    dir: &Path,
    prefix: &str,
) -> Result<(PathBuf, tempfile::TempDir), RdyError> {
    let target = std::path::absolute(dir).map_err(|e| io_at(dir, &e))?;
    let parent = target
        .parent()
        .ok_or_else(|| io_at(dir, &std::io::Error::other("it has no parent")))?;
    rdy_store::fsutil::create_dir_durable(parent).map_err(|e| io_at(parent, &e))?;
    let tmp = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(parent)
        .map_err(|e| io_at(parent, &e))?;
    Ok((target, tmp))
}

pub(crate) fn io_at(path: &Path, e: &std::io::Error) -> RdyError {
    RdyError::new(
        E_IO,
        format!("{}: {e}", path.display()),
        "check the path, its permissions and disk space",
    )
}

/// Step 4's local part: the pack file, a new project store, the hub's repo id and the origin,
/// then `.rdy.yaml` with the bundle's settings and the one share remote `hub` at `url`.
fn write_project(
    project: &Path,
    b: &ReadBundle,
    url: &str,
    origin: &Origin,
) -> Result<(), RdyError> {
    let packs = project.join(".rdy").join("packs");
    rdy_store::fsutil::create_dir_durable(&packs).map_err(|e| io_at(&packs, &e))?;
    if let Some((name, bytes)) = &b.pack_file {
        let file = packs.join(name);
        rdy_store::fsutil::write_durable(&file, bytes).map_err(|e| io_at(&file, &e))?;
    }
    let mut r = Rdy::init(project, &b.pack)?;
    let origin_set = r.store_mut().put_origin(origin, Some(origin.repo_id));
    let closed = r.close();
    origin_set?;
    closed?;
    let config = project.join(PROJECT_FILE);
    let text = settings_yaml(b, url)?;
    rdy_store::fsutil::write_durable(&config, text.as_bytes()).map_err(|e| io_at(&config, &e))
}

/// The `.rdy.yaml` of a URL clone: the pack, the title as `name`, the description, the gate
/// settings the bundle holds, and the share remote `hub` at `url`.
fn settings_yaml(b: &ReadBundle, url: &str) -> Result<String, RdyError> {
    use serde_yaml_ng::{Mapping, Value};
    let mut m = Mapping::new();
    m.insert("pack".into(), b.pack.as_str().into());
    if let Some(t) = &b.title {
        m.insert("name".into(), t.as_str().into());
    }
    if let Some(d) = &b.description {
        m.insert("description".into(), d.as_str().into());
    }
    if let Some(h) = &b.decisions_home {
        let mut d = Mapping::new();
        d.insert("home".into(), h.as_str().into());
        m.insert("decisions".into(), Value::Mapping(d));
    }
    if b.coverage_gate.is_some() || b.coverage_target.is_some() {
        let mut c = Mapping::new();
        if let Some(g) = &b.coverage_gate {
            c.insert("gate".into(), g.as_str().into());
        }
        if let Some(t) = b.coverage_target {
            c.insert("target".into(), t.into());
        }
        m.insert("coverage".into(), Value::Mapping(c));
    }
    if b.propose_cadence.is_some() || b.propose_changes.is_some() {
        let mut p = Mapping::new();
        if let Some(c) = &b.propose_cadence {
            p.insert("cadence".into(), c.as_str().into());
        }
        if let Some(n) = b.propose_changes {
            p.insert("changes".into(), n.into());
        }
        m.insert("propose".into(), Value::Mapping(p));
    }
    let entry = RemoteEntry {
        url: RemoteUrl::parse(url)?,
        mode: RemoteMode::Share,
    };
    crate::hub_config::set_remote(&mut m, HUB_REMOTE, Some(entry))?;
    serde_yaml_ng::to_string(&m).map_err(|e| {
        RdyError::new(
            E_INTERNAL,
            format!("the clone's settings do not serialize: {e}"),
            "report this as a bug",
        )
    })
}

/// What the checks of the new project found.
struct Checked {
    origin: Origin,
    tags: usize,
    views: usize,
    fsck: FsckReport,
}

/// Step 5's checks: the settings load (an open), the effective pack's digest, the origin's
/// newest version, `fsck`, and the merged hub settings with the user config.
fn check_project(
    project: &Path,
    user_file: &Path,
    b: &ReadBundle,
    mut origin: Origin,
    t: &Target,
) -> Result<Checked, RdyError> {
    let mut r = Rdy::open(project)?;
    let checked = (|| {
        let st = r.read();
        if pack_digest(st.pack()) != b.digest {
            return Err(unfit("pack", &t.url, b.captured.as_ref()));
        }
        let versions = st.versions()?;
        origin.tag = versions.last().map(|v| v.name.clone());
        let tags = versions.len();
        let views = st.view_names().len();
        drop(st);
        if origin.tag.is_some() {
            r.store_mut().put_origin(&origin, None)?;
        }
        let fsck = r.fsck()?;
        HubConfig::load(Some(project), Some(user_file))?;
        Ok(Checked {
            origin,
            tags,
            views,
            fsck,
        })
    })();
    let closed = r.close();
    let checked = checked?;
    closed?;
    Ok(checked)
}

/// The atomic install: one rename of the new project onto the target, which is absent or an
/// empty directory. A system that renames no directory onto an empty one (Windows) refuses the
/// first rename: the empty target then goes, the rename runs again, and a failure makes the
/// empty target again. On a failure the temporary directory goes and the target stays.
pub(crate) fn install(tmp: tempfile::TempDir, target: &Path) -> Result<(), RdyError> {
    if let Err(first) = std::fs::rename(tmp.path(), target) {
        if !target.is_dir() {
            return Err(io_at(target, &first));
        }
        std::fs::remove_dir(target).map_err(|_| io_at(target, &first))?;
        if let Err(e) = std::fs::rename(tmp.path(), target) {
            let _ = std::fs::create_dir(target);
            return Err(io_at(target, &e));
        }
    }
    // the directory moved: nothing is left to remove
    let _ = tmp.keep();
    if let Some(parent) = target.parent() {
        rdy_store::fsutil::sync_dir(parent).map_err(|e| io_at(parent, &e))?;
    }
    Ok(())
}
