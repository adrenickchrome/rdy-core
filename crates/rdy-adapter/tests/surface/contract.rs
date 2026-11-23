//! The adapter's public contract: a consumer imports everything from `rdy_adapter` alone.

use rdy_adapter::{
    Block, BlockId, Hash, Identity, Origin, RdyError, RepoId, Snapshot, StoreHandle, Tag,
    core_error_code, encode_block, hash_bytes,
};
use std::collections::BTreeMap;
use std::path::Path;

/// An empty project on disk, closed again.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    rdy_engine::Rdy::init(dir.path(), "rdy-standard")
        .unwrap()
        .close()
        .unwrap();
    dir
}

fn open(path: &Path) -> Result<StoreHandle, RdyError> {
    StoreHandle::open(path)
}

/// A block whose fields, links and pins are filled in a deliberately unsorted order.
fn block() -> Block {
    use rdy_model::{FieldValue, Link};
    let mut fields = BTreeMap::new();
    for key in ["zeta", "alpha", "mid"] {
        fields.insert(key.to_string(), FieldValue::Str(format!("{key} value")));
    }
    let mut inner = BTreeMap::new();
    inner.insert("z".to_string(), FieldValue::Int(-3));
    inner.insert(
        "a".to_string(),
        FieldValue::List(vec![FieldValue::Bool(true)]),
    );
    fields.insert("map".to_string(), FieldValue::Map(inner));
    let mut links = BTreeMap::new();
    for key in ["refs", "blocks"] {
        links.insert(
            key.to_string(),
            vec![Link {
                target: BlockId::parse("d-00001").unwrap(),
                part: Some("p".to_string()),
            }],
        );
    }
    let mut pins = BTreeMap::new();
    pins.insert(BlockId::parse("d-00003").unwrap(), hash_bytes(b"three"));
    pins.insert(BlockId::parse("d-00002").unwrap(), hash_bytes(b"two"));
    Block {
        id: BlockId::parse("d-00009").unwrap(),
        block_type: "doc".to_string(),
        parent: None,
        order: "a0".to_string(),
        fields,
        body: "A body.".to_string(),
        links,
        pins,
        foreign: Default::default(),
        read_identity: None,
    }
}

#[test]
fn ha3_a_consumer_needs_only_the_adapter_crate() {
    let dir = project();
    let handle: StoreHandle = open(dir.path()).unwrap();
    let snap: Snapshot = handle.snapshot().unwrap();
    let identity: &Identity = snap.identity();
    let _id: RepoId = identity.repo_id;
    let origin: &Option<Origin> = &identity.origin;
    assert!(origin.is_none());
    let canon: Option<Hash> = snap.canon();
    assert!(canon.is_none());
    let tags: Vec<(Hash, Tag)> = snap.tags().unwrap();
    assert!(tags.is_empty());
    let bytes: Vec<u8> = encode_block(&block()).unwrap();
    let h: Hash = hash_bytes(&bytes);
    assert_eq!(h, Hash::of(&bytes));
    let _e: Option<RdyError> = None;
    handle.close().unwrap();
}

#[test]
fn plz1_adapter_reexports_core_error_code() {
    let check: fn(&str) -> Option<&'static str> = core_error_code;
    let kept: Option<&'static str> = {
        let supplied = String::from("E_LOCKED");
        check(&supplied)
    };
    assert_eq!(kept, Some("E_LOCKED"));
    assert_eq!(check("E_HUB_BUSY"), None);
}

#[test]
fn ha3_open_uses_the_existing_exclusive_lock() {
    let dir = project();
    let first = open(dir.path()).unwrap();
    let second = open(dir.path()).err().expect("a second open is refused");
    assert_eq!(second.code, "E_LOCKED", "{second}");
    drop(first);
}

#[test]
fn ha3_close_releases_the_lock() {
    let dir = project();
    open(dir.path()).unwrap().close().unwrap();
    open(dir.path()).unwrap().close().unwrap();
}

