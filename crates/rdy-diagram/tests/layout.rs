//! rdb1 (D-438Y2): top-level subgraphs that each set their own direction are drawn apart. The
//! geometry pin reads the drawn SVG's rectangles and title anchors as integer hundredths with
//! the library's one reader (`rdy_diagram::layout`), which `render` also uses, and compares
//! integers only. Std only.

use rdy_diagram::layout::{self, Layout, show};
use rdy_diagram::{Diagram, render};
use std::path::PathBuf;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/diagram")
}

fn read(name: &str) -> Vec<u8> {
    let path = fixtures().join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Every way the drawing breaks the four assertions, by name and numbers.
fn violations(svg: &str, map: &[(&str, [&str; 3])]) -> Vec<String> {
    let drawing = layout::read(svg).unwrap_or_else(|e| panic!("{e}"));
    let nodes = layout::nodes(svg, &drawing).unwrap_or_else(|e| panic!("{e}"));
    let Layout {
        size: (w, h),
        subgraphs,
        ..
    } = &drawing;
    let rect = |id: &str| {
        subgraphs
            .iter()
            .find(|s| s.id == id)
            .unwrap_or_else(|| panic!("no subgraph {id}"))
            .rect
    };
    let mut out = Vec::new();
    // 1. no two rectangles, neither inside the other, have intersecting interiors
    for (a, b) in layout::overlapping(&drawing) {
        out.push(format!(
            "{a} and {b} overlap: {a} {} and {b} {}",
            rect(a),
            rect(b)
        ));
    }
    for a in subgraphs {
        // 2. every rectangle lies inside the viewBox
        let r = a.rect;
        if r.x < 0 || r.y < 0 || r.x + r.w > *w || r.y + r.h > *h {
            out.push(format!(
                "{} is outside the viewBox (width {}, height {}): {}",
                a.id,
                show(*w),
                show(*h),
                r
            ));
        }
        // 3. each title anchor lies inside its own rectangle, and inside no other rectangle
        // that does not hold its own
        let title = a.title.unwrap_or_else(|| panic!("{} has no title", a.id));
        if !a.rect.has(title) {
            out.push(format!(
                "{}'s title ({}, {}) is outside its rectangle {}",
                a.id,
                show(title.0),
                show(title.1),
                a.rect
            ));
        }
        for b in subgraphs {
            if b.id != a.id && !b.rect.holds(&a.rect) && b.rect.has(title) {
                out.push(format!(
                    "{}'s title ({}, {}) is inside {} {}",
                    a.id,
                    show(title.0),
                    show(title.1),
                    b.id,
                    b.rect
                ));
            }
        }
    }
    // 4. each node rectangle lies inside its own subgraph's rectangle
    for (group, members) in map {
        let g = rect(group);
        for member in members {
            let (_, r) = nodes
                .iter()
                .find(|(id, _)| id == member)
                .unwrap_or_else(|| panic!("no node {member}"));
            if !g.holds(r) {
                out.push(format!("node {member} {r} is outside {group} {g}"));
            }
        }
    }
    out
}

const FOUR_PANELS: [(&str, [&str; 3]); 4] = [
    ("P1", ["p1a", "p1b", "p1c"]),
    ("P2", ["p2a", "p2b", "p2r"]),
    ("P3", ["p3a", "p3b", "p3r"]),
    ("P4", ["p4a", "p4b", "p4r"]),
];

const FOUR_CASES: [(&str, [&str; 3]); 4] = [
    ("C1", ["c1l", "c1h", "c1o"]),
    ("C2", ["c2h", "c2r", "c2l"]),
    ("C3", ["c3l", "c3h", "c3r"]),
    ("C4", ["c4l", "c4r", "c4h"]),
];

/// rdb1 (D-438Y2): the four-panel and four-case drawings keep their subgraphs apart, inside the
/// drawing, with their titles and nodes in their own subgraphs. The four-case drawing is its
/// golden's exact bytes.
#[test]
fn rdb1_separate_subgraphs_do_not_overlap() {
    let mut failures = Vec::new();
    let mut drawn = Vec::new();
    for (name, map) in [
        ("four-panels", &FOUR_PANELS[..]),
        ("four-cases", &FOUR_CASES[..]),
    ] {
        let src = String::from_utf8(read(&format!("{name}.mmd"))).expect("UTF-8 source");
        let Diagram::Svg(svg) = render(&src) else {
            panic!("{name}: not drawn: {:?}", render(&src));
        };
        failures.extend(
            violations(&svg, map)
                .into_iter()
                .map(|v| format!("{name}: {v}")),
        );
        drawn.push((name, svg));
    }
    assert!(failures.is_empty(), "{failures:#?}");
    let (_, cases) = &drawn[1];
    assert!(
        cases.as_bytes() == read("four-cases.svg"),
        "four-cases differs from its golden"
    );
}

/// The geometry pin adds the root translate to every x and y it reads, and touching edges are no
/// overlap.
#[test]
fn rdb1_the_pin_reads_the_root_translate() {
    let svg = "<svg viewBox=\"0 0 100.00 50.00\"><g transform=\"translate(10.00,-5.00)\"><g class=\"clusters\"><g class=\"cluster\" id=\"A\" data-id=\"A\"><rect x=\"-10.00\" y=\"5.00\" width=\"40.00\" height=\"20.00\" data-svg-role=\"graph-subgraph-rect\" /><text x=\"0.00\" y=\"10.00\" data-svg-role=\"graph-subgraph-text\">A</text></g><g class=\"cluster\" id=\"B\" data-id=\"B\"><rect x=\"30.00\" y=\"5.00\" width=\"40.00\" height=\"20.00\" data-svg-role=\"graph-subgraph-rect\" /><text x=\"40.00\" y=\"10.00\" data-svg-role=\"graph-subgraph-text\">B</text></g></g><g class=\"nodes\"><g class=\"node default\" data-id=\"a\"><rect x=\"-5.00\" y=\"10.00\" width=\"10.00\" height=\"10.00\" data-svg-role=\"graph-node-shape\" /></g></g></g></svg>";
    // A spans 0 to 40 and B 40 to 80 once translated: they touch, and nothing breaks
    assert_eq!(violations(svg, &[]), Vec::<String>::new(), "touching edges");
    let drawing = layout::read(svg).unwrap();
    assert_eq!(drawing.size, (10000, 5000));
    assert_eq!(drawing.subgraphs[0].rect.x, 0);
    assert_eq!(drawing.subgraphs[0].title, Some((1000, 500)));
    assert_eq!(layout::nodes(svg, &drawing).unwrap()[0].1.x, 500);
}
