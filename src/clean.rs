use html5gum::Tokenizer;
use html5gum::emitters::default::DefaultEmitter;
use rustc_hash::FxHashSet;
use std::ops::Range;
use std::path::Path;
use std::sync::LazyLock;

static VOID_ELEMENTS: LazyLock<FxHashSet<&'static [u8]>> = LazyLock::new(|| {
    let mut set = FxHashSet::default();
    for e in [
        b"area" as &[u8],
        b"base",
        b"br",
        b"col",
        b"embed",
        b"hr",
        b"img",
        b"input",
        b"link",
        b"meta",
        b"param",
        b"source",
        b"track",
        b"wbr",
    ] {
        set.insert(e);
    }
    set
});

#[derive(Debug, Clone)]
pub struct BrokenLink {
    pub url: String,
    pub tag: String,
    pub attr: String,
    #[allow(dead_code)]
    pub url_span: Range<usize>,
    pub tag_span: Range<usize>,
    #[allow(dead_code)]
    pub line: usize,
    #[allow(dead_code)]
    pub action: &'static str,
}

#[derive(Debug)]
pub struct CleanResult {
    pub broken_links: Vec<BrokenLink>,
    pub error: Option<String>,
}

/// Build the set of all canonical hrefs defined by files on disk.
/// Every file under `root` contributes its relative path as a canonical href.
/// `index.html` / `index.htm` files contribute their parent directory instead.
pub fn build_href_set(root: &Path) -> FxHashSet<String> {
    let mut set = FxHashSet::default();
    for entry in walkdir::WalkDir::new(root) {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry.path().strip_prefix(root).unwrap_or(entry.path());
        let rel_str = rel.to_string_lossy().replace('\\', "/");

        let href = if rel_str.ends_with("/index.html") || rel_str.ends_with("/index.htm") {
            // Strip the filename, leaving the directory path.
            match rel_str.rfind('/') {
                Some(pos) => &rel_str[..pos],
                None => "", // root index.html → empty string
            }
        } else if rel_str == "index.html" || rel_str == "index.htm" {
            ""
        } else {
            &rel_str
        };

        set.insert(href.to_string());
    }
    set
}

/// Check whether a URL has an external scheme (http, mailto, etc.).
/// Matches hyperlink's `is_external_link`.
fn is_external_link(url: &str) -> bool {
    let bytes = url.as_bytes();
    let first = match bytes.first() {
        Some(&b) => b,
        None => return false,
    };
    if bytes.starts_with(b"//") {
        return true;
    }
    if !first.is_ascii_alphabetic() {
        return false;
    }
    for &c in &bytes[1..] {
        match c {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'+' | b'-' | b'.' => continue,
            b':' => return true,
            _ => return false,
        }
    }
    false
}

/// Returns true if the URL is a local link we should check (not external, not empty).
fn is_local_link(url: &str) -> bool {
    if url.is_empty() {
        return false;
    }
    !is_external_link(url)
}

/// Percent-decode into the provided scratch buffer. Returns the decoded slice.
fn percent_decode_into<'a>(input: &str, buf: &'a mut String) -> &'a str {
    buf.clear();
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = match bytes[i + 1] {
                b @ b'0'..=b'9' => b - b'0',
                b @ b'a'..=b'f' => b - b'a' + 10,
                b @ b'A'..=b'F' => b - b'A' + 10,
                _ => 255,
            };
            let lo = match bytes[i + 2] {
                b @ b'0'..=b'9' => b - b'0',
                b @ b'a'..=b'f' => b - b'a' + 10,
                b @ b'A'..=b'F' => b - b'A' + 10,
                _ => 255,
            };
            if hi < 16 && lo < 16 {
                buf.push((hi << 4 | lo) as char);
                i += 3;
                continue;
            }
        }
        buf.push(bytes[i] as char);
        i += 1;
    }
    buf
}

