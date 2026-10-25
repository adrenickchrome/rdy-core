//! The arrow-cost measurement: a one-off tool, not an rdy command or library API.
//!
//! It draws generated Mermaid sources with the pinned drawing crate and times each draw. A
//! source's cost is the sum over its expanded arrows of the arrow's rank span times its label
//! weight W (1 for an unlabelled arrow). A closed protocol picks the largest arrow count N at
//! which every source in a closed matrix draws in under two seconds.
//!
//! ```text
//! arrow_bound scan                  run the whole protocol and print the report
//! arrow_bound verify <N> <weight>   run the verify step once at N and W
//! ```
//!
//! The report goes to stdout, one tab-separated row per draw. Exit 0: the scan selected N of at
//! least 200, or the verify step passed. Exit 3: the selected N is under 200. Exit 1: no
//! positive N passed, the 12-hour limit ended the run, or the verify step failed. Exit 2: bad
//! arguments.
//!
//! The example calls the pinned crate directly, so the normal render path's bounds cannot
//! bypass it. The pinned crate reads six environment variables at run time (`MMDFLUX_*`); start
//! this tool without them.
//!
//! The clock is injected, so the protocol's tests run on synthetic timings.

use std::collections::HashMap;
use std::io::{self, Write};
use std::panic::{self, AssertUnwindSafe};
use std::process::ExitCode;
use std::time::Instant;

use mmdflux::{OutputFormat, RenderConfig, SvgThemeConfig, SvgThemeMode};
use rdy_diagram::{Diagram, MAX_DEPTH, MAX_SOURCE_BYTES, MAX_STATEMENTS, MAX_SVG_BYTES};

/// The ladder of budgets, in the order a case draws them.
const LADDER: [u32; 16] = [
    16, 32, 64, 128, 192, 256, 384, 512, 768, 1024, 1536, 2048, 3072, 4096, 6144, 8192,
];
/// The top of the ladder: the candidate when no case is unsafe on it.
const TOP: u32 = 8192;
/// The repetitions of one draw.
const REPS: usize = 3;
/// A repetition at or over this many nanoseconds is unsafe.
const SAFE_NS: u64 = 2_000_000_000;
/// The run stops at the next draw boundary once this many nanoseconds have passed.
const TWELVE_HOURS_NS: u64 = 12 * 3600 * 1_000_000_000;
/// The largest argument value.
const MAX_ARG: u32 = 65536;
/// A selected N under this stops the run with exit 3.
const MIN_N: u32 = 200;
/// The cost of the 15 links that join the stacked subgraphs.
const LINKS: u64 = 15;
/// The number of top-level subgraphs in the stacked shapes.
const STACKED: usize = 16;
/// The arrows of the source that finds out whether the render path holds the arrow bound. It is far
/// over the top of the ladder.
const LANDING_ARROWS: usize = 9000;
/// The start of the reason the render path gives once it holds the arrow bound.
const REASON_HEAD: &str = "the source has over ";
/// The end of that reason.
const REASON_TAIL: &str = " arrows, counting a longer or labelled arrow as more than one";

const USAGE: &str = "usage: arrow_bound scan | verify <N> <weight>";
const HEADER: &str = "phase\tshape\tstyle\tdirection\tpadding\tarrows\tlabelled\tspan\tweight\tbudget\tcost\tbytes\tstatements\tdepth\trepeat\telapsed_ns\tresult\tsvg_bytes";
const UNDER_200: &str = "the arrow bound is under 200";
const NO_POSITIVE: &str = "no positive arrow bound passed the measurement";
const TIMED_OUT: &str = "the arrow bound measurement did not finish in 12 hours";

// ---------------------------------------------------------------------------------------------
// The matrix
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Dir {
    Lr,
    Td,
    /// The families have no direction.
    Plain,
}

impl Dir {
    fn name(self) -> &'static str {
        match self {
            Dir::Lr => "LR",
            Dir::Td => "TD",
            Dir::Plain => "-",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Pad {
    None,
    Comments,
    Duplicates,
    Both,
}

impl Pad {
    const ALL: [Pad; 4] = [Pad::None, Pad::Comments, Pad::Duplicates, Pad::Both];

    fn name(self) -> &'static str {
        match self {
            Pad::None => "none",
            Pad::Comments => "comments",
            Pad::Duplicates => "duplicates",
            Pad::Both => "both",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Fam {
    Flow,
    Class,
    State,
    Seq,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Shape {
    OneLine,
    PerLine,
    Pairs,
    FanOut,
    FanIn,
    Nest16,
    Nest32,
    Stack,
    StackLinked,
    GroupSquare,
    GroupWide,
    ClassPairs,
    ClassChain,
    StatePairs,
    StateChain,
    StateBlocksPairs,
    StateBlocksChain,
    SeqPairs,
    SeqChain,
}

impl Shape {
    const FLOW: [Shape; 9] = [
        Shape::OneLine,
        Shape::PerLine,
        Shape::Pairs,
        Shape::FanOut,
        Shape::FanIn,
        Shape::Nest16,
        Shape::Nest32,
        Shape::Stack,
        Shape::StackLinked,
    ];
    const GROUPS: [Shape; 2] = [Shape::GroupSquare, Shape::GroupWide];
    const FAMILIES: [Shape; 8] = [
        Shape::ClassPairs,
        Shape::ClassChain,
        Shape::StatePairs,
        Shape::StateChain,
        Shape::StateBlocksPairs,
        Shape::StateBlocksChain,
        Shape::SeqPairs,
        Shape::SeqChain,
    ];

    fn family(self) -> Fam {
        match self {
            Shape::ClassPairs | Shape::ClassChain => Fam::Class,
            Shape::StatePairs
            | Shape::StateChain
            | Shape::StateBlocksPairs
            | Shape::StateBlocksChain => Fam::State,
            Shape::SeqPairs | Shape::SeqChain => Fam::Seq,
            _ => Fam::Flow,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Shape::OneLine => "chain-one-line",
            Shape::PerLine => "chain-per-line",
            Shape::Pairs => "pairs",
            Shape::FanOut => "fan-out",
            Shape::FanIn => "fan-in",
            Shape::Nest16 => "nest-16",
            Shape::Nest32 => "nest-32",
            Shape::Stack => "stack-16",
            Shape::StackLinked => "stack-16-linked",
            Shape::GroupSquare => "group-square",
            Shape::GroupWide => "group-wide",
            Shape::ClassPairs => "class-pairs",
            Shape::ClassChain => "class-chain",
            Shape::StatePairs => "state-pairs",
            Shape::StateChain => "state-chain",
            Shape::StateBlocksPairs => "state-blocks-pairs",
            Shape::StateBlocksChain => "state-blocks-chain",
            Shape::SeqPairs => "sequence-pairs",
            Shape::SeqChain => "sequence-chain",
        }
    }

    /// Whether the arrows of a level chain (each starts where the one before ended).
    fn chains(self) -> bool {
        matches!(
            self,
            Shape::PerLine
                | Shape::Nest16
                | Shape::Nest32
                | Shape::Stack
                | Shape::StackLinked
                | Shape::ClassChain
                | Shape::StateChain
                | Shape::StateBlocksChain
                | Shape::SeqChain
        )
    }
}

/// One arrow form. The flowchart forms are the connectors of the matrix; the two family forms
/// are the plain and the labelled arrow of class, state and sequence diagrams.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Form {
    Arrow,
    Arrow3,
    Dotted,
    Thick,
    Hidden,
    ArrowLong,
    DottedLong,
    ThickLong,
    LabelArrow,
    LabelDotted,
    LabelThick,
    PipeArrow,
    PipeArrow3,
    LabelArrowPad,
    LabelDottedPad,
    LabelThickPad,
    PipeArrowPad,
    FamPlain,
    FamLabelled,
}

/// A label `y` followed by `x` so that the connector has 256 bytes, when `fixed` bytes of the
/// connector are not label.
fn padded_label(fixed: usize) -> String {
    format!("y{}", "x".repeat(256 - fixed - 1))
}

impl Form {
    const UNLABELLED: [Form; 8] = [
        Form::Arrow,
        Form::Arrow3,
        Form::Dotted,
        Form::Thick,
        Form::Hidden,
        Form::ArrowLong,
        Form::DottedLong,
        Form::ThickLong,
    ];
    const LABELLED: [Form; 9] = [
        Form::LabelArrow,
        Form::LabelDotted,
        Form::LabelThick,
        Form::PipeArrow,
        Form::PipeArrow3,
        Form::LabelArrowPad,
        Form::LabelDottedPad,
        Form::LabelThickPad,
        Form::PipeArrowPad,
    ];

    fn labelled(self) -> bool {
        matches!(
            self,
            Form::LabelArrow
                | Form::LabelDotted
                | Form::LabelThick
                | Form::PipeArrow
                | Form::PipeArrow3
                | Form::LabelArrowPad
                | Form::LabelDottedPad
                | Form::LabelThickPad
                | Form::PipeArrowPad
                | Form::FamLabelled
        )
    }

    /// The rank span of one arrow of this form, as the pinned crate computes it.
    fn span(self) -> u64 {
        match self {
            Form::Arrow3 | Form::PipeArrow3 => 2,
            Form::ArrowLong | Form::ThickLong => 254,
            Form::DottedLong => 253,
            _ => 1,
        }
    }

    /// The cost of one arrow of this form: its span, times the weight when it has a label.
    fn cost(self, weight: u64) -> u64 {
        self.span()
            .saturating_mul(if self.labelled() { weight } else { 1 })
    }

    /// The connector as written between two nodes.
    fn text(self) -> String {
        match self {
            Form::Arrow | Form::FamPlain | Form::FamLabelled => "-->".to_string(),
            Form::Arrow3 => "--->".to_string(),
            Form::Dotted => "-.->".to_string(),
            Form::Thick => "==>".to_string(),
            Form::Hidden => "~~~".to_string(),
            Form::ArrowLong => format!("{}>", "-".repeat(255)),
            Form::DottedLong => format!("-{}->", ".".repeat(253)),
            Form::ThickLong => format!("{}>", "=".repeat(255)),
            Form::LabelArrow => "-- y -->".to_string(),
            Form::LabelDotted => "-. y .->".to_string(),
            Form::LabelThick => "== y ==>".to_string(),
            Form::PipeArrow => "-->|y|".to_string(),
            Form::PipeArrow3 => "--->|y|".to_string(),
            Form::LabelArrowPad => format!("-- {} -->", padded_label(7)),
            Form::LabelDottedPad => format!("-. {} .->", padded_label(7)),
            Form::LabelThickPad => format!("== {} ==>", padded_label(7)),
            Form::PipeArrowPad => format!("-->|{}|", padded_label(5)),
        }
    }

    /// The connector's extent in bytes, as the pinned crate's arrow bound reads it.
    fn connector(self) -> u64 {
        self.text().len() as u64
    }

    fn name(self) -> &'static str {
        match self {
            Form::Arrow => "-->",
            Form::Arrow3 => "--->",
            Form::Dotted => "-.->",
            Form::Thick => "==>",
            Form::Hidden => "~~~",
            Form::ArrowLong => "-->/256",
            Form::DottedLong => "-.->/256",
            Form::ThickLong => "==>/256",
            Form::LabelArrow => "-- y -->",
            Form::LabelDotted => "-. y .->",
            Form::LabelThick => "== y ==>",
            Form::PipeArrow => "-->|y|",
            Form::PipeArrow3 => "--->|y|",
            Form::LabelArrowPad => "-- y -->/256",
            Form::LabelDottedPad => "-. y .->/256",
            Form::LabelThickPad => "== y ==>/256",
            Form::PipeArrowPad => "-->|y|/256",
            Form::FamPlain => "arrow",
            Form::FamLabelled => "arrow-labelled",
        }
    }
}

/// The part of a mixed budget that goes to labelled arrows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Share {
    Zero,
    Half,
    All,
}

impl Share {
    fn name(self) -> &'static str {
        match self {
            Share::Zero => "0",
            Share::Half => "1/2",
            Share::All => "1",
        }
    }
}

/// Where the labelled arrows of a mixed source stand.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Order {
    First,
    Last,
    Alternating,
}

