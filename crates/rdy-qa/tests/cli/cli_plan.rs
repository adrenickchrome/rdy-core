//! cp11 (D-4JMPH, D-EJBPD, D-01QX5, D-KQ4CA; reference/cli p-FHCP2, p-0419N, p-VP1M4, p-ZR5RQ,
//! p-ET7DJ): `rdy plan`, `rdy reaffirm` and `rdy repin` are the chat tools' work at the command
//! line. They build the request for the ctl op, forward the engine's refusals unchanged and
//! print the closed text (O17); `--json` prints the engine's reply (for `repin` the write's
//! report alone).
use rdy_engine::plan::PlanQuery;
use rdy_engine::{DecisionInput, Rdy, RdyError, Scope};
use rdy_model::BlockId;
use rdy_qa::daemon::Served;
use rdy_qa::edit_loop::{brief_of, write_cited};
use rdy_qa::fixtures::{land, write, writer};
use rdy_qa::git_repo::GitRepo;
use serde_json::{Value, json};

/// Canon: doc `led` with section `s`, term `t`, claims `a` and `c`, and `b` (cites `a` and
/// `c`); `n` accepted decisions that cite `t`. View `w` changes `t` and `a`: in `w` the
/// decisions are suspect on `t`, and `b` on `a`.
struct Fx {
    s: String,
    t: String,
    a: String,
    b: String,
    d: Vec<String>,
    landing: String,
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn build(s: &Served, n: usize) -> (Rdy, Fx) {
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let seed = write(&mut r, "seed", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "led", "title": "Ledger", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Balances."},
        {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds 30 days."},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "C stands."},
        {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "B builds on {{$a}} and {{$c}}."}
    ]))
    .unwrap();
    let get = |k: &str| seed.created[k].clone();
    let (sec, t, a, b) = (get("$s"), get("$t"), get("$a"), get("$b"));
    let landing = land(&mut r, "seed").unwrap();
    // the section is stable: writes to it need a briefing
    let (sid, tid, aid) = (id(&sec), id(&t), id(&a));
    let token = brief_of(&r, &Scope::Canon, &[&sid]).base.token;
    write_cited(
        &mut r,
        "st",
        json!([{"op": "update", "id": sec, "fields": {"lifecycle": "stable"}}]),
        Some(&token),
    )
    .unwrap();
    land(&mut r, "st").unwrap();
    let token = brief_of(&r, &Scope::Canon, &[&sid]).base.token;
    let mut d = vec![];
    for i in 0..n {
        d.push(
            r.decide(
                &writer(),
                "dv",
                DecisionInput {
                    parent: sec.clone(),
                    statement: format!("Rule {i}"),
                    body: format!("Per the {{{{{t}}}}}."),
                    brief: Some(token.clone()),
                    ..Default::default()
                },
            )
            .unwrap_or_else(|e| panic!("decide {i}: {e}"))
            .created["$decision"]
                .clone(),
        );
    }
    land(&mut r, "dv").unwrap();
    d.sort();
    let token = brief_of(&r, &Scope::Canon, &[&tid, &aid]).base.token;
    write_cited(
        &mut r,
        "w",
        json!([
            {"op": "update", "id": t, "body": "Balances in pence."},
            {"op": "update", "id": a, "body": "A holds 60 days."}
        ]),
        Some(&token),
    )
    .unwrap();
    let f = Fx {
        s: sec,
        t,
        a,
        b,
        d,
        landing,
    };
    (r, f)
}

fn seeded(n: usize) -> (Served, Fx) {
    let s = Served::new();
    let (r, f) = build(&s, n);
    r.close().unwrap();
    (s, f)
}

/// The engine's own reply to `q`, with the daemon stopped.
fn engine_plan(s: &Served, q: PlanQuery, default_view: &str) -> Result<Value, RdyError> {
    s.stop();
    let r = Rdy::open(s.dir.path()).unwrap();
    let out = r
        .read()
        .change_plan(&q, default_view)
        .map(|p| serde_json::to_value(p).unwrap());
    r.close().unwrap();
    out
}

fn text_of(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stdout).trim_end().to_string()
}

fn items_of<'v>(v: &'v Value, kind: &str) -> Vec<&'v Value> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == kind)
        .collect()
}

fn ids(items: &[&Value]) -> Vec<String> {
    items
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect()
}

