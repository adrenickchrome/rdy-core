//! ib5: inbox entries from the logged tension and suspect moves (founder decision C, D-Z0EMP).
use rdy_engine::inbox::Entry;
use rdy_engine::inbox::inflight::RunInFlight;
use rdy_engine::inbox::moves::{MoveRef, transition_entries, why_text};
use rdy_model::{BlockId, Hash};
use rdy_store::inbox::{InboxCursor, InboxSub};
use rdy_store::segment::Location;
use rdy_store::transition::*;

const AT: Location = Location {
    segment: 0,
    offset: 4096,
};

fn sub(actor: &str, run: &str) -> InboxSub {
    InboxSub {
        actor: actor.into(),
        run: Some(run.into()),
        cursor: InboxCursor {
            at: Location {
                segment: 0,
                offset: 0,
            },
            skip: 0,
        },
        areas: vec![],
        role: "writer".into(),
    }
}
fn why(cause: Cause, id: Option<Hash>) -> Why {
    Why {
        cause,
        id,
        actor: None,
        run: None,
    }
}
fn side(c: &[u8], actor: &str, run: &str) -> Side {
    Side {
        change: Some(Hash::of(c)),
        actor: Some(actor.into()),
        run: Some(run.into()),
        landing: None,
    }
}
fn canon() -> Side {
    Side {
        change: None,
        actor: None,
        run: None,
        landing: Some(Hash::of(b"l1")),
    }
}
fn tm(id: &[u8], sides: Vec<Side>, w: Why) -> TensionMove {
    TensionMove {
        tension: Hash::of(id),
        kind: "edit_collision".into(),
        severity: "blocking".into(),
        sides,
        rekey: None,
        why: w,
    }
}
fn sm(block: &str, writers: &[(&str, &str)]) -> SuspectMove {
    SuspectMove {
        block: block.parse().unwrap(),
        target: "c-TTTTT".parse().unwrap(),
        reason: "changed".into(),
        writers: writers
            .iter()
            .map(|(a, r)| Author {
                actor: a.to_string(),
                run: Some(r.to_string()),
            })
            .collect(),
        why: why(Cause::Landed, Some(Hash::of(b"l1"))),
    }
}
fn events(es: &[Entry]) -> Vec<&str> {
    es.iter().map(|e| e.event).collect()
}
/// The entries alone, for the tests that read a whole batch (raw indexes from 0).
fn entries(sub: &InboxSub, infl: &RunInFlight, moves: &[MoveRef<'_>]) -> Vec<Entry> {
    transition_entries(sub, infl, moves, 0, AT)
        .into_iter()
        .map(|(_, e)| e)
        .collect()
}
/// A read that resumes a batch at raw index `skip`: it walks every move (a re-key fold reads its
/// partner) and delivers the entries whose last raw index is `skip` or later, as Task 6 does.
fn resume(
    sub: &InboxSub,
    infl: &RunInFlight,
    moves: &[MoveRef<'_>],
    first: usize,
    skip: usize,
) -> Vec<(usize, Entry)> {
    transition_entries(sub, infl, moves, first, AT)
        .into_iter()
        .filter(|(raw, _)| *raw >= skip)
        .collect()
}

#[test]
fn ib5_a_tension_on_your_change_opens_and_others_do_not() {
    let a = sub("agent:a", "a");
    let mine = tm(
        b"t1",
        vec![side(b"c1", "agent:a", "a"), side(b"c2", "agent:b", "b")],
        why(Cause::Committed, Some(Hash::of(b"c2"))),
    );
    let theirs = tm(
        b"t2",
        vec![side(b"c3", "agent:b", "b"), canon()],
        why(Cause::Committed, Some(Hash::of(b"c3"))),
    );
    let es = entries(
        &a,
        &RunInFlight::default(),
        &[MoveRef::Opened(&mine), MoveRef::Opened(&theirs)],
    );
    assert_eq!(events(&es), ["tension_opened"]);
    assert!(
        es[0].line.contains(" opened on your change ")
            && es[0].line.ends_with(&format!(
                ": a side changed (change {})",
                Hash::of(b"c2").short()
            )),
        "{}",
        es[0].line
    );
    assert_eq!(es[0].seq, AT);
}

#[test]
fn ib5_a_landed_side_says_the_tension_left_your_in_flight_changes() {
    let a = sub("agent:a", "a");
    let mut gone = tm(
        b"t1",
        vec![side(b"c1", "agent:a", "a"), side(b"c2", "agent:b", "b")],
        why(Cause::Landed, Some(Hash::of(b"l1"))),
    );
    let mut next = tm(
        b"t2",
        vec![canon(), side(b"c2", "agent:b", "b")],
        why(Cause::Landed, Some(Hash::of(b"l1"))),
    );
    gone.rekey = Some(next.tension);
    next.rekey = Some(gone.tension);
    let es = entries(
        &a,
        &RunInFlight::default(),
        &[MoveRef::Resolved(&gone), MoveRef::Opened(&next)],
    );
    assert_eq!(events(&es), ["tension_resolved"]);
    assert!(
        es[0].line.ends_with(&format!(
            "no longer involves your in-flight changes: landing {}",
            Hash::of(b"l1").short()
        )),
        "{}",
        es[0].line
    );
}

#[test]
fn ib5_a_rekey_that_stays_yours_is_one_entry() {
    let a = sub("agent:a", "a");
    let mut gone = tm(
        b"t1",
        vec![side(b"c1", "agent:a", "a"), side(b"c2", "agent:b", "b")],
        why(Cause::Reviewed, Some(Hash::of(b"rv"))),
    );
    let mut next = tm(
        b"t2",
        vec![side(b"c1", "agent:a", "a"), side(b"c2", "agent:b", "b")],
        why(Cause::Reviewed, Some(Hash::of(b"rv"))),
    );
    gone.rekey = Some(next.tension);
    next.rekey = Some(gone.tension);
    let es = entries(
        &a,
        &RunInFlight::default(),
        &[MoveRef::Resolved(&gone), MoveRef::Opened(&next)],
    );
    assert_eq!(events(&es), ["tension_opened"]);
    assert!(
        es[0].line.contains(&format!(
            " replaces {} on your change ",
            gone.tension.short()
        )),
        "{}",
        es[0].line
    );
}

#[test]
fn ib5_a_dismissal_names_its_reviewer() {
    let a = sub("agent:a", "a");
    let mut m = tm(
        b"t1",
        vec![side(b"c1", "agent:a", "a")],
        why(Cause::Dismissed, Some(Hash::of(b"rv"))),
    );
    m.why.actor = Some("human:nick".into());
    let es = entries(&a, &RunInFlight::default(), &[MoveRef::Resolved(&m)]);
    assert_eq!(events(&es), ["tension_dismissed"]);
    assert!(
        es[0].line.ends_with("dismissed by human:nick"),
        "{}",
        es[0].line
    );
    assert_eq!(
        es[0].ids,
        vec![Hash::of(b"t1").to_string(), Hash::of(b"rv").to_string()]
    );
}

#[test]
fn ib5_a_suspect_reaches_its_writers_and_the_writers_of_its_block_now() {
    let a = sub("agent:a", "a");
    let by_writer = sm("c-AAAAA", &[("agent:a", "a")]);
    let by_block = sm("c-BBBBB", &[]);
    let other = sm("c-CCCCC", &[("agent:b", "b")]);
    let mut infl = RunInFlight::default();
    infl.blocks.insert("c-BBBBB".parse::<BlockId>().unwrap());
    let es = entries(
        &a,
        &infl,
        &[
            MoveRef::New(&by_writer),
            MoveRef::Gone(&by_block),
            MoveRef::New(&other),
        ],
    );
    assert_eq!(events(&es), ["suspect_new", "suspect_gone"]);
    assert!(
        es[0]
            .line
            .starts_with("c-AAAAA is suspect: c-TTTTT changed: landing "),
        "{}",
        es[0].line
    );
    assert!(
        es[1]
            .line
            .starts_with("c-BBBBB is no longer suspect: c-TTTTT changed: "),
        "{}",
        es[1].line
    );
}

#[test]
fn ib5_every_cause_has_a_text() {
    // guard of the closed list: why_text has one arm per cause and no wildcard
    for c in [
        Cause::Committed,
        Cause::Landed,
        Cause::Dismissed,
        Cause::ViewChanged,
        Cause::Reviewed,
        Cause::AltSet,
        Cause::Written,
        Cause::PackReload,
        Cause::Reopened,
        Cause::Recheck,
    ] {
        let id = matches!(
            c,
            Cause::Committed
                | Cause::Landed
                | Cause::Dismissed
                | Cause::ViewChanged
                | Cause::Reviewed
                | Cause::AltSet
        )
        .then(|| Hash::of(b"x"));
        let mut w = why(c, id);
        if c == Cause::Dismissed {
            w.actor = Some("human:nick".into());
        }
        assert!(!why_text(&w).is_empty());
    }
}

#[test]
fn ib5_each_entry_reports_the_raw_index_of_its_last_move() {
    // orchestrator ruling after G2: raw indexes go on from the batch's events (`first`); a move
    // that matches nothing is still walked; a re-key fold reports the later of its two moves.
    let a = sub("agent:a", "a");
    let theirs = tm(
        b"t0",
        vec![side(b"c3", "agent:b", "b")],
        why(Cause::Landed, Some(Hash::of(b"l1"))),
    );
    let mut gone = tm(
        b"t1",
        vec![side(b"c1", "agent:a", "a"), side(b"c2", "agent:b", "b")],
        why(Cause::Reviewed, Some(Hash::of(b"rv"))),
    );
    let left = tm(
        b"t2",
        vec![side(b"c1", "agent:a", "a")],
        why(Cause::Reviewed, Some(Hash::of(b"rv"))),
    );
    let mut next = tm(
        b"t3",
        vec![side(b"c1", "agent:a", "a"), side(b"c2", "agent:b", "b")],
        why(Cause::Reviewed, Some(Hash::of(b"rv"))),
    );
    gone.rekey = Some(next.tension);
    next.rekey = Some(gone.tension);
    let new = sm("c-AAAAA", &[("agent:a", "a")]);
    let moves = [
        MoveRef::Resolved(&theirs),
        MoveRef::Resolved(&gone),
        MoveRef::Resolved(&left),
        MoveRef::Opened(&next),
        MoveRef::New(&new),
    ];
    let got: Vec<(usize, &str)> = transition_entries(&a, &RunInFlight::default(), &moves, 3, AT)
        .iter()
        .map(|(raw, e)| (*raw, e.event))
        .collect();
    assert_eq!(
        got,
        [
            (5, "tension_resolved"),
            (6, "tension_opened"),
            (7, "suspect_new")
        ]
    );
}

#[test]
fn ib5_a_cut_in_the_move_tail_resumes_with_exactly_the_remaining_moves() {
    // a batch of 2 events then 3 moves: raw indexes 2, 3, 4
    let a = sub("agent:a", "a");
    let x = sm("c-XXXXX", &[("agent:a", "a")]);
    let y = sm("c-YYYYY", &[]);
    let z = sm("c-ZZZZZ", &[]);
    let moves = [MoveRef::Gone(&x), MoveRef::New(&y), MoveRef::New(&z)];
    let mut before = RunInFlight::default();
    before.blocks.insert("c-YYYYY".parse::<BlockId>().unwrap());
    let page = resume(&a, &before, &moves, 2, 0);
    let got: Vec<(usize, &str)> = page.iter().map(|(raw, e)| (*raw, e.event)).collect();
    assert_eq!(got, [(2, "suspect_gone"), (3, "suspect_new")]);
    // the page is cut after the first entry: the cursor's skip is the raw index after it
    let skip = page[0].0 + 1;
    // then the state changes: a no longer writes c-YYYYY, and now writes c-ZZZZZ and c-XXXXX
    let mut after = RunInFlight::default();
    after.blocks.insert("c-ZZZZZ".parse::<BlockId>().unwrap());
    after.blocks.insert("c-XXXXX".parse::<BlockId>().unwrap());
    let rest = resume(&a, &after, &moves, 2, skip);
    let lines: Vec<(usize, &str)> = rest
        .iter()
        .map(|(raw, e)| (*raw, e.line.as_str()))
        .collect();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0].0, 4);
    assert!(
        lines[0].1.starts_with("c-ZZZZZ is suspect: "),
        "{}",
        lines[0].1
    );
}

