use clap::{ArgGroup, Parser, Subcommand};
use conn::{Conn, Env, bound_view, call, find_project, human_session, io_error, path_arg};
use output::{hash_short, print_error};
use rdy_daemon::ErrorReply;
use rdy_engine::wc::state::Place;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod alt_cmd;
mod auth_cmd;
mod brief_cmd;
mod brief_text;
mod config_cmd;
mod conn;
mod coverage_cmd;
mod dump_cmd;
mod edit_cmd;
mod history_text;
mod inbox_cmd;
mod intake_cmd;
mod intent_cmd;
mod land_cmd;
mod output;
mod project_cmd;
mod publication_cmd;
mod read_cmd;
mod redact_cmd;
mod search_cmd;
mod serve_cmd;
mod signing_cmd;
mod site_cmd;
mod tension_cmd;
mod view_cmd;
mod wc;

#[derive(Parser)]
#[command(
    name = "rdy",
    version,
    about = "Version control for agent-aided design"
)]
struct Cli {
    /// Print machine-readable JSON.
    #[arg(long, global = true)]
    json: bool,
    /// Project directory (default: nearest ancestor containing .rdy/).
    #[arg(long, global = true)]
    project: Option<PathBuf>,
    /// Open the store in this process instead of going through the project's daemon (also on
    /// when RDY_NO_DAEMON=1).
    #[arg(long, global = true)]
    no_daemon: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    Init(project_cmd::InitArgs),
    Status,
    Show(read_cmd::ShowArgs),
    Log(read_cmd::LogArgs),
    Impact(read_cmd::ImpactArgs),
    /// Brief on a file, a directory or a block of the working copy (and record the briefing for the next commit), or on a file in a linked code repo with --code.
    #[command(group(ArgGroup::new("what").required(true).multiple(false).args(["target", "code"])))]
    Brief(brief_cmd::BriefArgs),
    /// Find text in blocks: a literal (or, with --regex, a pattern) in every block's body and string fields.
    Search(search_cmd::SearchArgs),
    /// List the suspects in a scope (default canon), as rdy_suspects does.
    Suspects(read_cmd::SuspectsArgs),
    Propose(land_cmd::ProposeArgs),
    /// Request content removal: report what removing the targets would erase (--dry-run), or
    /// record the request in a view. Recording erases nothing: propose the view, and a person
    /// approves it (rdy approve, or the card relayed). The chat path is rdy_redact.
    Redact(redact_cmd::RedactArgs),
    /// Approve a proposal version: P-<id>@<n>, P-<id> (its latest version) or a version hash.
    Approve(land_cmd::ApproveArgs),
    /// Create, fork, include, exclude, compare, list, show, close and hand over views.
    View {
        #[command(subcommand)]
        cmd: view_cmd::ViewCmd,
    },
    /// Create, change, list, compare and abandon alternative sets: views that answer one question.
    Alt {
        #[command(subcommand)]
        cmd: alt_cmd::AltCmd,
    },
    /// Post, renew, withdraw and list intents: advisory notes on what you mean to change.
    Intent {
        #[command(subcommand)]
        cmd: intent_cmd::IntentCmd,
    },
    /// List tensions: disagreements between in-flight changes, with canon, or inside canon.
    Tensions(tension_cmd::TensionsArgs),
    /// Dismiss a tension with a reason, dismiss or restore a passing entity mention
    /// (--mention <block> <entity> with --dismiss or --restore), or accept the reviewers' advice
    /// to block on a proposal version's semantic tensions (--accept P-x@n). A tension is
    /// otherwise resolved by a change.
    Resolve(tension_cmd::ResolveArgs),
    /// Print your inbox: the events and tension moves since your cursor. The first call starts it; --ack moves your cursor past the entries shown. The chat path is rdy_inbox.
    Inbox(inbox_cmd::InboxArgs),
    /// Print what waits for the person: the cards of the "needs you" queue, then the versions that are not ready. The chat path is rdy_pending.
    Pending(intake_cmd::PendingArgs),
    /// Answer one card of the queue: rdy answer <ref> <option> [--card <hash>], or rdy answer --all-recommended <ref>... A person answers as themselves; an agent only by relaying the person's words (--relayed-by with --evidence). The CLI has no Later. The chat path is rdy_answer.
    Answer(intake_cmd::AnswerArgs),
    /// Print the coverage report: the score per dimension, the lowest pages, and each coverage issue with its fix
    Coverage(coverage_cmd::CoverageArgs),
    /// Undo the newest op of this session on a view, or the named op. The commits of one
    /// import undo and redo together; rdy view exclude takes out one change.
    Undo(view_cmd::UndoArgs),
    /// Name a landing (default: canon's head) as a version, e.g. v1.2.0. Approver only.
    Tag(land_cmd::TagArgs),
    /// List versions, oldest first.
    Tags,
    // the about is given in full: a doc comment would lose its closing period
    #[command(about = "Create a folder, doc, or publication profile.")]
    New {
        #[command(subcommand)]
        what: edit_cmd::NewCmd,
    },
    /// Check a publication profile's output: its selection, its references outside the selection, and the changes against a baseline.
    Publication {
        #[command(subcommand)]
        cmd: publication_cmd::PublicationCmd,
    },
    /// List the folder and doc tree.
    Ls(read_cmd::LsArgs),
    Edit(edit_cmd::EditArgs),
    /// Print the link to the live docs site; open it in a browser. Pages update by themselves as the scope changes.
    Site(site_cmd::SiteArgs),
    Render(site_cmd::RenderArgs),
    /// Print the docs as one PDF, one HTML file or a Markdown tree (--format pdf, html or md).
    Export(site_cmd::ExportArgs),
    Import(site_cmd::ImportArgs),
    /// Write the whole project, with its history, views and proposals, to one dump file; every hash is kept.
    Dump(dump_cmd::DumpArgs),
    /// Rebuild a project from a dump in a directory with no store (default: the current directory); every hash is kept.
    ///
    /// To move a project across a store format change, in this order: close every session on this project; with rdy 0.3.0 or a later rdy that reads store format <n>, run rdy dump --out <file>, then rdy serve --stop; move .rdy/store aside; then run rdy load <file> . with this rdy.
    Load(dump_cmd::LoadArgs),
    /// Make a new project from a project directory or a dump file, in a directory with no store (default: --project, else the current directory); every hash is kept, and the clone records its origin. --fork gives the clone a new repo id. Cloning from a hub comes with the sync client.
    Clone(dump_cmd::CloneArgs),
    Reindex,
    Fsck(project_cmd::FsckArgs),
    /// Run the MCP server over stdio.
    Mcp,
    /// Lint a doc file, a _folder.yaml or a directory without touching the store. Inside a
    /// checkout, with no path or a path in the working tree, it lints the working copy against
    /// the bound view; the working copy carries no format number. A new entry needs no id, and a
    /// file rdy does not read is a warning. Commit rewrites the committed files in canonical form
    /// and drops their YAML comments.
    Check(project_cmd::CheckArgs),
    Pack {
        #[command(subcommand)]
        cmd: project_cmd::PackCmd,
    },
    /// Keys: short names for docs and folders.
    Keys {
        #[command(subcommand)]
        cmd: project_cmd::KeysCmd,
    },
    /// Sign an existing landing or tag through your SSH agent: a person signs their own act, an agent a relay or the person's relayed act. Works when signing.sign is never. The chat path is rdy_sign.
    Sign(signing_cmd::SignArgs),
    /// Signing keys: show the configured person and agent keys (rdy key show). The chat path is rdy_key_show.
    Key {
        #[command(subcommand)]
        cmd: signing_cmd::KeyCmd,
    },
    /// Run the project's daemon in the foreground (every other command starts one as needed).
    Serve(serve_cmd::ServeArgs),
    /// Show the settings in effect, one row per key (no daemon is started).
    Config(config_cmd::ConfigArgs),
    /// Hub sign-in: start one (a confirmation card first), show its state, sign out, or list the saved sign-ins. No project or daemon is needed. The chat path is rdy_auth.
    Auth {
        #[command(subcommand)]
        cmd: auth_cmd::AuthCmd,
    },
    #[command(flatten)]
    Wc(wc::WcCmd),
}

