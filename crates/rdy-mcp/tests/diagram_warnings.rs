//! dgm3: rdy_schema's error rows publish the three native diagram warnings, each with its
//! exact "when" and "fix" text, between W_SITE_PROFILE and E_DUMP.
use rdy_engine::{W_DIAGRAM_FAILED, W_DIAGRAM_INVALID, W_DIAGRAM_UNSUPPORTED};
use rdy_mcp::tools::ERROR_ROWS;

const INVALID: (&str, &str, &str) = (
    "W_DIAGRAM_INVALID",
    "A Mermaid diagram does not parse, or is over one of rdy's size bounds, so the render shows it as its code block. The render never fails for it. The bounds limit the source's bytes, its statements, its nesting depth, the length of one arrow, and its arrows and groups. A longer or labelled arrow counts as more than one. A state note and each boundary between concurrent regions also count as one arrow. A smaller diagram, split into parts, or shorter arrows fix a bound.",
    "correct the diagram source",
);
const UNSUPPORTED: (&str, &str, &str) = (
    "W_DIAGRAM_UNSUPPORTED",
    "A Mermaid diagram whose header keyword names a type that rdy does not draw shows its source as a code block.",
    "use a flowchart, class, sequence or state diagram, or keep it as source",
);
const FAILED: (&str, &str, &str) = (
    "W_DIAGRAM_FAILED",
    "The drawn SVG of a diagram fails rdy's checks, so the render shows its source as a code block.",
    "simplify the diagram, or report it as a bug in rdy, with the diagram's source",
);

#[test]
fn dgm3_warning_rows_are_exact_and_unique() {
    assert_eq!(W_DIAGRAM_INVALID, "W_DIAGRAM_INVALID");
    assert_eq!(W_DIAGRAM_UNSUPPORTED, "W_DIAGRAM_UNSUPPORTED");
    assert_eq!(W_DIAGRAM_FAILED, "W_DIAGRAM_FAILED");
    for want in [INVALID, UNSUPPORTED, FAILED] {
        let found: Vec<&(&str, &str, &str)> = ERROR_ROWS.iter().filter(|r| r.0 == want.0).collect();
        assert_eq!(found.len(), 1, "{} occurs once", want.0);
        assert_eq!(*found[0], want, "{}", want.0);
    }
}

#[test]
fn dgm3_warning_order_keeps_existing_tail() {
    let at = ERROR_ROWS
        .iter()
        .position(|r| r.0 == "W_SITE_PROFILE")
        .expect("W_SITE_PROFILE row");
    let codes: Vec<&str> = ERROR_ROWS[at..at + 5].iter().map(|r| r.0).collect();
    assert_eq!(
        codes,
        [
            "W_SITE_PROFILE",
            "W_DIAGRAM_INVALID",
            "W_DIAGRAM_UNSUPPORTED",
            "W_DIAGRAM_FAILED",
            "E_DUMP"
        ]
    );
}
