//! The `schema` area of the rdy-qa suite: one test binary (`cargo qa --only schema`).
//! One `mod` line per test file in this directory; a file without its line never runs
//! (`tlay1_` checks). Keep the list sorted.
mod schema;
mod schema_areas;
mod schema_card_words;
mod schema_check_fields;
mod schema_class;
mod schema_packs;
mod schema_rules;