impl Order {
    fn name(self) -> &'static str {
        match self {
            Order::First => "labels-first",
            Order::Last => "labels-last",
            Order::Alternating => "labels-alternating",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Style {
    One(Form),
    Mixed {
        plain: Form,
        labelled: Form,
        share: Share,
        order: Order,
    },
}

impl Style {
    fn name(self) -> String {
        match self {
            Style::One(form) => form.name().to_string(),
            Style::Mixed {
                plain,
                labelled,
                share,
                order,
            } => format!(
                "{}+{} share={} {}",
                plain.name(),
                labelled.name(),
                share.name(),
                order.name()
            ),
        }
    }
}

/// The class of a case, which decides when the protocol draws it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Unlabelled,
    Labelled,
    Mixed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Case {
    shape: Shape,
    style: Style,
    dir: Dir,
    pad: Pad,
}

impl Case {
    fn class(&self) -> Class {
        match self.style {
            Style::One(form) if form.labelled() => Class::Labelled,
            Style::One(_) => Class::Unlabelled,
            Style::Mixed { .. } => Class::Mixed,
        }
    }

    /// Whether the weight changes this case's source. An unlabelled case draws the same at every
    /// weight.
    fn weighted(&self) -> bool {
        self.class() != Class::Unlabelled
    }
}

/// Every case of the closed matrix, in a fixed order.
fn enumerate() -> Vec<Case> {
    let mut cases = Vec::new();
    let mixed_pairs = [
        (Form::Arrow, Form::LabelArrow),
        (Form::ArrowLong, Form::LabelArrowPad),
    ];
    for shape in Shape::FLOW {
        for dir in [Dir::Lr, Dir::Td] {
            for pad in Pad::ALL {
                let mut push = |style| {
                    cases.push(Case {
                        shape,
                        style,
                        dir,
                        pad,
                    })
                };
                for form in Form::UNLABELLED.into_iter().chain(Form::LABELLED) {
                    push(Style::One(form));
                }
                for (plain, labelled) in mixed_pairs {
                    for share in [Share::Zero, Share::Half, Share::All] {
                        for order in [Order::First, Order::Last, Order::Alternating] {
                            push(Style::Mixed {
                                plain,
                                labelled,
                                share,
                                order,
                            });
                        }
                    }
                }
            }
        }
    }
    for shape in Shape::GROUPS {
        for dir in [Dir::Lr, Dir::Td] {
            for pad in Pad::ALL {
                for form in [Form::Arrow, Form::PipeArrow] {
                    cases.push(Case {
                        shape,
                        style: Style::One(form),
                        dir,
                        pad,
                    });
                }
            }
        }
    }
    for shape in Shape::FAMILIES {
        for pad in Pad::ALL {
            for form in [Form::FamPlain, Form::FamLabelled] {
                cases.push(Case {
                    shape,
                    style: Style::One(form),
                    dir: Dir::Plain,
                    pad,
                });
            }
        }
    }
    cases
}

// ---------------------------------------------------------------------------------------------
// Generating sources
// ---------------------------------------------------------------------------------------------

/// What the generator records about a source before it is drawn.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Meta {
    /// The expanded arrows.
    arrows: u64,
    /// The expanded arrows that have a label.
    labelled: u64,
    /// The largest rank span of any arrow.
    span: u64,
    /// The longest connector, in bytes.
    connector: u64,
    cost: u64,
    bytes: u64,
    statements: u64,
    depth: u64,
}

struct Source {
    text: String,
    meta: Meta,
}

/// A source being built, with its counters.
struct Builder {
    text: String,
    weight: u64,
    statements: u64,
    nesting: u64,
    meta: Meta,
}

impl Builder {
    fn new(weight: u64) -> Builder {
        Builder {
            text: String::new(),
            weight,
            statements: 0,
            nesting: 0,
            meta: Meta::default(),
        }
    }

    /// Adds one statement line.
    fn line(&mut self, text: &str) {
        self.text.push_str(text);
        self.text.push('\n');
        self.statements += 1;
    }

    fn open(&mut self, text: &str) {
        self.line(text);
        self.nesting += 1;
        self.meta.depth = self.meta.depth.max(self.nesting);
    }

    fn close(&mut self, text: &str) {
        self.line(text);
        self.nesting -= 1;
    }

    /// Records `n` expanded arrows of one form.
    fn count(&mut self, form: Form, n: u64) {
        let m = &mut self.meta;
        m.arrows = m.arrows.saturating_add(n);
        if form.labelled() {
            m.labelled = m.labelled.saturating_add(n);
        }
        m.cost = m
            .cost
            .saturating_add(n.saturating_mul(form.cost(self.weight)));
        m.span = m.span.max(form.span());
        m.connector = m.connector.max(form.connector());
    }
}

fn node(i: u64) -> String {
    format!("n{i}")
}

/// A bare declaration of a node, which does not add an arrow.
fn declaration(fam: Fam, id: &str) -> String {
    match fam {
        Fam::Flow | Fam::State => id.to_string(),
        Fam::Class => format!("class {id}"),
        Fam::Seq => format!("participant {id}"),
    }
}

/// The plain arrow of a family: what a budget's remainder is made of.
fn plain_form(fam: Fam) -> Form {
    match fam {
        Fam::Flow => Form::Arrow,
        _ => Form::FamPlain,
    }
}

fn header(fam: Fam, dir: Dir) -> String {
    match fam {
        Fam::Flow => format!("flowchart {}", dir.name()),
        Fam::Class => "classDiagram".to_string(),
        Fam::State => "stateDiagram-v2".to_string(),
        Fam::Seq => "sequenceDiagram".to_string(),
    }
}

/// Adds the line of one arrow between two nodes.
fn arrow_line(b: &mut Builder, fam: Fam, form: Form, from: &str, to: &str) {
    let labelled = form.labelled();
    let line = match (fam, labelled) {
        (Fam::Flow, _) => format!("{from} {} {to}", form.text()),
        (Fam::Class, true) => format!("{from} \"1\" --> \"*\" {to} : y"),
        (Fam::Class | Fam::State, false) => format!("{from} --> {to}"),
        (Fam::State, true) => format!("{from} --> {to} : y"),
        (Fam::Seq, true) => format!("{from}->>{to}: y"),
        (Fam::Seq, false) => format!("{from}->>{to}"),
    };
    b.line(&line);
    b.count(form, 1);
}

/// The level sizes when `total` arrows spread over `k` levels: each gets floor(total / k), and
/// the first (total mod k) get one more.
fn spread(total: usize, k: usize) -> Vec<usize> {
    (0..k)
        .map(|j| total / k + usize::from(j < total % k))
        .collect()
}

/// The lines of one level: its arrows as a chain or as independent pairs, or one node when it
/// has no arrow. `next` is the next free node number.
fn level(b: &mut Builder, shape: Shape, forms: &[Form], next: &mut u64) {
    let fam = shape.family();
    if forms.is_empty() {
        b.line(&declaration(fam, &node(*next)));
        *next += 1;
        return;
    }
    let len = forms.len() as u64;
    for (i, form) in forms.iter().enumerate() {
        let i = i as u64;
        let (from, to) = if shape.chains() {
            (*next + i, *next + i + 1)
        } else {
            (*next + 2 * i, *next + 2 * i + 1)
        };
        arrow_line(b, fam, *form, &node(from), &node(to));
    }
    *next += if shape.chains() { len + 1 } else { 2 * len };
}

/// Nested levels, each with its own nodes, all closed.
fn nested(b: &mut Builder, shape: Shape, forms: &[Form], k: usize) {
    let state = shape.family() == Fam::State;
    let mut next = 0;
    let mut at = 0;
    for (j, size) in spread(forms.len(), k).into_iter().enumerate() {
        if state {
            b.open(&format!("state S{j} {{"));
        } else {
            b.open(&format!("subgraph S{j}"));
        }
        level(b, shape, &forms[at..at + size], &mut next);
        at += size;
    }
    for _ in 0..k {
        b.close(if state { "}" } else { "end" });
    }
}

/// Stacked top-level subgraphs, each with a direction of its own.
fn stacked(b: &mut Builder, shape: Shape, forms: &[Form], linked: bool) {
    let mut next = 0;
    let mut at = 0;
    for (j, size) in spread(forms.len(), STACKED).into_iter().enumerate() {
        b.open(&format!("subgraph S{j}"));
        b.line("direction LR");
        level(b, shape, &forms[at..at + size], &mut next);
        at += size;
        b.close("end");
    }
    if linked {
        for j in 0..STACKED - 1 {
            b.line(&format!("S{j} ~~~ S{}", j + 1));
            b.count(Form::Hidden, 1);
        }
    }
}

/// The source of every shape but the node groups, for the arrows `forms` in order.
fn compose(shape: Shape, dir: Dir, forms: &[Form], weight: u64) -> Builder {
    let fam = shape.family();
    let mut b = Builder::new(weight);
    b.line(&header(fam, dir));
    match shape {
        Shape::OneLine => {
            let mut line = node(0);
            for (i, form) in forms.iter().enumerate() {
                line.push_str(&format!(" {} {}", form.text(), node(i as u64 + 1)));
                b.count(*form, 1);
            }
            b.line(&line);
        }
        Shape::FanOut | Shape::FanIn => {
            if forms.is_empty() {
                b.line(&node(0));
            }
            for (i, form) in forms.iter().enumerate() {
                let (hub, leaf) = (node(0), node(i as u64 + 1));
                if shape == Shape::FanOut {
                    arrow_line(&mut b, fam, *form, &hub, &leaf);
                } else {
                    arrow_line(&mut b, fam, *form, &leaf, &hub);
                }
            }
        }
        Shape::Nest16 => nested(&mut b, shape, forms, 16),
        Shape::Nest32 | Shape::StateBlocksPairs | Shape::StateBlocksChain => {
            nested(&mut b, shape, forms, 32)
        }
        Shape::Stack => stacked(&mut b, shape, forms, false),
        Shape::StackLinked => stacked(&mut b, shape, forms, true),
        _ => level(&mut b, shape, forms, &mut 0),
    }
    b
}

/// The node-group shapes: one `a & b & … --> c & d & …` statement whose arrows expand to a
/// product, then plain arrows for the rest of the budget. Only the two sides' ids are built,
/// never the product.
fn group(case: &Case, budget: u64, weight: u64) -> Option<Builder> {
    let Style::One(form) = case.style else {
        return None;
    };
    let cost = form.cost(weight);
    let (p, q) = match case.shape {
        Shape::GroupSquare => {
            let side = (budget / cost).isqrt();
            (side, side)
        }
        _ => (2, budget / cost.saturating_mul(2)),
    };
    let (p, q) = if p == 0 || q == 0 { (0, 0) } else { (p, q) };
    // every id takes at least four bytes with its separator
    if p.saturating_add(q).saturating_mul(4) > MAX_SOURCE_BYTES as u64 {
        return None;
    }
    let product = p.saturating_mul(q);
    let rest = budget.saturating_sub(product.saturating_mul(cost));
    if rest > MAX_STATEMENTS as u64 {
        return None;
    }
    let mut b = Builder::new(weight);
    b.line(&header(Fam::Flow, case.dir));
    if p > 0 {
        let side = |name: &str, n: u64| {
            (0..n)
                .map(|i| format!("{name}{i}"))
                .collect::<Vec<_>>()
                .join(" & ")
        };
        b.line(&format!(
            "{} {} {}",
            side("n", p),
            form.text(),
            side("m", q)
        ));
        b.count(form, product);
    }
    for i in 0..rest {
        b.line(&format!("r{} --> r{}", 2 * i, 2 * i + 1));
        b.count(Form::Arrow, 1);
    }
    Some(b)
}

/// The arrows of a style at a budget, in source order, or none when they cannot fit the source
/// bound. A budget is filled with floor(budget / cost) arrows of the style, then plain arrows
/// for the rest. The size is checked before anything is allocated.
fn forms_for(style: Style, budget: u64, weight: u64, plain: Form) -> Option<Vec<Form>> {
    let bytes = |form: Form, n: u64| n.saturating_mul(form.connector() + 1);
    let limit = MAX_SOURCE_BYTES as u64;
    match style {
        Style::One(form) => {
            let cost = form.cost(weight);
            let (n, rest) = (budget / cost, budget % cost);
            if bytes(form, n).saturating_add(bytes(plain, rest)) > limit {
                return None;
            }
            let mut forms = vec![form; n as usize];
            forms.resize(forms.len() + rest as usize, plain);
            Some(forms)
        }
        Style::Mixed {
            plain: unlabelled,
            labelled,
            share,
            order,
        } => {
            let budget_labelled = match share {
                Share::Zero => 0,
                Share::Half => budget / 2,
                Share::All => budget,
            };
            let budget_plain = budget - budget_labelled;
            let (cost_l, cost_u) = (labelled.cost(weight), unlabelled.cost(weight));
            let (n_l, n_u) = (budget_labelled / cost_l, budget_plain / cost_u);
            let rest = budget_labelled % cost_l + budget_plain % cost_u;
            if bytes(labelled, n_l)
                .saturating_add(bytes(unlabelled, n_u))
                .saturating_add(bytes(plain, rest))
                > limit
            {
                return None;
            }
            let mut forms = Vec::with_capacity((n_l + n_u + rest) as usize);
            match order {
                Order::First => {
                    forms.resize(n_l as usize, labelled);
                    forms.resize((n_l + n_u) as usize, unlabelled);
                }
                Order::Last => {
                    forms.resize(n_u as usize, unlabelled);
                    forms.resize((n_l + n_u) as usize, labelled);
                }
                Order::Alternating => {
                    for i in 0..n_l.max(n_u) {
                        if i < n_l {
                            forms.push(labelled);
                        }
                        if i < n_u {
                            forms.push(unlabelled);
                        }
                    }
                }
            }
            forms.resize(forms.len() + rest as usize, plain);
            Some(forms)
        }
    }
}

/// Comment lines that bring `text` to exactly the source bound. Blank lines make up a last one
/// or two bytes.
fn pad_with_comments(text: &mut String) {
    let mut left = MAX_SOURCE_BYTES - text.len();
    while left > 0 {
        if left < 3 {
            text.push_str(&"\n".repeat(left));
            return;
        }
        let mut len = left.min(1024);
        if matches!(left - len, 1 | 2) {
            len = left - 3;
        }
        text.push_str("%%");
        text.push_str(&"x".repeat(len - 3));
        text.push('\n');
        left -= len;
    }
}

/// Applies the padding and checks the bounds the pinned crate checks first.
fn finish(mut b: Builder, pad: Pad, fam: Fam) -> Option<Source> {
    let within = |b: &Builder| {
        b.text.len() <= MAX_SOURCE_BYTES
            && b.statements <= MAX_STATEMENTS as u64
            && b.meta.depth <= MAX_DEPTH as u64
    };
    if !within(&b) {
        return None;
    }
    if matches!(pad, Pad::Duplicates | Pad::Both) {
        let duplicate = declaration(fam, &node(0));
        let missing = MAX_STATEMENTS as u64 - b.statements;
        let added = missing.saturating_mul(duplicate.len() as u64 + 1);
        if (b.text.len() as u64).saturating_add(added) > MAX_SOURCE_BYTES as u64 {
            return None;
        }
        for _ in 0..missing {
            b.line(&duplicate);
        }
    }
    if matches!(pad, Pad::Comments | Pad::Both) {
        pad_with_comments(&mut b.text);
    }
    b.meta.bytes = b.text.len() as u64;
    b.meta.statements = b.statements;
    Some(Source {
        text: b.text,
        meta: b.meta,
    })
}

/// The source of a case at a budget and a weight, or none when the case cannot reach the budget
/// inside the pinned bounds on bytes, statements and depth.
fn generate(case: &Case, budget: u64, weight: u64) -> Option<Source> {
    let fam = case.shape.family();
    let builder = match case.shape {
        Shape::GroupSquare | Shape::GroupWide => group(case, budget, weight)?,
        shape => {
            let inner = if shape == Shape::StackLinked {
                budget.checked_sub(LINKS)?
            } else {
                budget
            };
            let forms = forms_for(case.style, inner, weight, plain_form(fam))?;
            compose(shape, case.dir, &forms, weight)
        }
    };
    finish(builder, case.pad, fam)
}

/// The four diagnostic sources. They report only and do not move N.
fn diagnostic_source(which: usize, weight: u64) -> Option<(&'static str, Form, Source)> {
    let (name, form, builder) = match which {
        0 => {
            // a plain one-line chain that fills the source bound
            let mut b = Builder::new(weight);
            b.line("flowchart LR");
            let mut line = node(0);
            let mut n = 0;
            loop {
                let piece = format!(" --> {}", node(n + 1));
                if b.text.len() + line.len() + piece.len() + 1 > MAX_SOURCE_BYTES {
                    break;
                }
                line.push_str(&piece);
                b.count(Form::Arrow, 1);
                n += 1;
            }
            b.line(&line);
            ("diagnostic-chain-65k", Form::Arrow, b)
        }
        1 => (
            "diagnostic-pairs-2047",
            Form::PipeArrow,
            compose(
                Shape::Pairs,
                Dir::Lr,
                &vec![Form::PipeArrow; MAX_STATEMENTS - 1],
                weight,
            ),
        ),
        2 => (
            "diagnostic-nest-32x30",
            Form::Arrow,
            compose(Shape::Nest32, Dir::Lr, &[Form::Arrow; 32 * 30], weight),
        ),
        3 => (
            "diagnostic-nest-16x30",
            Form::Arrow,
            compose(Shape::Nest16, Dir::Lr, &[Form::Arrow; 16 * 30], weight),
        ),
        _ => return None,
    };
    finish(builder, Pad::None, Fam::Flow).map(|source| (name, form, source))
}

// ---------------------------------------------------------------------------------------------
// Drawing and timing
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Res {
    Svg,
    Invalid,
    Failed,
    Unsupported,
    Error,
    Panic,
    Inapplicable,
}

impl Res {
    fn name(self) -> &'static str {
        match self {
            Res::Svg => "svg",
            Res::Invalid => "invalid",
            Res::Failed => "failed",
            Res::Unsupported => "unsupported",
            Res::Error => "error",
            Res::Panic => "panic",
            Res::Inapplicable => "inapplicable",
        }
    }
}

