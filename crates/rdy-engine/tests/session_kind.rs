//! sess2: who is human, what an agent may write into, and where a session writes by default.
use rdy_engine::intent::IntentFilter;
use rdy_engine::{
    E_BAD_INPUT, E_CONFIG, E_PERMISSION, E_VIEW_OWNER, Rdy, Session, SessionKind, WriteRequest,
    classify, human_actor, human_actor_for,
};
use rdy_model::Role;

fn project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    (dir, r)
}

fn doc(
    r: &mut Rdy,
    s: &Session,
    view: &str,
    name: &str,
) -> Result<rdy_engine::WriteResult, rdy_engine::RdyError> {
    let ops = serde_json::from_value(serde_json::json!([
        {"op": "create", "type": "doc", "fields": {"name": name, "title": "D", "doc_type": "page"}}
    ]))
    .unwrap();
    r.write(
        s,
        WriteRequest {
            view: view.into(),
            ops,
            justification: None,
            message: String::new(),
        },
    )
}

#[test]
fn sess2_human_actor_comes_from_xdg_then_home_then_user() {
    let xdg = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    assert_eq!(human_actor(None, None, Some("nick")).unwrap(), "human:nick");
    assert_eq!(human_actor(None, None, None).unwrap(), "human:unknown");
    assert_eq!(
        human_actor(Some(xdg.path()), Some(home.path()), Some("nick")).unwrap(),
        "human:nick",
        "no file yet"
    );
    std::fs::create_dir_all(home.path().join(".config/rdy")).unwrap();
    std::fs::write(
        home.path().join(".config/rdy/config.yaml"),
        "actor: human:founder\n",
    )
    .unwrap();
    assert_eq!(
        human_actor(None, Some(home.path()), Some("nick")).unwrap(),
        "human:founder"
    );
    std::fs::create_dir_all(xdg.path().join("rdy")).unwrap();
    std::fs::write(xdg.path().join("rdy/config.yaml"), "actor: human:x\n").unwrap();
    assert_eq!(
        human_actor(Some(xdg.path()), Some(home.path()), Some("nick")).unwrap(),
        "human:x",
        "xdg wins"
    );
    // id3 (D-Q8NFG): a file that does not parse is its error, never the fallback
    std::fs::write(xdg.path().join("rdy/config.yaml"), "actor: [\n").unwrap();
    let e = human_actor(Some(xdg.path()), None, Some("nick")).unwrap_err();
    assert_eq!(
        e.code, E_CONFIG,
        "unreadable: its error, not the fallback: {e}"
    );
    // an empty actor is present and invalid
    std::fs::write(xdg.path().join("rdy/config.yaml"), "actor: ''\n").unwrap();
    let e = human_actor(Some(xdg.path()), None, Some("nick")).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "empty: refused, not the fallback: {e}");
    assert_eq!(e.message, "the actor \"\" is not in rdy's actor form");
}

#[test]
fn sess2_classify_makes_the_human_or_an_agent_and_agents_never_approve() {
    let h = classify("human:n", None, Some("ignored"), Role::Writer).unwrap();
    assert_eq!(
        (h.kind, h.run.as_deref(), h.actor.as_str()),
        (SessionKind::Human, Some("cli"), "human:n")
    );
    let h2 = classify("human:n", Some("human:n"), None, Role::Approver).unwrap();
    assert_eq!((h2.kind, h2.role), (SessionKind::Human, Role::Approver));
    let a = classify("human:n", Some("agent:w"), Some("r1"), Role::Writer).unwrap();
    assert_eq!(
        (a.kind, a.run.as_deref(), a.actor.as_str()),
        (SessionKind::Agent, Some("r1"), "agent:w")
    );
    let e = classify("human:n", Some("agent:w"), None, Role::Approver).unwrap_err();
    assert_eq!(e.code, E_PERMISSION);
    assert!(e.fix.contains("RDY_ACTOR"), "{e}");
    // id3 (D-ME2C0): a human actor that is not the configured human is an agent claiming a
    // human, and refused
    let o = classify("human:n", Some("human:other"), None, Role::Writer).unwrap_err();
    assert_eq!(o.code, E_PERMISSION, "{o}");
    assert_eq!(o.message, "an agent session cannot claim a human actor");
    assert_eq!(
        o.fix,
        "unset RDY_ACTOR to act as the person, or relay their words"
    );
}

