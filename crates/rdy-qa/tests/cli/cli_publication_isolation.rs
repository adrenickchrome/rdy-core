//! pub10 (D-48NYM, D-VP42Y, D-0VCQ5, D-5CVWB, D-WDKWQ): a profile selects what a local output
//! holds and restricts nothing else. One project holds a canary in every field of every audited
//! record kind of an excluded doc and its history; the CLI and the chat tools write every local
//! output of an org profile and a public one, and no file name and no byte of any file, decoded
//! where an output encodes, holds an excluded canary, while the positive canaries of the selected
//! content stay. The same source session still reads the excluded docs and tags, dumps the full
//! verified history and clones it (a profile is not an access rule).
use rdy_engine::alt::AltMemberInput;
use rdy_engine::answer::AnswerRequest;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::pending::{CARD_MAX_TOKENS, ItemKind, PendingQuery};
use rdy_engine::tensions::{TensionKind, tension_label};
use rdy_engine::{DecisionInput, Local, Rdy, ReadState, Scope, Session};
use rdy_model::{BlockId, Hash, Role};
use rdy_qa::daemon::{Served, short_tempdir};
use rdy_qa::every_kind::{facts, verdict_with_reason};
use rdy_qa::fixtures::{approver, land, write, writer};
use rdy_qa::inbox::on;
use rdy_qa::intake::PERSON;
use rdy_qa::mcp::McpClient;
use rdy_qa::tensions::{ag, one_involving, put, upd};
use rdy_render::print::fonts::Fonts;
use rdy_render::print::layout::LItem;
use rdy_render::print::{self, Print};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

use super::cli_publication_output::{chat, files_of};

// --- bytes --------------------------------------------------------------------------------------

