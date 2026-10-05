//! Build-time syntax highlighting (Task 9 fills it).
use crate::html::escape;

/// The code as HTML: escaped text until Task 9 colours it.
pub fn html(_lang: &str, code: &str) -> String {
    escape(code)
}

/// The language as a reader names it, for the code label.
// Task 8 labels code blocks with it
#[allow(dead_code)]
pub fn human_name(lang: &str) -> String {
    let mut chars = lang.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