#[test]
fn sess2_default_views_follow_the_session_kind() {
    assert_eq!(
        Session::human("human:n".into(), Role::Writer)
            .unwrap()
            .default_view(),
        "work"
    );
    // id3 (D-ME2C0): a run holds no ':', so `run:1` is refused before any view is made
    let e = Session::agent("agent:w".into(), Some("run:1".into()), Role::Writer).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert_eq!(e.message, "the run \"run:1\" is not in rdy's run form");
    assert_eq!(
        Session::agent("agent:w".into(), Some("run-1".into()), Role::Writer)
            .unwrap()
            .default_view(),
        "run-1"
    );
    assert_eq!(
        Session::agent("agent:w".into(), None, Role::Writer)
            .unwrap()
            .default_view(),
        "work-agent-w"
    );
}

#[test]
fn sess2_a_first_commit_records_the_sessions_actor_and_run_as_the_views_owner() {
    let (_d, mut r) = project();
    let h = Session::human("human:n".into(), Role::Writer).unwrap();
    doc(&mut r, &h, "work", "a").unwrap();
    let head = r.read().view_head("work").cloned().unwrap();
    let o = head.owner.expect("owned");
    assert_eq!(
        (o.actor.as_str(), o.run.as_deref()),
        ("human:n", Some("cli"))
    );
    let a = Session::agent("agent:w".into(), Some("r1".into()), Role::Writer).unwrap();
    doc(&mut r, &a, "r1", "b").unwrap();
    let o = r.read().view_head("r1").cloned().unwrap().owner.unwrap();
    assert_eq!(
        (o.actor.as_str(), o.run.as_deref()),
        ("agent:w", Some("r1"))
    );
}

#[test]
fn sess2_an_agent_cannot_commit_into_a_view_another_session_owns() {
    let (_d, mut r) = project();
    let h = Session::human("human:n".into(), Role::Writer).unwrap();
    doc(&mut r, &h, "work", "a").unwrap();
    let a = Session::agent("agent:w".into(), None, Role::Writer).unwrap();
    let e = doc(&mut r, &a, "work", "b").unwrap_err();
    assert_eq!(e.code, E_VIEW_OWNER);
    // own1 (M2c Task 15): `work` is the human's default view, refused as such for any agent.
    assert_eq!(e.message, "view \"work\" is the human's default view");
    assert!(e.fix.contains("work-agent-w"), "{e}");
    assert_eq!(r.read().included("work").len(), 1, "nothing was written");
    // the same actor from another run is another session
    let a2 = Session::agent("agent:w".into(), Some("r2".into()), Role::Writer).unwrap();
    doc(&mut r, &a2, "r2", "c").unwrap();
    let e = doc(&mut r, &a, "r2", "d").unwrap_err();
    assert_eq!(e.code, E_VIEW_OWNER);
    // its own view is fine, twice
    doc(&mut r, &a, "work-agent-w", "e").unwrap();
    doc(&mut r, &a, "work-agent-w", "f").unwrap();
    assert_eq!(r.read().included("work-agent-w").len(), 2);
}

#[test]
fn sess2_a_human_may_write_into_any_view() {
    let (_d, mut r) = project();
    let a = Session::agent("agent:w".into(), Some("r1".into()), Role::Writer).unwrap();
    doc(&mut r, &a, "r1", "a").unwrap();
    let h = Session::human("human:n".into(), Role::Writer).unwrap();
    doc(&mut r, &h, "r1", "b").unwrap();
    assert_eq!(r.read().included("r1").len(), 2);
    let o = r.read().view_head("r1").cloned().unwrap().owner.unwrap();
    assert_eq!(o.actor, "agent:w", "a human's write does not take the view");
}

