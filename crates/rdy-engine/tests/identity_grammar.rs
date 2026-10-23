//! The actor and run grammars (D-ME2C0) and the bounded USER fallback (D-Q8NFG). rdy checks
//! every new session and every actor a person or agent names; the grammar is closed, and a
//! refusal echoes a bounded, quoted value. Its own binary: the global allocator below watches
//! the largest single allocation this thread makes, and no other test shares the process.
use rdy_engine::E_BAD_INPUT;
use rdy_engine::identity::{
    MAX_ACTOR_BYTES, MAX_NAME_BYTES, MAX_RUN_BYTES, fallback_actor, is_actor, is_name, is_run,
    validate_actor, validate_run,
};

/// A global allocator that keeps the largest single allocation this thread makes while watched.
mod largest_alloc {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    pub struct Largest;

    thread_local! {
        /// The largest allocation size seen while watching; `None` when not watching.
        static WATCH: Cell<Option<usize>> = const { Cell::new(None) };
    }

    fn note(size: usize) {
        let _ = WATCH.try_with(|w| {
            if let Some(max) = w.get() {
                w.set(Some(max.max(size)));
            }
        });
    }

    // SAFETY: every call goes to `System` with the caller's arguments unchanged.
    #[allow(unsafe_code)]
    unsafe impl GlobalAlloc for Largest {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            note(layout.size());
            // SAFETY: the caller's layout, as `GlobalAlloc::alloc` requires.
            unsafe { System.alloc(layout) }
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            note(layout.size());
            // SAFETY: the caller's layout, as `GlobalAlloc::alloc_zeroed` requires.
            unsafe { System.alloc_zeroed(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: `ptr` came from `System` with `layout` (every allocation here is System's).
            unsafe { System.dealloc(ptr, layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            note(new_size);
            // SAFETY: `ptr` came from `System` with `layout`; the caller's new size.
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }

    /// `f`'s result and the largest single allocation this thread made in it (0 for none).
    pub fn largest<T>(f: impl FnOnce() -> T) -> (T, usize) {
        WATCH.with(|w| w.set(Some(0)));
        let out = f();
        let max = WATCH.with(|w| w.replace(None)).unwrap_or(0);
        (out, max)
    }
}

#[global_allocator]
static ALLOC: largest_alloc::Largest = largest_alloc::Largest;

const HEX32: &str = "0123456789abcdef0123456789abcdef";

const ACTOR_FIX: &str =
    "use human:<name>, agent:<name>, agent:<handle>/<name> or ci:<repo_id>/<name>";
const RUN_FIX: &str = "use 1 to 64 letters, digits, -, _ or ., with a letter or digit at each end; not none, work, canon, inflight or reaffirm-\u{2026}";

fn actor_ok(v: &str) {
    assert!(is_actor(v), "{v:?} should be an actor");
    assert!(validate_actor(v).is_ok(), "{v:?} should validate");
}

fn actor_bad(v: &str) {
    assert!(!is_actor(v), "{v:?} should not be an actor");
    let e = validate_actor(v).expect_err("refused");
    assert_eq!(e.code, E_BAD_INPUT);
}

fn run_ok(v: &str) {
    assert!(is_run(v), "{v:?} should be a run");
    assert!(validate_run(v).is_ok(), "{v:?} should validate");
}

fn run_bad(v: &str) {
    assert!(!is_run(v), "{v:?} should not be a run");
    let e = validate_run(v).expect_err("refused");
    assert_eq!(e.code, E_BAD_INPUT);
}

#[test]
fn id1_actor_grammar_is_closed() {
    assert_eq!(MAX_NAME_BYTES, 64);
    // agent: + 64 + / + 64.
    assert_eq!(MAX_ACTOR_BYTES, "agent:".len() + 64 + 1 + 64);

    // The three kinds; one and two agent names.
    actor_ok("human:a");
    actor_ok("human:nick");
    actor_ok("agent:claude");
    actor_ok("agent:claude/r2d2");
    actor_ok(&format!("ci:{HEX32}/build"));

    // Name lengths 1, 64 and 65, in every name slot.
    let n64 = "a".repeat(64);
    let n65 = "a".repeat(65);
    assert!(is_name("a"));
    assert!(is_name(&n64));
    assert!(!is_name(&n65));
    assert!(!is_name(""));
    actor_ok(&format!("human:{n64}"));
    actor_bad(&format!("human:{n65}"));
    actor_ok(&format!("agent:{n64}"));
    actor_bad(&format!("agent:{n65}"));
    let longest = format!("agent:{n64}/{n64}");
    assert_eq!(longest.len(), MAX_ACTOR_BYTES);
    actor_ok(&longest);
    actor_bad(&format!("agent:{n65}/{n64}"));
    actor_bad(&format!("agent:{n64}/{n65}"));
    actor_ok(&format!("ci:{HEX32}/{n64}"));
    actor_bad(&format!("ci:{HEX32}/{n65}"));

    // The name character set and its ends.
    for good in ["a-b", "a_b", "a.b", "a..b", "0", "9z", "a0-_.9"] {
        assert!(is_name(good), "{good:?}");
        actor_ok(&format!("human:{good}"));
    }
    for bad in [
        "-a", "a-", "_a", "a_", ".a", "a.", "-", ".", "_", "A", "aB", "a b", "a+b",
    ] {
        assert!(!is_name(bad), "{bad:?}");
        actor_bad(&format!("human:{bad}"));
        actor_bad(&format!("agent:{bad}"));
        actor_bad(&format!("agent:x/{bad}"));
        actor_bad(&format!("agent:{bad}/x"));
        actor_bad(&format!("ci:{HEX32}/{bad}"));
    }

    // Repo IDs: exactly 32 lowercase hexadecimal bytes; syntax only.
    actor_bad(&format!("ci:{}/a", &HEX32[..31]));
    actor_bad(&format!("ci:{HEX32}0/a"));
    actor_bad(&format!("ci:{}/a", HEX32.to_uppercase()));
    actor_bad(&format!("ci:{}A/a", &HEX32[..31]));
    actor_bad(&format!("ci:{}g/a", &HEX32[..31]));
    actor_bad(&format!("ci:{HEX32}"));
    actor_bad(&format!("ci:{HEX32}/"));
    actor_bad("ci:/a");
    actor_bad("ci:a");

    // Zero, one and two actor slashes.
    actor_bad("human:a/b");
    actor_bad("agent:a/b/c");
    actor_bad("agent:a//b");
    actor_bad("agent:/a");
    actor_bad("agent:a/");
    actor_bad("agent:/");
    actor_bad(&format!("ci:{HEX32}/a/b"));
    actor_bad(&format!("ci:{HEX32}//a"));

    // Non-ASCII, controls, whitespace, and no or unknown prefix; no trimming or case-folding.
    for bad in [
        "",
        "human:",
        "agent:",
        "ci:",
        "human:\u{e9}",
        "human:caf\u{e9}",
        "agent:\u{e9}/a",
        "human:a\u{0}",
        "human:a\n",
        "human:\ta",
        "human: a",
        "human:a ",
        " human:a",
        "human:a\r",
        "Human:a",
        "HUMAN:a",
        "Agent:a",
        "CI:0123456789abcdef0123456789abcdef/a",
        "user:a",
        "humana",
        "human",
        "agent",
        "a",
        "human::a",
        "human:human:a",
        "bot:a",
        "human:a\u{200b}",
    ] {
        actor_bad(bad);
    }
}

#[test]
fn id1_run_grammar_is_closed() {
    assert_eq!(MAX_RUN_BYTES, 64);
    // Lengths 0, 1, 64 and 65.
    run_bad("");
    run_ok("a");
    run_ok("7");
    run_ok(&"r".repeat(64));
    run_bad(&"r".repeat(65));

    // Upper case is allowed in a run, unlike a name.
    run_ok("R7");
    run_ok("Build-42");
    run_ok("a.B_c-D");
    run_ok("cli");

    // Punctuation ends, other characters, and no trimming.
    for bad in [
        "-a", "a-", "_a", "a_", ".a", "a.", "-", ".", "a/b", "a b", " a", "a ", "a\n", "a+b",
        "r\u{e9}", "a:b", "\u{0}",
    ] {
        run_bad(bad);
    }

    // Every reserved exact name, and every run beginning reaffirm-; case-sensitive.
    for reserved in ["none", "canon", "inflight", "work"] {
        run_bad(reserved);
    }
    for reserved in ["reaffirm-a", "reaffirm-1", "reaffirm-x.y", "reaffirm--a"] {
        run_bad(reserved);
    }
    run_bad(&format!("reaffirm-{}", "a".repeat(55)));
    for accepted in [
        "None",
        "NONE",
        "Canon",
        "Inflight",
        "Work",
        "works",
        "nonee",
        "canon1",
        "reaffirm",
        "Reaffirm-a",
        "reaffirmed",
        "reaffirm_a",
        "reaffirm.a",
        "xreaffirm-a",
    ] {
        run_ok(accepted);
    }
}

#[test]
fn id1_bad_identity_has_the_decided_text() {
    // Exact code, message and fix.
    let e = validate_actor("user:nick").unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(
        e.message,
        r#"the actor "user:nick" is not in rdy's actor form"#
    );
    assert_eq!(e.fix, ACTOR_FIX);
    let e = validate_run("none").unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(e.message, r#"the run "none" is not in rdy's run form"#);
    assert_eq!(e.fix, RUN_FIX);

    // An embedded quote and newline are escaped.
    let e = validate_actor("human:a\"b\nc").unwrap_err();
    assert_eq!(
        e.message,
        r#"the actor "human:a\"b\nc" is not in rdy's actor form"#
    );
    let e = validate_run("a\"b\nc").unwrap_err();
    assert_eq!(e.message, r#"the run "a\"b\nc" is not in rdy's run form"#);

    // 80 scalars echo whole; 81 echo the first 80 and an ellipsis.
    let e80 = "\u{e9}".repeat(80);
    let e = validate_actor(&e80).unwrap_err();
    assert_eq!(
        e.message,
        format!("the actor \"{e80}\" is not in rdy's actor form")
    );
    let e = validate_run(&e80).unwrap_err();
    assert_eq!(
        e.message,
        format!("the run \"{e80}\" is not in rdy's run form")
    );
    let e81 = "\u{e9}".repeat(81);
    let e = validate_actor(&e81).unwrap_err();
    assert_eq!(
        e.message,
        format!("the actor \"{e80}\u{2026}\" is not in rdy's actor form")
    );
    let e = validate_run(&e81).unwrap_err();
    assert_eq!(
        e.message,
        format!("the run \"{e80}\u{2026}\" is not in rdy's run form")
    );

    // A 1,048,576-byte input: the full hostile input never appears in an error.
    let mut hostile = String::from("human:");
    hostile.push_str(&"a\"\n".repeat((1_048_576 - hostile.len()) / 3));
    while hostile.len() < 1_048_576 {
        hostile.push('a');
    }
    assert_eq!(hostile.len(), 1_048_576);
    // The first 80 scalars: human: then 24 triples of a, quote, newline, then a and a quote.
    let head = format!("human:{}a\\\"", "a\\\"\\n".repeat(24));
    for e in [
        validate_actor(&hostile).unwrap_err(),
        validate_run(&hostile).unwrap_err(),
    ] {
        assert_eq!(e.code, E_BAD_INPUT);
        assert!(
            e.message.contains(&format!("\"{head}\u{2026}\"")),
            "{}",
            e.message
        );
        assert!(e.message.len() < 400, "{} bytes", e.message.len());
        assert!(!e.message.contains(&hostile));
        assert!(!e.fix.contains("aaaa"));
    }
    let long_run = "r".repeat(1_048_576);
    let e = validate_run(&long_run).unwrap_err();
    assert_eq!(
        e.message,
        format!(
            "the run \"{}\u{2026}\" is not in rdy's run form",
            "r".repeat(80)
        )
    );
    let long_actor = format!("human:{}", "a".repeat(1_048_570));
    let e = validate_actor(&long_actor).unwrap_err();
    assert_eq!(
        e.message,
        format!(
            "the actor \"human:{}\u{2026}\" is not in rdy's actor form",
            "a".repeat(74)
        )
    );
}

/// The fallback as D-Q8NFG states it, written plainly and unbounded: the model the bounded
/// implementation must match.
fn reference_fallback(user: Option<&str>) -> String {
    let mut norm = String::new();
    for c in user.unwrap_or("").chars() {
        let c = c.to_ascii_lowercase();
        let c = if c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.') {
            c
        } else {
            '-'
        };
        if c == '-' && norm.ends_with('-') {
            continue;
        }
        norm.push(c);
    }
    norm.truncate(64);
    let name = norm.trim_matches(|c| matches!(c, '-' | '_' | '.'));
    if name.is_empty() {
        "human:unknown".to_string()
    } else {
        format!("human:{name}")
    }
}

fn fallback_checked(user: Option<&str>) -> String {
    let out = fallback_actor(user);
    assert!(is_actor(&out), "{user:?} -> {out:?}");
    assert!(out.len() <= 70, "{user:?} -> {} bytes", out.len());
    assert!(out.starts_with("human:"), "{out:?}");
    assert_eq!(out, reference_fallback(user), "{user:?}");
    out
}

#[test]
fn id1_fallback_always_forms_a_human_actor() {
    let a63 = "a".repeat(63);
    let a61 = "a".repeat(61);
    let a64 = "a".repeat(64);
    // The decided examples.
    assert_eq!(fallback_checked(Some("AL Ice__..")), "human:al-ice");
    assert_eq!(fallback_checked(Some("\u{c9}")), "human:unknown");
    assert_eq!(fallback_checked(Some("A..B")), "human:a..b");
    assert_eq!(
        fallback_checked(Some(&format!("{a63}-b"))),
        format!("human:{a63}")
    );
    assert_eq!(
        fallback_checked(Some(&format!("___{a64}"))),
        format!("human:{a61}")
    );
    // Empty and absent USER.
    assert_eq!(fallback_checked(Some("")), "human:unknown");
    assert_eq!(fallback_checked(None), "human:unknown");
    // Punctuation only.
    for p in [
        "-",
        "---",
        "_._",
        "...",
        " ",
        "/",
        "!@#",
        "\u{e9}\u{e9}",
        "- _ .",
    ] {
        assert_eq!(fallback_checked(Some(p)), "human:unknown", "{p:?}");
    }
    // Repeated hyphens fold, from the input and from replaced scalars alike.
    assert_eq!(fallback_checked(Some("a--b")), "human:a-b");
    assert_eq!(fallback_checked(Some("a - b")), "human:a-b");
    assert_eq!(fallback_checked(Some("a  /  b")), "human:a-b");
    assert_eq!(fallback_checked(Some("a-_-b")), "human:a-_-b");
    // Multibyte input: one hyphen per scalar, then folded.
    assert_eq!(fallback_checked(Some("Jos\u{e9}")), "human:jos");
    assert_eq!(
        fallback_checked(Some("Jos\u{e9} Mar\u{ed}a")),
        "human:jos-mar-a"
    );
    assert_eq!(fallback_checked(Some("\u{1f600}x\u{1f600}")), "human:x");
    assert_eq!(fallback_checked(Some("\u{4e2d}\u{6587}nick")), "human:nick");
    // Truncation that lands on punctuation trims after the cut.
    assert_eq!(
        fallback_checked(Some(&format!("{a63}.zzz"))),
        format!("human:{a63}")
    );
    assert_eq!(
        fallback_checked(Some(&format!("{a63}\u{e9}zzz"))),
        format!("human:{a63}")
    );
    assert_eq!(fallback_checked(Some(&a64)), format!("human:{a64}"));
    assert_eq!(
        fallback_checked(Some(&format!("{a64}b"))),
        format!("human:{a64}")
    );
    // Leading punctuation counts toward the cut; later input cannot change the prefix.
    assert_eq!(
        fallback_checked(Some(&format!("-{a64}"))),
        format!("human:{a63}")
    );
    assert_eq!(
        fallback_checked(Some(&format!("{}{}", "-".repeat(10), a64))),
        format!("human:{a63}")
    );
    assert_eq!(
        fallback_checked(Some(&format!("{}{}", "\u{e9}".repeat(10), a64))),
        format!("human:{a63}")
    );
    assert_eq!(
        fallback_checked(Some(&format!("{}{}", "_".repeat(64), a64))),
        "human:unknown"
    );

    // Exhaustive short strings over a A 0 - _ . space / é.
    let alphabet = ['a', 'A', '0', '-', '_', '.', ' ', '/', '\u{e9}'];
    let mut n = 0usize;
    let mut buf = String::new();
    fn walk(alphabet: &[char], depth: usize, buf: &mut String, n: &mut usize) {
        fallback_checked(Some(buf));
        *n += 1;
        if depth == 0 {
            return;
        }
        for &c in alphabet {
            buf.push(c);
            walk(alphabet, depth - 1, buf, n);
            buf.pop();
        }
    }
    walk(&alphabet, 5, &mut buf, &mut n);
    assert_eq!(n, 1 + 9 + 81 + 729 + 6561 + 59049);
}

#[test]
fn id1_fallback_does_not_allocate_for_the_whole_user() {
    // Built before the counter watches.
    let user = format!("{}{}", "Ab-".repeat(349_525), "c");
    assert_eq!(user.len(), 1_048_576);
    let wide = "\u{e9}".repeat(524_288);
    assert_eq!(wide.len(), 1_048_576);
    let tail_heavy = format!("{}{}", "_".repeat(1_048_512), "a".repeat(64));
    assert_eq!(tail_heavy.len(), 1_048_576);
    for u in [&user, &wide, &tail_heavy] {
        let (out, largest) = largest_alloc::largest(|| fallback_actor(Some(u)));
        assert!(largest <= 70, "an allocation of {largest} bytes");
        assert!(is_actor(&out), "{out:?}");
        assert!(out.len() <= 70);
    }
    // The prefix alone decides the result.
    assert_eq!(
        fallback_actor(Some(&user)),
        reference_fallback(Some(&user[..200]))
    );
    assert_eq!(fallback_actor(Some(&wide)), "human:unknown");
    assert_eq!(fallback_actor(Some(&tail_heavy)), "human:unknown");
}
