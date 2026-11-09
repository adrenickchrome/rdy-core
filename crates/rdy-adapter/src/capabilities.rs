//! What this build writes and reads (FW-UP1a D-69AY3), for a host that reads it at build time.
//! The value is derived from core's registries and check functions: no person keeps a list.
//! It opens no store and needs no project, config, daemon or home folder.
//!
//! The lists name registered readers. They do not prove that a file is valid: a real store or
//! dump still goes through its own checker. A legacy dump pair does not mean that scalar store
//! opens in place, and the written facts do not promise writing a historical format.

use rdy_engine::formats::{FormatFacts, formats};
use rdy_store::format::{
    FeatureClass, MANIFEST_VERSION, StoreAccess, StoreFeature, StoreFeatures, check_features,
    registered_features,
};
use serde::Serialize;

pub use rdy_engine::formats::DumpPair;

/// What this build does with a store that uses a feature it does not know.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    /// The build reads and writes the store.
    ReadWrite,
    /// The build reads the store and refuses writes.
    ReadOnly,
    /// The build refuses to open the store.
    Refused,
}

/// The access this build gives to a store with an unknown feature of each class.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct UnknownFeatureAccess {
    pub compatible: Access,
    pub read_only_compatible: Access,
    pub incompatible: Access,
}

/// What this build reads in a store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StoreReads {
    /// The feature-manifest grammars this build reads.
    pub manifest_versions: Vec<u32>,
    /// The features this build registers, sorted by name, with class and required version.
    pub known_features: Vec<StoreFeature>,
    /// The access an unknown feature of each class gives.
    pub unknown_features: UnknownFeatureAccess,
}

/// One dump format of a feature store, with the manifest grammar this build reads in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct DumpFeaturePair {
    pub dump: u32,
    pub manifest_version: u32,
}

/// What this build reads in a dump file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DumpReads {
    /// The released `(dump format, scalar store format)` pairs, in release order.
    pub legacy: Vec<DumpPair>,
    /// The dump formats of a feature store, with their manifest grammars.
    pub features: Vec<DumpFeaturePair>,
}

/// The formats this build writes and the formats it reads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FormatCapabilities {
    /// What this build writes, as `maintenance::formats` gives it.
    pub written: FormatFacts,
    pub store_reads: StoreReads,
    pub dump_reads: DumpReads,
}

/// The access `check_features` gives a manifest of one unknown feature of `class`.
fn unknown_access(class: FeatureClass, rdy: &str) -> Access {
    let probe = StoreFeatures::of(vec![StoreFeature {
        name: "zz_unknown_probe".into(),
        class,
        required_rdy: rdy.into(),
    }]);
    match check_features(&probe) {
        Ok(StoreAccess::ReadWrite) => Access::ReadWrite,
        Ok(StoreAccess::ReadOnly) => Access::ReadOnly,
        Err(_) => Access::Refused,
    }
}

/// The formats this build writes and reads, from core's registries and check functions.
pub fn format_capabilities() -> FormatCapabilities {
    let written = formats();
    let mut known_features = registered_features();
    known_features.sort();
    // the probe's required version is any valid one; use a fixed one
    let rdy = "99.0.0";
    let features = written
        .reads_features
        .iter()
        .map(|&manifest_version| DumpFeaturePair {
            dump: written.dump,
            manifest_version,
        })
        .collect();
    FormatCapabilities {
        store_reads: StoreReads {
            manifest_versions: vec![MANIFEST_VERSION],
            known_features,
            unknown_features: UnknownFeatureAccess {
                compatible: unknown_access(FeatureClass::Compatible, rdy),
                read_only_compatible: unknown_access(FeatureClass::ReadOnlyCompatible, rdy),
                incompatible: unknown_access(FeatureClass::Incompatible, rdy),
            },
        },
        dump_reads: DumpReads {
            legacy: written.reads.clone(),
            features,
        },
        written,
    }
}
