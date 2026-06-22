# `translate` subcommand

Translates HTML text content to another language using Apple's on-device
Translation framework (via the `macos-translate` crate). Dry-run by default;
`--apply` writes changes back.

## Usage

```
localize translate [ROOT] [flags]
```

| Flag | Default | Description |
|---|---|---|
| `--from <lang>` | auto-detect | Source language (BCP-47, e.g. `zh-Hans`). Detected per file if omitted. |
| `--to <lang>` | `en` | Target language (BCP-47). |
| `--apply` | off | Write translations back to HTML files. Without it, segments are printed but files are untouched. |
| `--include`, `--exclude` | `*.html`, `*.htm` | Glob patterns for file selection. |
| `--verbose` | off | Per-file cluster breakdown. |

**Examples:**

```sh
# Dry-run: see what would be translated
localize translate ~/mysite --to en

# Auto-detect source, translate to English
localize translate ~/mysite --to en --apply

# Explicit source language
localize translate ~/mysite --from zh-Hans --to en --apply

# Single file via include
localize translate . --include index.html --to en --apply --verbose
```

## Pipeline

Each HTML file goes through five phases sequentially (files are processed one
at a time to keep memory low — translation latency dominates, not I/O):

### 1. Extract

Walk the HTML with `html5gum`'s tokenizer in span-tracking mode
(`DefaultEmitter::<usize>::new_with_span()`). Maintain a tag stack (tag name,
classes, id, article index) for classification context.

**What's extracted:**
- Text nodes (`Token::String`) — trimmed of surrounding whitespace but the
  original byte span and full text are preserved for reconstruction.
- `alt` and `title` attribute values — the value portion only, parsed from
  the raw attribute text (`attr.span` covers `name="value"`, not just
  `value`).

**What's skipped:**
- Content inside `<script>`, `<style>`, `<pre>`, `<code>`, `<textarea>`,
  `<noscript>`.
- Empty/whitespace-only text nodes.
- Void elements (they have no text content).

### 2. Classify

Each text segment gets a `SegmentKind` by walking the tag stack
innermost-first. Priority order:

| Priority | Check | Kind |
|---|---|---|
| 1 | Ancestor is script/style/pre/code/textarea | `Skippable` |
| 2 | Ancestor is `<nav>`, or has `nav`/`menu` in class/id | `Nav` |
| 3 | Ancestor has `sidebar`/`widget`/`aside` in class/id | `Sidebar` |
| 4 | Ancestor is `<header>` or `id="header"` | `Nav` |
| 5 | Innermost tag is `h1`–`h6` | `Heading` |
| 6 | Innermost tag is `button`/`label`/`option`/`figcaption` | `UIElement` |
| 7 | Ancestor is an article container | `ArticleBody(n)` |
| 8 | Ancestor is `<footer>` or has `footer` in class/id | `Generic` |
| 9 | ≤ 3 words | `UIElement` |
| 10 | Everything else | `Generic` |

**Article container detection** — an element is treated as an article
container if:
- It is `<article>` or `<main>`, or
- It is `<div>`/`<section>` with a class containing `post`, `article`, or
  `entry-content`, or
- It is `<div>`/`<section>` with an id containing those keywords.

The article index is inherited by all descendants (unless a nested article
container overrides it).

### 3. Cluster

Segments are grouped into clusters for translation:

- **Article clusters** — contiguous `ArticleBody(n)` segments with the same
  article index are grouped together. A heading between paragraphs breaks
  contiguity (headings are their own cluster), so an article's body may
  produce multiple clusters if headings are interleaved.

- **Batch clusters** — one cluster per remaining kind: all `Heading` segments
  together, all `Nav` segments together, all `Sidebar` together, all
  `UIElement` together, all `AltText` together, all `Generic` together.

### 4. Translate

Article and batch clusters both use `translate_batch()`. Each segment's core
text is submitted as a separate request within the cluster batch, allowing the
Apple translation wrapper to use its fastest available batch path while still
mapping results back to their original HTML spans.

**Whitespace preservation** — each segment records its surrounding whitespace
(prefix/suffix) from the original HTML. After translation, the prefix and
suffix are reattached to the translated core text. This keeps inline elements
(`<a>`, `<span>`) properly spaced.

**Error resilience** — per-segment failures in batch mode leave the original
text in place with a warning logged in verbose mode. Article join failures
fall back to batch mode.

### 5. Reconstruct

Segments with translations are sorted by byte span **descending** (highest
offset first). Each span in the original HTML is replaced with its
translation via `String::replace_range`. Descending order keeps earlier byte
offsets valid as replacements shift the string.

This is the same pattern used by `rewriter::zap_html`.

## Language detection

When `--from` is omitted, all extracted text (trimmed cores) is concatenated
and passed to `apple_translate_rs_sync::detect_language()`, which uses Apple's
`NLLanguageRecognizer`. Detection runs once per file so short files get the
benefit of all their text combined.

If detection returns `None` (very short or ambiguous text), the file is
skipped with a warning.

## Dependencies

- **`macos-translate`** — wraps Apple's `Translation.framework` (on-device
  Neural Engine). Requires macOS 15.0+, Swift runtime. Linked via
  `build.rs` which adds `-rpath /usr/lib/swift`.
- **`html5gum`** — already used by the project for scanning, reused here for
  text extraction with span tracking.

## Limitations

- **Plain text only** — Apple's Translation framework doesn't accept HTML, so
  markup-aware translation (e.g. "don't translate `<code>` blocks") relies
  entirely on extraction heuristics.
- **No semantic HTML required** — classification works on class/id heuristics
  and tag names, so messy real-world HTML is handled, but unusual markup
  patterns may misclassify segments.
- **Per-segment article translation** — article body segments are translated
  independently within a batch. This favors throughput and reliable span
  mapping over full-article context.
- **No incremental translation** — re-running on already-translated files
  will translate them again (potentially degrading quality or introducing
  drift). A future normalization pass ([defuddle](https://github.com/kepano/defuddle)) could help by
  stripping boilerplate before translation.
- **Single language pair per run** — all files use the same `--from`/`--to`.
  Mixed-language sites need multiple invocations.
