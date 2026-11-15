//! scc7 (D-CDQF7, D-6WD17, D-W4YAE, D-MCH70, D-AMF3T, D-B2AC3, D-TBDA1): a portable publication
//! baseline. One profile at one tag becomes a JSON value with no excerpt, body or field value;
//! another store compares a candidate with it through the disclosure review seam. Its bytes are
//! RFC 8785 with safe integers, hashed with a prefix; a reader refuses every form outside the
//! closed rules with one text. A comparison against it checks the hash, the binding, the profile
//! and the lineage, and reads the same labels and counts as a comparison inside one store.
//!
//! The project tests build their states through real writes and landings. The fixture and the
//! byte tests build one state in memory over an empty snapshot, with fixed ids.

mod common;

use rdy_engine::clone::{CloneSource, clone};
use rdy_engine::publication::{
    self, BASELINE_FORMAT, Baseline, BaselineBinding, BaselineCaps, BaselineSource, DisclosureDiff,
    PartName, PublicationReview, PublicationReviewSide, ReadPublicationSource, ReaderReviewSide,
    ReviewProjection, ReviewRemovals, review_json,
};
use rdy_engine::redaction::RedactInput;
use rdy_engine::{Materialized, Rdy, RdyError, ReadState, Session, SessionKind, WriteRequest};
use rdy_model::redaction::{CONTENT_REMOVED, RedactedBlockIdentity};
use rdy_model::{Block, BlockId, FieldValue, Hash, Link, Provenance, RepoId, Role, StateSnapshot};
use rdy_store::LandRequest;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

const VERIFY: &str = "The publication baseline does not verify.";
const VERIFY_FIX: &str =
    "Make the baseline again with rdy publication baseline in its source project.";
const LINEAGE: &str =
    "The publication baseline comes from a project outside this project's lineage.";
const LONG_TITLE: &str = "A decision title that runs well past forty visible characters";

// ---- project helpers ------------------------------------------------------------------------

fn session(role: Role) -> Session {
    Session {
        actor: if role == Role::Approver {
            "human:founder".into()
        } else {
            "agent:a".into()
        },
        run: None,
        role,
        kind: SessionKind::Human,
    }
}

fn write(r: &mut Rdy, ops: Value) -> rdy_engine::WriteResult {
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "scc7".into(),
        },
    )
    .unwrap()
}

fn land(r: &mut Rdy) {
    let p = r
        .propose(
            &session(Role::Writer),
            "work",
            "scc7",
            Some(common::TEST_SUMMARY),
        )
        .unwrap();
    common::confirm_exceptions(r, &session(Role::Approver), &p);
    r.land(&session(Role::Approver), &p.id).unwrap();
}

fn tag(r: &mut Rdy, name: &str) {
    r.tag(&session(Role::Approver), name, None, None).unwrap();
}

/// Removes every version of block `target` through the store's own landing path.
fn remove(r: &mut Rdy, target: &str) {
    let s = session(Role::Writer);
    let req = r
        .redact(
            &s,
            RedactInput {
                targets: vec![target.to_string()],
                reason_class: "secret".into(),
                reason: "a leaked code".into(),
                dry_run: false,
            },
        )
        .unwrap();
    assert!(req.redaction.is_some());
    let p = r.propose(&s, "work", "Remove the code", None).unwrap();
    let version: Hash = p.id.parse().unwrap();
    r.seal_open().unwrap();
    r.store_mut().set_held(false).unwrap();
    r.store_mut()
        .land_atomic(LandRequest {
            proposal: version,
            accept: None,
            approver: Provenance {
                actor: "human:t".into(),
                run: None,
                role: Role::Approver,
                at_ms: 1_000_000,
            },
            alt: None,
            relay: None,
            evidence: None,
            assessment: None,
        })
        .unwrap();
    r.store_mut().set_held(true).unwrap();
    r.invalidate();
}

/// One change to a baseline value.
type Tamper = Box<dyn Fn(&mut Value)>;

/// One change to the fixture state.
type Change = Box<dyn Fn(Vec<Block>) -> Vec<Block>>;

/// The ids of the project's blocks by their write names.
type Ids = BTreeMap<String, String>;

