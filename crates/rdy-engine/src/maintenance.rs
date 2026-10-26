//! Store maintenance (D-WWAK7, D-1HDVA): the upgrade of a project's store as `rdy upgrade` and
//! `rdy_upgrade` show and run it.
//!
//! - [`connected`] answers for the project a session is connected to, from one captured read
//!   state: a store opened normally is already upgraded, so it reports no steps and the last
//!   `meta/upgrade` receipt. It never opens, locks or stops anything.
//! - [`run`] plans the store at a project directory with no store open ([`plan`]), and applies
//!   the plan under the store's lock ([`apply_planned`]): the backup, the one upgrade batch, the
//!   reindex and the store check, then the engine's open under that same lock, whose completion
//!   runs the full fsck and only then publishes `FORMAT`. It stops no daemon: a caller that may
//!   meet one (the CLI) stops it first.
//!
//! [`report_json`] is the one JSON shape both surfaces return.
use crate::{E_STORE_FORMAT, Rdy, RdyError, ReadState};
use rdy_store::format::{self, FeatureClass, StoreAccess, UpgradeReceipt};
use rdy_store::upgrade::{UpgradePlan, UpgradeStep};
use rdy_store::{StoreError, StoreLock, layout};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub use rdy_store::format::StoreFeatures;
pub use rdy_store::upgrade::UpgradeReport;

/// What [`crate::engine::Engine::upgrade`] asks for: `dry_run` inspects without writes (default
/// false). It names no directory: the engine acts only on its own project.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpgradeRequest {
    #[serde(default)]
    pub dry_run: bool,
}

/// The store directory of the project at `root`.
pub fn store_dir(root: &Path) -> PathBuf {
    root.join(".rdy").join(layout::STORE_DIR)
}

/// The upgrade plan of the closed store of the project at `root`, with this rdy's steps. It
/// opens no store, takes or creates no lock, and writes nothing. A preview store (a scalar
/// format) is refused with its dump-and-load fix. A sealed upgrade whose completion is pending
/// on a store with an unknown read-only feature is refused with the read-only open's text
/// (ruling task-9 item 3): this rdy cannot complete it (ruling task-11 item 1).
pub fn plan(root: &Path) -> Result<UpgradePlan, RdyError> {
    plan_with(root, None)
}

/// Test hook: [`plan`] with the steps `registry`. No CLI, MCP, config or environment path
/// reaches it.
#[doc(hidden)]
pub fn plan_with_registry_for_tests(
    root: &Path,
    registry: &[UpgradeStep],
) -> Result<UpgradePlan, RdyError> {
    plan_with(root, Some(registry))
}

fn plan_with(root: &Path, registry: Option<&[UpgradeStep]>) -> Result<UpgradePlan, RdyError> {
    let dir = store_dir(root);
    let plan = match registry {
        None => rdy_store::upgrade::inspect(&dir)?,
        Some(r) => rdy_store::upgrade::inspect_with_registry_for_tests(&dir, r)?,
    };
    if plan.pending_check && read_only(&plan.from) {
        return Err(rdy_store::upgrade::unfinished_upgrade(&plan.from).into());
    }
    Ok(plan)
}

/// Whether `plan` has work: steps to run, or a sealed upgrade whose fsck and `FORMAT` are still
/// to come. A store with an unknown read-only feature has none: it is never upgraded here.
pub fn needs_work(plan: &UpgradePlan) -> bool {
    (!plan.steps.is_empty() || plan.pending_check) && !read_only(&plan.from)
}

/// Whether `features` hold an unknown read-only feature (the store opens for reads only).
fn read_only(features: &StoreFeatures) -> bool {
    matches!(format::check_features(features), Ok(StoreAccess::ReadOnly))
}

/// The `E_STORE_READ_ONLY` refusal of a write to a store of `features`, naming the first unknown
/// read-only feature.
fn read_only_refusal(features: &StoreFeatures) -> RdyError {
    let known = format::registered_features();
    let f = features.features.iter().find(|f| {
        f.class == FeatureClass::ReadOnlyCompatible && !known.iter().any(|k| k.name == f.name)
    });
    match f {
        Some(f) => StoreError::ReadOnly {
            feature: f.name.clone(),
            required_rdy: f.required_rdy.clone(),
        }
        .into(),
        None => StoreError::ReadOnlyNoLock.into(),
    }
}

/// `E_STORE_READ_ONLY` when `plan` has work on a store with an unknown read-only feature: an
/// apply never upgrades such a store (a dry run still lists the plan).
pub fn refuse_read_only(plan: &UpgradePlan) -> Result<(), RdyError> {
    if read_only(&plan.from) && (!plan.steps.is_empty() || plan.pending_check) {
        return Err(read_only_refusal(&plan.from));
    }
    Ok(())
}

