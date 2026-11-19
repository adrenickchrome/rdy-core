//! Disclosure review Tasks 1a to 1c. Task 1a: the reader text, labels, cut and language names
//! moved into `rdy_engine::reader` read exactly as the site read them before the move. Task 1b:
//! the removal projection, the shape and the count rule. Task 1c: the side-view seam, its reader
//! view, the authored parts with their canonical bytes and hashes, and the frozen pre-change
//! hash vectors.
//!
//! The expected values were captured from rdy-render's own `Site::label`, `Site::label_coded`
//! (through `toc::change_label_coded`), `Site::summary`, `search::own_text`,
//! `search::own_text_coded` and `highlight::human_name` at the execution base, before any code
//! moved, over this same fixture. The engine has no dependency on rdy-render, so the values are
//! pinned here; a changed value is a failure, never a new capture.
use rdy_engine::Rdy;
use rdy_engine::as_built::Derived;
use rdy_engine::publication::{
    self, BlockParts, PartName, PublicationReviewSide, ReadPublicationSource, ReaderPartValue,
    ReaderReviewSide, ReferenceLookup, Resolved, ReviewRemovals, changed_parts,
};
use rdy_engine::reader::inline::{Ch, Decoder};
use rdy_engine::reader::labels::{
    self, LABEL_MAX, coded_label, cut_plain, human_name, plain_label,
};
use rdy_engine::reader::text::{self, Reading};
use rdy_engine::{Materialized, ReadState};
use rdy_model::redaction::{CONTENT_REMOVED, RedactedBlockIdentity};
use rdy_model::{Block, BlockId, FieldValue, Hash, Link, StateSnapshot};
use rdy_schema::{Pack, PackSet};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

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

// Task 1c: the side-view seam (`PublicationReviewSide`), its reader view, the authored parts
// and their hashes, and the frozen pre-change hash vectors.

fn bid(s: &str) -> BlockId {
    BlockId::parse(s).expect("an id")
}

fn blk(id: &str, ty: &str, parent: Option<&str>, order: &str) -> Block {
    Block {
        id: bid(id),
        block_type: ty.into(),
        parent: parent.map(bid),
        order: order.into(),
        fields: BTreeMap::new(),
        body: String::new(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    }
}

fn with(mut b: Block, k: &str, v: FieldValue) -> Block {
    b.fields.insert(k.into(), v);
    b
}

fn body(mut b: Block, text: &str) -> Block {
    b.body = text.into();
    b
}

fn link(mut b: Block, kind: &str, target: &str, part: Option<&str>) -> Block {
    b.links.entry(kind.into()).or_default().push(Link {
        target: bid(target),
        part: part.map(str::to_string),
    });
    b
}

fn doc(id: &str, parent: Option<&str>, name: &str, order: &str) -> Block {
    let b = with(blk(id, "doc", parent, order), "name", s(name));
    let b = with(b, "title", s(&format!("Doc {name}")));
    with(b, "doc_type", s("page"))
}

fn profile(id: &str, key: &str, select: &[&str], planned: &str) -> Block {
    let b = blk(id, "publication_profile", Some("d-ADM1N"), "z");
    let b = with(b, "key", s(key));
    let b = with(b, "title", s("Profile title"));
    let b = with(b, "audience", s("org"));
    let b = with(b, "planned", s(planned));
    with(
        b,
        "select",
        FieldValue::List(select.iter().map(|x| s(x)).collect()),
    )
}

fn stub_of(b: &Block, version: Hash, request: Hash) -> Block {
    Block::redacted(
        b.id.clone(),
        b.block_type.clone(),
        b.parent.clone(),
        b.order.clone(),
        RedactedBlockIdentity {
            version,
            content_hash: b.content_hash(),
            frozen: false,
            request,
        },
    )
}

fn synth(blocks: &[Block]) -> Materialized {
    Materialized::build(
        StateSnapshot {
            blocks: BTreeMap::new(),
        },
        blocks.iter().cloned().map(|b| (b.id.clone(), b)).collect(),
    )
}

fn project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().expect("a temporary home");
    let r = Rdy::init(dir.path(), "rdy-standard").expect("a project");
    (dir, r)
}

