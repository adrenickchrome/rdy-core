//! Line and column of every node in a YAML text, by its path from the document root.
//! serde_yaml_ng reports positions only for syntax errors; import and the linter need them for
//! nodes. saphyr-parser 0.1 reports 1-based lines and 0-based columns; this module returns both 1-based.
use saphyr_parser::{Event, Parser};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Seg {
    Index(usize),
    Key(String),
}

pub(crate) type Positions = BTreeMap<Vec<Seg>, (usize, usize)>;

/// Readers only look up paths of at most this many segments ([Index, Key, Key, Index, Key], a key
/// in a link object); deeper nodes are tracked but not recorded, which keeps hostile deeply
/// nested input linear.
const MAX_DEPTH: usize = 5;

enum Frame {
    Seq(usize),
    Map(Option<String>),
}

/// (line, column) of every node; a mapping entry's position is its key's. A syntax error is
/// returned as (line, column, message).
pub(crate) fn positions(text: &str) -> Result<Positions, (usize, usize, String)> {
    let mut out = Positions::new();
    let mut stack: Vec<Frame> = vec![];
    let mut path: Vec<Seg> = vec![];
    for ev in Parser::new_from_str(text) {
        let (ev, span) = ev.map_err(|e| {
            (
                e.marker().line(),
                e.marker().col() + 1,
                e.info().to_string(),
            )
        })?;
        let pos = (span.start.line(), span.start.col() + 1);
        match ev {
            Event::Scalar(..)
            | Event::SequenceStart(..)
            | Event::MappingStart(..)
            | Event::Alias(..) => {
                if let Some(Frame::Map(key @ None)) = stack.last_mut() {
                    let k = match &ev {
                        Event::Scalar(s, ..) => s.to_string(),
                        _ => String::new(),
                    };
                    *key = Some(k.clone());
                    if path.len() < MAX_DEPTH {
                        let mut p = path.clone();
                        p.push(Seg::Key(k));
                        out.entry(p).or_insert(pos);
                    }
                    continue;
                }
                let nested = match stack.last_mut() {
                    Some(Frame::Map(key)) => {
                        path.push(Seg::Key(key.take().expect("a value follows its key")));
                        true
                    }
                    Some(Frame::Seq(i)) => {
                        path.push(Seg::Index(*i));
                        *i += 1;
                        true
                    }
                    None => false,
                };
                if path.len() <= MAX_DEPTH {
                    out.entry(path.clone()).or_insert(pos);
                }
                match ev {
                    Event::SequenceStart(..) => stack.push(Frame::Seq(0)),
                    Event::MappingStart(..) => stack.push(Frame::Map(None)),
                    _ => {
                        if nested {
                            path.pop();
                        }
                    }
                }
            }
            Event::SequenceEnd | Event::MappingEnd => {
                stack.pop();
                if !stack.is_empty() {
                    path.pop();
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_of_nodes_and_syntax_errors() {
        let t = "- id: d-AAAAA\n  type: doc\n  fields:\n    title: Hello\n    name: bad name\n- id: s-BBBBB\n  parent: d-AAAAA\n";
        let p = positions(t).unwrap();
        let k = |i: usize, keys: &[&str]| {
            let mut v = vec![Seg::Index(i)];
            v.extend(keys.iter().map(|s| Seg::Key(s.to_string())));
            p[&v]
        };
        assert_eq!(k(0, &["id"]), (1, 3));
        assert_eq!(k(0, &["fields"]), (3, 3));
        assert_eq!(k(0, &["fields", "name"]), (5, 5));
        assert_eq!(k(1, &["parent"]), (7, 3));
        assert_eq!(p[&vec![Seg::Index(1)]], (6, 3));
        let (line, col, _) = positions("x: 1\n  y: 2\n").unwrap_err();
        assert_eq!((line, col), (2, 4));
    }

    #[test]
    fn deep_and_wide_input_records_a_bounded_number_of_entries() {
        let t = format!(
            "{}{}{}",
            "[".repeat(200),
            "1,".repeat(100_000),
            "]".repeat(200)
        );
        let p = positions(&t).unwrap();
        assert!(p.len() < 1000, "{} entries", p.len());
    }
}
