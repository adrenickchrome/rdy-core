//! The working-copy ops: wc_view, wc_put_blob, wc_commit and wc_nonce, and the commits in flight.
use super::*;
use rdy_engine::consequence::write_and_report;
use rdy_engine::{
    E_BAD_INPUT, E_IO, E_LIMIT, E_NOT_FOUND, E_PERMISSION, E_WC_OCCUPIED, OpInput, RdyError,
    ReadState, Scope, WriteRequest, parse_justification,
};
use rdy_mcp::tools::MAX_ASSET_BYTES;
use rdy_model::ChangeLinks;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WcViewArgs {
    view: Option<String>,
    ids: Option<Vec<String>>,
    blobs_dir: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WcCommitArgs {
    view: String,
    ops: Vec<Value>,
    nonce: String,
    rewrite: Vec<String>,
    message: Option<String>,
    implements: Option<Vec<String>>,
    editorial: Option<EditorialArg>,
    brief: Option<String>,
    max_tokens: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditorialArg {
    class: String,
    reason: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WcNonceArgs {
    view: String,
    nonce: String,
    rewrite: Vec<String>,
    /// Search only the recent changes (msw11): the probe after a refused commit.
    recent: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WcPutBlobArgs {
    path: String,
}

/// wcco1: a view as working-copy files. The scope is the view when it has a head, else canon;
/// `ids` resolve in that scope. With `blobs_dir` (resolved against the session's directory), each
/// blob whose file is missing or differs from the store's bytes is written from the store (temp
/// file and rename); a blob the store lacks is `E_NOT_FOUND`, as export's is. A `blobs_dir` or a
/// blob path that exists as something else (a link included) is `E_WC_OCCUPIED`.
pub(super) fn wc_view(cx: &OpContext, a: WcViewArgs) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    let view = view_of(cx.session, a.view);
    let scope = if st.view_head(&view).is_some() {
        Scope::View(view.clone())
    } else {
        Scope::Canon
    };
    let ids = a
        .ids
        .map(|ids| {
            ids.iter()
                .map(|t| st.resolve_id(&scope, t))
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    let mut proj = st.wc_project(&scope, ids.as_deref())?;
    proj.view = view;
    if let Some(dir) = a.blobs_dir {
        let dir = resolve_path(cx.cwd, &dir);
        // #642: a link at `_blobs` or at a blob's name would take the write outside the tree
        refuse_not_dir(&dir)?;
        for b in &proj.blobs {
            let path = dir.join(&b.name);
            let existing = match std::fs::symlink_metadata(&path) {
                Ok(m) if m.is_file() => Some(m.len()),
                Ok(_) => return Err(not_a_regular_file(&path)),
                Err(_) => None,
            };
            let not_found = || {
                RdyError::new(
                    E_NOT_FOUND,
                    format!(
                        "a figure names blob {}, which is not a blob in the store",
                        b.hash
                    ),
                    "attach the asset again (rdy_asset) and point the figure at it, or delete the figure",
                )
            };
            let h: rdy_model::Hash = b.hash.parse().map_err(|_| not_found())?;
            if !st.reader().has_blob(&h)? {
                return Err(not_found());
            }
            let bytes = st.reader().get_blob(&h)?;
            let io = |e: std::io::Error| {
                RdyError::new(
                    E_IO,
                    format!("cannot write {}: {e}", path.display()),
                    "check the directory and its permissions",
                )
            };
            // #649: the name is a manifest hash, not a hash of the file: a torn or changed file
            // (or one that does not read) is rewritten from the store's bytes
            if existing == Some(bytes.len() as u64)
                && std::fs::read(&path).is_ok_and(|f| f == bytes)
            {
                continue;
            }
            std::fs::create_dir_all(&dir).map_err(io)?;
            rdy_engine::write_durable(&path, &bytes).map_err(io)?;
        }
    }
    Ok(to_json(&proj))
}

/// `E_WC_OCCUPIED` when `dir` exists and is not a directory (a link to one included).
fn refuse_not_dir(dir: &std::path::Path) -> Result<(), RdyError> {
    match std::fs::symlink_metadata(dir) {
        Ok(m) if !m.is_dir() => Err(RdyError::new(
            E_WC_OCCUPIED,
            format!("{} is not a directory", dir.display()),
            "move it aside, then run the command again",
        )),
        _ => Ok(()),
    }
}

/// `E_WC_OCCUPIED` for a blob path that exists and is not a regular file.
fn not_a_regular_file(path: &std::path::Path) -> RdyError {
    RdyError::new(
        E_WC_OCCUPIED,
        format!("{} is not a regular file", path.display()),
        "move it aside, then run the command again",
    )
}

/// wcfg1: stores the file at `path` (resolved against the session's directory) as a blob and
/// returns its hash. A regular file of at most `MAX_ASSET_BYTES`, as `rdy_asset` limits it.
pub(super) fn wc_put_blob(cx: &OpContext, a: WcPutBlobArgs) -> Result<Value, RdyError> {
    if !cx.session.role.can_write() {
        return Err(RdyError::new(
            E_PERMISSION,
            format!("role {:?} cannot write", cx.session.role),
            "start the session as a writer",
        ));
    }
    let path = resolve_path(cx.cwd, &a.path);
    let io = |e: std::io::Error| {
        RdyError::new(
            E_IO,
            format!("cannot read {}: {e}", path.display()),
            "check that the file exists and can be read",
        )
    };
    let too_big = || {
        RdyError::new(
            E_LIMIT,
            format!(
                "{} is over the limit of {MAX_ASSET_BYTES} bytes",
                path.display()
            ),
            format!("use an image of at most {MAX_ASSET_BYTES} bytes"),
        )
    };
    let meta = std::fs::metadata(&path).map_err(io)?;
    if !meta.is_file() {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!("{} is not a regular file", path.display()),
            "name an image file",
        ));
    }
    if meta.len() > MAX_ASSET_BYTES as u64 {
        return Err(too_big());
    }
    // read at most one byte over the limit, so a file that grew since the check is still refused
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(
        &mut std::io::Read::take(
            std::fs::File::open(&path).map_err(io)?,
            MAX_ASSET_BYTES as u64 + 1,
        ),
        &mut bytes,
    )
    .map_err(io)?;
    if bytes.len() > MAX_ASSET_BYTES {
        return Err(too_big());
    }
    let n = bytes.len();
    let hash = cx.engine.write(Box::new(move |r| {
        Ok(json!(r.store_mut().put_blob(&bytes)?.to_string()))
    }))?;
    Ok(json!({"hash": hash, "bytes": n}))
}

/// The most changes `wc_nonce` reads before it answers that no change carries the nonce (R-I6).
const NONCE_SCAN_LIMIT: usize = 10_000;

/// How long `wc_nonce` waits for a `wc_commit` with its nonce that is still running before it
/// answers `pending`.
const NONCE_WAIT: Duration = Duration::from_secs(60);

/// The nonces of the `wc_commit` requests this daemon is running: from dispatch until their
/// write has returned (and its state is published). A client that dies after it sends one
/// leaves the request running; the next command's `wc_nonce` waits for it.
static IN_FLIGHT: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

static IN_FLIGHT_DONE: Condvar = Condvar::new();

fn in_flight() -> MutexGuard<'static, BTreeSet<String>> {
    IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether a `wc_commit` with `nonce` is running in this process.
pub fn wc_in_flight(nonce: &str) -> bool {
    in_flight().contains(nonce)
}

/// A `wc_commit`'s nonce, in flight while this lives.
struct InFlight(String);

impl InFlight {
    fn start(nonce: &str) -> InFlight {
        in_flight().insert(nonce.to_string());
        InFlight(nonce.to_string())
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        in_flight().remove(&self.0);
        IN_FLIGHT_DONE.notify_all();
    }
}

/// Waits until no `wc_commit` with `nonce` runs, up to `NONCE_WAIT`; false when one still runs.
fn wait_not_in_flight(nonce: &str) -> bool {
    let deadline = Instant::now() + NONCE_WAIT;
    let mut set = in_flight();
    while set.contains(nonce) {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        set = IN_FLIGHT_DONE
            .wait_timeout(set, left)
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
    true
}

/// The scope a working copy of `view` reads: the view when it has a head, else canon.
fn wc_scope(st: &ReadState, view: &str) -> Scope {
    if st.view_head(view).is_some() {
        Scope::View(view.to_string())
    } else {
        Scope::Canon
    }
}

/// The projection of the files of `rewrite` (ids as text) in `view`, as reply keys: `files`,
/// `blocks`, `blobs`, `missing` and `head`. An id that does not parse, or that the scope does
/// not hold, is listed in `missing`.
fn wc_rewrite(st: &ReadState, view: &str, rewrite: &[String]) -> Result<Value, RdyError> {
    let scope = wc_scope(st, view);
    let mut ids = vec![];
    let mut unparsed = vec![];
    for t in rewrite {
        match rdy_model::BlockId::parse(t) {
            Ok(id) => ids.push(id),
            Err(_) => unparsed.push(t.clone()),
        }
    }
    let mut proj = st.wc_project(&scope, Some(&ids))?;
    proj.missing.extend(unparsed);
    Ok(json!({
        "files": proj.files,
        "blocks": proj.blocks,
        "blobs": proj.blobs,
        "missing": proj.missing,
        "head": proj.head,
    }))
}

/// Merges the keys of the object `extra` into the object `v`.
fn merge_into(v: &mut Value, extra: Value) {
    if let (Some(o), Value::Object(e)) = (v.as_object_mut(), extra) {
        o.extend(e);
    }
}

/// wccm1: a working-copy commit. One write of `ops` into `view` that records `nonce` on the
/// change, justified as `rdy_write` justifies it; the reply gains the projection of the files
/// of `rewrite` as the write left them.
pub(super) fn wc_commit(cx: &OpContext, a: WcCommitArgs) -> Result<Value, RdyError> {
    // in flight from dispatch until the write has returned, before anything can fail or queue
    let _in_flight = InFlight::start(&a.nonce);
    check_max_tokens(a.max_tokens)?;
    if !cx.session.role.can_write() {
        return Err(RdyError::new(
            E_PERMISSION,
            format!("role {:?} cannot write", cx.session.role),
            "start the session as a writer",
        ));
    }
    let view = view_of(cx.session, Some(a.view));
    let justification = {
        let st = cx.engine.read();
        // the client's lint sees no view: a link target written as a key is checked here
        let m = st.materialized(&wc_scope(&st, &view))?;
        rdy_engine::wc::lint::check_keyed_targets(&m, &a.ops)?;
        parse_justification(
            &st,
            &Scope::View(view.clone()),
            a.implements,
            a.editorial.map(|e| (e.class, e.reason)),
        )?
    };
    let ops: Vec<OpInput> = a
        .ops
        .into_iter()
        .enumerate()
        .map(|(i, v)| {
            serde_json::from_value(v).map_err(|e| {
                RdyError::new(
                    E_BAD_INPUT,
                    format!("op {i}: {e}"),
                    "each op needs \"op\" (rdy_schema lists them under \"ops\"); create needs \"type\"",
                )
            })
        })
        .collect::<Result<_, _>>()?;
    let req = WriteRequest {
        view: view.clone(),
        ops,
        justification,
        message: a.message.unwrap_or_default(),
    };
    let s = cx.session.clone();
    let (nonce, brief) = (a.nonce, a.brief);
    let mut out = write_and_report(cx.engine, &view, a.max_tokens, move |r| {
        r.write_linked(
            &s,
            req,
            ChangeLinks {
                client_nonce: Some(nonce),
                ..Default::default()
            },
            brief.as_deref(),
        )
    })?;
    let proj = wc_rewrite(&cx.engine.read(), &view, &a.rewrite)?;
    merge_into(&mut out, proj);
    Ok(out)
}

/// wccm1, R-I6: the change a working-copy commit with `nonce` recorded, when the store has it,
/// and then the projection of the files of `rewrite` (as `wc_commit` adds it); else
/// `{change: null}`. The answer is conclusive: it first waits while a `wc_commit` with the nonce
/// runs in this daemon (`{change: null, pending: true}` when it still runs after `NONCE_WAIT`),
/// then reads on the writer, after every write queued before it. With `recent`, it reads only the
/// view's newest changes and canon's newest landing (`change_by_recent_nonce`): the probe a
/// commit makes after an error reply, which must not stall the writer with a full scan.
pub(super) fn wc_nonce(cx: &OpContext, a: WcNonceArgs) -> Result<Value, RdyError> {
    if !wait_not_in_flight(&a.nonce) {
        return Ok(json!({"change": null, "pending": true}));
    }
    let view = view_of(cx.session, Some(a.view));
    let (nonce, rewrite, recent) = (a.nonce, a.rewrite, a.recent == Some(true));
    cx.engine.write(Box::new(move |r| {
        let st = r.read();
        let found = if recent {
            st.change_by_recent_nonce(&view, &nonce)?
        } else {
            st.change_by_nonce(&view, &nonce, NONCE_SCAN_LIMIT)?
        };
        match found.change {
            None => Ok(json!({"change": null})),
            Some(h) => {
                let mut out = json!({"change": h.to_string()});
                merge_into(&mut out, wc_rewrite(&st, &view, &rewrite)?);
                Ok(out)
            }
        }
    }))
}
