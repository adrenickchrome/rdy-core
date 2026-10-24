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
    // node, edge and group labels keep their codes in every drawn family
    for (name, codes) in [
        (
            "class",
            &[
                ">first #40;one#41;</text>",
                ">uses #91;it#93;</text>",
                ">second #59; third</text>",
            ][..],
        ),
        (
            "sequence",
            &[
                ">first #35;1</text>",
                ">ask #60;x#62;</text>",
                ">answer #123;y#125;</text>",
                ">third #124; done</text>",
            ],
        ),
        (
            "state",
            &[
                ">first #40;A#41;</text>",
                ">second #96;B#96;</text>",
                ">go #59; now</text>",
            ],
        ),
    ] {
        let out = svg(&source(name));
        for code in codes {
            assert!(out.contains(code), "{name}: {code}");
        }
    }
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

/// The tags of an SVG: each tag's name and its attributes' names and values, lowercased.
fn tags(svg: &str) -> Vec<(String, Vec<(String, String)>)> {
    let mut out = Vec::new();
    let mut rest = svg;
    while let Some(start) = rest.find('<') {
        rest = &rest[start + 1..];
        let mut end = 0;
        let mut quote = None;
        for (i, c) in rest.char_indices() {
            match (quote, c) {
                (None, '"' | '\'') => quote = Some(c),
                (Some(q), c) if c == q => quote = None,
                (None, '>') => {
                    end = i;
                    break;
                }
                _ => {}
            }
        }
        let tag = &rest[..end];
        rest = &rest[end..];
        let tag = tag.trim_start_matches('/').trim_end_matches('/');
        let mut parts = tag.splitn(2, char::is_whitespace);
        let name = parts.next().unwrap_or("").to_ascii_lowercase();
        let mut attrs = Vec::new();
        let mut a = parts.next().unwrap_or("").trim();
        while !a.is_empty() {
            let eq = a.find('=').unwrap_or(a.len());
            let key = a[..eq].trim().to_ascii_lowercase();
            a = a[eq..].trim_start_matches('=').trim_start();
            let value = match a.chars().next() {
                Some(q @ ('"' | '\'')) => {
                    let close = a[1..].find(q).map(|i| i + 1).unwrap_or(a.len());
                    let v = a[1..close].to_string();
                    a = a.get(close + 1..).unwrap_or("").trim_start();
                    v
                }
                _ => {
                    let stop = a.find(char::is_whitespace).unwrap_or(a.len());
                    let v = a[..stop].to_string();
                    a = a[stop..].trim_start();
                    v
                }
            };
            attrs.push((key, value.to_ascii_lowercase()));
        }
        out.push((name, attrs));
    }
    out
}

/// The elements an inert SVG never holds.
const ACTIVE: [&str; 13] = [
    "script",
    "foreignobject",
    "iframe",
    "object",
    "embed",
    "image",
    "img",
    "a",
    "use",
    "animate",
    "set",
    "handler",
    "listener",
];

/// Whether a CSS or attribute value names a resource outside the SVG.
fn external(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("@import")
        || lower.contains("javascript:")
        || lower.contains("://")
        || lower.match_indices("url(").any(|(at, _)| {
            !lower[at + 4..]
                .trim_start_matches(['\'', '"', ' '])
                .starts_with('#')
        })
}

/// No active element and no event or link attribute. Visible label words are not inspected.
fn assert_no_active(svg: &str, what: &str) {
    for (name, attrs) in tags(svg) {
        assert!(
            !ACTIVE.contains(&name.as_str()),
            "{what}: a <{name}> element\n{svg}"
        );
        for (key, _) in attrs {
            assert!(
                !key.starts_with("on"),
                "{what}: event attribute {key}\n{svg}"
            );
            assert!(
                !key.ends_with("href") && key != "src",
                "{what}: a link attribute {key}\n{svg}"
            );
        }
    }
}

/// No active content and no external resource: attributes and style sheets name only local
/// `#id` targets. Visible label words are not inspected.
fn assert_inert(svg: &str, what: &str) {
    assert_no_active(svg, what);
    for (_, attrs) in tags(svg) {
        for (key, value) in attrs {
            if key == "xmlns" || key.starts_with("xmlns:") {
                assert!(value.starts_with("http://www.w3.org/"), "{what}: {value}");
                continue;
            }
            assert!(!external(&value), "{what}: external {key}={value}\n{svg}");
        }
    }
    let mut rest = svg;
    while let Some(open) = rest.find("<style") {
        let body = &rest[open..];
        let close = body.find("</style>").expect("a closed style element");
        assert!(
            !external(&body[..close]),
            "{what}: an external style\n{svg}"
        );
        rest = &body[close..];
    }
}

