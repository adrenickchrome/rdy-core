//! vxs8 (D-76Y9H, D-5VGAS): the three search modes at the command line. `rdy search` takes a
//! query, `--like ID` or `--like-text TEXT`, and `--sort coverage|relevance|reading`; it sends
//! them through the daemon's `search` op, so its JSON is the reply that MCP gives and its
//! refusals are MCP's, with the same codes, messages and fixes, in the same order. The text
//! output shows `score=` beside each ranked hit and, for a like hit, its shared words and link
//! targets on one following line; the default text and JSON keep their bytes.
use rdy_engine::{Rdy, Scope};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use rdy_qa::mcp::{McpClient, Reply};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// The ids and scopes of [`reef`].
struct Reef {
    ids: BTreeMap<String, String>,
    /// The first landing, tagged v1.0.0.
    past: String,
    /// A proposal version, and its short form.
    proposal: String,
    short: String,
}

/// Doc `reef`: a term (alias Kardex), the source claim `$src` and claims that share its words,
/// its link or both, a later landing and an open proposal.
fn reef(dir: &std::path::Path) -> Reef {
    let mut r = Rdy::open(dir).unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$doc", "fields": {"name": "reef", "title": "Reef", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Shore"}},
            {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "General ledger", "aliases": ["Kardex"]}, "body": "Holds balances."},
            {"op": "create", "type": "claim", "id": "$src", "parent": "$s", "body": "Kelp forests shelter otters at dawn.", "links": {"describes": [{"target": "$t"}]}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "Kelp forests shelter otters.\nMany more words live here in a long second line."},
            {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "Kelp forests shelter otters.\nShort."},
            {"op": "create", "type": "claim", "id": "$z", "parent": "$s", "body": "Zebra stripes.", "links": {"describes": [{"target": "$t"}]}},
            {"op": "create", "type": "claim", "id": "$y", "parent": "$s", "body": "Desert sand.", "links": {"describes": [{"target": "$t"}]}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": "Kelp grows in cold water."}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let past = r.read().canon_head().unwrap().to_string();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    write(
        &mut r,
        "more",
        json!([{"op": "create", "type": "claim", "parent": w.created["$s"], "body": "Kelp forests at noon."}]),
    )
    .unwrap();
    land(&mut r, "more").unwrap();
    r.tag(&approver(), "v1.1.0", None, None).unwrap();
    write(
        &mut r,
        "p",
        json!([{"op": "create", "type": "claim", "parent": w.created["$s"], "body": "Kelp forests at dusk."}]),
    )
    .unwrap();
    let p = r.propose(&writer(), "p", "p", Some("Add a claim")).unwrap();
    let pref = r.resolve_proposal(&p.id).unwrap();
    let proposal = format!("proposal:{pref}");
    let Scope::Proposal(v) = r.read().resolve_scope(&proposal).unwrap() else {
        panic!("a proposal version");
    };
    r.close().unwrap();
    Reef {
        ids: w.created,
        past,
        proposal,
        short: v.short(),
    }
}

/// A served project seeded by `seed`, and the MCP proxy to its daemon, in an agent session.
fn served<T>(seed: impl FnOnce(&std::path::Path) -> T) -> (Served, McpClient, T) {
    let s = Served::new();
    let t = seed(s.dir.path());
    let mut c = McpClient::start_proxy(&s);
    let opened = c.call(
        "rdy_session",
        json!({"actor": "agent:v", "run": "v8", "role": "writer"}),
    );
    assert!(opened.is_ok(), "{opened:?}");
    (s, c, t)
}

/// What a surface gives: the result, or the refusal's code, message and fix.
#[derive(Clone, Debug, PartialEq)]
enum Got {
    Ok(Value),
    No(String, String, String),
}

impl Got {
    fn ok(self) -> Value {
        match self {
            Got::Ok(v) => v,
            Got::No(c, m, f) => panic!("refused: {c}: {m} ({f})"),
        }
    }

