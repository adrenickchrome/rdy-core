//! kasg1: `rdy keys assign` through the daemon: text, JSON, and nothing left to assign.
use rdy_engine::Rdy;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;

#[test]
fn kasg1_rdy_keys_assign_prints_what_it_assigned() {
    let s = Served::new();
    {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        raw_commit(
            &mut r,
            "work",
            vec![
                raw(
                    "fd-AAAAA",
                    "folder",
                    None,
                    "a1",
                    &[("name", "guides"), ("title", "Guides")],
                    "",
                ),
                raw(
                    "d-BBBBB",
                    "doc",
                    Some("fd-AAAAA"),
                    "a1",
                    &[
                        ("name", "intro"),
                        ("title", "Intro"),
                        ("doc_type", "overview"),
                    ],
                    "",
                ),
            ],
        );
        r.close().unwrap();
    }
    let out = s.rdy(&["keys", "assign"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim_end(),
        "assigned 2 keys in view work\n  d-BBBBB  guides.intro\n  fd-AAAAA  guides"
    );
    let (ok, v) = s.json(&["--json", "keys", "assign"]);
    assert!(!ok);
    assert_eq!(v["code"], "E_BAD_INPUT", "{v}");
    s.stop();
}
