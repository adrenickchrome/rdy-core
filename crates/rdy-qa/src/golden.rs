//! The golden fixture of the `render_quality` area: the structural pattern matching proposals
//! (a design corpus with a specification, its rationale, a tutorial and the proposals that were
//! set aside) as a format-2 tree under `fixtures/pattern-matching/`, loaded with `rdy import`,
//! and the helpers that build projects, sites and the two older `render` area projects.
//!
//! Every block of the committed tree has an explicit id, so [`Ids`] is a table of constants.
use crate::fixtures;
pub use crate::fixtures::{approver, land, raw, raw_commit, write, writer};
use rdy_engine::{DecisionInput, Rdy, RdyError, Scope, Session, WriteRequest, WriteResult};
use rdy_model::{BlockId, FieldValue, Hash, Link};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// The committed export: `rdy-export.yaml`, `docs/` and `blobs/`.
pub const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/pattern-matching");

/// The keys the fixture gives blocks (the DX2 content): a requirement, a section and a decision.
pub const KEYS: [&str; 3] = ["REQ-SOFT-KEYWORDS", "SPEC-GUARDS", "DEC-WILDCARD"];

/// Text that is hostile in every context a renderer writes it into (the `render` area).
pub const EVIL: &str = "<script>alert(1)</script>\"'><img src=x onerror=alert(2)>&amp;";

const SITE_NAME: &str = "Structural pattern matching";
const SITE_DESCRIPTION: &str = "The match statement for Python 3.10: specification, rationale, tutorial and the proposals that were set aside.";

/// Declares [`Ids`]: the fixed id of every block a test names. The `canon` group is in the
/// landed corpus; the `view` group exists only in the `editing` view (a landing would refuse a
/// dangling reference and would turn a proposed decision into an accepted one).
macro_rules! ids {
    (canon { $($c:ident: $cid:literal,)* } view { $($v:ident: $vid:literal,)* }) => {
        #[derive(Clone, Debug)]
        pub struct Ids {
            $(pub $c: BlockId,)*
            $(pub $v: BlockId,)*
        }
        impl Ids {
            /// The ids as written in the fixture.
            pub fn new() -> Ids {
                Ids {
                    $($c: BlockId::parse($cid).expect("a fixture id"),)*
                    $($v: BlockId::parse($vid).expect("a fixture id"),)*
                }
            }
            /// Every block of the landed corpus, with its field name.
            pub fn named(&self) -> Vec<(&'static str, &BlockId)> {
                vec![$((stringify!($c), &self.$c),)*]
            }
            /// The blocks that exist only in the `editing` view, with their field names.
            pub fn view_named(&self) -> Vec<(&'static str, &BlockId)> {
                vec![$((stringify!($v), &self.$v),)*]
            }
        }
        impl Default for Ids {
            fn default() -> Ids {
                Ids::new()
            }
        }
    };
}

ids! {
    canon {
        overview_doc: "d-JBCJC",
        spec_doc: "d-8PBD5",
        rationale_doc: "d-3T4TW",
        tutorial_doc: "d-XM01W",
        evaluation_doc: "d-6TCS9",
        pep640_doc: "d-ZJWAW",
        pep642_doc: "d-16X0K",
        pep653_doc: "d-1BGP0",
        greek_doc: "d-FCG1T",
        long_title_doc: "d-7Z6JV",
        spec_folder: "fd-MJN6T",
        rationale_folder: "fd-W4CGA",
        tutorial_folder: "fd-27D0Y",
        proposals_folder: "fd-CXCY0",
        appendix_folder: "fd-ZY569",
        terms_section: "s-C7X9Z",
        guards_section: "s-EF1NX",
        terms_section_dup: "s-Z1QH6",
        guard_term: "t-XTQDJ",
        wildcard_term: "t-V8Q67",
        match_args_term: "t-8SX2E",
        match_component: "m-3ZB5J",
        grammar_interface: "i-WJ5ZR",
        author_actor: "a-MPEDJ",
        algorithm_flow: "f-KEF71",
        pattern_table: "tb-T4K0D",
        nested_table: "tb-500MS",
        wide_table: "tb-S8DXP",
        grammar_diagram: "dg-J21FA",
        flow_diagram: "dg-2MF0M",
        image_figure: "fg-TFCW2",
        file_figure: "fg-Z4AYY",
        example_block: "e-KN4GK",
        note_block: "n-MMVXD",
        rationale_block: "ra-ECFQR",
        soft_keywords_requirement: "r-CMSDK",
        must_requirement: "r-C6ZE9",
        should_requirement: "r-2KZGG",
        may_requirement: "r-8GYSR",
        bindings_claim: "c-JYHQT",
        length_constraint: "k-9CR0X",
        long_token_prose: "p-CMY61",
        big_code: "cd-EKTRM",
        brace_code: "cd-1R2V4",
        open_question: "Q-ADQZS",
        answered_question: "Q-EXNX0",
        wildcard_decision: "D-1RRV2",
        or_decision: "D-EDT1K",
        soft_keyword_decision: "D-G83ZN",
        match_args_decision: "D-QK28X",
        rejected_640_decision: "D-FD7AD",
        rejected_642_decision: "D-KN01Z",
        superseded_622_decision: "D-8XSBD",
        superseding_634_decision: "D-7Y52G",
        revoked_decision: "D-E4V9K",
        presentational_note: "n-83VA8",
        or_term: "t-WNTNS",
        revoking_decision: "D-J3BYE",
        motivation_prose: "p-97F12",
        guards_prose: "p-Q4N8W",
        tutorial_prose: "p-48JMX",
        overview_context_section: "s-ZSB5R",
        evaluation_order_section: "s-Y6THC",
        capture_section: "s-PZFV5",
        sequence_section: "s-PZK7F",
        opening_prose: "p-EF50Z",
        opening_list: "p-Q6S0X",
        opening_steps: "p-38BJB",
        opening_note: "n-ZHA5J",
        guards_markdown_prose: "p-W97Y4",
        quick_section: "s-2N76F",
        langless_code: "cd-ZNYMB",
        altless_diagram: "dg-HBT4Q",
    }
    view {
        dangling_ref_prose: "p-Y3BEZ",
        dangling_diagram: "dg-K4G0E",
        proposed_decision: "D-3Z81J",
        overview_note: "n-40PR2",
    }
}

