//! The store's format (D-WWAK7): a feature manifest, not a format number. The manifest lists the
//! features a store uses, each with its class and the first rdy that knows it. The authoritative
//! copy is the log's ref `meta/store_features` (the manifest's canonical JSON); the `FORMAT` file
//! mirrors it, so an open can refuse a store before it opens the index or the log.
//!
//! An rdy that knows every feature of a manifest reads and writes the store. For a feature it
//! does not know, the class decides: `compatible` permits writes, `read_only_compatible` permits
//! reads only, and `incompatible` refuses the open with the feature's required version.
//!
//! The scalar formats of earlier rdy builds (`rdy store format`, `store_format <n>`) are refused
//! here: 10 to 18 moved with dump and load, a later number is no build's, and an earlier one is
//! re-created. Opening never writes a `FORMAT`: only `Store::create`, `Store::create_holder`, a
//! load and a feature's first use do.
use crate::error::StoreError;
use crate::fsutil;
use crate::segment::Location;
use rdy_model::Hash;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::path::Path;

/// The last scalar store format (store format 18, the pair (3, 18)). No rdy writes a scalar
/// format after it: this is a historical number, never the current store.
pub const LAST_SCALAR_STORE_FORMAT: u32 = 18;
/// The first store format that a dump carries (dl2, D-69AY3): dump format 1 carries 10 to 12,
/// dump format 2 carries 13 to 15, dump format 3 carries 16 to 18. A store of this format or a
/// later scalar one moves to a feature store with dump and load; an earlier one cannot.
pub const FIRST_DUMP_STORE_FORMAT: u32 = 10;
/// The mirror file in the store directory.
pub const FORMAT_FILE: &str = "FORMAT";
/// The historical log ref of a scalar store format, in ASCII decimal. A load keeps the ones its
/// dump holds as they are; no rdy writes one now.
pub const REF_STORE_FORMAT: &str = "meta/store_format";
/// The authoritative log ref of the store's features: the manifest's canonical JSON.
pub const REF_STORE_FEATURES: &str = "meta/store_features";
/// The log ref of the last store upgrade's receipt ([`UpgradeReceipt`]). Only the internal
/// upgrade transaction writes it.
pub const REF_UPGRADE: &str = "meta/upgrade";
/// The grammar of the `FORMAT` file and of the manifest.
pub const MANIFEST_VERSION: u32 = 1;
/// The most bytes of one manifest's JSON.
pub const MAX_MANIFEST_BYTES: usize = 65_536;
/// The most features of one manifest.
pub const MAX_FEATURES: usize = 256;
/// The most bytes of one upgrade receipt's JSON.
pub const MAX_RECEIPT_BYTES: usize = 65_536;
/// The most steps of one upgrade receipt.
pub const MAX_RECEIPT_STEPS: usize = 256;
/// The name of the feature tests inject ([`inject_test_feature`]); production never registers
/// it.
pub const TEST_FEATURE: &str = "test_feature";
/// The required version of the injected test feature.
pub const TEST_FEATURE_RDY: &str = "99.0.0";

/// The first line of the `FORMAT` file.
const FEATURES_LINE: &str = "rdy store features 1";
/// The first line of a scalar `FORMAT` file (store formats up to 18).
const SCALAR_LINE: &str = "rdy store format";

/// How an rdy that does not know a feature treats a store that uses it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureClass {
    /// Read and write it.
    Compatible,
    /// Read it only.
    ReadOnlyCompatible,
    /// Do not open it.
    Incompatible,
}

impl FeatureClass {
    /// The class's JSON text.
    pub fn as_str(self) -> &'static str {
        match self {
            FeatureClass::Compatible => "compatible",
            FeatureClass::ReadOnlyCompatible => "read_only_compatible",
            FeatureClass::Incompatible => "incompatible",
        }
    }

    fn parse(s: &str) -> Option<FeatureClass> {
        match s {
            "compatible" => Some(FeatureClass::Compatible),
            "read_only_compatible" => Some(FeatureClass::ReadOnlyCompatible),
            "incompatible" => Some(FeatureClass::Incompatible),
            _ => None,
        }
    }
}

/// One feature of a manifest: its name, its class and the first rdy that knows it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct StoreFeature {
    pub name: String,
    pub class: FeatureClass,
    pub required_rdy: String,
}