    fn no(c: &str, m: &str, f: &str) -> Got {
        Got::No(c.into(), m.into(), f.into())
    }

    fn of_refusal(v: &Value) -> Got {
        let s = |k: &str| {
            v[k].as_str()
                .unwrap_or_else(|| panic!("no {k} in {v}"))
                .to_string()
        };
        Got::No(s("code"), s("message"), s("fix"))
    }
}

/// `rdy --json search <args>`.
fn cli(s: &Served, args: &[&str]) -> Got {
    let mut a = vec!["--json", "search"];
    a.extend_from_slice(args);
    let (ok, v) = s.json(&a);
    if ok { Got::Ok(v) } else { Got::of_refusal(&v) }
}

/// `rdy search <args>`: the text, after checking it succeeded.
fn cli_text(s: &Served, args: &[&str]) -> String {
    let mut a = vec!["search"];
    a.extend_from_slice(args);
    let out = s.rdy(&a);
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

/// `rdy_search` with `args`.
fn mcp(c: &mut McpClient, args: Value) -> Got {
    match c.call("rdy_search", args) {
        Reply::Ok(v) => Got::Ok(v),
        Reply::ToolError(v) => Got::of_refusal(&v),
        Reply::RpcError(v) => panic!("a protocol error: {v}"),
    }
}

/// The command line's flags for an `rdy_search` argument object (the scope is the default
/// canon unless it names one).
fn flags(args: &Value) -> Vec<String> {
    let mut out = vec![];
    let o = args.as_object().unwrap();
    if let Some(q) = o.get("query") {
        out.push(q.as_str().unwrap().to_string());
    }
    for (k, v) in o {
        let flag = match k.as_str() {
            "query" => continue,
            "like" => "--like",
            "like_text" => "--like-text",
            "sort" => "--sort",
            "scope" => "--scope",
            "path" => "--path",
            "max_tokens" => "--max-tokens",
            "page" => "--page",
            "regex" => {
                if v == &json!(true) {
                    out.push("--regex".into());
                }
                continue;
            }
            "ignore_case" => {
                if v == &json!(true) {
                    out.push("-i".into());
                }
                continue;
            }
            "types" => {
                for t in v.as_array().unwrap() {
                    out.push("--type".into());
                    out.push(t.as_str().unwrap().into());
                }
                continue;
            }
            other => panic!("no flag for {other}"),
        };
        out.push(flag.into());
        out.push(match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        });
    }
    out
}

/// The CLI and MCP replies for `args`, which must agree; the agreed reply.
fn both(s: &Served, c: &mut McpClient, args: Value) -> Got {
    let f = flags(&args);
    let f: Vec<&str> = f.iter().map(String::as_str).collect();
    let at_cli = cli(s, &f);
    let at_mcp = mcp(c, args.clone());
    let what = args.to_string();
    assert_eq!(at_cli, at_mcp, "{}", &what[..what.len().min(200)]);
    at_cli
}

fn ids(res: &Value) -> Vec<String> {
    res["matches"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| {
            std::iter::once(m["id"].as_str().unwrap().to_string()).chain(
                m["also"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|a| a["id"].as_str().unwrap().to_string()),
            )
        })
        .collect()
}

/// Whether any object in `v` has member `key`.
fn has(v: &Value, key: &str) -> bool {
    match v {
        Value::Object(o) => o.contains_key(key) || o.values().any(|x| has(x, key)),
        Value::Array(a) => a.iter().any(|x| has(x, key)),
        _ => false,
    }
}

/// `n` bytes of text with one known word, `kelp`, and two-byte characters.
fn sized(n: usize) -> String {
    let mut s = "kelp ".to_string();
    while s.len() + 2 <= n {
        s.push('é');
    }
    while s.len() < n {
        s.push('a');
    }
    assert_eq!(s.len(), n);
    s
}

