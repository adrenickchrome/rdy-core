//! nb1: one name matcher (D-YXBXH, D-JDW8A, D-NS6B3). The last word of a name or a text phrase
//! also yields its forms without a final `es` and without a final `s` (not `ss`); two phrases
//! match when their key sets meet. Every name user reads it. An everyday entity counts only the
//! mentions of a block tied to it (O5).
use rdy_engine::lexicon::{Lexicon, Mention, NameKind};
use rdy_engine::mention_review::MentionVerdict;
use rdy_engine::suggest::{MentionedBy, Suggestion};
use rdy_engine::tensions::TensionKind;
use rdy_engine::*;
use rdy_model::{Block, BlockId, FieldValue, Link};
use rdy_qa::fixtures::*;
use rdy_schema::PackSet;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn work() -> Scope {
    Scope::View("work".into())
}

fn block(r: &Rdy, b: &str) -> Block {
    r.read().block(&work(), &id(b)).unwrap()
}

fn mentions(r: &Rdy, b: &str) -> Vec<Mention> {
    r.read().lexicon(&work()).unwrap().mentions(&block(r, b))
}

/// The (block, target, mention) of each link suggestion of `w`.
fn links(r: &Rdy, w: &WriteResult) -> BTreeSet<(String, String, String)> {
    r.read()
        .suggestions(&work(), &w.change.parse().unwrap())
        .unwrap()
        .0
        .into_iter()
        .filter_map(|s| match s {
            Suggestion::Link {
                block,
                target,
                mention,
                ..
            } => Some((block, target, mention)),
            Suggestion::Entity { .. } => None,
        })
        .collect()
}

/// The entity ids of the case of the request whose blocks are exactly `pair`.
fn case_entities(r: &Rdy, pair: [&str; 2]) -> Vec<String> {
    let st = r.read();
    let want: BTreeSet<BlockId> = pair.iter().map(|b| id(b)).collect();
    let q = st
        .review_requests()
        .unwrap()
        .into_iter()
        .find(|q| {
            q.items
                .iter()
                .map(|i| i.block.clone())
                .collect::<BTreeSet<_>>()
                == want
        })
        .unwrap_or_else(|| panic!("no request for {pair:?}"));
    st.case_of(&q.key, &q.items)
        .unwrap()
        .entities
        .into_iter()
        .map(|e| e.id)
        .collect()
}

fn mention(entity: &str, field: &str, text: &str) -> Mention {
    Mention {
        entity: entity.into(),
        field: field.into(),
        text: text.into(),
    }
}

