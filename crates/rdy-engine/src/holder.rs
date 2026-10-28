//! Holders (D-BPYN8): a store that keeps uploaded objects and tags with no canon. A holder has
//! no config file, no pack directory and no holder settings, and none is created or read: its
//! handle is the project handle over the holder's store, filled with fixed values. Its pack is
//! the built-in `rdy-standard`, resolved in memory (O5), so publication profiles parse. Its
//! config holds that pack name and nothing else, and its fingerprint covers no file. Its name
//! and description are the `site_identity` fallback over the holder directory (O10).
//!
//! A holder reads at tag and landing scopes only, with the existing materialization and epoch
//! rules: canon, views, the in-flight scope and proposals are refused (`needs_tag`). Its
//! publications capture empty canon, no views and the current tags. Its batches log no tension
//! or suspect moves, and its fsck checks each tag's own history (`StoreReader::check_holder`).
use crate::config::{Config, ServeSection, SiteConfig};
use crate::handle::{Context, io};
use crate::pack_load::load_pack_from;
use crate::{E_BAD_INPUT, E_LIMIT, PackFingerprint, Rdy, RdyError};
use rdy_model::{Hash, RepoId};
use rdy_store::check_holder::holder_check_limit;
use rdy_store::holder::StoreMode;
use rdy_store::{OpenPolicy, Store, StoreError, layout};
use std::path::Path;

/// The pack a holder reads with: the built-in one, never a pack file.
const HOLDER_PACK: &str = "rdy-standard";

/// The refusal of a project store on the holder path.
pub(crate) fn is_a_project() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "this store is a project",
        "open it through the project adapter",
    )
}

/// The refusal of a holder store on the project path.
pub(crate) fn is_a_holder() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "this store is a holder",
        "open it through the holder adapter",
    )
}

/// The refusal of a holder read that needs canon: a canon, view, in-flight or proposal scope.
pub(crate) fn needs_tag() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "holder reads need a tag or landing",
        "name tag:<version> or landing:<hash>",
    )
}

/// A holder check's store error as the engine's: a bound is `E_LIMIT`, `holder check exceeds
/// {limit}`; anything else is the store error's own.
pub(crate) fn holder_error(e: StoreError) -> RdyError {
    match holder_check_limit(&e) {
        Some(limit) => RdyError::new(
            E_LIMIT,
            format!("holder check exceeds {limit}"),
            "reduce the history to the stated limit",
        ),
        None => e.into(),
    }
}

/// A holder's context: the built-in pack and its lineage, a config with that pack and every
/// other setting absent, and a fingerprint of no file. Reads no file.
pub(crate) fn holder_context() -> Result<Context, RdyError> {
    let (pack, lineage) = load_pack_from(&[], HOLDER_PACK)?;
    Ok(Context {
        pack,
        lineage,
        fingerprint: PackFingerprint {
            files: vec![],
            hash: Hash::of(b""),
            at: std::time::SystemTime::UNIX_EPOCH,
        },
        config: Config {
            pack: HOLDER_PACK.to_string(),
            name: None,
            description: None,
            reviews: None,
            views: None,
            decisions: None,
            serve: ServeSection::default(),
            site: SiteConfig::default(),
            coverage: None,
            propose: None,
        },
    })
}

impl Rdy {
    /// Creates a holder at `root` with repo id `repo_id`: its store at `.rdy/store`, through
    /// `Store::create_holder` (one durable batch with the format, the repo id and the mode). A
    /// directory there that holds anything is refused and left as it was. Nothing else is
    /// written: no config, pack directory or settings.
    pub fn create_holder(root: &Path, repo_id: RepoId) -> Result<Rdy, RdyError> {
        let dot = root.join(".rdy");
        rdy_store::fsutil::create_dir_durable(&dot).map_err(io)?;
        let store = Store::create_holder(&dot.join(layout::STORE_DIR), repo_id)?;
        Rdy::from_store(root, store, holder_context()?)
    }

    /// Opens the holder at `root`: its store at `.rdy/store`, which must be a holder (`this
    /// store is a project` otherwise). No config, pack or local settings file is read, whatever
    /// stands beside the store.
    pub fn open_holder(root: &Path) -> Result<Rdy, RdyError> {
        Rdy::open_holder_with_policy(root, OpenPolicy::AutoUpgrade)
    }

    /// Opens the holder at `root` as [`Rdy::open_holder`] does, with upgrade policy `policy`
    /// (see [`Rdy::open_with_policy`]).
    pub fn open_holder_with_policy(root: &Path, policy: OpenPolicy) -> Result<Rdy, RdyError> {
        Rdy::open_holder_with(root, policy, None)
    }

    /// [`Rdy::open_holder_with_policy`] with the upgrade steps `registry` (a test hook's).
    pub(crate) fn open_holder_with(
        root: &Path,
        policy: OpenPolicy,
        registry: Option<&[rdy_store::upgrade::UpgradeStep]>,
    ) -> Result<Rdy, RdyError> {
        let store = Rdy::open_store(&root.join(".rdy"), policy, registry)?;
        if store.mode()? != StoreMode::Holder {
            return Err(is_a_project());
        }
        let mut r = Rdy::from_store(root, store, holder_context()?)?;
        r.complete_upgrade()?;
        Ok(r)
    }

    /// Whether this handle's store is a holder. The mode is set at creation and never changes.
    pub(crate) fn is_holder(&self) -> bool {
        matches!(self.store.mode(), Ok(StoreMode::Holder))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ha7_a_holder_bound_is_e_limit() {
        for label in ["100000 landing depth", "1000000 objects", "1000000 edges"] {
            let e = holder_error(StoreError::Refused(format!("holder check exceeds {label}")));
            assert_eq!(e.code, E_LIMIT);
            assert_eq!(e.message, format!("holder check exceeds {label}"));
            assert_eq!(e.fix, "reduce the history to the stated limit");
        }
        let e = holder_error(StoreError::Decode("damage".into()));
        assert_eq!(e.code, crate::E_IO);
    }

    #[test]
    fn ha7_the_holder_context_reads_no_file() {
        let c = holder_context().unwrap();
        assert_eq!(c.pack.name, HOLDER_PACK);
        assert!(c.pack.block_types.contains_key("publication_profile"));
        assert!(c.fingerprint.files.is_empty());
        assert_eq!(c.config.pack, HOLDER_PACK);
    }
}
