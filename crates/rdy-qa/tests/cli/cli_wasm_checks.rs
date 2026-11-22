//! wck7 (D-ZRXY0, D-WV7ZS, D-N1GG5): the command line of module rules. `rdy pack check` lists
//! each module rule with the SHA-256 of its decoded module, its length and its interface, in
//! text and JSON, and runs no module; it refuses every bad pack with the open's refusal. The
//! propose and approve reports and the card print a module rule's warnings as a filter rule's,
//! with the same rows, fixes and omitted counts as the MCP replies. `rdy check` and the other
//! reads run no module.
use rdy_engine::Rdy;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::land;
use rdy_qa::intake::{TEST_SUMMARY, answer_open_items};
use rdy_qa::mcp::McpClient;
use rdy_qa::packs::use_pack;
use rdy_qa::tensions::{ag, j_typo, put_j};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Output;

const PASS: &str = include_str!("../../fixtures/wasm/pass.b64");
const FAIL_TEXT: &str = include_str!("../../fixtures/wasm/fail-text.b64");
const TRAP: &str = include_str!("../../fixtures/wasm/trap.b64");
const INVALID_UNUSED: &str = include_str!("../../fixtures/wasm/invalid-unused.b64");
const IMPORTS: &str = include_str!("../../fixtures/wasm/imports.b64");
const NO_ALLOC: &str = include_str!("../../fixtures/wasm/no-alloc.b64");
const START: &str = include_str!("../../fixtures/wasm/start.b64");
const MEMORY_257: &str = include_str!("../../fixtures/wasm/memory-257.b64");
const TWO_TABLES: &str = include_str!("../../fixtures/wasm/two-tables.b64");
const TABLE_10001: &str = include_str!("../../fixtures/wasm/table-10001.b64");

/// The decoded bytes of standard base64 `text` (spaces and line breaks ignored).
fn decode(text: &str) -> Vec<u8> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let digits: Vec<u8> = text
        .bytes()
        .filter(|b| !b" \n\r".contains(b))
        .take_while(|b| *b != b'=')
        .map(|b| ALPHABET.iter().position(|a| *a == b).expect("base64") as u8)
        .collect();
    let mut out = vec![];
    for chunk in digits.chunks(4) {
        let mut n = 0u32;
        for (k, d) in chunk.iter().enumerate() {
            n |= u32::from(*d) << (18 - 6 * k);
        }
        let bytes = n.to_be_bytes();
        out.extend_from_slice(&bytes[1..chunk.len()]);
    }
    out
}

fn sha_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// One module rule of claims.
fn mrule(name: &str, module: &str) -> String {
    format!(
        "  - name: {name}\n    select: type = claim\n    module: {}\n    message: {name} message\n    fix: {name} fix\n",
        module.trim()
    )
}

/// Pack `wck7` with `rules`.
fn pack(rules: &str) -> String {
    format!("name: wck7\nversion: \"1\"\nextends: rdy-standard\nrules:\n{rules}")
}

/// A served project of pack `yaml` (named `wck7`) with `extra` config lines.
fn served(yaml: &str, extra: &str) -> Served {
    let (yaml, extra) = (yaml.to_string(), extra.to_string());
    Served::seeded(move |p| {
        drop(Rdy::init(p, "rdy-standard").expect("init"));
        use_pack(p, "wck7", &yaml, &extra);
    })
}

