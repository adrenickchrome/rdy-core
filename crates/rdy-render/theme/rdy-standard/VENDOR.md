# Vendored third-party files

Every third-party file in the theme is listed here with its origin, version, licence and the
exact command that produced it. Nothing else third-party is in the theme. No Google Fonts, no
CDN, no `@import` of a URL: the site makes no request to another origin.

## Status

The two font files under `fonts/` are placeholders (empty files) until the founder approves the
download (Task 5b). Until then the stylesheet's `@font-face` rules point at them and the full
fallback stacks (`--font-text`, `--font-mono`) render the site. Entries marked **pending fetch**
have the pinned source and expected checksums recorded; the size and BLAKE3 of the result are
filled in when Task 5b fetches and checks them.

## fonts/onest.woff2: pending fetch

- Font: Onest (variable, weight 100 to 900), SIL Open Font License 1.1.
- Origin: the Google Fonts repository, pinned at commit `9710da1eacb3be272583c3224dcb70f9da6eadbb`.
- Source TTF: `https://raw.githubusercontent.com/google/fonts/9710da1eacb3be272583c3224dcb70f9da6eadbb/ofl/onest/Onest%5Bwght%5D.ttf`
  - 193,056 bytes; `git hash-object` = `477cb31027450d80e486a53119641bf87dde3d4c`. A mismatch stops the fetch.
- Licence file: `fonts/OFL-Onest.txt`, from `https://raw.githubusercontent.com/google/fonts/9710da1eacb3be272583c3224dcb70f9da6eadbb/ofl/onest/OFL.txt`
  - 4,384 bytes; `git hash-object` = `b6f4f91487900bb8f00ae991724121f1d59e30d9`.
- Commands (the scratch directory is `$SCRATCH`; the venv is thrown away afterwards):

  ```
  curl -fL -o "$SCRATCH/Onest.ttf" 'https://raw.githubusercontent.com/google/fonts/9710da1eacb3be272583c3224dcb70f9da6eadbb/ofl/onest/Onest%5Bwght%5D.ttf'
  curl -fL -o crates/rdy-render/theme/rdy-standard/fonts/OFL-Onest.txt 'https://raw.githubusercontent.com/google/fonts/9710da1eacb3be272583c3224dcb70f9da6eadbb/ofl/onest/OFL.txt'
  python3 -m venv "$SCRATCH/venv"
  "$SCRATCH/venv/bin/pip" install fonttools==4.60.1 brotli==1.1.0
  "$SCRATCH/venv/bin/pyftsubset" "$SCRATCH/Onest.ttf" --flavor=woff2 \
    --unicodes="U+0000-00FF,U+0100-024F,U+0370-03FF,U+1E00-1EFF,U+2000-206F,U+20A0-20CF,U+2122,U+2190-21FF,U+2200-22FF,U+25A0-25FF,U+FB00-FB06" \
    --layout-features='*' \
    --output-file=crates/rdy-render/theme/rdy-standard/fonts/onest.woff2
  ```

- Expected size: under 120 KB. Result: pending fetch (bytes and BLAKE3).
- Fallback if PyPI is unreachable: `https://cdn.jsdelivr.net/npm/@fontsource-variable/onest@5.3.1/files/onest-latin-wght-normal.woff2`
  (33,760 bytes, sha256 `052df74533250c2e0ca0c4bdd32de594e107d99c1648e63820ef9c6142067828`). It covers Latin
  only, without Latin Extended; if it is used, this entry says so.

## fonts/geist-mono.woff2: pending fetch

- Font: Geist Mono (variable), SIL Open Font License 1.1, used as shipped with no conversion.
- Origin: the `geist` npm package, version 1.7.2, from jsDelivr.
- Source: `https://cdn.jsdelivr.net/npm/geist@1.7.2/dist/fonts/geist-mono/GeistMono-Variable.woff2`
  - 71,368 bytes; sha256 `fba8f577f38a2bbcbe818efa6348dd58f36303a10b8737c42fefad275be563ab`. A mismatch stops the fetch.
- Licence file: `fonts/OFL-GeistMono.txt`, from `https://cdn.jsdelivr.net/npm/geist@1.7.2/LICENSE.txt`
  - 4,368 bytes; sha256 `930853ee1daa68554d9e35c8a9175affb74f699fad9a5da6ee5ebe76379d9137`.
- Commands:

  ```
  curl -fL -o crates/rdy-render/theme/rdy-standard/fonts/geist-mono.woff2 'https://cdn.jsdelivr.net/npm/geist@1.7.2/dist/fonts/geist-mono/GeistMono-Variable.woff2'
  curl -fL -o crates/rdy-render/theme/rdy-standard/fonts/OFL-GeistMono.txt 'https://cdn.jsdelivr.net/npm/geist@1.7.2/LICENSE.txt'
  shasum -a 256 crates/rdy-render/theme/rdy-standard/fonts/geist-mono.woff2 crates/rdy-render/theme/rdy-standard/fonts/OFL-GeistMono.txt
  ```

- Expected size: under 200 KB. Result: pending fetch (BLAKE3 of the file).

## vendor/mermaid/mermaid.min.js and vendor/mermaid/LICENSE: fetched

- Library: Mermaid 11.17.2, MIT licence. Written to `assets/js/mermaid.min.js` only for a site that has a Mermaid diagram.
- Origin: the npm package `mermaid@11.17.2` (tarball `https://registry.npmjs.org/mermaid/-/mermaid-11.17.2.tgz`, integrity
  `sha512-V6K3C8EBdEsPFZXSKMJe6ppQOENxuHARr9GvHX4hh47lAbhMRD9qf4oEK7LoaRQxULMa80/qt5gHO73aCleBBg==`). `dist/mermaid.min.js` and `LICENSE` are
  byte-identical in the tarball and on jsDelivr, which is where the pinned fetch commands point.
- `mermaid.min.js`: 3,572,661 bytes, sha256 `581ed7d74bd9048d0e3a91363927d72ef22942d7722546b27f7cc29e35390eb8`.
- `LICENSE`: 1,089 bytes, sha256 `ec9fb67dcb25eccc416ed56e1aab819222c805a2a4bfe4cb19e7556bf2ffde80`.
- It is an esbuild IIFE that ends `globalThis["mermaid"] = ….default`, so `window.mermaid` exists once the file has run (it is not a UMD module). The
  file contains URLs inside its licence comments; `js/mermaid-init.js` and `app.js` contain none.
- Commands (a mismatch of either checksum stops the fetch):

  ```
  curl -fL -o crates/rdy-render/theme/rdy-standard/vendor/mermaid/mermaid.min.js 'https://cdn.jsdelivr.net/npm/mermaid@11.17.2/dist/mermaid.min.js'
  curl -fL -o crates/rdy-render/theme/rdy-standard/vendor/mermaid/LICENSE 'https://cdn.jsdelivr.net/npm/mermaid@11.17.2/LICENSE'
  shasum -a 256 crates/rdy-render/theme/rdy-standard/vendor/mermaid/mermaid.min.js crates/rdy-render/theme/rdy-standard/vendor/mermaid/LICENSE
  ```

- How the files got into this checkout: no network fetch was made. Both files were copied from an existing local `mermaid@11.17.2` package
  (`node_modules/mermaid/dist/mermaid.min.js` and `node_modules/mermaid/LICENSE`) after their size and sha256 matched the values above.