fn run(cli: Cli, env: &Env) -> Result<(Value, String), ErrorReply> {
    let as_json = cli.json;
    match cli.cmd {
        Cmd::Config(a) => config_cmd::run(&env.project, a.show_origin),
        Cmd::Wc(c) => wc::run(c, env),
        Cmd::Init(a) => project_cmd::init(env, a, cli.project),
        Cmd::Mcp => serve_cmd::mcp(env),
        Cmd::Pack { cmd } => project_cmd::pack(env, cmd),
        Cmd::Keys { cmd } => project_cmd::keys(env, cmd),
        Cmd::Sign(a) => signing_cmd::sign(env, a),
        Cmd::Key { cmd } => signing_cmd::key(env, cmd),
        Cmd::Status => read_cmd::status(env),
        Cmd::Show(a) => read_cmd::show(env, a),
        Cmd::New { what } => edit_cmd::new(env, what),
        Cmd::Publication { cmd } => publication_cmd::publication(env, cmd),
        Cmd::Ls(a) => read_cmd::ls(env, a),
        Cmd::Log(a) => read_cmd::log(env, a),
        Cmd::Impact(a) => read_cmd::impact(env, a),
        Cmd::Brief(a) => brief_cmd::brief(env, a),
        Cmd::Search(a) => search_cmd::search(env, a),
        Cmd::Suspects(a) => read_cmd::suspects(env, a),
        Cmd::Propose(a) => land_cmd::propose(env, a),
        Cmd::Redact(a) => redact_cmd::redact(env, a),
        Cmd::Approve(a) => land_cmd::approve(env, a, as_json),
        Cmd::View { cmd } => view_cmd::view(env, cmd),
        Cmd::Alt { cmd } => alt_cmd::alt(env, cmd),
        Cmd::Intent { cmd } => intent_cmd::intent(env, cmd),
        Cmd::Tensions(a) => tension_cmd::tensions(env, a),
        Cmd::Resolve(a) => tension_cmd::resolve(env, a),
        Cmd::Inbox(a) => inbox_cmd::inbox(env, a),
        Cmd::Pending(a) => intake_cmd::pending(env, a),
        Cmd::Answer(a) => intake_cmd::answer(env, a),
        Cmd::Coverage(a) => coverage_cmd::coverage(env, a),
        Cmd::Undo(a) => view_cmd::undo(env, a),
        Cmd::Tag(a) => land_cmd::tag(env, a),
        Cmd::Tags => read_cmd::tags(env),
        Cmd::Edit(a) => edit_cmd::run_edit(env, a),
        Cmd::Site(a) => site_cmd::site(env, a),
        Cmd::Render(a) => site_cmd::render(env, a),
        Cmd::Export(a) => site_cmd::export(env, a),
        Cmd::Import(a) => site_cmd::import(env, a),
        Cmd::Dump(a) => dump_cmd::dump(env, a),
        Cmd::Load(a) => dump_cmd::load(env, a, cli.project),
        Cmd::Clone(a) => dump_cmd::clone(env, a, cli.project),
        Cmd::Reindex => project_cmd::reindex(env),
        Cmd::Fsck(a) => project_cmd::fsck(env, a),
        Cmd::Check(_) | Cmd::Serve(_) | Cmd::Auth { .. } => unreachable!("handled in main"),
    }
}

