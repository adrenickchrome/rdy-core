//! scc9 (D-47HVF, D-3RSCW, D-KQ4CA, D-VRH3Z, D-2VM48): `rdy push` and `rdy pull`, and the sync
//! lines of `rdy status`, run against a loopback TLS hub that speaks the sync protocol. Each test
//! drives the real binary through the project's daemon, with a private user config home that
//! trusts the hub's generated CA and holds a fresh sign-in.
//!
//! The hub takes each put batch with core's checks and moves canon on a write. A pull is
//! served from a copy of the project that the test lands more work in, as a hub reads its store.
#[allow(clippy::duplicate_mod)]
#[path = "../support/sync_hub.rs"]
mod sync_hub;

use rdy_engine::Rdy;
use rdy_model::Hash;
use rdy_qa::fixtures::{land, writer};
use serde_json::{Value, json};
use sync_hub::*;

/// The hub tip no project here holds: a landing that only the hub has.
fn hub_only() -> Hash {
    Hash::of(b"scc9 hub-only landing")
}

fn writes(k: &Kit) -> Vec<Req> {
    k.hub.of("/write")
}

/// scc9: `rdy push` and `rdy pull` round trips. A push seeds an empty hub repo, and its write
/// carries the capturing client's `rdy`. `--force` with no force move says `force not needed`
/// exactly and returns no card. A forced push that moves hub canon returns a card first; the
/// answer sends the force move. A pull takes hub canon; `--take --view` takes only the named
/// view, and a view the hub lacks is `E_NOT_FOUND`.
#[test]
fn scc9_cli_push_pull_force_and_take() {
    let k = kit(&["a", "b"]);
    let l2 = k.proj.landings[1];
    // a push seeds an empty hub repo: render inputs name this client
    let v = cli_ok(&k, &["push"]);
    assert_eq!(v["canon"]["status"], "taken", "{v}");
    assert_eq!(v["force_not_needed"], false);
    assert_eq!(k.hub.canon(), Some(l2.to_string()));
    let sent = writes(&k)[0].json();
    assert_eq!(sent["kind"], "landings");
    assert_eq!(
        sent["render_inputs"]["rdy"],
        rdy_engine::formats::formats().rdy
    );
    // `force not needed` exactly, no card, an ordinary extend of the hub tip: a hub repo that
    // holds the project's first landing
    let k2 = kit(&["a", "b"]);
    k2.hub.set_canon(Some(k2.proj.landings[0]), None);
    let v = cli_ok(&k2, &["push", "--force"]);
    assert_eq!(v["force_not_needed"], true, "{v}");
    assert_eq!(v["notes"], json!(["force not needed"]));
    assert!(v.get("card").is_none(), "{v}");
    let last = writes(&k2).pop().unwrap().json();
    assert_eq!(last["kind"], "landings");
    assert_eq!(last["expected"]["hash"], k2.proj.landings[0].to_string());
    // the text says it too, and prints no card
    let k3 = kit(&["a", "b"]);
    k3.hub.set_canon(Some(k3.proj.landings[0]), None);
    let t = cli_text(&k3, &["push", "--force"]);
    assert!(t.contains("force not needed"), "{t}");
    assert!(!t.contains("card hash"), "{t}");
    // a forced push that moves hub canon returns a card first and sends nothing
    let kf = kit(&["a", "b"]);
    let l2f = kf.proj.landings[1];
    let tip = hub_only();
    kf.hub.set_canon(Some(tip), None);
    let card = cli_ok(&kf, &["push", "--force"]);
    assert_eq!(card["status"], "confirmation_required", "{card}");
    assert_eq!(card["move"], "force_push");
    assert_eq!(card["before"], tip.to_string());
    assert_eq!(card["after"], l2f.to_string());
    assert_eq!(card["remote"], "origin");
    assert!(writes(&kf).is_empty(), "the card sent no write");
    let text = card["card"].as_str().unwrap();
    assert!(text.contains(&format!("Hub canon before: {tip}")), "{text}");
    // the person's answer, with the card's hash, sends the force move
    let hash = card["card_hash"].as_str().unwrap().to_string();
    let v = cli_ok(
        &kf,
        &[
            "push",
            "--force",
            "--card-hash",
            &hash,
            "--answer",
            "confirm",
        ],
    );
    assert_eq!(v["canon"]["status"], "taken", "{v}");
    let forced = writes(&kf).pop().unwrap().json();
    assert_eq!(forced["kind"], "force");
    assert_eq!(forced["expected"]["hash"], tip.to_string());
    assert_eq!(kf.hub.canon(), Some(l2f.to_string()));
    // a pull takes hub canon: the hub serves a copy of this project with one more landing
    let (_hub_dir, mut store) = k.proj.copy(&k.home);
    let l3 = sync_hub::land_doc(&mut store, "c");
    k.hub.serve(store.read());
    k.hub.set_canon(Some(l3), Some(k.proj.repo_id()));
    let v = cli_ok(&k, &["pull"]);
    assert_eq!(v["canon"]["status"], "pulled", "{v}");
    assert_eq!(v["canon"]["canon"], l3.to_string());
    // a take of the hub's tip that removes nothing needs no card
    let v = cli_ok(&k, &["pull", "--take"]);
    assert!(v.get("card").is_none(), "{v}");
    // a view the hub lacks is E_NOT_FOUND, with D-47HVF's words
    let e = cli_err(&k, &["pull", "--take", "--view", "main"]);
    assert_eq!(e["code"], "E_NOT_FOUND", "{e}");
    assert_eq!(e["message"], "view main is not on remote origin");
    assert_eq!(
        e["fix"],
        "check the name, or push the view from a machine that has it"
    );
    drop(store);
}

/// scc9: a canon take returns a card that names the remote, local canon before and the hub's
/// tip after, and each landing that leaves canon. The project holds a local landing that the
/// hub's chain lacks, and has not fetched the hub's tip, so the card says it cannot name every
/// landing. The person's answer takes the hub's tip.
#[test]
fn scc9_cli_take_card_names_the_leaving_landing() {
    let hub = Hub::start();
    let home = Home::new(&hub);
    home.signed_in(hub.origin());
    let mut proj = Proj::new(&["a"]);
    proj.remote(&home, &format!("{}{REMOTE_PATH}", hub.origin()));
    let (_hub_dir, mut store) = proj.copy(&home);
    proj.land_more(&["local"]);
    let l3 = sync_hub::land_doc(&mut store, "hub");
    hub.serve(store.read());
    hub.set_canon(Some(l3), Some(proj.repo_id()));
    let local_head = proj.landings[1];
    let k = Kit { hub, home, proj };
    let card = cli_ok(&k, &["pull", "--take"]);
    assert_eq!(card["status"], "confirmation_required", "{card}");
    assert_eq!(card["move"], "take");
    assert_eq!(card["remote"], "origin");
    assert_eq!(card["before"], local_head.to_string());
    assert_eq!(card["after"], l3.to_string());
    let leaving: Vec<&str> = card["leaving"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap())
        .collect();
    assert!(leaving.contains(&local_head.to_string().as_str()), "{card}");
    assert_eq!(card["complete"], false, "the hub's tip was never fetched");
    let text = card["card"].as_str().unwrap();
    assert!(text.contains(&format!("Local canon after: {l3}")), "{text}");
    assert!(text.contains("does not hold every landing"), "{text}");
    // the person's answer takes the hub's tip
    let hash = card["card_hash"].as_str().unwrap().to_string();
    let v = cli_ok(
        &k,
        &[
            "pull",
            "--take",
            "--card-hash",
            &hash,
            "--answer",
            "confirm",
        ],
    );
    assert!(v.get("card").is_none(), "{v}");
    let status = cli_ok(&k, &["status"]);
    assert_eq!(status["sync"][0]["applied"], l3.to_string(), "{status}");
    drop(store);
}

