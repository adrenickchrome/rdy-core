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
//! re-created. Only `Store::create`, `Store::create_holder`, a load and a feature's first use
//! write a `FORMAT`; an open writes one only to put back a `FORMAT` that lists more than its
//! sealed log (the leftover of a first use whose batch never sealed), because the log's ref is
//! the authority.
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
/// The first rdy that knows the registered features (O20): the release that carries them.
pub const FEATURE_REQUIRED_RDY: &str = "0.23.0";
/// A version's stored readiness assessment (ASSESSMENT, a landing's `assessment`).
pub const FEATURE_ASSESSMENT: &str = "assessment";
/// A check request (CHECK_REQUEST, a Check review or verdict, an AcceptVerdict answer).
pub const FEATURE_CHECK_REQUEST: &str = "check_request";
/// Content identity without sibling order (content-2, D-TX3KH): no first-use record.
pub const FEATURE_CONTENT_HASH_V2: &str = "content_hash_v2";
/// A dispute and its resolution (DISPUTE, DISPUTE_RESOLUTION, a Dispute or ResolveDispute
/// answer).
pub const FEATURE_DISPUTE: &str = "dispute";
/// Versioned object encodings (D-WWAK7): any frame of encoding 1 or more.
pub const FEATURE_OBJECT_ENCODING: &str = "object_encoding";
/// A tension plan item (PlanKind::Tension, PlanAct::Dismiss, PlanOutcome::Dismissed).
pub const FEATURE_PLAN_TENSION: &str = "plan_tension";
/// A replayed change (`ChangeLinks::replayed_from`).
pub const FEATURE_VIEW_REPLAY: &str = "view_replay";

/// The features this rdy registers (O11, D-1HDVA), sorted by name. Each is `incompatible` and
/// needs [`FEATURE_REQUIRED_RDY`].
pub const REGISTERED_FEATURES: [&str; 7] = [
    FEATURE_ASSESSMENT,
    FEATURE_CHECK_REQUEST,
    FEATURE_CONTENT_HASH_V2,
    FEATURE_DISPUTE,
    FEATURE_OBJECT_ENCODING,
    FEATURE_PLAN_TENSION,
    FEATURE_VIEW_REPLAY,
];