/// A project with the org profile `site` over the folder `pubs` and the top-level doc
/// `glossary`, which has no page, and the public profile `open` over `guide`, which holds a
/// planned note.
/// v1.0.0: `pubs/base` holds a section with a prose, a prose that cites the doc `outside`, a
/// figure with a dark image and a section that holds a decision with a long title; `pubs/gone`
/// holds a prose. The landing accepts the decision.
/// v2.0.0: the base prose has other text, the section prose moved out of its section, the
/// decision's section moved to `outside`, `gone` moved out of `pubs`, the doc `added` with a
/// light figure added, and a second planned note in `guide`.
fn project(dir: &Path) -> (Rdy, Ids) {
    let mut r = Rdy::init(dir, "rdy-standard").unwrap();
    let light = r.store_mut().put_blob(b"scc7 light").unwrap();
    let dark = r.store_mut().put_blob(b"scc7 dark").unwrap();
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "folder", "id": "$pubs", "fields": {"name": "pubs", "title": "Pubs"}},
            {"op": "create", "type": "doc", "id": "$base", "parent": "$pubs", "fields": {"name": "base", "title": "Base", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$sec", "parent": "$base", "fields": {"title": "Intro"}},
            {"op": "create", "type": "prose", "id": "$sp", "parent": "$sec", "body": "Section text."},
            {"op": "create", "type": "prose", "id": "$bp", "parent": "$base", "body": "Base text."},
            {"op": "create", "type": "figure", "id": "$bf", "parent": "$base", "fields": {"blob": light.to_string(), "dark_blob": dark.to_string(), "dark_media_type": "application/octet-stream", "media_type": "application/octet-stream", "alt": "Base figure"}},
            {"op": "create", "type": "doc", "id": "$gone", "parent": "$pubs", "fields": {"name": "gone", "title": "Gone", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$gp", "parent": "$gone", "body": "Gone text."},
            {"op": "create", "type": "section", "id": "$dsec", "parent": "$base", "fields": {"title": "Decisions"}},
            {"op": "create", "type": "decision", "id": "$dd", "parent": "$dsec", "fields": {"title": LONG_TITLE, "statement": "Ship the base.", "status": "proposed"}},
            {"op": "create", "type": "doc", "id": "$glossary", "fields": {"name": "glossary", "title": "Glossary", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$glossary", "body": "Glossary words."},
            {"op": "create", "type": "doc", "id": "$outside", "fields": {"name": "outside", "title": "Outside", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$guide", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$guide", "body": "Shown words."},
            {"op": "create", "type": "note", "id": "$n1", "parent": "$guide", "body": "Planned.", "fields": {"planned": true}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let mut ids: Ids = w.created.clone().into_iter().collect();
    let id = |ids: &Ids, n: &str| ids[n].clone();
    let w2 = write(
        &mut r,
        json!([
            {"op": "create", "type": "prose", "id": "$cite", "parent": id(&ids, "$base"), "body": format!("See {{{{{}}}}}.", id(&ids, "$outside"))},
            {"op": "create", "type": "publication_profile", "id": "$site", "parent": id(&ids, "$admin"),
             "fields": {"key": "site", "title": "Site", "audience": "org", "select": [id(&ids, "$pubs"), id(&ids, "$glossary")]}},
            {"op": "create", "type": "publication_profile", "id": "$open", "parent": id(&ids, "$admin"),
             "fields": {"key": "open", "title": "Open", "audience": "public", "select": [id(&ids, "$guide")]}}
        ]),
    );
    ids.extend(w2.created.clone());
    land(&mut r);
    tag(&mut r, "v1.0.0");

    let w3 = write(
        &mut r,
        json!([
            {"op": "update", "id": id(&ids, "$bp"), "body": "Base text, changed."},
            {"op": "move", "id": id(&ids, "$sp"), "parent": id(&ids, "$base")},
            {"op": "move", "id": id(&ids, "$dsec"), "parent": id(&ids, "$outside")},
            {"op": "move", "id": id(&ids, "$gone")},
            {"op": "create", "type": "doc", "id": "$added", "parent": id(&ids, "$pubs"), "fields": {"name": "added", "title": "Added", "doc_type": "page"}},
            {"op": "create", "type": "figure", "parent": "$added", "fields": {"blob": light.to_string(), "media_type": "application/octet-stream", "alt": "Added figure"}},
            {"op": "create", "type": "prose", "parent": "$added", "body": "Added words."},
            {"op": "create", "type": "note", "id": "$n2", "parent": id(&ids, "$guide"), "body": "Second planned.", "fields": {"planned": true}}
        ]),
    );
    ids.extend(w3.created.clone());
    land(&mut r);
    tag(&mut r, "v2.0.0");
    (r, ids)
}

fn scope(st: &ReadState, tag: &str) -> rdy_engine::Scope {
    st.resolve_scope(&format!("tag:{tag}")).unwrap()
}

fn resolved(st: &ReadState, tag: &str, profile: &str) -> publication::Resolved {
    publication::resolve(st, &scope(st, tag), profile).unwrap()
}

fn source(st: &ReadState) -> ReadPublicationSource<'_> {
    ReadPublicationSource::with_settings(st, st.pack(), "Project", None)
}

/// The JSON of a review as the adapter writes it: `references`, `disclosure` and `included`.
fn shown(c: &publication::Resolved, diff: &DisclosureDiff, review: &PublicationReview) -> Value {
    let j = review_json(ReviewProjection::Adapter, &c.references, Some(diff), review);
    json!({"references": j.references, "disclosure": j.disclosure, "included": j.included})
}

/// The comparison of `profile` at `tag` with `before`, inside the store.
fn same_store(st: &ReadState, tag: &str, before: &str, profile: &str) -> (DisclosureDiff, Value) {
    let (b, a) = (resolved(st, before, profile), resolved(st, tag, profile));
    let diff = publication::disclosure_diff(&b, &a).unwrap();
    let state = st.materialized(&scope(st, tag)).unwrap();
    let review = PublicationReview::build(&source(st), &state, &a, Some((&b, &diff))).unwrap();
    let j = shown(&a, &diff, &review);
    (diff, j)
}

/// The comparison of `profile` at `tag` with `baseline`, in the store `st` reads.
fn against(
    st: &ReadState,
    tag: &str,
    profile: &str,
    baseline: &Baseline,
    binding: Option<&BaselineBinding>,
) -> Result<(DisclosureDiff, Value), RdyError> {
    let a = resolved(st, tag, profile);
    let state = st.materialized(&scope(st, tag)).unwrap();
    let (diff, review) =
        publication::compare_with_baseline(st, &source(st), &state, &a, baseline, binding)?;
    let j = shown(&a, &diff, &review);
    Ok((diff, j))
}

/// `v` with what a baseline comparison changes put back to the same-store form: every
/// `excerpt` and `diff` null, `disclosure.baseline` gone and `disclosure.redactions` one sorted
/// list of both sides' requests.
fn as_same_store(mut v: Value) -> Value {
    fn walk(v: &mut Value) {
        match v {
            Value::Object(m) => {
                for (k, x) in m.iter_mut() {
                    if k == "excerpt" || k == "diff" {
                        *x = Value::Null;
                    } else {
                        walk(x);
                    }
                }
            }
            Value::Array(a) => a.iter_mut().for_each(walk),
            _ => {}
        }
    }
    let d = &mut v["disclosure"];
    if let Some(m) = d.as_object_mut() {
        m.remove("baseline");
        if let Some(r) = m.get("redactions").filter(|r| r.is_object()).cloned() {
            // inside one store the list is the requests of both sides
            let mut all: Vec<String> = ["candidate", "baseline"]
                .iter()
                .flat_map(|k| r[k].as_array().unwrap().iter())
                .map(|h| h.as_str().unwrap().to_string())
                .collect();
            all.sort();
            all.dedup();
            m.insert("redactions".into(), json!(all));
        }
    }
    walk(&mut v);
    v
}

fn assert_verify(e: &RdyError) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        ("E_PUBLICATION_BASELINE", VERIFY, VERIFY_FIX),
        "{e}"
    );
}

fn binding_of(b: &Baseline) -> BaselineBinding {
    BaselineBinding {
        repo_id: b.repo_id(),
        tag: b.tag().to_string(),
        profile_version: b.profile_version(),
        selection_identity: b.selection_identity(),
    }
}

// ---- value helpers --------------------------------------------------------------------------

/// The hash a baseline value's members give (D-6WD17).
fn hash_of(v: &Value) -> String {
    let mut m = v.as_object().unwrap().clone();
    m.remove("baseline_hash");
    let canonical =
        publication::baseline_canonical_json(Value::Object(m).to_string().as_bytes()).unwrap();
    let mut h = blake3::Hasher::new();
    h.update(BASELINE_FORMAT.as_bytes());
    h.update(&[0]);
    h.update(&canonical);
    h.finalize().to_hex().to_string()
}

/// `v` with its `baseline_hash` set to what its members give.
fn rehash(mut v: Value) -> Value {
    v["baseline_hash"] = json!(hash_of(&v));
    v
}

fn read_value(v: &Value) -> Result<Baseline, RdyError> {
    Baseline::read(v.to_string().as_bytes())
}

fn fixture_path(name: &str) -> String {
    format!(
        "{}/../rdy-qa/fixtures/publication/{name}",
        env!("CARGO_MANIFEST_DIR")
    )
}

// ---- the in-memory state of the fixture ------------------------------------------------------

fn bid(s: &str) -> BlockId {
    BlockId::parse(s).expect("an id")
}

fn s(v: &str) -> FieldValue {
    FieldValue::Str(v.into())
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

fn link(mut b: Block, kind: &str, target: &str) -> Block {
    b.links.entry(kind.into()).or_default().push(Link {
        target: bid(target),
        part: None,
    });
    b
}

fn removed(b: &Block) -> Block {
    removed_by(b, b"a removal request")
}

fn removed_by(b: &Block, request: &[u8]) -> Block {
    Block::redacted(
        b.id.clone(),
        b.block_type.clone(),
        b.parent.clone(),
        b.order.clone(),
        RedactedBlockIdentity {
            version: b.version_hash(),
            content_hash: b.content_hash(),
            frozen: false,
            request: Hash::of(request),
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

/// The fixture state: a folder with a removed folder and a removed doc in it, docs with
/// sections, a term pair with equal titles, two proses with equal reader text, nested and
/// boundary fields, links, a figure with both images, a decision with a long title, a decision
/// outside docs and a top-level glossary doc.
fn fixture_blocks() -> Vec<Block> {
    let doc = |id: &str, parent: Option<&str>, name: &str, title: &str, order: &str| {
        let b = with(blk(id, "doc", parent, order), "name", s(name));
        with(with(b, "title", s(title)), "doc_type", s("page"))
    };
    let folder = |id: &str, parent: Option<&str>, name: &str, title: &str, order: &str| {
        with(
            with(blk(id, "folder", parent, order), "name", s(name)),
            "title",
            s(title),
        )
    };
    let hidden_folder = folder("f-SECRT", Some("f-PBSS1"), "secret", "Secret", "c");
    let hidden_doc = doc("d-HDEN1", Some("f-PBSS1"), "hidden", "Hidden", "d");
    let hidden_doc = removed_by(&hidden_doc, b"another removal request");
    let nested = FieldValue::Map(BTreeMap::from([
        (
            "café ✓".to_string(),
            FieldValue::List(vec![
                FieldValue::Int(i64::MIN),
                FieldValue::Int(i64::MAX),
                FieldValue::Int(0),
            ]),
        ),
        ("empty".to_string(), FieldValue::List(vec![])),
        ("flag".to_string(), FieldValue::Bool(false)),
    ]));
    vec![
        doc("d-ADM1N", None, "admin", "Admin", "zz"),
        {
            let b = blk("p-PRF01", "publication_profile", Some("d-ADM1N"), "z");
            let b = with(b, "key", s("site"));
            let b = with(b, "title", s("Profile title"));
            let b = with(b, "audience", s("public"));
            with(
                b,
                "select",
                FieldValue::List(vec![s("f-PBSS1"), s("d-GSS01")]),
            )
        },
        folder("f-PBSS1", None, "pubs", "Pubs", "a"),
        doc("d-APHA1", Some("f-PBSS1"), "alpha", "Alpha ✓ 日本語", "a"),
        with(
            blk("s-SECT1", "section", Some("d-APHA1"), "a"),
            "title",
            s("Intro"),
        ),
        link(
            body(
                blk("p-PRS01", "prose", Some("s-SECT1"), "a"),
                "Plain prose: café ✓ 😀.",
            ),
            "decided_by",
            "D-XT001",
        ),
        with(
            body(
                blk("p-PRS02", "prose", Some("s-SECT1"), "b"),
                "See {{t-TERM1}}.",
            ),
            "custom",
            nested,
        ),
        body(
            blk("p-PRS03", "prose", Some("d-APHA1"), "c"),
            "See {{t-TERM2}}.",
        ),
        with(
            with(
                blk("t-TERM1", "term", Some("d-APHA1"), "d"),
                "name",
                s("Widget"),
            ),
            "definition",
            s("One."),
        ),
        with(
            with(
                blk("t-TERM2", "term", Some("d-APHA1"), "e"),
                "name",
                s("Widget"),
            ),
            "definition",
            s("Two."),
        ),
        with(
            with(
                with(
                    blk("D-DEC01", "decision", Some("d-APHA1"), "f"),
                    "title",
                    s(LONG_TITLE),
                ),
                "statement",
                s("Ship it."),
            ),
            "status",
            s("proposed"),
        ),
        {
            let b = blk("g-FGR01", "figure", Some("d-APHA1"), "g");
            let b = with(b, "blob", s(&Hash::of(b"light").to_string()));
            let b = with(b, "media_type", s("image/png"));
            let b = with(b, "dark_blob", s(&Hash::of(b"dark").to_string()));
            let b = with(b, "dark_media_type", s("image/png"));
            with(b, "alt", s("A figure"))
        },
        link(
            with(
                blk("p-NNN01", "prose", Some("d-APHA1"), "h"),
                "empty",
                s(""),
            ),
            "references",
            "t-TERM1",
        ),
        doc("d-BETA1", Some("f-PBSS1"), "beta", "Beta", "b"),
        body(
            blk("p-BETA1", "prose", Some("d-BETA1"), "a"),
            "Beta words that run on past the cut, so the tail stays out. See {{d-ADM1N}} and {{t-ZZZZ1}}.",
        ),
        with(
            body(blk("n-PNN01", "note", Some("d-BETA1"), "b"), "Planned one."),
            "planned",
            FieldValue::Bool(true),
        ),
        with(
            body(blk("n-PNN02", "note", Some("d-BETA1"), "c"), "Planned two."),
            "planned",
            FieldValue::Bool(true),
        ),
        removed(&hidden_folder),
        hidden_doc,
        body(
            blk("p-HDE01", "prose", Some("d-HDEN1"), "a"),
            "Under a removed doc.",
        ),
        doc("d-GSS01", None, "glossary", "Glossary", "b"),
        body(
            blk("p-GSS01", "prose", Some("d-GSS01"), "a"),
            "Glossary words.",
        ),
        with(
            with(
                with(blk("D-XT001", "decision", None, "c"), "title", s("Outside")),
                "statement",
                s("Decided outside docs."),
            ),
            "status",
            s("accepted"),
        ),
    ]
}

fn repo() -> RepoId {
    "0123456789abcdef0123456789abcdef".parse().unwrap()
}

/// The fixture baseline: the fixture state resolved in a fresh store, with fixed source facts.
fn fixture_baseline(st: &ReadState) -> (Baseline, publication::Resolved) {
    let m = synth(&fixture_blocks());
    let r = publication::resolve_in_for_tests(st, &m, "site").expect("the fixture resolves");
    let facts = BaselineSource {
        repo_id: repo(),
        fork_origins: vec!["fedcba9876543210fedcba9876543210".parse().unwrap()],
        tag: "v1.0.0".into(),
        landing: Hash::of(b"the fixture landing"),
    };
    let b = publication::build_baseline(facts, &r, &source(st)).expect("the baseline");
    (b, r)
}

fn store() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    (dir, r)
}

// ---- bytes and the reader -------------------------------------------------------------------

#[test]
fn scc7_rfc8785_prefixed_blake3_vectors_and_spaced_input() {
    // canonical form: no white space, members by UTF-16 code units, rows kept in their order
    let spaced = b"{ \"b\" : 1 ,\n \"a\" : [ 0 , 9007199254740991 , \"\\u00e9\\ud83d\\ude00\" ] }";
    let canonical = publication::baseline_canonical_json(spaced).unwrap();
    assert_eq!(
        String::from_utf8(canonical).unwrap(),
        "{\"a\":[0,9007199254740991,\"é😀\"],\"b\":1}"
    );
    // U+1F600 is D83D DE00 in UTF-16, so it sorts before U+E000; in UTF-8 it sorts after
    let members = "{\"\u{e000}\":1,\"\u{1f600}\":2}".as_bytes();
    assert_eq!(
        String::from_utf8(publication::baseline_canonical_json(members).unwrap()).unwrap(),
        "{\"\u{1f600}\":2,\"\u{e000}\":1}"
    );

    // the fixture's hash is BLAKE3 over the prefix, a zero byte and the canonical bytes, as an
    // independent implementation gives it
    let bytes = std::fs::read(fixture_path("baseline-v1.json")).unwrap();
    let expect: Value =
        serde_json::from_slice(&std::fs::read(fixture_path("baseline-v1.expect.json")).unwrap())
            .unwrap();
    let b = Baseline::read(&bytes).unwrap();
    assert_eq!(
        b.baseline_hash().to_string(),
        expect["baseline_hash"].as_str().unwrap()
    );
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(hash_of(&v), expect["baseline_hash"].as_str().unwrap());
    // the same baseline with white space reads to the same value and bytes
    let pretty = serde_json::to_vec_pretty(&v).unwrap();
    assert!(pretty.len() > bytes.len());
    let again = Baseline::read(&pretty).unwrap();
    assert_eq!(again, b);
    assert_eq!(again.to_bytes(), bytes);
}

#[test]
fn scc7_closed_array_orders_and_duplicates_refuse() {
    let v: Value =
        serde_json::from_slice(&std::fs::read(fixture_path("baseline-v1.json")).unwrap()).unwrap();
    read_value(&v).unwrap();
    let mut cases: Vec<(&str, Value)> = vec![];
    for array in [
        "entries",
        "docs",
        "folders",
        "hidden",
        "attachments",
        "references",
        "redactions",
    ] {
        let n = v[array].as_array().unwrap().len();
        assert!(n >= 2, "the fixture holds two {array} rows");
        let mut swapped = v.clone();
        swapped[array].as_array_mut().unwrap().swap(0, 1);
        cases.push((array, swapped));
        let mut twice = v.clone();
        let first = twice[array][0].clone();
        twice[array].as_array_mut().unwrap().insert(0, first);
        cases.push((array, twice));
    }
    let mut origins = v.clone();
    origins["source"]["fork_origins"] = json!([
        "fedcba9876543210fedcba9876543210",
        "0123456789abcdef0123456789abcdef"
    ]);
    cases.push(("fork_origins", origins));
    // the dark row of a figure comes after its light row
    let mut dark_first = v.clone();
    let rows = dark_first["attachments"].as_array_mut().unwrap();
    let dark = rows
        .iter()
        .position(|r| r.get("variant").is_some())
        .expect("a dark row");
    rows.swap(dark - 1, dark);
    cases.push(("variant", dark_first));
    // a path holds its parts root first
    let mut path = v.clone();
    let doc = path["docs"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|d| d["path"].as_array().unwrap().len() == 2)
        .unwrap();
    doc["path"].as_array_mut().unwrap().reverse();
    cases.push(("path", path));
    for (what, case) in cases {
        let e = read_value(&rehash(case)).expect_err(what);
        assert_verify(&e);
    }

    // rows sort by UTF-8 bytes: U+E000 comes before U+1F600 in a reference location
    let mut utf8 = v.clone();
    let mut a = utf8["references"][0].clone();
    a["location"] = json!("\u{e000}");
    let mut b = a.clone();
    b["location"] = json!("\u{1f600}");
    utf8["references"] = json!([a.clone(), b.clone()]);
    read_value(&rehash(utf8.clone())).unwrap();
    utf8["references"] = json!([b, a]);
    assert_verify(&read_value(&rehash(utf8)).unwrap_err());
}

#[test]
fn scc7_safe_integer_tokens_and_unicode_are_strict() {
    let canon = |s: &str| publication::baseline_canonical_json(s.as_bytes());
    for ok in [
        "[0]",
        "[9007199254740991]",
        "[\"\\ud83d\\ude00\"]",
        "[\"\\u0000\"]",
    ] {
        canon(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
    }
    for bad in [
        "[9007199254740992]",
        "[18446744073709551616]",
        "[1.0]",
        "[1e2]",
        "[1E2]",
        "[-1]",
        "[-0]",
        "[+1]",
        "[01]",
        "[\"\\ud800\"]",
        "[\"\\udc00\"]",
        "[\"\\ud800x\"]",
        "[\"\\ud800\\u0041\"]",
        "[\"a\u{1}b\"]",
        "[\"\\x\"]",
        "[1] [2]",
    ] {
        assert_verify(&canon(bad).expect_err(bad));
    }
    // in a whole baseline: a position of 2^53 - 1 reads; past it, as a fraction or signed, not
    let v: Value =
        serde_json::from_slice(&std::fs::read(fixture_path("baseline-v1.json")).unwrap()).unwrap();
    let i = v["docs"]
        .as_array()
        .unwrap()
        .iter()
        .position(|d| d["position"].is_u64())
        .unwrap();
    let mut max = v.clone();
    max["docs"][i]["position"] = json!(9_007_199_254_740_991u64);
    let max = rehash(max);
    read_value(&max).unwrap();
    let text = max.to_string();
    for token in ["9007199254740992", "1.5", "-1", "1e3"] {
        let bad = text.replace("9007199254740991", token);
        assert_verify(&Baseline::read(bad.as_bytes()).unwrap_err());
    }
}

#[test]
fn scc7_required_members_and_duplicate_keys_refuse() {
    let v: Value =
        serde_json::from_slice(&std::fs::read(fixture_path("baseline-v1.json")).unwrap()).unwrap();
    let mut cases: Vec<(String, Value)> = vec![];
    for k in v.as_object().unwrap().keys() {
        let mut x = v.clone();
        x.as_object_mut().unwrap().remove(k);
        cases.push((format!("no {k}"), x));
    }
    let mut extra = v.clone();
    extra["generated_at"] = json!("now");
    cases.push(("unknown member".into(), extra));
    let mut format = v.clone();
    format["format"] = json!("rdy-publication-baseline-2");
    cases.push(("format".into(), format));
    for (array, member) in [
        ("entries", "parts"),
        ("entries", "label"),
        ("entries", "position"),
        ("docs", "page"),
        ("docs", "title"),
        ("folders", "title"),
        ("attachments", "label"),
        ("attachments", "redaction"),
        ("references", "occurrences"),
    ] {
        let mut x = v.clone();
        x[array][0].as_object_mut().unwrap().remove(member);
        cases.push((format!("{array} without {member}"), x));
    }
    let entries = v["entries"].as_array().unwrap();
    let decision = entries
        .iter()
        .position(|e| e["type"] == "decision")
        .unwrap();
    let prose = entries.iter().position(|e| e["type"] == "prose").unwrap();
    for member in ["title", "status_tag"] {
        let mut x = v.clone();
        x["entries"][decision]
            .as_object_mut()
            .unwrap()
            .remove(member);
        cases.push((format!("decision without {member}"), x));
        let mut y = v.clone();
        y["entries"][prose][member] = json!(null);
        cases.push((format!("prose with {member}"), y));
    }
    for (part, value) in [
        ("statement", json!(null)),
        ("text", json!("A".repeat(64))),
        ("text", json!("a".repeat(63))),
    ] {
        let mut x = v.clone();
        let parts = x["entries"][decision]["parts"].as_object_mut().unwrap();
        match value {
            Value::Null => {
                parts.remove(part);
            }
            h => {
                parts.insert(part.into(), h);
            }
        }
        cases.push((format!("decision part {part}"), x));
    }
    for part in ["body", "excerpt", "field:title", "statement"] {
        let mut x = v.clone();
        x["entries"][prose]["parts"][part] = json!("0".repeat(64));
        cases.push((format!("prose part {part}"), x));
    }
    let mut no_text = v.clone();
    no_text["entries"][prose]["parts"]
        .as_object_mut()
        .unwrap()
        .remove("text");
    cases.push(("prose without text".into(), no_text));
    let mut excerpt = v.clone();
    excerpt["entries"][prose]["excerpt"] = json!("Plain prose");
    cases.push(("prose excerpt".into(), excerpt));
    let mut light = v.clone();
    light["attachments"][0]["variant"] = json!("light");
    cases.push(("light variant".into(), light));
    for (what, case) in cases {
        let case = match what.as_str() {
            "no baseline_hash" => case,
            _ => rehash(case),
        };
        let e = read_value(&case).expect_err(&what);
        assert_verify(&e);
    }

    // a duplicate key, at the top and in a row
    let text = v.to_string();
    let dup_top = text.replacen("{", "{\"format\":\"rdy-publication-baseline-1\",", 1);
    assert_verify(&Baseline::read(dup_top.as_bytes()).unwrap_err());
    let dup_row = text.replacen("\"label\":", "\"label\":\"x\",\"label\":", 1);
    assert_verify(&Baseline::read(dup_row.as_bytes()).unwrap_err());
}

#[test]
fn scc7_input_and_canonical_caps_are_independent() {
    let bytes = std::fs::read(fixture_path("baseline-v1.json")).unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    let pretty = serde_json::to_vec_pretty(&v).unwrap();
    let mut no_hash = v.as_object().unwrap().clone();
    no_hash.remove("baseline_hash");
    let canonical =
        publication::baseline_canonical_json(Value::Object(no_hash).to_string().as_bytes())
            .unwrap()
            .len();
    let big = 1 << 30;
    {
        // the input bound counts the bytes given, white space included
        let _g = publication::override_baseline_caps_for_tests(BaselineCaps {
            input: bytes.len(),
            canonical: big,
        });
        Baseline::read(&bytes).unwrap();
        assert_verify(&Baseline::read(&pretty).unwrap_err());
    }
    {
        let _g = publication::override_baseline_caps_for_tests(BaselineCaps {
            input: bytes.len() - 1,
            canonical: big,
        });
        assert_verify(&Baseline::read(&bytes).unwrap_err());
    }
    {
        // the canonical cap counts the canonical bytes without the hash, whatever the input
        let _g = publication::override_baseline_caps_for_tests(BaselineCaps {
            input: big,
            canonical,
        });
        Baseline::read(&pretty).unwrap();
    }
    {
        let _g = publication::override_baseline_caps_for_tests(BaselineCaps {
            input: big,
            canonical: canonical - 1,
        });
        assert_verify(&Baseline::read(&pretty).unwrap_err());
        assert_verify(&Baseline::read(&bytes).unwrap_err());
    }
    // depth: 32 levels parse, 33 do not
    let nest = |n: usize| format!("{}{}", "[".repeat(n), "]".repeat(n));
    publication::baseline_canonical_json(nest(32).as_bytes()).unwrap();
    assert_verify(&publication::baseline_canonical_json(nest(33).as_bytes()).unwrap_err());
    let objects = |n: usize| format!("{}1{}", "{\"a\":".repeat(n), "}".repeat(n));
    publication::baseline_canonical_json(objects(32).as_bytes()).unwrap();
    assert_verify(&publication::baseline_canonical_json(objects(33).as_bytes()).unwrap_err());

    // a make whose canonical form would pass the cap is E_LIMIT naming the cap, with no baseline
    let (_d, r) = store();
    let st = r.read();
    let _g = publication::override_baseline_caps_for_tests(BaselineCaps {
        input: big,
        canonical: 1000,
    });
    let m = synth(&fixture_blocks());
    let res = publication::resolve_in_for_tests(&st, &m, "site").unwrap();
    let facts = BaselineSource {
        repo_id: repo(),
        fork_origins: vec![],
        tag: "v1.0.0".into(),
        landing: Hash::of(b"x"),
    };
    let e = publication::build_baseline(facts, &res, &source(&st)).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_LIMIT",
            "Publication baseline exceeds the limit of 1000 bytes.",
            "Reduce the selection or split the source project."
        )
    );
}

#[test]
fn scc7_row_limits_and_path_bounds_refuse() {
    let v: Value =
        serde_json::from_slice(&std::fs::read(fixture_path("baseline-v1.json")).unwrap()).unwrap();
    let len = |k: &str| v[k].as_array().unwrap().len();
    let base = publication::limits();
    let tight = |f: &dyn Fn(&mut publication::Limits)| {
        let mut l = base;
        f(&mut l);
        let _g = publication::override_limits_for_tests(l);
        read_value(&v).map(|_| ())
    };
    let rows = len("entries")
        .max(len("docs"))
        .max(len("folders"))
        .max(len("hidden"));
    tight(&|l| l.selection = rows).unwrap();
    assert_verify(&tight(&|l| l.selection = rows - 1).unwrap_err());
    tight(&|l| l.attachments = len("attachments")).unwrap();
    assert_verify(&tight(&|l| l.attachments = len("attachments") - 1).unwrap_err());
    let occurrences: u64 = v["references"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["occurrences"].as_u64().unwrap())
        .sum();
    tight(&|l| l.references = occurrences as usize).unwrap();
    assert_verify(&tight(&|l| l.references = occurrences as usize - 1).unwrap_err());
    let longest = v["docs"]
        .as_array()
        .unwrap()
        .iter()
        .chain(v["folders"].as_array().unwrap())
        .map(|d| {
            let p = d["path"].as_array().unwrap();
            p.iter()
                .map(|x| x["name"].as_str().unwrap().len())
                .sum::<usize>()
                + p.len()
                - 1
        })
        .max()
        .unwrap();
    tight(&|l| l.path = longest).unwrap();
    assert_verify(&tight(&|l| l.path = longest - 1).unwrap_err());
    let location = v["references"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["location"].as_str().unwrap().len())
        .max()
        .unwrap();
    tight(&|l| l.location = location).unwrap();
    assert_verify(&tight(&|l| l.location = location - 1).unwrap_err());
}

// ---- the fixture ----------------------------------------------------------------------------

#[test]
fn scc7_reader_part_bytes_and_hashes_match_fixture() {
    let (_d, r) = store();
    let st = r.read();
    let (b, res) = fixture_baseline(&st);
    let removals = ReviewRemovals::of_sides(&source(&st), &[&res]).unwrap();
    let side = ReaderReviewSide::new(&res, &removals);
    let bytes = b.to_bytes();
    if let Ok(dir) = std::env::var("SCC7_FIXTURE_OUT") {
        // writes the values an independent implementation hashes; it never writes the fixture
        let parts: Vec<Value> = b
            .entries()
            .iter()
            .flat_map(|e| {
                let p = side.parts(&e.id).unwrap();
                p.iter()
                    .map(|(n, x)| json!({"block": e.id.to_string(), "part": n.to_string(), "tagged": x.tagged()}))
                    .collect::<Vec<_>>()
            })
            .collect();
        std::fs::write(format!("{dir}/baseline-v1.json"), &bytes).unwrap();
        std::fs::write(
            format!("{dir}/parts.json"),
            serde_json::to_vec_pretty(&parts).unwrap(),
        )
        .unwrap();
    }
    assert_eq!(
        String::from_utf8(bytes.clone()).unwrap(),
        std::fs::read_to_string(fixture_path("baseline-v1.json")).unwrap(),
        "the baseline bytes"
    );
    let expect: Value =
        serde_json::from_slice(&std::fs::read(fixture_path("baseline-v1.expect.json")).unwrap())
            .unwrap();
    assert_eq!(
        b.baseline_hash().to_string(),
        expect["baseline_hash"].as_str().unwrap()
    );
    // each part: its tagged value, the canonical text of it and its digest, worked out apart
    let by_id: BTreeMap<String, &publication::BaselineEntry> =
        b.entries().iter().map(|e| (e.id.to_string(), e)).collect();
    let rows = expect["parts"].as_array().unwrap();
    let mut seen = 0;
    for row in rows {
        let id = row["block"].as_str().unwrap();
        let part = PartName::parse(row["part"].as_str().unwrap()).unwrap();
        let value = side.parts(&bid(id)).unwrap().get(&part).clone();
        assert_eq!(value.tagged(), row["tagged"], "{id} {part}");
        assert_eq!(
            value.canonical(),
            row["canonical"].as_str().unwrap(),
            "{id} {part}"
        );
        assert_eq!(value.hash(), row["hash"].as_str().unwrap(), "{id} {part}");
        assert_eq!(by_id[id].parts[&part], row["hash"].as_str().unwrap());
        seen += 1;
    }
    let total: usize = b.entries().iter().map(|e| e.parts.len()).sum();
    assert_eq!(seen, total, "the fixture pins every part");

    // equal compared values give equal part bytes: the two terms share their name, which is
    // their text, and differ in their definition
    let part = |id: &str, p: PartName| by_id[id].parts[&p].clone();
    assert_eq!(
        part("t-TERM1", PartName::Text),
        part("t-TERM2", PartName::Text)
    );
    assert_eq!(
        part("t-TERM1", PartName::Title),
        part("t-TERM2", PartName::Title)
    );
    let definition = PartName::parse("field:definition").unwrap();
    assert_ne!(
        part("t-TERM1", definition.clone()),
        part("t-TERM2", definition)
    );
    // equal reader text, other authored bodies: the text hashes differ
    let case = &expect["equal_reader_text"];
    let (x, y) = (
        case["blocks"][0].as_str().unwrap(),
        case["blocks"][1].as_str().unwrap(),
    );
    assert_eq!(side.reader_text(&bid(x)), side.reader_text(&bid(y)));
    assert_ne!(part(x, PartName::Text), part(y, PartName::Text));
    // changed links: the link part of a block with links and one without differ
    assert_ne!(
        part("p-NNN01", PartName::Links),
        part("p-PRS01", PartName::Links)
    );

    // the bytes hold no excerpt, body or field value: only the permitted titles and labels
    let text = String::from_utf8(bytes).unwrap();
    // a label keeps the first words of a block (p-SHBDB); nothing past them is held
    for word in [
        "the tail stays out",
        "Planned one",
        "Decided outside docs",
        "Ship it",
        "One.",
        "Two.",
        "image/png",
        "café ✓\":",
        "\"secret\"",
        "\"name\":\"hidden\"",
    ] {
        assert!(!text.contains(word), "the baseline holds {word:?}");
    }
    for title in [LONG_TITLE, "Alpha ✓ 日本語", "Intro", "Pubs", "A figure"] {
        assert!(text.contains(title), "the baseline lacks {title:?}");
    }
}

#[test]
fn scc7_new_members_are_required_and_hash_covered() {
    let v: Value =
        serde_json::from_slice(&std::fs::read(fixture_path("baseline-v1.json")).unwrap()).unwrap();
    read_value(&v).unwrap();
    // each new member changed and the old hash kept: the hash refuses it
    let entries = v["entries"].as_array().unwrap();
    let decision = entries
        .iter()
        .position(|e| e["type"] == "decision")
        .unwrap();
    let mut edits: Vec<(&str, Tamper)> = vec![
        (
            "doc title",
            Box::new(|v| v["docs"][0]["title"] = json!("Other")),
        ),
        (
            "doc page",
            Box::new(|v| v["docs"][0]["page"] = json!("other.html")),
        ),
        (
            "doc position",
            Box::new(|v| v["docs"][0]["position"] = json!(99)),
        ),
        (
            "folder title",
            Box::new(|v| v["folders"][0]["title"] = json!("Other")),
        ),
        (
            "entry doc",
            Box::new(|v| v["entries"][0]["doc"] = json!(null)),
        ),
        (
            "entry label",
            Box::new(|v| v["entries"][0]["label"] = json!("Other")),
        ),
        (
            "entry section",
            Box::new(|v| v["entries"][0]["section"] = json!("s-THER1")),
        ),
        (
            "entry order",
            Box::new(|v| v["entries"][0]["order"] = json!("zz")),
        ),
        (
            "entry position",
            Box::new(|v| v["entries"][0]["position"] = json!(99)),
        ),
        (
            "entry type",
            Box::new(|v| v["entries"][0]["type"] = json!("note")),
        ),
        (
            "attachment label",
            Box::new(|v| v["attachments"][0]["label"] = json!("Other")),
        ),
        ("hidden", Box::new(|v| v["hidden"] = json!([]))),
    ];
    edits.push((
        "decision title",
        Box::new(move |v| v["entries"][decision]["title"] = json!("Other")),
    ));
    edits.push((
        "decision status tag",
        Box::new(move |v| v["entries"][decision]["status_tag"] = json!("accepted")),
    ));
    edits.push((
        "part hash",
        Box::new(move |v| v["entries"][decision]["parts"]["statement"] = json!("0".repeat(64))),
    ));
    for (what, edit) in &edits {
        let mut x = v.clone();
        edit(&mut x);
        assert_ne!(x, v, "{what} changes the value");
        assert_verify(&read_value(&x).expect_err(what));
        // with the hash made again, the same value reads, so the hash alone refused it
        if *what != "entry type" && *what != "entry section" {
            read_value(&rehash(x)).unwrap_or_else(|e| panic!("{what}: {e}"));
        }
    }
}

// ---- make and compare -----------------------------------------------------------------------

#[test]
fn scc7_equal_inputs_give_equal_labelled_baseline_bytes() {
    let (_d, r) = store();
    let st = r.read();
    let (a, _) = fixture_baseline(&st);
    let (b, _) = fixture_baseline(&st);
    assert_eq!(a.to_bytes(), b.to_bytes());
    assert_eq!(a.baseline_hash(), b.baseline_hash());
    // a project's baseline made twice from one tag is the same, and it is labelled
    let dir = tempfile::tempdir().unwrap();
    let (r, ids) = project(dir.path());
    let st = r.read();
    let x = publication::make_baseline(&st, "tag:v1.0.0", "site").unwrap();
    let y = publication::make_baseline(&st, "tag:1.0.0", "site").unwrap();
    assert_eq!(x.to_bytes(), y.to_bytes());
    assert_eq!(x.tag(), "v1.0.0");
    let res = resolved(&st, "v1.0.0", "site");
    assert_eq!(x.selection_hash(), res.selection_hash);
    assert_eq!(x.selection_identity(), res.selection_identity);
    let base = x
        .entries()
        .iter()
        .find(|e| e.id.as_str() == ids["$bp"])
        .unwrap();
    assert_eq!(base.label, "Base text.");
    // the scope must be a tag
    let e = publication::make_baseline(&st, "main", "site").unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_BAD_INPUT",
            "A publication baseline requires a tag.",
            "Use scope tag:<name>."
        )
    );
}

#[test]
fn scc7_same_store_comparison_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let (r, _) = project(dir.path());
    let st = r.read();
    let (diff, j) = same_store(&st, "v2.0.0", "v1.0.0", "site");
    assert_eq!(diff.baseline, None);
    let d = j["disclosure"].as_object().unwrap();
    assert!(d.get("baseline").is_none());
    assert!(d["redactions"].is_array());
    // the report's members are the base's, in its one form
    let report =
        publication::check(&st, "site", Some("tag:v2.0.0"), Some("tag:v1.0.0"), None).unwrap();
    assert_eq!(report["disclosure"], j["disclosure"]);
    let mut keys: Vec<&String> = d.keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "added_attachments",
            "added_docs",
            "after_profile_version",
            "after_selection_hash",
            "after_selection_identity",
            "audience",
            "before_profile_version",
            "before_selection_hash",
            "before_selection_identity",
            "changed_attachments",
            "changed_docs",
            "changed_folders",
            "changed_outside_docs",
            "new_reference_count",
            "new_references",
            "newly_hidden_planned",
            "omitted",
            "profile_changed",
            "profile_id",
            "redactions",
            "removed_attachments",
            "removed_docs",
            "totals",
        ]
    );
}

#[test]
fn scc7_baseline_labels_and_counts_match_same_store() {
    let dir = tempfile::tempdir().unwrap();
    let (r, ids) = project(dir.path());
    let st = r.read();
    for profile in ["site", "open"] {
        let b = publication::make_baseline(&st, "tag:v1.0.0", profile).unwrap();
        let (same_diff, same) = same_store(&st, "v2.0.0", "v1.0.0", profile);
        let (diff, got) = against(&st, "v2.0.0", profile, &b, Some(&binding_of(&b))).unwrap();
        // the comparison rows are the same; only the baseline member is added
        let mut plain = diff.clone();
        plain.baseline = None;
        assert_eq!(plain, same_diff, "{profile}");
        // titles, pages, positions, labels, sections, counts, totals and `what` are the same
        assert_eq!(
            as_same_store(got.clone()),
            as_same_store(same.clone()),
            "{profile}"
        );
    }
    let b = publication::make_baseline(&st, "tag:v1.0.0", "site").unwrap();
    let (_, got) = against(&st, "v2.0.0", "site", &b, None).unwrap();
    let text = got.to_string();
    // the decision's whole title, past 40 characters, comes from its entry (D-AMF3T)
    assert!(LONG_TITLE.chars().count() > 40);
    let entry = b
        .entries()
        .iter()
        .find(|e| e.id.as_str() == ids["$dd"])
        .unwrap();
    assert_eq!(
        entry.decision.as_ref().unwrap().title.as_deref(),
        Some(LONG_TITLE)
    );
    assert!(text.contains(LONG_TITLE), "{text}");
    // the glossary doc has no page; its place is the seam's place in the reading order
    let glossary = b
        .docs()
        .iter()
        .find(|d| d.id.as_str() == ids["$glossary"])
        .unwrap();
    assert_eq!(glossary.page, None);
    let st_m = st.materialized(&scope(&st, "v1.0.0")).unwrap();
    let shape = rdy_engine::reader::shape::Shape::build(&st_m);
    assert_eq!(
        glossary.position,
        shape.position(&glossary.id).map(|p| p as u64)
    );
    let base = b
        .docs()
        .iter()
        .find(|d| d.id.as_str() == ids["$base"])
        .unwrap();
    assert_eq!(base.page.as_deref(), Some("pubs/base.html"));
    assert_eq!(base.title.as_deref(), Some("Base"));
    let pubs = b
        .folders()
        .iter()
        .find(|f| f.id.as_str() == ids["$pubs"])
        .unwrap();
    assert_eq!(pubs.title.as_deref(), Some("Pubs"));
    let sp = b
        .entries()
        .iter()
        .find(|e| e.id.as_str() == ids["$sp"])
        .unwrap();
    assert_eq!(
        sp.section.as_ref().map(BlockId::as_str),
        Some(ids["$sec"].as_str())
    );
    assert_eq!(
        sp.doc.as_ref().map(BlockId::as_str),
        Some(ids["$base"].as_str())
    );
    let figure = b
        .attachments()
        .iter()
        .find(|a| a.row.block.as_str() == ids["$bf"])
        .unwrap();
    assert_eq!(figure.label, "Base figure");
}

#[test]
fn scc7_baseline_side_has_no_excerpt_or_word_diff() {
    let dir = tempfile::tempdir().unwrap();
    let (r, ids) = project(dir.path());
    let st = r.read();
    let b = publication::make_baseline(&st, "tag:v1.0.0", "site").unwrap();
    let (_, same) = same_store(&st, "v2.0.0", "v1.0.0", "site");
    let (_, got) = against(&st, "v2.0.0", "site", &b, None).unwrap();
    let rows = |v: &Value| -> Vec<Value> {
        v["disclosure"]["changed_docs"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|d| d["changes"].as_array().cloned().unwrap_or_default())
            .collect()
    };
    let find = |v: &Value, id: &str| {
        rows(v)
            .into_iter()
            .find(|c| c["id"] == json!(id))
            .unwrap_or_else(|| panic!("no row for {id} in {v}"))
    };
    // inside one store the updated prose has its word diff; against a baseline it is null
    let updated = &ids["$bp"];
    assert!(!find(&same, updated)["diff"].is_null());
    assert!(find(&got, updated)["diff"].is_null());
    assert_eq!(find(&got, updated)["label"], find(&same, updated)["label"]);
    // a removed doc's blocks have no excerpt from the baseline
    let removed = got["disclosure"]["removed_docs"].to_string();
    assert!(!removed.contains("Gone text"), "{removed}");
    for v in [&got] {
        for c in rows(v) {
            if c["change"] == "removed" {
                assert!(c["excerpt"].is_null(), "{c}");
            }
        }
    }
    // an added doc keeps the candidate's text where the row form shows it
    let added = got["disclosure"]["added_docs"].to_string();
    assert_eq!(added, same["disclosure"]["added_docs"].to_string());
}

#[test]
fn scc7_baseline_comparison_names_its_baseline_and_both_redaction_sets() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, ids) = project(dir.path());
    let b = publication::make_baseline(&r.read(), "tag:v1.0.0", "site").unwrap();
    remove(&mut r, &ids["$gp"]);
    remove(&mut r, &ids["$bp"]);
    tag(&mut r, "v3.0.0");
    let st = r.read();
    let (diff, got) = against(&st, "v3.0.0", "site", &b, None).unwrap();
    let cmp = diff.baseline.clone().unwrap();
    let d = &got["disclosure"];
    assert_eq!(
        d["baseline"],
        json!({
            "repo_id": b.repo_id().to_string(),
            "tag": "v1.0.0",
            "landing": b.landing().to_string(),
            "baseline_hash": b.baseline_hash().to_string(),
        })
    );
    let res = resolved(&st, "v3.0.0", "site");
    assert!(!res.redactions.is_empty());
    let mut sorted = cmp.candidate_redactions.clone();
    sorted.sort_by_key(ToString::to_string);
    assert_eq!(sorted, cmp.candidate_redactions);
    for h in &res.redactions {
        assert!(cmp.candidate_redactions.contains(h));
    }
    assert_eq!(cmp.baseline_redactions, Vec::<Hash>::new());
    let hex = |x: &[Hash]| x.iter().map(ToString::to_string).collect::<Vec<_>>();
    assert_eq!(
        d["redactions"],
        json!({"candidate": hex(&cmp.candidate_redactions), "baseline": []})
    );
    assert_eq!(
        d["omitted"]["redactions"],
        json!(0),
        "the adapter keeps every row"
    );
    // and the other way: a baseline made after the removals names them on its side
    let b3 = publication::make_baseline(&st, "tag:v3.0.0", "site").unwrap();
    assert_eq!(b3.redactions(), res.redactions.as_slice());
    let (_, back) = against(&st, "v3.0.0", "site", &b3, None).unwrap();
    assert_eq!(
        back["disclosure"]["redactions"]["baseline"],
        json!(hex(&res.redactions))
    );
}

