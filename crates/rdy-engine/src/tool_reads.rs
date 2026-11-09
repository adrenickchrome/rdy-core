//! The read and search replies of `rdy_read` and `rdy_search`, built from a read state, a resolved
//! scope and the parameters (rpl38). The MCP tools keep the session, the session view, the cursor
//! cache and brief registration; the adapter calls the same builders, so a reply is one value
//! whichever door asks for it. A reply here carries no transport `reminder`.

pub mod markdown;

use crate::brief::BriefRegistry;
use crate::error::{E_BAD_INPUT, E_INTERNAL, E_LIMIT, E_UNKNOWN_BLOCK};
use crate::search::{DEFAULT_SEARCH_TOKENS, SearchQuery};
use crate::{
    Coverage, RdyError, ReadState, Scope, block_to_yaml, code_lines, compact_summary, proposal_text,
};
use rdy_model::BlockId;
use serde::Serialize;
use serde_json::{Value, json};

/// The largest `max_tokens` any read may ask for.
pub const MAX_TOKENS_LIMIT: usize = 100_000;

/// The longest pattern `rdy_read` accepts, in characters.
pub const MAX_PATTERN: usize = 1024;

/// The most text, in bytes, a `/regex/` read matches against. Every page call runs the pattern
/// over the whole read while it holds the session lock, so the text is capped.
pub const READ_REGEX_HAYSTACK_MAX: usize = 4 * 1024 * 1024;

/// Refuses a `max_tokens` over [`MAX_TOKENS_LIMIT`].
pub fn check_tokens(max_tokens: Option<usize>) -> Result<(), RdyError> {
    match max_tokens {
        Some(n) if n > MAX_TOKENS_LIMIT => Err(RdyError::new(
            E_LIMIT,
            format!("max_tokens {n} is over the limit of {MAX_TOKENS_LIMIT}"),
            format!("ask for at most {MAX_TOKENS_LIMIT} tokens and page through the rest"),
        )),
        _ => Ok(()),
    }
}

/// The scope a tool reads, resolved on the state the tool reads (`read`), so that a view created
/// by a later publish is never accepted here and then read on a state without it. `view` is the
/// session view `s` resolves against, and the scope when `s` is none.
pub fn scope_of(view: &str, read: &ReadState, s: &Option<String>) -> Result<Scope, RdyError> {
    match s {
        // scope1: an unknown view is not found, unless it is this session's own view
        Some(text) => read.resolve_read_scope(text, Some(view)),
        // ses1: a reader session whose view is canon reads canon by default
        None if view == "canon" => Ok(Scope::Canon),
        None => Ok(Scope::View(view.to_string())),
    }
}

/// refv1: the scope and the block id of a read call whose `id` may be `id@version` or a ref line.
/// A pinned version reads under `tag:<version>`; an explicit scope that differs is refused.
pub fn read_target(
    view: &str,
    read: &ReadState,
    id: &str,
    scope: &Option<String>,
) -> Result<(Scope, BlockId), RdyError> {
    let t = rdy_model::reference::parse_target(id)
        .map_err(|m| RdyError::new(E_BAD_INPUT, m, rdy_model::reference::BAD_REF_FIX))?;
    let pinned = t
        .scope_text(scope.as_deref())
        .map_err(|m| RdyError::new(E_BAD_INPUT, m, rdy_model::reference::PIN_CLASH_FIX))?;
    let scope = match pinned {
        Some(text) => read.resolve_read_scope(&text, Some(view))?,
        None => scope_of(view, read, scope)?,
    };
    let bid = read.resolve_id(&scope, &t.id)?;
    Ok((scope, bid))
}

/// What a read keeps: a substring (any case), or a `/regex/`.
pub enum Pattern {
    /// Lowercased.
    Substring(String),
    Regex(regex::Regex),
}

impl Pattern {
    pub fn parse(text: &str) -> Result<Pattern, RdyError> {
        if text.is_empty() {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "pattern is empty",
                "pass a substring, or /regex/",
            ));
        }
        if text.chars().count() > MAX_PATTERN {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("pattern is longer than {MAX_PATTERN} characters"),
                "use a shorter pattern",
            ));
        }
        if text.len() >= 2 && text.starts_with('/') && text.ends_with('/') {
            let inner = &text[1..text.len() - 1];
            return crate::search::bounded_regex(inner, false)
                .map(Pattern::Regex)
                .map_err(|e| {
                    if e.code != E_BAD_INPUT {
                        return e;
                    }
                    let detail = e
                        .message
                        .strip_prefix("query is not a valid pattern: ")
                        .unwrap_or(&e.message);
                    RdyError::new(
                        E_BAD_INPUT,
                        format!("pattern is not a valid regex: {detail}"),
                        "write /regex/ in Rust regex syntax, or a plain substring",
                    )
                });
        }
        Ok(Pattern::Substring(text.to_lowercase()))
    }

    pub fn matches(&self, line: &str) -> bool {
        match self {
            Pattern::Substring(s) => line.to_lowercase().contains(s.as_str()),
            Pattern::Regex(r) => r.is_match(line),
        }
    }
}