/// One side resolved and viewed with the comparison's removals.
fn sides(
    st: &ReadState,
    before: &Materialized,
    after: &Materialized,
    profile: &str,
) -> (
    Resolved,
    Resolved,
    ReaderReviewSide,
    ReaderReviewSide,
    ReviewRemovals,
) {
    let b = publication::resolve_in_for_tests(st, before, profile).expect("the baseline");
    let a = publication::resolve_in_for_tests(st, after, profile).expect("the candidate");
    let source = ReadPublicationSource::with_settings(st, st.pack(), "Project", None);
    let removals = ReviewRemovals::of_sides(&source, &[&b, &a]).expect("the removals");
    let (bv, av) = (
        ReaderReviewSide::new(&b, &removals),
        ReaderReviewSide::new(&a, &removals),
    );
    (b, a, bv, av, removals)
}

/// The common blocks of the two profile revisions.
fn revision_blocks() -> Vec<Block> {
    vec![
        doc("d-ADM1N", None, "admin", "zz"),
        doc("d-1NTR1", None, "intro", "a"),
        link(
            body(
                blk("p-KEEP1", "prose", Some("d-1NTR1"), "a"),
                "See {{t-0THR1}}.",
            ),
            "references",
            "t-0THR1",
            None,
        ),
        body(
            with(
                blk("p-HDN01", "prose", Some("d-1NTR1"), "b"),
                "planned",
                FieldValue::Bool(true),
            ),
            "Planned words.",
        ),
        body(
            blk("p-G0NE1", "prose", Some("d-1NTR1"), "c"),
            "Words a removal takes.",
        ),
        doc("d-0THR1", None, "other", "b"),
        body(
            with(
                blk("t-0THR1", "term", Some("d-0THR1"), "a"),
                "name",
                s("Other term"),
            ),
            "A term in the other doc.",
        ),
    ]
}

