//! vcreate1: create and fork, pinned bases, the floating rule, the claim and closed views.
use rdy_engine::*;
use rdy_qa::fixtures::*;
use rdy_qa::invariants::check;
use rdy_store::view::{Owner, ViewOpKind};
use serde_json::json;

fn claim(parent: &str, body: &str) -> serde_json::Value {
    json!([{"op": "create", "type": "claim", "parent": parent, "body": body}])
}
fn update(id: &str, base: &str, body: &str) -> serde_json::Value {
    json!([{"op": "update", "id": id, "base": base, "body": body}])
}
fn owner(s: &Session) -> Owner {
    Owner {
        actor: s.actor.clone(),
        run: s.run.clone(),
    }
}
fn view(v: &str) -> Scope {
    Scope::View(v.into())
}

#[test]
fn vcreate1_a_first_write_owns_the_view_and_a_commit_claims_an_unowned_one() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let a = as_agent("agent:a", "r1");
    write_as(&mut r, &a, "v", claim(&section, "a1")).unwrap();
    assert_eq!(
        r.read().view_head("v").unwrap().owner,
        Some(owner(&a)),
        "the first write owns the view"
    );
    r.store_mut()
        .view_append(
            "v",
            vec![ViewOpKind::Own { owner: None }],
            &human_writer().provenance(),
        )
        .unwrap();
    r.invalidate();
    let b = as_agent("agent:b", "r1");
    write_as(&mut r, &b, "v", claim(&section, "b1")).unwrap();
    let st = r.read();
    let h = st.view_head("v").unwrap().clone();
    assert_eq!(
        h.owner,
        Some(owner(&b)),
        "the first commit into an unowned view claims it"
    );
    let commit = st.reader().view_op(&h.head).unwrap();
    assert_eq!(
        st.reader().view_op(&commit.prev.unwrap()).unwrap().op,
        ViewOpKind::Own {
            owner: Some(owner(&b))
        },
        "the claim is an op in the same batch, before the commit"
    );
    drop(st);
    write_as(&mut r, &b, "v", claim(&section, "b2")).unwrap();
    let st = r.read();
    let h = st.view_head("v").unwrap().clone();
    let prev = st.reader().view_op(&h.head).unwrap().prev.unwrap();
    assert!(
        matches!(
            st.reader().view_op(&prev).unwrap().op,
            ViewOpKind::Commit { .. }
        ),
        "an owned view is not claimed again"
    );
    assert_eq!(st.included("v").len(), 3);
    drop(st);
    check(&r, &view("v")).unwrap();
}

#[test]
fn vcreate1_a_closed_view_refuses_commits_for_everyone() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let a = as_agent("agent:a", "r1");
    write_as(&mut r, &a, "v", claim(&section, "a1")).unwrap();
    r.store_mut()
        .view_append(
            "v",
            vec![ViewOpKind::Close {
                reason: "done".into(),
            }],
            &a.provenance(),
        )
        .unwrap();
    r.invalidate();
    let e = write_as(&mut r, &a, "v", claim(&section, "a2")).unwrap_err();
    assert_eq!(e.code, E_VIEW_CLOSED, "{e}");
    assert!(e.fix.contains("rdy undo"), "{e}");
    let e = write_as(&mut r, &human_writer(), "v", claim(&section, "h")).unwrap_err();
    assert_eq!(
        e.code, E_VIEW_CLOSED,
        "closed is closed for humans too: {e}"
    );
    assert_eq!(r.read().included("v").len(), 1, "nothing written");
}