/// Resolve a relative href to its canonical form, replicating hyperlink's
/// `push_and_canonicalize`. The document's href (e.g. "material/1642.html") is the
/// base. `scratch` and `decode_buf` are reused across calls.
///
/// Fragment (`#`) and query (`?`) are stripped from the RAW href BEFORE
/// percent-decoding, so that `%23` (encoded `#`) in a filename is preserved
/// as a literal `#` in the resolved path rather than treated as a fragment.
fn resolve_href<'a>(
    doc_href: &str,
    doc_is_index: bool,
    raw_href: &str,
    scratch: &'a mut String,
    decode_buf: &'a mut String,
) -> &'a str {
    let trimmed = raw_href.trim();

    // Strip fragment and query from the raw string, matching hyperlink's
    // approach: `%23` must survive decoding as a literal `#` in the path.
    let qs = trimmed.find(&['?', '#'][..]).unwrap_or(trimmed.len());
    let raw_path = &trimmed[..qs];

    // Percent-decode only the path portion.
    let path = percent_decode_into(raw_path, decode_buf);

    scratch.clear();

    // External link or absolute path: replace base entirely.
    if is_external_link(path) {
        scratch.push_str(path);
        return scratch;
    }
    if let Some(stripped) = path.strip_prefix('/') {
        scratch.push_str(stripped);
        return scratch;
    }

    // Start from the document's base.
    scratch.push_str(doc_href);
    if doc_is_index {
        scratch.push('/');
    }

    // Handle empty path (self-reference).
    if path.is_empty() {
        // Strip trailing slash from index pages.
        if scratch.ends_with('/') {
            scratch.pop();
        }
        return scratch;
    }

    // Strip to the directory containing the document.
    if let Some(pos) = scratch.rfind('/') {
        scratch.truncate(pos);
    } else {
        scratch.clear();
    }

    // Process each path component.
    let mut components = path.split('/').peekable();
    while let Some(comp) = components.next() {
        let is_last = components.peek().is_none();
        match comp {
            "index.html" | "index.htm" if is_last => {}
            "" | "." => {}
            ".." => {
                if let Some(pos) = scratch.rfind('/') {
                    scratch.truncate(pos);
                } else {
                    scratch.clear();
                }
            }
            _ => {
                if !scratch.is_empty() {
                    scratch.push('/');
                }
                scratch.push_str(comp);
            }
        }
    }

    scratch
}

fn tag_attrs(tag: &[u8]) -> Option<&'static [&'static str]> {
    match tag {
        b"a" | b"area" | b"link" => Some(&["href"]),
        b"img" => Some(&["src", "srcset"]),
        b"script" | b"iframe" => Some(&["src"]),
        b"object" => Some(&["data"]),
        _ => None,
    }
}

fn find_value_in_attr(raw: &str, attr_start: usize, value: &str) -> Range<usize> {
    let offset = raw.find(value).unwrap_or(0);
    let start = attr_start + offset;
    start..start + value.len()
}

fn line_of_offset(source: &str, offset: usize) -> usize {
    let prefix = &source[..offset.min(source.len())];
    prefix.bytes().filter(|&b| b == b'\n').count() + 1
}

fn span_to_range(start: usize, end: usize) -> Range<usize> {
    start..end
}

