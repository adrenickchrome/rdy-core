//! Disclosure review Task 1a: the reader text, labels, cut and language names moved into
//! `rdy_engine::reader` read exactly as the site read them before the move.
//!
//! The expected values were captured from rdy-render's own `Site::label`, `Site::label_coded`
//! (through `toc::change_label_coded`), `Site::summary`, `search::own_text`,
//! `search::own_text_coded` and `highlight::human_name` at the execution base, before any code
//! moved, over this same fixture. The engine has no dependency on rdy-render, so the values are
//! pinned here; a changed value is a failure, never a new capture.
use rdy_engine::Materialized;
use rdy_engine::reader::inline::{Ch, Decoder};
use rdy_engine::reader::labels::{
    self, LABEL_MAX, coded_label, cut_plain, human_name, plain_label,
};
use rdy_engine::reader::text::{self, Reading};
use rdy_model::redaction::CONTENT_REMOVED;
use rdy_model::{Block, BlockId, FieldValue};
use std::collections::BTreeMap;

fn id(s: &str) -> BlockId {
    BlockId::parse(s).expect("an id")
}

fn s(v: &str) -> FieldValue {
    FieldValue::Str(v.into())
}

fn block(
    i: &str,
    ty: &str,
    parent: Option<&str>,
    order: &str,
    fields: &[(&str, FieldValue)],
    body: &str,
) -> Block {
    Block {
        id: id(i),
        block_type: ty.into(),
        parent: parent.map(id),
        order: order.into(),
        fields: fields
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    }
}

