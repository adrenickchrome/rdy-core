//! drv6 (D-RDH1C, D-9BWMV, D-MCH70, D-TXFHR): the publication check returns one review JSON form
//! through the engine, the command line and the chat tool. The fixture
//! (`rdy_qa::disclosure_review`) fills every list of the review, with seeded ids and a pinned
//! clock, so the goldens under `tests/golden/disclosure_review/v1/` pin its bytes. Bless them
//! with `RDY_QA_BLESS=1 cargo test -p rdy-qa --test publication drv6_`; after a release a changed
//! shape adds `v2` and never recaptures `v1`.
use rdy_engine::Rdy;
use rdy_engine::publication::{
    self, PublicationReview, ReadPublicationSource, ReviewProjection, disclosure_diff,
    resolve_inspection, review_json,
};
use rdy_qa::disclosure_review::{PROFILE, V1, V2, review_project, review_project_before_removals};
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/disclosure_review/v1")
}

/// `text` against the golden `name`; `RDY_QA_BLESS` writes it instead.
fn check_golden(name: &str, text: &str) {
    let file = golden_dir().join(name);
    if std::env::var_os("RDY_QA_BLESS").is_some() {
        std::fs::create_dir_all(golden_dir()).unwrap();
        std::fs::write(&file, text).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&file).unwrap_or_else(|_| {
        panic!(
            "{name}: no golden at {}; write it with RDY_QA_BLESS=1",
            file.display()
        )
    });
    assert_eq!(
        text, want,
        "{name}: the review changed from the golden (a shipped shape adds v2)"
    );
}

/// The engine's report of the fixture's profile, read in process.
fn engine(dir: &Path, scope: Option<&str>, against: Option<&str>) -> Value {
    let r = Rdy::open(dir).unwrap();
    let v = publication::check(&r.read(), PROFILE, scope, against, None).unwrap();
    r.close().unwrap();
    v
}

/// The command `rdy --no-daemon --project <dir> publication check --profile review ...`, with the
/// person's places in `home`.
fn cli_command(
    dir: &Path,
    home: &Path,
    json: bool,
    scope: Option<&str>,
    against: Option<&str>,
) -> Command {
    let mut cmd = Command::new(rdy_qa::fixtures::rdy_bin());
    rdy_qa::fixtures::isolate_person(&mut cmd, home, &home.join(".config"));
    cmd.args(["--no-daemon", "--project"])
        .arg(dir)
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .current_dir(dir);
    if json {
        cmd.arg("--json");
    }
    cmd.args(["publication", "check", "--profile", PROFILE]);
    if let Some(s) = scope {
        cmd.args(["--scope", s]);
    }
    if let Some(a) = against {
        cmd.args(["--against", a]);
    }
    cmd
}

