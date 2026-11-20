//! cra4 (D-AJ7T2): a store-less publication of one state with the marks of a review page against
//! a second state. Each page keeps its empty notice slot; the hub writes its notes there.
use rdy_engine::publication::{IMPLICIT_ORG, ReadPublicationSource};
use rdy_engine::publication_manifest::NOTICE_SLOT;
use rdy_engine::reader::review::between_states;
use rdy_engine::render_inputs::RenderInputs;
use rdy_engine::site_scope::ScopeRef;
use rdy_engine::site_target::SiteTarget;
use rdy_engine::*;
use rdy_model::*;
use rdy_render::live::ProfileHistoryMemo;
use rdy_render::live_review::prepare_profile_review;
use rdy_render::render_input::capture_inputs;
use rdy_render::{PublicationArtifact, build_publication, build_review_publication};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const DRAFT: &str = "draft";

// --- fixtures -----------------------------------------------------------------------------------

fn writer() -> Session {
    Session::human("human:founder".into(), Role::Writer).unwrap()
}

fn founder() -> Session {
    Session::human("human:founder".into(), Role::Approver).unwrap()
}

/// F2 (D-DKJS5): the person confirms each exception beside version `p` before it lands.
fn confirm_exceptions(r: &mut Rdy, s: &Session, p: &ProposalInfo) {
    use rdy_engine::answer::{AnswerRequest, relay::RelayInput};
    use rdy_engine::pending::{CARD_MAX_TOKENS, ItemKind, PendingQuery};
    let q = r
        .read()
        .pending(&PendingQuery {
            person: s.actor.clone(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .expect("pending");
    let refs: Vec<String> = q
        .items
        .iter()
        .filter(|i| i.kind == ItemKind::Confirm && i.batch.as_deref() == Some(p.r#ref.as_str()))
        .map(|i| i.r#ref.clone())
        .collect();
    for item in refs {
        let req = AnswerRequest {
            r#ref: item.clone(),
            option: "confirm".into(),
            card_hash: None,
            note: None,
            relay: RelayInput::default(),
            brief: None,
        };
        r.answer(s, &s.actor, req)
            .unwrap_or_else(|e| panic!("confirm {item}: {e}"));
    }
}

fn put(r: &mut Rdy, view: &str, ops: Value) -> BTreeMap<String, String> {
    let w = r
        .write(
            &writer(),
            WriteRequest {
                view: view.into(),
                ops: serde_json::from_value(ops).unwrap(),
                justification: None,
                message: "cra4".into(),
            },
        )
        .unwrap();
    w.created.into_iter().collect()
}

fn land(r: &mut Rdy) {
    let p = r
        .propose(&writer(), "work", "batch", Some("test summary"))
        .unwrap();
    confirm_exceptions(r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap();
}

/// Canon at L1: folder `guide` with doc `intro` (a section with three prose blocks) and doc
/// `other`. The view `draft` adds one prose block, edits one and deletes one, all in `intro`.
struct Project {
    _dir: tempfile::TempDir,
    r: Rdy,
    ids: BTreeMap<String, String>,
}

fn project() -> Project {
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    std::fs::write(
        dir.path().join(".rdy.yaml"),
        "pack: rdy-standard\nname: \"Review project\"\n",
    )
    .unwrap();
    let mut r = Rdy::open(dir.path()).unwrap();
    let mut ids = put(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "folder", "id": "$guide", "fields": {"name": "guide", "title": "Guide"}},
            {"op": "create", "type": "doc", "id": "$intro", "parent": "$guide", "fields": {"name": "intro", "title": "Intro page", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$intro", "fields": {"title": "Start here"}},
            {"op": "create", "type": "prose", "id": "$keep", "parent": "$s", "body": "KEEPCANARY words stay."},
            {"op": "create", "type": "prose", "id": "$edit", "parent": "$s", "body": "EDITCANARY old words."},
            {"op": "create", "type": "prose", "id": "$gone", "parent": "$s", "body": "GONECANARY words go."},
            {"op": "create", "type": "doc", "id": "$other", "fields": {"name": "other", "title": "Other page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$b", "parent": "$other", "body": "Other words."}
        ]),
    );
    land(&mut r);
    let added = put(
        &mut r,
        DRAFT,
        json!([
            {"op": "create", "type": "prose", "id": "$new", "parent": ids["$s"], "body": "NEWCANARY words arrive."},
            {"op": "update", "id": ids["$edit"], "body": "EDITCANARY new words."},
            {"op": "delete", "id": ids["$gone"]}
        ]),
    );
    ids.extend(added);
    Project { _dir: dir, r, ids }
}

impl Project {
    fn id(&self, ph: &str) -> BlockId {
        BlockId::parse(&self.ids[ph]).unwrap()
    }

    fn inputs(&self) -> RenderInputs {
        capture_inputs(&self.r.read(), &Scope::Canon).unwrap()
    }

    fn state(&self, scope: &Scope) -> Materialized {
        self.r.read().materialized(scope).unwrap().as_ref().clone()
    }

    /// The review publication of `to` against `from`, both read from this project's store.
    fn review(&self, from: &Scope, to: &Scope) -> PublicationArtifact {
        let read = self.r.read();
        let inputs = self.inputs();
        let source = ReadPublicationSource::with_settings(
            &read,
            &inputs.pack,
            &inputs.title,
            inputs.description.as_deref(),
        );
        let (from_m, to_m) = (self.state(from), self.state(to));
        build_review_publication(
            &source,
            &to_m,
            &source,
            &from_m,
            false,
            IMPLICIT_ORG,
            &inputs,
        )
        .unwrap()
    }

    /// The plain publication of `scope` with no chosen tag.
    fn plain(&self, scope: &Scope) -> PublicationArtifact {
        let read = self.r.read();
        let inputs = self.inputs();
        let source = ReadPublicationSource::with_settings(
            &read,
            &inputs.pack,
            &inputs.title,
            inputs.description.as_deref(),
        );
        let m = self.state(scope);
        build_publication(&source, &m, "", false, IMPLICIT_ORG, &inputs).unwrap()
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// The HTML pages of an artifact, by path.
fn pages(a: &PublicationArtifact) -> BTreeMap<String, String> {
    a.manifest
        .files
        .iter()
        .filter(|e| e.media_type == "text/html")
        .map(|e| (e.path.clone(), text(&a.files[&e.path])))
        .collect()
}

/// Each (block id, mark word) a page writes: every element with a change mark that names its
/// block (`has-chg chg-<mark>` and `data-rdy-block` in one opening tag).
fn marks(html: &str) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    let mut rest = html;
    while let Some(at) = rest.find('<') {
        rest = &rest[at + 1..];
        let end = rest.find('>').unwrap_or(rest.len());
        let tag = &rest[..end];
        let word = if tag.contains(" has-chg chg-new") {
            Some("New")
        } else if tag.contains(" has-chg chg-edited") {
            Some("Edited")
        } else {
            None
        };
        if let Some(word) = word
            && let Some(i) = tag.find("data-rdy-block=\"")
        {
            let id = &tag[i + 16..];
            let id = &id[..id.find('"').unwrap()];
            out.insert((id.to_string(), word.to_string()));
        }
    }
    out
}

// --- tests --------------------------------------------------------------------------------------

#[test]
fn cra4_marks_equal_the_engine_review() {
    let p = project();
    let (from, to) = (Scope::Canon, Scope::View(DRAFT.into()));
    let (from_m, to_m) = (p.state(&from), p.state(&to));
    let engine = between_states(&from_m, &to_m);
    let a = p.review(&from, &to);
    let mut written: BTreeSet<(String, String)> = BTreeSet::new();
    for (path, html) in pages(&a) {
        for (id, word) in marks(&html) {
            let block = BlockId::parse(&id).unwrap();
            let want = engine.mark(&block).map(|m| m.word().to_string());
            assert_eq!(want.as_deref(), Some(word.as_str()), "{path}: {id}");
            written.insert((id, word));
        }
    }
    let expected: BTreeSet<(String, String)> = to_m
        .iter()
        .filter_map(|b| {
            engine
                .mark(&b.id)
                .map(|m| (b.id.to_string(), m.word().into()))
        })
        .collect();
    assert_eq!(written, expected);
    assert!(expected.contains(&(p.id("$new").to_string(), "New".into())));
    assert!(expected.contains(&(p.id("$edit").to_string(), "Edited".into())));
    // an unchanged block carries no mark
    for ph in ["$keep", "$b"] {
        assert!(engine.mark(&p.id(ph)).is_none(), "{ph}");
        assert!(!written.iter().any(|(id, _)| *id == p.ids[ph]), "{ph}");
    }
    // the deleted block shows as a removed line on its doc's page, and nowhere as a block
    let intro = text(&a.files["guide/intro.html"]);
    let removed = intro
        .find("<details class=\"chg chg-removed\"")
        .expect("a removed line");
    assert!(intro[removed..].contains("GONECANARY words go."));
    assert!(!intro.contains(&format!("data-rdy-block=\"{}\"", p.ids["$gone"])));
    // the edited block carries its word diff for Show diff, and the page offers both buttons
    assert!(intro.contains("data-diff hidden"));
    assert!(intro.contains("id=\"show-changes\""));
    assert!(intro.contains("id=\"show-diff\""));
}

#[test]
fn cra4_marks_equal_the_live_review_route() {
    let p = project();
    let a = p.review(&Scope::Canon, &Scope::View(DRAFT.into()));
    let st = p.r.read();
    let target = SiteTarget::Profile {
        scope: ScopeRef::View(DRAFT.into()),
        key: IMPLICIT_ORG.into(),
    };
    let live = prepare_profile_review(&st, &target, &mut ProfileHistoryMemo::default()).unwrap();
    let mut marked = 0;
    for (path, html) in pages(&a) {
        let Some(file) = live.render(&st, &path).unwrap() else {
            continue;
        };
        let ours = marks(&html);
        marked += ours.len();
        assert_eq!(ours, marks(&text(&file.bytes)), "{path}");
        // the same removed lines
        assert_eq!(
            html.matches("<details class=\"chg chg-removed\"").count(),
            text(&file.bytes)
                .matches("<details class=\"chg chg-removed\"")
                .count(),
            "{path}"
        );
    }
    assert!(marked >= 2, "the pair has marked blocks");
}

#[test]
fn cra4_every_page_keeps_an_empty_notice_slot() {
    let p = project();
    let a = p.review(&Scope::Canon, &Scope::View(DRAFT.into()));
    let mut html = 0;
    for e in &a.manifest.files {
        if e.media_type != "text/html" {
            assert_eq!(e.notice_offset, None, "{}", e.path);
            continue;
        }
        html += 1;
        let o = e.notice_offset.unwrap_or_else(|| panic!("{}", e.path)) as usize;
        let bytes = &a.files[&e.path];
        assert!(
            bytes[o..].starts_with(NOTICE_SLOT),
            "{}: the slot at {o} is empty",
            e.path
        );
        let slot = text(NOTICE_SLOT);
        assert_eq!(text(bytes).matches(slot.as_str()).count(), 1, "{}", e.path);
    }
    assert!(html > 0);
}

#[test]
fn cra4_same_state_has_no_marks() {
    let p = project();
    let draft = Scope::View(DRAFT.into());
    let a = p.review(&draft, &draft);
    for (path, html) in pages(&a) {
        assert!(marks(&html).is_empty(), "{path}");
        assert!(!html.contains("chg-removed"), "{path}");
    }
    let plain = p.plain(&draft);
    assert_eq!(a.manifest, plain.manifest);
    assert_eq!(a.files, plain.files);
    assert_eq!(a.manifest_hash, plain.manifest_hash);
}
