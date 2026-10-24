//! Load stop selectors and the dump chain error contract.
use rdy_engine::dump_error as de;
use rdy_engine::dump_stop::{DumpStop, MAX_CHAIN_FILES, MAX_STOP_BYTES, MAX_TAG_BYTES};
use rdy_engine::{
    E_BAD_INPUT, E_DUMP_CHAIN, E_LIMIT, E_LOAD_STOP, E_REDACTED_STATE, REDACTED_STATE_FIX, RdyError,
};
use rdy_model::{Hash, RepoId};
use rdy_store::StoreError;
use std::path::Path;

const SYNTAX_FIX: &str = "Use landing:<hash>, tag:<name>, or pos:<segment>.<offset>.";

fn syntax(value: &str) -> (String, String) {
    (
        format!("Invalid load stop {value:?}."),
        SYNTAX_FIX.to_string(),
    )
}

fn bad(input: &str) {
    let e = DumpStop::parse(input).expect_err(input);
    let (message, fix) = syntax(input);
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (E_BAD_INPUT, message.as_str(), fix.as_str()),
        "{input:?}"
    );
}

fn pos(segment: u32, offset: u64) -> DumpStop {
    DumpStop::Position { segment, offset }
}

#[test]
fn hc2_stop_grammar_is_closed() {
    let h64 = "ab".repeat(32);
    let h6 = "abcdef";
    let h65 = format!("{h64}0");
    let h5 = "abcde";

    // landings: 6..=64 hex digits, lowercased
    assert_eq!(
        DumpStop::parse(&format!("landing:{h6}")).unwrap(),
        DumpStop::Landing(h6.into())
    );
    assert_eq!(
        DumpStop::parse(&format!("landing:{h64}")).unwrap(),
        DumpStop::Landing(h64.clone())
    );
    assert_eq!(
        DumpStop::parse("landing:ABCDEF0123").unwrap(),
        DumpStop::Landing("abcdef0123".into())
    );
    assert_eq!(
        DumpStop::parse("landing:AbCdEf").unwrap(),
        DumpStop::Landing("abcdef".into())
    );
    bad(&format!("landing:{h5}"));
    bad(&format!("landing:{h65}"));
    bad("landing:");
    bad("landing:abcdeg");
    bad("landing: abcdef");
    bad("landing:abcdef ");
    bad("landing:abc def");
    bad("landing:0xabcdef");
    bad("landing:\u{ff21}\u{ff22}\u{ff23}\u{ff24}\u{ff25}\u{ff26}");

    // tags: exact nonempty suffix
    for tag in [
        "v1",
        "1.2.3",
        " v1",
        "v1 ",
        "a b",
        "Tag:Mixed",
        "\u{e9}t\u{e9}",
        "tag:",
        "landing:abcdef",
        "pos:1.2",
        "\n",
    ] {
        assert_eq!(
            DumpStop::parse(&format!("tag:{tag}")).unwrap(),
            DumpStop::Tag(tag.into()),
            "{tag:?}"
        );
    }
    bad("tag:");
    let max_tag = "t".repeat(MAX_TAG_BYTES);
    assert_eq!(
        DumpStop::parse(&format!("tag:{max_tag}")).unwrap(),
        DumpStop::Tag(max_tag)
    );

    // positions: two decimal sequences, one dot
    assert_eq!(DumpStop::parse("pos:0.0").unwrap(), pos(0, 0));
    assert_eq!(DumpStop::parse("pos:3.4096").unwrap(), pos(3, 4096));
    assert_eq!(DumpStop::parse("pos:007.0042").unwrap(), pos(7, 42));
    assert_eq!(
        DumpStop::parse("pos:4294967295.18446744073709551615").unwrap(),
        pos(u32::MAX, u64::MAX)
    );
    let zeros = "0".repeat(1000);
    assert_eq!(
        DumpStop::parse(&format!("pos:{zeros}5.{zeros}9")).unwrap(),
        pos(5, 9)
    );
    bad("pos:4294967296.0");
    bad("pos:0.18446744073709551616");
    bad(&format!("pos:{}.0", "9".repeat(40)));
    bad("pos:");
    bad("pos:1");
    bad("pos:1.");
    bad("pos:.1");
    bad("pos:.");
    bad("pos:1.2.3");
    bad("pos:1..2");
    bad("pos:+1.2");
    bad("pos:1.+2");
    bad("pos:-1.2");
    bad("pos:1.-2");
    bad("pos: 1.2");
    bad("pos:1 .2");
    bad("pos:1. 2");
    bad("pos:1.2 ");
    bad("pos:1,2");
    bad("pos:0x1.2");
    bad("pos:1e3.2");
    bad("pos:\u{661}.2");

    // prefixes are lowercase and exact
    for text in [
        "",
        "x",
        "landing",
        "tag",
        "pos",
        "Landing:abcdef",
        "LANDING:abcdef",
        "Tag:v1",
        "TAG:v1",
        "Pos:1.2",
        "POS:1.2",
        " tag:v1",
        "tag :v1",
        "tags:v1",
        "landings:abcdef",
        "abcdef",
        "1.2",
        "v1",
        ":",
    ] {
        bad(text);
    }
}

