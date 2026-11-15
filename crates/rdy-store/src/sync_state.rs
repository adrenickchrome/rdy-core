//! A project's sync state (D-3RSCW): for each remote, the meta refs under `meta/sync/<remote>/`.
//!
//! - `meta/sync/<remote>/canon` holds [`SyncCanon`]: the fetched, applied and held hub tips.
//! - `meta/sync/<remote>/view/<name>` holds [`SyncView`]: the heads of one synced view.
//! - `meta/sync/<remote>/upload` holds [`SyncUpload`]: an upload whose write was not sent yet.
//! - `meta/sync/<remote>/pending` holds [`SyncPending`]: a write that was sent and has no reply.
//!
//! Each value is the postcard encoding of its closed struct, as the `meta/origin` value is, and
//! at most [`MAX_VALUE_BYTES`] bytes. A value names the hub repo id and the remote's URL in
//! canonical form, and holds no token, credential or hub reply: a URL with user information, a
//! query or a fragment is refused. An empty value is a cleared ref: refs are never removed.
//!
//! Only the store writes these refs ([`Store::write_sync`], and a project fetch's batch in
//! `crate::take_batch`). The first batch that writes one activates the incompatible feature
//! `sync_state` in that batch, so an older rdy refuses the store and names the version it needs.
//! A holder never writes them (D-DZDB7). A batch that writes `upload` or `pending` expects the
//! ref to be absent, and a batch that clears one expects it to name its own push: a compare that
//! fails is `StoreError::SyncBusy` (E_LOCKED) and writes nothing.
//!
//! `rdy fsck` checks each ref: its name shape, its closed value and bound, and that each hash
//! names a landing or view op that the store holds ([`check_sync_refs`]).
//!
//! The unsent table ([`StoreReader::sync_unsent`]) is derived from canon, the views and these
//! refs on each read, as the other derived reads are: no log record holds it, so a reindex
//! reproduces it.
use crate::Store;
use crate::error::StoreError;
use crate::holder::{RefWrite, StoreMode};
use crate::kinds;
use crate::store::{StoreReader, batch_open};
use rdy_model::{Hash, RepoId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The prefix of every sync state ref.
pub const SYNC_REF: &str = "meta/sync/";
/// The most bytes of one sync state value.
pub const MAX_VALUE_BYTES: usize = 8_192;
/// The most bytes of a value's URL: the hub URL bound (`rdy_engine::hub_types::MAX_URL_BYTES`).
pub const MAX_URL_BYTES: usize = 4_096;
/// The most bytes of a push id.
pub const MAX_PUSH_BYTES: usize = 64;
/// The most bytes of a remote name, as `rdy remote` names it.
pub const MAX_REMOTE_BYTES: usize = 64;
/// The most characters of a view name, as a view write accepts it.
pub const MAX_VIEW_CHARS: usize = 256;
/// The write kind of a canon write in `upload` and `pending`.
pub const KIND_CANON: u8 = 0;
/// The write kind of a view write in `upload` and `pending`.
pub const KIND_VIEW: u8 = 1;
/// The fix of the E_LOCKED refusal of a second sync with one remote (D-47HVF `SYNC_LOCKED`).
pub const SYNC_LOCKED_FIX: &str = "wait for it to end, then try again";

/// The message of the E_LOCKED refusal of a second sync with remote `remote` (D-47HVF
/// `SYNC_LOCKED`).
pub fn sync_locked_message(remote: &str) -> String {
    format!("a push or pull with remote {remote} is running")
}

/// The held landing request of a canon state: the head of the last canon write that the hub held
/// as a landing request, and that request's number.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncHeld {
    pub head: Hash,
    pub request: u64,
}

/// `meta/sync/<remote>/canon`: the hub canon tip of the last fetch, the last hub tip that this
/// machine applied by pull or saw taken by push, and the held request. Each may be null.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncCanon {
    pub repo_id: RepoId,
    pub url: String,
    pub fetched: Option<Hash>,
    pub applied: Option<Hash>,
    pub held: Option<SyncHeld>,
}