#[test]
fn scc7_redacted_labels_and_titles_follow_source() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, ids) = project(dir.path());
    // each removed version is the one both tags hold: the citing prose, the doc `base` and the
    // decision
    remove(&mut r, &ids["$cite"]);
    remove(&mut r, &ids["$base"]);
    remove(&mut r, &ids["$dd"]);
    tag(&mut r, "v3.0.0");
    let st = r.read();
    // made after the removals: a removed block reads "Content removed." and a title that a
    // removal removed is null
    let b = publication::make_baseline(&st, "tag:v1.0.0", "site").unwrap();
    let entry = |id: &str| b.entries().iter().find(|e| e.id.as_str() == id).unwrap();
    assert_eq!(entry(&ids["$cite"]).label, CONTENT_REMOVED);
    let dd = entry(&ids["$dd"]);
    assert_eq!(dd.label, CONTENT_REMOVED);
    assert_eq!(dd.decision.as_ref().unwrap().title, None);
    let base = b
        .docs()
        .iter()
        .find(|d| d.id.as_str() == ids["$base"])
        .unwrap();
    assert_eq!(base.title, None);
    assert_eq!(entry(&ids["$base"]).label, CONTENT_REMOVED);
    let text = String::from_utf8(b.to_bytes()).unwrap();
    for word in ["See ", LONG_TITLE, "\"base\"", "\"Base\""] {
        assert!(!text.contains(word), "{word}");
    }
    // compared, the removed rows read as inside the store
    let (_, same) = same_store(&st, "v3.0.0", "v1.0.0", "site");
    let (_, got) = against(&st, "v3.0.0", "site", &b, None).unwrap();
    assert_eq!(as_same_store(got), as_same_store(same));

    // a baseline made before a removal, compared after: the candidate's removal applies to the
    // baseline's side by version, so the folder's name and title read as removed there too
    let dir2 = tempfile::tempdir().unwrap();
    let (mut r2, ids2) = project(dir2.path());
    let early = publication::make_baseline(&r2.read(), "tag:v1.0.0", "site").unwrap();
    remove(&mut r2, &ids2["$pubs"]);
    tag(&mut r2, "v3.0.0");
    let st2 = r2.read();
    let (_, same) = same_store(&st2, "v3.0.0", "v1.0.0", "site");
    let (_, got) = against(&st2, "v3.0.0", "site", &early, None).unwrap();
    let text = got.to_string();
    assert!(!text.contains("\"pubs/"), "{text}");
    assert!(!text.contains("\"Pubs\""), "{text}");
    assert!(text.contains(&format!("{}/base", ids2["$pubs"])), "{text}");
    assert_eq!(as_same_store(got), as_same_store(same));
}

