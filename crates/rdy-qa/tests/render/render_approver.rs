//! ui7 (D-6M0AF, D-WPE4R, D-N507H, D-X61DT, D-TF78H, D-JQD5X, D-Y9DNZ, D-WQ5GC; TASK-979, TASK-982
//! I1, C1, O3): the approval pages of the local UI. A page shows the engine's items and complete
//! card text with the same card hash, an answer form that posts only the bound fields, a
//! question's briefing below its card, and a refusal with its code and fix in details. A page is
//! built in memory in the site's theme: it adds nothing to any site output, and it needs no
//! script for a form or for details.
use rdy_engine::answer::AnsweredInView;
use rdy_engine::pending::{
    CARD_MAX_TOKENS, ExceptionKind, Fact, Item, ItemKind, ItemOption, Pending, PendingQuery,
    Recommended, RemovalCard, card_hash, render,
};
use rdy_engine::publication::{IMPLICIT_ORG, ReadPublicationSource};
use rdy_engine::{
    Briefing, Coverage, E_ITEM_MOVED, E_VALIDATION, Rdy, RdyError, ReadState, Scope,
    site_scope::percent_encode,
};
use rdy_model::redaction::ReasonClass;
use rdy_model::{BlockId, Hash};
use rdy_qa::fixtures::{approver, land, write};
use rdy_qa::golden::tiny;
use rdy_qa::intake::{PERSON, batch_project, ready_and_not_ready};
use rdy_render::approver::{UiPage, item_page, pending_page, refusal_page};
use rdy_render::render_input::capture_inputs;
use rdy_render::{Mode, build_profile_site, build_publication, build_site, escape};
use serde_json::json;
use std::borrow::Cow;
use std::collections::BTreeMap;

const EVENTS: &str = "/_rdy/events?ui=1&rev=ab12";
const NOSCRIPT: &str = "Live updates need JavaScript. Reload this page to see the current queue.";

fn text(p: &UiPage) -> String {
    String::from_utf8(p.html.clone()).unwrap()
}

fn option(id: &str, label: &str, consequences: &[&str], rec: Option<&str>) -> ItemOption {
    ItemOption {
        id: id.into(),
        label: label.into(),
        consequences: consequences.iter().map(|c| c.to_string()).collect(),
        recommended: rec.map(Recommended::new),
    }
}

fn fact(name: &'static str, value: &str) -> Fact {
    Fact {
        name: Cow::Borrowed(name),
        value: value.into(),
    }
}

/// An item of `kind` with every structured field set, its card and hash as the engine computes
/// them.
fn item(kind: ItemKind, r#ref: &str, title: &str) -> Item {
    let mut i = Item {
        r#ref: r#ref.into(),
        kind,
        title: title.into(),
        why: "Support waits on this".into(),
        blocking: true,
        summary: Some("Lets a visitor read a shared doc without an account.".into()),
        summary_by: Some("agent:writer".into()),
        facts: vec![
            fact("Policy", "role approver, one signature"),
            fact("Imported", "2 imported changes need a person"),
            fact("Check verdict", "concern: accepted"),
        ],
        members: vec!["D-0931  Guest readers need no account".into()],
        options: vec![
            option(
                "approve",
                "Approve",
                &["Lands the version.", "Opens its decisions."],
                Some("It passes every check"),
            ),
            option("reject", "Reject", &["Keeps the version open."], None),
            option("later", "Later", &[], None),
        ],
        raised_by: "agent:writer".into(),
        raised_at: "2026-10-30T10:00:00Z".into(),
        age: "3 days".into(),
        batch: None,
        exception: None,
        card: String::new(),
        card_hash: String::new(),
        removal: None,
        removal_reports: BTreeMap::new(),
        plan_binding: None,
        check_verdicts: vec![],
    };
    seal(&mut i);
    i
}

/// `i` with its card and hash recomputed.
fn seal(i: &mut Item) {
    i.card = render(i, CARD_MAX_TOKENS);
    i.card_hash = card_hash(i).to_string();
}

