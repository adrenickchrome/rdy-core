//! r59017 (D-GGTYA, D-4PV25): the fixture hub's canon state path. Hub canon is seeded from the
//! frozen format-4 canon history in process ([`Hub::seed`]); each sync step is served in process
//! ([`Hub::call`]), as the listener serves it. The state reply lists format 5; a known pull of
//! purpose `canon_state` names the state's keys and its exact object frame bytes; a get sends a
//! checked format-5 batch; and no step reads a history page, a history closure or a prepared
//! pull. With format 5 switched off, the format-4 traffic is unchanged.
//!
//! `sync-client-wire-v6.json` records this exchange for the hand-off to the hub's own tests. It
//! is the mock's exchange: it shows the shapes this client reads, never what a deployed hub
//! sends. The ignored writer freezes it once; the pin checks the exchange against it.
use super::mock::{Called, Hub};
use rdy_adapter::Hash;
use rdy_adapter::stream::{ObjectKey, StreamPurpose};
use rdy_engine::hub_sync_wire::{GetRequest, KnownPull, decode_known_page, decode_state};
use rdy_engine::hub_types::Secret;
use rdy_engine::stream_check::check_state_batches;
use rdy_store::sync_stream::{check_batch, check_canon_state_batch};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const ROOT: &str = "d91a816b6a2eed457b598edf90b3684a457fe608f4b12be581201f49f34f4e75";
const WIRE: &str = "sync-client-wire-v6.json";
const WIRE_DIGEST: &str = "476f6d49dc7609c6c9088e539a8004d7621101a863db75180f559c921c91dfd2";

fn sync_fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../rdy-qa/fixtures/sync")
        .join(name)
}

fn root() -> ObjectKey {
    ObjectKey {
        kind: rdy_store::kinds::LANDING,
        hash: ROOT.parse().unwrap(),
    }
}

/// A seeded hub: canon at the source history's root.
fn hub() -> Hub {
    let hub = Hub::start();
    hub.seed(&[sync_fixture("sync4-canon-history.bin")]);
    assert_eq!(hub.canon(), Some(root().hash));
    hub
}

/// The state request of this client: it names format 4 alone.
fn state_request() -> Value {
    json!({"actor": "agent:ana/claude", "rdy": "0.59.0", "run": "run-ana", "site": null,
        "stream_read": [4], "stream_write": [4]})
}

fn known_request(purpose: StreamPurpose, after: Option<ObjectKey>) -> Value {
    serde_json::to_value(KnownPull {
        after,
        base: None,
        purpose,
        root: root(),
    })
    .unwrap()
}

fn get_request(purpose: StreamPurpose, keys: &[ObjectKey]) -> Value {
    serde_json::to_value(GetRequest {
        keys: keys.to_vec(),
        purpose,
        root: root(),
    })
    .unwrap()
}

/// One recorded step: the request and the reply, a batch body by its bytes and blake3.
fn step(case: &str, route: &str, request: &Value, reply: &Called, offer_zstd: bool) -> Value {
    let body = match reply.header("content-type") {
        Some("application/json") => reply.json(),
        _ => {
            let decoded = match reply.header("content-encoding") {
                Some("zstd") => zstd::decode_all(&reply.body[..]).unwrap(),
                _ => reply.body.clone(),
            };
            json!({"batch_bytes": decoded.len(), "batch_blake3": Hash::of(&decoded).to_string(),
                "wire_bytes": reply.body.len()})
        }
    };
    json!({"case": case, "request": {"accept_encoding": offer_zstd.then_some("zstd"),
        "body": request, "route": route},
        "reply": {"body": body, "content_encoding": reply.header("content-encoding"),
            "content_type": reply.header("content-type"), "status": reply.status}})
}

