//! rdb1 (D-438Y2): top-level subgraphs that each set their own direction are drawn apart. The
//! geometry pin reads the drawn SVG's rectangles and title anchors as integer hundredths and
//! compares integers only. Std only.

use rdy_diagram::{Diagram, render};
use std::path::PathBuf;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/diagram")
}

fn read(name: &str) -> Vec<u8> {
    let path = fixtures().join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// A number as an integer count of hundredths: two decimals, an optional minus.
fn hundredths(text: &str) -> i64 {
    let fail = || panic!("not a number with two decimals: {text:?}");
    let (neg, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let Some((whole, frac)) = digits.split_once('.') else {
        fail()
    };
    let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !all_digits(whole) || frac.len() != 2 || !all_digits(frac) || whole.len() > 12 {
        fail()
    }
    let n: i64 = whole.parse::<i64>().unwrap() * 100 + frac.parse::<i64>().unwrap();
    if neg { -n } else { n }
}

/// Hundredths as the SVG writes them.
fn show(n: i64) -> String {
    let sign = if n < 0 { "-" } else { "" };
    format!("{sign}{}.{:02}", n.abs() / 100, n.abs() % 100)
}

/// The tag that starts at `at`: from `<` through its `>`.
fn tag_at(svg: &str, at: usize) -> &str {
    let end = svg[at..]
        .find('>')
        .unwrap_or_else(|| panic!("an unclosed tag at {at}"));
    &svg[at..=at + end]
}

/// The value of attribute `name` in `tag`.
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let key = format!(" {name}=\"");
    let start = tag.find(&key)? + key.len();
    let end = tag[start..].find('"')? + start;
    Some(&tag[start..end])
}

fn number(tag: &str, name: &str) -> i64 {
    hundredths(attr(tag, name).unwrap_or_else(|| panic!("no {name}: {tag}")))
}

/// The first `<element` tag at or after `from` whose `data-svg-role` is `role`.
fn first_with_role<'a>(svg: &'a str, from: usize, element: &str, role: &str) -> &'a str {
    let open = format!("<{element} ");
    let mut at = from;
    while let Some(i) = svg[at..].find(&open) {
        let tag = tag_at(svg, at + i);
        if attr(tag, "data-svg-role") == Some(role) {
            return tag;
        }
        at += i + open.len();
    }
    panic!("no {element} with role {role} after {from}")
}

#[derive(Clone, Copy, Debug)]
struct Rect {
    x: i64,
    y: i64,
    w: i64,
    h: i64,
}

impl Rect {
    fn of(tag: &str, dx: i64, dy: i64) -> Rect {
        Rect {
            x: number(tag, "x") + dx,
            y: number(tag, "y") + dy,
            w: number(tag, "width"),
            h: number(tag, "height"),
        }
    }

    /// Whether `other` lies inside this one; its edges may touch.
    fn holds(&self, other: &Rect) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.x + other.w <= self.x + self.w
            && other.y + other.h <= self.y + self.h
    }

    /// Whether the interiors intersect; touching edges do not.
    fn overlaps(&self, other: &Rect) -> bool {
        self.x < other.x + other.w
            && other.x < self.x + self.w
            && self.y < other.y + other.h
            && other.y < self.y + self.h
    }

    /// Whether the point lies inside; a point on an edge is inside.
    fn has(&self, (x, y): (i64, i64)) -> bool {
        x >= self.x && x <= self.x + self.w && y >= self.y && y <= self.y + self.h
    }
}

impl std::fmt::Display for Rect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "(x {}, y {}, width {}, height {})",
            show(self.x),
            show(self.y),
            show(self.w),
            show(self.h)
        )
    }
}

struct Subgraph {
    id: String,
    rect: Rect,
    title: (i64, i64),
}

/// A drawing's viewBox size, its subgraphs and its nodes, in the root translate's frame.
struct Drawing {
    size: (i64, i64),
    subgraphs: Vec<Subgraph>,
    nodes: Vec<(String, Rect)>,
}