/// The manifest an upgrade of `plan` at the project `root` goes from: the `FORMAT` mirror's
/// while a sealed upgrade waits for its completion (the completion publishes the sealed
/// manifest over it), else the plan's. A dry run and an apply of one store report the same.
fn from_of(root: &Path, plan: &UpgradePlan) -> Result<StoreFeatures, RdyError> {
    if !plan.pending_check {
        return Ok(plan.from.clone());
    }
    Ok(format::read_format_file(&store_dir(root))?.unwrap_or_else(|| plan.from.clone()))
}

/// The report of `plan`, the plan of the project at `root`, when nothing runs: a dry run lists
/// the planned steps and whether a completion is pending (ruling task-11 item 1); an apply
/// with no work lists none. Nothing changed, no backup or batch, and neither check ran.
pub fn unchanged(
    root: &Path,
    plan: &UpgradePlan,
    dry_run: bool,
) -> Result<UpgradeReport, RdyError> {
    Ok(UpgradeReport {
        dry_run,
        changed: false,
        from: from_of(root, plan)?,
        to: plan.to.clone(),
        steps: plan.steps.clone(),
        backup: None,
        batch: None,
        reindexed: false,
        checked: false,
        receipt: plan.receipt.clone(),
        pending: dry_run && plan.pending_check,
    })
}

/// Inspects (`dry_run`) or upgrades the store of the project at `root`, with no stop
/// coordination: a daemon that holds the store makes the apply `E_LOCKED`. A dry run only
/// plans; it opens, locks and writes nothing.
pub fn run(root: &Path, dry_run: bool) -> Result<UpgradeReport, RdyError> {
    run_with(root, dry_run, None)
}

/// Test hook: [`run`] with the steps `registry`, through the same planner and runner.
#[doc(hidden)]
pub fn run_with_registry_for_tests(
    root: &Path,
    dry_run: bool,
    registry: &[UpgradeStep],
) -> Result<UpgradeReport, RdyError> {
    run_with(root, dry_run, Some(registry))
}

fn run_with(
    root: &Path,
    dry_run: bool,
    registry: Option<&[UpgradeStep]>,
) -> Result<UpgradeReport, RdyError> {
    let planned = plan_with(root, registry)?;
    if !dry_run {
        refuse_read_only(&planned)?;
    }
    if dry_run || !needs_work(&planned) {
        return unchanged(root, &planned, dry_run);
    }
    apply_with(root, &planned, registry)
}

/// Applies `planned` to the store of the project at `root`: takes the store's lock, plans
/// again under it, runs the upgrade batch (`rdy_store::upgrade::apply_locked`), then opens the
/// project under the same lock, whose completion runs the full fsck before `FORMAT` is
/// published. A plan with no work left (another coordinator finished it) returns unchanged; a
/// plan with work that differs from `planned` is `E_UPGRADE_MOVED` (ruling task-11 item 2).
/// The project's context (its config and pack, or a holder's) is resolved before anything is
/// written, so a project that cannot open is refused as [`Rdy::open`] refuses it, with no
/// backup or batch. The completed report goes to the caller.
pub fn apply_planned(root: &Path, planned: &UpgradePlan) -> Result<UpgradeReport, RdyError> {
    apply_with(root, planned, None)
}

/// Test hook: [`apply_planned`] with the steps `registry`.
#[doc(hidden)]
pub fn apply_planned_with_registry_for_tests(
    root: &Path,
    planned: &UpgradePlan,
    registry: &[UpgradeStep],
) -> Result<UpgradeReport, RdyError> {
    apply_with(root, planned, Some(registry))
}

/// Whether two plans plan the same upgrade: the same manifests, steps and pending completion.
/// The backup size may differ (an old daemon's last writes before its stop).
fn same_plan(a: &UpgradePlan, b: &UpgradePlan) -> bool {
    a.from == b.from && a.to == b.to && a.steps == b.steps && a.pending_check == b.pending_check
}

fn moved() -> RdyError {
    StoreError::Upgrade {
        failure: rdy_store::error::UpgradeFailure::Moved,
        backup: None,
    }
    .into()
}

