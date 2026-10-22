//! cidx1: the git log parser, the index it builds and the git command line (no git process).
use rdy_engine::code_index::*;
use std::collections::BTreeSet;
use std::path::Path;

/// The nonce the records below carry.
const N: &str = "0123456789abcdef0123456789abcdef";

/// Full commit hashes (40 hex characters): `parse_log` skips a record with any other hash.
const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const C: &str = "cccccccccccccccccccccccccccccccccccccccc";

/// One commit record as `git log -z --name-status --format=<log_format(N)>` prints it.
fn rec(hash: &str, trailers: &str, changes: &[&str]) -> Vec<u8> {
    let mut v = vec![0x1e];
    v.extend_from_slice(N.as_bytes());
    v.extend_from_slice(hash.as_bytes());
    v.push(0x1f);
    v.extend_from_slice(N.as_bytes());
    v.extend_from_slice(trailers.as_bytes());
    v.push(0x1f);
    v.extend_from_slice(N.as_bytes());
    if !changes.is_empty() {
        v.extend_from_slice(b"\0\n");
    }
    for c in changes {
        v.extend_from_slice(c.as_bytes());
        v.push(0);
    }
    v
}

fn commit(hash: &str, implements: &[&str], changes: Vec<FileChange>) -> Commit {
    Commit {
        hash: hash.into(),
        implements: implements.iter().map(|s| s.to_string()).collect(),
        changes,
    }
}

fn t(p: &str) -> FileChange {
    FileChange::Touch(p.into())
}

fn hashes(h: &[&str]) -> Vec<String> {
    h.iter().map(|s| s.to_string()).collect()
}

#[test]
fn cidx1_parse_log_reads_hashes_trailers_and_changes() {
    let mut out = rec(
        A,
        "s-AAAAA, k-BBBBB,D-CCCCC",
        &["A", "src/a.rs", "M", "src/b.rs"],
    );
    out.extend(rec(
        B,
        "",
        &[
            "R096", "src/a.rs", "src/c.rs", "D", "src/b.rs", "T", "src/t.rs",
        ],
    ));
    assert_eq!(
        parse_log(&out, N),
        vec![
            commit(
                A,
                &["s-AAAAA", "k-BBBBB", "D-CCCCC"],
                vec![t("src/a.rs"), t("src/b.rs")]
            ),
            commit(
                B,
                &[],
                vec![
                    FileChange::Rename {
                        from: "src/a.rs".into(),
                        to: "src/c.rs".into()
                    },
                    FileChange::Delete("src/b.rs".into()),
                    t("src/t.rs"),
                ]
            ),
        ]
    );
}

#[test]
fn cidx1_empty_values_and_repeats_in_a_trailer_are_dropped() {
    let c = parse_log(&rec(A, " , s-AAAAA,,s-AAAAA , ", &["M", "x.rs"]), N);
    assert_eq!(c[0].implements, vec!["s-AAAAA".to_string()]);
}

#[test]
fn cidx1_a_commit_with_no_changes_parses() {
    let mut out = rec(A, "s-AAAAA", &[]);
    out.extend(rec(B, "", &["M", "x.rs"]));
    let c = parse_log(&out, N);
    assert_eq!(c.len(), 2);
    assert!(c[0].changes.is_empty());
    assert_eq!(c[1].changes, vec![t("x.rs")]);
}

#[test]
fn cidx1_a_path_that_is_not_utf8_is_skipped() {
    let mut out = rec(A, "s-AAAAA", &["M"]);
    out.extend_from_slice(b"bad\xff.rs\0");
    out.extend_from_slice(b"M\0ok.rs\0");
    assert_eq!(parse_log(&out, N)[0].changes, vec![t("ok.rs")]);
}

#[test]
fn cidx1_a_truncated_record_ends_without_a_panic() {
    let mut out = rec(A, "s-AAAAA", &["M", "a.rs", "R100", "b.rs"]);
    out.push(0x1e);
    out.extend_from_slice(N.as_bytes());
    out.extend_from_slice(C.as_bytes());
    let c = parse_log(&out, N);
    assert_eq!(c.len(), 1, "the chunk without separators is skipped");
    assert_eq!(c[0].changes, vec![t("a.rs")], "the rename lacks its target");
}

#[test]
fn cidx1_a_rename_carries_the_files_citations_to_its_new_path() {
    let idx = CodeIndex::from_commits(
        &[
            commit("c1", &["s-AAAAA"], vec![t("src/a.rs")]),
            commit("c2", &[], vec![t("src/a.rs")]),
            commit(
                "c3",
                &["r-DDDDD"],
                vec![FileChange::Rename {
                    from: "src/a.rs".into(),
                    to: "src/b.rs".into(),
                }],
            ),
        ],
        None,
        Some("c3".into()),
    );
    assert!(!idx.files.contains_key("src/a.rs"));
    let b = &idx.files["src/b.rs"];
    assert_eq!(b["s-AAAAA"], hashes(&["c1"]));
    assert_eq!(b["r-DDDDD"], hashes(&["c3"]));
    assert_eq!(idx.commits, 3);
    assert_eq!(idx.cited["s-AAAAA"], hashes(&["c1"]));
    assert_eq!(idx.head.as_deref(), Some("c3"));
}

