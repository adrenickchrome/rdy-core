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
use serde_json::json;
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
