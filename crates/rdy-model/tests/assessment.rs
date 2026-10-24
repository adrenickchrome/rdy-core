//! The stored assessment snapshot: wire order, canonical form, limits, vocabularies and
//! unknown keys.

use rdy_model::assessment::*;
use rdy_model::{BlockId, CROCKFORD, Hash};
use serde_yaml_ng::Value;

// ---- builders -----------------------------------------------------------------------------

/// The id with body `n` in Crockford base 32: ids rise with `n`.
fn id(n: u32) -> BlockId {
    let mut body = [b'0'; 5];
    let mut rest = n;
    for slot in body.iter_mut().rev() {
        *slot = CROCKFORD[(rest % 32) as usize];
        rest /= 32;
    }
    BlockId::parse(&format!("b-{}", std::str::from_utf8(&body).unwrap())).unwrap()
}

/// The hash whose bytes rise with `n`.
fn h(n: u32) -> Hash {
    let mut bytes = [0u8; 32];
    bytes[28..].copy_from_slice(&n.to_be_bytes());
    Hash(bytes)
}

fn scores(base: u32) -> AssessmentScores {
    AssessmentScores {
        overall: base,
        grounded: base + 1,
        consistent: base + 2,
        current: base + 3,
        connected: base + 4,
        complete: base + 5,
        governed: base + 6,
    }
}

fn issue(block: u32, code: &str) -> AssessmentIssue {
    AssessmentIssue {
        block: id(block),
        dimension: "grounded".into(),
        code: code.into(),
        subject: Some("subject".into()),
        fix: "tie it".into(),
        dismissal: Some(h(70)),
    }
}

fn full() -> Assessment {
    Assessment {
        proposal: h(1),
        base: Some(h(2)),
        method: 2,
        pack: AssessmentPack {
            name: "core".into(),
            version: "1.0".into(),
            digest: h(3),
        },
        config: h(4),
        policy: Some(AssessedPolicy {
            policy: Some(PolicyRef {
                block: id(100),
                version: h(5),
            }),
            verdict: PolicyVerdict::NeedsPerson {
                role: Some("approver".into()),
                reasons: vec!["b first".into(), "a second".into()],
            },
        }),
        facts: AssessmentFacts {
            changes: 3,
            consequences: 4,
            plan: AssessmentPlanCounts {
                open: 5,
                closed: 6,
                deferred: 7,
                by_kind: vec![(
                    "update".into(),
                    AssessmentCounts {
                        open: 1,
                        closed: 2,
                        deferred: 3,
                    },
                )],
            },
            supersedes: vec![id(10)],
            reaffirms: vec![id(11)],
            reversibility: vec![AssessmentReversal {
                decision: id(12),
                value: AssessmentReversibility::TwoWay,
            }],
            exceptions: vec![AssessmentException {
                kind: AssessmentExceptionKind::ReaffirmOneWay,
                reference: "ref".into(),
            }],
        },
        tensions: vec![AssessmentTension {
            id: h(20),
            kind: "fork".into(),
            severity: AssessmentSeverity::Blocking,
            status: AssessmentTensionStatus::Dismissed,
            answer: Some(AssessmentPlanAnswerRef {
                plan: h(21),
                item: h(22),
            }),
            dismissal: Some(h(23)),
            summary: "two forks".into(),
            blocks: vec![id(13), id(14)],
            vkey: h(24),
        }],
        tensions_omitted: 300,
        suspects: vec![AssessmentSuspect {
            block: id(15),
            cause: id(16),
        }],
        suspects_omitted: 0,
        coverage: AssessmentCoverage {
            pages: vec![AssessmentPage {
                doc: id(17),
                before: scores(10),
                after: scores(20),
            }],
            corpus: AssessmentScoreChange {
                before: scores(30),
                after: scores(40),
            },
        },
        confidence: AssessmentConfidence {
            level: AssessmentLevel::Low,
            reasons: vec![issue(18, "open_tension")],
        },
        reach: vec![id(19)],
        card: AssessmentCard {
            hash: h(30),
            facts: vec!["fact".into(), "fact".into()],
        },
        scope: AssessmentScope {
            proposal: h(1),
            blocks: vec![id(31)],
        },
        issues: vec![issue(32, "ungrounded")],
    }
}