#[test]
fn vxs8_three_modes_have_cli_mcp_and_daemon_parity() {
    let (s, mut c, f) = served(reef);
    let id = |k: &str| f.ids[k].clone();
    let modes = [
        json!({"query": "Kelp"}),
        json!({"query": "Kelp forests", "sort": "relevance"}),
        json!({"query": "Kelp forests", "sort": "reading"}),
        json!({"query": "Kelp forests", "sort": "coverage"}),
        json!({"like": id("$src")}),
        json!({"like": id("$src"), "types": ["claim"]}),
        json!({"like_text": "the Kardex kelp otters"}),
        json!({"like_text": "kelp otters", "path": "reef", "scope": "tag:v1.1.0"}),
    ];
    let mut through_daemon = vec![];
    for args in &modes {
        let res = both(&s, &mut c, args.clone()).ok();
        assert!(res["total"].as_u64().unwrap() > 0, "{args}: {res}");
        through_daemon.push(res);
    }
    // the like modes echo their source; the CLI's JSON carries the scored fields
    let like = &through_daemon[4];
    assert_eq!(like["like"], id("$src"), "{like}");
    assert_eq!(like["query"], "", "{like}");
    assert!(like["matches"][0]["score"].is_number(), "{like}");
    assert!(like["matches"][0]["words"].is_array(), "{like}");
    assert!(like["matches"][0]["links"].is_array(), "{like}");
    assert_eq!(through_daemon[6]["like_text"], "the Kardex kelp otters");
    assert!(through_daemon[1]["matches"][0]["score"].is_number());
    drop(c);
    s.stop();
    // the in-process server, with no daemon, gives the same replies
    let mut direct = McpClient::start(s.dir.path());
    let opened = direct.call(
        "rdy_session",
        json!({"actor": "agent:v", "run": "v8", "role": "writer"}),
    );
    assert!(opened.is_ok(), "{opened:?}");
    for (args, want) in modes.iter().zip(&through_daemon) {
        assert_eq!(&mcp(&mut direct, args.clone()).ok(), want, "{args}");
    }
    direct.alive();
}