/// A store's features: the manifest grammar and the features, sorted by name. Its canonical JSON
/// is compact, with the fields in this order.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StoreFeatures {
    pub manifest_version: u32,
    pub features: Vec<StoreFeature>,
}

/// Why a manifest is invalid: one of the closed reasons (`unsupported manifest version`,
/// `manifest exceeds 65536 bytes`, `more than 256 features`, `duplicate feature`, `features are
/// not sorted`, `invalid feature name`, `invalid required_rdy`, `wrong feature class`, `unknown
/// field {key}` and `invalid JSON`).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ManifestError(pub String);

impl ManifestError {
    fn of(reason: &str) -> ManifestError {
        ManifestError(reason.to_string())
    }

    /// The store's refusal of a manifest with this reason (E_STORE_FORMAT).
    pub fn store_error(&self) -> StoreError {
        StoreError::Format {
            message: format!("store feature manifest is invalid: {}", self.0),
            fix: "restore the store from a backup".into(),
        }
    }
}

const INVALID_JSON: &str = "invalid JSON";

/// `text` as an error message echoes a key: its first 80 characters, with an ellipsis when there
/// are more.
fn clip(text: &str) -> String {
    match text.char_indices().nth(80) {
        Some((at, _)) => format!("{}\u{2026}", &text[..at]),
        None => text.to_string(),
    }
}

/// Whether `name` is a feature name: `[a-z][a-z0-9_]{0,63}`.
pub fn valid_feature_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && b[0].is_ascii_lowercase()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

/// Whether `v` is a required version: three decimal `u32` components joined by dots, with no
/// leading zero except zero itself.
pub fn valid_required_rdy(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.bytes().all(|c| c.is_ascii_digit())
                && (p.len() == 1 || !p.starts_with('0'))
                && p.parse::<u32>().is_ok()
        })
}

impl StoreFeatures {
    /// A manifest of `features`, sorted by name (the caller's are distinct).
    pub fn of(mut features: Vec<StoreFeature>) -> StoreFeatures {
        features.sort_by(|a, b| a.name.cmp(&b.name));
        StoreFeatures {
            manifest_version: MANIFEST_VERSION,
            features,
        }
    }