#[test]
fn hc2_stop_size_limit_precedes_parse() {
    assert_eq!(MAX_STOP_BYTES, 1_048_580);
    assert_eq!(MAX_TAG_BYTES, 1_048_576);
    // N bytes parse; N+1 is E_LIMIT whatever the prefix says
    let at = format!("tag:{}", "x".repeat(MAX_TAG_BYTES));
    assert_eq!(at.len(), MAX_STOP_BYTES);
    assert!(DumpStop::parse(&at).is_ok());
    let over = format!("{at}x");
    let message = "The load stop exceeds the limit of 1048580 bytes.";
    let fix = "Use a shorter landing, tag, or position selector.";
    let e = DumpStop::parse(&over).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (E_LIMIT, message, fix)
    );
    // the limit wins over a syntax error
    for prefix in ["", "bogus:", "landing:", "pos:", "Tag:"] {
        let text = format!("{prefix}{}", "z".repeat(MAX_STOP_BYTES + 1 - prefix.len()));
        let e = DumpStop::parse(&text).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (E_LIMIT, message, fix),
            "{prefix:?}"
        );
    }
    // a multi-byte tag is counted in bytes
    let two = "\u{e9}".repeat(MAX_TAG_BYTES / 2);
    assert!(DumpStop::parse(&format!("tag:{two}")).is_ok());
    let e = DumpStop::parse(&format!("tag:{two}\u{e9}")).unwrap_err();
    assert_eq!(e.code, E_LIMIT);
    // an overlong position or landing is E_LIMIT too, never a parse error
    let e = DumpStop::parse(&format!("pos:{}", "1".repeat(MAX_STOP_BYTES))).unwrap_err();
    assert_eq!(e.code, E_LIMIT);
}

fn hash(b: u8) -> Hash {
    Hash([b; 32])
}

fn repo(b: u8) -> RepoId {
    RepoId([b; 16])
}

fn check(e: RdyError, code: &str, message: &str, fix: &str) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (code, message, fix)
    );
    assert!(e.issues.is_empty());
    assert!(e.corrected.is_none() && e.briefing.is_none());
}

