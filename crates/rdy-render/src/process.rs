//! The stateless render process (hub i-N0G2N, r-B0ZYE, r-7GS51, r-VHP43, n-DFWNV; D-KDFZ3,
//! D-48NYM, D-0VCQ5, D-QYCRB, D-5X0ZK, D-5CVWB): one site or Markdown artifact from one render
//! request, with no store, project, config or working directory. `rdy render-stream` runs
//! [`run`] over its standard input and output; a caller that renders through that process
//! writes its request with [`write_request_prelude`] and reads the result with
//! [`read_artifact`], so both sides share these encoders and their checks.
//!
//! # Input
//!
//! A u32 little-endian length, then that many bytes of compact UTF-8 JSON
//! `{profile,format,render_inputs}`, then exactly one complete version-1 tag history stream and
//! the end of the input. `profile` is a full profile id or `implicit:org`
//! (`publication::check_stable_selector`: a key is no stable selector); `format` is `site`,
//! `markdown` or `techdocs`; `render_inputs` is the strict [`RenderInputs`] object. The prelude
//! is checked whole (at most 8,400,896 bytes, depth 33, 200,010 values, 1,048,576 bytes per
//! string, no duplicate key) and then read strictly, before any stream byte is read. The stream
//! is read through [`RenderSource::read`]: copied to one anonymous scratch file and checked
//! whole, end of input included.
//!
//! # Output
//!
//! Only once the whole input has passed and the render is complete: a u32 little-endian length,
//! then that many bytes of compact UTF-8 JSON `{manifest_recipe:1,manifest_hash,manifest,
//! warnings}`, then each file's raw bytes in manifest path order with no separator, then the
//! end. Each manifest size delimits its file. The metadata is at most 268,435,456 bytes, depth
//! 32, 1,000,000 values and 1,048,576 bytes per string. A refusal writes no byte; an output
//! failure may leave a partial packet, which the refusal invalidates.
//!
//! `site` builds with [`build_publication`]. `markdown` and `techdocs` build the same
//! preparation into the print and hand its files to [`emit_files`] with the generated header,
//! laid out plainly or for TechDocs; their manifest has the same schema and recipe and no
//! notice offset. The process writes no publication record.
use crate::print::{self, Layout, md::emit_files};
use crate::{
    Budget, PublicationArtifact, bad_manifest, build_publication, publication, without_parameters,
};
use rdy_engine::did_you_mean::unknown_field_error_in;
use rdy_engine::hub_http::{JsonLimits, check_json};
use rdy_engine::publication::{PublicationSource, check_stable_selector, resolve_source};
use rdy_engine::publication_manifest::{
    ArtifactHasher, MANIFEST_RECIPE, Manifest, ManifestEntry, NOTICE_SLOT, READ_BUFFER_BYTES,
    allocation_failed, artifact_mismatch, artifact_unreadable, hex_hash, manifest_hash,
};
use rdy_engine::render_inputs::{RenderInputs, validate_inputs};
use rdy_engine::render_source::RenderSource;
use rdy_engine::{E_BAD_INPUT, E_IO, E_LIMIT, Materialized, RdyError};
use rdy_model::Hash;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{BufWriter, ErrorKind, Read, Write};

/// Most bytes of the prelude's JSON: the render inputs' 8,388,608 bytes and room for the
/// selector, the format and the keys.
pub const MAX_PRELUDE_BYTES: usize = 8_400_896;
/// Most bytes of the output's metadata JSON.
pub const MAX_METADATA_BYTES: usize = 268_435_456;
/// Most bytes of one error line on standard error, its line feed included.
pub const MAX_ERROR_BYTES: usize = 8_388_608;

/// The prelude's bounds: the render inputs' own, one level and ten values more.
const PRELUDE_LIMITS: JsonLimits = JsonLimits {
    bytes: MAX_PRELUDE_BYTES,
    depth: 33,
    values: 200_010,
    scalar_bytes: 1_048_576,
};

/// The output metadata's bounds.
const METADATA_LIMITS: JsonLimits = JsonLimits {
    bytes: MAX_METADATA_BYTES,
    depth: 32,
    values: 1_000_000,
    scalar_bytes: 1_048_576,
};

/// What an unknown prelude key's refusal names as its context.
const CONTEXT: &str = "render process input";

