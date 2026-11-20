//! The pull world of the gate tests (scc3, crb4): a hub project and a local copy that last
//! synced at one shared landing, on a pack with one rule on claims, and the fetch and second
//! batch of a pull between them. It holds helpers only.
use rdy_engine::{Rdy, ReloadOutcome};
use rdy_model::Hash;
use rdy_qa::fixtures::{approver, land};
use rdy_qa::packs::use_pack;
use rdy_qa::tensions::{j_typo, put_j};
use rdy_store::segment::Location;
use serde_json::json;

/// A pack with one rule on claims, `ok-claims`, of `severity`.
pub(crate) fn pack(severity: &str) -> String {
    format!(
        "\
name: scc3
version: \"1\"
extends: rdy-standard
block_types:
  claim:
    prefix: c
    traits: [normative, describes]
    body: required
    fields:
      ok: {{ kind: bool }}
rules:
  - name: ok-claims
    select: type = claim
    require: fields.ok = true
    severity: {severity}
    message: ok-claims message
    fix: ok-claims fix
"
    )
}

/// Points the project at `pack(severity)` and reloads it.
pub(crate) fn switch_pack(dir: &std::path::Path, r: &mut Rdy, severity: &str) {
    use_pack(dir, "scc3", &pack(severity), "");
    assert!(matches!(r.reload_pack().unwrap(), ReloadOutcome::Reloaded));
}

pub(crate) const URL: &str = "https://127.0.0.1:9/r/demo";

pub(crate) fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &to.join(e.file_name()));
        } else {
            std::fs::copy(&p, to.join(e.file_name())).unwrap();
        }
    }
}

/// The hub's copy and the local project after one shared landing that holds a section `s`, on
/// pack `pack(severity)`, in a stable doc; the local project last synced at that landing.
pub(crate) struct Synced {
    pub(crate) dir: tempfile::TempDir,
    pub(crate) hub: Rdy,
    pub(crate) local: Rdy,
    pub(crate) base: Hash,
    pub(crate) section: String,
    /// The log end after the last pull's fetch: where its second batch starts.
    pub(crate) fetched: Option<Location>,
}

pub(crate) fn synced(severity: &str) -> Synced {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("hub");
    drop(Rdy::init(&a, "rdy-standard").unwrap());
    use_pack(&a, "scc3", &pack(severity), "");
    let mut hub = Rdy::open(&a).unwrap();
    let w = put_j(
        &mut hub,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview", "lifecycle": "stable"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$n", "fields": {"title": "Rules"}}
        ]),
        j_typo(),
    );
    land(&mut hub, "seed").unwrap();
    let base = hub.read().canon_head().unwrap();
    drop(hub);
    let b = dir.path().join("local");
    copy_dir(&a, &b);
    let mut local = Rdy::open(&b).unwrap();
    let repo = local.store().reader().repo_id().unwrap().unwrap();
    local
        .store_mut()
        .write_sync(
            "origin",
            vec![rdy_store::sync_state::SyncStep::Canon(
                rdy_store::sync_state::SyncCanon {
                    repo_id: repo,
                    url: URL.into(),
                    fetched: Some(base),
                    applied: Some(base),
                    held: None,
                },
            )],
        )
        .unwrap();
    Synced {
        hub: Rdy::open(&a).unwrap(),
        local,
        base,
        section: w.created["$s"].clone(),
        dir,
        fetched: None,
    }
}

/// Writes a claim (`ok` when `ok`) under the section in view `view` of `r` and lands it.
pub(crate) fn land_claim(r: &mut Rdy, section: &str, view: &str, ok: bool) -> Hash {
    let fields = json!({ "ok": ok });
    put_j(
        r,
        view,
        json!([{"op": "create", "type": "claim", "parent": section, "body": format!("{view} holds."), "fields": fields}]),
        j_typo(),
    );
    land(r, view).unwrap();
    r.read().canon_head().unwrap()
}

/// Lands a new draft doc named `name` in view `name` of `r`: a hub landing apart from the
/// stable section, so it opens no change-plan item for a replayed claim.
pub(crate) fn land_apart(r: &mut Rdy, name: &str) -> Hash {
    put_j(
        r,
        name,
        json!([{"op": "create", "type": "doc", "fields": {"name": name, "title": name, "doc_type": "overview"}}]),
        j_typo(),
    );
    land(r, name).unwrap();
    r.read().canon_head().unwrap()
}

impl Synced {
    /// Fetches the hub's canon into the local project (a pull's first batch); returns the
    /// hub's tip.
    pub(crate) fn fetch(&mut self) -> Hash {
        let tip = self.hub.read().canon_head().unwrap();
        let mut out = Vec::new();
        rdy_engine::stream_export::write_history(
            &self.hub.read(),
            rdy_store::sync_stream::StreamPurpose::CanonHistory,
            tip,
            Some(self.base),
            &std::collections::BTreeSet::new(),
            &mut out,
        )
        .unwrap();
        let path = self.dir.path().join("batch");
        std::fs::write(&path, out).unwrap();
        let target = rdy_store::take_batch::FetchTarget {
            remote: "origin".into(),
            url: URL.into(),
            purpose: rdy_store::sync_stream::StreamPurpose::CanonHistory,
            root: tip,
            view: None,
        };
        rdy_engine::sync_fetch::fetch(
            &mut self.local,
            &[std::fs::File::open(&path).unwrap()],
            &target,
        )
        .unwrap();
        self.fetched = Some(self.local.store().log_end());
        tip
    }

    /// The pull request of the hub's tip `tip`, after its fetch.
    pub(crate) fn request(&self, tip: Hash, strict: bool) -> rdy_engine::sync_replay::PullRequest {
        let repo = self.hub.store().reader().repo_id().unwrap();
        rdy_engine::sync_replay::PullRequest {
            remote: "origin".into(),
            hub: rdy_engine::sync_fetch::HubState {
                repo_id: repo,
                canon: Some(tip),
            },
            take: false,
            strict,
        }
    }

    /// Runs the pull's second batch of the hub's tip `tip`, after its fetch.
    pub(crate) fn second_batch(
        &mut self,
        tip: Hash,
        strict: bool,
    ) -> rdy_engine::sync_replay::PullOutcome {
        let req = self.request(tip, strict);
        rdy_engine::sync_replay::pull(&mut self.local, &approver(), &req).unwrap()
    }

    /// Fetches the hub's canon and runs the pull's second batch.
    pub(crate) fn pull(&mut self, strict: bool) -> rdy_engine::sync_replay::PullOutcome {
        let tip = self.fetch();
        self.second_batch(tip, strict)
    }
}