/// One configuration warning as the CLI prints it: `warning <code>: <message>; <fix>`.
pub(crate) fn config_warning_line(w: &rdy_schema::Issue) -> String {
    format!("warning {}: {}; {}", w.code, w.message, w.fix)
}

fn main() -> ExitCode {
    // `rdy load` refuses a target a live daemon still serves; the engine asks this probe
    rdy_engine::load::set_daemon_probe(rdy_daemon::live_daemon);
    let cli = Cli::parse();
    // hub sign-in reads the user config only: before the working directory, the project
    // lookup, a store or a daemon
    let cli = match cli {
        Cli {
            cmd: Cmd::Auth { cmd },
            json,
            ..
        } => return auth_cmd::run(cmd, json),
        cli => cli,
    };
    let as_json = cli.json;
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            print_error(&io_error(e, "run from an existing directory"), as_json);
            return ExitCode::FAILURE;
        }
    };
    let project = cli.project.clone().unwrap_or_else(|| find_project(&cwd));
    let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    };
    let located = match Place::try_locate(&cwd) {
        Ok(p) => p,
        Err(e) => {
            print_error(&e.into(), as_json);
            return ExitCode::FAILURE;
        }
    };
    let place = located
        .filter(|p| same(&p.project, &project))
        .unwrap_or_else(|| Place::main(&project));
    let env = Env {
        project,
        cwd,
        no_daemon: cli.no_daemon || std::env::var("RDY_NO_DAEMON").is_ok_and(|v| v == "1"),
        place: Some(place),
    };
    match &cli.cmd {
        Cmd::Check(a) => {
            return project_cmd::run_check(&env, a.path.as_deref(), a.pack.as_deref(), as_json);
        }
        Cmd::Serve(a) => {
            return serve_cmd::run_serve(&env, a.detach, a.stop, a.spawned, as_json);
        }
        _ => {}
    }
    // the config warnings print once per process on stderr; `rdy config --show-origin` and
    // `rdy status` carry them in their own output
    let own_warnings = match &cli.cmd {
        Cmd::Status | Cmd::Mcp => true,
        Cmd::Config(a) => a.show_origin && !as_json,
        // key show prints them after its three lines
        Cmd::Key { .. } => !as_json,
        _ => false,
    };
    match run(cli, &env) {
        Ok((v, text)) => {
            if !own_warnings {
                for w in rdy_engine::config_levels::config_warnings_for_env(&env.project) {
                    eprintln!("{}", config_warning_line(&w));
                }
            }
            if v.is_null() {
            } else if as_json {
                println!("{v}");
            } else if !text.is_empty() {
                println!("{text}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            print_error(&e, as_json);
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn about_of(name: &str) -> String {
        let cmd = Cli::command();
        let sub = cmd
            .get_subcommands()
            .find(|c| c.get_name() == name)
            .unwrap_or_else(|| panic!("{name}"));
        sub.get_long_about()
            .or(sub.get_about())
            .map(|a| a.to_string())
            .unwrap_or_default()
    }

    /// POST-LAUNCH 51 (orchestrator ruling 2026-10-17): `rdy check`'s help names no export
    /// manifest format.
    #[test]
    fn rdx13_the_check_help_names_no_manifest_format() {
        let about = about_of("check");
        assert!(
            about.starts_with(
                "Lint a doc file, a _folder.yaml or a directory without touching the store. Inside a checkout"
            ),
            "{about}"
        );
        assert!(!about.contains("manifest"), "{about}");
        assert!(!about.contains("format 3"), "{about}");
    }

    /// rdx13 (D-KQ4CA, D-CC90E): `rdy redact <target>... --reason-class <class> --reason <text>
    /// [--dry-run] [--view <view>] [--max-tokens <n>]` sends the shared op's keys with its
    /// defaults (false, the session's view, 4000 tokens); neither it nor `rdy approve` takes a
    /// switch that skips the person.
    #[test]
    fn rdx13_redact_sends_the_shared_request() {
        let parse = |args: &[&str]| {
            let mut all = vec!["rdy"];
            all.extend_from_slice(args);
            Cli::try_parse_from(all)
        };
        let Ok(Cli {
            cmd: Cmd::Redact(a),
            ..
        }) = parse(&[
            "redact",
            "p-AAAAA",
            "blob:00",
            "--reason-class",
            "secret",
            "--reason",
            "a leak",
        ])
        else {
            panic!("redact parses");
        };
        assert_eq!(
            redact_cmd::request(&a),
            serde_json::json!({"targets": ["p-AAAAA", "blob:00"], "reason_class": "secret",
                "reason": "a leak", "dry_run": false, "view": null, "max_tokens": null})
        );
        let Ok(Cli {
            cmd: Cmd::Redact(a),
            ..
        }) = parse(&[
            "redact",
            "p-AAAAA",
            "--reason-class",
            "legal",
            "--reason",
            "x",
            "--dry-run",
            "--view",
            "r1",
            "--max-tokens",
            "300",
        ])
        else {
            panic!("redact parses");
        };
        assert_eq!(
            redact_cmd::request(&a),
            serde_json::json!({"targets": ["p-AAAAA"], "reason_class": "legal", "reason": "x",
                "dry_run": true, "view": "r1", "max_tokens": 300})
        );
        // zero targets reach the op, which refuses them with its own text
        let Ok(Cli {
            cmd: Cmd::Redact(a),
            ..
        }) = parse(&["redact", "--reason-class", "secret", "--reason", "x"])
        else {
            panic!("redact parses");
        };
        assert_eq!(redact_cmd::request(&a)["targets"], serde_json::json!([]));
        for flag in ["--force", "--yes", "--policy", "--approve", "--apply"] {
            assert!(
                parse(&[
                    "redact",
                    "p-AAAAA",
                    "--reason-class",
                    "secret",
                    "--reason",
                    "x",
                    flag
                ])
                .is_err(),
                "{flag}"
            );
            assert!(parse(&["approve", "P-0123@1", flag]).is_err(), "{flag}");
        }
    }

    /// Final review M3: `rdy --help` names the mention form of `rdy resolve`, so a person finds
    /// `--mention <block> <entity>` with `--dismiss` or `--restore`.
    #[test]
    fn cc8_the_resolve_help_names_the_mention_form() {
        let cmd = Cli::command();
        let resolve = cmd
            .get_subcommands()
            .find(|c| c.get_name() == "resolve")
            .expect("resolve");
        let about = resolve
            .get_about()
            .map(|a| a.to_string())
            .unwrap_or_default();
        assert!(
            about.contains("--mention <block> <entity>")
                && about.contains("--dismiss")
                && about.contains("--restore"),
            "{about}"
        );
    }
}