/// The registered feature `name`, with its class and required version.
pub(crate) fn registered(name: &str) -> StoreFeature {
    StoreFeature {
        name: name.into(),
        class: FeatureClass::Incompatible,
        required_rdy: FEATURE_REQUIRED_RDY.into(),
    }
}

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

    /// The error of `reason`, in fallibly grown storage: a refused allocation is `Allocation`,
    /// so a fallible reader's refusal never aborts.
    fn try_of(reason: &str) -> Result<ManifestError, ParseFailure> {
        let mut text = String::new();
        text.try_reserve_exact(reason.len())
            .map_err(|_| ParseFailure::Allocation)?;
        text.push_str(reason);
        Ok(ManifestError(text))
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

/// The most bytes of a feature name.
const MAX_NAME_BYTES: usize = 64;
/// The most bytes of a valid required version: three `u32` components and two dots.
const MAX_REQUIRED_RDY_BYTES: usize = 32;

/// Whether `v` is a required version: three decimal `u32` components joined by dots, with no
/// leading zero except zero itself. It allocates nothing: the components are counted as they
/// are read.
pub fn valid_required_rdy(v: &str) -> bool {
    let mut count = 0;
    for p in v.split('.') {
        count += 1;
        let valid = !p.is_empty()
            && p.bytes().all(|c| c.is_ascii_digit())
            && (p.len() == 1 || !p.starts_with('0'))
            && p.parse::<u32>().is_ok();
        if count > 3 || !valid {
            return false;
        }
    }
    count == 3
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
    /// nothing but the refusal's text.
    pub fn validate(&self) -> Result<(), ManifestError> {
        self.rule().map_err(ManifestError::of)
    }

    /// The first manifest rule this manifest breaks, as its reason ([`StoreFeatures::validate`]).
    /// It allocates nothing.
    fn rule(&self) -> Result<(), &'static str> {
        if self.manifest_version != MANIFEST_VERSION {
            return Err("unsupported manifest version");
        }
        if self.features.len() > MAX_FEATURES {
            return Err("more than 256 features");
        }
        for f in &self.features {
            if !valid_feature_name(&f.name) {
                return Err("invalid feature name");
            }
            if !valid_required_rdy(&f.required_rdy) {
                return Err("invalid required_rdy");
            }
        }
        let fs = &self.features;
        if (0..fs.len()).any(|i| fs[i + 1..].iter().any(|g| g.name == fs[i].name)) {
            return Err("duplicate feature");
        }
        if fs.windows(2).any(|w| w[0].name >= w[1].name) {
            return Err("features are not sorted");
        }
        if fs
            .iter()
            .any(|f| known_class(&f.name).is_some_and(|c| c != f.class))
        {
            return Err("wrong feature class");
        }
        Ok(())
    }

    /// [`StoreFeatures::validate`] for a manifest a batch is to write (a first use's or a take's
    /// merged manifest): its rules, and its canonical JSON within 65,536 bytes, so every reader
    /// of the ref and of `FORMAT` accepts what is written.
    pub fn validate_written(&self) -> Result<(), ManifestError> {
        self.validate()?;
        if self.to_json().len() > MAX_MANIFEST_BYTES {
            return Err(ManifestError::of("manifest exceeds 65536 bytes"));
        }
        Ok(())
    }

    /// [`StoreFeatures::parse`] for a reader whose every allocation is fallible (a dump's or a
    /// sync batch's header). Any JSON text is read by the bounded reader ([`json`]): the whole
    /// text is checked with no allocation, the feature count before any storage grows for it,
    /// and each name and version is taken by fallible growth (`Allocation` when it is refused),
    /// then the manifest's rules. A name or version longer than any valid one is kept as the
    /// empty text, which the rules refuse in its place, so no request grows with the input. The
    /// refusal reasons are the closed manifest reasons, and their texts grow fallibly too.
    pub fn parse_fallible(bytes: &[u8]) -> Result<StoreFeatures, ParseFailure> {
        let refuse = |reason: &str| match ManifestError::try_of(reason) {
            Ok(e) => ParseFailure::Manifest(e),
            Err(a) => a,
        };
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(refuse("manifest exceeds 65536 bytes"));
        }
        let invalid = || refuse(INVALID_JSON);
        let b = bytes;
        let top = json::document(b).ok_or_else(invalid)?;
        if b[top] != b'{' {
            return Err(invalid());
        }
        // the last of a repeated key wins; the first unknown key in name order is refused
        let (mut version, mut list) = (None, None);
        let mut unknown: Option<json::Span> = None;
        for (key, value) in json::members(b, top) {
            if json::is(b, key, "manifest_version") {
                version = Some(value);
            } else if json::is(b, key, "features") {
                list = Some(value);
            } else if unknown.is_none_or(|u| json::key_order(b, key, u).is_lt()) {
                unknown = Some(key);
            }
        }
        if let Some(k) = unknown {
            return Err(ParseFailure::Manifest(unknown_field(b, k)?));
        }
        let version = version
            .and_then(|v| json::as_u64(&b[v.0..v.1]))
            .ok_or_else(invalid)?;
        if version != u64::from(MANIFEST_VERSION) {
            return Err(refuse("unsupported manifest version"));
        }
        let list = list.filter(|v| b[v.0] == b'[').ok_or_else(invalid)?;
        let count = json::elements(b, list.0).count();
        if count > MAX_FEATURES {
            return Err(refuse("more than 256 features"));
        }
        let mut features: Vec<StoreFeature> = Vec::new();
        features
            .try_reserve_exact(count)
            .map_err(|_| ParseFailure::Allocation)?;
        for item in json::elements(b, list.0) {
            if b[item.0] != b'{' {
                return Err(invalid());
            }
            let (mut name, mut class, mut required) = (None, None, None);
            let mut unknown: Option<json::Span> = None;
            for (key, value) in json::members(b, item.0) {
                if json::is(b, key, "name") {
                    name = Some(value);
                } else if json::is(b, key, "class") {
                    class = Some(value);
                } else if json::is(b, key, "required_rdy") {
                    required = Some(value);
                } else if unknown.is_none_or(|u| json::key_order(b, key, u).is_lt()) {
                    unknown = Some(key);
                }
            }
            if let Some(k) = unknown {
                return Err(ParseFailure::Manifest(unknown_field(b, k)?));
            }
            // a string value's text, without its quotes
            let text = |v: Option<json::Span>| {
                v.filter(|v| b[v.0] == b'"')
                    .map(|v| (v.0 + 1, v.1 - 1))
                    .ok_or_else(invalid)
            };
            let name = text(name)?;
            let class = text(class)?;
            let class = [
                FeatureClass::Compatible,
                FeatureClass::ReadOnlyCompatible,
                FeatureClass::Incompatible,
            ]
            .into_iter()
            .find(|c| json::is(b, class, c.as_str()))
            .ok_or_else(invalid)?;
            let required = text(required)?;
            features.push(StoreFeature {
                name: json::decode(b, name, MAX_NAME_BYTES).ok_or(ParseFailure::Allocation)?,
                class,
                required_rdy: json::decode(b, required, MAX_REQUIRED_RDY_BYTES)
                    .ok_or(ParseFailure::Allocation)?,
            });
        }
        let m = StoreFeatures {
            manifest_version: MANIFEST_VERSION,
            features,
        };
        m.rule().map_err(refuse)?;
        Ok(m)
    }

    /// Parses and validates a manifest's JSON, as [`StoreFeatures::parse_fallible`] does: the
    /// length is checked before anything is parsed; unknown keys, missing or mistyped fields and
    /// an unknown class are refused. A refused allocation is the allocator's failure, as for any
    /// infallible collection.
    pub fn parse(bytes: &[u8]) -> Result<StoreFeatures, ManifestError> {
        match StoreFeatures::parse_fallible(bytes) {
            Ok(m) => Ok(m),
            Err(ParseFailure::Manifest(e)) => Err(e),
            Err(ParseFailure::Allocation) => {
                std::alloc::handle_alloc_error(std::alloc::Layout::new::<StoreFeature>())
            }
        }
    }
}

