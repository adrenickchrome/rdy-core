//! rpl14 (repo links, D-Y49PX): suspects follow edges to blocks cited from a linked project,
//! with today's rules. Foreign nodes are keyed by repo and block id and hold the current
//! delivered version and its derived facts. A ref, or an edge in the suspect links (with
//! `deviates_from`), whose foreign pin differs from the current version's content is `changed`;
//! a pinned target that no delivery selects is `deleted`. A cited block that is superseded or
//! revoked makes its citer suspect through any edge, and the suspect names a successor only when
//! the selection holds it. Local suspects and their identities do not change.
use rdy_graph::foreign::{FOREIGN_SUSPECT_LINKS, ForeignNodes, ForeignSuspect, suspects_of};
use rdy_graph::{Graph, SUSPECT_LINKS};
use rdy_model::foreign::{
    ForeignDerived, ForeignId, ForeignLink, ForeignParts, ForeignPin, ForeignVersion,
};
use rdy_model::*;
use std::collections::BTreeMap;

const CITED: RepoId = RepoId([7; 16]);
const OTHER: RepoId = RepoId([9; 16]);

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn fid(repo: RepoId, s: &str) -> ForeignId {
    ForeignId::new(repo, id(s))
}

fn content(tag: &str) -> Hash {
    Hash::of(tag.as_bytes())
}

/// A delivered version of `i` with content `tag` and derived facts `derived`.
fn version(i: &str, tag: &str, derived: ForeignDerived) -> ForeignVersion {
    ForeignVersion {
        id: id(i),
        version: Hash::of(format!("version {i} {tag}").as_bytes()),
        content: content(tag),
        encoding: 0,
        derived,
    }
}

fn accepted() -> ForeignDerived {
    ForeignDerived {
        status: Some("accepted".into()),
        lifecycle: None,
        superseded_by: vec![],
    }
}

