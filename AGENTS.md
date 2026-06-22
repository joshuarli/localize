# Architecture

`localize` is a maintenance toolkit for static HTML sites. Seven subcommands:

- **check** — find remote media URLs and broken local links in HTML files. Outputs uniform `kind: ./file:line:col  url` lines. Remote URLs are prefixed `remote-url:`, broken local URLs `broken-local-url:`. Valid local URLs are not printed. Dry-run prints a summary of error counts by type. With `--download`, fetches remote assets and rewrites HTML to use local relative paths. With `--clean`, fixes broken local links by unwrapping dead `<a>` tags and removing dead resource elements. Supports `--json` for structured output.
- **bundle-css** — bundle all `<link rel="stylesheet">` CSS files across the site into a single monolithic `bundle/bundle.css` file, then rewrite every HTML file to reference the single bundle. Dry-run by default, `--apply` to write. Designed to follow `extract-css --apply`. Full architecture in [`docs/CSS.md`](docs/CSS.md).
- **extract-css** — extract inline `<style>` CSS blocks into content-addressed `.css` files and replace them with `<link rel="stylesheet">` references in `<head>`. Byte-exact CSS preservation (no minification). Content-addressed via XXH3-64 hash with two-char shard prefix: `{dir}/{hash[..2]}/{hash}.css` — identical CSS blocks across pages share one file. Concurrency-safe writes via `O_CREAT | O_EXCL`. Two-step HTML modification: delete all `<style>` blocks via descending-span surgery (same proven pattern as zap), then insert all `<link>` tags at a single anchor point. Dry-run by default, `--apply` to write. Full architecture in [`docs/CSS.md`](docs/CSS.md).
- **minify-html** — minify HTML files using the [`minify-html`](https://github.com/wilsonzlin/minify-html) crate (HTML-only, no CSS/JS minification). Strips whitespace with per-element strategies, removes comments, collapses redundant attributes, omits optional tags, and optimizes entity encoding. Dry-run by default, `--apply` to write.
- **zap** — remove HTML elements matching a CSS selector whose inner text contains a query string. Dry-run by default, `--apply` to remove. Detection via `html5gum` (text-aware matching); modification via span-based replacement.
- **towebp** — convert `.jpg`/`.jpeg`/`.png` images to `.webp` (via `zenwebp`, pure-Rust encoder) and rewrite HTML references. Two-phase: first converts all unique images in parallel, moving originals to `.trash/`; then rewrites HTML only for successfully-converted images. Concurrency capped to half of available system memory. Dry-run by default, `--apply` to convert and rewrite.
- **translate** — translate HTML text content to another language via Apple's on-device Translation framework (`macos-translate` crate). Extracts text from HTML elements, clusters related segments (article body, headings, nav, sidebar, UI labels), translates with contextual batching, and reconstructs the HTML via span-based replacement. Dry-run by default, `--apply` to write. Full architecture in [`docs/TRANSLATE.md`](docs/TRANSLATE.md).

## Key files

- `src/main.rs` — entry point, calls `cli::run()`. Conditionally wires `alloc::Counter` as global allocator behind `count-alloc` feature.
- `src/alloc.rs` — counting global allocator, gated behind `cargo build --features count-alloc`. Prints heap stats (allocation count, bytes, deallocations) on exit. For profiling only — adds measurable overhead.
- `src/cli.rs` — argument parsing (`lexopt`), file discovery (`glob` + `jwalk`), orchestrates all workflows. Contains `cmd_check` (scan + optional download/clean), `cmd_minify_html`, `cmd_zap`, `cmd_towebp`, `cmd_translate`, `discover_and_index` (single walk building both the HTML file list and canonical href set via parallel `jwalk`), `scan_all` (parallel HTML tokenization via `tokio`), `print_human`/`print_json` (unified output).
- `src/scanner.rs` — HTML tokenizer (`html5gum`) that finds URLs in `<img src>`, `<video src>`, `<audio src>`, `<source src/srcset>`, `<track src>`, `<script src>`, `<a href>`, `<link href>`, `<object data>`, `<meta content>` (og:image / twitter:image), `srcset` attributes, inline `style=`, and `<style>` blocks. Checks local URL existence inline via `FxHashSet` lookups (zero per-URL syscalls) — only broken local URLs are captured. CSS `url()` references are remote-only (local CSS references skipped for performance). Returns `MediaReference` structs with byte spans, 1-based line:col positions (computed in O(log n) via binary search on precomputed line starts), and a `broken` flag.
- `src/bundle_css.rs` — monolithic CSS bundling. Finds `<link rel="stylesheet">` tags in HTML via manual attribute parser (handles quoted/unquoted values, case-insensitive attribute names), classifies links as bundlable/non-bundlable by `media` attribute value (bundles `all`/`screen`/absent, preserves `print`/`max-width`/etc.), resolves CSS paths against HTML file directories, concatenates unique files in `BTreeSet` lexicographic order to a fixed `bundle/bundle.css`, and rewrites HTML by removing bundlable `<link>` spans via descending-span surgery and inserting a single bundle `<link>` tag before `</head>`. Full details in [`docs/CSS.md`](docs/CSS.md).
- `src/extract_css.rs` — inline CSS extraction. Uses `html5gum` to find `<style>` elements and their byte spans, extracts content byte-exact, hashes with XXH3-64 for content-addressed file paths (`{dir}/{hash[..2]}/{hash}.css`), and returns the structured result (hash→content writes, link tags, spans to delete). The CLI handles the two-step HTML modification: delete all `<style>` blocks via descending-span removal, then insert `<link rel="stylesheet">` tags at a single `<head>` anchor point. CSS files are written with `create_new(true)` for concurrency safety; file discovery uses depth-first traversal to group related pages. Full details in [`docs/CSS.md`](docs/CSS.md).
- `src/downloader.rs` — async HTTP client (`hyper` + `rustls`) that downloads assets into a content-addressed directory: `{assets_dir}/{host}/{sha256[:2]}/{sha256[:8]}-{basename}`. Handles retries, redirects, and 404 marking. Rewriting is delegated to `rewriter::apply_html`.
- `src/rewriter.rs` — unified HTML modification via `lol_html`. Provides `apply_html` (URL rewriting via element handlers), `clean_html` (broken link removal using `resolve_href`), `towebp_html` (image extension rewriting), and `zap_html` (html5gum-based text-aware detection + span removal, since lol_html can't retroactively remove elements based on text content). Also contains shared helpers: `compute_relative_path`, `rewrite_srcset_value`, `towebp_url`, `has_image_ext`.
- `src/clean.rs` — shared URL resolution helpers. Contains `resolve_href` (replicates hyperlink's `push_and_canonicalize`), `resolve_href_raw` (no percent-decode fallback for grab-preserved filenames), `link_exists` (two-step existence check used by both scanner and rewriter), `is_local_link`, and `is_external_link`.
- `src/zap.rs` — CSS selector parser and element detection (modification is in rewriter.rs). Supports `tag`, `.class`, `#id`, `[attr]`, and `[attr=value]` selectors (combinable). `scan_html` uses html5gum to find elements matching the selector whose inner text contains the query string.
- `src/towebp.rs` — image extension detection (modification is in rewriter.rs). `scan_towebp` scans HTML for URLs ending in `.jpg`/`.jpeg`/`.png` in `href`, `src`, and `srcset` attributes. Preserves query strings and fragments.
- `src/webp_encode.rs` — actual WebP image conversion. Decodes PNG (via `png` crate) and JPEG (via `zune-jpeg`), encodes to WebP at quality 90 via `zenwebp` (pure Rust). No metadata, no animation, no ICC profiles.
- `src/translate.rs` — HTML text translation pipeline. Five phases: `extract_segments` (html5gum tokenizer with span tracking, extracts text nodes and alt/title attributes), classification via tag-stack heuristics, `cluster_segments` (article body contiguity + batch grouping by kind), translation orchestration (article join-and-split with fallback, batch via `translate_batch`), `apply_translations` (descending-span replacement). Also contains `build.rs` which adds `-rpath /usr/lib/swift` for the macos-translate dependency. Full documentation in [`docs/TRANSLATE.md`](docs/TRANSLATE.md).

## Href resolution (clean.rs)

`resolve_href` replicates hyperlink's `push_and_canonicalize` exactly. Used by `scanner.rs` and `rewriter.rs` via the shared `link_exists` helper.

1. Strip `?` and `#` from the **raw** (undecoded) href so `%23` (encoded `#`) survives as a literal `#` in filenames.
2. Percent-decode the remaining path.
3. Resolve `..`, `.`, and trailing `index.html`/`index.htm` components relative to the document's canonical href (with `index.html` files contributing their parent directory as the base).

`build_href_set` walks every file under the root and computes its canonical href (stripping `index.html`/`index.htm` to just the directory), stored in an `FxHashSet<String>`. Links are checked with a single `set.contains()`.

Element coverage matches hyperlink's parser: `a[href]`, `area[href]`, `link[href]`, `img[src]/[srcset]`, `script[src]`, `iframe[src]`, `object[data]`.

## Data flow

1. **minify-html**: discover HTML files via jwalk → process in parallel (tokio + `spawn_blocking`) → read file, minify via `minify_html::minify`, write back if `--apply`. Emits per-file savings with `--verbose`, summary total otherwise.
2. **check**: discover HTML files + build canonical href set in a single parallel walk (`jwalk`) → scan each HTML file in parallel (`tokio` + `spawn_blocking`) → tokenize for `MediaReference`s → local URLs are resolved and checked against the href set inline via `link_exists` (only broken ones captured) → remote URLs captured unconditionally → print as unified `kind: file:line:col  url` lines (or JSON) with a dry-run summary of error counts by type. With `--download`: same scan → filter to remote URLs → deduplicate → download assets in parallel (capped by `--jobs`) → rewrite each file via `lol_html` element handlers (`apply_html`) as soon as all its URLs finish downloading. With `--clean`: filter to broken local URLs → group by file → fix via `lol_html` element handlers (`clean_html`). `--download` and `--clean` may be combined.
3. **extract-css**: discover HTML files via depth-first traversal (`std::fs::read_dir` recursion, sorted entries) — groups sibling files contiguously to reduce duplicate-write race windows. Process in parallel (`crossbeam` thread scope) → for each file: tokenize with `html5gum` to find all `<style>` elements and their byte spans → extract CSS content byte-exact → hash with XXH3-64 for content-addressed paths (`{dir}/{hash[..2]}/{hash}.css`) → compute relative paths for `<link>` tags. When `--apply`: delete all `<style>` blocks via descending-span removal (same proven pattern as zap) → write .css files with `create_new(true)` (O_CREAT | O_EXCL, concurrency-safe) → insert `<link rel="stylesheet">` tags before `</head>` (with fallbacks for minified/HTML5-optional head markup) → write modified HTML via tmp+rename. Full details in [`docs/CSS.md`](docs/CSS.md).
4. **bundle-css**: discover HTML files via depth-first traversal (same as extract-css) → Phase 1: scan all files in parallel (`crossbeam` thread scope) for `<link rel="stylesheet">` tags via manual attribute parser → classify links as bundlable/non-bundlable by `media` attribute → resolve each bundlable href against the HTML file's directory → collect unique CSS file paths in a `BTreeSet`. Phase 2: concatenate CSS files in lexicographic order → write to fixed path `{bundle-dir}/bundle.css`. Phase 3: rewrite each HTML file in parallel → remove bundlable `<link>` spans via descending-span surgery → compute relative path from HTML file to bundle → insert single `<link rel="stylesheet">` tag before `</head>` → write modified HTML via tmp+rename. Non-bundlable links (media-specific, remote) are preserved. Dry-run by default, `--apply` to write. Full details in [`docs/CSS.md`](docs/CSS.md).
5. **zap**: discover HTML files → parse selector → for each file, detect matches via `scan_html` (html5gum, text-aware) → print matches grouped by file (dry-run default) or remove elements via span-based replacement (`rewriter::zap_html`, `--apply`). Zap uses html5gum for modification too, since lol_html can't retroactively remove elements based on text content discovered after the element handler fires.
6. **towebp**: discover HTML files → Phase 1a: scan all files in parallel for image references, deduplicate by resolved filesystem path → Phase 1b: convert each unique image in parallel (PNG via `png` crate, JPEG via `zune-jpeg`, encode to WebP via `zenwebp` at quality 90), write `.webp` alongside original, move original to `.trash/` preserving directory structure → Phase 2: rewrite HTML via `lol_html` element handlers (`towebp_html`, gated on successful conversion). Concurrency is bounded by a semaphore capped to `(available_memory / 2) / 20MB` workers. Images already converted (`.webp` exists, original in trash) are detected and skipped — HTML is still rewritten.
7. **translate**: discover HTML files → process sequentially (one file at a time — translation latency dominates, not I/O) → for each file: extract text segments via html5gum (text nodes + alt/title attributes, skipping script/style/pre/code) → classify segments by tag-stack heuristics (nav, sidebar, heading, article body, UI element, etc.) → cluster (contiguous article body segments joined for contextual translation; everything else batched by kind) → translate via macos-translate (article clusters: join with unique separator, translate as one, split back with fallback to batch; batch clusters: `translate_batch`) → reconstruct HTML via descending-span replacement. Dry-run by default, `--apply` to write. Full details in [`docs/TRANSLATE.md`](docs/TRANSLATE.md).

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
- **HTML minification**: `minify-html` (HTML-only, CSS/JS features disabled). Custom parser with per-element whitespace strategies, WHATWG tag omission, entity optimization, attribute minification, and template syntax preservation.
- **HTTP**: `ureq` (blocking HTTP/1.1) + `native-tls` for TLS.
- **CLI**: `lexopt` for argument parsing.
- **File walking**: `jwalk` (parallel, for scan discovery) + `glob` for pattern filtering.
- **Hashing**: `xxhash-rust` (XXH3-64 for content-addressed asset paths and CSS filename fallback) + `rustc-hash` for `FxHashMap`/`FxHashSet`.
- **Concurrency**: `std::thread::scope` + `Arc<AtomicUsize>` work-stealing for all parallel work. Download-rewrite pipelining replaced with two-phase (download all → rewrite all).
- **URL parsing**: `url` crate for origin extraction and path handling.
- **Image codecs**: `png` (PNG decoding), `zune-jpeg` (JPEG decoding), `zenwebp` (pure-Rust WebP encoding, quality 90).
- **Regex**: `regex-lite` for CSS `url()` pattern matching in style attributes.

## Testing

```sh
cargo test
```

Tests cover: scanner (tag/attribute extraction, local URL capture, broken detection, span correctness, edge cases), rewriter (URL replacement, relative path computation, broken-URL attribute renaming), downloader (asset path determinism, URL encoding, HTML detection), clean (href resolution including percent-encoding and fragment handling, regression for `%23`-in-filename cases), extract_css (style block discovery, content addressing, relative path computation), bundle_css (link tag parsing with attribute extraction, media classification, path resolution, content-addressed concatenation, HTML rewriting), e2e_css (full extract→bundle pipeline on mock sites, media-specific link preservation, fixed-path bundle references).

## Profiling

```sh
# Allocation stats (adds overhead, debug only):
cargo run --release --features count-alloc -- check /path/to/site

# macOS Instruments (no SIP required):
cargo instruments -t Allocations --release -- check /path/to/site

# CPU sampling:
sample ./target/release/localize 1 -f /tmp/localize.sample
```

## Validation

Broken link counts should be validated against [hyperlink](https://github.com/untitaker/hyperlink) on the same root:

```sh
hyperlink ~/Downloads/dfa-localized
cargo run --release -- check ~/Downloads/dfa-localized
```