// ---- expected bytes -----------------------------------------------------------------------

fn varint(mut n: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

fn s(text: &str) -> Vec<u8> {
    let mut out = varint(text.len() as u64);
    out.extend_from_slice(text.as_bytes());
    out
}

fn hb(hash: Hash) -> Vec<u8> {
    hash.0.to_vec()
}

fn bid(n: u32) -> Vec<u8> {
    s(id(n).as_str())
}

fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}

fn some(bytes: Vec<u8>) -> Vec<u8> {
    cat(&[vec![1], bytes])
}

fn len(n: u64) -> Vec<u8> {
    varint(n)
}

fn score_bytes(base: u8) -> Vec<u8> {
    (0..7).map(|i| base + i).collect()
}

fn issue_bytes(block: u32, code: &str) -> Vec<u8> {
    cat(&[
        bid(block),
        s("grounded"),
        s(code),
        some(s("subject")),
        s("tie it"),
        some(hb(h(70))),
    ])
}

fn full_bytes() -> Vec<u8> {
    cat(&[
        hb(h(1)),
        some(hb(h(2))),
        vec![2],
        cat(&[s("core"), s("1.0"), hb(h(3))]),
        hb(h(4)),
        // policy: Some(AssessedPolicy { Some(PolicyRef), NeedsPerson { Some(role), reasons } })
        some(cat(&[
            some(cat(&[bid(100), hb(h(5))])),
            vec![1],
            some(s("approver")),
            len(2),
            s("b first"),
            s("a second"),
        ])),
        // facts
        cat(&[
            vec![3, 4],
            vec![5, 6, 7],
            len(1),
            s("update"),
            vec![1, 2, 3],
            len(1),
            bid(10),
            len(1),
            bid(11),
            len(1),
            bid(12),
            vec![1],
            len(1),
            vec![5],
            s("ref"),
        ]),
        // tensions
        len(1),
        cat(&[
            hb(h(20)),
            s("fork"),
            vec![1],
            vec![1],
            some(cat(&[hb(h(21)), hb(h(22))])),
            some(hb(h(23))),
            s("two forks"),
            len(2),
            bid(13),
            bid(14),
            hb(h(24)),
        ]),
        varint(300),
        // suspects
        len(1),
        bid(15),
        bid(16),
        vec![0],
        // coverage
        len(1),
        bid(17),
        score_bytes(10),
        score_bytes(20),
        score_bytes(30),
        score_bytes(40),
        // confidence
        vec![2],
        len(1),
        issue_bytes(18, "open_tension"),
        // reach
        len(1),
        bid(19),
        // card
        hb(h(30)),
        len(2),
        s("fact"),
        s("fact"),
        // scope
        hb(h(1)),
        len(1),
        bid(31),
        // issues
        len(1),
        issue_bytes(32, "ungrounded"),
    ])
}

fn roundtrip<T>(value: &T, bytes: &[u8])
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    assert_eq!(
        postcard::to_allocvec(value).unwrap(),
        bytes,
        "encoded bytes"
    );
    assert_eq!(
        &postcard::from_bytes::<T>(bytes).unwrap(),
        value,
        "decoded value"
    );
    let yaml = serde_yaml_ng::to_string(value).unwrap();
    assert_eq!(
        &serde_yaml_ng::from_str::<T>(&yaml).unwrap(),
        value,
        "yaml form"
    );
}

// ---- tests --------------------------------------------------------------------------------