fn out_text(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// The JSON of a command that must succeed.
fn ok_json(s: &Served, args: &[&str]) -> Value {
    let mut all = vec!["--json"];
    all.extend_from_slice(args);
    let (ok, v) = s.json(&all);
    assert!(ok, "rdy {args:?}: {v}");
    v
}

/// The text of a command that must succeed.
fn ok_text(s: &Served, args: &[&str]) -> String {
    let o = s.rdy(args);
    assert!(
        o.status.success(),
        "rdy {args:?}: {}{}",
        out_text(&o),
        String::from_utf8_lossy(&o.stderr)
    );
    out_text(&o)
}

#[test]
fn wck7_pack_check_reports_decoded_digest_and_interface() {
    // two module rules out of name order, one module written over several lines with spaces,
    // and a filter rule
    let pass = PASS.trim();
    let (head, tail) = pass.split_at(pass.len() / 2);
    let rules = format!(
        "  - name: zeta\n    select: type = claim\n    module: {head}\n      {} {}\n    message: m\n    fix: f\n{}  - name: mid-filter\n    select: type = claim\n    require: type = claim\n    message: m\n    fix: f\n",
        &tail[..tail.len() / 2],
        &tail[tail.len() / 2..],
        mrule("alpha", FAIL_TEXT),
    );
    let s = served(&pack(&rules), "");
    let (alpha, zeta) = (decode(FAIL_TEXT), decode(PASS));
    let want = json!([
        {"rule": "alpha", "sha256": sha_hex(&alpha), "bytes": alpha.len(), "interface": 1},
        {"rule": "zeta", "sha256": sha_hex(&zeta), "bytes": zeta.len(), "interface": 1},
    ]);
    let v = ok_json(&s, &["pack", "check"]);
    assert_eq!(v["modules"], want, "{v}");
    assert_eq!(
        v.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["areas", "classes", "modules", "pack", "types", "version"]
    );
    // the text: one line per module rule, in rule name order, after the other lines
    let text = ok_text(&s, &["pack", "check"]);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "pack wck7 1 ok");
    let tail: Vec<String> = lines[lines.len() - 2..]
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        tail,
        [
            format!(
                "rule alpha module sha256:{} {} bytes, interface 1",
                sha_hex(&alpha),
                alpha.len()
            ),
            format!(
                "rule zeta module sha256:{} {} bytes, interface 1",
                sha_hex(&zeta),
                zeta.len()
            ),
        ]
    );
    assert_eq!(
        lines.iter().filter(|l| l.starts_with("rule ")).count(),
        2,
        "a filter rule has no line: {text}"
    );
    // the hex is lowercase
    assert!(
        v["modules"][0]["sha256"]
            .as_str()
            .unwrap()
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    drop(s);
    // a pack with no module rule: an empty list, and no line
    let plain = Served::new();
    let v = ok_json(&plain, &["pack", "check"]);
    assert_eq!(v["modules"], json!([]), "{v}");
    let text = ok_text(&plain, &["pack", "check"]);
    assert!(!text.lines().any(|l| l.starts_with("rule ")), "{text}");
}

/// Every refusal of a pack load, as the rules that give it.
fn refusal_cases() -> Vec<(&'static str, String)> {
    let rule = |test: &str| {
        format!("  - name: r\n    select: type = section\n{test}    message: m\n    fix: f\n")
    };
    let module = |b64: &str| rule(&format!("    module: {}\n", b64.trim()));
    let zeros = "AAAA".repeat(333_333);
    let nine: String = (0..9)
        .map(|k| {
            format!("  - name: r{k}\n    select: type = section\n    module: {zeros}\n    message: m\n    fix: f\n")
        })
        .collect();
    vec![
        ("module_null", rule("    module: null\n")),
        (
            "require_and_module",
            rule(&format!(
                "    require: type = section\n    module: {}\n",
                PASS.trim()
            )),
        ),
        ("no_test", rule("")),
        (
            "args_on_filter_rule",
            rule("    require: type = section\n    args:\n      k: v\n"),
        ),
        (
            "depth_on_filter_rule",
            rule("    require: type = section\n    depth: 1\n"),
        ),
        (
            "depth_too_deep",
            rule(&format!("    module: {}\n    depth: 4\n", PASS.trim())),
        ),
        ("module_not_base64", rule("    module: AAA*\n")),
        ("pack_module_total", nine),
        ("module_invalid", module(INVALID_UNUSED)),
        ("module_imports", module(IMPORTS)),
        ("module_lacks_export", module(NO_ALLOC)),
        ("module_start", module(START)),
        ("module_memory", module(MEMORY_257)),
        ("module_two_tables", module(TWO_TABLES)),
        ("module_table_size", module(TABLE_10001)),
    ]
}

/// `rdy <args>` in `dir` with no daemon, as the human of `env`.
fn no_daemon(dir: &Path, env: &[(String, String)], args: &[&str]) -> Output {
    let mut all = vec!["--no-daemon"];
    all.extend_from_slice(args);
    rdy_qa::daemon::human_command(dir, env, &all)
        .output()
        .expect("run rdy")
}

fn error_json(o: &Output, what: &str) -> Value {
    assert!(!o.status.success(), "{what} refuses: {}", out_text(o));
    let text = String::from_utf8_lossy(&o.stderr);
    serde_json::from_str(text.trim())
        .or_else(|_| serde_json::from_str(text.trim().lines().last().unwrap_or("")))
        .unwrap_or_else(|e| panic!("{what}: {e}: {text}"))
}