/// A local claim with no foreign part.
fn claim(i: &str) -> Block {
    Block {
        id: id(i),
        block_type: "claim".into(),
        parent: Some(id("s-SSSSS")),
        order: "V".into(),
        fields: BTreeMap::new(),
        body: String::new(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: ForeignParts::default(),
        read_identity: None,
    }
}

/// `b` links to `target` by `kind` and pins it to content `tag`.
fn link(b: &mut Block, kind: &str, target: &ForeignId, tag: &str) {
    b.foreign
        .links
        .entry(kind.into())
        .or_default()
        .push(ForeignLink {
            target: target.clone(),
            part: None,
        });
    pin(b, target, tag);
}

fn pin(b: &mut Block, target: &ForeignId, tag: &str) {
    b.foreign.pins.insert(
        target.clone(),
        ForeignPin {
            content: content(tag),
            encoding: 0,
        },
    );
}

/// `b`'s body cites `target` in the stored form and pins it to content `tag`.
fn cite(b: &mut Block, target: &ForeignId, tag: &str) {
    b.body
        .push_str(&format!("See {{{{{}:{}}}}}. ", target.repo, target.id));
    pin(b, target, tag);
}

fn nodes(repo: RepoId, versions: &[ForeignVersion]) -> ForeignNodes {
    let mut n = ForeignNodes::default();
    n.add_selection(repo, versions);
    n
}

fn found(b: &Block, n: &ForeignNodes) -> Vec<(String, String, String, Option<String>)> {
    suspects_of(b, n)
        .into_iter()
        .map(|s: ForeignSuspect| {
            (
                s.block.to_string(),
                s.target.to_string(),
                s.reason,
                s.successor.map(|x| x.to_string()),
            )
        })
        .collect()
}

fn row(block: &str, target: &ForeignId, reason: &str) -> (String, String, String, Option<String>) {
    (block.into(), target.to_string(), reason.into(), None)
}

#[test]
fn rpl14_content_mismatch_is_suspect() {
    // the foreign suspect links are the local ones, in order, then deviates_from
    let mut expected: Vec<&str> = SUSPECT_LINKS.to_vec();
    expected.push("deviates_from");
    assert_eq!(FOREIGN_SUSPECT_LINKS.to_vec(), expected);

    let (x, y, z, w) = (
        fid(CITED, "d-FRGNX"),
        fid(CITED, "d-FRGNY"),
        fid(CITED, "d-FRGNZ"),
        fid(CITED, "d-FRGNW"),
    );
    let mut c = claim("c-CCCCC");
    cite(&mut c, &x, "x1");
    link(&mut c, "depends_on", &y, "y1");
    link(&mut c, "affects", &z, "z1");
    let mut dev = claim("d-VVVVV");
    dev.block_type = "decision".into();
    link(&mut dev, "deviates_from", &w, "w1");

    // every pin holds the current content: nothing is suspect
    let same = nodes(
        CITED,
        &[
            version("d-FRGNX", "x1", accepted()),
            version("d-FRGNY", "y1", accepted()),
            version("d-FRGNZ", "z1", accepted()),
            version("d-FRGNW", "w1", accepted()),
        ],
    );
    assert!(found(&c, &same).is_empty());
    assert!(found(&dev, &same).is_empty());

    // a new delivery changes every target's content: a reference, a premise link and
    // deviates_from are changed; a link outside the suspect links is not
    let moved = nodes(
        CITED,
        &[
            version("d-FRGNX", "x2", accepted()),
            version("d-FRGNY", "y2", accepted()),
            version("d-FRGNZ", "z2", accepted()),
            version("d-FRGNW", "w2", accepted()),
        ],
    );
    assert_eq!(
        found(&c, &moved),
        vec![row("c-CCCCC", &x, "changed"), row("c-CCCCC", &y, "changed"),]
    );
    assert_eq!(found(&dev, &moved), vec![row("d-VVVVV", &w, "changed")]);

    // a pin is content and its encoding: another encoding of the same hash is a change
    let mut reencoded = version("d-FRGNX", "x1", accepted());
    reencoded.encoding = 1;
    let n = nodes(
        CITED,
        &[
            reencoded,
            version("d-FRGNY", "y1", accepted()),
            version("d-FRGNZ", "z1", accepted()),
        ],
    );
    assert_eq!(found(&c, &n), vec![row("c-CCCCC", &x, "changed")]);

    // a pinned target that the selection no longer holds is deleted; one of another repo, too
    let gone = nodes(CITED, &[version("d-FRGNZ", "z1", accepted())]);
    assert_eq!(
        found(&c, &gone),
        vec![row("c-CCCCC", &x, "deleted"), row("c-CCCCC", &y, "deleted"),]
    );
    let elsewhere = nodes(
        OTHER,
        &[
            version("d-FRGNX", "x1", accepted()),
            version("d-FRGNY", "y1", accepted()),
        ],
    );
    assert_eq!(found(&c, &elsewhere).len(), 2, "nodes are keyed by repo");

    // the local graph never reads a foreign edge: no local suspect, no local edge
    let g = Graph::build_from([&c, &dev]);
    assert!(g.suspects().is_empty());
    assert!(g.outgoing(&c.id).is_empty());
    assert!(g.outgoing(&dev.id).is_empty());
}

#[test]
fn rpl14_superseded_or_revoked_is_suspect_on_any_edge() {
    let (x, y) = (fid(CITED, "d-FRGNX"), fid(CITED, "d-FRGNY"));
    // `affects` is no suspect link: only a retired target makes it suspect
    let mut c = claim("c-CCCCC");
    link(&mut c, "affects", &x, "x1");
    link(&mut c, "affects", &y, "y1");

    // superseded by a block the selection holds: the suspect names it
    let by_new = ForeignDerived {
        status: Some("accepted".into()),
        lifecycle: None,
        superseded_by: vec![id("d-FRGNN")],
    };
    let n = nodes(
        CITED,
        &[
            version("d-FRGNX", "x1", by_new.clone()),
            version("d-FRGNY", "y1", accepted()),
            version("d-FRGNN", "n1", accepted()),
        ],
    );
    assert_eq!(
        found(&c, &n),
        vec![(
            "c-CCCCC".into(),
            x.to_string(),
            "superseded".into(),
            Some("d-FRGNN".into())
        )]
    );

    // the successor is offered only when the selection holds it
    let n = nodes(
        CITED,
        &[
            version("d-FRGNX", "x1", by_new),
            version("d-FRGNY", "y1", accepted()),
        ],
    );
    assert_eq!(found(&c, &n), vec![row("c-CCCCC", &x, "superseded")]);

    // a revoked target is suspect through any edge, and the strongest reason wins over a
    // content change on the same pair
    let revoked = ForeignDerived {
        status: Some("revoked".into()),
        lifecycle: None,
        superseded_by: vec![],
    };
    let mut both = claim("c-BBBBB");
    cite(&mut both, &y, "y0");
    link(&mut both, "affects", &y, "y0");
    let n = nodes(
        CITED,
        &[
            version("d-FRGNX", "x1", accepted()),
            version("d-FRGNY", "y1", revoked),
        ],
    );
    assert_eq!(found(&c, &n), vec![row("c-CCCCC", &y, "revoked")]);
    assert_eq!(found(&both, &n), vec![row("c-BBBBB", &y, "revoked")]);

    // a status the cited side names superseded is superseded, with no successor to offer
    let named = ForeignDerived {
        status: Some("superseded".into()),
        lifecycle: None,
        superseded_by: vec![],
    };
    let n = nodes(
        CITED,
        &[
            version("d-FRGNX", "x1", named),
            version("d-FRGNY", "y1", accepted()),
        ],
    );
    assert_eq!(found(&c, &n), vec![row("c-CCCCC", &x, "superseded")]);

    // a local decision's own supersedes or revokes link is the cause, never a suspect
    let mut d = claim("d-DDDDD");
    d.block_type = "decision".into();
    link(&mut d, "supersedes", &x, "x0");
    let n = nodes(
        CITED,
        &[version(
            "d-FRGNX",
            "x1",
            ForeignDerived {
                status: Some("revoked".into()),
                lifecycle: None,
                superseded_by: vec![],
            },
        )],
    );
    assert!(found(&d, &n).is_empty());
}