/// One doc with a block of every kind the reader text and labels read.
fn fixture() -> Materialized {
    let doc = Some("d-AAAAA");
    let sec = Some("s-AAAAA");
    let removed = Block::redacted(
        id("p-RRRRR"),
        "prose".into(),
        sec.map(id),
        "a9".into(),
        rdy_model::redaction::RedactedBlockIdentity {
            version: rdy_model::Hash([1; 32]),
            content_hash: rdy_model::Hash([2; 32]),
            frozen: false,
            request: rdy_model::Hash([3; 32]),
        },
    );
    let blocks = vec![
        block(
            "d-AAAAA",
            "doc",
            None,
            "a0",
            &[
                ("name", s("payments-guide")),
                ("title", s("Payments `API` guide")),
                ("doc_type", s("overview")),
            ],
            "",
        ),
        block(
            "d-BBBBB",
            "doc",
            None,
            "a1",
            &[("name", s("plain-notes")), ("doc_type", s("overview"))],
            "",
        ),
        block(
            "s-AAAAA",
            "section",
            doc,
            "a0",
            &[("title", s("Café — naïve `résumé` notes"))],
            "",
        ),
        block(
            "t-AAAAA",
            "term",
            sec,
            "a0",
            &[("name", s("Wallet  rule")), ("key", s("wallet-rule"))],
            "A wallet holds **funds**.",
        ),
        block(
            "p-AAAAA",
            "prose",
            sec,
            "a1",
            &[],
            "Per {{t-AAAAA}}, {{t-AAAAA|the wallet}} and {{t-AAAAA|@key}} see {{c-ZZZZZ}}.\n\n- one  item with `code {{t-AAAAA}}`\n- [a link](https://example.com) *em* _under_ 日本語 🚀\n\n```\nfenced\n```",
        ),
        block(
            "D-AAAAA",
            "decision",
            sec,
            "a2",
            &[(
                "statement",
                s("Revoke D-BBBBB: keep `ledger` rows for {{t-AAAAA}}."),
            )],
            "",
        ),
        block(
            "k-AAAAA",
            "code",
            sec,
            "a3",
            &[("lang", s("py"))],
            "print('x')  # {{t-AAAAA}}",
        ),
        block(
            "k-BBBBB",
            "code",
            sec,
            "a4",
            &[("lang", s("zig"))],
            "const x = 1;",
        ),
        block("g-AAAAA", "diagram", sec, "a5", &[], "graph TD; A-->B"),
        block(
            "v-AAAAA",
            "value",
            sec,
            "a6",
            &[
                ("name", s("Fee `rate`")),
                ("value", s("2.5")),
                ("unit", s("%")),
            ],
            "Per  trade.",
        ),
        block(
            "b-AAAAA",
            "table",
            sec,
            "a7",
            &[
                ("columns", FieldValue::List(vec![s("Name"), s("`Code`")])),
                (
                    "rows",
                    FieldValue::List(vec![
                        FieldValue::List(vec![s("a {{t-AAAAA}}"), s("b")]),
                        FieldValue::List(vec![FieldValue::Int(3), FieldValue::Bool(true)]),
                    ]),
                ),
            ],
            "",
        ),
        block(
            "q-AAAAA",
            "question",
            sec,
            "a8",
            &[("prompt", s("Should we   ship {{t-AAAAA}}?"))],
            "",
        ),
        removed,
        block(
            "f-AAAAA",
            "flow",
            sec,
            "b0",
            &[
                ("name", s("Checkout")),
                ("steps", FieldValue::List(vec![s("Pay `now`"), s("Done")])),
            ],
            "End.",
        ),
        block(
            "x-AAAAA",
            "figure",
            sec,
            "b1",
            &[("alt", s("A chart of fees"))],
            "",
        ),
        // the 40-character edge: 40 characters exactly, then 41
        block(
            "p-BBBBB",
            "prose",
            sec,
            "b2",
            &[],
            "abcdefghij abcdefghi abcdefghi abcdefghi",
        ),
        block(
            "p-CCCCC",
            "prose",
            sec,
            "b3",
            &[],
            "abcdefghij abcdefghij abcdefghi abcdefghi",
        ),
        // 40 visible characters with a code span: the backticks are not counted
        block(
            "p-DDDDD",
            "prose",
            sec,
            "b4",
            &[],
            "`abcdefghij` abcdefghi abcdefghi abcdefghi",
        ),
        block(
            "p-EEEEE",
            "prose",
            sec,
            "b5",
            &[],
            "`abcdefghij` abcdefghij abcdefghi abcdefghi",
        ),
        // Unicode: 40 characters of more than one byte each, then 41
        block(
            "p-FFFFF",
            "prose",
            sec,
            "b6",
            &[],
            "éééééééééé ééééééééé ééééééééé ééééééééé",
        ),
        block(
            "p-GGGGG",
            "prose",
            sec,
            "b7",
            &[],
            "éééééééééé éééééééééé ééééééééé ééééééééé",
        ),
        // whitespace runs collapse before the count
        block(
            "p-HHHHH",
            "prose",
            sec,
            "b8",
            &[],
            "  abcdefghij \n\n abcdefghi\tabcdefghi   abcdefghi  ",
        ),
        // a reference in the label reads as its target's label
        block(
            "p-JJJJJ",
            "prose",
            sec,
            "b9",
            &[],
            "See {{t-AAAAA}} and {{D-AAAAA}} for every wallet rule",
        ),
    ];
    Materialized::build(
        rdy_model::StateSnapshot::default(),
        blocks.into_iter().map(|b| (b.id.clone(), b)).collect(),
    )
}

const IDS: &[&str] = &[
    "d-AAAAA", "d-BBBBB", "s-AAAAA", "t-AAAAA", "p-AAAAA", "D-AAAAA", "k-AAAAA", "k-BBBBB",
    "g-AAAAA", "v-AAAAA", "b-AAAAA", "q-AAAAA", "p-RRRRR", "f-AAAAA", "x-AAAAA", "p-BBBBB",
    "p-CCCCC", "p-DDDDD", "p-EEEEE", "p-FFFFF", "p-GGGGG", "p-HHHHH", "p-JJJJJ",
];

/// The fixture's side as the site read it: its numbers are the site's (table, code and diagram
/// blocks counted per kind in reading order).
struct Side<'m> {
    m: &'m Materialized,
    numbers: BTreeMap<BlockId, usize>,
}