/// What one draw returned. The SVG itself is dropped; only its length is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Attempt {
    result: Res,
    svg_bytes: u64,
}

/// One timed repetition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rep {
    ns: u64,
    attempt: Attempt,
}

/// Draws a source with the pinned crate, with the render path's dynamic SVG config
/// (`lib.rs` `render`). It checks no bound but the SVG length.
fn direct(source: &str) -> Attempt {
    let config = RenderConfig {
        svg_theme: Some(SvgThemeConfig {
            mode: SvgThemeMode::Dynamic,
            ..Default::default()
        }),
        ..Default::default()
    };
    let drawn = panic::catch_unwind(AssertUnwindSafe(|| {
        mmdflux::render_diagram(source, OutputFormat::Svg, &config)
    }));
    match drawn {
        Err(_) => Attempt {
            result: Res::Panic,
            svg_bytes: 0,
        },
        Ok(Ok(svg)) => {
            let svg_bytes = svg.len() as u64;
            drop(svg);
            let result = if svg_bytes > MAX_SVG_BYTES as u64 {
                Res::Failed
            } else {
                Res::Svg
            };
            Attempt { result, svg_bytes }
        }
        Ok(Err(e)) if e.message.starts_with("parse error: ") => Attempt {
            result: Res::Invalid,
            svg_bytes: 0,
        },
        Ok(Err(_)) => Attempt {
            result: Res::Error,
            svg_bytes: 0,
        },
    }
}

/// Draws a source through the render path. The reason of a refusal comes with it.
fn wrapped(source: &str) -> (Attempt, Option<String>) {
    let (result, svg_bytes, reason) = match rdy_diagram::render(source) {
        Diagram::Svg(svg) => (Res::Svg, svg.len() as u64, None),
        Diagram::Invalid { reason } => (Res::Invalid, 0, Some(reason)),
        Diagram::Failed { reason } => (Res::Failed, 0, Some(reason)),
        Diagram::Unsupported { header } => (Res::Unsupported, 0, Some(header)),
    };
    (Attempt { result, svg_bytes }, reason)
}

/// A monotonic clock in nanoseconds.
trait Clock {
    fn now(&mut self) -> u64;
}

struct Wall(Instant);

impl Clock for Wall {
    fn now(&mut self) -> u64 {
        u64::try_from(self.0.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}

fn rep_safe(rep: &Rep) -> bool {
    rep.attempt.result == Res::Svg && rep.ns < SAFE_NS
}

/// Draws up to three times. The repetitions stop at the first one that is not safe: a result
/// that is not `svg`, or a time of 2 s or more.
fn run_reps(clock: &mut dyn Clock, draw: &mut dyn FnMut() -> Attempt) -> Vec<Rep> {
    let mut reps = Vec::with_capacity(REPS);
    for _ in 0..REPS {
        let start = clock.now();
        let attempt = draw();
        let ns = clock.now().saturating_sub(start);
        let rep = Rep { ns, attempt };
        reps.push(rep);
        if !rep_safe(&rep) {
            break;
        }
    }
    reps
}

// ---------------------------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Ladder,
    Refine,
    VerifyLibrary,
    VerifyWrapper,
    Diagnostic,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Phase::Ladder => "ladder",
            Phase::Refine => "refine",
            Phase::VerifyLibrary => "verify-library",
            Phase::VerifyWrapper => "verify-wrapper",
            Phase::Diagnostic => "diagnostic",
        }
    }
}

/// The text columns of a row.
#[derive(Clone, Debug)]
struct Labels {
    shape: String,
    style: String,
    direction: &'static str,
    padding: &'static str,
}

impl Labels {
    fn of(case: &Case) -> Labels {
        Labels {
            shape: case.shape.name().to_string(),
            style: case.style.name(),
            direction: case.dir.name(),
            padding: case.pad.name(),
        }
    }
}

#[derive(Clone, Debug)]
struct Row {
    phase: Phase,
    labels: Labels,
    meta: Meta,
    weight: u32,
    budget: u32,
    repeat: u32,
    elapsed_ns: u64,
    result: Res,
    svg_bytes: u64,
}

impl Row {
    fn line(&self) -> String {
        let m = &self.meta;
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            self.phase.name(),
            self.labels.shape,
            self.labels.style,
            self.labels.direction,
            self.labels.padding,
            m.arrows,
            m.labelled,
            m.span,
            self.weight,
            self.budget,
            m.cost,
            m.bytes,
            m.statements,
            m.depth,
            self.repeat,
            self.elapsed_ns,
            self.result.name(),
            self.svg_bytes
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    /// The case cannot reach the budget inside the bounds. It is never a safe draw.
    Inapplicable,
    Safe,
    Unsafe,
}

/// The rows of one draw, and what they mean.
struct Drawn {
    rows: Vec<Row>,
    verdict: Verdict,
    /// The result of the last repetition.
    last: Res,
    /// The reason the render path gave for the last repetition, if any.
    reason: Option<String>,
}

impl Drawn {
    fn inapplicable(labels: &Labels, phase: Phase, budget: u32, weight: u32) -> Drawn {
        let row = Row {
            phase,
            labels: labels.clone(),
            meta: Meta::default(),
            weight,
            budget,
            repeat: 0,
            elapsed_ns: 0,
            result: Res::Inapplicable,
            svg_bytes: 0,
        };
        Drawn {
            rows: vec![row],
            verdict: Verdict::Inapplicable,
            last: Res::Inapplicable,
            reason: None,
        }
    }

    fn timed(
        labels: &Labels,
        phase: Phase,
        meta: &Meta,
        (budget, weight): (u32, u32),
        reps: &[Rep],
        reason: Option<String>,
    ) -> Drawn {
        let rows = reps
            .iter()
            .enumerate()
            .map(|(i, rep)| Row {
                phase,
                labels: labels.clone(),
                meta: meta.clone(),
                weight,
                budget,
                repeat: i as u32 + 1,
                elapsed_ns: rep.ns,
                result: rep.attempt.result,
                svg_bytes: rep.attempt.svg_bytes,
            })
            .collect();
        let safe = reps.len() == REPS && reps.iter().all(rep_safe);
        Drawn {
            rows,
            verdict: if safe { Verdict::Safe } else { Verdict::Unsafe },
            last: reps.last().map_or(Res::Inapplicable, |r| r.attempt.result),
            reason,
        }
    }
}

/// The first two lines of a report: the plan and the column header.
fn report_head(cases: usize) -> String {
    let draws = cases as u64 * LADDER.len() as u64 * REPS as u64;
    format!("plan\t{cases}\t{draws}\t{}\n{HEADER}\n", draws * 2)
}

// ---------------------------------------------------------------------------------------------
// The protocol
// ---------------------------------------------------------------------------------------------

/// What the protocol asks of a run: draws, and the time since the run began.
trait Probe {
    /// Draws a case at a budget and weight: through the pinned crate, or in the verify-wrapper
    /// phase through the render path.
    fn draw(&mut self, case: &Case, budget: u32, weight: u32, phase: Phase) -> Drawn;
    /// Draws one of the four diagnostic sources.
    fn diagnostic(&mut self, which: usize, weight: u32) -> Drawn;
    /// Whether the render path refuses a source by its arrow cost.
    fn bound_landed(&mut self) -> bool;
    /// Nanoseconds since the run began.
    fn elapsed(&mut self) -> u64;
}

/// How a run ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Finish {
    /// N and W are selected, or verified. Exit 0.
    Selected(u32, u32),
    /// N is selected and is under 200. Exit 3.
    Under(u32, u32),
    /// Nothing positive passed. Exit 1.
    NoPositive,
    /// The 12 hours ran out. Exit 1.
    TimedOut,
    /// The verify step failed at N. Exit 1.
    NotVerified(u32),
}

impl Finish {
    fn code(self) -> u8 {
        match self {
            Finish::Selected(..) => 0,
            Finish::Under(..) => 3,
            _ => 1,
        }
    }

    fn message(self) -> Option<String> {
        match self {
            Finish::Selected(..) => None,
            Finish::Under(..) => Some(UNDER_200.to_string()),
            Finish::NoPositive => Some(NO_POSITIVE.to_string()),
            Finish::TimedOut => Some(TIMED_OUT.to_string()),
            Finish::NotVerified(n) => Some(format!("the arrow bound did not verify at {n}")),
        }
    }
}

enum Halt {
    TimedOut,
    Io(io::Error),
}

impl From<io::Error> for Halt {
    fn from(e: io::Error) -> Halt {
        Halt::Io(e)
    }
}

enum Verified {
    Passed(u32),
    NoPositive,
    Failed(u32),
}