/// `meta/sync/<remote>/view/<name>`: one synced view's root op and its fetched and applied heads,
/// view op hashes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncView {
    pub repo_id: RepoId,
    pub url: String,
    pub root: Hash,
    pub fetched: Option<Hash>,
    pub applied: Option<Hash>,
}

/// `meta/sync/<remote>/upload`: an upload whose write was not sent yet. `kind` is
/// [`KIND_CANON`] or [`KIND_VIEW`]; `view` names the view of a view write, else null. `at_ms`
/// comes from the writer's clock seam.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncUpload {
    pub repo_id: RepoId,
    pub url: String,
    pub push: String,
    pub kind: u8,
    pub view: Option<String>,
    pub at_ms: i64,
}

/// `meta/sync/<remote>/pending`: a write that was sent and has no reply yet. `expected` is the
/// tip or head the write expects, or null; `head` is the pushed head.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncPending {
    pub repo_id: RepoId,
    pub url: String,
    pub push: String,
    pub kind: u8,
    pub view: Option<String>,
    pub expected: Option<Hash>,
    pub head: Hash,
    pub at_ms: i64,
}

/// One sync ref's name: its remote, and which ref of the remote it is.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SyncRefName {
    Canon { remote: String },
    View { remote: String, name: String },
    Upload { remote: String },
    Pending { remote: String },
}

impl SyncRefName {
    /// The name of `name`, when it is a sync ref of a valid shape: a remote name ([`is_remote`])
    /// and `canon`, `upload`, `pending` or `view/<view name>` ([`is_view_name`]).
    pub fn parse(name: &str) -> Option<SyncRefName> {
        let rest = name.strip_prefix(SYNC_REF)?;
        let (remote, rest) = rest.split_once('/')?;
        if !is_remote(remote) {
            return None;
        }
        let remote = remote.to_string();
        match rest {
            "canon" => Some(SyncRefName::Canon { remote }),
            "upload" => Some(SyncRefName::Upload { remote }),
            "pending" => Some(SyncRefName::Pending { remote }),
            _ => {
                let view = rest.strip_prefix("view/")?;
                is_view_name(view).then(|| SyncRefName::View {
                    remote,
                    name: view.to_string(),
                })
            }
        }
    }

    /// The ref's remote.
    pub fn remote(&self) -> &str {
        match self {
            SyncRefName::Canon { remote }
            | SyncRefName::View { remote, .. }
            | SyncRefName::Upload { remote }
            | SyncRefName::Pending { remote } => remote,
        }
    }

    /// The ref's full name.
    pub fn full(&self) -> String {
        match self {
            SyncRefName::Canon { remote } => canon_ref(remote),
            SyncRefName::View { remote, name } => view_ref(remote, name),
            SyncRefName::Upload { remote } => upload_ref(remote),
            SyncRefName::Pending { remote } => pending_ref(remote),
        }
    }
}

/// `meta/sync/<remote>/canon`.
pub fn canon_ref(remote: &str) -> String {
    format!("{SYNC_REF}{remote}/canon")
}

/// `meta/sync/<remote>/view/<name>`.
pub fn view_ref(remote: &str, name: &str) -> String {
    format!("{SYNC_REF}{remote}/view/{name}")
}

/// `meta/sync/<remote>/upload`.
pub fn upload_ref(remote: &str) -> String {
    format!("{SYNC_REF}{remote}/upload")
}

/// `meta/sync/<remote>/pending`.
pub fn pending_ref(remote: &str) -> String {
    format!("{SYNC_REF}{remote}/pending")
}

/// Whether `name` is a sync state ref name (any shape).
pub fn is_sync_ref(name: &str) -> bool {
    name.starts_with(SYNC_REF)
}