/// The exchange: the state step, the state's known pages and gets (identity and zstd), then the
/// same hub with format 5 switched off: its state step, a refused state pull, and the format-4
/// pull that stays.
fn exchange(hub: &Hub) -> Vec<Value> {
    let bearer = Secret::new("<token>");
    let mut steps = vec![];
    let walks = rdy_engine::stream_export::walks_for_tests();

    // the state step lists format 5
    let req = state_request();
    let state = hub.call("/state", &req, false);
    assert_eq!(state.status, 200);
    assert_eq!(state.json()["stream_write"], json!([2, 4, 5]));
    let decoded = decode_state(&state.body, &[], &bearer).unwrap();
    assert!(decoded.sends_canon_state());
    steps.push(step(
        "a client of format 4; the hub sends canon states",
        "/state",
        &req,
        &state,
        false,
    ));

    // the known pages: one scope-owned state, its keys and its object frame bytes
    let _scope = hub.scope("clone");
    let expect: Value = serde_json::from_slice(
        &std::fs::read(sync_fixture("sync5-canon-state.expect.json")).unwrap(),
    )
    .unwrap();
    let frozen = std::fs::read(sync_fixture("sync5-canon-state.bin")).unwrap();
    let mut keys: Vec<ObjectKey> = vec![];
    let req = known_request(StreamPurpose::CanonState, None);
    let known = hub.call("/known", &req, false);
    assert_eq!(
        known.status,
        200,
        "{}",
        String::from_utf8_lossy(&known.body)
    );
    assert_eq!(known.json()["bytes"], expect["known_bytes"]);
    let page = decode_known_page(&known.body, &bearer).unwrap();
    keys.extend(page.keys.iter().copied());
    assert_eq!(page.next, None, "one page holds the state");
    let frozen_keys: Vec<ObjectKey> =
        serde_json::from_value(expect["stream"]["keys"].clone()).unwrap();
    assert_eq!(keys, frozen_keys);
    steps.push(step(
        "the known pull of the canon tip's state",
        "/known",
        &req,
        &known,
        false,
    ));

    // the gets: the whole state as identity bytes, two split gets and a zstd get
    let req = get_request(StreamPurpose::CanonState, &keys);
    let whole = hub.call("/objects/get", &req, false);
    assert_eq!(whole.status, 200);
    assert_eq!(whole.body, frozen, "the get sends the frozen state batch");
    assert_eq!(check_canon_state_batch(&whole.body[..]).unwrap().keys, keys);
    steps.push(step(
        "the get of the whole state",
        "/objects/get",
        &req,
        &whole,
        false,
    ));
    let (a, b) = keys.split_at(keys.len() / 2);
    let mut files = vec![];
    for (n, part) in [a, b].into_iter().enumerate() {
        let req = get_request(StreamPurpose::CanonState, part);
        let got = hub.call("/objects/get", &req, false);
        assert_eq!(got.status, 200);
        steps.push(step(
            &format!("split get {} of 2", n + 1),
            "/objects/get",
            &req,
            &got,
            false,
        ));
        files.push(got.body);
    }
    let reports = check_state_batches(files.iter().map(|f| &f[..]).collect(), None).unwrap();
    assert_eq!(
        reports.iter().map(|r| r.keys.len()).sum::<usize>(),
        keys.len()
    );
    let req = get_request(StreamPurpose::CanonState, &keys);
    let packed = hub.call("/objects/get", &req, true);
    assert_eq!(packed.header("content-encoding"), Some("zstd"));
    assert_eq!(zstd::decode_all(&packed.body[..]).unwrap(), frozen);
    steps.push(step(
        "the zstd get of the whole state",
        "/objects/get",
        &req,
        &packed,
        true,
    ));
    assert_eq!(
        hub.states_prepared(),
        1,
        "the scope prepared its state once"
    );
    drop(_scope);
    assert_eq!(
        hub.history_reads(),
        0,
        "no history page, get or prepared pull"
    );
    assert_eq!(hub.history_walks(), 0, "no history closure walk");
    assert_eq!(rdy_engine::stream_export::walks_for_tests(), walks);
    assert_eq!(hub.prepared(), 0, "no prepared pull");

    // format 5 switched off: the format-4 traffic as before
    hub.send_canon_state(false);
    let req = state_request();
    let state = hub.call("/state", &req, false);
    assert_eq!(state.json()["stream_write"], json!([2, 4]));
    assert!(
        !decode_state(&state.body, &[], &bearer)
            .unwrap()
            .sends_canon_state()
    );
    steps.push(step(
        "a hub that sends no canon state",
        "/state",
        &req,
        &state,
        false,
    ));
    let req = known_request(StreamPurpose::CanonState, None);
    let refused = hub.call("/known", &req, false);
    assert_eq!(refused.status, 422);
    steps.push(step(
        "its refusal of a state pull",
        "/known",
        &req,
        &refused,
        false,
    ));
    let req = known_request(StreamPurpose::CanonHistory, None);
    let history = hub.call("/known", &req, false);
    let page = decode_known_page(&history.body, &bearer).unwrap();
    let source: Vec<ObjectKey> =
        check_batch(&std::fs::read(sync_fixture("sync4-canon-history.bin")).unwrap()[..])
            .unwrap()
            .keys;
    assert_eq!(
        page.keys.iter().collect::<std::collections::BTreeSet<_>>(),
        source.iter().collect::<std::collections::BTreeSet<_>>(),
        "the canon history pull names the whole history"
    );
    assert!(
        history.json().get("bytes").is_none(),
        "a history page names no bytes"
    );
    steps.push(step(
        "the canon history pull that stays",
        "/known",
        &req,
        &history,
        false,
    ));
    let req = get_request(StreamPurpose::CanonHistory, &page.keys);
    let got = hub.call("/objects/get", &req, false);
    let report = check_batch(&got.body[..]).unwrap();
    assert_eq!(
        got.body,
        std::fs::read(sync_fixture("sync4-canon-history.bin")).unwrap(),
        "the format-4 get sends the frozen history's bytes"
    );
    assert_eq!(report.header.stream_format, 4);
    assert_eq!(report.header.purpose, StreamPurpose::CanonHistory);
    assert_eq!(report.keys.len(), source.len());
    steps.push(step("its get", "/objects/get", &req, &got, false));
    assert_eq!(
        hub.history_reads(),
        2,
        "the format-4 pull reads the history"
    );
    steps
}