/// The tokens `v` costs as JSON (4 bytes a token).
pub fn tokens_of(v: &Value) -> usize {
    serde_json::to_string(v).map(|s| s.len() / 4).unwrap_or(0)
}

/// Cuts the string at `k` until `obj` fits `max` tokens (or the string is empty), and marks it
/// `truncated`; `mark` records how many bytes were cut (it runs before each fit check).
pub fn cut_text(
    obj: &mut serde_json::Map<String, Value>,
    k: &str,
    max: usize,
    mark: impl Fn(&mut serde_json::Map<String, Value>, usize),
) {
    let text = obj[k].as_str().unwrap_or_default().to_string();
    let mut keep = (max * 4).saturating_sub(200).min(text.len());
    loop {
        let cut = (0..=keep)
            .rev()
            .find(|i| text.is_char_boundary(*i))
            .unwrap_or(0);
        obj.insert(k.to_string(), json!(text[..cut]));
        obj.insert("truncated".into(), json!(true));
        mark(obj, text.len() - cut);
        if tokens_of(&Value::Object(obj.clone())) <= max || cut == 0 {
            break;
        }
        keep = cut.saturating_sub((cut / 8).max(1));
    }
}

/// A read's blocks, one entry each in reading order (rpag1), after any pattern (rpat1).
pub struct ReadEntries {
    pub root: BlockId,
    pub format: String,
    pub entries: Vec<String>,
    /// The block of each entry.
    pub ids: Vec<BlockId>,
    pub sep: &'static str,
    pub conflict: Option<Value>,
    pub warnings: Vec<Value>,
    /// The entries kept, when a pattern is given.
    pub matched: Option<usize>,
    /// ab2: the as-built status of each entry's block, when it is a content block.
    pub as_built: Vec<Option<crate::as_built::AsBuilt>>,
    /// ab2: the as-built override warning of each entry's block, when it has one.
    pub as_built_warnings: Vec<Option<Value>>,
}

impl ReadEntries {
    /// The page token: the whole hash of the query (the caller's serializable query) and of every
    /// entry.
    pub fn token(&self, q: &impl Serialize) -> String {
        let mut bytes = b"rdy-read-1\0".to_vec();
        bytes.extend(serde_json::to_vec(q).unwrap_or_default());
        bytes.push(0);
        bytes.extend(self.entries.join("\n\x1e").into_bytes());
        // ab2: a built-only change moves a status but no entry text: the read moved too
        bytes.push(0);
        bytes.extend(serde_json::to_vec(&self.as_built).unwrap_or_default());
        rdy_model::Hash::of(&bytes).to_string()
    }

    /// The bytes entry `i`'s page metadata takes: its `as_built` member and its warning, each
    /// with a comma.
    fn meta_bytes(&self, i: usize) -> usize {
        let status = self.as_built[i].as_ref().map_or(0, |s| {
            json_len(&json!(self.ids[i].to_string())) + 1 + json_len(&json!(s)) + 1
        });
        let warning = self.as_built_warnings[i]
            .as_ref()
            .map_or(0, |w| json_len(w) + 1);
        status + warning
    }
}

fn json_len(v: &Value) -> usize {
    serde_json::to_string(v).map_or(0, |t| t.len())
}

/// rpag1 with ab2's metadata: the end (exclusive) of the page that starts at `from`, the most
/// entries whose join with `sep`, JSON-escaped, plus their metadata bytes (`meta`) is at most
/// `budget_bytes`; never less than one entry while any is left. With no metadata it is
/// the mcp `page_entries`.
fn page_end(
    entries: &[String],
    meta: &[usize],
    from: usize,
    sep: &str,
    budget_bytes: usize,
) -> usize {
    if from >= entries.len() {
        return from;
    }
    let escaped = |t: &str| json_len(&json!(t)).saturating_sub(2);
    let sep_len = escaped(sep);
    let mut total = escaped(&entries[from]) + meta[from];
    let mut end = from + 1;
    while end < entries.len() {
        let next = total + sep_len + escaped(&entries[end]) + meta[end];
        if next > budget_bytes {
            break;
        }
        total = next;
        end += 1;
    }
    end
}

