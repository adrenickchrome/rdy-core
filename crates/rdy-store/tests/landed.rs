//! The change → landing index (plan M2a1, lidx1).
use rdy_model::*;
use rdy_store::Store;
use std::collections::BTreeMap;

fn block(id: &str, body: &str) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: "claim".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn prov(role: Role) -> Provenance {
    Provenance {
        actor: "someone".into(),
        run: None,
        role,
        at_ms: 1,
    }
}

fn create(view: &str, id: &str) -> Change {
    Change {
        view: view.into(),
        ops: vec![Op::Create {
            block: block(id, "x"),
        }],
        deps: vec![],
        provenance: prov(Role::Writer),
        justification: None,
        message: String::new(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

#[test]
fn lidx1_every_landed_change_names_its_landing_and_reindex_rebuilds_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    let c1 = s.commit_to_view("work", create("work", "b-00001")).unwrap();
    let c2 = s.commit_to_view("work", create("work", "b-00002")).unwrap();
    let p = s.propose("work", "two", prov(Role::Writer)).unwrap();
    let l = s.land(&p, prov(Role::Approver)).unwrap();
    let c3 = s.commit_to_view("work", create("work", "b-00003")).unwrap();
    let check = |s: &Store| {
        assert_eq!(s.landing_of(&c1).unwrap(), Some(l));
        assert_eq!(s.landing_of(&c2).unwrap(), Some(l));
        assert_eq!(s.landing_of(&c3).unwrap(), None);
    };
    check(&s);
    s.reindex().unwrap();
    check(&s);
}