#[test]
fn wck7_pack_check_and_open_share_all_load_refusals() {
    let (_cfg, env) = rdy_qa::fixtures::human_env("human:qa");
    for (name, rules) in refusal_cases() {
        let d = tempfile::tempdir().unwrap();
        drop(Rdy::init(d.path(), "rdy-standard").unwrap());
        use_pack(d.path(), "wck7", &pack(&rules), "");
        // the open's refusal
        let open = Rdy::open(d.path()).err().expect("the open refuses");
        let want = json!({"code": open.code, "message": open.message, "fix": open.fix});
        assert_eq!(want["code"], "E_CONFIG", "{name}: {want}");
        assert_eq!(want["fix"], "fix the pack file", "{name}");
        assert!(
            want["message"].as_str().unwrap().contains("invalid pack: "),
            "{name}: {want}"
        );
        // pack check refuses with it, in JSON and in text
        let o = no_daemon(d.path(), &env, &["--json", "pack", "check"]);
        let got = error_json(&o, name);
        for k in ["code", "message", "fix"] {
            assert_eq!(got[k], want[k], "{name}: pack check {k}");
        }
        let o = no_daemon(d.path(), &env, &["pack", "check"]);
        assert!(!o.status.success(), "{name}");
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(
            err.contains(want["message"].as_str().unwrap())
                && err.contains(want["fix"].as_str().unwrap()),
            "{name}: {err}"
        );
        // a command that opens the project refuses with it too
        let o = no_daemon(d.path(), &env, &["--json", "status"]);
        let got = error_json(&o, name);
        for k in ["code", "message", "fix"] {
            assert_eq!(got[k], want[k], "{name}: open {k}");
        }
    }
}

/// A served project of two advisory module rules of claims (one fails with its own text, one
/// traps) and a filter rule that every claim fails, and agent:a's view `a` with 129 claims in a
/// draft doc, its plan items answered: 387 rows, of which the propose keeps 256.
fn many() -> Served {
    let filter = "  - name: d-filter\n    select: type = claim\n    require: type = note\n    message: d-filter message\n    fix: d-filter fix\n";
    let s = served(
        &pack(&(mrule("a-text", FAIL_TEXT) + &mrule("b-traps", TRAP) + filter)),
        "coverage:\n  gate: off\nreviews:\n  require_up_to_date: false\n",
    );
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let w = put_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Ideas"}}
        ]),
        j_typo(),
    );
    land(&mut r, "seed").unwrap();
    let sec = w.created["$s"].clone();
    let ops: Vec<Value> = (0..129)
        .map(|k| json!({"op": "create", "type": "claim", "parent": sec, "body": format!("Claim {k}.")}))
        .collect();
    put_j(&mut r, "a", Value::Array(ops), j_typo());
    answer_open_items(&mut r, &ag("a"), "a");
    r.close().unwrap();
    s
}