/// The six kind lines and the code line a read finishes with, from its JSON.
fn tail_lines(v: &Value) -> Vec<String> {
    let mut out: Vec<String> = [
        "update",
        "rejustify",
        "reaffirm",
        "confirm",
        "update_implementation",
        "tension",
    ]
    .iter()
    .map(|k| {
        let c = &v["counts"]["by_kind"][k];
        format!(
            "{k}: {} open, {} closed, {} deferred",
            c["open"], c["closed"], c["deferred"]
        )
    })
    .collect();
    out.push(if v["code"]["unavailable"] == true {
        "code: unavailable".to_string()
    } else {
        format!("code: {} files", v["code"]["files"])
    });
    out
}

#[test]
fn cp11_cli_plan_text_and_json_match_engine() {
    let s = Served::new();
    let (mut r, f) = build(&s, 2);
    let proposal = r
        .propose(&writer(), "w", "change", None)
        .unwrap_or_else(|e| panic!("propose: {e}"));
    r.close().unwrap();

    // all four selectors: the reply is the engine's, JSON as it is
    let selectors: Vec<(Vec<String>, PlanQuery)> = vec![
        (
            vec!["--view".into(), "w".into()],
            PlanQuery {
                view: Some("w".into()),
                ..Default::default()
            },
        ),
        (
            vec!["--proposal".into(), proposal.r#ref.clone()],
            PlanQuery {
                proposal: Some(proposal.r#ref.clone()),
                ..Default::default()
            },
        ),
        (
            vec!["--landing".into(), f.landing.clone()],
            PlanQuery {
                landing: Some(f.landing.clone()),
                ..Default::default()
            },
        ),
        (
            vec!["--decision".into(), f.d[0].clone()],
            PlanQuery {
                decision: Some(f.d[0].clone()),
                ..Default::default()
            },
        ),
        (vec![], PlanQuery::default()),
    ];
    for (flags, q) in selectors {
        let mut args = vec!["--json", "plan"];
        args.extend(flags.iter().map(String::as_str));
        let (ok, got) = s.json(&args);
        match engine_plan(&s, q, "work") {
            Ok(want) => {
                assert!(ok, "{flags:?}: {got}");
                assert_eq!(got, want, "{flags:?}");
            }
            Err(e) => {
                assert!(!ok, "{flags:?}: {got}");
                assert_eq!(got["code"], e.code, "{flags:?}: {got}");
            }
        }
    }

    // the text of a view's plan: its rows, then the kind lines and the code line
    let (_, v) = s.json(&["--json", "plan", "--view", "w"]);
    assert_eq!(v["scope"], "view:w", "{v}");
    let out = s.rdy(&["plan", "--view", "w"]);
    assert!(out.status.success(), "{out:?}");
    let text = text_of(&out);
    let lines: Vec<&str> = text.lines().collect();
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 3, "{v:#}");
    for i in items {
        let row = format!(
            "{} {} {}: {} ({})",
            i["id"].as_str().unwrap(),
            i["kind"].as_str().unwrap(),
            i["subject"].as_str().unwrap(),
            i["state"].as_str().unwrap(),
            i["cause"].as_str().unwrap()
        );
        assert!(lines.contains(&row.as_str()), "{row} in {text}");
    }
    let confirm = items_of(&v, "confirm");
    assert_eq!(confirm[0]["subject"], f.b.as_str(), "{v:#}");
    assert_eq!(confirm[0]["cause"], f.a.as_str(), "{v:#}");
    assert_eq!(lines.len(), items.len() + 6, "{text}");
    assert_eq!(&lines[items.len()..], tail_lines(&v).as_slice(), "{text}");

    // a cut read prints its cursor last, and the cursor continues it; the pages together are the
    // whole, and the counts are the whole's on each
    let (ok, first) = s.json(&["--json", "plan", "--view", "w", "--max-tokens", "1"]);
    assert!(ok, "{first}");
    assert_eq!(first["items"].as_array().unwrap().len(), 1, "{first:#}");
    assert_eq!(first["counts"], v["counts"]);
    let next = first["next"].as_str().unwrap().to_string();
    let cut = s.rdy(&["plan", "--view", "w", "--max-tokens", "1"]);
    let cut_text = text_of(&cut);
    assert_eq!(
        cut_text.lines().last().unwrap(),
        format!("next: {next}"),
        "{cut_text}"
    );
    let (ok, rest) = s.json(&["--json", "plan", "--page", &next, "--max-tokens", "100000"]);
    assert!(ok, "{rest}");
    let mut both = ids(&items_of(&first, "confirm"));
    both.extend(ids(&items_of(&first, "reaffirm")));
    both.extend(ids(&items_of(&rest, "confirm")));
    both.extend(ids(&items_of(&rest, "reaffirm")));
    both.sort();
    let mut whole = ids(&items.iter().collect::<Vec<_>>());
    whole.sort();
    assert_eq!(both, whole);
    assert!(rest.get("next").is_none(), "{rest:#}");

    // the engine's refusals come back as they are
    for (args, code, message) in [
        (
            vec!["--page", next.as_str(), "--view", "w"],
            "E_BAD_INPUT",
            "page continues a plan: pass it alone",
        ),
        (
            vec!["--view", "w", "--decision", "D-AAAAA"],
            "E_BAD_INPUT",
            "plan takes at most one of view, proposal, landing and decision",
        ),
        (
            vec!["--max-tokens", "100001"],
            "E_LIMIT",
            "max_tokens 100001 is over the limit of 100000",
        ),
    ] {
        let mut all = vec!["--json", "plan"];
        all.extend(args.iter().copied());
        let (ok, e) = s.json(&all);
        assert!(!ok, "{args:?}: {e}");
        assert_eq!(
            (e["code"].as_str(), e["message"].as_str()),
            (Some(code), Some(message))
        );
    }
}

#[test]
fn cp11_cli_answers_use_the_same_atomic_request() {
    let (s, f) = seeded(2);
    let (_, v) = s.json(&["--json", "plan", "--view", "w"]);
    let reaffirm = ids(&items_of(&v, "reaffirm"));
    let confirm = ids(&items_of(&v, "confirm"));
    assert_eq!((reaffirm.len(), confirm.len()), (2, 1), "{v:#}");
    assert!(v["plan"].is_null());

    // one bad item among several: none is recorded
    let (ok, e) = s.json(&[
        "--json",
        "plan",
        "defer",
        &reaffirm[0],
        &reaffirm[1],
        "pi-zzzz",
        "--reason",
        "later",
        "--view",
        "w",
    ]);
    assert!(!ok, "{e}");
    assert_eq!(e["code"], "E_PLAN_ITEM", "{e}");
    let (_, after) = s.json(&["--json", "plan", "--view", "w"]);
    assert!(after["plan"].is_null(), "nothing was recorded: {after}");
    assert_eq!(after, v);

    // the reasons and acts are the engine's rules
    let (ok, e) = s.json(&["--json", "plan", "confirm", &confirm[0], "--view", "w"]);
    assert!(!ok, "{e}");
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    let (ok, e) = s.json(&[
        "--json",
        "plan",
        "done",
        &confirm[0],
        "--reason",
        "x",
        "--view",
        "w",
    ]);
    assert!(!ok, "{e}");
    assert_eq!(e["message"], "done does not answer a confirm item", "{e}");
    // no items is the grammar's refusal, not a read
    let none = s.rdy(&["plan", "defer", "--reason", "x"]);
    assert!(!none.status.success());
    // another agent does not own the view
    let (ok, e) = s.json_as(
        "agent:x",
        Some("r1"),
        &[
            "--json",
            "plan",
            "defer",
            &reaffirm[0],
            "--reason",
            "x",
            "--view",
            "w",
        ],
    );
    assert!(!ok, "{e}");
    assert_eq!(e["code"], "E_VIEW_OWNER", "{e}");

    // several ids in one request: the text lists them in request order, as typed (a prefix too)
    let prefix = &reaffirm[1][..9];
    let out = s.rdy(&[
        "plan",
        "defer",
        &reaffirm[0],
        prefix,
        "--reason",
        "later",
        "--view",
        "w",
    ]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        text_of(&out),
        format!("answered {}: defer\nanswered {prefix}: defer", reaffirm[0])
    );
    let (ok, recorded) = s.json(&[
        "--json",
        "plan",
        "confirm",
        &confirm[0],
        "--reason",
        "still holds",
        "--view",
        "w",
    ]);
    assert!(ok, "{recorded}");
    // the JSON reply is the plan after the answers: what a read gives now
    let (_, now) = s.json(&["--json", "plan", "--view", "w"]);
    assert_eq!(recorded, now);
    assert_eq!(now["counts"]["open"], 0, "{now:#}");
    assert_eq!(now["counts"]["closed"], 1, "{now:#}");
    assert_eq!(now["counts"]["deferred"], 2, "{now:#}");
    let row = items_of(&now, "reaffirm")[0];
    assert_eq!(row["answer"]["reason"], "later");
    // a deferred row prints its reason on the line after it
    let text = text_of(&s.rdy(&["plan", "--view", "w"]));
    assert!(
        text.contains(&format!(
            "{} reaffirm {}: deferred ({})\n  later",
            row["id"].as_str().unwrap(),
            row["subject"].as_str().unwrap(),
            f.t
        )),
        "{text}"
    );
}

#[test]
fn cp11_cli_reaffirm_fetches_the_whole_briefing() {
    let s = Served::new();
    let (mut r, f) = build(&s, 300);
    // a second view changes the term too: the ids form is tried there
    let token = brief_of(&r, &Scope::Canon, &[&id(&f.t)]).base.token;
    write_cited(
        &mut r,
        "w2",
        json!([{"op": "update", "id": f.t, "body": "Balances in cents."}]),
        Some(&token),
    )
    .unwrap();
    r.close().unwrap();
    assert_eq!(f.d.len(), 300);

    // the grammar: ids xor because, and a reason
    for (args, message) in [
        (
            vec!["--json", "reaffirm", "--reason", "x", "--view", "w"],
            "reaffirm needs ids or because",
        ),
        (
            vec![
                "--json",
                "reaffirm",
                &f.d[0],
                "--because",
                &f.t,
                "--reason",
                "x",
                "--view",
                "w",
            ],
            "reaffirm takes ids or because, not both",
        ),
    ] {
        let (ok, e) = s.json(&args);
        assert!(!ok, "{args:?}: {e}");
        assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
        assert_eq!(e["message"], message, "{e}");
    }
    assert!(
        !s.rdy(&["reaffirm", "--because", &f.t, "--view", "w"])
            .status
            .success(),
        "a reason is required"
    );
    // a target that is not one of the decision's suspects is the write's refusal
    let (ok, e) = s.json(&[
        "--json",
        "reaffirm",
        &f.d[0],
        "--reason",
        "x",
        "--targets",
        &f.a,
        "--view",
        "w",
    ]);
    assert!(!ok, "{e}");
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");

    // because: every decision suspect on the term, 300 of them, one briefing, one write
    let (ok, v) = s.json(&[
        "--json",
        "reaffirm",
        "--because",
        &f.t,
        "--reason",
        "Holds in pence.",
        "--view",
        "w",
    ]);
    assert!(ok, "{v}");
    let rows = v["reaffirmed"].as_array().unwrap();
    assert_eq!(rows.len(), 300, "{} rows", rows.len());
    assert_eq!(
        rows.iter()
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        f.d
    );
    assert!(rows.iter().all(|r| r["causes"] == json!([f.t])));
    // nothing is left to re-affirm in that view
    let (ok, e) = s.json(&[
        "--json",
        "reaffirm",
        "--because",
        &f.t,
        "--reason",
        "again",
        "--view",
        "w",
    ]);
    assert!(!ok, "{e}");
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");

    // ids: the same 300 on the command line, in a text run
    let mut args: Vec<&str> = vec!["reaffirm"];
    args.extend(f.d.iter().map(String::as_str));
    args.extend(["--reason", "Holds in cents.", "--view", "w2"]);
    let out = s.rdy(&args);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = text_of(&out);
    let lines: Vec<&str> = text.lines().collect();
    for (i, id) in f.d.iter().enumerate() {
        assert_eq!(
            lines[i],
            format!("reaffirmed {id} (after {})", f.t),
            "line {i}"
        );
    }
    assert!(lines[300].starts_with("change "), "{}", lines[300]);
    let _ = (&f.s, &f.b);
}

#[test]
fn cp11_cli_repin_reports_selected_design_pin_count() {
    let s = Served::new();
    let (mut r, f) = build(&s, 1);
    // a claim that reflects code and cites nothing, in `w`
    let g = GitRepo::new();
    g.write("src/a.rs", "a\n");
    g.commit("first");
    g.link(s.dir.path());
    let token = brief_of(&r, &Scope::Canon, &[&id(&f.s)]).base.token;
    let q = write_cited(
        &mut r,
        "w",
        json!([{"op": "create", "type": "claim", "id": "$q", "parent": f.s,
            "body": "Q follows the code.", "fields": {"reflects": ["src/*.rs"]}}]),
        Some(&token),
    )
    .unwrap()
    .created["$q"]
        .clone();
    r.close().unwrap();
    g.write("src/a.rs", "a2\n");
    g.commit("drift");

    // changed and current: `b` cites `a` (changed in `w`) and `c` (current): both are selected
    let out = s.rdy(&["repin", &f.b, "--view", "w"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = text_of(&out);
    assert!(
        text.starts_with(&format!("repinned {}: 2 pin(s)\nchange ", f.b)),
        "{text}"
    );
    // JSON is the write's report alone, as rdy_write gives it
    let (ok, e) = s.json(&["--json", "repin", &f.b, "--view", "w"]);
    assert!(!ok, "{e}");
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    assert!(
        e["message"].as_str().unwrap().contains("nothing to repin"),
        "{e}"
    );
    // a repin that selects nothing and records no code is the write's refusal
    let (ok, e) = s.json(&["--json", "repin", &f.a, "--view", "w"]);
    assert!(!ok, "{e}");

    // code: the block's code state is recorded, and the design pins counted are the cited ones
    // (none): zero, and still a change
    let out = s.rdy(&["repin", &q, "--code", "--view", "w"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = text_of(&out);
    assert!(
        text.starts_with(&format!("repinned {q}: 0 pin(s)\nchange ")),
        "{text}"
    );
    let (ok, v) = s.json(&["--json", "repin", &q, "--code", "--view", "w"]);
    assert!(ok, "a code repin is a change of its own: {v}");
    assert!(v["change"].is_string(), "{v}");
    assert!(v.get("pins").is_none() && v.get("result").is_none(), "{v}");

    // the write guards are the write path's: an accepted decision is frozen
    let (ok, e) = s.json(&["--json", "repin", &f.d[0], "--view", "w"]);
    assert!(!ok, "{e}");
    assert_eq!(e["code"], "E_FROZEN", "{e}");
    // a target the block does not cite is refused before the write, with no count
    let (ok, e) = s.json(&["--json", "repin", &f.b, "--targets", &f.t, "--view", "w"]);
    assert!(!ok, "{e}");
    assert_eq!(
        e["message"].as_str().unwrap(),
        format!("{} does not reference or link to {}", f.b, f.t)
    );
}

/// Replaces the ids a run makes up (block ids, hashes) so two runs on two projects compare.
fn mask(text: &str) -> Vec<String> {
    let re = regex::Regex::new(r"\b[A-Za-z]{1,2}-[0-9A-Z]{5}\b|[0-9a-f]{12,64}").unwrap();
    let mut lines: Vec<String> = text
        .lines()
        .map(|l| re.replace_all(l, "<id>").into_owned())
        .collect();
    lines.sort();
    lines
}

#[test]
fn cp11_daemon_and_inprocess_paths_match() {
    // two identical projects: one is driven with --no-daemon, the other through its daemon
    let (a, fa) = seeded(2);
    let (b, fb) = seeded(2);
    // a write runs once, in text; a read also gives its JSON
    let run = |s: &Served, direct: bool, args: &[&str], json_too: bool| -> (bool, String, Value) {
        let mut all: Vec<&str> = if direct { vec!["--no-daemon"] } else { vec![] };
        all.extend(args);
        let out = s.rdy(&all);
        let text = if out.status.success() {
            text_of(&out)
        } else {
            String::from_utf8_lossy(&out.stderr).trim_end().to_string()
        };
        let mut v = Value::Null;
        if json_too {
            let mut all_json: Vec<&str> = vec!["--json"];
            if direct {
                all_json.push("--no-daemon");
            }
            all_json.extend(args);
            v = s.json(&all_json).1;
        }
        (out.status.success(), text, v)
    };
    // the read
    let (oa, ta, ja) = run(&a, true, &["plan", "--view", "w"], true);
    let (ob, tb, jb) = run(&b, false, &["plan", "--view", "w"], true);
    assert!(oa && ob);
    assert_eq!(mask(&ta), mask(&tb));
    assert_eq!(ja["counts"], jb["counts"]);
    let keys = |v: &Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
    assert_eq!(keys(&ja), keys(&jb));

    // the answers
    let defer = |j: &Value| -> Vec<String> { ids(&items_of(j, "reaffirm")) };
    let (da, db) = (defer(&ja), defer(&jb));
    let (oa, ta, _) = run(
        &a,
        true,
        &["plan", "defer", &da[0], "--reason", "later", "--view", "w"],
        false,
    );
    let (ob, tb, _) = run(
        &b,
        false,
        &["plan", "defer", &db[0], "--reason", "later", "--view", "w"],
        false,
    );
    assert!(oa && ob, "{ta} / {tb}");
    assert_eq!(mask(&ta), mask(&tb));

    // reaffirm
    let (oa, ta, _) = run(
        &a,
        true,
        &[
            "reaffirm",
            "--because",
            &fa.t,
            "--reason",
            "Holds.",
            "--view",
            "w",
        ],
        false,
    );
    let (ob, tb, _) = run(
        &b,
        false,
        &[
            "reaffirm",
            "--because",
            &fb.t,
            "--reason",
            "Holds.",
            "--view",
            "w",
        ],
        false,
    );
    assert!(oa && ob, "{ta} / {tb}");
    assert_eq!(mask(&ta), mask(&tb));
    assert_eq!(ta.matches("\nreaffirmed ").count() + 1, 2, "{ta}");
    // the same refusal when there is nothing left
    let (oa, ta, _) = run(
        &a,
        true,
        &[
            "reaffirm",
            "--because",
            &fa.t,
            "--reason",
            "Holds.",
            "--view",
            "w",
        ],
        false,
    );
    let (ob, tb, _) = run(
        &b,
        false,
        &[
            "reaffirm",
            "--because",
            &fb.t,
            "--reason",
            "Holds.",
            "--view",
            "w",
        ],
        false,
    );
    assert!(!oa && !ob);
    assert_eq!(mask(&ta), mask(&tb));

    // repin
    let (oa, ta, _) = run(&a, true, &["repin", &fa.b, "--view", "w"], false);
    let (ob, tb, _) = run(&b, false, &["repin", &fb.b, "--view", "w"], false);
    assert!(oa && ob, "{ta} / {tb}");
    assert_eq!(mask(&ta), mask(&tb));
    assert!(
        ta.starts_with(&format!("repinned {}: 2 pin(s)", fa.b)),
        "{ta}"
    );
}

#[test]
fn cp11_cli_writes_use_the_bound_view() {
    let s = Served::new();
    let (mut r, f) = build(&s, 2);
    let proposal = r
        .propose(&writer(), "w", "change", None)
        .unwrap_or_else(|e| panic!("propose: {e}"));
    // a second view, so a read can name a view other than the bound one
    write(&mut r, "other", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "oth", "title": "Other", "doc_type": "page"}}
    ]))
    .unwrap();
    r.close().unwrap();
    // the working copy is bound to `w`
    let out = s.rdy(&["checkout", "w"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // a read that names nothing reads the bound view; one that names a selector keeps it
    let (ok, bound) = s.json(&["--json", "plan"]);
    assert!(ok, "{bound}");
    assert_eq!(bound["scope"], "view:w", "{bound}");
    let (ok, keep) = s.json(&["--json", "plan", "--proposal", &proposal.r#ref]);
    assert!(ok, "{keep}");
    assert_eq!(
        keep["scope"],
        format!("proposal:{}", proposal.r#ref),
        "{keep}"
    );
    let (ok, keep) = s.json(&["--json", "plan", "--decision", &f.d[0]]);
    assert!(ok, "{keep}");
    assert_eq!(keep["scope"], format!("decision:{}", f.d[0]), "{keep}");
    let (ok, named) = s.json(&["--json", "plan", "--view", "other"]);
    assert!(ok, "{named}");
    assert_eq!(named["scope"], "view:other", "{named}");

    // answers go to the bound view
    let reaffirm = ids(&items_of(&bound, "reaffirm"));
    let out = s.rdy(&["plan", "defer", &reaffirm[0], "--reason", "later"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (_, now) = s.json(&["--json", "plan", "--view", "w"]);
    assert_eq!(now["counts"]["deferred"], 1, "{now:#}");

    // a repin and a re-affirmation write the bound view too
    let (ok, v) = s.json(&["--json", "repin", &f.b]);
    assert!(ok, "{v}");
    assert!(v["change"].is_string(), "{v}");
    let (ok, v) = s.json(&[
        "--json",
        "reaffirm",
        "--because",
        &f.t,
        "--reason",
        "Holds.",
    ]);
    assert!(ok, "{v}");
    assert_eq!(v["reaffirmed"].as_array().unwrap().len(), 2, "{v}");
    let (_, after) = s.json(&["--json", "plan"]);
    assert_eq!(after["scope"], "view:w");
    assert_eq!(items_of(&after, "reaffirm").len(), 2, "{after:#}");
}
