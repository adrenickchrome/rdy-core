//! dgm1: the pinned diagram crate behind one module. Each fixture renders to the exact bytes of
//! its golden on every host, in every process, every time. Std only.

use rdy_diagram::{Diagram, encode_label, render};
use std::path::PathBuf;
use std::process::Command;

/// The drawn fixtures: `<name>.mmd` renders to `<name>.svg`.
const DRAWN: [&str; 15] = [
    "loop",
    "fan-in",
    "round-trip",
    "replay",
    "four-panels",
    "fan-out",
    "chain",
    "graph",
    "class",
    "sequence",
    "state",
    "labels",
    "routing",
    "diagonal-marker",
    "document-wave",
];

/// The fixtures that draw nothing: `<name>.mmd` renders to the result in `<name>.json`.
const UNDRAWN: [&str; 2] = ["unsupported", "invalid"];

/// The headers rdy recognizes but does not draw, matched with their exact case.
const UNDRAWN_EXACT: [&str; 38] = [
    "C4Context",
    "C4Container",
    "C4Component",
    "C4Dynamic",
    "C4Deployment",
    "erDiagram",
    "gantt",
    "gitGraph",
    "journey",
    "pie",
    "info",
    "quadrantChart",
    "requirement",
    "requirementDiagram",
    "mindmap",
    "timeline",
    "zenuml",
    "sankey",
    "sankey-beta",
    "xychart",
    "xychart-beta",
    "block",
    "block-beta",
    "packet",
    "packet-beta",
    "kanban",
    "architecture",
    "architecture-beta",
    "radar-beta",
    "treemap",
    "treemap-beta",
    "swimlane-beta",
    "treeView-beta",
    "eventmodeling",
    "venn-beta",
    "cynefin-beta",
    "classDiagram-v2",
    "flowchart-elk",
];

/// The headers rdy recognizes but does not draw, matched in any case.
const UNDRAWN_ANY_CASE: [&str; 7] = [
    "ishikawa",
    "ishikawa-beta",
    "wardley-beta",
    "railroad-beta",
    "railroad-ebnf-beta",
    "railroad-abnf-beta",
    "railroad-peg-beta",
];

const FRESH_PROCESSES: usize = 20;
const CHILD_TEST: &str = "dgm1_fresh_process_renders_one_fixture";
const CHILD_FIXTURE: &str = "RDY_DIAGRAM_QA_FIXTURE";
const CHILD_OUT: &str = "RDY_DIAGRAM_QA_OUT";

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/diagram")
}