#[test]
fn ok3_assessment_roundtrips_every_field() {
    let a = full();
    a.validate().unwrap();
    roundtrip(&a, &full_bytes());

    // every enum encodes by its zero-based declaration index, and by snake_case name
    for (i, level) in [
        AssessmentLevel::High,
        AssessmentLevel::Medium,
        AssessmentLevel::Low,
    ]
    .into_iter()
    .enumerate()
    {
        roundtrip(&level, &[i as u8]);
    }
    for (i, v) in [AssessmentSeverity::Advisory, AssessmentSeverity::Blocking]
        .into_iter()
        .enumerate()
    {
        roundtrip(&v, &[i as u8]);
    }
    for (i, v) in [
        AssessmentTensionStatus::Open,
        AssessmentTensionStatus::Dismissed,
    ]
    .into_iter()
    .enumerate()
    {
        roundtrip(&v, &[i as u8]);
    }
    for (i, v) in [
        AssessmentReversibility::OneWay,
        AssessmentReversibility::TwoWay,
    ]
    .into_iter()
    .enumerate()
    {
        roundtrip(&v, &[i as u8]);
    }
    let kinds = [
        (AssessmentExceptionKind::Against, "against"),
        (AssessmentExceptionKind::OneWay, "one_way"),
        (AssessmentExceptionKind::Unapplied, "unapplied"),
        (AssessmentExceptionKind::NotRecommended, "not_recommended"),
        (AssessmentExceptionKind::BlockingTension, "blocking_tension"),
        (AssessmentExceptionKind::ReaffirmOneWay, "reaffirm_one_way"),
    ];
    for (i, (kind, name)) in kinds.into_iter().enumerate() {
        roundtrip(&kind, &[i as u8]);
        assert_eq!(serde_yaml_ng::to_string(&kind).unwrap().trim(), name);
    }
    // declaration order is the sort order
    assert!(AssessmentExceptionKind::Against < AssessmentExceptionKind::ReaffirmOneWay);
    assert!(AssessmentLevel::High < AssessmentLevel::Low);

    // a record without its options, and a version with no base
    let mut bare = full();
    bare.base = None;
    bare.policy = None;
    bare.tensions[0].answer = None;
    bare.tensions[0].dismissal = None;
    bare.confidence.reasons[0].subject = None;
    bare.confidence.reasons[0].dismissal = None;
    bare.validate().unwrap();
    let decoded: Assessment = postcard::from_bytes(&postcard::to_allocvec(&bare).unwrap()).unwrap();
    assert_eq!(decoded, bare);
}