fn queue(r: &Rdy) -> Pending {
    r.read()
        .pending(&PendingQuery {
            person: PERSON.into(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .unwrap()
}

/// The html of the card section of `html`: from its opening tag to its closing one.
fn card_of(html: &str) -> &str {
    let from = html.find("<section class=\"ui-card\"").expect("a card");
    let to = html[from..].find("</section>").expect("card end") + from;
    &html[from..to]
}

/// The forms of `html`, each as its opening tag and its inner html.
fn forms(html: &str) -> Vec<(String, String)> {
    let mut out = vec![];
    let mut rest = html;
    while let Some(at) = rest.find("<form") {
        let tag_end = rest[at..].find('>').unwrap() + at;
        let end = rest[tag_end..].find("</form>").unwrap() + tag_end;
        out.push((
            rest[at..=tag_end].to_string(),
            rest[tag_end + 1..end].to_string(),
        ));
        rest = &rest[end + 7..];
    }
    out
}

/// The value of attribute `name` in `tag`.
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let key = format!(" {name}=\"");
    let at = tag.find(&key)? + key.len();
    let end = tag[at..].find('"')? + at;
    Some(&tag[at..end])
}

/// The tags of `html` that start with `<{name}`, whole.
fn tags<'a>(html: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("<{name}");
    let mut out = vec![];
    let mut rest = html;
    while let Some(at) = rest.find(&open) {
        let after = &rest[at + open.len()..];
        if after.starts_with(|c: char| c.is_ascii_alphanumeric()) {
            rest = after;
            continue;
        }
        let end = rest[at..].find('>').unwrap() + at;
        out.push(&rest[at..=end]);
        rest = &rest[end + 1..];
    }
    out
}

/// The form fields a browser would post for `form` when the person presses the button of
/// `option`: the hidden inputs, the note and the button.
fn posted(inner: &str, option: &str, note: &str) -> Vec<(String, String)> {
    let mut out = vec![];
    for t in tags(inner, "input") {
        assert_eq!(attr(t, "type"), Some("hidden"), "{t}");
        out.push((
            attr(t, "name").unwrap().to_string(),
            unescape(attr(t, "value").unwrap()),
        ));
    }
    let area = tags(inner, "textarea");
    assert_eq!(area.len(), 1, "{inner}");
    out.push(("note".into(), note.into()));
    let button = tags(inner, "button")
        .into_iter()
        .find(|b| unescape(attr(b, "value").unwrap()) == option)
        .unwrap_or_else(|| panic!("a button for {option}"));
    assert_eq!(attr(button, "type"), Some("submit"));
    out.push((attr(button, "name").unwrap().to_string(), option.into()));
    out
}

fn unescape(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn href(r: &str) -> String {
    format!("/~ui/item/{}", percent_encode(r))
}

/// A project named `Tiny` with claim H and a claim that cites it with markup in its words; the
/// briefing of H.
fn briefing_with_markup() -> (tempfile::TempDir, Rdy, Briefing) {
    let (dir, mut r) = tiny();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "hub", "title": "Hub", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}},
            {"op": "create", "type": "claim", "id": "$h", "parent": "$s", "body": "H holds."}
        ]),
    )
    .unwrap();
    let h = w.created["$h"].clone();
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "claim", "parent": w.created["$s"].clone(),
            "body": format!("Cites {{{{{h}}}}}: <script>alert(\"brief\")</script> & <b>bold</b>.")}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let b = r
        .read()
        .brief(
            &Scope::Canon,
            &Coverage {
                ids: vec![BlockId::parse(&h).unwrap().to_string()],
                path: None,
            },
            None,
        )
        .unwrap();
    (dir, r, b)
}