    /// The manifest's canonical JSON: compact, fields in declaration order.
    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a manifest always serializes")
    }

    /// `Hash::of` the canonical JSON.
    pub fn hash(&self) -> Hash {
        Hash::of(&self.to_json())
    }

    /// The `FORMAT` text of this manifest: `rdy store features 1`, then the canonical JSON, each
    /// on its own line.
    pub fn to_text(&self) -> String {
        format!(
            "{FEATURES_LINE}\n{}\n",
            String::from_utf8(self.to_json()).expect("JSON is UTF-8")
        )
    }

    /// Whether this manifest names feature `name`.
    pub fn has(&self, name: &str) -> bool {
        self.features.iter().any(|f| f.name == name)
    }

    /// This manifest with `add`'s features that it does not name, sorted by name. It never
    /// removes a feature, and keeps every entry it has as it is.
    pub fn merged(&self, add: &[StoreFeature]) -> StoreFeatures {
        let mut features = self.features.clone();
        for f in add {
            if !features.iter().any(|g| g.name == f.name) {
                features.push(f.clone());
            }
        }
        StoreFeatures {
            manifest_version: self.manifest_version,
            ..StoreFeatures::of(features)
        }
    }

    /// Whether every feature of `self` is in `other`, with the same class and version.
    pub fn is_subset_of(&self, other: &StoreFeatures) -> bool {
        self.manifest_version == other.manifest_version
            && self.features.iter().all(|f| other.features.contains(f))
    }

    /// The manifest's own rules: grammar 1, at most 256 features with valid names and versions,
    /// no repeat, sorted by name, and a known feature in its registered class. It allocates
    /// nothing, so a reader whose every allocation is fallible may call it.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.manifest_version != MANIFEST_VERSION {
            return Err(ManifestError::of("unsupported manifest version"));
        }
        if self.features.len() > MAX_FEATURES {
            return Err(ManifestError::of("more than 256 features"));
        }
        for f in &self.features {
            if !valid_feature_name(&f.name) {
                return Err(ManifestError::of("invalid feature name"));
            }
            if !valid_required_rdy(&f.required_rdy) {
                return Err(ManifestError::of("invalid required_rdy"));
            }
        }
        let fs = &self.features;
        if (0..fs.len()).any(|i| fs[i + 1..].iter().any(|g| g.name == fs[i].name)) {
            return Err(ManifestError::of("duplicate feature"));
        }
        if fs.windows(2).any(|w| w[0].name >= w[1].name) {
            return Err(ManifestError::of("features are not sorted"));
        }
        if fs
            .iter()
            .any(|f| known_class(&f.name).is_some_and(|c| c != f.class))
        {
            return Err(ManifestError::of("wrong feature class"));
        }
        Ok(())
    }

    /// [`StoreFeatures::parse`] for a reader whose every allocation is fallible (a dump's or a
    /// sync batch's header): the canonical JSON, each name and version taken by fallible growth,
    /// then [`StoreFeatures::validate`]. Any other text gets the refusal of the general parse
    /// (bounded by [`MAX_MANIFEST_BYTES`]), whose reason names the fault.
    pub fn parse_fallible(bytes: &[u8]) -> Result<StoreFeatures, ParseFailure> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(ParseFailure::Manifest(ManifestError::of(
                "manifest exceeds 65536 bytes",
            )));
        }
        match canonical(bytes) {
            Ok(Some(m)) => {
                m.validate().map_err(ParseFailure::Manifest)?;
                Ok(m)
            }
            Ok(None) => StoreFeatures::parse(bytes).map_err(ParseFailure::Manifest),
            Err(()) => Err(ParseFailure::Allocation),
        }
    }

    /// Parses and validates a manifest's JSON. The length is checked before anything is parsed;
    /// unknown keys, missing or mistyped fields and an unknown class are refused.
    pub fn parse(bytes: &[u8]) -> Result<StoreFeatures, ManifestError> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(ManifestError::of("manifest exceeds 65536 bytes"));
        }
        let invalid = || ManifestError::of(INVALID_JSON);
        let v: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| invalid())?;
        let top = v.as_object().ok_or_else(invalid)?;
        unknown_key(top, &["manifest_version", "features"])?;
        let version = top
            .get("manifest_version")
            .and_then(|v| v.as_u64())
            .ok_or_else(invalid)?;
        if version != u64::from(MANIFEST_VERSION) {
            return Err(ManifestError::of("unsupported manifest version"));
        }
        let list = top
            .get("features")
            .and_then(|v| v.as_array())
            .ok_or_else(invalid)?;
        if list.len() > MAX_FEATURES {
            return Err(ManifestError::of("more than 256 features"));
        }
        let mut features = Vec::with_capacity(list.len());
        for item in list {
            let o = item.as_object().ok_or_else(invalid)?;
            unknown_key(o, &["name", "class", "required_rdy"])?;
            let text = |k: &str| o.get(k).and_then(|v| v.as_str()).ok_or_else(invalid);
            let name = text("name")?;
            let class = FeatureClass::parse(text("class")?).ok_or_else(invalid)?;
            let required_rdy = text("required_rdy")?;
            features.push(StoreFeature {
                name: name.to_string(),
                class,
                required_rdy: required_rdy.to_string(),
            });
        }
        let m = StoreFeatures {
            manifest_version: MANIFEST_VERSION,
            features,
        };
        m.validate()?;
        Ok(m)
    }
}

/// Refuses an object key outside `allowed`: the first unknown key in name order, clipped.
fn unknown_key(
    o: &serde_json::Map<String, serde_json::Value>,
    allowed: &[&str],
) -> Result<(), ManifestError> {
    let mut unknown: Vec<&String> = o
        .keys()
        .filter(|k| !allowed.contains(&k.as_str()))
        .collect();
    unknown.sort();
    match unknown.first() {
        Some(k) => Err(ManifestError(format!("unknown field {}", clip(k)))),
        None => Ok(()),
    }
}

/// What a manifest lets this rdy do with its store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreAccess {
    ReadWrite,
    ReadOnly,
}

/// The refusal of a store that uses feature `f`, which this rdy does not know.
fn needs(f: &StoreFeature) -> StoreError {
    StoreError::Format {
        message: format!("store feature {} needs rdy {}", f.name, f.required_rdy),
        fix: format!("open the project with rdy {} or later", f.required_rdy),
    }
}