#[test]
fn ok3_assessment_is_full_and_canonical() {
    full().validate().unwrap();

    // the scope names the outer proposal
    let mut a = full();
    a.scope.proposal = h(99);
    assert_eq!(a.validate(), Err("invalid assessment scope"));

    // both omission counts take every u64 value, and a roundtrip keeps them
    for n in [0u64, 1, u64::MAX] {
        let mut a = full();
        a.tensions_omitted = n;
        a.suspects_omitted = n;
        a.validate().unwrap();
        let back: Assessment = postcard::from_bytes(&postcard::to_allocvec(&a).unwrap()).unwrap();
        assert_eq!((back.tensions_omitted, back.suspects_omitted), (n, n));
    }

    // method and pack
    let mut a = full();
    a.method = 0;
    assert_eq!(a.validate(), Err("invalid assessment method"));
    for (name, version, ok) in [
        ("", "1", false),
        ("n", "", false),
        (&"n".repeat(256), "v", true),
        (&"n".repeat(257), "v", false),
        ("n", &"v".repeat(256), true),
        ("n", &"v".repeat(257), false),
    ] {
        let mut a = full();
        a.pack.name = name.to_string();
        a.pack.version = version.to_string();
        let want = if ok {
            Ok(())
        } else {
            Err("invalid assessment pack")
        };
        assert_eq!(a.validate(), want, "pack {} {}", name.len(), version.len());
    }

    // scores: 0..=100 everywhere, including the all-100 before scores of a missing page
    let mut a = full();
    a.coverage.pages[0].before = scores(94); // governed is 100
    a.validate().unwrap();
    a.coverage.pages[0].before = scores(95); // governed is 101
    assert_eq!(a.validate(), Err("invalid assessment score"));
    let all = |v: u32| AssessmentScores {
        overall: v,
        grounded: v,
        consistent: v,
        current: v,
        connected: v,
        complete: v,
        governed: v,
    };
    let mut a = full();
    a.coverage.pages[0].before = all(100);
    a.coverage.corpus.before = all(0);
    a.validate().unwrap();
    for place in 0..3 {
        let mut a = full();
        let bad = AssessmentScores {
            complete: 101,
            ..all(0)
        };
        match place {
            0 => a.coverage.pages[0].after = bad,
            1 => a.coverage.corpus.before = bad,
            _ => a.coverage.corpus.after = bad,
        }
        assert_eq!(a.validate(), Err("invalid assessment score"));
    }

    // text limits: 4,096 bytes pass, 4,097 fail, at each place
    let ok = "x".repeat(4096);
    let long = "x".repeat(4097);
    let texts: [(&str, fn(&mut Assessment, String)); 3] = [
        ("card fact", |a, t| a.card.facts[0] = t),
        ("issue fix", |a, t| a.issues[0].fix = t),
        ("tension summary", |a, t| a.tensions[0].summary = t),
    ];
    for (place, set) in texts {
        let mut a = full();
        set(&mut a, ok.clone());
        a.validate().unwrap_or_else(|e| panic!("{place}: {e}"));
        set(&mut a, long.clone());
        assert_eq!(
            a.validate(),
            Err("assessment text exceeds its limit"),
            "{place}"
        );
    }
    let mut a = full();
    a.confidence.reasons[0].fix = long;
    assert_eq!(a.validate(), Err("assessment text exceeds its limit"));

    // closed vocabularies
    for (place, set) in [
        (
            "dimension",
            (|a: &mut Assessment| a.issues[0].dimension = "overall".into()) as fn(&mut Assessment),
        ),
        ("issue code", |a| a.issues[0].code = "other".into()),
        ("reason dimension", |a| {
            a.confidence.reasons[0].dimension = "x".into()
        }),
        ("reason code", |a| a.confidence.reasons[0].code = "x".into()),
        ("tension kind", |a| a.tensions[0].kind = "other".into()),
        ("plan kind", |a| a.facts.plan.by_kind[0].0 = "other".into()),
    ] {
        let mut a = full();
        set(&mut a);
        assert_eq!(
            a.validate(),
            Err("invalid assessment vocabulary"),
            "{place}"
        );
    }
    for dim in DIMENSIONS {
        let mut a = full();
        a.issues[0].dimension = dim.into();
        a.validate().unwrap();
    }
    for code in ISSUE_CODES {
        let mut a = full();
        a.issues[0].code = code.into();
        a.validate().unwrap();
    }
    for kind in TENSION_KINDS {
        let mut a = full();
        a.tensions[0].kind = kind.into();
        a.validate().unwrap();
    }
    for kind in PLAN_KINDS {
        let mut a = full();
        a.facts.plan.by_kind[0].0 = kind.into();
        a.validate().unwrap();
    }
    assert_eq!(DIMENSIONS.len(), 6);
    assert_eq!(ISSUE_CODES.len(), 12);
    assert_eq!(TENSION_KINDS.len(), 10);
    assert_eq!(PLAN_KINDS.len(), 6);

    // ordering and distinct rows, in every vector
    let unsorted: [(&str, fn(&mut Assessment)); 18] = [
        ("scope blocks", |a| a.scope.blocks = vec![id(32), id(31)]),
        ("scope duplicate", |a| a.scope.blocks = vec![id(31), id(31)]),
        ("reach", |a| a.reach = vec![id(20), id(19)]),
        ("reach duplicate", |a| a.reach = vec![id(19), id(19)]),
        ("supersedes", |a| a.facts.supersedes = vec![id(2), id(1)]),
        ("reaffirms", |a| a.facts.reaffirms = vec![id(2), id(2)]),
        ("tension blocks", |a| {
            a.tensions[0].blocks = vec![id(14), id(13)]
        }),
        ("tensions by id", |a| {
            let mut t = a.tensions[0].clone();
            t.id = h(19);
            a.tensions.push(t);
        }),
        ("suspects by cause", |a| {
            a.suspects = vec![
                AssessmentSuspect {
                    block: id(15),
                    cause: id(17),
                },
                AssessmentSuspect {
                    block: id(15),
                    cause: id(16),
                },
            ]
        }),
        ("suspects duplicate", |a| {
            a.suspects.push(a.suspects[0].clone())
        }),
        ("pages by doc", |a| {
            let mut p = a.coverage.pages[0].clone();
            p.doc = id(1);
            a.coverage.pages.push(p);
        }),
        ("reversibility", |a| {
            a.facts.reversibility.push(AssessmentReversal {
                decision: id(12),
                value: AssessmentReversibility::OneWay,
            })
        }),
        ("exceptions by kind", |a| {
            a.facts.exceptions.push(AssessmentException {
                kind: AssessmentExceptionKind::Against,
                reference: "z".into(),
            })
        }),
        ("exceptions by reference", |a| {
            a.facts.exceptions = vec![
                AssessmentException {
                    kind: AssessmentExceptionKind::OneWay,
                    reference: "b".into(),
                },
                AssessmentException {
                    kind: AssessmentExceptionKind::OneWay,
                    reference: "a".into(),
                },
            ]
        }),
        ("by_kind by name", |a| {
            let counts = a.facts.plan.by_kind[0].1.clone();
            a.facts.plan.by_kind.push(("rejustify".into(), counts));
        }),
        ("issues by code", |a| {
            let mut i = a.issues[0].clone();
            i.code = "dangling_ref".into();
            a.issues.push(i);
        }),
        ("issues duplicate", |a| a.issues.push(a.issues[0].clone())),
        ("reasons by block", |a| {
            let mut i = a.confidence.reasons[0].clone();
            i.block = id(1);
            a.confidence.reasons.push(i);
        }),
    ];
    for (place, edit) in unsorted {
        let mut a = full();
        edit(&mut a);
        assert_eq!(
            a.validate(),
            Err("assessment rows are not sorted and distinct"),
            "{place}"
        );
    }
    // the issue key is (block, dimension, code, subject, fix, dismissal): each part breaks a tie
    let base = issue(40, "ungrounded");
    let tie: [(&str, fn(&mut AssessmentIssue)); 5] = [
        ("dimension", |i| i.dimension = "governed".into()),
        ("code", |i| i.code = "dangling_ref".into()),
        ("subject", |i| i.subject = None),
        ("fix", |i| i.fix = "a".into()),
        ("dismissal", |i| i.dismissal = None),
    ];
    for (part, lower) in tie {
        let mut low = base.clone();
        lower(&mut low);
        // the row with the smaller part sorts first
        let (first, second) = (low, base.clone());
        let mut a = full();
        a.issues = vec![first, second];
        a.validate().unwrap_or_else(|e| panic!("{part}: {e}"));
        a.issues.reverse();
        assert_eq!(
            a.validate(),
            Err("assessment rows are not sorted and distinct"),
            "{part}"
        );
    }
    // equal ids with a different tension content stay rows of one id: refused
    let mut a = full();
    let mut twin = a.tensions[0].clone();
    twin.summary = "other".into();
    a.tensions.push(twin);
    assert_eq!(
        a.validate(),
        Err("assessment rows are not sorted and distinct")
    );

    // the first failed check wins, in wire order: method, pack, policy, facts, tensions,
    // suspects, coverage, confidence, reach, card, scope, issues
    let mut a = full();
    a.method = 0;
    a.pack.name = String::new();
    assert_eq!(a.validate(), Err("invalid assessment method"));
    a.method = 1;
    assert_eq!(a.validate(), Err("invalid assessment pack"));
    a.pack.name = "core".into();
    a.policy = Some(AssessedPolicy {
        policy: None,
        verdict: PolicyVerdict::Accepts { rule: "r".into() },
    });
    a.facts.supersedes = vec![id(2), id(1)];
    assert_eq!(a.validate(), Err("invalid assessment policy"));
    a.policy = None;
    assert_eq!(
        a.validate(),
        Err("assessment rows are not sorted and distinct")
    );
    a.facts.supersedes = vec![id(1)];
    a.tensions[0].kind = "x".into();
    a.coverage.corpus.after = scores(95);
    assert_eq!(a.validate(), Err("invalid assessment vocabulary"));
    a.tensions[0].kind = "fork".into();
    assert_eq!(a.validate(), Err("invalid assessment score"));
    a.coverage.corpus.after = scores(40);
    a.card.facts[0] = "x".repeat(4097);
    a.scope.proposal = h(99);
    a.reach = vec![id(2), id(1)];
    assert_eq!(
        a.validate(),
        Err("assessment rows are not sorted and distinct")
    );
    a.reach = vec![id(1)];
    assert_eq!(a.validate(), Err("assessment text exceeds its limit"));
    a.card.facts[0] = "x".into();
    assert_eq!(a.validate(), Err("invalid assessment scope"));
    a.scope.proposal = h(1);
    a.validate().unwrap();

    // a decoded record is never normalized: validate takes &self and leaves it as it was
    let before = full();
    let mut a = before.clone();
    a.reach = vec![id(2), id(1)];
    let _ = a.validate();
    assert_eq!(a.reach, vec![id(2), id(1)]);
}

