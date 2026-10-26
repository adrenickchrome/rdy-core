//! ok11: `rdy upgrade [dir] [--dry-run] [--json]` (D-WWAK7, D-KQ4CA, D-1HDVA), the CLI twin of
//! `rdy_upgrade` for any project directory. It runs before any ordinary project connection: a
//! dry run plans only (no daemon is started or stopped, no writer opened, no lock, backup or
//! index written); an apply with work stops the project's daemon first
//! (`rdy_daemon::maintenance`). The result has `rdy_upgrade`'s JSON shape.
use crate::output::print_error;
use rdy_daemon::ErrorReply;
use rdy_engine::RdyError;
use rdy_engine::maintenance::{UpgradeReport, report_json, report_text};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(clap::Args)]
pub(crate) struct UpgradeArgs {
    /// The project directory (default: --project, else the nearest ancestor containing .rdy/).
    dir: Option<PathBuf>,
    /// Inspect the upgrade without writes: no daemon is started or stopped, and no lock, backup or index is written.
    #[arg(long)]
    dry_run: bool,
}

/// The directory `rdy upgrade` acts on, and whether it was searched for: its `dir` exactly,
/// else the global `--project`, else the project every other command finds from `cwd` (the
/// nearest ancestor containing `.rdy/`).
pub(crate) fn target(a: &UpgradeArgs, project: Option<&Path>, cwd: &Path) -> (PathBuf, bool) {
    match a.dir.clone().or_else(|| project.map(Path::to_path_buf)) {
        Some(dir) => (dir, false),
        None => (crate::conn::find_project(cwd), true),
    }
}

/// Runs `rdy upgrade` and prints its result: the report's JSON with `--json`, else its text;
/// an rdy error in the shared error form, with the failure exit. A searched directory that is no
/// project gets the error every other command gives there (`E_NO_PROJECT`).
pub(crate) fn upgrade(a: &UpgradeArgs, project: Option<&Path>, cwd: &Path, json: bool) -> ExitCode {
    let (dir, searched) = target(a, project, cwd);
    if searched && let Err(e) = crate::conn::require_project(&dir) {
        print_error(&e, json);
        return ExitCode::FAILURE;
    }
    match result(rdy_daemon::maintenance::run(&dir, a.dry_run)) {
        Ok((v, text)) => {
            if json {
                println!("{v}");
            } else {
                println!("{text}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            print_error(&e, json);
            ExitCode::FAILURE
        }
    }
}

/// The JSON and text of an upgrade's outcome `r`, or its error as the CLI reports it.
pub(crate) fn result(r: Result<UpgradeReport, RdyError>) -> Result<(Value, String), ErrorReply> {
    let r = r.map_err(ErrorReply::from)?;
    Ok((report_json(&r), report_text(&r)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdy_engine::E_UPGRADE_MOVED;
    use rdy_engine::maintenance::StoreFeatures;
    use serde_json::json;

    fn baseline_features() -> StoreFeatures {
        StoreFeatures::of(vec![])
    }

    fn report(dry_run: bool, changed: bool, steps: &[&str]) -> UpgradeReport {
        UpgradeReport {
            dry_run,
            changed,
            from: baseline_features(),
            to: baseline_features(),
            steps: steps.iter().map(|s| s.to_string()).collect(),
            backup: changed.then(|| PathBuf::from("/p/.rdy/.store-upgrade-a-b")),
            batch: None,
            reindexed: changed,
            checked: changed,
            receipt: None,
            pending: false,
        }
    }

    /// ok11: the handler prints exactly one of the four texts (ruling task-11 item 1 adds the
    /// pending one), and the report's JSON shape.
    #[test]
    fn ok11_the_handler_prints_the_report_text_and_json() {
        let hash = baseline_features().hash().to_string();
        let (v, text) = result(Ok(report(false, false, &[]))).unwrap();
        assert_eq!(text, "store is current");
        assert_eq!(
            v,
            json!({"dry_run": false, "changed": false, "from": hash, "to": hash, "steps": [],
                "backup": null, "batch": null, "reindexed": false, "checked": false,
                "receipt": null, "pending": false})
        );
        let (_, text) = result(Ok(report(true, false, &[]))).unwrap();
        assert_eq!(text, "store is current");
        let (v, text) = result(Ok(report(true, false, &["one", "two"]))).unwrap();
        assert_eq!(text, "store upgrade needs 2 step(s)\n  one\n  two");
        assert_eq!(v["steps"], json!(["one", "two"]));
        assert_eq!(v["dry_run"], json!(true));
        let (v, text) = result(Ok(report(false, true, &["one"]))).unwrap();
        assert_eq!(text, "store upgraded; backup: /p/.rdy/.store-upgrade-a-b");
        assert_eq!(v["backup"], json!("/p/.rdy/.store-upgrade-a-b"));
        assert_eq!(v["checked"], json!(true));
        let (v, text) = result(Ok(UpgradeReport {
            pending: true,
            ..report(true, false, &[])
        }))
        .unwrap();
        assert_eq!(
            text,
            "store upgrade is pending: run rdy upgrade to finish it"
        );
        assert_eq!(v["pending"], json!(true));
    }

    /// ok11: an rdy error reaches the caller with its code, message and fix unchanged.
    #[test]
    fn ok11_the_handler_passes_an_rdy_error_through() {
        let e = result(Err(RdyError::new(E_UPGRADE_MOVED, "m", "f"))).unwrap_err();
        assert_eq!(
            (e.code.as_str(), e.message.as_str(), e.fix.as_str()),
            (E_UPGRADE_MOVED, "m", "f")
        );
    }

    /// ok11: the directory is the argument, else `--project`, else the project found from the
    /// working directory (the nearest ancestor containing `.rdy/`).
    #[test]
    fn ok11_the_target_is_dir_then_project_then_cwd() {
        let a = |dir: Option<&str>| UpgradeArgs {
            dir: dir.map(PathBuf::from),
            dry_run: false,
        };
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".rdy")).unwrap();
        let sub = root.path().join("sub/deeper");
        std::fs::create_dir_all(&sub).unwrap();
        let p = Path::new("/project");
        // an explicit directory means that directory exactly, even inside a project
        assert_eq!(
            target(&a(Some("/d")), Some(p), &sub),
            (PathBuf::from("/d"), false)
        );
        assert_eq!(
            target(&a(Some("sub")), None, root.path()),
            (PathBuf::from("sub"), false)
        );
        assert_eq!(
            target(&a(None), Some(p), &sub),
            (PathBuf::from("/project"), false)
        );
        assert_eq!(
            target(&a(None), None, &sub),
            (root.path().to_path_buf(), true)
        );
    }
}