#[test]
fn vcreate1_create_at_canon_tag_and_landing_sets_the_base_and_pin() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    let l1: rdy_model::Hash = land(&mut r, "seed").unwrap().parse().unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    write(&mut r, "seed", claim(&section, "two")).unwrap();
    let l2: rdy_model::Hash = land(&mut r, "seed").unwrap().parse().unwrap();
    let a = as_agent("agent:a", "r1");
    let x = r.view_create(&a, "x", None, false).unwrap();
    assert_eq!(
        (x.base.as_deref(), x.pinned, x.closed, x.changes),
        (Some(l2.to_string().as_str()), false, false, 0)
    );
    assert_eq!(
        x.owner,
        Some(OwnerInfo {
            actor: "agent:a".into(),
            run: Some("r1".into())
        })
    );
    let y = r.view_create(&a, "y", Some("tag:v1.0.0"), false).unwrap();
    assert_eq!(
        (y.base.as_deref(), y.pinned),
        (Some(l1.to_string().as_str()), true),
        "a tag base pins"
    );
    let z = r
        .view_create(&a, "z", Some(&format!("landing:{}", l1.short())), false)
        .unwrap();
    assert_eq!(
        (z.base.as_deref(), z.pinned),
        (Some(l1.to_string().as_str()), true),
        "a landing base pins"
    );
    let p = r.view_create(&a, "p", Some("canon"), true).unwrap();
    assert_eq!(
        (p.base.as_deref(), p.pinned),
        (Some(l2.to_string().as_str()), true),
        "--pin pins at canon"
    );
    let e = r.view_create(&a, "x", None, false).unwrap_err();
    assert_eq!(e.code, E_VIEW_EXISTS, "{e}");
    let e = r.view_create(&a, "q", Some("seed"), false).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert!(e.fix.contains("fork"), "{e}");
    let e = r.view_create(&a, "canon", None, false).unwrap_err();
    assert_eq!(e.code, E_RESERVED_VIEW, "{e}");
    let e = r.view_create(&reader(), "r", None, false).unwrap_err();
    assert_eq!(e.code, E_PERMISSION, "{e}");
    let st = r.read();
    assert_eq!(st.view_names(), vec!["p", "seed", "x", "y", "z"]);
    assert_eq!(
        st.materialized(&view("y")).unwrap().len(),
        3,
        "y reads canon as of landing 1"
    );
    assert_eq!(
        st.materialized(&view("x")).unwrap().len(),
        4,
        "x floats on canon"
    );
    assert_eq!(st.effective_base("x"), Some(l2));
    assert_eq!(st.effective_base("y"), Some(l1));
    assert_eq!(st.effective_base("never-written"), Some(l2));
    assert!(
        std::sync::Arc::ptr_eq(
            &st.materialized(&view("x")).unwrap(),
            &st.materialized(&Scope::Canon).unwrap()
        ),
        "an empty unpinned view shares canon's build"
    );
}

/// Review Focus 5 at the engine.
#[test]
fn vcreate1_an_empty_unpinned_view_floats_to_the_next_canon_but_a_pinned_one_stays() {
    let (_d, mut r) = project();
    let (_doc, section, c) = seed_doc(&mut r, "seed");
    let l1: rdy_model::Hash = land(&mut r, "seed").unwrap().parse().unwrap();
    let a = as_agent("agent:a", "r1");
    r.view_create(&a, "float", None, false).unwrap();
    r.view_create(&a, "pin", None, true).unwrap();
    let v1 = version_of(&r, &Scope::Canon, &c);
    write(&mut r, "seed", claim(&section, "two")).unwrap();
    write(&mut r, "seed", update(&c, &v1, "one, revised")).unwrap();
    let l2: rdy_model::Hash = land(&mut r, "seed").unwrap().parse().unwrap();
    let st = r.read();
    assert_eq!(st.materialized(&view("float")).unwrap().len(), 4);
    assert_eq!(st.materialized(&view("pin")).unwrap().len(), 3);
    assert_eq!(st.effective_base("float"), Some(l2));
    assert_eq!(
        st.view_head("float").unwrap().base,
        Some(l1),
        "no rebase op was written for an empty view"
    );
    // adapted: VersionPosition has no `base` field; its version and landing count are what the base decides
    let (fp, cp) = (
        st.version_position(&view("float")).unwrap(),
        st.version_position(&Scope::Canon).unwrap(),
    );
    assert_eq!(
        (fp.version, fp.landings_since),
        (cp.version, cp.landings_since),
        "a floating view stands where canon stands"
    );
    drop(st);
    // the first write into the floating view is based on canon's version and moves its base
    let v2 = version_of(&r, &Scope::Canon, &c);
    write_as(&mut r, &a, "float", update(&c, &v2, "edited in float")).unwrap();
    let st = r.read();
    assert_eq!(
        st.view_head("float").unwrap().base,
        Some(l2),
        "the float catch-up moved the base"
    );
    assert!(
        st.conflicts(&view("float")).unwrap().is_empty(),
        "no phantom conflict against landing 1"
    );
    drop(st);
    assert_eq!(body_of(&r, &view("float"), &c), "edited in float");
    assert_eq!(
        body_of(&r, &view("pin"), &c),
        "one",
        "the pinned view still reads landing 1"
    );
    check(&r, &view("float")).unwrap();
}

