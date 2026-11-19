//! The release preflight: before any release-dependent step the gate checks which build it is
//! about to test. A run once tested a bundle that printed 0.49.0 as 0.49.1 and recorded no
//! revision. The checks are pure functions of an [`Identity`], so they need no packaging;
//! [`gather`] reads the identity from the machine.
//!
//! Environment: `RDY_QA_SOURCE_REVISION` names the revision an archive was made from (a
//! `git archive` copy has no `.git`); `RDY_QA_EXPECTED_VERSION` overrides the workspace version.

use std::path::{Path, PathBuf};
use std::process::Command;

/// What the gate knows about the build under test.
pub struct Identity {
    /// The revision of the source tree (`git rev-parse`), when there is a `.git`.
    pub source_revision: Option<String>,
    /// The revision the archive was made from, when the run is of an archive.
    pub archive_revision: Option<String>,
    /// The version the release must carry.
    pub expected_version: String,
    /// What the bundle's `rdy --version` printed.
    pub bundle_output: String,
    /// The CLI the release area will run.
    pub cli_path: PathBuf,
    /// The CLI the gate just built.
    pub built_path: PathBuf,
}

/// Whether two revisions name the same commit (one may be the short form of the other).
fn same_revision(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim(), b.trim());
    !a.is_empty() && !b.is_empty() && (a.starts_with(b) || b.starts_with(a))
}

/// The revision the report records: git's, else the archive's, else `None`.
pub fn recorded_revision(
    source_revision: Option<&str>,
    archive_revision: Option<&str>,
) -> Option<String> {
    source_revision
        .or(archive_revision)
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .map(String::from)
}

/// Refuses on the first mismatch; on success returns the revision to record.
pub fn check(id: &Identity) -> Result<String, String> {
    if let (Some(s), Some(a)) = (&id.source_revision, &id.archive_revision)
        && !same_revision(s, a)
    {
        return Err(format!("source is {s}, archive is {a}"));
    }
    let revision = recorded_revision(
        id.source_revision.as_deref(),
        id.archive_revision.as_deref(),
    )
    .ok_or("revision is unknown: no git checkout and no RDY_QA_SOURCE_REVISION")?;
    let got = id
        .bundle_output
        .trim()
        .strip_prefix("rdy ")
        .and_then(|v| v.split_whitespace().next())
        .ok_or_else(|| {
            format!(
                "bundle printed {:?}, expected `rdy <version>`",
                id.bundle_output
            )
        })?;
    if got != id.expected_version {
        return Err(format!("bundle is {got}, expected {}", id.expected_version));
    }
    if id.cli_path != id.built_path {
        return Err(format!(
            "CLI is {}, not the one just built ({})",
            id.cli_path.display(),
            id.built_path.display()
        ));
    }
    Ok(revision)
}

fn canon(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// The workspace's release CLI: `<target dir>/release/rdy`.
pub fn built_cli(root: &Path) -> PathBuf {
    let target =
        env_nonempty("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from);
    target
        .join("release")
        .join(format!("rdy{}", std::env::consts::EXE_SUFFIX))
}

/// Reads the identity from the machine: git, the environment and the CLI's `--version`.
pub fn gather(root: &Path, git_revision: Option<String>) -> Identity {
    let cli = crate::fixtures::rdy_bin_path_unchecked();
    let bundle_output = Command::new(&cli)
        .arg("--version")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    Identity {
        source_revision: git_revision,
        archive_revision: env_nonempty("RDY_QA_SOURCE_REVISION"),
        expected_version: env_nonempty("RDY_QA_EXPECTED_VERSION")
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").into()),
        bundle_output,
        cli_path: canon(&cli),
        built_path: canon(&built_cli(root)),
    }
}

/// The preflight of this machine: the revision to record, or the refusal.
pub fn run(root: &Path, git_revision: Option<String>) -> Result<String, String> {
    check(&gather(root, git_revision))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> Identity {
        Identity {
            source_revision: Some("abc1234".into()),
            archive_revision: Some("abc1234def".into()),
            expected_version: "0.49.1".into(),
            bundle_output: "rdy 0.49.1\n".into(),
            cli_path: "/t/release/rdy".into(),
            built_path: "/t/release/rdy".into(),
        }
    }

    #[test]
    fn preflight_accepts_a_matching_build() {
        assert_eq!(check(&id()).as_deref(), Ok("abc1234"));
    }

    #[test]
    fn preflight_refuses_version_mismatch() {
        let mut i = id();
        i.bundle_output = "rdy 0.49.0\n".into();
        assert_eq!(check(&i), Err("bundle is 0.49.0, expected 0.49.1".into()));
    }

    #[test]
    fn preflight_refuses_revision_mismatch_and_unknown_revision() {
        let mut i = id();
        i.archive_revision = Some("ffff000".into());
        assert_eq!(
            check(&i),
            Err("source is abc1234, archive is ffff000".into())
        );
        i.source_revision = None;
        i.archive_revision = None;
        assert!(check(&i).unwrap_err().starts_with("revision is unknown"));
    }

    #[test]
    fn preflight_refuses_a_cli_that_was_not_just_built() {
        let mut i = id();
        i.cli_path = "/elsewhere/rdy".into();
        assert!(check(&i).unwrap_err().contains("not the one just built"));
    }
}
