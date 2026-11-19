//! The map cache charges what its maps hold: the bytes it reports for a kept map are the bytes
//! the allocator handed out for it, within a small margin, whatever shape the map came in. Its
//! own binary: the global allocator below counts what this thread holds, and no other test
//! shares the process.
use rdy_model::{BlockId, Hash};
use rdy_store::snapshot::{BlockMap, CacheTag, MapCache};
use std::sync::Arc;

/// A global allocator that keeps the live bytes this thread holds.
mod live_alloc {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    pub struct Live;

    thread_local! {
        static LIVE: Cell<isize> = const { Cell::new(0) };
    }

    fn add(n: isize) {
        let _ = LIVE.try_with(|l| l.set(l.get().saturating_add(n)));
    }

    // SAFETY: every call goes to `System` with the caller's arguments unchanged.
    #[allow(unsafe_code)]
    unsafe impl GlobalAlloc for Live {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            add(layout.size() as isize);
            // SAFETY: the caller's layout, as `GlobalAlloc::alloc` requires.
            unsafe { System.alloc(layout) }
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            add(layout.size() as isize);
            // SAFETY: the caller's layout, as `GlobalAlloc::alloc_zeroed` requires.
            unsafe { System.alloc_zeroed(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            add(-(layout.size() as isize));
            // SAFETY: `ptr` came from `System` with `layout` (every allocation here is System's).
            unsafe { System.dealloc(ptr, layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            add(new_size as isize - layout.size() as isize);
            // SAFETY: `ptr` came from `System` with `layout`; the caller's new size.
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }

    /// The bytes this thread holds now.
    pub fn live() -> isize {
        LIVE.with(Cell::get)
    }
}

#[global_allocator]
static ALLOC: live_alloc::Live = live_alloc::Live;

const DIGITS: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// The `i`th id: prefix `c`, `cl` or `doc` by turn, and a 5-digit body.
fn id(i: usize) -> BlockId {
    let prefix = ["c", "cl", "doc"][i % 3];
    let body: String = (0..5)
        .map(|k| DIGITS[(i >> (5 * k)) & 31] as char)
        .collect();
    BlockId::parse(&format!("{prefix}-{body}")).unwrap()
}

/// A map of `n` entries, inserted one at a time in a scattered order (the worst node fill).
fn scattered(n: usize) -> BlockMap {
    let mut m = BlockMap::new();
    let step = 7919; // prime, so `i * step % n` visits every index when n is not its multiple
    for i in 0..n {
        let k = (i * step) % n;
        m.insert(id(k), Hash([k as u8; 32]));
    }
    m
}

/// A map of `n` entries, inserted in order.
fn sorted(n: usize) -> BlockMap {
    (0..n).map(|k| (id(k), Hash([k as u8; 32]))).collect()
}

/// What the cache charges for `map` and what the allocator counted for keeping it.
fn charged_and_held(map: BlockMap) -> (usize, usize) {
    let cache = MapCache::new();
    let tag = CacheTag {
        source: 1,
        epoch: 0,
    };
    let map = Arc::new(map);
    let before = live_alloc::live();
    cache.insert(tag, Hash([9; 32]), Arc::clone(&map));
    let held = (live_alloc::live() - before) as usize;
    (cache.stats().1, held)
}

#[test]
fn kc1_the_map_cache_charges_the_bytes_its_maps_hold() {
    for n in [1_000, 20_000, 100_000] {
        for (shape, map) in [("scattered", scattered(n)), ("sorted", sorted(n))] {
            assert_eq!(map.len(), n);
            let (charged, held) = charged_and_held(map);
            let off = charged.abs_diff(held) as f64 / held as f64;
            eprintln!("{n} {shape}: charged {charged} held {held}");
            assert!(
                off < 0.01,
                "{n} entries ({shape}): charged {charged} bytes, held {held} ({:.1}% off)",
                off * 100.0
            );
        }
    }
}
