# Architecture

`localize` scans HTML files for remote media URLs, downloads the assets locally, and rewrites the HTML to use relative paths.

## Key files

- `src/main.rs` — entry point, calls `cli::run()`.
- `src/cli.rs` — argument parsing (`lexopt`), file discovery (`glob` + `walkdir`), orchestrates the scan and apply workflows.
- `src/scanner.rs` — HTML tokenizer (`html5gum`) that finds remote URLs in `<img src>`, `<video src>`, `<meta content>`, `srcset`, inline `style=`, `<style>` blocks, etc. Returns `MediaReference` structs with byte spans used for rewriting.
- `src/downloader.rs` — async HTTP client (`hyper` + `rustls`) that downloads assets into a content-addressed directory: `{assets_dir}/{host}/{sha256[:2]}/{sha256[:8]}-{basename}`. Handles retries, redirects, and broken (404) URL marking.
- `src/rewriter.rs` — rewrites HTML files in-place: replaces remote URLs with computed relative paths, renames attributes to `data-broken-*` for permanently-failed URLs so the original URL is preserved in source but the browser won't request it.

## Data flow

1. **scan**: discover HTML files → tokenize each for `MediaReference`s → print as text or JSON.
2. **apply**: discover → scan → deduplicate URLs → download assets in parallel (capped by `--jobs`) → rewrite each file as soon as all its URLs finish downloading → verify no remote URLs remain.

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

Tests cover: scanner (tag/attribute extraction, span correctness, edge cases), rewriter (URL replacement, relative path computation, broken-URL attribute renaming), downloader (asset path determinism, URL encoding, HTML detection).