/// The ids, after checking that every one of them is in canon.
pub fn ids(r: &Rdy) -> Ids {
    let ids = Ids::new();
    let m = r.read().materialized(&Scope::Canon).expect("canon");
    for (name, id) in ids.named() {
        assert!(m.get(id).is_some(), "{name} ({id}) is not in canon");
    }
    ids
}

fn s(id: &BlockId) -> String {
    id.to_string()
}

/// Sixty appendix docs under the appendix folder (one section and one prose block each), so
/// the sidebar's cap is exercised.
pub fn generate_appendix(r: &mut Rdy, view: &str) {
    let ids = Ids::new();
    let mut ops: Vec<Value> = vec![];
    for n in 1..=60 {
        let (doc, sec) = (format!("$a{n}"), format!("$as{n}"));
        ops.push(json!({"op": "create", "type": "doc", "id": doc, "parent": s(&ids.appendix_folder),
            "fields": {"name": format!("appendix-{n}"), "title": format!("Appendix {n}"), "doc_type": "overview"}}));
        ops.push(json!({"op": "create", "type": "section", "id": sec, "parent": doc, "fields": {"title": "Notes"}}));
        ops.push(json!({"op": "create", "type": "prose", "parent": sec,
            "body": format!("Reference note number {n}: one short paragraph, so that the folder page has sixty entries to list.")}));
    }
    write(r, view, json!(ops)).expect("appendix");
}

/// A chain of folders under `specification/` as deep as folders may nest (`deep`, then `d1` to
/// `d30`), holding one doc, "The bottom".
pub fn generate_deep_chain(r: &mut Rdy, view: &str) {
    let ids = Ids::new();
    let mut ops: Vec<Value> = vec![];
    let mut parent = s(&ids.spec_folder);
    for n in 0..=30 {
        let (var, name) = (
            format!("$f{n}"),
            if n == 0 {
                "deep".to_string()
            } else {
                format!("d{n}")
            },
        );
        ops.push(
            json!({"op": "create", "type": "folder", "id": var, "parent": parent,
            "fields": {"name": name, "title": format!("Depth {n}")}}),
        );
        parent = var;
    }
    ops.push(
        json!({"op": "create", "type": "doc", "id": "$bottom", "parent": parent,
        "fields": {"name": "bottom", "title": "The bottom", "doc_type": "overview"}}),
    );
    ops.push(json!({"op": "create", "type": "section", "id": "$bs", "parent": "$bottom", "fields": {"title": "Summary"}}));
    ops.push(json!({"op": "create", "type": "prose", "parent": "$bs",
        "body": "The last folder of the deepest chain a project may hold."}));
    write(r, view, json!(ops)).expect("deep chain");
}

/// When the fixtures are written: 5 October 2026, 09:00 UTC. A fixture pins its thread's clock
/// here while it writes, so every version, landing and decision date it renders is the same on
/// every run (gold1).
pub const FIXTURE_MS: i64 = 1_791_190_800_000;

/// Pins this thread's clock at [`FIXTURE_MS`] until the guard drops.
pub fn fixture_clock() -> rdy_engine::PinnedClock {
    rdy_engine::pin_clock(FIXTURE_MS)
}

/// The golden project: the pattern matching corpus, landed and tagged `v1.0.0`, two landings
/// after it, and a view `editing` with two unlanded changes. Canon reads
/// `v1.0.0 + 2 landings`, the view `+ 2 unlanded changes`.
pub fn project() -> (tempfile::TempDir, Rdy, Ids) {
    let _clock = fixture_clock();
    let dir = tempfile::tempdir().expect("tempdir");
    // the store is locked while a handle is open: close it before the config is rewritten
    drop(Rdy::init(dir.path(), "rdy-standard").expect("init"));
    // `name` and `description` are the site's; the engine reads them from a later task on and
    // ignores unknown keys until then
    std::fs::write(
        dir.path().join(".rdy/config.yaml"),
        format!("pack: rdy-standard\nname: {SITE_NAME:?}\ndescription: {SITE_DESCRIPTION:?}\n"),
    )
    .expect("config");
    let mut r = Rdy::open(dir.path()).expect("open");
    r.import(&approver(), "import", Path::new(FIXTURE_DIR))
        .expect("the fixture imports (the diagnostics name the file and line)");
    land(&mut r, "import").expect("land the corpus");
    generate_appendix(&mut r, "generated");
    generate_deep_chain(&mut r, "generated");
    land(&mut r, "generated").expect("land the generated pages");
    r.tag(
        &approver(),
        "v1.0.0",
        None,
        Some("Accepted by the Steering Council".into()),
    )
    .expect("tag v1.0.0");
    let ids = ids(&r);
    for (view, id, body) in [
        (
            "clarify",
            &ids.motivation_prose,
            "Three idioms recur in Python code and each is clumsier than it needs to be. The motivation is easier to state once the idioms are named: a chain of tests, a walk over a syntax tree and a loader for structured data.",
        ),
        (
            "tighten",
            &ids.guards_prose,
            "Patterns say what the subject must look like. Conditions that no pattern can express, such as one value being less than another, belong in a guard.",
        ),
    ] {
        write(
            &mut r,
            view,
            json!([{"op": "update", "id": s(id), "body": body}]),
        )
        .expect("prose update");
        land(&mut r, view).expect("land the update");
    }
    write(&mut r, "editing", json!([{"op": "update", "id": s(&ids.tutorial_prose),
        "body": "So far we have used three simple patterns: capture patterns, which are bare names like `direction`; literal patterns; and the wildcard `_`. They compose."}]))
    .expect("the first unlanded change");
    raw_commit(&mut r, "editing", editing_blocks(&ids));
    (dir, r, ids)
}

