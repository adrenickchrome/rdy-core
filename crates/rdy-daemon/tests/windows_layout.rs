//! The project's endpoint and run files through the `sys` seam: the name of a project's pipe, the
//! exact grammars of `daemon.sock` and `daemon.pid` on Windows, and what stands in a run
//! directory. The grammars are pure, so they run on every platform; what a real file system and a
//! real pipe do runs natively. The Windows crate does not build until every `sys` part exists, so
//! an isolated probe runs this file there until then.

use rdy_daemon::sys::records::{
    MAX_PID_RECORD, MAX_PIPE_RECORD, PIPE_PREFIX, hash_of_units, parse_pipe_pid_record,
    parse_pipe_record, pipe_name, pipe_pid_record, read_capped,
};
use rdy_daemon::{RunDir, socket};
use rdy_store::fileid::FileId;
use rdy_store::fsutil::{record_syncs, write_private_durable};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// A new directory for a project. On Unix under /tmp, so `.rdy/run/rdy.sock` fits `sun_path`.
fn temp() -> tempfile::TempDir {
    if cfg!(windows) {
        tempfile::tempdir().unwrap()
    } else {
        tempfile::Builder::new()
            .prefix("rdy-wx15")
            .tempdir_in("/tmp")
            .unwrap()
    }
}

/// A project at `root`: its `.rdy` exists, its run directory does not yet.
fn project(root: &Path) -> RunDir {
    fs::create_dir_all(root.join(".rdy")).unwrap();
    RunDir::of_project(root)
}

const HASH: &str = "abababababababababababababababababababababababababababababababab";
const ID: &str = "w:0123456789abcdef:000102030405060708090a0b0c0d0e0f";

/// A well-formed pipe name.
fn pipe() -> String {
    format!("{PIPE_PREFIX}{HASH}")
}

/// The UTF-16LE code units of `text`, as bytes.
fn le_units(units: &[u16]) -> Vec<u8> {
    units.iter().flat_map(|u| u.to_le_bytes()).collect()
}

/// The pipe of the canonical path whose code units are `units`.
fn pipe_of(units: &[u16]) -> PathBuf {
    PathBuf::from(format!(
        r"\\.\pipe\rdy-{}",
        blake3::hash(&le_units(units)).to_hex()
    ))
}

/// Yields `byte` for ever, and counts how many a reader took.
struct Endless {
    byte: u8,
    taken: usize,
}

impl Read for Endless {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        buf.fill(self.byte);
        self.taken += buf.len();
        Ok(buf.len())
    }
}

/// Gives every account access to `path`, beyond this one.
#[cfg(windows)]
fn widen(path: &Path) {
    run(
        "icacls",
        &[path.as_os_str(), "/grant".as_ref(), "*S-1-1-0:(M)".as_ref()],
    );
}

