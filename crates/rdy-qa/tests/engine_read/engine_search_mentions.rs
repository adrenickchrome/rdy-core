//! smen1: the `mentions` search filter (FW-SR4): blocks that name an entity and are linked to it,
//! or name one they are not linked to yet; through the engine, MCP and the CLI.
use rdy_engine::search::{SearchFilters, SearchResult};
use rdy_engine::*;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use rdy_qa::mcp::McpClient;
use serde_json::json;

/// Doc `ops`, section "Ledger notes, nightly" (structural: it cannot link): entity Ledger service
/// (alias ledger); claim c1 names the ledger and describes it; claim c2 names the ledger with no
/// link; prose p3 names nothing. Every block but the entity says "nightly". Landed. Returns
/// (entity, c1, c2, p3).
fn seed(r: &mut Rdy) -> (String, String, String, String) {
    let w = write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Ledger notes, nightly"}},
        {"op": "create", "type": "entity", "id": "$e", "parent": "$s", "fields": {"name": "Ledger service", "aliases": ["ledger"]}},
        {"op": "create", "type": "claim", "id": "$c1", "parent": "$s", "body": "The ledger closes nightly.", "links": {"describes": [{"target": "$e"}]}},
        {"op": "create", "type": "claim", "id": "$c2", "parent": "$s", "body": "The ledger is copied nightly."},
        {"op": "create", "type": "prose", "id": "$p3", "parent": "$s", "body": "Backups run nightly."}
    ]))
    .unwrap();
    land(r, "work").unwrap();
    let c = |k: &str| w.created[k].clone();
    (c("$e"), c("$c1"), c("$c2"), c("$p3"))
}

fn find(r: &Rdy, mentions: Option<&str>) -> Result<SearchResult, RdyError> {
    r.read().search(
        &Scope::Canon,
        &SearchQuery {
            text: "nightly".into(),
            filters: SearchFilters {
                mentions: mentions.map(String::from),
                ..Default::default()
            },
            ..Default::default()
        },
        None,
        Some(100_000),
    )
}

fn ids(res: &SearchResult) -> Vec<String> {
    let mut v: Vec<String> = res
        .matches
        .iter()
        .flat_map(|m| std::iter::once(m.id.clone()).chain(m.also.iter().map(|a| a.id.clone())))
        .collect();
    v.sort();
    v
}

#[test]
fn smen1_linked_and_unlinked_mentions_filter_the_matches() {
    let (_d, mut r) = project();
    let (e, c1, c2, _p3) = seed(&mut r);
    assert_eq!(
        find(&r, None).unwrap().total,
        4,
        "guard: the section, c1, c2 and p3 say nightly"
    );
    assert_eq!(ids(&find(&r, Some("linked")).unwrap()), vec![c1.clone()]);
    assert_eq!(
        ids(&find(&r, Some("unlinked")).unwrap()),
        vec![c2.clone()],
        "the section names the ledger but cannot link, and p3 names nothing"
    );
    let err = find(&r, Some("maybe")).unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str()),
        (E_BAD_INPUT, "mentions: \"maybe\" is not linked or unlinked")
    );
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": c2, "links": {"describes": [{"target": e}]}}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    assert!(find(&r, Some("unlinked")).unwrap().matches.is_empty());
    let mut both = vec![c1, c2];
    both.sort();
    assert_eq!(ids(&find(&r, Some("linked")).unwrap()), both);
}

#[test]
fn smen1_rdy_search_takes_mentions_over_mcp() {
    let (dir, mut r) = project();
    let (_e, _c1, c2, _p3) = seed(&mut r);
    drop(r);
    let mut c = McpClient::start(dir.path());
    assert!(
        c.call(
            "rdy_session",
            json!({"actor": "agent:m", "run": "m1", "role": "writer"})
        )
        .code()
        .is_none()
    );
    let v = c
        .call(
            "rdy_search",
            json!({"query": "nightly", "scope": "canon", "mentions": "unlinked"}),
        )
        .ok();
    assert_eq!(
        (v["total"].as_u64(), v["matches"][0]["id"].as_str()),
        (Some(1), Some(c2.as_str())),
        "{v}"
    );
    assert_eq!(
        c.call(
            "rdy_search",
            json!({"query": "nightly", "mentions": "nope"})
        )
        .code(),
        Some("E_BAD_INPUT")
    );
    c.alive();
}