impl<'m> Side<'m> {
    fn new(m: &'m Materialized) -> Side<'m> {
        let numbers = [
            ("k-AAAAA", 1),
            ("k-BBBBB", 2),
            ("g-AAAAA", 1),
            ("b-AAAAA", 1),
        ]
        .into_iter()
        .map(|(i, n)| (id(i), n))
        .collect();
        Side { m, numbers }
    }
}

impl Reading for Side<'_> {
    fn block(&self, id: &BlockId) -> Option<&Block> {
        self.m.get(id)
    }

    fn doc_of(&self, id: &BlockId) -> Option<&Block> {
        self.m.doc_of(id)
    }

    fn tree_name(&self, id: &BlockId) -> Option<String> {
        self.m.name_of(id)
    }

    fn number(&self, b: &Block) -> usize {
        self.numbers.get(&b.id).copied().unwrap_or(1)
    }

    fn unavailable(&self) -> Option<&'static str> {
        None
    }

    fn drops(&self, _: &BlockId) -> bool {
        false
    }
}

/// A coded label as the site's change list wrote it, less the escaping: each code span's run in
/// a `<code>` element.
fn runs(chars: &[Ch]) -> String {
    let mut out = String::new();
    for run in chars.chunk_by(|a, b| a.1 == b.1) {
        let t: String = run.iter().map(|c| c.0).collect();
        if run[0].1 == 0 {
            out.push_str(&t);
        } else {
            out.push_str(&format!("<code>{t}</code>"));
        }
    }
    out
}

struct Expect {
    id: &'static str,
    label: &'static str,
    coded_label: Option<&'static str>,
    summary: &'static str,
    own: &'static str,
    own_coded: &'static str,
}