/// The refusal of the unknown key `key` (a string's text span in `b`), clipped as [`clip`]
/// clips it: only the clipped text is read, into fallibly grown storage.
fn unknown_field(b: &[u8], key: json::Span) -> Result<ManifestError, ParseFailure> {
    const PREFIX: &str = "unknown field ";
    const ELLIPSIS: char = '\u{2026}';
    let raw = &b[key.0..key.1];
    let kept = || json::chars(raw).take(80);
    let more = json::chars(raw).nth(80).is_some();
    let len = PREFIX.len()
        + kept().map(char::len_utf8).sum::<usize>()
        + if more { ELLIPSIS.len_utf8() } else { 0 };
    let mut text = String::new();
    text.try_reserve_exact(len)
        .map_err(|_| ParseFailure::Allocation)?;
    text.push_str(PREFIX);
    text.extend(kept());
    if more {
        text.push(ELLIPSIS);
    }
    Ok(ManifestError(text))
}

/// A bounded JSON reader. [`document`] checks a whole text with no allocation and the accept set
/// of `serde_json::from_slice`: RFC 8259's grammar, UTF-8, string escapes with no lone
/// surrogate, numbers that fit an `f64`, and at most 127 nested arrays and objects. The walkers
/// then read values by their byte spans; none allocates but [`decode`], which grows fallibly.
mod json {
    use std::cmp::Ordering;