#[cfg(windows)]
fn run(program: &str, args: &[&std::ffi::OsStr]) {
    let out = std::process::Command::new(program)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{program} {args:?}: {} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A junction at `at` to the directory `target`.
#[cfg(windows)]
fn junction(target: &Path, at: &Path) {
    run(
        "cmd",
        &[
            "/C".as_ref(),
            "mklink".as_ref(),
            "/J".as_ref(),
            at.as_os_str(),
            target.as_os_str(),
        ],
    );
}

/// A drive letter for `target`, for as long as it lives.
#[cfg(windows)]
struct Subst(PathBuf);

#[cfg(windows)]
impl Subst {
    fn of(target: &Path) -> Subst {
        let plain = target.to_str().unwrap().trim_start_matches(r"\\?\");
        let letter = ('G'..='Z')
            .rev()
            .map(|c| format!("{c}:"))
            .find(|d| !Path::new(&format!(r"{d}\")).exists())
            .expect("a free drive letter");
        run("subst", &[letter.as_ref(), plain.as_ref()]);
        Subst(PathBuf::from(format!(r"{letter}\")))
    }
}

#[cfg(windows)]
impl Drop for Subst {
    fn drop(&mut self) {
        let letter = self.0.to_str().unwrap().trim_end_matches('\\').to_string();
        let _ = std::process::Command::new("subst")
            .args([&letter, "/D"])
            .output();
    }
}

/// `path`'s 8.3 form; `None` on a volume that makes no short names.
#[cfg(windows)]
#[allow(unsafe_code)]
fn short_name(path: &Path) -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut out = vec![0u16; 32768];
    // SAFETY: `wide` is NUL-terminated and `out` is writable for the length given.
    let n = unsafe { GetShortPathNameW(wide.as_ptr(), out.as_mut_ptr(), out.len() as u32) };
    let short = PathBuf::from(OsString::from_wide(&out[..n as usize]));
    (n != 0 && short != path).then_some(short)
}

#[test]
fn wx15_aliases_reach_one_pipe() {
    // the name itself, on every platform: all 64 digits of the hash of the UTF-16LE code units
    let units: Vec<u16> = r"C:\Proj".encode_utf16().collect();
    let name = pipe_name(&hash_of_units(units.iter().copied()));
    assert_eq!(name, pipe_of(&units));
    assert_eq!(name.as_os_str().len(), PIPE_PREFIX.len() + 64);
    // no folding of case, and no lossy step: an unpaired surrogate is not U+FFFD
    let lower: Vec<u16> = r"c:\proj".encode_utf16().collect();
    assert_ne!(name, pipe_name(&hash_of_units(lower.iter().copied())));
    let (lone, lossy) = ([0x61, 0xD800, 0x62], [0x61, 0xFFFD, 0x62]);
    assert_ne!(
        pipe_name(&hash_of_units(lone.iter().copied())),
        pipe_name(&hash_of_units(lossy.iter().copied()))
    );

    let d = temp();
    let parent = fs::canonicalize(d.path()).unwrap();
    let root = parent.join("Long Project Name");
    let own = project(&root);
    let want = own.socket();
    let id = own.project_id().unwrap();
    let mut aliases: Vec<PathBuf> = vec![root.join("."), root.join("x").join("..")];
    fs::create_dir(root.join("x")).unwrap();
    #[cfg(unix)]
    {
        let link = parent.join("link");
        rdy_store::pio::symlink(&root, &link).unwrap();
        aliases.push(link);
    }
    #[cfg(windows)]
    let _subst = {
        use std::os::windows::ffi::OsStrExt;
        let canonical = fs::canonicalize(&root).unwrap();
        let plain = canonical
            .to_str()
            .unwrap()
            .trim_start_matches(r"\\?\")
            .to_string();
        // the name is the hash of the canonical path's own code units
        let units: Vec<u16> = canonical.as_os_str().encode_wide().collect();
        assert_eq!(want, pipe_of(&units));
        // case, separators, the extended form, and no prefix at all
        aliases.push(PathBuf::from(plain.to_uppercase()));
        aliases.push(PathBuf::from(plain.to_lowercase()));
        aliases.push(PathBuf::from(plain.replace('\\', "/")));
        aliases.push(PathBuf::from(&plain));
        aliases.push(canonical.clone());
        // a junction to it
        let through = parent.join("through");
        junction(Path::new(&plain), &through);
        // Windows may refuse to pass a junction an ordinary account made, with error 448. Such a
        // path reaches no project, and is refused by name, never given a pipe of its own.
        if fs::read_dir(&through).err().and_then(|e| e.raw_os_error()) == Some(448) {
            let e = socket::check_socket_path(&RunDir::of_project(&through)).unwrap_err();
            assert!(
                e.to_string().starts_with(&format!(
                    "could not resolve the canonical project path {}: ",
                    through.display()
                )),
                "{e}"
            );
        } else {
            aliases.push(through);
        }
        // a drive letter for it
        let subst = Subst::of(&canonical);
        aliases.push(subst.0.clone());
        // its 8.3 name
        match short_name(Path::new(&plain)) {
            Some(short) => aliases.push(short),
            None => eprintln!("no short name on this volume: that alias is not exercised"),
        }
        subst
    };
    for alias in &aliases {
        let run = RunDir::of_project(alias);
        assert_eq!(run.socket(), want, "{alias:?}");
        assert_eq!(run.project_id().unwrap(), id, "{alias:?}");
        assert_eq!(run.short_socket(), own.short_socket(), "{alias:?}");
    }
    assert!(socket::check_socket_path(&own).is_ok());

    // a project root that no short form of the name could hold still has one bounded endpoint
    let mut long = parent.clone();
    for _ in 0..12 {
        long = long.join("a-directory-whose-name-is-rather-long");
    }
    fs::create_dir_all(long.join(".rdy")).unwrap();
    let far = RunDir::of_project(&long);
    assert!(far.socket().as_os_str().len() <= rdy_daemon::MAX_SOCKET_PATH);
    assert!(socket::check_socket_path(&far).is_ok());
    #[cfg(windows)]
    {
        use std::ffi::OsString;
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let canonical = fs::canonicalize(&long).unwrap();
        let units: Vec<u16> = canonical.as_os_str().encode_wide().collect();
        assert!(units.len() > 260, "a root past MAX_PATH");
        assert_eq!(far.socket(), pipe_of(&units));
        assert_eq!(far.short_socket(), None);
        // a name that is no valid Unicode keeps its own code units
        let odd = parent.join(OsString::from_wide(&[0x61, 0xD800, 0x62]));
        fs::create_dir_all(&odd).unwrap();
        let units: Vec<u16> = fs::canonicalize(&odd)
            .unwrap()
            .as_os_str()
            .encode_wide()
            .collect();
        assert_eq!(RunDir::of_project(&odd).socket(), pipe_of(&units));
    }
}

#[tokio::test]
async fn wx15_copied_project_ignores_the_old_record() {
    let d = temp();
    let (a_root, b_root) = (d.path().join("a"), d.path().join("b"));
    let (a, b) = (project(&a_root), project(&b_root));
    let (listener, endpoint, a_id) = socket::bind(&a).await.unwrap();
    assert_eq!(a.recorded_socket(), Some(endpoint.clone()));
    // a copy carries the original's record, and finds its own endpoint instead
    b.ensure().unwrap();
    let record = fs::read(a.socket_record()).unwrap();
    write_private_durable(&b.socket_record(), &record).unwrap();
    assert_eq!(b.recorded_socket(), None, "a copied record is stale");
    assert_ne!(b.socket(), endpoint);
    drop(listener);

    // a restore: the same path, another `.rdy`, and the old one's record
    let c_root = d.path().join("c");
    let c = project(&c_root);
    c.ensure().unwrap();
    let own = c.socket();
    let id = c.project_id().unwrap();
    let mut line = own.as_os_str().as_encoded_bytes().to_vec();
    line.push(b'\n');
    line.extend_from_slice(format!("{id}\n").as_bytes());
    write_private_durable(&c.socket_record(), &line).unwrap();
    assert_eq!(
        c.recorded_socket(),
        Some(own.clone()),
        "a stale record is tried"
    );
    fs::rename(c_root.join(".rdy"), c_root.join(".rdy-old")).unwrap();
    let restored = project(&c_root);
    restored.ensure().unwrap();
    write_private_durable(&restored.socket_record(), &line).unwrap();
    assert_ne!(restored.project_id().unwrap(), id);
    assert_eq!(
        restored.recorded_socket(),
        None,
        "a restored record is stale"
    );
    assert_eq!(restored.socket(), own, "the name goes with the path");
    // the record still names the daemon that wrote it, for that daemon's own cleanup
    assert!(restored.record_names(&own, id));
    assert!(a.record_names(&endpoint, a_id));
}

#[test]
fn wx15_pipe_record_is_bounded_and_local() {
    let id: FileId = ID.parse().unwrap();
    let good = format!("{}\n{ID}\n", pipe());
    assert_eq!(
        parse_pipe_record(good.as_bytes()),
        Some((PathBuf::from(pipe()), id))
    );
    let digits = |n: usize| "a".repeat(n);
    let refused: Vec<(&str, Vec<u8>)> = vec![
        ("empty", vec![]),
        ("no endpoint", format!("\n{ID}\n").into()),
        ("no identity", format!("{}\n", pipe()).into()),
        ("no last newline", format!("{}\n{ID}", pipe()).into()),
        (
            "a carriage return",
            format!("{}\r\n{ID}\r\n", pipe()).into(),
        ),
        ("an extra line", format!("{}\n{ID}\nmore\n", pipe()).into()),
        (
            "an extra empty line",
            format!("{}\n{ID}\n\n", pipe()).into(),
        ),
        ("a line before", format!("\n{}\n{ID}\n", pipe()).into()),
        ("a NUL in the name", format!("{}\0\n{ID}\n", pipe()).into()),
        (
            "a NUL in the identity",
            format!("{}\n{ID}\0\n", pipe()).into(),
        ),
        (
            "a remote pipe",
            format!("\\\\server\\pipe\\rdy-{HASH}\n{ID}\n").into(),
        ),
        (
            "an extended name",
            format!("\\\\?\\pipe\\rdy-{HASH}\n{ID}\n").into(),
        ),
        (
            "another case",
            format!("\\\\.\\PIPE\\rdy-{HASH}\n{ID}\n").into(),
        ),
        ("slashes", format!("//./pipe/rdy-{HASH}\n{ID}\n").into()),
        (
            "another pipe",
            format!("\\\\.\\pipe\\other-{HASH}\n{ID}\n").into(),
        ),
        ("a file", format!("C:\\run\\rdy.sock\n{ID}\n").into()),
        ("a Unix socket", format!("/tmp/rdy.sock\n{ID}\n").into()),
        (
            "short hash",
            format!("{PIPE_PREFIX}{}\n{ID}\n", digits(63)).into(),
        ),
        (
            "long hash",
            format!("{PIPE_PREFIX}{}\n{ID}\n", digits(65)).into(),
        ),
        (
            "upper hash",
            format!("{PIPE_PREFIX}{}\n{ID}\n", "A".repeat(64)).into(),
        ),
        (
            "not hex",
            format!("{PIPE_PREFIX}{}\n{ID}\n", "g".repeat(64)).into(),
        ),
        (
            "a path in the name",
            format!("{PIPE_PREFIX}..\\{}\n{ID}\n", digits(61)).into(),
        ),
        ("a Unix identity", format!("{}\n1:2\n", pipe()).into()),
        ("a bad identity", format!("{}\nw:x\n", pipe()).into()),
        (
            "an upper identity",
            format!("{}\n{}\n", pipe(), ID.to_uppercase()).into(),
        ),
        ("not UTF-8", [pipe().as_bytes(), b"\n\xff\xfe\n"].concat()),
    ];
    for (what, bytes) in &refused {
        assert_eq!(parse_pipe_record(bytes), None, "{what}");
    }
    // the bound: a record of at most 256 bytes reaches the parser, and no more is ever read
    assert_eq!(MAX_PIPE_RECORD, 256);
    assert!(good.len() < MAX_PIPE_RECORD);
    let padded = |n: usize| {
        let mut b = good.clone().into_bytes();
        b.resize(n, b'\n');
        b
    };
    let (at, over) = (padded(256), padded(257));
    assert_eq!(
        read_capped(&at[..], MAX_PIPE_RECORD).unwrap().as_deref(),
        Some(&at[..])
    );
    assert_eq!(read_capped(&over[..], MAX_PIPE_RECORD).unwrap(), None);
    assert_eq!(parse_pipe_record(&at), None, "extra lines");
    assert_eq!(parse_pipe_record(&over), None, "too long");
    let mut endless = Endless {
        byte: b'a',
        taken: 0,
    };
    assert_eq!(read_capped(&mut endless, MAX_PIPE_RECORD).unwrap(), None);
    assert!(endless.taken <= 257, "read {} bytes", endless.taken);

    // on a real file system, through the run directory
    #[cfg(windows)]
    {
        let d = temp();
        let run = project(d.path());
        run.ensure().unwrap();
        let own = run.project_id().unwrap();
        let record = run.socket_record();
        write_private_durable(&record, format!("{}\n{own}\n", pipe()).as_bytes()).unwrap();
        assert_eq!(run.recorded_socket(), Some(PathBuf::from(pipe())));
        for (what, bytes) in &refused {
            write_private_durable(&record, bytes).unwrap();
            assert_eq!(run.recorded_socket(), None, "{what}");
        }
        for n in [256, 257, 1 << 20] {
            let mut bytes = format!("{}\n{own}\n", pipe()).into_bytes();
            bytes.resize(n, b'\n');
            write_private_durable(&record, &bytes).unwrap();
            assert_eq!(run.recorded_socket(), None, "{n} bytes");
        }
    }
}

#[test]
fn wx15_pid_record_requires_creation_time() {
    assert_eq!(
        pipe_pid_record(1234, 133_000_000_000_000_000),
        b"1234\n133000000000000000\n"
    );
    let good = pipe_pid_record(u32::MAX, u64::MAX);
    assert_eq!(parse_pipe_pid_record(&good), Some((u32::MAX, u64::MAX)));
    assert!(good.len() <= MAX_PID_RECORD);
    assert_eq!(MAX_PID_RECORD, 64);
    let refused: &[(&str, &[u8])] = &[
        ("empty", b""),
        ("no creation time", b"1234\n"),
        ("no pid", b"\n1234\n"),
        ("no last newline", b"1234\n5678"),
        ("an extra line", b"1234\n5678\n9\n"),
        ("an extra empty line", b"1234\n5678\n\n"),
        ("a carriage return", b"1234\r\n5678\r\n"),
        ("a plus", b"+1234\n5678\n"),
        ("a minus", b"1234\n-5678\n"),
        ("a space", b" 1234\n5678\n"),
        ("a trailing space", b"1234 \n5678\n"),
        ("hex", b"0x10\n5678\n"),
        ("a leading zero", b"01234\n5678\n"),
        ("no process", b"0\n5678\n"),
        ("no time", b"1234\n0\n"),
        ("a pid past u32", b"4294967296\n5678\n"),
        ("a time past u64", b"1234\n18446744073709551616\n"),
        ("a NUL", b"1234\0\n5678\n"),
        ("not UTF-8", b"1234\n\xff\n"),
    ];
    for (what, bytes) in refused {
        assert_eq!(parse_pipe_pid_record(bytes), None, "{what}");
    }
    let mut endless = Endless {
        byte: b'1',
        taken: 0,
    };
    assert_eq!(read_capped(&mut endless, MAX_PID_RECORD).unwrap(), None);
    assert!(endless.taken <= 65, "read {} bytes", endless.taken);
    assert_eq!(
        read_capped(&[b'1'; 64][..], MAX_PID_RECORD)
            .unwrap()
            .map(|b| b.len()),
        Some(64)
    );
    assert_eq!(read_capped(&[b'1'; 65][..], MAX_PID_RECORD).unwrap(), None);

    // what a bind writes, read back through the run directory
    let d = temp();
    let run = project(d.path());
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let ((listener, endpoint, _), synced) =
        record_syncs(|| rt.block_on(socket::bind(&run)).unwrap());
    assert!(
        synced.contains(&run.path().to_path_buf()),
        "the run dir is flushed: {synced:?}"
    );
    let record = run.pid_record().expect("the record of this process");
    assert_eq!(record.pid(), std::process::id());
    #[cfg(windows)]
    {
        let bytes = fs::read(run.pid_file()).unwrap();
        assert_eq!(bytes, pipe_pid_record(std::process::id(), record.created()));
        assert_eq!(bytes, rdy_daemon::sys::own_pid_record().unwrap());
        // the system's word for the daemon at the other end of the pipe is the record's
        let stream = rt.block_on(rdy_daemon::sys::connect(&endpoint)).unwrap();
        let me = rdy_daemon::sys::own_account().unwrap();
        let peer = rdy_daemon::sys::check_daemon(&stream, &endpoint, me).unwrap();
        assert!(peer.is_process(record.pid(), record.created()));
        drop(stream);
        // a pid alone proves nothing here
        write_private_durable(&run.pid_file(), format!("{}\n", record.pid()).as_bytes()).unwrap();
        assert!(run.pid_record().is_none(), "a pid with no creation time");
        for n in [65, 1 << 20] {
            let mut bytes = format!("{}\n{}\n", record.pid(), record.created()).into_bytes();
            bytes.resize(n, b'\n');
            write_private_durable(&run.pid_file(), &bytes).unwrap();
            assert!(run.pid_record().is_none(), "{n} bytes");
        }
    }
    drop(listener);
    let _ = endpoint;
    #[cfg(unix)]
    {
        assert_eq!(
            fs::read(run.pid_file()).unwrap(),
            format!("{}\n", std::process::id()).into_bytes()
        );
        for (text, want) in [
            ("42\n", Some(42)),
            ("42", Some(42)),
            ("x\n", None),
            ("-1\n", None),
            ("", None),
        ] {
            fs::write(run.pid_file(), text).unwrap();
            assert_eq!(run.pid_record().map(|r| r.pid()), want, "{text:?}");
        }
    }
}

#[test]
fn wx15_run_directory_reparse_or_acl_violation_is_refused() {
    let d = temp();
    let elsewhere = temp();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    // a link in place of the run directory is refused, and nothing lands at its target
    let root = d.path().join("linked");
    let run = project(&root);
    #[cfg(unix)]
    {
        rdy_store::pio::symlink(elsewhere.path(), run.path()).unwrap();
        let e = run.ensure().unwrap_err();
        assert_eq!(
            e.to_string(),
            format!("{} exists and is not a directory", run.path().display())
        );
        assert_eq!(
            rt.block_on(socket::bind(&run)).unwrap_err().to_string(),
            e.to_string()
        );
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::symlink_dir;
        junction(elsewhere.path(), run.path());
        let want = format!(
            "{} is a reparse point; rdy will not follow it",
            run.path().display()
        );
        assert_eq!(run.ensure().unwrap_err().to_string(), want);
        assert_eq!(
            rt.block_on(socket::bind(&run)).unwrap_err().to_string(),
            want
        );
        fs::remove_dir(run.path()).unwrap();
        symlink_dir(elsewhere.path(), run.path()).unwrap();
        assert_eq!(run.ensure().unwrap_err().to_string(), want);
    }
    assert_eq!(
        fs::read_dir(elsewhere.path()).unwrap().count(),
        0,
        "written through"
    );

    // a file in place of the run directory
    let root = d.path().join("file");
    let run = project(&root);
    fs::write(run.path(), b"x").unwrap();
    assert_eq!(
        run.ensure().unwrap_err().to_string(),
        format!("{} exists and is not a directory", run.path().display())
    );

    #[cfg(windows)]
    {
        use rdy_store::pio::{self, Ace, ObjectKind};
        use std::os::windows::fs::symlink_file;
        let me = pio::current_account().unwrap();
        let only_me = |path: &Path, kind: ObjectKind| {
            let f = pio::open_no_follow(fs::OpenOptions::new().read(true), path).unwrap();
            let s = pio::security_of(&f).unwrap();
            assert_eq!(s.owner, Some(me), "{path:?}: the owner");
            assert!(s.protected, "{path:?}: no inherited access");
            assert_eq!(
                s.dacl,
                Some(vec![Ace::private(me, kind)]),
                "{path:?}: the ACEs"
            );
        };

        // a directory of another account is never repaired, and stays as it is
        let theirs = PathBuf::from(std::env::var_os("SystemRoot").unwrap());
        let open = |p: &Path| pio::open_no_follow(fs::OpenOptions::new().read(true), p).unwrap();
        let before = pio::security_of(&open(&theirs)).unwrap();
        assert_eq!(
            rdy_daemon::layout::ensure_run_dir(&theirs)
                .unwrap_err()
                .to_string(),
            format!("{} is not private to this account", theirs.display())
        );
        assert_eq!(pio::security_of(&open(&theirs)).unwrap(), before);

        // every run file of a bind is private, and no file stands for the pipe
        let root = d.path().join("files");
        let run = project(&root);
        let (listener, endpoint, _) = rt.block_on(socket::bind(&run)).unwrap();
        only_me(run.path(), ObjectKind::Dir);
        for name in [".gitignore", "daemon.pid", "daemon.sock"] {
            only_me(&run.path().join(name), ObjectKind::File);
        }
        let mut names: Vec<String> = fs::read_dir(run.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, [".gitignore", "daemon.pid", "daemon.sock"]);
        assert!(endpoint.starts_with(r"\\.\pipe\"));

        // a second bind fails on the live pipe, and leaves the first one's files
        let (pid, record) = (
            fs::read(run.pid_file()).unwrap(),
            fs::read(run.socket_record()).unwrap(),
        );
        let e = rt.block_on(socket::bind(&run)).unwrap_err();
        assert!(
            e.to_string().starts_with(&format!(
                "could not create named pipe {}: ",
                endpoint.display()
            )),
            "{e}"
        );
        assert_eq!(fs::read(run.pid_file()).unwrap(), pid);
        assert_eq!(fs::read(run.socket_record()).unwrap(), record);
        assert_eq!(run.recorded_socket(), Some(endpoint.clone()));

        // a record or a pid file open to others is no record, and a repaired one is one again
        widen(&run.socket_record());
        assert_eq!(run.recorded_socket(), None, "a record others may write");
        widen(&run.pid_file());
        assert!(run.pid_record().is_none(), "a pid file others may write");
        drop(listener);
        write_private_durable(&run.socket_record(), &record).unwrap();
        assert_eq!(run.recorded_socket(), Some(endpoint));

        // a link at a record's name is no record, and a link at its temp name stops the bind
        let victim = elsewhere.path().join("victim");
        fs::write(&victim, b"keep").unwrap();
        fs::remove_file(run.socket_record()).unwrap();
        symlink_file(&victim, run.socket_record()).unwrap();
        assert_eq!(run.recorded_socket(), None, "a link at the record's name");
        fs::remove_file(run.socket_record()).unwrap();
        fs::remove_file(run.pid_file()).unwrap();
        symlink_file(&victim, run.pid_file()).unwrap();
        assert!(run.pid_record().is_none(), "a link at the pid file's name");
        fs::remove_file(run.pid_file()).unwrap();
        let tmp = run.path().join("daemon.pid.tmp");
        symlink_file(&victim, &tmp).unwrap();
        let e = rt.block_on(socket::bind(&run)).unwrap_err();
        assert!(
            e.to_string()
                .ends_with("is a reparse point; rdy will not follow it"),
            "{e}"
        );
        assert_eq!(fs::read(&victim).unwrap(), b"keep", "written through");
        assert!(!run.pid_file().exists());
        assert_eq!(run.recorded_socket(), None);

        // this account's run directory that is open to others is narrowed, as on Unix
        let root = d.path().join("open");
        let run = project(&root);
        pio::create_private_dir(run.path()).unwrap();
        widen(run.path());
        assert_eq!(
            rdy_daemon::sys::dir_state(run.path()).unwrap(),
            rdy_daemon::sys::DirState::NotPrivate
        );
        run.ensure().unwrap();
        only_me(run.path(), ObjectKind::Dir);
    }
}
