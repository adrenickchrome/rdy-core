//! The tests that Windows does not compile, one row per file (FW-XP1 D-1RXEA, Windows plan task 32).
//!
//! A test leaves Windows in two ways: its file begins with `#![cfg(unix)]` (a whole-file gate),
//! or the test itself follows `#[cfg(unix)]`, above or below `#[test]`, or sits in a `mod` that does
//! (an item gate; the module may be `pub(crate)` or have its brace on the next line, and any
//! attribute may span several lines). A Unix gate written any other way (`all(unix, ..)`, `target_family`, `cfg_attr`)
//! is named by [`problems`] as unsupported. Every such test is named here, with
//! the Unix mechanism that cannot exist on Windows and the tests that hold the same safety
//! property on Windows. [`problems`] compares this manifest with the tree and fails on any
//! difference, so a new, renamed or removed exclusion cannot shrink the test population without a
//! reviewer seeing it. The gate's report counts these as OS exclusions, apart from ignored and
//! deep-only tests.
//!
//! Not listed: tests inside the seam files themselves (they test the seam's own calls), and
//! assertions inside a portable test that a `#[cfg(unix)]` statement guards.

use crate::seams::{GATE_LINE, rust_files};
use std::path::Path;

/// How a test leaves Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// The file begins with `#![cfg(unix)]`; every test of the file is excluded.
    File,
    /// Each test follows `#[cfg(unix)]`.
    Item,
}

/// The excluded tests of one file.
#[derive(Debug)]
pub struct Exclusion {
    /// The file, from the workspace root, with `/` separators.
    pub file: &'static str,
    pub gate: Gate,
    /// The exact test names. A whole-file gate lists every test of the file.
    pub tests: &'static [&'static str],
    /// The Unix mechanism that Windows does not have.
    pub mechanism: &'static str,
    /// Tests that exist in the tree and hold the same safety property on Windows.
    pub counterparts: &'static [&'static str],
}