    /// A value's bytes in the text: `[start, end)`; for a string's text, without its quotes.
    pub(super) type Span = (usize, usize);

    /// The most nested arrays and objects, plus one (serde_json's recursion limit).
    const DEPTH: usize = 128;

    fn ws(b: &[u8], mut i: usize) -> usize {
        while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | b'\r') {
            i += 1;
        }
        i
    }

    /// Where the one value of text `b` starts, if `b` is valid JSON: UTF-8, one value and
    /// whitespace around it.
    pub(super) fn document(b: &[u8]) -> Option<usize> {
        std::str::from_utf8(b).ok()?;
        let start = ws(b, 0);
        let end = value(b, start, DEPTH)?;
        (ws(b, end) == b.len()).then_some(start)
    }

    /// The end of the valid value at `i`, with `depth` containers still allowed.
    fn value(b: &[u8], i: usize, depth: usize) -> Option<usize> {
        match *b.get(i)? {
            b'{' => {
                let depth = depth - 1;
                if depth == 0 {
                    return None;
                }
                let mut j = ws(b, i + 1);
                if *b.get(j)? == b'}' {
                    return Some(j + 1);
                }
                loop {
                    j = string(b, j)?;
                    j = ws(b, j);
                    if *b.get(j)? != b':' {
                        return None;
                    }
                    j = value(b, ws(b, j + 1), depth)?;
                    j = ws(b, j);
                    match *b.get(j)? {
                        b',' => j = ws(b, j + 1),
                        b'}' => return Some(j + 1),
                        _ => return None,
                    }
                }
            }
            b'[' => {
                let depth = depth - 1;
                if depth == 0 {
                    return None;
                }
                let mut j = ws(b, i + 1);
                if *b.get(j)? == b']' {
                    return Some(j + 1);
                }
                loop {
                    j = value(b, j, depth)?;
                    j = ws(b, j);
                    match *b.get(j)? {
                        b',' => j = ws(b, j + 1),
                        b']' => return Some(j + 1),
                        _ => return None,
                    }
                }
            }
            b'"' => string(b, i),
            b't' => literal(b, i, b"true"),
            b'f' => literal(b, i, b"false"),
            b'n' => literal(b, i, b"null"),
            b'-' | b'0'..=b'9' => number(b, i),
            _ => None,
        }
    }

    fn literal(b: &[u8], i: usize, word: &[u8]) -> Option<usize> {
        b[i..].starts_with(word).then_some(i + word.len())
    }

    fn digits(b: &[u8], mut i: usize) -> usize {
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        i
    }

    /// The end of the number at `i`: `-? (0 | [1-9][0-9]*) (. [0-9]+)? ([eE] [+-]? [0-9]+)?`,
    /// whose value is a finite `f64`.
    fn number(b: &[u8], i: usize) -> Option<usize> {
        let mut j = i + usize::from(b[i] == b'-');
        match *b.get(j)? {
            b'0' => j += 1,
            b'1'..=b'9' => j = digits(b, j),
            _ => return None,
        }
        if b.get(j) == Some(&b'.') {
            let k = digits(b, j + 1);
            if k == j + 1 {
                return None;
            }
            j = k;
        }
        if matches!(b.get(j), Some(b'e' | b'E')) {
            j += 1;
            if matches!(b.get(j), Some(b'+' | b'-')) {
                j += 1;
            }
            let k = digits(b, j);
            if k == j {
                return None;
            }
            j = k;
        }
        let text = std::str::from_utf8(&b[i..j]).ok()?;
        text.parse::<f64>().ok().filter(|v| v.is_finite())?;
        Some(j)
    }

    fn hex4(b: &[u8], i: usize) -> Option<u16> {
        let h = b.get(i..i + 4)?;
        let text = std::str::from_utf8(h).ok()?;
        if !h.iter().all(u8::is_ascii_hexdigit) {
            return None;
        }
        u16::from_str_radix(text, 16).ok()
    }

    /// The end of the valid string at `i` (just past its closing quote).
    fn string(b: &[u8], i: usize) -> Option<usize> {
        if *b.get(i)? != b'"' {
            return None;
        }
        let mut j = i + 1;
        loop {
            match *b.get(j)? {
                b'"' => return Some(j + 1),
                b'\\' => match *b.get(j + 1)? {
                    b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => j += 2,
                    b'u' => {
                        let n = hex4(b, j + 2)?;
                        j += 6;
                        match n {
                            0xDC00..=0xDFFF => return None,
                            0xD800..=0xDBFF => {
                                if b.get(j..j + 2)? != b"\\u" {
                                    return None;
                                }
                                if !(0xDC00..=0xDFFF).contains(&hex4(b, j + 2)?) {
                                    return None;
                                }
                                j += 6;
                            }
                            _ => {}
                        }
                    }
                    _ => return None,
                },
                0..=0x1f => return None,
                _ => j += 1,
            }
        }
    }

    /// The members of the valid object at `open` (its `{`): each key's text span and value span.
    pub(super) fn members(b: &[u8], open: usize) -> impl Iterator<Item = (Span, Span)> + '_ {
        let mut i = open + 1;
        std::iter::from_fn(move || {
            let mut j = ws(b, i);
            match *b.get(j)? {
                b'}' => return None,
                b',' => j = ws(b, j + 1),
                _ => {}
            }
            let key_end = string(b, j)?;
            let start = ws(b, ws(b, key_end) + 1);
            let end = value(b, start, usize::MAX)?;
            i = end;
            Some(((j + 1, key_end - 1), (start, end)))
        })
    }

    /// The element spans of the valid array at `open` (its `[`).
    pub(super) fn elements(b: &[u8], open: usize) -> impl Iterator<Item = Span> + '_ {
        let mut i = open + 1;
        std::iter::from_fn(move || {
            let mut j = ws(b, i);
            match *b.get(j)? {
                b']' => return None,
                b',' => j = ws(b, j + 1),
                _ => {}
            }
            let end = value(b, j, usize::MAX)?;
            i = end;
            Some((j, end))
        })
    }

    /// The characters of a valid string's text `raw` (between its quotes), escapes decoded.
    pub(super) fn chars(raw: &[u8]) -> impl Iterator<Item = char> + '_ {
        let mut i = 0;
        std::iter::from_fn(move || {
            let c = *raw.get(i)?;
            if c != b'\\' {
                // the one character at `i` (the text is valid UTF-8): its lead byte's width
                let width = match c {
                    0..=0x7f => 1,
                    0xc0..=0xdf => 2,
                    0xe0..=0xef => 3,
                    _ => 4,
                };
                let one = std::str::from_utf8(raw.get(i..i + width)?).ok()?;
                let ch = one.chars().next()?;
                i += ch.len_utf8();
                return Some(ch);
            }
            let e = *raw.get(i + 1)?;
            i += 2;
            Some(match e {
                b'b' => '\u{8}',
                b'f' => '\u{c}',
                b'n' => '\n',
                b'r' => '\r',
                b't' => '\t',
                b'u' => {
                    let n = u32::from(hex4(raw, i)?);
                    i += 4;
                    if (0xD800..=0xDBFF).contains(&n) {
                        let low = u32::from(hex4(raw, i + 2)?);
                        i += 6;
                        char::from_u32(0x10000 + ((n - 0xD800) << 10) + (low - 0xDC00))?
                    } else {
                        char::from_u32(n)?
                    }
                }
                other => char::from(other),
            })
        })
    }

    /// Whether the string text at `key` is `lit`.
    pub(super) fn is(b: &[u8], key: Span, lit: &str) -> bool {
        chars(&b[key.0..key.1]).eq(lit.chars())
    }

    /// The order of two string texts by their characters (a `String`'s order).
    pub(super) fn key_order(b: &[u8], x: Span, y: Span) -> Ordering {
        chars(&b[x.0..x.1]).cmp(chars(&b[y.0..y.1]))
    }

    /// The decoded string text at `text`, in fallibly grown storage; None when it is refused.
    /// A text of more than `most` bytes is the empty text, with nothing allocated.
    pub(super) fn decode(b: &[u8], text: Span, most: usize) -> Option<String> {
        let raw = &b[text.0..text.1];
        let len: usize = chars(raw).map(char::len_utf8).sum();
        if len > most {
            return Some(String::new());
        }
        let mut s = String::new();
        s.try_reserve_exact(len).ok()?;
        s.extend(chars(raw));
        Some(s)
    }

    /// A number token's value when it is a `u64` (no sign, fraction or exponent, and in range).
    pub(super) fn as_u64(token: &[u8]) -> Option<u64> {
        if token.is_empty() || !token.iter().all(u8::is_ascii_digit) {
            return None;
        }
        token.iter().try_fold(0u64, |n, d| {
            n.checked_mul(10)?.checked_add(u64::from(d - b'0'))
        })
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
    if REGISTERED_FEATURES.contains(&name) {
        return Some(FeatureClass::Incompatible);
    }
    TEST_FEATURES.with(|t| {
        t.borrow()
            .iter()
            .find(|(f, _)| f.name == name)
            .map(|(f, _)| f.class)
    })
}

