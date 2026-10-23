//! ha13 (final review): a take bounds the memory of a snapshot check by the Publications limits,
//! not by the upload. A landing snapshot whose text passes the aggregate text limit is refused
//! before its blocks are all kept: the bytes live during the take follow the limit, not the
//! much larger snapshot. Its own binary: the global allocator below counts what this thread
//! holds, and no other test shares the process.
mod common;

use rdy_engine::stream_take::take;
use rdy_engine::*;
use rdy_model::*;
use rdy_store::take_batch::UploadTarget;
use serde_json::json;
use std::collections::BTreeSet;
use std::fs::File;

/// A global allocator that keeps the live bytes this thread holds, and their peak.
mod live_alloc {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    pub struct Live;

    thread_local! {
        static LIVE: Cell<isize> = const { Cell::new(0) };
        static PEAK: Cell<isize> = const { Cell::new(0) };
    }

    fn grow(n: usize) {
        let _ = LIVE.try_with(|l| {
            let now = l.get().saturating_add(n as isize);
            l.set(now);
            let _ = PEAK.try_with(|p| p.set(p.get().max(now)));
        });
    }

    fn shrink(n: usize) {
        let _ = LIVE.try_with(|l| l.set(l.get().saturating_sub(n as isize)));
    }

    // SAFETY: every call goes to `System` with the caller's arguments unchanged.
    #[allow(unsafe_code)]
    unsafe impl GlobalAlloc for Live {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            grow(layout.size());
            // SAFETY: the caller's layout, as `GlobalAlloc::alloc` requires.
            unsafe { System.alloc(layout) }
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            grow(layout.size());
            // SAFETY: the caller's layout, as `GlobalAlloc::alloc_zeroed` requires.
            unsafe { System.alloc_zeroed(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            shrink(layout.size());
            // SAFETY: `ptr` came from `System` with `layout` (every allocation here is System's).
            unsafe { System.dealloc(ptr, layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            shrink(layout.size());
            grow(new_size);
            // SAFETY: `ptr` came from `System` with `layout`; the caller's new size.
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }

    /// `f`'s result and the most bytes above the start this thread held while it ran.
    pub fn peak_above_start<T>(f: impl FnOnce() -> T) -> (T, usize) {
        let start = LIVE.with(Cell::get);
        PEAK.with(|p| p.set(start));
        let out = f();
        (out, (PEAK.with(Cell::get) - start).max(0) as usize)
    }
}

#[global_allocator]
static ALLOC: live_alloc::Live = live_alloc::Live;

fn writer() -> Session {
    Session::human("human:founder".into(), Role::Writer).unwrap()
}

fn founder() -> Session {
    Session::human("human:founder".into(), Role::Approver).unwrap()
}

#[test]
fn ha13_an_oversized_snapshot_is_refused_within_the_text_limit() {
    const BODY: usize = 512 * 1024;
    const BODIES: usize = 40;
    const TEXT_LIMIT: usize = 4 * 1024 * 1024;
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let mut r = Rdy::init(&src, "rdy-standard").unwrap();
    let doc = r
        .write(
            &writer(),
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(json!([
                    {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}}
                ]))
                .unwrap(),
                justification: None,
                message: "doc".into(),
            },
        )
        .unwrap()
        .created["$d"]
        .clone();
    // 40 prose bodies of 512 KiB, each legal alone (a text value holds 1 MiB), 20 MiB together:
    // five times the aggregate text limit the take runs under. One change each.
    for i in 0..BODIES {
        let body: String = format!("{i:04} ").repeat(BODY / 5);
        r.write(
            &writer(),
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(json!([
                    {"op": "create", "type": "prose", "parent": doc, "body": body}
                ]))
                .unwrap(),
                justification: None,
                message: format!("body {i}"),
            },
        )
        .unwrap();
    }
    let p = r
        .propose(&writer(), "work", "bodies", Some(common::TEST_SUMMARY))
        .unwrap();
    common::confirm_exceptions(&mut r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap();
    r.tag(&founder(), "v1.0.0", None, None).unwrap();
    let (_, t, tag) = r
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == "v1.0.0")
        .unwrap();
    let path = dir.path().join("v1.rdysync");
    let mut bytes = Vec::new();
    rdy_engine::stream_export::write(&r.read(), t, &BTreeSet::new(), &mut bytes).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    drop(bytes);
    let files = vec![File::open(&path).unwrap()];
    let mut h =
        Rdy::create_holder(&dir.path().join("holder"), r.read().repo_id().unwrap()).unwrap();
    drop(r);

    let _limits = publication::override_limits_for_tests(publication::Limits {
        text: TEXT_LIMIT,
        ..publication::Limits::default()
    });
    let (got, peak) = live_alloc::peak_above_start(|| {
        take(
            &mut h,
            &files,
            UploadTarget {
                tag: "v1.0.0".into(),
                landing: tag.landing,
            },
        )
    });
    let e = got.unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            "E_LIMIT",
            format!("Publication text exceeds the limit of {TEXT_LIMIT} bytes.").as_str()
        ),
        "{e:?}"
    );
    // what the take held follows the limit (and one body read past it), never the 20 MiB
    // snapshot: twice the limit leaves room for the take's own buffers
    assert!(
        peak < 2 * TEXT_LIMIT,
        "the take held {peak} bytes for a text limit of {TEXT_LIMIT}"
    );
    h.fsck().unwrap();
}