#[test]
fn nb1_every_name_user_uses_the_same_keys() {
    let (_d, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Summary"}},
        {"op": "create", "type": "entity", "id": "$e", "parent": "$s", "body": "Ships on a fixed day.", "fields": {"name": "Release train", "aliases": ["Pass"], "key": "REL-TRAINS"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "body": "Two edits that disagree.", "fields": {"name": "Conflicts"}}
    ]))
    .unwrap();
    let (s, e, t) = (
        w.created["$s"].clone(),
        w.created["$e"].clone(),
        w.created["$t"].clone(),
    );
    let w2 = write(&mut r, "work", json!([
        {"op": "create", "type": "claim", "id": "$c", "parent": s, "body": "Two release trains ship each week."},
        {"op": "create", "type": "claim", "id": "$k", "parent": s, "body": "Passes resolve every conflict."}
    ]))
    .unwrap();
    let (c, k) = (w2.created["$c"].clone(), w2.created["$k"].clone());

    // mentions: a plural text names a singular name, a singular text a plural name, an alias too
    assert_eq!(
        mentions(&r, &c),
        vec![mention(&e, "body", "release trains")]
    );
    assert_eq!(
        mentions(&r, &k),
        vec![
            mention(&e, "body", "Passes"),
            mention(&t, "body", "conflict")
        ]
    );

    // suggestions
    assert_eq!(
        links(&r, &w2),
        BTreeSet::from([
            (c.clone(), e.clone(), "release trains".to_string()),
            (k.clone(), e.clone(), "Passes".to_string()),
            (k.clone(), t.clone(), "conflict".to_string()),
        ])
    );

    // dismiss and restore a mention
    let dismissed = r
        .review_mention(
            &writer(),
            &work(),
            &k,
            &t,
            MentionVerdict::Dismiss,
            "a passing word",
        )
        .unwrap();
    assert_eq!(dismissed.status, "dismissed");
    let restored = r
        .review_mention(
            &writer(),
            &work(),
            &k,
            &t,
            MentionVerdict::Restore,
            "it is the term",
        )
        .unwrap();
    assert_eq!(restored.status, "open");

    // the semantic case names the entity both blocks name
    assert_eq!(case_entities(&r, [&c, &k]), vec![e.clone()]);

    // fact-sheet selection: names and aliases fold, a key stays exact
    let st = r.read();
    assert_eq!(
        st.resolve_entities(&work(), "Release Trains").unwrap(),
        vec![(id(&e), NameKind::Name)]
    );
    assert_eq!(
        st.resolve_entities(&work(), "passes").unwrap(),
        vec![(id(&e), NameKind::Alias)]
    );
    assert_eq!(
        st.resolve_entities(&work(), "conflict").unwrap(),
        vec![(id(&t), NameKind::Name)]
    );
    assert_eq!(
        st.resolve_entities(&work(), "REL-TRAINS").unwrap(),
        vec![(id(&e), NameKind::Key)]
    );
    assert!(
        st.resolve_entities(&work(), "rel-train")
            .unwrap()
            .is_empty(),
        "a key is an exact identifier"
    );

    // entity search
    let sheets = st
        .search(
            &work(),
            &SearchQuery {
                text: "release trains".into(),
                ..Default::default()
            },
            None,
            Some(100_000),
        )
        .unwrap()
        .entities;
    assert_eq!(
        sheets
            .iter()
            .map(|f| (f.id.clone(), f.matched_by))
            .collect::<Vec<_>>(),
        vec![(e.clone(), NameKind::Name)]
    );
    drop(st);

    // consequence entity literals: the target drops the entity, the dependent states it in the
    // plural; an exact unit and a code literal keep their own identities
    let a = write(&mut r, "work", json!([
        {"op": "create", "type": "claim", "id": "$a", "parent": s, "body": "The release train keeps `max_conn` for 30 days."}
    ]))
    .unwrap()
    .created["$a"]
        .clone();
    let b = write(&mut r, "work", json!([
        {"op": "create", "type": "claim", "id": "$b", "parent": s, "body": format!("Release Trains keep `max_conns` for 30 day windows; see {{{{{a}}}}}.")}
    ]))
    .unwrap()
    .created["$b"]
        .clone();
    let edit = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": a, "body": "Weekly."}]),
    )
    .unwrap();
    let cons = r
        .read()
        .consequences(&work(), &edit.change.parse().unwrap(), None)
        .unwrap();
    let row = cons
        .items
        .iter()
        .find(|i| i.block == b)
        .unwrap_or_else(|| panic!("no entry for the dependent: {cons:#?}"));
    assert_eq!(
        row.matched
            .iter()
            .map(|m| (m.value.as_str(), m.field.as_str(), m.line, m.span))
            .collect::<Vec<_>>(),
        vec![("release train", "body", 1, [0, 14])],
        "{row:#?}"
    );

    // doc-title mentions
    let w6 = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Release Note", "doc_type": "page"}},
        {"op": "create", "type": "claim", "id": "$m", "parent": s, "body": "Every release notes the fix."}
    ]))
    .unwrap();
    let (n, m) = (w6.created["$n"].clone(), w6.created["$m"].clone());
    assert_eq!(
        r.read()
            .mentioned_by(&work(), &BTreeSet::from([id(&n)]))
            .unwrap(),
        (
            vec![MentionedBy {
                doc: n,
                block: m,
                field: "body".into(),
                mention: "release notes".into(),
            }],
            0
        )
    );
}

/// A raw block with string `fields`, then `extra` fields as given.
fn raw_with(
    id: &str,
    ty: &str,
    parent: &str,
    fields: &[(&str, &str)],
    extra: &[(&str, FieldValue)],
    body: &str,
) -> Block {
    let mut b = raw(id, ty, Some(parent), "V", fields, body);
    for (k, v) in extra {
        b.fields.insert(k.to_string(), v.clone());
    }
    b
}

fn link_to(b: &mut Block, name: &str, target: &str) {
    b.links.entry(name.into()).or_default().push(Link {
        target: id(target),
        part: None,
    });
}