/// The reason the render path gives once the arrow bound is in.
fn bound_reason(n: u32) -> String {
    format!("{REASON_HEAD}{n}{REASON_TAIL}")
}

/// Whether a draw through the render path shows that it holds the arrow bound: the result is
/// `invalid`, and the reason has the bound's two texts.
fn bound_landed(attempt: &Attempt, reason: Option<&str>) -> bool {
    attempt.result == Res::Invalid
        && reason.is_some_and(|r| r.starts_with(REASON_HEAD) && r.ends_with(REASON_TAIL))
}

/// The source that finds out whether the render path holds the arrow bound: one flowchart line of
/// plain `-->` arrows, far over the ladder top and inside the five existing bounds. The node ids
/// repeat after 676, so the chain stays inside the source-size bound.
fn landing_source() -> String {
    let id = |i: usize| -> String {
        let (hi, lo) = (i % 676 / 26, i % 26);
        format!(
            "x{}{}",
            char::from(b'a' + u8::try_from(hi).unwrap_or(0)),
            char::from(b'a' + u8::try_from(lo).unwrap_or(0))
        )
    };
    let mut text = String::from("flowchart LR\n");
    text.push_str(&id(0));
    for i in 1..=LANDING_ARROWS {
        text.push_str("-->");
        text.push_str(&id(i));
    }
    text.push('\n');
    text
}

struct Search<'a> {
    probe: &'a mut dyn Probe,
    cases: &'a [Case],
    out: &'a mut dyn Write,
    /// The verdicts of ladder and refine draws, so that a draw is never repeated.
    memo: HashMap<(usize, u32, u32), Verdict>,
}