/// The bytes of [`block`] under the existing postcard block encoding (pinned).
const BLOCK_BYTES: &[u8] = &[
    7, 100, 45, 48, 48, 48, 48, 57, 3, 100, 111, 99, 0, 2, 97, 48, 4, 5, 97, 108, 112, 104, 97, 3,
    11, 97, 108, 112, 104, 97, 32, 118, 97, 108, 117, 101, 3, 109, 97, 112, 5, 2, 1, 97, 4, 1, 1,
    1, 1, 122, 2, 5, 3, 109, 105, 100, 3, 9, 109, 105, 100, 32, 118, 97, 108, 117, 101, 4, 122,
    101, 116, 97, 3, 10, 122, 101, 116, 97, 32, 118, 97, 108, 117, 101, 7, 65, 32, 98, 111, 100,
    121, 46, 2, 6, 98, 108, 111, 99, 107, 115, 1, 7, 100, 45, 48, 48, 48, 48, 49, 1, 1, 112, 4,
    114, 101, 102, 115, 1, 7, 100, 45, 48, 48, 48, 48, 49, 1, 1, 112, 2, 7, 100, 45, 48, 48, 48,
    48, 50, 220, 119, 15, 255, 83, 245, 8, 53, 248, 204, 149, 126, 1, 192, 213, 115, 29, 60, 46,
    213, 68, 195, 117, 73, 58, 40, 192, 155, 229, 224, 151, 99, 7, 100, 45, 48, 48, 48, 48, 51,
    219, 236, 14, 104, 159, 182, 59, 215, 41, 20, 119, 39, 18, 157, 133, 78, 157, 89, 10, 182, 32,
    161, 139, 187, 203, 51, 23, 210, 104, 214, 253, 114,
];
/// The hash of those bytes (pinned).
const BLOCK_HASH: &str = "8619d6f1a53b3803f185581997429479393fa43e066b46bcb8cf47b21e34ce30";

#[test]
fn ha3_block_encoding_matches_core_exactly() {
    let b = block();
    let bytes = encode_block(&b).unwrap();
    assert_eq!(bytes, postcard::to_allocvec(&b).unwrap());
    assert_eq!(hash_bytes(&bytes), b.version_hash());
    assert_eq!(hash_bytes(&bytes), rdy_model::hash_of(&b));
    assert_eq!(bytes, BLOCK_BYTES, "{bytes:?}");
    assert_eq!(hash_bytes(&bytes).to_string(), BLOCK_HASH);
    // map ordering: the same entries inserted in the opposite order encode to the same bytes
    let mut again = b.clone();
    again.fields = b.fields.clone().into_iter().rev().collect();
    again.links = b.links.clone().into_iter().rev().collect();
    again.pins = b.pins.clone().into_iter().rev().collect();
    assert_eq!(encode_block(&again).unwrap(), bytes);
}

#[test]
fn ha3_block_encoding_limit_is_exact() {
    let mut b = block();
    b.body = String::new();
    let base = encode_block(&b).unwrap().len();
    // a body this long has a four-byte length prefix instead of one byte
    let fits = rdy_model::redaction::MAX_RECORD_BYTES - base - 3;
    b.body = "x".repeat(fits);
    assert_eq!(
        encode_block(&b).unwrap().len(),
        rdy_model::redaction::MAX_RECORD_BYTES
    );
    b.body.push('x');
    let e = encode_block(&b).unwrap_err();
    assert_eq!(e.code, "E_LIMIT");
    assert_eq!(e.message, "block encoding exceeds 16777216 bytes");
    assert_eq!(e.fix, "reduce the block to the stated limit");
}