#[test]
fn vxs8_ranked_text_shows_scores_and_like_reasons() {
    let (s, mut c, f) = served(reef);
    let id = |k: &str| f.ids[k].clone();
    let src = id("$src");
    // like: every hit's head carries its score; a following line names its words and links
    let v = mcp(&mut c, json!({"like": src})).ok();
    let text = cli_text(&s, &["--like", &src]);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[0],
        format!("{} matching blocks", v["total"]),
        "{text}"
    );
    for m in v["matches"].as_array().unwrap() {
        let hit = m["id"].as_str().unwrap();
        let score = format!("score={:.3}", m["score"].as_f64().unwrap());
        let at = lines
            .iter()
            .position(|l| l.starts_with(&format!("reef  {hit} (")))
            .unwrap_or_else(|| panic!("{hit}: {text}"));
        assert!(lines[at].ends_with(&format!("  {score}")), "{text}");
        let list = |k: &str| {
            m[k].as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_str().unwrap().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut parts = vec![];
        if !list("words").is_empty() {
            parts.push(format!("words {}", list("words")));
        }
        if !list("links").is_empty() {
            parts.push(format!("links {}", list("links")));
        }
        assert_eq!(
            lines[at + 1],
            format!("  shares: {}", parts.join("; ")),
            "{text}"
        );
    }
    // the link-only hit: no matching line, its link on the shares line
    let z = id("$z");
    let t = id("$t");
    let at = lines
        .iter()
        .position(|l| l.starts_with(&format!("reef  {z} (claim)")))
        .unwrap();
    assert_eq!(lines[at + 1], format!("  shares: links {t}"), "{text}");
    assert!(!lines[at + 2].starts_with("  body:"), "{text}");
    // a collapsed member keeps its own score and reasons
    let b = id("$b");
    let a = id("$a");
    let entry = v["matches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == b)
        .unwrap();
    let member = &entry["also"][0];
    assert_eq!(member["id"], a);
    let words = member["words"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect::<Vec<_>>()
        .join(", ");
    let also = format!(
        "  also: reef {a} score={:.3} (words {words})",
        member["score"].as_f64().unwrap()
    );
    assert!(lines.contains(&also.as_str()), "{also}\n{text}");
    // like_text shows the same layout
    let text = cli_text(&s, &["--like-text", "the Kardex kelp otters"]);
    assert!(
        text.lines()
            .any(|l| l.starts_with(&format!("reef  {z} (claim)")) && l.contains("  score=")),
        "{text}"
    );
    assert!(
        text.contains(&format!("  shares: links {t}")),
        "the alias names the term: {text}"
    );
    // relevance: scores beside each hit and member, and no shares line
    let v = mcp(
        &mut c,
        json!({"query": "Kelp forests shelter otters", "sort": "relevance"}),
    )
    .ok();
    let text = cli_text(&s, &["Kelp forests shelter otters", "--sort", "relevance"]);
    assert!(!text.contains("shares:"), "{text}");
    for m in v["matches"].as_array().unwrap() {
        let head = text
            .lines()
            .find(|l| l.starts_with(&format!("reef  {} (", m["id"].as_str().unwrap())))
            .unwrap_or_else(|| panic!("{m}: {text}"));
        assert!(
            head.ends_with(&format!("  score={:.3}", m["score"].as_f64().unwrap())),
            "{head}"
        );
        for member in m["also"].as_array().into_iter().flatten() {
            let shown = format!(
                "reef {} score={:.3}",
                member["id"].as_str().unwrap(),
                member["score"].as_f64().unwrap()
            );
            assert!(text.contains(&shown), "{shown}: {text}");
        }
    }
    // reading and coverage order show no score
    for sort in ["reading", "coverage"] {
        let text = cli_text(&s, &["Kelp", "--sort", sort]);
        assert!(!text.contains("score="), "{sort}: {text}");
    }
    s.stop();
}

#[test]
fn vxs8_default_cli_text_and_json_keep_their_bytes() {
    let (s, mut c, (p, k)) = served(|dir| {
        let mut r = Rdy::open(dir).unwrap();
        let w = write(&mut r, "work", json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Setup"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": "Install the tool.\nRun rdy init to start."},
            {"op": "create", "type": "code", "id": "$c", "parent": "$s", "body": "rdy init", "fields": {"lang": "sh"}}
        ]))
        .unwrap();
        land(&mut r, "work").unwrap();
        r.close().unwrap();
        (w.created["$p"].clone(), w.created["$c"].clone())
    });
    assert_eq!(
        cli_text(&s, &["rdy init"]),
        format!(
            "2 matching blocks\nby doc: guide 2\nby type: code 1, prose 1\nguide  {p} (prose)  coverage 80  issues 1  low\n  body:2: Run rdy init to start.\nguide  {k} (code)  coverage 75  issues 1  low\n  body:1: rdy init"
        )
    );
    let v = both(&s, &mut c, json!({"query": "rdy init"})).ok();
    for key in ["score", "words", "links", "like", "like_text"] {
        assert!(!has(&v, key), "{key}: {v}");
    }
    assert_eq!(v["query"], "rdy init");
    assert_eq!(v["total"], 2);
    let (ok, v) = s.json(&["--json", "search", "RDY.INIT", "--regex", "-i"]);
    assert!(ok, "{v}");
    assert_eq!(v["total"], 2);
    s.stop();
}