#[test]
fn ib5_a_cut_before_a_rekey_fold_resumes_with_the_whole_fold() {
    // the fold's resolution comes before the cut and its opening after it: the resume still
    // reads the pair and says "replaces", once
    let a = sub("agent:a", "a");
    let mut gone = tm(
        b"t1",
        vec![side(b"c1", "agent:a", "a"), side(b"c2", "agent:b", "b")],
        why(Cause::Reviewed, Some(Hash::of(b"rv"))),
    );
    let left = tm(
        b"t2",
        vec![side(b"c1", "agent:a", "a")],
        why(Cause::Reviewed, Some(Hash::of(b"rv"))),
    );
    let mut next = tm(
        b"t3",
        vec![side(b"c1", "agent:a", "a"), side(b"c2", "agent:b", "b")],
        why(Cause::Reviewed, Some(Hash::of(b"rv"))),
    );
    gone.rekey = Some(next.tension);
    next.rekey = Some(gone.tension);
    let moves = [
        MoveRef::Resolved(&gone),
        MoveRef::Resolved(&left),
        MoveRef::Opened(&next),
    ];
    let infl = RunInFlight::default();
    let page = resume(&a, &infl, &moves, 0, 0);
    let got: Vec<(usize, &str)> = page.iter().map(|(raw, e)| (*raw, e.event)).collect();
    assert_eq!(got, [(1, "tension_resolved"), (2, "tension_opened")]);
    let rest = resume(&a, &infl, &moves, 0, page[0].0 + 1);
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].1, page[1].1);
    assert!(
        rest[0].1.line.contains(&format!(
            " replaces {} on your change ",
            gone.tension.short()
        )),
        "{}",
        rest[0].1.line
    );
}

