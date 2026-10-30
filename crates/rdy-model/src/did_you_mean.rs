//! FW-HD1: the did-you-mean rule for an unknown field: one closed rule (`nearest`) over a closed
//! list of known names. The engine's refusals and the schema's authority policy both read it.

/// The Levenshtein distance between `a` and `b`, over chars.
pub fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != *cb)).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

/// The closed rule: the entry of `known` within 2 edits of `word` (smallest distance first, then
/// the first in `known`); else the first entry where one string is a prefix of the other and the
/// shorter has at least 3 characters; else nothing.
pub fn nearest<'a>(word: &str, known: &[&'a str]) -> Option<&'a str> {
    let close = known
        .iter()
        .enumerate()
        .map(|(i, k)| (edit_distance(word, k), i, *k))
        .filter(|(d, _, _)| *d <= 2)
        .min()
        .map(|(_, _, k)| k);
    close.or_else(|| {
        known.iter().copied().find(|k| {
            let (short, long) = if k.chars().count() <= word.chars().count() {
                (*k, word)
            } else {
                (word, *k)
            };
            short.chars().count() >= 3 && long.starts_with(short)
        })
    })
}