/// Parse the HTML and find all broken local links.
/// `href_set` is the pre-built set of canonical hrefs from `build_href_set`.
pub fn scan_file(file_path: &str, html: &str, href_set: &FxHashSet<String>) -> CleanResult {
    // Compute the document's canonical href for resolving relative links.
    // Kept as an owned String so we have a stable borrow for the &str used by resolve_href.
    let normalized = file_path.replace('\\', "/");
    let (doc_href, doc_is_index): (String, bool) =
        if normalized.ends_with("/index.html") || normalized.ends_with("/index.htm") {
            match normalized.rfind('/') {
                Some(pos) => (normalized[..pos].to_string(), true),
                None => (String::new(), true),
            }
        } else if normalized == "index.html" || normalized == "index.htm" {
            (String::new(), true)
        } else {
            (normalized, false)
        };
    let doc_href = doc_href.as_str();

    let mut broken: Vec<BrokenLink> = Vec::new();
    let mut scratch = String::new();
    let mut decode_buf = String::new();

    let tokenizer = Tokenizer::new_with_emitter(html, DefaultEmitter::<usize>::new_with_span());

    for token_result in tokenizer {
        let token = match token_result {
            Ok(t) => t,
            Err(e) => {
                return CleanResult {
                    broken_links: broken,
                    error: Some(format!("Parse error: {e}")),
                };
            }
        };

        if let html5gum::Token::StartTag(tag) = token {
            let tag_name = &tag.name[..];
            let attrs_to_check = tag_attrs(tag_name);

            if attrs_to_check.is_none() {
                continue;
            }
            let attrs_to_check = attrs_to_check.unwrap();

            for (name, attr) in &tag.attributes {
                let attr_name = std::str::from_utf8(&name[..])
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if !attrs_to_check.contains(&attr_name.as_str()) {
                    continue;
                }
                let attr_value = std::str::from_utf8(&attr[..]).unwrap_or("");
                let trimmed = attr_value.trim();
                let raw = &html[attr.span.start..attr.span.end];

                if attr_name == "srcset" {
                    for (url_str, _descriptor) in parse_srcset_entries(attr_value) {
                        let url_trimmed = url_str.trim();
                        if is_local_link(url_trimmed) {
                            let resolved = resolve_href(
                                doc_href,
                                doc_is_index,
                                url_trimmed,
                                &mut scratch,
                                &mut decode_buf,
                            );
                            if !href_set.contains(resolved) {
                                let url_span = find_value_in_attr(raw, attr.span.start, &url_str);
                                let action = action_for_tag(tag_name);
                                broken.push(BrokenLink {
                                    url: url_str.to_string(),
                                    tag: std::str::from_utf8(tag_name)
                                        .unwrap_or("")
                                        .to_ascii_lowercase(),
                                    attr: "srcset".into(),
                                    url_span,
                                    tag_span: span_to_range(tag.span.start, tag.span.end),
                                    line: line_of_offset(html, tag.span.start),
                                    action,
                                });
                            }
                        }
                    }
                } else if is_local_link(trimmed) {
                    let resolved = resolve_href(
                        doc_href,
                        doc_is_index,
                        trimmed,
                        &mut scratch,
                        &mut decode_buf,
                    );
                    if !href_set.contains(resolved) {
                        let url_span = find_value_in_attr(raw, attr.span.start, attr_value);
                        let action = action_for_tag(tag_name);
                        broken.push(BrokenLink {
                            url: attr_value.to_string(),
                            tag: std::str::from_utf8(tag_name)
                                .unwrap_or("")
                                .to_ascii_lowercase(),
                            attr: attr_name.to_string(),
                            url_span,
                            tag_span: span_to_range(tag.span.start, tag.span.end),
                            line: line_of_offset(html, tag.span.start),
                            action,
                        });
                    }
                }
            }
        }
    }

    CleanResult {
        broken_links: broken,
        error: None,
    }
}

fn action_for_tag(tag: &[u8]) -> &'static str {
    match tag {
        b"a" | b"video" | b"audio" | b"object" | b"iframe" => "unwrap",
        _ => "remove",
    }
}

fn is_void(tag: &[u8]) -> bool {
    VOID_ELEMENTS.contains(tag)
}

fn parse_srcset_entries(raw: &str) -> Vec<(String, Option<String>)> {
    let mut entries = Vec::new();
    for part in raw.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let tokens: Vec<&str> = part.split_whitespace().collect();
        if tokens.is_empty() {
            continue;
        }
        let url = tokens[0];
        let descriptor = if tokens.len() > 1 {
            Some(tokens[1..].join(" "))
        } else {
            None
        };
        entries.push((url.to_string(), descriptor));
    }
    entries
}

#[derive(Debug)]
pub struct RemovalOp {
    pub span: Range<usize>,
    #[allow(dead_code)]
    pub description: String,
}