/// The blocks of the second unlanded change, which no write or landing would accept.
fn editing_blocks(ids: &Ids) -> Vec<rdy_model::Block> {
    let note = raw(
        ids.overview_note.as_str(),
        "note",
        Some(ids.overview_context_section.as_str()),
        "b1",
        &[],
        "A note added while the overview is being edited.",
    );
    let dangling = raw(
        ids.dangling_ref_prose.as_str(),
        "prose",
        Some(ids.capture_section.as_str()),
        "b1",
        &[],
        "A half-written sentence: see {{p-ZZZZZ}} for the history.",
    );
    let diagram = raw(
        ids.dangling_diagram.as_str(),
        "diagram",
        Some(ids.evaluation_order_section.as_str()),
        "b1",
        &[("lang", "mermaid"), ("alt", "A broken reference")],
        "flowchart TD\n  A[{{p-ZZZZZ}}] --> B[Guard]",
    );
    let mut decision = raw(
        ids.proposed_decision.as_str(),
        "decision",
        Some(ids.sequence_section.as_str()),
        "b1",
        &[
            (
                "statement",
                "A `__match_container__` attribute replaces the abstract-base-class test",
            ),
            ("status", "proposed"),
        ],
        "",
    );
    decision.fields.insert(
        "options".into(),
        FieldValue::List(vec![
            FieldValue::Str("an integer attribute".into()),
            FieldValue::Str("the abstract base class".into()),
        ]),
    );
    decision.links.insert(
        "affects".into(),
        vec![Link {
            target: ids.length_constraint.clone(),
            part: None,
        }],
    );
    vec![note, dangling, diagram, decision]
}

/// [`project`], then the site of `scope` rendered into the fourth value.
pub fn site(scope: &Scope) -> (tempfile::TempDir, Rdy, Ids, tempfile::TempDir) {
    let (dir, r, ids) = project();
    let out = tempfile::tempdir().expect("tempdir");
    rdy_render::render_site(&r.read(), scope, out.path()).expect("render the fixture");
    (dir, r, ids, out)
}

/// The site of canon.
pub fn site_canon() -> (tempfile::TempDir, Rdy, Ids, tempfile::TempDir) {
    site(&Scope::Canon)
}

/// The frozen site of `v1.0.0`'s landing.
pub fn site_frozen() -> (tempfile::TempDir, Rdy, Ids, tempfile::TempDir) {
    let (dir, r, ids) = project();
    let landing: Hash = r.read().versions().expect("versions")[0]
        .landing
        .parse()
        .expect("a landing hash");
    let out = tempfile::tempdir().expect("tempdir");
    rdy_render::render_site(&r.read(), &Scope::At(landing), out.path())
        .expect("render the fixture");
    (dir, r, ids, out)
}

/// The site of the `editing` view.
pub fn site_view() -> (tempfile::TempDir, Rdy, Ids, tempfile::TempDir) {
    site(&Scope::View("editing".into()))
}

/// The page `page` (a path relative to the site root) of a rendered site.
pub fn html(site: &Path, page: &str) -> String {
    let p = site.join(page);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// Every `.html` page of a rendered site, sorted.
pub fn pages(site: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let mut stack = vec![site.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p)
            } else if p.extension().is_some_and(|x| x == "html") {
                out.push(p)
            }
        }
    }
    out.sort();
    out
}

/// The `render` area's hostile project: every context a renderer writes text into, holding
/// [`EVIL`], landed and tagged.
pub fn hostile() -> (tempfile::TempDir, Rdy) {
    let _clock = fixture_clock();
    let (d, mut r) = fixtures::project();
    let blob = r.store_mut().put_blob(b"png").unwrap().to_string();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "evil", "title": EVIL, "description": EVIL}},
        {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"name": "page", "title": EVIL, "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": EVIL}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": EVIL}, "body": EVIL},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": format!("{EVIL} {{{{$t}}}}")},
        {"op": "create", "type": "table", "parent": "$s", "fields": {"columns": [EVIL], "rows": [[EVIL]]}},
        {"op": "create", "type": "flow", "parent": "$s", "fields": {"name": EVIL, "steps": [EVIL]}},
        {"op": "create", "type": "diagram", "parent": "$s", "fields": {"lang": "mermaid", "alt": EVIL}, "body": format!("graph TD\n A[{EVIL}]")},
        {"op": "create", "type": "code", "parent": "$s", "fields": {"lang": EVIL}, "body": EVIL},
        {"op": "create", "type": "figure", "parent": "$s", "fields": {"blob": blob, "media_type": "image/png", "alt": EVIL}}
    ])).unwrap();
    r.decide(
        &writer(),
        "work",
        DecisionInput {
            parent: res.created["$s"].clone(),
            statement: EVIL.into(),
            options: vec![EVIL.into()],
            body: EVIL.into(),
            affects: vec![res.created["$c"].clone()],
            ..Default::default()
        },
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    r.tag(&approver(), "v0.1.0", None, Some(EVIL.into()))
        .unwrap();
    (d, r)
}

