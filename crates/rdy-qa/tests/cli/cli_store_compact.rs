//! mh7: `rdy compact [--dry-run] [--json]` and the byte report of `rdy fsck` and `rdy status
//! --json` (D-4YRQ4, D-KQ4CA). The CLI sends the `compact` daemon op and prints the shared
//! report; a dry run prints the same lines and punches nothing.
use rdy_engine::Rdy;
use rdy_engine::redaction::RedactInput;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::{approver, land, write};
use rdy_qa::tensions::{ag, created};
use serde_json::{Value, json};

/// A served project whose log holds a removed span, and no daemon running.
fn removed() -> Served {
    let s = Served::new();
    s.stop();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let w = write(
        &mut r,
        "gone",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "gone", "title": "Gone", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "The launch code is ZEBRA-7731."}
        ]),
    )
    .unwrap();
    land(&mut r, "gone").unwrap();
    let rq = ag("rq");
    r.redact_in(
        &rq,
        "rq",
        RedactInput {
            targets: vec![created(&w, "$p").to_string()],
            reason_class: "secret".into(),
            reason: "a leaked code".into(),
            dry_run: false,
        },
    )
    .unwrap()
    .redaction
    .expect("the request is recorded");
    let p = r
        .propose(&rq, "rq", "Remove the code", Some("Removes a leaked code."))
        .unwrap();
    r.land(&approver(), &p.id).unwrap();
    r.seal_open().unwrap();
    r.close().unwrap();
    s
}

fn text_of(o: &std::process::Output) -> String {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// The `<name>: <bytes> bytes, <removed_bytes> removed` lines of a report.
fn kind_lines(v: &Value) -> Vec<String> {
    v["by_kind"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| {
            format!(
                "{}: {} bytes, {} removed",
                k["name"].as_str().unwrap(),
                k["bytes"],
                k["removed_bytes"]
            )
        })
        .collect()
}

#[test]
fn mh7_compact_prints_four_lines_the_kind_rows_and_the_notes() {
    let s = removed();
    let (ok, dry) = s.json(&["--json", "compact", "--dry-run"]);
    assert!(ok, "{dry}");
    let out = text_of(&s.rdy(&["compact", "--dry-run"]));
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], format!("log bytes: {}", dry["log_bytes"]));
    assert_eq!(lines[1], format!("removed bytes: {}", dry["removed_bytes"]));
    assert_eq!(lines[2], "punched bytes: 0");
    assert_eq!(lines[3], format!("on-disk bytes: {}", dry["on_disk_bytes"]));
    let kinds = kind_lines(&dry);
    assert!(kinds.len() > 3, "{kinds:?}");
    assert_eq!(&lines[4..4 + kinds.len()], &kinds[..], "{out}");
    let notes = dry["notes"].as_array().unwrap();
    assert_eq!(lines.len(), 4 + kinds.len() + notes.len(), "{out}");
    for (line, note) in lines[4 + kinds.len()..].iter().zip(notes) {
        assert_eq!(Some(*line), note.as_str());
    }
    // a real run keeps the log and the removed bytes; only the disk numbers may move
    let (ok, real) = s.json(&["--json", "compact"]);
    assert!(ok, "{real}");
    assert_eq!(real["log_bytes"], dry["log_bytes"]);
    assert_eq!(real["removed_bytes"], dry["removed_bytes"]);
    assert_eq!(real["by_kind"], dry["by_kind"]);
    let again = text_of(&s.rdy(&["compact"]));
    assert!(
        again.starts_with(&format!("log bytes: {}\n", dry["log_bytes"])),
        "{again}"
    );
    // the direct path prints the same report
    s.stop();
    let (ok, direct) = s.json(&["--json", "--no-daemon", "compact", "--dry-run"]);
    assert!(ok, "{direct}");
    assert_eq!(direct["by_kind"], dry["by_kind"]);
    assert_eq!(direct["log_bytes"], dry["log_bytes"]);
}

#[test]
fn mh7_compact_help_and_flags_are_closed() {
    let s = Served::new();
    let help = text_of(&s.rdy(&["compact", "--help"]));
    assert!(
        help.contains("Free disk space used by removed payloads. The chat path is rdy_compact."),
        "{help}"
    );
    assert!(help.contains("--dry-run"), "{help}");
    let o = s.rdy(&["compact", "--force"]);
    assert!(!o.status.success());
    let top = text_of(&s.rdy(&["--help"]));
    assert!(
        top.contains("Free disk space used by removed payloads. The chat path is rdy_compact."),
        "{top}"
    );
}

#[test]
fn mh7_status_and_fsck_add_counts_without_losing_keys() {
    let s = removed();
    // every key the engine's own fsck report has, before the byte report joins
    let engine_keys: Value = {
        let r = Rdy::open(s.dir.path()).unwrap();
        let v = serde_json::to_value(r.fsck().unwrap()).unwrap();
        r.close().unwrap();
        v
    };
    let (ok, fsck) = s.json(&["--json", "fsck"]);
    assert!(ok, "{fsck}");
    for (key, value) in engine_keys.as_object().unwrap() {
        assert!(fsck.get(key).is_some(), "fsck lost {key}: {fsck}");
        if !matches!(key.as_str(), "warnings") {
            assert_eq!(&fsck[key], value, "fsck changed {key}");
        }
    }
    for key in ["open_report", "quarantine"] {
        assert!(fsck.get(key).is_some(), "fsck lost {key}");
    }
    assert!(fsck["log_bytes"].as_u64().unwrap() > 0, "{fsck}");
    assert!(fsck["removed_bytes"].as_u64().unwrap() > 0, "{fsck}");
    let (ok, status) = s.json(&["--json", "status"]);
    assert!(ok, "{status}");
    for key in [
        "pack",
        "daemon",
        "landings",
        "suspects",
        "version_position",
        "views",
        "warnings",
        "seq",
        "open_report",
        "quarantine",
    ] {
        assert!(status.get(key).is_some(), "status lost {key}: {status}");
    }
    for key in ["log_bytes", "removed_bytes", "by_kind"] {
        assert_eq!(status[key], fsck[key], "status and fsck agree on {key}");
    }
    // the text: the first line, then the lines that were already there, then the byte lines
    let out = text_of(&s.rdy(&["fsck"]));
    let lines: Vec<&str> = out.lines().collect();
    let kinds = kind_lines(&fsck);
    let tail = 2 + kinds.len();
    assert!(lines[0].starts_with("ok: "), "{out}");
    assert_eq!(
        lines[lines.len() - tail],
        format!("log bytes: {}", fsck["log_bytes"])
    );
    assert_eq!(
        lines[lines.len() - tail + 1],
        format!("removed bytes: {}", fsck["removed_bytes"])
    );
    assert_eq!(&lines[lines.len() - kinds.len()..], &kinds[..], "{out}");
    // the status text is unchanged: it gains no byte line
    let status_text = text_of(&s.rdy(&["status"]));
    assert!(!status_text.contains("log bytes"), "{status_text}");
    // an agent's fsck gets the same byte report
    let (ok, agent) = s.json_as("agent:x", Some("r"), &["--json", "fsck"]);
    assert!(ok, "{agent}");
    assert_eq!(agent["log_bytes"], fsck["log_bytes"]);
}
