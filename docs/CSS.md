# `extract-css` subcommand

Extracts inline `<style>` CSS blocks into separate `.css` files and replaces
them with `<link rel="stylesheet">` references. This is a safe 1:1
transformation — CSS content is preserved byte-for-byte, no minimization or
reformatting. Dry-run by default; `--apply` writes changes back.

## Usage

```
localize extract-css [ROOT] [flags]
```

| Flag | Default | Description |
|---|---|---|
| `--apply` | off | Write .css files and rewrite HTML. Without it, styles are found and reported but files are untouched. |
| `--css-dir <dir>` | `assets/css` | Output directory for extracted CSS files (relative to root). |
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
localize extract-css ~/mysite --apply --css-dir static/styles

# Single file via include
localize extract-css . --include index.html --apply --verbose
```

## Pipeline

Each HTML file is processed independently in parallel. The pipeline has two
stages per file:

### 1. Parse & extract

Walk the HTML with `html5gum`'s tokenizer in span-tracking mode
(`DefaultEmitter::<usize>::new_with_span()`). When a `<style>` start tag is
encountered, the tokenizer records the opening tag's byte span and any `id`
attribute. When the matching `</style>` end tag arrives, the CSS content
between them is extracted as a raw byte slice — no encoding transformation, no
reformatting.

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

## Naming strategy

CSS filenames are derived with three priority levels, plus collision handling:

| Priority | Source | Example |
|---|---|---|
| 1 | `/*# sourceURL=... */` comment | Full URL → extract path → `wp-includes__blocks__site-logo__style.min.css` |
| 2 | `id` attribute | `wp-block-site-logo-inline-css` → `wp-block-site-logo.css` |
| 3 | Content hash (XXH3-64) | `style-c17071a5cdb9e236.css` |

**sourceURL parsing details:**

Three variants are handled:

| Variant | Input | Output |
|---|---|---|
| Full URL | `https://example.com/wp-includes/blocks/site-logo/style.min.css` | `wp-includes__blocks__site-logo__style.min.css` |
| Root-relative | `/wp-includes/css/dist/block-library/common.min.css` | `wp-includes__css__dist__block-library__common.min.css` |
| Bare name | `wp-emoji-styles-inline-css` | `wp-emoji-styles-inline-css.css` |

Path separators (`/`) are replaced with `__` (double underscore) to keep the
output directory flat. Query strings are stripped. A `.css` extension is
appended if not already present.

**id parsing:** the common WordPress `-inline-css` suffix is stripped:
`wp-block-site-logo-inline-css` → `wp-block-site-logo.css`.

**Collisions:** if a generated filename already exists in the same run, a
numeric suffix is inserted before `.css`: `my-style.css`, `my-style_1.css`,
`my-style_2.css`.

## Edge cases

| Case | Behavior |
|---|---|
| Empty `<style>` element | Deleted from HTML, no .css file, no `<link>` tag |
| `<style>` with no id or sourceURL | Falls back to content hash |
| Malformed HTML (unclosed `<style>`) | Dropped — no EndTag match |
| HTML5 without `</head>` tag | Inserts `<link>` tags before `<body` |
| HTML without `<head>` or `<body>` | Inserts after `<html>` opening tag |
| `<style>` inside HTML comments | Skipped — html5gum ignores comments |
| `.css` already present in output dir | File overwritten (running again is idempotent) |

## Dependencies

- **`html5gum`** — tokenizer with span tracking, already used by the project
  for scanning and translate. Reused here for `<style>` element discovery.
- **`xxhash-rust`** (XXH3-64) — content hash for fallback filenames.
- **`regex-lite`** — parsing `/*# sourceURL=... */` comments from CSS content.
- **`rustc-hash`** — `FxHashMap` for collision tracking.

## Limitations

- **Per-file extraction** — the same CSS block inlined identically across
  multiple pages will be extracted to separate files per page. A future
  site-wide deduplication pass could group identical blocks and have all pages
  reference a shared file.
- **No CSS minimization** — the extracted files are byte-identical to the
  inlined originals. Minification is a separate concern (`localize minify-html`
  handles HTML only, not CSS).
- **HTML `<style>` only** — CSS inside SVG `<style>` elements (different
  namespace) is also extracted. This is usually harmless (SVG in WordPress
  content is rare).
- **Flat output directory** — all CSS files go into `--css-dir` without
  subdirectories. Path separators in sourceURLs are flattened with `__`.