#[test]
fn ha3_projection_encoding_is_refused() {
    let request = hash_bytes(b"request");
    let projection = Block::redacted(
        BlockId::parse("d-00009").unwrap(),
        "doc".to_string(),
        None,
        "a0".to_string(),
        rdy_model::RedactedBlockIdentity {
            version: hash_bytes(b"version"),
            content_hash: hash_bytes(b"content"),
            frozen: false,
            request,
        },
    );
    let e = encode_block(&projection).unwrap_err();
    assert_eq!(e.code, "E_REDACTED");
    assert_eq!(
        e.message,
        format!("Content removed by redaction {request}.")
    );
    assert!(!e.message.contains(&projection.body), "{e}");
}

/// `depth` nested singleton lists (or maps) around one scalar, built without recursion.
fn nested(depth: usize, maps: bool) -> rdy_model::FieldValue {
    use rdy_model::FieldValue;
    let mut v = FieldValue::Int(1);
    for _ in 0..depth {
        v = if maps {
            FieldValue::Map(BTreeMap::from([("k".to_string(), v)]))
        } else {
            FieldValue::List(vec![v])
        };
    }
    v
}

/// Takes a nested value apart level by level, so dropping it does not recurse.
fn dismantle(mut v: rdy_model::FieldValue) {
    use rdy_model::FieldValue;
    loop {
        v = match v {
            FieldValue::List(mut items) => match items.pop() {
                Some(inner) => inner,
                None => return,
            },
            FieldValue::Map(mut m) => match m.pop_first() {
                Some((_, inner)) => inner,
                None => return,
            },
            _ => return,
        };
    }
}

#[test]
fn ha3_nesting_depth_is_bounded_before_encoding() {
    // The deepest nesting that encodes: bytes equal core's own encoding.
    for maps in [false, true] {
        let mut b = block();
        b.fields.insert("deep".to_string(), nested(128, maps));
        assert_eq!(
            encode_block(&b).unwrap(),
            postcard::to_allocvec(&b).unwrap()
        );
        let before = b.fields.remove("deep").unwrap();
        dismantle(before);
        // One level more, and a hostile depth far below the byte limit, are refused with no
        // recursion into the serializer (65,536 levels overflowed the stack before this bound).
        for depth in [129, 65_536, 1_000_000] {
            let mut b = block();
            b.fields.insert("deep".to_string(), nested(depth, maps));
            let e = encode_block(&b).unwrap_err();
            assert_eq!(e.code, "E_LIMIT", "depth {depth}: {e}");
            assert_eq!(e.message, "a field value nests deeper than 128 levels");
            assert_eq!(
                e.fix,
                "reduce the nesting of the field value to the stated limit"
            );
            dismantle(b.fields.remove("deep").unwrap());
        }
    }
}

#[test]
fn ha3_wide_values_encode_as_core_encodes_them() {
    use rdy_model::FieldValue;
    // A shallow value of any width is within the depth bound: bytes equal core's encoding.
    let wide_list = FieldValue::List(vec![FieldValue::Null; 1_000_000]);
    let wide_map = FieldValue::Map(
        (0..100_000)
            .map(|i| (format!("k{i:06}"), FieldValue::Int(i)))
            .collect(),
    );
    for wide in [wide_list, wide_map] {
        let mut b = block();
        b.fields.insert("wide".to_string(), wide);
        assert_eq!(
            encode_block(&b).unwrap(),
            postcard::to_allocvec(&b).unwrap()
        );
    }
    // A wide level above a too-deep one is still refused.
    let mut b = block();
    b.fields.insert(
        "wide".to_string(),
        FieldValue::List(vec![FieldValue::Null; 100_000]),
    );
    b.fields.insert("deep".to_string(), nested(129, false));
    assert_eq!(encode_block(&b).unwrap_err().code, "E_LIMIT");
    dismantle(b.fields.remove("deep").unwrap());
}