#[test]
fn scc7_both_removal_sets_mask_paths() {
    // the candidate removed a folder the baseline names: the baseline's paths show its id
    let (_d, r) = store();
    let st = r.read();
    let blocks = fixture_blocks();
    let clear: Vec<Block> = blocks
        .iter()
        .map(|b| match b.id.as_str() {
            // the fixture's removed folder and doc, before their removal
            "f-SECRT" => with(
                with(
                    blk("f-SECRT", "folder", Some("f-PBSS1"), "c"),
                    "name",
                    s("secret"),
                ),
                "title",
                s("Secret"),
            ),
            "d-HDEN1" => with(
                with(
                    with(
                        blk("d-HDEN1", "doc", Some("f-PBSS1"), "d"),
                        "name",
                        s("hidden"),
                    ),
                    "title",
                    s("Hidden"),
                ),
                "doc_type",
                s("page"),
            ),
            _ => b.clone(),
        })
        .collect();
    // a doc under the folder, in both states
    let under = |mut v: Vec<Block>| {
        v.push(with(
            with(
                with(
                    blk("d-NDER1", "doc", Some("f-SECRT"), "a"),
                    "name",
                    s("under"),
                ),
                "title",
                s("Under"),
            ),
            "doc_type",
            s("page"),
        ));
        v.push(body(
            blk("p-NDR01", "prose", Some("d-NDER1"), "a"),
            "Under words.",
        ));
        v
    };
    let (clear, masked) = (under(clear), under(blocks));
    let facts = || BaselineSource {
        repo_id: st.repo_id().unwrap(),
        fork_origins: vec![],
        tag: "v1.0.0".into(),
        landing: Hash::of(b"landing"),
    };
    let make = |blocks: &[Block]| {
        let m = synth(blocks);
        let res = publication::resolve_in_for_tests(&st, &m, "site").unwrap();
        let b = publication::build_baseline(facts(), &res, &source(&st)).unwrap();
        (b, res, m)
    };
    let compare = |b: &Baseline, blocks: &[Block], changed: &dyn Fn(Vec<Block>) -> Vec<Block>| {
        let blocks = changed(blocks.to_vec());
        let m = synth(&blocks);
        let res = publication::resolve_in_for_tests(&st, &m, "site").unwrap();
        let (diff, review) =
            publication::compare_with_baseline(&st, &source(&st), &m, &res, b, None).unwrap();
        shown(&res, &diff, &review)
    };
    // change the doc under the folder so its row shows its path
    let edit = |mut v: Vec<Block>| {
        for b in v.iter_mut() {
            if b.id.as_str() == "p-NDR01" {
                b.body = "Other words.".into();
            }
        }
        v
    };
    let (clear_base, ..) = make(&clear);
    let got = compare(&clear_base, &masked, &edit);
    let text = got.to_string();
    assert!(!text.contains("secret/"), "{text}");
    assert!(text.contains("pubs/f-SECRT/under"), "{text}");
    // the baseline removed the folder; the candidate holds it: its paths show the id too
    let (masked_base, ..) = make(&masked);
    assert!(
        masked_base
            .docs()
            .iter()
            .all(|d| d.path.iter().all(|p| p.name != "secret")),
        "the baseline names the removed folder by its id"
    );
    let got = compare(&masked_base, &clear, &edit);
    let text = got.to_string();
    assert!(!text.contains("secret/"), "{text}");
    assert!(text.contains("pubs/f-SECRT/under"), "{text}");
}

