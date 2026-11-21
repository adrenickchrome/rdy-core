//! wck8: the body-required example check module, as its checked-in bytes. The tests need no
//! WebAssembly target: they read the module the QA crate keeps, and run it with the production
//! configuration. The module judges the block's own body and no other text of the input.
use rdy_check_wasm::config::BLOCK_FUEL;
use rdy_check_wasm::{CheckResult, RunFailure, compile_module, run_block};

const MODULE: &[u8] = include_bytes!("../../rdy-qa/fixtures/wasm/body-required.wasm");

fn judge(input: &str) -> Result<CheckResult, RunFailure> {
    let module = compile_module(MODULE).expect("the example loads");
    let out = run_block(&module, input.as_bytes(), BLOCK_FUEL);
    assert!(out.fuel > 0 && out.fuel < BLOCK_FUEL, "fuel {}", out.fuel);
    out.result
}

fn passes(input: &str) -> bool {
    match judge(input) {
        Ok(CheckResult::Pass) => true,
        Ok(CheckResult::Fail {
            message: None,
            fix: None,
        }) => false,
        other => panic!("{other:?} for {input}"),
    }
}

/// An input of interface 1 with `block` as the judged block and `rest` as further top-level
/// members, in byte order of their keys.
fn input(block: &str, neighbours: &str) -> String {
    format!(
        r#"{{"args":{{}},"block":{block},"interface":1,"neighbours":{neighbours},"neighbours_omitted":0,"rule":"r"}}"#
    )
}

#[test]
fn wck8_example_checks_only_the_judged_body() {
    // a body passes; an empty body is left out of the input, or an empty string, and fails
    assert!(passes(&input(
        r#"{"body":"text","id":"B-1","type":"section"}"#,
        "[]"
    )));
    assert!(!passes(&input(r#"{"id":"B-1","type":"section"}"#, "[]")));
    assert!(!passes(&input(
        r#"{"body":"","id":"B-1","type":"section"}"#,
        "[]"
    )));
    // a neighbour's body never stands in for the judged block's, nor does the body of any
    // other member
    let near = r#"[{"body":"near text","hops":1,"id":"B-2","type":"section"}]"#;
    assert!(!passes(&input(r#"{"id":"B-1","type":"section"}"#, near)));
    assert!(passes(&input(r#"{"body":"x","id":"B-1"}"#, near)));
    assert!(!passes(
        r#"{"args":{"block":{"body":"in args"},"body":"top"},"block":{"id":"B-1"},"interface":1,"neighbours":[],"neighbours_omitted":0,"rule":"r"}"#
    ));
    // the text is read by its escapes: a quote, a backslash or a brace inside a string never
    // ends a string or a value early, whatever follows it
    for body in [
        r#"a\"b"#,
        r#"\\"#,
        r#"}\"}{"#,
        r#"\"\\\"body\":\"y"#,
        r#"\u0000"#,
        r#"é😀"#,
    ] {
        let block = format!(r#"{{"body":"{body}","id":"B-1"}}"#);
        assert!(passes(&input(&block, "[]")), "{body}");
    }
    let tricky = r#"{"args":{"k":"\"block\":{\"body\":\"z\"}"},"block":{"id":"}\"{","references":["B-3"]},"interface":1,"neighbours":[{"body":"n","id":"B-2"}],"neighbours_omitted":0,"rule":"r"}"#;
    assert!(!passes(tricky));
    // a key spelt with escapes is the same key
    assert!(passes(r#"{"block":{"body":"x"},"interface":1,"rule":"r"}"#));
    // unknown keys, of every value kind, are ignored, in both objects and in any order
    assert!(passes(
        r#"{"args":{"a":[1,[2,{"b":null}],-9007199254740992,true,false],"z":{}},"block":{"area":["a","b"],"body":"x","depth":0,"doc":"D-1","extra":[[],{}],"id":"B-1","links":{"affects":["B-2"]},"planned":false,"traits":[],"zeta":-17},"future":{"body":""},"interface":1,"neighbours":[],"neighbours_omitted":12,"rule":"r"}"#
    ));
    assert!(passes(r#"{ "block" : { "body" : "x" } }"#));
    assert!(passes("{\n\"block\":\n{\n\"body\":\n\"x\"\n}\n}"));
    // an input that is not the expected shape fails the block with the rule's defaults
    for bad in [
        "",
        "{",
        "[]",
        r#"{"block":[]}"#,
        r#"{"block":{"body":5}}"#,
        r#"{"block":{"body":"x""#,
    ] {
        assert!(!passes(bad), "{bad}");
    }
    // a deeply nested unknown value takes no call depth
    let deep = format!(
        r#"{{"args":{}0{},"block":{{"body":"x"}}}}"#,
        "[".repeat(4000),
        "]".repeat(4000)
    );
    assert!(passes(&deep));
}