#[test]
fn cidx1_a_rename_onto_a_cited_path_merges_without_repeats() {
    let idx = CodeIndex::from_commits(
        &[
            commit("c1", &["s-AAAAA"], vec![t("a.rs"), t("b.rs")]),
            commit("c2", &["s-AAAAA"], vec![t("a.rs")]),
            commit(
                "c3",
                &[],
                vec![FileChange::Rename {
                    from: "a.rs".into(),
                    to: "b.rs".into(),
                }],
            ),
        ],
        None,
        None,
    );
    assert_eq!(idx.files["b.rs"]["s-AAAAA"], hashes(&["c1", "c2"]));
}

#[test]
fn cidx1_a_delete_drops_the_paths_citations_and_a_re_add_starts_clean() {
    let idx = CodeIndex::from_commits(
        &[
            commit("c1", &["s-AAAAA"], vec![t("x.rs")]),
            commit("c2", &[], vec![FileChange::Delete("x.rs".into())]),
            commit("c3", &["s-BBBBB"], vec![t("x.rs")]),
        ],
        None,
        Some("c3".into()),
    );
    assert_eq!(
        idx.files["x.rs"].keys().collect::<Vec<_>>(),
        vec!["s-BBBBB"]
    );
    assert_eq!(
        idx.cited["s-AAAAA"],
        hashes(&["c1"]),
        "the commit still cites A"
    );
}

#[test]
fn cidx1_only_paths_present_at_head_are_kept() {
    let head = BTreeSet::from(["kept.rs".to_string()]);
    let idx = CodeIndex::from_commits(
        &[commit("c1", &["s-AAAAA"], vec![t("kept.rs"), t("gone.rs")])],
        Some(&head),
        Some("c1".into()),
    );
    assert_eq!(idx.files.keys().collect::<Vec<_>>(), vec!["kept.rs"]);
}

#[test]
fn cidx1_one_commit_citing_one_block_counts_once_per_file() {
    let idx = CodeIndex::from_commits(
        &[
            commit("c1", &["s-AAAAA"], vec![t("x.rs"), t("x.rs")]),
            commit("c2", &["s-AAAAA"], vec![t("x.rs")]),
        ],
        None,
        Some("c2".into()),
    );
    assert_eq!(idx.files["x.rs"]["s-AAAAA"], hashes(&["c1", "c2"]));
    assert_eq!(idx.cited["s-AAAAA"], hashes(&["c1", "c2"]));
}

#[test]
fn cidx1_the_git_command_sheds_the_callers_git_environment() {
    let c = git_command(Path::new("/repo"));
    let removed: BTreeSet<String> = c
        .get_envs()
        .filter(|(_, v)| v.is_none())
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect();
    for k in GIT_ENV_REMOVED {
        assert!(removed.contains(*k), "{k} is not removed");
    }
    for k in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_CONFIG_PARAMETERS",
    ] {
        assert!(GIT_ENV_REMOVED.contains(&k), "{k}");
    }
    let args: Vec<String> = c
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(args[..2], ["-C".to_string(), "/repo".to_string()]);
    assert!(
        args.windows(2)
            .any(|w| w[0] == "-c" && w[1] == "core.fsmonitor=false"),
        "{args:?}"
    );
}

#[test]
fn cidx1_the_git_command_never_fetches_from_a_promisor_remote() {
    // A partial clone lazily fetches missing blobs (rename detection reads them); rdy reads
    // git and never writes it, so the command forbids that fetch (git >= 2.45).
    let c = git_command(Path::new("/repo"));
    let set: Vec<(String, String)> = c
        .get_envs()
        .filter_map(|(k, v)| {
            v.map(|v| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().into_owned(),
                )
            })
        })
        .collect();
    assert!(
        set.contains(&("GIT_NO_LAZY_FETCH".to_string(), "1".to_string())),
        "{set:?}"
    );
}

#[test]
fn cidx1_a_copy_status_parses_as_a_touch_of_the_copy() {
    // `C` (copy detection, -C) names the source and the copy; only the copy changed.
    let out = rec(
        A,
        "s-AAAAA",
        &["C075", "src/a.rs", "src/b.rs", "M", "src/a.rs"],
    );
    assert_eq!(
        parse_log(&out, N)[0].changes,
        vec![t("src/b.rs"), t("src/a.rs")]
    );
}

/// Final review F2: Git puts a newline before the first status token of a record and nowhere
/// else. Only status tokens lose it: a path is kept byte for byte, so a path that starts with
/// a newline, or is one newline, is read as written and the tokens after it stay aligned.
#[test]
fn cp2_fix1_newline_paths_are_kept_byte_for_byte() {
    let out = rec(
        A,
        "c-AAAAA",
        &["A", "\nodd.rs", "M", "plain.rs", "A", "\n", "D", "gone.rs"],
    );
    assert_eq!(
        parse_log(&out, N)[0].changes,
        vec![
            t("\nodd.rs"),
            t("plain.rs"),
            t("\n"),
            FileChange::Delete("gone.rs".into()),
        ]
    );
    // renames and copies: both paths are kept as written
    let mut out = rec(
        B,
        "",
        &[
            "R100", "\n", "\nnew.rs", "C050", "plain.rs", "\n\n", "M", "x.rs",
        ],
    );
    out.extend(rec(C, "c-AAAAA", &["R100", "a.rs", "\n"]));
    let c = parse_log(&out, N);
    assert_eq!(
        c[0].changes,
        vec![
            FileChange::Rename {
                from: "\n".into(),
                to: "\nnew.rs".into()
            },
            t("\n\n"),
            t("x.rs"),
        ]
    );
    assert_eq!(
        c[1].changes,
        vec![FileChange::Rename {
            from: "a.rs".into(),
            to: "\n".into()
        }]
    );
}
