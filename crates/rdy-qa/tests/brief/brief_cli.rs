//! cbrf1: rdy edit briefs, shows the briefing, cites its token and prints consequences.
use rdy_engine::Rdy;
use rdy_qa::daemon::Served;
use rdy_qa::edit_loop::*;
use rdy_qa::fixtures::rdy_bin;
use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn script(s: &Served, name: &str, body: &str) -> PathBuf {
    let p = s.dir.path().join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

/// `rdy --project <dir> [--json] edit <id>` as the human, with `EDITOR=editor`.
fn edit(s: &Served, editor: &Path, id: &str, json: bool) -> std::process::Output {
    let mut cmd = std::process::Command::new(rdy_bin());
    cmd.arg("--project").arg(s.dir.path());
    if json {
        cmd.arg("--json");
    }
    cmd.args(["edit", id])
        .env("EDITOR", editor)
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .env_remove("RDY_NO_DAEMON")
        .envs(s.env_pairs())
        .output()
        .unwrap()
}

fn new_doc(s: &Served) -> String {
    let (ok, v) = s.json(&["--json", "new", "doc", "d", "--title", "Before"]);
    assert!(ok, "{v}");
    v["created"]
        .as_object()
        .unwrap()
        .values()
        .find_map(|x| x.as_str().filter(|s| s.starts_with("d-")))
        .unwrap()
        .to_string()
}

/// Marks the doc stable through rdy edit (the CLI briefs it: setting stable needs a briefing).
fn make_stable(s: &Served, id: &str) {
    let ed = script(
        s,
        "stable.sh",
        "awk '{print} /^  title: /{print \"  lifecycle: stable\"}' \"$1\" > \"$1.new\" && mv \"$1.new\" \"$1\"\n",
    );
    let out = edit(s, &ed, id, true);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn cbrf1_rdy_edit_shows_the_briefing_header_and_its_token_lands_a_stable_edit() {
    let s = Served::new();
    let id = new_doc(&s);
    make_stable(&s, &id);
    let seen = s.dir.path().join("seen.yaml");
    let ed = script(
        &s,
        "title.sh",
        &format!(
            "cp \"$1\" '{}'\nsed -i.bak 's/title: Before/title: After/' \"$1\"\n",
            seen.display()
        ),
    );
    let out = edit(&s, &ed, &id, true);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v["consequences"].is_array(), "{v}");
    let buffer = std::fs::read_to_string(&seen).unwrap();
    assert!(
        buffer.starts_with(&format!("# rdy briefing for {id} (token ")),
        "{buffer}"
    );
    assert!(
        buffer.contains("# Lines starting with # are dropped when you save."),
        "{buffer}"
    );
    let (ok, shown) = s.json(&["--json", "show", &id, "--scope", "work"]);
    assert!(ok);
    assert_eq!(shown["fields"]["title"], "After");
    assert_eq!(shown["fields"]["lifecycle"], "stable");
}

#[test]
fn cbrf1_an_edit_whose_briefing_moved_while_the_editor_was_open_is_brief_stale_and_keeps_the_edit()
{
    let s = Served::new();
    let id = new_doc(&s);
    make_stable(&s, &id);
    let inner = script(
        &s,
        "inner.sh",
        "sed -i.bak 's/title: Before/title: Inner/' \"$1\"\n",
    );
    let nested = format!(
        "if [ -z \"$RDY_NESTED\" ]; then RDY_NESTED=1 EDITOR='{}' '{}' --project '{}' edit {id} >/dev/null; fi\nsed -i.bak 's/title: Before/title: Outer/' \"$1\"\n",
        inner.display(),
        rdy_bin().display(),
        s.dir.path().display()
    );
    let outer = script(&s, "outer.sh", &nested);
    let out = edit(&s, &outer, &id, true);
    assert!(!out.status.success());
    let e: Value = serde_json::from_str(
        String::from_utf8_lossy(&out.stderr)
            .trim()
            .lines()
            .last()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(e["code"], "E_BRIEF_STALE", "{e}");
    let saved = e["saved_edit"].as_str().expect("the edit is kept");
    assert!(
        std::fs::read_to_string(saved)
            .unwrap()
            .contains("title: Outer")
    );
    let (_, shown) = s.json(&["--json", "show", &id, "--scope", "work"]);
    assert_eq!(
        shown["fields"]["title"], "Inner",
        "the nested edit landed; the outer did not"
    );
}

#[test]
fn cbrf1_rdy_edit_prints_consequences_after_saving() {
    let s = Served::new();
    let spec = {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        let spec = spec_doc(&mut r);
        r.close().unwrap();
        spec
    };
    let ed = script(
        &s,
        "a.sh",
        "sed -i.bak 's/A holds./A holds firmly./' \"$1\"\n",
    );
    let out = edit(&s, &ed, &spec.a.to_string(), false);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("consequences:"), "{text}");
    assert!(
        text.contains(&format!(
            "suspect {}: update (references {})",
            spec.b, spec.a
        )),
        "{text}"
    );
}

#[test]
fn cbrf1_rdy_edit_in_a_fresh_view_briefs_it_as_a_write_target() {
    let s = Served::new();
    let spec = {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        let spec = stable_spec(&mut r);
        r.close().unwrap();
        spec
    };
    let ed = script(
        &s,
        "fresh.sh",
        "sed -i.bak 's/A holds./A holds in fresh./' \"$1\"\n",
    );
    let id = spec.a.to_string();
    let out = std::process::Command::new(rdy_bin())
        .arg("--project")
        .arg(s.dir.path())
        .args(["--json", "edit", &id, "--view", "fresh"])
        .env("EDITOR", &ed)
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .env_remove("RDY_NO_DAEMON")
        .envs(s.env_pairs())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (ok, shown) = s.json(&["--json", "show", &id, "--scope", "fresh"]);
    assert!(ok, "{shown}");
    assert!(
        shown["body"].to_string().contains("A holds in fresh."),
        "{shown}"
    );
}

#[test]
fn cbrf1_the_brief_op_answers_for_a_path_and_edit_block_cites_its_token() {
    let s = Served::new();
    let spec = {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        let spec = stable_spec(&mut r);
        r.close().unwrap();
        spec
    };
    assert!(s.rdy(&["status"]).status.success(), "starts the daemon");
    let rt = rdy_qa::daemon::Hosted::rt();
    rt.block_on(async {
        let hello = rdy_daemon::Hello {
            rdy: rdy_daemon::PROTO_VERSION,
            version: env!("CARGO_PKG_VERSION").into(),
            proto: rdy_daemon::Proto::Ctl,
            cwd: s.dir.path().into(),
            project: rdy_daemon::RunDir::of_project(std::path::Path::new(&s.dir.path()))
                .project_id()
                .expect("the project's .rdy"),
        };
        let rdy_daemon::Attempt::Ready(mut c) = rdy_daemon::connect(&s.run_dir(), &hello).await
        else {
            panic!("no daemon")
        };
        c.ctl("session", serde_json::json!({"actor": null, "run": null}))
            .await
            .unwrap();
        let b = c
            .ctl(
                "brief",
                serde_json::json!({"path": "spec", "scope": "canon"}),
            )
            .await
            .unwrap();
        assert_eq!(b["covers"], 6, "{b}");
        // view w9 has no head yet: it reads canon, where the spec is stable
        let yaml = c
            .ctl(
                "block",
                serde_json::json!({"id": spec.a.to_string(), "view": "w9"}),
            )
            .await
            .unwrap()["yaml"]
            .as_str()
            .unwrap()
            .replace("A holds.", "A, via ctl.");
        let e = c
            .ctl(
                "edit_block",
                serde_json::json!({"yaml": yaml, "view": "w9"}),
            )
            .await
            .unwrap_err();
        assert_eq!(e.code, "E_BRIEF_REQUIRED", "{e}");
        assert!(
            e.extra["briefing"]["base"]["token"].is_string(),
            "the refusal carries a fresh briefing"
        );
        let w = c
            .ctl(
                "edit_block",
                serde_json::json!({"yaml": yaml, "view": "w9",
            "brief": b["base"]["token"]}),
            )
            .await
            .unwrap();
        assert!(w["consequences"].is_array(), "{w}");
    });
}

/// A pack whose decisions track `disposition` (trk1).
const TRK_PACK: &str = "\
name: trk
version: \"1\"
extends: rdy-standard
block_types:
  decision:
    prefix: D
    traits: [normative]
    fields:
      disposition: { kind: str }
    tracking: [disposition]
";

/// Final-review fix round 1: the human moves a tracking field of an accepted decision with
/// `rdy edit --reason`; without a reason the refusal says to pass one, and the edit is kept.
#[test]
fn trk1_rdy_edit_moves_a_tracking_field_with_a_reason() {
    use rdy_qa::fixtures::{land, seed_doc, writer};
    let s = Served::new();
    std::fs::write(s.dir.path().join(".rdy/packs/trk.yaml"), TRK_PACK).unwrap();
    std::fs::write(s.dir.path().join(".rdy.yaml"), "pack: trk\n").unwrap();
    s.stop();
    let dec = {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        let (_doc, section, _c) = seed_doc(&mut r, "seed");
        let d = r
            .decide(
                &writer(),
                "seed",
                rdy_engine::DecisionInput {
                    parent: section,
                    statement: "Ship weekly".into(),
                    fields: [("disposition".to_string(), serde_json::json!("M2d1"))]
                        .into_iter()
                        .collect(),
                    ..Default::default()
                },
            )
            .unwrap();
        land(&mut r, "seed").unwrap();
        r.close().unwrap();
        d.created["$decision"].clone()
    };
    let ed = script(
        &s,
        "disposition.sh",
        "sed -i.bak 's/disposition: M2d1/disposition: M2d2/' \"$1\"\n",
    );
    let out = edit(&s, &ed, &dec, true);
    assert!(!out.status.success());
    let e: Value = serde_json::from_str(
        String::from_utf8_lossy(&out.stderr)
            .trim()
            .lines()
            .last()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    assert!(
        e["fix"]
            .as_str()
            .unwrap()
            .contains(&format!("rdy edit {dec} --reason")),
        "{e}"
    );
    assert!(e["saved_edit"].is_string(), "the edit is kept: {e}");
    let out = std::process::Command::new(rdy_bin())
        .arg("--project")
        .arg(s.dir.path())
        .args(["--json", "edit", &dec, "--reason", "re-sliced into M2d2"])
        .env("EDITOR", &ed)
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .env_remove("RDY_NO_DAEMON")
        .envs(s.env_pairs())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (ok, shown) = s.json(&["--json", "show", &dec, "--scope", "work", "--blame"]);
    assert!(ok, "{shown}");
    assert_eq!(shown["fields"]["disposition"], "M2d2");
    assert_eq!(shown["fields"]["status"], "accepted", "{shown}");
    assert_eq!(
        shown["history"][0]["justification"],
        serde_json::json!({"editorial": {"class": "clarify", "reason": "re-sliced into M2d2"}}),
        "{shown}"
    );
}

/// kroot1 (FW-PR3b, F4): `rdy edit` prints a `new key <id>: <key>` line for each key rdy
/// re-derived, here the doc's own key following a rename.
#[test]
fn kroot1_rdy_edit_prints_each_new_key() {
    let s = Served::new();
    let id = new_doc(&s);
    let ed = script(
        &s,
        "rename.sh",
        "sed -i.bak -E 's/^( *)name: d$/\\1name: renamed/' \"$1\"\n",
    );
    let out = edit(&s, &ed, &id, false);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.lines().any(|l| l == format!("new key {id}: renamed")),
        "{text}"
    );
}

#[test]
fn msw8_a_stale_rdy_edit_of_a_draft_is_stale_base_and_keeps_the_concurrent_change() {
    let s = Served::new();
    let id = new_doc(&s);
    let inner = script(
        &s,
        "inner.sh",
        "sed -i.bak 's/title: Before/title: Second/' \"$1\"\n",
    );
    let nested = format!(
        "if [ -z \"$RDY_NESTED\" ]; then RDY_NESTED=1 EDITOR='{}' '{}' --project '{}' edit {id} >/dev/null; fi\nsed -i.bak 's/title: Before/title: Outer/' \"$1\"\n",
        inner.display(),
        rdy_bin().display(),
        s.dir.path().display()
    );
    let outer = script(&s, "outer.sh", &nested);
    let out = edit(&s, &outer, &id, true);
    assert!(
        !out.status.success(),
        "the stale edit landed: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let e: Value = serde_json::from_str(
        String::from_utf8_lossy(&out.stderr)
            .trim()
            .lines()
            .last()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(e["code"], "E_STALE_BASE", "{e}");
    assert!(e["saved_edit"].is_string(), "the edit is kept: {e}");
    let (_, shown) = s.json(&["--json", "show", &id, "--scope", "work"]);
    assert_eq!(
        shown["fields"]["title"], "Second",
        "the concurrent change stands"
    );
}

#[test]
fn msw8_rdy_edit_text_mode_prints_a_stale_briefing_warning() {
    let s = Served::new();
    let spec = {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        let spec = spec_doc(&mut r);
        r.close().unwrap();
        spec
    };
    let inner = script(&s, "inner.sh", "sed -i.bak 's/A holds./A moved./' \"$1\"\n");
    let nested = format!(
        "if [ -z \"$RDY_NESTED\" ]; then RDY_NESTED=1 EDITOR='{}' '{}' --project '{}' edit {} >/dev/null; fi\nsed -i.bak 's/B builds on/B now builds on/' \"$1\"\n",
        inner.display(),
        rdy_bin().display(),
        s.dir.path().display(),
        spec.a
    );
    let outer = script(&s, "outer.sh", &nested);
    let out = edit(&s, &outer, &spec.b.to_string(), false);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("warning W_BRIEF_STALE: the cited briefing is stale"),
        "{text}"
    );
}

/// msw8 fix round 1: an edit token (base and conflict) checked against a block that became
/// conflicted, or whose conflict changed, while the editor was open.
mod msw8_conflicts {
    use rdy_engine::{EditBase, Rdy, RdyError, Scope};
    use rdy_model::BlockId;
    use rdy_qa::fixtures::{as_agent, body_of, land, project, seed_doc, version_of, write_as};
    use serde_json::json;

    fn view(v: &str) -> Scope {
        Scope::View(v.into())
    }

    fn update(r: &mut Rdy, v: &str, actor: &str, id: &str, body: &str) -> String {
        let base = version_of(r, &view(v), id);
        let ops = json!([{"op": "update", "id": id, "base": base, "body": body}]);
        write_as(r, &as_agent(actor, "r1"), v, ops).unwrap().change
    }

    fn conflict_of(r: &Rdy, v: &str, id: &str) -> Option<String> {
        let id = BlockId::parse(id).unwrap();
        r.read()
            .conflict_on(&view(v), &id)
            .unwrap()
            .map(|c| c.id.to_string())
    }

    /// `rdy edit` in view a: the editor buffer is the block at `base`, its body `body`.
    fn save(
        r: &mut Rdy,
        base: &str,
        conflict: Option<&str>,
        body: &str,
    ) -> Result<rdy_engine::WriteResult, RdyError> {
        let mut b = r.store().get_block(&base.parse().unwrap()).unwrap();
        b.body = body.into();
        let base = EditBase {
            version: base,
            conflict,
        };
        r.edit_block_at(&as_agent("agent:a", "r1"), "a", b, Some(base), None, None)
    }

    /// The claim lands "one" (vc); view a updates it to "alpha" (v0); view b updates it to
    /// "bravo" and lands, which rebases a onto a conflict (base vc, sides v0 and b's).
    fn a_conflicted_after_b_lands() -> (tempfile::TempDir, Rdy, String, String, String) {
        let (d, mut r) = project();
        let (_doc, _section, c) = seed_doc(&mut r, "seed");
        land(&mut r, "seed").unwrap();
        let vc = version_of(&r, &Scope::Canon, &c);
        update(&mut r, "a", "agent:a", &c, "alpha");
        let v0 = version_of(&r, &view("a"), &c);
        update(&mut r, "b", "agent:b", &c, "bravo");
        land(&mut r, "b").unwrap();
        assert!(
            conflict_of(&r, "a", &c).is_some(),
            "b's landing conflicts a"
        );
        (d, r, c, vc, v0)
    }

    #[test]
    fn msw8_a_stale_buffer_on_a_block_that_became_conflicted_is_stale_base() {
        let (_d, mut r, c, vc, v0) = a_conflicted_after_b_lands();
        let cid = conflict_of(&r, "a", &c);
        // the editor opened on a at v0, before b landed (no conflict then)
        let e = save(&mut r, &v0, None, "alpha, extended").unwrap_err();
        assert_eq!(e.code, "E_STALE_BASE", "{e:?}");
        assert!(
            e.message
                .contains(&format!("{c} changed since you read it: your base is ")),
            "{e:?}"
        );
        // even a caller that names the current conflict cannot resolve it from a stale base
        let e = save(&mut r, &v0, cid.as_deref(), "alpha, extended").unwrap_err();
        assert_eq!(e.code, "E_STALE_BASE", "{e:?}");
        assert_eq!(conflict_of(&r, "a", &c), cid, "the conflict stands");
        assert_eq!(
            body_of(&r, &view("a"), &c),
            "one",
            "a reads the conflict base"
        );
        assert_eq!(version_of(&r, &view("a"), &c), vc);
    }

    /// Final review M5: the first refusal's fix gives both parts of the edit token, so an
    /// op-level caller that does exactly what it says is not refused again.
    #[test]
    fn msw8_the_stale_buffer_fix_names_the_base_and_the_conflict() {
        let (_d, mut r, c, vc, v0) = a_conflicted_after_b_lands();
        let cid = conflict_of(&r, "a", &c).unwrap();
        let e = save(&mut r, &v0, None, "alpha, extended").unwrap_err();
        assert_eq!(e.code, "E_STALE_BASE", "{e:?}");
        let base = format!("base={vc}");
        let conflict = format!("conflict={cid}");
        assert!(e.fix.contains(&base) && e.fix.contains(&conflict), "{e:?}");
        // the retry the fix names resolves the conflict
        save(&mut r, &vc, Some(&cid), "merged").unwrap();
        assert_eq!(conflict_of(&r, "a", &c), None);
        assert_eq!(body_of(&r, &view("a"), &c), "merged");
    }

    #[test]
    fn msw8_a_conflict_rekeyed_while_the_editor_is_open_is_stale_base() {
        let (_d, mut r, c, vc, _v0) = a_conflicted_after_b_lands();
        // the editor opens on a: it reads the conflict base and the conflict
        let read = conflict_of(&r, "a", &c).unwrap();
        // view c lands "charlie": a's conflict re-keys (same base, a new side)
        update(&mut r, "c", "agent:c", &c, "charlie");
        land(&mut r, "c").unwrap();
        let now = conflict_of(&r, "a", &c).unwrap();
        assert_ne!(now, read, "the conflict re-keyed");
        assert_eq!(version_of(&r, &view("a"), &c), vc, "same base");
        let e = save(&mut r, &vc, Some(&read), "merged").unwrap_err();
        assert_eq!(e.code, "E_STALE_BASE", "{e:?}");
        assert_eq!(conflict_of(&r, "a", &c).as_deref(), Some(now.as_str()));
        // a token that names the block as unconflicted is refused too
        let e = save(&mut r, &vc, None, "merged").unwrap_err();
        assert_eq!(e.code, "E_STALE_BASE", "{e:?}");
        // the current token resolves the conflict
        save(&mut r, &vc, Some(&now), "merged").unwrap();
        assert_eq!(conflict_of(&r, "a", &c), None);
        assert_eq!(body_of(&r, &view("a"), &c), "merged");
    }

    #[test]
    fn msw8_a_token_read_as_conflicted_on_a_resolved_block_is_stale_base() {
        let (_d, mut r, c, vc, _v0) = a_conflicted_after_b_lands();
        let read = conflict_of(&r, "a", &c).unwrap();
        // another session resolves a's conflict while the editor is open
        save(&mut r, &vc, Some(&read), "resolved").unwrap();
        let cur = version_of(&r, &view("a"), &c);
        let e = save(&mut r, &cur, Some(&read), "mine").unwrap_err();
        assert_eq!(e.code, "E_STALE_BASE", "{e:?}");
        assert_eq!(body_of(&r, &view("a"), &c), "resolved");
    }
}

#[test]
fn hd8_rdy_edit_refuses_a_misspelt_key_and_keeps_the_buffer() {
    let s = Served::new();
    let id = new_doc(&s);
    let ed = script(
        &s,
        "typo.sh",
        "sed -i.bak 's/title: Before/title: After/' \"$1\"\nprintf 'bdoy: x\\n' >> \"$1\"\n",
    );
    let out = edit(&s, &ed, &id, true);
    assert!(
        !out.status.success(),
        "a misspelt key is refused, not dropped"
    );
    let e: Value = serde_json::from_str(
        String::from_utf8_lossy(&out.stderr)
            .trim()
            .lines()
            .last()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(e["code"], "E_FILE_FORMAT", "{e}");
    assert!(
        e["message"]
            .as_str()
            .unwrap()
            .ends_with("unknown key \"bdoy\" in the block"),
        "{e}"
    );
    assert!(
        e["fix"]
            .as_str()
            .unwrap()
            .starts_with("did you mean \"body\"? rename it"),
        "{e}"
    );
    let saved = e["saved_edit"].as_str().expect("the buffer is kept");
    let kept = std::fs::read_to_string(saved).unwrap();
    assert!(
        kept.contains("bdoy: x") && kept.contains("title: After"),
        "{kept}"
    );
    std::fs::remove_file(saved).unwrap();
    let (ok, v) = s.json(&["--json", "show", &id, "--scope", "work"]);
    assert!(ok, "{v}");
    assert_eq!(v["fields"]["title"], "Before", "nothing written");
}