#[test]
fn sess2_import_is_refused_to_agents_before_any_file_is_read() {
    let (d, mut r) = project();
    let b = Session::agent("agent:b".into(), None, Role::Writer).unwrap();
    let e = r
        .import(&b, "imp", &d.path().join("does-not-exist"))
        .unwrap_err();
    assert_eq!(e.code, E_PERMISSION);
}

#[test]
fn sess2_human_provenance_records_the_cli_run() {
    let (_d, mut r) = project();
    let h = Session::human("human:n".into(), Role::Writer).unwrap();
    let res = doc(&mut r, &h, "work", "a").unwrap();
    let st = r.read();
    let c = st.reader().change(&res.change.parse().unwrap()).unwrap();
    assert_eq!(c.provenance.run.as_deref(), Some("cli"));
}

/// Final review fix round 1 (minor 10): when the refused view is the agent's own default view,
/// the fix does not send it back there.
#[test]
fn sess2_a_refused_default_view_suggests_a_run_name_or_a_fork_not_itself() {
    let (_d, mut r) = project();
    let h = Session::human("human:n".into(), Role::Writer).unwrap();
    doc(&mut r, &h, "work-agent-x", "a").unwrap();
    let a = Session::agent("agent:x".into(), None, Role::Writer).unwrap();
    assert_eq!(a.default_view(), "work-agent-x");
    let e = doc(&mut r, &a, "work-agent-x", "b").unwrap_err();
    assert_eq!(e.code, E_VIEW_OWNER);
    assert!(
        !e.fix.contains("write to your own view"),
        "the fix names the view just refused: {e}"
    );
    assert!(e.fix.contains("RDY_RUN"), "{e}");
    assert!(e.fix.contains("rdy_session"), "{e}");
    assert!(e.fix.contains("fork"), "{e}");
    // with a run, the default view is the run's name: the same fix
    doc(&mut r, &h, "r9", "c").unwrap();
    let a9 = Session::agent("agent:x".into(), Some("r9".into()), Role::Writer).unwrap();
    let e = doc(&mut r, &a9, "r9", "d").unwrap_err();
    assert!(e.fix.contains("RDY_RUN"), "{e}");
}

const ACTOR_FIX: &str =
    "use human:<name>, agent:<name>, agent:<handle>/<name> or ci:<repo_id>/<name>";
const RUN_FIX: &str = "use 1 to 64 letters, digits, -, _ or ., with a letter or digit at each end; not none, work, canon, inflight or reaffirm-\u{2026}";
const REPO_ID: &str = "00000000000000000000000000000000";

/// A project directory with `.rdy/local.yaml` holding `local` (none if `None`), and a user
/// config holding `user` (none if `None`).
fn config_dirs(local: Option<&str>, user: Option<&str>) -> (tempfile::TempDir, std::path::PathBuf) {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join(".rdy")).unwrap();
    if let Some(l) = local {
        std::fs::write(d.path().join(".rdy/local.yaml"), l).unwrap();
    }
    let uf = d.path().join("user.yaml");
    if let Some(u) = user {
        std::fs::write(&uf, u).unwrap();
    }
    (d, uf)
}