/// Hostile labels and click directives draw no active content and no external resource.
#[test]
fn dgm1_svg_has_no_active_content() {
    for name in DRAWN {
        assert_inert(&svg(&source(name)), name);
    }
    let labels_and_clicks = [
        "flowchart LR\n    A[\"<script>alert(1)</script>\"] --> B[\"<img src=x onerror=alert(1)>\"]\n",
        "flowchart LR\n    A[\"<foreignObject><div>x</div></foreignObject>\"] --> B\n",
        "flowchart LR\n    A --> B\n    click A call alert(1)\n    click B href \"javascript:alert(1)\"\n",
        "flowchart LR\n    A --> B\n    click A \"https://example.com\" \"tip\" _blank\n",
        "flowchart LR\n    A[\"x\\\" onload=\\\"alert(1)\"] --> B[\"' onclick='alert(1)\"]\n",
        "flowchart LR\n    A[\"&lt;script&gt;alert(1)&lt;/script&gt;\"] --> B[\"javascript:alert(1)\"]\n",
        "flowchart LR\n    A[\"]]><script>alert(1)</script>\"] --> B[\"<!-- x --> <![CDATA[y]]>\"]\n",
        "flowchart LR\n    A[\"@import url(https://example.com/s.css)\"] --> B\n",
        "classDiagram\n    class A[\"<script>x</script>\"]\n    A --> B : <img onerror=x>\n    click A href \"javascript:x\"\n",
        "sequenceDiagram\n    participant A as <script>x</script>\n    A->>B: <img src=x onerror=alert(1)>\n    Note over A: javascript:alert(1)\n",
        "stateDiagram-v2\n    state \"<script>x</script>\" as S\n    S --> T : <a href=\"https://example.com\">x</a>\n",
    ];
    for (i, src) in labels_and_clicks.iter().enumerate() {
        match render(src) {
            Diagram::Svg(out) => assert_inert(&out, &format!("hostile {i}")),
            other => assert!(
                !matches!(other, Diagram::Failed { .. }),
                "hostile {i}: {other:?}"
            ),
        }
    }
    // style values without a quote draw no active element, event or link attribute
    for (i, src) in STYLES.iter().enumerate() {
        match render(src) {
            Diagram::Svg(out) => assert_no_active(&out, &format!("style {i}")),
            other => assert!(
                !matches!(other, Diagram::Failed { .. }),
                "style {i}: {other:?}"
            ),
        }
    }
}

const STYLES: [&str; 6] = [
    "flowchart LR\n    A --> B\n    style A fill:url(https://example.com/x.svg),stroke:#f00\n",
    "flowchart LR\n    A --> B\n    classDef evil fill:url(http://example.com/a),color:expression(alert(1))\n    class A evil\n",
    "flowchart LR\n    A --> B\n    linkStyle 0 stroke:url(https://example.com/m)\n",
    "flowchart LR\n    A --> B\n    style A fill:<script>x</script>\n",
    "classDiagram\n    class A\n    A --> B\n    style A fill:url(https://example.com/x)\n",
    "stateDiagram-v2\n    S --> T\n    classDef evil fill:url(https://example.com/x)\n    class S evil\n",
];

/// A defect of the pinned generator, pinned so the admission check that refuses it stays
/// required: a style value reaches a presentation attribute as written and unescaped. A raw
/// drawing can then name an external resource, hold a `<` in an attribute, or end the
/// attribute with a quote and add an event attribute. Raw SVG is never served as is.
#[test]
fn dgm1_style_values_reach_raw_attributes() {
    let raw = |i: usize| svg(STYLES[i]);
    assert!(raw(0).contains(" fill=\"url(https://example.com/x.svg)\""));
    assert!(raw(1).contains(" fill=\"url(http://example.com/a)\""));
    assert!(raw(1).contains(" fill=\"expression(alert(1))\""));
    assert!(raw(2).contains(" stroke=\"url(https://example.com/m)\""));
    assert!(raw(3).contains(" fill=\"<script>x</script>\""));
    assert!(raw(5).contains(" fill=\"url(https://example.com/x)\""));
    // a quote in a style value ends the attribute, so a raw drawing can hold an event attribute
    for src in [
        "flowchart LR\n    A --> B\n    style A fill:#f00\" onload=\"alert(1)\n",
        "flowchart LR\n    A --> B\n    classDef e fill:#f00\" onload=\"alert(1)\n    class A e\n",
        "flowchart LR\n    A --> B\n    linkStyle 0 stroke:#f00\" onload=\"alert(1)\n",
        "stateDiagram-v2\n    S --> T\n    classDef e fill:#f00\" onload=\"alert(1)\n    class S e\n",
    ] {
        assert!(svg(src).contains("=\"#f00\" onload=\"alert(1)\""), "{src}");
    }
}