/// rpl34: the adapter surface of repo links, by its exact names. This function is never called:
/// it compiles only while each method has the name, the receiver and the arguments the contract
/// lists, so a removed or reshaped method fails the build of this test file. The names are the
/// seventeen of the adapter table (`link_adoption` came after the sixteen the plan counted).
#[allow(dead_code)]
fn rpl34_the_exact_adapter_names(
    snap: &Snapshot,
    handle: &StoreHandle,
    stream: &std::fs::File,
    link: &BlockId,
    repo: &RepoId,
    landing: &Hash,
) {
    use rdy_adapter::{DeliverySource, LinkSource, ReadRequest, SearchRequest};
    // Snapshot (every read takes one captured snapshot)
    let _ = snap.link_export("v1.0.0", "profile", Vec::<u8>::new());
    let _ = snap.tag_deliveries("v1.0.0");
    let _ = snap.foreign_refs();
    let _ = snap.selection_changes(link, None);
    let _ = snap.foreign_blast_radius(repo, &[]);
    let _ = snap.foreign_signals();
    let _ = snap.foreign_tension_gate(landing);
    let _ = snap.lock_facts(landing);
    let _ = snap.lock_changes(landing);
    let _ = snap.unadopted_locked();
    let _ = snap.link_adoption();
    let _ = snap.foreign_bytes(repo);
    let _ = snap.read(ReadRequest::default());
    let _ = snap.search(SearchRequest::default());
    // StoreHandle (every write takes &self)
    let _ = handle.take_delivery(
        stream,
        DeliverySource {
            link: link.clone(),
            site: String::new(),
            at_ms: 0,
            set: None,
        },
    );
    let _ = handle.revoke_link(LinkSource {
        link: link.clone(),
        site: String::new(),
        at_ms: 0,
    });
    let _ = handle.take_foreign_removal(stream, *repo);
}

/// cra1: the reads of a full landing hash (D-Y32MN), by their exact names. This function is never
/// called: it compiles only while each name has the receiver and the arguments listed.
#[allow(dead_code)]
fn cra1_adapter_names(
    state: &rdy_engine::ReadState,
    heads: &[Hash],
    hashes: &[Hash],
    scope: &rdy_engine::Scope,
) {
    let _: Result<Vec<bool>, RdyError> = rdy_engine::history_facts::reached(state, heads, hashes);
    let _: Result<rdy_engine::tool_reads::ReadEntries, RdyError> =
        rdy_engine::tool_reads::read_entries_at(state, "id", scope, "compact", None);
}

/// crb1: the compare and the diff of two scopes (D-2QX9W), by their exact names. This function
/// is never called: it compiles only while each name has the receiver, the arguments and the
/// result listed, and each type is re-exported from the module and the crate root.
#[allow(dead_code)]
fn crb1_adapter_names(snap: &Snapshot) {
    let _: Result<rdy_adapter::Compare, RdyError> = snap.compare(Some("canon"), "canon");
    let _: Result<rdy_adapter::Diff, RdyError> = snap.diff(Some("canon"), "canon");
    let _: Option<(
        rdy_adapter::compare::Compare,
        rdy_adapter::compare::CompareEntry,
        rdy_adapter::compare::Diff,
        rdy_adapter::compare::DiffChange,
        rdy_adapter::compare::DiffEntry,
        rdy_adapter::compare::DiffCounts,
    )> = None;
    let _: Option<(
        rdy_adapter::CompareEntry,
        rdy_adapter::DiffChange,
        rdy_adapter::DiffEntry,
        rdy_adapter::DiffCounts,
    )> = None;
}

/// crb3: the rebase test (D-2QX9W), by its exact names. This function is never called: it
/// compiles only while the name has the receiver, the arguments and the result listed, and each
/// type is re-exported from the module and the crate root.
#[allow(dead_code)]
fn crb3_adapter_names(snap: &Snapshot, chain: &[Hash]) {
    let _: Result<rdy_adapter::RebaseTest, RdyError> = snap.rebase_test(chain, "canon");
    let _: Option<(
        rdy_adapter::version_tests::RebaseTest,
        rdy_adapter::version_tests::MergeConflict,
        rdy_adapter::version_tests::ConflictKind,
        rdy_adapter::MergeConflict,
        rdy_adapter::ConflictKind,
    )> = None;
}