/// Whether `s` is a remote name, as `rdy remote` names it: 1 to 64 ASCII bytes, a letter or
/// digit first, then letters, digits, `.`, `_` and `-`.
pub fn is_remote(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= MAX_REMOTE_BYTES
        && b[0].is_ascii_alphanumeric()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

/// Whether `s` is a view name as a view write accepts it: not empty, at most 256 characters.
pub fn is_view_name(s: &str) -> bool {
    !s.is_empty() && s.chars().count() <= MAX_VIEW_CHARS
}

/// Whether `url` may stand in a value: `https://`, at most [`MAX_URL_BYTES`] bytes, with no user
/// information, query or fragment (so no credential rides in it) and no control character.
fn url_rule(url: &str) -> Result<(), &'static str> {
    if url.len() > MAX_URL_BYTES {
        return Err("url exceeds 4096 bytes");
    }
    let Some(rest) = url.strip_prefix("https://") else {
        return Err("url is not https");
    };
    let authority = rest.split('/').next().unwrap_or("");
    if authority.is_empty() {
        return Err("url has no host");
    }
    if authority.contains('@') || url.contains(['?', '#']) || url.chars().any(char::is_control) {
        return Err("url holds more than a canonical hub url");
    }
    Ok(())
}

/// Whether `push` is a push id: 1 to 64 ASCII letters, digits, `-` and `_`.
fn push_rule(push: &str) -> Result<(), &'static str> {
    let ok = !push.is_empty()
        && push.len() <= MAX_PUSH_BYTES
        && push
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'));
    if ok { Ok(()) } else { Err("invalid push id") }
}

/// The write kind and view rule: kind 0 names no view, kind 1 a view name.
fn kind_rule(kind: u8, view: &Option<String>) -> Result<(), &'static str> {
    match (kind, view) {
        (KIND_CANON, None) => Ok(()),
        (KIND_VIEW, Some(v)) if is_view_name(v) => Ok(()),
        (KIND_CANON | KIND_VIEW, _) => Err("write kind and view do not agree"),
        _ => Err("unknown write kind"),
    }
}

/// One decoded sync value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncValue {
    Canon(SyncCanon),
    View(SyncView),
    Upload(SyncUpload),
    Pending(SyncPending),
}

impl SyncValue {
    /// The value's postcard encoding.
    pub fn encode(&self) -> Vec<u8> {
        match self {
            SyncValue::Canon(v) => encode(v),
            SyncValue::View(v) => encode(v),
            SyncValue::Upload(v) => encode(v),
            SyncValue::Pending(v) => encode(v),
        }
    }

    /// The objects the value names, each with its kind: a canon value's tips are landings, a
    /// view value's heads view ops, and a pending write's tips are those of its kind.
    pub fn targets(&self) -> Vec<(u8, Hash)> {
        let landing = |h: &Hash| (kinds::LANDING, *h);
        let op = |h: &Hash| (kinds::VIEW_OP, *h);
        match self {
            SyncValue::Canon(c) => c
                .fetched
                .iter()
                .chain(&c.applied)
                .chain(c.held.as_ref().map(|h| &h.head))
                .map(landing)
                .collect(),
            SyncValue::View(v) => std::iter::once(&v.root)
                .chain(&v.fetched)
                .chain(&v.applied)
                .map(op)
                .collect(),
            SyncValue::Upload(_) => Vec::new(),
            SyncValue::Pending(p) => {
                let of = if p.kind == KIND_VIEW { op } else { landing };
                p.expected.iter().chain([&p.head]).map(of).collect()
            }
        }
    }

    /// The value's URL and repo id.
    pub fn identity(&self) -> (&RepoId, &str) {
        match self {
            SyncValue::Canon(v) => (&v.repo_id, &v.url),
            SyncValue::View(v) => (&v.repo_id, &v.url),
            SyncValue::Upload(v) => (&v.repo_id, &v.url),
            SyncValue::Pending(v) => (&v.repo_id, &v.url),
        }
    }
}

/// The postcard encoding of a value.
fn encode<T: Serialize>(v: &T) -> Vec<u8> {
    postcard::to_allocvec(v).expect("a sync value always encodes")
}

/// `v` as `T`: the whole value decodes, and it is its own canonical encoding (no alias).
fn decode<T: Serialize + for<'de> Deserialize<'de>>(v: &[u8]) -> Option<T> {
    match postcard::take_from_bytes::<T>(v) {
        Ok((t, [])) if encode(&t) == v => Some(t),
        _ => None,
    }
}