/// What rdy-render read for each block at the execution base.
const EXPECT: &[Expect] = &[
    Expect {
        id: "d-AAAAA",
        label: "Payments API guide",
        coded_label: None,
        summary: "Payments API guide",
        own: "",
        own_coded: "",
    },
    Expect {
        id: "d-BBBBB",
        label: "Plain notes",
        coded_label: None,
        summary: "plain-notes",
        own: "",
        own_coded: "",
    },
    Expect {
        id: "s-AAAAA",
        label: "Café — naïve résumé notes",
        coded_label: None,
        summary: "Café — naïve résumé notes",
        own: "",
        own_coded: "",
    },
    Expect {
        id: "t-AAAAA",
        label: "Wallet  rule",
        coded_label: None,
        summary: "Wallet rule",
        own: "Wallet  rule: A wallet holds funds.",
        own_coded: "Wallet  rule: A wallet holds funds.",
    },
    Expect {
        id: "p-AAAAA",
        label: "Per Wallet rule, the wallet and wallet-rule see …. one item with code Wallet rul",
        coded_label: Some("Per Wallet rule, the wallet and…"),
        summary: "Per Wallet rule, the wallet and wallet-rule see …. one item with code Wallet rul",
        own: "Per Wallet  rule, the wallet and wallet-rule see .\n\none  item with code Wallet  rule\na link em _under_ 日本語 🚀\n\n\nfenced\n",
        own_coded: "Per Wallet  rule, the wallet and wallet-rule see .\n\none  item with \u{e000}code Wallet  rule\u{e001}\na link em _under_ 日本語 🚀\n\n\u{e000}\nfenced\n\u{e001}",
    },
    Expect {
        id: "D-AAAAA",
        label: "Revoke …: keep ledger rows for Wallet rule.",
        coded_label: Some("Revoke …: keep <code>ledger</code> rows for Wallet…"),
        summary: "Revoke …: keep ledger rows for Wallet rule.",
        own: "",
        own_coded: "",
    },
    Expect {
        id: "k-AAAAA",
        label: "Python code 1",
        coded_label: None,
        summary: "print('x') # {{…}}",
        own: "print('x')  # {{t-AAAAA}}",
        own_coded: "print('x')  # {{t-AAAAA}}",
    },
    Expect {
        id: "k-BBBBB",
        label: "Zig code 2",
        coded_label: None,
        summary: "const x = 1;",
        own: "const x = 1;",
        own_coded: "const x = 1;",
    },
    Expect {
        id: "g-AAAAA",
        label: "Diagram 1",
        coded_label: None,
        summary: "graph TD; A-->B",
        own: "",
        own_coded: "",
    },
    Expect {
        id: "v-AAAAA",
        label: "2.5%",
        coded_label: None,
        summary: "Fee rate",
        own: "Fee rate: 2.5% Per  trade.",
        own_coded: "Fee \u{e000}rate\u{e001}: 2.5% Per  trade.",
    },
    Expect {
        id: "b-AAAAA",
        label: "Untitled",
        coded_label: None,
        summary: "",
        own: "Name Code a Wallet  rule b 3 true",
        own_coded: "Name \u{e000}Code\u{e001} a Wallet  rule b 3 true",
    },
    Expect {
        id: "q-AAAAA",
        label: "Should we ship Wallet rule?",
        coded_label: Some("Should we ship Wallet rule?"),
        summary: "Should we ship Wallet rule?",
        own: "Should we   ship Wallet  rule?",
        own_coded: "Should we   ship Wallet  rule?",
    },
    Expect {
        id: "p-RRRRR",
        label: "Content removed.",
        coded_label: None,
        summary: "Content removed.",
        own: "Content removed.",
        own_coded: "Content removed.",
    },
    Expect {
        id: "f-AAAAA",
        label: "Checkout",
        coded_label: None,
        summary: "Checkout",
        own: "Checkout Pay now Done End.",
        own_coded: "Checkout Pay \u{e000}now\u{e001} Done End.",
    },
    Expect {
        id: "x-AAAAA",
        label: "A chart of fees",
        coded_label: Some("A chart of fees"),
        summary: "A chart of fees",
        own: "A chart of fees",
        own_coded: "A chart of fees",
    },
    Expect {
        id: "p-BBBBB",
        label: "abcdefghij abcdefghi abcdefghi abcdefghi",
        coded_label: Some("abcdefghij abcdefghi abcdefghi abcdefghi"),
        summary: "abcdefghij abcdefghi abcdefghi abcdefghi",
        own: "abcdefghij abcdefghi abcdefghi abcdefghi",
        own_coded: "abcdefghij abcdefghi abcdefghi abcdefghi",
    },
    Expect {
        id: "p-CCCCC",
        label: "abcdefghij abcdefghij abcdefghi abcdefghi",
        coded_label: Some("abcdefghij abcdefghij abcdefghi…"),
        summary: "abcdefghij abcdefghij abcdefghi abcdefghi",
        own: "abcdefghij abcdefghij abcdefghi abcdefghi",
        own_coded: "abcdefghij abcdefghij abcdefghi abcdefghi",
    },
    Expect {
        id: "p-DDDDD",
        label: "abcdefghij abcdefghi abcdefghi abcdefghi",
        coded_label: Some("<code>abcdefghij</code> abcdefghi abcdefghi abcdefghi"),
        summary: "abcdefghij abcdefghi abcdefghi abcdefghi",
        own: "abcdefghij abcdefghi abcdefghi abcdefghi",
        own_coded: "\u{e000}abcdefghij\u{e001} abcdefghi abcdefghi abcdefghi",
    },
    Expect {
        id: "p-EEEEE",
        label: "abcdefghij abcdefghij abcdefghi abcdefghi",
        coded_label: Some("<code>abcdefghij</code> abcdefghij abcdefghi…"),
        summary: "abcdefghij abcdefghij abcdefghi abcdefghi",
        own: "abcdefghij abcdefghij abcdefghi abcdefghi",
        own_coded: "\u{e000}abcdefghij\u{e001} abcdefghij abcdefghi abcdefghi",
    },
    Expect {
        id: "p-FFFFF",
        label: "éééééééééé ééééééééé ééééééééé ééééééééé",
        coded_label: Some("éééééééééé ééééééééé ééééééééé ééééééééé"),
        summary: "éééééééééé ééééééééé ééééééééé ééééééééé",
        own: "éééééééééé ééééééééé ééééééééé ééééééééé",
        own_coded: "éééééééééé ééééééééé ééééééééé ééééééééé",
    },
    Expect {
        id: "p-GGGGG",
        label: "éééééééééé éééééééééé ééééééééé ééééééééé",
        coded_label: Some("éééééééééé éééééééééé ééééééééé…"),
        summary: "éééééééééé éééééééééé ééééééééé ééééééééé",
        own: "éééééééééé éééééééééé ééééééééé ééééééééé",
        own_coded: "éééééééééé éééééééééé ééééééééé ééééééééé",
    },
    Expect {
        id: "p-HHHHH",
        label: "abcdefghij abcdefghi abcdefghi abcdefghi",
        coded_label: Some("abcdefghij abcdefghi abcdefghi abcdefghi"),
        summary: "abcdefghij abcdefghi abcdefghi abcdefghi",
        own: "  abcdefghij \n\n abcdefghi\tabcdefghi   abcdefghi  ",
        own_coded: "  abcdefghij \n\n abcdefghi\tabcdefghi   abcdefghi  ",
    },
    Expect {
        id: "p-JJJJJ",
        label: "See Wallet rule and Revoke …: keep ledger rows for Wallet rule. for every wallet",
        coded_label: Some("See Wallet rule and Revoke …: keep…"),
        summary: "See Wallet rule and Revoke …: keep ledger rows for Wallet rule. for every wallet",
        own: "See Wallet  rule and Revoke …: keep ledger rows for Wallet rule. for every wallet rule",
        own_coded: "See Wallet  rule and Revoke …: keep ledger rows for Wallet rule. for every wallet rule",
    },
];