/// crb5: the compare page with the whole diff's counts (D-V2X3H), from an optional side
/// (D-AJW0S), by its exact names. This function is never called: it compiles only while each
/// name has the receiver, the arguments and the result listed.
#[allow(dead_code)]
fn crb5_adapter_names(snap: &Snapshot, state: &rdy_engine::ReadState) {
    let q = rdy_adapter::compare::DiffQuery {
        from: None,
        to: "canon".to_string(),
        doc: None,
        detail: false,
        max_tokens: None,
        page: None,
    };
    let _: rdy_adapter::DiffQuery = q.clone();
    let _: Result<serde_json::Value, RdyError> = snap.diff_reply(&q);
    let _: Result<rdy_adapter::Compare, RdyError> = snap.compare(None, "canon");
    let _: Result<rdy_adapter::Diff, RdyError> = snap.diff(None, "canon");
    let side: Result<rdy_engine::diff_request::DiffSide, RdyError> =
        state.diff_side_of("canon", rdy_engine::Scope::Canon);
    let side = side.unwrap();
    let _: Result<rdy_engine::DiffReply, RdyError> = state.diff_page_of(
        &rdy_engine::diff_request::DiffSide::Empty,
        &side,
        None,
        false,
        None,
        4_000,
        "diff_reply",
    );
}

/// crb6: the version pair of the carry-over (D-G8T2W), by its exact names. This function is never
/// called: it compiles only while the name has the receiver, the arguments and the result listed,
/// and each type is re-exported from the module and the crate root.
#[allow(dead_code)]
fn crb6_adapter_names(snap: &Snapshot, base: Option<Hash>, chain: Vec<Hash>) {
    let old = rdy_adapter::VersionChain { base, chain };
    let new: rdy_adapter::version_tests::VersionChain = old.clone();
    let t: Result<rdy_adapter::VersionTests, RdyError> = snap.compare_versions(&old, &new);
    let _: Option<rdy_adapter::version_tests::VersionTests> = t.ok();
    let _ = |t: rdy_adapter::VersionTests| -> (bool, bool) {
        (t.replays_one_for_one, t.base_moves_write_request_blocks)
    };
}

/// crb4: the replay test (D-2QX9W), by its exact names. This function is never called: it
/// compiles only while each name has the receiver, the arguments and the result listed.
#[allow(dead_code)]
fn crb4_adapter_names(
    snap: &Snapshot,
    chain: &[Hash],
    inputs: &rdy_adapter::landing_check::CheckInputs,
) {
    use rdy_adapter::replay_test::{ReplayStop, ReplayStopCause, ReplayTest, ReplayedCheck};
    let _: Result<rdy_adapter::ReplayTest, RdyError> = snap.replay_test(chain, "canon", inputs);
    let test = ReplayTest {
        replayed: vec![],
        stop: None,
    };
    let _: Vec<ReplayedCheck> = test.replayed;
    let _: Option<ReplayStop> = test.stop;
    let _: Option<(
        rdy_adapter::ReplayStop,
        rdy_adapter::ReplayedCheck,
        rdy_adapter::LandingCheck,
    )> = None;
    let _ = matches!(
        ReplayStopCause::Conflict,
        ReplayStopCause::Conflict
            | ReplayStopCause::Gate
            | ReplayStopCause::Policy
            | ReplayStopCause::Removal
            | ReplayStopCause::UpToDate
    );
    let _: rdy_adapter::ReplayStopCause = ReplayStopCause::Gate;
}