/// What a render request builds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RenderFormat {
    /// The static site of the profile ([`build_publication`]).
    Site,
    /// The print as a plain Markdown tree, with the generated header.
    Markdown,
    /// The print as a TechDocs tree (`docs/` and `mkdocs.yml`), with the generated header.
    Techdocs,
}

/// What [`run`] wrote: the artifact's files, their bytes (the sum of the manifest sizes) and
/// the manifest hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessSummary {
    pub files: u32,
    pub bytes: u64,
    pub manifest_hash: Hash,
}

/// The prelude as it is written: its keys in this order.
#[derive(Serialize)]
struct PreludeOut<'a> {
    profile: &'a str,
    format: RenderFormat,
    render_inputs: &'a RenderInputs,
}

/// The prelude as it is read: every key, no other.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Prelude {
    profile: String,
    format: RenderFormat,
    render_inputs: RenderInputs,
}

/// The output metadata as it is written.
#[derive(Serialize)]
struct MetadataOut<'a> {
    manifest_recipe: u32,
    #[serde(serialize_with = "hex_hash::serialize")]
    manifest_hash: Hash,
    manifest: &'a Manifest,
    warnings: &'a [String],
}

/// The output metadata as it is read: every key, no other.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    manifest_recipe: u32,
    #[serde(with = "hex_hash")]
    manifest_hash: Hash,
    manifest: Manifest,
    warnings: Vec<String>,
}

/// `E_BAD_INPUT`: a prelude that is no prelude, or input bytes that frame none. No input byte
/// enters the text.
fn bad_input() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "render process input is invalid",
        "send a render prelude followed by one complete tag stream",
    )
}

/// `E_BAD_INPUT`: an output packet that is no packet this renderer writes, or whose bytes do not
/// match its manifest.
fn bad_output() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "render process output is invalid",
        "run the render with the same rdy version again",
    )
}

/// `E_IO`: the request could not be read.
fn input_unreadable() -> RdyError {
    RdyError::new(
        E_IO,
        "render process input could not be read",
        "send the render request again",
    )
}

/// `E_IO`: the packet could not be written; what was written is invalid.
fn output_failed() -> RdyError {
    RdyError::new(
        E_IO,
        "render process output could not be written",
        "read the render's output until it ends, then render again",
    )
}

/// `E_IO`: a request could not be written to the render process.
fn request_failed() -> RdyError {
    RdyError::new(
        E_IO,
        "render process input could not be written",
        "check that the render process is running, then render again",
    )
}

/// `E_LIMIT`: an error line over [`MAX_ERROR_BYTES`], which replaces it.
fn error_too_long() -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!("render error exceeds {MAX_ERROR_BYTES} bytes"),
        "reduce the input and render again",
    )
}

/// Renders the request on `input` and writes its packet to `out` (see the module docs). The
/// prelude is checked first, then the stream whole; the profile resolves once in the tag's
/// state, its output check runs before any output is collected, and the packet is written only
/// after the render is complete. Nothing is written on a refusal. No process is spawned and no
/// file is made but the render source's anonymous scratch file.
pub fn run<R: Read, W: Write>(mut input: R, out: W) -> Result<ProcessSummary, RdyError> {
    let prelude = read_prelude(&mut input)?;
    let artifact = render(prelude, input)?;
    let summary = check_packet(&artifact)?;
    write_packet(&artifact, out)?;
    Ok(summary)
}

/// Writes the prelude of a request for `profile` (a full profile id or `implicit:org`) in
/// `format` with `inputs` to `out`: the u32 little-endian length, then the compact JSON. The
/// selector and the inputs are checked as [`run`] checks them, and the prelude within the same
/// bounds, before any byte is written. The caller writes the tag history stream after it.
pub fn write_request_prelude<W: Write>(
    profile: &str,
    format: RenderFormat,
    inputs: &RenderInputs,
    out: &mut W,
) -> Result<(), RdyError> {
    check_stable_selector(profile)?;
    validate_inputs(inputs)?;
    let json = serde_json::to_vec(&PreludeOut {
        profile,
        format,
        render_inputs: inputs,
    })
    .map_err(|_| bad_input())?;
    // what the process would refuse is never sent
    parse_prelude(&json)?;
    let len = u32::try_from(json.len()).map_err(|_| bad_input())?;
    out.write_all(&len.to_le_bytes())
        .and_then(|()| out.write_all(&json))
        .map_err(|_| request_failed())
}

