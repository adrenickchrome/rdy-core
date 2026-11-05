//! ab5 (D-WJ0SJ, D-69YCN, D-CWC91, D-DK03K): the live site follows as-built status. A landing
//! that changes only a decision's `built` refreshes every profile that shows a block it governs,
//! on a full route under canon, a view, a proposal and a tag, and on the profile-only listener,
//! even when the decision is outside the profile's selection. A landing that changes the built of
//! a decision no shown block cites changes no page and sends no reload. A review route compares
//! each side in its own revision. A removed and recreated profile serves its new settings.
//! Everything waits on the daemon's own refresh and subscription calls: no sleep, no clock.
use rdy_daemon::Shared;
use rdy_daemon::site::ensure_profile_site;
use rdy_daemon::site::guard::{ScopeRef, SiteTarget};
use rdy_daemon::site::live::{LiveEvent, SiteBuild, Subscription};
use rdy_engine::Rdy;
use rdy_engine::engine::write_typed;
use rdy_engine::site_scope::proposal_ref;
use rdy_model::{EditorialClass, Justification};
use rdy_qa::daemon::Hosted;
use rdy_qa::fixtures::{approver, land, parse_test_ops, write, writer};
use rdy_qa::golden::tiny;
use rdy_qa::http;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::mpsc::error::TryRecvError;

const PLAN: &str = "PLANNEDWORDS";
const SHIP: &str = "SHIPPEDWORDS";

struct World {
    h: Hosted,
    ids: BTreeMap<String, String>,
}

fn put(r: &mut Rdy, view: &str, ops: Value) -> BTreeMap<String, String> {
    write(r, view, ops)
        .unwrap_or_else(|e| panic!("write to {view}: {e}"))
        .created
        .iter()
        .map(|(k, v)| (k.clone(), v.to_string()))
        .collect()
}

/// A built-only edit of decision `id` in `view`: tracking changes carry an editorial reason.
fn set_built(r: &mut Rdy, view: &str, id: &str, built: &str) {
    r.write(
        &writer(),
        rdy_engine::WriteRequest {
            view: view.into(),
            ops: parse_test_ops(json!([{"op": "update", "id": id, "fields": {"built": built}}]))
                .unwrap(),
            justification: Some(Justification::Editorial {
                class: EditorialClass::Clarify,
                reason: "it shipped".into(),
            }),
            message: String::new(),
        },
    )
    .unwrap_or_else(|e| panic!("set built in {view}: {e}"));
}

/// Canon holds decision docs `rules` (the planned `dp` that `guide`'s block `plan` cites, and
/// `dx`, which no block cites) and the doc `guide` (a shipped and a planned prose). Profiles
/// `hide` (org, hides planned) and `mark` (org, marks) select the guide. Tag `v1.0.0` is this
/// state. View `later` and proposal `prop` start from it.
fn world() -> World {
    let (dir, mut r) = tiny();
    let mut ids = put(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "doc", "id": "$rules", "fields": {"name": "rules", "title": "Rules", "doc_type": "page"}},
            {"op": "create", "type": "decision", "id": "$dp", "parent": "$rules", "fields": {"statement": "Later part.", "status": "proposed", "built": "planned"}},
            {"op": "create", "type": "decision", "id": "$dx", "parent": "$rules", "fields": {"statement": "Unused part.", "status": "proposed", "built": "planned"}},
            {"op": "create", "type": "doc", "id": "$guide", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$ship", "parent": "$guide", "body": format!("{SHIP} words.")},
            {"op": "create", "type": "prose", "id": "$plan", "parent": "$guide", "body": format!("{PLAN} words."), "links": {"decided_by": ["$dp"]}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
        ]),
    );
    land(&mut r, "w1").unwrap();
    let admin = ids["$admin"].clone();
    let guide = ids["$guide"].clone();
    ids.extend(put(
        &mut r,
        "admin",
        json!([
            {"op": "create", "type": "publication_profile", "id": "$hide", "parent": admin,
             "fields": {"key": "hide", "title": "Hide book", "audience": "org", "planned": "hide", "select": [guide]}},
            {"op": "create", "type": "publication_profile", "id": "$mark", "parent": admin,
             "fields": {"key": "mark", "title": "Mark book", "audience": "org", "planned": "mark", "select": [guide]}}
        ]),
    ));
    land(&mut r, "admin").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    // the view and the proposal each ship the planned part, through the decision outside the guide
    set_built(&mut r, "later", &ids["$dp"], "1.1.0");
    set_built(&mut r, "prop", &ids["$dp"], "1.1.0");
    let p = r
        .propose(&writer(), "prop", "Ship it", Some("The part shipped."))
        .unwrap();
    ids.insert("prop".into(), proposal_ref(&p.r#ref).unwrap());
    drop(r);
    World {
        h: Hosted::start_in(dir),
        ids,
    }
}

fn shared(h: &Hosted) -> Arc<Shared> {
    Arc::clone(h.run.as_ref().unwrap().shared())
}

/// Runs `f` on the daemon's writer.
fn on_writer<T: serde::Serialize + 'static>(
    sh: &Arc<Shared>,
    f: impl FnOnce(&mut Rdy) -> T + Send + 'static,
) -> Value {
    write_typed(&*sh.engine(), move |r| Ok(f(r))).unwrap()
}