/// The language names rdy-render's highlighter gave at the execution base.
const LANGUAGES: &[(&str, &str)] = &[
    ("py", "Python"),
    ("Python3", "Python"),
    (" YML ", "YAML"),
    ("json", "JSON"),
    ("rs", "Rust"),
    ("bash", "Shell"),
    ("console", "Shell"),
    ("toml", "TOML"),
    ("peg", "Grammar"),
    ("mermaid", "Mermaid"),
    ("d2", "D2"),
    ("", "Text"),
    ("txt", "Text"),
    ("html", "HTML"),
    ("css", "CSS"),
    ("js", "JavaScript"),
    ("typescript", "TypeScript"),
    ("sql", "SQL"),
    ("md", "Markdown"),
    ("zig", "Zig"),
    ("élan", "Élan"),
];

/// D-0C4TB (Labels, Q3): a label keeps 40 visible characters; the 41st cuts it at a word
/// boundary with one `…`. Code-span backticks are not counted, a code span stays code,
/// whitespace collapses before the count, and the count is in characters, not bytes.
#[test]
fn drv1_labels_keep_40_and_41_character_edges() {
    assert_eq!(LABEL_MAX, 40);
    // the plain cut, alone
    let forty = "a".repeat(40);
    assert_eq!(plain_label(&forty), forty);
    assert_eq!(plain_label(&"a".repeat(41)), format!("{forty}…"));
    assert_eq!(cut_plain(&"a".repeat(41), 40), format!("{forty}…"));
    assert_eq!(
        plain_label("abcdefghij abcdefghi abcdefghi abcdefghi"),
        "abcdefghij abcdefghi abcdefghi abcdefghi"
    );
    assert_eq!(
        plain_label("abcdefghij abcdefghij abcdefghi abcdefghi"),
        "abcdefghij abcdefghij abcdefghi…"
    );
    // the cut drops trailing punctuation before the ellipsis
    assert_eq!(
        plain_label("abcdefghij abcdefghij abcdefghij, abcdefghi"),
        "abcdefghij abcdefghij abcdefghij…"
    );
    assert_eq!(
        plain_label("éééééééééé ééééééééé ééééééééé ééééééééé")
            .chars()
            .count(),
        40
    );
    assert_eq!(plain_label(" a \n\t b "), "a b");
    // a coded label: the code span's backticks are marks, not characters
    let coded = |t: &str| {
        Decoder::default().read(&rdy_engine::reader::inline::plain_refs_coded(
            t,
            rdy_model::RefMode::Strict,
            rdy_engine::reader::inline::Form::Spans,
            &|s| s.token.clone(),
            &|_| false,
        ))
    };
    assert_eq!(
        runs(&coded_label(&coded(
            "`abcdefghij` abcdefghi abcdefghi abcdefghi"
        ))),
        "<code>abcdefghij</code> abcdefghi abcdefghi abcdefghi"
    );
    assert_eq!(
        runs(&coded_label(&coded(
            "`abcdefghij` abcdefghij abcdefghi abcdefghi"
        ))),
        "<code>abcdefghij</code> abcdefghij abcdefghi…"
    );
    // a cut inside a code span ends the span there
    assert_eq!(
        runs(&coded_label(&coded(
            "abcdefghij abcdefghij abcdefghi `abcde fghij`"
        ))),
        "abcdefghij abcdefghij abcdefghi <code>abcde</code>…"
    );

    // through the label rule, as the site's change list cut each block's label
    let m = fixture();
    let side = Side::new(&m);
    for i in [
        "p-BBBBB", "p-CCCCC", "p-DDDDD", "p-EEEEE", "p-FFFFF", "p-GGGGG", "p-HHHHH", "p-JJJJJ",
        "p-AAAAA", "D-AAAAA",
    ] {
        let e = EXPECT.iter().find(|e| e.id == i).expect("an expectation");
        let b = m.get(&id(i)).expect("the block");
        let chars = labels::label_coded(&side, b).expect("a summary label");
        assert_eq!(
            Some(runs(&coded_label(&chars))).as_deref(),
            e.coded_label,
            "{i}"
        );
        assert_eq!(
            plain_label(&side.label_at(b, 0)),
            plain_label(e.label),
            "{i}"
        );
    }
    let edge = |i: &str| plain_label(&side.label_at(m.get(&id(i)).expect("the block"), 0));
    assert_eq!(edge("p-BBBBB"), "abcdefghij abcdefghi abcdefghi abcdefghi");
    assert_eq!(edge("p-CCCCC"), "abcdefghij abcdefghij abcdefghi…");
    assert_eq!(edge("p-FFFFF"), "éééééééééé ééééééééé ééééééééé ééééééééé");
    assert_eq!(edge("p-GGGGG"), "éééééééééé éééééééééé ééééééééé…");
    assert_eq!(edge("p-HHHHH"), "abcdefghij abcdefghi abcdefghi abcdefghi");
}