#[test]
fn vxs8_eight_refusals_and_precedence_match_mcp_exactly() {
    let (s, mut c, f) = served(reef);
    let src = f.ids["$src"].clone();
    let one = Got::no(
        "E_BAD_INPUT",
        "give exactly one of query, like or like_text",
        "send one of query, like or like_text",
    );
    let regex = Got::no("E_BAD_INPUT", "regex applies to query only", "drop regex");
    let sort = Got::no(
        "E_BAD_INPUT",
        "sort applies to query only",
        "drop sort; like and like_text hits come in score order",
    );
    let literal = Got::no(
        "E_BAD_INPUT",
        "sort relevance needs a literal query",
        "drop regex, or drop sort",
    );
    let long = Got::no(
        "E_LIMIT",
        "like_text is over 16384 bytes",
        "send a shorter like_text",
    );
    let unknown_word = Got::no(
        "E_BAD_INPUT",
        "like_text has no word that a block in this scope holds",
        "use words that the project uses, or search with query",
    );
    let unknown_block = Got::no(
        "E_UNKNOWN_BLOCK",
        "no block c-ZZZZZ",
        "check the id with rdy_read",
    );
    let at_past = |p: &str| {
        Got::no(
            "E_BAD_INPUT",
            &format!("{p} ranks the current state only, not landing:{}", f.past),
            "ask for canon or a view; ranking over time is not part of rdy core",
        )
    };
    let at_proposal = |p: &str| {
        Got::no(
            "E_BAD_INPUT",
            &format!("{p} ranks canon and views only, not proposal:{}", f.short),
            "ask for the proposal's view, or canon",
        )
    };
    let past = "tag:v1.0.0";
    let cases: Vec<(Value, Got)> = vec![
        // 1: two or three sources; a blank value is a supplied value
        (json!({"query": "kelp", "like": src}), one.clone()),
        (json!({"query": "kelp", "like_text": "kelp"}), one.clone()),
        (json!({"like": src, "like_text": "kelp"}), one.clone()),
        (
            json!({"query": "kelp", "like": src, "like_text": "kelp"}),
            one.clone(),
        ),
        (json!({"query": "", "like_text": ""}), one.clone()),
        (
            json!({"query": "kelp", "like": src, "regex": true, "sort": "relevance"}),
            one.clone(),
        ),
        // 2: regex with a like mode, before the sort refusal
        (json!({"like": src, "regex": true}), regex.clone()),
        (json!({"like_text": "kelp", "regex": true}), regex.clone()),
        (
            json!({"like": src, "regex": true, "sort": "relevance"}),
            regex.clone(),
        ),
        (
            json!({"like_text": sized(16_385), "regex": true}),
            regex.clone(),
        ),
        // 3: any sort with a like mode, valid or not
        (json!({"like": src, "sort": "relevance"}), sort.clone()),
        (json!({"like": src, "sort": "reading"}), sort.clone()),
        (
            json!({"like_text": "kelp", "sort": "coverage"}),
            sort.clone(),
        ),
        (json!({"like_text": "kelp", "sort": "score"}), sort.clone()),
        (json!({"like": "c-ZZZZZ", "sort": "reading"}), sort.clone()),
        // 4: relevance needs a literal, before the scope refusal
        (
            json!({"query": "kelp", "regex": true, "sort": "relevance"}),
            literal.clone(),
        ),
        (
            json!({"query": "kelp", "regex": true, "sort": "relevance", "scope": past}),
            literal.clone(),
        ),
        // 5: the like_text limit, in bytes, before the word check and the scope refusal
        (json!({"like_text": sized(16_385)}), long.clone()),
        (
            json!({"like_text": format!("zzqq{}", "é".repeat(8191))}),
            long.clone(),
        ),
        (
            json!({"like_text": sized(16_385), "scope": past}),
            long.clone(),
        ),
        // 6: no word a block holds, before the scope refusal
        (json!({"like_text": "zzqq wwvv"}), unknown_word.clone()),
        (json!({"like_text": ""}), unknown_word.clone()),
        (
            json!({"like_text": "zzqq", "scope": past}),
            unknown_word.clone(),
        ),
        (
            json!({"like_text": "zzqq", "scope": f.proposal}),
            unknown_word.clone(),
        ),
        // 7: an unknown like id, before the scope refusal
        (json!({"like": "c-ZZZZZ"}), unknown_block.clone()),
        (
            json!({"like": "c-ZZZZZ", "scope": past}),
            unknown_block.clone(),
        ),
        (
            json!({"like": "c-ZZZZZ", "scope": f.proposal}),
            unknown_block.clone(),
        ),
        // 8: ranked modes at a past landing and in a proposal version
        (json!({"like": src, "scope": past}), at_past("like")),
        (
            json!({"like": src, "scope": format!("landing:{}", f.past)}),
            at_past("like"),
        ),
        (
            json!({"like_text": "kelp", "scope": past}),
            at_past("like_text"),
        ),
        (
            json!({"query": "kelp", "sort": "relevance", "scope": past}),
            at_past("sort relevance"),
        ),
        (
            json!({"like": src, "scope": f.proposal}),
            at_proposal("like"),
        ),
        (
            json!({"like_text": "kelp", "scope": f.proposal}),
            at_proposal("like_text"),
        ),
        (
            json!({"query": "kelp", "sort": "relevance", "scope": f.proposal}),
            at_proposal("sort relevance"),
        ),
        // an unknown sort value with a query names the three
        (
            json!({"query": "kelp", "sort": "score"}),
            Got::no(
                "E_BAD_INPUT",
                "sort takes coverage, relevance or reading, not score",
                "pass coverage, relevance or reading, or omit sort",
            ),
        ),
    ];
    for (args, want) in cases {
        let what = args.to_string();
        let got = both(&s, &mut c, args);
        assert_eq!(got, want, "{}", &what[..what.len().min(200)]);
    }
    // the limit is inclusive, counted in bytes of multibyte text
    for n in [16_383, 16_384] {
        let text = sized(n);
        let res = both(&s, &mut c, json!({"like_text": text})).ok();
        assert_eq!(res["like_text"].as_str().unwrap().len(), n);
    }
    // with no source at all, MCP refuses the input and the command line keeps its usage error
    assert_eq!(mcp(&mut c, json!({})), one);
    let out = s.rdy(&["search"]);
    assert_eq!(out.status.code(), Some(2), "a usage error");
    let out = s.rdy(&["search", "--scope", "canon"]);
    assert_eq!(out.status.code(), Some(2), "a usage error");
    // a misspelt flag is a usage error that names the nearest flag
    let out = s.rdy(&["search", "--liek", &src]);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--like"), "{err}");
    s.stop();
}