#[test]
fn hc2_chain_errors_match_every_documented_row() {
    // the chain-length bound
    assert_eq!(MAX_CHAIN_FILES, 4096);
    check(
        de::chain_too_long(),
        E_LIMIT,
        "The dump chain exceeds the limit of 4096 increments.",
        "Load from a newer full dump of the store.",
    );

    // load rows
    let load_fix = "pass the full dump, then each increment in chain order";
    let f = Path::new("a/b.dump");
    check(
        de::first_is_increment(f),
        E_DUMP_CHAIN,
        "a/b.dump: it is an increment; a dump chain starts with a full dump",
        load_fix,
    );
    check(
        de::later_is_full(f),
        E_DUMP_CHAIN,
        "a/b.dump: it is a full dump; only the first file of a dump chain is one",
        load_fix,
    );
    check(
        de::load_legacy_file(f, 2),
        E_DUMP_CHAIN,
        "a/b.dump: dump format 2 holds no chain facts; a dump chain starts with a full dump of format 3",
        load_fix,
    );
    check(
        de::load_repo_mismatch(f, &repo(0xab), &repo(0x01)),
        E_DUMP_CHAIN,
        &format!(
            "a/b.dump holds repo {}, but the chain holds repo {}",
            "ab".repeat(16),
            "01".repeat(16)
        ),
        load_fix,
    );
    check(
        de::load_store_mismatch(
            f,
            &rdy_store::format::DumpStore::Legacy(16),
            &rdy_store::format::DumpStore::Legacy(17),
        ),
        E_DUMP_CHAIN,
        "a/b.dump is of store format 16, but the chain is of store format 17",
        load_fix,
    );
    check(
        de::load_position_mismatch(f, 3, 4096, 2, 77),
        E_DUMP_CHAIN,
        "a/b.dump starts at 3.4096, but the file before it ends at 2.77",
        load_fix,
    );
    check(
        de::load_marker_mismatch(f),
        E_DUMP_CHAIN,
        "a/b.dump follows a batch that differs from the last batch of the file before it",
        load_fix,
    );
    check(
        de::load_digest_mismatch(f, &hash(0xcd), &hash(0x0e)),
        E_DUMP_CHAIN,
        &format!(
            "a/b.dump follows the file with digest {}, but the file before it has digest {}",
            "cd".repeat(32),
            "0e".repeat(32)
        ),
        load_fix,
    );

    // export rows; the adapter has no path and says `previous dump`
    let export_fix =
        "follow the newest file of this store's chain, or start a new chain with rdy dump";
    for (file, text) in [(Some(f), "a/b.dump"), (None, "previous dump")] {
        check(
            de::export_repo_mismatch(file, &repo(0xab), &repo(0x01)),
            E_DUMP_CHAIN,
            &format!(
                "{text} holds repo {}, but this project is repo {}",
                "ab".repeat(16),
                "01".repeat(16)
            ),
            export_fix,
        );
        check(
            de::export_store_mismatch(
                file,
                &rdy_store::format::DumpStore::Legacy(15),
                &rdy_store::format::DumpStore::Legacy(16),
            ),
            E_DUMP_CHAIN,
            &format!("{text} is of store format 15, but this store is of store format 16"),
            export_fix,
        );
        check(
            de::export_end_past_state(file, 9, 100),
            E_DUMP_CHAIN,
            &format!("{text} ends at 9.100, after the end of this store"),
            export_fix,
        );
        check(
            de::export_end_inside_batch(file, 0, 12),
            E_DUMP_CHAIN,
            &format!("{text} ends at 0.12, which is not the end of a batch in this store"),
            export_fix,
        );
        check(
            de::export_marker_mismatch(file, 1, 2),
            E_DUMP_CHAIN,
            &format!("the batch of this store at 1.2 differs from the last batch of {text}"),
            export_fix,
        );
        check(
            de::export_legacy_predecessor(file, 1),
            E_DUMP_CHAIN,
            &format!("{text}: dump format 1 holds no chain facts; dump the project again"),
            export_fix,
        );
    }

    // stop rows
    let stop_fix = "name a landing, a tag or a batch end that the dump chain holds";
    check(
        de::stop_no_landing("abcdef"),
        E_LOAD_STOP,
        "the dump chain holds no landing abcdef",
        stop_fix,
    );
    check(
        de::stop_no_tag("v 1"),
        E_LOAD_STOP,
        "the dump chain holds no tag v 1",
        stop_fix,
    );
    check(
        de::stop_ambiguous_landing("abcdef"),
        E_LOAD_STOP,
        "abcdef names more than one landing in the dump chain",
        stop_fix,
    );
    check(
        de::stop_not_batch_end(2, 5),
        E_LOAD_STOP,
        "2.5 is not the end of a batch in the dump chain",
        stop_fix,
    );
    check(
        de::stop_legacy_file(f, 1),
        E_LOAD_STOP,
        "a/b.dump: dump format 1 holds no chain facts; a stop needs dump format 3",
        stop_fix,
    );
}

#[test]
fn hc2_epoch_fix_names_retrying_dump() {
    assert_eq!(
        REDACTED_STATE_FIX,
        "Read the project again, or run the dump again."
    );
    let e = RdyError::from(StoreError::RedactedState);
    assert_eq!(e.code, E_REDACTED_STATE);
    assert_eq!(e.message, "This read state predates content removal.");
    assert_eq!(e.fix, "Read the project again, or run the dump again.");
}