/// The rules of the sync ref `name` with `value` (closed): a name of a valid shape, a value of
/// at most [`MAX_VALUE_BYTES`] bytes, and the closed value of its ref with its URL, push id and
/// kind rules. An empty value is a cleared ref: None. The error is the fsck text.
pub fn check_ref(name: &str, value: &[u8]) -> Result<Option<SyncValue>, String> {
    let Some(parsed) = SyncRefName::parse(name) else {
        return Err(format!("ref {name}: not a sync ref name"));
    };
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() > MAX_VALUE_BYTES {
        return Err(format!("ref {name}: value exceeds {MAX_VALUE_BYTES} bytes"));
    }
    let bad = || format!("ref {name}: not a sync value");
    let rule = |r: &'static str| format!("ref {name}: {r}");
    let v = match parsed {
        SyncRefName::Canon { .. } => SyncValue::Canon(decode(value).ok_or_else(bad)?),
        SyncRefName::View { .. } => SyncValue::View(decode(value).ok_or_else(bad)?),
        SyncRefName::Upload { .. } => SyncValue::Upload(decode(value).ok_or_else(bad)?),
        SyncRefName::Pending { .. } => SyncValue::Pending(decode(value).ok_or_else(bad)?),
    };
    url_rule(v.identity().1).map_err(rule)?;
    match &v {
        SyncValue::Upload(u) => {
            push_rule(&u.push).map_err(rule)?;
            kind_rule(u.kind, &u.view).map_err(rule)?;
        }
        SyncValue::Pending(p) => {
            push_rule(&p.push).map_err(rule)?;
            kind_rule(p.kind, &p.view).map_err(rule)?;
        }
        SyncValue::Canon(_) | SyncValue::View(_) => {}
    }
    Ok(Some(v))
}

/// The fsck check of every sync ref of a store (D-3RSCW): a project only, with `sync_state` in
/// `features`; each ref's name shape, closed value and bound ([`check_ref`]), and each hash it
/// names a landing or view op that the store holds (`has`). The first failure, in ref name
/// order, is the error text.
pub(crate) fn check_sync_refs(
    refs: &[(String, Vec<u8>)],
    mode: StoreMode,
    listed: bool,
    has: impl Fn(u8, &Hash) -> Result<bool, StoreError>,
) -> Result<(), StoreError> {
    for (name, value) in refs {
        if mode == StoreMode::Holder || !listed {
            return Err(StoreError::Decode(format!(
                "{name:?} is not a meta ref this rdy knows (meta/store_features, meta/upgrade, meta/store_format, meta/repo_id, meta/origin, meta/store_mode)"
            )));
        }
        let v = check_ref(name, value).map_err(StoreError::Decode)?;
        for (kind, h) in v.iter().flat_map(SyncValue::targets) {
            if !has(kind, &h)? {
                let what = if kind == kinds::LANDING {
                    "landing"
                } else {
                    "view op"
                };
                return Err(StoreError::Decode(format!(
                    "ref {name}: names {what} {h}, which the store does not hold"
                )));
            }
        }
    }
    Ok(())
}

/// One step of a sync state batch ([`Store::write_sync`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncStep {
    /// Writes the canon state: a pull's fetched or applied tip, a reply's applied or held tip.
    Canon(SyncCanon),
    /// Writes one view's state.
    View { name: String, value: SyncView },
    /// A push before its first put: writes `upload`, which must be absent.
    Upload(SyncUpload),
    /// The batch before the write is sent: `upload` must name the same push and `pending` must
    /// be absent; `upload` is cleared and `pending` written.
    Send(SyncPending),
    /// The batch that records the reply: `pending` must name `push`; it is cleared.
    Reply { push: String },
    /// A push that ends before it sends: `upload` must name `push`; it is cleared.
    ClearUpload { push: String },
}

/// What the sync refs of a store named at any time ([`StoreReader::sync_named_heads`]): the
/// landings of canon states (fetched, applied and held tips) and the view ops of view states
/// (root, fetched and applied heads).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncNamed {
    pub landings: BTreeSet<Hash>,
    pub view_ops: BTreeSet<Hash>,
}

/// The push a current `upload` or `pending` value names; None for an absent or cleared ref.
fn push_of(v: Option<&[u8]>) -> Option<String> {
    let v = v.filter(|v| !v.is_empty())?;
    if let Some(u) = decode::<SyncUpload>(v) {
        return Some(u.push);
    }
    decode::<SyncPending>(v).map(|p| p.push)
}