/// D-RDH1C, D-9BWMV: the field-aware reader text, the labels and summaries, and the language
/// names read as rdy-render's search index and site read them; a decision's statement reads as
/// its trail statement's reader text.
#[test]
fn drv1_reader_text_and_language_names_match_render() {
    let m = fixture();
    let side = Side::new(&m);
    assert_eq!(EXPECT.len(), IDS.len());
    for e in EXPECT {
        let b = m.get(&id(e.id)).expect("the block");
        assert_eq!(side.label_at(b, 0), e.label, "label of {}", e.id);
        assert_eq!(labels::summary(&side, b), e.summary, "summary of {}", e.id);
        assert_eq!(text::own_text(&side, b), e.own, "reader text of {}", e.id);
        assert_eq!(
            text::own_text_coded(&side, b),
            e.own_coded,
            "coded reader text of {}",
            e.id
        );
    }
    // the statement of a decision that revokes: the prefix goes, the first letter is upper case
    let d = m.get(&id("D-AAAAA")).expect("the decision");
    assert_eq!(
        text::decision_statement(&side, d, true),
        "Keep ledger rows for Wallet  rule."
    );
    assert_eq!(
        text::decision_statement(&side, d, false),
        "Revoke D-BBBBB: keep ledger rows for Wallet  rule."
    );
    let removed = Block::redacted(
        id("D-CCCCC"),
        "decision".into(),
        None,
        "a0".into(),
        rdy_model::redaction::RedactedBlockIdentity {
            version: rdy_model::Hash([4; 32]),
            content_hash: rdy_model::Hash([5; 32]),
            frozen: true,
            request: rdy_model::Hash([6; 32]),
        },
    );
    assert_eq!(
        text::decision_statement(&side, &removed, false),
        CONTENT_REMOVED
    );
    for (lang, name) in LANGUAGES {
        assert_eq!(human_name(lang), *name, "{lang:?}");
    }
    // one table: a highlighted language and its aliases share one name
    assert_eq!(labels::language("Python3"), Some("python"));
    assert_eq!(labels::language(" YML "), Some("yaml"));
    assert_eq!(labels::language("zig"), None);
}

