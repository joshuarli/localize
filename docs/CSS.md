# `extract-css` subcommand

Extracts inline `<style>` CSS blocks into content-addressed `.css` files and
replaces them with `<link rel="stylesheet">` references. CSS content is
preserved byte-for-byte — no minimization or reformatting. Files are stored
under a sharded directory by XXH3-64 content hash, so identical CSS blocks
across any number of pages share a single file on disk. Dry-run by default;
`--apply` writes changes back.

## Usage

```
localize extract-css [ROOT] [flags]
```

| Flag | Default | Description |
|---|---|---|
| `--apply` | off | Write .css files and rewrite HTML. Without it, styles are found and reported but files are untouched. |
| `-d`, `--dir <dir>` | `localized-css` | Output directory for extracted CSS files (relative to root). |
| `--include`, `--exclude` | `*.html`, `*.htm` | Glob patterns for file selection. |
| `--verbose` | off | Per-file file listing during discovery. |
| `--jobs <n>` | CPUs × 4 | Max parallel workers. |

**Examples:**

```sh
# Dry-run: see what would be extracted
localize extract-css ~/mysite

# Extract all inline CSS
localize extract-css ~/mysite --apply

# Custom output directory
localize extract-css ~/mysite --apply -d static/styles

# Single file via include
localize extract-css . --include index.html --apply --verbose
```

## Pipeline

Each HTML file is processed independently in parallel. The pipeline has two
stages per file:

### 1. Parse & extract

Walk the HTML with `html5gum`'s tokenizer in span-tracking mode
(`DefaultEmitter::<usize>::new_with_span()`). When a `<style>` start tag is
encountered, the tokenizer records the opening tag's byte span. When the
matching `</style>` end tag arrives, the CSS content between them is extracted
as a raw byte slice — no encoding transformation, no reformatting.

Empty `<style>` elements (e.g. `<style class="wp-fonts-local"></style>`) are
recorded for deletion but produce no CSS file or `<link>` tag.

### 2. Delete & insert

Two clean operations with no per-element byte-range replacement:

1. **Delete** — all `<style>` blocks are removed by span. Spans are sorted
   descending by start position (highest byte offset first), then each is
   removed via `String::replace_range` with `""`. Descending order keeps
   earlier byte offsets valid as deletions shift the string. This is the same
   proven pattern used by `rewriter::zap_html`.

2. **Insert** — all `<link rel="stylesheet" href="...">` tags are inserted at a
   single point in `<head>`. The anchor is found by searching for `</head>`
   (with `>`), falling back to `</head` without `>` (handles minified HTML
   where the closing `>` is dropped), then `<body` (HTML5 may omit `</head>`
   entirely), then `<html`, then position 0. The relative `href` path from the
   HTML file to each CSS file is computed via `compute_relative_path`.

## Content-addressed storage

Each CSS block is hashed with XXH3-64. The hash determines the file path:

```
{dir}/{hash[..2]}/{hash}.css
```

For example, a CSS block hashing to `c886f41ebddde45a` is stored at
`localized-css/c8/c886f41ebddde45a.css`. The two-character shard prefix
mirrors the pattern used by `downloader::asset_path`.

### Deduplication

Because the file path is derived from content, identical CSS blocks across
different pages map to the same file. A 2306-page WordPress site with the same
theme and block-library styles inlined on every page produces 56 unique CSS
files rather than ~66,000.

### Concurrency safety

CSS files are written with `OpenOptions::create_new(true)`, which maps to
`O_CREAT | O_EXCL` — an atomic create-or-fail operation on all modern
filesystems. If two workers race to write the same block, one wins and the
other silently skips (content is identical by definition). If a file already
exists from a prior run, it is also skipped.

File discovery uses depth-first traversal (`std::fs::read_dir` recursion with
sorted entries) rather than `jwalk`'s breadth-first parallel walk. This
groups sibling files contiguously in the work queue so that pages sharing the
same CSS blocks (typically in the same directory) are processed close in time,
minimizing the race window for duplicate writes.

## Edge cases

| Case | Behavior |
|---|---|
| Empty `<style>` element | Deleted from HTML, no .css file, no `<link>` tag |
| Malformed HTML (unclosed `<style>`) | Dropped — no EndTag match |
| HTML5 without `</head>` tag | Inserts `<link>` tags before `<body` |
| HTML without `<head>` or `<body>` | Inserts after `<html>` opening tag |
| `<style>` inside HTML comments | Skipped — html5gum ignores comments |
| Worker race on same hash | `create_new(true)` atomic — one wins, others skip |
| File already exists from prior run | `create_new(true)` fails → skip (content identical) |
| Crash during `write_all` | Partial file may remain; next run skips it. Delete `--dir` to reset. |

## Dependencies

- **`html5gum`** — tokenizer with span tracking, already used by the project
  for scanning and translate. Reused here for `<style>` element discovery.
- **`xxhash-rust`** (XXH3-64) — content hash for all filenames.
- **`rustc-hash`** — `FxHashMap`/`FxHashSet` for internal data structures.

## Limitations

- **No CSS minimization** — the extracted files are byte-identical to the
  inlined originals. Minification is a separate concern (`localize minify-html`
  handles HTML only, not CSS).
