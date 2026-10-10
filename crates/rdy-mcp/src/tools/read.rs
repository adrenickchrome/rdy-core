//! rdy_read: one block, a page of entries or a proposal.

use super::*;
use crate::read_page;
use rdy_engine::*;
use rdy_model::BlockId;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadParams {
    /// A block id, key or path (wallets/transactions); <doc>#<heading-slug> reads one section of a doc; id@version or a ref line [id@version] … reads under that version; P-<id>@<n> reads a proposal version. Required unless page is given.
    pub id: Option<String>,
    /// "canon", a view name (an existing view, or your session's), "tag:<version>", "landing:<id>" or "view:<name>". Defaults to the session view.
    pub scope: Option<String>,
    /// compact (default), markdown or yaml.
    pub format: Option<String>,
    /// Only the blocks whose text matches: a substring (any case), or /regex/. A code block shows only its matching lines; in compact format, a block matched only past its compact line (a prose body, table rows, diagram source) shows that line alone. The result's matched counts the blocks kept.
    pub pattern: Option<String>,
    /// Continue a cut read: its next cursor. Alone (scope and max_tokens allowed).
    pub page: Option<String>,
    pub max_tokens: Option<usize>,
}

/// A read's blocks, one entry each in reading order (rpag1), after any pattern (rpat1).
struct ReadEntries {
    root: BlockId,
    format: String,
    entries: Vec<String>,
    /// The block of each entry.
    ids: Vec<BlockId>,
    sep: &'static str,
    conflict: Option<Value>,
    warnings: Vec<Value>,
    /// The entries kept, when a pattern is given.
    matched: Option<usize>,
}

impl ReadEntries {
    /// The page token: the whole hash of the query and of every entry.
    fn token(&self, q: &read_page::ReadQuery) -> String {
        let mut bytes = b"rdy-read-1\0".to_vec();
        bytes.extend(serde_json::to_vec(q).unwrap_or_default());
        bytes.push(0);
        bytes.extend(self.entries.join("\n\x1e").into_bytes());
        rdy_model::Hash::of(&bytes).to_string()
    }
}

pub fn rdy_read(st: &mut McpState, p: ReadParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    session(st)?;
    if let Some(cursor) = &p.page {
        if p.id.is_some() || p.format.is_some() || p.pattern.is_some() {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "page continues a read: pass it alone",
                "pass only page, with scope or max_tokens if you like",
            ));
        }
        let bad_cursor = || {
            RdyError::new(
                E_BAD_INPUT,
                "page is not a cursor from rdy_read's next",
                "pass the next value of a cut rdy_read unchanged",
            )
        };
        let (token, from) = read_page::parse_cursor(cursor).ok_or_else(bad_cursor)?;
        let q = st.reads.get(&token).ok_or_else(|| {
            RdyError::new(
                E_PAGE_STALE,
                "the read behind this page is unknown (the session restarted, or it was evicted): read again",
                PAGE_STALE_FIX,
            )
        })?;
        // a target that is gone, or a view that closed, is a read that moved
        let read = read_entries(st, &q).map_err(|e| {
            if [E_UNKNOWN_BLOCK, E_NOT_FOUND, E_VIEW_CLOSED].contains(&e.code) {
                RdyError::new(
                    E_PAGE_STALE,
                    "the read moved since its first page: read again",
                    PAGE_STALE_FIX,
                )
            } else {
                e
            }
        })?;
        if read.token(&q) != token {
            return Err(RdyError::new(
                E_PAGE_STALE,
                "the read moved since its first page: read again",
                PAGE_STALE_FIX,
            ));
        }
        // a cursor rdy issued always points inside its read
        if from >= read.entries.len() {
            return Err(bad_cursor());
        }
        return Ok(read_page_of(st, q, read, &token, from, p.max_tokens));
    }
    let Some(id) = p.id.as_deref() else {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "pass id, or page to continue a read",
            "id is a block id, key, path, <doc>#<heading-slug> or P-<id>@<n>",
        ));
    };
    if is_proposal_ref(id) {
        if p.pattern.is_some() {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "pattern filters block reads, not a proposal version",
                "omit pattern",
            ));
        }
        return read_proposal(st, id, &p);
    }
    let q = read_page::ReadQuery {
        id: id.to_string(),
        scope: p.scope.clone(),
        view: st.view.clone(),
        format: p.format.clone().unwrap_or_else(|| "compact".into()),
        pattern: p.pattern.clone(),
    };
    let read = read_entries(st, &q)?;
    let token = read.token(&q);
    Ok(read_page_of(st, q, read, &token, 0, p.max_tokens))
}