#[test]
fn scc7_hidden_ids_and_dark_rows_compare_correctly() {
    let dir = tempfile::tempdir().unwrap();
    let (r, ids) = project(dir.path());
    let st = r.read();
    // the public profile hides the planned notes: the baseline keeps their ids alone
    let b = publication::make_baseline(&st, "tag:v1.0.0", "open").unwrap();
    assert_eq!(
        b.hidden().iter().map(BlockId::as_str).collect::<Vec<_>>(),
        [ids["$n1"].as_str()]
    );
    let res = resolved(&st, "v1.0.0", "open");
    assert_eq!(b.selection_hash(), res.selection_hash);
    assert_eq!(b.selection_identity(), res.selection_identity);
    assert_eq!(
        b.docs().iter().map(|d| d.digest).collect::<Vec<_>>(),
        res.docs.iter().map(|d| d.digest).collect::<Vec<_>>()
    );
    assert!(
        !String::from_utf8(b.to_bytes())
            .unwrap()
            .contains("Planned.")
    );
    // the second note is newly hidden against the baseline, as inside the store
    let (same, _) = same_store(&st, "v2.0.0", "v1.0.0", "open");
    let (diff, _) = against(&st, "v2.0.0", "open", &b, None).unwrap();
    assert_eq!(diff.newly_hidden_planned, same.newly_hidden_planned);
    assert_eq!(diff.newly_hidden_planned.blocks, 1);

    // the figure's light and dark rows: light first, then dark, each by block and variant
    let site = publication::make_baseline(&st, "tag:v1.0.0", "site").unwrap();
    let rows: Vec<_> = site
        .attachments()
        .iter()
        .filter(|a| a.row.block.as_str() == ids["$bf"])
        .collect();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].row.variant, None);
    assert_eq!(
        rows[1].row.variant,
        Some(publication::AttachmentVariant::Dark)
    );
    let json = site.to_json();
    let dark = json["attachments"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a.get("variant").is_some())
        .collect::<Vec<_>>();
    assert_eq!(dark.len(), 1);
    assert_eq!(dark[0]["variant"], "dark");
    let (same, _) = same_store(&st, "v2.0.0", "v1.0.0", "site");
    let (diff, _) = against(&st, "v2.0.0", "site", &site, None).unwrap();
    assert_eq!(diff.added_attachments, same.added_attachments);
    assert_eq!(diff.removed_attachments, same.removed_attachments);
    assert_eq!(diff.changed_attachments, same.changed_attachments);
    assert!(!diff.added_attachments.is_empty());
}