- **HTML `<style>` only** — CSS inside SVG `<style>` elements (different
  namespace) is also extracted. This is usually harmless (SVG in WordPress
  content is rare).
- **Single output directory** — all CSS files go into `--dir` (sharded
  underneath). There is no per-site or per-host segmentation.
- **No incremental mode** — re-running on already-extracted files will attempt
  to create the same CSS files again (harmlessly skipped by `create_new`).
  Already-extracted HTML files will be modified again (link tags will
  accumulate if `<style>` blocks have already been removed on a previous run).

---

# `bundle-css` subcommand

Bundles all `<link rel="stylesheet">` CSS files across the entire site into a
single monolithic content-addressed `.css` file, then rewrites every HTML file
to reference the single bundle instead of multiple individual stylesheets.

Designed to follow `extract-css --apply`: first extract inline `<style>` blocks
into CSS files, then bundle everything (extracted + original external CSS) into
one file.

## Usage

```
localize bundle-css [ROOT] [flags]
```

| Flag | Default | Description |
|---|---|---|
| `--apply` | off | Write the bundle and rewrite HTML. Without it, files are scanned and reported but untouched. |
| `--bundle-dir <dir>` | `bundle` | Output directory for the bundle file (relative to root). |
| `--include`, `--exclude` | `*.html`, `*.htm` | Glob patterns for file selection. |
| `--verbose` | off | Per-file output during discovery. |
| `--jobs <n>` | CPUs × 4 | Max parallel workers. |

**Examples:**

```sh
# Full pipeline: extract inline CSS, then bundle everything
localize extract-css ~/mysite --apply
localize bundle-css ~/mysite --apply

# Dry-run: see what would be bundled
localize bundle-css ~/mysite

# Custom output directory
localize bundle-css ~/mysite --apply --bundle-dir static/bundle
```

## Pipeline

Three phases, all parallelized with `crossbeam`:

### Phase 1: Collection

All HTML files are scanned in parallel for `<link rel="stylesheet">` tags.
Each link's `href` is resolved against the containing HTML file's directory
to produce a root-relative filesystem path. All unique CSS file paths are
collected in a `BTreeSet` (ordered, unique). Per-file link spans are recorded
for the rewrite phase.

Links are classified as **bundlable** or **non-bundlable** based on their
`media` attribute:

| `media` value | Bundled? |
|---|---|
| (absent) | ✓ bundled |
| `all` | ✓ bundled |
| `screen` | ✓ bundled |
| `print` | ✗ preserved as-is |
| `only screen and (...)` | ✗ preserved as-is |
| any other value | ✗ preserved as-is |

Remote (`http://`/`https://`) CSS URLs are not bundled and their `<link>` tags
are preserved.

### Phase 2: Concatenation

Unique CSS files are concatenated in lexicographic order by root-relative path.
This is deterministic across runs. The result is written to a fixed path:

```
{bundle-dir}/bundle.css
```

Empty CSS files are skipped. Missing files are skipped with a warning.

### Phase 3: HTML rewriting

Each HTML file that has bundlable links is rewritten in parallel:
1. All bundlable `<link>` tags are removed via descending-span surgery (same
   pattern as `extract-css`)
2. A single `<link rel="stylesheet" href="{relative_path}">` tag is inserted
   before `</head>` (with the same fallback chain: `</head>` → `<body` →
   `<html>` → position 0)
3. Non-bundlable `<link>` tags (media-specific, remote) are left untouched

The relative `href` path from each HTML file to the bundle is computed
dynamically, so files in subdirectories get paths like `../bundle/xx/hash.css`.

## Idempotency

Re-running on an already-bundled site is safe: the bundle overwrites the
existing `bundle/bundle.css` with identical content, and the HTML rewrite
replaces the old bundle `<link>` with an identical new one.

## Edge cases

| Case | Behavior |
|---|---|
| No CSS files found | Command exits with "No local CSS files to bundle" |
| Media-specific link (`media="print"`, etc.) | Preserved as a separate `<link>` tag |
| Remote CSS URL | Skipped, `<link>` tag preserved |
| Missing CSS file on disk | Skipped with warning |
| `<link>` in `<body>` | Bundled regardless of position in HTML |
| Site with only one CSS file | Still processed — single file becomes the bundle |
| HTML5 without `</head>` | Inserts bundle link before `<body` |
| Links with `rel="stylesheet preload"` | Bundled (contains "stylesheet" token) |
| Links with `rel="preload"` only | Skipped (not a stylesheet) |

## Dependencies

- **`std::collections::BTreeSet`** — sorted unique CSS path collection.
- No new third-party dependencies.

## Limitations

- **No CSS minification** — the bundle is byte-for-byte concatenation of
  source files. Minification is a separate concern.
- **No dead-code elimination** — all CSS from all pages is included in the
  bundle, even rules that don't apply to a given page.
- **No source maps** — the bundle has no mapping back to original files.
- **No media query wrapping** — files linked with `media="screen"` are bundled
  without wrapping, which is safe since `screen` is the default medium.
  Media-specific links (`print`, `max-width`, etc.) are preserved as-is.
- **Lexicographic ordering** — CSS files are concatenated in alphabetical
  order by root-relative path, which may not match the original cascade order.
  This works correctly when CSS specificity and source order don't interact
  across files, which is the common case for WordPress themes.