fn profile(scope: ScopeRef, key: &str) -> SiteTarget {
    SiteTarget::Profile {
        scope,
        key: key.into(),
    }
}

fn page(b: &SiteBuild) -> String {
    String::from_utf8_lossy(&b.live.files["guide.html"]).into_owned()
}

fn build(sh: &Arc<Shared>, t: &SiteTarget) -> Arc<SiteBuild> {
    sh.site
        .refresh(sh, t)
        .unwrap_or_else(|e| panic!("{t:?}: {e}"))
}

fn sub(sh: &Arc<Shared>, target: &SiteTarget, b: &SiteBuild) -> Subscription {
    sh.site
        .subscribe(
            sh,
            target,
            "guide.html",
            &b.live.revs["guide.html"],
            &b.live.label_rev,
        )
        .unwrap_or_else(|_| panic!("the site is busy"))
}

fn empty(s: &mut Subscription) -> bool {
    s.rx.try_recv() == Err(TryRecvError::Empty)
}

fn hides(b: &SiteBuild) -> bool {
    !page(b).contains(PLAN)
}

fn marks(b: &SiteBuild) -> bool {
    page(b).contains(PLAN) && page(b).contains(">Planned</span>")
}

#[test]
fn ab5_live_scopes_refresh_after_built_only_landings() {
    let w = world();
    let sh = shared(&w.h);
    let (dp, dx) = (w.ids["$dp"].clone(), w.ids["$dx"].clone());
    let canon_hide = profile(ScopeRef::Canon, "hide");
    let canon_mark = profile(ScopeRef::Canon, "mark");
    let tag_hide = profile(ScopeRef::Tag("v1.0.0".into()), "hide");
    let view_hide = profile(ScopeRef::View("later".into()), "hide");
    let prop_hide = profile(ScopeRef::Proposal(w.ids["prop"].clone()), "hide");
    let only_hide = SiteTarget::ProfileOnly { key: "hide".into() };

    // before any landing: canon and the tag hide the planned part; the marking profile shows it
    // marked; the view and the proposal already ship it, so they show it plain
    let (c, m, t) = (
        build(&sh, &canon_hide),
        build(&sh, &canon_mark),
        build(&sh, &tag_hide),
    );
    assert!(hides(&c) && hides(&t) && marks(&m));
    let (v, p) = (build(&sh, &view_hide), build(&sh, &prop_hide));
    for b in [&v, &p] {
        assert!(page(b).contains(PLAN) && !page(b).contains(">Planned</span>"));
    }
    let only = build(&sh, &only_hide);
    assert!(hides(&only));
    // the profile-only listener serves the same bytes
    let info = ensure_profile_site(Arc::clone(&sh), "hide").unwrap();
    let url = info.url.clone().expect("the profile site listens");
    let served = |url: &str| {
        let r = http::get(&format!("{url}guide.html"));
        assert_eq!(r.status, 200, "{}", r.head);
        String::from_utf8_lossy(&r.body).into_owned()
    };
    assert!(!served(&url).contains(PLAN));

    // open pages on the full route and the marking profile
    let (mut c_sub, mut m_sub, mut t_sub) = (
        sub(&sh, &canon_hide, &c),
        sub(&sh, &canon_mark, &m),
        sub(&sh, &tag_hide, &t),
    );
    for s in [&mut c_sub, &mut m_sub, &mut t_sub] {
        assert!(empty(s), "a current page gets nothing on subscribe");
    }

    // a built-only landing on a decision no shown block cites: nothing a profile shows moves
    {
        let dx = dx.clone();
        on_writer(&sh, move |r| {
            set_built(r, "ux", &dx, "9.9.9");
            land(r, "ux").unwrap();
        });
    }
    let c2 = build(&sh, &canon_hide);
    assert_eq!(page(&c2), page(&c), "unrelated built change");
    assert!(empty(&mut c_sub) && empty(&mut m_sub), "no reload for it");

    // a built-only landing on the governing decision, outside the selection: the profile that
    // hid the block now shows it; the marking profile drops the marker; open pages reload
    {
        let dp = dp.clone();
        on_writer(&sh, move |r| {
            set_built(r, "ship", &dp, "1.1.0");
            land(r, "ship").unwrap();
        });
    }
    let (c3, m3, o3) = (
        build(&sh, &canon_hide),
        build(&sh, &canon_mark),
        build(&sh, &only_hide),
    );
    for b in [&c3, &m3, &o3] {
        assert!(page(b).contains(PLAN) && !page(b).contains(">Planned</span>"));
    }
    assert_eq!(
        c_sub.rx.try_recv(),
        Ok(LiveEvent::Reload),
        "the full route reloads"
    );
    assert_eq!(
        m_sub.rx.try_recv(),
        Ok(LiveEvent::Reload),
        "the marking profile reloads"
    );
    let only_page = String::from_utf8_lossy(&o3.live.files["guide.html"]).into_owned();
    assert!(only_page.contains(PLAN));
    assert!(
        served(&url).contains(PLAN),
        "the listener serves the new state"
    );
    // the tag keeps its frozen state
    let t3 = build(&sh, &tag_hide);
    assert_eq!(page(&t3), page(&t));
    assert!(empty(&mut t_sub));

    // a view that goes back to planned hides the block again there only; canon is unaffected
    {
        let dp = dp.clone();
        on_writer(&sh, move |r| set_built(r, "later", &dp, "planned"));
    }
    // the review route compares each side in its own revision: the old side (canon, which now
    // ships the block) shows it, the new side (the view, which plans it again) hides it, so the
    // page lists it as removed and holds its words nowhere else
    let v2 = build(&sh, &view_hide);
    let body = page(&v2);
    assert!(
        body.contains("chg-removed"),
        "the block reads as removed in review"
    );
    assert!(
        !body.contains("block-prose\" id=\"plannedwords"),
        "the new side hides it"
    );
    assert_eq!(
        body.matches(PLAN).count(),
        2,
        "only the removed line and its index row"
    );
    assert!(
        page(&build(&sh, &canon_hide)).contains(PLAN),
        "canon is unaffected"
    );
    // the proposal keeps its own version, which ships the block
    assert!(page(&build(&sh, &prop_hide)).contains(PLAN));

    // a removed profile refuses; a recreated one serves its new settings
    let hide_id = w.ids["$hide"].clone();
    {
        let hide_id = hide_id.clone();
        on_writer(&sh, move |r| {
            write(r, "gone", json!([{"op": "delete", "id": hide_id}])).unwrap();
            land(r, "gone").unwrap();
        });
    }
    assert!(
        sh.site.refresh(&sh, &canon_hide).is_err(),
        "the profile is gone"
    );
    assert!(
        sh.site.refresh(&sh, &only_hide).is_err(),
        "the profile-only site is gone"
    );
    let admin = w.ids["$admin"].clone();
    let guide = w.ids["$guide"].clone();
    on_writer(&sh, move |r| {
        put(
            r,
            "back",
            json!([{"op": "create", "type": "publication_profile", "id": "$hide2", "parent": admin,
                "fields": {"key": "hide", "title": "Hide again", "audience": "org", "planned": "mark", "select": [guide]}}]),
        );
        land(r, "back").unwrap();
    });
    let again = build(&sh, &canon_hide);
    assert!(page(&again).contains(PLAN));
}

/// A review route under a marking profile compares each side's displayed status: a built value
/// changed only on a decision outside the selection turns the block's `Planned` marker off, and
/// the review lists the block as edited though the selected block is the same on both sides.
#[test]
fn ab5_review_counts_a_status_only_change_under_a_marking_profile() {
    let w = world();
    let sh = shared(&w.h);
    let view_mark = profile(ScopeRef::View("later".into()), "mark");
    let b = build(&sh, &view_mark);
    let body = page(&b);
    assert!(body.contains(PLAN), "the block is shown on both sides");
    assert!(
        !body.contains(">Planned</span>"),
        "the new side ships the block"
    );
    assert!(
        body.contains("chg-label chg-edited") && body.contains("1 change"),
        "the status change counts as an edit"
    );
    // the same compare through the proposal route
    let prop_mark = profile(ScopeRef::Proposal(w.ids["prop"].clone()), "mark");
    let prop_page = page(&build(&sh, &prop_mark));
    assert!(prop_page.contains("chg-label chg-edited") && prop_page.contains("1 change"));
}