/// The `render` area's generated tree: three nested folders of three docs each, cross-linked
/// terms, a figure, a diagram and three decisions (one superseded), landed and tagged `v1.0.0`,
/// plus a view `draft` holding one unlanded doc.
pub fn generated_tree() -> (tempfile::TempDir, Rdy) {
    let _clock = fixture_clock();
    let (d, mut r) = fixtures::project();
    let blob = r.store_mut().put_blob(b"png").unwrap().to_string();
    let mut ops = vec![
        json!({"op": "create", "type": "folder", "id": "$fa", "fields": {"name": "a", "title": "A"}}),
        json!({"op": "create", "type": "folder", "id": "$fb", "parent": "$fa", "fields": {"name": "b", "title": "B"}}),
        json!({"op": "create", "type": "folder", "id": "$fc", "parent": "$fb", "fields": {"name": "c", "title": "C"}}),
    ];
    let folders = ["$fa", "$fb", "$fc"];
    for (fi, f) in folders.iter().enumerate() {
        for d in 0..3 {
            let (doc, sec, term) = (
                format!("$d{fi}{d}"),
                format!("$s{fi}{d}"),
                format!("$t{fi}{d}"),
            );
            ops.push(json!({"op": "create", "type": "doc", "id": doc, "parent": f, "fields": {"title": format!("Doc {fi}{d}"), "doc_type": "glossary"}}));
            ops.push(json!({"op": "create", "type": "section", "id": sec, "parent": doc, "fields": {"title": "Terms"}}));
            ops.push(json!({"op": "create", "type": "term", "id": term, "parent": sec, "fields": {"name": format!("Term{fi}{d}")}, "body": "A term."}));
        }
    }
    for fi in 0..3 {
        for d in 0..3 {
            let (sec, term) = (format!("$s{fi}{d}"), format!("$t{fi}{d}"));
            // cross-doc refs to other folders and depths
            let other = format!("$t{}{}", (fi + 1) % 3, d);
            let far = format!("$t{}{}", (fi + 2) % 3, (d + 1) % 3);
            ops.push(json!({"op": "create", "type": "claim", "parent": sec, "body": format!("Uses {{{{{other}}}}} and {{{{{far}}}}} and {{{{{term}}}}}.")}));
        }
    }
    ops.push(json!({"op": "create", "type": "figure", "parent": "$s22", "fields": {"blob": blob, "media_type": "image/png", "alt": "pic"}}));
    ops.push(json!({"op": "create", "type": "diagram", "parent": "$s22", "fields": {"lang": "mermaid", "alt": "d"}, "body": "graph TD\n A[{{$t00}}] --> B"}));
    let res = write(&mut r, "work", json!(ops)).unwrap();
    let sec = |k: &str| res.created[k].clone();
    let dec = |statement: &str, parent: String, affects: Vec<String>| DecisionInput {
        parent,
        statement: statement.into(),
        options: vec!["yes".into(), "no".into()],
        affects,
        body: "Because.".into(),
        ..Default::default()
    };
    let d1 = r
        .decide(
            &writer(),
            "work",
            dec("First", sec("$s00"), vec![sec("$t22")]),
        )
        .unwrap();
    let d1_id = d1.created.values().next().expect("decision id").clone();
    r.decide(
        &writer(),
        "work",
        dec("Second", sec("$s11"), vec![sec("$t00")]),
    )
    .unwrap();
    r.supersede(
        &writer(),
        "work",
        &d1_id,
        None,
        dec("Third supersedes first", sec("$s22"), vec![sec("$t11")]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    // unlanded changes in a view
    write(&mut r, "draft", json!([
        {"op": "create", "type": "doc", "parent": sec("$fc"), "fields": {"title": "Draft doc", "doc_type": "glossary"}},
    ])).unwrap();
    (d, r)
}

/// [`fixtures::write`] as `s` rather than as the writer (an approver sets `status` and
/// `decided_at` on a decision).
pub fn write_as(s: &Session, r: &mut Rdy, view: &str, ops: Value) -> Result<WriteResult, RdyError> {
    let ops = serde_json::from_value(ops)
        .map_err(|e| RdyError::new(rdy_engine::E_BAD_INPUT, e.to_string(), "fix the test's ops"))?;
    r.write(
        s,
        WriteRequest {
            view: view.into(),
            ops,
            justification: None,
            message: String::new(),
        },
    )
}

/// An empty project named `name`: init, then `name:` into the config, then open (the store is
/// locked while a handle is open, so the first handle is dropped).
fn named_project(name: &str) -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().expect("tempdir");
    drop(Rdy::init(dir.path(), "rdy-standard").expect("init"));
    std::fs::write(
        dir.path().join(".rdy/config.yaml"),
        format!("pack: rdy-standard\nname: {name:?}\n"),
    )
    .expect("config");
    let r = Rdy::open(dir.path()).expect("open");
    (dir, r)
}

/// An empty project named `Tiny`.
pub fn tiny() -> (tempfile::TempDir, Rdy) {
    let _clock = fixture_clock();
    named_project("Tiny")
}

/// The id of the first doc at the top level of canon, in tree order.
pub fn first_doc_id(r: &Rdy) -> BlockId {
    let m = r.read().materialized(&Scope::Canon).expect("canon");
    m.children(None)
        .into_iter()
        .find(|b| b.block_type == rdy_model::tree::DOC)
        .map(|b| b.id.clone())
        .expect("canon holds a doc")
}

/// Writes and lands one doc, "Identity" (a glossary), with sections "Terms" and "Notes", the
/// first holding one prose block.
pub fn small_site(r: &mut Rdy) {
    write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Identity", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
        {"op": "create", "type": "prose", "parent": "$s", "body": "Who the ledger belongs to."}
    ]))
    .expect("small site");
    land(r, "work").expect("land the small site");
}

/// `Tiny`, with one doc, "Identity", with two sections, landed.
pub fn tiny_with_doc() -> (tempfile::TempDir, Rdy) {
    let _clock = fixture_clock();
    let (d, mut r) = tiny();
    write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Identity", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "parent": "$d", "fields": {"title": "Terms"}},
        {"op": "create", "type": "section", "parent": "$d", "fields": {"title": "Notes"}}
    ]))
    .expect("the doc");
    land(&mut r, "work").expect("land the doc");
    (d, r)
}

/// The blocks of [`site_with_decision`].
#[derive(Clone, Debug)]
pub struct WalletIds {
    pub doc: BlockId,
    pub folder: BlockId,
    pub section: BlockId,
    pub term: BlockId,
    pub claim: BlockId,
    pub question: BlockId,
    pub decision: BlockId,
}

fn created(res: &WriteResult, key: &str) -> BlockId {
    BlockId::parse(&res.created[key]).expect("a created id")
}