fn source(name: &str) -> String {
    let path = fixtures().join(format!("{name}.mmd"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn golden(name: &str) -> Vec<u8> {
    let ext = if UNDRAWN.contains(&name) {
        "json"
    } else {
        "svg"
    };
    let path = fixtures().join(format!("{name}.{ext}"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn json_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A result as golden bytes: the SVG itself, else a small JSON object.
fn as_bytes(d: &Diagram) -> Vec<u8> {
    let (kind, key, value) = match d {
        Diagram::Svg(svg) => return svg.as_bytes().to_vec(),
        Diagram::Unsupported { header } => ("unsupported", "header", header),
        Diagram::Invalid { reason } => ("invalid", "reason", reason),
        Diagram::Failed { reason } => ("failed", "reason", reason),
    };
    format!(
        "{{\n  \"kind\": \"{kind}\",\n  \"{key}\": {}\n}}\n",
        json_string(value)
    )
    .into_bytes()
}

fn svg(source: &str) -> String {
    match render(source) {
        Diagram::Svg(svg) => svg,
        other => panic!("not drawn: {other:?}\n{source}"),
    }
}

fn invalid(reason: &str) -> Diagram {
    Diagram::Invalid {
        reason: reason.to_string(),
    }
}

fn unsupported(header: &str) -> Diagram {
    Diagram::Unsupported {
        header: header.to_string(),
    }
}

const OVER_BYTES: &str = "the source is over 65536 bytes";
const OVER_STATEMENTS: &str = "the source has over 2048 statements";
const OVER_DEPTH: &str = "the source nests subgraphs or blocks over 32 deep";
const UNKNOWN: &str = "unknown diagram type";
const UNCLOSED: &str = "a subgraph is not closed with end";
const OVER_ARROW: &str = "the source has an arrow over 256 bytes";

/// The child half of the fresh-process check: renders the fixture the parent names and writes
/// the golden form where the parent says. Without the parent's variables it does nothing.
#[test]
fn dgm1_fresh_process_renders_one_fixture() {
    let (Some(name), Some(out)) = (std::env::var_os(CHILD_FIXTURE), std::env::var_os(CHILD_OUT))
    else {
        return;
    };
    let name = name.into_string().expect("fixture name");
    std::fs::write(out, as_bytes(&render(&source(&name)))).expect("write the result");
}

#[test]
fn dgm1_same_input_is_the_same_bytes() {
    let all: Vec<&str> = DRAWN.iter().chain(UNDRAWN.iter()).copied().collect();
    for name in &all {
        let src = source(name);
        let want = golden(name);
        let first = render(&src);
        let second = render(&src);
        assert_eq!(first, second, "{name}: two renders in one process");
        assert!(
            as_bytes(&first) == want,
            "{name}: differs from its golden\n{}",
            String::from_utf8_lossy(&as_bytes(&first))
        );
        let drawn = matches!(first, Diagram::Svg(_));
        assert_eq!(drawn, DRAWN.contains(name), "{name}: {first:?}");
    }
    let exe = std::env::current_exe().expect("the test binary");
    let dir = std::env::temp_dir().join(format!("rdy-diagram-dgm1-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    for name in &all {
        let want = golden(name);
        for i in 0..FRESH_PROCESSES {
            let out = dir.join(format!("{name}-{i}.out"));
            let status = Command::new(&exe)
                .args([CHILD_TEST, "--exact", "--test-threads=1", "--quiet"])
                .env(CHILD_FIXTURE, name)
                .env(CHILD_OUT, &out)
                .stdout(std::process::Stdio::null())
                .status()
                .expect("run a fresh process");
            assert!(status.success(), "{name}: process {i} failed");
            let got = std::fs::read(&out).expect("the child's result");
            assert!(got == want, "{name}: process {i} differs from its golden");
            std::fs::remove_file(&out).expect("remove the result");
        }
    }
    std::fs::remove_dir_all(&dir).expect("remove the scratch dir");
}

#[test]
fn dgm1_all_drawn_types_and_graph_alias() {
    for (header, body) in [
        ("flowchart LR", "\n    A --> B\n"),
        ("graph LR", "\n    A --> B\n"),
        ("classDiagram", "\n    A --> B\n"),
        ("sequenceDiagram", "\n    A->>B: first\n"),
        ("stateDiagram-v2", "\n    [*] --> A\n    A --> B\n"),
        ("stateDiagram", "\n    [*] --> A\n    A --> B\n"),
    ] {
        let out = svg(&format!("{header}{body}"));
        assert!(out.starts_with("<svg"), "{header}: {out}");
        assert!(out.trim_end().ends_with("</svg>"), "{header}");
    }
    // the graph alias draws what flowchart draws
    assert_eq!(
        svg("graph LR\n    A --> B\n"),
        svg("flowchart LR\n    A --> B\n")
    );
    // detection is case-insensitive and reads past comments and front matter
    let plain = svg("flowchart LR\n    A --> B\n");
    assert_eq!(svg("%% a comment\n\nflowchart LR\n    A --> B\n"), plain);
    assert!(svg("---\ntitle: first\n---\nflowchart LR\n    A --> B\n").starts_with("<svg"));
    assert_eq!(
        render("FLOWCHART LR\n    A --> B\n"),
        Diagram::Svg(plain.clone()),
        "uppercase drawn spelling"
    );
    // one leading byte-order mark is not part of the source
    assert_eq!(svg("\u{feff}flowchart LR\n    A --> B\n"), plain);
}

#[test]
fn dgm1_unsupported_and_invalid_are_distinct() {
    let broken = "\n    {{{ ]]] -->\n    \"unterminated\n    end end end\n";
    for header in UNDRAWN_EXACT {
        assert_eq!(
            render(&format!("{header}{broken}")),
            unsupported(header),
            "{header}: a broken body under an undrawn header"
        );
        assert_eq!(render(header), unsupported(header), "{header} alone");
    }
    for header in UNDRAWN_ANY_CASE {
        let upper = header.to_ascii_uppercase();
        let mixed: String = header
            .chars()
            .enumerate()
            .map(|(i, c)| {
                if i % 2 == 0 {
                    c.to_ascii_uppercase()
                } else {
                    c
                }
            })
            .collect();
        for spelling in [header.to_string(), upper, mixed] {
            assert_eq!(
                render(&format!("{spelling}{broken}")),
                unsupported(&spelling),
                "keeps the source spelling"
            );
        }
    }
    // the exact-case set is exact: another case is not recognized, so mmdflux refuses it
    for header in ["Pie", "GANTT", "c4context", "erdiagram", "Zenuml"] {
        assert_eq!(
            render(&format!("{header}\n    A\n")),
            invalid(UNKNOWN),
            "{header}"
        );
    }
    // the header token is letters, digits and hyphens, after blank and comment lines
    assert_eq!(
        render("\n%% c\n\n   gantt\n    title A\n"),
        unsupported("gantt")
    );
    assert_eq!(render("gitGraph:\n    commit\n"), unsupported("gitGraph"));
    assert_eq!(render("pie title A\n"), unsupported("pie"));
    // front matter: a drawn header, an undrawn header, an unclosed block
    assert!(matches!(
        render("---\ntitle: first\n---\nflowchart LR\n    A --> B\n"),
        Diagram::Svg(_)
    ));
    assert_eq!(
        render("---\ntitle: first\n---\n%% c\npie\n    \"first\" : 1\n"),
        unsupported("pie")
    );
    assert_eq!(render("---\ntitle: first\npie\n"), invalid(UNKNOWN));
    // MMDS JSON and other JSON are not Mermaid
    let mmds = "{\"version\": 1, \"geometry_level\": \"layout\", \"metadata\": {\"diagram_type\": \"flowchart\"}, \"nodes\": [], \"edges\": []}";
    assert_eq!(render(mmds), invalid(UNKNOWN));
    assert_eq!(render(&format!("  \n{mmds}")), invalid(UNKNOWN));
    assert_eq!(render("[1, 2]"), invalid(UNKNOWN));
    // an unknown or absent header goes to mmdflux, which refuses it
    assert_eq!(render("notADiagram\n    A --> B\n"), invalid(UNKNOWN));
    assert_eq!(render(""), invalid(UNKNOWN));
    assert_eq!(render("%% only a comment\n"), invalid(UNKNOWN));
    // a parse failure is invalid, with mmdflux's message after its one prefix
    let Diagram::Invalid { reason } = render(&source("invalid")) else {
        panic!("the malformed flowchart is invalid");
    };
    assert!(!reason.is_empty());
    assert!(!reason.starts_with("parse error: "), "{reason}");
    for name in UNDRAWN {
        assert!(!matches!(render(&source(name)), Diagram::Svg(_)), "{name}");
    }
    assert_eq!(render(&source("unsupported")), unsupported("pie"));
    // an undrawn header is source-only before its body is counted, but after the byte bound
    let many = format!("gantt\n{}", "    A\n".repeat(3000));
    assert!(many.len() < 65536);
    assert_eq!(render(&many), unsupported("gantt"));
    let deep = format!("block-beta\n{}", "    subgraph A\n".repeat(40));
    assert_eq!(render(&deep), unsupported("block-beta"));
    let big = format!("pie\n%%{}", "x".repeat(65536));
    assert_eq!(render(&big), invalid(OVER_BYTES));
}

#[test]
fn dgm1_labels_keep_escape_codes() {
    // encode_label: exactly thirteen characters become decimal codes; CR and LF become spaces
    assert_eq!(
        encode_label("\"#;<>[](){}|`"),
        "#34;#35;#59;#60;#62;#91;#93;#40;#41;#123;#125;#124;#96;"
    );
    assert_eq!(encode_label("a\nb\r\nc"), "a b  c");
    for kept in ["& \\ ' café ✓ 日本", "plain words", "%%", "--", "&lt", ""] {
        assert_eq!(encode_label(kept), kept);
    }
    assert_eq!(encode_label("A[{{x}}]"), "A#91;#123;#123;x#125;#125;#93;");
    // the drawn SVG keeps the codes as written: the crate decodes no entity
    let out = svg(&source("labels"));
    for code in [
        "#91;one#93;",
        "#124; two",
        "#34;first#34;",
        "#35;tag #59; end",
        "#40;x#41;",
        "#123;y#125;",
        "#96;z#96;",
        "#60;second#62;",
    ] {
        assert!(out.contains(code), "{code}");
    }
    // ampersand, backslash, apostrophe and Unicode are text; authored entities stay text
    assert!(out.contains(">#60;second#62; &amp; back\\slash it&apos;s café ✓</text>"));
    assert!(out.contains(">&amp;lt;command&amp;gt;</text>"));
    // a long unbroken label wraps across text lines, and the wrap splits a code
    assert!(
        out.contains(">thirdthirdthirdthirdthirdthir#5</text>"),
        "{out}"
    );
    assert!(out.contains(">9;secondsecondsecondsec</text>"), "{out}");
    // every drawn family keeps the full payload in each of its label contexts
    let p = PAYLOAD_SVG;
    for (name, whole, contexts) in [
        // class: a class label, a member and an edge label (wrapped); a namespace name is an
        // identifier, so a class diagram has no group label
        (
            "class",
            vec![format!("first {p}"), format!("+String {p}")],
            3,
        ),
        // sequence: a box (group) label, a participant, a loop (group) label, a message, a note
        (
            "sequence",
            vec![
                format!("first {p}"),
                format!("[second {p}]"),
                format!("ask {p}"),
                format!("third {p}"),
            ],
            5,
        ),
        // state: a state label, a description, a transition label (wrapped) and a note; a
        // composite state shows its id, so a state diagram has no group label
        (
            "state",
            vec![
                format!("first {p}"),
                format!("second {p}"),
                format!("third {p}"),
            ],
            4,
        ),
        // flowchart: a subgraph label, a node label and an edge label (wrapped)
        (
            "labels",
            vec![format!("fourth {p}"), format!("first {p}")],
            3,
        ),
    ] {
        let out = svg(&source(name));
        for text in &whole {
            assert!(out.contains(&format!(">{text}</text>")), "{name}: {text}");
        }
        let texts = texts(&out);
        for part in CODES
            .iter()
            .chain(&["&amp;", "back\\slash", "it&apos;s", "café ✓"])
        {
            let n: usize = texts.iter().map(|t| t.matches(part).count()).sum();
            assert!(
                n >= contexts,
                "{name}: {part} in {n} of {contexts} contexts"
            );
        }
    }
}

/// The thirteen decimal codes encode_label writes.
const CODES: [&str; 13] = [
    "#34;", "#35;", "#59;", "#60;", "#62;", "#91;", "#93;", "#40;", "#41;", "#123;", "#125;",
    "#124;", "#96;",
];

/// The full label payload as the drawn SVG writes it: every code kept as written, `&` and `'`
/// as entities. The fixtures write it with a bare `&` and `'`.
const PAYLOAD_SVG: &str = "#34;q#34; #35;h #59;s #60;l#62; #91;b#93; #40;p#41; #123;c#125; #124;v #96;t#96; &amp; back\\slash it&apos;s café ✓";

/// The content of every `<text>` element, in order.
fn texts(svg: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = svg;
    while let Some(open) = rest.find("<text") {
        rest = &rest[open..];
        let start = rest.find('>').expect("a whole text tag") + 1;
        let end = rest.find("</text>").expect("a closed text element");
        out.push(&rest[start..end]);
        rest = &rest[end..];
    }
    out
}

#[test]
fn dgm1_hostile_input_is_bounded() {
    // bytes: checked before anything else
    let pad = |total: usize| {
        let head = "flowchart LR\n    A --> B\n%% ";
        let mut s = String::from(head);
        s.push_str(&"x".repeat(total - head.len() - 1));
        s.push('\n');
        assert_eq!(s.len(), total);
        s
    };
    assert!(matches!(render(&pad(65536)), Diagram::Svg(_)));
    assert_eq!(render(&pad(65537)), invalid(OVER_BYTES));
    assert_eq!(render(&"{".repeat(70000)), invalid(OVER_BYTES));
    assert_eq!(render(&"\u{feff}".repeat(30000)), invalid(OVER_BYTES));

    // statements: 2048 is allowed, 2049 is not
    let statements = |n: usize, sep: &str| {
        let mut s = String::from("flowchart LR");
        for _ in 1..n {
            s.push_str(sep);
            s.push('A');
        }
        s.push('\n');
        s
    };
    assert!(matches!(render(&statements(2048, "\n")), Diagram::Svg(_)));
    assert_eq!(render(&statements(2049, "\n")), invalid(OVER_STATEMENTS));
    assert!(matches!(render(&statements(2048, ";")), Diagram::Svg(_)));
    assert_eq!(render(&statements(2049, ";")), invalid(OVER_STATEMENTS));
    // blank and comment-only segments do not count; front matter does not count
    let mut quiet = statements(2048, "\n");
    quiet.push_str(&"\n\n   \n%% a comment; with \"a quote\n".repeat(500));
    assert!(matches!(render(&quiet), Diagram::Svg(_)));
    let fronted = format!(
        "---\n{}---\n{}",
        "title: x\n".repeat(100),
        statements(2048, "\n")
    );
    assert!(matches!(render(&fronted), Diagram::Svg(_)));
    // quoted separators, escaped quotes and entity codes do not split a statement
    let mut quoted = String::from("flowchart LR");
    for _ in 1..2048 {
        quoted.push_str("\n    A[\"x;y \\\" ; z\nnext\"]");
    }
    quoted.push('\n');
    assert!(quoted.len() < 65536);
    assert!(matches!(render(&quoted), Diagram::Svg(_)));
    let codes = ["#59;", "&#59;", "&#x3b;", "&lt;", "&gt;", "&amp;", "&quot;"];
    let mut coded = String::from("flowchart LR");
    for i in 1..2048 {
        coded.push_str(&format!("\n    A[x{}y]", codes[i % codes.len()]));
    }
    coded.push('\n');
    assert!(coded.len() < 65536);
    assert!(matches!(render(&coded), Diagram::Svg(_)));
    // an undrawn header's body is never counted; an unknown header's is
    assert_eq!(
        render(&statements(2049, "\n").replacen("flowchart LR", "gantt", 1)),
        unsupported("gantt")
    );
    assert_eq!(
        render(&statements(2049, "\n").replacen("flowchart LR", "notADiagram", 1)),
        invalid(OVER_STATEMENTS)
    );

    // depth: 32 nested subgraphs are allowed, 33 are not
    let nested = |n: usize, sep: &str| {
        let mut s = String::from("flowchart TD");
        for i in 0..n {
            s.push_str(&format!("{sep}subgraph S{i}"));
        }
        s.push_str(&format!("{sep}A --> B"));
        for _ in 0..n {
            s.push_str(&format!("{sep}end"));
        }
        s.push('\n');
        s
    };
    assert!(matches!(render(&nested(32, "\n")), Diagram::Svg(_)));
    assert_eq!(render(&nested(33, "\n")), invalid(OVER_DEPTH));
    assert!(matches!(render(&nested(32, ";")), Diagram::Svg(_)));
    assert_eq!(render(&nested(33, ";")), invalid(OVER_DEPTH));
    assert_eq!(
        render(
            &nested(33, "\n")
                .replace("subgraph", "SUBGRAPH")
                .replace("end", "END")
        ),
        invalid(OVER_DEPTH),
        "case-insensitive keywords"
    );
    // quoted keywords and separators are not structure
    let mut labelled = String::from("flowchart TD");
    for i in 0..40 {
        labelled.push_str(&format!("\n    N{i}[\"subgraph; subgraph {i}\"]"));
    }
    labelled.push('\n');
    assert!(matches!(render(&labelled), Diagram::Svg(_)));
    // closers never underflow the counter
    let mut closers = String::from("flowchart TD");
    closers.push_str(&"\nend".repeat(100));
    closers.push_str(&nested(32, "\n").replacen("flowchart TD", "", 1));
    assert!(matches!(render(&closers), Diagram::Svg(_)));
    let mut closers = String::from("flowchart TD");
    closers.push_str(&"\nend".repeat(100));
    closers.push_str(&nested(33, "\n").replacen("flowchart TD", "", 1));
    assert_eq!(render(&closers), invalid(OVER_DEPTH));
    // class and state bodies nest by braces at the end of a statement
    let braces = |header: &str, n: usize| {
        let mut s = String::from(header);
        for i in 0..n {
            s.push_str(&format!("\n    state S{i} {{"));
        }
        s.push_str("\n    [*] --> A");
        for _ in 0..n {
            s.push_str("\n    }");
        }
        s.push('\n');
        s
    };
    assert!(matches!(
        render(&braces("stateDiagram-v2", 32)),
        Diagram::Svg(_)
    ));
    assert_eq!(render(&braces("stateDiagram-v2", 33)), invalid(OVER_DEPTH));
    assert_eq!(render(&braces("stateDiagram", 33)), invalid(OVER_DEPTH));
    let mut class = String::from("classDiagram");
    for i in 0..33 {
        class.push_str(&format!("\n    class C{i} {{"));
    }
    assert_eq!(render(&class), invalid(OVER_DEPTH));
    // leading closers never take the counter below zero, so they hide no later depth
    let closed_first = format!("stateDiagram-v2{}{}", "\n    }".repeat(100), braces("", 33));
    assert_eq!(render(&closed_first), invalid(OVER_DEPTH));
    // braces count only in class and state bodies, and not inside labels
    let mut flow_braces = String::from("flowchart TD");
    for i in 0..40 {
        flow_braces.push_str(&format!("\n    N{i}{{B}}"));
    }
    flow_braces.push('\n');
    assert!(matches!(render(&flow_braces), Diagram::Svg(_)));
    let mut quoted_brace = String::from("stateDiagram-v2");
    for i in 0..40 {
        quoted_brace.push_str(&format!("\n    state \"S {{\" as S{i}"));
    }
    quoted_brace.push('\n');
    assert!(matches!(render(&quoted_brace), Diagram::Svg(_)));

    // valid shapes that look like structure
    let mut plans = String::from("flowchart TD");
    for i in 0..200 {
        plans.push_str(&format!("\n    A{i}[1) Plan] --> A{}[1) Plan]", i + 1));
    }
    plans.push('\n');
    assert!(
        matches!(render(&plans), Diagram::Svg(_)),
        "repeated A[1) Plan]"
    );
    let mut classes = String::from("classDiagram");
    for i in 0..100 {
        classes.push_str(&format!("\n    class C{i} {{"));
        for m in 0..5 {
            classes.push_str(&format!("\n        +int field{m}"));
        }
        classes.push_str("\n    }");
    }
    classes.push('\n');
    assert!(
        matches!(render(&classes), Diagram::Svg(_)),
        "100 classes, five members each"
    );

    // malformed input is an error, never a crash
    for hostile in [
        "flowchart LR\n A[",
        "flowchart LR\n A --> ",
        "flowchart LR\n A[\"unterminated\n",
        "flowchart LR\n subgraph\n",
        "flowchart LR\n end\n end\n",
        "classDiagram\n }\n }\n class {\n",
        "stateDiagram-v2\n }\n state {\n",
        "sequenceDiagram\n loop\n loop\n",
        "sequenceDiagram\n end\n end\n",
        "flowchart LR\n A -->|unclosed B\n",
        "flowchart LR\n A((((((((((((((((((((B\n",
        "flowchart\0LR\n A\0-->B\n",
        "flowchart LR\n A[\u{202e}\u{0}\u{7}]\n",
        "---\n",
        "---",
        "%%",
        ";;;;;;;;",
        "\u{feff}",
    ] {
        let _ = render(hostile);
    }
    let deep_sequence = format!(
        "sequenceDiagram\n{}    A->>B: x\n{}",
        "    loop x\n".repeat(1000),
        "    end\n".repeat(1000)
    );
    let _ = render(&deep_sequence);
    let parens = format!(
        "flowchart LR\n A{}B{}\n",
        "(".repeat(2000),
        ")".repeat(2000)
    );
    let _ = render(&parens);
}

/// `header`, then `prefix`, then `n` openers, `inner`, and `n` copies of `close`.
fn nest(
    header: &str,
    prefix: &str,
    n: usize,
    open: impl Fn(usize) -> String,
    inner: &str,
    close: &str,
) -> String {
    let mut s = format!("{header}\n{prefix}");
    for i in 0..n {
        s.push_str(&open(i));
    }
    s.push_str(inner);
    s.push_str(&close.repeat(n));
    s
}

/// A result for an assertion message: an SVG shows as its length only.
fn short(d: Diagram) -> Diagram {
    match d {
        Diagram::Svg(svg) => Diagram::Svg(format!("<{} bytes of SVG>", svg.len())),
        other => other,
    }
}

fn drawn(d: &Diagram) -> bool {
    matches!(d, Diagram::Svg(_))
}

/// A comment, or a quote the pinned parser does not read as a string, hides no structure: the
/// bounds see every block the pinned parser nests and every statement it reads, and they refuse
/// before anything is drawn.
#[test]
fn dgm1_comments_and_quotes_hide_no_structure() {
    let states = |prefix: &str, n: usize| {
        nest(
            "stateDiagram-v2",
            prefix,
            n,
            |i| format!("state S{i} {{\n"),
            "[*] --> A\n",
            "}\n",
        )
    };
    // an inline comment's quote opens no string: the pinned state parser strips the comment
    assert!(drawn(&render(&states("X: x %% \"\n", 32))));
    assert_eq!(
        short(render(&states("X: x %% \"\n", 33))),
        invalid(OVER_DEPTH)
    );
    // a commented opening brace still opens a block, and a commented closer still closes
    let commented = |n: usize| {
        nest(
            "stateDiagram-v2",
            "",
            n,
            |i| format!("state S{i} {{ %% open {i}\n"),
            "[*] --> A\n",
            "} %% close\n",
        )
    };
    assert!(drawn(&render(&commented(32))));
    assert_eq!(short(render(&commented(33))), invalid(OVER_DEPTH));
    // in line-based families a quote never reaches the next line
    assert_eq!(short(render(&states("X : \"\n", 33))), invalid(OVER_DEPTH));
    let namespaces = |n: usize| {
        nest(
            "classDiagram",
            "class X[\"\n",
            n,
            |i| format!("namespace N{i} {{\n"),
            "class A\n",
            "}\n",
        )
    };
    assert!(drawn(&render(&namespaces(32))));
    assert_eq!(short(render(&namespaces(33))), invalid(OVER_DEPTH));
    // flowchart: a comment's quote, a quote in a round label, a quote never closed, and an
    // `end` inside a label hide no subgraph
    let subgraphs = |prefix: &str, inner: &str, n: usize| {
        nest(
            "flowchart TD",
            prefix,
            n,
            |i| format!("subgraph S{i}\n"),
            inner,
            "end\n",
        )
    };
    for (prefix, inner) in [
        ("A %% \"\n", "B\n"),
        ("A(\"x)\n", "B(\"y)\n"),
        ("A[\"x]\n", "B\n"),
    ] {
        assert!(drawn(&render(&subgraphs(prefix, inner, 32))), "{prefix}");
        assert_eq!(
            short(render(&subgraphs(prefix, inner, 33))),
            invalid(OVER_DEPTH),
            "{prefix}"
        );
    }
    let label_end = nest(
        "flowchart TD",
        "",
        33,
        |i| format!("subgraph S{i}\nA{i}(\nend\nx)\n"),
        "B\n",
        "end\n",
    );
    assert_eq!(short(render(&label_end)), invalid(OVER_DEPTH));
    // subgraphs in a row never add up
    let mut row = String::from("flowchart TD\n");
    for i in 0..64 {
        row.push_str(&format!("subgraph S{i}\nA{i}\nend\n"));
    }
    assert!(drawn(&render(&row)), "64 subgraphs in a row");
    // the pinned flowchart grammar reads a lone CR as a line end
    let cr = |n: usize| {
        nest(
            "flowchart TD",
            "",
            n,
            |i| format!("subgraph S{i}\r"),
            "B\r",
            "end\r",
        )
    };
    assert!(drawn(&render(&cr(32))));
    assert_eq!(short(render(&cr(33))), invalid(OVER_DEPTH));
    // statements: a comment's quote and an unclosed quote hide no statements
    let flow = format!("flowchart LR\nA %% \"\n{}", "A\n".repeat(2048));
    assert_eq!(short(render(&flow)), invalid(OVER_STATEMENTS));
    let state = format!("stateDiagram-v2\nX : \"\n{}", "A --> B\n".repeat(2048));
    assert_eq!(short(render(&state)), invalid(OVER_STATEMENTS));
    // the reviewer's input: 2000 nested states after a comment's quote are 4003 statements,
    // refused before drawing; 1000 nested blocks stay under the statement bound and are refused
    // for their depth
    let deep = states("X: x %% \"\n", 2000);
    assert_eq!(deep.len(), 30926);
    assert_eq!(short(render(&deep)), invalid(OVER_STATEMENTS));
    // the two round-label quotes pair up as one string for the statement count, so this one
    // counts few statements; the depth walk reads each round label and sees all 2000 subgraphs
    assert_eq!(
        short(render(&subgraphs("A(\"x)\n", "B(\"y)\n", 2000))),
        invalid(OVER_DEPTH)
    );
    assert_eq!(
        short(render(&states("X: x %% \"\n", 1000))),
        invalid(OVER_DEPTH)
    );
    assert_eq!(
        short(render(&subgraphs("A(\"x)\n", "B(\"y)\n", 1000))),
        invalid(OVER_DEPTH)
    );
    assert_eq!(short(render(&namespaces(1000))), invalid(OVER_DEPTH));
}

/// Braces in state descriptions, transition labels and note text are label text, not blocks;
/// a brace the pinned parser does not read as a closer closes nothing.
#[test]
fn dgm1_state_label_braces_are_not_structure() {
    let lines = |line: &dyn Fn(usize) -> String, n: usize| {
        let mut s = String::from("stateDiagram-v2\n");
        for i in 0..n {
            s.push_str(&line(i));
        }
        s
    };
    for line in [
        &(|_| "S : brace {\n".to_string()) as &dyn Fn(usize) -> String,
        &|i| format!("S{i} : brace {{\n"),
        &|i| format!("A{i} --> B{i} : go {{\n"),
        &|i| format!("note right of A{i} : text {{\n"),
    ] {
        let src = lines(line, 33);
        assert!(drawn(&render(&src)), "{src}");
    }
    let note = format!(
        "stateDiagram-v2\nA --> B\nnote right of A\n{}end note\n",
        "text {\n".repeat(33)
    );
    assert!(drawn(&render(&note)), "multi-line note text");
    // a closer inside a note's text, or a brace with text after it, closes nothing
    for close_line in ["note left of X\n}\nend note\n", "} x\n"] {
        let src = nest(
            "stateDiagram-v2",
            "",
            33,
            |i| format!("state S{i} {{\n{close_line}"),
            "[*] --> A\n",
            "}\n",
        );
        assert_eq!(short(render(&src)), invalid(OVER_DEPTH), "{close_line}");
    }
    let src = nest(
        "classDiagram",
        "",
        33,
        |i| format!("namespace N{i} {{\n}} x\n"),
        "class A\n",
        "}\n",
    );
    assert_eq!(short(render(&src)), invalid(OVER_DEPTH), "class");
}

/// Front matter with CRLF line ends draws what the same source with LF draws.
#[test]
fn dgm1_crlf_front_matter_draws_like_lf() {
    let crlf = "---\r\nk0: v\r\nk1: v\r\nk2: v\r\nk3: v\r\nk4: v\r\nk5: v\r\ntitle: café\r\n---\r\nflowchart LR\r\nA --> B\r\n";
    assert_eq!(crlf.len(), 89);
    assert!(
        render(crlf) == Diagram::Svg(svg(&crlf.replace("\r\n", "\n"))),
        "{:?}",
        short(render(crlf))
    );
    let two = "---\r\ntitle: first\r\nk: v\r\n---\r\nflowchart LR\r\nA --> B\r\n";
    assert!(
        render(two) == Diagram::Svg(svg(&two.replace("\r\n", "\n"))),
        "{:?}",
        short(render(two))
    );
    // a CRLF body without front matter draws like LF too
    let body = "flowchart LR\r\n    A --> B\r\n";
    assert!(
        render(body) == Diagram::Svg(svg(&body.replace("\r\n", "\n"))),
        "{:?}",
        short(render(body))
    );
}

/// The hostile fixtures under `fixtures/hostile`: `<name>.mmd` renders to `<name>.svg`.
///
/// The pinned crate copies `style`, `classDef` and `linkStyle` values into attributes as written,
/// so a raw drawing can hold an event attribute or an external `url()`. Raw SVG carries no safety
/// claim: these drawings are captured here, and the admission check that rewrites SVG before a
/// page uses it refuses them.
const HOSTILE: [&str; 19] = [
    "label-script",
    "label-foreign-object",
    "label-attribute-quote",
    "label-entities",
    "label-cdata",
    "label-import",
    "click-call",
    "click-url",
    "class-hostile",
    "sequence-hostile",
    "state-hostile",
    "style-event",
    "style-url",
    "style-script",
    "classdef-event",
    "classdef-url",
    "linkstyle-event",
    "linkstyle-url",
    "state-classdef-event",
];

fn hostile(name: &str, ext: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/hostile")
        .join(format!("{name}.{ext}"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Hostile labels, click directives and style values draw the same raw bytes every time. The
/// bytes are captured as fixtures for the admission check; nothing here claims they are safe.
#[test]
fn dgm1_hostile_raw_svg_is_captured_for_admission() {
    for name in HOSTILE {
        let src = String::from_utf8(hostile(name, "mmd")).expect("UTF-8 source");
        let first = render(&src);
        assert_eq!(first, render(&src), "{name}: two renders");
        assert!(drawn(&first), "{name}: {:?}", short(first.clone()));
        assert!(
            as_bytes(&first) == hostile(name, "svg"),
            "{name}: differs from its fixture"
        );
    }
}

/// A state line opens a block only where the pinned state parser reads a composite state: its
/// earlier rules come first. A transition, an inline description and a declaration with no id
/// open nothing, even with `state` as the identifier.
#[test]
fn dgm1_state_blocks_follow_the_parser_precedence() {
    let lines = |line: &dyn Fn(usize) -> String, n: usize| {
        let mut s = String::from("stateDiagram-v2\n");
        for i in 0..n {
            s.push_str(&line(i));
        }
        s
    };
    for line in [
        &(|_| "state --> B {\n".to_string()) as &dyn Fn(usize) -> String,
        &|i| format!("state --> B{i} : go {{\n"),
        &|i| format!("STATE --> state{i} {{\n"),
        &|_| "state\t: brace {\n".to_string(),
        &|_| "state: brace {\n".to_string(),
        &|_| "state {\n".to_string(),
        &|_| "state \"unclosed {\n".to_string(),
        &|_| "direction LR {\n".to_string(),
    ] {
        let src = lines(line, 33);
        assert!(drawn(&render(&src)), "{src}");
    }
    // `state ` with a space is never an inline description: the parser declares a state whose
    // id is `:` and nests its body, so 33 of them nest 33 deep
    assert_eq!(
        short(render(&lines(&|_| "state : brace {\n".to_string(), 33))),
        invalid(OVER_DEPTH)
    );
    assert!(drawn(&render(&lines(
        &|_| "state : brace {\n".to_string(),
        32
    ))));
    for line in [
        &(|i| format!("state S{i} {{\n")) as &dyn Fn(usize) -> String,
        &|i| format!("state \"x\" as S{i} {{\n"),
        &|i| format!("STATE S{i}:::c {{\n"),
    ] {
        assert_eq!(short(render(&lines(line, 33))), invalid(OVER_DEPTH));
    }
}

/// Flowchart depth comes from the pinned grammar's structure: keywords in comments and labels
/// are text, and subgraphs in a row never add up. A subgraph opens wherever the grammar starts a
/// statement, also after a node on the same line.
#[test]
fn dgm1_flowchart_depth_reads_structure_only() {
    let opens = "subgraph ".repeat(33);
    let closes = "end ".repeat(33);
    for src in [
        format!("flowchart LR\nA --> B\n%% {opens}{closes}\n"),
        format!("flowchart LR\nA --> B %% {opens}{closes}\n"),
        format!("flowchart LR\nA[\"{opens}{closes}\"] --> B\n"),
        format!("flowchart LR\nA[{opens}{closes}] --> B\n"),
        format!(
            "flowchart LR\nA(\n{}{}x)\n",
            "subgraph x\n".repeat(33),
            "end\n".repeat(33)
        ),
        format!(
            "flowchart LR\n{}",
            (0..70)
                .map(|i| format!("subgraph S{i}\nA{i}\nend\n"))
                .collect::<String>()
        ),
        format!(
            "flowchart LR\n{}\n",
            (0..70)
                .map(|i| format!("subgraph S{i};A{i};end;"))
                .collect::<String>()
        ),
    ] {
        assert!(drawn(&render(&src)), "{src}");
    }
    // a separator right after a subgraph's header ends it: the grammar finds no `end` for it and
    // reads its header as plain nodes, so it adds no depth, and the subgraph is not closed
    // (ruling final)
    let separated = nest(
        "flowchart TD",
        "",
        33,
        |i| format!("subgraph S{i}\n\n"),
        "B\n",
        "end\n",
    );
    assert_eq!(short(render(&separated)), invalid(UNCLOSED), "{separated}");
    // keywords in an arrow's label: the depth bound comes first, so the arrow bound's reason
    // shows that they add no depth (ruling final: an arrow is over 256 bytes here)
    for src in [
        format!("flowchart LR\nA -->|{opens}{closes}| B\n"),
        format!("flowchart LR\nA -- {opens}{closes} --> B\n"),
    ] {
        assert_eq!(short(render(&src)), invalid(OVER_ARROW), "{src}");
    }
    // a subgraph after a node on the same line still nests
    let after_node = |n: usize| {
        nest(
            "flowchart TD",
            "",
            n,
            |i| format!("A{i} subgraph S{i}\n"),
            "B\n",
            "end\n",
        )
    };
    assert!(drawn(&render(&after_node(32))));
    assert_eq!(short(render(&after_node(33))), invalid(OVER_DEPTH));
    // a multi-line label hides no subgraph that follows it
    let after_label = |n: usize| {
        nest(
            "flowchart TD",
            "",
            n,
            |i| format!("subgraph S{i}\nA{i}[\"x\nend\"]\n"),
            "B\n",
            "end\n",
        )
    };
    assert!(drawn(&render(&after_label(32))));
    assert_eq!(short(render(&after_label(33))), invalid(OVER_DEPTH));
}

/// A subgraph left open is read again as plain nodes, also when its title reads differently
/// that way. Its body is read as structure both times, so keywords in comments and labels and
/// subgraphs in a row add no depth. Nesting the parser attempts is still refused at 33.
#[test]
fn dgm1_open_subgraphs_are_read_again_as_structure() {
    let opens = "subgraph ".repeat(33);
    let closes = "end ".repeat(33);
    // a title whose quote no later quote closes, and one whose quote a later label closes
    let open = "subgraph S[\"x]\n";
    for src in [
        format!("flowchart LR\n{open}%% {opens}{closes}\n"),
        format!("flowchart LR\n{open}A[\"{opens}{closes}\"]\n"),
        format!("flowchart LR\n{open}A[\"y\"]\n%% {opens}{closes}\n"),
        format!("flowchart LR\n{open}A[\"y\"] --> B[{opens}{closes}]\n"),
        format!("flowchart LR\n{open}A -->|{opens}{closes}| B\n"),
        format!(
            "flowchart LR\n{open}{}",
            (0..70)
                .map(|i| format!("subgraph T{i}\nA{i}\nend\n"))
                .collect::<String>()
        ),
        format!(
            "flowchart LR\n{open}A[\"y\"]\n{}",
            (0..70)
                .map(|i| format!("subgraph T{i};A{i};end;"))
                .collect::<String>()
        ),
        // the other titles that read differently as nodes
        format!("flowchart LR\nsubgraph S[(x]\n%% {opens}{closes}\n"),
        format!("flowchart LR\nsubgraph a--b\n%% {opens}{closes}\n"),
    ] {
        // the depth bound comes first: the unclosed reason shows the body added no depth
        // (ruling final: an open subgraph is refused before the pinned crate reads it again)
        assert_eq!(short(render(&src)), invalid(UNCLOSED), "{src}");
    }
    // nesting inside or after an open subgraph is still refused at 33 attempted levels
    for open in ["subgraph S[\"x]\n", "subgraph S[(x]\n", "subgraph S\n"] {
        let after = |n: usize| {
            nest(
                "flowchart TD",
                open,
                n,
                |i| format!("subgraph T{i}\n"),
                "B\n",
                "end\n",
            )
        };
        assert_eq!(short(render(&after(31))), invalid(UNCLOSED), "{open}");
        assert_eq!(short(render(&after(32))), invalid(OVER_DEPTH), "{open}");
    }
}

/// A source the pinned crate panics on while it lays the drawing out: an empty subgraph inside
/// another.
const LIBRARY_PANICS: &str = "flowchart TD\nsubgraph S0\nsubgraph S1\nend\nend\n";

/// The variable that makes [`dgm1_library_panic_child`] run.
const PANIC_CHILD: &str = "RDY_DIAGRAM_LIBRARY_PANIC_CHILD";

/// The child half of [`dgm1_a_drawing_library_panic_prints_nothing`]. Without its variable it
/// does nothing.
#[test]
fn dgm1_library_panic_child() {
    if std::env::var_os(PANIC_CHILD).is_none() {
        return;
    }
    let drawn = render(LIBRARY_PANICS);
    assert_eq!(
        drawn,
        Diagram::Failed {
            reason: "the drawing library failed on this source".to_string()
        }
    );
    println!("child rendered {drawn:?}");
}

/// The pinned crate's own panic is a drawing fault, and a fresh process prints nothing of it:
/// stderr holds no panic message and no payload.
#[test]
fn dgm1_a_drawing_library_panic_prints_nothing() {
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "dgm1_library_panic_child",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(PANIC_CHILD, "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.contains("child rendered Failed"),
        "stdout:\n{stdout}"
    );
    assert!(!stderr.contains("panicked"), "stderr:\n{stderr}");
    assert!(
        !stderr.contains("no entry found for key"),
        "stderr:\n{stderr}"
    );
}

/// Final review I2 (ruling final): the two inputs inside every other bound that held the pinned
/// crate for minutes or hours are refused before it is called. A subgraph that no `end` closes
/// (the pinned grammar reads its body again for each one, so k of them cost about 2^k) and an
/// arrow whose run and label pass 256 bytes (a long arrow spans as many ranks as it has dots or
/// dashes). The reasons are exact; no test here times anything.
#[test]
fn dgm1_unclosed_subgraphs_and_long_arrows_are_refused() {
    // the reviewer's sources: k nested subgraphs that no `end` closes
    for k in [16, 18, 20, 22] {
        let src = format!(
            "flowchart LR\n{}A\n",
            (0..k)
                .map(|i| format!("subgraph U{i}\n"))
                .collect::<String>()
        );
        assert_eq!(short(render(&src)), invalid(UNCLOSED), "k = {k}");
    }
    // one is enough, wherever it is
    for src in [
        "flowchart LR\nsubgraph S\nA --> B\n",
        "flowchart LR\nA --> B\nsubgraph S[\"x]\nC\n",
        "graph TD\nsubgraph S\nsubgraph T\nA\nend\n",
    ] {
        assert_eq!(short(render(src)), invalid(UNCLOSED), "{src}");
    }
    // a closed nesting 32 deep still draws
    let closed = format!(
        "flowchart TD\n{}A --> B\n{}",
        (0..32)
            .map(|i| format!("subgraph S{i}\n"))
            .collect::<String>(),
        "end\n".repeat(32)
    );
    assert!(drawn(&render(&closed)), "32 closed subgraphs");

    // the reviewer's long dotted arrows: 16,000 and 60,000 dots
    for dots in [16000, 60000] {
        let src = format!("flowchart LR\nA -. x{}- B\n", ".".repeat(dots));
        assert!(src.len() <= 65536);
        assert_eq!(short(render(&src)), invalid(OVER_ARROW), "{dots} dots");
    }
    // an arrow is its run and its label: 256 bytes draw, 257 do not, in every arrow form
    let dotted = |n: usize| format!("-. x{}->", ".".repeat(n - 6));
    let solid = |n: usize| format!("-- {} -->", "y".repeat(n - 7));
    let thick = |n: usize| format!("== {} ==>", "y".repeat(n - 7));
    let piped = |n: usize| format!("-->|{}|", "y".repeat(n - 5));
    let long = |n: usize| format!("{}>", "-".repeat(n - 1));
    for (form, arrow) in [
        ("dotted", &dotted as &dyn Fn(usize) -> String),
        ("solid", &solid),
        ("thick", &thick),
        ("piped", &piped),
        ("long", &long),
    ] {
        assert_eq!(arrow(256).len(), 256, "{form}");
        let src = |n: usize| format!("flowchart LR\nA {} B\n", arrow(n));
        assert!(drawn(&render(&src(256))), "{form}: 256 bytes");
        assert_eq!(
            short(render(&src(257))),
            invalid(OVER_ARROW),
            "{form}: 257 bytes"
        );
        // later in a chain, or after a subgraph, it is still one arrow
        let chained = format!("flowchart LR\nA --> B {} C --> D\n", arrow(257));
        assert_eq!(
            short(render(&chained)),
            invalid(OVER_ARROW),
            "{form}: chained"
        );
    }
    // the same bytes in a node label are no arrow
    let label = format!("flowchart LR\nA[\"{}\"] --> B\n", ".".repeat(1000));
    assert!(drawn(&render(&label)), "dots in a node label");
}
