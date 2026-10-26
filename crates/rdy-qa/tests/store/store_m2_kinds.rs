//! sg6 (spec §15.3 store, extended): every M2 object kind and ref family survives a reindex and
//! fsck; a store of the next format is refused before anything is written (D-W7RBV).
use rdy_engine::{Engine, Local, Rdy};
use rdy_qa::inbox::on;
use rdy_qa::invariants::check_all;
use rdy_qa::reindex_check::reindex_reproduces;
use rdy_store::StoreCheck;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn fields(c: &StoreCheck) -> [(&'static str, usize); 11] {
    [
        ("views", c.views),
        ("view_ops", c.view_ops),
        ("landed_changes", c.landed_changes),
        ("relayed", c.relayed),
        ("conflicts_recorded", c.conflicts_recorded),
        ("intents", c.intents),
        ("alt_sets", c.alt_sets),
        ("reviews", c.reviews),
        ("tensions", c.tensions),
        ("inboxes", c.inboxes),
        ("transitions", c.transitions),
    ]
}

/// `check_objects` and `fsck` of the project at `dir` (opened through `Local`, closed after).
fn counts(dir: &Path) -> (StoreCheck, serde_json::Value) {
    let e = Local::open(dir).unwrap();
    let check = e.read().reader().check_objects().unwrap();
    let fsck = on(&e, |r| r.fsck().unwrap());
    e.close().unwrap();
    (check, serde_json::to_value(fsck).unwrap())
}

#[test]
fn sg6_every_m2_kind_and_ref_family_survives_reindex_and_fsck() {
    let dir = rdy_qa::every_kind::project();
    let (check, fsck) = counts(dir.path());
    eprintln!("sg6 store: {check:?}; fsck {fsck}");
    for (name, n) in fields(&check) {
        assert!(n >= 1, "StoreCheck.{name} is {n}: {check:?}");
    }
    assert_eq!(fsck["unlogged_moves"], 0, "{fsck}");
    for key in ["intents", "alt_sets", "reviews", "transitions"] {
        assert!(fsck[key].as_u64().unwrap() >= 1, "fsck {key}: {fsck}");
    }
    {
        let r = Rdy::open(dir.path()).unwrap();
        let st = r.read();
        let reader = st.reader();
        for family in rdy_qa::every_kind::FAMILIES {
            assert!(
                reader.count_refs(family).unwrap() >= 1,
                "no ref of family {family}"
            );
        }
        r.close().unwrap();
    }
    // a reindex logs no batch: the counts stay
    let e = Local::open(dir.path()).unwrap();
    on(&e, |r| r.reindex().unwrap());
    e.close().unwrap();
    let (check2, fsck2) = counts(dir.path());
    assert_eq!(check2, check, "check_objects after the reindex");
    assert_eq!(fsck2, fsck, "fsck after the reindex");
    reindex_reproduces(dir.path()).unwrap_or_else(|v| panic!("{v:#?}"));
    let r = Rdy::open(dir.path()).unwrap();
    check_all(&r).unwrap_or_else(|v| panic!("{v:#?}"));
    r.close().unwrap();
}

/// Every path under `dir` with its contents (None for directories).
fn tree(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.insert(p.clone(), None);
            out.extend(tree(&p));
        } else {
            out.insert(p.clone(), Some(std::fs::read(&p).unwrap()));
        }
    }
    out
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &dest);
        } else {
            std::fs::copy(e.path(), &dest).unwrap();
        }
    }
}

#[test]
fn sg6_a_store_of_the_next_format_is_refused_and_untouched() {
    // guard of fmt2_, on a copy of an M2 project with data
    let dir = rdy_qa::every_kind::project();
    let copy = tempfile::tempdir().unwrap();
    let project = copy.path().join("p");
    copy_tree(dir.path(), &project);
    // a feature this rdy does not know, of class incompatible
    let next = rdy_store::format::StoreFeatures::of(vec![rdy_store::format::StoreFeature {
        name: "future_x".into(),
        class: rdy_store::format::FeatureClass::Incompatible,
        required_rdy: "99.0.0".into(),
    }]);
    // the log's features ref names it too: a FORMAT alone that lists more than its sealed log
    // is a failed first use's leftover, and opens
    let mut s = rdy_store::Store::open(&project.join(".rdy/store")).unwrap();
    s.set_refs_unchecked_for_tests(vec![(
        rdy_store::format::REF_STORE_FEATURES.into(),
        next.to_json(),
    )])
    .unwrap();
    drop(s);
    std::fs::write(project.join(".rdy/store/FEATURES"), next.to_text()).unwrap();
    let before = tree(&project);
    let e = Rdy::open(&project)
        .err()
        .expect("a store of the next format is refused");
    assert_eq!(e.code, "E_STORE_FORMAT", "{e}");
    assert_eq!(tree(&project), before, "nothing under the project changed");
    // the copy's source still opens: the refusal is about the format alone
    Rdy::open(dir.path()).unwrap().close().unwrap();
}

#[test]
fn dl3_the_every_kind_project_holds_every_kind_and_ref_family() {
    let dir = rdy_qa::every_kind::project();
    let r = Rdy::open(dir.path()).unwrap();
    let f = rdy_qa::every_kind::facts(&r);
    for k in rdy_qa::every_kind::V11_KINDS {
        // the facts count frames by raw kind: a kind's frames of either encoding
        let count = |raw: u8| f["objects"][raw.to_string()].as_u64().unwrap_or(0);
        assert!(
            count(k) + count(k | rdy_store::codec::VERSIONED) >= 1,
            "no frame of kind {k}: {f}"
        );
    }
    for family in rdy_qa::every_kind::FAMILIES {
        assert!(
            f["refs"]
                .as_object()
                .unwrap()
                .keys()
                .any(|n| n.starts_with(family)),
            "no ref of family {family}"
        );
    }
    assert_eq!(f["fsck"]["unlogged_moves"], 0);
    assert!(f["landings"].as_array().unwrap().len() >= 3);
    assert!(dir.path().join(".rdyignore").is_file());
    assert!(dir.path().join(".rdy/local.yaml").is_file());
    r.close().unwrap();
}