#[test]
fn smen1_rdy_search_takes_mentions_on_the_cli() {
    let s = Served::new();
    let c2 = {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        let (_e, _c1, c2, _p3) = seed(&mut r);
        r.close().unwrap();
        c2
    };
    let (ok, v) = s.json(&["--json", "search", "nightly", "--mentions", "unlinked"]);
    assert!(ok, "{v}");
    assert_eq!(
        (v["total"].as_u64(), v["matches"][0]["id"].as_str()),
        (Some(1), Some(c2.as_str())),
        "{v}"
    );
    let (ok, v) = s.json(&["--json", "search", "nightly", "--mentions", "nope"]);
    assert!(!ok);
    assert_eq!(v["code"], "E_BAD_INPUT", "{v}");
    s.stop();
}

#[test]
fn cc14_a_dismissed_mention_is_not_unlinked() {
    let (_d, mut r) = project();
    let (e, c1, c2, _p3) = seed(&mut r);
    assert_eq!(ids(&find(&r, Some("unlinked")).unwrap()), vec![c2.clone()]);
    r.review_mention(
        &writer(),
        &Scope::Canon,
        &c2,
        &e,
        rdy_engine::mention_review::MentionVerdict::Dismiss,
        "everyday word here",
    )
    .unwrap();
    assert!(
        find(&r, Some("unlinked")).unwrap().matches.is_empty(),
        "a dismissed mention is settled"
    );
    assert_eq!(
        ids(&find(&r, Some("linked")).unwrap()),
        vec![c1],
        "linked is unchanged"
    );
    r.review_mention(
        &writer(),
        &Scope::Canon,
        &c2,
        &e,
        rdy_engine::mention_review::MentionVerdict::Restore,
        "it is the ledger service",
    )
    .unwrap();
    assert_eq!(
        ids(&find(&r, Some("unlinked")).unwrap()),
        vec![c2],
        "a restore makes it unlinked again"
    );
}

/// Final review I5 (D-JV4QB, D-E6JD1): a dismissed (block, entity) pair gets no link suggestion
/// on a search hit either, until a restore. Searching the alias `ledger` resolves the entity;
/// the unlinked hit c2 carries a suggestion to link it before the dismissal and after the
/// restore, and none between.
#[test]
fn cc14_a_dismissed_mention_gets_no_link_suggestion_on_a_search_hit() {
    let (_d, mut r) = project();
    let (e, _c1, c2, _p3) = seed(&mut r);
    let suggested = |r: &Rdy| -> bool {
        let res = r
            .read()
            .search(
                &Scope::Canon,
                &SearchQuery {
                    text: "ledger".into(),
                    ..Default::default()
                },
                None,
                Some(100_000),
            )
            .unwrap();
        let hits: Vec<_> = res.matches.iter().filter(|m| m.id == c2).collect();
        assert_eq!(hits.len(), 1, "guard: c2 is a hit: {:?}", res.matches);
        !hits[0].suggestions.is_empty()
    };
    assert!(
        suggested(&r),
        "guard: an unlinked mention gets a suggestion"
    );
    r.review_mention(
        &writer(),
        &Scope::Canon,
        &c2,
        &e,
        rdy_engine::mention_review::MentionVerdict::Dismiss,
        "everyday word here",
    )
    .unwrap();
    assert!(!suggested(&r), "a dismissed mention is settled");
    r.review_mention(
        &writer(),
        &Scope::Canon,
        &c2,
        &e,
        rdy_engine::mention_review::MentionVerdict::Restore,
        "it is the ledger service",
    )
    .unwrap();
    assert!(suggested(&r), "a restore brings the suggestion back");
}
