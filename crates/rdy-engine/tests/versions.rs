use rdy_engine::*;
use rdy_model::*;
use serde_json::json;

fn session(role: Role) -> Session {
    Session {
        actor: if role == Role::Approver {
            "human:founder".into()
        } else {
            "agent:a".into()
        },
        run: None,
        role,
    }
}
fn writer() -> Session {
    session(Role::Writer)
}
fn founder() -> Session {
    session(Role::Approver)
}

/// Writes one doc to view `work`, proposes and lands it; returns the landing id.
fn land_one(r: &mut Rdy, title: &str) -> String {
    r.write(
        &writer(),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(json!([
                {"op": "create", "type": "doc", "fields": {"title": title, "doc_type": "glossary"}}
            ]))
            .unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap();
    let p = r.propose(&writer(), "work", title).unwrap();
    r.land(&founder(), &p.id).unwrap()
}

/// A project with `n` landings; returns their ids oldest first.
fn project(n: usize) -> (tempfile::TempDir, Rdy, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let landings = (0..n)
        .map(|i| land_one(&mut r, &format!("Doc {i}")))
        .collect();
    (dir, r, landings)
}

#[test]
fn version_names_must_be_semver_with_an_optional_v() {
    let (_d, mut r, _) = project(1);
    for good in ["v0.1.0", "1.2.0", "v2.0.0-rc.1"] {
        assert_eq!(r.tag(&founder(), good, None, None).unwrap().name, good);
    }
    for bad in [
        "release-1",
        "v1.2",
        "1.2.0.0",
        "vv3.0.0",
        "V3.0.0",
        " v3.0.0",
        "v3.0.0 ",
        "03.0.0",
        "",
    ] {
        assert_eq!(
            r.tag(&founder(), bad, None, None).unwrap_err().code,
            "E_BAD_TAG_NAME",
            "{bad:?}"
        );
    }
    let e = r.tag(&founder(), "3.0.0+build.5", None, None).unwrap_err();
    assert_eq!(e.code, "E_BAD_TAG_NAME");
    assert!(e.message.contains("build metadata"), "{e}");
}

#[test]
fn a_version_cannot_exist_twice_under_another_spelling() {
    let (_d, mut r, _) = project(1);
    r.tag(&founder(), "v1.0.0", None, None).unwrap();
    let e = r.tag(&founder(), "1.0.0", None, None).unwrap_err();
    assert_eq!(e.code, "E_TAG_EXISTS");
    assert!(e.message.contains("v1.0.0"), "{e}");
}

#[test]
fn versions_only_go_up_along_one_line() {
    let (_d, mut r, _) = project(1);
    r.tag(&founder(), "v2.0.0-rc.1", None, None).unwrap();
    let e = r.tag(&founder(), "v1.9.0", None, None).unwrap_err();
    assert_eq!(e.code, "E_VERSION_ORDER");
    assert!(
        e.message.contains("v2.0.0-rc.1"),
        "names the current version: {e}"
    );
    r.tag(&founder(), "v2.0.0-rc.2", None, None).unwrap();
    r.tag(&founder(), "v2.0.0", None, None).unwrap();
}

#[test]
fn a_version_cannot_name_an_earlier_landing_than_the_current_one() {
    let (_d, mut r, l) = project(3);
    r.tag(&founder(), "v1.0.0", None, None).unwrap(); // head = l[2]
    let e = r.tag(&founder(), "v1.1.0", Some(&l[1]), None).unwrap_err();
    assert_eq!(e.code, "E_VERSION_ORDER");
    assert!(
        e.message.contains(&l[2][..12]),
        "names the current version's landing: {e}"
    );
    // the same landing is fine
    assert_eq!(
        r.tag(&founder(), "v1.1.0", Some(&l[2]), None)
            .unwrap()
            .landing,
        l[2]
    );
}

#[test]
fn landings_resolve_by_full_hash_or_a_unique_prefix() {
    let (_d, mut r, l) = project(3);
    let first: Hash = l[0].parse().unwrap();
    assert_eq!(r.read().resolve_landing(&l[0]).unwrap(), first);
    assert_eq!(
        r.read().resolve_landing(&l[0][..12]).unwrap(),
        first,
        "rdy log shows 12 characters"
    );
    assert_eq!(
        r.read()
            .resolve_landing(&l[0][..12].to_uppercase())
            .unwrap(),
        first
    );
    assert_eq!(
        r.read().resolve_landing(&l[0][..5]).unwrap_err().code,
        "E_NOT_FOUND",
        "fewer than 6"
    );
    assert_eq!(
        r.read().resolve_landing("ffffffffffff").unwrap_err().code,
        "E_NOT_FOUND"
    );
    assert_eq!(
        r.read().resolve_landing("not-hex").unwrap_err().code,
        "E_NOT_FOUND"
    );
    let tagged = r
        .tag(&founder(), "v0.1.0", Some(&l[0][..8]), Some("first".into()))
        .unwrap();
    assert_eq!(tagged.landing, l[0]);
    assert_eq!(tagged.message.as_deref(), Some("first"));
    assert_eq!(tagged.tagged_by, "human:founder");
}

#[test]
fn tagging_needs_a_landing_and_an_approver() {
    let (_d, mut r, _) = project(0);
    let e = r.tag(&founder(), "v0.1.0", None, None).unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND");
    assert!(e.fix.contains("land a proposal first"), "{e}");
    land_one(&mut r, "Doc");
    for role in [Role::Reader, Role::Writer, Role::Reviewer] {
        assert_eq!(
            r.tag(&session(role), "v0.1.0", None, None)
                .unwrap_err()
                .code,
            "E_PERMISSION"
        );
    }
    r.tag(&founder(), "v0.1.0", None, None).unwrap();
}

#[test]
fn writes_proposals_and_landings_never_create_versions() {
    let (_d, r, _) = project(3);
    assert!(r.read().versions().unwrap().is_empty());
    assert!(r.store().tags().unwrap().is_empty());
}

#[test]
fn versions_are_listed_oldest_first() {
    let (_d, mut r, l) = project(2);
    r.tag(&founder(), "v0.1.0", Some(&l[0]), None).unwrap();
    r.tag(&founder(), "v0.2.0", Some(&l[1]), None).unwrap();
    r.tag(&founder(), "v0.10.0", None, None).unwrap();
    let names: Vec<String> = r
        .read()
        .versions()
        .unwrap()
        .into_iter()
        .map(|v| v.name)
        .collect();
    assert_eq!(
        names,
        ["v0.1.0", "v0.2.0", "v0.10.0"],
        "SemVer precedence, not string order"
    );
}

fn label(r: &Rdy, scope: &Scope) -> String {
    r.read().version_position(scope).unwrap().label
}

#[test]
fn position_without_versions() {
    let (_d, r, _) = project(2);
    let p = r.read().version_position(&Scope::Canon).unwrap();
    assert_eq!(p.version, None);
    assert_eq!(p.landings_since, 2);
    assert_eq!(p.label, "no version yet");
    let (_d, r, _) = project(0);
    assert_eq!(label(&r, &Scope::Canon), "no version yet");
}

#[test]
fn position_at_and_after_a_version() {
    let (_d, mut r, l) = project(1);
    r.tag(&founder(), "v1.0.0", None, None).unwrap();
    let p = r.read().version_position(&Scope::Canon).unwrap();
    assert_eq!(
        p.version,
        Some(VersionRef {
            name: "v1.0.0".into(),
            landing: l[0].clone()
        })
    );
    assert_eq!((p.landings_since, p.frozen), (0, false));
    assert_eq!(p.label, "v1.0.0");
    land_one(&mut r, "Next");
    assert_eq!(label(&r, &Scope::Canon), "v1.0.0 + 1 landing");
    land_one(&mut r, "Next 2");
    land_one(&mut r, "Next 3");
    assert_eq!(label(&r, &Scope::Canon), "v1.0.0 + 3 landings");
    // two versions on one landing: the higher one is shown
    r.tag(&founder(), "v1.1.0", None, None).unwrap();
    r.tag(&founder(), "v1.1.1", None, None).unwrap();
    assert_eq!(label(&r, &Scope::Canon), "v1.1.1");
}

#[test]
fn position_of_a_view_with_unlanded_changes() {
    let (_d, mut r, _) = project(1);
    r.tag(&founder(), "v1.0.0", None, None).unwrap();
    for _ in 0..3 {
        land_one(&mut r, "More");
    }
    for t in ["One", "Two"] {
        r.write(
            &writer(),
            WriteRequest {
                view: "run-1".into(),
                ops: serde_json::from_value(json!([
                    {"op": "create", "type": "doc", "fields": {"title": t, "doc_type": "glossary"}}
                ]))
                .unwrap(),
                justification: None,
                message: String::new(),
            },
        )
        .unwrap();
    }
    let p = r
        .read()
        .version_position(&Scope::View("run-1".into()))
        .unwrap();
    assert_eq!(
        p.view,
        Some(ViewPosition {
            name: "run-1".into(),
            unlanded_changes: 2
        })
    );
    assert_eq!(
        p.label,
        "v1.0.0 + 3 landings · view run-1, 2 unlanded changes"
    );
}

#[test]
fn a_view_behind_canon_counts_to_its_own_base() {
    let (_d, mut r, _) = project(1);
    r.tag(&founder(), "v1.0.0", None, None).unwrap();
    r.write(
        &writer(),
        WriteRequest {
            view: "slow".into(),
            ops: serde_json::from_value(json!([
                {"op": "create", "type": "doc", "fields": {"title": "Slow", "doc_type": "glossary"}}
            ]))
            .unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap();
    land_one(&mut r, "Elsewhere"); // canon moves on; view `slow` still rests on v1.0.0
    assert_eq!(label(&r, &Scope::Canon), "v1.0.0 + 1 landing");
    assert_eq!(
        label(&r, &Scope::View("slow".into())),
        "v1.0.0 · view slow, 1 unlanded change"
    );
}

#[test]
fn position_of_an_as_of_scope_is_frozen() {
    let (_d, mut r, l) = project(1);
    r.tag(&founder(), "v1.0.0", None, None).unwrap();
    let l1 = land_one(&mut r, "B");
    let l2 = land_one(&mut r, "C");
    r.tag(&founder(), "v2.0.0", None, None).unwrap();
    let at = r.read().resolve_scope("tag:v1.0.0").unwrap();
    assert_eq!(at, Scope::At(l[0].parse().unwrap()));
    let p = r.read().version_position(&at).unwrap();
    assert!(p.frozen);
    assert_eq!(p.label, "v1.0.0 · frozen snapshot");
    assert_eq!(
        label(&r, &Scope::At(l1.parse().unwrap())),
        "v1.0.0 + 1 landing · frozen snapshot"
    );
    assert_eq!(
        label(&r, &Scope::At(l2.parse().unwrap())),
        "v2.0.0 · frozen snapshot"
    );
}

#[test]
fn a_damaged_tag_ref_does_not_break_version_reads() {
    let (_d, mut r, l) = project(1);
    r.tag(&founder(), "v0.1.0", Some(&l[0]), None).unwrap();
    r.store_mut()
        .set_refs(vec![("tag/v9.0.0".into(), b"garbage".to_vec())])
        .unwrap();
    // a write outside the engine must be followed by invalidate() (snap1 precondition)
    r.invalidate();
    let names: Vec<String> = r
        .read()
        .versions()
        .unwrap()
        .into_iter()
        .map(|v| v.name)
        .collect();
    assert_eq!(names, ["v0.1.0"]);
    assert_eq!(label(&r, &Scope::Canon), "v0.1.0");
    assert!(matches!(
        r.read().resolve_scope("tag:v0.1.0").unwrap(),
        Scope::At(_)
    ));
    let e = r.fsck().unwrap_err();
    assert!(e.message.contains("tag/v9.0.0"), "{e}");
}

#[test]
fn tagging_an_explicit_landing_on_an_empty_canon_says_to_land_first() {
    let (_d, mut r, _) = project(0);
    let e = r
        .tag(&founder(), "v0.1.0", Some("abcdef"), None)
        .unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND");
    assert!(e.fix.contains("land a proposal first"), "{e}");
}