#[test]
fn ib5_a_batch_of_many_rekey_folds_pairs_each_resolution_with_its_opening() {
    // a pack reload that changes a kind's severity re-keys every tension of that kind: one batch
    // holds thousands of resolution and opening pairs, all on the session's changes, and each
    // pair folds into one "replaces" entry (fix round 1: the fold lookup is a map, not a scan)
    const K: usize = 5000;
    let a = sub("agent:a", "a");
    let mine = || vec![side(b"c1", "agent:a", "a"), side(b"c2", "agent:b", "b")];
    let old = |i: usize| Hash::of(format!("old{i}").as_bytes());
    let new = |i: usize| Hash::of(format!("new{i}").as_bytes());
    let resolutions: Vec<TensionMove> = (0..K)
        .map(|i| {
            let mut m = tm(b"", mine(), why(Cause::PackReload, None));
            m.tension = old(i);
            m.rekey = Some(new(i));
            m
        })
        .collect();
    // the openings come in reverse order, so a fold must find its partner, not its neighbour
    let openings: Vec<TensionMove> = (0..K)
        .rev()
        .map(|i| {
            let mut m = tm(b"", mine(), why(Cause::PackReload, None));
            m.tension = new(i);
            m.rekey = Some(old(i));
            m
        })
        .collect();
    let moves: Vec<MoveRef<'_>> = resolutions
        .iter()
        .map(MoveRef::Resolved)
        .chain(openings.iter().map(MoveRef::Opened))
        .collect();
    let got = transition_entries(&a, &RunInFlight::default(), &moves, 0, AT);
    assert_eq!(got.len(), K);
    for (n, (raw, e)) in got.iter().enumerate() {
        let i = K - 1 - n;
        assert_eq!(*raw, K + n);
        assert_eq!(e.event, "tension_opened");
        assert_eq!(e.ids[0], new(i).to_string());
        assert!(
            e.line.starts_with(&format!(
                "tension {} replaces {} on your change ",
                new(i).short(),
                old(i).short()
            )),
            "{}",
            e.line
        );
    }
}
