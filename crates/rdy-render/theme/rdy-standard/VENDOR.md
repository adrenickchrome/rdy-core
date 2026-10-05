# Vendored third-party files

Every third-party file in the theme is listed here with its origin, version, licence and the
exact command that produced it. Nothing else third-party is in the theme. No Google Fonts, no
CDN, no `@import` of a URL: the site makes no request to another origin.

## Status

All vendored files are fetched and checksum-verified. Fonts are referenced by the stylesheet's
`@font-face` rules (`fonts/onest.woff2`, `fonts/geist-mono.woff2`) with the full fallback stacks
(`--font-text`, `--font-mono`) behind them.

## fonts/onest.woff2: fetched

- Font: Onest (variable, weight axis 100 to 900 kept), SIL Open Font License 1.1.
- Origin: the Google Fonts repository, pinned at commit `9710da1eacb3be272583c3224dcb70f9da6eadbb`.
- Source TTF: `https://raw.githubusercontent.com/google/fonts/9710da1eacb3be272583c3224dcb70f9da6eadbb/ofl/onest/Onest%5Bwght%5D.ttf`
  - 193,056 bytes; `git hash-object` = `477cb31027450d80e486a53119641bf87dde3d4c` (matched); sha256 `966c5c29b4755da84b6854d5c21dd4eaa2420225d0e9874de602de176d4a9f31`.
- Licence file: `fonts/OFL-Onest.txt`, from `https://raw.githubusercontent.com/google/fonts/9710da1eacb3be272583c3224dcb70f9da6eadbb/ofl/onest/OFL.txt`
  - 4,384 bytes; `git hash-object` = `b6f4f91487900bb8f00ae991724121f1d59e30d9` (matched); sha256 `071195d8806e226faeee60259c28ca67b458227af5195a73f5cfcab06e3003bc`.
- Conversion: the TTF is converted to woff2 with fonttools 4.60.1 and brotli 1.1.0 in a scratch venv (thrown away afterwards). The whole font is kept
  (no subsetting), so every glyph and the full `wght` axis are present.
- Result: `onest.woff2` is 83,960 bytes, sha256 `71156cc8de4851cc9da5a8fc3d44389bdbc5780bd3a634f9fa5e0dafd3b667c3`.
- Commands (run from the repo root; `$SCRATCH` is any empty scratch directory; a blob mismatch stops the fetch):

  ```
  curl -fL -o "$SCRATCH/Onest[wght].ttf" 'https://raw.githubusercontent.com/google/fonts/9710da1eacb3be272583c3224dcb70f9da6eadbb/ofl/onest/Onest%5Bwght%5D.ttf'
  curl -fL -o crates/rdy-render/theme/rdy-standard/fonts/OFL-Onest.txt 'https://raw.githubusercontent.com/google/fonts/9710da1eacb3be272583c3224dcb70f9da6eadbb/ofl/onest/OFL.txt'
  git hash-object "$SCRATCH/Onest[wght].ttf" crates/rdy-render/theme/rdy-standard/fonts/OFL-Onest.txt   # 477cb310…, b6f4f914…
  python3 -m venv "$SCRATCH/venv"
  "$SCRATCH/venv/bin/pip" install fonttools==4.60.1 brotli==1.1.0
  "$SCRATCH/venv/bin/python" -c "import sys; from fontTools.ttLib import TTFont; f = TTFont(sys.argv[1]); f.flavor = 'woff2'; f.save(sys.argv[2])" "$SCRATCH/Onest[wght].ttf" crates/rdy-render/theme/rdy-standard/fonts/onest.woff2
  shasum -a 256 crates/rdy-render/theme/rdy-standard/fonts/onest.woff2
  ```

- The Fontsource fallback (`@fontsource-variable/onest@5.3.1`, Latin only) was not needed and is not used.

## fonts/geist-mono.woff2: fetched

- Font: Geist Mono (variable), SIL Open Font License 1.1, used as shipped with no conversion.
- Origin: the `geist` npm package, version 1.7.2, from jsDelivr.
- Source: `https://cdn.jsdelivr.net/npm/geist@1.7.2/dist/fonts/geist-mono/GeistMono-Variable.woff2`
  - 71,368 bytes; sha256 `fba8f577f38a2bbcbe818efa6348dd58f36303a10b8737c42fefad275be563ab` (matched).
- Licence file: `fonts/OFL-GeistMono.txt`, from `https://cdn.jsdelivr.net/npm/geist@1.7.2/LICENSE.txt`
  - 4,368 bytes; sha256 `930853ee1daa68554d9e35c8a9175affb74f699fad9a5da6ee5ebe76379d9137` (matched).
- Commands (a mismatch of either checksum stops the fetch):

  ```
  curl -fL -o crates/rdy-render/theme/rdy-standard/fonts/geist-mono.woff2 'https://cdn.jsdelivr.net/npm/geist@1.7.2/dist/fonts/geist-mono/GeistMono-Variable.woff2'
  curl -fL -o crates/rdy-render/theme/rdy-standard/fonts/OFL-GeistMono.txt 'https://cdn.jsdelivr.net/npm/geist@1.7.2/LICENSE.txt'
  shasum -a 256 crates/rdy-render/theme/rdy-standard/fonts/geist-mono.woff2 crates/rdy-render/theme/rdy-standard/fonts/OFL-GeistMono.txt
  ```

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
