//! The golden fixture of the `render_quality` area: the structural pattern matching proposals
//! (a design corpus with a specification, its rationale, a tutorial and the proposals that were
//! set aside) as a format-2 tree under `fixtures/pattern-matching/`, loaded with `rdy import`,
//! and the helpers that build projects, sites and the two older `render` area projects.
//!
//! Every block of the committed tree has an explicit id, so [`Ids`] is a table of constants.
use crate::fixtures;
pub use crate::fixtures::{approver, land, raw, raw_commit, write, writer};
use rdy_engine::{DecisionInput, Rdy, Scope};
use rdy_model::{BlockId, FieldValue, Hash, Link};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// The committed export: `rdy-export.yaml`, `docs/` and `blobs/`.
pub const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/pattern-matching");

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
    }
    view {
        dangling_ref_prose: "p-Y3BEZ",
        dangling_diagram: "dg-K4G0E",
        proposed_decision: "D-3Z81J",
        overview_note: "n-40PR2",
    }
}

/// The ids, after checking that every one of them is in canon.
pub fn ids(r: &mut Rdy) -> Ids {
    let ids = Ids::new();
    let m = r.materialized(&Scope::Canon).expect("canon").clone();
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

/// The golden project: the pattern matching corpus, landed and tagged `v1.0.0`, two landings
/// after it, and a view `editing` with two unlanded changes. Canon reads
/// `v1.0.0 + 2 landings`, the view `+ 2 unlanded changes`.
pub fn project() -> (tempfile::TempDir, Rdy, Ids) {
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
    let ids = ids(&mut r);
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
    let (dir, mut r, ids) = project();
    let out = tempfile::tempdir().expect("tempdir");
    rdy_render::render_site(&mut r, scope, out.path()).expect("render the fixture");
    (dir, r, ids, out)
}

/// The site of canon.
pub fn site_canon() -> (tempfile::TempDir, Rdy, Ids, tempfile::TempDir) {
    site(&Scope::Canon)
}

/// The frozen site of `v1.0.0`'s landing.
pub fn site_frozen() -> (tempfile::TempDir, Rdy, Ids, tempfile::TempDir) {
    let (dir, mut r, ids) = project();
    let landing: Hash = r.versions().expect("versions")[0]
        .landing
        .parse()
        .expect("a landing hash");
    let out = tempfile::tempdir().expect("tempdir");
    rdy_render::render_site(&mut r, &Scope::At(landing), out.path()).expect("render the fixture");
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
    let (d, mut r) = fixtures::project();
    let blob = r.store_mut().put_blob(b"png").unwrap().to_string();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "evil", "title": EVIL, "description": EVIL}},
        {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"name": "page", "title": EVIL, "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": EVIL}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": EVIL}, "body": EVIL},
        {"op": "create", "type": "claim", "parent": "$s", "body": format!("{EVIL} {{{{$t}}}}")},
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