/// Project `Wallets`: folder `wallets`, doc `transactions` ("Transactions"), section "Guards",
/// term "Ledger", a claim referencing the decision and the term, a question and a decision that
/// affects the term; landed.
pub fn site_with_decision() -> (tempfile::TempDir, Rdy, WalletIds) {
    let _clock = fixture_clock();
    let (d, mut r) = named_project("Wallets");
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "wallets", "title": "Wallets"}},
        {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"name": "transactions", "title": "Transactions", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Guards"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Balances."},
        {"op": "create", "type": "question", "id": "$q", "parent": "$s", "fields": {"prompt": "Is a ledger a sequence?"}}
    ]))
    .expect("the wallets");
    let (section, term) = (created(&res, "$s"), created(&res, "$t"));
    let dec = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent: s(&section),
                statement: "Ledgers are atomic".into(),
                affects: vec![s(&term)],
                ..Default::default()
            },
        )
        .expect("the decision");
    let decision = created(&dec, "$decision");
    let claim = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "claim", "id": "$c", "parent": s(&section),
             "body": format!("See {{{{{decision}}}}} and {{{{{term}}}}}.")}
        ]),
    )
    .expect("the claim");
    land(&mut r, "work").expect("land the wallets");
    let ids = WalletIds {
        doc: created(&res, "$d"),
        folder: created(&res, "$f"),
        section,
        term,
        claim: created(&claim, "$c"),
        question: created(&res, "$q"),
        decision,
    };
    (d, r, ids)
}

/// Project `Tree`; top level in order: doc `intro` ("Intro", one section "Summary" with one prose
/// block), folder `a` ("Alpha": doc `one` with a section "Terms" holding the term "Ledger", doc
/// `two` with one section), folder `b` ("Beta": folder `c` ("Gamma") holding doc `three` with
/// five sections, the third holding a question); landed.
pub fn tree_site() -> (tempfile::TempDir, Rdy) {
    let _clock = fixture_clock();
    let (d, mut r) = named_project("Tree");
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$intro", "fields": {"name": "intro", "title": "Intro", "doc_type": "overview"}}),
        json!({"op": "create", "type": "section", "id": "$is", "parent": "$intro", "fields": {"title": "Summary"}}),
        json!({"op": "create", "type": "prose", "parent": "$is", "body": "Where to start."}),
        json!({"op": "create", "type": "folder", "id": "$a", "fields": {"name": "a", "title": "Alpha"}}),
        json!({"op": "create", "type": "folder", "id": "$b", "fields": {"name": "b", "title": "Beta"}}),
        json!({"op": "create", "type": "doc", "id": "$one", "parent": "$a", "fields": {"name": "one", "title": "One", "doc_type": "glossary"}}),
        json!({"op": "create", "type": "section", "id": "$os", "parent": "$one", "fields": {"title": "Terms"}}),
        json!({"op": "create", "type": "term", "parent": "$os", "fields": {"name": "Ledger"}, "body": "Balances are derived."}),
        json!({"op": "create", "type": "doc", "id": "$two", "parent": "$a", "fields": {"name": "two", "title": "Two", "doc_type": "overview"}}),
        json!({"op": "create", "type": "section", "parent": "$two", "fields": {"title": "Summary"}}),
        json!({"op": "create", "type": "folder", "id": "$c", "parent": "$b", "fields": {"name": "c", "title": "Gamma"}}),
        json!({"op": "create", "type": "doc", "id": "$three", "parent": "$c", "fields": {"name": "three", "title": "Three", "doc_type": "overview"}}),
    ];
    for n in 1..=5 {
        ops.push(json!({"op": "create", "type": "section", "id": format!("$ts{n}"), "parent": "$three", "fields": {"title": format!("Part {n}")}}));
    }
    ops.push(json!({"op": "create", "type": "question", "parent": "$ts3", "fields": {"prompt": "Is a ledger a sequence?"}}));
    write(&mut r, "work", json!(ops)).expect("the tree");
    land(&mut r, "work").expect("land the tree");
    (d, r)
}

/// Project `Many`: folder `many` holding `n` docs, `Doc 1` to `Doc n` (names `doc-1`...); landed.
pub fn wide_folder(n: usize) -> (tempfile::TempDir, Rdy) {
    let _clock = fixture_clock();
    let (d, mut r) = named_project("Many");
    let mut ops = vec![
        json!({"op": "create", "type": "folder", "id": "$f", "fields": {"name": "many", "title": "Many"}}),
    ];
    for i in 1..=n {
        ops.push(json!({"op": "create", "type": "doc", "parent": "$f",
            "fields": {"name": format!("doc-{i}"), "title": format!("Doc {i}"), "doc_type": "overview"}}));
    }
    write(&mut r, "work", json!(ops)).expect("the wide folder");
    land(&mut r, "work").expect("land the wide folder");
    (d, r)
}

/// The blocks of [`every_type_site`].
#[derive(Clone, Debug)]
pub struct TypeIds {
    pub doc: BlockId,
    pub section: BlockId,
    pub subsection: BlockId,
    pub prose: BlockId,
    pub requirement: BlockId,
    pub claim: BlockId,
    pub constraint: BlockId,
    pub example: BlockId,
    pub note: BlockId,
    pub rationale: BlockId,
    pub term: BlockId,
    pub component: BlockId,
    pub actor: BlockId,
    pub interface: BlockId,
    pub flow: BlockId,
    pub table: BlockId,
    pub diagram: BlockId,
    pub figure: BlockId,
    pub code: BlockId,
    pub question: BlockId,
    pub decision: BlockId,
    /// Exists only in the view `dangling`: it references an id that does not exist.
    pub dangling_prose: BlockId,
}

