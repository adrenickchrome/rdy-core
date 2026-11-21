//! wck6 (D-N1GG5, D-WV7ZS): every pack-load entry point gives one refusal for one bad pack.
//! Opening a project, reloading its pack and the one-shot compiled read (`rdy pack check`)
//! refuse with the same `E_CONFIG` text and fix, for each refusal of a rule's structure, of its
//! module's decoding, of the pack's module total and of a module that does not compile. The
//! structural read (`project_pack`) compiles no module, so a module that does not compile is
//! not its refusal.
use rdy_engine::{E_CONFIG, Rdy, RdyError, ReloadOutcome, project_pack, project_pack_compiled};
use rdy_qa::fixtures::project;
use rdy_qa::packs::use_pack;
use rdy_schema::{MAX_MODULE_BYTES, MAX_PACK_MODULE_BYTES};

const PASS: &str = include_str!("../../fixtures/wasm/pass.b64");
const INVALID_UNUSED: &str = include_str!("../../fixtures/wasm/invalid-unused.b64");
const IMPORTS: &str = include_str!("../../fixtures/wasm/imports.b64");
const NO_ALLOC: &str = include_str!("../../fixtures/wasm/no-alloc.b64");
const START: &str = include_str!("../../fixtures/wasm/start.b64");
const MEMORY_257: &str = include_str!("../../fixtures/wasm/memory-257.b64");
const TWO_TABLES: &str = include_str!("../../fixtures/wasm/two-tables.b64");
const TABLE_10001: &str = include_str!("../../fixtures/wasm/table-10001.b64");

/// Pack `wck6`, with `rules`.
fn pack(rules: &str) -> String {
    format!("name: wck6\nversion: \"1\"\nextends: rdy-standard\nrules:\n{rules}")
}

/// A rule `r` of `type = section` whose test lines are `test`.
fn rule(test: &str) -> String {
    format!("  - name: r\n    select: type = section\n{test}    message: m\n    fix: f\n")
}

fn module(b64: &str) -> String {
    rule(&format!("    module: {}\n", b64.trim()))
}

/// Where a refusal is found.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Found {
    /// While the pack file parses: the file's path heads the text, and the structural read
    /// refuses too.
    Parse,
    /// While the resolved pack's rules are checked: the structural read refuses too.
    Resolve,
    /// While a module compiles: the structural read does not refuse.
    Compile,
}

/// Every refusal of a pack load, as (rules, detail, where it is found).
fn cases() -> Vec<(String, String, Found)> {
    // 999,999 zero bytes are "AAAA" 333,333 times: nine of them are over the pack's total
    let zeros = "AAAA".repeat(333_333);
    const { assert!(999_999 <= MAX_MODULE_BYTES && 9 * 999_999 > MAX_PACK_MODULE_BYTES) };
    let nine: String = (0..9)
        .map(|k| {
            format!("  - name: r{k}\n    select: type = section\n    module: {zeros}\n    message: m\n    fix: f\n")
        })
        .collect();
    vec![
        (
            rule("    module: null\n"),
            "rule r module: null is not allowed".into(),
            Found::Parse,
        ),
        (
            rule(&format!(
                "    require: type = section\n    module: {}\n",
                PASS.trim()
            )),
            "rule r takes require or module, not both".into(),
            Found::Parse,
        ),
        (
            rule(""),
            "rule r needs require or module".into(),
            Found::Parse,
        ),
        (
            rule("    require: type = section\n    args:\n      k: v\n"),
            "rule r args applies to module rules only".into(),
            Found::Parse,
        ),
        (
            rule("    require: type = section\n    depth: 1\n"),
            "rule r depth applies to module rules only".into(),
            Found::Parse,
        ),
        (
            rule("    module: AAA*\n"),
            "rule r module: not base64".into(),
            Found::Parse,
        ),
        (
            nine,
            format!(
                "the pack's modules hold {} bytes; a pack takes at most {MAX_PACK_MODULE_BYTES}",
                9 * 999_999
            ),
            Found::Resolve,
        ),
        (
            module(INVALID_UNUSED),
            "rule r module: ".into(),
            Found::Compile,
        ),
        (
            module(IMPORTS),
            "rule r module imports env.clock; a check module imports nothing".into(),
            Found::Compile,
        ),
        (
            module(NO_ALLOC),
            "rule r module lacks the export rdy_alloc of type ".into(),
            Found::Compile,
        ),
        (
            module(START),
            "rule r module has a start function; a check module has none".into(),
            Found::Compile,
        ),
        (
            module(MEMORY_257),
            "rule r module memory starts above 256 pages".into(),
            Found::Compile,
        ),
        (
            module(TWO_TABLES),
            "rule r module has more than one table".into(),
            Found::Compile,
        ),
        (
            module(TABLE_10001),
            "rule r module table exceeds 10000 elements".into(),
            Found::Compile,
        ),
    ]
}

fn same(a: &RdyError, b: &RdyError, what: &str) {
    assert_eq!(
        (a.code, &a.message, &a.fix),
        (b.code, &b.message, &b.fix),
        "{what}"
    );
}

#[test]
fn wck6_all_pack_entry_points_return_the_same_refusal() {
    for (rules, detail, found) in cases() {
        let (d, r) = project();
        drop(r);
        // a project whose pack loads, then the bad pack written over it
        use_pack(
            d.path(),
            "wck6",
            &pack(&rule("    require: type = section\n")),
            "",
        );
        let mut loaded = Rdy::open(d.path()).unwrap();
        use_pack(d.path(), "wck6", &pack(&rules), "");
        let file = d.path().join(".rdy/packs/wck6.yaml");
        // open
        let open = Rdy::open(d.path()).err().expect("open refuses");
        assert_eq!(open.code, E_CONFIG, "{detail}: {open:#?}");
        assert_eq!(open.fix, "fix the pack file", "{detail}");
        let text = match found {
            Found::Parse => format!("{}: invalid pack: {detail}", file.display()),
            _ => format!("invalid pack: {detail}"),
        };
        if detail.ends_with(": ") || detail.ends_with("of type ") {
            assert!(
                open.message.starts_with(&text) && open.message.len() > text.len(),
                "{:?} starts with {text:?}",
                open.message
            );
        } else {
            assert_eq!(open.message, text);
        }
        // reload: the loaded pack stays
        let before = loaded.pack().name.clone();
        let ReloadOutcome::Invalid { error, .. } = loaded.reload_pack().unwrap() else {
            panic!("{detail}: the reload refuses");
        };
        same(&error, &open, &format!("{detail}: reload"));
        assert_eq!(loaded.pack().name, before);
        // the one-shot compiled read
        let compiled = project_pack_compiled(d.path())
            .err()
            .expect("compiled read refuses");
        same(
            &compiled,
            &open,
            &format!("{detail}: project_pack_compiled"),
        );
        // the structural read refuses everything but a module that does not compile
        match (found, project_pack(d.path())) {
            (Found::Compile, Ok(p)) => assert_eq!(p.name, "wck6"),
            (Found::Compile, Err(e)) => panic!("{detail}: structural read refused: {e:#?}"),
            (_, Ok(_)) => panic!("{detail}: structural read loaded"),
            (_, Err(e)) => assert_eq!(e.code, E_CONFIG, "{detail}"),
        }
    }
}