/// Doc `a` with 12 claims and 12 prose blocks and doc `b` with one claim, each holding `walrus
/// pups haul` and its own number, and doc `c` with 40 other blocks, so the shared words are not
/// common.
fn walrus(dir: &std::path::Path) -> BTreeMap<String, String> {
    let mut r = Rdy::open(dir).unwrap();
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "Alpha", "doc_type": "page"}}),
        json!({"op": "create", "type": "doc", "id": "$b", "fields": {"name": "b", "title": "Beta", "doc_type": "page"}}),
        json!({"op": "create", "type": "doc", "id": "$c", "fields": {"name": "c", "title": "Gamma", "doc_type": "page"}}),
        json!({"op": "create", "type": "claim", "id": "$b0", "parent": "$b", "body": "Walrus pups haul out near rock 100."}),
    ];
    for i in 0..12 {
        ops.push(json!({"op": "create", "type": "claim", "parent": "$a",
            "body": format!("Walrus pups haul out near rock {i}.")}));
        ops.push(json!({"op": "create", "type": "prose", "parent": "$a",
            "body": format!("Walrus pups haul in at stone {i}.")}));
    }
    for i in 0..40 {
        ops.push(json!({"op": "create", "type": "prose", "parent": "$c", "body": format!("Filler block {i}.")}));
    }
    let w = write(&mut r, "work", Value::Array(ops)).unwrap();
    land(&mut r, "work").unwrap();
    r.close().unwrap();
    w.created
}