#[test]
fn vcreate1_a_fork_copies_base_pin_and_set_and_is_owned_by_the_forker() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    let l1: rdy_model::Hash = land(&mut r, "seed").unwrap().parse().unwrap();
    let a = as_agent("agent:a", "r1");
    r.view_create(&a, "a", Some(&format!("landing:{l1}")), false)
        .unwrap();
    write_as(&mut r, &a, "a", claim(&section, "a1")).unwrap();
    write_as(&mut r, &a, "a", claim(&section, "a2")).unwrap();
    let b = as_agent("agent:b", "r9");
    let f = r.view_fork(&b, "a", "b").unwrap();
    assert_eq!(
        (f.view.as_str(), f.changes, f.pinned, f.base.as_deref()),
        ("b", 2, true, Some(l1.to_string().as_str()))
    );
    assert_eq!(
        f.owner,
        Some(OwnerInfo {
            actor: "agent:b".into(),
            run: Some("r9".into())
        })
    );
    let st = r.read();
    assert_eq!(st.included("b"), st.included("a"));
    assert_eq!(
        st.materialized(&view("b")).unwrap().len(),
        st.materialized(&view("a")).unwrap().len()
    );
    drop(st);
    write_as(&mut r, &b, "b", claim(&section, "b1")).unwrap();
    assert_eq!(
        (r.read().included("a").len(), r.read().included("b").len()),
        (2, 3),
        "the fork is independent"
    );
    let e = r.view_fork(&b, "nope", "c").unwrap_err();
    assert_eq!(e.code, E_NOT_FOUND, "{e}");
    let e = r.view_fork(&b, "a", "b").unwrap_err();
    assert_eq!(e.code, E_VIEW_EXISTS, "{e}");
    check(&r, &view("b")).unwrap();
}

#[test]
fn vcreate1_a_fork_of_a_closed_view_is_open_and_owned_by_the_forker() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let a = as_agent("agent:a", "r1");
    write_as(&mut r, &a, "a", claim(&section, "a1")).unwrap();
    r.store_mut()
        .view_append(
            "a",
            vec![ViewOpKind::Close {
                reason: "abandoned".into(),
            }],
            &a.provenance(),
        )
        .unwrap();
    r.invalidate();
    write(&mut r, "seed", claim(&section, "two")).unwrap();
    let l2 = land(&mut r, "seed").unwrap();
    assert_ne!(
        r.read().view_head("a").unwrap().base.map(|h| h.to_string()),
        Some(l2.clone()),
        "a closed view is not rebased"
    );
    let b = as_agent("agent:b", "r1");
    let f = r.view_fork(&b, "a", "b").unwrap();
    assert!(!f.closed && f.changes == 1, "{f:?}");
    assert_eq!(
        f.base.as_deref(),
        Some(l2.as_str()),
        "a live unpinned fork never starts behind canon"
    );
    assert_eq!(f.owner.as_ref().unwrap().actor, "agent:b");
    write_as(&mut r, &b, "b", claim(&section, "b1")).unwrap();
    assert!(
        r.read().view_head("a").unwrap().closed,
        "the source stays closed"
    );
}

#[test]
fn hb1_require_up_to_date_is_read_from_the_project_config_on_a_full_publish() {
    let (d, mut r) = project();
    assert!(!r.read().require_up_to_date(), "off by default");
    let cfg = d.path().join(".rdy.yaml");
    let text = std::fs::read_to_string(&cfg).unwrap();
    std::fs::write(
        &cfg,
        format!("{text}reviews:\n  require_up_to_date: true\n"),
    )
    .unwrap();
    assert!(
        !r.read().require_up_to_date(),
        "a published state never changes"
    );
    // the config is loaded at open and by a pack reload; a full publish keeps it (msw19)
    assert!(matches!(r.reload_pack().unwrap(), ReloadOutcome::Reloaded));
    assert!(r.read().require_up_to_date());
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    write(
        &mut r,
        "seed",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "x"}]),
    )
    .unwrap();
    assert!(
        r.read().require_up_to_date(),
        "carried over by a commit's publish"
    );
}

#[test]
fn msw19_a_full_publish_over_a_broken_config_keeps_the_review_gate_on() {
    let (d, mut r) = project();
    let cfg = d.path().join(".rdy.yaml");
    let text = std::fs::read_to_string(&cfg).unwrap();
    std::fs::write(
        &cfg,
        format!("{text}reviews:\n  require_up_to_date: true\n"),
    )
    .unwrap();
    assert!(matches!(r.reload_pack().unwrap(), ReloadOutcome::Reloaded));
    assert!(r.read().require_up_to_date());
    for broken in ["reviews:\n  require_up_to_date: \"true\"\n", "reviews: [\n"] {
        std::fs::write(&cfg, format!("{text}{broken}")).unwrap();
        assert!(
            matches!(r.reload_pack().unwrap(), ReloadOutcome::Invalid { .. }),
            "{broken:?}"
        );
        r.invalidate();
        assert!(
            r.read().require_up_to_date(),
            "{broken:?} must not turn the review gate off"
        );
    }
}