// Task 1b: the removal projection, the shape and the count rule moved into
// `rdy_engine::reader::shape` and `rdy_engine::reader::review`.

fn hash(n: u8) -> rdy_model::Hash {
    rdy_model::Hash([n; 32])
}

/// One doc with two prose blocks, a reference to the first, and two figures with a light and a
/// dark image each.
fn masking_fixture() -> Materialized {
    let doc = Some("d-MMMMM");
    let sec = Some("s-MMMMM");
    let blocks = vec![
        block(
            "d-MMMMM",
            "doc",
            None,
            "a0",
            &[("name", s("masked")), ("doc_type", s("overview"))],
            "",
        ),
        block(
            "p-KKKKK",
            "prose",
            doc,
            "a0",
            &[],
            "Leading words before any section.",
        ),
        block(
            "s-MMMMM",
            "section",
            doc,
            "a1",
            &[("title", s("Masked part"))],
            "",
        ),
        block("p-PPPPP", "prose", sec, "a0", &[], "Words a removal names."),
        block("p-NNNNN", "prose", sec, "a1", &[], "See {{p-PPPPP}} here."),
        block(
            "x-PPPPP",
            "figure",
            sec,
            "a2",
            &[
                ("alt", s("Light image lost")),
                ("blob", s(&hash(11).to_string())),
                ("dark_blob", s(&hash(12).to_string())),
            ],
            "",
        ),
        block(
            "x-MMMMM",
            "figure",
            sec,
            "a3",
            &[
                ("alt", s("Dark image lost")),
                ("blob", s(&hash(13).to_string())),
                ("dark_blob", s(&hash(14).to_string())),
            ],
            "",
        ),
        block(
            "x-NNNNN",
            "figure",
            sec,
            "a4",
            &[
                ("alt", s("Both images kept")),
                ("blob", s(&hash(15).to_string())),
                ("dark_blob", s(&hash(16).to_string())),
            ],
            "",
        ),
    ];
    Materialized::build(
        rdy_model::StateSnapshot::default(),
        blocks.into_iter().map(|b| (b.id.clone(), b)).collect(),
    )
}