/// One page of a read, from entry `from`: the whole blocks that fit `max_tokens`. A cut read
/// carries `next` and is registered so that a page call can continue it.
fn read_page_of(
    st: &mut McpState,
    q: read_page::ReadQuery,
    read: ReadEntries,
    token: &str,
    from: usize,
    max_tokens: Option<usize>,
) -> Value {
    let max = max_tokens.unwrap_or(4000);
    let mut out = json!({
        "id": read.root.to_string(),
        "format": read.format,
        "text": "",
    });
    if let Some(c) = read.conflict {
        out["conflict"] = c;
    }
    if !read.warnings.is_empty() {
        out["warnings"] = json!(read.warnings);
    }
    if let Some(n) = read.matched {
        out["matched"] = json!(n);
    }
    // the page's text gets what the envelope leaves: measured with the longest next, truncated
    // and omitted a page of this read can carry, so the whole page fits max_tokens
    let all = read.entries.len();
    let mut envelope = out.clone();
    envelope["truncated"] = json!(true);
    envelope["omitted"] = json!(all);
    envelope["next"] = json!(format!("{token}:{all}"));
    let envelope_bytes = serde_json::to_string(&envelope).map_or(0, |e| e.len());
    let end = read_page::page_entries(
        &read.entries,
        from,
        read.sep,
        (max * 4).saturating_sub(envelope_bytes).max(1),
    );
    out["text"] = json!(read.entries[from..end].join(read.sep));
    if end < read.entries.len() {
        out["truncated"] = json!(true);
        out["omitted"] = json!(read.entries.len() - end);
        out["next"] = json!(format!("{token}:{end}"));
        st.reads.register(token, q);
    }
    // one block larger than the whole budget is shown alone and cut as budget cuts text; cut
    // names the block and the bytes it lost (omitted always counts entries)
    if end == from + 1
        && tokens_of(&out) > max
        && let Some(obj) = out.as_object_mut()
    {
        let id = read.ids[from].to_string();
        cut_text(obj, "text", max, |o, n| {
            o.insert("cut".into(), json!({"id": id, "omitted_bytes": n}));
        });
    }
    out
}

/// Whether any line of a block's markdown is kept. A code block's body lines are matched
/// elsewhere (`code_lines`), so only its id line, blank line and opening fence (with the
/// language) are scanned here.
fn markdown_hit(md: &str, b: &rdy_model::Block, keeps: &impl Fn(&str) -> bool) -> bool {
    let n = if b.block_type == "code" {
        3
    } else {
        usize::MAX
    };
    md.lines().take(n).any(keeps)
}