#[test]
fn ok3_assessment_row_limits() {
    // 100,000 rows pass in every list that has a limit; 100,001 fail
    let limit = 100_000u32;
    let lists: [(&str, fn(&mut Assessment, u32)); 13] = [
        ("scope blocks", |a, n| {
            a.scope.blocks = (0..n).map(id).collect()
        }),
        ("reach", |a, n| a.reach = (0..n).map(id).collect()),
        ("supersedes", |a, n| {
            a.facts.supersedes = (0..n).map(id).collect()
        }),
        ("reaffirms", |a, n| {
            a.facts.reaffirms = (0..n).map(id).collect()
        }),
        ("tension blocks", |a, n| {
            a.tensions[0].blocks = (0..n).map(id).collect()
        }),
        ("suspects", |a, n| {
            a.suspects = (0..n)
                .map(|i| AssessmentSuspect {
                    block: id(i),
                    cause: id(0),
                })
                .collect()
        }),
        ("card facts", |a, n| {
            a.card.facts = (0..n).map(|_| "f".to_string()).collect()
        }),
        ("tensions", |a, n| {
            a.tensions = (0..n)
                .map(|i| AssessmentTension {
                    id: h(i),
                    kind: "fork".into(),
                    severity: AssessmentSeverity::Advisory,
                    status: AssessmentTensionStatus::Open,
                    answer: None,
                    dismissal: None,
                    summary: String::new(),
                    blocks: vec![],
                    vkey: h(0),
                })
                .collect()
        }),
        ("pages", |a, n| {
            a.coverage.pages = (0..n)
                .map(|i| AssessmentPage {
                    doc: id(i),
                    before: scores(0),
                    after: scores(0),
                })
                .collect()
        }),
        ("reversibility", |a, n| {
            a.facts.reversibility = (0..n)
                .map(|i| AssessmentReversal {
                    decision: id(i),
                    value: AssessmentReversibility::OneWay,
                })
                .collect()
        }),
        ("exceptions", |a, n| {
            a.facts.exceptions = (0..n)
                .map(|i| AssessmentException {
                    kind: AssessmentExceptionKind::Against,
                    reference: format!("{i:08}"),
                })
                .collect()
        }),
        ("issues", |a, n| {
            a.issues = (0..n).map(|i| issue(i, "ungrounded")).collect()
        }),
        ("reasons", |a, n| {
            a.confidence.reasons = (0..n).map(|i| issue(i, "ungrounded")).collect()
        }),
    ];
    for (place, set) in lists {
        let mut a = full();
        set(&mut a, limit);
        a.validate()
            .unwrap_or_else(|e| panic!("{place} at the limit: {e}"));
        set(&mut a, limit + 1);
        assert_eq!(
            a.validate(),
            Err("assessment text exceeds its limit"),
            "{place} over the limit"
        );
    }
}

