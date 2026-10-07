//! The coordination graph of the in-flight state (plan M2d1 Task 5).
use rdy_model::Hash;

/// An intent's display id: `in-` and its stable id's short form.
pub fn intent_label(h: &Hash) -> String {
    format!("in-{}", h.short())
}

/// An alternative set's display id: `as-` and its stable id's short form.
pub fn alt_label(h: &Hash) -> String {
    format!("as-{}", h.short())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cevt1_labels_are_prefixed_short_hashes() {
        let h = Hash::of(b"x");
        assert_eq!(intent_label(&h), format!("in-{}", &h.to_string()[..12]));
        assert_eq!(alt_label(&h), format!("as-{}", &h.to_string()[..12]));
    }
}
