# Architecture

`localize` is a maintenance toolkit for static HTML sites. Five subcommands:

- **scan** — find remote media URLs and broken local media URLs in HTML files. Outputs uniform `kind: ./file:line:col  url` lines. Remote URLs are prefixed `remote-url:`, broken local URLs `broken-local-url:`. Valid local URLs are not printed. Supports `--json` for structured output.
- **apply** — download remote assets and rewrite HTML to use local relative paths.
- **clean** — find and fix broken local links by unwrapping dead `<a>` tags and removing dead resource elements.
- **zap** — remove HTML elements matching a CSS selector whose inner text contains a query string. Dry-run by default, `--apply` to remove.
- **towebp** — replace `.jpg`/`.jpeg`/`.png` URL extensions with `.webp` in `href`, `src`, and `srcset` attributes. Works on both local and remote URLs. Dry-run by default, `--apply` to rewrite.

## Key files

- `src/main.rs` — entry point, calls `cli::run()`. Conditionally wires `alloc::Counter` as global allocator behind `count-alloc` feature.
- `src/alloc.rs` — counting global allocator, gated behind `cargo build --features count-alloc`. Prints heap stats (allocation count, bytes, deallocations) on exit. For profiling only — adds measurable overhead.
- `src/cli.rs` — argument parsing (`lexopt`), file discovery (`glob` + `walkdir` for other commands; `jwalk` for `discover_and_index`), orchestrates all workflows. Contains `discover_and_index` (single walk building both the HTML file list and canonical href set via parallel `jwalk`), `scan_all` (parallel HTML tokenization via `tokio`), `print_human`/`print_json` (unified output).
- `src/scanner.rs` — HTML tokenizer (`html5gum`) that finds URLs in `<img src>`, `<video src>`, `<audio src>`, `<source src/srcset>`, `<track src>`, `<script src>`, `<a href>`, `<link href>`, `<object data>`, `<meta content>` (og:image / twitter:image), `srcset` attributes, inline `style=`, and `<style>` blocks. Checks local URL existence inline via `FxHashSet` lookups (zero per-URL syscalls) — only broken local URLs are captured. CSS `url()` references are remote-only (local CSS references skipped for performance). Returns `MediaReference` structs with byte spans, 1-based line:col positions (computed in O(log n) via binary search on precomputed line starts), and a `broken` flag.
- `src/downloader.rs` — async HTTP client (`hyper` + `rustls`) that downloads assets into a content-addressed directory: `{assets_dir}/{host}/{sha256[:2]}/{sha256[:8]}-{basename}`. Handles retries, redirects, and 404 marking.
- `src/rewriter.rs` — rewrites HTML files in-place: replaces remote URLs with computed relative paths, renames attributes to `data-broken-*` for permanently-failed URLs.
- `src/clean.rs` — broken local link detection and removal. Two-pass design: first builds an `FxHashSet` of all canonical file hrefs by walking the filesystem once, then scans each HTML file resolving every local link against the set (one hash lookup per link, zero syscalls). Removals are byte-span-based via `html5gum` tokenizer: `<a>` tags are unwrapped (start + end tag removed, inner content preserved), void elements (`<img>`, `<link>`, etc.) are removed entirely, `<script>` elements are removed start-to-end.
- `src/zap.rs` — CSS selector parser and element removal. Supports `tag`, `.class`, `#id`, `[attr]`, and `[attr=value]` selectors (combinable). Matches elements whose inner text contains a query string, then removes them via byte-span replacement.
- `src/towebp.rs` — image extension converter. Scans HTML for URLs ending in `.jpg`/`.jpeg`/`.png` (case-insensitive) in `href`, `src`, and `srcset` attributes. Preserves query strings and fragments during replacement. Dry-run reports each match; `--apply` rewrites extensions to `.webp` in-place.

## Href resolution (clean.rs)

`resolve_href` replicates hyperlink's `push_and_canonicalize` exactly. Also used by `scanner.rs` for resolving local URLs during existence checks, and by `cli.rs` for `cmd_clean`.

1. Strip `?` and `#` from the **raw** (undecoded) href so `%23` (encoded `#`) survives as a literal `#` in filenames.
2. Percent-decode the remaining path.
3. Resolve `..`, `.`, and trailing `index.html`/`index.htm` components relative to the document's canonical href (with `index.html` files contributing their parent directory as the base).