#[test]
fn ok3_policy_verdicts_keep_rule_role_reasons_and_no_policy() {
    let policy_ref = PolicyRef {
        block: id(1),
        version: h(2),
    };

    // Accepts: the rule is 1-256 bytes
    let accepts = AssessedPolicy {
        policy: Some(policy_ref.clone()),
        verdict: PolicyVerdict::Accepts {
            rule: "rule-1".into(),
        },
    };
    accepts.validate().unwrap();
    roundtrip(
        &accepts,
        &cat(&[some(cat(&[bid(1), hb(h(2))])), vec![0], s("rule-1")]),
    );
    for (rule, ok) in [
        ("", false),
        ("r", true),
        (&"r".repeat(256)[..], true),
        (&"r".repeat(257)[..], false),
    ] {
        let p = AssessedPolicy {
            policy: Some(policy_ref.clone()),
            verdict: PolicyVerdict::Accepts { rule: rule.into() },
        };
        assert_eq!(p.validate().is_ok(), ok, "rule of {} bytes", rule.len());
        if !ok {
            assert_eq!(p.validate(), Err("invalid assessment policy"));
        }
    }

    // NeedsPerson: a present role is 1-256 bytes; reasons keep their order
    let needs = AssessedPolicy {
        policy: Some(policy_ref.clone()),
        verdict: PolicyVerdict::NeedsPerson {
            role: Some("lead".into()),
            reasons: vec!["z last".into(), "a first".into()],
        },
    };
    needs.validate().unwrap();
    roundtrip(
        &needs,
        &cat(&[
            some(cat(&[bid(1), hb(h(2))])),
            vec![1],
            some(s("lead")),
            len(2),
            s("z last"),
            s("a first"),
        ]),
    );
    let with = |role: Option<String>, reasons: Vec<String>| AssessedPolicy {
        policy: Some(policy_ref.clone()),
        verdict: PolicyVerdict::NeedsPerson { role, reasons },
    };
    let one = vec!["why".to_string()];
    for (role, ok) in [
        (None, true),
        (Some(String::new()), false),
        (Some("r".repeat(256)), true),
        (Some("r".repeat(257)), false),
    ] {
        assert_eq!(
            with(role.clone(), one.clone()).validate().is_ok(),
            ok,
            "role {role:?}"
        );
    }
    // 1-100 reasons, each 1-4,096 bytes
    assert!(with(None, vec![]).validate().is_err());
    assert!(with(None, vec!["r".into(); 100]).validate().is_ok());
    assert!(with(None, vec!["r".into(); 101]).validate().is_err());
    assert!(with(None, vec!["r".repeat(4096)]).validate().is_ok());
    assert!(with(None, vec!["r".repeat(4097)]).validate().is_err());
    assert!(with(None, vec![String::new()]).validate().is_err());
    assert!(
        with(None, vec!["ok".into(), String::new()])
            .validate()
            .is_err()
    );
    assert_eq!(
        with(None, vec![]).validate(),
        Err("invalid assessment policy")
    );

    // no policy: valid only with NeedsPerson, and the pinned reason is the exact text
    let none = AssessedPolicy {
        policy: None,
        verdict: PolicyVerdict::NeedsPerson {
            role: None,
            reasons: vec!["no authority policy is set".into()],
        },
    };
    none.validate().unwrap();
    roundtrip(
        &none,
        &cat(&[
            vec![0],
            vec![1],
            vec![0],
            len(1),
            s("no authority policy is set"),
        ]),
    );
    let none_accepts = AssessedPolicy {
        policy: None,
        verdict: PolicyVerdict::Accepts { rule: "r".into() },
    };
    assert_eq!(none_accepts.validate(), Err("invalid assessment policy"));

    // inside an assessment, an absent outer policy is the pre-policy form and is valid; a bad
    // one fails the assessment at the policy check
    let mut a = full();
    a.policy = None;
    a.validate().unwrap();
    a.policy = Some(none_accepts);
    assert_eq!(a.validate(), Err("invalid assessment policy"));
    a.policy = Some(none);
    a.validate().unwrap();
}