/// Reads one packet [`run`] wrote from `input`, to its end, and checks it before it returns
/// anything: the metadata's bounds and keys, the recipe, the manifest and its stated hash, the
/// path order, each file's declared size and hash and its notice slot, at most 32,000 files and
/// 2 GiB together (the manifest's limits), and the end of input right after the last file. Each
/// file is read through a 65,536-byte buffer for exactly its declared length, so a file never
/// takes a byte of the next one. A packet that fails is refused with no partial result.
pub fn read_artifact<R: Read>(mut input: R) -> Result<PublicationArtifact, RdyError> {
    let mut len = [0u8; 4];
    read_exactly(&mut input, &mut len, bad_output, artifact_unreadable)?;
    let len = usize::try_from(u32::from_le_bytes(len)).map_err(|_| bad_output())?;
    if len > MAX_METADATA_BYTES {
        return Err(bad_output());
    }
    let json = read_vec(&mut input, len, bad_output, artifact_unreadable)?;
    check_json(&json, METADATA_LIMITS).map_err(|_| bad_output())?;
    let meta: Metadata = serde_json::from_slice(&json).map_err(|_| bad_output())?;
    drop(json);
    let computed = manifest_hash(&meta.manifest, meta.manifest_recipe)?;
    if computed != meta.manifest_hash || !path_ordered(&meta.manifest) {
        return Err(bad_output());
    }
    let mut buf: Vec<u8> = Vec::new();
    buf.try_reserve_exact(READ_BUFFER_BYTES)
        .map_err(|_| allocation_failed())?;
    buf.resize(READ_BUFFER_BYTES, 0);
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for e in &meta.manifest.files {
        let bytes = read_file(&mut input, e, &mut buf)?;
        files.insert(e.path.clone(), bytes);
    }
    // the end of input, tested only after the last file
    loop {
        match input.read(&mut buf[..1]) {
            Ok(0) => break,
            Ok(_) => return Err(bad_output()),
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return Err(artifact_unreadable()),
        }
    }
    Ok(PublicationArtifact {
        files,
        manifest: meta.manifest,
        manifest_hash: computed,
        warnings: meta.warnings,
    })
}

/// The line `rdy render-stream` writes on standard error for `e`: one compact JSON object
/// `{code,message,fix,issues}` (the CLI's error fields, `issues` always present) and a line
/// feed. A line over [`MAX_ERROR_BYTES`] is replaced by the `E_LIMIT` line that says so.
pub fn error_line(e: &RdyError) -> Vec<u8> {
    let line = |e: &RdyError| {
        let mut out = serde_json::to_vec(&ErrorLine {
            code: e.code,
            message: &e.message,
            fix: &e.fix,
            issues: &e.issues,
        })
        .unwrap_or_default();
        out.push(b'\n');
        out
    };
    let out = line(e);
    if out.len() <= MAX_ERROR_BYTES {
        out
    } else {
        line(&error_too_long())
    }
}

/// One error as the process's standard error writes it.
#[derive(Serialize)]
struct ErrorLine<'a, I: Serialize> {
    code: &'a str,
    message: &'a str,
    fix: &'a str,
    issues: &'a [I],
}

// ---------------------------------------------------------------------------------------------
// input

/// Reads the prelude from `input`: the length, bounded before anything is allocated, then the
/// JSON, checked ([`parse_prelude`]). No stream byte is read.
fn read_prelude(input: &mut impl Read) -> Result<Prelude, RdyError> {
    let mut len = [0u8; 4];
    read_exactly(input, &mut len, bad_input, input_unreadable)?;
    let len = usize::try_from(u32::from_le_bytes(len)).map_err(|_| bad_input())?;
    if len > MAX_PRELUDE_BYTES {
        return Err(bad_input());
    }
    let json = read_vec(input, len, bad_input, input_unreadable)?;
    parse_prelude(&json)
}

/// The prelude of `json`: within the prelude's bounds, every key known (an unknown one is named
/// by its path, with the nearest key), the selector stable and the render inputs this
/// renderer's. Any other malformed prelude is the fixed refusal.
fn parse_prelude(json: &[u8]) -> Result<Prelude, RdyError> {
    check_json(json, PRELUDE_LIMITS).map_err(|_| bad_input())?;
    let value: serde_json::Value = serde_json::from_slice(json).map_err(|_| bad_input())?;
    let prelude = Prelude::deserialize(&value).map_err(|e| {
        unknown_field_error_in(CONTEXT, &e.to_string(), &value).unwrap_or_else(bad_input)
    })?;
    check_stable_selector(&prelude.profile)?;
    validate_inputs(&prelude.render_inputs)?;
    Ok(prelude)
}

