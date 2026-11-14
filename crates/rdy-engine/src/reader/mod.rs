//! What a reader reads, read once for every output: the inline Markdown subset and its plain
//! words ([`inline`]), the field-aware reader text ([`text`]), and what a reader calls a block
//! ([`labels`]). The site (rdy-render) writes HTML from these rules, and the publication review
//! reads its labels and excerpts from them.
pub mod inline;
pub mod labels;
pub mod text;
