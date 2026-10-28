//! The theme's files, served under `assets/`, and the blob extensions of figure files.

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
    theme!("js/menu.js"),
    theme!("js/sidebar.js"),
];

/// The text of the inline script that sets the theme before the page paints.
pub const INIT_JS: &str = include_str!("../theme/rdy-standard/js/init.js");

/// The bytes of `assets/app.js`: the same for every page and every site.
pub fn app_js() -> Vec<u8> {
    let mut out = vec![];
    for part in APP_JS {
        out.extend_from_slice(part);
        if !part.is_empty() && !part.ends_with(b"\n") {
            out.push(b'\n');
        }
    }
    out
}

/// The theme's files by their site-relative paths: the stylesheet, the fonts and the scripts.
/// Diagrams are drawn when the site is built, so the theme has no diagram library.
pub fn files() -> Vec<(&'static str, Vec<u8>)> {
    let mut out: Vec<(&'static str, Vec<u8>)> = THEME
        .iter()
        .map(|(name, bytes)| (*name, bytes.to_vec()))
        .collect();
    out.push(("assets/app.js", app_js()));
    out
}

/// The extension of a figure file whose media type has no extension of its own.
pub(crate) const OTHER_EXTENSION: &str = "bin";

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
        _ => OTHER_EXTENSION,
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
        // no diagram library: diagrams are drawn when the site is built
        let files = files();
        assert_eq!(files[0].0, "assets/site.css");
        assert!(
            files
                .iter()
                .all(|(path, bytes)| !path.starts_with("assets/js/")
                    && !String::from_utf8_lossy(bytes).contains("mermaid"))
        );
    }
}