#[test]
fn nb1_everyday_counts_only_linked_mentions() {
    let (_d, mut r) = project();
    let yes = FieldValue::Bool(true);
    let mut described = raw(
        "c-CCCCC",
        "claim",
        Some("s-AAAAA"),
        "V",
        &[],
        "The store keeps rows.",
    );
    link_to(&mut described, "describes", "en-AAAAA");
    raw_commit(
        &mut r,
        "work",
        vec![
            raw(
                "d-AAAAA",
                "doc",
                None,
                "V",
                &[("name", "ops"), ("title", "Ops"), ("doc_type", "overview")],
                "",
            ),
            raw(
                "s-AAAAA",
                "section",
                Some("d-AAAAA"),
                "V",
                &[("title", "Rows")],
                "",
            ),
            raw_with(
                "en-AAAAA",
                "entity",
                "s-AAAAA",
                &[("name", "Store")],
                &[("everyday", yes.clone())],
                "Holds rows.",
            ),
            raw_with(
                "en-BBBBB",
                "entity",
                "s-AAAAA",
                &[("name", "Ledger")],
                &[],
                "Holds entries.",
            ),
            raw_with(
                "en-CCCCC",
                "entity",
                "s-AAAAA",
                &[("name", "Journal")],
                &[("everyday", FieldValue::Bool(false))],
                "Holds days.",
            ),
            raw_with(
                "en-DDDDD",
                "entity",
                "s-AAAAA",
                &[("name", "Queue"), ("everyday", "true")],
                &[],
                "Holds jobs.",
            ),
            raw_with(
                "en-EEEEE",
                "entity",
                "s-AAAAA",
                &[("name", "Stores")],
                &[("everyday", yes)],
                "Holds shops.",
            ),
            raw(
                "c-AAAAA",
                "claim",
                Some("s-AAAAA"),
                "V",
                &[],
                "The stores, ledgers, journals and queues keep rows.",
            ),
            raw(
                "c-BBBBB",
                "claim",
                Some("s-AAAAA"),
                "V",
                &[],
                "The store keeps rows; see {{en-AAAAA}}.",
            ),
            described,
        ],
    );
    let affects = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent: "s-AAAAA".into(),
                statement: "The store is final".into(),
                affects: vec!["en-AAAAA".into()],
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    let ids = |ms: Vec<Mention>| ms.into_iter().map(|m| m.entity).collect::<Vec<_>>();

    // unlinked: absent, false and a non-boolean value count; true does not
    assert_eq!(
        ids(mentions(&r, "c-AAAAA")),
        ["en-BBBBB", "en-CCCCC", "en-DDDDD"]
    );
    // a reference, a describes link and an affects link each admit their target only
    assert_eq!(ids(mentions(&r, "c-BBBBB")), ["en-AAAAA"]);
    assert_eq!(ids(mentions(&r, "c-CCCCC")), ["en-AAAAA"]);
    assert_eq!(ids(mentions(&r, &affects)), ["en-AAAAA"]);

    // explicit lookup still resolves an everyday entity
    assert_eq!(
        r.read().resolve_entities(&work(), "stores").unwrap(),
        vec![
            (id("en-AAAAA"), NameKind::Name),
            (id("en-EEEEE"), NameKind::Name)
        ]
    );
    // the duplicate-definition check still works
    let dups: Vec<Vec<BlockId>> = r
        .read()
        .tensions()
        .unwrap()
        .iter()
        .filter(|t| t.kind == TensionKind::DuplicateDefinition)
        .map(|t| t.blocks())
        .collect();
    assert_eq!(dups, vec![vec![id("en-AAAAA"), id("en-EEEEE")]]);

    // suggestions and the semantic case read only linked everyday mentions
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "claim", "id": "$x", "parent": "s-AAAAA", "body": "Stores and ledgers matter."},
        {"op": "create", "type": "claim", "id": "$y", "parent": "s-AAAAA", "body": "Every store and ledger holds rows."}
    ]))
    .unwrap();
    let (x, y) = (w.created["$x"].clone(), w.created["$y"].clone());
    assert_eq!(
        links(&r, &w),
        BTreeSet::from([
            (x.clone(), "en-BBBBB".to_string(), "ledgers".to_string()),
            (y.clone(), "en-BBBBB".to_string(), "ledger".to_string()),
        ])
    );
    assert_eq!(case_entities(&r, [&x, &y]), vec!["en-BBBBB".to_string()]);
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "claim", "id": "$p", "parent": "s-AAAAA", "body": "The store keeps rows; see {{en-AAAAA}}."},
        {"op": "create", "type": "claim", "id": "$q", "parent": "s-AAAAA", "body": "The store sorts rows; see {{en-AAAAA}}."}
    ]))
    .unwrap();
    let (p, q) = (w.created["$p"].clone(), w.created["$q"].clone());
    assert_eq!(case_entities(&r, [&p, &q]), vec!["en-AAAAA".to_string()]);
}