/// D-CWC91: each same-store side reads its own profile revision's filtered selection. The
/// views are built after the filter and after both sides' removals, which match by version;
/// the reference-target lookup is separate and widens no view.
#[test]
fn drv1_same_store_sides_keep_each_profile_revision() {
    let (_dir, r) = project();
    let st = r.read();
    let mut before = revision_blocks();
    before.push(profile("pf-S1TE1", "site", &["d-1NTR1"], "hide"));
    let mut after = revision_blocks();
    after.push(profile("pf-S1TE1", "site", &["d-1NTR1", "d-0THR1"], "mark"));
    // the candidate holds the baseline's version of p-G0NE1 removed; p-KEEP1's removal names
    // another version than the baseline's, so the baseline keeps its words
    let request = Hash::of(b"a removal request");
    let gone = before
        .iter()
        .find(|b| b.id == bid("p-G0NE1"))
        .unwrap()
        .clone();
    let keep = before
        .iter()
        .find(|b| b.id == bid("p-KEEP1"))
        .unwrap()
        .clone();
    let older = Hash::of(b"an older version of p-KEEP1");
    for b in after.iter_mut() {
        if b.id == gone.id {
            *b = stub_of(&gone, gone.version_hash(), request);
        } else if b.id == keep.id {
            *b = stub_of(&keep, older, request);
        }
    }
    let (bm, am) = (synth(&before), synth(&after));
    let (b, a, bv, av, removals) = sides(&st, &bm, &am, "site");

    // each side keeps its own revision and its recorded selection facts
    assert_ne!(b.profile.version, a.profile.version);
    for (r, v) in [(&b, &bv), (&a, &av)] {
        let f = v.selection();
        assert_eq!(f.profile_id, r.profile.id);
        assert_eq!(f.profile_version, r.profile.version);
        assert_eq!(f.selection_hash, r.selection_hash);
        assert_eq!(f.selection_identity, r.selection_identity);
        assert_eq!(f.entries, r.entries);
        assert_eq!(f.docs, r.docs);
        assert_eq!(f.references, r.references);
        assert_eq!(f.redactions, r.redactions);
    }
    let docs = |v: &ReaderReviewSide| -> Vec<String> {
        v.reading().iter().map(BlockId::to_string).collect()
    };
    assert_eq!(docs(&bv), vec!["d-1NTR1"]);
    assert_eq!(
        docs(&av),
        vec!["d-1NTR1", "d-0THR1"],
        "reading order, not id order"
    );
    assert!(bv.tree_entry(&bid("d-0THR1")).is_none());
    let other = av.tree_entry(&bid("d-0THR1")).expect("the added doc");
    assert_eq!(other.title.as_deref(), Some("Doc other"));
    assert_eq!(other.page.as_deref(), Some("other.html"));
    assert_eq!(other.position, Some(1));

    // the hiding revision shows no planned block; the marking one shows it, planned
    let plan = bid("p-HDN01");
    assert!(bv.shown(&plan).is_none() && bv.parts(&plan).is_none());
    assert!(bv.reader_text(&plan).is_none());
    assert_eq!(
        av.parts(&plan).unwrap().get(&PartName::Status),
        &ReaderPartValue::Str("planned".into())
    );
    assert_eq!(av.reader_text(&plan), Some("Planned words."));

    // the removal of the baseline's version masks both sides before any text is read
    assert_eq!(
        removals.versions.get(&gone.version_hash()),
        Some(&request),
        "{removals:?}"
    );
    for v in [&bv, &av] {
        let g = v
            .shown(&gone.id)
            .expect("the removed block keeps its place");
        assert!(g.redacted);
        assert_eq!(g.label, CONTENT_REMOVED);
        assert!(
            v.reader_text(&gone.id).is_none(),
            "no excerpt of removed text"
        );
    }
    // a removal of another version masks nothing on the baseline
    let k = bv.shown(&keep.id).unwrap();
    assert!(!k.redacted);
    assert_eq!(k.label, "See unavailable reference.");
    assert!(av.shown(&keep.id).unwrap().redacted);

    // the reference target: the baseline's view lacks it, the separate lookup names it
    let target = bid("t-0THR1");
    assert!(bv.shown(&target).is_none());
    let lookup = ReferenceLookup::new(&am, &removals);
    assert_eq!(lookup.label(&target).as_deref(), Some("Other term"));
    assert_eq!(lookup.doc_title(&target).as_deref(), Some("Doc other"));
    assert_eq!(lookup.label(&gone.id).as_deref(), Some(CONTENT_REMOVED));
    assert_eq!(lookup.label(&bid("p-N0NE1")), None);
    // and the lookup leaves the baseline's view as it was
    assert!(bv.shown(&target).is_none());
    assert_eq!(bv.selection().docs, b.docs);
}

