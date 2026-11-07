//! The tests that Windows does not compile, one row per file (FW-XP1 D-1RXEA, Windows plan task 32).
//!
//! A test leaves Windows in two ways: its file begins with `#![cfg(unix)]` (a whole-file gate),
//! or the test itself follows `#[cfg(unix)]`, above or below `#[test]`, or sits in a `mod` that does
//! (an item gate). A Unix gate written any other way (`all(unix, ..)`, `target_family`, `cfg_attr`)
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

/// A `mod name { .. }` block: its attribute lines and the lines it spans.
struct Module {
    attrs: Vec<String>,
    first: usize,
    last: usize,
}

/// A line that gates on Unix in a way this scanner cannot read (anything but the two exact forms).
fn unsupported_gate(attr: &str) -> bool {
    (attr.starts_with("#[cfg(") || attr.starts_with("#[cfg_attr("))
        && attr.contains("unix")
        && attr != "#[cfg(unix)]"
        && attr != "#[cfg(not(unix))]"
}

/// The line of the `}` that closes the block opened on line `from`, skipping braces in strings,
/// characters and comments.
fn block_end(lines: &[&str], from: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut opened = false;
    let mut in_block_comment = false;
    let mut raw_hashes: Option<usize> = None;
    let mut in_str = false;
    for (n, line) in lines.iter().enumerate().skip(from) {
        let c: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < c.len() {
            if in_block_comment {
                if c[i] == '*' && c.get(i + 1) == Some(&'/') {
                    in_block_comment = false;
                    i += 1;
                }
            } else if let Some(h) = raw_hashes {
                if c[i] == '"' && c[i + 1..].iter().take(h).filter(|x| **x == '#').count() == h {
                    raw_hashes = None;
                    i += h;
                }
            } else if in_str {
                if c[i] == '\\' {
                    i += 1;
                } else if c[i] == '"' {
                    in_str = false;
                }
            } else if c[i] == '/' && c.get(i + 1) == Some(&'/') {
                break;
            } else if c[i] == '/' && c.get(i + 1) == Some(&'*') {
                in_block_comment = true;
                i += 1;
            } else if c[i] == 'r'
                && (i == 0 || !(c[i - 1].is_alphanumeric() || c[i - 1] == '_'))
                && {
                    let h = c[i + 1..].iter().take_while(|x| **x == '#').count();
                    c.get(i + 1 + h) == Some(&'"')
                }
            {
                let h = c[i + 1..].iter().take_while(|x| **x == '#').count();
                raw_hashes = Some(h);
                i += 1 + h;
            } else if c[i] == '"' {
                in_str = true;
            } else if c[i] == '\'' {
                // a character literal; a lifetime has no closing quote two or three places on
                if c.get(i + 1) == Some(&'\\') {
                    i += 3;
                    while i < c.len() && c[i] != '\'' {
                        i += 1;
                    }
                } else if c.get(i + 2) == Some(&'\'') {
                    i += 2;
                }
            } else if c[i] == '{' {
                depth += 1;
                opened = true;
            } else if c[i] == '}' {
                depth = depth.saturating_sub(1);
                if opened && depth == 0 {
                    return Some(n);
                }
            }
            i += 1;
        }
    }
    None
}

/// Every `mod name { .. }` block of the file.
fn modules(lines: &[&str]) -> Vec<Module> {
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        let t = t.strip_prefix("pub ").unwrap_or(t);
        if !(t.starts_with("mod ") && t.ends_with('{')) {
            continue;
        }
        if let Some(last) = block_end(lines, i) {
            out.push(Module {
                attrs: attributes_above(lines, i).map(str::to_string).collect(),
                first: i,
                last,
            });
        }
    }
    out
}

/// The attribute lines of the test on line `at`: those above it and those between it and its `fn`.
fn test_attributes(lines: &[&str], at: usize) -> Vec<String> {
    let mut attrs: Vec<String> = attributes_above(lines, at).map(str::to_string).collect();
    for l in &lines[at..] {
        let t = l.trim();
        if t.starts_with("#[") {
            attrs.push(t.to_string());
        } else if t.starts_with("fn ") || t.starts_with("pub ") || t.starts_with("async ") {
            break;
        }
    }
    attrs
}

/// The tests of `files` that leave Windows, as (file, name, gate) in file then line order, and the
/// tests whose Unix gate this scanner cannot read.
fn gated_tests(files: &[(String, String)]) -> (Vec<(String, String, Gate)>, Vec<String>) {
    let mut out = Vec::new();
    let mut bad = Vec::new();
    for (path, text) in files {
        if is_seam_internal(path) {
            continue;
        }
        let lines: Vec<&str> = text.lines().collect();
        let whole = lines.first() == Some(&GATE_LINE);
        let mods = modules(&lines);
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim();
            if !(t.starts_with("#[test") || t.starts_with("#[tokio::test")) {
                continue;
            }
            let Some(name) = fn_name_after(&lines, i) else {
                continue;
            };
            let mut attrs = test_attributes(&lines, i);
            for m in mods.iter().filter(|m| m.first < i && i <= m.last) {
                attrs.extend(m.attrs.iter().cloned());
            }
            if let Some(a) = attrs.iter().find(|a| unsupported_gate(a)) {
                bad.push(format!(
                    "{path}::{name}: an unsupported Unix gate `{a}`; use `#[cfg(unix)]` on the test \
                     or its module, or `#![cfg(unix)]` on the file"
                ));
                continue;
            }
            if whole {
                out.push((path.clone(), name, Gate::File));
            } else if attrs.iter().any(|a| a == "#[cfg(unix)]") {
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

/// The name of the first `fn` at or after line `from`.
fn fn_name_after(lines: &[&str], from: usize) -> Option<String> {
    lines[from..].iter().find_map(|l| {
        let rest = l.trim_start();
        let rest = rest.strip_prefix("pub ").unwrap_or(rest);
        let rest = rest.strip_prefix("async ").unwrap_or(rest);
        let rest = rest.strip_prefix("fn ")?;
        let end = rest.find(|c: char| !(c.is_alphanumeric() || c == '_'))?;
        Some(rest[..end].to_string())
    })
}

/// The attribute and doc lines directly above line `at`, nearest first.
fn attributes_above<'a>(lines: &'a [&'a str], at: usize) -> impl Iterator<Item = &'a str> {
    lines[..at]
        .iter()
        .rev()
        .map(|l| l.trim())
        .take_while(|l| l.starts_with("#[") || l.starts_with("///") || l.starts_with("//"))
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
