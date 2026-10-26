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
    /// The project directory (default: --project, else the current directory).
    dir: Option<PathBuf>,
    /// Inspect the upgrade without writes: no daemon is started or stopped, and no lock, backup or index is written.
    #[arg(long)]
    dry_run: bool,
}

/// The directory `rdy upgrade` acts on: its `dir`, else the global `--project`, else `cwd`.
pub(crate) fn target(a: &UpgradeArgs, project: Option<&Path>, cwd: &Path) -> PathBuf {
    a.dir
        .clone()
        .or_else(|| project.map(Path::to_path_buf))
        .unwrap_or_else(|| cwd.to_path_buf())
}

/// Runs `rdy upgrade` and prints its result: the report's JSON with `--json`, else its text;
/// an rdy error in the shared error form, with the failure exit.
pub(crate) fn upgrade(a: &UpgradeArgs, project: Option<&Path>, cwd: &Path, json: bool) -> ExitCode {
    let dir = target(a, project, cwd);
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

    /// ok11: the directory is the argument, else `--project`, else the working directory.
    #[test]
    fn ok11_the_target_is_dir_then_project_then_cwd() {
        let a = |dir: Option<&str>| UpgradeArgs {
            dir: dir.map(PathBuf::from),
            dry_run: false,
        };
        let cwd = Path::new("/cwd");
        let p = Path::new("/project");
        assert_eq!(target(&a(Some("/d")), Some(p), cwd), PathBuf::from("/d"));
        assert_eq!(target(&a(None), Some(p), cwd), PathBuf::from("/project"));
        assert_eq!(target(&a(None), None, cwd), PathBuf::from("/cwd"));
    }
}