/// The rule warnings of a reply, as (block, message, fix).
fn rule_warnings(v: &Value) -> Vec<(String, String, String)> {
    v["warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("warnings: {v}"))
        .iter()
        .filter(|w| w["code"] == "W_RULE_CHECK")
        .map(|w| {
            (
                w["block"].as_str().unwrap().to_string(),
                w["message"].as_str().unwrap().to_string(),
                w["fix"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// The lines `warning W_RULE_CHECK: <message>` and `  fix: <fix>` of each warning.
fn warning_text(rows: &[(String, String, String)]) -> String {
    rows.iter()
        .map(|(_, m, f)| format!("\nwarning W_RULE_CHECK: {m}\n  fix: {f}"))
        .collect()
}

/// The card facts of item `r#ref` in a pending reply, as (name, value).
fn card_facts(v: &Value, r#ref: &str) -> Vec<(String, String)> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["ref"] == r#ref)
        .unwrap_or_else(|| panic!("{ref} in {v}"))["facts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["name"].as_str().unwrap().to_string(),
                f["value"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
fn wck7_rule_text_and_land_warnings_match_mcp() {
    let s = many();
    let agent = Some("a");
    let propose = [
        "propose",
        "--view",
        "a",
        "--title",
        "Claims",
        "--summary",
        TEST_SUMMARY,
    ];
    // the propose report: 256 warnings and 131 left out, in JSON
    let mut all = vec!["--json"];
    all.extend_from_slice(&propose);
    let (ok, v) = s.json_as("agent:a", agent, &all);
    assert!(ok, "{v}");
    let rows = rule_warnings(&v);
    assert_eq!(rows.len(), 256);
    assert_eq!(v["checks_omitted"]["warnings"], 131, "{v}");
    assert_eq!(v["checks_omitted"]["blocking"], 0, "{v}");
    let r#ref = v["ref"].as_str().unwrap().to_string();
    assert!(rows.iter().any(|(b, m, f)| *m
        == format!("rule a-text fails on {b}: the module says no")
        && f == "ask the module"));
    assert!(rows.iter().any(|(b, m, f)| m.starts_with(&format!(
        "rule b-traps could not judge {b}: the module trapped: "
    )) && f
        == "fix the module of rule b-traps in the pack file, or make the rule advisory"));
    // the same rows in the text, each with its fix on the next line
    let o = s.rdy_as("agent:a", agent, &propose);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let text = out_text(&o);
    assert!(text.contains(&warning_text(&rows)), "{text}");
    // the MCP propose gives the same rows and count
    let mut c = McpClient::start_proxy(&s);
    c.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "a", "role": "writer"}),
    )
    .ok();
    let mcp = c
        .call(
            "rdy_propose",
            json!({"view": "a", "title": "Claims", "summary": TEST_SUMMARY}),
        )
        .ok();
    assert_eq!(rule_warnings(&mcp), rows);
    assert_eq!(mcp["checks_omitted"], v["checks_omitted"]);
    // each propose is a new version of the one proposal; the card is the latest version's
    let proposal = |r: &str| r.split_once('@').unwrap().0.to_string();
    let latest = mcp["ref"].as_str().unwrap().to_string();
    assert_eq!(proposal(&latest), proposal(&r#ref), "{mcp}");
    let r#ref = latest;
    // the card: the CLI's facts are the MCP's, with the rows and the omitted count; the text
    // prints the engine's card
    let pending = ok_json(&s, &["pending", "--max-tokens", "100000"]);
    let facts = card_facts(&pending, &r#ref);
    let mut relay = McpClient::start_proxy(&s);
    relay
        .call(
            "rdy_session",
            json!({"actor": "agent:relay", "run": "r1", "role": "writer"}),
        )
        .ok();
    let mcp_pending = relay
        .call("rdy_pending", json!({"max_tokens": 100_000}))
        .ok();
    assert_eq!(card_facts(&mcp_pending, &r#ref), facts);
    let checks: Vec<String> = facts
        .iter()
        .filter(|(n, _)| n == "Rule check")
        .map(|(_, v)| v.clone())
        .collect();
    let want: Vec<String> = rows
        .iter()
        .map(|(_, m, f)| format!("{m}; fix: {f}"))
        .collect();
    assert_eq!(checks, want);
    assert!(
        facts.contains(&(
            "Rule checks omitted".to_string(),
            "0 blocking, 131 advisory".to_string()
        )),
        "{facts:#?}"
    );
    let card = pending["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["ref"] == r#ref.as_str())
        .unwrap()["card"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(ok_text(&s, &["pending", "--max-tokens", "100000"]).contains(&card));
    assert_eq!(
        mcp_pending["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["ref"] == r#ref.as_str())
            .unwrap()["card"],
        card.as_str()
    );
    assert!(card.contains("Rule checks omitted"), "{card}");
    // the kept rows: module rows and filter rows in rule then block order
    let rules: Vec<&str> = rows
        .iter()
        .map(|(_, m, _)| m.split(' ').nth(1).unwrap())
        .collect();
    let mut sorted = rules.clone();
    sorted.sort();
    assert_eq!(rules, sorted);
    assert!(rules.contains(&"a-text") && rules.contains(&"b-traps"));
    // the landing runs no rule, filter or module (D-ZRXY0: never at land): the approval's
    // warnings hold no rule row of either kind, in JSON or in text
    let landed = ok_json(&s, &["approve", &r#ref]);
    assert!(landed["landing"].is_string(), "{landed}");
    assert_eq!(rule_warnings(&landed), vec![], "{landed}");
    assert!(!landed.to_string().contains("W_RULE_CHECK"), "{landed}");
}

#[test]
fn wck7_check_does_not_run_pack_modules() {
    // every claim traps the module: a read that ran it would show a cannot-judge row
    let s = served(
        &pack(&mrule("traps", TRAP)),
        "coverage:\n  gate: off\nreviews:\n  require_up_to_date: false\n",
    );
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let w = put_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Ideas"}},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "C holds."}
        ]),
        j_typo(),
    );
    land(&mut r, "seed").unwrap();
    r.close().unwrap();
    let c = w.created["$c"].clone();
    let no_rows = |what: &str, text: &str| {
        assert!(
            !text.contains("W_RULE_CHECK")
                && !text.contains("E_RULE_CHECK")
                && !text.contains("could not judge")
                && !text.contains("rule traps"),
            "{what} ran a module: {text}"
        );
    };
    // pack check compiles the module and runs none
    let v = ok_json(&s, &["pack", "check"]);
    assert_eq!(v["modules"][0]["rule"], "traps", "{v}");
    // the working copy's lint: no rule row
    ok_text(&s, &["checkout"]);
    let o = s.rdy(&["--json", "check"]);
    assert!(
        o.status.success(),
        "{}{}",
        out_text(&o),
        String::from_utf8_lossy(&o.stderr)
    );
    no_rows("check", &out_text(&o));
    let o = s.rdy(&["check"]);
    no_rows(
        "check text",
        &format!("{}{}", out_text(&o), String::from_utf8_lossy(&o.stderr)),
    );
    // the reads
    for args in [
        vec!["show", c.as_str()],
        vec!["brief", c.as_str()],
        vec!["status"],
        vec!["ls"],
        vec!["coverage"],
    ] {
        no_rows(&format!("{args:?}"), &ok_text(&s, &args));
        no_rows(&format!("{args:?} --json"), &ok_json(&s, &args).to_string());
    }
}