impl StoreReader {
    /// The value of the sync ref `name` of its kind; None when the ref is absent or cleared, or
    /// does not decode (fsck reports that).
    fn sync_value(&self, name: &str) -> Result<Option<SyncValue>, StoreError> {
        match self.get_ref(name)? {
            Some(v) => Ok(check_ref(name, &v).ok().flatten()),
            None => Ok(None),
        }
    }

    /// `meta/sync/<remote>/canon`, when it holds a value.
    pub fn sync_canon(&self, remote: &str) -> Result<Option<SyncCanon>, StoreError> {
        Ok(match self.sync_value(&canon_ref(remote))? {
            Some(SyncValue::Canon(c)) => Some(c),
            _ => None,
        })
    }

    /// `meta/sync/<remote>/view/<name>`, when it holds a value.
    pub fn sync_view(&self, remote: &str, name: &str) -> Result<Option<SyncView>, StoreError> {
        Ok(match self.sync_value(&view_ref(remote, name))? {
            Some(SyncValue::View(v)) => Some(v),
            _ => None,
        })
    }

    /// Every view state of `remote` that holds a value, by view name.
    pub fn sync_views(&self, remote: &str) -> Result<BTreeMap<String, SyncView>, StoreError> {
        let prefix = view_ref(remote, "");
        let mut out = BTreeMap::new();
        for (name, value) in self.refs_with_prefix(&prefix)? {
            if let Ok(Some(SyncValue::View(v))) = check_ref(&name, &value) {
                out.insert(name[prefix.len()..].to_string(), v);
            }
        }
        Ok(out)
    }

    /// `meta/sync/<remote>/upload`, when it holds a value.
    pub fn sync_upload(&self, remote: &str) -> Result<Option<SyncUpload>, StoreError> {
        Ok(match self.sync_value(&upload_ref(remote))? {
            Some(SyncValue::Upload(u)) => Some(u),
            _ => None,
        })
    }

    /// `meta/sync/<remote>/pending`, when it holds a value.
    pub fn sync_pending(&self, remote: &str) -> Result<Option<SyncPending>, StoreError> {
        Ok(match self.sync_value(&pending_ref(remote))? {
            Some(SyncValue::Pending(p)) => Some(p),
            _ => None,
        })
    }

    /// Every landing and view op that a canon or view sync ref of this store named at any time:
    /// each hash of each such value that the log's ref updates hold, not only the current
    /// values. A fetch moves no canon or view ref (D-3RSCW), so what it took is known by the
    /// sync state it wrote. A later fetch of an earlier tip or head, and a fork's origin batch
    /// that clears the refs, change the current value only: the log keeps each earlier value, so
    /// the records each one named stay a fetched history's. A ref update that does not decode,
    /// or a value that does not read, names nothing here (fsck reports it elsewhere).
    pub fn sync_named_heads(&self) -> Result<SyncNamed, StoreError> {
        let mut out = SyncNamed::default();
        self.for_each_object_of(kinds::REF_UPDATE, |hash| {
            let Ok(u) = self.get_typed::<kinds::RefUpdate>(kinds::REF_UPDATE, &hash) else {
                return Ok(());
            };
            if !is_sync_ref(&u.name) {
                return Ok(());
            }
            let Ok(Some(v @ (SyncValue::Canon(_) | SyncValue::View(_)))) =
                check_ref(&u.name, &u.value)
            else {
                return Ok(());
            };
            for (kind, h) in v.targets() {
                if kind == kinds::LANDING {
                    out.landings.insert(h);
                } else {
                    out.view_ops.insert(h);
                }
            }
            Ok(())
        })?;
        Ok(out)
    }