/// id3 (D-Q8NFG, O5, O9): the configured person is the local `actor`, else the user's, else the
/// bounded fallback of USER; a present value is validated as written and never rewritten.
#[test]
fn id3_human_fallback_and_config_precedence() {
    let resolve = |local: Option<&str>, user: Option<&str>, env_user: Option<&str>| {
        let (d, uf) = config_dirs(local, user);
        human_actor_for(d.path(), Some(&uf), env_user)
    };
    // local, then user, then the fallback
    assert_eq!(
        resolve(
            Some("actor: human:l\n"),
            Some("actor: human:u\n"),
            Some("x")
        )
        .unwrap(),
        "human:l"
    );
    assert_eq!(
        resolve(None, Some("actor: human:u\n"), Some("x")).unwrap(),
        "human:u"
    );
    assert_eq!(
        resolve(
            Some("serve:\n  idle_timeout: 5\n"),
            Some("views: {}\n"),
            Some("x")
        )
        .unwrap(),
        "human:x",
        "no actor key anywhere: the fallback"
    );
    // the fallback is USER normalized, and human:unknown with no USER
    assert_eq!(
        resolve(None, None, Some("Nick O'Neill")).unwrap(),
        "human:nick-o-neill"
    );
    assert_eq!(resolve(None, None, Some("--")).unwrap(), "human:unknown");
    assert_eq!(resolve(None, None, None).unwrap(), "human:unknown");
    let (d, _) = config_dirs(None, None);
    assert_eq!(
        human_actor_for(d.path(), None, Some("Ada")).unwrap(),
        "human:ada",
        "no user config at all"
    );
    // an empty actor is present: refused, naming its file; it never falls back
    let (d, uf) = config_dirs(None, Some("actor: \"\"\n"));
    let e = human_actor_for(d.path(), Some(&uf), Some("x")).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert_eq!(e.message, "the actor \"\" is not in rdy's actor form");
    assert_eq!(
        e.fix,
        format!(
            "set actor in {} to human:<name>, agent:<name>, agent:<handle>/<name> or ci:<repo_id>/<name>",
            uf.display()
        )
    );
    // an invalid local actor names the local file
    let e = resolve(Some("actor: Nick\n"), Some("actor: human:u\n"), Some("x")).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert_eq!(e.message, "the actor \"Nick\" is not in rdy's actor form");
    assert_eq!(
        e.fix,
        "set actor in .rdy/local.yaml to human:<name>, agent:<name>, agent:<handle>/<name> or ci:<repo_id>/<name>"
    );
    // a valid local actor overrides an invalid user actor
    assert_eq!(
        resolve(
            Some("actor: human:l\n"),
            Some("actor: not an actor\n"),
            Some("x")
        )
        .unwrap(),
        "human:l"
    );
    // ... but never hides a user file that does not parse
    let e = resolve(Some("actor: human:l\n"), Some("actor: [\n"), Some("x")).unwrap_err();
    assert_eq!(e.code, E_CONFIG, "{e}");
    // a wrong type stays E_CONFIG with its file and key
    let (d, uf) = config_dirs(None, Some("actor: [1, 2]\n"));
    let e = human_actor_for(d.path(), Some(&uf), Some("x")).unwrap_err();
    assert_eq!(e.code, E_CONFIG, "{e}");
    assert!(
        e.message
            .starts_with(&format!("actor in {}: ", uf.display())),
        "{e}"
    );
    assert_eq!(e.fix, format!("fix actor in {}", uf.display()));
    // all three actor kinds are accepted in the configured slot (O9), byte for byte
    let ci = format!("ci:{REPO_ID}/nightly");
    for actor in ["human:ada", "agent:claude/r7", "agent:claude", ci.as_str()] {
        assert_eq!(
            resolve(Some(&format!("actor: \"{actor}\"\n")), None, Some("x")).unwrap(),
            actor
        );
    }
    // no silent rewriting: case, space and a missing prefix are refused, not repaired
    for actor in [
        "Human:Nick",
        "human:Nick",
        " human:nick",
        "human:nick ",
        "nick",
    ] {
        let e = resolve(None, Some(&format!("actor: \"{actor}\"\n")), Some("x")).unwrap_err();
        assert_eq!(e.code, E_BAD_INPUT, "{actor:?}: {e}");
        assert_eq!(
            e.message,
            format!("the actor {actor:?} is not in rdy's actor form")
        );
    }
    // the user-file-only resolution agrees
    let x = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(x.path().join("rdy")).unwrap();
    std::fs::write(x.path().join("rdy/config.yaml"), "actor: Nick\n").unwrap();
    let e = human_actor(Some(x.path()), None, Some("x")).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert!(e.fix.starts_with("set actor in "), "{e}");
    assert!(e.fix.ends_with("rdy/config.yaml to human:<name>, agent:<name>, agent:<handle>/<name> or ci:<repo_id>/<name>"), "{e}");
    assert_eq!(
        human_actor(None, None, Some("Ada Lovelace")).unwrap(),
        "human:ada-lovelace"
    );
    // the human's own run: a supplied run is checked, then ignored
    let h = classify("human:n", None, Some("r1"), Role::Writer).unwrap();
    assert_eq!(
        (h.kind, h.run.as_deref()),
        (SessionKind::Human, Some("cli"))
    );
    for (actor, run) in [(None, "../x"), (Some("human:n"), "bad run"), (None, "none")] {
        let e = classify("human:n", actor, Some(run), Role::Writer).unwrap_err();
        assert_eq!(e.code, E_BAD_INPUT, "{run:?}: {e}");
        assert_eq!(
            e.message,
            format!("the run {run:?} is not in rdy's run form")
        );
        assert_eq!(e.fix, RUN_FIX);
    }
    // a configured human outside the form is refused before classification
    let e = classify("Nick", None, None, Role::Writer).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_BAD_INPUT,
            "the actor \"Nick\" is not in rdy's actor form",
            ACTOR_FIX
        )
    );
    // a supplied actor is checked before the run
    let e = classify("human:n", Some("Agent:W"), Some("bad run"), Role::Writer).unwrap_err();
    assert_eq!(
        e.message,
        "the actor \"Agent:W\" is not in rdy's actor form"
    );
}