fn apply_with(
    root: &Path,
    planned: &UpgradePlan,
    registry: Option<&[UpgradeStep]>,
) -> Result<UpgradeReport, RdyError> {
    let dir = store_dir(root);
    let lock = StoreLock::take(&dir)?;
    let plan = plan_with(root, registry)?;
    refuse_read_only(&plan)?;
    if !needs_work(&plan) {
        return unchanged(root, &plan, false);
    }
    if !same_plan(&plan, planned) {
        return Err(moved());
    }
    // the ordinary open's refusals come first: nothing is written for a project that cannot open
    let mode = rdy_store::holder::sealed_mode(&dir)?;
    let context = Rdy::context_of(root, mode)?;
    let from = from_of(root, &plan)?;
    let applied = match registry {
        None => rdy_store::upgrade::apply_locked(&dir, &lock, &plan)?,
        Some(r) => rdy_store::upgrade::apply_locked_with_registry_for_tests(&dir, &lock, &plan, r)?,
    };
    // the engine's open completes the sealed upgrade: the full fsck, then `FORMAT`
    let r = Rdy::open_locked_with(root, &lock, mode, context)?;
    let receipt = receipt_of(r.store().get_ref(format::REF_UPGRADE)?)?;
    let backup = applied.backup.clone().or_else(|| {
        receipt
            .as_ref()
            .and_then(|rc| dir.parent().map(|p| p.join(&rc.backup)))
    });
    let report = UpgradeReport {
        dry_run: false,
        changed: true,
        from,
        to: r.store().features().clone(),
        steps: plan.steps.clone(),
        backup,
        batch: applied.batch,
        reindexed: applied.reindexed,
        checked: true,
        receipt,
        pending: false,
    };
    r.close()?;
    drop(lock);
    Ok(report)
}

/// The receipt a `meta/upgrade` value holds; an invalid one is a decode error.
fn receipt_of(value: Option<Vec<u8>>) -> Result<Option<UpgradeReceipt>, RdyError> {
    value
        .map(|v| UpgradeReceipt::parse(&v))
        .transpose()
        .map_err(|e| StoreError::Decode(format!("meta/upgrade is not a valid receipt: {e}")).into())
}

/// The upgrade facts of the connected project, from the captured `state` alone: its features,
/// no steps (an open store is already upgraded) and the last `meta/upgrade` receipt. An apply
/// to a read-only store is `E_STORE_READ_ONLY`; any other apply returns unchanged. It never
/// opens, locks or reopens the store, and never stops a process.
pub fn connected(state: &ReadState, dry_run: bool) -> Result<UpgradeReport, RdyError> {
    let reader = state.reader();
    if !dry_run {
        reader.ensure_writable()?;
    }
    let features = match reader.get_ref(format::REF_STORE_FEATURES)? {
        Some(v) => format::decode_features(&v)?,
        None => {
            return Err(RdyError::new(
                E_STORE_FORMAT,
                "the store has no store features",
                "restore the store from backup, or re-create it with this rdy",
            ));
        }
    };
    let receipt = receipt_of(reader.get_ref(format::REF_UPGRADE)?)?;
    Ok(UpgradeReport {
        dry_run,
        changed: false,
        from: features.clone(),
        to: features,
        steps: vec![],
        backup: None,
        batch: None,
        reindexed: false,
        checked: false,
        receipt,
        pending: false,
    })
}

/// The report as `rdy upgrade --json` and `rdy_upgrade` return it: `from` and `to` are the
/// manifests' hashes and `batch` the upgrade batch's marker, in full lower-case hex; `backup`
/// is the backup's path as display text; an absent backup, batch or receipt is null. The
/// receipt keeps its own canonical field names. `pending` (ruling task-11 item 1) follows them.
pub fn report_json(r: &UpgradeReport) -> Value {
    json!({
        "dry_run": r.dry_run,
        "changed": r.changed,
        "from": r.from.hash().to_string(),
        "to": r.to.hash().to_string(),
        "steps": r.steps,
        "backup": r.backup.as_ref().map(|p| p.display().to_string()),
        "batch": r.batch.map(|h| h.to_string()),
        "reindexed": r.reindexed,
        "checked": r.checked,
        "receipt": r.receipt.as_ref().map(|rc| {
            serde_json::to_value(rc).expect("a receipt always serializes")
        }),
        "pending": r.pending,
    })
}

/// The text `rdy upgrade` prints: `store is current`; `store upgrade needs {n} step(s)` and one
/// `  {step}` line per step for a dry run with steps; `store upgrade is pending: run rdy upgrade
/// to finish it` for a dry run of a pending completion (ruling task-11 item 1); `store
/// upgraded; backup: {path}` after an apply.
pub fn report_text(r: &UpgradeReport) -> String {
    if r.changed {
        let backup = r
            .backup
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        return format!("store upgraded; backup: {backup}");
    }
    if r.dry_run && !r.steps.is_empty() {
        let mut out = format!("store upgrade needs {} step(s)", r.steps.len());
        for s in &r.steps {
            out.push_str(&format!("\n  {s}"));
        }
        return out;
    }
    if r.pending {
        return "store upgrade is pending: run rdy upgrade to finish it".into();
    }
    "store is current".into()
}