impl<'a> Search<'a> {
    fn new(probe: &'a mut dyn Probe, cases: &'a [Case], out: &'a mut dyn Write) -> Search<'a> {
        Search {
            probe,
            cases,
            out,
            memo: HashMap::new(),
        }
    }

    fn emit(&mut self, drawn: &Drawn) -> Result<(), Halt> {
        for row in &drawn.rows {
            writeln!(self.out, "{}", row.line())?;
        }
        self.out.flush()?;
        Ok(())
    }

    /// Stops at a draw boundary once 12 hours have passed.
    fn boundary(&mut self) -> Result<(), Halt> {
        if self.probe.elapsed() >= TWELVE_HOURS_NS {
            return Err(Halt::TimedOut);
        }
        Ok(())
    }

    fn draw(&mut self, case: usize, budget: u32, weight: u32, phase: Phase) -> Result<Drawn, Halt> {
        self.boundary()?;
        let drawn = self.probe.draw(&self.cases[case], budget, weight, phase);
        self.emit(&drawn)?;
        Ok(drawn)
    }

    /// How a case draws at a budget in the ladder or the refine range. An unlabelled case draws
    /// the same at every weight, so its draw is shared.
    fn verdict(
        &mut self,
        case: usize,
        budget: u32,
        weight: u32,
        phase: Phase,
    ) -> Result<Verdict, Halt> {
        let shared = if self.cases[case].weighted() {
            weight
        } else {
            1
        };
        let key = (case, budget, shared);
        if let Some(verdict) = self.memo.get(&key) {
            return Ok(*verdict);
        }
        let verdict = self.draw(case, budget, weight, phase)?.verdict;
        self.memo.insert(key, verdict);
        Ok(verdict)
    }

    /// Steps 1 and 2 for the cases `set` at a weight: the ladder, then the refine range. The
    /// result is the candidate. A budget the case cannot be built at ends that case's ladder or
    /// refine. It sets no first unsafe budget and gives no candidate.
    fn candidate(&mut self, set: &[usize], weight: u32) -> Result<u32, Halt> {
        let mut firsts = Vec::new();
        for &case in set {
            for (rung, &budget) in LADDER.iter().enumerate() {
                match self.verdict(case, budget, weight, Phase::Ladder)? {
                    Verdict::Safe => {}
                    Verdict::Unsafe => {
                        firsts.push((case, rung));
                        break;
                    }
                    Verdict::Inapplicable => break,
                }
            }
        }
        let Some(first) = firsts.iter().map(|(_, rung)| *rung).min() else {
            return Ok(TOP);
        };
        let failed = LADDER[first];
        let before = if first == 0 { 0 } else { LADDER[first - 1] };
        let mut unsafe_at: Option<u32> = None;
        for &(case, rung) in &firsts {
            if rung != first {
                continue;
            }
            for budget in before + 1..failed {
                match self.verdict(case, budget, weight, Phase::Refine)? {
                    Verdict::Safe => {}
                    Verdict::Unsafe => {
                        unsafe_at = Some(unsafe_at.map_or(budget, |u| u.min(budget)));
                        break;
                    }
                    Verdict::Inapplicable => break,
                }
            }
        }
        Ok(unsafe_at.unwrap_or(failed) - 1)
    }

    /// The four diagnostic rows. They report only.
    fn diagnostics(&mut self, weight: u32) -> Result<(), Halt> {
        for which in 0..4 {
            self.boundary()?;
            let drawn = self.probe.diagnostic(which, weight);
            self.emit(&drawn)?;
        }
        Ok(())
    }

    /// Step 5: every applicable case must draw safely at N through the pinned crate, and at
    /// least one applicable case must draw unsafely at N + 1, unless N is the top of the ladder.
    /// An inapplicable draw is skipped at N and is never the unsafe row at N + 1. A failure at N
    /// lowers N by one and repeats when `lowering`; the first unsafe case then ends the matrix.
    /// Without `lowering`, the whole matrix at N is drawn, then the four diagnostic rows, and the
    /// result is `Failed`. The wrapper rows only report until the render path holds the bound,
    /// which one draw through it finds out before the first phase. The diagnostic rows follow a
    /// pass.
    fn verify(&mut self, mut n: u32, weight: u32, lowering: bool) -> Result<Verified, Halt> {
        let landed = self.probe.bound_landed();
        loop {
            if n == 0 {
                return Ok(Verified::NoPositive);
            }
            let mut all_safe = true;
            for case in 0..self.cases.len() {
                let drawn = self.draw(case, n, weight, Phase::VerifyLibrary)?;
                if drawn.verdict == Verdict::Unsafe {
                    all_safe = false;
                    if lowering {
                        break;
                    }
                }
            }
            if !all_safe {
                if lowering {
                    n -= 1;
                    continue;
                }
                self.diagnostics(weight)?;
                return Ok(Verified::Failed(n));
            }
            let mut wrapper_passes = true;
            for case in 0..self.cases.len() {
                let drawn = self.draw(case, n, weight, Phase::VerifyWrapper)?;
                if landed && drawn.verdict != Verdict::Inapplicable && drawn.last != Res::Svg {
                    wrapper_passes = false;
                }
            }
            let mut any_unsafe = false;
            for case in 0..self.cases.len() {
                let drawn = self.draw(case, n + 1, weight, Phase::VerifyLibrary)?;
                any_unsafe |= drawn.verdict == Verdict::Unsafe;
            }
            for case in 0..self.cases.len() {
                let drawn = self.draw(case, n + 1, weight, Phase::VerifyWrapper)?;
                if landed
                    && drawn.verdict != Verdict::Inapplicable
                    && !(drawn.last == Res::Invalid && drawn.reason == Some(bound_reason(n)))
                {
                    wrapper_passes = false;
                }
            }
            if !wrapper_passes || (!any_unsafe && n != TOP) {
                return Ok(Verified::Failed(n));
            }
            self.diagnostics(weight)?;
            return Ok(Verified::Passed(n));
        }
    }

    fn selected(&mut self, n: u32, weight: u32) -> Result<(), Halt> {
        writeln!(self.out, "selected\t{n}\t{weight}")?;
        self.out.flush()?;
        Ok(())
    }

    /// Steps 3 to 6.
    fn scan(&mut self) -> Result<Finish, Halt> {
        let of = |cases: &[Case], class: Class| -> Vec<usize> {
            (0..cases.len())
                .filter(|i| cases[*i].class() == class)
                .collect()
        };
        let unlabelled = of(self.cases, Class::Unlabelled);
        let labelled = of(self.cases, Class::Labelled);
        let all: Vec<usize> = (0..self.cases.len()).collect();

        let u = self.candidate(&unlabelled, 1)?;
        if u < MIN_N {
            self.selected(u, 1)?;
            return Ok(Finish::Under(u, 1));
        }
        let l = self.candidate(&labelled, 1)?;
        let weight = if l >= MIN_N {
            1
        } else if l == 0 {
            return Ok(Finish::NoPositive);
        } else {
            u.div_ceil(l).max(2)
        };
        let n = self.candidate(&all, weight)?;
        match self.verify(n, weight, true)? {
            Verified::NoPositive => Ok(Finish::NoPositive),
            Verified::Failed(n) => Ok(Finish::NotVerified(n)),
            Verified::Passed(n) => {
                self.selected(n, weight)?;
                Ok(if n < MIN_N {
                    Finish::Under(n, weight)
                } else {
                    Finish::Selected(n, weight)
                })
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Mode {
    Scan,
    Verify(u32, u32),
}

/// The two modes, or none for any other arguments.
fn parse_args(args: &[String]) -> Option<Mode> {
    let number = |text: &str| -> Option<u32> {
        if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let value: u32 = text.parse().ok()?;
        (1..=MAX_ARG).contains(&value).then_some(value)
    };
    match args {
        [mode] if mode == "scan" => Some(Mode::Scan),
        [mode, n, w] if mode == "verify" => Some(Mode::Verify(number(n)?, number(w)?)),
        _ => None,
    }
}

/// Runs a mode and writes the report to `out`.
fn run(
    mode: Mode,
    probe: &mut dyn Probe,
    cases: &[Case],
    out: &mut dyn Write,
) -> io::Result<Finish> {
    out.write_all(report_head(cases.len()).as_bytes())?;
    let mut search = Search::new(probe, cases, out);
    let result = match mode {
        Mode::Scan => search.scan(),
        Mode::Verify(n, weight) => search.verify(n, weight, false).and_then(|v| match v {
            Verified::Passed(n) => search
                .selected(n, weight)
                .map(|()| Finish::Selected(n, weight)),
            Verified::Failed(n) => Ok(Finish::NotVerified(n)),
            Verified::NoPositive => Ok(Finish::NoPositive),
        }),
    };
    match result {
        Ok(finish) => Ok(finish),
        Err(Halt::TimedOut) => Ok(Finish::TimedOut),
        Err(Halt::Io(e)) => Err(e),
    }
}

// ---------------------------------------------------------------------------------------------
// The real probe
// ---------------------------------------------------------------------------------------------

struct Real {
    clock: Wall,
}

impl Real {
    fn timed(
        &mut self,
        labels: &Labels,
        phase: Phase,
        source: Source,
        counts: (u32, u32),
    ) -> Drawn {
        let wrapper = phase == Phase::VerifyWrapper;
        let mut reason = None;
        let reps = run_reps(&mut self.clock, &mut || {
            if wrapper {
                let (attempt, why) = wrapped(&source.text);
                reason = why;
                attempt
            } else {
                direct(&source.text)
            }
        });
        Drawn::timed(labels, phase, &source.meta, counts, &reps, reason)
    }
}

impl Probe for Real {
    fn draw(&mut self, case: &Case, budget: u32, weight: u32, phase: Phase) -> Drawn {
        let labels = Labels::of(case);
        match generate(case, u64::from(budget), u64::from(weight)) {
            Some(source) => self.timed(&labels, phase, source, (budget, weight)),
            None => Drawn::inapplicable(&labels, phase, budget, weight),
        }
    }

    fn diagnostic(&mut self, which: usize, weight: u32) -> Drawn {
        let Some((name, form, source)) = diagnostic_source(which, u64::from(weight)) else {
            return Drawn {
                rows: Vec::new(),
                verdict: Verdict::Inapplicable,
                last: Res::Inapplicable,
                reason: None,
            };
        };
        let labels = Labels {
            shape: name.to_string(),
            style: form.name().to_string(),
            direction: Dir::Lr.name(),
            padding: Pad::None.name(),
        };
        let budget = u32::try_from(source.meta.cost).unwrap_or(u32::MAX);
        self.timed(&labels, Phase::Diagnostic, source, (budget, weight))
    }

    fn bound_landed(&mut self) -> bool {
        let (attempt, reason) = wrapped(&landing_source());
        bound_landed(&attempt, reason.as_deref())
    }

    fn elapsed(&mut self) -> u64 {
        self.clock.now()
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(mode) = parse_args(&args) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    // a panic in the pinned crate is a `panic` row, and prints nothing
    panic::set_hook(Box::new(|_| {}));
    let cases = enumerate();
    let mut probe = Real {
        clock: Wall(Instant::now()),
    };
    let stdout = io::stdout();
    let mut out = stdout.lock();
    match run(mode, &mut probe, &cases, &mut out) {
        Ok(finish) => {
            if let Some(message) = finish.message() {
                eprintln!("{message}");
            }
            ExitCode::from(finish.code())
        }
        Err(e) => {
            eprintln!("cannot write the report: {e}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const SAFE: u64 = 1_000_000;
    const SLOW: u64 = 2_000_000_000;
    const HOUR: u64 = 3_600_000_000_000;

    fn case(shape: Shape, form: Form, dir: Dir, pad: Pad) -> Case {
        Case {
            shape,
            style: Style::One(form),
            dir,
            pad,
        }
    }

    fn source(shape: Shape, form: Form, budget: u64, weight: u64) -> Source {
        generate(&case(shape, form, Dir::Lr, Pad::None), budget, weight).unwrap()
    }

    // An independent reading of a generated source: it knows the text forms, not the generator.

    /// The byte length, rank span and label of the connector at the start of `rest`.
    fn connector_at(rest: &str) -> (usize, u64, bool) {
        if rest.starts_with("~~~") {
            return (3, 1, false);
        }
        for (open, close) in [("-- ", " -->"), ("-. ", " .->"), ("== ", " ==>")] {
            if rest.starts_with(open) {
                return (rest.find(close).unwrap() + close.len(), 1, true);
            }
        }
        let bytes = rest.as_bytes();
        let mut end = 0;
        while end < bytes.len() && matches!(bytes[end], b'-' | b'.' | b'=' | b'>') {
            end += 1;
        }
        let run = &rest[..end];
        let span = if run.starts_with("-.") {
            run.matches('.').count() as u64
        } else if run.starts_with('-') {
            run.matches('-').count() as u64 - 1
        } else {
            run.matches('=').count() as u64 - 1
        };
        if bytes.get(end) == Some(&b'|') {
            let close = rest[end + 1..].find('|').unwrap();
            return (end + close + 2, span, true);
        }
        (end, span, false)
    }

    fn add_arrows(m: &mut Meta, n: u64, span: u64, labelled: bool, len: u64, weight: u64) {
        m.arrows += n;
        if labelled {
            m.labelled += n;
        }
        m.cost += n * span * if labelled { weight } else { 1 };
        m.span = m.span.max(span);
        m.connector = m.connector.max(len);
    }

    /// Reads the arrows of one flowchart statement: node groups joined by connectors.
    fn read_flow_line(line: &str, m: &mut Meta, weight: u64) {
        let bytes = line.as_bytes();
        let mut i = 0;
        let mut groups = Vec::new();
        let mut links = Vec::new();
        let mut size = 0;
        while i < bytes.len() {
            let c = bytes[i];
            if c == b' ' || c == b'&' {
                i += 1;
            } else if c.is_ascii_alphanumeric() || c == b'_' {
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                size += 1;
            } else {
                groups.push(size);
                size = 0;
                let (len, span, labelled) = connector_at(&line[i..]);
                links.push((len as u64, span, labelled));
                i += len;
            }
        }
        groups.push(size);
        for (k, (len, span, labelled)) in links.into_iter().enumerate() {
            add_arrows(m, groups[k] * groups[k + 1], span, labelled, len, weight);
        }
    }

    /// Reads a whole source.
    fn read(text: &str, weight: u64) -> Meta {
        let mut m = Meta {
            bytes: text.len() as u64,
            ..Meta::default()
        };
        let mut open = 0;
        let mut kind = None;
        for line in text.lines() {
            let t = line.trim();
            if t.is_empty() || t.starts_with("%%") {
                continue;
            }
            m.statements += 1;
            let Some(kind) = kind else {
                kind = Some(if t.starts_with("flowchart") {
                    Fam::Flow
                } else if t.starts_with("classDiagram") {
                    Fam::Class
                } else if t.starts_with("stateDiagram") {
                    Fam::State
                } else {
                    Fam::Seq
                });
                continue;
            };
            if t.starts_with("subgraph ") || t.ends_with(" {") {
                open += 1;
                m.depth = m.depth.max(open);
            } else if t == "end" || t == "}" {
                open -= 1;
            } else if kind == Fam::Flow {
                read_flow_line(t, &mut m, weight);
            } else if t.contains("-->") || t.contains("->>") {
                add_arrows(&mut m, 1, 1, t.ends_with(": y"), 3, weight);
            }
        }
        assert_eq!(open, 0, "every level closes");
        m
    }

    #[test]
    fn rdb3_matrix_metadata_matches_sources() {
        let cases = enumerate();
        assert_eq!(cases.len(), 2616, "the matrix is closed");
        let budgets = [1u64, 2, 31, 32, 33, 199, 200];
        let mut made = 0;
        for budget in budgets {
            for weight in [1u64, 4] {
                if weight == 4 && ![33, 200].contains(&budget) {
                    continue;
                }
                for case in &cases {
                    let name = format!("{case:?} at {budget} weight {weight}");
                    let Some(src) = generate(case, budget, weight) else {
                        assert!(
                            case.shape == Shape::StackLinked && budget < LINKS,
                            "{name} is inapplicable for a reason that is not the links"
                        );
                        continue;
                    };
                    made += 1;
                    let m = &src.meta;
                    assert!(m.bytes <= MAX_SOURCE_BYTES as u64, "{name}");
                    assert!(m.statements <= MAX_STATEMENTS as u64, "{name}");
                    assert!(m.depth <= MAX_DEPTH as u64, "{name}");
                    assert!(m.connector <= 256, "{name}");
                    assert_eq!(m.cost, budget, "{name}: the cost is the budget");
                    assert_eq!(read(&src.text, weight), src.meta, "{name}");
                }
            }
        }
        assert!(made > 20_000);
    }

    #[test]
    fn rdb3_metadata_fixtures_are_exact() {
        let s = source(Shape::PerLine, Form::Arrow, 3, 1);
        assert_eq!(s.text, "flowchart LR\nn0 --> n1\nn1 --> n2\nn2 --> n3\n");
        assert_eq!(
            s.meta,
            Meta {
                arrows: 3,
                labelled: 0,
                span: 1,
                connector: 3,
                cost: 3,
                bytes: 43,
                statements: 4,
                depth: 0
            }
        );
        let s = generate(
            &case(Shape::OneLine, Form::LabelArrow, Dir::Td, Pad::None),
            5,
            2,
        )
        .unwrap();
        assert_eq!(s.text, "flowchart TD\nn0 -- y --> n1 -- y --> n2 --> n3\n");
        assert_eq!((s.meta.arrows, s.meta.labelled, s.meta.cost), (3, 2, 5));
        let s = source(Shape::Pairs, Form::Arrow3, 5, 1);
        assert_eq!(s.text, "flowchart LR\nn0 ---> n1\nn2 ---> n3\nn4 --> n5\n");
        assert_eq!((s.meta.span, s.meta.connector), (2, 4));
        let s = source(Shape::FanIn, Form::Hidden, 2, 1);
        assert_eq!(s.text, "flowchart LR\nn1 ~~~ n0\nn2 ~~~ n0\n");
        // the 256-byte forms: the run, or the label, is extended
        for form in [
            Form::ArrowLong,
            Form::DottedLong,
            Form::ThickLong,
            Form::LabelArrowPad,
            Form::LabelDottedPad,
            Form::LabelThickPad,
            Form::PipeArrowPad,
        ] {
            assert_eq!(form.text().len(), 256, "{form:?}");
            assert_eq!(form.connector(), 256);
        }
        let s = source(Shape::PerLine, Form::DottedLong, 300, 1);
        assert_eq!(
            (s.meta.span, s.meta.connector, s.meta.arrows),
            (253, 256, 48)
        );
        let s = source(Shape::PerLine, Form::ArrowLong, 300, 1);
        assert_eq!((s.meta.span, s.meta.arrows, s.meta.cost), (254, 47, 300));
        // padding is exact
        let comments = generate(
            &case(Shape::PerLine, Form::Arrow, Dir::Lr, Pad::Comments),
            3,
            1,
        )
        .unwrap();
        assert_eq!(comments.text.len(), MAX_SOURCE_BYTES);
        assert_eq!(comments.meta.statements, 4);
        assert!(comments.text.starts_with("flowchart LR\nn0 --> n1\n"));
        let both = generate(&case(Shape::PerLine, Form::Arrow, Dir::Lr, Pad::Both), 3, 1).unwrap();
        assert_eq!(both.text.len(), MAX_SOURCE_BYTES);
        assert_eq!(both.meta.statements, MAX_STATEMENTS as u64);
        let dups = generate(
            &case(Shape::PerLine, Form::Arrow, Dir::Lr, Pad::Duplicates),
            3,
            1,
        )
        .unwrap();
        assert_eq!(dups.meta.statements, MAX_STATEMENTS as u64);
        assert!(dups.text.ends_with("\nn0\n"));
        assert_eq!(dups.text.lines().filter(|l| *l == "n0").count(), 2044);
        // a source over a bound is inapplicable, never cut
        assert!(
            generate(
                &case(Shape::PerLine, Form::Arrow, Dir::Lr, Pad::None),
                MAX_STATEMENTS as u64,
                1
            )
            .is_none()
        );
        assert!(
            generate(
                &case(Shape::PerLine, Form::Arrow, Dir::Lr, Pad::None),
                MAX_STATEMENTS as u64 - 1,
                1
            )
            .is_some()
        );
        // the four diagnostic sources
        let (name, _, chain) = diagnostic_source(0, 1).unwrap();
        assert_eq!(name, "diagnostic-chain-65k");
        assert!(chain.meta.bytes > 65000 && chain.meta.bytes <= 65536);
        assert_eq!(chain.meta.statements, 2);
        assert_eq!(read(&chain.text, 1), chain.meta);
        let (_, _, pairs) = diagnostic_source(1, 3).unwrap();
        assert_eq!(pairs.meta.statements, 2048);
        assert_eq!((pairs.meta.arrows, pairs.meta.cost), (2047, 2047 * 3));
        let (_, _, wide) = diagnostic_source(2, 1).unwrap();
        assert_eq!((wide.meta.depth, wide.meta.arrows), (32, 960));
        let (_, _, narrow) = diagnostic_source(3, 1).unwrap();
        assert_eq!((narrow.meta.depth, narrow.meta.arrows), (16, 480));
        assert!(diagnostic_source(4, 1).is_none());
    }

    /// Structure of a nested or stacked source: for each opener, its node ids and arrow lines.
    fn levels_of(text: &str) -> Vec<(Vec<String>, usize)> {
        let mut levels: Vec<(Vec<String>, usize)> = Vec::new();
        let mut current: Option<usize> = None;
        let mut open = 0usize;
        let mut after_ends = false;
        for line in text.lines().skip(1) {
            if line.starts_with("subgraph ") || (line.starts_with("state S") && line.ends_with('{'))
            {
                levels.push((Vec::new(), 0));
                current = Some(levels.len() - 1);
                open += 1;
            } else if line == "end" || line == "}" {
                open -= 1;
                after_ends = true;
                current = None;
            } else if line.starts_with("direction ") || line.contains("~~~") && after_ends {
                // a stacked subgraph's direction, or a link after the last level
            } else if let Some(at) = current {
                let words: Vec<&str> = line.split(' ').collect();
                if line.contains("-->") {
                    levels[at].1 += 1;
                    levels[at].0.push(words[0].to_string());
                    levels[at].0.push(words[2].to_string());
                } else {
                    levels[at].0.push(words[0].to_string());
                }
            }
        }
        assert_eq!(open, 0);
        levels
    }

    #[test]
    fn rdb3_nested_and_stacked_sources_close_every_level() {
        let shapes = [
            (Shape::Nest16, 16, 16),
            (Shape::Nest32, 32, 32),
            (Shape::Stack, 16, 1),
            (Shape::StackLinked, 16, 1),
            (Shape::StateBlocksChain, 32, 32),
            (Shape::StateBlocksPairs, 32, 32),
        ];
        for (shape, k, depth) in shapes {
            for arrows in [0usize, 1, 32, 33, 960] {
                let linked = shape == Shape::StackLinked;
                let family = shape.family();
                let form = if family == Fam::Flow {
                    Form::Arrow
                } else {
                    Form::FamPlain
                };
                let budget = arrows as u64 + if linked { LINKS } else { 0 };
                let src = source(shape, form, budget, 1);
                let name = format!("{shape:?} with {arrows} arrows");
                assert_eq!(src.meta.depth, depth, "{name}");
                assert_eq!(read(&src.text, 1), src.meta, "{name}");
                let levels = levels_of(&src.text);
                assert_eq!(levels.len(), k, "{name}");
                let mut union = std::collections::HashSet::new();
                let mut total = 0;
                for (j, (ids, count)) in levels.iter().enumerate() {
                    let expected = arrows / k + usize::from(j < arrows % k);
                    assert_eq!(*count, expected, "{name}: level {j} holds its share");
                    if expected == 0 {
                        assert_eq!(ids.len(), 1, "{name}: an empty level has one node");
                    }
                    let own: std::collections::HashSet<&String> = ids.iter().collect();
                    total += own.len();
                    union.extend(own);
                }
                assert_eq!(union.len(), total, "{name}: levels do not share nodes");
                let ends = src
                    .text
                    .lines()
                    .filter(|l| *l == "end" || *l == "}")
                    .count();
                assert_eq!(ends, k, "{name}");
                if linked {
                    let links: Vec<&str> =
                        src.text.lines().filter(|l| l.contains(" ~~~ ")).collect();
                    assert_eq!(links.len(), 15, "{name}");
                    assert_eq!(links[0], "S0 ~~~ S1");
                    assert_eq!(links[14], "S14 ~~~ S15");
                    assert!(src.text.ends_with("S14 ~~~ S15\n"));
                }
                if shape == Shape::Stack || linked {
                    assert_eq!(
                        src.text.lines().filter(|l| *l == "direction LR").count(),
                        16,
                        "{name}"
                    );
                }
                if arrows == 1 {
                    assert_eq!(direct(&src.text).result, Res::Svg, "{name} draws");
                }
            }
        }
        // a stack below its links is inapplicable
        assert!(
            generate(
                &case(Shape::StackLinked, Form::Arrow, Dir::Lr, Pad::None),
                14,
                1
            )
            .is_none()
        );
        assert!(
            generate(
                &case(Shape::StackLinked, Form::Arrow, Dir::Lr, Pad::None),
                15,
                1
            )
            .is_some()
        );
    }

    /// Whether a source of each form and family is valid to the pinned crate. The deeply nested
    /// shapes take seconds to draw in a debug build, so the nesting test draws those.
    #[test]
    fn rdb3_every_form_and_family_draws_when_small() {
        let nested = |c: &Case| {
            matches!(
                c.shape,
                Shape::Nest16 | Shape::Nest32 | Shape::StateBlocksPairs | Shape::StateBlocksChain
            )
        };
        let cases = enumerate();
        let mut drawn = 0;
        for case in cases
            .iter()
            .filter(|c| c.pad == Pad::None && c.dir != Dir::Td && !nested(c))
        {
            let Some(src) = generate(case, 16, 2) else {
                continue;
            };
            assert_eq!(direct(&src.text).result, Res::Svg, "{case:?}\n{}", src.text);
            drawn += 1;
        }
        // one padded source of each family and of the flowchart shapes
        for case in cases.iter().filter(|c| {
            c.pad == Pad::Both
                && c.dir != Dir::Td
                && !nested(c)
                && matches!(c.style, Style::One(Form::FamPlain | Form::Arrow))
        }) {
            let Some(src) = generate(case, 16, 1) else {
                continue;
            };
            assert_eq!(direct(&src.text).result, Res::Svg, "{case:?}");
            drawn += 1;
        }
        assert!(drawn > 250, "{drawn} sources drew");
    }

    // The protocol on synthetic timings.

    struct Scripted<'a>(&'a Cell<u64>);

    impl Clock for Scripted<'_> {
        fn now(&mut self) -> u64 {
            self.0.get()
        }
    }

    /// A logged draw: the case, the budget, the weight and the phase.
    type Draw = (usize, u32, u32, Phase);

    type Rule<'a> = Box<dyn Fn(&Case, usize, u32, u32, Phase) -> u64 + 'a>;

    type Gap<'a> = Box<dyn Fn(&Case, usize, u32, Phase) -> bool + 'a>;

    /// A probe whose repetitions take the time a rule gives, on a clock the repetitions move.
    /// A gap rule makes a draw inapplicable. `landed` and `wrapper_limit` stand for the render
    /// path: whether the detection draw is refused with the bound's reason, and the largest
    /// budget the wrapper phase still draws.
    struct Mock<'a> {
        index: HashMap<Case, usize>,
        time: Cell<u64>,
        rule: Rule<'a>,
        gap: Option<Gap<'a>>,
        landed: bool,
        wrapper_limit: Option<u32>,
        log: Vec<Draw>,
    }

    impl<'a> Mock<'a> {
        fn new(cases: &[Case], rule: Rule<'a>) -> Mock<'a> {
            Mock {
                index: cases.iter().enumerate().map(|(i, c)| (*c, i)).collect(),
                time: Cell::new(0),
                rule,
                gap: None,
                landed: false,
                wrapper_limit: None,
                log: Vec::new(),
            }
        }

        fn gap(mut self, gap: impl Fn(&Case, usize, u32, Phase) -> bool + 'a) -> Mock<'a> {
            self.gap = Some(Box::new(gap));
            self
        }

        fn render_path(mut self, landed: bool, wrapper_limit: Option<u32>) -> Mock<'a> {
            self.landed = landed;
            self.wrapper_limit = wrapper_limit;
            self
        }
    }

    impl Probe for Mock<'_> {
        fn draw(&mut self, case: &Case, budget: u32, weight: u32, phase: Phase) -> Drawn {
            let at = self.index[case];
            self.log.push((at, budget, weight, phase));
            if self
                .gap
                .as_ref()
                .is_some_and(|gap| gap(case, at, budget, phase))
            {
                return Drawn::inapplicable(&Labels::of(case), phase, budget, weight);
            }
            let ns = (self.rule)(case, at, budget, weight, phase);
            let refusal = match (phase, self.wrapper_limit) {
                (Phase::VerifyWrapper, Some(limit)) if budget > limit => Some(bound_reason(limit)),
                _ => None,
            };
            let result = if refusal.is_some() {
                Res::Invalid
            } else {
                Res::Svg
            };
            let mut clock = Scripted(&self.time);
            let time = &self.time;
            let reps = run_reps(&mut clock, &mut || {
                time.set(time.get() + ns);
                Attempt {
                    result,
                    svg_bytes: 10,
                }
            });
            Drawn::timed(
                &Labels::of(case),
                phase,
                &Meta::default(),
                (budget, weight),
                &reps,
                refusal,
            )
        }

        fn bound_landed(&mut self) -> bool {
            self.landed
        }

        fn diagnostic(&mut self, which: usize, weight: u32) -> Drawn {
            let labels = Labels {
                shape: format!("diagnostic-{which}"),
                style: "-->".to_string(),
                direction: "LR",
                padding: "none",
            };
            let rep = Rep {
                ns: 1,
                attempt: Attempt {
                    result: Res::Svg,
                    svg_bytes: 1,
                },
            };
            Drawn::timed(
                &labels,
                Phase::Diagnostic,
                &Meta::default(),
                (1, weight),
                &[rep, rep, rep],
                None,
            )
        }

        fn elapsed(&mut self) -> u64 {
            self.time.get()
        }
    }

    fn indexes(cases: &[Case], class: Class) -> Vec<usize> {
        (0..cases.len())
            .filter(|i| cases[*i].class() == class)
            .collect()
    }

    /// Runs a mode on a built mock and returns how it ended, its report and the draws.
    fn run_mock(mode: Mode, mut mock: Mock) -> (Finish, String, Vec<Draw>) {
        let cases = enumerate();
        let mut out = Vec::new();
        let finish = run(mode, &mut mock, &cases, &mut out).unwrap();
        (finish, String::from_utf8(out).unwrap(), mock.log)
    }

    /// Runs a mode on a rule and returns how it ended, its report and the draws.
    fn drive(
        mode: Mode,
        rule: impl Fn(&Case, usize, u32, u32, Phase) -> u64,
    ) -> (Finish, String, Vec<Draw>) {
        let cases = enumerate();
        let mut mock = Mock::new(&cases, Box::new(rule));
        let mut out = Vec::new();
        let finish = run(mode, &mut mock, &cases, &mut out).unwrap();
        (finish, String::from_utf8(out).unwrap(), mock.log)
    }

    #[test]
    fn rdb3_protocol_selects_n_and_w() {
        let cases = enumerate();
        // exactly 2,000,000,000 ns is unsafe; the repetitions stop at the first unsafe one
        let time = Cell::new(0);
        for (script, reps, safe) in [
            (vec![1_999_999_999; 3], 3, true),
            (vec![SLOW], 1, false),
            (vec![SAFE, SLOW], 2, false),
            (vec![SAFE, SAFE, SLOW], 3, false),
            (vec![SAFE, SAFE, 2_000_000_001], 3, false),
        ] {
            time.set(0);
            let mut calls = 0;
            let got = run_reps(&mut Scripted(&time), &mut || {
                time.set(time.get() + script[calls]);
                calls += 1;
                Attempt {
                    result: Res::Svg,
                    svg_bytes: 1,
                }
            });
            assert_eq!(got.len(), reps, "{script:?}");
            let drawn = Drawn::timed(
                &Labels::of(&cases[0]),
                Phase::Ladder,
                &Meta::default(),
                (16, 1),
                &got,
                None,
            );
            assert_eq!(drawn.verdict == Verdict::Safe, safe, "{script:?}");
        }
        // a result that is not svg stops the repetitions and is unsafe
        let mut calls = 0;
        let got = run_reps(&mut Scripted(&time), &mut || {
            calls += 1;
            Attempt {
                result: Res::Invalid,
                svg_bytes: 0,
            }
        });
        assert_eq!((got.len(), calls), (1, 1));
        assert!(!rep_safe(&got[0]));

        // the ladder stops each case at its first unsafe budget; the refine range holds only
        // the cases whose first unsafe budget is the smallest
        let unlabelled = indexes(&cases, Class::Unlabelled);
        let (a, b, c, d) = (unlabelled[0], unlabelled[1], unlabelled[2], unlabelled[3]);
        let rule = |_: &Case, at: usize, budget: u32, _: u32, _: Phase| {
            let limit = match at {
                x if x == a => 500,
                x if x == b => 400,
                x if x == c => 1000,
                _ => u32::MAX,
            };
            if budget >= limit { SLOW } else { SAFE }
        };
        let mut mock = Mock::new(&cases, Box::new(rule));
        let mut out = Vec::new();
        let n = Search::new(&mut mock, &cases, &mut out)
            .candidate(&[a, b, c, d], 1)
            .map_err(|_| "halt")
            .unwrap();
        assert_eq!(n, 399, "the first unsafe budget of the F cases, minus one");
        let draws_of = |at: usize, phase: Phase| -> Vec<u32> {
            mock.log
                .iter()
                .filter(|e| e.0 == at && e.3 == phase)
                .map(|e| e.1)
                .collect()
        };
        assert_eq!(
            draws_of(a, Phase::Ladder),
            [16, 32, 64, 128, 192, 256, 384, 512]
        );
        assert_eq!(draws_of(a, Phase::Refine), (385..=500).collect::<Vec<_>>());
        assert_eq!(draws_of(b, Phase::Refine), (385..=400).collect::<Vec<_>>());
        assert_eq!(draws_of(c, Phase::Refine), Vec::<u32>::new());
        assert_eq!(draws_of(d, Phase::Refine), Vec::<u32>::new());
        assert_eq!(*draws_of(c, Phase::Ladder).last().unwrap(), 1024);
        assert_eq!(draws_of(d, Phase::Ladder), LADDER);
        // a: 8 ladder + 116 refine; b: 8 + 16; c: 10 ladder; d: 16 ladder
        assert_eq!(mock.log.len(), 8 + 116 + 8 + 16 + 10 + 16);

        // no unsafe draw in the refine range: the candidate is F - 1
        let candidate = |slow_from: u32, only_at: Option<u32>| {
            let rule = move |_: &Case, at: usize, budget: u32, _: u32, _: Phase| {
                let slow = match only_at {
                    Some(only) => budget == only,
                    None => budget >= slow_from,
                };
                if at == 0 && slow { SLOW } else { SAFE }
            };
            let mut mock = Mock::new(&cases, Box::new(rule));
            let mut out = Vec::new();
            Search::new(&mut mock, &cases, &mut out)
                .candidate(&[0, 1], 1)
                .map_err(|_| "halt")
                .unwrap()
        };
        assert_eq!(candidate(0, Some(512)), 511);
        assert_eq!(candidate(5, None), 4, "F = 16 refines from budget 1");
        assert_eq!(candidate(1, None), 0);
        assert_eq!(
            candidate(u32::MAX, None),
            8192,
            "no unsafe draw on the ladder"
        );

        // U = 400 and L = 100 give W = 4
        let (finish, report, _) = drive(Mode::Scan, |case, _, budget, weight, _| {
            let limit = if case.weighted() { 100 * weight } else { 400 };
            if budget > limit { SLOW } else { SAFE }
        });
        assert_eq!(finish, Finish::Selected(400, 4));
        assert!(report.starts_with("plan\t2616\t125568\t251136\nphase\tshape\t"));
        assert!(report.ends_with("selected\t400\t4\n"));
        assert!(report.contains("\nverify-library\t"));
        assert!(report.contains("\nverify-wrapper\t"));
        assert!(report.contains("\ndiagnostic\t"));
        assert!(report.contains("\nrefine\t"));
        // U = 400 and L = 200 give W = 1
        let (finish, report, _) = drive(Mode::Scan, |case, _, budget, _, _| {
            let limit = if case.weighted() { 200 } else { 400 };
            if budget > limit { SLOW } else { SAFE }
        });
        assert_eq!(finish, Finish::Selected(200, 1));
        assert!(report.ends_with("selected\t200\t1\n"));
        // U = 150 stops at once with exit 3, and no labelled case is drawn
        let (finish, report, log) = drive(Mode::Scan, |case, _, budget, _, _| {
            let limit = if case.weighted() { 1000 } else { 150 };
            if budget > limit { SLOW } else { SAFE }
        });
        assert_eq!(finish, Finish::Under(150, 1));
        assert_eq!(finish.code(), 3);
        assert!(report.ends_with("selected\t150\t1\n"));
        assert!(log.iter().all(|e| !cases[e.0].weighted()));
        // U = 0 is under 200 too: it prints the selection and exits 3, and draws no labelled case
        let (finish, report, log) = drive(
            Mode::Scan,
            |case, _, _, _, _| {
                if case.weighted() { SAFE } else { SLOW }
            },
        );
        assert_eq!(finish, Finish::Under(0, 1));
        assert_eq!(finish.code(), 3);
        assert!(report.ends_with("selected\t0\t1\n"));
        assert!(log.iter().all(|e| !cases[e.0].weighted()));
        // L = 0 exits 1
        let (finish, report, _) = drive(Mode::Scan, |case, _, budget, _, _| {
            if case.weighted() || budget > 400 {
                SLOW
            } else {
                SAFE
            }
        });
        assert_eq!(finish, Finish::NoPositive);
        assert_eq!(finish.code(), 1);
        assert!(!report.contains("selected"));
        // a verify failure at N lowers N by one and repeats
        let (finish, report, log) = drive(Mode::Scan, |_, at, budget, _, phase| {
            let slow = budget > 400 || (phase == Phase::VerifyLibrary && at == 7 && budget >= 400);
            if slow { SLOW } else { SAFE }
        });
        assert_eq!(finish, Finish::Selected(399, 1));
        assert!(report.ends_with("selected\t399\t1\n"));
        let verified: Vec<u32> = log
            .iter()
            .filter(|e| e.3 == Phase::VerifyLibrary)
            .map(|e| e.1)
            .collect();
        assert_eq!(
            verified[..8],
            [400; 8],
            "N = 400 stops at its first unsafe case"
        );
        assert_eq!(verified[8], 399);
        // nothing unsafe at N + 1 fails the verify
        let (finish, report, _) = drive(Mode::Scan, |_, _, budget, _, phase| {
            let slow = budget > 400 && matches!(phase, Phase::Ladder | Phase::Refine);
            if slow { SLOW } else { SAFE }
        });
        assert_eq!(finish, Finish::NotVerified(400));
        assert!(!report.contains("selected"));
        assert!(!report.contains("\ndiagnostic\t"));
        // nothing unsafe at all: the candidate is the top of the ladder, which needs no N + 1
        let (finish, _, _) = drive(Mode::Scan, |_, _, _, _, _| SAFE);
        assert_eq!(finish, Finish::Selected(8192, 1));
        // the 12-hour stop: at the next draw boundary, with the rows so far
        let (finish, report, log) = drive(Mode::Scan, |_, _, _, _, _| HOUR);
        assert_eq!(finish, Finish::TimedOut);
        assert_eq!(
            log.len(),
            12,
            "one hour per draw, the thirteenth never starts"
        );
        assert_eq!(report.lines().count(), 2 + 12);
        assert!(!report.contains("selected"));
        // verify mode: one pass, no lowering
        let (finish, report, log) = drive(Mode::Verify(300, 4), |_, at, budget, _, _| {
            if budget > 300 && at == 5 { SLOW } else { SAFE }
        });
        assert_eq!(finish, Finish::Selected(300, 4));
        assert!(report.ends_with("selected\t300\t4\n"));
        assert!(
            log.iter()
                .all(|e| e.3 != Phase::Ladder && e.3 != Phase::Refine)
        );
        let (finish, report, log) = drive(Mode::Verify(300, 4), |_, at, budget, _, _| {
            if budget >= 300 && at == 5 { SLOW } else { SAFE }
        });
        assert_eq!(finish, Finish::NotVerified(300));
        assert!(!report.contains("selected"));
        assert!(log.iter().all(|e| e.1 == 300));
    }

    #[test]
    fn rdb3_verify_failure_draws_the_matrix_and_diagnostics() {
        let cases = enumerate();
        // verify mode: the first unsafe case at N does not end the matrix
        let (finish, report, log) = drive(Mode::Verify(300, 4), |_, at, budget, _, _| {
            if budget >= 300 && at == 5 { SLOW } else { SAFE }
        });
        assert_eq!(finish, Finish::NotVerified(300));
        assert!(!report.contains("selected"));
        let drawn: Vec<usize> = log
            .iter()
            .filter(|e| e.3 == Phase::VerifyLibrary)
            .map(|e| e.0)
            .collect();
        assert_eq!(drawn, (0..cases.len()).collect::<Vec<_>>());
        assert!(
            log.iter()
                .all(|e| e.3 == Phase::VerifyLibrary && e.1 == 300)
        );
        let diagnostics = report
            .lines()
            .filter(|l| l.starts_with("diagnostic\t"))
            .count();
        assert_eq!(
            diagnostics,
            4 * REPS,
            "the four diagnostic rows, three repetitions each"
        );
        // the diagnostic rows follow the matrix
        let last_matrix = report.rfind("\nverify-library\t").unwrap();
        let first_diagnostic = report.find("\ndiagnostic\t").unwrap();
        assert!(last_matrix < first_diagnostic);

        // scan mode keeps the early break and lowers N: the first unsafe case ends the matrix
        let (finish, report, log) = drive(Mode::Scan, |_, at, budget, _, phase| {
            let slow = budget > 400 || (phase == Phase::VerifyLibrary && at == 7 && budget >= 400);
            if slow { SLOW } else { SAFE }
        });
        assert_eq!(finish, Finish::Selected(399, 1));
        let first_lowered = log
            .iter()
            .position(|e| e.3 == Phase::VerifyLibrary && e.1 == 399)
            .unwrap();
        let at_400 = log[..first_lowered]
            .iter()
            .filter(|e| e.3 == Phase::VerifyLibrary && e.1 == 400)
            .count();
        assert_eq!(
            at_400, 8,
            "scan mode stops at the first unsafe case and lowers N"
        );
        // the diagnostics are drawn once, after the pass
        assert_eq!(
            report
                .lines()
                .filter(|l| l.starts_with("diagnostic\t"))
                .count(),
            4 * REPS
        );
    }

    #[test]
    fn rdb3_inapplicable_is_not_unsafe() {
        let cases = enumerate();
        let unlabelled = indexes(&cases, Class::Unlabelled);
        let (a, b) = (unlabelled[0], unlabelled[1]);
        let candidate = |mock: &mut Mock, set: &[usize]| {
            let mut out = Vec::new();
            Search::new(mock, &cases, &mut out)
                .candidate(set, 1)
                .map_err(|_| "halt")
                .unwrap()
        };
        let draws_of = |mock: &Mock, at: usize, phase: Phase| -> Vec<u32> {
            mock.log
                .iter()
                .filter(|e| e.0 == at && e.3 == phase)
                .map(|e| e.1)
                .collect()
        };

        // the ladder stops at an inapplicable budget and sets no first unsafe budget
        let mut mock = Mock::new(&cases, Box::new(|_, _, _, _, _| SAFE))
            .gap(move |_, at, budget, _| at == a && budget >= 256);
        assert_eq!(candidate(&mut mock, &[a, b]), 8192, "no case is unsafe");
        assert_eq!(
            draws_of(&mock, a, Phase::Ladder),
            [16, 32, 64, 128, 192, 256]
        );
        assert!(draws_of(&mock, a, Phase::Refine).is_empty());
        assert_eq!(draws_of(&mock, b, Phase::Ladder), LADDER);

        // the refine treats an inapplicable budget as the end of that case, not as a failure:
        // a is unsafe from 500; b is inapplicable from 385 to 511 and unsafe from 512
        let rule = move |_: &Case, at: usize, budget: u32, _: u32, _: Phase| {
            let limit = if at == a { 500 } else { 512 };
            if budget >= limit { SLOW } else { SAFE }
        };
        let mut mock = Mock::new(&cases, Box::new(rule))
            .gap(move |_, at, budget, _| at == b && (385..512).contains(&budget));
        assert_eq!(candidate(&mut mock, &[a, b]), 499);
        assert_eq!(
            draws_of(&mock, b, Phase::Refine),
            [385],
            "b ends at its first gap"
        );
        assert_eq!(
            draws_of(&mock, a, Phase::Refine),
            (385..=500).collect::<Vec<_>>()
        );
        // when no F case finds an unsafe budget, the candidate is F - 1
        let mut mock = Mock::new(&cases, Box::new(rule))
            .gap(move |_, at, budget, _| at == b && (385..512).contains(&budget));
        assert_eq!(candidate(&mut mock, &[b]), 511);

        // a stack-16-linked case with F_c = 16 gives no candidate of 0: its refine range 1..15
        // is inapplicable from the first budget
        let stack = cases
            .iter()
            .position(|c| {
                c.shape == Shape::StackLinked
                    && c.style == Style::One(Form::Arrow)
                    && c.dir == Dir::Lr
                    && c.pad == Pad::None
            })
            .unwrap();
        assert!(generate(&cases[stack], 14, 1).is_none());
        assert!(generate(&cases[stack], 16, 1).is_some());
        let mut mock = Mock::new(&cases, Box::new(|_, _, _, _, _| SLOW))
            .gap(|case, _, budget, _| generate(case, u64::from(budget), 1).is_none());
        assert_eq!(candidate(&mut mock, &[stack]), 15);
        assert_eq!(draws_of(&mock, stack, Phase::Ladder), [16]);
        assert_eq!(
            draws_of(&mock, stack, Phase::Refine),
            [1],
            "it ends at the first gap"
        );

        // verify at N skips an inapplicable case; N + 1 needs an applicable unsafe row
        let x = 3;
        let rule = |_: &Case, at: usize, budget: u32, _: u32, _: Phase| {
            if at == 9 && budget > 300 { SLOW } else { SAFE }
        };
        let mock =
            Mock::new(&cases, Box::new(rule)).gap(move |_, at, budget, _| at == x && budget >= 300);
        let (finish, report, log) = run_mock(Mode::Verify(300, 1), mock);
        assert_eq!(finish, Finish::Selected(300, 1));
        assert!(log.contains(&(x, 300, 1, Phase::VerifyLibrary)));
        let skipped = Drawn::inapplicable(&Labels::of(&cases[x]), Phase::VerifyLibrary, 300, 1);
        assert!(report.contains(&format!("{}\n", skipped.rows[0].line())));
        // every row at N + 1 is inapplicable: no unsafe row, so the limit is not found
        let mock =
            Mock::new(&cases, Box::new(|_, _, _, _, _| SAFE)).gap(|_, _, budget, _| budget >= 301);
        let (finish, report, _) = run_mock(Mode::Verify(300, 1), mock);
        assert_eq!(finish, Finish::NotVerified(300));
        assert!(!report.contains("selected"));
        // one case is inapplicable at N + 1 and the rest are safe: still not found
        let mock = Mock::new(&cases, Box::new(|_, _, _, _, _| SAFE))
            .gap(move |_, at, budget, _| at == x && budget >= 301);
        let (finish, _, _) = run_mock(Mode::Verify(300, 1), mock);
        assert_eq!(finish, Finish::NotVerified(300));
    }

    #[test]
    fn rdb3_the_wrapper_gate_is_detected_at_run_time() {
        // the detection reads the render path's refusal: invalid, with the bound's two texts
        let refused = Attempt {
            result: Res::Invalid,
            svg_bytes: 0,
        };
        let drawn = Attempt {
            result: Res::Svg,
            svg_bytes: 5,
        };
        assert!(bound_landed(&refused, Some(&bound_reason(300))));
        assert!(bound_landed(&refused, Some(&bound_reason(1))));
        assert!(!bound_landed(&drawn, None));
        assert!(!bound_landed(&drawn, Some(&bound_reason(300))));
        assert!(!bound_landed(&refused, None));
        assert!(!bound_landed(
            &refused,
            Some("the source has an arrow over 256 bytes")
        ));
        assert!(!bound_landed(
            &refused,
            Some("the source has over 300 arrows")
        ));
        assert!(!bound_landed(
            &refused,
            Some("a subgraph is not closed, counting a longer or labelled arrow as more than one")
        ));
        // the detection source is far over the ladder top and inside every other bound
        let text = landing_source();
        assert_eq!(text.matches("-->").count(), 9000);
        assert!(text.len() <= MAX_SOURCE_BYTES, "{} bytes", text.len());
        assert_eq!(text.lines().count(), 2, "one header line and one statement");
        assert!(text.lines().all(|l| l.len() <= 256 * 1024));

        // not landed: the wrapper rows only report, whatever the wrapper does
        let cases = enumerate();
        let unsafe_above = |_: &Case, _: usize, budget: u32, _: u32, phase: Phase| {
            if budget > 300 && phase == Phase::VerifyLibrary {
                SLOW
            } else {
                SAFE
            }
        };
        for limit in [None, Some(299), Some(300), Some(500)] {
            let mock = Mock::new(&cases, Box::new(unsafe_above)).render_path(false, limit);
            let (finish, _, _) = run_mock(Mode::Verify(300, 1), mock);
            assert_eq!(
                finish,
                Finish::Selected(300, 1),
                "not landed, limit {limit:?}"
            );
        }
        // landed: svg at N and invalid with the exact reason at N + 1, or the verify fails
        let mock = Mock::new(&cases, Box::new(unsafe_above)).render_path(true, Some(300));
        let (finish, report, _) = run_mock(Mode::Verify(300, 1), mock);
        assert_eq!(finish, Finish::Selected(300, 1));
        assert!(report.contains("\tinvalid\t"));
        // the wrapper refuses already at N
        let mock = Mock::new(&cases, Box::new(unsafe_above)).render_path(true, Some(299));
        let (finish, _, _) = run_mock(Mode::Verify(300, 1), mock);
        assert_eq!(finish, Finish::NotVerified(300));
        // the wrapper still draws at N + 1
        for limit in [Some(500), None] {
            let mock = Mock::new(&cases, Box::new(unsafe_above)).render_path(true, limit);
            let (finish, _, _) = run_mock(Mode::Verify(300, 1), mock);
            assert_eq!(finish, Finish::NotVerified(300), "limit {limit:?}");
        }
    }

    #[test]
    fn rdb3_the_real_probe_draws_and_reports() {
        let mut real = Real {
            clock: Wall(Instant::now()),
        };
        let c = case(Shape::PerLine, Form::Arrow, Dir::Lr, Pad::None);
        for phase in [Phase::Ladder, Phase::VerifyWrapper] {
            let drawn = real.draw(&c, 16, 1, phase);
            assert_eq!(drawn.rows.len(), REPS);
            assert!(drawn.rows.iter().all(|r| r.result == Res::Svg));
            assert!(
                drawn
                    .rows
                    .iter()
                    .all(|r| r.svg_bytes > 0 && r.meta.cost == 16)
            );
            assert_eq!(drawn.rows[2].repeat, 3);
            assert!(drawn.reason.is_none());
        }
        let stack = case(Shape::StackLinked, Form::Arrow, Dir::Lr, Pad::None);
        let drawn = real.draw(&stack, 14, 1, Phase::Ladder);
        assert_eq!(drawn.verdict, Verdict::Inapplicable);
        assert_eq!(drawn.rows.len(), 1);
        for which in 0..3 {
            // the diagnostic sources are too slow for a debug build: only build them
            assert!(diagnostic_source(which, 1).is_some());
        }
        assert!(real.diagnostic(9, 1).rows.is_empty());
        assert!(real.elapsed() > 0);
    }

    #[test]
    fn rdb3_budgets_and_group_products_are_exact() {
        let cases = enumerate();
        for budget in [16u64, 200, 2048, 8192] {
            for weight in [1u64, 3] {
                for case in &cases {
                    let got = generate(case, budget, weight);
                    if budget <= 200 {
                        assert!(got.is_some(), "{case:?} at {budget}");
                    }
                    if let Some(src) = got {
                        assert_eq!(
                            src.meta.cost, budget,
                            "{case:?} at {budget} weight {weight}"
                        );
                        assert!(src.meta.bytes <= MAX_SOURCE_BYTES as u64);
                        if budget <= 200 {
                            assert_eq!(read(&src.text, weight).cost, budget, "{case:?}");
                        }
                    }
                }
            }
        }
        // labelled shares and orders: the sequence of arrows is exact
        let order_case = |plain: Form, labelled: Form, share: Share, order: Order| Case {
            shape: Shape::PerLine,
            style: Style::Mixed {
                plain,
                labelled,
                share,
                order,
            },
            dir: Dir::Lr,
            pad: Pad::None,
        };
        let pairs = [
            (
                Form::Arrow,
                Form::LabelArrow,
                'U',
                [(33u64, 3u64), (32, 4), (200, 1), (7, 2)],
            ),
            (
                Form::ArrowLong,
                Form::LabelArrowPad,
                'R',
                [(600, 3), (700, 4), (300, 2), (254, 2)],
            ),
        ];
        for (plain, labelled, rest_symbol, budgets) in pairs {
            for (budget, weight) in budgets {
                for share in [Share::Zero, Share::Half, Share::All] {
                    for order in [Order::First, Order::Last, Order::Alternating] {
                        let c = order_case(plain, labelled, share, order);
                        let src = generate(&c, budget, weight).unwrap();
                        let got: String = src
                            .text
                            .lines()
                            .skip(1)
                            .map(|line| {
                                if line.contains(" -- ") {
                                    'L'
                                } else if line.len() > 100 {
                                    'U'
                                } else if rest_symbol == 'R' {
                                    'R'
                                } else {
                                    'U'
                                }
                            })
                            .collect();
                        let labelled_budget = match share {
                            Share::Zero => 0,
                            Share::Half => budget / 2,
                            Share::All => budget,
                        };
                        let plain_budget = budget - labelled_budget;
                        let plain_cost = if plain == Form::Arrow { 1 } else { 254 };
                        let (n_l, n_u) = (labelled_budget / weight, plain_budget / plain_cost);
                        let rest = labelled_budget % weight + plain_budget % plain_cost;
                        let mut want = String::new();
                        match order {
                            Order::First => {
                                want.push_str(&"L".repeat(n_l as usize));
                                want.push_str(&"U".repeat(n_u as usize));
                            }
                            Order::Last => {
                                want.push_str(&"U".repeat(n_u as usize));
                                want.push_str(&"L".repeat(n_l as usize));
                            }
                            Order::Alternating => {
                                for i in 0..n_l.max(n_u) {
                                    if i < n_l {
                                        want.push('L');
                                    }
                                    if i < n_u {
                                        want.push('U');
                                    }
                                }
                            }
                        }
                        want.push_str(&rest_symbol.to_string().repeat(rest as usize));
                        assert_eq!(got, want, "{plain:?} {share:?} {order:?} {budget}/{weight}");
                        assert_eq!(src.meta.labelled, n_l);
                        assert_eq!(src.meta.cost, budget);
                    }
                }
            }
        }
        // node groups: the two sides' ids are written, never the product
        let group_text = |shape, form, budget, weight| {
            generate(&case(shape, form, Dir::Lr, Pad::None), budget, weight)
                .unwrap()
                .text
        };
        assert_eq!(
            group_text(Shape::GroupSquare, Form::Arrow, 10, 1),
            "flowchart LR\nn0 & n1 & n2 --> m0 & m1 & m2\nr0 --> r1\n"
        );
        assert_eq!(
            group_text(Shape::GroupWide, Form::Arrow, 10, 1),
            "flowchart LR\nn0 & n1 --> m0 & m1 & m2 & m3 & m4\n"
        );
        let square = generate(
            &case(Shape::GroupSquare, Form::PipeArrow, Dir::Lr, Pad::None),
            20,
            3,
        )
        .unwrap();
        assert!(
            square
                .text
                .starts_with("flowchart LR\nn0 & n1 -->|y| m0 & m1\nr0 --> r1\n")
        );
        assert_eq!((square.meta.arrows, square.meta.labelled), (4 + 8, 4));
        assert_eq!(square.meta.cost, 20);
        let wide = generate(
            &case(Shape::GroupWide, Form::PipeArrow, Dir::Lr, Pad::None),
            20,
            3,
        )
        .unwrap();
        assert!(
            wide.text
                .starts_with("flowchart LR\nn0 & n1 -->|y| m0 & m1 & m2\nr0 --> r1\n")
        );
        assert_eq!((wide.meta.arrows, wide.meta.labelled), (6 + 2, 6));
        // a budget no source can hold is inapplicable at once, with no product-sized allocation
        for shape in Shape::GROUPS.into_iter().chain(Shape::FLOW) {
            for budget in [4_000_000_000u64, u64::MAX / 2, u64::MAX] {
                for form in [Form::Arrow, Form::PipeArrow] {
                    let c = case(shape, form, Dir::Lr, Pad::None);
                    assert!(generate(&c, budget, 1).is_none(), "{shape:?} {budget}");
                    assert!(generate(&c, budget, u64::MAX).is_none() || budget == u64::MAX);
                }
            }
        }
        // counts saturate
        let mut b = Builder::new(u64::MAX);
        b.count(Form::PipeArrow3, u64::MAX);
        b.count(Form::PipeArrow3, u64::MAX);
        assert_eq!(
            (b.meta.arrows, b.meta.labelled, b.meta.cost),
            (u64::MAX, u64::MAX, u64::MAX)
        );
        assert_eq!(Form::PipeArrow3.cost(u64::MAX), u64::MAX);
    }

    #[test]
    fn rdb3_arguments_and_texts_are_closed() {
        let args = |list: &[&str]| -> Option<Mode> {
            parse_args(&list.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        };
        assert_eq!(args(&["scan"]), Some(Mode::Scan));
        assert_eq!(args(&["verify", "300", "4"]), Some(Mode::Verify(300, 4)));
        assert_eq!(
            args(&["verify", "65536", "65536"]),
            Some(Mode::Verify(65536, 65536))
        );
        assert_eq!(args(&["verify", "1", "1"]), Some(Mode::Verify(1, 1)));
        for bad in [
            &[][..],
            &["scan", "x"],
            &["scan", "300"],
            &["Scan"],
            &["verify"],
            &["verify", "300"],
            &["verify", "300", "4", "5"],
            &["verify", "+300", "4"],
            &["verify", "-300", "4"],
            &["verify", "300", "-4"],
            &["verify", " 300", "4"],
            &["verify", "300 ", "4"],
            &["verify", "3 00", "4"],
            &["verify", "65537", "4"],
            &["verify", "300", "65537"],
            &["verify", "99999999999999999999", "4"],
            &["verify", "0", "4"],
            &["verify", "300", "0"],
            &["verify", "", "4"],
            &["verify", "3.5", "4"],
            &["verify", "0x10", "4"],
            &["scan", "verify", "300", "4"],
            &["--help"],
        ] {
            assert_eq!(args(bad), None, "{bad:?}");
        }
        assert_eq!(USAGE, "usage: arrow_bound scan | verify <N> <weight>");
        // the plan line and the header line
        let cases = enumerate().len();
        assert_eq!(
            report_head(cases),
            "plan\t2616\t125568\t251136\nphase\tshape\tstyle\tdirection\tpadding\tarrows\tlabelled\tspan\tweight\tbudget\tcost\tbytes\tstatements\tdepth\trepeat\telapsed_ns\tresult\tsvg_bytes\n"
        );
        assert_eq!(cases as u64 * 16 * 3, 125568);
        assert_eq!(HEADER.split('\t').count(), 18);
        // a row has one column for each header column
        let drawn = Drawn::inapplicable(&Labels::of(&enumerate()[0]), Phase::Ladder, 16, 1);
        assert_eq!(drawn.rows[0].line().split('\t').count(), 18);
        assert_eq!(
            drawn.rows[0].line(),
            "ladder\tchain-one-line\t-->\tLR\tnone\t0\t0\t0\t1\t16\t0\t0\t0\t0\t0\t0\tinapplicable\t0"
        );
        for phase in [
            (Phase::Ladder, "ladder"),
            (Phase::Refine, "refine"),
            (Phase::VerifyLibrary, "verify-library"),
            (Phase::VerifyWrapper, "verify-wrapper"),
            (Phase::Diagnostic, "diagnostic"),
        ] {
            assert_eq!(phase.0.name(), phase.1);
        }
        for res in [
            (Res::Svg, "svg"),
            (Res::Invalid, "invalid"),
            (Res::Failed, "failed"),
            (Res::Unsupported, "unsupported"),
            (Res::Error, "error"),
            (Res::Panic, "panic"),
            (Res::Inapplicable, "inapplicable"),
        ] {
            assert_eq!(res.0.name(), res.1);
        }
        // exit codes and texts
        assert_eq!(Finish::Selected(300, 1).code(), 0);
        assert_eq!(Finish::Selected(300, 1).message(), None);
        assert_eq!(Finish::Under(150, 1).code(), 3);
        assert_eq!(
            Finish::Under(150, 1).message().as_deref(),
            Some("the arrow bound is under 200")
        );
        assert_eq!(
            Finish::NoPositive.message().as_deref(),
            Some("no positive arrow bound passed the measurement")
        );
        assert_eq!(
            Finish::TimedOut.message().as_deref(),
            Some("the arrow bound measurement did not finish in 12 hours")
        );
        assert_eq!(
            Finish::NotVerified(300).message().as_deref(),
            Some("the arrow bound did not verify at 300")
        );
        for finish in [Finish::NoPositive, Finish::TimedOut, Finish::NotVerified(3)] {
            assert_eq!(finish.code(), 1);
        }
        assert_eq!(
            bound_reason(300),
            "the source has over 300 arrows, counting a longer or labelled arrow as more than one"
        );
        assert_eq!(LADDER.len(), 16);
        assert_eq!(MAX_ARG, 65536);
    }
}
