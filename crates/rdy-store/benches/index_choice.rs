//! redb versus fjall on the index workload rdy performs: 100k (32-byte hash -> 12-byte
//! location) entries committed in batches of 10 with fsync, then 100k random point reads.
use criterion::{Criterion, criterion_group, criterion_main};
use fjall::{Database, KeyspaceCreateOptions, PersistMode};
use rdy_model::Hash;
use rdy_store::index::{Index, IndexEntry};
use rdy_store::segment::Location;

fn keys() -> Vec<Hash> {
    (0..100_000u32)
        .map(|i| Hash::of(&i.to_le_bytes()))
        .collect()
}

/// The same 12-byte value for both engines: u32 segment + u64 offset, little endian.
fn value(n: u64) -> [u8; 12] {
    let mut v = [0u8; 12];
    v[4..].copy_from_slice(&n.to_le_bytes());
    v
}

/// Deterministic pseudo-random read order (same for both engines).
fn shuffled(ks: &[Hash]) -> Vec<Hash> {
    let mut v = ks.to_vec();
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    for i in (1..v.len()).rev() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.swap(i, (x % (i as u64 + 1)) as usize);
    }
    v
}

fn redb(c: &mut Criterion) {
    let ks = keys();
    c.bench_function("redb_insert_100k_batched_10", |b| {
        b.iter(|| {
            let dir = tempfile::tempdir().unwrap();
            let idx = Index::open(&dir.path().join("i.redb")).unwrap();
            for (n, chunk) in ks.chunks(10).enumerate() {
                let entries: Vec<IndexEntry> = chunk
                    .iter()
                    .map(|h| {
                        IndexEntry::Object(
                            *h,
                            Location {
                                segment: 0,
                                offset: n as u64,
                            },
                        )
                    })
                    .collect();
                idx.apply(
                    &entries,
                    Location {
                        segment: 0,
                        offset: n as u64,
                    },
                )
                .unwrap();
            }
        })
    });
    let dir = tempfile::tempdir().unwrap();
    let idx = Index::open(&dir.path().join("i.redb")).unwrap();
    for (n, chunk) in ks.chunks(1000).enumerate() {
        let entries: Vec<IndexEntry> = chunk
            .iter()
            .map(|h| {
                IndexEntry::Object(
                    *h,
                    Location {
                        segment: 0,
                        offset: n as u64,
                    },
                )
            })
            .collect();
        idx.apply(
            &entries,
            Location {
                segment: 0,
                offset: n as u64,
            },
        )
        .unwrap();
    }
    let order = shuffled(&ks);
    c.bench_function("redb_get_100k", |b| {
        b.iter(|| {
            for h in &order {
                idx.object(h).unwrap().unwrap();
            }
        })
    });
}

fn fjall(c: &mut Criterion) {
    let ks = keys();
    c.bench_function("fjall_insert_100k_batched_10", |b| {
        b.iter(|| {
            let dir = tempfile::tempdir().unwrap();
            let db = Database::builder(dir.path()).open().unwrap();
            let objects = db
                .keyspace("objects", KeyspaceCreateOptions::default)
                .unwrap();
            for (n, chunk) in ks.chunks(10).enumerate() {
                let mut batch = db.batch().durability(Some(PersistMode::SyncAll));
                for h in chunk {
                    batch.insert(&objects, h.0.to_vec(), value(n as u64).to_vec());
                }
                batch.commit().unwrap();
            }
        })
    });
    let dir = tempfile::tempdir().unwrap();
    let db = Database::builder(dir.path()).open().unwrap();
    let objects = db
        .keyspace("objects", KeyspaceCreateOptions::default)
        .unwrap();
    for (n, chunk) in ks.chunks(1000).enumerate() {
        let mut batch = db.batch().durability(Some(PersistMode::SyncAll));
        for h in chunk {
            batch.insert(&objects, h.0.to_vec(), value(n as u64).to_vec());
        }
        batch.commit().unwrap();
    }
    let order = shuffled(&ks);
    c.bench_function("fjall_get_100k", |b| {
        b.iter(|| {
            for h in &order {
                objects.get(h.0).unwrap().unwrap();
            }
        })
    });
}

criterion_group! { name = benches; config = Criterion::default().sample_size(10); targets = redb, fjall }
criterion_main!(benches);