/// What this rdy may do with a store of `features`: an invalid manifest is refused; every known
/// feature permits writes; an unknown compatible one permits writes, an unknown read-only one
/// reads only, and an unknown incompatible one refuses the open. The first unknown incompatible
/// feature by name is the refusal (a valid manifest is sorted by name). It allocates nothing
/// but the refusal's text.
pub fn check_features(features: &StoreFeatures) -> Result<StoreAccess, StoreError> {
    features.validate().map_err(|e| e.store_error())?;
    let mut unknown = features
        .features
        .iter()
        .filter(|f| known_class(&f.name).is_none());
    if let Some(f) = unknown
        .clone()
        .find(|f| f.class == FeatureClass::Incompatible)
    {
        return Err(needs(f));
    }
    if unknown.any(|f| f.class == FeatureClass::ReadOnlyCompatible) {
        return Ok(StoreAccess::ReadOnly);
    }
    Ok(StoreAccess::ReadWrite)
}

/// [`check_features`] for a store this rdy writes: until a read-only open exists, a read-only
/// result is refused with the first unknown read-only feature's required build.
pub fn check_writable(features: &StoreFeatures) -> Result<(), StoreError> {
    match check_features(features)? {
        StoreAccess::ReadWrite => Ok(()),
        StoreAccess::ReadOnly => {
            let f = features
                .features
                .iter()
                .find(|f| {
                    known_class(&f.name).is_none() && f.class == FeatureClass::ReadOnlyCompatible
                })
                .expect("a read-only result has an unknown read-only feature");
            Err(needs(f))
        }
    }
}

/// Why a fallible manifest parse stopped: the manifest's refusal, or a refused allocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseFailure {
    Manifest(ManifestError),
    Allocation,
}

/// The registered class of feature `name`, if this rdy registers it. Allocates nothing.
fn known_class(name: &str) -> Option<FeatureClass> {
    TEST_FEATURES.with(|t| {
        t.borrow()
            .iter()
            .find(|(f, _)| f.name == name)
            .map(|(f, _)| f.class)
    })
}

/// A copy of `b` (ASCII) in a fallibly grown string; Err when the growth is refused.
fn try_text(b: &[u8]) -> Result<String, ()> {
    let mut s = String::new();
    s.try_reserve_exact(b.len()).map_err(|_| ())?;
    s.push_str(std::str::from_utf8(b).map_err(|_| ())?);
    Ok(s)
}

/// The canonical manifest JSON `b`, parsed with fallible growth only: Ok(None) when `b` is not
/// canonical, Err when a growth is refused. Names and versions are read as the characters their
/// grammars allow; [`StoreFeatures::validate`] checks the grammars.
fn canonical(b: &[u8]) -> Result<Option<StoreFeatures>, ()> {
    struct Cur<'a>(&'a [u8]);
    impl<'a> Cur<'a> {
        fn lit(&mut self, l: &[u8]) -> bool {
            match self.0.strip_prefix(l) {
                Some(rest) => {
                    self.0 = rest;
                    true
                }
                None => false,
            }
        }
        /// The bytes up to the next `"`, each one of `ok`.
        fn word(&mut self, ok: impl Fn(u8) -> bool) -> Option<&'a [u8]> {
            let n = self.0.iter().position(|c| *c == b'"')?;
            let (w, rest) = self.0.split_at(n);
            if !w.iter().all(|c| ok(*c)) {
                return None;
            }
            self.0 = &rest[1..];
            Some(w)
        }
    }
    let mut c = Cur(b);
    if !c.lit(br#"{"manifest_version":"#) {
        return Ok(None);
    }
    let digits = c.0.iter().take_while(|d| d.is_ascii_digit()).count();
    let Some(version) = std::str::from_utf8(&c.0[..digits])
        .ok()
        .and_then(|d| d.parse::<u32>().ok())
        .filter(|_| digits == 1 || c.0[0] != b'0')
    else {
        return Ok(None);
    };
    c.0 = &c.0[digits..];
    if !c.lit(br#","features":["#) {
        return Ok(None);
    }
    let mut features: Vec<StoreFeature> = Vec::new();
    let mut first = true;
    loop {
        if c.lit(b"]}") {
            break;
        }
        if !first && !c.lit(b",") {
            return Ok(None);
        }
        first = false;
        if features.len() > MAX_FEATURES {
            return Ok(None);
        }
        if !c.lit(br#"{"name":""#) {
            return Ok(None);
        }
        let Some(name) = c.word(|x| x.is_ascii_lowercase() || x.is_ascii_digit() || x == b'_')
        else {
            return Ok(None);
        };
        if !c.lit(br#","class":""#) {
            return Ok(None);
        }
        let Some(class) = c
            .word(|x| x.is_ascii_lowercase() || x == b'_')
            .and_then(|w| FeatureClass::parse(std::str::from_utf8(w).ok()?))
        else {
            return Ok(None);
        };
        if !c.lit(br#","required_rdy":""#) {
            return Ok(None);
        }
        let Some(required) = c.word(|x| x.is_ascii_digit() || x == b'.') else {
            return Ok(None);
        };
        if !c.lit(b"}") {
            return Ok(None);
        }
        features.try_reserve(1).map_err(|_| ())?;
        features.push(StoreFeature {
            name: try_text(name)?,
            class,
            required_rdy: try_text(required)?,
        });
    }
    if !c.0.is_empty() {
        return Ok(None);
    }
    Ok(Some(StoreFeatures {
        manifest_version: version,
        features,
    }))
}