/// D-Z8Q4N: a removal matches a block by its recorded version, never by its id alone. D-TBDA1:
/// a figure whose light or dark image lost content reads as removed, and the other image never
/// stands in. The mask applies before any reader fact: the label, the text, a label reached
/// through a reference, the reading order and the count rule all read the stub.
#[test]
fn drv1_removed_versions_and_either_image_mask_reader_facts() {
    use rdy_engine::reader::review::{between_states, figure_blobs, mask_removed};
    use rdy_engine::reader::shape::{Shape, section_of};

    let m = masking_fixture();
    let v = |i: &str| m.version_of(&id(i)).expect("a version");
    // nothing to remove: no projection
    assert!(mask_removed(&m, &BTreeMap::new(), &BTreeMap::new()).is_none());
    // a removal of another version of p-KKKKK masks nothing: it matches by version
    let mut stale = m.get(&id("p-KKKKK")).unwrap().clone();
    stale.body = "An older version.".into();
    let other_version = BTreeMap::from([(stale.version_hash(), hash(90))]);
    assert_ne!(stale.version_hash(), v("p-KKKKK"));
    assert!(mask_removed(&m, &other_version, &BTreeMap::new()).is_none());

    assert_eq!(
        figure_blobs(m.get(&id("x-PPPPP")).unwrap()),
        vec![hash(11), hash(12)],
        "light first"
    );
    let versions = BTreeMap::from([(v("p-PPPPP"), hash(91))]);
    // the light image of one figure and the dark image of another lost content
    let blobs = BTreeMap::from([(hash(11), hash(92)), (hash(14), hash(93))]);
    let masked = mask_removed(&m, &versions, &blobs).expect("a projection");

    let removed = ["p-PPPPP", "x-PPPPP", "x-MMMMM"];
    let kept = ["d-MMMMM", "p-KKKKK", "s-MMMMM", "p-NNNNN", "x-NNNNN"];
    let requests = [hash(91), hash(92), hash(93)];
    for (i, request) in removed.iter().zip(requests) {
        let (was, is) = (m.get(&id(i)).unwrap(), masked.get(&id(i)).unwrap());
        assert!(is.is_redacted(), "{i}");
        assert_eq!(is.redaction(), Some(request), "{i}");
        // the stub keeps its id, type, parent, order and recorded version
        assert_eq!(
            (&is.block_type, &is.parent, &is.order),
            (&was.block_type, &was.parent, &was.order),
            "{i}"
        );
        assert_eq!(masked.version_of(&id(i)), m.version_of(&id(i)), "{i}");
        // no authored word survives: no alt, no file names, no body
        assert!(is.fields.is_empty(), "{i}");
        assert_eq!(is.body, CONTENT_REMOVED, "{i}");
        assert!(figure_blobs(is).is_empty(), "{i}");
    }
    for i in kept {
        assert_eq!(masked.get(&id(i)), m.get(&id(i)), "{i}");
    }

    // reader facts read the stub: its label is the notice, and so is a label reached through a
    // reference to it
    let side = Side::new(&masked);
    for i in removed {
        let b = masked.get(&id(i)).unwrap();
        assert_eq!(labels::label(&side, b, 0), CONTENT_REMOVED, "{i}");
        // the search's text of a stub is the notice alone
        assert_eq!(text::own_text(&side, b), CONTENT_REMOVED, "{i}");
    }
    let refers = masked.get(&id("p-NNNNN")).unwrap();
    assert_eq!(
        labels::label(&side, refers, 0),
        format!("See {CONTENT_REMOVED} here.")
    );
    let before = Side::new(&m);
    assert_eq!(
        labels::label(&before, m.get(&id("p-NNNNN")).unwrap(), 0),
        "See Words a removal names. here."
    );
    assert_eq!(
        labels::label(&side, masked.get(&id("x-NNNNN")).unwrap(), 0),
        "Both images kept"
    );

    // the shape keeps every place: the doc's page, the reading order and the sections
    let (shape, was) = (Shape::build(&masked), Shape::build(&m));
    assert_eq!(shape.page(&id("d-MMMMM")).as_deref(), Some("masked.html"));
    let order = |s: &Shape| -> Vec<String> {
        s.blocks_of(&id("d-MMMMM"))
            .iter()
            .map(|b| b.id.to_string())
            .collect()
    };
    assert_eq!(order(&shape), order(&was));
    assert_eq!(
        section_of(&masked, &id("x-MMMMM")).map(|s| s.id.to_string()),
        Some("s-MMMMM".into())
    );
    assert_eq!(section_of(&masked, &id("p-KKKKK")), None, "a leading block");

    // a stub keeps its recorded version, so the count rule counts no change for the removal
    // itself; a side's later edit counts against the masked side, and the masked blocks stay
    // uncounted
    assert_eq!(between_states(&m, &masked).changed_blocks(), 0);
    let mut edited = m.get(&id("p-KKKKK")).unwrap().clone();
    edited.body = "Leading words, edited.".into();
    let next = masked.with(&[rdy_model::Op::Update {
        base: v("p-KKKKK"),
        block: edited,
    }]);
    let rv = between_states(&masked, &next);
    let changed: Vec<String> = rv.changed_set().iter().map(|b| b.to_string()).collect();
    assert_eq!(changed, vec!["p-KKKKK"]);
    let doc = rv.doc(&id("d-MMMMM")).expect("the doc changed");
    assert_eq!((doc.edited, doc.new, doc.removed), (1, 0, 0));
}
