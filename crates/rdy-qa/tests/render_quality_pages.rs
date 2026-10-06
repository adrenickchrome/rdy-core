//! The visible-text goldens of the review set's nine pages (gold1). Each golden is the page's
//! runs, one per line: `zone`, `L` for a label or `-`, then the text, tab-separated. Write them
//! with `RDY_QA_BLESS=1 cargo test -p rdy-qa --release --test render_quality_pages`.
use rdy_qa::{golden, site::*};
use std::path::{Path, PathBuf};

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/site")
}

/// The golden text of a page: one run per line.
fn render_runs(html: &str) -> String {
    visible_runs(html)
        .iter()
        .map(|r| {
            format!(
                "{}\t{}\t{}\n",
                format!("{:?}", r.zone).to_lowercase(),
                if r.label { "L" } else { "-" },
                r.text
            )
        })
        .collect()
}

/// Masks the date a landing was made on: the project's landings are stamped with the wall clock,
/// so the home page's "<date> · Accepted by …" line changes every day (not a rendering change).
fn mask_landing_date(runs: &str) -> String {
    let re = regex::Regex::new(
        r"(?m)^(chrome\t-\t)\d{1,2} (?:January|February|March|April|May|June|July|August|September|October|November|December) \d{4}( · )",
    )
    .expect("landing date pattern");
    re.replace_all(runs, "${1}<landing date>${2}").into_owned()
}

/// Compares the visible text of `page` in `site` with `golden/site/<name>.txt` (written
/// instead when `RDY_QA_BLESS` is set).
fn check(site: &Path, scope: &str, page: &str) {
    let name = format!(
        "{scope}--{}",
        page.trim_end_matches(".html").replace('/', "--")
    );
    let file = golden_dir().join(format!("{name}.txt"));
    let now = mask_landing_date(&render_runs(&golden::html(site, page)));
    if std::env::var_os("RDY_QA_BLESS").is_some() {
        std::fs::create_dir_all(golden_dir()).unwrap();
        std::fs::write(&file, &now).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&file).unwrap_or_else(|_| {
        panic!(
            "{name}: no golden at {}; write it with RDY_QA_BLESS=1",
            file.display()
        )
    });
    let want = mask_landing_date(&want);
    if want != now {
        let (a, b): (Vec<&str>, Vec<&str>) = (want.lines().collect(), now.lines().collect());
        let at = (0..a.len().max(b.len()))
            .find(|&i| a.get(i) != b.get(i))
            .unwrap_or(0);
        panic!(
            "{name}: the visible text changed from the golden at line {}\n  golden: {:?}\n  now:    {:?}\n(bless with RDY_QA_BLESS=1 if the change is intended)",
            at + 1,
            a.get(at),
            b.get(at)
        );
    }
}

#[test]
fn gold1_a_golden_line_is_zone_label_flag_and_text() {
    let html = "<main><h1>Guards</h1><div data-rdy=\"content\"><p>A  guard\nis <code>x</code> evaluated.</p></div><details data-rdy=\"agents\"><summary>For agents</summary></details></main>";
    assert_eq!(
        render_runs(html),
        "chrome\tL\tGuards\ncontent\t-\tA guard is\ncontent\t-\tevaluated.\nagents\tL\tFor agents\n"
    );
}

#[test]
fn gold1_the_landing_date_is_masked_in_the_home_pages_status_line() {
    let runs = "chrome\t-\t6 October 2026 · Accepted by the Steering Council\nchrome\t-\t12 May 2026 · Dated\n";
    assert_eq!(
        mask_landing_date(runs),
        "chrome\t-\t<landing date> · Accepted by the Steering Council\nchrome\t-\t<landing date> · Dated\n"
    );
}

#[test]
fn gold1_the_canon_pages_match_their_text_goldens() {
    let (_d, _r, _ids, out) = golden::site_canon();
    for page in [
        "index.html",
        "specification/index.html",
        "specification/patterns.html",
        "tutorial/adventure.html",
        "glossary.html",
        "specification/evaluation.html",
        "questions.html",
    ] {
        check(out.path(), "canon", page);
    }
}

#[test]
fn gold1_the_frozen_page_matches_its_text_golden() {
    let (_d, _r, _ids, out) = golden::site_frozen();
    check(out.path(), "frozen", "specification/patterns.html");
}

#[test]
fn gold1_the_view_page_matches_its_text_golden() {
    let (_d, _r, _ids, out) = golden::site_view();
    check(out.path(), "view", "tutorial/adventure.html");
}