/// The entries of a read as `q` gives it, on the state the engine publishes now.
fn read_entries(st: &McpState, q: &read_page::ReadQuery) -> Result<ReadEntries, RdyError> {
    let pattern = q
        .pattern
        .as_deref()
        .map(read_page::Pattern::parse)
        .transpose()?;
    // a ref line's trailing words may hold a #; ids, keys, paths, versions and slugs never do
    let (target, slug) = match q.id.split_once('#') {
        Some((t, s)) if !q.id.trim_start().starts_with('[') => (t, Some(s)),
        _ => (q.id.as_str(), None),
    };
    if slug == Some("") {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "a # needs a heading slug after it",
            "write <doc>#<heading-slug>, e.g. guide#set-up",
        ));
    }
    let st_ = st.engine.read();
    let (scope, id) = read_target(&q.view, &st_, target, &q.scope)?;
    let suspects: std::collections::BTreeSet<BlockId> =
        st_.suspects(&scope)?.into_iter().map(|s| s.block).collect();
    // the scope's recorded conflicts by block, for the marker on each conflicted block's line
    let conflicts: std::collections::BTreeMap<BlockId, String> = st_
        .conflicts(&scope)?
        .iter()
        .map(|c| (c.block.clone(), c.id.short()))
        .collect();
    let m = st_.materialized(&scope)?;
    let g = st_.graph(&scope)?;
    let mut root = m.get(&id).ok_or_else(|| {
        RdyError::new(
            E_UNKNOWN_BLOCK,
            format!("no block {id}"),
            "list docs with rdy_overview",
        )
    })?;
    if let Some(slug) = slug {
        let section = read_page::section_by_slug(&m, root, slug)?;
        root = m
            .get(&section)
            .expect("section_by_slug returns a block of m");
    }
    let format = q.format.as_str();
    if !["compact", "markdown", "yaml"].contains(&format) {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!("unknown format {format:?}"),
            "use compact, markdown or yaml",
        ));
    }
    // compact lines mark the justification of the change that last wrote each block; a damaged
    // producer index (status and fsck report it) costs the markers and a warning, not the read
    let mut warnings = vec![];
    let justified: std::collections::BTreeMap<BlockId, String> = if format == "compact" {
        let mut ids = vec![];
        let mut walk = vec![root];
        while let Some(b) = walk.pop() {
            ids.push(b.id.clone());
            walk.extend(m.children(Some(&b.id)));
        }
        match st_.last_changes(&scope, &ids) {
            Ok(last) => last
                .into_iter()
                .filter_map(|(id, e)| e.justification.map(|j| (id, j.label())))
                .collect(),
            Err(e) => {
                warnings.push(json!({"code": e.code, "message": e.message, "fix": e.fix}));
                Default::default()
            }
        }
    } else {
        Default::default()
    };
    // a /regex/ read is refused, before any regex runs, when the text it would match exceeds
    // READ_REGEX_HAYSTACK_MAX. A first pass counts every candidate line (no short-circuit) and
    // matches nothing; the second pass matches.
    let is_regex = matches!(pattern, Some(read_page::Pattern::Regex(_)));
    let haystack = std::cell::Cell::new(0usize);
    let walk = |counting: bool| -> (Vec<String>, Vec<BlockId>) {
        let keeps = |line: &str| {
            let Some(p) = pattern.as_ref() else {
                return true;
            };
            if counting {
                haystack.set(haystack.get().saturating_add(line.len() + 1));
                return false;
            }
            p.matches(line)
        };
        let (mut entries, mut ids) = (vec![], vec![]);
        let mut stack = vec![(root, 0usize)];
        while let Some((b, depth)) = stack.pop() {
            let status = g.status(&b.id).map(|s| {
                serde_json::to_value(s)
                    .ok()
                    .and_then(|v| v["status"].as_str().map(String::from))
                    .unwrap_or_default()
            });
            let markdown = || {
                crate::markdown::block_markdown(
                    b,
                    &m,
                    crate::markdown::doc_depth(&m, b),
                    status.as_deref(),
                )
            };
            let entry = match format {
                "yaml" | "markdown" => {
                    let text = if format == "yaml" {
                        block_to_yaml(b)
                    } else {
                        markdown()
                    };
                    (pattern.is_none() || text.lines().any(keeps)).then_some(text)
                }
                _ => {
                    let pad = "  ".repeat(depth);
                    // the pattern sees the head without its indent, as it sees a code line
                    let mut head = format!("{} {}: {}", b.id, b.block_type, compact_summary(b));
                    if suspects.contains(&b.id) {
                        head.push_str(" [suspect]");
                    }
                    if let Some(c) = conflicts.get(&b.id) {
                        head.push_str(&format!(" [conflict {c}]"));
                    }
                    if let Some(s) = &status {
                        head.push_str(&format!(" [{s}]"));
                    }
                    if let Some(j) = justified.get(&b.id) {
                        head.push_str(&format!(" [{j}]"));
                    }
                    // a pattern matches the head line, a code line's own text, or any line of the
                    // block's whole text (as markdown gives it: a prose body past its first sentence,
                    // table rows, diagram source); a kept code block shows only the lines that match.
                    // A code block's lines are matched once, so markdown does not repeat them.
                    let body = code_lines(b);
                    let matching: Vec<&String> = body.iter().filter(|l| keeps(l)).collect();
                    let hit = pattern.is_none()
                        || keeps(&head)
                        || !matching.is_empty()
                        || markdown_hit(&markdown(), b, &keeps);
                    hit.then(|| {
                        let shown: Vec<&String> = if pattern.is_some() {
                            matching
                        } else {
                            body.iter().collect()
                        };
                        let mut lines = vec![format!("{pad}{head}")];
                        for l in shown {
                            lines.push(if l.is_empty() {
                                format!("{pad}  |")
                            } else {
                                format!("{pad}  | {l}")
                            });
                        }
                        lines.join("\n")
                    })
                }
            };
            if let Some(e) = entry {
                entries.push(e);
                ids.push(b.id.clone());
            }
            for c in m.children(Some(&b.id)).into_iter().rev() {
                stack.push((c, depth + 1));
            }
        }
        (entries, ids)
    };
    if is_regex {
        walk(true);
        if haystack.get() > read_page::READ_REGEX_HAYSTACK_MAX {
            return Err(RdyError::new(
                E_LIMIT,
                format!(
                    "the read is too large to match a regex against ({} bytes; the limit is 4 MiB)",
                    haystack.get()
                ),
                "narrow the read with #section, or use a plain-text pattern",
            ));
        }
    }
    let (entries, ids) = walk(false);
    let sep = match format {
        "yaml" => "---\n",
        "markdown" => "\n\n",
        _ => "\n",
    };
    Ok(ReadEntries {
        conflict: st_.conflict_info(&scope, &root.id)?.map(|c| json!(c)),
        root: root.id.clone(),
        format: format.to_string(),
        matched: pattern.as_ref().map(|_| entries.len()),
        entries,
        ids,
        sep,
        warnings,
    })
}

/// pshow1: a proposal version reads as its own compact text and the structured `proposal`.
fn read_proposal(st: &mut McpState, id: &str, p: &ReadParams) -> Result<Value, RdyError> {
    if let Some(f) = p.format.as_deref()
        && f != "compact"
    {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!("a proposal version reads as compact only, not {f:?}"),
            "omit format, or pass \"compact\"",
        ));
    }
    let show = st.engine.read().proposal_show(id)?;
    let v = serde_json::to_value(&show).map_err(|e| {
        RdyError::new(
            E_INTERNAL,
            format!("a proposal did not serialize: {e}"),
            "report this as a bug in rdy",
        )
    })?;
    Ok(budget(
        json!({"id": show.r#ref, "format": "compact", "text": proposal_text(&v), "proposal": v}),
        p.max_tokens,
    ))
}