pub fn plan_removals(html: &str, broken_links: &[BrokenLink]) -> Vec<RemovalOp> {
    if broken_links.is_empty() {
        return Vec::new();
    }

    let mut removals: Vec<RemovalOp> = Vec::new();

    struct OpenEl {
        name: Vec<u8>,
        broken: bool,
        start_tag_end: usize,
        action: &'static str,
    }
    let mut stack: Vec<OpenEl> = Vec::new();
    let mut pending_broken_a: Option<usize> = None;

    let tokenizer = Tokenizer::new_with_emitter(html, DefaultEmitter::<usize>::new_with_span());

    for token_result in tokenizer {
        let token = match token_result {
            Ok(t) => t,
            Err(_) => continue,
        };

        match token {
            html5gum::Token::StartTag(tag) => {
                let tag_name = &tag.name[..];
                let is_self_closing = tag.self_closing;

                let broken_entry = broken_links
                    .iter()
                    .find(|b| b.tag_span.start == tag.span.start);

                if let Some(b) = broken_entry {
                    removals.push(RemovalOp {
                        span: span_to_range(tag.span.start, tag.span.end),
                        description: format!(
                            "remove <{}> start tag (broken {} link to {})",
                            b.tag, b.attr, b.url
                        ),
                    });

                    if is_self_closing || is_void(tag_name) {
                    } else if tag_name == b"a" {
                        pending_broken_a = Some(tag.span.start);
                    } else if tag_name == b"script" {
                        stack.push(OpenEl {
                            name: tag_name.to_vec(),
                            broken: true,
                            start_tag_end: tag.span.end,
                            action: "remove",
                        });
                    } else {
                        stack.push(OpenEl {
                            name: tag_name.to_vec(),
                            broken: true,
                            start_tag_end: tag.span.end,
                            action: "unwrap",
                        });
                    }
                }
            }
            html5gum::Token::EndTag(tag) => {
                let tag_name = &tag.name[..];

                if tag_name == b"a"
                    && let Some(start_pos) = pending_broken_a.take()
                    && broken_links
                        .iter()
                        .any(|b| b.tag == "a" && b.tag_span.start == start_pos)
                {
                    removals.push(RemovalOp {
                        span: span_to_range(tag.span.start, tag.span.end),
                        description: "remove </a> end tag (broken link)".into(),
                    });
                }

                let mut pop_idx: Option<usize> = None;
                for i in (0..stack.len()).rev() {
                    if stack[i].name == tag_name {
                        pop_idx = Some(i);
                        break;
                    }
                }

                if let Some(idx) = pop_idx {
                    let open = &stack[idx];
                    if open.broken {
                        if open.action == "remove" {
                            removals.push(RemovalOp {
                                span: open.start_tag_end..tag.span.end,
                                description: format!(
                                    "remove <{}> body and </{}> (broken src)",
                                    String::from_utf8_lossy(&open.name),
                                    String::from_utf8_lossy(&open.name)
                                ),
                            });
                        } else {
                            removals.push(RemovalOp {
                                span: span_to_range(tag.span.start, tag.span.end),
                                description: format!(
                                    "remove </{}> end tag (broken link)",
                                    String::from_utf8_lossy(&open.name)
                                ),
                            });
                        }
                    }
                    stack.truncate(idx);
                }
            }
            _ => {}
        }
    }

    removals.sort_by_key(|r| std::cmp::Reverse(r.span.start));
    removals
}

fn apply_removals(content: &mut String, removals: &[RemovalOp]) {
    for r in removals {
        content.replace_range(r.span.clone(), "");
    }
}