fn parse(svg: &str) -> Drawing {
    let root = tag_at(svg, svg.find("<svg").expect("an svg element"));
    let view = attr(root, "viewBox").unwrap_or_else(|| panic!("no viewBox: {root}"));
    let parts: Vec<&str> = view.split(' ').collect();
    assert!(
        parts.len() == 4 && parts[0] == "0" && parts[1] == "0",
        "viewBox is not \"0 0 W H\": {view:?}"
    );
    let size = (hundredths(parts[2]), hundredths(parts[3]));
    // one root translate, which opens before the clusters
    assert_eq!(
        svg.matches("transform=").count(),
        1,
        "the SVG holds exactly one transform"
    );
    let at = svg.find("transform=").unwrap();
    let g = svg[..at].rfind('<').unwrap();
    let tag = tag_at(svg, g);
    let translate = attr(tag, "transform").unwrap();
    let (dx, dy) = translate
        .strip_prefix("translate(")
        .and_then(|t| t.strip_suffix(')'))
        .and_then(|t| t.split_once(','))
        .filter(|_| tag.starts_with("<g transform="))
        .unwrap_or_else(|| panic!("not a root translate: {tag}"));
    let (dx, dy) = (hundredths(dx), hundredths(dy));
    let clusters = svg
        .find("<g class=\"clusters\">")
        .unwrap_or_else(|| panic!("no clusters group"));
    assert!(g < clusters, "the translate opens after the clusters");
    let mut subgraphs = Vec::new();
    let mut from = 0;
    while let Some(i) = svg[from..].find("<g class=\"cluster\" id=\"") {
        let at = from + i;
        let tag = tag_at(svg, at);
        let id = attr(tag, "id").unwrap();
        assert_eq!(attr(tag, "data-id"), Some(id), "{tag}");
        let rect = Rect::of(
            first_with_role(svg, at, "rect", "graph-subgraph-rect"),
            dx,
            dy,
        );
        let text = first_with_role(svg, at, "text", "graph-subgraph-text");
        let title = (number(text, "x") + dx, number(text, "y") + dy);
        subgraphs.push(Subgraph {
            id: id.to_string(),
            rect,
            title,
        });
        from = at + tag.len();
    }
    let mut nodes = Vec::new();
    let mut from = 0;
    while let Some(i) = svg[from..].find("<g class=\"node") {
        let at = from + i;
        let tag = tag_at(svg, at);
        if let Some(id) = attr(tag, "data-id") {
            let rect = Rect::of(first_with_role(svg, at, "rect", "graph-node-shape"), dx, dy);
            nodes.push((id.to_string(), rect));
        }
        from = at + tag.len();
    }
    Drawing {
        size,
        subgraphs,
        nodes,
    }
}

/// Every way the drawing breaks the four assertions, by name and numbers.
fn violations(svg: &str, map: &[(&str, [&str; 3])]) -> Vec<String> {
    let Drawing {
        size: (w, h),
        subgraphs,
        nodes,
    } = parse(svg);
    let mut out = Vec::new();
    for (i, a) in subgraphs.iter().enumerate() {
        for b in &subgraphs[i + 1..] {
            // 1. no two rectangles, neither inside the other, have intersecting interiors
            if !a.rect.holds(&b.rect) && !b.rect.holds(&a.rect) && a.rect.overlaps(&b.rect) {
                out.push(format!(
                    "{} and {} overlap: {} {} and {} {}",
                    a.id, b.id, a.id, a.rect, b.id, b.rect
                ));
            }
        }
        // 2. every rectangle lies inside the viewBox
        let r = a.rect;
        if r.x < 0 || r.y < 0 || r.x + r.w > w || r.y + r.h > h {
            out.push(format!(
                "{} is outside the viewBox (width {}, height {}): {}",
                a.id,
                show(w),
                show(h),
                r
            ));
        }
        // 3. each title anchor lies inside its own rectangle, and inside no other rectangle
        // that does not hold its own
        if !a.rect.has(a.title) {
            out.push(format!(
                "{}'s title ({}, {}) is outside its rectangle {}",
                a.id,
                show(a.title.0),
                show(a.title.1),
                a.rect
            ));
        }
        for b in &subgraphs {
            if b.id != a.id && !b.rect.holds(&a.rect) && b.rect.has(a.title) {
                out.push(format!(
                    "{}'s title ({}, {}) is inside {} {}",
                    a.id,
                    show(a.title.0),
                    show(a.title.1),
                    b.id,
                    b.rect
                ));
            }
        }
    }
    // 4. each node rectangle lies inside its own subgraph's rectangle
    for (group, members) in map {
        let g = subgraphs
            .iter()
            .find(|s| s.id == *group)
            .unwrap_or_else(|| panic!("no subgraph {group}"));
        for member in members {
            let (_, r) = nodes
                .iter()
                .find(|(id, _)| id == member)
                .unwrap_or_else(|| panic!("no node {member}"));
            if !g.rect.holds(r) {
                out.push(format!("node {member} {r} is outside {group} {}", g.rect));
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
    let Drawing {
        size,
        subgraphs,
        nodes,
    } = parse(svg);
    assert_eq!(size, (10000, 5000));
    assert_eq!(subgraphs[0].rect.x, 0);
    assert_eq!(subgraphs[0].title, (1000, 500));
    assert_eq!(nodes[0].1.x, 500);
}