fn wire(steps: Vec<Value>) -> String {
    let v = json!({
        "about": [
            "The fixture hub's canon state exchange (D-GGTYA, D-4PV25), version 6: recorded from the adapter's mock hub for the hand-off to the hub's own tests. It shows the shapes this client reads; it is not a record of a deployed hub.",
            "Hub canon is seeded with the frozen format-4 canon history sync4-canon-history.bin; each get body is named by its decoded batch bytes and blake3 (the whole state is sync5-canon-state.bin).",
            "The client names format 4 alone in its state request; a hub that lists format 5 in stream_write sends a canon tip's state under purpose canon_state. Its known page carries bytes, the state's exact object frame bytes. The last steps are a hub with format 5 switched off."
        ],
        "fixture": "sync-client-wire",
        "steps": steps,
        "version": 6,
    });
    let mut s = serde_json::to_string_pretty(&v).unwrap();
    s.push('\n');
    s
}

/// Writes the wire fixture once. Run by hand, never by the gate, after the state fixtures are
/// frozen: `cargo test -p rdy-adapter --test sync_e2e r59017_write_state_wire_once -- --ignored`.
/// It refuses a file that exists.
#[test]
#[ignore]
fn r59017_write_state_wire_once() {
    assert!(
        !sync_fixture(WIRE).exists(),
        "{WIRE}: the wire fixture is written once: never rewrite it"
    );
    let hub = hub();
    let text = wire(exchange(&hub));
    std::fs::write(sync_fixture(WIRE), &text).unwrap();
    println!(
        "{WIRE}: {} bytes, blake3 {}",
        text.len(),
        Hash::of(text.as_bytes())
    );
    hub.finish();
}

#[test]
fn r59017_mock_state_path_is_snapshot_only() {
    let hub = hub();
    let text = wire(exchange(&hub));
    let frozen = std::fs::read_to_string(sync_fixture(WIRE)).unwrap();
    assert_eq!(Hash::of(frozen.as_bytes()).to_string(), WIRE_DIGEST);
    assert_eq!(text, frozen, "the exchange is the frozen wire fixture");
    hub.finish();
}
