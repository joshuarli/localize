# Architecture

`localize` is a maintenance toolkit for static HTML sites. Four subcommands:

- **check-remote** — find remote media URLs and broken local media URLs in HTML files. Outputs uniform `kind: ./file:line:col  url` lines. Remote URLs are prefixed `remote-url:`, broken local URLs `broken-local-url:`. Valid local URLs are not printed. With `--download`, fetches remote assets and rewrites HTML to use local relative paths. Supports `--json` for structured output.
- **clean** — find and fix broken local links by unwrapping dead `<a>` tags and removing dead resource elements. Detection via `html5gum`; modification via `lol_html`.
- **zap** — remove HTML elements matching a CSS selector whose inner text contains a query string. Dry-run by default, `--apply` to remove. Detection via `html5gum` (text-aware matching); modification via span-based replacement.
- **towebp** — convert `.jpg`/`.jpeg`/`.png` images to `.webp` (via `zenwebp`, pure-Rust encoder) and rewrite HTML references. Two-phase: first converts all unique images in parallel, moving originals to `.trash/`; then rewrites HTML only for successfully-converted images. Concurrency capped to half of available system memory. Dry-run by default, `--apply` to convert and rewrite.

## Key files

- `src/main.rs` — entry point, calls `cli::run()`. Conditionally wires `alloc::Counter` as global allocator behind `count-alloc` feature.
- `src/alloc.rs` — counting global allocator, gated behind `cargo build --features count-alloc`. Prints heap stats (allocation count, bytes, deallocations) on exit. For profiling only — adds measurable overhead.
- `src/cli.rs` — argument parsing (`lexopt`), file discovery (`glob` + `walkdir` for other commands; `jwalk` for `discover_and_index`), orchestrates all workflows. Contains `discover_and_index` (single walk building both the HTML file list and canonical href set via parallel `jwalk`), `scan_all` (parallel HTML tokenization via `tokio`), `print_human`/`print_json` (unified output).
- `src/scanner.rs` — HTML tokenizer (`html5gum`) that finds URLs in `<img src>`, `<video src>`, `<audio src>`, `<source src/srcset>`, `<track src>`, `<script src>`, `<a href>`, `<link href>`, `<object data>`, `<meta content>` (og:image / twitter:image), `srcset` attributes, inline `style=`, and `<style>` blocks. Checks local URL existence inline via `FxHashSet` lookups (zero per-URL syscalls) — only broken local URLs are captured. CSS `url()` references are remote-only (local CSS references skipped for performance). Returns `MediaReference` structs with byte spans, 1-based line:col positions (computed in O(log n) via binary search on precomputed line starts), and a `broken` flag.
- `src/downloader.rs` — async HTTP client (`hyper` + `rustls`) that downloads assets into a content-addressed directory: `{assets_dir}/{host}/{sha256[:2]}/{sha256[:8]}-{basename}`. Handles retries, redirects, and 404 marking. Rewriting is delegated to `rewriter::apply_html`.
- `src/rewriter.rs` — unified HTML modification via `lol_html`. Provides `apply_html` (URL rewriting via element handlers), `clean_html` (broken link removal using `resolve_href`), `towebp_html` (image extension rewriting), and `zap_html` (html5gum-based text-aware detection + span removal, since lol_html can't retroactively remove elements based on text content). Also contains shared helpers: `compute_relative_path`, `rewrite_srcset_value`, `towebp_url`, `has_image_ext`.
- `src/clean.rs` — broken local link detection (modification is in rewriter.rs). Contains `build_href_set` (walks filesystem once for canonical href set), `resolve_href` (replicates hyperlink's `push_and_canonicalize`), and `scan_file` (html5gum-based link scanner for dry-run reporting). `resolve_href` is shared with `rewriter::clean_html`.
- `src/zap.rs` — CSS selector parser and element detection (modification is in rewriter.rs). Supports `tag`, `.class`, `#id`, `[attr]`, and `[attr=value]` selectors (combinable). `scan_html` uses html5gum to find elements matching the selector whose inner text contains the query string.
- `src/towebp.rs` — image extension detection (modification is in rewriter.rs). `scan_towebp` scans HTML for URLs ending in `.jpg`/`.jpeg`/`.png` in `href`, `src`, and `srcset` attributes. Preserves query strings and fragments.
- `src/webp_encode.rs` — actual WebP image conversion. Decodes PNG (via `png` crate) and JPEG (via `zune-jpeg`), encodes to WebP at quality 90 via `zenwebp` (pure Rust). No metadata, no animation, no ICC profiles.

## Href resolution (clean.rs)

`resolve_href` replicates hyperlink's `push_and_canonicalize` exactly. Also used by `scanner.rs` for resolving local URLs during existence checks, and by `cli.rs` for `cmd_clean`.

1. Strip `?` and `#` from the **raw** (undecoded) href so `%23` (encoded `#`) survives as a literal `#` in filenames.
2. Percent-decode the remaining path.
3. Resolve `..`, `.`, and trailing `index.html`/`index.htm` components relative to the document's canonical href (with `index.html` files contributing their parent directory as the base).

`build_href_set` walks every file under the root and computes its canonical href (stripping `index.html`/`index.htm` to just the directory), stored in an `FxHashSet<String>`. Links are checked with a single `set.contains()`.

Element coverage matches hyperlink's parser: `a[href]`, `area[href]`, `link[href]`, `img[src]/[srcset]`, `script[src]`, `iframe[src]`, `object[data]`.

## Data flow

1. **check-remote**: discover HTML files + build canonical href set in a single parallel walk (`jwalk`) → scan each HTML file in parallel (`tokio` + `spawn_blocking`) → tokenize for `MediaReference`s → local URLs are resolved and checked against the href set inline (only broken ones captured) → remote URLs captured unconditionally → print as unified `kind: file:line:col  url` lines (or JSON). With `--download`: same scan → filter to remote URLs → deduplicate → download assets in parallel (capped by `--jobs`) → rewrite each file via `lol_html` element handlers (`apply_html`) as soon as all its URLs finish downloading.
2. **clean**: discover HTML files → `build_href_set` (walk all files once) → for each HTML file, detect broken links via `scan_file` (html5gum + href resolution) → print broken links grouped by file (dry-run default) or apply removals via `lol_html` element handlers (`rewriter::clean_html`, `--force`).
3. **zap**: discover HTML files → parse selector → for each file, detect matches via `scan_html` (html5gum, text-aware) → print matches grouped by file (dry-run default) or remove elements via span-based replacement (`rewriter::zap_html`, `--apply`). Zap uses html5gum for modification too, since lol_html can't retroactively remove elements based on text content discovered after the element handler fires.
4. **towebp**: discover HTML files → Phase 1a: scan all files in parallel for image references, deduplicate by resolved filesystem path → Phase 1b: convert each unique image in parallel (PNG via `png` crate, JPEG via `zune-jpeg`, encode to WebP via `zenwebp` at quality 90), write `.webp` alongside original, move original to `.trash/` preserving directory structure → Phase 2: rewrite HTML via `lol_html` element handlers (`towebp_html`, gated on successful conversion). Concurrency is bounded by a semaphore capped to `(available_memory / 2) / 20MB` workers. Images already converted (`.webp` exists, original in trash) are detected and skipped — HTML is still rewritten.

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

- **HTML parsing**: `html5gum` tokenizer with span tracking (for scan detection). `lol_html` for HTML modification (element handlers, single-pass rewriting).
- **HTTP**: `hyper` (HTTP/1.1) + `rustls` with `ring` crypto + `webpki-roots` for TLS.
- **Async runtime**: `tokio` (multi-threaded).
- **CLI**: `lexopt` for argument parsing.
- **File walking**: `jwalk` (parallel, for scan discovery) + `walkdir` (for other commands) + `glob` for pattern filtering.
- **Hashing**: `ring` (SHA-256 for content-addressed asset paths) + `rustc-hash` for `FxHashMap`/`FxHashSet`.
- **Concurrency**: `tokio::sync::Semaphore` (limiting parallel downloads/rewrites) and `tokio::sync::Notify` (signaling between download and rewrite tasks).
- **URL parsing**: `url` crate for origin extraction and path handling.
- **Image codecs**: `png` (PNG decoding), `zune-jpeg` (JPEG decoding), `zenwebp` (pure-Rust WebP encoding, quality 90).
- **Regex**: `regex-lite` for CSS `url()` pattern matching in style attributes.

## Testing

```sh
cargo test
```

Tests cover: scanner (tag/attribute extraction, local URL capture, broken detection, span correctness, edge cases), rewriter (URL replacement, relative path computation, broken-URL attribute renaming), downloader (asset path determinism, URL encoding, HTML detection), clean (href resolution including percent-encoding and fragment handling, element removal planning, regression for `%23`-in-filename cases).

## Profiling

```sh
# Allocation stats (adds overhead, debug only):
cargo run --release --features count-alloc -- check-remote /path/to/site

# macOS Instruments (no SIP required):
cargo instruments -t Allocations --release -- check-remote /path/to/site

# CPU sampling:
sample ./target/release/localize 1 -f /tmp/localize.sample
```

## Validation

Broken link counts should be validated against [hyperlink](https://github.com/untitaker/hyperlink) on the same root:

```sh
hyperlink ~/Downloads/dfa-localized
cargo run --release -- clean ~/Downloads/dfa-localized
```