/// scc9: `rdy push --restore <dump>` into an empty hub repo needs no card; into one with canon
/// it returns the card first, and the answer restores. The reply names the restore's omissions.
#[test]
fn scc9_restore_cli_path() {
    let k = kit(&["a", "b"]);
    let dump = k.proj.path().join("p.rdydump");
    let o = run(&k, &["dump", "--out", dump.to_str().unwrap()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    // an empty hub repo: no card, the dump's tip becomes hub canon
    let v = cli_ok(&k, &["push", "--restore", "p.rdydump"]);
    assert!(v.get("card").is_none(), "{v}");
    assert_eq!(v["canon"]["status"], "taken", "{v}");
    assert_eq!(k.hub.canon(), Some(k.proj.landings[1].to_string()));
    let sent = writes(&k).pop().unwrap().json();
    assert_eq!(sent["kind"], "restore");
    // a hub repo that holds canon: the card first, and nothing is sent
    let tip = hub_only();
    k.hub.set_canon(Some(tip), None);
    let before = writes(&k).len();
    let card = cli_ok(&k, &["push", "--restore", "p.rdydump"]);
    assert_eq!(card["status"], "confirmation_required", "{card}");
    assert_eq!(card["move"], "restore");
    assert_eq!(card["before"], tip.to_string());
    assert_eq!(card["after"], k.proj.landings[1].to_string());
    assert_eq!(writes(&k).len(), before);
    // the answer restores; the text names what the restore did and did not do
    let hash = card["card_hash"].as_str().unwrap().to_string();
    let v = cli_ok(
        &k,
        &[
            "push",
            "--restore",
            "p.rdydump",
            "--card-hash",
            &hash,
            "--answer",
            "confirm",
        ],
    );
    assert_eq!(v["canon"]["status"], "taken", "{v}");
    assert_eq!(v["holder"], false);
    assert_eq!(k.hub.canon(), Some(k.proj.landings[1].to_string()));
    // the text names what the restore sent without render inputs: an untitled project's tag
    let k2 = kit(&["a"]);
    {
        let mut r = Rdy::open(k2.proj.path()).unwrap();
        r.tag(&rdy_qa::fixtures::approver(), "v1.0.0", None, None)
            .unwrap();
        r.close().unwrap();
    }
    let dump2 = k2.proj.path().join("q.rdydump");
    let o = run(&k2, &["dump", "--out", dump2.to_str().unwrap()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let t = cli_text(&k2, &["push", "--restore", "q.rdydump"]);
    assert!(
        t.contains("tag v1.0.0 was sent without render inputs"),
        "{t}"
    );
    assert!(t.contains("canon taken"), "{t}");
    // a restore takes no view and no force
    let e = cli_err(&k, &["push", "--restore", "p.rdydump", "--force"]);
    assert_eq!(e["message"], "force is not used by push with restore");
    assert_eq!(e["fix"], "remove force");
    let e = cli_err(&k, &["push", "--restore", "p.rdydump", "--view", "main"]);
    assert_eq!(e["message"], "views is not used by push with restore");
}

/// scc9: a tip that moves after the card changes its hash: the answer to the old card is
/// refused with `E_ITEM_MOVED` and the fresh card, and nothing is sent.
#[test]
fn scc9_cli_answer_to_a_changed_card_is_refused() {
    let k = kit(&["a"]);
    k.hub.set_canon(Some(hub_only()), None);
    let card = cli_ok(&k, &["push", "--force"]);
    let hash = card["card_hash"].as_str().unwrap().to_string();
    let moved = Hash::of(b"scc9 the tip moved");
    k.hub.set_canon(Some(moved), None);
    let before = writes(&k).len();
    let e = cli_err(
        &k,
        &[
            "push",
            "--force",
            "--card-hash",
            &hash,
            "--answer",
            "confirm",
        ],
    );
    assert_eq!(e["code"], "E_ITEM_MOVED", "{e}");
    assert_eq!(e["corrected"]["before"], moved.to_string());
    assert_ne!(e["corrected"]["card_hash"], json!(hash));
    assert_eq!(writes(&k).len(), before);
    // a cancel of the current card sends nothing either
    let fresh = e["corrected"]["card_hash"].as_str().unwrap().to_string();
    let v = cli_ok(
        &k,
        &[
            "push",
            "--force",
            "--card-hash",
            &fresh,
            "--answer",
            "cancel",
        ],
    );
    assert_eq!(v["status"], "cancelled", "{v}");
    assert_eq!(writes(&k).len(), before);
    assert_eq!(k.hub.canon(), Some(moved.to_string()));
}

/// scc9: `rdy status` shows each remote's sync state and repeats each uncovered-removal warning
/// with D-47HVF's words. A local change that holds a removed block version and that no removal
/// covers keeps its bytes; a pull of the hub's removal leaves it uncovered.
#[test]
fn scc9_status_repeats_uncovered_removals() {
    use rdy_engine::redaction::RedactInput;
    use rdy_engine::{SYNC_UNCOVERED_FIX, SYNC_UNCOVERED_MESSAGE};
    const SECRET: &str = "scc9-secret-token";
    let hub = Hub::start();
    let home = Home::new(&hub);
    home.signed_in(hub.origin());
    // the hub's project holds a doc whose prose is the secret
    let dir = rdy_qa::daemon::short_tempdir();
    let hub_dir = dir.path().join("hub");
    let mut store = Rdy::init(&hub_dir, "rdy-standard").unwrap();
    let w = rdy_qa::fixtures::write(
        &mut store,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": SECRET}
        ]),
    )
    .unwrap();
    let prose = w.created["$p"].clone();
    land(&mut store, "seed").unwrap();
    store.close().unwrap();
    // the project is a copy; it writes the prose away and back in a view of its own
    let local = dir.path().join("local");
    copy_tree(&hub_dir, &local);
    let mut mine = Rdy::open(&local).unwrap();
    for body in ["scc9-away-text", SECRET] {
        rdy_qa::fixtures::write(
            &mut mine,
            "keep",
            json!([{"op": "update", "id": prose, "body": body}]),
        )
        .unwrap();
    }
    mine.close().unwrap();
    // the hub removes the secret and lands the removal
    let mut store = Rdy::open(&hub_dir).unwrap();
    store
        .redact_in(
            &writer(),
            "redact",
            RedactInput {
                targets: vec![prose.clone()],
                reason_class: "secret".into(),
                reason: "a leaked token".into(),
                dry_run: false,
            },
        )
        .unwrap();
    land(&mut store, "redact").unwrap();
    let tip = store.read().canon_head().unwrap();
    hub.serve(store.read());
    hub.set_canon(Some(tip), None);
    let id = store.read().repo_id().unwrap().to_string();
    hub.set_canon(Some(tip), Some(id));
    // serve the local copy as the project
    let served = Served::at(&local, &home, &hub);
    let o = served.run(&["--json", "pull"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let (_, status) = parsed(&served.run(&["--json", "status"]));
    // the sync state of the remote
    let sync = &status["sync"][0];
    assert_eq!(sync["remote"], "origin", "{status}");
    assert_eq!(sync["applied"], tip.to_string());
    // each uncovered change is repeated, with the words of D-47HVF
    let w = status["uncovered_warnings"].as_array().expect("warnings");
    assert_eq!(w.len(), 1, "{status}");
    let (change, removal) = (
        w[0]["change"].as_str().unwrap(),
        w[0]["removal"].as_str().unwrap(),
    );
    let message = SYNC_UNCOVERED_MESSAGE
        .replace("<change>", change)
        .replace("<removal>", removal);
    assert_eq!(w[0]["code"], "W_REDACTION_UNCOVERED");
    assert_eq!(w[0]["message"], message);
    assert_eq!(w[0]["fix"], SYNC_UNCOVERED_FIX);
    let line = format!("W_REDACTION_UNCOVERED: {message}; {SYNC_UNCOVERED_FIX}");
    assert!(
        status["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x == &json!(line)),
        "{status}"
    );
    // the text repeats it on every read, after the sync lines
    let text = String::from_utf8(served.run(&["status"]).stdout).unwrap();
    assert!(text.contains("sync origin: fetched"), "{text}");
    assert!(text.contains(&format!("warning: {line}")), "{text}");
    let again = String::from_utf8(served.run(&["status"]).stdout).unwrap();
    assert_eq!(again.matches("W_REDACTION_UNCOVERED").count(), 1, "{again}");
    served.stop();
    drop(store);
}

fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let p = entry.path();
        if p.is_dir() {
            copy_tree(&p, &to.join(entry.file_name()));
        } else {
            std::fs::copy(&p, to.join(entry.file_name())).unwrap();
        }
    }
}

/// A project at a path of its own with the share remote `origin` on `hub`.
struct Served<'a> {
    path: std::path::PathBuf,
    home: &'a Home,
}

impl<'a> Served<'a> {
    fn at(path: &std::path::Path, home: &'a Home, hub: &Hub) -> Served<'a> {
        let cfg = path.join(".rdy.yaml");
        let mut text = std::fs::read_to_string(&cfg).unwrap();
        text.push_str("serve:\n  idle_timeout: 60\n");
        std::fs::write(&cfg, text).unwrap();
        rdy_engine::hub_remotes::RemoteService::new(path, &home.config())
            .add(
                "origin",
                &format!("{}{REMOTE_PATH}", hub.origin()),
                rdy_engine::hub_config::RemoteMode::Share,
                rdy_engine::hub_remotes::RemoteLevel::Project,
            )
            .unwrap();
        Served {
            path: path.to_path_buf(),
            home,
        }
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        command(&self.path, self.home, args).output().unwrap()
    }

    fn stop(&self) {
        let _ = self.run(&["serve", "--stop"]);
    }
}

impl Drop for Served<'_> {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------------------------------------------------------------------------------------------
// rdy clone <url> (D-D5J18, D-7PYYS, D-47HVF)
// ---------------------------------------------------------------------------------------------

/// A parent directory for a clone, and the clone's path in it (absent).
fn target() -> (tempfile::TempDir, std::path::PathBuf) {
    let parent = tempfile::tempdir().unwrap();
    let dir = parent.path().join("clone");
    (parent, dir)
}

/// The names in `dir`.
fn names(dir: &std::path::Path) -> Vec<String> {
    let mut n: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    n.sort();
    n
}

/// `rdy --json clone <url> <dir>` from `at`, as the kit's person: its JSON and whether it passed.
fn clone_at(k: &Kit, at: &std::path::Path, url: &str, dir: &std::path::Path) -> (bool, Value) {
    let o = command(
        at,
        &k.home,
        &["--json", "clone", url, dir.to_str().unwrap()],
    )
    .output()
    .unwrap();
    parsed(&o)
}

/// The words of an error reply.
fn words(v: &Value) -> (&str, &str, &str) {
    (
        v["code"].as_str().unwrap_or_default(),
        v["message"].as_str().unwrap_or_default(),
        v["fix"].as_str().unwrap_or_default(),
    )
}

/// scc9: a URL clone installs in one step: the parent then holds the clone alone, with the
/// hub's repo id, the origin at the captured tip, the remote `hub` (mode share, the URL in
/// canonical form) and the sync state fetched and applied at that tip. It writes no
/// credential, no local config and no user config, and sends hub read steps only.
#[test]
fn scc9_clone_url_is_atomic_and_records_origin() {
    let k = kit(&["a", "b"]);
    let (_copy, _store) = serve_copy(&k, &[]);
    let tip = k.proj.landings[1];
    let (parent, dir) = target();
    let creds = std::fs::read(k.home.creds()).unwrap();
    let config = std::fs::read(k.home.config()).unwrap();
    let url = hub_url(&k);
    // a scheme in capitals is the same URL: the clone records its canonical form
    let given = url.replace("https://", "HTTPS://");
    let v = cli_ok(&k, &["clone", &given, dir.to_str().unwrap()]);
    assert_eq!(v["url"], url, "{v}");
    assert_eq!(v["remote"], "hub");
    assert_eq!(v["repo_id"], k.proj.repo_id());
    assert_eq!(v["fork"], false);
    assert_eq!(v["canon"], tip.to_string());
    assert_eq!(v["origin"]["repo_id"], k.proj.repo_id());
    assert_eq!(v["origin"]["landing"], tip.to_string());
    assert_eq!(v["landings"], 2);
    assert_eq!(
        v["fetched"]["records_taken"].as_u64().map(|n| n > 0),
        Some(true)
    );
    // one step: nothing beside the clone, no temporary directory left
    assert_eq!(names(parent.path()), ["clone"]);
    let r = Rdy::open(&dir).unwrap();
    let st = r.read();
    assert_eq!(st.canon_head(), Some(tip));
    assert_eq!(st.repo_id().unwrap().to_string(), k.proj.repo_id());
    let reader = st.reader();
    assert_eq!(reader.sync_remotes().unwrap(), ["hub"]);
    let sync = reader.sync_canon("hub").unwrap().expect("sync state");
    assert_eq!(sync.url, url);
    assert_eq!(sync.repo_id.to_string(), k.proj.repo_id());
    assert_eq!(
        (sync.fetched, sync.applied, sync.held),
        (Some(tip), Some(tip), None)
    );
    drop(st);
    r.close().unwrap();
    let hub = rdy_engine::hub_config::HubConfig::load(Some(&dir), Some(&k.home.config())).unwrap();
    let remote = hub.remote("hub").expect("the hub remote");
    assert_eq!(remote.url.as_str(), url);
    assert_eq!(remote.mode, rdy_engine::hub_config::RemoteMode::Share);
    assert_eq!(hub.remotes().len(), 1);
    // no credential, local config or user config write
    assert_eq!(std::fs::read(k.home.creds()).unwrap(), creds);
    assert_eq!(std::fs::read(k.home.config()).unwrap(), config);
    assert!(!dir.join(".rdy/local.yaml").exists());
    // hub read steps only, the settings read before any fetch
    let routes = k.hub.routes();
    assert!(
        routes.iter().all(|r| READ_ROUTES.contains(&r.as_str())),
        "{routes:?}"
    );
    assert_eq!(&routes[..3], ["discovery", "state", "settings"]);
}

/// scc9: a source with another URL scheme gets `rdy remote add`'s refusal of an address that is
/// not HTTPS; a hub repo whose batches carry another repo id is `E_REPO_MISMATCH`. Each leaves
/// the target as it was: absent, or an empty directory.
#[test]
fn scc9_clone_unsupported_scheme_and_repo_mismatch_leave_target() {
    let k = kit(&["a"]);
    let (_copy, _store) = serve_copy(&k, &[]);
    let added = cli_err(
        &k,
        &[
            "remote",
            "add",
            "--mode",
            "share",
            "x",
            "ftp://127.0.0.1/acme/docs",
        ],
    );
    assert_eq!(
        words(&added),
        (
            "E_BAD_INPUT",
            rdy_engine::hub_types::BAD_URL_MESSAGE,
            rdy_engine::hub_types::BAD_URL_FIX
        )
    );
    for url in [
        "ftp://127.0.0.1/acme/docs",
        "rdy://hub/acme/docs",
        "file:///tmp/x",
    ] {
        for existing in [false, true] {
            let (parent, dir) = target();
            if existing {
                std::fs::create_dir(&dir).unwrap();
            }
            let before = tree(&dir);
            let (ok, v) = clone_at(&k, k.proj.path(), url, &dir);
            assert!(!ok, "{url}: {v}");
            assert_eq!(words(&v), words(&added), "{url}");
            assert_eq!(tree(&dir), before, "{url}");
            assert_eq!(names(parent.path()).len(), usize::from(existing));
        }
    }
    assert!(k.hub.requests().is_empty());
    // the hub repo names another repo id than its batches carry
    k.hub.set_canon(
        Some(k.proj.landings[0]),
        Some("0123456789abcdef0123456789abcdef".into()),
    );
    for existing in [false, true] {
        let (parent, dir) = target();
        if existing {
            std::fs::create_dir(&dir).unwrap();
        }
        let before = tree(&dir);
        let (ok, v) = clone_at(&k, k.proj.path(), &hub_url(&k), &dir);
        assert!(!ok, "{v}");
        assert_eq!(v["code"], "E_REPO_MISMATCH", "{v}");
        assert_eq!(tree(&dir), before);
        assert_eq!(names(parent.path()).len(), usize::from(existing));
    }
}

/// scc9: the clone keeps the tip its state step captured, though hub canon moves after the
/// settings read; the tags come as a pull takes them, and no view comes.
#[test]
fn scc9_clone_uses_captured_canon_and_tags_without_views() {
    let k = kit(&["a", "b"]);
    {
        let mut r = Rdy::open(k.proj.path()).unwrap();
        r.tag(&rdy_qa::fixtures::approver(), "v1.0.0", None, None)
            .unwrap();
        r.close().unwrap();
    }
    let (_copy, store) = serve_copy(&k, &["c"]);
    let captured = k.proj.landings[1];
    let moved = store.read().canon_head().unwrap();
    assert_ne!(captured, moved);
    assert!(
        !store.read().view_names().is_empty(),
        "the source has a view"
    );
    k.hub.set_canon(Some(captured), Some(k.proj.repo_id()));
    k.hub.move_on_settings(moved);
    let (_parent, dir) = target();
    let v = cli_ok(&k, &["clone", &hub_url(&k), dir.to_str().unwrap()]);
    assert_eq!(v["canon"], captured.to_string(), "{v}");
    assert_eq!(k.hub.canon(), Some(moved.to_string()));
    assert_eq!(v["fetched"]["tags_taken"], json!(["v1.0.0"]));
    assert_eq!(
        (v["tags"].as_u64(), v["views"].as_u64()),
        (Some(1), Some(0))
    );
    assert_eq!(v["origin"]["tag"], "v1.0.0");
    // every fetch request is rooted at the captured tip, with no base
    for r in k.hub.of("/known") {
        let b = r.json();
        assert_eq!(b["root"]["hash"], captured.to_string());
        assert!(b["base"].is_null());
    }
    let r = Rdy::open(&dir).unwrap();
    let st = r.read();
    assert_eq!(st.canon_head(), Some(captured));
    assert!(st.view_names().is_empty());
    assert_eq!(st.versions().unwrap().len(), 1);
    drop(st);
    r.close().unwrap();
}

/// scc9: a user config that already holds a share remote refuses the clone with the present
/// `E_REMOTE_EXISTS`, before any hub request and before anything is written.
#[test]
fn scc9_clone_share_remote_conflict_refuses_before_writes() {
    let k = kit(&["a"]);
    let (_copy, _store) = serve_copy(&k, &[]);
    let mut config = std::fs::read_to_string(k.home.config()).unwrap();
    config.push_str(&format!(
        "remotes:\n  team:\n    url: {}\n    mode: share\n",
        hub_url(&k)
    ));
    std::fs::write(k.home.config(), &config).unwrap();
    let (parent, dir) = target();
    let (ok, v) = clone_at(&k, parent.path(), &hub_url(&k), &dir);
    assert!(!ok, "{v}");
    assert_eq!(
        words(&v),
        (
            "E_REMOTE_EXISTS",
            rdy_engine::REMOTE_EXISTS_MESSAGE,
            rdy_engine::REMOTE_EXISTS_FIX
        )
    );
    assert!(k.hub.requests().is_empty(), "{:?}", k.hub.routes());
    assert!(names(parent.path()).is_empty());
    assert_eq!(std::fs::read_to_string(k.home.config()).unwrap(), config);
}

/// scc9: a hub repo with no canon (fixture v2's step 2: an upload and no canon) refuses with
/// D-D5J18's words exactly, before the settings read.
#[test]
fn scc9_clone_no_canon_refusal_is_exact() {
    let k = kit(&["a"]);
    let f = fixture_v2();
    let state = &f["steps"][2]["reply"]["body"];
    assert!(state["canon"].is_null());
    k.hub
        .set_canon(None, state["repo_id"].as_str().map(str::to_string));
    let (parent, dir) = target();
    let url = hub_url(&k);
    let (ok, v) = clone_at(&k, k.proj.path(), &url, &dir);
    assert!(!ok, "{v}");
    assert_eq!(
        words(&v),
        (
            "E_BAD_INPUT",
            format!("hub repo {url} has no canon to clone").as_str(),
            "make the project with rdy init, add this hub repo as a share remote, then push"
        )
    );
    assert_eq!(k.hub.routes(), ["discovery", "state"]);
    assert!(names(parent.path()).is_empty());
}

/// The bundle a clone writes in full: every captured member set, with a pack of its own.
fn full_bundle() -> Value {
    let mut pack = builtin_pack();
    pack["name"] = json!("team-pack");
    let mut b = pack_settings(pack, Some(&this_rdy()));
    b["coverage_gate"] = json!("no_lower");
    b["coverage_target"] = json!(70);
    b["decisions_home"] = json!("decisions");
    b["propose"] = json!({"cadence": "task", "changes": 5});
    b["title"] = json!("Team docs");
    b["description"] = json!("The team's docs.");
    b
}

/// The entries of a project outside its store, `.rdy.yaml` and its packs.
fn layout(dir: &std::path::Path) -> Vec<String> {
    tree(dir)
        .unwrap()
        .into_keys()
        .filter(|p| !p.starts_with(".rdy/store/") && !p.starts_with(".rdy/packs/"))
        .collect()
}

/// scc9: a URL clone writes the bundle's settings into `.rdy.yaml`, its pack into one file
/// under `.rdy/packs/` in the source form that resolves to it, the one remote `hub`, and the
/// sync state; nothing else beside what `rdy init` makes.
#[test]
fn scc9_clone_writes_only_bundle_settings_hub_remote_and_sync_state() {
    let k = kit(&["a"]);
    let (_copy, _store) = serve_copy(&k, &[]);
    let bundle = full_bundle();
    k.hub.settings(bundle.clone());
    let (_parent, dir) = target();
    let url = hub_url(&k);
    let v = cli_ok(&k, &["clone", &url, dir.to_str().unwrap()]);
    assert_eq!(
        v["settings"]["hub"],
        json!([
            "pack",
            "coverage_gate",
            "coverage_target",
            "decisions_home",
            "propose",
            "title",
            "description"
        ])
    );
    assert_eq!(v["settings"]["defaults"], json!([]));
    assert_eq!(v["warnings"], json!([]));
    // the layout rdy init makes, and no more
    let fresh = tempfile::tempdir().unwrap();
    Rdy::init(fresh.path(), "rdy-standard")
        .unwrap()
        .close()
        .unwrap();
    assert_eq!(layout(&dir), layout(fresh.path()));
    let packs: Vec<String> = tree(&dir)
        .unwrap()
        .into_keys()
        .filter(|p| p.starts_with(".rdy/packs/"))
        .collect();
    assert_eq!(packs, [".rdy/packs/", ".rdy/packs/team-pack.yaml"]);
    // the file holds the pack's source form: the effective pack less what the engine adds
    let pack: Value =
        serde_yaml_ng::from_slice(&std::fs::read(dir.join(".rdy/packs/team-pack.yaml")).unwrap())
            .unwrap();
    let effective: rdy_schema::Pack = serde_json::from_value(bundle["pack"].clone()).unwrap();
    assert_eq!(
        pack,
        serde_json::to_value(rdy_schema::source_form(&effective)).unwrap()
    );
    assert_ne!(pack, bundle["pack"]);
    let config: Value =
        serde_yaml_ng::from_slice(&std::fs::read(dir.join(".rdy.yaml")).unwrap()).unwrap();
    assert_eq!(
        config,
        json!({
            "pack": "team-pack",
            "name": "Team docs",
            "description": "The team's docs.",
            "decisions": {"home": "decisions"},
            "coverage": {"gate": "no_lower", "target": 70},
            "propose": {"cadence": "task", "changes": 5},
            "remotes": {"hub": {"url": url, "mode": "share"}},
        })
    );
    // the effective settings are the bundle's
    let r = Rdy::open(&dir).unwrap();
    let st = r.read();
    assert_eq!(serde_json::to_value(st.pack()).unwrap(), bundle["pack"]);
    assert_eq!(st.project_name(), "Team docs");
    let reader = st.reader();
    assert_eq!(reader.sync_remotes().unwrap(), ["hub"]);
    assert!(
        reader
            .sync_unsent("hub")
            .unwrap()
            .is_none_or(|u| u.counts().0 == 0)
    );
    drop(st);
    r.close().unwrap();
    assert!(!dir.join(".rdy/local.yaml").exists());
}

/// scc9: a failing settings load, a failing `fsck` and a broken SD1 closure each refuse before
/// the install and leave the target as it was; so does the hub's `min_rdy` stop. A clone that
/// passes keeps the SD1 deltas and the `snapshot_delta` feature.
#[test]
fn scc9_clone_checks_fsck_settings_and_sd1_before_install() {
    let k = kit(&["a", "b"]);
    let (_copy, _store) = serve_copy(&k, &[]);
    let url = hub_url(&k);
    let session = rdy_engine::classify("human:qa", None, None, rdy_model::Role::Reader).unwrap();
    let run = |hook: &mut dyn FnMut(&std::path::Path) -> Result<(), rdy_engine::RdyError>| {
        let (parent, dir) = target();
        let got = rdy_engine::clone::clone_url_checked(
            &session,
            &url,
            &dir,
            &k.home.config(),
            Some(hook),
        );
        (got.map(|_| ()), parent, dir)
    };
    // a settings load that fails: a pack the edited settings name, but no file holds
    let (got, parent, dir) = run(&mut |p| {
        let cfg = p.join(".rdy.yaml");
        let text = std::fs::read_to_string(&cfg).unwrap();
        std::fs::write(
            &cfg,
            text.replace("pack: rdy-standard", "pack: no-such-pack"),
        )
        .unwrap();
        Ok(())
    });
    assert_eq!(got.unwrap_err().code, "E_CONFIG");
    assert!(!dir.exists());
    assert!(names(parent.path()).is_empty());
    // a store that fails its checks: one byte in the middle of its largest log segment changed
    let (got, parent, dir) = run(&mut |p| {
        let mut files: Vec<_> = tree(&p.join(".rdy/store"))
            .unwrap()
            .into_iter()
            .filter(|(n, b)| n.starts_with("segments/") && n.ends_with(".log") && b.len() > 64)
            .collect();
        files.sort_by_key(|(_, b)| std::cmp::Reverse(b.len()));
        let (name, mut bytes) = files.remove(0);
        let at = bytes.len() / 2;
        bytes[at] ^= 0xff;
        std::fs::write(p.join(".rdy/store").join(name), bytes).unwrap();
        Ok(())
    });
    assert!(got.is_err());
    assert!(!dir.exists());
    assert!(names(parent.path()).is_empty());
    // the hook sees a full project; with no fault, the clone passes
    let (got, _parent, dir) = run(&mut |_| Ok(()));
    got.unwrap();
    let r = Rdy::open(&dir).unwrap();
    assert!(r.fsck().is_ok());
    // the snapshot_delta feature, and one SD1 delta for every landing
    assert!(
        r.store()
            .features()
            .features
            .iter()
            .any(|f| f.name == rdy_store::format::FEATURE_SNAPSHOT_DELTA)
    );
    let st = r.read();
    let mut at = st.canon_head();
    let mut landings = 0;
    while let Some(h) = at {
        assert!(
            st.reader().snapshot_delta_of(&h).unwrap().is_some(),
            "landing {h}"
        );
        at = st.reader().landing(&h).unwrap().prev;
        landings += 1;
    }
    assert_eq!(landings, 2);
    drop(st);
    r.close().unwrap();
    // a broken SD1 closure: the hub's batches leave out every delta
    k.hub.get_omits(rdy_store::kinds::SNAPSHOT_DELTA);
    let (parent, dir) = target();
    let (ok, v) = clone_at(&k, k.proj.path(), &url, &dir);
    assert!(!ok, "{v}");
    assert!(!dir.exists());
    assert!(names(parent.path()).is_empty());
    // the hub's min_rdy above this rdy stops the clone before its settings read
    k.hub.min_rdy("99.0.0");
    let before = k.hub.requests().len();
    let (ok, v) = clone_at(&k, k.proj.path(), &url, &dir);
    assert!(!ok, "{v}");
    assert_eq!(v["code"], "E_STORE_FORMAT", "{v}");
    assert_eq!(&k.hub.routes()[before..], ["discovery", "state"]);
    assert!(names(parent.path()).is_empty());
}

/// The session of an in-process clone.
fn reader_session() -> rdy_engine::Session {
    rdy_engine::classify("human:qa", None, None, rdy_model::Role::Reader).unwrap()
}

/// r59014: a URL clone opens its new project once and builds canon's tension set once, with and
/// without tags: the handle that applied canon runs `fsck` and the origin completion. After the
/// install, the complete `fsck` and origin match a fresh, independent check. The test counts
/// work and times nothing.
#[test]
fn r59014_clone_reuses_one_handle_and_canon_set() {
    for tagged in [false, true] {
        let k = kit(&["a", "b"]);
        if tagged {
            let mut r = Rdy::open(k.proj.path()).unwrap();
            r.tag(&rdy_qa::fixtures::approver(), "v1.0.0", None, None)
                .unwrap();
            r.close().unwrap();
        }
        let (_copy, _store) = serve_copy(&k, &[]);
        let url = hub_url(&k);
        let (_parent, dir) = target();
        let (opens, runs) = (
            rdy_engine::clone::clone_opens(),
            rdy_engine::tensions::set::canon_part_runs(),
        );
        let got = rdy_engine::clone::clone_url_checked(
            &reader_session(),
            &url,
            &dir,
            &k.home.config(),
            None,
        )
        .unwrap();
        let (opened, built) = (
            rdy_engine::clone::clone_opens() - opens,
            rdy_engine::tensions::set::canon_part_runs() - runs,
        );
        // the clone's new store makes its empty canon's part when it is made (an init); the
        // build of the canon the clone applies is the one more
        let init_runs = {
            let before = rdy_engine::tensions::set::canon_part_runs();
            let fresh = tempfile::tempdir().unwrap();
            Rdy::init(fresh.path(), "rdy-standard")
                .unwrap()
                .close()
                .unwrap();
            rdy_engine::tensions::set::canon_part_runs() - before
        };
        assert_eq!(
            (opened, built - init_runs),
            (1, 1),
            "tagged {tagged}: one normal open and one canon tension-set build"
        );
        assert_eq!(got.tags, usize::from(tagged));
        assert_eq!(got.landings, 2);
        // a fresh, independent check of the installed project
        let r = Rdy::open(&dir).unwrap();
        let fresh = r.fsck().unwrap();
        assert_eq!(
            serde_json::to_value(&got.fsck).unwrap(),
            serde_json::to_value(&fresh).unwrap(),
            "tagged {tagged}"
        );
        let origin = r.read().origin().unwrap().expect("the origin");
        assert_eq!(
            serde_json::to_value(&got.origin).unwrap(),
            serde_json::to_value(rdy_engine::clone::OriginReply::from(&origin)).unwrap()
        );
        assert_eq!(origin.tag.as_deref(), tagged.then_some("v1.0.0"));
        r.close().unwrap();
    }
}

/// r59014: the clone checks on the handle that applied canon, and each refusal it gave before
/// stays the same: a corrupt record frame and a settings file the project cannot load (a step
/// edits the new project, which then opens again, as a person's edit would), a hub that leaves
/// out the snapshot deltas, a pack whose digest is not the clone's, and a user config with a
/// share remote already. Each leaves the destination, absent or empty, as it was.
#[test]
fn r59014_clone_keeps_bad_record_and_settings_refusals() {
    let k = kit(&["a", "b"]);
    let (_copy, _store) = serve_copy(&k, &[]);
    let url = hub_url(&k);
    let session = reader_session();
    for existing in [false, true] {
        let hooked =
            |hook: &mut dyn FnMut(&std::path::Path) -> Result<(), rdy_engine::RdyError>| {
                let (parent, dir) = target();
                if existing {
                    std::fs::create_dir(&dir).unwrap();
                }
                let before = tree(&dir);
                let got = rdy_engine::clone::clone_url_checked(
                    &session,
                    &url,
                    &dir,
                    &k.home.config(),
                    Some(hook),
                );
                assert_eq!(tree(&dir), before, "existing {existing}");
                assert_eq!(names(parent.path()).len(), usize::from(existing));
                got.map(|_| ()).unwrap_err()
            };
        // a corrupt frame: one byte in the middle of the largest log segment changed
        let e = hooked(&mut |p| {
            let mut files: Vec<_> = tree(&p.join(".rdy/store"))
                .unwrap()
                .into_iter()
                .filter(|(n, b)| n.starts_with("segments/") && n.ends_with(".log") && b.len() > 64)
                .collect();
            files.sort_by_key(|(_, b)| std::cmp::Reverse(b.len()));
            let (name, mut bytes) = files.remove(0);
            let at = bytes.len() / 2;
            bytes[at] ^= 0xff;
            std::fs::write(p.join(".rdy/store").join(name), bytes).unwrap();
            Ok(())
        });
        assert_eq!(e.code, "E_IO", "{}", e.message);
        assert!(
            e.message
                .contains("damages acknowledged data; refusing to truncate"),
            "{}",
            e.message
        );
        // settings that do not load: a pack no file holds
        let e = hooked(&mut |p| {
            let cfg = p.join(".rdy.yaml");
            let text = std::fs::read_to_string(&cfg).unwrap();
            std::fs::write(
                &cfg,
                text.replace("pack: rdy-standard", "pack: no-such-pack"),
            )
            .unwrap();
            Ok(())
        });
        assert_eq!(e.code, "E_CONFIG", "{}", e.message);
    }
    // the refusals a person's clone meets: the hub's, and the user config's
    let refused = |k: &Kit, label: &str| {
        for existing in [false, true] {
            let (parent, dir) = target();
            if existing {
                std::fs::create_dir(&dir).unwrap();
            }
            let before = tree(&dir);
            let (ok, v) = clone_at(k, k.proj.path(), &url, &dir);
            assert!(!ok, "{label}: {v}");
            assert_eq!(tree(&dir), before, "{label}");
            assert_eq!(names(parent.path()).len(), usize::from(existing), "{label}");
            if !existing {
                return v;
            }
        }
        unreachable!()
    };
    // a pack whose digest is not the clone's
    let mut other = builtin_pack();
    other["version"] = json!("0.0.1-other");
    k.hub
        .settings(pack_settings(other, Some(this_rdy().as_str())));
    let v = refused(&k, "pack digest");
    let (message, fix) = unfit_words("pack", &url, Some(this_rdy().as_str()));
    assert_eq!(
        words(&v),
        ("E_BAD_INPUT", message.as_str(), fix.as_str()),
        "pack digest"
    );
    k.hub.settings(settings_step(5));
    // a hub that leaves out the snapshot deltas
    k.hub.get_omits(rdy_store::kinds::SNAPSHOT_DELTA);
    let v = refused(&k, "missing snapshot delta");
    assert_eq!(v["code"], "E_BAD_RECORD", "{v}");
    assert_eq!(
        v["fix"],
        "request this stream again; run rdy fsck at its source"
    );
    let message = v["message"].as_str().unwrap();
    assert!(
        message.starts_with("stream record ") && message.contains("unlisted dependency"),
        "{v}"
    );
    // a user config that already holds a share remote
    let k2 = kit(&["a"]);
    let (_copy2, _store2) = serve_copy(&k2, &[]);
    let mut config = std::fs::read_to_string(k2.home.config()).unwrap();
    config.push_str(&format!(
        "remotes:\n  team:\n    url: {}\n    mode: share\n",
        hub_url(&k2)
    ));
    std::fs::write(k2.home.config(), &config).unwrap();
    let (parent, dir) = target();
    let (ok, v) = clone_at(&k2, parent.path(), &hub_url(&k2), &dir);
    assert!(!ok, "{v}");
    assert_eq!(
        words(&v),
        (
            "E_REMOTE_EXISTS",
            rdy_engine::REMOTE_EXISTS_MESSAGE,
            rdy_engine::REMOTE_EXISTS_FIX
        )
    );
    assert!(names(parent.path()).is_empty());
}

/// scc9 (CLI part): each settings refusal comes after discovery, state and the `min_rdy` check
/// and before any fetch, and leaves the target as it was.
#[test]
fn scc9_clone_settings_refusal_fetches_nothing_and_leaves_dir() {
    let k = kit(&["a"]);
    let (_copy, _store) = serve_copy(&k, &[]);
    let url = hub_url(&k);
    for (label, body, member, captured) in refusal_cases() {
        k.hub.settings(body);
        for existing in [false, true] {
            let (parent, dir) = target();
            if existing {
                std::fs::create_dir(&dir).unwrap();
            }
            let before = tree(&dir);
            let from = k.hub.requests().len();
            let (ok, v) = clone_at(&k, k.proj.path(), &url, &dir);
            assert!(!ok, "{label}: {v}");
            let (message, fix) = unfit_words(member, &url, captured.as_deref());
            assert_eq!(
                words(&v),
                ("E_BAD_INPUT", message.as_str(), fix.as_str()),
                "{label}"
            );
            assert_eq!(
                &k.hub.routes()[from..],
                ["discovery", "state", "settings"],
                "{label}"
            );
            assert_eq!(tree(&dir), before, "{label}");
            assert_eq!(names(parent.path()).len(), usize::from(existing), "{label}");
        }
    }
    assert_eq!(fetches(&k), 0);
}

/// D-H67EN: a pull whose lookup of the held push the hub refuses keeps `held`, goes on and warns
/// `W_UPSTREAM_HELD_LOOKUP` with the hub's code and fix.
fn not_found_body(push: &str) -> String {
    json!({"code": "E_HUB_NOT_FOUND", "message": "No push of that id.",
        "fix": format!("check the push id, and sign in with the account that opened push {push}"),
        "issues": [], "request_id": "req_00000000000000000000000009"})
    .to_string()
}

#[test]
fn scc8_cli_held_lookup_refusal_warns_and_the_pull_goes_on() {
    let k = kit(&["a", "b"]);
    k.hub.fault(Fault::HoldWrite(4));
    let v = cli_ok(&k, &["push"]);
    assert_eq!(v["canon"]["status"], "held", "{v}");
    let push = v["canon"]["push"].as_str().unwrap().to_string();
    k.hub
        .fault(Fault::RefuseOutcome(404, not_found_body(&push)));
    let o = run(&k, &["pull"]);
    assert!(o.status.success(), "{o:?}");
    let out = String::from_utf8_lossy(&o.stdout);
    let want = format!(
        "W_UPSTREAM_HELD_LOOKUP: the outcome of held push {push} could not be read: E_HUB_NOT_FOUND\nfix: check the push id, and sign in with the account that opened push {push}"
    );
    assert!(out.contains(&want), "{out}");
}

// ---------------------------------------------------------------------------------------------
// The fetch of a hub dump (D-7CDEQ)
// ---------------------------------------------------------------------------------------------

const DUMP_ID: &str = "dmp_01J9";
const DUMP_NEXT: &str = "rdy push --restore <file>, or rdy load <file> <dir> for a copy";

/// A kit whose hub serves a dump of the kit's own project as [`DUMP_ID`], with the project's
/// repo id in its state step; and that dump's `rdy dump` reply.
fn dump_kit() -> (Kit, Value) {
    let k = kit(&["a", "b"]);
    let src = k.proj.path().join("src.rdydump");
    let made = cli_ok(&k, &["dump", "--out", src.to_str().unwrap()]);
    k.hub.dump(DUMP_ID, std::fs::read(&src).unwrap());
    std::fs::remove_file(&src).unwrap();
    // the daemon holds the store: the repo id comes from the dump's reply
    k.hub
        .set_canon(None, made["repo_id"].as_str().map(str::to_string));
    (k, made)
}

fn dump_words(v: &Value) -> (String, String, String) {
    (
        v["code"].as_str().unwrap_or_default().into(),
        v["message"].as_str().unwrap_or_default().into(),
        v["fix"].as_str().unwrap_or_default().into(),
    )
}

/// A refusal's code, message and fix.
type Refusal = (String, String, String);

/// scc9: the closed inputs of `rdy pull --dump`, each with D-7CDEQ's exact words, before any
/// request: `--out` or `--url` without `--dump`, `--dump` without `--out`, `--dump` with any
/// other pull flag (the relay flags too), and `--remote` with `--url`. A dump id of 1 or 256
/// bytes passes; 0 and 257 bytes, a byte outside ASCII and a control byte are refused.
#[test]
fn scc9_dump_inputs_and_cli_refusals_are_exact() {
    let (k, _) = dump_kit();
    let url = hub_url(&k);
    let bad = |m: &str, f: &str| ("E_BAD_INPUT".to_string(), m.to_string(), f.to_string());
    let only = bad(
        "a dump fetch takes only out and remote or url",
        "run the dump fetch on its own",
    );
    let cases: Vec<(Vec<&str>, Refusal)> = vec![
        (
            vec!["pull", "--out", "f.rdydump"],
            bad("out goes only with dump", "add --dump <id>, or remove out"),
        ),
        (
            vec!["pull", "--url", &url],
            bad("url goes only with dump", "add --dump <id>, or remove url"),
        ),
        (
            vec!["pull", "--dump", DUMP_ID],
            bad("a dump fetch needs out", "add --out <file>"),
        ),
        (
            vec!["pull", "--dump", DUMP_ID, "--out", "f", "--take"],
            only.clone(),
        ),
        (
            vec!["pull", "--dump", DUMP_ID, "--out", "f", "--strict"],
            only.clone(),
        ),
        (
            vec!["pull", "--dump", DUMP_ID, "--out", "f", "--view", "main"],
            only.clone(),
        ),
        (
            vec!["pull", "--dump", DUMP_ID, "--out", "f", "--card-hash", "x"],
            only.clone(),
        ),
        (
            vec![
                "pull", "--dump", DUMP_ID, "--out", "f", "--remote", "origin", "--url", &url,
            ],
            bad("name remote or url, not both", "keep one of them"),
        ),
    ];
    let id_refusal = bad(
        "the dump id is not 1 to 256 bytes of printable ASCII",
        "pass the id that the hub's dump job gave",
    );
    let long = "x".repeat(257);
    let mut all = cases;
    for id in ["", long.as_str(), "d\u{e9}", "a\tb"] {
        all.push((
            vec!["pull", "--dump", id, "--out", "f.rdydump"],
            id_refusal.clone(),
        ));
    }
    for (args, want) in &all {
        assert_eq!(dump_words(&cli_err(&k, args)), *want, "{args:?}");
    }
    assert!(k.hub.requests().is_empty(), "no refusal reaches the hub");
    // 1 and 256 bytes pass the checks: the hub refuses the ids it never made
    for id in ["x".to_string(), "x".repeat(256)] {
        let e = cli_err(&k, &["pull", "--dump", &id, "--out", "n.rdydump"]);
        assert_eq!(e["code"], "E_HUB_NOT_FOUND", "{e}");
        assert_eq!(
            e["message"],
            format!("no dump {id} of yours in acme/docs"),
            "{e}"
        );
    }
    assert!(!k.proj.path().join("n.rdydump").exists());
}

/// scc9: the fetch by the default remote and by `--url` in a project go through the daemon, and
/// by `--url` outside every project in this process; each runs discovery, state and dump with
/// the machine's sign-in, and needs no project role. The project's store and sync state do not
/// move.
#[test]
fn scc9_dump_remote_and_url_use_sign_in_without_project_role() {
    let (k, made) = dump_kit();
    let before = cli_ok(&k, &["status"]);
    let url = hub_url(&k);
    let outside = tempfile::tempdir().unwrap();
    let runs: Vec<(&str, Vec<String>)> = vec![
        (
            "remote",
            vec![
                "pull".into(),
                "--dump".into(),
                DUMP_ID.into(),
                "--out".into(),
                "r.rdydump".into(),
            ],
        ),
        (
            "url in a project",
            vec![
                "pull".into(),
                "--dump".into(),
                DUMP_ID.into(),
                "--out".into(),
                "u.rdydump".into(),
                "--url".into(),
                url.clone(),
            ],
        ),
    ];
    for (case, args) in runs {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let n = k.hub.requests().len();
        let v = cli_ok(&k, &args);
        assert_eq!(v["digest"], made["digest"], "{case}: {v}");
        let routes: Vec<String> = k.hub.routes()[n..].to_vec();
        assert_eq!(routes, ["discovery", "state", "dump"], "{case}");
        for r in &k.hub.requests()[n..] {
            assert!(
                r.headers
                    .iter()
                    .any(|(h, v)| h == "authorization" && v == &format!("Bearer {ACCESS}")),
                "{case}: the machine's sign-in"
            );
        }
    }
    // outside every project: in this process, with the user config's trust and sign-in
    let n = k.hub.requests().len();
    let o = command_in(
        outside.path(),
        &k.home,
        &[
            "--json",
            "pull",
            "--dump",
            DUMP_ID,
            "--out",
            "o.rdydump",
            "--url",
            &url,
        ],
    )
    .output()
    .unwrap();
    let (ok, v) = parsed(&o);
    assert!(ok, "{v}");
    assert_eq!(v["digest"], made["digest"]);
    assert!(outside.path().join("o.rdydump").is_file());
    assert_eq!(k.hub.routes()[n..], ["discovery", "state", "dump"]);
    assert!(!outside.path().join(".rdy").exists(), "no project is made");
    // the store and its sync state did not move
    let after = cli_ok(&k, &["status"]);
    assert_eq!(before["sync"], after["sync"]);
    assert!(k.hub.of("/write").is_empty());
}

/// scc9: the reply names the checked file as `rdy dump` names it (out, size, dump format, kind,
/// repo id, trailer digest) and the next step, exactly; the text says the same.
#[test]
fn scc9_dump_reply_names_checked_file_and_next() {
    let (k, made) = dump_kit();
    let out = k.proj.path().join("fetched.rdydump");
    let v = cli_ok(
        &k,
        &["pull", "--dump", DUMP_ID, "--out", out.to_str().unwrap()],
    );
    for m in ["bytes", "dump_format", "kind", "repo_id", "digest"] {
        assert_eq!(v[m], made[m], "{m}: {v}");
    }
    assert_eq!(v["out"], out.display().to_string());
    assert_eq!(v["next"], DUMP_NEXT);
    let t = cli_text(&k, &["pull", "--dump", DUMP_ID, "--out", "again.rdydump"]);
    assert!(t.contains(&format!("next: {DUMP_NEXT}")), "{t}");
    assert!(t.contains(made["digest"].as_str().unwrap()), "{t}");
    assert!(t.contains(made["repo_id"].as_str().unwrap()), "{t}");
}

/// cra2 (D-FNRXT): a push carries a landed removal with its proof. The put batches hold one
/// dropped record for the removed block version (and one for its carrier change), all under the
/// removal's apply; the apply
/// and the removal landing travel present; the hub holds all of them after the write; and no
/// batch holds the removed bytes.
#[test]
fn cra2_push_carries_a_landed_removal_with_its_proof() {
    use rdy_engine::redaction::RedactInput;
    use rdy_store::kinds;
    use rdy_store::object_refs::ObjectKey;
    use rdy_store::sync_stream::{StreamReader, StreamRecord};

    const SECRET: &str = "cra2-secret-token";
    let k = kit(&["a"]);
    let removal = {
        let mut r = Rdy::open(k.proj.path()).unwrap();
        let w = rdy_qa::fixtures::write(
            &mut r,
            "seed",
            json!([
                {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "b", "title": "B", "doc_type": "page"}},
                {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}},
                {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": SECRET}
            ]),
        )
        .unwrap();
        land(&mut r, "seed").unwrap();
        let prose = w.created["$p"].clone();
        r.redact_in(
            &writer(),
            "rm",
            RedactInput {
                targets: vec![prose],
                reason_class: "secret".into(),
                reason: "a leaked token".into(),
                dry_run: false,
            },
        )
        .unwrap();
        let landing: Hash = land(&mut r, "rm").unwrap().parse().unwrap();
        r.close().unwrap();
        landing
    };
    let v = cli_ok(&k, &["push"]);
    assert_eq!(v["canon"]["status"], "taken", "{v}");
    assert_eq!(k.hub.canon(), Some(removal.to_string()));

    let mut dropped = vec![];
    let mut present: Vec<ObjectKey> = vec![];
    for put in k.hub.of("/objects/put") {
        assert!(
            !put.body
                .windows(SECRET.len())
                .any(|w| w == SECRET.as_bytes()),
            "a put body holds the removed bytes"
        );
        let (mut reader, _) = StreamReader::open_history(&put.body[..]).unwrap();
        while let Some(rec) = reader.next().unwrap() {
            match rec {
                StreamRecord::Dropped { key, apply, .. } => dropped.push((key, apply)),
                StreamRecord::Present { key, .. } => present.push(key),
            }
        }
    }
    // the removed block version, and the change that carried it, drop under one apply
    let blocks: Vec<_> = dropped
        .iter()
        .filter(|(key, _)| key.kind == kinds::BLOCK)
        .collect();
    assert_eq!(blocks.len(), 1, "{dropped:?}");
    let (gone, apply) = *blocks[0];
    assert!(dropped.iter().all(|(_, a)| *a == apply), "{dropped:?}");
    let proof = ObjectKey {
        kind: kinds::REDACTION_APPLY,
        hash: apply,
    };
    let landing = ObjectKey {
        kind: kinds::LANDING,
        hash: removal,
    };
    assert!(present.contains(&proof), "the apply travels present");
    assert!(present.contains(&landing), "the removal landing travels");
    let held = k.hub.shared.repo.lock().unwrap().held.clone();
    for key in dropped
        .iter()
        .map(|(key, _)| *key)
        .chain([gone, proof, landing])
    {
        assert!(held.contains(&key), "the hub holds {key:?}");
    }
}

/// A URL clone of `k`'s hub repo from a hub that sends a canon tip's state, returned at Ready;
/// then the hub serves no record, so the history stays incomplete whatever fetches it. Returns
/// the clone's parent, its directory and its progress.
fn partial_clone(
    k: &Kit,
) -> (
    tempfile::TempDir,
    std::path::PathBuf,
    rdy_engine::partial_history::HistoryProgress,
) {
    k.hub.send_canon_state();
    let session = rdy_engine::classify("human:qa", None, None, rdy_model::Role::Reader).unwrap();
    let (parent, dir) = target();
    let s = rdy_engine::clone::clone_url(&session, &hub_url(k), &dir, &k.home.config()).unwrap();
    let p = s.history.expect("a ready clone's progress");
    assert!(!p.complete && p.held < p.total, "{p:?}");
    k.hub.shared.repo.lock().unwrap().store = None;
    (parent, dir, p)
}

/// A13: `rdy push --restore` in a project whose history is still arriving is
/// E_HISTORY_PENDING with the exact message and fix, before any capture, put or write.
#[test]
fn r59019_restore_refuses_while_history_pending() {
    let k = kit(&["a", "b"]);
    let (_copy, _store) = serve_copy(&k, &[]);
    let dump = k.proj.path().join("p.rdydump");
    let o = run(&k, &["dump", "--out", dump.to_str().unwrap()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let (_parent, dir, p) = partial_clone(&k);
    let before = k.hub.routes().len();
    let o = command(
        &dir,
        &k.home,
        &["--json", "push", "--restore", dump.to_str().unwrap()],
    )
    .output()
    .unwrap();
    let (ok, v) = parsed(&o);
    let _ = command(&dir, &k.home, &["serve", "--stop"]).output();
    assert!(!ok, "{v}");
    assert_eq!(
        words(&v),
        (
            "E_HISTORY_PENDING",
            rdy_engine::partial_history::history_pending_message(p.held, p.total).as_str(),
            rdy_engine::partial_history::HISTORY_PENDING_FIX,
        )
    );
    let routes = k.hub.routes();
    assert!(
        routes[before..]
            .iter()
            .all(|r| r != "objects/put" && r != "write"),
        "{routes:?}"
    );
}

/// O17: while `rdy clone <url>` fetches the history in the foreground, the clone is served to
/// every other client: a second command and an MCP session in the clone answer, its history
/// still arriving, and the clone's reply is complete once the held get is let go. No daemon log line reaches the
/// clone's stderr.
#[test]
fn r59019_clone_serves_other_commands_while_history_arrives() {
    let k = kit(&["a", "b"]);
    let (_copy, _store) = serve_copy(&k, &[]);
    k.hub.send_canon_state();
    let hold = k.hub.hold_history_get();
    let (_parent, dir) = target();
    let clone = command(
        k.proj.path(),
        &k.home,
        &["--json", "clone", &hub_url(&k), dir.to_str().unwrap()],
    )
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
    hold.wait_arrived();
    let o = command(&dir, &k.home, &["--json", "status"])
        .output()
        .unwrap();
    let (ok, status) = parsed(&o);
    let mut mcp = rdy_qa::mcp::McpClient::start_proxy_at(&dir, &k.home.env());
    mcp.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "a", "role": "reader"}),
    )
    .ok();
    let log = mcp.call("rdy_log", json!({}));
    drop(mcp);
    hold.release();
    let done = clone.wait_with_output().unwrap();
    let _ = command(&dir, &k.home, &["serve", "--stop"]).output();
    assert!(ok, "{status}\n{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(status["history"]["complete"], false, "{status}");
    let log = log.ok();
    assert_eq!(log["warnings"][0]["code"], "W_HISTORY_PENDING", "{log}");
    let (ok, v) = parsed(&done);
    let err = String::from_utf8_lossy(&done.stderr);
    assert!(ok, "{v}\n{err}");
    assert_eq!(v["history"]["complete"], true, "{v}");
    assert_eq!(v["warnings"], json!([]), "{v}");
    assert!(!err.contains("rdy serve:"), "{err}");
}