/// Project `All`: doc "All" (`all`), a section "Terms" and a sub-section holding the component
/// and the flow, then one block of every type in the order prose, requirement, claim,
/// constraint, example, note, rationale, term, component, actor, interface, flow, table,
/// diagram, figure, code, question; a decision that affects the requirement; landed. A prose
/// block with a dangling reference is in the view `dangling`, never landed.
pub fn every_type_site() -> (tempfile::TempDir, Rdy, TypeIds) {
    let _clock = fixture_clock();
    let (d, mut r) = named_project("All");
    let blob = r
        .store_mut()
        .put_blob(b"\x89PNG fake")
        .expect("blob")
        .to_string();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$doc", "fields": {"name": "all", "title": "All", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$sec", "parent": "$doc", "fields": {"title": "Terms"}},
        {"op": "create", "type": "prose", "id": "$prose", "parent": "$sec", "body": "Plain words."},
        {"op": "create", "type": "requirement", "id": "$req", "parent": "$sec", "fields": {"priority": "must"}, "body": "It must."},
        {"op": "create", "type": "claim", "id": "$claim", "parent": "$sec", "body": "Balances are derived."},
        {"op": "create", "type": "constraint", "id": "$constraint", "parent": "$sec", "body": "No balance column."},
        {"op": "create", "type": "example", "id": "$example", "parent": "$sec", "body": "A deposit of five."},
        {"op": "create", "type": "note", "id": "$note", "parent": "$sec", "body": "Mind the rounding."},
        {"op": "create", "type": "rationale", "id": "$rationale", "parent": "$sec", "body": "Because reasons."},
        {"op": "create", "type": "term", "id": "$term", "parent": "$sec", "fields": {"name": "Ledger"}, "body": "Balances."},
        {"op": "create", "type": "section", "id": "$sub", "parent": "$sec", "fields": {"title": "Parts"}},
        {"op": "create", "type": "component", "id": "$component", "parent": "$sub", "fields": {"name": "Core"}, "body": "The core."},
        {"op": "create", "type": "actor", "id": "$actor", "parent": "$sec", "fields": {"name": "Teller"}, "body": "A person at the counter."},
        {"op": "create", "type": "interface", "id": "$interface", "parent": "$sec", "fields": {"name": "Post"}, "body": "Posts an entry."},
        {"op": "create", "type": "flow", "id": "$flow", "parent": "$sub", "fields": {"name": "Settle", "steps": ["One", "Two"]}},
        {"op": "create", "type": "table", "id": "$table", "parent": "$sec", "fields": {"columns": ["Name", "Count"], "rows": [["a", 1], [["a", "b"], 2], [{"k": "v"}, 3]]}},
        {"op": "create", "type": "diagram", "id": "$diagram", "parent": "$sec", "fields": {"lang": "mermaid", "alt": "The flow"}, "body": "graph TD\n A --> B"},
        {"op": "create", "type": "figure", "id": "$figure", "parent": "$sec", "fields": {"blob": blob, "media_type": "image/png", "alt": "A picture"}},
        {"op": "create", "type": "code", "id": "$code", "parent": "$sec", "fields": {"lang": "python"}, "body": "print(1)"},
        {"op": "create", "type": "question", "id": "$question", "parent": "$sec", "fields": {"prompt": "Is a ledger a sequence?"}}
    ]))
    .expect("every type");
    let dec = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent: res.created["$sec"].clone(),
                statement: "Ledgers are atomic".into(),
                affects: vec![res.created["$req"].clone()],
                ..Default::default()
            },
        )
        .expect("the decision");
    land(&mut r, "work").expect("land every type");
    let dangling_prose = BlockId::parse("p-DNG01").expect("an id");
    raw_commit(
        &mut r,
        "dangling",
        vec![raw(
            dangling_prose.as_str(),
            "prose",
            Some(&res.created["$sec"]),
            "zz",
            &[],
            &format!("See {{{{{}}}}} and {{{{p-ZZZZZ}}}}.", res.created["$term"]),
        )],
    );
    let ids = TypeIds {
        doc: created(&res, "$doc"),
        section: created(&res, "$sec"),
        subsection: created(&res, "$sub"),
        prose: created(&res, "$prose"),
        requirement: created(&res, "$req"),
        claim: created(&res, "$claim"),
        constraint: created(&res, "$constraint"),
        example: created(&res, "$example"),
        note: created(&res, "$note"),
        rationale: created(&res, "$rationale"),
        term: created(&res, "$term"),
        component: created(&res, "$component"),
        actor: created(&res, "$actor"),
        interface: created(&res, "$interface"),
        flow: created(&res, "$flow"),
        table: created(&res, "$table"),
        diagram: created(&res, "$diagram"),
        figure: created(&res, "$figure"),
        code: created(&res, "$code"),
        question: created(&res, "$question"),
        decision: created(&dec, "$decision"),
        dangling_prose,
    };
    (d, r, ids)
}

/// The blocks of [`decided_site`].
#[derive(Clone, Debug)]
pub struct DecidedIds {
    pub doc: BlockId,
    pub requirement: BlockId,
    pub term: BlockId,
    /// Accepted 2020-06-23, superseded by `d1`.
    pub d0: BlockId,
    /// Accepted 2021-02-08, supersedes `d0`.
    pub d1: BlockId,
    /// Rejected 2021-01-10.
    pub d2: BlockId,
    /// Accepted 2021-03-01, revoked by `d3_revoker`.
    pub d3: BlockId,
    /// Accepted 2021-04-02.
    pub d3_revoker: BlockId,
    /// Proposed; exists only in the view `pending`.
    pub d4: BlockId,
    pub q: BlockId,
}