#[test]
fn scc7_hash_binding_format_and_limits_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let (r, _) = project(dir.path());
    let st = r.read();
    let b = publication::make_baseline(&st, "tag:v1.0.0", "site").unwrap();
    let good = binding_of(&b);
    against(&st, "v2.0.0", "site", &b, Some(&good)).unwrap();
    // `1.0.0` names the tag `v1.0.0`
    let short = BaselineBinding {
        tag: "1.0.0".into(),
        ..good.clone()
    };
    against(&st, "v2.0.0", "site", &b, Some(&short)).unwrap();
    // a binding that names another repo id, tag, profile version or selection identity
    let other: RepoId = "00000000000000000000000000000001".parse().unwrap();
    let cases = [
        BaselineBinding {
            repo_id: other,
            ..good.clone()
        },
        BaselineBinding {
            tag: "v2.0.0".into(),
            ..good.clone()
        },
        BaselineBinding {
            profile_version: Some(Hash::of(b"another version")),
            ..good.clone()
        },
        BaselineBinding {
            selection_identity: Hash::of(b"another identity"),
            ..good.clone()
        },
    ];
    for bad in &cases {
        let e = against(&st, "v2.0.0", "site", &b, Some(bad)).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                "E_PUBLICATION_BASELINE",
                format!(
                    "The publication baseline does not match the previous publication {} of {}.",
                    bad.tag, bad.repo_id
                )
                .as_str(),
                "Make the baseline again from that tag, or pass the record of the publication it came from."
            )
        );
    }
    // the binding is checked before the profile: another profile with a wrong binding names
    // the binding
    let e = against(&st, "v2.0.0", "open", &b, Some(&cases[0])).unwrap_err();
    assert_eq!(e.code, "E_PUBLICATION_BASELINE");
    let e = against(&st, "v2.0.0", "open", &b, None).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            "E_BAD_INPUT",
            "Disclosure comparison requires the same publication profile."
        )
    );
    // a tampered or reformatted baseline never reads
    let v = b.to_json();
    let mut tampered = v.clone();
    tampered["selection_hash"] = json!(Hash::of(b"other").to_string());
    assert_verify(&read_value(&tampered).unwrap_err());
    let mut format = v.clone();
    format["format"] = json!("rdy-publication-baseline-0");
    assert_verify(&read_value(&rehash(format)).unwrap_err());
    let mut hash = v.clone();
    hash["baseline_hash"] = json!(b.baseline_hash().to_string().to_uppercase());
    assert_verify(&read_value(&hash).unwrap_err());
    assert_verify(&Baseline::read(b"").unwrap_err());
    assert_verify(&Baseline::read(b"[]").unwrap_err());
    // what reads back compares the same
    let back = Baseline::read(&b.to_bytes()).unwrap();
    assert_eq!(back, b);
}