/// Every test that Windows does not compile (closed list).
pub const EXCLUSIONS: &[Exclusion] = &[
    Exclusion {
        file: "crates/rdy-cli/tests/cli_unix.rs",
        gate: Gate::File,
        tests: &[
            "edit_runs_editor_with_args_and_failure_changes_nothing",
            "wx20_failed_edit_keeps_its_recovery_file",
        ],
        mechanism: "The editor is a shell script made executable with a file mode (chmod 0755).",
        counterparts: &[
            "wx20_editor_receives_one_literal_native_path",
            "wx20_editor_can_reopen_the_closed_temp_file",
        ],
    },
    Exclusion {
        file: "crates/rdy-daemon/src/layout.rs",
        gate: Gate::Item,
        tests: &[
            "msw15_a_failed_gitignore_write_leaves_no_gitignore_and_no_temp_file",
            "msw15_a_file_system_without_hard_links_has_no_gitignore_and_still_ensures",
            "msw15_an_existing_gitignore_is_left_untouched_and_no_temp_file_remains",
            "msw15_a_link_at_the_gitignore_name_is_not_written_through",
            "df3_a_short_socket_directory_that_is_not_private_is_refused",
            "df3_a_short_socket_directory_that_cannot_be_prepared_names_it",
            "df3_a_socket_record_is_read_only_when_it_names_this_project",
        ],
        mechanism: "Errno values, symlinks, hard links and modes of the short socket directory and the run directory's .gitignore (the whole tests module is `#[cfg(unix)]`).",
        counterparts: &[
            "wx15_run_directory_reparse_or_acl_violation_is_refused",
            "wx15_pipe_record_is_bounded_and_local",
        ],
    },
    Exclusion {
        file: "crates/rdy-daemon/src/layout.rs",
        gate: Gate::Item,
        tests: &["ps8_project_id_is_the_store_file_id"],
        mechanism: "The project identity is the Unix device and inode of .rdy (the whole tests module is `#[cfg(unix)]`).",
        counterparts: &["wx2_full_file_id_round_trips_without_truncation"],
    },
    Exclusion {
        file: "crates/rdy-daemon/src/layout.rs",
        gate: Gate::Item,
        tests: &["ps8_the_identity_text_is_unchanged"],
        mechanism: "The `dev:ino` text of a Unix project identity (the whole tests module is `#[cfg(unix)]`).",
        counterparts: &["wx14_session_identity_matches_the_settled_schema"],
    },
    Exclusion {
        file: "crates/rdy-daemon/src/socket.rs",
        gate: Gate::Item,
        tests: &["ps7_the_socket_record_is_the_path_bytes_then_the_identity"],
        mechanism: "The socket record holds a Unix socket path and a device and inode identity (the whole tests module is `#[cfg(unix)]`).",
        counterparts: &["wx15_pipe_record_is_bounded_and_local"],
    },
    Exclusion {
        file: "crates/rdy-daemon/src/client.rs",
        gate: Gate::Item,
        tests: &["df3_a_short_socket_dir_that_cannot_be_prepared_names_it"],
        mechanism: "A read-only parent of the short socket directory, made with a mode.",
        counterparts: &["wx15_run_directory_reparse_or_acl_violation_is_refused"],
    },
    Exclusion {
        file: "crates/rdy-daemon/src/site/addr.rs",
        gate: Gate::Item,
        tests: &["ls4_two_spellings_give_one_port"],
        mechanism: "Two spellings of one root through a symbolic link.",
        counterparts: &["wx19_canonical_aliases_choose_the_same_port"],
    },
    Exclusion {
        file: "crates/rdy-daemon/tests/client_unix.rs",
        gate: Gate::File,
        tests: &[
            "start1_a_socket_whose_peer_is_another_uid_is_no_daemon_and_hears_no_hello",
            "start1_a_symlinked_socket_is_not_followed",
            "start1_a_long_project_path_gets_a_short_socket",
        ],
        mechanism: "A Unix socket, its peer's uid and its length limit; a named pipe has none of them.",
        counterparts: &[
            "wx14_server_checks_client_sid_before_hello",
            "wx15_aliases_reach_one_pipe",
        ],
    },
    Exclusion {
        file: "crates/rdy-daemon/tests/signing_transport.rs",
        gate: Gate::Item,
        tests: &["sig5_unix_sigint_cancels_a_request_while_the_callers_agent_waits"],
        mechanism: "SIGINT delivered to the process while the caller's agent waits.",
        counterparts: &["wx18_cancel_interrupts_a_blocked_agent_read"],
    },
    Exclusion {
        file: "crates/rdy-daemon/tests/socket.rs",
        gate: Gate::Item,
        tests: &[
            "sock1_an_existing_run_dir_is_tightened_and_a_file_is_refused",
            "sock1_a_long_project_path_binds_its_short_socket_privately",
            "sock1_a_peer_of_another_uid_is_closed_without_a_byte",
            "sock1_peer_uid_of_a_same_user_connection_is_our_uid",
            "df3_the_daemon_records_its_socket_and_removes_the_record_on_exit",
            "df3_a_socket_that_cannot_be_bound_names_its_path",
        ],
        mechanism: "Socket file modes, the peer uid and the short socket under /tmp.",
        counterparts: &[
            "wx14_server_checks_client_sid_before_hello",
            "wx15_run_directory_reparse_or_acl_violation_is_refused",
            "wx13_the_hello_bound_holds_over_the_endpoint",
        ],
    },
    Exclusion {
        file: "crates/rdy-daemon/tests/spawn.rs",
        gate: Gate::Item,
        tests: &[
            "spawn1_a_spawned_child_writes_to_the_log_in_its_own_process_group",
            "spawn1_detach_makes_a_new_session_in_a_child",
            "spawn1_a_spawned_serve_child_detaches_into_a_session_of_its_own",
            "spawn1_the_serve_command_for_a_long_project_path_prepares_the_short_socket_dir",
            "spawn1_a_spawned_child_is_reaped_and_reports_its_code_after_the_spawned_is_dropped",
        ],
        mechanism: "Process groups and sessions (setsid), and the short socket directory.",
        counterparts: &[
            "wx16_detach_does_not_inherit_store_or_console_handles",
            "wx17_native_auto_start_reaches_ready_and_a_stop_leaves_nothing",
        ],
    },
    Exclusion {
        file: "crates/rdy-daemon/tests/start_handoff.rs",
        gate: Gate::File,
        tests: &[
            "wx8a_child_serves",
            "wx8a_auto_start_under_parent_lock_reaches_ready",
            "wx8a_racing_start_cannot_steal_handoff",
            "wx8a_failed_handoff_releases_only_its_own_lock",
            "wx8a_explicit_upgrade_borrows_guard_without_reacquiring",
            "wx8a_automatic_upgrade_borrows_guard_without_reacquiring",
            "wx8a_an_upgrade_refuses_another_projects_guard",
            "wx8a_shutdown_without_usable_pid_waits_for_pipe_and_store",
        ],
        mechanism: "A shell script stands in for the rdy binary and a Unix socket for the old daemon.",
        counterparts: &[
            "wx17_native_racing_starts_share_one_daemon",
            "wx25b_automatic_upgrade_borrows_the_guard",
            "wx25b_explicit_upgrade_borrows_the_guard",
            "wx25b_no_pid_shutdown_waits_for_store_release",
        ],
    },
    Exclusion {
        file: "crates/rdy-engine/src/wc/files.rs",
        gate: Gate::Item,
        tests: &["ps3_identity_is_none_behind_a_link"],
        mechanism: "A symbolic link made with std::os::unix in a checkout.",
        counterparts: &["wx29b_a_copied_tree_keeps_links_as_links"],
    },
    Exclusion {
        file: "crates/rdy-engine/tests/config_edit.rs",
        gate: Gate::Item,
        tests: &[
            "hsa2_new_directories_are_0700_or_not_made_at_all",
            "hsa2_new_directories_child",
            "hsa2_a_group_writable_folder_takes_settings_not_secrets_under_umask_002_and_022",
            "hsa2_group_writable_folder_child",
            "hsa2_config_tests_pass_under_umask_002_and_022",
        ],
        mechanism: "Directory modes and the umask of a child process started through sh.",
        counterparts: &[
            "wx9_config_replace_preserves_private_acl",
            "wx12d_shared_folders_take_settings_not_secrets",
        ],
    },
    Exclusion {
        file: "crates/rdy-engine/tests/hub_remotes.rs",
        gate: Gate::Item,
        tests: &["hsa6_remote_tests_pass_under_umask_002_and_022"],
        mechanism: "The umask of a child process started through sh.",
        counterparts: &["wx12d_shared_folders_take_settings_not_secrets"],
    },
    Exclusion {
        file: "crates/rdy-engine/tests/kungfu_counts.rs",
        gate: Gate::Item,
        tests: &["kfu2_private_paths_and_replaced_projects_are_safe"],
        mechanism: "Permission modes and symbolic links.",
        counterparts: &["wx4_private_objects_have_only_the_account_ace"],
    },
    Exclusion {
        file: "crates/rdy-qa/src/seams.rs",
        gate: Gate::Item,
        tests: &[
            "ps11_an_unreadable_source_file_is_named",
            "ps11_an_unreadable_directory_is_named",
            "ps11_a_dangling_link_in_a_scanned_directory_is_named",
            "ps11_a_dangling_crate_or_optional_path_link_is_named",
            "ps11_a_directory_cycle_is_named_and_the_scan_ends",
        ],
        mechanism: "A mode 000 file or directory, and symbolic links made with std::os::unix.",
        counterparts: &["ps11_the_real_tree_has_no_unix_code_outside_the_seams"],
    },
    Exclusion {
        file: "crates/rdy-qa/tests/cli/cli_daemon_unix.rs",
        gate: Gate::File,
        tests: &["cli2_sigint_and_sigterm_stop_a_foreground_serve_cleanly"],
        mechanism: "SIGINT and SIGTERM sent to a foreground serve.",
        counterparts: &["wx25b_the_stop_event_stops_a_foreground_serve_cleanly"],
    },
    Exclusion {
        file: "crates/rdy-qa/tests/cli/cli_hub_keys.rs",
        gate: Gate::Item,
        tests: &["hsa9_the_containment_check_fails_on_each_output_channel"],
        mechanism: "The stub MCP server is a POSIX shell script run with sh -c.",
        counterparts: &["hsa9_signin_remote_bind_retire_revoke_is_one_client_flow"],
    },
    Exclusion {
        file: "crates/rdy-qa/tests/code/code_brief.rs",
        gate: Gate::Item,
        tests: &["cbrief1_a_new_file_in_a_new_folder_under_a_symlinked_prefix_is_inside_the_repo"],
        mechanism: "A symbolic link as a prefix of the new path.",
        counterparts: &["wx27_code_paths_with_drive_and_backslash_work_through_tools"],
    },
    Exclusion {
        file: "crates/rdy-qa/tests/daemon/daemon_short_socket_unix.rs",
        gate: Gate::File,
        tests: &[
            "df3_long_socket_path",
            "df3_a_short_project_keeps_its_socket_in_rdy_run",
            "df3_every_spelling_of_a_long_project_path_reaches_one_socket",
            "df3_two_spellings_of_one_project_reach_one_daemon",
        ],
        mechanism: "The length limit of a socket path, the short directory under /tmp, symbolic links and macOS firmlinks.",
        counterparts: &[
            "wx25b_every_spelling_of_a_project_reaches_one_pipe",
            "wx25b_a_project_has_a_pipe_and_no_socket_file",
        ],
    },
    Exclusion {
        file: "crates/rdy-qa/tests/daemon/daemon_site_profile_listeners.rs",
        gate: Gate::Item,
        tests: &["plb4_a_child_held_before_exec_keeps_no_listener_of_a_stopped_daemon"],
        mechanism: "A child held before exec by fork; Windows creates children suspended instead.",
        counterparts: &["wx16_detach_does_not_inherit_store_or_console_handles"],
    },
    Exclusion {
        file: "crates/rdy-qa/tests/daemon/daemon_stop_unix.rs",
        gate: Gate::File,
        tests: &[
            "dl11_terminate_never_signals_init_or_a_group",
            "dl11_terminate_says_when_the_process_is_gone",
        ],
        mechanism: "SIGTERM sent by pid; Windows never ends a process by its pid alone.",
        counterparts: &["wx16_native_a_forged_pid_file_never_ends_a_process"],
    },
    Exclusion {
        file: "crates/rdy-qa/tests/release/release_install_unix.rs",
        gate: Gate::File,
        tests: &["pre3_render_refuses_non_utf8_bytes"],
        mechanism: "A token that is not UTF-8, made with OsStrExt.",
        counterparts: &["wx36a_render_refuses_unpaired_surrogate_tokens"],
    },
    Exclusion {
        file: "crates/rdy-qa/tests/release/release_notices_unix.rs",
        gate: Gate::File,
        tests: &["plx7_source_tool_stages_crate_files_with_mode_0644"],
        mechanism: "Permission bits (mode 0644) of staged files.",
        counterparts: &["wx35b_windows_notices_match_the_locked_target_graph"],
    },
    Exclusion {
        file: "crates/rdy-qa/tests/release/release_package_unix.rs",
        gate: Gate::File,
        tests: &["pre1_package_stage_stays_private_under_any_umask"],
        mechanism: "Permission bits under a umask, and synchronisation on named pipes.",
        counterparts: &["wx35a_native_package_run_takes_the_native_branch"],
    },
    Exclusion {
        file: "crates/rdy-qa/tests/wc/wc_unix.rs",
        gate: Gate::File,
        tests: &["wcly1_a_fifo_and_a_name_of_raw_bytes_are_untracked"],
        mechanism: "A FIFO in a directory and a file name of raw bytes.",
        counterparts: &["wx29b_a_reparse_point_of_no_link_is_untracked_and_kept"],
    },
    Exclusion {
        file: "crates/rdy-store/tests/fileid.rs",
        gate: Gate::Item,
        tests: &["ps2_open_locked_takes_its_own_lock_after_the_store_dir_is_renamed"],
        mechanism: "The Unix held-lock contract of a renamed store directory; Windows closes the loading store under the start lock instead.",
        counterparts: &["wx8b_start_lock_covers_close_rename_reopen"],
    },
    Exclusion {
        file: "crates/rdy-store/tests/private_write.rs",
        gate: Gate::Item,
        tests: &[
            "hsa2_private_bytes_are_private_before_write",
            "hsa2_settings_writes_keep_the_target_mode_and_new_files_are_0600",
            "hsa2_pio_privacy_helpers_judge_files_by_owner_kind_and_mode",
            "hsa2_pio_directory_calls_never_follow_a_link_at_the_entry",
            "hsa2_pio_the_umask_check_keeps_nothing_and_follows_no_link",
            "hsa2_a_replacement_in_a_directory_handle_ignores_later_path_changes",
        ],
        mechanism: "Unix modes, the umask and owners of private files and directories.",
        counterparts: &[
            "wx4_private_objects_have_only_the_account_ace",
            "wx4_private_temp_is_private_at_creation",
            "wx5a_parent_swap_cannot_redirect_open",
        ],
    },
];