/// The features a new store, and a legacy dump's load, start with.
pub fn baseline_features() -> StoreFeatures {
    StoreFeatures {
        manifest_version: 1,
        features: vec![],
    }
}

/// The features this rdy registers (production registers none), and the injected test feature
/// while a test holds it.
pub fn registered_features() -> Vec<StoreFeature> {
    TEST_FEATURES.with(|t| t.borrow().iter().map(|(f, _)| f.clone()).collect())
}

/// The features a new object of logical kind `kind` with body `body` uses: what its first write
/// activates in the batch that writes it. Production registers none.
pub fn required_features(kind: u8, _body: &[u8]) -> Vec<StoreFeature> {
    TEST_FEATURES.with(|t| {
        t.borrow()
            .iter()
            .filter(|(_, trigger)| *trigger == kind)
            .map(|(f, _)| f.clone())
            .collect()
    })
}

thread_local! {
    /// The injected test feature of this thread, with the logical kind whose objects use it.
    static TEST_FEATURES: RefCell<Vec<(StoreFeature, u8)>> = const { RefCell::new(Vec::new()) };
}

/// While held, this thread registers [`TEST_FEATURE`] (required version [`TEST_FEATURE_RDY`]).
#[doc(hidden)]
#[must_use]
pub struct TestFeature(());

impl Drop for TestFeature {
    fn drop(&mut self) {
        TEST_FEATURES.with(|t| t.borrow_mut().clear());
    }
}

/// Test hook: registers [`TEST_FEATURE`] of `class` on this thread, used by every new object of
/// logical kind `trigger`, until the guard drops. No environment, CLI or MCP path reaches it.
#[doc(hidden)]
pub fn inject_test_feature(class: FeatureClass, trigger: u8) -> TestFeature {
    TEST_FEATURES.with(|t| {
        *t.borrow_mut() = vec![(
            StoreFeature {
                name: TEST_FEATURE.into(),
                class,
                required_rdy: TEST_FEATURE_RDY.into(),
            },
            trigger,
        )]
    });
    TestFeature(())
}

/// The store a dump's frames are of: a scalar store format (dump formats 1 to 3) or a feature
/// manifest (dump format 4). JSON writes `{"legacy": 18}` or `{"features": {...}}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum DumpStore {
    Legacy(u32),
    Features(StoreFeatures),
}

impl std::fmt::Display for DumpStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DumpStore::Legacy(n) => write!(f, "store format {n}"),
            DumpStore::Features(m) => write!(
                f,
                "store features {}",
                String::from_utf8_lossy(&m.to_json())
            ),
        }
    }
}

/// The receipt of a store upgrade (`meta/upgrade`): the manifest hashes before and after, its
/// steps in execution order, the sealed end it started from and the name of its backup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpgradeReceipt {
    #[serde(with = "crate::transition::hex::one")]
    pub from: Hash,
    #[serde(with = "crate::transition::hex::one")]
    pub to: Hash,
    pub steps: Vec<String>,
    pub source_end: Location,
    pub backup: String,
}