/// An 8-bit RGB PNG of `w` x `h`, its one IDAT stored (no compression).
fn png(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
    fn crc32(data: &[u8]) -> u32 {
        let mut c = 0xffff_ffffu32;
        for b in data {
            c ^= u32::from(*b);
            for _ in 0..8 {
                c = if c & 1 == 1 {
                    0xedb8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
        }
        !c
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    }
    let line: Vec<u8> = std::iter::once(0)
        .chain(rgb.iter().copied().cycle().take(3 * w as usize))
        .collect();
    let raw = line.repeat(h as usize);
    let (mut a, mut b) = (1u32, 0u32);
    for x in &raw {
        a = (a + u32::from(*x)) % 65521;
        b = (b + a) % 65521;
    }
    let mut z = vec![0x78, 0x01, 1];
    let len = raw.len() as u16;
    z.extend_from_slice(&len.to_le_bytes());
    z.extend_from_slice(&(!len).to_le_bytes());
    z.extend_from_slice(&raw);
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = vec![];
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// The bytes a standard base64 text (with padding) holds; none when it is not base64.
fn unbase64(text: &str) -> Option<Vec<u8>> {
    let mut out = vec![];
    let mut acc = 0u32;
    let mut bits = 0;
    for ch in text.bytes() {
        let v = match ch {
            b'A'..=b'Z' => ch - b'A',
            b'a'..=b'z' => ch - b'a' + 26,
            b'0'..=b'9' => ch - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// Every base64 payload a text embeds as a `data:...;base64,` URI, decoded.
fn data_uris(text: &str) -> Vec<Vec<u8>> {
    let mut out = vec![];
    for (at, _) in text.match_indices(";base64,") {
        let rest = &text[at + ";base64,".len()..];
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '='))
            .unwrap_or(rest.len());
        if let Some(bytes) = unbase64(&rest[..end]) {
            out.push(bytes);
        }
    }
    out
}

fn has(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

// --- the fixture --------------------------------------------------------------------------------

/// What the source project holds, by role.
struct Iso {
    /// The hidden doc and a hidden claim (ids a read can name).
    hidden: String,
    /// The selected prose.
    sel_prose: String,
    /// The ids of the profile blocks and their doc.
    profiles: Vec<String>,
    neg_png: Vec<u8>,
    pos_png: Vec<u8>,
    neg_blob: String,
}

fn bid(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn approve_item(r: &Rdy, r#ref: &str) -> rdy_engine::pending::Item {
    let q = r
        .read()
        .pending(&PendingQuery {
            person: PERSON.into(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .unwrap();
    q.items
        .iter()
        .find(|i| i.kind == ItemKind::Approve && i.r#ref == r#ref)
        .unwrap_or_else(|| panic!("the approve item {ref}: {q:#?}"))
        .clone()
}

/// The person's `answer` item of the open question `qid` in their queue.
fn question_item(r: &Rdy, qid: &str) -> rdy_engine::pending::Item {
    let q = r
        .read()
        .pending(&PendingQuery {
            person: PERSON.into(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .unwrap();
    q.items
        .iter()
        .find(|i| i.kind == ItemKind::Answer && i.r#ref.starts_with(&format!("{qid}@")))
        .unwrap_or_else(|| panic!("the answer item of {qid}: {q:#?}"))
        .clone()
}

/// Builds the project in `dir` (closed on return). The excluded canaries all start with `NEG`;
/// the selected content's with `POS`. The org profile `orgp` selects the folder `pubdocs`, whose
/// prose refers to an excluded claim; the public profile `openp` selects the clean folder
/// `cleandocs`; the public profile `blockedp` selects `pubdocs` too, so its check is blocked.
fn iso_seed(dir: &Path) -> Iso {
    // the project's name and description are excluded words too
    std::fs::write(
        dir.join(".rdy.yaml"),
        "pack: rdy-standard\nname: \"NEGPROJNAME\"\ndescription: \"NEGPROJDESC project text\"\nserve:\n  idle_timeout: 60\n",
    )
    .unwrap();
    let mut r = Rdy::open(dir).unwrap();
    let (pos_png, neg_png) = (png(5, 3, [10, 120, 200]), png(4, 7, [201, 33, 44]));
    let pos_blob = r.store_mut().put_blob(&pos_png).unwrap();
    let neg_blob = r.store_mut().put_blob(&neg_png).unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "folder", "id": "$pubf", "fields": {"name": "pubdocs", "title": "POSFOLDER docs", "description": "POSFOLDERDESC words."}},
            {"op": "create", "type": "doc", "id": "$sel", "parent": "$pubf", "fields": {"name": "posdocname", "title": "POSTITLE selected doc", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$ss", "parent": "$sel", "fields": {"title": "POSSECTION heading"}},
            {"op": "create", "type": "prose", "id": "$sp", "parent": "$ss", "body": "OLDSELWORDS first revision."},
            {"op": "create", "type": "term", "parent": "$ss", "fields": {"name": "POSTERM", "key": "pos-term-key"}, "body": "POSTERMBODY words."},
            {"op": "create", "type": "question", "parent": "$ss", "fields": {"prompt": "POSQUESTION prompt?"}},
            {"op": "create", "type": "figure", "parent": "$ss", "fields": {"blob": pos_blob.to_string(), "media_type": "image/png", "alt": "POSFIGCAP caption"}},
            {"op": "create", "type": "folder", "id": "$clf", "fields": {"name": "cleandocs", "title": "POSCLEANFOLDER docs"}},
            {"op": "create", "type": "doc", "id": "$cld", "parent": "$clf", "fields": {"name": "cleandoc", "title": "POSCLEANTITLE doc", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$cld", "body": "POSCLEANBODY words."},
            {"op": "create", "type": "folder", "id": "$negf", "fields": {"name": "negfoldername", "title": "NEGFOLDER hidden", "description": "NEGFOLDERDESC words."}},
            {"op": "create", "type": "doc", "id": "$hid", "parent": "$negf", "fields": {"name": "neghidname", "key": "NEGDOCKEY", "title": "NEGTITLE hidden doc", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$hs", "parent": "$hid", "fields": {"title": "NEGSECTION heading"}},
            {"op": "create", "type": "prose", "parent": "$hs", "body": "NEGBODY words."},
            {"op": "create", "type": "term", "parent": "$hs", "fields": {"name": "NEGTERM", "key": "NEG-KEY"}, "body": "NEGTERMBODY words."},
            {"op": "create", "type": "question", "id": "$hq", "parent": "$hs", "fields": {"prompt": "NEGQUESTION prompt?", "options": ["NEGANSWERLABEL one", "NEGANSWERLABEL two"]}},
            {"op": "create", "type": "figure", "parent": "$hs", "fields": {"blob": neg_blob.to_string(), "media_type": "image/png", "alt": "NEGFIGCAP caption"}},
            {"op": "create", "type": "claim", "id": "$c1", "parent": "$hs", "body": "NEGCLAIM one."},
            {"op": "create", "type": "claim", "id": "$c2", "parent": "$hs", "after": "$c1", "body": "NEGCLAIM two."},
            {"op": "create", "type": "claim", "id": "$c3", "parent": "$hs", "after": "$c2", "body": "NEGCLAIM three."},
            {"op": "create", "type": "claim", "id": "$c4", "parent": "$hs", "after": "$c3", "body": "NEGCLAIM four."},
            {"op": "create", "type": "claim", "id": "$c5", "parent": "$hs", "after": "$c4", "body": "NEGCLAIM five."},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
        ]),
    )
    .unwrap();
    let id = |k: &str| w.created[k].clone();
    // the selected prose refers to an excluded claim (once without display words, once with); an
    // excluded decision governs it; a selected decision governs it too
    write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "prose", "parent": id("$ss"), "body": format!("See {{{{{}}}}} and {{{{{}|POSSHOWN display words}}}}.", id("$c1"), id("$c2"))}
        ]),
    )
    .unwrap();
    for (parent, statement) in [
        (id("$hs"), "NEGDECISION statement about the prose."),
        (id("$ss"), "POSDECISION statement about the prose."),
    ] {
        r.decide(
            &writer(),
            "work",
            DecisionInput {
                parent,
                statement: statement.into(),
                affects: vec![id("$sp")],
                ..Default::default()
            },
        )
        .unwrap();
    }
    land(&mut r, "work").unwrap();
    // the excluded doc's own key field holds its canary in canon
    {
        let st = r.read();
        let hid = st.block(&Scope::Canon, &bid(&id("$hid"))).unwrap();
        assert_eq!(
            hid.fields.get("key").and_then(|k| k.as_str()),
            Some("NEGDOCKEY"),
            "the excluded doc's key"
        );
    }
    let admin = id("$admin");
    let pw = write(
        &mut r,
        "adm",
        json!([
            {"op": "create", "type": "publication_profile", "id": "$orgp", "parent": admin,
             "fields": {"key": "orgp", "title": "POSORGTITLE profile", "description": "POSORGDESC words.", "audience": "org", "select": [id("$pubf")]}},
            {"op": "create", "type": "publication_profile", "id": "$openp", "parent": admin,
             "fields": {"key": "openp", "title": "POSOPENTITLE profile", "description": "POSOPENDESC words.", "audience": "public", "select": [id("$clf")]}},
            {"op": "create", "type": "publication_profile", "id": "$blockedp", "parent": admin,
             "fields": {"key": "blockedp", "title": "Blocked profile", "audience": "public", "select": [id("$pubf")]}}
        ]),
    )
    .unwrap();
    land(&mut r, "adm").unwrap();
    // a source tag between two edits of the selected prose
    r.tag(
        &approver(),
        "v1.0.0",
        None,
        Some("NEGTAGMSG release words".into()),
    )
    .unwrap();
    write(
        &mut r,
        "edit2",
        json!([{"op": "update", "id": id("$sp"), "body": "POSBODYWORDS second revision."}]),
    )
    .unwrap();
    land(&mut r, "edit2").unwrap();

    // the history: every record kind that carries text, all on excluded blocks
    let e = Local::new(r);
    let ids: Vec<BlockId> = ["$c1", "$c2", "$c3", "$c4", "$c5"]
        .iter()
        .map(|k| bid(&id(k)))
        .collect();
    let c = ids.clone();
    // a landed proposal: title and summary
    on(&e, move |r| {
        let s = ag("negpv");
        put(r, "negpv", upd(&c[0], "NEGCLAIM one, edited."));
        let p = r
            .propose(
                &s,
                "negpv",
                "NEGPROPTITLE landed",
                Some("NEGPROPSUM landed words"),
            )
            .unwrap();
        r.land(&approver(), &p.id).unwrap();
    });
    // an open proposal with a relayed answer: the note and the relayed words
    let c = ids.clone();
    on(&e, move |r| {
        put(r, "av", upd(&c[1], "NEGCLAIM two, edited."));
        let p = r
            .propose(&ag("av"), "av", "NEGOPENTITLE", Some("NEGOPENSUM words"))
            .unwrap();
        let item = approve_item(r, &p.r#ref);
        let relay = RelayInput {
            evidence: Some("NEGRELAYWORDS said later".into()),
            question: Some("Shall I record your answer?".into()),
            session_ref: Some("chat-9".into()),
            ..Default::default()
        };
        let relaying =
            Session::agent("agent:relay".into(), Some("chat".into()), Role::Writer).unwrap();
        let a = r
            .answer(
                &relaying,
                PERSON,
                AnswerRequest {
                    r#ref: item.r#ref.clone(),
                    option: "later".into(),
                    card_hash: Some(item.card_hash.clone()),
                    note: Some("NEGANSWERNOTE text".into()),
                    relay,
                    brief: None,
                },
            )
            .unwrap();
        assert_eq!(a.act, "later");
    });
    // the excluded question, answered with a card option: the answer's label is the authored
    // option label, and the decision it records carries the label as its statement
    let question = id("$hq");
    on(&e, move |r| {
        let item = question_item(r, &question);
        assert_eq!(item.options[0].label, "NEGANSWERLABEL one");
        let relay = RelayInput {
            evidence: Some("NEGRELAYWORDS chose the first".into()),
            question: Some("Which option do you choose?".into()),
            session_ref: Some("chat-9".into()),
            ..Default::default()
        };
        let relaying =
            Session::agent("agent:relay".into(), Some("chat".into()), Role::Writer).unwrap();
        let a = r
            .answer(
                &relaying,
                PERSON,
                AnswerRequest {
                    r#ref: item.r#ref.clone(),
                    option: "o1".into(),
                    card_hash: Some(item.card_hash.clone()),
                    note: None,
                    relay,
                    brief: None,
                },
            )
            .unwrap();
        assert_eq!(a.act, "answer_question");
        // the source record holds the canary in its own label field
        let (_, rec) = r.store().answer_of(&item.r#ref).unwrap().unwrap();
        assert_eq!(rec.label.as_deref(), Some("NEGANSWERLABEL one"));
    });
    // an edit collision, dismissed with a reason
    let c = ids.clone();
    on(&e, move |r| {
        put(r, "p", upd(&c[2], "NEGCLAIM three by p."));
        put(r, "q", upd(&c[2], "NEGCLAIM three by q."));
    });
    let c = ids.clone();
    on(&e, move |r| {
        let t = one_involving(r, TensionKind::EditCollision, &c[2]);
        let rev = Session::agent("agent:rev".into(), Some("rev".into()), Role::Reviewer).unwrap();
        r.dismiss_tension(&rev, &tension_label(&t.id), "NEGDISMISSREASON both stand")
            .unwrap();
    });
    // a semantic verdict: a finding note and advice
    let c = ids.clone();
    on(&e, move |r| {
        put(r, "sv", upd(&c[4], "NEGCLAIM five, edited."));
        let h = verdict_with_reason(
            r,
            &c[3],
            &c[4],
            Hash([9; 32]),
            "NEGFINDINGNOTE on the pair",
            "NEGADVICE text for the pair",
            "NEGREVIEWREASON for the verdict",
        );
        // the source record holds the canary in its own reason field
        let review = r.store().review(&h).unwrap();
        assert_eq!(review.reason, "NEGREVIEWREASON for the verdict");
    });
    // an intent: its angle
    let c = ids.clone();
    on(&e, move |r| {
        put(r, "it", upd(&c[0], "NEGCLAIM one, by it."));
        r.intent_post(
            &ag("it"),
            Some("it"),
            None,
            &[c[0].to_string()],
            "NEGINTENT angle words",
            None,
        )
        .unwrap();
    });
    // an alternative set: its title and member labels, answering the excluded question
    let c = ids.clone();
    let question = id("$hq");
    on(&e, move |r| {
        put(r, "x", upd(&c[3], "NEGCLAIM four, option x."));
        put(r, "y", upd(&c[3], "NEGCLAIM four, option y."));
        let m = |label: &str, v: &str| AltMemberInput {
            label: label.into(),
            view: v.into(),
        };
        r.alt_create(
            &ag("x"),
            "NEGALTTITLE which option",
            Some(question.as_str()),
            &[m("NEGALTLABEL one", "x"), m("NEGALTLABEL two", "y")],
        )
        .unwrap();
    });
    e.close().unwrap();
    Iso {
        hidden: id("$hid"),
        sel_prose: id("$sp"),
        profiles: ["$orgp", "$openp", "$blockedp"]
            .iter()
            .map(|k| pw.created[*k].clone())
            .chain([admin])
            .collect(),
        neg_png,
        pos_png,
        neg_blob: neg_blob.to_string(),
    }
}

/// Copies the project at `from` (closed) into a fresh directory, for a read the daemon's lock
/// does not hold.
fn copy_project(from: &Path) -> tempfile::TempDir {
    fn copy(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap() {
            let e = e.unwrap();
            let (src, dst) = (e.path(), to.join(e.file_name()));
            if e.file_name() == "run" {
                continue;
            }
            if src.is_dir() {
                copy(&src, &dst);
            } else {
                std::fs::copy(&src, &dst).unwrap();
            }
        }
    }
    let to = short_tempdir();
    copy(from, to.path());
    to
}

/// The canaries of a scope: (excluded, selected). The scope `canon` reads the second revision of
/// the selected prose, the tag the first.
fn canaries(iso: &Iso, scope: &str, profile: &str) -> (Vec<String>, Vec<String>) {
    let mut neg: Vec<String> = [
        "NEGFOLDER",
        "NEGFOLDERDESC",
        "NEGTITLE",
        "NEGSECTION",
        "NEGBODY",
        "NEGTERM",
        "NEG-KEY",
        "NEGQUESTION",
        "NEGFIGCAP",
        "NEGCLAIM",
        "NEGDECISION",
        "NEGTAGMSG",
        "NEGPROJNAME",
        "NEGPROJDESC",
        "NEGPROPTITLE",
        "NEGPROPSUM",
        "NEGOPENTITLE",
        "NEGOPENSUM",
        "NEGDISMISSREASON",
        "NEGFINDINGNOTE",
        "NEGADVICE",
        "NEGANSWERNOTE",
        "NEGANSWERLABEL",
        "NEGREVIEWREASON",
        "NEGDOCKEY",
        "NEGRELAYWORDS",
        "NEGINTENT",
        "NEGALTTITLE",
        "NEGALTLABEL",
        "negfoldername",
        "neghidname",
        "Admin page",
        "Blocked profile",
        "chat-9",
        "agent:relay",
        "agent:rev",
        "agent:negpv",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    neg.push(iso.neg_blob.clone());
    neg.push(iso.hidden.clone());
    neg.extend(iso.profiles.iter().cloned());
    let mut pos: Vec<String> = vec![];
    match profile {
        "orgp" => {
            pos.extend(
                [
                    "POSFOLDER",
                    "POSFOLDERDESC",
                    "POSTITLE",
                    "POSSECTION",
                    "POSTERM",
                    "POSTERMBODY",
                    "POSQUESTION",
                    "POSFIGCAP",
                    "POSORGTITLE",
                    "POSORGDESC",
                    "POSSHOWN display words",
                ]
                .map(String::from),
            );
            neg.extend(["POSCLEAN", "POSOPEN"].map(String::from));
        }
        _ => {
            pos.extend(
                [
                    "POSCLEANFOLDER",
                    "POSCLEANTITLE",
                    "POSCLEANBODY",
                    "POSOPENTITLE",
                    "POSOPENDESC",
                ]
                .map(String::from),
            );
            neg.extend(["POSFOLDER", "POSTITLE", "POSORG", "POSTERM"].map(String::from));
        }
    }
    if profile == "orgp" {
        if scope == "canon" {
            pos.push("POSBODYWORDS".into());
            neg.push("OLDSELWORDS".into());
        } else {
            pos.push("OLDSELWORDS".into());
            neg.push("POSBODYWORDS".into());
        }
    }
    (neg, pos)
}

// --- the expected print --------------------------------------------------------------------------

fn pdf_words(p: &Print) -> String {
    print::pdf::lay_out(p, &Fonts::load())
        .pages
        .iter()
        .flat_map(|page| {
            page.items.iter().filter_map(|it| match it {
                LItem::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// One output of a surface: what it wrote, by path.
type Files = BTreeMap<String, Vec<u8>>;

/// Every output of `profile` at `scope`: the site and each print, by each of the two paths.
fn write_all(
    s: &Served,
    c: &mut McpClient,
    scratch: &Path,
    profile: &str,
    scope: &str,
) -> Vec<(String, Files)> {
    let tag = format!("{profile}-{}", scope.replace(':', "-"));
    let mut out = vec![];
    // the static site
    let cli = scratch.join(format!("cli-site-{tag}"));
    let (ok, v) = s.json(&[
        "--json",
        "render",
        "--profile",
        profile,
        "--scope",
        scope,
        "--out",
        cli.to_str().unwrap(),
    ]);
    assert!(ok, "{tag}: {v}");
    let tool = scratch.join(format!("mcp-site-{tag}"));
    c.call(
        "rdy_render",
        json!({"out": tool.to_str().unwrap(), "profile": profile, "scope": scope}),
    )
    .ok();
    out.push((format!("cli site {tag}"), files_of(&cli)));
    out.push((format!("mcp site {tag}"), files_of(&tool)));
    // every print format and layout
    for (format, layout, name) in [
        ("pdf", None, "p.pdf"),
        ("html", None, "p.html"),
        ("md", None, "md"),
        ("md", Some("mkdocs"), "mk"),
    ] {
        let cli = scratch.join(format!("cli-{tag}-{name}"));
        let mut args = vec![
            "--json",
            "export",
            "--profile",
            profile,
            "--scope",
            scope,
            "--format",
            format,
            "--out",
            cli.to_str().unwrap(),
        ];
        let tool = scratch.join(format!("mcp-{tag}-{name}"));
        let mut chat_args = json!({"out": tool.to_str().unwrap(), "format": format, "profile": profile, "scope": scope});
        if let Some(l) = layout {
            args.extend(["--layout", l]);
            chat_args["layout"] = json!(l);
        }
        let (ok, v) = s.json(&args);
        assert!(ok, "{tag} {name}: {v}");
        c.call("rdy_export", chat_args).ok();
        out.push((format!("cli {name} {tag}"), files_of(&cli)));
        out.push((format!("mcp {name} {tag}"), files_of(&tool)));
    }
    out
}

#[test]
fn pub10_all_local_outputs_exclude_source_canaries() {
    let s = Served::new();
    let iso = iso_seed(s.dir.path());
    // the expected prints come from a copy of the project, read before the daemon starts
    let reference = copy_project(s.dir.path());
    let ref_r = Rdy::open(reference.path()).unwrap();
    let st: std::sync::Arc<ReadState> = ref_r.read();
    let mut c = chat(&s);
    let scratch = short_tempdir();

    // the canaries are real: the source's full print holds them
    let full = format!("{:?}", print::build(&st, &Scope::Canon).unwrap());
    for word in [
        "NEGTITLE",
        "NEGFOLDER",
        "NEGCLAIM",
        "POSTITLE",
        "POSBODYWORDS",
    ] {
        assert!(full.contains(word), "{word}");
    }

    for profile in ["orgp", "openp"] {
        for scope in ["canon", "tag:v1.0.0"] {
            let what = format!("{profile} at {scope}");
            let (neg, pos) = canaries(&iso, scope, profile);
            let outputs = write_all(&s, &mut c, scratch.path(), profile, scope);
            assert_eq!(outputs.len(), 10, "{what}");

            // the selected print model, and its PDF text
            let model = print::build_profile(&st, scope, profile).unwrap();
            let model_text = format!("{model:?}");
            let words = pdf_words(&model);
            for word in &neg {
                assert!(
                    !model_text.contains(word.as_str()),
                    "{what}: {word} in the model"
                );
                assert!(
                    !words.contains(word.as_str()),
                    "{what}: {word} in the PDF text"
                );
            }
            for word in &pos {
                assert!(
                    model_text.contains(word.as_str()),
                    "{what}: {word} not in the model"
                );
            }
            assert!(
                model.blobs.values().any(|b| b.bytes == iso.pos_png) == (profile == "orgp"),
                "{what}: the selected figure is in the model"
            );
            assert!(
                !model.blobs.values().any(|b| b.bytes == iso.neg_png),
                "{what}: the excluded figure is in the model"
            );
            let (pdf, ..) = print::pdf::pdf_bytes(&model, &Fonts::load());

            // every file name and byte, raw and decoded
            let mut seen_pos: Vec<bool> = vec![false; pos.len()];
            for (label, files) in &outputs {
                assert!(!files.is_empty(), "{what}: {label}");
                if label.contains(" p.pdf ") {
                    // the public surfaces wrote the bytes of the selected model
                    assert_eq!(files.len(), 1);
                    assert!(
                        files.values().next().unwrap() == &pdf,
                        "{what}: {label}: not the selected model's PDF"
                    );
                }
                for (path, bytes) in files {
                    let mut layers: Vec<Vec<u8>> = vec![bytes.clone()];
                    let text = String::from_utf8_lossy(bytes).into_owned();
                    layers.extend(data_uris(&text));
                    for word in &neg {
                        assert!(
                            !has(path.as_bytes(), word.as_bytes()),
                            "{what}: {label}: {word} in the name {path}"
                        );
                        for layer in &layers {
                            assert!(
                                !has(layer, word.as_bytes()),
                                "{what}: {label}: {word} in {path}"
                            );
                        }
                    }
                    assert!(
                        !layers.iter().any(|l| has(l, &iso.neg_png)),
                        "{what}: {label}: the excluded figure's bytes in {path}"
                    );
                    if path.ends_with("search-index.json") {
                        // the decoded index: entries hold the selected words alone
                        let index: Value = serde_json::from_slice(bytes).unwrap();
                        let flat = index.to_string();
                        for word in &neg {
                            assert!(!flat.contains(word.as_str()), "{what}: {word} in the index");
                        }
                        assert!(
                            pos.iter().any(|w| flat.contains(w.as_str())),
                            "{what}: the index holds the selected words"
                        );
                    }
                    for (i, word) in pos.iter().enumerate() {
                        if layers.iter().any(|l| has(l, word.as_bytes())) {
                            seen_pos[i] = true;
                        }
                    }
                }
            }
            // the positive canaries remain somewhere in the written outputs; the PDF holds its
            // words in compressed streams, which the model check above covers
            for (i, word) in pos.iter().enumerate() {
                assert!(seen_pos[i], "{what}: {word} is in no output");
            }
            // the selected figure's bytes reach the site and the single HTML file
            if profile == "orgp" {
                for (label, files) in &outputs {
                    if label.contains("site") || label.contains(" p.html ") {
                        let found = files.values().any(|b| {
                            has(b, &iso.pos_png)
                                || data_uris(&String::from_utf8_lossy(b))
                                    .iter()
                                    .any(|d| has(d, &iso.pos_png))
                        });
                        assert!(found, "{what}: {label}: the selected figure");
                    }
                }
            }
        }
    }

    // the org profile returns its warning for the excluded reference
    let probe = scratch.path().join("warn");
    let (ok, v) = s.json(&[
        "--json",
        "render",
        "--profile",
        "orgp",
        "--out",
        probe.to_str().unwrap(),
    ]);
    assert!(ok, "{v}");
    assert_eq!(v["warnings"].as_array().unwrap().len(), 1, "{v}");
    // the blocked public profile refuses and writes nothing
    let blocked = scratch.path().join("blocked");
    let (ok, v) = s.json(&[
        "--json",
        "render",
        "--profile",
        "blockedp",
        "--out",
        blocked.to_str().unwrap(),
    ]);
    assert!(!ok && v["code"] == "E_PUBLICATION_REFERENCE", "{v}");
    assert!(!blocked.exists());
}

#[test]
fn pub10_profiles_do_not_restrict_source_reads_dumps_or_clones() {
    let s = Served::new();
    let iso = iso_seed(s.dir.path());
    // the source's facts, read from a copy before the daemon starts
    let reference = copy_project(s.dir.path());
    let ref_r = Rdy::open(reference.path()).unwrap();
    let source_facts = facts(&ref_r);
    let versions: Vec<(String, String)> = {
        let st = ref_r.read();
        let m = st.materialized(&Scope::Canon).unwrap();
        iso.profiles
            .iter()
            .chain([&iso.hidden, &iso.sel_prose])
            .map(|id| {
                let b = bid(id);
                (id.clone(), m.version_of(&b).unwrap().to_string())
            })
            .collect()
    };
    let mut c = chat(&s);

    // profiles exist, and the source session reads the excluded doc and tag as it did
    let hidden = c
        .call("rdy_read", json!({"id": iso.hidden, "scope": "canon"}))
        .ok()
        .to_string();
    assert!(
        hidden.contains("NEGTITLE") && hidden.contains("NEGSECTION"),
        "{hidden}"
    );
    let old = c
        .call(
            "rdy_read",
            json!({"id": iso.sel_prose, "scope": "tag:v1.0.0"}),
        )
        .ok()
        .to_string();
    assert!(old.contains("OLDSELWORDS"), "{old}");
    let tags = c.call("rdy_tags", json!({})).ok().to_string();
    assert!(
        tags.contains("v1.0.0") && tags.contains("NEGTAGMSG"),
        "{tags}"
    );
    let (ok, cli_tags) = s.json(&["--json", "tags"]);
    assert!(ok, "{cli_tags}");
    assert!(cli_tags.to_string().contains("NEGTAGMSG"), "{cli_tags}");
    // profile outputs ran, and the reads are still whole
    let scratch = short_tempdir();
    let site = scratch.path().join("site");
    let (ok, v) = s.json(&[
        "--json",
        "render",
        "--profile",
        "orgp",
        "--out",
        site.to_str().unwrap(),
    ]);
    assert!(ok, "{v}");
    let again = c
        .call("rdy_read", json!({"id": iso.hidden, "scope": "canon"}))
        .ok()
        .to_string();
    assert_eq!(hidden, again);

    // the dump holds the full verified history and loads whole
    let dump = scratch.path().join("full.dump");
    let (ok, v) = s.json(&["--json", "dump", "--out", dump.to_str().unwrap()]);
    assert!(ok, "{v}");
    let dumped = std::fs::read(&dump).unwrap();
    for (word, what) in [
        ("NEGTAGMSG release words", "tag message"),
        ("NEGREVIEWREASON for the verdict", "review reason"),
        ("NEGANSWERLABEL one", "answer label"),
        ("NEGDOCKEY", "doc key"),
    ] {
        assert!(has(&dumped, word.as_bytes()), "the dump holds the {what}");
    }
    let loaded = short_tempdir();
    let out = s.rdy(&[
        "load",
        dump.to_str().unwrap(),
        loaded.path().to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("fsck ok"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let l = Rdy::open(loaded.path()).unwrap();
    let loaded_facts = facts(&l);
    // every ref the source held (the chat session's own inbox cursor is a ref of the later day)
    let mut now_refs = loaded_facts["refs"].as_object().unwrap().clone();
    now_refs.retain(|name, _| !name.starts_with("inbox/"));
    assert_eq!(Value::Object(now_refs), source_facts["refs"], "every ref");
    // every object of every kind; the one extra ref update (kind 8) is the chat session's cursor
    let mut now_objects = loaded_facts["objects"].as_object().unwrap().clone();
    let cursor =
        now_objects["8"].as_u64().unwrap() - source_facts["objects"]["8"].as_u64().unwrap();
    assert_eq!(cursor, 1, "only the inbox cursor moved");
    now_objects.insert("8".into(), source_facts["objects"]["8"].clone());
    assert_eq!(
        Value::Object(now_objects),
        source_facts["objects"],
        "every object"
    );
    assert_eq!(loaded_facts["landings"], source_facts["landings"]);
    // the fsck report, whole but for the frame the cursor wrote
    let (mut now_fsck, mut was_fsck) = (loaded_facts["fsck"].clone(), source_facts["fsck"].clone());
    assert_eq!(
        now_fsck["frames"].as_u64().unwrap(),
        was_fsck["frames"].as_u64().unwrap() + 1
    );
    now_fsck["frames"] = Value::Null;
    was_fsck["frames"] = Value::Null;
    assert_eq!(now_fsck, was_fsck);
    let kept = |r: &Rdy| -> Vec<(String, String)> {
        let st = r.read();
        let m = st.materialized(&Scope::Canon).unwrap();
        versions
            .iter()
            .map(|(id, _)| (id.clone(), m.version_of(&bid(id)).unwrap().to_string()))
            .collect()
    };
    assert_eq!(
        kept(&l),
        versions,
        "profile and excluded block hashes survive a load"
    );

    // the clone keeps the landings and the same block versions, profiles and excluded docs too
    let cloned = short_tempdir();
    let out = s.rdy(&[
        "clone",
        s.dir.path().to_str().unwrap(),
        cloned.path().to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let k = Rdy::open(cloned.path()).unwrap();
    assert_eq!(facts(&k)["landings"], source_facts["landings"]);
    assert_eq!(
        kept(&k),
        versions,
        "profile and excluded block hashes survive a clone"
    );
    let st = k.read();
    assert!(
        st.versions()
            .unwrap()
            .iter()
            .any(|v| v.name.contains("1.0.0")
                && v.message
                    .as_deref()
                    .is_some_and(|m| m.contains("NEGTAGMSG"))),
        "the clone holds the excluded tag"
    );
    drop(st);
    drop(k);
    drop(l);
    // the unfiltered reads remain on the original project after the whole run
    let last = c
        .call("rdy_read", json!({"id": iso.hidden, "scope": "canon"}))
        .ok()
        .to_string();
    assert_eq!(hidden, last);
}