/// A raw entity `name` with `aliases` and an optional everyday flag.
fn entity(id: &str, name: &str, aliases: &[&str], everyday: Option<bool>) -> Block {
    let mut b = raw(
        id,
        "entity",
        Some("s-AAAAA"),
        "V",
        &[("name", name)],
        "An entity.",
    );
    if !aliases.is_empty() {
        b.fields.insert(
            "aliases".into(),
            FieldValue::List(
                aliases
                    .iter()
                    .map(|a| FieldValue::Str(a.to_string()))
                    .collect(),
            ),
        );
    }
    if let Some(e) = everyday {
        b.fields.insert("everyday".into(), FieldValue::Bool(e));
    }
    b
}

#[test]
fn nb1_lexicon_carry_matches_rebuild_after_everyday_and_alias_edits() {
    let pack = PackSet::with_builtin().resolve("rdy-standard").unwrap();
    let mut blocks: BTreeMap<String, Block> = BTreeMap::new();
    for b in [
        raw("s-AAAAA", "section", None, "V", &[("title", "Rows")], ""),
        entity("en-AAAAA", "Store", &[], None),
        entity("en-BBBBB", "Ledger", &["book"], None),
        raw(
            "c-AAAAA",
            "claim",
            Some("s-AAAAA"),
            "V",
            &[],
            "The stores keep books.",
        ),
        {
            let mut d = raw(
                "c-BBBBB",
                "claim",
                Some("s-AAAAA"),
                "V",
                &[],
                "Every store counts.",
            );
            link_to(&mut d, "describes", "en-AAAAA");
            d
        },
    ] {
        blocks.insert(b.id.to_string(), b);
    }
    let mut lex = Lexicon::build(&pack, blocks.values());
    let named =
        |lex: &Lexicon, blocks: &BTreeMap<String, Block>, b: &str| -> Vec<(String, String)> {
            lex.mentions(&blocks[b])
                .into_iter()
                .map(|m| (m.entity, m.text))
                .collect()
        };
    let pair = |e: &str, t: &str| (e.to_string(), t.to_string());
    assert_eq!(
        named(&lex, &blocks, "c-AAAAA"),
        vec![pair("en-AAAAA", "stores"), pair("en-BBBBB", "books")]
    );
    // (step, block id, the block after it: None deletes it, the mentions of the two claims)
    let steps: Vec<(
        &str,
        &str,
        Option<Block>,
        Vec<(String, String)>,
        Vec<(String, String)>,
    )> = vec![
        (
            "everyday true",
            "en-AAAAA",
            Some(entity("en-AAAAA", "Store", &[], Some(true))),
            vec![pair("en-BBBBB", "books")],
            vec![pair("en-AAAAA", "store")],
        ),
        (
            "a new everyday entity",
            "en-CCCCC",
            Some(entity("en-CCCCC", "Book", &[], Some(true))),
            vec![pair("en-BBBBB", "books")],
            vec![pair("en-AAAAA", "store")],
        ),
        (
            "rename",
            "en-BBBBB",
            Some(entity("en-BBBBB", "Journal", &["book"], None)),
            vec![pair("en-BBBBB", "books")],
            vec![pair("en-AAAAA", "store")],
        ),
        (
            "alias change",
            "en-BBBBB",
            Some(entity("en-BBBBB", "Journal", &["tome"], None)),
            vec![],
            vec![pair("en-AAAAA", "store")],
        ),
        (
            "everyday true to false",
            "en-AAAAA",
            Some(entity("en-AAAAA", "Store", &[], Some(false))),
            vec![pair("en-AAAAA", "stores")],
            vec![pair("en-AAAAA", "store")],
        ),
        (
            "everyday entity deleted",
            "en-CCCCC",
            None,
            vec![pair("en-AAAAA", "stores")],
            vec![pair("en-AAAAA", "store")],
        ),
        (
            "everyday true again, then the entity deleted",
            "en-AAAAA",
            Some(entity("en-AAAAA", "Store", &[], Some(true))),
            vec![],
            vec![pair("en-AAAAA", "store")],
        ),
        ("entity deleted", "en-AAAAA", None, vec![], vec![]),
    ];
    for (step, key, new, unlinked, linked) in steps {
        let old = blocks.get(key).cloned();
        match &new {
            Some(b) => blocks.insert(key.to_string(), b.clone()),
            None => blocks.remove(key),
        };
        lex = lex.with_changes(old.iter(), new.iter());
        assert_eq!(
            lex,
            Lexicon::build(&pack, blocks.values()),
            "{step}: carried"
        );
        assert_eq!(
            named(&lex, &blocks, "c-AAAAA"),
            unlinked,
            "{step}: unlinked"
        );
        assert_eq!(named(&lex, &blocks, "c-BBBBB"), linked, "{step}: linked");
    }
}