#[test]
fn scc7_sibling_baseline_needs_logged_common_lineage() {
    let dir = tempfile::tempdir().unwrap();
    let (r, _) = project(&dir.path().join("src"));
    let st = r.read();
    let own = publication::make_baseline(&st, "tag:v1.0.0", "site").unwrap();
    assert!(own.source().fork_origins.is_empty());
    // two forks of the source: each logs the source as its fork origin
    clone(CloneSource::State(&st), &dir.path().join("a"), true).unwrap();
    clone(CloneSource::State(&st), &dir.path().join("b"), true).unwrap();
    // an ordinary clone keeps the source's repo id
    clone(CloneSource::State(&st), &dir.path().join("plain"), false).unwrap();
    drop(st);
    r.close().unwrap();
    let a = Rdy::open(&dir.path().join("a")).unwrap();
    let b = Rdy::open(&dir.path().join("b")).unwrap();
    let plain = Rdy::open(&dir.path().join("plain")).unwrap();
    let (sa, sb, sp) = (a.read(), b.read(), plain.read());
    let from_a = publication::make_baseline(&sa, "tag:v1.0.0", "site").unwrap();
    assert_eq!(from_a.source().fork_origins, vec![own.repo_id()]);
    // the source's baseline in a fork, a fork's in the source, a sibling's in a sibling, and
    // the source's in an ordinary clone
    against(&sa, "v2.0.0", "site", &own, None).unwrap();
    against(&sb, "v2.0.0", "site", &from_a, None).unwrap();
    against(&sp, "v2.0.0", "site", &own, None).unwrap();
    // a project with the same content and no logged lineage is refused
    let (u, _) = project(&dir.path().join("unrelated"));
    let su = u.read();
    for x in [&own, &from_a] {
        let e = against(&su, "v2.0.0", "site", x, None);
        // the profile ids differ in another project: a same-id profile is the lineage case
        let e = e.unwrap_err();
        assert!(e.code == "E_BAD_INPUT" || e.message == LINEAGE, "{e}");
    }
    let e = publication::check_baseline_lineage(&su, &own).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_PUBLICATION_BASELINE",
            LINEAGE,
            "Use a baseline from this project or from a project that shares a fork origin with it."
        )
    );
    publication::check_baseline_lineage(&sa, &own).unwrap();
    publication::check_baseline_lineage(&sb, &from_a).unwrap();
}