    /// The view ops of each fetched view history, by view name: every op that a walk reaches
    /// from a view op the sync refs ever named ([`StoreReader::sync_named_heads`]). The walk
    /// follows `prev` and a `Fork`'s source op `from_head` into the source view, as a view
    /// history's closure does (`crate::closure`), so a fetched fork brings the ops of the view it
    /// forked. A view's chain need not hold these ops: no fetch moves a view ref. The walk stops
    /// at an op the store does not hold and visits each op once.
    pub fn sync_fetched_view_ops(&self) -> Result<BTreeMap<String, BTreeSet<Hash>>, StoreError> {
        let heads = self.sync_named_heads()?;
        let mut out: BTreeMap<String, BTreeSet<Hash>> = BTreeMap::new();
        let mut seen: BTreeSet<Hash> = BTreeSet::new();
        let mut queue: Vec<Hash> = heads.view_ops.into_iter().collect();
        while let Some(h) = queue.pop() {
            if !seen.insert(h) {
                continue;
            }
            let op = match self.view_op(&h) {
                Ok(op) => op,
                Err(StoreError::NotFound(_)) => continue,
                Err(e) => return Err(e),
            };
            queue.extend(op.prev);
            if let crate::view::ViewOpKind::Fork { from_head, .. } = &op.op {
                queue.push(*from_head);
            }
            out.entry(op.view).or_default().insert(h);
        }
        Ok(out)
    }

    /// The remotes with sync state: each remote that has a sync ref holding a value.
    pub fn sync_remotes(&self) -> Result<Vec<String>, StoreError> {
        let mut out: Vec<String> = Vec::new();
        for (name, value) in self.refs_with_prefix(SYNC_REF)? {
            if value.is_empty() {
                continue;
            }
            if let Some(r) = SyncRefName::parse(&name)
                && !out.iter().any(|o| o == r.remote())
            {
                out.push(r.remote().to_string());
            }
        }
        out.sort();
        Ok(out)
    }

    /// The unsent table of `remote` (D-3RSCW), derived from canon, the views and the remote's
    /// sync refs on each read: the landings of local canon after `applied`, oldest first (those
    /// up to `held.head` are in that landing request, not unsent), and each synced view's ops
    /// after its `applied` head. None for a remote with no canon or view state. A walk stops at
    /// the chain's start; at most [`crate::sync_stream::MAX_HISTORY_DEPTH`] steps are read, else
    /// E_LIMIT.
    pub fn sync_unsent(&self, remote: &str) -> Result<Option<Unsent>, StoreError> {
        let canon = self.sync_canon(remote)?;
        let views = self.sync_views(remote)?;
        if canon.is_none() && views.is_empty() {
            return Ok(None);
        }
        let mut out = Unsent::default();
        if let Some(c) = &canon {
            let mut chain = Vec::new();
            let mut at = self.canon_head()?;
            while let Some(l) = at {
                if Some(l) == c.applied {
                    break;
                }
                chain.push(l);
                if chain.len() as u64 > crate::sync_stream::MAX_HISTORY_DEPTH {
                    return Err(unsent_limit());
                }
                at = self.landing(&l)?.prev;
            }
            chain.reverse();
            let held = c
                .held
                .as_ref()
                .and_then(|h| chain.iter().position(|l| *l == h.head));
            match held {
                Some(i) => {
                    out.landings = chain.split_off(i + 1);
                    out.held = chain;
                }
                None => out.landings = chain,
            }
        }
        for (name, v) in views {
            let mut ops = Vec::new();
            let mut at = self.view_head(&name)?.map(|h| h.head);
            while let Some(op) = at {
                if Some(op) == v.applied {
                    break;
                }
                ops.push(op);
                if ops.len() as u64 > crate::sync_stream::MAX_HISTORY_DEPTH {
                    return Err(unsent_limit());
                }
                at = self.view_op(&op)?.prev;
            }
            ops.reverse();
            out.views.insert(name, ops);
        }
        Ok(Some(out))
    }
}

/// The unsent table's walk past its bound.
fn unsent_limit() -> StoreError {
    StoreError::Refused("the sync state walk exceeds 100000 history depth".into())
}

/// One remote's unsent table (D-3RSCW): the landings of local canon after `applied`, those in the
/// held landing request, and each synced view's ops after its `applied` head, each oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Unsent {
    pub landings: Vec<Hash>,
    pub held: Vec<Hash>,
    pub views: BTreeMap<String, Vec<Hash>>,
}