impl UpgradeReceipt {
    /// The receipt's canonical JSON.
    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a receipt always serializes")
    }

    /// Parses and validates a receipt: at most 65,536 bytes, no unknown key, at most 256
    /// distinct steps with feature names, and a backup named
    /// `.store-upgrade-<64 hex>-<64 hex>` in lower case. Err is the refusal's text.
    pub fn parse(bytes: &[u8]) -> Result<UpgradeReceipt, String> {
        if bytes.len() > MAX_RECEIPT_BYTES {
            return Err("upgrade receipt exceeds 65536 bytes".into());
        }
        let r: UpgradeReceipt = serde_json::from_slice(bytes)
            .map_err(|_| "upgrade receipt does not decode".to_string())?;
        r.validate()?;
        Ok(r)
    }

    /// The receipt's own rules (see [`UpgradeReceipt::parse`]).
    pub fn validate(&self) -> Result<(), String> {
        if self.steps.len() > MAX_RECEIPT_STEPS {
            return Err("upgrade receipt has more than 256 steps".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for s in &self.steps {
            if !valid_feature_name(s) || !seen.insert(s.as_str()) {
                return Err(format!("upgrade receipt step {} is not valid", clip(s)));
            }
        }
        let hex64 =
            |s: &str| s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        let ok = self
            .backup
            .strip_prefix(".store-upgrade-")
            .and_then(|rest| rest.split_once('-'))
            .is_some_and(|(a, b)| hex64(a) && hex64(b));
        if !ok {
            return Err("upgrade receipt names an invalid backup".into());
        }
        Ok(())
    }
}

/// The fix for a refusal with no store format to go by.
const RE_CREATE: &str = "re-create it with this rdy";

/// The in-place move of a project across a store format change (final ruling 1, worded by
/// final-round1 ruling 2), in order. `found` is the store format the project has now: the rdy
/// that wrote the store may have no `rdy dump` (0.2.0 and earlier), so the fix names a release
/// that has one and reads that format.
pub fn move_with_dump(found: &str) -> String {
    format!(
        "close every session on this project; with rdy 0.3.0 or a later rdy that reads store \
         format {found}, run rdy dump --out <file>, then rdy serve --stop; move .rdy/store aside; \
         then run rdy load <file> . with this rdy"
    )
}

/// The fix text for a store of scalar format `found` (dl2, a closed rule): a scalar format from
/// `FIRST_DUMP_STORE_FORMAT` to `LAST_SCALAR_STORE_FORMAT` moves with dump and load, a later one
/// opens only with the build that wrote it, and an earlier one is re-created.
pub fn fix_for(found: u32) -> String {
    if found > LAST_SCALAR_STORE_FORMAT {
        "open the project with the rdy build that wrote it".into()
    } else if found >= FIRST_DUMP_STORE_FORMAT {
        "dump with the rdy that wrote this store, then load into a directory with no store".into()
    } else {
        RE_CREATE.into()
    }
}

/// The refusal of a store of scalar format `n` at `store_dir` (dl2): `store format {n} predates
/// feature-based stores` for 10 to 18, `store format {n} is not supported` past 18, and the
/// other-build refusal before 10.
pub fn scalar_refusal(store_dir: &Path, n: u32) -> StoreError {
    let message = if n > LAST_SCALAR_STORE_FORMAT {
        format!("store format {n} is not supported")
    } else if n >= FIRST_DUMP_STORE_FORMAT {
        format!("store format {n} predates feature-based stores")
    } else {
        return other_format(store_dir, &format!("has store format {n}"), RE_CREATE);
    };
    StoreError::Format {
        message,
        fix: fix_for(n),
    }
}

/// The one refusal for a store this binary does not open, with the fix the caller chooses:
/// `what` says how the store at `store_dir` tells it was written by another build.
pub(crate) fn other_format(store_dir: &Path, what: &str, fix: &str) -> StoreError {
    StoreError::Format {
        message: format!(
            "this store was written by a different rdy build: {} {what}, and this rdy uses \
             feature-based stores",
            store_dir.display()
        ),
        fix: fix.into(),
    }
}

/// The refusal for a store with no format to go by (the fix is to re-create it).
pub(crate) fn other_build(store_dir: &Path, what: &str) -> StoreError {
    other_format(store_dir, what, RE_CREATE)
}

/// The refusal for a store path where there is nothing: `rdy init` creates a store, and opening
/// never does.
pub(crate) fn no_store(store_dir: &Path) -> StoreError {
    StoreError::Format {
        message: format!(
            "there is no store at {}: rdy init creates one and opening never does, and this rdy \
             uses feature-based stores",
            store_dir.display()
        ),
        fix: RE_CREATE.into(),
    }
}

/// The refusal of a log whose features its `FORMAT` file does not list.
pub(crate) fn unmirrored(store_dir: &Path) -> StoreError {
    other_format(
        store_dir,
        "has store features its FORMAT file does not list",
        "restore FORMAT from backup, or re-create it with this rdy",
    )
}

/// The most bytes of a `FORMAT` file: its first line and a manifest of the largest size.
const MAX_FORMAT_FILE_BYTES: u64 = (FEATURES_LINE.len() + 1 + MAX_MANIFEST_BYTES + 1) as u64;

/// Parses `FORMAT` text. Err is the refusal (a scalar store, an invalid manifest, or text that
/// is no `FORMAT` file); `path` and `store_dir` name the file and its store in the texts.
fn parse_text(text: &str, path: &Path, store_dir: &Path) -> Result<StoreFeatures, StoreError> {
    let unreadable = |reason: &str| StoreError::Format {
        message: format!(
            "the store's FORMAT file at {} is unreadable: {reason}",
            path.display()
        ),
        fix: "restore FORMAT from backup, or re-create it with this rdy".into(),
    };
    let (first, rest) = text.split_once('\n').unwrap_or((text, ""));
    if first == SCALAR_LINE {
        let mut store_format = None;
        for line in rest.lines() {
            let number = line
                .split_once(' ')
                .and_then(|(key, n)| decode_u32(n.as_bytes()).map(|n| (key, n)));
            match number {
                Some(("store_format", n)) => store_format = Some(n),
                Some(_) => {}
                None => {
                    return Err(unreadable(&format!(
                        "bad line {line:?}: expected `<key> <number>`"
                    )));
                }
            }
        }
        return Err(match store_format {
            Some(n) => scalar_refusal(store_dir, n),
            None => other_build(store_dir, "has a FORMAT file with no store_format line"),
        });
    }
    if first != FEATURES_LINE {
        return Err(unreadable(&format!(
            "the first line is not {FEATURES_LINE:?}"
        )));
    }
    let Some(json) = rest.strip_suffix('\n').filter(|j| !j.contains('\n')) else {
        return Err(ManifestError::of(INVALID_JSON).store_error());
    };
    StoreFeatures::parse(json.as_bytes()).map_err(|e| e.store_error())
}

/// Reads `<store_dir>/FORMAT`: None if the file does not exist. A scalar store's file, and a
/// file that is not a valid manifest, are refused; a file longer than a manifest can make it is
/// refused before it is read.
pub fn read_format_file(store_dir: &Path) -> Result<Option<StoreFeatures>, StoreError> {
    let path = store_dir.join(FORMAT_FILE);
    let too_long = || ManifestError::of("manifest exceeds 65536 bytes").store_error();
    let len = match std::fs::metadata(&path) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if len > MAX_FORMAT_FILE_BYTES {
        return Err(too_long());
    }
    let bytes = std::fs::read(&path)?;
    if bytes.len() as u64 > MAX_FORMAT_FILE_BYTES {
        return Err(too_long());
    }
    let text = String::from_utf8(bytes).map_err(|_| StoreError::Format {
        message: format!(
            "the store's FORMAT file at {} is unreadable: it is not UTF-8 text",
            path.display()
        ),
        fix: "restore FORMAT from backup, or re-create it with this rdy".into(),
    })?;
    parse_text(&text, &path, store_dir).map(Some)
}

/// Writes `<store_dir>/FORMAT` durably, as the mirror of `features`.
pub fn write_format_file(store_dir: &Path, features: &StoreFeatures) -> Result<(), StoreError> {
    fsutil::write_durable(&store_dir.join(FORMAT_FILE), features.to_text().as_bytes())?;
    Ok(())
}

/// The manifest a `meta/store_features` value holds; an invalid one is the manifest refusal.
pub fn decode_features(value: &[u8]) -> Result<StoreFeatures, StoreError> {
    StoreFeatures::parse(value).map_err(|e| e.store_error())
}

pub fn encode_u32(n: u32) -> Vec<u8> {
    n.to_string().into_bytes()
}

/// Strict ASCII decimal: digits only, no sign, no whitespace.
pub fn decode_u32(b: &[u8]) -> Option<u32> {
    if b.is_empty() || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(b).ok()?.parse().ok()
}
