# Architecture

`localize` is a maintenance toolkit for static HTML sites. Five subcommands:

- **scan** — find remote media URLs in HTML files.
- **apply** — download remote assets and rewrite HTML to use local relative paths.
- **clean** — find and fix broken local links by unwrapping dead `<a>` tags and removing dead resource elements.
- **zap** — remove HTML elements matching a CSS selector whose inner text contains a query string. Dry-run by default, `--apply` to remove.
- **towebp** — replace `.jpg`/`.jpeg`/`.png` URL extensions with `.webp` in `href`, `src`, and `srcset` attributes. Works on both local and remote URLs. Dry-run by default, `--apply` to rewrite.

## Key files

- `src/main.rs` — entry point, calls `cli::run()`.
- `src/cli.rs` — argument parsing (`lexopt`), file discovery (`glob` + `walkdir`), orchestrates scan/apply/clean workflows.
- `src/scanner.rs` — HTML tokenizer (`html5gum`) that finds remote URLs in `<img src>`, `<video src>`, `<meta content>`, `srcset`, inline `style=`, `<style>` blocks. Returns `MediaReference` structs with byte spans.
- `src/downloader.rs` — async HTTP client (`hyper` + `rustls`) that downloads assets into a content-addressed directory: `{assets_dir}/{host}/{sha256[:2]}/{sha256[:8]}-{basename}`. Handles retries, redirects, and 404 marking.
- `src/rewriter.rs` — rewrites HTML files in-place: replaces remote URLs with computed relative paths, renames attributes to `data-broken-*` for permanently-failed URLs.
- `src/clean.rs` — broken local link detection and removal. Two-pass design: first builds an `FxHashSet` of all canonical file hrefs by walking the filesystem once, then scans each HTML file resolving every local link against the set (one hash lookup per link, zero syscalls). Removals are byte-span-based via `html5gum` tokenizer: `<a>` tags are unwrapped (start + end tag removed, inner content preserved), void elements (`<img>`, `<link>`, etc.) are removed entirely, `<script>` elements are removed start-to-end.
- `src/zap.rs` — CSS selector parser and element removal. Supports `tag`, `.class`, `#id`, `[attr]`, and `[attr=value]` selectors (combinable). Matches elements whose inner text contains a query string, then removes them via byte-span replacement.
- `src/towebp.rs` — image extension converter. Scans HTML for URLs ending in `.jpg`/`.jpeg`/`.png` (case-insensitive) in `href`, `src`, and `srcset` attributes (covers `<a>`, `<link>`, `<img>`, `<source>`, `<video>`, `<audio>`, `<track>`, `<script>`, `<embed>`, `<iframe>`, `<object>`). Preserves query strings and fragments during replacement. Dry-run reports each match; `--apply` rewrites extensions to `.webp` in-place.

## Href resolution (clean.rs)

`resolve_href` replicates hyperlink's `push_and_canonicalize` exactly:

1. Strip `?` and `#` from the **raw** (undecoded) href so `%23` (encoded `#`) survives as a literal `#` in filenames.
2. Percent-decode the remaining path.
3. Resolve `..`, `.`, and trailing `index.html`/`index.htm` components relative to the document's canonical href (with `index.html` files contributing their parent directory as the base).

`build_href_set` walks every file under the root and computes its canonical href (stripping `index.html`/`index.htm` to just the directory), stored in an `FxHashSet<String>`. Links are checked with a single `set.contains()`.

Element coverage matches hyperlink's parser: `a[href]`, `area[href]`, `link[href]`, `img[src]/[srcset]`, `script[src]`, `iframe[src]`, `object[data]`.

## Data flow

1. **scan**: discover HTML files → tokenize each for `MediaReference`s → print as text or JSON.
2. **apply**: discover → scan → deduplicate URLs → download assets in parallel (capped by `--jobs`) → rewrite each file as soon as all its URLs finish downloading → verify no remote URLs remain.
3. **clean**: discover HTML files → `build_href_set` (walk all files once) → scan each HTML file in parallel (`tokio` + `spawn_blocking`) → for each local link, resolve href and check the set → print broken links grouped by file (dry-run default) or apply removals (`--force`).
4. **zap**: discover HTML files → parse selector → scan each file for matching elements containing the query → print matches grouped by file (dry-run default) or remove elements in-place (`--apply`).
5. **towebp**: discover HTML files → scan each file for image URLs (`href`/`src`/`srcset`) with `.jpg`/`.jpeg`/`.png` extensions → print matches grouped by file (dry-run default) or rewrite extensions to `.webp` in-place (`--apply`).

## Dependencies

- **HTTP**: `hyper` (HTTP/1.1) + `rustls` with `ring` crypto + `webpki-roots` for TLS.
- **HTML parsing**: `html5gum` tokenizer with span tracking.
- **Async runtime**: `tokio` (multi-threaded).
- **CLI**: `lexopt` for argument parsing.
- **File walking**: `walkdir` + `glob` for pattern filtering.
- **Hashing**: `ring` (SHA-256 for content-addressed asset paths).
- **Maps/sets**: `rustc-hash` for `FxHashMap`/`FxHashSet` (faster than SipHash for small string keys).
- **Concurrency**: `tokio::sync::Semaphore` (limiting parallel downloads/rewrites) and `tokio::sync::Notify` (signaling between download and rewrite tasks).

## Testing

```sh
cargo test
```

Tests cover: scanner (tag/attribute extraction, span correctness, edge cases), rewriter (URL replacement, relative path computation, broken-URL attribute renaming), downloader (asset path determinism, URL encoding, HTML detection), clean (href resolution including percent-encoding and fragment handling, element removal planning, regression for `%23`-in-filename cases).

## Validation

Broken link counts should be validated against [hyperlink](https://github.com/untitaker/hyperlink) on the same root:

```sh
hyperlink ~/Downloads/dfa-localized
cargo run --release -- clean ~/Downloads/dfa-localized
```