/// The JSON pages of `rdy search <args> --max-tokens 250`, walked to the end; the first cursor.
fn walk(s: &Served, args: &[&str]) -> (Vec<String>, Vec<Value>, String) {
    let mut walked = vec![];
    let mut pages = vec![];
    let mut first = None;
    let mut page: Option<String> = None;
    loop {
        let mut a: Vec<&str> = args.to_vec();
        a.extend(["--max-tokens", "250"]);
        if let Some(p) = &page {
            a.extend(["--page", p.as_str()]);
        }
        let res = cli(s, &a).ok();
        walked.extend(ids(&res));
        let next = res["next"].as_str().map(str::to_string);
        pages.push(res);
        match next {
            Some(n) => {
                first.get_or_insert_with(|| n.clone());
                page = Some(n);
            }
            None => break,
        }
    }
    (walked, pages, first.expect("more than one page"))
}

#[test]
fn vxs8_cli_ranked_pages_keep_filters_and_mode() {
    let (s, mut c, w) = served(walrus);
    let b0 = w["$b0"].clone();
    let modes: Vec<Vec<&str>> = vec![
        vec!["Walrus pups", "--sort", "relevance", "--type", "claim"],
        vec!["--like", &b0, "--type", "claim"],
        vec!["--like-text", "walrus pups haul", "--type", "claim"],
    ];
    let mut cursors = vec![];
    for mode in &modes {
        let whole = cli(&s, mode).ok();
        let all = ids(&whole);
        assert!(all.len() >= 12, "{mode:?}: {all:?}");
        // one small page after another gives the whole list, in order, and only claims
        let (walked, pages, first) = walk(&s, mode);
        assert_eq!(walked, all, "{mode:?}");
        for p in &pages {
            for m in p["matches"].as_array().unwrap() {
                assert_eq!(m["type"], "claim", "{mode:?}: {p}");
                assert!(m["score"].is_number(), "{mode:?}: {p}");
            }
        }
        // the text prints the cursor that the next page takes
        let mut a = mode.clone();
        a.extend(["--max-tokens", "250"]);
        let text = cli_text(&s, &a);
        let last = text.lines().last().unwrap();
        assert_eq!(last, format!("next page: --page {first}"), "{text}");
        // a page asked with the filter dropped is stale
        let unfiltered: Vec<&str> = mode
            .iter()
            .copied()
            .filter(|x| *x != "--type" && *x != "claim")
            .collect();
        let mut a = unfiltered;
        a.extend(["--max-tokens", "250", "--page", &first]);
        let Got::No(code, ..) = cli(&s, &a) else {
            panic!("{mode:?}: the cursor took a dropped filter");
        };
        assert_eq!(code, "E_PAGE_STALE", "{mode:?}");
        cursors.push(first);
    }
    // a cursor pages its own mode only
    for (i, cursor) in cursors.iter().enumerate() {
        for (j, mode) in modes.iter().enumerate() {
            if i == j {
                continue;
            }
            let mut a = mode.clone();
            a.extend(["--max-tokens", "250", "--page", cursor]);
            let Got::No(code, ..) = cli(&s, &a) else {
                panic!("{mode:?} took the cursor of mode {i}");
            };
            assert_eq!(code, "E_PAGE_STALE", "{mode:?} {i}");
        }
    }
    // a CLI cursor pages the same mode through MCP
    let like = json!({"like": b0, "types": ["claim"], "max_tokens": 250, "page": cursors[1]});
    let at_mcp = mcp(&mut c, like).ok();
    let (_, pages, _) = walk(&s, &modes[1]);
    assert_eq!(at_mcp, pages[1]);
    s.stop();
}