/// id3 (O10): a supplied intent actor filter is a named input; listing reads stored actors as
/// they are.
#[test]
fn id3_intent_actor_filter_is_a_named_input() {
    let (_d, mut r) = project();
    let a = Session::agent("agent:w".into(), Some("r1".into()), Role::Writer).unwrap();
    let w = doc(&mut r, &a, "r1", "a").unwrap();
    let block = w.created.values().next().unwrap().clone();
    r.intent_post(
        &a,
        Some("r1"),
        None,
        std::slice::from_ref(&block),
        "x",
        None,
    )
    .unwrap();
    // a historical intent, stored with an actor outside today's form
    let old = rdy_model::Intent {
        stable: None,
        prev: None,
        actor: "Old Name".into(),
        run: Some("run:7".into()),
        view: "r1".into(),
        area: None,
        blocks: vec![block.parse().unwrap()],
        angle: "old".into(),
        expires_at_ms: i64::MAX / 2,
        status: rdy_model::IntentStatus::Open,
        at_ms: 1,
    };
    r.store_mut().put_intent(&old).unwrap();
    r.invalidate();
    let all = r.read().intents(&IntentFilter::default(), 2).unwrap();
    let mut actors: Vec<(&str, Option<&str>)> = all
        .iter()
        .map(|i| (i.actor.as_str(), i.run.as_deref()))
        .collect();
    actors.sort();
    assert_eq!(
        actors,
        vec![("Old Name", Some("run:7")), ("agent:w", Some("r1"))],
        "an unfiltered listing reads the stored actor as it is"
    );
    // a valid filter selects; an invalid one is refused with the shared grammar error
    let f = |actor: &str| IntentFilter {
        actor: Some(actor.into()),
        ..IntentFilter::default()
    };
    assert_eq!(r.read().intents(&f("agent:w"), 2).unwrap().len(), 1);
    for bad in ["Old Name", "", "agent:", "x".repeat(300).as_str()] {
        let e = r.read().intents(&f(bad), 2).unwrap_err();
        assert_eq!(e.code, E_BAD_INPUT, "{bad:?}: {e}");
        assert!(e.message.starts_with("the actor \""), "{e}");
        assert!(e.message.ends_with("\" is not in rdy's actor form"), "{e}");
        assert!(
            e.message.chars().count() <= 80 + 1 + 39,
            "the echo is clipped to 80 scalars: {e}"
        );
        assert_eq!(e.fix, ACTOR_FIX);
    }
}