/// Clean a single HTML file. Returns the scan result (broken links found).
pub fn clean_file(
    path: &Path,
    root: &Path,
    href_set: &FxHashSet<String>,
    dry_run: bool,
) -> Result<CleanResult, String> {
    let content =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let rel = path.strip_prefix(root).unwrap_or(path);
    let rel_str = rel.to_string_lossy().to_string();

    let result = scan_file(&rel_str, &content, href_set);
    if let Some(err) = &result.error {
        return Err(format!("{}: {err}", path.display()));
    }

    if !result.broken_links.is_empty() && !dry_run {
        let removals = plan_removals(&content, &result.broken_links);
        let mut modified = content.clone();
        apply_removals(&mut modified, &removals);

        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &modified).map_err(|e| format!("write tmp: {e}"))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("rename: {e}"))?;
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn make_set(root: &Path) -> FxHashSet<String> {
        build_href_set(root)
    }

    #[test]
    fn test_is_local_link() {
        assert!(is_local_link("../picture/926.html"));
        assert!(is_local_link("picture/926.html"));
        assert!(is_local_link("/assets/logo.png"));
        assert!(is_local_link("about.html"));
        assert!(!is_local_link(""));
        assert!(is_local_link("#section"));
        assert!(!is_local_link("http://example.com"));
        assert!(!is_local_link("https://example.com"));
        assert!(!is_local_link("//example.com/foo"));
        assert!(!is_local_link("mailto:user@example.com"));
        assert!(!is_local_link("javascript:void(0)"));
        assert!(!is_local_link("data:image/png;base64,abc"));
    }

    #[test]
    fn test_is_external_link() {
        assert!(is_external_link("http://example.com"));
        assert!(is_external_link("https://example.com"));
        assert!(is_external_link("//example.com/foo"));
        assert!(is_external_link("mailto:user@example.com"));
        assert!(is_external_link("tel:+1234567890"));
        assert!(is_external_link("ftp://example.com/file"));
        assert!(!is_external_link(""));
        assert!(!is_external_link("../foo.html"));
        assert!(!is_external_link("foo.html"));
        assert!(!is_external_link("/absolute/path"));
        assert!(!is_external_link("#fragment"));
        assert!(!is_external_link("?query"));
    }

    #[test]
    fn test_build_href_set() {
        let tmpdir = tempfile::tempdir().unwrap();
        let root = tmpdir.path();
        std::fs::create_dir_all(root.join("picture")).unwrap();
        std::fs::write(root.join("picture/926.html"), "test").unwrap();
        std::fs::create_dir_all(root.join("glossary")).unwrap();
        std::fs::write(root.join("glossary/index.html"), "test").unwrap();
        std::fs::write(root.join("home.html"), "test").unwrap();
        std::fs::write(root.join("data.txt"), "test").unwrap();

        let set = make_set(root);
        assert!(set.contains("picture/926.html"));
        assert!(set.contains("glossary")); // index.html stripped
        assert!(set.contains("home.html"));
        assert!(set.contains("data.txt")); // non-HTML files too
    }

    #[test]
    fn test_build_href_set_root_index() {
        let tmpdir = tempfile::tempdir().unwrap();
        let root = tmpdir.path();
        std::fs::write(root.join("index.html"), "test").unwrap();

        let set = make_set(root);
        assert!(set.contains("")); // root index → empty string
    }

    #[test]
    fn test_resolve_href_relative() {
        let mut scratch = String::new();
        let mut decode = String::new();
        // From material/1642.html, link to ../picture/926.html
        let result = resolve_href(
            "material/1642.html",
            false,
            "../picture/926.html",
            &mut scratch,
            &mut decode,
        );
        assert_eq!(result, "picture/926.html");
    }

    #[test]
    fn test_resolve_href_same_dir() {
        let mut scratch = String::new();
        let mut decode = String::new();
        let result = resolve_href(
            "material/1642.html",
            false,
            "list.html",
            &mut scratch,
            &mut decode,
        );
        assert_eq!(result, "material/list.html");
    }

    #[test]
    fn test_resolve_href_from_index() {
        let mut scratch = String::new();
        let mut decode = String::new();
        // From glossary/index.html, link to list.html
        let result = resolve_href("glossary", true, "list.html", &mut scratch, &mut decode);
        assert_eq!(result, "glossary/list.html");
    }

    #[test]
    fn test_resolve_href_absolute() {
        let mut scratch = String::new();
        let mut decode = String::new();
        let result = resolve_href(
            "deep/nested/file.html",
            false,
            "/home.html",
            &mut scratch,
            &mut decode,
        );
        assert_eq!(result, "home.html");
    }

    #[test]
    fn test_resolve_href_empty() {
        let mut scratch = String::new();
        let mut decode = String::new();
        let result = resolve_href("material/1642.html", false, "", &mut scratch, &mut decode);
        assert_eq!(result, "material/1642.html");
    }

    #[test]
    fn test_resolve_href_empty_from_index() {
        let mut scratch = String::new();
        let mut decode = String::new();
        let result = resolve_href("glossary", true, "", &mut scratch, &mut decode);
        assert_eq!(result, "glossary");
    }

    #[test]
    fn test_resolve_href_percent_encoded() {
        let mut scratch = String::new();
        let mut decode = String::new();
        // %28 = (, %29 = ) — hyperlink decodes these before resolution
        let result = resolve_href(
            "hazard/19.html",
            false,
            "man-made+vitreous+fibers+%28mmvf%29+toxicology.html",
            &mut scratch,
            &mut decode,
        );
        assert_eq!(
            result,
            "hazard/man-made+vitreous+fibers+(mmvf)+toxicology.html"
        );
    }

    #[test]
    fn test_resolve_href_fragment_self_ref() {
        let mut scratch = String::new();
        let mut decode = String::new();
        let result = resolve_href(
            "material/1642.html",
            false,
            "#section",
            &mut scratch,
            &mut decode,
        );
        assert_eq!(result, "material/1642.html");
    }

    /// Regression: `%23` is a percent-encoded `#` and must survive as a literal
    /// `#` in the path, not be stripped as a fragment separator.
    #[test]
    fn test_resolve_href_encoded_hash_in_filename() {
        let mut scratch = String::new();
        let mut decode = String::new();
        let result = resolve_href(
            "material/1246.html",
            false,
            "%231+q-rok.html",
            &mut scratch,
            &mut decode,
        );
        assert_eq!(result, "material/#1+q-rok.html");
    }

    /// Regression: `%23` should also work with relative paths.
    #[test]
    fn test_resolve_href_encoded_hash_relative() {
        let mut scratch = String::new();
        let mut decode = String::new();
        let result = resolve_href(
            "hazard/317.html",
            false,
            "../material/%232280+clay.html",
            &mut scratch,
            &mut decode,
        );
        assert_eq!(result, "material/#2280+clay.html");
    }

    /// A raw `#` in the href (not percent-encoded) IS a fragment separator.
    #[test]
    fn test_resolve_href_raw_hash_is_fragment() {
        let mut scratch = String::new();
        let mut decode = String::new();
        // This should be treated as a self-reference + fragment, not a file lookup.
        let result = resolve_href(
            "material/1246.html",
            false,
            "#1+q-rok.html",
            &mut scratch,
            &mut decode,
        );
        assert_eq!(result, "material/1246.html");
    }

    /// When the file with an encoded hash in the name actually exists, the
    /// link should NOT be reported as broken.
    #[test]
    fn test_scan_encoded_hash_file_exists() {
        let tmpdir = tempfile::tempdir().unwrap();
        let root = tmpdir.path();
        std::fs::create_dir_all(root.join("material")).unwrap();
        // Create the file with a literal # in its name.
        std::fs::write(root.join("material/#1+q-rok.html"), "test").unwrap();
        let set = make_set(root);

        // href uses %23 to encode the #.
        let html = r#"<a href="%231+q-rok.html">link</a>"#;
        let result = scan_file("material/1246.html", html, &set);
        assert_eq!(result.broken_links.len(), 0);
    }

    /// When the file does NOT exist, the link IS broken.
    #[test]
    fn test_scan_encoded_hash_file_missing() {
        let tmpdir = tempfile::tempdir().unwrap();
        let root = tmpdir.path();
        std::fs::create_dir_all(root.join("material")).unwrap();
        // Do NOT create the target file.
        let set = make_set(root);

        let html = r#"<a href="%231+q-rok.html">link</a>"#;
        let result = scan_file("material/1246.html", html, &set);
        assert_eq!(result.broken_links.len(), 1);
        assert_eq!(result.broken_links[0].url, "%231+q-rok.html");
    }

    #[test]
    fn test_scan_a_href_broken() {
        let tmpdir = tempfile::tempdir().unwrap();
        let root = tmpdir.path();
        std::fs::create_dir_all(root.join("material")).unwrap();
        std::fs::create_dir_all(root.join("media")).unwrap();
        std::fs::write(root.join("media/ok.jpg"), "fake").unwrap();
        // Note: picture/926.html does NOT exist
        let set = make_set(root);

        let html = r#"<a href="../picture/926.html"><img src="../media/ok.jpg"></a>"#;
        let result = scan_file("material/test.html", html, &set);
        assert!(result.error.is_none());
        assert_eq!(result.broken_links.len(), 1);
        let b = &result.broken_links[0];
        assert_eq!(b.tag, "a");
        assert_eq!(b.attr, "href");
        assert_eq!(b.url, "../picture/926.html");
        assert_eq!(b.action, "unwrap");
    }

    #[test]
    fn test_scan_img_src_broken() {
        let tmpdir = tempfile::tempdir().unwrap();
        let root = tmpdir.path();
        let set = make_set(root);

        let html = r#"<img src="missing.jpg" alt="x">"#;
        let result = scan_file("test.html", html, &set);
        assert!(result.error.is_none());
        assert_eq!(result.broken_links.len(), 1);
        let b = &result.broken_links[0];
        assert_eq!(b.tag, "img");
        assert_eq!(b.action, "remove");
    }

    #[test]
    fn test_scan_ignores_valid_link() {
        let tmpdir = tempfile::tempdir().unwrap();
        let root = tmpdir.path();
        std::fs::write(root.join("about.html"), "test").unwrap();
        let set = make_set(root);

        let html = r#"<a href="about.html">About</a>"#;
        let result = scan_file("test.html", html, &set);
        assert_eq!(result.broken_links.len(), 0);
    }

    #[test]
    fn test_scan_ignores_remote() {
        let tmpdir = tempfile::tempdir().unwrap();
        let root = tmpdir.path();
        let set = make_set(root);

        let html = r#"<a href="https://example.com/page.html">link</a>"#;
        let result = scan_file("test.html", html, &set);
        assert_eq!(result.broken_links.len(), 0);
    }

    #[test]
    fn test_scan_ignores_fragment() {
        let mut set = FxHashSet::default();
        // The document's own href must be in the set for self-references to be valid.
        set.insert("test.html".to_string());

        let html = "<a href=\"#section\">link</a>";
        let result = scan_file("test.html", html, &set);
        assert_eq!(result.broken_links.len(), 0);
    }

    #[test]
    fn test_scan_index_html_link() {
        let tmpdir = tempfile::tempdir().unwrap();
        let root = tmpdir.path();
        // Create glossary/index.html and a page that links to it
        std::fs::create_dir_all(root.join("glossary")).unwrap();
        std::fs::write(root.join("glossary/index.html"), "test").unwrap();
        std::fs::create_dir_all(root.join("material")).unwrap();
        let set = make_set(root);

        // Link to ../glossary/ (with trailing slash) should resolve to "glossary"
        let html = r#"<a href="../glossary/">Glossary</a>"#;
        let result = scan_file("material/test.html", html, &set);
        assert_eq!(result.broken_links.len(), 0);
    }

    #[test]
    fn test_plan_removals_unwrap_a() {
        let html = r#"<p><a href="../broken.html">click <b>here</b></a></p>"#;
        // Use empty set so all links are broken
        let set = FxHashSet::default();
        let result = scan_file("test.html", html, &set);

        let removals = plan_removals(html, &result.broken_links);
        assert_eq!(removals.len(), 2);

        let mut modified = html.to_string();
        apply_removals(&mut modified, &removals);
        assert_eq!(modified, "<p>click <b>here</b></p>");
    }

    #[test]
    fn test_plan_removals_remove_img() {
        let html = r#"<div><img src="broken.jpg" alt="x"></div>"#;
        let set = FxHashSet::default();
        let result = scan_file("test.html", html, &set);

        let removals = plan_removals(html, &result.broken_links);
        assert_eq!(removals.len(), 1);

        let mut modified = html.to_string();
        apply_removals(&mut modified, &removals);
        assert_eq!(modified, "<div></div>");
    }

    #[test]
    fn test_plan_removals_script_src_broken() {
        let html = r#"<script src="broken.js"></script>"#;
        let set = FxHashSet::default();
        let result = scan_file("test.html", html, &set);

        let removals = plan_removals(html, &result.broken_links);
        assert_eq!(result.broken_links.len(), 1);
        assert_eq!(result.broken_links[0].tag, "script");

        let mut modified = html.to_string();
        apply_removals(&mut modified, &removals);
        assert_eq!(modified.trim(), "");
    }

    #[test]
    fn test_plan_removals_multiple_broken() {
        let html = concat!(
            r#"<a href="../broken.html"><img src="broken.jpg"></a>"#,
            r#"<a href="also-broken.html">text</a>"#,
        );
        let set = FxHashSet::default();
        let result = scan_file("test.html", html, &set);

        let removals = plan_removals(html, &result.broken_links);
        let mut modified = html.to_string();
        apply_removals(&mut modified, &removals);
        assert!(!modified.contains("<a "));
        assert!(!modified.contains("</a>"));
        assert!(!modified.contains("<img "));
        assert!(modified.contains("text"));
    }

    #[test]
    fn test_line_of_offset() {
        let source = "line1\nline2\nline3\nline4";
        assert_eq!(line_of_offset(source, 0), 1);
        assert_eq!(line_of_offset(source, 6), 2);
        assert_eq!(line_of_offset(source, 12), 3);
    }
}