#[test]
fn scc7_capture_refuses_epoch_change() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, ids) = project(dir.path());
    let stale = r.read();
    publication::make_baseline(&stale, "tag:v1.0.0", "site").unwrap();
    remove(&mut r, &ids["$bp"]);
    let e = publication::make_baseline(&stale, "tag:v1.0.0", "site").unwrap_err();
    assert_eq!(e.code, "E_REDACTED_STATE", "{e}");
    let landing = match scope(&r.read(), "v1.0.0") {
        rdy_engine::Scope::At(h) => h,
        _ => unreachable!(),
    };
    let e = publication::make_baseline_at(&stale, "v1.0.0", landing, "site").unwrap_err();
    assert_eq!(e.code, "E_REDACTED_STATE", "{e}");
    // a fresh read makes it, with the removal applied: the removal took the version v2.0.0
    // holds
    let fresh = publication::make_baseline(&r.read(), "tag:v2.0.0", "site").unwrap();
    let bp = fresh
        .entries()
        .iter()
        .find(|e| e.id.as_str() == ids["$bp"])
        .unwrap();
    assert_eq!(bp.label, CONTENT_REMOVED);
}

#[test]
fn scc7_source_absence_does_not_expire_baseline() {
    let dir = tempfile::tempdir().unwrap();
    let (r, _) = project(&dir.path().join("src"));
    let st = r.read();
    let bytes = publication::make_baseline(&st, "tag:v1.0.0", "site")
        .unwrap()
        .to_bytes();
    clone(CloneSource::State(&st), &dir.path().join("fork"), true).unwrap();
    drop(st);
    r.close().unwrap();
    std::fs::remove_dir_all(dir.path().join("src")).unwrap();
    // the source is gone: the baseline still reads and compares in the fork
    let b = Baseline::read(&bytes).unwrap();
    let fork = Rdy::open(&dir.path().join("fork")).unwrap();
    let st = fork.read();
    let (diff, got) = against(&st, "v2.0.0", "site", &b, Some(&binding_of(&b))).unwrap();
    assert!(!diff.added_docs.is_empty());
    assert_eq!(got["disclosure"]["baseline"]["tag"], "v1.0.0");
}

/// A change of the fixture state: block `id` changed by `f`.
fn edit(id: &'static str, f: impl Fn(&mut Block) + 'static) -> Change {
    Box::new(move |mut v: Vec<Block>| {
        for b in v.iter_mut().filter(|b| b.id.as_str() == id) {
            f(b);
        }
        v
    })
}

#[test]
fn scc7_each_part_and_move_counts_match_same_store_in_memory() {
    let (_d, r) = store();
    let st = r.read();
    let before = fixture_blocks();
    let cases: Vec<(&str, Change, bool)> = vec![
        (
            "title",
            edit("s-SECT1", |b| {
                b.fields.insert("title".into(), s("Opening"));
            }),
            true,
        ),
        (
            "text",
            edit("p-PRS03", |b| b.body = "See {{t-TERM1}}.".into()),
            true,
        ),
        (
            "links",
            edit("p-NNN01", |b| {
                b.links.clear();
            }),
            true,
        ),
        (
            "field",
            edit("p-PRS02", |b| {
                b.fields.insert("custom".into(), FieldValue::Int(-1));
            }),
            true,
        ),
        (
            "figure text",
            edit("g-FGR01", |b| {
                b.fields.insert("alt".into(), s("Another figure"));
            }),
            true,
        ),
        (
            "statement",
            edit("D-DEC01", |b| {
                b.fields.insert("statement".into(), s("Ship it twice."));
            }),
            true,
        ),
        (
            "derived status",
            edit("D-XT001", |b| {
                b.fields.insert("status".into(), s("rejected"));
            }),
            true,
        ),
        (
            "rationale only",
            edit("D-DEC01", |b| {
                b.fields.insert("rationale".into(), s("Because."));
            }),
            false,
        ),
        (
            "pins only",
            edit("p-PRS01", |b| {
                b.pins.insert(bid("t-TERM1"), Hash::of(b"a pin"));
            }),
            false,
        ),
        (
            "order key only",
            edit("p-PRS03", |b| b.order = "cc".into()),
            false,
        ),
        (
            "shared siblings",
            edit("t-TERM1", |b| b.order = "ee".into()),
            true,
        ),
        (
            "cross doc",
            edit("p-BETA1", |b| b.parent = Some(bid("d-APHA1"))),
            true,
        ),
        (
            "doc title",
            edit("d-BETA1", |b| {
                b.fields.insert("title".into(), s("Beta two"));
            }),
            true,
        ),
    ];
    let bm = synth(&before);
    let bres = publication::resolve_in_for_tests(&st, &bm, "site").unwrap();
    let facts = BaselineSource {
        repo_id: st.repo_id().unwrap(),
        fork_origins: vec![],
        tag: "v1.0.0".into(),
        landing: Hash::of(b"landing"),
    };
    let baseline = publication::build_baseline(facts, &bres, &source(&st)).unwrap();
    for (what, change, counted) in cases {
        let am = synth(&change(before.clone()));
        let ares = publication::resolve_in_for_tests(&st, &am, "site").unwrap();
        let diff = publication::disclosure_diff(&bres, &ares).unwrap();
        let review =
            PublicationReview::build(&source(&st), &am, &ares, Some((&bres, &diff))).unwrap();
        let same = shown(&ares, &diff, &review);
        let (bdiff, breview) =
            publication::compare_with_baseline(&st, &source(&st), &am, &ares, &baseline, None)
                .unwrap();
        let got = shown(&ares, &bdiff, &breview);
        assert_eq!(
            as_same_store(got.clone()),
            as_same_store(same.clone()),
            "{what}"
        );
        let rows = got["disclosure"]["changed_docs"]
            .as_array()
            .unwrap()
            .iter()
            .chain(
                got["disclosure"]["changed_outside_docs"]
                    .as_array()
                    .unwrap(),
            )
            .count();
        if counted {
            assert!(rows > 0, "{what}: {got}");
        } else {
            let changes: usize = got["disclosure"]["changed_docs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|d| d["changes"].as_array().map_or(0, Vec::len))
                .sum();
            assert_eq!(changes, 0, "{what}: {got}");
        }
    }
}
