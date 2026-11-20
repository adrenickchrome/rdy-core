//! The fetch of a hub repo's dump (D-7CDEQ, D-H0BZN, D-47HVF, D-N8PR2): `rdy pull --dump <id>
//! --out <file>` and `rdy_pull {dump, out, remote?, url?}`. The hub's dump job made the dump;
//! this fetch reads it through the sync step `dump` and places it as `rdy dump` places a file.
//!
//! 1. **Which hub repo.** In a project, the remote as a pull selects it, with the share-mode
//!    check of D-47HVF ([`SyncService::fetch_dump`]); or `url`, an HTTPS hub repo URL, in a
//!    project or outside one ([`fetch_dump_url`]), with discovery under the origin fence as a URL
//!    clone runs it (D-D5J18). Each call uses the machine's sign-in for that hub.
//! 2. **State.** Discovery, then the state step. The fetch stops when the hub's `min_rdy` is
//!    higher than this rdy (`E_STORE_FORMAT`, D-4XHPH), and, with a remote, when the hub repo's
//!    id is not this project's (`E_REPO_MISMATCH`). No dump step is sent then.
//! 3. **Dump.** The dump step names the id as it was given. The body streams into
//!    `<file>.partial-<pid>` and is never held whole in memory.
//! 4. **Check.** The partial file is checked as `rdy load` checks a dump before it writes
//!    (`crate::load::check_dump`): a failure gives the load's code and text, naming `<file>`.
//!    The dump's repo id must equal the hub repo id of the state step, else `E_REPO_MISMATCH`.
//! 5. **Place.** The checked file is linked to `<file>` as `rdy dump` links it
//!    (`crate::dump::place`): `<file>` must not exist and appears whole or not at all. The texts
//!    of `rdy dump` apply, with `rdy pull --dump` as the command name.
//!
//! The fetch writes no store, no ref and no sync state, and takes no sync lock, so it needs no
//! project role: the hub authorizes the step by the credential. A lost reply, a cut body or a
//! failed check removes the partial file and places nothing; the person runs the fetch again.
use crate::dump::{DUMP_FETCH_CMD, ids, place};
use crate::engine::Engine;
use crate::error::{E_BAD_INPUT, RdyError, upstream_protocol};
use crate::hub_http::HttpHubTransport;
use crate::hub_publish::repo_mismatch;
use crate::hub_sync::{SyncService, Target, check_minimum, repo_name};
use crate::hub_sync_wire::{DumpRequest, HttpSyncWire, SyncStateRequest};
use crate::hub_types::{RemoteUrl, SystemHubClock};
use crate::load::{CheckedDump, check_dump, io_at};
use crate::session::Session;
use rdy_model::{Hash, RepoId};
use rdy_store::dump_chain::DumpKind;
use rdy_store::transition::hex;
use serde::Serialize;
use std::io::{BufWriter, Write};
use std::path::Path;

/// The most bytes of a dump id (D-7CDEQ).
pub const MAX_DUMP_ID_BYTES: usize = 256;

/// The next step a fetch names (D-7CDEQ).
pub const DUMP_FETCH_NEXT: &str = "rdy push --restore <file>, or rdy load <file> <dir> for a copy";

/// What the fetch placed, as `rdy dump` reports a dump: the file, its length, its dump format,
/// its kind, its repo id and its trailer digest, which a person compares with the digest the
/// hub's dump job shows; and the next step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DumpFetched {
    pub out: String,
    pub bytes: u64,
    pub dump_format: u32,
    pub kind: DumpKind,
    #[serde(serialize_with = "ids::repo")]
    pub repo_id: RepoId,
    #[serde(serialize_with = "hex::one::serialize")]
    pub digest: Hash,
    pub next: String,
}

/// Refuses a dump id that is not 1 to [`MAX_DUMP_ID_BYTES`] bytes of printable ASCII
/// (D-7CDEQ). The id is never parsed further.
pub fn check_dump_id(id: &str) -> Result<(), RdyError> {
    let ok = (1..=MAX_DUMP_ID_BYTES).contains(&id.len())
        && id.bytes().all(|b| (0x20..=0x7e).contains(&b));
    if ok {
        return Ok(());
    }
    Err(RdyError::new(
        E_BAD_INPUT,
        "the dump id is not 1 to 256 bytes of printable ASCII",
        "pass the id that the hub's dump job gave",
    ))
}