/// The number of excluded tests, for the gate's report.
pub fn count() -> usize {
    EXCLUSIONS.iter().map(|e| e.tests.len()).sum()
}

/// Every excluded test as (file, test name), in manifest order, for the gate's report.
pub fn names() -> Vec<(&'static str, &'static str)> {
    EXCLUSIONS
        .iter()
        .flat_map(|e| e.tests.iter().map(|t| (e.file, *t)))
        .collect()
}

/// Every difference between [`EXCLUSIONS`] and the tree under `root` (the workspace); empty when
/// they agree.
pub fn problems(root: &Path) -> Vec<String> {
    let (files, mut found) = rust_files(root);
    let mut named: Vec<String> = found.drain(..).map(|(_, _, m)| m).collect();
    named.extend(problems_in(&files, EXCLUSIONS));
    named
}

/// A file read for scanning: `code` is the text with comments and the insides of string and
/// character literals blanked (same length, same line breaks), so braces, brackets and keywords
/// in it are real. `orig` keeps the text for reading an attribute.
struct Source {
    orig: Vec<char>,
    code: Vec<char>,
}

impl Source {
    fn new(text: &str) -> Source {
        let orig: Vec<char> = text.chars().collect();
        let code = mask(&orig);
        Source { orig, code }
    }
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `src` with comments and the insides of literals replaced by spaces (line breaks kept).
fn mask(src: &[char]) -> Vec<char> {
    let n = src.len();
    let mut out = src.to_vec();
    let blank = |out: &mut Vec<char>, from: usize, to: usize| {
        for c in &mut out[from..to] {
            if *c != '\n' {
                *c = ' ';
            }
        }
    };
    let mut i = 0;
    while i < n {
        let next = src.get(i + 1).copied();
        if src[i] == '/' && next == Some('/') {
            let mut j = i;
            while j < n && src[j] != '\n' {
                j += 1;
            }
            blank(&mut out, i, j);
            i = j;
        } else if src[i] == '/' && next == Some('*') {
            let (mut depth, mut j) = (1, i + 2);
            while j < n && depth > 0 {
                if src[j] == '/' && src.get(j + 1) == Some(&'*') {
                    depth += 1;
                    j += 2;
                } else if src[j] == '*' && src.get(j + 1) == Some(&'/') {
                    depth -= 1;
                    j += 2;
                } else {
                    j += 1;
                }
            }
            blank(&mut out, i, j);
            i = j;
        } else if src[i] == 'r'
            && (i == 0
                || !is_ident(src[i - 1])
                || (src[i - 1] == 'b' && (i == 1 || !is_ident(src[i - 2]))))
            && {
                let h = src[i + 1..].iter().take_while(|x| **x == '#').count();
                src.get(i + 1 + h) == Some(&'"')
            }
        {
            let h = src[i + 1..].iter().take_while(|x| **x == '#').count();
            let start = i + 2 + h;
            let mut j = start;
            while j < n
                && !(src[j] == '"'
                    && src[j + 1..].iter().take(h).filter(|x| **x == '#').count() == h)
            {
                j += 1;
            }
            blank(&mut out, start, j.min(n));
            i = (j + 1 + h).min(n);
        } else if src[i] == '"' {
            let mut j = i + 1;
            while j < n && src[j] != '"' {
                j += if src[j] == '\\' { 2 } else { 1 };
            }
            blank(&mut out, i + 1, j.min(n));
            i = j + 1;
        } else if src[i] == '\'' {
            // a character literal; a lifetime has no closing quote two or three places on
            if next == Some('\\') {
                let mut j = i + 3;
                while j < n && src[j] != '\'' {
                    j += 1;
                }
                blank(&mut out, i + 1, j.min(n));
                i = j + 1;
            } else if src.get(i + 2) == Some(&'\'') {
                blank(&mut out, i + 1, i + 2);
                i += 3;
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    out
}

/// One attribute group `#[..]` or `#![..]`, whole, however many lines it spans.
struct Attr {
    start: usize,
    end: usize,
    inner: bool,
    /// The attribute as written, without white space.
    text: String,
}

/// Every attribute of the file, in order.
fn attributes(src: &Source) -> Vec<Attr> {
    let c = &src.code;
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        if c[i] == '#' {
            let mut j = i + 1;
            while c.get(j).is_some_and(|x| x.is_whitespace()) {
                j += 1;
            }
            let inner = c.get(j) == Some(&'!');
            if inner {
                j += 1;
                while c.get(j).is_some_and(|x| x.is_whitespace()) {
                    j += 1;
                }
            }
            if c.get(j) == Some(&'[') {
                let mut depth = 0usize;
                let mut k = j;
                while k < c.len() {
                    if c[k] == '[' {
                        depth += 1;
                    } else if c[k] == ']' {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    k += 1;
                }
                if k < c.len() {
                    out.push(Attr {
                        start: i,
                        end: k + 1,
                        inner,
                        text: src.orig[i..=k]
                            .iter()
                            .filter(|x| !x.is_whitespace())
                            .collect(),
                    });
                    i = k + 1;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

/// A `mod name { .. }` block: the attributes of its declaration and the span of its braces.
struct Module {
    attrs: Vec<String>,
    open: usize,
    close: usize,
}

/// The index of the closer that matches the opener at `open`.
fn matching(code: &[char], open: usize) -> Option<usize> {
    let (o, c) = (code[open], if code[open] == '{' { '}' } else { ')' });
    let mut depth = 0usize;
    for (k, ch) in code.iter().enumerate().skip(open) {
        if *ch == o {
            depth += 1;
        } else if *ch == c {
            depth -= 1;
            if depth == 0 {
                return Some(k);
            }
        }
    }
    None
}

fn skip_space(code: &[char], mut i: usize) -> usize {
    while code.get(i).is_some_and(|x| x.is_whitespace()) {
        i += 1;
    }
    i
}

/// The start of a visibility (`pub`, `pub(crate)`, `pub(in a::b)`) that ends just before `at`, or
/// `at` when there is none.
fn visibility_start(code: &[char], at: usize) -> usize {
    let mut k = at;
    while k > 0 && code[k - 1].is_whitespace() {
        k -= 1;
    }
    if k > 0 && code[k - 1] == ')' {
        let mut depth = 0usize;
        let mut m = k;
        while m > 0 {
            m -= 1;
            if code[m] == ')' {
                depth += 1;
            } else if code[m] == '(' {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
        }
        k = m;
        while k > 0 && code[k - 1].is_whitespace() {
            k -= 1;
        }
    }
    if k >= 3 && code[k - 3..k] == ['p', 'u', 'b'] && (k == 3 || !is_ident(code[k - 4])) {
        k - 3
    } else {
        at
    }
}

/// The outer attributes directly above the item that starts at `head` (nothing but white space and
/// comments between them), nearest first.
fn attrs_above<'a>(attrs: &'a [Attr], code: &[char], head: usize) -> Vec<&'a Attr> {
    let mut out = Vec::new();
    let mut cursor = head;
    while let Some(a) = attrs.iter().find(|a| {
        !a.inner && a.end <= cursor && code[a.end..cursor].iter().all(|c| c.is_whitespace())
    }) {
        out.push(a);
        cursor = a.start;
    }
    out
}

/// Every `mod name { .. }` block of the file, however it is spelt (`pub(crate)`, a brace on the
/// next line). An out-of-line `mod name;` has no block here.
fn modules(src: &Source, attrs: &[Attr]) -> Vec<Module> {
    let c = &src.code;
    let mut out = Vec::new();
    for i in 0..c.len().saturating_sub(3) {
        if c[i..i + 3] != ['m', 'o', 'd']
            || (i > 0 && is_ident(c[i - 1]))
            || !c[i + 3].is_whitespace()
        {
            continue;
        }
        let mut j = skip_space(c, i + 3);
        let name = j;
        while c.get(j).is_some_and(|x| is_ident(*x)) {
            j += 1;
        }
        if j == name {
            continue;
        }
        let j = skip_space(c, j);
        if c.get(j) != Some(&'{') {
            continue;
        }
        if let Some(close) = matching(c, j) {
            out.push(Module {
                attrs: attrs_above(attrs, c, visibility_start(c, i))
                    .into_iter()
                    .map(|a| a.text.clone())
                    .collect(),
                open: j,
                close,
            });
        }
    }
    out
}

/// The name of the `fn` that the item head at `i` declares (after `pub`, `async`, `unsafe`, ..).
fn fn_name_at(code: &[char], mut i: usize) -> Option<String> {
    loop {
        i = skip_space(code, i);
        if code.get(i) == Some(&'"') {
            // the ABI of `extern "C"`
            i += 1;
            while code.get(i).is_some_and(|x| *x != '"') {
                i += 1;
            }
            i += 1;
            continue;
        }
        let start = i;
        while code.get(i).is_some_and(|x| is_ident(*x)) {
            i += 1;
        }
        let word: String = code[start..i].iter().collect();
        match word.as_str() {
            "fn" => {
                let s = skip_space(code, i);
                let mut e = s;
                while code.get(e).is_some_and(|x| is_ident(*x)) {
                    e += 1;
                }
                return (e > s).then(|| code[s..e].iter().collect());
            }
            "pub" => {
                let k = skip_space(code, i);
                if code.get(k) == Some(&'(') {
                    i = matching(code, k)? + 1;
                }
            }
            "async" | "unsafe" | "const" | "extern" => {}
            _ => return None,
        }
    }
}

/// A line that gates on Unix in a way this scanner cannot read (anything but the two exact forms).
fn unsupported_gate(attr: &str) -> bool {
    (attr.starts_with("#[cfg(") || attr.starts_with("#[cfg_attr("))
        && attr.contains("unix")
        && attr != "#[cfg(unix)]"
        && attr != "#[cfg(not(unix))]"
}

/// The tests of `files` that leave Windows, as (file, name, gate) in file then source order, and the
/// gates this scanner cannot read.
fn gated_tests(files: &[(String, String)]) -> (Vec<(String, String, Gate)>, Vec<String>) {
    let mut out = Vec::new();
    let mut bad = Vec::new();
    for (path, text) in files {
        if is_seam_internal(path) {
            continue;
        }
        let src = Source::new(text);
        let attrs = attributes(&src);
        let mods = modules(&src, &attrs);
        let whole = text.lines().next() == Some(GATE_LINE);
        for a in attrs.iter().filter(|a| a.inner) {
            let as_outer = a.text.replacen("#!", "#", 1);
            let first_line = a.start == 0 && whole;
            if !first_line && (as_outer == "#[cfg(unix)]" || unsupported_gate(&as_outer)) {
                bad.push(format!(
                    "{path}: an inner Unix gate `{}` that is not the first line of the file; put \
                     `{GATE_LINE}` on line one or gate the test with `#[cfg(unix)]`",
                    a.text
                ));
            }
        }
        for (k, t) in attrs.iter().enumerate() {
            if t.inner || !(t.text.starts_with("#[test") || t.text.starts_with("#[tokio::test")) {
                continue;
            }
            // the attributes of the test: above `t`, and those that follow it before the `fn`
            let mut own: Vec<&Attr> = attrs_above(&attrs, &src.code, t.start);
            let mut cursor = t.end;
            while let Some(a) = attrs[k + 1..].iter().find(|a| {
                !a.inner
                    && a.start >= cursor
                    && src.code[cursor..a.start].iter().all(|c| c.is_whitespace())
            }) {
                own.push(a);
                cursor = a.end;
            }
            let Some(name) = fn_name_at(&src.code, cursor) else {
                continue;
            };
            let mut gates: Vec<&str> = own.iter().map(|a| a.text.as_str()).collect();
            for m in mods
                .iter()
                .filter(|m| m.open < t.start && t.start < m.close)
            {
                gates.extend(m.attrs.iter().map(String::as_str));
            }
            if let Some(a) = gates.iter().find(|a| unsupported_gate(a)) {
                bad.push(format!(
                    "{path}::{name}: an unsupported Unix gate `{a}`; use `#[cfg(unix)]` on the test \
                     or its module, or `#![cfg(unix)]` on the file"
                ));
                continue;
            }
            if whole {
                out.push((path.clone(), name, Gate::File));
            } else if gates.contains(&"#[cfg(unix)]") {
                out.push((path.clone(), name, Gate::Item));
            }
        }
    }
    (out, bad)
}

/// The product seams' own unit tests test the seam's calls and are not part of the population.
fn is_seam_internal(path: &str) -> bool {
    path.starts_with("crates/rdy-daemon/src/sys/")
        || path.starts_with("crates/rdy-store/src/pio")
        || path.starts_with("crates/rdy-store/src/fileid")
        || path.starts_with("crates/rdy-qa/src/platform")
}

/// [`problems`] over scanned `files` and a given manifest.
fn problems_in(files: &[(String, String)], manifest: &[Exclusion]) -> Vec<String> {
    let mut out = Vec::new();
    let (found, unreadable) = gated_tests(files);
    out.extend(unreadable);
    let mut listed: Vec<(&str, &str, Gate)> = Vec::new();
    for e in manifest {
        if e.mechanism.trim().is_empty() {
            out.push(format!(
                "{}: an exclusion with no Unix mechanism in {MANIFEST}",
                e.file
            ));
        }
        if e.counterparts.is_empty() {
            out.push(format!(
                "{}: an exclusion with no Windows counterpart in {MANIFEST}",
                e.file
            ));
        }
        for c in e.counterparts {
            let needle = format!("fn {c}(");
            if !files.iter().any(|(_, text)| text.contains(&needle)) {
                out.push(format!(
                    "{}: the Windows counterpart {c} is not a test in the tree",
                    e.file
                ));
            }
        }
        for t in e.tests {
            if listed.iter().any(|(f, n, _)| *f == e.file && n == t) {
                out.push(format!("{}::{t}: is listed twice in {MANIFEST}", e.file));
            }
            listed.push((e.file, t, e.gate));
        }
    }
    for (file, name, gate) in &found {
        if !listed
            .iter()
            .any(|(f, n, g)| f == file && n == name && g == gate)
        {
            out.push(format!(
                "{file}::{name}: does not compile on Windows ({}) but {MANIFEST} does not list it \
                 with that gate",
                match gate {
                    Gate::File => GATE_LINE,
                    Gate::Item => "#[cfg(unix)]",
                }
            ));
        }
    }
    for (file, name, gate) in &listed {
        if !found
            .iter()
            .any(|(f, n, g)| f == file && n == name && g == gate)
        {
            out.push(format!(
                "{file}::{name}: is listed in {MANIFEST} but the file has no such test behind the \
                 {} gate",
                match gate {
                    Gate::File => "whole-file",
                    Gate::Item => "item",
                }
            ));
        }
    }
    out.sort();
    out
}

const MANIFEST: &str = "crates/rdy-qa/src/platform/exclusions.rs";

#[cfg(test)]
mod tests {
    use super::*;

    const ROW: Exclusion = Exclusion {
        file: "crates/rdy-daemon/tests/x_unix.rs",
        gate: Gate::File,
        tests: &["a_test"],
        mechanism: "a socket",
        counterparts: &["a_counterpart"],
    };

    fn files(extra: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut v = vec![(
            "crates/rdy-daemon/tests/windows_x.rs".to_string(),
            "#[test]\nfn a_counterpart() {}\n".to_string(),
        )];
        v.extend(extra.iter().map(|(p, t)| (p.to_string(), t.to_string())));
        v
    }

    const GATED: &str = "#![cfg(unix)]\n// Unix only: a socket.\n#[test]\nfn a_test() {}\n";

    #[test]
    fn wx32_gated_test_requires_a_mechanism_and_counterpart() {
        let f = files(&[("crates/rdy-daemon/tests/x_unix.rs", GATED)]);
        // the listed gate, with a mechanism and a counterpart that exists, is clean
        assert_eq!(problems_in(&f, &[ROW]), Vec::<String>::new());
        // a gated test nobody listed is named
        assert_eq!(
            problems_in(&f, &[]),
            vec![format!(
                "crates/rdy-daemon/tests/x_unix.rs::a_test: does not compile on Windows \
                 (#![cfg(unix)]) but {MANIFEST} does not list it with that gate"
            )]
        );
        // an item gate is found as well, and a wrong gate kind is both unlisted and stale
        let item = files(&[(
            "crates/rdy-daemon/tests/y.rs",
            "#[cfg(unix)]\n#[test]\nfn b_test() {}\n/// doc\n#[cfg(unix)]\n#[tokio::test]\nasync fn c_test() {}\n#[test]\nfn plain() {}\n",
        )]);
        let got = problems_in(&item, &[]);
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got[0].starts_with("crates/rdy-daemon/tests/y.rs::b_test:"));
        assert!(got[1].starts_with("crates/rdy-daemon/tests/y.rs::c_test:"));
        let wrong = Exclusion {
            gate: Gate::Item,
            ..ROW
        };
        assert_eq!(problems_in(&f, &[wrong]).len(), 2);
        // no mechanism, no counterpart, and a counterpart that is no test are each named
        let bare = Exclusion {
            mechanism: " ",
            counterparts: &[],
            ..ROW
        };
        assert_eq!(
            problems_in(&f, &[bare]),
            vec![
                format!(
                    "crates/rdy-daemon/tests/x_unix.rs: an exclusion with no Unix mechanism in {MANIFEST}"
                ),
                format!(
                    "crates/rdy-daemon/tests/x_unix.rs: an exclusion with no Windows counterpart in {MANIFEST}"
                ),
            ]
        );
        let ghost = Exclusion {
            counterparts: &["not_a_test"],
            ..ROW
        };
        assert_eq!(
            problems_in(&f, &[ghost]),
            vec!["crates/rdy-daemon/tests/x_unix.rs: the Windows counterpart not_a_test is not a test in the tree".to_string()]
        );
    }

    #[test]
    fn wx32_module_gates_and_attribute_order_are_accounted_for() {
        // a test inside a `#[cfg(unix)]` module is excluded, whatever its own attributes
        let module = files(&[(
            "crates/rdy-daemon/src/m.rs",
            "fn plain() {}\n#[cfg(test)]\n#[cfg(unix)]\nmod tests {\n    #[test]\n    fn in_mod() { let _ = \"}\"; }\n    #[test]\n    fn in_mod_two() {}\n}\n#[test]\nfn after_mod() {}\n",
        )]);
        let got = problems_in(&module, &[]);
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(
            got[0].starts_with("crates/rdy-daemon/src/m.rs::in_mod:"),
            "{got:?}"
        );
        assert!(
            got[1].starts_with("crates/rdy-daemon/src/m.rs::in_mod_two:"),
            "{got:?}"
        );
        // a gate written below `#[test]` counts
        let below = files(&[(
            "crates/rdy-daemon/tests/y.rs",
            "#[test]\n#[cfg(unix)]\nfn low() {}\n",
        )]);
        let got = problems_in(&below, &[]);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(
            got[0].starts_with("crates/rdy-daemon/tests/y.rs::low:"),
            "{got:?}"
        );
        // a gate arrangement the scanner cannot read is refused by name
        for odd in [
            "#[cfg(all(unix, test))]\n#[test]\nfn odd() {}\n",
            "#[test]\n#[cfg_attr(unix, ignore)]\nfn odd() {}\n",
            "#[cfg(any(unix, windows))]\nmod tests {\n#[test]\nfn odd() {}\n}\n",
        ] {
            let got = problems_in(&files(&[("crates/rdy-daemon/tests/z.rs", odd)]), &[]);
            assert_eq!(got.len(), 1, "{odd}: {got:?}");
            assert!(got[0].contains("unsupported Unix gate"), "{got:?}");
        }
        // `not(unix)` and a plain cfg(test) are not Unix gates
        let fine = files(&[(
            "crates/rdy-daemon/tests/w.rs",
            "#[cfg(test)]\nmod t {\n#[cfg(not(unix))]\n#[test]\nfn ok() {}\n}\n",
        )]);
        assert_eq!(problems_in(&fine, &[]), Vec::<String>::new());
    }

    #[test]
    fn wx32_restricted_visibility_modules_and_multiline_gates_are_accounted_for() {
        // (source, the tests it must name as excluded)
        let named: &[(&str, &[&str])] = &[
            // restricted-visibility modules
            (
                "#[cfg(unix)]\npub(crate) mod t {\n    #[test]\n    fn a() {}\n}\n",
                &["a"],
            ),
            (
                "#[cfg(unix)]\npub(super) mod t {\n    #[test]\n    fn a() {}\n}\n",
                &["a"],
            ),
            (
                "#[cfg(unix)]\npub ( in crate::x ) mod t {\n    #[test]\n    fn a() {}\n}\n",
                &["a"],
            ),
            // the brace on the next line
            (
                "#[cfg(unix)]\nmod t\n{\n    #[test]\n    fn a() {}\n}\n",
                &["a"],
            ),
            // a multiline gate above and below `#[test]`
            ("#[cfg(\n    unix\n)]\n#[test]\nfn a() {}\n", &["a"]),
            ("#[test]\n#[cfg(\n    unix,\n)]\nfn a() {}\n", &["a"]),
            ("#[test]\n#[cfg(\n    unix\n)]\nfn a() {}\n", &["a"]),
            // a multiline gate on a module
            (
                "#[cfg(\n    unix\n)]\npub(crate) mod t {\n    #[test]\n    fn a() {}\n}\n",
                &["a"],
            ),
            // two attributes on one line
            ("#[cfg(unix)] #[test] fn a() {}\n", &["a"]),
            // `not(unix)` over several lines is not a gate
            ("#[cfg(\n    not(unix)\n)]\n#[test]\nfn a() {}\n", &[]),
            // a gate that sits in a string or a comment is not a gate
            (
                "#[test]\nfn a() { let _ = \"#[cfg(unix)]\"; /* #[cfg(unix)] */ }\n#[test]\nfn b() {}\n",
                &[],
            ),
        ];
        for (i, (src, want)) in named.iter().enumerate() {
            let got = problems_in(&files(&[("crates/rdy-daemon/tests/q.rs", src)]), &[]);
            assert_eq!(got.len(), want.len(), "case {i} {src:?}: {got:?}");
            for (g, w) in got.iter().zip(*want) {
                assert!(
                    g.starts_with(&format!("crates/rdy-daemon/tests/q.rs::{w}:")),
                    "case {i}: {got:?}"
                );
            }
        }
        // a multiline gate the scanner cannot read is refused by name, on a test or a module
        for odd in [
            "#[cfg(all(\n    unix,\n    test\n))]\n#[test]\nfn odd() {}\n",
            "#[test]\n#[cfg(all(\n    unix,\n    test\n))]\nfn odd() {}\n",
            "#[cfg(any(\n    unix,\n    windows\n))]\npub(crate) mod t {\n#[test]\nfn odd() {}\n}\n",
            "#[cfg(target_family = \"unix\")]\npub(crate) mod t {\n#[test]\nfn odd() {}\n}\n",
        ] {
            let got = problems_in(&files(&[("crates/rdy-daemon/tests/z.rs", odd)]), &[]);
            assert_eq!(got.len(), 1, "{odd}: {got:?}");
            assert!(got[0].contains("unsupported Unix gate"), "{got:?}");
        }
        // an inner gate that is not line one is no whole-file gate: named, not skipped
        let got = problems_in(
            &files(&[(
                "crates/rdy-daemon/tests/z.rs",
                "// lead\n#![cfg(unix)]\n#[test]\nfn odd() {}\n",
            )]),
            &[],
        );
        assert!(got.iter().any(|g| g.contains("inner Unix gate")), "{got:?}");
    }

    #[test]
    fn wx32_a_renamed_or_removed_exclusion_is_named() {
        let renamed = files(&[(
            "crates/rdy-daemon/tests/x_unix.rs",
            "#![cfg(unix)]\n// Unix only: a socket.\n#[test]\nfn a_test_renamed() {}\n",
        )]);
        let got = problems_in(&renamed, &[ROW]);
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got[0].contains("a_test: is listed in"), "{got:?}");
        assert!(
            got[1].contains("a_test_renamed: does not compile"),
            "{got:?}"
        );
        // a file that lost its gate: the test now runs on Windows, and the row is stale
        let ungated = files(&[(
            "crates/rdy-daemon/tests/x_unix.rs",
            "#[test]\nfn a_test() {}\n",
        )]);
        let got = problems_in(&ungated, &[ROW]);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(
            got[0].contains("no such test behind the whole-file gate"),
            "{got:?}"
        );
        // a test listed twice
        let twice = problems_in(
            &files(&[("crates/rdy-daemon/tests/x_unix.rs", GATED)]),
            &[ROW, ROW],
        );
        assert_eq!(twice.len(), 1, "{twice:?}");
        assert!(twice[0].contains("is listed twice"), "{twice:?}");
    }

    #[test]
    fn wx32_the_real_exclusions_match_the_tree() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        assert_eq!(problems(&root), Vec::<String>::new());
        assert_eq!(count(), names().len());
    }
}
