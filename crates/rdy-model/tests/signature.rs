//! Signatures (kind SIGNATURE) and the canonical statements they sign (D-17AA8, D-1Z951,
//! D-QVH4N, D-F0904). Every expected statement and byte vector here is a fixed literal: none is
//! derived with the code under test.
use rdy_model::signature::{
    ParsedStatement, Signature, StatementError, StatementKind, StatementSource, decode_signature,
    parse_statement, relay_evidence_hash, statement,
};
use rdy_model::*;

const REPO: RepoId = RepoId([0xab; 16]);
const KEY: &str = "SHA256:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3tD2s";

fn h(b: u8) -> Hash {
    Hash([b; 32])
}

fn enc<T: serde::Serialize>(v: &T) -> Vec<u8> {
    postcard::to_allocvec(v).unwrap()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn prov(actor: &str, run: Option<&str>) -> Provenance {
    Provenance {
        actor: actor.into(),
        run: run.map(Into::into),
        role: Role::Approver,
        at_ms: 1_760_000_000_000,
    }
}

fn relayer(actor: &str, run: Option<&str>) -> ActorRun {
    ActorRun {
        actor: actor.into(),
        run: run.map(Into::into),
    }
}

/// A relayed landing with every relay field set.
fn relay_landing() -> LandingRecord {
    LandingRecord {
        proposal: h(1),
        prev: Some(h(3)),
        state: h(7),
        approved_by: prov("human:nick", Some("run-1")),
        changes: vec![h(8)],
        chosen: None,
        checks: vec![],
        assurance: Assurance::Relayed,
        relayed_by: Some(relayer("agent:claude", Some("run-7"))),
        relay_evidence: Some(RelayEvidence {
            quoted: "yes, land it".into(),
            session_ref: "sess-1".into(),
            question: Some("Land P-1@2?".into()),
        }),
        approved_version: Some(h(2)),
        rebased_from: vec![],
        evidence: Some(ApprovalEvidence {
            kind: EvidenceKind::Card,
            options: vec!["Approve P-1@2".into(), "Wait".into()],
            choice: Some("Approve P-1@2".into()),
            card_hash: Some(h(9)),
        }),
    }
}

fn local_landing() -> LandingRecord {
    LandingRecord {
        assurance: Assurance::Local,
        relayed_by: None,
        relay_evidence: None,
        evidence: None,
        ..relay_landing()
    }
}

fn tag() -> Tag {
    Tag {
        name: "v1.0".into(),
        landing: h(4),
        message: Some("first".into()),
        tagged_by: prov("human:nick", None),
    }
}

fn answer() -> Answer {
    Answer {
        item: "tag:v1.0".into(),
        act: AnswerAct::Tag,
        option: None,
        label: None,
        options: vec![],
        card_hash: Some(h(9)),
        targets: vec![h(5)],
        person: prov("human:nick", None),
        relayed_by: Some(relayer("agent:claude", Some("run-7"))),
        relay_evidence: Some(RelayEvidence {
            quoted: "tag it".into(),
            session_ref: "sess-1".into(),
            question: None,
        }),
        evidence_kind: Some(EvidenceKind::Text),
        note: None,
    }
}

const EVIDENCE_HEX: &str = "9b37d84342a6ae69a57c6080cca1080c7f5574fc1947ac83aa7346b3a087afe4";

const APPROVAL_LOCAL: &str = "rdy-statement: 1\n\
kind: approval\n\
repo: abababababababababababababababab\n\
proposal_version: 0202020202020202020202020202020202020202020202020202020202020202\n\
base: 0303030303030303030303030303030303030303030303030303030303030303\n\
actor: human:nick\n\
assurance: local\n\
key: SHA256:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3tD2s\n";

const APPROVAL_RELAYED: &str = "rdy-statement: 1\n\
kind: approval\n\
repo: abababababababababababababababab\n\
proposal_version: 0202020202020202020202020202020202020202020202020202020202020202\n\
base: 0303030303030303030303030303030303030303030303030303030303030303\n\
actor: human:nick\n\
assurance: relayed\n\
key: SHA256:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3tD2s\n";

const RELAY_APPROVE: &str = "rdy-statement: 1\n\
kind: relay\n\
repo: abababababababababababababababab\n\
act: approve\n\
proposal_version: 0202020202020202020202020202020202020202020202020202020202020202\n\
base: 0303030303030303030303030303030303030303030303030303030303030303\n\
actor: human:nick\n\
relayed_by: agent:claude/run-7\n\
evidence: 9b37d84342a6ae69a57c6080cca1080c7f5574fc1947ac83aa7346b3a087afe4\n\
key: SHA256:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3tD2s\n";

const TAG_STATEMENT: &str = "rdy-statement: 1\n\
kind: tag\n\
repo: abababababababababababababababab\n\
tag: 0505050505050505050505050505050505050505050505050505050505050505\n\
name: v1.0\n\
landing: 0404040404040404040404040404040404040404040404040404040404040404\n\
actor: human:nick\n\
key: SHA256:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3tD2s\n";

const RELAY_TAG: &str = "rdy-statement: 1\n\
kind: relay\n\
repo: abababababababababababababababab\n\
act: tag\n\
tag: 0505050505050505050505050505050505050505050505050505050505050505\n\
name: v1.0\n\
landing: 0404040404040404040404040404040404040404040404040404040404040404\n\
actor: human:nick\n\
relayed_by: agent:claude/run-7\n\
answer: 0606060606060606060606060606060606060606060606060606060606060606\n\
key: SHA256:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3tD2s\n";

const KEYBIND: &str = "rdy-statement: 1\n\
kind: keybind\n\
account: nick\n\
challenge: c-123\n\
key: SHA256:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3tD2s\n";

type AnswerEdit = (&'static str, fn(&mut Answer));

fn text(r: Result<Vec<u8>, StatementError>) -> String {
    String::from_utf8(r.expect("a statement")).expect("UTF-8")
}

fn parse(s: &str) -> ParsedStatement<'_> {
    parse_statement(s.as_bytes()).unwrap_or_else(|e| panic!("{e:?}: {s:?}"))
}

#[test]
fn sig1_each_statement_has_one_exact_byte_vector() {
    let (relay, local, t, a) = (relay_landing(), local_landing(), tag(), answer());
    let cases: [(
        StatementSource<'_>,
        Option<RepoId>,
        &str,
        StatementKind,
        &str,
    ); 6] = [
        (
            StatementSource::Approval(&local),
            Some(REPO),
            APPROVAL_LOCAL,
            StatementKind::Approval,
            "rdy-approval-v1",
        ),
        (
            StatementSource::Approval(&relay),
            Some(REPO),
            APPROVAL_RELAYED,
            StatementKind::Approval,
            "rdy-approval-v1",
        ),
        (
            StatementSource::RelayApproval(&relay),
            Some(REPO),
            RELAY_APPROVE,
            StatementKind::Relay,
            "rdy-relay-v1",
        ),
        (
            StatementSource::Tag {
                hash: h(5),
                tag: &t,
            },
            Some(REPO),
            TAG_STATEMENT,
            StatementKind::Tag,
            "rdy-tag-v1",
        ),
        (
            StatementSource::RelayTag {
                hash: h(5),
                tag: &t,
                answer_hash: h(6),
                answer: &a,
            },
            Some(REPO),
            RELAY_TAG,
            StatementKind::Relay,
            "rdy-relay-v1",
        ),
        (
            StatementSource::Keybind {
                account: "nick",
                challenge: "c-123",
            },
            None,
            KEYBIND,
            StatementKind::Keybind,
            "rdy-keybind-v1",
        ),
    ];
    for (source, repo, want, kind, namespace) in cases {
        let got = statement(source, repo, KEY).unwrap_or_else(|e| panic!("{e:?}: {want}"));
        assert_eq!(String::from_utf8(got.clone()).unwrap(), want);
        assert_eq!(got, want.as_bytes(), "the exact bytes");
        assert_eq!(*got.last().unwrap(), b'\n', "the final LF");
        assert_eq!(kind.namespace(), namespace);
        let p = parse_statement(&got).expect("a built statement parses");
        assert_eq!(p.kind(), kind);
        assert_eq!(p.namespace(), namespace);
        assert_eq!(p.field("key"), Some(KEY));
        assert_eq!(p.text(), want);
        match kind {
            StatementKind::Keybind => assert_eq!(p.field("repo"), None),
            _ => assert_eq!(p.field("repo"), Some("abababababababababababababababab")),
        }
    }
    assert_eq!(
        [
            StatementKind::Approval,
            StatementKind::Relay,
            StatementKind::Tag,
            StatementKind::Keybind
        ]
        .map(StatementKind::namespace),
        [
            "rdy-approval-v1",
            "rdy-relay-v1",
            "rdy-tag-v1",
            "rdy-keybind-v1"
        ]
    );
    let r = parse(RELAY_APPROVE);
    assert_eq!(r.act(), Some("approve"));
    assert_eq!(r.field("relayed_by"), Some("agent:claude/run-7"));
    assert_eq!(r.field("evidence"), Some(EVIDENCE_HEX));
    assert_eq!(r.field("kind"), None, "fields are the ones after kind");
    let names: Vec<&str> = r.fields().map(|(n, _)| n).collect();
    assert_eq!(
        names,
        [
            "repo",
            "act",
            "proposal_version",
            "base",
            "actor",
            "relayed_by",
            "evidence",
            "key"
        ]
    );
    assert_eq!(parse(RELAY_TAG).act(), Some("tag"));
    assert_eq!(parse(RELAY_TAG).field("answer"), Some(&"06".repeat(32)[..]));
    assert_eq!(parse(TAG_STATEMENT).act(), None);
    assert_eq!(parse(APPROVAL_LOCAL).act(), None);
}

#[test]
fn sig1_evidence_hash_covers_the_two_stored_optional_fields() {
    let l = relay_landing();
    // The two fields are slices of the landing's own encoding, in field order: relay_evidence
    // follows relayed_by, and evidence is the last field.
    let full = enc(&l);
    let head = enc(&(
        &l.proposal,
        &l.prev,
        &l.state,
        &l.approved_by,
        &l.changes,
        &l.chosen,
        &l.checks,
        &l.assurance,
        &l.relayed_by,
    ));
    let relay_part = enc(&l.relay_evidence);
    let evidence_part = enc(&l.evidence);
    assert_eq!(
        &full[head.len()..head.len() + relay_part.len()],
        &relay_part[..]
    );
    assert_eq!(
        &full[full.len() - evidence_part.len()..],
        &evidence_part[..]
    );
    assert_eq!(
        relay_part[0], 1,
        "the relay_evidence discriminant is hashed"
    );
    assert_eq!(evidence_part[0], 1, "the evidence discriminant is hashed");
    let mut both = relay_part.clone();
    both.extend(&evidence_part);
    assert_eq!(relay_evidence_hash(&l), Ok(Hash::of(&both)));
    assert_eq!(relay_evidence_hash(&l).unwrap().to_string(), EVIDENCE_HEX);

    // A missing evidence value still contributes its discriminant.
    let none = LandingRecord {
        evidence: None,
        ..l.clone()
    };
    let mut with_none = relay_part.clone();
    with_none.push(0);
    assert_eq!(relay_evidence_hash(&none), Ok(Hash::of(&with_none)));
    assert_ne!(relay_evidence_hash(&none), Ok(Hash::of(&relay_part)));

    // Each of the seven components moves the hash.
    let base = relay_evidence_hash(&l).unwrap();
    let edits: [fn(&mut LandingRecord); 7] = [
        |l| l.relay_evidence.as_mut().unwrap().quoted = "yes, land it!".into(),
        |l| l.relay_evidence.as_mut().unwrap().session_ref = "sess-2".into(),
        |l| l.relay_evidence.as_mut().unwrap().question = None,
        |l| l.evidence.as_mut().unwrap().kind = EvidenceKind::Text,
        |l| l.evidence.as_mut().unwrap().options.push("Reject".into()),
        |l| l.evidence.as_mut().unwrap().choice = Some("Wait".into()),
        |l| l.evidence.as_mut().unwrap().card_hash = Some(h(0x0a)),
    ];
    let mut seen = vec![base];
    for (i, edit) in edits.iter().enumerate() {
        let mut m = l.clone();
        edit(&mut m);
        let got = relay_evidence_hash(&m).unwrap();
        assert!(!seen.contains(&got), "component {i} changes the hash");
        seen.push(got);
    }
    // Other landing fields do not enter the evidence hash.
    let other = LandingRecord {
        state: h(0x0b),
        ..l.clone()
    };
    assert_eq!(relay_evidence_hash(&other), Ok(base));
    assert_eq!(
        relay_evidence_hash(&local_landing()),
        Err(StatementError::Source),
        "no relay evidence, no relay"
    );
}

#[test]
fn sig1_parser_refuses_noncanonical_text() {
    for ok in [
        APPROVAL_LOCAL,
        APPROVAL_RELAYED,
        RELAY_APPROVE,
        TAG_STATEMENT,
        RELAY_TAG,
        KEYBIND,
    ] {
        parse(ok);
    }
    parse(&APPROVAL_LOCAL.replace(&format!("base: {}", "03".repeat(32)), "base: none"));
    let actor_none = APPROVAL_LOCAL.replace("actor: human:nick", "actor: none");
    assert_eq!(parse(&actor_none).field("actor"), Some("none"));

    let pv = format!("proposal_version: {}\n", "02".repeat(32));
    let base = format!("base: {}\n", "03".repeat(32));
    let bad_key = |k: &str| APPROVAL_LOCAL.replace(KEY, k);
    let bad: Vec<(&str, String)> = vec![
        ("CRLF", APPROVAL_LOCAL.replace('\n', "\r\n")),
        (
            "CR inside",
            APPROVAL_LOCAL.replace("actor: human:nick", "actor: human:\rnick"),
        ),
        (
            "absent final LF",
            APPROVAL_LOCAL[..APPROVAL_LOCAL.len() - 1].to_string(),
        ),
        (
            "blank line",
            APPROVAL_LOCAL.replace("kind: approval\n", "kind: approval\n\n"),
        ),
        ("blank last line", format!("{APPROVAL_LOCAL}\n")),
        (
            "comment line",
            APPROVAL_LOCAL.replace("kind: approval\n", "kind: approval\n# note\n"),
        ),
        (
            "comment with colon",
            APPROVAL_LOCAL.replace("kind: approval\n", "kind: approval\n# note: x\n"),
        ),
        (
            "uppercase hash",
            APPROVAL_LOCAL.replace(&"02".repeat(32), &"0A".repeat(32)),
        ),
        (
            "uppercase repo",
            APPROVAL_LOCAL.replace(&"ab".repeat(16), &"AB".repeat(16)),
        ),
        (
            "short hash",
            APPROVAL_LOCAL.replace(&"02".repeat(32), &"02".repeat(31)),
        ),
        (
            "duplicate field",
            APPROVAL_LOCAL.replace(
                "actor: human:nick\n",
                "actor: human:nick\nactor: human:nick\n",
            ),
        ),
        (
            "extra field",
            APPROVAL_LOCAL.replace("assurance: local\n", "assurance: local\nextra: x\n"),
        ),
        (
            "extra trailing field",
            format!("{APPROVAL_LOCAL}extra: x\n"),
        ),
        (
            "missing field",
            APPROVAL_LOCAL.replace("assurance: local\n", ""),
        ),
        (
            "reordered field",
            APPROVAL_LOCAL.replace(&format!("{pv}{base}"), &format!("{base}{pv}")),
        ),
        (
            "uppercase name",
            APPROVAL_LOCAL.replace("actor: ", "Actor: "),
        ),
        (
            "unknown version",
            APPROVAL_LOCAL.replace("rdy-statement: 1\n", "rdy-statement: 2\n"),
        ),
        (
            "no header",
            APPROVAL_LOCAL.replace("rdy-statement: 1\n", ""),
        ),
        (
            "unknown kind",
            APPROVAL_LOCAL.replace("kind: approval", "kind: approve"),
        ),
        (
            "kind of another layout",
            APPROVAL_LOCAL.replace("kind: approval", "kind: tag"),
        ),
        (
            "unknown relay act",
            RELAY_APPROVE.replace("act: approve", "act: dismiss"),
        ),
        (
            "relay act of the other layout",
            RELAY_APPROVE.replace("act: approve", "act: tag"),
        ),
        (
            "none as a proposal version",
            APPROVAL_LOCAL.replace(&"02".repeat(32), "none"),
        ),
        (
            "unknown assurance",
            APPROVAL_LOCAL.replace("assurance: local", "assurance: policy"),
        ),
        ("two spaces", APPROVAL_LOCAL.replace("actor: ", "actor:  ")),
        ("no space", APPROVAL_LOCAL.replace("actor: ", "actor:")),
        (
            "trailing space",
            APPROVAL_LOCAL.replace("actor: human:nick", "actor: human:nick "),
        ),
        ("empty value", KEYBIND.replace("account: nick", "account: ")),
        ("padded fingerprint", bad_key(&format!("{KEY}="))),
        (
            "padded fingerprint 44",
            bad_key("SHA256:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3tD2s="),
        ),
        (
            "noncanonical trailing bits",
            bad_key("SHA256:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3tD2t"),
        ),
        (
            "short fingerprint",
            bad_key("SHA256:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3tD2"),
        ),
        (
            "lowercase algorithm",
            bad_key("sha256:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3tD2s"),
        ),
        (
            "url-safe alphabet",
            bad_key("SHA256:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3t-2s"),
        ),
        ("raw key", bad_key("ssh-ed25519 AAAA")),
        (
            "DEL",
            APPROVAL_LOCAL.replace("actor: human:nick", "actor: human:\u{7f}nick"),
        ),
        (
            "C1 control",
            APPROVAL_LOCAL.replace("actor: human:nick", "actor: human:\u{85}nick"),
        ),
        (
            "tab",
            APPROVAL_LOCAL.replace("actor: human:nick", "actor: human:\tnick"),
        ),
        (
            "NUL",
            APPROVAL_LOCAL.replace("actor: human:nick", "actor: human:\0nick"),
        ),
    ];
    for (what, s) in bad {
        assert_eq!(
            parse_statement(s.as_bytes()).err(),
            Some(StatementError::Syntax),
            "{what}: {s:?}"
        );
    }
    let mut not_utf8 = APPROVAL_LOCAL.as_bytes().to_vec();
    not_utf8[60] = 0xff;
    assert_eq!(
        parse_statement(&not_utf8).err(),
        Some(StatementError::Syntax)
    );
    assert_eq!(parse_statement(b"").err(), Some(StatementError::Syntax));
}

#[test]
fn sig1_sources_are_the_stored_approval_and_tag_relay() {
    // approved_version falls back to the proposal; the first landing's base is `none`.
    let first = LandingRecord {
        approved_version: None,
        prev: None,
        ..local_landing()
    };
    assert_eq!(
        text(statement(
            StatementSource::Approval(&first),
            Some(REPO),
            KEY
        )),
        "rdy-statement: 1\n\
kind: approval\n\
repo: abababababababababababababababab\n\
proposal_version: 0101010101010101010101010101010101010101010101010101010101010101\n\
base: none\n\
actor: human:nick\n\
assurance: local\n\
key: SHA256:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3tD2s\n"
    );

    // A relayer whose actor holds `/` is joined, never split; an absent run adds nothing.
    let slash = LandingRecord {
        relayed_by: Some(relayer("agent:x/y", None)),
        approved_by: prov("human:a/b", None),
        ..relay_landing()
    };
    assert_eq!(
        text(statement(
            StatementSource::RelayApproval(&slash),
            Some(REPO),
            KEY
        )),
        "rdy-statement: 1\n\
kind: relay\n\
repo: abababababababababababababababab\n\
act: approve\n\
proposal_version: 0202020202020202020202020202020202020202020202020202020202020202\n\
base: 0303030303030303030303030303030303030303030303030303030303030303\n\
actor: human:a/b\n\
relayed_by: agent:x/y\n\
evidence: 9b37d84342a6ae69a57c6080cca1080c7f5574fc1947ac83aa7346b3a087afe4\n\
key: SHA256:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3tD2s\n"
    );
    let slash_run = LandingRecord {
        relayed_by: Some(relayer("agent:x/y", Some("run-7"))),
        ..relay_landing()
    };
    assert!(
        text(statement(
            StatementSource::RelayApproval(&slash_run),
            Some(REPO),
            KEY
        ))
        .contains("\nrelayed_by: agent:x/y/run-7\n")
    );
    let no_run = LandingRecord {
        relayed_by: Some(relayer("agent:claude", None)),
        ..relay_landing()
    };
    assert!(
        text(statement(
            StatementSource::RelayApproval(&no_run),
            Some(REPO),
            KEY
        ))
        .contains("\nrelayed_by: agent:claude\nevidence: ")
    );
    // A stored actor is never trimmed or normalized; the text `none` is an actor like any other.
    let spaced = LandingRecord {
        approved_by: prov(" human:nick", None),
        ..local_landing()
    };
    assert_eq!(
        statement(StatementSource::Approval(&spaced), Some(REPO), KEY),
        Err(StatementError::Syntax)
    );
    let named_none = LandingRecord {
        approved_by: prov("none", None),
        ..local_landing()
    };
    assert!(
        text(statement(
            StatementSource::Approval(&named_none),
            Some(REPO),
            KEY
        ))
        .contains("\nactor: none\nassurance: local\n")
    );

    // Policy landings, and relays without their stored relay facts, are not sources.
    let policy = LandingRecord {
        assurance: Assurance::Policy,
        ..local_landing()
    };
    let relay_policy = LandingRecord {
        assurance: Assurance::Policy,
        ..relay_landing()
    };
    let relay_local = LandingRecord {
        assurance: Assurance::Local,
        ..relay_landing()
    };
    let no_relayer = LandingRecord {
        relayed_by: None,
        ..relay_landing()
    };
    let no_evidence = LandingRecord {
        relay_evidence: None,
        ..relay_landing()
    };
    for (what, source) in [
        ("policy approval", StatementSource::Approval(&policy)),
        (
            "policy relay",
            StatementSource::RelayApproval(&relay_policy),
        ),
        ("local relay", StatementSource::RelayApproval(&relay_local)),
        ("no relayed_by", StatementSource::RelayApproval(&no_relayer)),
        (
            "no relay_evidence",
            StatementSource::RelayApproval(&no_evidence),
        ),
    ] {
        assert_eq!(
            statement(source, Some(REPO), KEY),
            Err(StatementError::Source),
            "{what}"
        );
    }

    // The tag relay reuses the stored ANSWER and refuses one that does not match the tag.
    let t = tag();
    let mismatches: [AnswerEdit; 5] = [
        ("act", |a| a.act = AnswerAct::Dismiss),
        ("item", |a| a.item = "tag:v2.0".into()),
        ("item without prefix", |a| a.item = "v1.0".into()),
        ("person", |a| a.person.actor = "human:other".into()),
        ("no relayer", |a| a.relayed_by = None),
    ];
    for (what, edit) in mismatches {
        let mut a = answer();
        edit(&mut a);
        let source = StatementSource::RelayTag {
            hash: h(5),
            tag: &t,
            answer_hash: h(6),
            answer: &a,
        };
        assert_eq!(
            statement(source, Some(REPO), KEY),
            Err(StatementError::Source),
            "tag answer {what}"
        );
    }
    let mut no_run_answer = answer();
    no_run_answer.relayed_by = Some(relayer("agent:claude", None));
    let source = StatementSource::RelayTag {
        hash: h(5),
        tag: &t,
        answer_hash: h(6),
        answer: &no_run_answer,
    };
    assert!(
        text(statement(source, Some(REPO), KEY)).contains("\nrelayed_by: agent:claude\nanswer: ")
    );

    // The repo is required except for a key binding, which never takes one.
    assert_eq!(
        statement(StatementSource::Approval(&local_landing()), None, KEY),
        Err(StatementError::Source)
    );
    assert_eq!(
        statement(
            StatementSource::Tag {
                hash: h(5),
                tag: &t
            },
            None,
            KEY
        ),
        Err(StatementError::Source)
    );
    assert_eq!(
        statement(
            StatementSource::Keybind {
                account: "nick",
                challenge: "c-123"
            },
            Some(REPO),
            KEY
        ),
        Err(StatementError::Source)
    );
    // The key is a canonical SHA256 fingerprint.
    for k in [
        "",
        "SHA256:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3tD2t",
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5",
    ] {
        assert_eq!(
            statement(StatementSource::Approval(&local_landing()), Some(REPO), k),
            Err(StatementError::Syntax),
            "{k:?}"
        );
    }
}

const PROV_HEX: &str = "0a68756d616e3a6e69636b010572756e2d31038080e682b966";
const PROV_HASH: &str = "a47f48bc02bd963239e6def239bb268a4e34b45fccd153094fdadeb39664ce56";
const LANDING_HEX: &str = "010101010101010101010101010101010101010101010101010101010101010101030303030303030303030303030303030303030303030303030303030303030307070707070707070707070707070707070707070707070707070707070707070a68756d616e3a6e69636b010572756e2d31038080e682b966010808080808080808080808080808080808080808080808080808080808080808000001010c6167656e743a636c61756465010572756e2d37010c7965732c206c616e6420697406736573732d31010b4c616e6420502d3140323f010202020202020202020202020202020202020202020202020202020202020202000101020d417070726f766520502d3140320457616974010d417070726f766520502d314032010909090909090909090909090909090909090909090909090909090909090909";
const LANDING_HASH: &str = "d9574c05f35d8339770a64414fccae70ad05edd2ca09b276be19fabed92f2bae";
const TAG_HEX: &str = "0476312e300404040404040404040404040404040404040404040404040404040404040404010566697273740a68756d616e3a6e69636b00038080e682b966";
const TAG_HASH: &str = "e232cefbbf07a58d5f88e4e57599659e822a72768836824049ec762cb71acb4f";
const ANSWER_HEX: &str = "087461673a76312e30060000000109090909090909090909090909090909090909090909090909090909090909090105050505050505050505050505050505050505050505050505050505050505050a68756d616e3a6e69636b00038080e682b966010c6167656e743a636c61756465010572756e2d37010674616720697406736573732d3100010200";
const ANSWER_HASH: &str = "7a22cc4e85f1e17ab5311e27a6cd23c665ad431cd5f6192fd5a637b43bc58b19";

/// The vectors were taken at the base commit, before the signature module existed.
#[test]
fn sig1_signature_adds_no_bytes_to_existing_kinds() {
    let p = prov("human:nick", Some("run-1"));
    assert_eq!(hex(&enc(&p)), PROV_HEX);
    assert_eq!(Hash::of(&enc(&p)).to_string(), PROV_HASH);
    let l = relay_landing();
    assert_eq!(hex(&enc(&l)), LANDING_HEX);
    assert_eq!(l.id().to_string(), LANDING_HASH);
    assert_eq!(hex(&enc(&tag())), TAG_HEX);
    assert_eq!(tag().id().to_string(), TAG_HASH);
    assert_eq!(hex(&enc(&answer())), ANSWER_HEX);
    assert_eq!(answer().id().to_string(), ANSWER_HASH);

    // A SIGNATURE is its five fields in order, with no derived key or key type.
    let s = Signature {
        subject: h(1),
        namespace: "rdy-tag-v1".into(),
        statement: b"st".to_vec(),
        sig: vec![0xee, 0xff],
        signed_by: p.clone(),
    };
    let fields = enc(&(
        h(1),
        "rdy-tag-v1",
        b"st".to_vec(),
        vec![0xeeu8, 0xff],
        p.clone(),
    ));
    assert_eq!(enc(&s), fields);
    let want = format!(
        "{}0a7264792d7461672d763102737402eeff{PROV_HEX}",
        "01".repeat(32)
    );
    assert_eq!(hex(&enc(&s)), want, "the pinned bytes");
    assert_eq!(s.id(), Hash::of(&enc(&s)));
    assert_eq!(s.id(), hash_of(&s));
    assert_eq!(decode_signature(&enc(&s)), Ok(s));
}

fn sig_with(namespace: usize, statement: usize, sig: usize, actor: usize, run: usize) -> Signature {
    Signature {
        subject: h(1),
        namespace: "n".repeat(namespace),
        statement: vec![b's'; statement],
        sig: vec![0xff; sig],
        signed_by: Provenance {
            actor: "a".repeat(actor),
            run: Some("r".repeat(run)),
            role: Role::Approver,
            at_ms: i64::MIN,
        },
    }
}

#[test]
fn sig1_hostile_lengths_are_checked_before_allocation() {
    // The maximal legal record round-trips and fits the encoded limit.
    let max = sig_with(64, 16384, 16384, 4096, 4096);
    let bytes = enc(&max);
    assert!(bytes.len() <= 65536, "{}", bytes.len());
    assert_eq!(decode_signature(&bytes), Ok(max.clone()));
    let short = Signature {
        signed_by: Provenance {
            run: None,
            at_ms: i64::MAX,
            role: Role::Reader,
            ..max.signed_by.clone()
        },
        ..max.clone()
    };
    assert_eq!(decode_signature(&enc(&short)), Ok(short));

    // One byte over each limit.
    for (what, s, limit) in [
        ("namespace", sig_with(65, 1, 1, 1, 1), 64),
        ("statement", sig_with(1, 16385, 1, 1, 1), 16384),
        ("sig", sig_with(1, 1, 16385, 1, 1), 16384),
        ("actor", sig_with(1, 1, 1, 4097, 1), 4096),
        ("run", sig_with(1, 1, 1, 1, 4097), 4096),
    ] {
        assert_eq!(
            decode_signature(&enc(&s)),
            Err(StatementError::Limit(limit)),
            "{what}"
        );
    }
    assert_eq!(
        decode_signature(&vec![0u8; 65537]),
        Err(StatementError::Limit(65536))
    );

    // Hostile varints after the 32-byte subject.
    let subject = [1u8; 32];
    let with = |tail: &[u8]| {
        let mut v = subject.to_vec();
        v.extend_from_slice(tail);
        v
    };
    let mut max_u64 = vec![0xff; 9];
    max_u64.push(0x01);
    assert_eq!(
        decode_signature(&with(&max_u64)),
        Err(StatementError::Limit(64)),
        "a length of u64::MAX"
    );
    for (what, tail) in [
        ("eleven-byte varint", vec![0xff; 11]),
        ("ten-byte overflow", {
            let mut v = vec![0xff; 9];
            v.push(0x02);
            v
        }),
        ("unterminated varint", vec![0x80, 0x80]),
        ("non-minimal zero", vec![0x80, 0x00]),
        ("length past the end", vec![0x05, b'a']),
        ("no namespace", vec![]),
        ("namespace not UTF-8", vec![0x01, 0xff]),
    ] {
        assert_eq!(
            decode_signature(&with(&tail)),
            Err(StatementError::Syntax),
            "{what}"
        );
    }

    // Every truncation and any trailing byte is refused.
    let small = sig_with(10, 40, 30, 12, 5);
    let bytes = enc(&small);
    for n in 0..bytes.len() {
        assert!(decode_signature(&bytes[..n]).is_err(), "truncated at {n}");
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert_eq!(decode_signature(&trailing), Err(StatementError::Syntax));
    // A non-minimal length varint (0x8a 0x00 for 10) in an otherwise valid record.
    assert_eq!(bytes[32], 10, "the namespace length");
    let mut padded = bytes[..32].to_vec();
    padded.extend_from_slice(&[0x8a, 0x00]);
    padded.extend_from_slice(&bytes[33..]);
    assert_eq!(decode_signature(&padded), Err(StatementError::Syntax));

    // A role or option tag outside its range.
    let role_at = bytes.len() - 1 - 10; // role, then at_ms i64::MIN (10 bytes)
    assert_eq!(bytes[role_at], 3, "Approver");
    let mut bad_role = bytes.clone();
    bad_role[role_at] = 4;
    assert_eq!(decode_signature(&bad_role), Err(StatementError::Syntax));
    let tag_at = role_at - 1 - 5 - 1; // run's Some tag, its length byte and 5 bytes
    assert_eq!(bytes[tag_at], 1, "Some");
    let mut bad_tag = bytes.clone();
    bad_tag[tag_at] = 2;
    assert_eq!(decode_signature(&bad_tag), Err(StatementError::Syntax));

    // Statements: a maximal legal statement parses; one byte over a value or the whole fails.
    let big = "n".repeat(4096);
    let mut a = answer();
    a.relayed_by = Some(relayer(&"r".repeat(4096), None));
    a.person.actor = "p".repeat(4096);
    let t = Tag {
        name: big.clone(),
        tagged_by: prov(&"p".repeat(4096), None),
        ..tag()
    };
    a.item = format!("tag:{big}");
    let built = statement(
        StatementSource::RelayTag {
            hash: h(5),
            tag: &t,
            answer_hash: h(6),
            answer: &a,
        },
        Some(REPO),
        KEY,
    )
    .expect("a maximal tag relay");
    assert!(built.len() <= 16384);
    let parsed = parse_statement(&built).expect("parses");
    assert_eq!(parsed.field("name"), Some(&big[..]));
    let over = Tag {
        name: "n".repeat(4097),
        ..tag()
    };
    assert_eq!(
        statement(
            StatementSource::Tag {
                hash: h(5),
                tag: &over
            },
            Some(REPO),
            KEY
        ),
        Err(StatementError::Limit(4096))
    );
    let long_run = LandingRecord {
        relayed_by: Some(relayer(&"r".repeat(4000), Some(&"x".repeat(96)))),
        ..relay_landing()
    };
    assert_eq!(
        statement(StatementSource::RelayApproval(&long_run), Some(REPO), KEY),
        Err(StatementError::Limit(4096)),
        "the joined relayer counts as one value"
    );
    let at_limit = LandingRecord {
        relayed_by: Some(relayer(&"r".repeat(4000), Some(&"x".repeat(95)))),
        ..relay_landing()
    };
    assert!(statement(StatementSource::RelayApproval(&at_limit), Some(REPO), KEY).is_ok());
    let long_value = KEYBIND.replace("account: nick", &format!("account: {}", "a".repeat(4097)));
    assert_eq!(
        parse_statement(long_value.as_bytes()).err(),
        Some(StatementError::Limit(4096))
    );
    let mut huge = KEYBIND.as_bytes().to_vec();
    huge.resize(16385, b'a');
    assert_eq!(
        parse_statement(&huge).err(),
        Some(StatementError::Limit(16384))
    );
    assert_eq!(
        parse_statement(&vec![b'\n'; 1 << 20]).err(),
        Some(StatementError::Limit(16384))
    );
    let many_lines = format!("rdy-statement: 1\nkind: keybind\n{}", "a: b\n".repeat(3000));
    assert_eq!(
        parse_statement(many_lines.as_bytes()).err(),
        Some(StatementError::Syntax)
    );
}