fn run(mut cmd: Command) -> String {
    let o = cmd.output().expect("run rdy");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// The chat tool's report, from a reader session of a server with `home` as its person.
fn mcp(c: &mut McpClient, scope: Option<&str>, against: Option<&str>) -> Value {
    let mut args = json!({"profile": PROFILE});
    if let Some(s) = scope {
        args["scope"] = json!(s);
    }
    if let Some(a) = against {
        args["against"] = json!(a);
    }
    c.call("rdy_publication_check", args).ok()
}

fn reader(c: &mut McpClient) {
    assert!(
        c.call(
            "rdy_session",
            json!({"actor": "agent:drv6", "run": "drv6", "role": "reader"}),
        )
        .is_ok()
    );
}

/// The scopes the tests read: a first check of each tag and of canon, and comparisons of the
/// second tag and of canon with the first.
const CASES: [(Option<&str>, Option<&str>); 5] = [
    (None, None),
    (Some(V2), None),
    (Some(V2), Some(V1)),
    (None, Some(V1)),
    (None, Some(V2)),
];

#[test]
fn drv6_same_store_cli_mcp_and_engine_reviews_match() {
    let dir = review_project();
    let home = tempfile::tempdir().unwrap();
    // one in-process server holds the store at a time: the engine and the command line read
    // first
    let reports: Vec<(Value, Value)> = CASES
        .iter()
        .map(|(scope, against)| {
            let e = engine(dir.path(), *scope, *against);
            let cli = run(cli_command(dir.path(), home.path(), true, *scope, *against));
            (e, serde_json::from_str(&cli).unwrap())
        })
        .collect();
    let mut c = McpClient::start_isolated(dir.path(), home.path());
    reader(&mut c);
    for ((scope, against), (e, cli)) in CASES.into_iter().zip(reports) {
        let chat = mcp(&mut c, scope, against);
        // one form, key for key and byte for byte
        assert_eq!(cli, e, "{scope:?} {against:?}");
        assert_eq!(chat, e, "{scope:?} {against:?}");
        assert_eq!(cli.to_string(), e.to_string(), "{scope:?} {against:?}");
        assert_eq!(chat.to_string(), e.to_string(), "{scope:?} {against:?}");
        // a comparison has a disclosure and no included list; a first check the other way round
        assert_eq!(e["disclosure"].is_object(), against.is_some());
        assert_eq!(e["included"].is_object(), against.is_none());
    }
    drop(c);

    // the report's three review members are the engine's one builder and serializer
    let r = Rdy::open(dir.path()).unwrap();
    let st = r.read();
    for (scope, against) in [(Some(V2), Some(V1)), (Some(V2), None)] {
        let s = st.resolve_read_scope(scope.unwrap(), None).unwrap();
        let after = resolve_inspection(&st, &s, PROFILE).unwrap();
        let compared = against.map(|a| {
            let b = st.resolve_scope(a).unwrap();
            let before = publication::resolve(&st, &b, PROFILE).unwrap();
            let diff = disclosure_diff(&before, &after).unwrap();
            (before, diff)
        });
        let state = st.materialized(&s).unwrap();
        let (title, description) = (st.project_name(), st.project_description());
        let source =
            ReadPublicationSource::with_settings(&st, st.pack(), &title, description.as_deref());
        let review = PublicationReview::build(
            &source,
            &state,
            &after,
            compared.as_ref().map(|(b, d)| (b, d)),
        )
        .unwrap();
        let j = review_json(
            ReviewProjection::Report,
            &after.references,
            compared.as_ref().map(|(_, d)| d),
            &review,
        );
        let v = publication::check(&st, PROFILE, scope, against, None).unwrap();
        assert_eq!(v["references"], j.references, "{against:?}");
        assert_eq!(v["disclosure"], j.disclosure, "{against:?}");
        assert_eq!(v["included"], j.included, "{against:?}");
    }
    r.close().unwrap();
}

#[test]
fn drv6_first_against_and_redacted_reviews_match_goldens() {
    let dir = review_project_before_removals();
    let removed = review_project();
    let home = tempfile::tempdir().unwrap();
    let pretty = |v: &Value| format!("{}\n", serde_json::to_string_pretty(v).unwrap());
    let first = engine(dir.path(), Some(V2), None);
    let against = engine(dir.path(), Some(V2), Some(V1));
    let redacted = engine(removed.path(), None, Some(V1));
    // the fixture replays: a second project gives the same reports
    let again = review_project_before_removals();
    assert_eq!(first, engine(again.path(), Some(V2), None));
    assert_eq!(against, engine(again.path(), Some(V2), Some(V1)));
    drop(again);
    let again = review_project();
    assert_eq!(redacted, engine(again.path(), None, Some(V1)));

    // the contents the goldens hold, read directly
    let inc = &first["included"];
    assert_eq!(
        (&inc["docs"], &inc["folders"]),
        (&json!(2), &json!(1)),
        "{inc}"
    );
    assert_eq!(inc["docs"], inc["totals"]["docs"]);
    let titles: Vec<&Value> = inc["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| &r["title"])
        .collect();
    assert_eq!(
        titles,
        [&json!("Intro"), &json!("Setup")],
        "reading order: {inc}"
    );
    let d = &against["disclosure"];
    assert_eq!(d["changed_docs"][0]["title"], "Intro", "{d}");
    assert_eq!(d["added_docs"][0]["title"], "Setup", "{d}");
    assert_eq!(d["added_docs"][0]["folder"], "User guides", "{d}");
    assert_eq!(d["removed_docs"][0]["title"], "Old", "{d}");
    assert_eq!(d["changed_folders"][0]["title"], "User guides", "{d}");
    assert_eq!(d["changed_folders"][0]["before_title"], "Guides", "{d}");
    let changes = d["changed_docs"][0]["changes"].as_array().unwrap();
    let p1 = changes
        .iter()
        .find(|c| {
            c["excerpt"]
                .as_str()
                .is_some_and(|e| e.contains("`rdy init`"))
        })
        .unwrap_or_else(|| panic!("the updated prose keeps its code span: {d}"));
    assert_eq!(p1["change"], "updated");
    assert_eq!(p1["what"], json!(["text"]));
    assert!(p1["diff"].is_array(), "{p1}");
    let decisions: Vec<(&Value, &Value, &Value)> = d["changed_docs"][0]["decisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| (&x["title"], &x["change"], &x["what"]))
        .collect();
    assert!(
        decisions.contains(&(&json!("Keep rows"), &json!("updated"), &json!(["status"]))),
        "{decisions:?}"
    );
    assert!(
        decisions.contains(&(&json!("Name pages"), &json!("added"), &json!([]))),
        "{decisions:?}"
    );
    assert_eq!(
        against["references"]["dropped_decisions"]["rows"][0]["title"],
        "Outside ruling"
    );
    assert_eq!(d["changed_outside_docs"][0]["title"], "Root ruling", "{d}");
    // canon removes the second new claim, the second new decision and the added doc
    let rd = &redacted["disclosure"];
    let row = &rd["changed_docs"][0];
    let gone: Vec<&Value> = row["changes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["redacted"] == true)
        .collect();
    assert!(!gone.is_empty(), "{row}");
    for c in gone {
        assert_eq!(c["label"], "Content removed.", "{c}");
        assert!(c["excerpt"].is_null() && c["diff"].is_null(), "{c}");
    }
    assert!(
        row["decisions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["title"] == "Content removed." && x["excerpt"].is_null()),
        "{row}"
    );
    assert!(rd["added_docs"][0]["title"].is_null(), "{rd}");
    assert_eq!(rd["added_docs"][0]["redacted"], true, "{rd}");

    check_golden("first.json", &pretty(&first));
    check_golden("against.json", &pretty(&against));
    check_golden("redacted.json", &pretty(&redacted));
    check_golden(
        "first.txt",
        &run(cli_command(dir.path(), home.path(), false, Some(V2), None)),
    );
    check_golden(
        "against.txt",
        &run(cli_command(
            dir.path(),
            home.path(),
            false,
            Some(V2),
            Some(V1),
        )),
    );
}

/// The bytes of every file under `dir`, by relative path, without the run directory and the
/// index, which are not the record.
fn files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let path = e.path();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            if rel == ".rdy/run" || rel.starts_with(".rdy/run/") || rel.contains("index.redb") {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, out);
            } else if let Ok(bytes) = std::fs::read(&path) {
                out.insert(rel, bytes);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

#[test]
fn drv6_check_is_read_only_and_uses_isolated_home() {
    let dir = review_project();
    let home = tempfile::tempdir().unwrap();
    let before = files(dir.path());
    assert!(
        before.keys().any(|k| k.starts_with(".rdy/store/segments/")),
        "the log is in the print"
    );
    for (scope, against) in CASES {
        let _ = engine(dir.path(), scope, against);
        for json in [true, false] {
            run(cli_command(dir.path(), home.path(), json, scope, against));
        }
    }
    // no check wrote to the project
    let changed = |a: &BTreeMap<String, Vec<u8>>, b: &BTreeMap<String, Vec<u8>>| -> Vec<String> {
        a.keys()
            .chain(b.keys())
            .filter(|k| a.get(*k) != b.get(*k))
            .cloned()
            .collect()
    };
    assert_eq!(changed(&before, &files(dir.path())), Vec::<String>::new());
    // the chat path: opening the session is recorded; the checks after it add nothing
    let mut c = McpClient::start_isolated(dir.path(), home.path());
    reader(&mut c);
    let opened = files(dir.path());
    for (scope, against) in CASES {
        assert!(mcp(&mut c, scope, against).is_object());
    }
    drop(c);
    let after = files(dir.path());
    let log = |m: &BTreeMap<String, Vec<u8>>| -> BTreeMap<String, Vec<u8>> {
        m.iter()
            .filter(|(k, _)| k.starts_with(".rdy/store/segments/"))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    };
    assert_eq!(log(&after), log(&opened), "a check wrote to the log");
    // and none read or wrote the person's real places: the home it was given holds nothing
    let left: Vec<String> = files(home.path()).into_keys().collect();
    assert!(left.is_empty(), "the check wrote into its home: {left:?}");
}

#[test]
fn drv6_review_inputs_and_existing_scope_rules_stay_closed() {
    let dir = review_project();
    let home = tempfile::tempdir().unwrap();
    let mut c = McpClient::start_isolated(dir.path(), home.path());
    let tools = c.tools();
    let t = tools
        .iter()
        .find(|t| t["name"] == "rdy_publication_check")
        .expect("the tool is listed");
    let mut props: Vec<&str> = t["inputSchema"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    props.sort();
    assert_eq!(props, ["against", "profile", "release", "scope"]);
    assert_eq!(t["inputSchema"]["required"], json!(["profile"]));
    reader(&mut c);
    // no review input: a baseline, a projection or a limit is an unknown key
    for key in ["baseline", "review", "limit", "projection"] {
        let r = c.call(
            "rdy_publication_check",
            json!({"profile": PROFILE, key: "canon"}),
        );
        assert_eq!(r.code(), Some("E_BAD_INPUT"), "{key}: {r:?}");
    }
    // a misspelt key names the key it was close to
    let r = c.call(
        "rdy_publication_check",
        json!({"profile": PROFILE, "againts": V1}),
    );
    assert_eq!(r.code(), Some("E_BAD_INPUT"), "{r:?}");
    assert!(format!("{r:?}").contains("against"), "{r:?}");
    // the scope rules stay: a baseline is canon, a tag or a landing
    for bad in ["work", "view:work", "inflight", "proposal:P-ABCDE@1"] {
        let r = c.call(
            "rdy_publication_check",
            json!({"profile": PROFILE, "against": bad}),
        );
        assert_eq!(r.code(), Some("E_BAD_INPUT"), "{bad}: {r:?}");
        assert!(
            format!("{r:?}").contains(
                "Publication check against must be canon, tag:<name>, or landing:<hash>."
            ),
            "{bad}: {r:?}"
        );
    }
    for bad in ["inflight", ""] {
        let r = c.call(
            "rdy_publication_check",
            json!({"profile": PROFILE, "scope": bad}),
        );
        assert_eq!(r.code(), Some("E_BAD_INPUT"), "{bad}: {r:?}");
    }
    // an empty comparison is still a comparison: no included list
    let same = mcp(&mut c, Some(V2), Some(V2));
    assert!(same["included"].is_null(), "{same}");
    let totals = &same["disclosure"]["totals"];
    assert!(
        totals.as_object().unwrap().values().all(|n| n == &json!(0)),
        "{totals}"
    );
    drop(c);
    // the command line takes no review flag
    let mut cmd = cli_command(dir.path(), home.path(), true, None, None);
    cmd.args(["--baseline", "canon"]);
    let o = cmd.output().unwrap();
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("--baseline"));
}