/// D-MCH70: a part is the authored value. Scalar types, list order, map values and field
/// presence survive; reader text stays apart; pins and order keys are no part.
#[test]
fn drv1_reader_parts_preserve_types_and_structure() {
    let (_dir, r) = project();
    let st = r.read();
    let pin = Hash([0x22; 32]);
    let typed = |b: Block| {
        let b = with(b, "n", FieldValue::Int(1));
        let b = with(b, "nul", FieldValue::Null);
        let b = with(
            b,
            "list",
            FieldValue::List(vec![FieldValue::Int(2), FieldValue::Int(1)]),
        );
        with(
            b,
            "map",
            FieldValue::Map(BTreeMap::from([
                ("b".to_string(), FieldValue::Int(1)),
                ("a".to_string(), s("one")),
            ])),
        )
    };
    let base = vec![
        with(
            with(
                doc("d-AAAAA", None, "guide", "a"),
                "description",
                s("Desc."),
            ),
            "everyday",
            s("A guide."),
        ),
        with(
            with(
                with(
                    blk("v-AAAAA", "value", Some("d-AAAAA"), "a"),
                    "name",
                    s("Limit"),
                ),
                "value",
                FieldValue::Int(9_007_199_254_740_993),
            ),
            "unit",
            s("bytes"),
        ),
        link(
            link(
                typed(body(
                    blk("p-AAAAA", "prose", Some("d-AAAAA"), "b"),
                    "A **bold** word.",
                )),
                "references",
                "v-AAAAA",
                None,
            ),
            "references",
            "d-AAAAA",
            Some("name"),
        ),
        with(
            with(
                with(
                    with(
                        blk("D-AAAAA", "decision", Some("d-AAAAA"), "c"),
                        "status",
                        s("accepted"),
                    ),
                    "statement",
                    s("Keep `rows`."),
                ),
                "rationale",
                s("Audits."),
            ),
            "built",
            s("1.0.0"),
        ),
        link(
            body(
                blk("p-G0VRN", "prose", Some("d-AAAAA"), "d"),
                "Governed words.",
            ),
            "decided_by",
            "D-AAAAA",
            None,
        ),
        body(
            blk("p-PNS01", "prose", Some("d-AAAAA"), "e"),
            "Pinned words.",
        ),
        body(blk("p-0RDER", "prose", Some("d-AAAAA"), "f"), "Last words."),
        typed(body(
            blk("p-TYPE1", "prose", Some("d-AAAAA"), "g"),
            "Typed.",
        )),
        typed(body(
            blk("p-QST01", "prose", Some("d-AAAAA"), "h"),
            "Listed.",
        )),
        typed(body(
            blk("p-NVZ01", "prose", Some("d-AAAAA"), "j"),
            "Nulled.",
        )),
        link(
            body(blk("p-RFS01", "prose", Some("d-AAAAA"), "k"), "Linked."),
            "references",
            "v-AAAAA",
            None,
        ),
        body(blk("p-B0DY1", "prose", Some("d-AAAAA"), "m"), "Old words."),
    ];
    let edit = |id: &str, f: &dyn Fn(Block) -> Block| -> Vec<Block> {
        base.iter()
            .map(|b| {
                if b.id == bid(id) {
                    f(b.clone())
                } else {
                    b.clone()
                }
            })
            .collect()
    };
    let m = synth(&base);
    let res = publication::resolve_in_for_tests(&st, &m, publication::IMPLICIT_ORG).unwrap();
    let source = ReadPublicationSource::with_settings(&st, st.pack(), "Project", None);
    let removals = ReviewRemovals::of_sides(&source, &[&res]).unwrap();
    let v = ReaderReviewSide::new(&res, &removals);
    let parts = |id: &str| v.parts(&bid(id)).expect("parts").clone();
    let names = |p: &BlockParts| -> Vec<String> { p.iter().map(|(n, _)| n.to_string()).collect() };

    // an ordinary block: every part, with its authored values
    let p = parts("p-AAAAA");
    assert_eq!(
        names(&p),
        vec![
            "title",
            "text",
            "type",
            "links",
            "status",
            "field:list",
            "field:map",
            "field:n",
            "field:nul"
        ]
    );
    let str_ = |t: &str| ReaderPartValue::Str(t.into());
    assert_eq!(
        p.get(&PartName::Title),
        &ReaderPartValue::Null,
        "a missing title"
    );
    assert_eq!(
        p.get(&PartName::Text),
        &ReaderPartValue::Map(BTreeMap::from([("body".into(), str_("A **bold** word."))]))
    );
    assert_eq!(
        v.reader_text(&bid("p-AAAAA")),
        Some("A bold word."),
        "reader text apart"
    );
    assert_eq!(p.get(&PartName::Type), &str_("prose"));
    let target = |t: &str, part: Option<&str>| {
        let mut m = BTreeMap::from([("target".to_string(), str_(t))]);
        if let Some(part) = part {
            m.insert("part".into(), str_(part));
        }
        ReaderPartValue::Map(m)
    };
    assert_eq!(
        p.get(&PartName::Links),
        &ReaderPartValue::Map(BTreeMap::from([(
            "references".into(),
            ReaderPartValue::List(vec![
                target("v-AAAAA", None),
                target("d-AAAAA", Some("name"))
            ])
        )])),
        "targets keep their order; a link keeps its part"
    );
    assert_eq!(
        p.get(&PartName::Status),
        &ReaderPartValue::Null,
        "unversioned"
    );
    assert_eq!(
        p.get(&PartName::Field("n".into())),
        &ReaderPartValue::Int(1)
    );
    assert_eq!(
        p.get(&PartName::Field("nul".into())),
        &ReaderPartValue::Null
    );
    assert_eq!(
        p.get(&PartName::Field("gone".into())),
        &ReaderPartValue::Absent
    );
    assert_eq!(
        p.get(&PartName::Field("list".into())),
        &ReaderPartValue::List(vec![ReaderPartValue::Int(2), ReaderPartValue::Int(1)])
    );
    assert_eq!(
        p.get(&PartName::Field("map".into())),
        &ReaderPartValue::Map(BTreeMap::from([
            ("a".into(), str_("one")),
            ("b".into(), ReaderPartValue::Int(1))
        ]))
    );
    // a covered field keeps its type inside `text`
    assert_eq!(
        parts("v-AAAAA").get(&PartName::Text),
        &ReaderPartValue::Map(BTreeMap::from([
            ("body".into(), str_("")),
            ("name".into(), str_("Limit")),
            ("unit".into(), str_("bytes")),
            ("value".into(), ReaderPartValue::Int(9_007_199_254_740_993)),
        ]))
    );
    // the shown status the doc digest writes
    let governed = res.as_built.get(&bid("p-G0VRN")).expect("a shown status");
    let label = Derived {
        status: governed.status,
        since: governed.since.clone(),
    }
    .label();
    assert_ne!(label, "unversioned");
    assert_eq!(parts("p-G0VRN").get(&PartName::Status), &str_(&label));
    // a doc compares only its title, name and description
    assert_eq!(
        names(&parts("d-AAAAA")),
        vec!["title", "field:description", "field:name"]
    );
    // a decision compares its title, its authored statement and its derived status tag
    let d = parts("D-AAAAA");
    assert_eq!(names(&d), vec!["title", "statement", "status"]);
    assert_eq!(d.get(&PartName::Statement), &str_("Keep `rows`."));
    let tag = res.graph.status(&bid("D-AAAAA")).expect("a status").tag();
    assert_eq!(d.get(&PartName::Status), &str_(tag));
    let facts = v.decision(&bid("D-AAAAA")).expect("decision facts");
    assert_eq!(facts.status.as_deref(), Some(tag));
    assert_eq!(facts.text.as_deref(), Some("Keep rows."));
    assert!(
        v.reader_text(&bid("D-AAAAA")).is_none(),
        "a decision has no page text"
    );

    // the one comparison: each edit gives exactly its part, and pins, order keys and a
    // decision's rationale give none
    let cases: Vec<(&str, Vec<Block>, Vec<&str>)> = vec![
        (
            "p-PNS01",
            edit("p-PNS01", &|mut b| {
                b.pins.insert(bid("v-AAAAA"), pin);
                b
            }),
            vec![],
        ),
        (
            "p-0RDER",
            edit("p-0RDER", &|mut b| {
                b.order = "fz".into();
                b
            }),
            vec![],
        ),
        (
            "D-AAAAA",
            edit("D-AAAAA", &|b| with(b, "rationale", s("Other."))),
            vec![],
        ),
        (
            "p-TYPE1",
            edit("p-TYPE1", &|b| with(b, "n", s("1"))),
            vec!["field:n"],
        ),
        (
            "p-QST01",
            edit("p-QST01", &|b| {
                with(
                    b,
                    "list",
                    FieldValue::List(vec![FieldValue::Int(1), FieldValue::Int(2)]),
                )
            }),
            vec!["field:list"],
        ),
        (
            "p-NVZ01",
            edit("p-NVZ01", &|mut b| {
                b.fields.remove("nul");
                b
            }),
            vec!["field:nul"],
        ),
        (
            "p-RFS01",
            edit("p-RFS01", &|mut b| {
                b.links.get_mut("references").unwrap()[0].part = Some("name".into());
                b
            }),
            vec!["links"],
        ),
        (
            "p-B0DY1",
            edit("p-B0DY1", &|b| body(b, "New words.")),
            vec!["text"],
        ),
        (
            "p-B0DY1",
            edit("p-B0DY1", &|b| with(b, "title", s("Titled"))),
            vec!["title"],
        ),
        (
            "p-B0DY1",
            edit("p-B0DY1", &|mut b| {
                b.block_type = "note".into();
                b
            }),
            vec!["type"],
        ),
        (
            "d-AAAAA",
            edit("d-AAAAA", &|b| {
                with(with(b, "doc_type", s("reference")), "everyday", s("Other."))
            }),
            vec![],
        ),
        (
            "d-AAAAA",
            edit("d-AAAAA", &|b| with(b, "description", s("New."))),
            vec!["field:description"],
        ),
        (
            "D-AAAAA",
            edit("D-AAAAA", &|b| with(b, "statement", s("Keep all rows."))),
            vec!["statement"],
        ),
        (
            "p-PNS01",
            edit("p-PNS01", &|b| with(b, "since", s("2.0.0"))),
            vec!["status", "field:since"],
        ),
    ];
    for (id, blocks, want) in cases {
        let next =
            publication::resolve_in_for_tests(&st, &synth(&blocks), publication::IMPLICIT_ORG)
                .unwrap();
        let removals = ReviewRemovals::of_sides(&source, &[&res, &next]).unwrap();
        let (was, now) = (
            ReaderReviewSide::new(&res, &removals),
            ReaderReviewSide::new(&next, &removals),
        );
        let got: Vec<String> =
            changed_parts(was.parts(&bid(id)).unwrap(), now.parts(&bid(id)).unwrap())
                .iter()
                .map(PartName::to_string)
                .collect();
        assert_eq!(got, want, "{id}");
        // equal values have equal hashes, so a hash comparison agrees
        for (name, value) in now.parts(&bid(id)).unwrap().iter() {
            let old = was.parts(&bid(id)).unwrap().get(name);
            assert_eq!(old.hash() == value.hash(), old == value, "{id} {name}");
        }
    }
}

