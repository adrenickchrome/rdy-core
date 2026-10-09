use crate::conn::{Env, bound_view, call, is_project, path_arg};
use crate::output::{num, open_warnings, print_error, text};
use clap::Subcommand;
use rdy_daemon::ErrorReply;
use rdy_engine::{
    E_BAD_INPUT, E_CONFIG, E_NO_PROJECT, Rdy, check_path, lexical_path, project_pack,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(clap::Args)]
pub(crate) struct InitArgs {
    dir: Option<PathBuf>,
    #[arg(long, default_value = "rdy-standard")]
    pack: String,
}

#[derive(clap::Args)]
pub(crate) struct CheckArgs {
    /// The file or directory (required outside a checkout; default: the working tree).
    pub(crate) path: Option<PathBuf>,
    /// Check against this pack (required outside a project).
    #[arg(long)]
    pub(crate) pack: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct FsckArgs {
    /// Also remove the quarantine files a crash left (.torn- segment tails and damaged indexes). Human only.
    #[arg(long)]
    clean_quarantine: bool,
}

#[derive(Subcommand)]
pub(crate) enum KeysCmd {
    /// Give every doc and folder without a key the key derived from its path, in one change.
    Assign {
        /// The view (default: the session's, `work` for the human).
        #[arg(long)]
        view: Option<String>,
    },
}

#[derive(Subcommand)]
pub(crate) enum PackCmd {
    /// Resolve the project's pack and report problems.
    Check,
}

pub(crate) fn init(
    env: &Env,
    a: InitArgs,
    project: Option<PathBuf>,
) -> Result<(Value, String), ErrorReply> {
    let InitArgs { dir, pack } = a;
    let dir = dir.or(project).unwrap_or_else(|| env.cwd.clone());
    Rdy::init(&dir, &pack)?;
    Ok((
        json!({"project": dir, "pack": pack}),
        format!("Initialized rdy project in {} (pack {pack})", dir.display()),
    ))
}

pub(crate) fn pack(env: &Env, cmd: PackCmd) -> Result<(Value, String), ErrorReply> {
    let PackCmd::Check = cmd;
    let pack = project_pack(&env.project)?;
    let classes: serde_json::Map<String, Value> = pack
        .block_types
        .keys()
        .map(|t| (t.clone(), json!(pack.class_of(t).as_str())))
        .collect();
    let areas: Vec<Value> = pack
        .areas
        .iter()
        .map(|(n, a)| json!({"name": n, "description": a.description, "paths": a.paths}))
        .collect();
    let mut text = format!("pack {} {} ok", pack.name, pack.version);
    for t in pack.block_types.keys() {
        text.push_str(&format!("\n  {t}  {}", pack.class_of(t).as_str()));
    }
    for (name, a) in &pack.areas {
        text.push_str(&format!("\narea {name}: {}", a.paths.join(", ")));
        if !a.description.is_empty() {
            text.push_str(&format!(" ({})", a.description));
        }
    }
    Ok((
        json!({"pack": pack.name, "version": pack.version, "types": pack.block_types.len(), "classes": classes, "areas": areas}),
        text,
    ))
}

pub(crate) fn keys(env: &Env, cmd: KeysCmd) -> Result<(Value, String), ErrorReply> {
    let KeysCmd::Assign { view } = cmd;
    let v = call(env, "keys_assign", json!({"view": view}))?;
    let keys = v["keys"].as_object().cloned().unwrap_or_default();
    let mut lines = vec![format!(
        "assigned {} keys in view {}",
        keys.len(),
        v["view"].as_str().unwrap_or_default()
    )];
    for (id, k) in &keys {
        lines.push(format!("  {id}  {}", k.as_str().unwrap_or_default()));
    }
    Ok((v, lines.join("\n")))
}

pub(crate) fn reindex(env: &Env) -> Result<(Value, String), ErrorReply> {
    let v = call(env, "reindex", json!({}))?;
    Ok((v, "index rebuilt".into()))
}

pub(crate) fn fsck(env: &Env, a: FsckArgs) -> Result<(Value, String), ErrorReply> {
    let FsckArgs { clean_quarantine } = a;
    let v = call(env, "fsck", json!({"clean_quarantine": clean_quarantine}))?;
    let mut t = format!(
        "ok: {} frames, {} landings, {} versions",
        num(&v["frames"]),
        num(&v["landings"]),
        num(&v["versions"]),
    );
    for name in v["removed"].as_array().into_iter().flatten() {
        t.push_str(&format!("\nremoved {}", text(name)));
    }
    for s in v["skipped"].as_array().into_iter().flatten() {
        t.push_str(&format!(
            "\nskipped {}: {}",
            text(&s["name"]),
            text(&s["reason"])
        ));
    }
    t.push_str(&open_warnings(&v));
    if v["open_report"]["unclean_shutdown"] == json!(true) {
        t.push_str(&format!(
                "\nnote: this daemon started after an unclean shutdown and recovered (caught up {} frames)",
                num(&v["open_report"]["caught_up_frames"])
            ));
    }
    Ok((v, t))
}

/// Whether `path` (relative to `cwd`) is the working-tree `root` or under it. `..` never counts
/// toward "under": a path that exists is compared by its real location, and one that does not
/// by its words with `.` and `..` resolved (`/root/../x` is not under `/root`).
fn in_tree(root: &Path, cwd: &Path, path: &Path) -> bool {
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let canon_root = canon(root);
    if let Ok(real) = abs.canonicalize() {
        return real.starts_with(&canon_root);
    }
    lexical_path(&abs).is_some_and(|flat| flat.starts_with(root) || flat.starts_with(&canon_root))
}

/// `rdy check`: inside a project through the `check` op (the pack and canon come from the
/// project's state); outside one, against a built-in pack, with no store.
pub(crate) fn run_check(
    env: &Env,
    path: Option<&Path>,
    pack: Option<&str>,
    as_json: bool,
) -> ExitCode {
    let result = (|| -> Result<Value, ErrorReply> {
        let missing = || ErrorReply::new(E_BAD_INPUT, "no path to check", "pass a path to check");
        if is_project(&env.project) {
            // inside a bound checkout, no path or a path in the working tree lints the working copy
            if let Some(view) = bound_view(env)?
                && let Some(place) = &env.place
                && path.is_none_or(|p| in_tree(&place.root, &env.cwd, p))
            {
                let target = path.unwrap_or(&place.root);
                return call(
                    env,
                    "check",
                    json!({"path": path_arg(target)?, "pack": pack, "wc": true, "view": view}),
                );
            }
            let path = path.ok_or_else(missing)?;
            call(env, "check", json!({"path": path_arg(path)?, "pack": pack}))
        } else {
            let path = path.ok_or_else(missing)?;
            let name = pack.ok_or_else(|| {
                ErrorReply::new(
                    E_NO_PROJECT,
                    "not inside an rdy project",
                    "run rdy check inside a project, or pass --pack rdy-standard",
                )
            })?;
            let pk = rdy_schema::PackSet::with_builtin()
                .resolve(name)
                .map_err(|e| {
                    ErrorReply::new(
                        E_CONFIG,
                        e.to_string(),
                        "pass a built-in pack such as rdy-standard",
                    )
                })?;
            Ok(serde_json::to_value(check_path(&pk, None, path)?).expect("diagnostics serialize"))
        }
    })();
    match result {
        Ok(diags) => {
            let diags = diags.as_array().cloned().unwrap_or_default();
            if as_json {
                println!("{}", Value::Array(diags.clone()));
            } else if diags.is_empty() {
                println!("ok");
            } else {
                for d in &diags {
                    println!(
                        "{}:{}:{}: {} {} (fix: {})",
                        text(&d["file"]),
                        num(&d["line"]),
                        num(&d["column"]),
                        text(&d["code"]),
                        text(&d["message"]),
                        text(&d["fix"])
                    );
                }
            }
            if diags.iter().any(|d| d["severity"] == "error") {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            print_error(&e, as_json);
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::in_tree;
    use std::path::Path;

    /// Fix round 1 (wcck1): `..` is resolved before the tree test, for paths that exist and
    /// for paths that do not.
    #[test]
    fn wcck1_in_tree_resolves_dotdot() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("tree/sub")).unwrap();
        std::fs::create_dir_all(root.join("elsewhere")).unwrap();
        let tree = root.join("tree");
        let cwd = tree.join("sub");
        assert!(in_tree(&tree, &cwd, Path::new("..")), "the root through ..");
        assert!(in_tree(&tree, &cwd, Path::new("../sub")));
        assert!(
            !in_tree(&tree, &tree, Path::new("../elsewhere")),
            "exists, outside"
        );
        assert!(!in_tree(&tree, &tree, &tree.join("../elsewhere")));
        assert!(
            !in_tree(&tree, &tree, Path::new("../nothing")),
            "missing, outside"
        );
        assert!(!in_tree(
            &tree,
            &tree,
            &tree.join("a/../../elsewhere/x.yaml")
        ));
        assert!(
            in_tree(&tree, &tree, &tree.join("a/../b.yaml")),
            "missing, inside"
        );
        assert!(in_tree(&tree, &tree, Path::new("new.yaml")));
    }
}