/// The artifact of `prelude` over the stream `input`. The stream is read and checked whole
/// before the profile resolves; the tag's stored name and its redaction mark are the only
/// history the output shows.
fn render(prelude: Prelude, input: impl Read) -> Result<PublicationArtifact, RdyError> {
    let Prelude {
        profile,
        format,
        render_inputs,
    } = prelude;
    let source = RenderSource::read(input, render_inputs)?;
    let m = source.materialized();
    let tag = source.facts().root.name.as_str();
    let redacted = source.tag_redacted();
    let inputs = source.inputs();
    match format {
        RenderFormat::Site => build_publication(&source, m, tag, redacted, &profile, inputs),
        RenderFormat::Markdown => {
            build_markdown(&source, m, tag, redacted, &profile, inputs, Layout::Plain)
        }
        RenderFormat::Techdocs => build_markdown(
            &source,
            m,
            tag,
            redacted,
            &profile,
            inputs,
            Layout::Techdocs,
        ),
    }
}

/// The Markdown artifact of `selector` in `m`, laid out by `layout`: the profile resolves once
/// and its output check runs first, then the publication's preparation is printed and the
/// print's files are collected from [`emit_files`] with the generated header, within the
/// manifest's limits as they arrive. Every manifest entry has no notice offset; a Markdown file
/// is `text/markdown`, `mkdocs.yml` is `application/yaml`, and a copied figure keeps its
/// source's media type without parameters.
fn build_markdown(
    source: &dyn PublicationSource,
    m: &Materialized,
    tag_name: &str,
    tag_redacted: bool,
    selector: &str,
    inputs: &RenderInputs,
    layout: Layout,
) -> Result<PublicationArtifact, RdyError> {
    publication::check_inputs(source, inputs)?;
    let resolved = resolve_source(source, m, selector)?;
    resolved.require_output()?;
    let prepared = publication::prepare_resolved(source, resolved, tag_name, tag_redacted, inputs)?;
    let p = print::build_from(prepared)?;
    let budget = Budget::publication();
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    emit_files(&p, layout, true, |path, data| {
        budget.reserve(&files, path, data.len())?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(data.len())
            .map_err(|_| allocation_failed())?;
        bytes.extend_from_slice(data);
        match files.insert(path.to_string(), bytes) {
            Some(_) => Err(bad_manifest()),
            None => Ok(()),
        }
    })?;
    // a copied figure by the path the tree gives it
    let base = match layout {
        Layout::Plain => "",
        Layout::Mkdocs | Layout::Techdocs => "docs/",
    };
    let assets: BTreeMap<String, &str> = p
        .blobs
        .values()
        .map(|b| (format!("{base}{}", b.file), b.media_type.as_str()))
        .collect();
    let mut entries = Vec::with_capacity(files.len());
    for (path, bytes) in &files {
        let media_type = match assets.get(path) {
            Some(m) => without_parameters(m),
            None if path.ends_with(".md") => "text/markdown",
            None if path == "mkdocs.yml" => "application/yaml",
            None => return Err(bad_manifest()),
        };
        entries.push(ManifestEntry {
            path: path.clone(),
            hash: Hash::of(bytes),
            media_type: media_type.to_string(),
            size: u64::try_from(bytes.len()).map_err(|_| bad_manifest())?,
            notice_offset: None,
        });
    }
    let manifest = Manifest { files: entries };
    let manifest_hash = manifest_hash(&manifest, MANIFEST_RECIPE)?;
    Ok(PublicationArtifact {
        files,
        manifest,
        manifest_hash,
        warnings: p.warnings,
    })
}

// ---------------------------------------------------------------------------------------------
// output

/// Whether the manifest lists its files in strictly increasing path byte order.
fn path_ordered(m: &Manifest) -> bool {
    m.files
        .windows(2)
        .all(|w| w[0].path.as_bytes() < w[1].path.as_bytes())
}