fn json_fixture(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn hex_of(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// The frozen part fixture: for every case, each part's name in order, its tagged form, its
/// exact RFC 8785 text and its BLAKE3 hash. The fixture was written apart from this code;
/// a changed value is a failure, never a new capture.
#[test]
fn drv1_part_hashes_pin_authored_values_and_canonical_bytes() {
    let f = json_fixture("disclosure-review-parts-v1.json");
    // an absent part
    let absent = &f["absent"];
    assert_eq!(ReaderPartValue::Absent.tagged(), absent["tagged"]);
    assert_eq!(
        ReaderPartValue::Absent.canonical(),
        absent["canonical"].as_str().unwrap()
    );
    assert_eq!(
        ReaderPartValue::Absent.hash(),
        absent["hash"].as_str().unwrap()
    );
    let cases = f["cases"].as_array().expect("cases");
    let mut kinds = std::collections::BTreeSet::new();
    let mut covered = std::collections::BTreeSet::new();
    let mut seen = 0;
    for case in cases {
        let name = case["case"].as_str().unwrap();
        kinds.insert(case["kind"].as_str().unwrap().to_string());
        let b: Block =
            serde_json::from_value(case["block"].clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
        covered.insert(b.block_type.clone());
        let parts = BlockParts::of(&b, case["status"].as_str());
        let want = case["parts"].as_array().unwrap();
        let got: Vec<String> = parts.iter().map(|(n, _)| n.to_string()).collect();
        let names: Vec<&str> = want.iter().map(|p| p["part"].as_str().unwrap()).collect();
        assert_eq!(got, names, "{name}");
        for (p, (_, value)) in want.iter().zip(parts.iter()) {
            let at = format!("{name} {}", p["part"]);
            let canonical = p["canonical"].as_str().unwrap();
            let hash = p["hash"].as_str().unwrap();
            // the fixture is consistent with itself: its hash is BLAKE3 of its bytes, and its
            // canonical text is JSON of its tagged form
            assert_eq!(hex_of(canonical.as_bytes()), hash, "{at}");
            assert_eq!(
                serde_json::from_str::<Value>(canonical).unwrap(),
                p["tagged"],
                "{at}"
            );
            assert_eq!(hash.len(), 64, "{at}");
            assert!(
                hash.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')),
                "{at}"
            );
            // and the code gives the same value, bytes and hash
            assert_eq!(value.tagged(), p["tagged"], "{at}");
            assert_eq!(value.canonical(), canonical, "{at}");
            assert_eq!(value.hash(), hash, "{at}");
            seen += 1;
        }
    }
    assert_eq!(seen, 132, "every pinned part was compared");
    let kinds: Vec<String> = kinds.into_iter().collect();
    assert_eq!(kinds, vec!["block", "decision", "doc", "folder", "outside"]);
    for ty in [
        "term",
        "component",
        "actor",
        "interface",
        "entity",
        "value",
        "question",
        "diagram",
        "figure",
        "table",
        "flow",
        "prose",
        "code",
        "doc",
        "folder",
        "decision",
    ] {
        assert!(covered.contains(ty), "{ty}");
    }
    // the edge values the fixture pins
    let canon_of = |case: &str, part: &str| -> String {
        let c = cases.iter().find(|c| c["case"] == case).unwrap();
        let p = c["parts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["part"] == part)
            .unwrap();
        p["canonical"].as_str().unwrap().to_string()
    };
    assert_eq!(canon_of("prose-structural-names", "title"), "[\"null\"]");
    assert_eq!(canon_of("prose-typed-values", "title"), "[\"null\"]");
    assert_eq!(
        canon_of("prose-typed-values", "field:past"),
        "[\"int\",\"9007199254740993\"]"
    );
    assert_eq!(
        canon_of("prose-typed-values", "field:below"),
        "[\"int\",\"-9007199254740993\"]"
    );
    assert!(
        canon_of("prose-typed-values", "field:map")
            .contains("\"\u{1f600}\":[\"str\",\"emoji\"],\"\u{ff61}\""),
        "UTF-16 key order"
    );
    assert!(
        canon_of("prose-typed-values", "field:controls").starts_with("[\"str\",\"\\u0000\\u0001")
    );
    let links = canon_of("prose-links-pins-status", "links");
    assert!(links.contains("[[\"map\",{\"target\":[\"str\",\"t-BBBBB\"]}],[\"map\",{\"part\":[\"str\",\"name\"],\"target\":[\"str\",\"t-AAAAA\"]}]]"), "{links}");
    assert!(!links.contains("pins"));
    for structural in ["type", "links", "body", "order", "status"] {
        let field = canon_of("prose-structural-names", &format!("field:{structural}"));
        assert_eq!(field, format!("[\"str\",\"custom {structural}\"]"));
    }
}

/// The standard pack with one more content type, `memo`.
fn with_memo() -> Pack {
    let mut set = PackSet::with_builtin();
    let name = set
            .add_yaml(
                "name: team\nversion: \"1\"\nextends: rdy-standard\nblock_types:\n  memo:\n    prefix: mm\n    traits: [informative]\n    body: required\n",
            )
            .unwrap();
    set.resolve(&name).unwrap()
}

fn hash_of(v: &Value) -> Hash {
    v.as_str().expect("a hash").parse().expect("64 hex digits")
}

/// Each fixed input block: its encoding-0 form, or for a removed version its stub, rebuilt
/// with `Block::redacted` (the serde form drops a stub's identity).
fn input_block(input: &Value) -> Block {
    let b: Block = serde_json::from_value(input["block"].clone()).expect("a block");
    let Some(stub) = input.get("stub") else {
        return b;
    };
    let bytes = |k: &str| stub[k]["of_bytes"].as_str().unwrap().as_bytes().to_vec();
    let version = Hash::of(&bytes("version"));
    let content_hash = Hash::of(&bytes("content_hash"));
    assert_eq!(version, hash_of(&stub["version"]["hash"]));
    assert_eq!(content_hash, hash_of(&stub["content_hash"]["hash"]));
    Block::redacted(
        b.id,
        b.block_type,
        b.parent,
        stub["order"].as_str().unwrap().to_string(),
        RedactedBlockIdentity {
            version,
            content_hash,
            frozen: stub["frozen"].as_bool().unwrap(),
            request: hash_of(&stub["request"]),
        },
    )
}

/// The pre-change hash vectors, captured at the execution base before any review code: each
/// case resolved again and viewed through the reader seam keeps every selection hash,
/// selection identity and doc digest. The fixture never changes to fit the code.
#[test]
fn drv1_reader_projection_preserves_frozen_hashes() {
    let f = json_fixture("disclosure-review-hashes-v1.json");
    let (_dir, mut r) = project();
    for held in f["held_blobs"].as_array().unwrap() {
        let h = r
            .store_mut()
            .put_blob(held["bytes"].as_str().unwrap().as_bytes())
            .unwrap();
        assert_eq!(h, hash_of(&held["hash"]));
    }
    let absent = &f["absent_blob"];
    assert_eq!(
        Hash::of(absent["bytes"].as_str().unwrap().as_bytes()),
        hash_of(&absent["hash"])
    );
    let request = &f["removal_request"];
    assert_eq!(
        Hash::of(request["bytes"].as_str().unwrap().as_bytes()),
        hash_of(&request["hash"])
    );
    let st = r.read();
    let memo = with_memo();

    let mut states = BTreeMap::new();
    for state in f["states"].as_array().unwrap() {
        let blocks: Vec<Block> = state["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|input| {
                let b = input_block(input);
                assert_eq!(
                    b.version_hash(),
                    hash_of(&input["version_hash"]),
                    "{}",
                    b.id
                );
                assert_eq!(
                    b.is_redacted(),
                    input["redacted"].as_bool().unwrap(),
                    "{}",
                    b.id
                );
                b
            })
            .collect();
        states.insert(state["state"].as_str().unwrap().to_string(), synth(&blocks));
    }
    assert_eq!(states.len(), 5);

    let (mut cases, mut digests) = (0, 0);
    for case in f["cases"].as_array().unwrap() {
        let label = case["case"].as_str().unwrap();
        let m = &states[case["state"].as_str().unwrap()];
        let profile = case["profile_request"].as_str().unwrap();
        let (res, source) = match case["pack"].as_str().unwrap() {
            "rdy-standard" => (
                publication::resolve_in_for_tests(&st, m, profile).unwrap(),
                ReadPublicationSource::with_settings(&st, st.pack(), "Project", None),
            ),
            "rdy-standard + memo" => {
                let source = ReadPublicationSource::with_settings(&st, &memo, "Project", None);
                (
                    publication::resolve_source(&source, m, profile).unwrap(),
                    source,
                )
            }
            other => panic!("{label}: pack {other}"),
        };
        let removals = ReviewRemovals::of_sides(&source, &[&res]).unwrap();
        let view = ReaderReviewSide::new(&res, &removals);
        let lookup = ReferenceLookup::new(m, &removals);
        for b in m.iter() {
            let _ = lookup.label(&b.id);
        }
        for facts in [
            (res.selection_hash, res.selection_identity, res.docs.clone()),
            (
                view.selection().selection_hash,
                view.selection().selection_identity,
                view.selection().docs.clone(),
            ),
        ] {
            assert_eq!(facts.0, hash_of(&case["selection_hash"]), "{label}");
            assert_eq!(facts.1, hash_of(&case["selection_identity"]), "{label}");
            let want = case["docs"].as_array().unwrap();
            assert_eq!(facts.2.len(), want.len(), "{label}");
            for (d, w) in facts.2.iter().zip(want) {
                assert_eq!(d.id.to_string(), w["id"].as_str().unwrap(), "{label}");
                assert_eq!(d.path, w["path"].as_str().unwrap(), "{label}");
                assert_eq!(d.digest, hash_of(&w["digest"]), "{label} {}", d.id);
                assert_eq!(
                    d.redacted,
                    w["redacted"].as_bool().unwrap(),
                    "{label} {}",
                    d.id
                );
            }
        }
        // the resolution itself is the captured one
        let entries: Vec<(String, String)> = res
            .entries
            .iter()
            .map(|e| (e.id.to_string(), e.version.to_string()))
            .collect();
        let want: Vec<(String, String)> = case["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                (
                    e["id"].as_str().unwrap().into(),
                    e["version"].as_str().unwrap().into(),
                )
            })
            .collect();
        assert_eq!(entries, want, "{label}");
        cases += 1;
        digests += res.docs.len();
    }
    assert_eq!((cases, digests), (15, 33));
}