/// Project `Ledgers`: doc "Ledgers" with a section "Rules" holding the prose `Plain words.`, the
/// requirement `Ledgers are atomic when settled.`, the term "Balance", five decisions the
/// approver wrote with their `status` and `decided_at` (D0 to D3 and D3's revoker, see
/// [`DecidedIds`]) and a question that references the requirement; landed. D4, proposed, is in
/// the view `pending`, which is never landed (canon cannot hold a proposed decision).
pub fn decided_site() -> (tempfile::TempDir, Rdy, DecidedIds) {
    let _clock = fixture_clock();
    let (d, mut r) = named_project("Ledgers");
    let base = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$doc", "fields": {"name": "ledgers", "title": "Ledgers", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Rules"}},
        {"op": "create", "type": "prose", "parent": "$s", "body": "Plain words."},
        {"op": "create", "type": "requirement", "id": "$r", "parent": "$s", "fields": {"priority": "must"}, "body": "Ledgers are atomic when settled."},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Balance"}, "body": "What an account holds."}
    ]))
    .expect("the base");
    let (section, requirement, term) = (
        s(&created(&base, "$s")),
        s(&created(&base, "$r")),
        s(&created(&base, "$t")),
    );
    let decision = |id: &str,
                    statement: &str,
                    status: &str,
                    at: &str,
                    options: &[&str],
                    links: Value| {
        json!({"op": "create", "type": "decision", "id": id, "parent": section,
            "fields": {"statement": statement, "status": status, "decided_at": at, "recorded_at": at, "options": options},
            "links": links})
    };
    let dated = write_as(
        &approver(),
        &mut r,
        "work",
        json!([
            decision(
                "$d0",
                "Ledgers are eventually consistent",
                "accepted",
                "2020-06-23T09:00:00Z",
                &[],
                json!({"affects": [{"target": requirement}]})
            ),
            decision(
                "$d1",
                "Ledgers are atomic",
                "accepted",
                "2021-02-08T10:00:00Z",
                &["atomic", "eventual"],
                json!({"affects": [{"target": requirement}], "supersedes": [{"target": "$d0"}]})
            ),
            decision(
                "$d2",
                "Ledgers are optional",
                "rejected",
                "2021-01-10T09:00:00Z",
                &["optional", "required"],
                json!({"affects": [{"target": requirement}]})
            ),
            decision(
                "$d3",
                "Balances round down",
                "accepted",
                "2021-03-01T09:00:00Z",
                &[],
                json!({"affects": [{"target": term}]})
            )
        ]),
    )
    .expect("the dated decisions");
    let (d0, d1, d2, d3) = (
        created(&dated, "$d0"),
        created(&dated, "$d1"),
        created(&dated, "$d2"),
        created(&dated, "$d3"),
    );
    let revoker = write_as(
        &approver(),
        &mut r,
        "work",
        json!([decision(
            "$rv",
            &format!("Revoke {d3}: balances round half to even"),
            "accepted",
            "2021-04-02T09:00:00Z",
            &[],
            json!({"revokes": [{"target": s(&d3)}]})
        )]),
    )
    .expect("the revoking decision");
    let q = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "question", "id": "$q", "parent": section,
             "fields": {"prompt": "Is a ledger a sequence?"}, "body": format!("See {{{{{requirement}}}}}.")}
        ]),
    )
    .expect("the question");
    land(&mut r, "work").expect("land the decided site");
    let ids = DecidedIds {
        doc: created(&base, "$doc"),
        requirement: created(&base, "$r"),
        term: created(&base, "$t"),
        d0,
        d1,
        d2,
        d3,
        d3_revoker: created(&revoker, "$rv"),
        d4: BlockId::parse("D-AAAAA").expect("a placeholder"),
        q: created(&q, "$q"),
    };
    let m = r.read().materialized(&Scope::Canon).expect("canon");
    for (id, at) in [
        (&ids.d0, "2020-06-23T09:00:00Z"),
        (&ids.d1, "2021-02-08T10:00:00Z"),
        (&ids.d2, "2021-01-10T09:00:00Z"),
        (&ids.d3, "2021-03-01T09:00:00Z"),
        (&ids.d3_revoker, "2021-04-02T09:00:00Z"),
    ] {
        assert_eq!(
            m.get(id).and_then(|b| b.field_str("decided_at")),
            Some(at),
            "the gate rewrote decided_at of {id}"
        );
    }
    let pending = r
        .decide(
            &writer(),
            "pending",
            DecisionInput {
                parent: section,
                statement: "Ledgers may be sharded".into(),
                affects: vec![requirement],
                ..Default::default()
            },
        )
        .expect("the proposed decision");
    let ids = DecidedIds {
        d4: created(&pending, "$decision"),
        ..ids
    };
    (d, r, ids)
}

/// A pack of its own whose `aside` type is presentational; a section may hold one (pres1).
pub const PRESENTATIONAL_PACK: &str = "name: p\nversion: '1'\nextends: rdy-standard\nblock_types:\n  aside: { prefix: as, traits: [presentational], body: required }\n  section:\n    prefix: s\n    traits: [structural]\n    children: [section, prose, claim, requirement, constraint, term, component, actor, interface, flow, example, note, rationale, table, diagram, figure, code, decision, question, aside]\n";

/// The ids [`presentational_site`] returns.
#[derive(Clone, Debug)]
pub struct PresentationalIds {
    pub doc: BlockId,
    /// A presentational block, governed by `decision`.
    pub aside: BlockId,
    /// A claim that depends on the aside.
    pub claim: BlockId,
    pub decision: BlockId,
}

/// A project named `Presentational` whose own pack makes `aside` presentational: doc `t`
/// ("T") with a section, an aside ("Decorative."), a claim that depends on it and an accepted
/// decision (written by the approver) that affects the aside; landed.
pub fn presentational_site() -> (tempfile::TempDir, Rdy, PresentationalIds) {
    let _clock = fixture_clock();
    let dir = tempfile::tempdir().expect("tempdir");
    drop(Rdy::init(dir.path(), "rdy-standard").expect("init"));
    std::fs::write(dir.path().join(".rdy/packs/p.yaml"), PRESENTATIONAL_PACK).expect("pack");
    std::fs::write(
        dir.path().join(".rdy/config.yaml"),
        "pack: p\nname: Presentational\n",
    )
    .expect("config");
    let mut r = Rdy::open(dir.path()).expect("open");
    let base = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "t", "title": "T", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
        {"op": "create", "type": "aside", "id": "$a", "parent": "$s", "body": "Decorative."},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Depends on {{$a}}.",
         "links": {"depends_on": [{"target": "$a"}]}}
    ]))
    .expect("the base");
    let (section, aside) = (s(&created(&base, "$s")), s(&created(&base, "$a")));
    let decided = crate::golden::write_as(
        &approver(),
        &mut r,
        "work",
        json!([{"op": "create", "type": "decision", "id": "$dec", "parent": section,
            "fields": {"statement": "Asides stay decorative", "status": "accepted",
                       "decided_at": "2021-02-08T10:00:00Z", "recorded_at": "2021-02-08T10:00:00Z"},
            "links": {"affects": [{"target": aside}]}}]),
    )
    .expect("the decision");
    land(&mut r, "work").expect("land the presentational site");
    let ids = PresentationalIds {
        doc: created(&base, "$d"),
        aside: created(&base, "$a"),
        claim: created(&base, "$c"),
        decision: created(&decided, "$dec"),
    };
    (dir, r, ids)
}