/// The features a new store, and a legacy dump's load, start with: content-2 identity and
/// versioned encodings, which every store this rdy writes uses.
pub fn baseline_features() -> StoreFeatures {
    StoreFeatures {
        manifest_version: 1,
        features: vec![
            registered(FEATURE_CONTENT_HASH_V2),
            registered(FEATURE_OBJECT_ENCODING),
        ],
    }
}

/// The features this rdy registers ([`REGISTERED_FEATURES`]), and the injected test feature
/// while a test holds it.
pub fn registered_features() -> Vec<StoreFeature> {
    let mut out: Vec<StoreFeature> = REGISTERED_FEATURES.iter().map(|n| registered(n)).collect();
    TEST_FEATURES.with(|t| out.extend(t.borrow().iter().map(|(f, _)| f.clone())));
    out
}

/// The features a new object uses: what its first write activates in the batch that writes it.
/// `raw` is the frame's raw kind and `payload` its stored payload (`crate::codec`). A frame of
/// encoding 1 or more uses `object_encoding`; a record that uses a carried shape uses that
/// shape's feature (D-1HDVA):
///
/// - `view_replay`: a CHANGE whose `links.replayed_from` is set;
/// - `assessment`: an ASSESSMENT, or a LANDING whose `assessment` is set;
/// - `plan_tension`: a PLAN with a sealed item of kind Tension or outcome Dismissed, or an answer
///   with act Dismiss;
/// - `dispute`: a DISPUTE, a DISPUTE_RESOLUTION, or an ANSWER with act Dispute or
///   ResolveDispute;
/// - `check_request`: a CHECK_REQUEST, a REVIEW with subject Check or verdict Pass, Concern or
///   Fail, or an ANSWER with act AcceptVerdict.
///
/// A frame that does not split or decode uses nothing here: its own checks refuse it.
pub fn required_features(raw: u8, payload: &[u8]) -> Vec<StoreFeature> {
    use crate::kinds;
    let mut out: Vec<StoreFeature> = vec![];
    let Ok(o) = crate::codec::split(raw, payload) else {
        return out;
    };
    let mut add = |name: &str| {
        if !out.iter().any(|f| f.name == name) {
            out.push(registered(name));
        }
    };
    if o.version >= 1 {
        add(FEATURE_OBJECT_ENCODING);
    }
    fn typed<T: serde::de::DeserializeOwned + 'static>(
        o: &crate::codec::ObjectBytes<'_>,
    ) -> Option<T> {
        crate::codec::decode_bounded_split(o).ok()
    }
    // an encoding-0 change or landing has no carried field: it is not decoded
    match o.kind {
        kinds::CHANGE if o.version >= 1 => {
            if typed::<rdy_model::Change>(&o).is_some_and(|c| c.links.replayed_from.is_some()) {
                add(FEATURE_VIEW_REPLAY);
            }
        }
        kinds::LANDING if o.version >= 1 => {
            if typed::<rdy_model::LandingRecord>(&o).is_some_and(|l| l.assessment.is_some()) {
                add(FEATURE_ASSESSMENT);
            }
        }
        kinds::ASSESSMENT => add(FEATURE_ASSESSMENT),
        kinds::PLAN => {
            use rdy_model::plan::{PlanAct, PlanKind, PlanOutcome};
            if let Ok(p) = crate::plan::decode_plan(o.body) {
                let sealed =
                    p.seal.iter().flat_map(|s| &s.items).any(|i| {
                        i.kind == PlanKind::Tension || i.outcome == PlanOutcome::Dismissed
                    });
                if sealed || p.answers.iter().any(|a| a.act == PlanAct::Dismiss) {
                    add(FEATURE_PLAN_TENSION);
                }
            }
        }
        kinds::DISPUTE | kinds::DISPUTE_RESOLUTION => add(FEATURE_DISPUTE),
        kinds::ANSWER => {
            use rdy_model::AnswerAct;
            match typed::<rdy_model::Answer>(&o).map(|a| a.act) {
                Some(AnswerAct::Dispute | AnswerAct::ResolveDispute) => add(FEATURE_DISPUTE),
                Some(AnswerAct::AcceptVerdict) => add(FEATURE_CHECK_REQUEST),
                _ => {}
            }
        }
        kinds::CHECK_REQUEST => add(FEATURE_CHECK_REQUEST),
        kinds::REVIEW => {
            use rdy_model::{ReviewSubject, Verdict};
            if typed::<rdy_model::Review>(&o).is_some_and(|r| {
                matches!(r.subject, ReviewSubject::Check { .. })
                    || matches!(r.verdict, Verdict::Pass | Verdict::Concern | Verdict::Fail)
            }) {
                add(FEATURE_CHECK_REQUEST);
            }
        }
        _ => {}
    }
    TEST_FEATURES.with(|t| {
        out.extend(
            t.borrow()
                .iter()
                .filter(|(_, trigger)| *trigger == o.kind)
                .map(|(f, _)| f.clone()),
        )
    });
    out
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
    #[serde(deserialize_with = "bounded_steps")]
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

/// A receipt's steps, counted before storage grows for them: the first
/// `MAX_RECEIPT_STEPS + 1` are kept (so [`UpgradeReceipt::validate`] refuses the count), each
/// later one is read as a string and dropped, so every decode fault is still the decoder's.
fn bounded_steps<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    struct Steps;
    impl<'de> serde::de::Visitor<'de> for Steps {
        type Value = Vec<String>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a list of step names")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Vec<String>, A::Error> {
            let mut steps = Vec::new();
            while steps.len() <= MAX_RECEIPT_STEPS {
                match seq.next_element::<String>()? {
                    Some(s) => steps.push(s),
                    None => return Ok(steps),
                }
            }
            while seq.next_element::<Text>()?.is_some() {}
            Ok(steps)
        }
    }
    /// A string, read and dropped.
    struct Text;
    impl<'de> Deserialize<'de> for Text {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Text, D::Error> {
            struct V;
            impl serde::de::Visitor<'_> for V {
                type Value = Text;
                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("a step name")
                }
                fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Text, E> {
                    Ok(Text)
                }
            }
            d.deserialize_str(V)
        }
    }
    d.deserialize_seq(Steps)
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