`build_href_set` walks every file under the root and computes its canonical href (stripping `index.html`/`index.htm` to just the directory), stored in an `FxHashSet<String>`. Links are checked with a single `set.contains()`.

Element coverage matches hyperlink's parser: `a[href]`, `area[href]`, `link[href]`, `img[src]/[srcset]`, `script[src]`, `iframe[src]`, `object[data]`.

## Data flow

1. **scan**: discover HTML files + build canonical href set in a single parallel walk (`jwalk`) → scan each HTML file in parallel (`tokio` + `spawn_blocking`) → tokenize for `MediaReference`s → local URLs are resolved and checked against the href set inline (only broken ones captured) → remote URLs captured unconditionally → print as unified `kind: file:line:col  url` lines (or JSON).
2. **apply**: discover → scan → deduplicate URLs → download assets in parallel (capped by `--jobs`) → rewrite each file as soon as all its URLs finish downloading → verify no remote URLs remain.
3. **clean**: discover HTML files → `build_href_set` (walk all files once) → scan each HTML file in parallel (`tokio` + `spawn_blocking`) → for each local link, resolve href and check the set → print broken links grouped by file (dry-run default) or apply removals (`--force`).
4. **zap**: discover HTML files → parse selector → scan each file for matching elements containing the query → print matches grouped by file (dry-run default) or remove elements in-place (`--apply`).
5. **towebp**: discover HTML files → scan each file for image URLs (`href`/`src`/`srcset`) with `.jpg`/`.jpeg`/`.png` extensions → print matches grouped by file (dry-run default) or rewrite extensions to `.webp` in-place (`--apply`).

## Performance

Key design decisions for scan performance (~880ms on a 9777-file site, 2365 HTML files):

- **Single walkdir** — `discover_and_index` uses `jwalk` for parallel directory traversal, collecting both the HTML file list and the canonical href set in one pass.
- **Inline existence check** — the scanner resolves local URLs against the href set during tokenization. Valid local URLs are never allocated or stored.
- **Fast-path glob** — default `*.html`/`*.htm` patterns use `ends_with` instead of full glob matching.
- **Remote-only CSS** — `CSS_URL_RE` only matches `https?://` URLs. Local CSS `url()` references are skipped (too noisy, rarely actionable).
- **O(log n) line/col** — byte-offset-to-line mapping uses binary search on a precomputed line-start table.
- **Batched progress** — stderr progress updates every 16 files to reduce flush syscalls.
- **Pre-sized collections** — `href_set`, `html_files`, and `all_refs` use `with_capacity` to avoid mid-scan resizes.

## Dependencies

- **HTML parsing**: `html5gum` tokenizer with span tracking.
- **HTTP**: `hyper` (HTTP/1.1) + `rustls` with `ring` crypto + `webpki-roots` for TLS.
- **Async runtime**: `tokio` (multi-threaded).
- **CLI**: `lexopt` for argument parsing.
- **File walking**: `jwalk` (parallel, for scan discovery) + `walkdir` (for other commands) + `glob` for pattern filtering.
- **Hashing**: `ring` (SHA-256 for content-addressed asset paths) + `rustc-hash` for `FxHashMap`/`FxHashSet`.
- **Concurrency**: `tokio::sync::Semaphore` (limiting parallel downloads/rewrites) and `tokio::sync::Notify` (signaling between download and rewrite tasks).
- **URL parsing**: `url` crate for origin extraction and path handling.

## Testing

```sh
cargo test
```

Tests cover: scanner (tag/attribute extraction, local URL capture, broken detection, span correctness, edge cases), rewriter (URL replacement, relative path computation, broken-URL attribute renaming), downloader (asset path determinism, URL encoding, HTML detection), clean (href resolution including percent-encoding and fragment handling, element removal planning, regression for `%23`-in-filename cases).

## Profiling

```sh
# Allocation stats (adds overhead, debug only):
cargo run --release --features count-alloc -- scan /path/to/site

# macOS Instruments (no SIP required):
cargo instruments -t Allocations --release -- scan /path/to/site

# CPU sampling:
sample ./target/release/localize 1 -f /tmp/localize.sample
```

## Validation

Broken link counts should be validated against [hyperlink](https://github.com/untitaker/hyperlink) on the same root:

```sh
hyperlink ~/Downloads/dfa-localized
cargo run --release -- clean ~/Downloads/dfa-localized
```