/// The view [`review_project`] writes its draft into.
pub const REVIEW_VIEW: &str = "rewrite";

/// The blocks [`review_project`] names.
#[derive(Clone, Debug)]
pub struct ReviewIds {
    pub intro: BlockId,
    pub lede: BlockId,
    pub summary: BlockId,
    pub kept: BlockId,
    pub edited: BlockId,
    pub gone: BlockId,
    pub after: BlockId,
    pub added: BlockId,
    pub keep: BlockId,
    pub keep_summary: BlockId,
    pub keep_prose: BlockId,
    pub old: BlockId,
    pub fresh: BlockId,
    pub fresh_prose: BlockId,
}

/// DX4's review project `Review`, on the fixture clock. Canon holds three top-level docs: `Intro`
/// (a one-paragraph lede, then a `Summary` of four proses), `Keep` and `Old guide`, landed and
/// tagged `v1.0.0`. The view [`REVIEW_VIEW`], written on that landing, edits the lede and one
/// prose, removes one prose, adds a prose and a decision, adds the doc `Fresh` and removes
/// `Old guide`. After it, a landing from the view `later` edits `Keep`'s prose in canon.
pub fn review_project() -> (tempfile::TempDir, Rdy, ReviewIds) {
    let _clock = fixture_clock();
    let (d, mut r) = named_project("Review");
    let canon = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$intro", "fields": {"name": "intro", "title": "Intro", "doc_type": "overview"}},
        {"op": "create", "type": "prose", "id": "$lede", "parent": "$intro", "body": "Where to start."},
        {"op": "create", "type": "section", "id": "$summary", "parent": "$intro", "fields": {"title": "Summary"}},
        {"op": "create", "type": "prose", "id": "$kept", "parent": "$summary", "body": "Kept as it was."},
        {"op": "create", "type": "prose", "id": "$edited", "parent": "$summary", "body": "Old wording."},
        {"op": "create", "type": "prose", "id": "$gone", "parent": "$summary", "body": "Gone soon."},
        {"op": "create", "type": "prose", "id": "$after", "parent": "$summary", "body": "Stays after."},
        {"op": "create", "type": "doc", "id": "$keep", "fields": {"name": "keep", "title": "Keep", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$ks", "parent": "$keep", "fields": {"title": "Summary"}},
        {"op": "create", "type": "prose", "id": "$kp", "parent": "$ks", "body": "Nothing changes here."},
        {"op": "create", "type": "doc", "id": "$old", "fields": {"name": "old", "title": "Old guide", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$os", "parent": "$old", "fields": {"title": "Summary"}},
        {"op": "create", "type": "prose", "id": "$op", "parent": "$os", "body": "Old guide text."}
    ]))
    .expect("the canon docs");
    land(&mut r, "work").expect("land the canon docs");
    r.tag(&approver(), "v1.0.0", None, None)
        .expect("tag v1.0.0");
    let s = |res: &WriteResult, k: &str| res.created[k].clone();
    let id = |res: &WriteResult, k: &str| BlockId::parse(&res.created[k]).expect("a created id");
    let draft = write(&mut r, REVIEW_VIEW, json!([
        {"op": "update", "id": s(&canon, "$lede"), "body": "Where to begin."},
        {"op": "update", "id": s(&canon, "$edited"), "body": "New wording."},
        {"op": "delete", "id": s(&canon, "$gone")},
        {"op": "create", "type": "prose", "id": "$added", "parent": s(&canon, "$summary"), "body": "Brand new paragraph."},
        {"op": "create", "type": "doc", "id": "$fresh", "fields": {"name": "fresh", "title": "Fresh", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$fs", "parent": "$fresh", "fields": {"title": "Summary"}},
        {"op": "create", "type": "prose", "id": "$fp", "parent": "$fs", "body": "Fresh text."},
        {"op": "delete", "id": s(&canon, "$op")},
        {"op": "delete", "id": s(&canon, "$os")},
        {"op": "delete", "id": s(&canon, "$old")}
    ]))
    .expect("the draft");
    r.decide(
        &writer(),
        REVIEW_VIEW,
        DecisionInput {
            parent: s(&canon, "$summary"),
            statement: "Keep the summary short".into(),
            ..Default::default()
        },
    )
    .expect("a decision in the draft");
    write(
        &mut r,
        "later",
        json!([{"op": "update", "id": s(&canon, "$kp"), "body": "Changed in canon later."}]),
    )
    .expect("a later canon edit");
    land(&mut r, "later").expect("land the later edit");
    let ids = ReviewIds {
        intro: id(&canon, "$intro"),
        lede: id(&canon, "$lede"),
        summary: id(&canon, "$summary"),
        kept: id(&canon, "$kept"),
        edited: id(&canon, "$edited"),
        gone: id(&canon, "$gone"),
        after: id(&canon, "$after"),
        added: id(&draft, "$added"),
        keep: id(&canon, "$keep"),
        keep_summary: id(&canon, "$ks"),
        keep_prose: id(&canon, "$kp"),
        old: id(&canon, "$old"),
        fresh: id(&draft, "$fresh"),
        fresh_prose: id(&draft, "$fp"),
    };
    (d, r, ids)
}

/// [`review_project`], then the site of `scope` rendered into the fourth value.
pub fn review_site(scope: &Scope) -> (tempfile::TempDir, Rdy, ReviewIds, tempfile::TempDir) {
    let (dir, r, ids) = review_project();
    let out = tempfile::tempdir().expect("tempdir");
    rdy_render::render_site(&r.read(), scope, out.path()).expect("render the review project");
    (dir, r, ids, out)
}