/// How many mappings (struct or struct-variant bodies) a value holds.
fn mappings(v: &Value) -> usize {
    match v {
        Value::Mapping(m) => 1 + m.values().map(mappings).sum::<usize>(),
        Value::Sequence(items) => items.iter().map(mappings).sum(),
        Value::Tagged(t) => mappings(&t.value),
        _ => 0,
    }
}

/// Adds an unknown key to the `n`th mapping in traversal order.
fn add_unknown_key(v: &mut Value, n: &mut usize) -> bool {
    match v {
        Value::Mapping(m) => {
            if *n == 0 {
                m.insert(Value::String("zzz_unknown".into()), Value::Bool(true));
                return true;
            }
            *n -= 1;
            m.iter_mut().any(|(_, child)| add_unknown_key(child, n))
        }
        Value::Sequence(items) => items.iter_mut().any(|child| add_unknown_key(child, n)),
        Value::Tagged(t) => add_unknown_key(&mut t.value, n),
        _ => false,
    }
}

/// The sorted key list of every mapping, with its path.
fn key_sets(v: &Value, path: &str, out: &mut Vec<String>) {
    match v {
        Value::Mapping(m) => {
            let mut keys: Vec<String> = m.keys().map(|k| k.as_str().unwrap().to_string()).collect();
            keys.sort();
            out.push(format!("{path}: {}", keys.join(",")));
            for (k, child) in m {
                key_sets(child, &format!("{path}.{}", k.as_str().unwrap()), out);
            }
        }
        Value::Sequence(items) => {
            for child in items {
                key_sets(child, &format!("{path}[]"), out);
            }
        }
        Value::Tagged(t) => key_sets(
            &t.value,
            &format!("{path}!{}", t.tag.to_string().trim_start_matches('!')),
            out,
        ),
        _ => {}
    }
}

