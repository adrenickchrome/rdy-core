//! The `render` area of the rdy-qa suite: one test binary (`cargo qa --only render`).
//! One `mod` line per test file in this directory; a file without its line never runs
//! (`tlay1_` checks). Keep the list sorted.
mod render;
mod render_diagrams;
mod render_dx2;
mod render_llms;
mod render_markdown;
mod render_mermaid;
mod render_nav;
mod render_no_coverage;
mod render_plain;
mod render_presentational;
mod render_ref;
mod render_review;
mod render_review_nav;
mod render_review_page;
mod render_review_toggle;
mod render_search;
mod render_site;
mod render_theme;
mod render_toolbar;
mod render_trail;
mod render_types;
mod render_urls;
mod render_values;
mod render_version;
