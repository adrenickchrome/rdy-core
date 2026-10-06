//! The theme's files, written under `assets/`, and the blob extensions of figure files.
use rdy_engine::{E_IO, RdyError};
use std::path::Path;

macro_rules! theme {
    ($p:literal) => {
        include_bytes!(concat!("../theme/rdy-standard/", $p))
    };
}

/// The theme files that are copied as they are, in the order they are written.
pub const THEME: &[(&str, &[u8])] = &[
    ("assets/site.css", theme!("site.css")),
    ("assets/fonts/onest.woff2", theme!("fonts/onest.woff2")),
    (
        "assets/fonts/geist-mono.woff2",
        theme!("fonts/geist-mono.woff2"),
    ),
];

/// The scripts `assets/app.js` is made of, in order. A constant cannot concatenate included
/// files, so [`app_js`] does it when the site is written.
const APP_JS: &[&[u8]] = &[
    theme!("js/init.js"),
    theme!("js/theme.js"),
    theme!("js/toc.js"),
    theme!("js/search.js"),
    theme!("js/copy.js"),
    theme!("js/code.js"),
    theme!("js/review.js"),
];

/// The diagram library, written only for a site that has a diagram.
pub const MERMAID: (&str, &[u8]) = (
    "assets/js/mermaid.min.js",
    theme!("vendor/mermaid/mermaid.min.js"),
);

/// What starts the diagram library, appended to `app.js` only for a site that has a diagram.
pub const MERMAID_INIT: &[u8] = theme!("js/mermaid-init.js");

/// The text of the inline script that sets the theme before the page paints.
pub const INIT_JS: &str = include_str!("../theme/rdy-standard/js/init.js");

/// The bytes of `assets/app.js`.
pub fn app_js(has_mermaid: bool) -> Vec<u8> {
    let mut out = vec![];
    let init: &[&[u8]] = if has_mermaid { &[MERMAID_INIT] } else { &[] };
    for part in APP_JS.iter().chain(init) {
        out.extend_from_slice(part);
        if !part.is_empty() && !part.ends_with(b"\n") {
            out.push(b'\n');
        }
    }
    out
}

fn io(path: &Path, e: std::io::Error) -> RdyError {
    RdyError::new(
        E_IO,
        format!("cannot write {}: {e}", path.display()),
        "check the output directory",
    )
}

fn put(out: &Path, name: &str, bytes: &[u8]) -> Result<(), RdyError> {
    let path = out.join(name);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
    }
    std::fs::write(&path, bytes).map_err(|e| io(&path, e))
}

/// Writes the theme under `out`: the stylesheet, the scripts and the fonts, and the diagram
/// library when `has_mermaid`. Returns how many files it wrote.
pub fn write(out: &Path, has_mermaid: bool) -> Result<usize, RdyError> {
    let mut n = 0;
    for (name, bytes) in THEME {
        put(out, name, bytes)?;
        n += 1;
    }
    put(out, "assets/app.js", &app_js(has_mermaid))?;
    n += 1;
    if has_mermaid {
        put(out, MERMAID.0, MERMAID.1)?;
        n += 1;
    }
    Ok(n)
}

/// The file extension of a figure's blob by its media type.
pub fn extension(media_type: &str) -> &'static str {
    match media_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/svg+xml" => "svg",
        "image/webp" => "webp",
        "application/pdf" => "pdf",
        "text/plain" => "txt",
        _ => "bin",
    }
}

/// The site-relative path of a figure's blob.
pub fn blob_path(hash: &str, media_type: &str) -> String {
    format!("assets/{hash}.{}", extension(media_type))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blob_lives_at_its_hash_with_the_extension_of_its_media_type() {
        assert_eq!(blob_path("ab12", "image/png"), "assets/ab12.png");
        assert_eq!(blob_path("ab12", "image/svg+xml"), "assets/ab12.svg");
        assert_eq!(
            blob_path("ab12", "application/x-unknown"),
            "assets/ab12.bin"
        );
    }

    #[test]
    fn the_theme_lists_the_stylesheet_first_and_the_diagram_library_is_separate() {
        assert_eq!(THEME[0].0, "assets/site.css");
        assert_eq!(MERMAID.0, "assets/js/mermaid.min.js");
    }
}