/// The one warning `Rdy::open` gives for the project config `text` appended with `bad`; the
/// project opens and the review gate stays off (founder ruling: config typos warn).
fn opens_with_one_warning(bad: &str) -> rdy_schema::Issue {
    let (d, r) = project();
    drop(r);
    let cfg = d.path().join(".rdy.yaml");
    let text = std::fs::read_to_string(&cfg).unwrap();
    std::fs::write(&cfg, format!("{text}{bad}")).unwrap();
    let r = Rdy::open(d.path()).unwrap_or_else(|e| panic!("{bad:?} must open: {e}"));
    assert!(!r.read().require_up_to_date(), "{bad:?}");
    let w = rdy_engine::config_levels::config_warnings(d.path(), None);
    assert_eq!(w.len(), 1, "{bad:?}: {w:?}");
    assert_eq!(w[0].code, "W_CONFIG_UNKNOWN", "{bad:?}");
    w[0].clone()
}

#[test]
fn msw19_an_unknown_key_under_reviews_opens_with_the_warning() {
    for bad in [
        "reviews:\n  require_uptodate: true\n",
        "review:\n  require_up_to_date: true\n",
    ] {
        let w = opens_with_one_warning(bad);
        assert!(
            format!("{} {}", w.message, w.fix).contains("reviews.require_up_to_date"),
            "{bad:?}: {w:?}"
        );
    }
}

#[test]
fn msw19_a_reload_whose_publish_panics_keeps_the_previous_pack_and_config() {
    let (d, mut r) = project();
    let old_pack = r.pack().name.clone();
    let old_fp = r.pack_fingerprint().clone();
    std::fs::write(
        d.path().join(".rdy/packs/soft.yaml"),
        "name: soft\nversion: \"1\"\nextends: rdy-standard\n",
    )
    .unwrap();
    std::fs::write(
        d.path().join(".rdy.yaml"),
        "pack: soft\nreviews:\n  require_up_to_date: true\n",
    )
    .unwrap();
    r.panic_full_publishes_for_tests(1);
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| r.reload_pack()));
    assert!(panicked.is_err(), "the publish's panic propagates");
    assert_eq!(r.pack().name, old_pack, "the previous pack stays loaded");
    assert_eq!(r.pack_fingerprint(), &old_fp, "the fingerprint stays");
    r.invalidate();
    assert_eq!(r.read().pack().name, old_pack);
    assert!(
        !r.read().require_up_to_date(),
        "the previous config stays loaded"
    );
    // the files did not change since: the next reload loads them
    assert!(matches!(r.reload_pack().unwrap(), ReloadOutcome::Reloaded));
    assert_eq!(r.pack().name, "soft");
    assert!(r.read().require_up_to_date());
}

#[test]
fn msw19_a_dotted_review_key_opens_with_the_warning() {
    for bad in [
        "reviews.require_up_to_date: true\n",
        "\"reviews.require_up_to_date\": true\n",
        "'reviews.require_up_to_date': true\n",
    ] {
        let w = opens_with_one_warning(bad);
        assert_eq!(
            w.message,
            ".rdy.yaml: `reviews.require_up_to_date` is written as one dotted key; \
             rdy reads only nested keys, so the review setting would be ignored",
            "{bad:?}"
        );
        assert_eq!(
            w.fix, "write it nested: `reviews:` then `  require_up_to_date: true` on the next line",
            "{bad:?}"
        );
    }
}

#[test]
fn msw19_a_review_like_key_outside_reviews_opens_with_a_plain_warning() {
    for path in ["require_up_to_date", "require_uptodate"] {
        let w = opens_with_one_warning(&format!("{path}: true\n"));
        assert_eq!(
            w.message,
            format!("unknown key {path} in .rdy.yaml (project level); rdy ignores it"),
            "{path}"
        );
        assert_eq!(
            w.fix,
            format!("remove {path} from .rdy.yaml, or check its spelling"),
            "{path}"
        );
    }
    // `views.require_up_to_date` is 2 edits from `reviews.require_up_to_date`: the closed
    // did-you-mean rule names it, so the warning keeps the review text
    let w = opens_with_one_warning("views:\n  require_up_to_date: true\n");
    assert_eq!(
        w.message,
        "unknown review setting views.require_up_to_date in .rdy.yaml; the review gate would stay off"
    );
    assert_eq!(
        w.fix,
        "did you mean reviews.require_up_to_date? rename views.require_up_to_date in .rdy.yaml"
    );
}

#[test]
fn hd9_a_review_setting_in_local_yaml_opens_and_the_gate_stays_off() {
    let (d, r) = project();
    drop(r);
    std::fs::write(
        d.path().join(".rdy/local.yaml"),
        "reviews:\n  require_up_to_date: true\n",
    )
    .unwrap();
    let r = Rdy::open(d.path()).unwrap_or_else(|e| panic!("must open: {e}"));
    assert!(
        !r.read().require_up_to_date(),
        "the local value is not applied"
    );
    let w = rdy_engine::config_levels::config_warnings(d.path(), None);
    assert_eq!(w.len(), 1, "{w:?}");
    assert_eq!(w[0].code, "W_CONFIG_UNKNOWN");
}
