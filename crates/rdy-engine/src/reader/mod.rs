//! What a reader reads, read once for every output: the inline Markdown subset and its plain
//! words ([`inline`]), the field-aware reader text ([`text`]), what a reader calls a block
//! ([`labels`]), the site's pages, reading order, numbers and sections ([`shape`]), and what one
//! state changes against another as a reader sees it ([`review`]). The site (rdy-render) writes
//! HTML from these rules, and the publication review reads its labels, pages, counts and
//! excerpts from them.
pub mod inline;
pub mod labels;
pub mod review;
pub mod shape;
pub mod text;
