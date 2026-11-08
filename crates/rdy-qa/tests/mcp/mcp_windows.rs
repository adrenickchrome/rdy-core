//! wx26a: the MCP area's stub peers are native. A stub is the test executable running a scripted
//! `script` fixture mode (23c), so no shell, `cat`, `head`, `tr` or `sleep` is needed and the same
//! bytes run on every platform. The tests pin that the stub is the fixture and not a shell, that
//! its stdout holds only its own bytes (libtest's banner is dropped before anything is counted),
//! that every bound of the bounded session holds, and that no stub of the area names a shell.
use child_fixture::{BANNER, script_command};
use rdy_qa::mcp::{DEADLINE, McpClient, McpFailure};
use rdy_qa::platform;
use std::path::Path;
use std::process::Command;

// The scripted stub peers of the area are the native child fixture. This binary includes the
// fixture source here, which installs the fixture entry test in its own libtest executable; the
// other files of the area reach it as `crate::mcp_windows::child_fixture`.
#[path = "../../src/platform/child_fixture.rs"]
pub(crate) mod child_fixture;

const CAP: usize = platform::RELEASE_CHILD_OUTPUT_MAX;
const INIT: &str = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{},"serverInfo":{"name":"peer","version":"0"}}}"#;

fn after_init(then: &str) -> String {
    format!("read\nsay {INIT}\nread\n{then}")
}

/// The files of the MCP area that build stub peers, as `(name, text)`.
fn area_sources() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = vec![root.join("src/mcp.rs")];
    for entry in std::fs::read_dir(root.join("tests/mcp")).expect("the mcp area") {
        let path = entry.expect("an entry").path();
        if path.extension().is_some_and(|e| e == "rs") {
            files.push(path);
        }
    }
    files
        .into_iter()
        .map(|p| {
            let text = std::fs::read_to_string(&p).expect("read a source");
            (p.display().to_string(), text)
        })
        .collect()
}

#[test]
fn wx26a_mcp_stub_modes_are_native_and_bounded() {
    // the stub is this test executable in the script mode, never a shell
    let cmd: Command = script_command("exit 0", None);
    assert_eq!(
        cmd.get_program(),
        std::env::current_exe()
            .expect("the test executable")
            .as_os_str()
    );
    // no stub of the area names a shell or a shell tool; the words are built here so this file
    // does not match itself
    let words: Vec<String> = vec![
        format!("{}{}", "shell", "_script"),
        format!("{}{}", "Command::new(", "\"sh\")"),
        format!("{} {}", "exec", "sleep"),
        format!("{} {}", "cat >", "/dev/null"),
        format!("{} {}", "head", "-c"),
        format!("{}{}", "read -", "r "),
        format!("{}{}", "tr '\\", "0'"),
    ];
    let mut named = Vec::new();
    for (file, text) in area_sources() {
        if file.ends_with("mcp_windows.rs") {
            continue;
        }
        for word in &words {
            if text.contains(word.as_str()) {
                named.push(format!("{file}: {word}"));
            }
        }
    }
    assert!(named.is_empty(), "shell stubs remain: {named:#?}");

    // a missing shell cannot matter: the stub runs with nothing on its PATH
    let mut cmd = script_command(&after_init("drain"), None);
    cmd.env("PATH", "");
    let c = McpClient::try_launch_bounded(cmd, CAP).expect("a stub with no PATH starts");
    let t = c.try_finish(DEADLINE).expect("finish");
    assert_eq!(t.code, Some(0));
    // stdout isolation: the stub's stdout is its own bytes, without libtest's banner
    assert_eq!(t.stdout, format!("{INIT}\n"));
    assert!(!t.stdout.as_bytes().starts_with(BANNER));

    // the cap counts the stub's own bytes: the banner is not among them
    let rest = CAP - (INIT.len() + 1);
    for (extra, want) in [(0, Ok(CAP)), (1, Err(McpFailure::OutputLimit))] {
        let c = McpClient::try_launch_bounded(
            script_command(
                &after_init(&format!("fill err {} 00\ndrain", rest + extra)),
                None,
            ),
            CAP,
        )
        .expect("launch");
        let got = c
            .try_finish(DEADLINE)
            .map(|t| t.stdout.len() + t.stderr.len());
        assert_eq!(got, want, "{extra} byte(s) past the cap");
    }

    // output that is not JSON, not UTF-8, cut off, or too large; a server that closes; a mode
    // the fixture does not know
    for (what, script, want) in [
        ("not JSON", "read\nsay not json\nhang", McpFailure::Protocol),
        ("not UTF-8", "read\nrawout ff0a\nhang", McpFailure::Protocol),
        (
            "cut off",
            "read\nput {\"jsonrpc\":\"2.0\"",
            McpFailure::Protocol,
        ),
        ("closed", "read\nexit 0", McpFailure::Io),
        ("an unknown step", "frobnicate", McpFailure::Io),
    ] {
        let got = McpClient::try_launch_bounded(script_command(script, None), CAP).map(|_| ());
        assert_eq!(got, Err(want), "{what}");
    }
    let over = format!("read\nfill out {} 00\nhang", CAP + 1);
    assert_eq!(
        McpClient::try_launch_bounded(script_command(&over, None), CAP).map(|_| ()),
        Err(McpFailure::OutputLimit)
    );

    // a stub that hangs is stopped with its session, and its own child with it
    let dir = tempfile::tempdir().expect("tempdir");
    let pidfile = dir.path().join("pid");
    let c = McpClient::try_launch_bounded(
        script_command(&after_init("bghang\nrecord\nhang"), Some(&pidfile)),
        CAP,
    )
    .expect("launch");
    let mut pid = None;
    for _ in 0..12_000 {
        pid = std::fs::read_to_string(&pidfile)
            .ok()
            .filter(|t| t.ends_with('\n'))
            .and_then(|t| t.trim().parse::<u32>().ok());
        if pid.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let pid = pid.expect("the stub's own child");
    assert!(!platform::process_exited(pid));
    assert_eq!(
        c.try_finish(std::time::Duration::ZERO).map(|_| ()),
        Err(McpFailure::Deadline)
    );
    for _ in 0..12_000 {
        if platform::process_exited(pid) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("the stub's child outlived its session");
}

#[test]
fn wx26a_a_directory_alias_names_the_same_directory_without_a_privilege_gate() {
    let dir = tempfile::tempdir().expect("tempdir");
    let real = dir.path().join("real");
    std::fs::create_dir(&real).expect("mkdir");
    std::fs::write(real.join("f"), b"same").expect("write");
    let alias = dir.path().join("alias");
    platform::dir_alias(&real, &alias).expect("an alias needs no privilege");
    assert_ne!(alias, real);
    if platform::follow_refused(&alias) {
        // a logon whose process policy refuses links a standard user made: following the alias
        // fails with 448 and nothing else. The alias still names the directory, read as an entry.
        let named = std::fs::read_link(&alias).expect("the alias names its target");
        let plain = |p: &Path| {
            let text = p.to_string_lossy().to_lowercase();
            text.strip_prefix(r"\\?\").unwrap_or(&text).to_string()
        };
        assert_eq!(plain(&named), plain(&std::fs::canonicalize(&real).unwrap()));
        assert_eq!(
            std::fs::read(alias.join("f")).unwrap_err().raw_os_error(),
            Some(448)
        );
        eprintln!("wx26a: the link path was not exercised (error 448)");
        return;
    }
    assert_eq!(std::fs::read(alias.join("f")).expect("read"), b"same");
}