/// cra3: the place of each landing (D-H1SHC), by its exact names. This function is never called:
/// it compiles only while each name has the receiver, the arguments and the result listed.
#[allow(dead_code)]
fn cra3_adapter_names(snap: &Snapshot, state: &rdy_engine::ReadState, hashes: &[Hash]) {
    use rdy_adapter::history::{LandingPlace, LandingPlaceFact};
    let _: Result<Vec<LandingPlaceFact>, RdyError> = snap.landing_places(hashes);
    let _: Result<Vec<rdy_engine::history_facts::LandingPlaceFact>, RdyError> =
        rdy_engine::history_facts::places(state, None, hashes, hashes);
    let fact = LandingPlaceFact {
        hash: Hash::of(b"x"),
        place: LandingPlace::InCanon,
    };
    let _ = matches!(
        fact.place,
        LandingPlace::InCanon | LandingPlace::OffCanon | LandingPlace::NotStored
    );
}

/// cra5: the render request of a landing and of a review pair (D-CST6K), by its exact names. This
/// function is never called: it compiles only while each name has the receiver and the arguments
/// listed.
#[allow(dead_code)]
fn cra5_adapter_names(snap: &Snapshot, inputs: &rdy_adapter::rendering::RenderInputs) {
    use rdy_adapter::rendering::{RenderFormat, ScopeRender, ScopeRenderReport};
    let target = ScopeRender {
        scope: String::new(),
        against: Some(String::new()),
    };
    let r: Result<ScopeRenderReport, RdyError> = snap.write_scope_render_request(
        &target,
        "implicit:org",
        inputs,
        RenderFormat::Site,
        Vec::<u8>::new(),
    );
    if let Ok(ScopeRenderReport { scope, against }) = r {
        let _: (rdy_adapter::rendering::BatchReport, Option<_>) = (scope, against);
    }
}

/// cra6: a pushed view at an op as a read and render scope (D-00ZG2, D-V9RF5), by its exact
/// names. This function is never called: it compiles only while each name has the receiver, the
/// arguments and the result listed.
#[allow(dead_code)]
fn cra6_adapter_names(
    state: &rdy_engine::ReadState,
    reader: &rdy_store::StoreReader,
    head: rdy_store::view::ViewHead,
    inputs: rdy_adapter::rendering::RenderInputs,
    stream: &[u8],
    history: &mut dyn rdy_engine::render_source::TagHistorySource,
    pack: &rdy_schema::Pack,
) {
    let h = Hash::of(b"x");
    let _: Result<rdy_store::view::ViewHead, rdy_store::StoreError> =
        reader.view_head_at("draft", &h, &h);
    let _: Result<rdy_engine::ReadState, RdyError> = state.pushed_view_state("draft", head);
    let _: Result<rdy_store::hub_canon::ViewAtOp, rdy_store::StoreError> =
        rdy_store::hub_canon::view_at_op(&h, &mut |_| Ok(None));
    let _: Result<(), RdyError> =
        rdy_render::process::write_state_list(1, &[], &mut Vec::<u8>::new());
    let opened: Result<rdy_engine::render_source::ViewStream, RdyError> =
        rdy_engine::render_source::RenderSource::open_view(stream, inputs, h, |_| {
            RdyError::new("E_BAD_INPUT", "", "")
        });
    if let Ok(view) = opened {
        let _: (&str, Option<Hash>, bool) = (view.name(), view.base(), view.holds_version(&h));
        let _: &std::collections::BTreeSet<rdy_model::BlockId> = view.touched();
        let _: Result<
            rdy_engine::render_source::RenderSource<rdy_engine::render_source::RootedFacts>,
            RdyError,
        > = view.into_source(&BTreeMap::new());
    }
    let _: Result<bool, RdyError> =
        rdy_engine::render_source::view_is_redacted(history, None, &[], pack);
}