/// One page of a read, from entry `from`: the whole blocks that fit `max_tokens`. A cut read
/// carries `next`; a host that pages registers the read behind it.
pub fn read_page(read: &ReadEntries, token: &str, from: usize, max_tokens: Option<usize>) -> Value {
    let max = max_tokens.unwrap_or(4000);
    let mut out = json!({
        "id": read.root.to_string(),
        "format": read.format,
        "text": "",
    });
    if let Some(c) = read.conflict.clone() {
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
    // ab2: a page's as-built metadata counts against its budget, entry by entry
    if read.as_built.iter().any(Option::is_some) {
        envelope["as_built"] = json!({});
    }
    if read.as_built_warnings.iter().any(Option::is_some) && envelope.get("warnings").is_none() {
        envelope["warnings"] = json!([]);
    }
    let envelope_bytes = serde_json::to_string(&envelope).map_or(0, |e| e.len());
    let meta: Vec<usize> = (0..all).map(|i| read.meta_bytes(i)).collect();
    let end = page_end(
        &read.entries,
        &meta,
        from,
        read.sep,
        (max * 4).saturating_sub(envelope_bytes).max(1),
    );
    out["text"] = json!(read.entries[from..end].join(read.sep));
    // ab2: the as-built status of the content blocks this page shows, and their warnings
    let statuses: serde_json::Map<String, Value> = (from..end)
        .filter_map(|i| {
            let s = read.as_built[i].as_ref()?;
            Some((read.ids[i].to_string(), json!(s)))
        })
        .collect();
    if !statuses.is_empty() {
        out["as_built"] = Value::Object(statuses);
    }
    let warned: Vec<Value> = (from..end)
        .filter_map(|i| read.as_built_warnings[i].clone())
        .take(crate::as_built::MAX_AS_BUILT_WARNINGS)
        .collect();
    if !warned.is_empty() {
        let mut all_warnings = out["warnings"].as_array().cloned().unwrap_or_default();
        all_warnings.extend(warned);
        out["warnings"] = json!(all_warnings);
    }
    if end < read.entries.len() {
        out["truncated"] = json!(true);
        out["omitted"] = json!(read.entries.len() - end);
        out["next"] = json!(format!("{token}:{end}"));
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
pub fn read_entries(
    st_: &ReadState,
    id: &str,
    view: &str,
    scope: &Option<String>,
    format: &str,
    pattern: Option<&str>,
) -> Result<ReadEntries, RdyError> {
    let pattern = pattern.map(Pattern::parse).transpose()?;
    let (target, slug) = crate::section::split_address(id)?;
    let (scope, id) = read_target(view, st_, target, scope)?;
    let suspects: std::collections::BTreeSet<BlockId> = st_
        .suspect_details(&scope)?
        .into_iter()
        .map(|s| s.block)
        .collect();
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
        let section = crate::section::section_by_slug(&m, root, slug)?;
        root = m
            .get(&section)
            .expect("section_by_slug returns a block of m");
    }
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
    let is_regex = matches!(pattern, Some(Pattern::Regex(_)));
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
            // a source read: a stub of removed content reads as its id and the neutral source
            // stub, `Content removed.` and then the line that names its redaction (rdx6, rdx12),
            // before any type's own formatter (which reads fields a stub does not have)
            let markdown = || {
                if b.is_redacted() {
                    return format!("`{}` {}", b.id, crate::source_body(b));
                }
                markdown::block_markdown(b, &m, markdown::doc_depth(&m, b), status.as_deref())
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
        if haystack.get() > READ_REGEX_HAYSTACK_MAX {
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
    // ab2: each entry's as-built status and its override warning, from the one warning function
    let statuses: std::collections::BTreeMap<BlockId, crate::as_built::AsBuilt> =
        st_.as_built(&scope, &ids)?.into_iter().collect();
    let mut as_built = Vec::with_capacity(ids.len());
    let mut as_built_warnings = Vec::with_capacity(ids.len());
    for id in &ids {
        let status = statuses.get(id).cloned();
        let warning = match &status {
            Some(s) => st_
                .as_built_warnings(&scope, &[(id.clone(), s.clone())])?
                .pop()
                .map(|w| {
                    json!({"code": w.code, "block": id.to_string(), "message": w.message, "fix": w.fix})
                }),
            None => None,
        };
        as_built.push(status);
        as_built_warnings.push(warning);
    }
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
        as_built,
        as_built_warnings,
    })
}

/// pshow1: a proposal version reads as its own compact text and the structured `proposal`.
pub fn read_proposal(
    st_: &ReadState,
    id: &str,
    format: Option<&str>,
    max_tokens: Option<usize>,
) -> Result<Value, RdyError> {
    if let Some(f) = format
        && f != "compact"
    {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!("a proposal version reads as compact only, not {f:?}"),
            "omit format, or pass \"compact\"",
        ));
    }
    let show = st_.proposal_show(id)?;
    let v = serde_json::to_value(&show).map_err(|e| {
        RdyError::new(
            E_INTERNAL,
            format!("a proposal did not serialize: {e}"),
            "report this as a bug in rdy",
        )
    })?;
    Ok(budget(
        json!({"id": show.r#ref, "format": "compact", "text": proposal_text(&v), "proposal": v}),
        max_tokens,
    ))
}

pub fn budget(v: Value, max_tokens: Option<usize>) -> Value {
    let max = max_tokens.unwrap_or(4000);
    if tokens_of(&v) <= max {
        return v;
    }
    let mut v = match v {
        Value::Array(a) => json!({ "items": a }),
        other => other,
    };
    let Some(obj) = v.as_object_mut() else {
        return v;
    };
    let key = obj
        .iter()
        .filter_map(|(k, x)| {
            x.as_array().map(|a| {
                (
                    serde_json::to_string(a).map(|s| s.len()).unwrap_or(0),
                    k.clone(),
                )
            })
        })
        .max()
        .map(|(_, k)| k);
    let Some(key) = key else {
        // no list to shorten: cut the longest string instead (e.g. rdy_read's text)
        let longest = obj
            .iter()
            .filter_map(|(k, x)| x.as_str().map(|s| (s.len(), k.clone())))
            .max()
            .map(|(_, k)| k);
        if let Some(k) = longest {
            cut_text(obj, &k, max, |o, n| {
                o.insert("omitted".into(), json!(n));
            });
        }
        return v;
    };
    let full = obj[&key].as_array().cloned().unwrap_or_default();
    let (mut lo, mut hi) = (0usize, full.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        obj.insert(key.clone(), Value::Array(full[..mid].to_vec()));
        obj.insert("truncated".into(), json!(true));
        obj.insert("omitted".into(), json!(full.len() - mid));
        if tokens_of(&Value::Object(obj.clone())) <= max {
            lo = mid
        } else {
            hi = mid - 1
        }
    }
    obj.insert(key, Value::Array(full[..lo].to_vec()));
    obj.insert("truncated".into(), json!(true));
    obj.insert("omitted".into(), json!(full.len() - lo));
    v
}

/// msrch1: the reply of `rdy_search` on `scope`: one page of hits, and with `brief` a briefing over
/// the page's hits and the blocks of the entity sheets, registered in `brief`. The search budget
/// is the caller's `max_tokens`, halved with the briefing when there is one.
pub fn search_reply(
    st_: &ReadState,
    scope: &Scope,
    query: &SearchQuery,
    page: Option<&str>,
    max_tokens: Option<usize>,
    brief: Option<&BriefRegistry>,
) -> Result<Value, RdyError> {
    // With a briefing the caller's budget covers both halves: the hits and the briefing.
    let total = max_tokens.unwrap_or(DEFAULT_SEARCH_TOKENS);
    let (search_tokens, briefing_tokens) = if brief.is_some() {
        (Some(total / 2), Some(total - total / 2))
    } else {
        (max_tokens, None)
    };
    let r = st_.search(scope, query, page, search_tokens)?;
    // The page's hits, and the blocks of each entity sheet: a call whose every hit is held by a
    // sheet still gets a briefing over the sheet's blocks.
    let mut ids: Vec<String> = Vec::new();
    if brief.is_some() {
        let hits = r
            .matches
            .iter()
            .flat_map(|m| std::iter::once(&m.id).chain(m.also.iter().map(|a| &a.id)));
        let sheets = r.entities.iter().flat_map(|e| {
            std::iter::once(&e.id)
                .chain(e.facts.iter().map(|b| &b.id))
                .chain(e.also_described_in.iter().map(|b| &b.id))
        });
        // the order of first sight, de-duplicated in O(1) a step
        let mut seen: std::collections::HashSet<&String> = std::collections::HashSet::new();
        for id in hits.chain(sheets) {
            if seen.insert(id) {
                ids.push(id.clone());
            }
        }
    }
    let briefing = match brief {
        Some(registry) if !ids.is_empty() => {
            // A briefing that fails fails the call: never a token without its briefing.
            let b = st_.brief(scope, &Coverage { ids, path: None }, briefing_tokens)?;
            b.register(registry);
            Some(serialize_briefing(&b)?)
        }
        _ => None,
    };
    let mut v = serde_json::to_value(&r).map_err(|e| {
        RdyError::new(
            E_INTERNAL,
            format!("a search result did not serialize: {e}"),
            "report this as a bug in rdy",
        )
    })?;
    if let (Some(b), Some(o)) = (briefing, v.as_object_mut()) {
        o.insert("briefing".to_string(), b);
    }
    Ok(v)
}

/// A briefing as the reply carries it.
pub fn serialize_briefing<T: Serialize>(b: &T) -> Result<Value, RdyError> {
    serde_json::to_value(b).map_err(|e| {
        RdyError::new(
            E_INTERNAL,
            format!("a briefing did not serialize: {e}"),
            "report this as a bug in rdy",
        )
    })
}
