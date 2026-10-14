//! The `render_quality` area of the rdy-qa suite: one test binary (`cargo qa --only render_quality`).
//! One `mod` line per test file in this directory; a file without its line never runs
//! (`tlay1_` checks). Keep the list sorted.
mod render_quality;
mod render_quality_css;
mod render_quality_fixture;
mod render_quality_leaks;
mod render_quality_live;
mod render_quality_pages;
mod render_quality_visual;