impl SyncService<'_> {
    /// Fetches the dump `dump` of the hub repo of the project's remote `remote` (the share
    /// remote, else the only remote, when None) to the new file `out` (see the module docs).
    /// The project's store is read for its repo id and the output guards of `rdy dump`, never
    /// written; no remote guard is taken.
    pub fn fetch_dump(
        &self,
        engine: &dyn Engine,
        session: &Session,
        remote: Option<&str>,
        dump: &str,
        out: &Path,
    ) -> Result<DumpFetched, RdyError> {
        check_dump_id(dump)?;
        self.with_target_guarded(session, remote, false, |t| {
            let local = {
                let read = engine.read();
                read.check_out(out, DUMP_FETCH_CMD)?;
                read.repo_id()?
            };
            self.fetch_on(t, Some(local), dump, out)
        })
    }

    /// Fetches the dump `dump` of the hub repo at `url` to the new file `out`, with this
    /// coordinator's settings: the project's in a project, else the user config's. No repo id is
    /// compared with a project's: the dump's must equal the hub repo's.
    pub fn fetch_dump_at(
        &self,
        session: &Session,
        url: &str,
        dump: &str,
        out: &Path,
    ) -> Result<DumpFetched, RdyError> {
        let url = RemoteUrl::parse(url)?;
        check_dump_id(dump)?;
        self.url_target(session, &url, false, |t| self.fetch_on(t, None, dump, out))
    }

    /// Steps 2 to 5 on `t`; `local` is the project's repo id for a remote's fetch.
    fn fetch_on(
        &self,
        t: &Target,
        local: Option<RepoId>,
        dump: &str,
        out: &Path,
    ) -> Result<DumpFetched, RdyError> {
        // 2. discovery and state; the minimum and the remote's repo id stop before the dump
        let cx = self.context_at(t, None, None)?;
        let base_url = self.wire.discovery(&cx, &t.url)?.sync_base;
        let cx = self.context_at(t, None, Some(&base_url))?;
        let state = self.wire.state(&cx, &SyncStateRequest::new(&cx.claim))?;
        drop(cx);
        check_minimum(&state, &repo_name(&t.url))?;
        if let Some(local) = local
            && state.repo_id.is_some_and(|r| r != local)
        {
            return Err(repo_mismatch());
        }
        let request = DumpRequest {
            dump: dump.to_string(),
        };
        // 3 to 5: stream, sync and check the partial file, then link it in place
        let checked: CheckedDump = place(out, DUMP_FETCH_CMD, |partial, file| {
            let cx = self.context_at(t, None, Some(&base_url))?;
            let mut sink = BufWriter::new(file);
            self.wire.dump(&cx, &request, &mut sink, u64::MAX)?;
            drop(cx);
            sink.flush().map_err(|e| io_at(partial, e))?;
            let file = sink
                .into_inner()
                .map_err(|e| io_at(partial, e.into_error()))?;
            file.sync_all().map_err(|e| io_at(partial, e))?;
            drop(file);
            let checked = check_dump(partial, out)?;
            if checked.repo_id.is_none() || checked.repo_id != state.repo_id {
                return Err(repo_mismatch());
            }
            Ok(checked)
        })?;
        Ok(DumpFetched {
            out: out.display().to_string(),
            bytes: checked.bytes,
            dump_format: checked.dump_format,
            kind: checked.kind,
            repo_id: checked.repo_id.ok_or_else(upstream_protocol)?,
            digest: checked.digest,
            next: DUMP_FETCH_NEXT.to_string(),
        })
    }
}

/// Fetches the dump `dump` of the hub repo at `url` to the new file `out` for `session`, outside
/// a project: the trust and sign-in of the user config `user_file` alone, as a URL clone takes
/// them (see the module docs). No project or store is made.
pub fn fetch_dump_url(
    session: &Session,
    url: &str,
    dump: &str,
    out: &Path,
    user_file: &Path,
) -> Result<DumpFetched, RdyError> {
    let parsed = RemoteUrl::parse(url)?;
    check_dump_id(dump)?;
    let settings = crate::hub_config::HubConfig::load(None, Some(user_file))?;
    let api = HttpHubTransport::new(settings.fence(parsed.origin())?)?;
    let wire = HttpSyncWire::new(HttpHubTransport::new(settings.fence(parsed.origin())?)?);
    let service = SyncService::for_clone(
        user_file,
        &api,
        &SystemHubClock,
        &wire,
        &crate::clone::NoRenderer,
    );
    service.fetch_dump_at(session, url, dump, out)
}