#[test]
fn ui7_pending_and_item_render_the_complete_engine_card() {
    // the real queue of a batch: an approve card, its confirm cards by exception kind and a
    // resolve card
    let (_d, r, batch) = batch_project();
    let st = r.read();
    let pending = queue(&r);
    let kinds: Vec<ItemKind> = pending.items.iter().map(|i| i.kind).collect();
    assert!(kinds.contains(&ItemKind::Approve) && kinds.contains(&ItemKind::Confirm));
    assert!(pending.items.iter().any(|i| i.exception.is_some()));
    assert!(pending.items.iter().any(|i| i.r#ref == batch.r#ref));
    let page = text(&pending_page(&st, &pending, None, Some(EVENTS), &[]).unwrap());
    assert!(page.contains("<h1>Needs you</h1>"), "{page}");
    assert!(page.contains("<title>Needs you · "), "{page}");
    let mut at = 0;
    for i in &pending.items {
        // engine order, no new sort or filter
        let pos = page[at..]
            .find(&format!(
                "<article class=\"ui-item\" data-ref=\"{}\"",
                i.r#ref
            ))
            .unwrap_or_else(|| panic!("{} after {at}", i.r#ref));
        at += pos + 1;
        assert!(page.contains(&escape(&i.card)), "card of {}", i.r#ref);
        assert!(page.contains(&format!("<code>{}</code>", i.card_hash)));
        for f in &i.facts {
            assert!(page.contains(&format!(
                "<dt>{}</dt><dd>{}</dd>",
                escape(&f.name),
                escape(&f.value)
            )));
        }
        for o in &i.options {
            assert!(
                page.contains(&format!(" value=\"{}\"", escape(&o.id)))
                    || i.kind == ItemKind::Answer
            );
        }
    }
    // the same page for every item: the item page shows the same card and hash
    for i in &pending.items {
        let one = text(&item_page(&st, i, None, None, None, None).unwrap());
        assert!(one.contains(&escape(&i.card)) && one.contains(&i.card_hash));
        assert!(one.contains(&format!(
            "<title>{} · {}</title>",
            escape(&i.title),
            escape(&st.project_name())
        )));
    }

    // the engine's own words for the versions that are not ready, each issue with its code and fix
    let (_d2, r2, _ready) = ready_and_not_ready();
    let st2 = r2.read();
    let q2 = queue(&r2);
    assert!(!q2.not_ready.is_empty());
    let page = text(&pending_page(&st2, &q2, None, None, &[]).unwrap());
    assert!(page.contains("<h2>Not ready</h2>"), "{page}");
    for n in &q2.not_ready {
        assert!(page.contains(&format!("<code>{}</code>", n.r#ref)));
        for issue in &n.issues {
            assert!(page.contains(&format!(
                "<p class=\"refusal-message\">{}</p>",
                escape(&issue.message)
            )));
            assert!(page.contains(&format!(
                "<div class=\"refusal-popover\"><p><code>{}</code></p><p>{}</p></div>",
                issue.code,
                escape(&issue.fix)
            )));
        }
    }

    // every kind, every exception, a long fact, a removal card: all retained, in order
    let mut items = vec![];
    for (n, kind) in [
        ItemKind::Approve,
        ItemKind::Confirm,
        ItemKind::Answer,
        ItemKind::Resolve,
        ItemKind::Defer,
        ItemKind::Reaffirm,
    ]
    .into_iter()
    .enumerate()
    {
        items.push(item(
            kind,
            &format!("x-{n}@abcdef012345"),
            &format!("Title {n}"),
        ));
    }
    for (n, k) in ExceptionKind::ALL.into_iter().enumerate() {
        let mut i = item(ItemKind::Confirm, &format!("D-{n}@123456abcdef"), "Beside");
        i.batch = Some("P-ABCDE@1".into());
        i.exception = Some(k);
        seal(&mut i);
        items.push(i);
    }
    let mut long = item(ItemKind::Approve, "p-long@0123456789ab", "Long facts");
    long.facts = (0..60)
        .map(|n| fact("Fact", &format!("{n} {}", "word ".repeat(80))))
        .collect();
    long.members = (0..30).map(|n| format!("D-{n}  Decision {n}")).collect();
    seal(&mut long);
    // a queue read at a small budget cuts the card text; its hash is still the full card's
    long.card = render(&long, 400);
    assert!(
        long.card.contains("cut;"),
        "the card text cuts at its budget"
    );
    items.push(long.clone());
    let mut removal = item(ItemKind::Approve, "p-rm@0123456789ab", "Removal");
    let rc = RemovalCard {
        request: Hash::of(b"request"),
        report: Hash::of(b"report"),
        versions: 2,
        blobs: 1,
        shared_blobs: 0,
        payloads: 3,
        bytes: 900,
        reason_class: ReasonClass::Personal,
        reason: "A person asked.".into(),
    };
    removal.removal_reports.insert(rc.report, vec![]);
    removal.removal = Some(vec![rc.clone()]);
    seal(&mut removal);
    items.push(removal.clone());
    let all = Pending {
        items: items.clone(),
        not_ready: vec![],
        snoozed: 0,
        omitted: 0,
    };
    let page = text(&pending_page(&st, &all, None, None, &[]).unwrap());
    let mut at = 0;
    for i in &items {
        let pos = page[at..]
            .find(&format!("data-ref=\"{}\"", i.r#ref))
            .unwrap_or_else(|| panic!("{}", i.r#ref));
        at += pos + 1;
        assert!(page.contains(&escape(&i.card)), "{}", i.r#ref);
        assert!(page.contains(&i.card_hash));
    }
    // the cut marker and every fact the text cut are both kept
    assert!(page.contains("facts cut; rdy_pending with a larger max_tokens shows them"));
    assert_eq!(page.matches("<dt>Fact</dt>").count(), 60);
    assert!(page.contains(&format!("<code>{}</code>", rc.request)));
    assert!(page.contains("<h3>Content removal</h3>"));
    assert!(page.contains(&escape(&removal.card)) && removal.card.contains("Content removal"));
    // the exception words come from the card text, the structured line names the batch
    assert!(page.contains("Beside <code>P-ABCDE@1</code>, on its own: <code>against</code>"));
    // recommended options are marked beside the label, never in the stored value
    let rec = page
        .matches("<span class=\"ui-rec\">Recommended</span>")
        .count();
    let want: usize = items
        .iter()
        .map(|i| i.options.iter().filter(|o| o.recommended.is_some()).count())
        .sum();
    assert_eq!(rec, want);
    assert!(!page.contains("value=\"Recommended") && !page.contains("Recommended\""));
    assert!(page.contains("Why recommended: It passes every check"));
    // an option without a recommendation is never marked
    let mut none = item(ItemKind::Approve, "p-none@0123456789ab", "None");
    for o in &mut none.options {
        o.recommended = None;
    }
    seal(&mut none);
    assert!(!text(&item_page(&st, &none, None, None, None, None).unwrap()).contains("Recommended"));

    // a card beyond the pending display cap renders alone
    let capped = Pending {
        items: vec![],
        not_ready: vec![],
        snoozed: 0,
        omitted: 3,
    };
    let page = text(&pending_page(&st, &capped, None, None, &[]).unwrap());
    assert!(page.contains("3 more items wait. Answer these first, then reload this page."));
    let alone = text(&item_page(&st, &long, None, None, None, None).unwrap());
    assert!(alone.contains(&escape(&long.card)) && alone.contains(&long.card_hash));
    assert_eq!(alone.matches("<dt>Fact</dt>").count(), 60);
    // the singular and the empty queue
    let one = Pending {
        omitted: 1,
        ..capped.clone()
    };
    assert!(
        text(&pending_page(&st, &one, None, None, &[]).unwrap())
            .contains("1 more item waits. Answer these first, then reload this page.")
    );
    let empty = text(&pending_page(&st, &Pending::default(), None, None, &[]).unwrap());
    assert!(empty.contains("Nothing needs your answer."), "{empty}");
    assert!(!empty.contains("more item"));
    // a notice stands above the cards
    let noted =
        text(&pending_page(&st, &Pending::default(), Some("Saved <1>"), None, &[]).unwrap());
    assert!(noted.contains("<p class=\"ui-notice\" role=\"status\">Saved &lt;1&gt;</p>"));
}

#[test]
fn ui7_forms_submit_only_bound_answer_fields() {
    let (_d, _r, brief) = briefing_with_markup();
    let (_d2, r2) = tiny();
    let st = r2.read();
    let evil = "<script>alert(1)</script>\"><img src=x onerror=alert(2)>";
    let mut approve = item(ItemKind::Approve, "p-ABCDE@1", evil);
    approve.options[0].label = "Approve <b>now</b>".into();
    approve.options[0].id = "approve".into();
    approve.facts[0].value = evil.into();
    approve.summary = Some(evil.into());
    seal(&mut approve);
    let mut question = item(
        ItemKind::Answer,
        "q-ABCDE@abcdef012345",
        "Who approves refunds?",
    );
    question.options = vec![
        option("o1", "Finance", &[], Some("Finance owns the ledger")),
        option("o2", "Support lead", &[], None),
    ];
    seal(&mut question);
    let pending = Pending {
        items: vec![approve.clone(), question.clone()],
        not_ready: vec![],
        snoozed: 0,
        omitted: 0,
    };
    let page = text(&pending_page(&st, &pending, None, None, &[]).unwrap());
    // a question's title links to its item page; it has no form here
    let forms_here = forms(&page);
    assert_eq!(forms_here.len(), 1, "{page}");
    let (open, inner) = &forms_here[0];
    assert_eq!(attr(open, "method"), Some("post"));
    assert_eq!(attr(open, "action"), Some("/~ui/answer"));
    assert_eq!(
        attr(open, "enctype"),
        Some("application/x-www-form-urlencoded")
    );
    assert!(page.contains(&format!(
        "<a href=\"{}\">Who approves refunds?</a>",
        href("q-ABCDE@abcdef012345")
    )));
    assert!(href("q-ABCDE@abcdef012345").ends_with("q-ABCDE%40abcdef012345"));
    assert!(!page.contains("name=\"brief\""));
    // the non-question form posts the bound fields and nothing else
    assert_eq!(
        posted(inner, "reject", "maybe"),
        vec![
            ("ref".into(), "p-ABCDE@1".to_string()),
            ("card_hash".into(), approve.card_hash.clone()),
            ("note".into(), "maybe".into()),
            ("option".into(), "reject".into()),
        ]
    );
    assert!(
        inner.contains("<label class=\"ui-note-label\" for=\"note-1\">Note (optional)</label>")
    );
    assert_eq!(attr(tags(inner, "textarea")[0], "name"), Some("note"));
    let buttons: Vec<&str> = tags(inner, "button")
        .into_iter()
        .map(|b| attr(b, "value").unwrap())
        .collect();
    assert_eq!(buttons, ["approve", "reject", "later"]);
    for banned in [
        "evidence",
        "actor",
        "run",
        "role",
        "assurance",
        "imported_by",
    ] {
        assert!(
            !page.contains(&format!("name=\"{banned}\"")),
            "{banned} is not a form field"
        );
    }
    // a recommended option shows the word beside its unchanged label
    assert!(inner.contains(
        "<button type=\"submit\" name=\"option\" value=\"approve\">Approve &lt;b&gt;now&lt;/b&gt;</button> <span class=\"ui-rec\">Recommended</span>"
    ));
    // no input of the text can make markup
    for gone in ["<script>alert", "<img src=x", "<b>now", "onerror=alert(2)>"] {
        assert!(!page.contains(gone), "{gone} in {page}");
    }
    assert!(page.contains(
        "&lt;script&gt;alert(1)&lt;/script&gt;&quot;&gt;&lt;img src=x onerror=alert(2)&gt;"
    ));

    // the item page of the question: one form, the token in a hidden brief field, the briefing
    // below the card and outside it
    let one = text(&item_page(&st, &question, Some(&brief), None, Some(EVENTS), None).unwrap());
    let fs = forms(&one);
    assert_eq!(fs.len(), 1);
    assert_eq!(attr(&fs[0].0, "action"), Some("/~ui/answer"));
    assert_eq!(
        posted(&fs[0].1, "o2", ""),
        vec![
            ("ref".into(), question.r#ref.clone()),
            ("card_hash".into(), question.card_hash.clone()),
            ("brief".into(), brief.base.token.clone()),
            ("note".into(), "".into()),
            ("option".into(), "o2".into()),
        ]
    );
    let card = card_of(&one);
    assert!(!card.contains("Briefing") && !card.contains(&brief.base.token));
    let heading = one.find("<h2>Briefing</h2>").expect("the briefing heading");
    assert!(heading > one.find(card).unwrap() + card.len());
    assert!(heading < one.find("<form").unwrap());
    // the briefing's markup is escaped, its names and values show in serialized order, and the
    // card hash is the card's own
    assert!(!one.contains("<script>alert(\"brief\")"));
    assert!(one.contains(
        "&lt;script&gt;alert(&quot;brief&quot;)&lt;/script&gt; &amp; &lt;b&gt;bold&lt;/b&gt;"
    ));
    let order: Vec<usize> = ["covers", "governs", "dependents", "lineage", "base"]
        .iter()
        .map(|k| {
            one.find(&format!("<dt>{k}</dt>"))
                .unwrap_or_else(|| panic!("{k}"))
        })
        .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
    assert!(one.contains(&format!("<dt>token</dt><dd>{}</dd>", brief.base.token)));
    let without = text(&item_page(&st, &question, None, None, None, None).unwrap());
    assert!(!without.contains("Briefing") && !without.contains("name=\"brief\""));
    assert!(without.contains(&question.card_hash) && one.contains(&question.card_hash));
    assert_eq!(card_of(&without), card);
    // a briefing given beside a card that is not a question shows nowhere
    let other = text(&item_page(&st, &approve, Some(&brief), None, None, None).unwrap());
    assert!(!other.contains("Briefing") && !other.contains("name=\"brief\""));
    // a question answered in a view: the line stands below the card and above the form, outside
    // the card; the card and its hash do not move, and the form stays
    let answered = AnsweredInView {
        item: question.r#ref.clone(),
        view: "a<b>".into(),
        decision: "D-TEST1".into(),
    };
    let line = rdy_render::escape(
        "You answered this in view a<b>: D-TEST1. It lands with that view's next proposal. Another answer records a second decision.",
    )
    .to_string();
    let noted = text(&item_page(&st, &question, None, None, None, Some(&answered)).unwrap());
    assert_eq!(noted.matches(&line).count(), 1, "{noted}");
    assert_eq!(card_of(&noted), card_of(&without));
    let at = noted.find(&line).unwrap();
    assert!(at > noted.find(card_of(&noted)).unwrap() + card_of(&noted).len());
    assert!(at < noted.find("<form").unwrap());
    assert_eq!(forms(&noted).len(), 1);
    assert_eq!(forms(&noted)[0].1, forms(&without)[0].1);
    assert!(!without.contains("You answered this"));
    // the pending page shows it only beside the card it names
    let queue = Pending {
        items: vec![approve.clone(), question.clone()],
        ..Pending::default()
    };
    let listed =
        text(&pending_page(&st, &queue, None, None, std::slice::from_ref(&answered)).unwrap());
    assert_eq!(listed.matches(&line).count(), 1, "{listed}");
    assert!(
        listed.find(&line).unwrap()
            > listed
                .find(&format!("data-ref=\"{}\"", question.r#ref))
                .unwrap()
    );
    let bare = text(&pending_page(&st, &queue, None, None, &[]).unwrap());
    assert!(!bare.contains("You answered this"));
    // the page of an item names its title and the site
    assert!(
        text(&item_page(&st, &question, None, None, None, None).unwrap())
            .contains("<title>Who approves refunds? · Tiny</title>")
    );
    assert!(one.contains("<meta name=\"robots\" content=\"noindex\">"));
}

#[test]
fn ui7_refusal_details_and_corrected_cards_are_distinct() {
    let (_d, r) = tiny();
    let st = r.read();
    let stale = Hash::of(b"stale").to_string();
    let fresh = item(ItemKind::Approve, "p-ABCDE@2", "Fresh card");
    let moved = RdyError::new(
        E_ITEM_MOVED,
        "the item changed after you read it <b>",
        "answer the corrected card",
    );
    // a corrected card that is not a question has its fresh form, and no old hash
    let page = text(&item_page(&st, &fresh, None, Some(&moved), None, None).unwrap());
    assert!(
        page.contains(
            "<p class=\"refusal-message\">the item changed after you read it &lt;b&gt;</p>"
        )
    );
    assert!(page.contains(&format!(
        "<div class=\"refusal-popover\"><p><code>{E_ITEM_MOVED}</code></p><p>answer the corrected card</p></div>"
    )));
    // the code and the fix appear only in the details
    let plain = page.replace(
        &format!("<p><code>{E_ITEM_MOVED}</code></p><p>answer the corrected card</p>"),
        "",
    );
    assert!(!plain.contains(E_ITEM_MOVED) && !plain.contains("answer the corrected card"));
    assert!(!page.contains(&stale));
    let fs = forms(&page);
    assert_eq!(fs.len(), 1);
    assert_eq!(posted(&fs[0].1, "approve", "")[1].1, fresh.card_hash);
    for gone in ["request", "Request", "req-", "<script>alert"] {
        assert!(!page.contains(gone), "{gone}");
    }

    // a corrected question shows its current card and the item link: no form, no briefing, no token
    let (_d2, _r2, brief) = briefing_with_markup();
    let mut q = item(ItemKind::Answer, "q-ABCDE@abcdef012345", "Which way?");
    q.options = vec![
        option("o1", "East", &[], None),
        option("o2", "West", &[], None),
    ];
    seal(&mut q);
    let page = text(&item_page(&st, &q, Some(&brief), Some(&moved), None, None).unwrap());
    assert!(!page.contains("<form") && !page.contains("name=\"brief\""));
    assert!(!page.contains("Briefing") && !page.contains(&brief.base.token));
    assert!(page.contains(&q.card_hash) && page.contains(&escape(&q.card)));
    assert!(page.contains(&format!("<a href=\"{}\">Which way?</a>", href(&q.r#ref))));
    assert!(page.contains(E_ITEM_MOVED));

    // a refusal with no item: its words, its details, the corrected card text and no form
    let lost = RdyError::new(E_VALIDATION, "nothing waits at that ref", "reload the page")
        .with_corrected(
            json!({"ref": "q-1@abc", "card_hash": stale, "card": "q-1  <Gone>  answer"}),
        );
    let page = text(&refusal_page(&st, &lost).unwrap());
    assert!(
        page.contains("<h1>Could not answer</h1>")
            && page.contains("<title>Could not answer · Tiny</title>")
    );
    assert!(page.contains("<p class=\"refusal-message\">nothing waits at that ref</p>"));
    assert!(page.contains(&format!(
        "<p><code>{E_VALIDATION}</code></p><p>reload the page</p>"
    )));
    assert!(page.contains("q-1  &lt;Gone&gt;  answer"));
    assert!(!page.contains("<form") && !page.contains(&stale));
    assert!(!page.contains("request") && !page.contains("Request"));
    let bare = text(&refusal_page(&st, &RdyError::new(E_VALIDATION, "no", "fix")).unwrap());
    assert!(!bare.contains("ui-card-text"));
}

#[test]
fn ui7_ui_rendering_leaves_site_outputs_identical() {
    let ops = json!([
        {"op": "create", "type": "doc", "id": "$hid", "fields": {"name": "hidden", "title": "Hidden doc", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$hs", "parent": "$hid", "fields": {"title": "Hidden section"}},
        {"op": "create", "type": "prose", "parent": "$hs", "body": "Hidden words."},
        {"op": "create", "type": "doc", "id": "$shown", "fields": {"name": "shown", "title": "Shown page", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$ss", "parent": "$shown", "fields": {"title": "Shown section"}},
        {"op": "create", "type": "prose", "parent": "$ss", "body": "Shown words."}
    ]);
    let (_d, mut r) = tiny();
    let ids = write(&mut r, "work", ops).unwrap().created;
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "publication_profile", "parent": ids["$hid"],
            "fields": {"key": "shownorg", "title": "Shown site", "audience": "org", "select": [ids["$shown"]]}}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    let tag = r
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == "v1.0.0")
        .unwrap()
        .1;
    let landing = r.store().read_tag(&tag).unwrap().landing;
    let build = |st: &ReadState| {
        let stat = build_site(st, &Scope::Canon, Mode::Static).unwrap();
        let live = build_site(st, &Scope::Canon, Mode::Live { root: "/" }).unwrap();
        let profile = build_profile_site(st, "canon", "shownorg").unwrap();
        let inputs = capture_inputs(st, &Scope::At(landing)).unwrap();
        let source = ReadPublicationSource::with_settings(
            st,
            &inputs.pack,
            &inputs.title,
            inputs.description.as_deref(),
        );
        let m = st.materialized(&Scope::At(landing)).unwrap();
        let publication =
            build_publication(&source, &m, "v1.0.0", false, IMPLICIT_ORG, &inputs).unwrap();
        (stat, live, profile, publication)
    };
    let st = r.read();
    let before = build(&st);
    // every UI page, with a live stream
    let q = item(ItemKind::Approve, "p-ABCDE@1", "A card");
    let pending = Pending {
        items: vec![q.clone()],
        ..Pending::default()
    };
    pending_page(&st, &pending, Some("note"), Some(EVENTS), &[]).unwrap();
    item_page(&st, &q, None, None, Some(EVENTS), None).unwrap();
    refusal_page(&st, &RdyError::new(E_VALIDATION, "m", "f")).unwrap();
    let after = build(&st);
    for (what, b, a) in [
        ("static", &before.0, &after.0),
        ("live", &before.1, &after.1),
        ("profile", &before.2, &after.2),
    ] {
        assert_eq!(b.files, a.files, "{what}: bytes, indexes and llms files");
        assert_eq!(
            (b.summary.pages, b.summary.assets, b.summary.redirects),
            (a.summary.pages, a.summary.assets, a.summary.redirects),
            "{what}: counts"
        );
        for (path, bytes) in &a.files {
            let t = String::from_utf8_lossy(bytes);
            assert!(
                !t.contains("/~ui/") && !t.contains("ui-card"),
                "{what} {path}"
            );
        }
    }
    assert_eq!(before.3.files, after.3.files);
    assert_eq!(before.3.manifest.files, after.3.manifest.files);
    for (path, bytes) in &after.3.files {
        let t = String::from_utf8_lossy(bytes);
        assert!(!t.contains("/~ui/"), "publication {path}");
    }
}

#[test]
fn ui7_forms_and_details_need_no_script() {
    let (_d, r) = tiny();
    let st = r.read();
    let init = std::fs::read_to_string(rdy_qa::site::theme_js("init.js")).unwrap();
    let mut approve = item(ItemKind::Approve, "p-ABCDE@1", "Approve me");
    approve.options.push(option("more", "More time", &[], None));
    seal(&mut approve);
    let q = item(ItemKind::Answer, "q-ABCDE@abcdef012345", "A question");
    let (_d2, _r2, brief) = briefing_with_markup();
    let pending = Pending {
        items: vec![approve.clone(), q.clone()],
        not_ready: vec![],
        snoozed: 0,
        omitted: 0,
    };
    let stale = RdyError::new(E_ITEM_MOVED, "moved", "reload");
    let pages = [
        text(&pending_page(&st, &pending, None, Some(EVENTS), &[]).unwrap()),
        text(&pending_page(&st, &pending, None, None, &[]).unwrap()),
        text(&item_page(&st, &approve, None, None, Some(EVENTS), None).unwrap()),
        text(&item_page(&st, &q, Some(&brief), None, Some(EVENTS), None).unwrap()),
        text(&item_page(&st, &approve, None, Some(&stale), None, None).unwrap()),
        text(&refusal_page(&st, &stale).unwrap()),
    ];
    for page in &pages {
        // the only scripts are the theme's: its inline start, its app file and, with a stream, the
        // live file with the caller's escaped URL
        let mut scripts = tags(page, "script");
        scripts.sort();
        for s in &scripts {
            let ok = *s == "<script>"
                || *s == "<script src=\"/assets/app.js\" defer>"
                || *s
                    == format!(
                        "<script src=\"/_rdy/live.js\" data-events=\"{}\" defer>",
                        escape(EVENTS)
                    );
            assert!(ok, "{s}");
        }
        assert_eq!(page.matches("</script>").count(), scripts.len());
        assert!(page.contains(&format!("<script>{init}</script>")));
        assert!(!page.contains("<script src=\"/_rdy/live.js\"") || page.contains(&escape(EVENTS)));
        // no handler attribute, no automatic refresh
        for t in [
            "<form",
            "<button",
            "<a ",
            "<details",
            "<textarea",
            "<input",
            "<body",
            "<main",
        ] {
            for tag in tags(page, &t[1..]) {
                assert!(!tag.contains(" on"), "{tag}");
            }
        }
        assert!(!page.contains("http-equiv"));
        assert!(page.contains("<noscript>"));
    }
    // the live file only where the caller gave a stream
    assert!(pages[0].contains("data-events=\"/_rdy/events?ui=1&amp;rev=ab12\""));
    assert!(!pages[1].contains("live.js") && !pages[5].contains("live.js"));
    assert!(pages[0].contains(NOSCRIPT) && pages[2].contains(NOSCRIPT));
    // with scripts disabled every choice is a button of a form that submits bound values
    let fs = forms(&pages[2]);
    assert_eq!(fs.len(), 1);
    for o in &approve.options {
        let fields = posted(&fs[0].1, &o.id, "");
        assert_eq!(
            fields.last().unwrap(),
            &("option".to_string(), o.id.clone())
        );
    }
    // details open without a script: a native control
    assert!(
        pages[4]
            .contains("<details class=\"refusal-detail\"><summary aria-label=\"Error details\">")
    );
    // a hidden field needs no script: the brief rides in the question's form
    assert!(pages[3].contains(&format!("name=\"brief\" value=\"{}\"", brief.base.token)));
    // the page keeps the percent-encoded segment of a ref in its link
    assert_eq!(
        href("q-ABCDE@abcdef012345"),
        "/~ui/item/q-ABCDE%40abcdef012345"
    );
}