impl Unsent {
    /// The counts the status and the session overview report: unsent landings, landings in the
    /// held request, and unsent view ops by view.
    pub fn counts(&self) -> (u64, u64, BTreeMap<String, u64>) {
        (
            self.landings.len() as u64,
            self.held.len() as u64,
            self.views
                .iter()
                .map(|(n, o)| (n.clone(), o.len() as u64))
                .collect(),
        )
    }
}

impl Store {
    /// Writes the sync state steps `steps` of `remote` as one sealed batch of its own (D-3RSCW).
    /// The first batch that writes a sync ref activates `sync_state` in it. Every value keeps
    /// its rules ([`check_ref`]) and names only objects the store holds. `Upload` expects
    /// `upload` absent; `Send` expects `upload` to name its push and `pending` absent; `Reply`
    /// and `ClearUpload` expect their ref to name their push. A failed compare is
    /// `StoreError::SyncBusy` and writes nothing; so is any other refusal. A holder never
    /// writes sync state, and an open held batch refuses the write.
    pub fn write_sync(&mut self, remote: &str, steps: Vec<SyncStep>) -> Result<(), StoreError> {
        self.ensure_writable()?;
        if self.mode()? == StoreMode::Holder {
            return Err(StoreError::Refused(
                "a holder store cannot write ref meta/sync/".into(),
            ));
        }
        if !is_remote(remote) {
            return Err(StoreError::Refused(format!(
                "{remote:?} is not a remote name"
            )));
        }
        if self.open_batch().is_some() {
            return Err(batch_open("before a sync state write"));
        }
        let mut upload: Option<String> = push_of(self.get_ref(&upload_ref(remote))?.as_deref());
        let mut pending: Option<String> = push_of(self.get_ref(&pending_ref(remote))?.as_deref());
        let busy = || StoreError::SyncBusy(remote.to_string());
        let mut refs: Vec<(String, Vec<u8>)> = Vec::new();
        for step in steps {
            match step {
                SyncStep::Canon(v) => {
                    refs.push((canon_ref(remote), SyncValue::Canon(v).encode()));
                }
                SyncStep::View { name, value } => {
                    if !is_view_name(&name) {
                        return Err(StoreError::Refused(format!("{name:?} is not a view name")));
                    }
                    refs.push((view_ref(remote, &name), SyncValue::View(value).encode()));
                }
                SyncStep::Upload(u) => {
                    if upload.is_some() {
                        return Err(busy());
                    }
                    upload = Some(u.push.clone());
                    refs.push((upload_ref(remote), SyncValue::Upload(u).encode()));
                }
                SyncStep::Send(p) => {
                    if upload.as_deref() != Some(p.push.as_str()) || pending.is_some() {
                        return Err(busy());
                    }
                    upload = None;
                    pending = Some(p.push.clone());
                    refs.push((upload_ref(remote), Vec::new()));
                    refs.push((pending_ref(remote), SyncValue::Pending(p).encode()));
                }
                SyncStep::Reply { push } => {
                    if pending.as_deref() != Some(push.as_str()) {
                        return Err(busy());
                    }
                    pending = None;
                    refs.push((pending_ref(remote), Vec::new()));
                }
                SyncStep::ClearUpload { push } => {
                    if upload.as_deref() != Some(push.as_str()) {
                        return Err(busy());
                    }
                    upload = None;
                    refs.push((upload_ref(remote), Vec::new()));
                }
            }
        }
        if refs.is_empty() {
            return Ok(());
        }
        let items = self.ref_items_pub(refs);
        self.write_sync_items(items)
    }

    /// Writes ref `items` that may hold sync refs (a sync state batch, a clone's origin batch),
    /// as one sealed batch of its own, held batches off for it, with the sync ref rules.
    pub(crate) fn write_sync_items(&mut self, items: Vec<(u8, Vec<u8>)>) -> Result<(), StoreError> {
        let held = self.held();
        // no batch is open (the callers refuse one), so turning held batches off seals nothing
        self.set_held(false)?;
        let wrote = self.write_refs_as(items, RefWrite::Sync);
        self.set_held(held)?;
        wrote.map(|_| ())
    }
}
