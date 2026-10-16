//! pshow1: `rdy show P-x@n` prints a proposal version; `--json` gives {proposal}.
use rdy_engine::{ProposalInfo, Rdy};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use serde_json::json;

fn claim(parent: &str, body: &str) -> serde_json::Value {
    json!([{"op": "create", "type": "claim", "parent": parent, "body": body}])
}

fn two_versions(s: &Served) -> (ProposalInfo, ProposalInfo) {
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    write(&mut r, "work", claim(&section, "a")).unwrap();
    let p1 = r.propose(&writer(), "work", "first", None).unwrap();
    write(&mut r, "work", claim(&section, "b")).unwrap();
    let p2 = r.propose(&writer(), "work", "second", None).unwrap();
    r.close().unwrap();
    (p1, p2)
}

#[test]
fn pshow1_rdy_show_prints_a_proposal_version_and_json_has_its_state() {
    let s = Served::new();
    let (p1, p2) = two_versions(&s);
    let out = s.rdy(&["show", &p2.r#ref]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.starts_with(&format!(
            "proposal {} (version {}): second\n",
            p2.r#ref,
            &p2.id[..12]
        )),
        "{text}"
    );
    assert!(text.contains("state: open\n"), "{text}");
    assert!(text.contains("changes: 2 (2 blocks)\n"), "{text}");
    assert!(
        text.contains(&format!("versions: {}, {} (latest)\n", p1.r#ref, p2.r#ref)),
        "{text}"
    );
    assert!(
        text.ends_with(&format!("approve with: rdy approve {}\n", p2.r#ref)),
        "{text}"
    );
    let old = s.rdy(&["show", &p1.r#ref]);
    let old_text = String::from_utf8_lossy(&old.stdout).into_owned();
    assert!(old_text.contains("state: superseded\n"), "{old_text}");
    assert!(
        old_text.ends_with(&format!("blocking: superseded by {}\n", p2.r#ref)),
        "{old_text}"
    );
    assert!(!old_text.contains("approve with:"), "{old_text}");
    let (ok, v) = s.json(&["--json", "show", &p1.r#ref]);
    assert!(ok, "{v}");
    assert_eq!(v["proposal"]["ref"], p1.r#ref.as_str());
    assert_eq!(v["proposal"]["state"], "superseded");
    assert_eq!(v["proposal"]["versions"][1]["latest"], true);
    let (ok, v) = s.json(&["--json", "show", "P-0000@1"]);
    assert!(!ok);
    assert_eq!(v["code"], "E_NOT_FOUND", "{v}");
    let (ok, v) = s.json(&["--json", "show", "--blame", &p2.r#ref]);
    assert!(!ok, "{v}");
    assert_eq!(v["code"], "E_BAD_INPUT", "{v}");
    assert!(
        v["message"]
            .as_str()
            .unwrap()
            .contains("blame applies to blocks, not proposals"),
        "{v}"
    );
}

/// D-K0SS1: `rdy show` reads a withdrawn version, and `rdy approve` of it is `E_VIEW_CLOSED`.
#[test]
fn vw2_rdy_show_and_rdy_approve_of_a_withdrawn_version() {
    let s = Served::new();
    let p = {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        let (_doc, section, _c) = seed_doc(&mut r, "seed");
        land(&mut r, "seed").unwrap();
        write(&mut r, "v", claim(&section, "a")).unwrap();
        let p = r.propose(&writer(), "v", "first", None).unwrap();
        r.view_close(&writer(), &["v".into()], "done").unwrap();
        r.close().unwrap();
        p
    };
    let out = s.rdy(&["show", &p.r#ref]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("state: withdrawn (view v closed)\n"),
        "{text}"
    );
    assert!(
        text.ends_with("blocking: withdrawn (view v closed)\n"),
        "{text}"
    );
    let (ok, v) = s.json(&["--json", "show", &p.r#ref]);
    assert!(ok, "{v}");
    assert_eq!(v["proposal"]["state"], "withdrawn", "{v}");
    let (ok, v) = s.json(&["--json", "approve", &p.r#ref]);
    assert!(!ok, "{v}");
    assert_eq!(v["code"], "E_VIEW_CLOSED", "{v}");
}