/// rpl34: the surface runs on an empty project (its reads answer with no rows, never with an
/// error that names a missing method), its writes enter the store's one writer lane through a
/// shared handle, and the build's format capabilities name `foreign_copy` with its minimum rdy.
#[test]
fn rpl34_adapter_surface_is_complete() {
    use rdy_adapter::{ReadRequest, SearchRequest};
    let dir = project();
    let handle = open(dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    let landing = Hash::of(b"no such landing");
    let repo = RepoId([9; 16]);
    // the reads of an empty project
    assert!(snap.foreign_refs().unwrap().is_empty());
    assert!(snap.unadopted_locked().unwrap().is_empty());
    assert!(snap.link_adoption().unwrap().is_empty());
    assert_eq!(snap.foreign_bytes(&repo).unwrap(), 0);
    assert!(snap.foreign_blast_radius(&repo, &[]).unwrap().is_empty());
    let signals = snap.foreign_signals().unwrap();
    assert!(format!("{signals:?}").contains("suspects"), "{signals:?}");
    assert!(
        snap.tag_deliveries("v9").is_err(),
        "a tag it lacks is refused"
    );
    assert!(
        snap.link_export("v9", "agents", Vec::<u8>::new()).is_err(),
        "a tag it lacks is refused"
    );
    // a landing the snapshot lacks is a refusal, not a silent empty answer
    for refusal in [
        snap.foreign_tension_gate(&landing).err(),
        snap.lock_facts(&landing).err(),
        snap.lock_changes(&landing).err(),
    ] {
        assert!(refusal.is_some(), "an unknown landing is refused");
    }
    // the tool reads of Task 38 serve the same snapshot
    let request = ReadRequest {
        id: "c-NOSUCH".into(),
        ..ReadRequest::default()
    };
    assert!(snap.read(request).is_err(), "an unknown block is refused");
    let found = snap
        .search(SearchRequest {
            query: "nothing here".into(),
            ..SearchRequest::default()
        })
        .unwrap();
    assert!(found.is_object(), "{found}");
    // the capabilities the hub reads at build time
    let caps = rdy_adapter::capabilities::format_capabilities();
    let foreign = caps
        .store_reads
        .known_features
        .iter()
        .find(|f| f.name == "foreign_copy")
        .expect("foreign_copy is a registered feature");
    assert_eq!(
        foreign.required_rdy,
        rdy_store::format::FOREIGN_COPY_REQUIRED_RDY
    );
    // the capabilities write the base stream; a link delivery is the third format, which this
    // build reads (the value has no member of its own in the capabilities)
    assert_eq!(caps.written.stream, rdy_store::sync_stream::STREAM_FORMAT);
    assert_eq!(rdy_store::sync_stream::LINK_DELIVERY_FORMAT, 3);
    assert!(rdy_store::sync_stream::READ_FORMATS.contains(&3));
    drop(snap);
    handle.close().unwrap();
}

/// hfp2 (D-99CWC): a hub forks a holder snapshot with the adapter's own types: the call, its
/// handle, its identity and origin, and its error all come from `rdy_adapter`.
#[test]
fn hfp2_adapter_types_need_no_core_import() {
    let fork: fn(&Snapshot, &str, &Path) -> Result<StoreHandle, RdyError> = Snapshot::fork_at;
    let dir = project();
    let handle: StoreHandle = open(dir.path()).unwrap();
    let snap: Snapshot = handle.snapshot().unwrap();
    let target = tempfile::tempdir().unwrap();
    let to = target.path().join("fork");
    let e: RdyError = fork(&snap, "v1.0.0", &to).err().unwrap();
    assert_eq!(
        (e.code, e.message.as_str()),
        ("E_BAD_INPUT", "this store is a project")
    );
    assert!(!to.exists());
    // a holder's fork answers through the same types
    let hub = StoreHandle::create_holder(&target.path().join("hub"), RepoId::new()).unwrap();
    let held: Snapshot = hub.snapshot().unwrap();
    let e: RdyError = held.fork_at("v1.0.0", &to).err().unwrap();
    assert_eq!(
        (e.code, e.message.as_str()),
        ("E_NOT_FOUND", "no version v1.0.0")
    );
    let identity: &Identity = held.identity();
    let origin: &Option<Origin> = &identity.origin;
    assert!(origin.is_none());
    drop((snap, held));
    handle.close().unwrap();
    hub.close().unwrap();
}