/// Checks that `a` is one packet: its manifest hash, its files one for one with the manifest in
/// path order, each of its declared size. Its summary.
fn check_packet(a: &PublicationArtifact) -> Result<ProcessSummary, RdyError> {
    if manifest_hash(&a.manifest, MANIFEST_RECIPE)? != a.manifest_hash
        || !path_ordered(&a.manifest)
        || a.files.len() != a.manifest.files.len()
    {
        return Err(artifact_mismatch());
    }
    let mut bytes: u64 = 0;
    for e in &a.manifest.files {
        let file = a.files.get(&e.path).ok_or_else(artifact_mismatch)?;
        if u64::try_from(file.len()).ok() != Some(e.size) {
            return Err(artifact_mismatch());
        }
        bytes = bytes.checked_add(e.size).ok_or_else(artifact_mismatch)?;
    }
    Ok(ProcessSummary {
        files: u32::try_from(a.manifest.files.len()).map_err(|_| artifact_mismatch())?,
        bytes,
        manifest_hash: a.manifest_hash,
    })
}

/// Writes the packet of `a` ([`check_packet`] passed) to `out`: the metadata, bounded as a
/// reader bounds it, then each file in manifest order.
fn write_packet<W: Write>(a: &PublicationArtifact, out: W) -> Result<(), RdyError> {
    let json = serde_json::to_vec(&MetadataOut {
        manifest_recipe: MANIFEST_RECIPE,
        manifest_hash: a.manifest_hash,
        manifest: &a.manifest,
        warnings: &a.warnings,
    })
    .map_err(|_| bad_output())?;
    // what a reader would refuse is never written
    check_json(&json, METADATA_LIMITS).map_err(|_| bad_output())?;
    let len = u32::try_from(json.len()).map_err(|_| bad_output())?;
    let mut out = BufWriter::with_capacity(READ_BUFFER_BYTES, out);
    let mut put = |bytes: &[u8]| out.write_all(bytes).map_err(|_| output_failed());
    put(&len.to_le_bytes())?;
    put(&json)?;
    drop(json);
    for e in &a.manifest.files {
        put(a.files.get(&e.path).ok_or_else(artifact_mismatch)?)?;
    }
    out.flush().map_err(|_| output_failed())
}

/// Reads one file of `e`'s declared size from `input` through `buf`, hashing it as it arrives;
/// refused when the input ends first, or the bytes or the notice slot are not the manifest's.
fn read_file(
    input: &mut impl Read,
    e: &ManifestEntry,
    buf: &mut [u8],
) -> Result<Vec<u8>, RdyError> {
    let size = usize::try_from(e.size).map_err(|_| allocation_failed())?;
    let mut bytes: Vec<u8> = Vec::new();
    bytes
        .try_reserve_exact(size)
        .map_err(|_| allocation_failed())?;
    let mut hasher = ArtifactHasher::new(e.size)?;
    while bytes.len() < size {
        let want = (size - bytes.len()).min(buf.len());
        let n = match input.read(&mut buf[..want]) {
            Ok(0) => return Err(bad_output()),
            Ok(n) => n,
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return Err(artifact_unreadable()),
        };
        hasher.update(&buf[..n]).map_err(|_| bad_output())?;
        bytes.extend_from_slice(&buf[..n]);
    }
    if hasher.finish().map_err(|_| bad_output())? != e.hash {
        return Err(bad_output());
    }
    if let Some(at) = e.notice_offset {
        // the manifest's check put the slot inside the file
        let at = usize::try_from(at).map_err(|_| bad_output())?;
        if bytes.get(at..at + NOTICE_SLOT.len()) != Some(NOTICE_SLOT) {
            return Err(bad_output());
        }
    }
    Ok(bytes)
}

/// Fills `buf` from `input`; `short` when the input ends first, `failed` on a read error.
fn read_exactly(
    input: &mut impl Read,
    buf: &mut [u8],
    short: fn() -> RdyError,
    failed: fn() -> RdyError,
) -> Result<(), RdyError> {
    let mut at = 0;
    while at < buf.len() {
        match input.read(&mut buf[at..]) {
            Ok(0) => return Err(short()),
            Ok(n) => at += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return Err(failed()),
        }
    }
    Ok(())
}

/// Exactly `len` bytes of `input`, reserved before they are read (the caller bounded `len`).
fn read_vec(
    input: &mut impl Read,
    len: usize,
    short: fn() -> RdyError,
    failed: fn() -> RdyError,
) -> Result<Vec<u8>, RdyError> {
    let mut out: Vec<u8> = Vec::new();
    out.try_reserve_exact(len)
        .map_err(|_| allocation_failed())?;
    out.resize(len, 0);
    read_exactly(input, &mut out, short, failed)?;
    Ok(out)
}