#[test]
fn ok3_no_content_copy_fields() {
    let value = serde_yaml_ng::to_value(full()).unwrap();
    let mut sets = Vec::new();
    key_sets(&value, "", &mut sets);
    sets.sort();
    sets.dedup();
    let want = [
        ": base,card,confidence,config,coverage,facts,issues,method,pack,policy,proposal,reach,scope,suspects,suspects_omitted,tensions,tensions_omitted",
        ".card: facts,hash",
        ".confidence: level,reasons",
        ".confidence.reasons[]: block,code,dimension,dismissal,fix,subject",
        ".coverage: corpus,pages",
        ".coverage.corpus: after,before",
        ".coverage.corpus.after: complete,connected,consistent,current,governed,grounded,overall",
        ".coverage.corpus.before: complete,connected,consistent,current,governed,grounded,overall",
        ".coverage.pages[]: after,before,doc",
        ".coverage.pages[].after: complete,connected,consistent,current,governed,grounded,overall",
        ".coverage.pages[].before: complete,connected,consistent,current,governed,grounded,overall",
        ".facts: changes,consequences,exceptions,plan,reaffirms,reversibility,supersedes",
        ".facts.exceptions[]: kind,reference",
        ".facts.plan: by_kind,closed,deferred,open",
        ".facts.plan.by_kind[][]: closed,deferred,open",
        ".facts.reversibility[]: decision,value",
        ".issues[]: block,code,dimension,dismissal,fix,subject",
        ".pack: digest,name,version",
        ".policy: policy,verdict",
        ".policy.policy: block,version",
        ".policy.verdict!needs_person: reasons,role",
        ".scope: blocks,proposal",
        ".suspects[]: block,cause",
        ".tensions[]: answer,blocks,dismissal,id,kind,severity,status,summary,vkey",
        ".tensions[].answer: item,plan",
    ];
    let mut want = want;
    want.sort();
    assert_eq!(sets, want);
    // no body, field map, copied block or stored graph appears anywhere in the field set
    for banned in [
        "body",
        "fields",
        "content",
        "text",
        "graph",
        "cache",
        "block_body",
    ] {
        assert!(
            !sets.iter().any(|line| line
                .split(": ")
                .nth(1)
                .is_some_and(|keys| keys.split(',').any(|k| k == banned))),
            "{banned} is a content-copy field"
        );
    }
}

#[test]
fn ok3_future_record_inputs_reject_unknown_keys() {
    let value = serde_yaml_ng::to_value(full()).unwrap();
    let total = mappings(&value);
    // the root, every nested struct and the struct variant are all reached
    assert_eq!(total, 25);
    serde_yaml_ng::from_value::<Assessment>(value.clone()).unwrap();
    for n in 0..total {
        let mut edited = value.clone();
        let mut at = n;
        assert!(add_unknown_key(&mut edited, &mut at));
        assert!(
            serde_yaml_ng::from_value::<Assessment>(edited).is_err(),
            "mapping {n} accepted an unknown key"
        );
    }
    // the verdict variant on its own, in both shapes
    let accepts = serde_yaml_ng::to_value(PolicyVerdict::Accepts { rule: "r".into() }).unwrap();
    let mut edited = accepts.clone();
    assert!(add_unknown_key(&mut edited, &mut 0));
    assert!(serde_yaml_ng::from_value::<PolicyVerdict>(edited).is_err());
    serde_yaml_ng::from_value::<PolicyVerdict>(accepts).unwrap();
}