/// id3 (D-ME2C0): records stored before the grammar read, reindex, dump, load and check as they
/// are; their actor and run bytes never change.
#[test]
fn id3_old_stored_identities_remain_readable() {
    use rdy_model::{Change, Op, Provenance};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("p");
    let mut r = Rdy::init(&root, "rdy-standard").unwrap();
    let h = Session::human("human:n".into(), Role::Writer).unwrap();
    let w = doc(&mut r, &h, "work", "a").unwrap();
    let block_id = w.created.values().next().unwrap().clone();
    let b: rdy_model::BlockId = block_id.parse().unwrap();
    // a historical change through the store, as an older rdy recorded it
    let scope = rdy_engine::Scope::View("work".into());
    let mut next = r.read().block(&scope, &b).unwrap();
    let base = next.version_hash();
    next.body = "Old".into();
    let deps = r.read().included("work").to_vec();
    let old_actor = "Old Person";
    let old_run = "run:7";
    let change = r
        .store_mut()
        .commit_to_view(
            "work",
            Change {
                view: "work".into(),
                ops: vec![Op::Update { base, block: next }],
                deps,
                provenance: Provenance {
                    actor: old_actor.into(),
                    run: Some(old_run.into()),
                    role: Role::Writer,
                    at_ms: 5,
                },
                justification: None,
                message: String::new(),
                links: Default::default(),
                read_identity: None,
            },
        )
        .unwrap();
    let intent = rdy_model::Intent {
        stable: None,
        prev: None,
        actor: "agent:Old/Name".into(),
        run: Some("reaffirm-x".into()),
        view: "work".into(),
        area: None,
        blocks: vec![b.clone()],
        angle: "old".into(),
        expires_at_ms: i64::MAX / 2,
        status: rdy_model::IntentStatus::Open,
        at_ms: 6,
    };
    r.store_mut().put_intent(&intent).unwrap();
    r.invalidate();
    let check = |r: &Rdy, what: &str| {
        let st = r.read();
        let c = st.reader().change(&change).unwrap();
        assert_eq!(
            (
                c.provenance.actor.as_bytes(),
                c.provenance.run.as_deref().map(str::as_bytes)
            ),
            (old_actor.as_bytes(), Some(old_run.as_bytes())),
            "{what}: the change's stored identity"
        );
        let all = st
            .intents(
                &IntentFilter {
                    include_expired: true,
                    ..IntentFilter::default()
                },
                7,
            )
            .unwrap();
        assert_eq!(all.len(), 1, "{what}");
        assert_eq!(
            (
                all[0].actor.as_bytes(),
                all[0].run.as_deref().map(str::as_bytes)
            ),
            (b"agent:Old/Name".as_slice(), Some(b"reaffirm-x".as_slice())),
            "{what}: the intent's stored identity"
        );
    };
    check(&r, "read");
    r.reindex().unwrap();
    check(&r, "reindex");
    r.fsck().unwrap();
    let dump = dir.path().join("p.rdydump");
    r.read().dump(&dump).unwrap();
    drop(r);
    let reopened = Rdy::open(&root).unwrap();
    check(&reopened, "reopen");
    drop(reopened);
    let target = dir.path().join("q");
    rdy_engine::load::load(&dump, &target).unwrap();
    let loaded = Rdy::open(&target).unwrap();
    loaded.fsck().unwrap();
    check(&loaded, "dump and load");
}
