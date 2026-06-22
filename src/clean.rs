use rustc_hash::FxHashSet;

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
pub(crate) fn is_local_link(url: &str) -> bool {
    if url.is_empty() {
        return false;
    }
    !is_external_link(url)
}

/// Decode numeric HTML entities like `&#123;` or `&#x1F600;`.
fn decode_numeric_entity(s: &str) -> Option<char> {
    if s.len() < 4 || !s.ends_with(';') {
        return None;
    }
    let inner = &s[2..s.len() - 1]; // strip "&#" and ";"
    if let Some(hex) = inner.strip_prefix('x').or_else(|| inner.strip_prefix('X')) {
        u32::from_str_radix(hex, 16).ok().and_then(std::char::from_u32)
    } else {
        inner.parse::<u32>().ok().and_then(std::char::from_u32)
    }
}

/// Decode percent-encoding and common HTML entities into the provided buffer.
/// Returns the decoded slice.
fn decode_url_into<'a>(input: &str, buf: &'a mut String) -> &'a str {
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
        } else if bytes[i] == b'&' {
            if bytes[i..].starts_with(b"&amp;") {
                buf.push('&');
                i += 5;
                continue;
            } else if bytes[i..].starts_with(b"&lt;") {
                buf.push('<');
                i += 4;
                continue;
            } else if bytes[i..].starts_with(b"&gt;") {
                buf.push('>');
                i += 4;
                continue;
            } else if bytes[i..].starts_with(b"&quot;") {
                buf.push('"');
                i += 6;
                continue;
            } else if bytes[i..].starts_with(b"&apos;") {
                buf.push('\'');
                i += 6;
                continue;
            } else if bytes[i..].starts_with(b"&#") {
                if let Some(semi) = bytes[i..].iter().position(|&b| b == b';') {
                    let entity_str =
                        std::str::from_utf8(&bytes[i..=i + semi]).unwrap_or("");
                    if let Some(c) = decode_numeric_entity(entity_str) {
                        buf.push(c);
                        i += semi + 1;
                        continue;
                    }
                }
            }
        }
        buf.push(bytes[i] as char);
        i += 1;
    }
    buf
}

/// Decode HTML entities only (no percent-decoding) into the provided buffer.
fn decode_html_entities_into<'a>(input: &str, buf: &'a mut String) -> &'a str {
    buf.clear();
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'&' {
            if bytes[i..].starts_with(b"&amp;") {
                buf.push('&');
                i += 5;
                continue;
            } else if bytes[i..].starts_with(b"&lt;") {
                buf.push('<');
                i += 4;
                continue;
            } else if bytes[i..].starts_with(b"&gt;") {
                buf.push('>');
                i += 4;
                continue;
            } else if bytes[i..].starts_with(b"&quot;") {
                buf.push('"');
                i += 6;
                continue;
            } else if bytes[i..].starts_with(b"&apos;") {
                buf.push('\'');
                i += 6;
                continue;
            } else if bytes[i..].starts_with(b"&#") {
                if let Some(semi) = bytes[i..].iter().position(|&b| b == b';') {
                    let entity_str =
                        std::str::from_utf8(&bytes[i..=i + semi]).unwrap_or("");
                    if let Some(c) = decode_numeric_entity(entity_str) {
                        buf.push(c);
                        i += semi + 1;
                        continue;
                    }
                }
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
pub(crate) fn resolve_href<'a>(
    doc_href: &str,
    doc_is_index: bool,
    raw_href: &str,
    scratch: &'a mut String,
    decode_buf: &'a mut String,
) -> &'a str {
    let trimmed = raw_href.trim();

    // Entity-decode HTML entities FIRST so that &#64; → @ before fragment
    // stripping sees the raw # in &#64; as a fragment separator.
    // Use scratch for the entity-decoded intermediate.
    let decoded = decode_html_entities_into(trimmed, scratch);

    // Strip fragment and query from the entity-decoded string.
    // `%23` must survive here as a literal `#` in the path; it will be
    // percent-decoded in the next step.
    let qs = decoded.find(&['?', '#'][..]).unwrap_or(decoded.len());
    let raw_path = &decoded[..qs];

    // Percent-decode the path portion into decode_buf.
    let path = decode_url_into(raw_path, decode_buf);

    // Now scratch can be reused for the resolution output.
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

/// Like `resolve_href` but does NOT percent-decode the URL path.
/// This is the fallback for filesystems where filenames were written with
/// percent-encoded bytes (e.g. `grab` preserves URL encoding on disk).
pub(crate) fn resolve_href_raw<'a>(
    doc_href: &str,
    doc_is_index: bool,
    raw_href: &str,
    scratch: &'a mut String,
) -> &'a str {
    let trimmed = raw_href.trim();

    // Entity-decode HTML entities FIRST so that &#64; → @ before fragment
    // stripping sees the raw # in &#64; as a fragment separator.
    let mut entity_buf;
    let decoded: &str = if trimmed.contains('&') {
        entity_buf = String::with_capacity(trimmed.len());
        decode_html_entities_into(trimmed, &mut entity_buf);
        &entity_buf
    } else {
        trimmed
    };

    let qs = decoded.find(&['?', '#'][..]).unwrap_or(decoded.len());
    let raw_path = &decoded[..qs];

    scratch.clear();

    if is_external_link(raw_path) {
        scratch.push_str(raw_path);
        return scratch;
    }
    if let Some(stripped) = raw_path.strip_prefix('/') {
        scratch.push_str(stripped);
        return scratch;
    }

    scratch.push_str(doc_href);
    if doc_is_index {
        scratch.push('/');
    }

    if raw_path.is_empty() {
        if scratch.ends_with('/') {
            scratch.pop();
        }
        return scratch;
    }

    if let Some(pos) = scratch.rfind('/') {
        scratch.truncate(pos);
    } else {
        scratch.clear();
    }

    let mut components = raw_path.split('/').peekable();
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

/// Check whether a local URL resolves to a file that exists in `href_set`.
/// Tries percent-decoded resolution first, then falls back to raw (undecoded)
/// resolution for filesystems where filenames were written with percent-encoded
/// bytes (e.g. `grab` preserves URL encoding on disk).
pub(crate) fn link_exists(
    doc_href: &str,
    doc_is_index: bool,
    raw_href: &str,
    scratch: &mut String,
    decode_buf: &mut String,
    href_set: &FxHashSet<String>,
) -> bool {
    let resolved = resolve_href(doc_href, doc_is_index, raw_href, scratch, decode_buf);
    if href_set.contains(resolved) {
        return true;
    }
    let raw = resolve_href_raw(doc_href, doc_is_index, raw_href, scratch);
    href_set.contains(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_resolve_href_relative() {
        let mut scratch = String::new();
        let mut decode = String::new();
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
        let result = resolve_href(
            "material/1246.html",
            false,
            "#1+q-rok.html",
            &mut scratch,
            &mut decode,
        );
        assert_eq!(result, "material/1246.html");
    }

    #[test]
    fn test_resolve_href_decodes_amp_entity() {
        let mut scratch = String::new();
        let mut decode = String::new();
        let result = resolve_href(
            "archives/3517.html",
            true,
            "../../_grab/tudou.com/v/ZmBu2R6WuJk/&amp;resourceId=0_05_05_99/v.swf",
            &mut scratch,
            &mut decode,
        );
        assert_eq!(
            result,
            "_grab/tudou.com/v/ZmBu2R6WuJk/&resourceId=0_05_05_99/v.swf"
        );
    }

    #[test]
    fn test_resolve_href_decodes_lt_gt_entities() {
        let mut scratch = String::new();
        let mut decode = String::new();
        let result = resolve_href(
            "dir/doc.html",
            false,
            "../files/&lt;readme&gt;.pdf",
            &mut scratch,
            &mut decode,
        );
        assert_eq!(result, "files/<readme>.pdf");
    }

    #[test]
    fn test_resolve_href_decodes_quot_entity() {
        let mut scratch = String::new();
        let mut decode = String::new();
        let result = resolve_href(
            "dir/doc.html",
            false,
            "&quot;quoted&quot;.jpg",
            &mut scratch,
            &mut decode,
        );
        assert_eq!(result, r#"dir/"quoted".jpg"#);
    }

    #[test]
    fn test_resolve_href_decodes_numeric_entity() {
        let mut scratch = String::new();
        let mut decode = String::new();
        let result = resolve_href(
            "dir/doc.html",
            false,
            "file&#64;at.pdf",
            &mut scratch,
            &mut decode,
        );
        assert_eq!(result, "dir/file@at.pdf");
    }

    #[test]
    fn test_resolve_href_decodes_hex_numeric_entity() {
        let mut scratch = String::new();
        let mut decode = String::new();
        let result = resolve_href(
            "dir/doc.html",
            false,
            "file&#x40;at.pdf",
            &mut scratch,
            &mut decode,
        );
        assert_eq!(result, "dir/file@at.pdf");
    }

    #[test]
    fn test_resolve_href_decodes_entity_and_percent_together() {
        let mut scratch = String::new();
        let mut decode = String::new();
        let result = resolve_href(
            "dir/doc.html",
            false,
            "&amp;name=%20value.jpg",
            &mut scratch,
            &mut decode,
        );
        assert_eq!(result, "dir/&name= value.jpg");
    }

    #[test]
    fn test_resolve_href_raw_decodes_amp_entity() {
        let mut scratch = String::new();
        let result = resolve_href_raw(
            "archives/3517.html",
            true,
            "../../_grab/tudou.com/v/ZmBu2R6WuJk/&amp;resourceId=0_05_05_99/v.swf",
            &mut scratch,
        );
        assert_eq!(
            result,
            "_grab/tudou.com/v/ZmBu2R6WuJk/&resourceId=0_05_05_99/v.swf"
        );
    }

    #[test]
    fn test_resolve_href_bare_ampersand_unchanged() {
        let mut scratch = String::new();
        let mut decode = String::new();
        let result = resolve_href(
            "dir/doc.html",
            false,
            "file&name.jpg",
            &mut scratch,
            &mut decode,
        );
        assert_eq!(result, "dir/file&name.jpg");
    }

    #[test]
    fn test_link_exists_with_amp_entity() {
        let mut hs = FxHashSet::default();
        // File on disk has literal &amp; — the decoded URL (with &) won't match.
        hs.insert(
            "_grab/tudou.com/v/ZmBu2R6WuJk/&amp;resourceId=0_05_05_99/v.swf".to_string(),
        );
        let mut scratch = String::new();
        let mut decode = String::new();
        // The HTML attribute has &amp; which decodes to &
        let exists = link_exists(
            "archives/3517.html",
            true,
            "../../_grab/tudou.com/v/ZmBu2R6WuJk/&amp;resourceId=0_05_05_99/v.swf",
            &mut scratch,
            &mut decode,
            &hs,
        );
        // The decoded URL (with &) does NOT match the disk file (with &amp;)
        assert!(!exists);
    }

    #[test]
    fn test_link_exists_with_amp_entity_decoded_path_matches() {
        let mut hs = FxHashSet::default();
        // File on disk has the decoded &
        hs.insert(
            "_grab/tudou.com/v/ZmBu2R6WuJk/&resourceId=0_05_05_99/v.swf".to_string(),
        );
        let mut scratch = String::new();
        let mut decode = String::new();
        let exists = link_exists(
            "archives/3517.html",
            true,
            "../../_grab/tudou.com/v/ZmBu2R6WuJk/&amp;resourceId=0_05_05_99/v.swf",
            &mut scratch,
            &mut decode,
            &hs,
        );
        assert!(exists);
    }

    #[test]
    fn test_link_exists_with_amp_entity_raw_fallback() {
        let mut hs = FxHashSet::default();
        // File on disk has literal &amp; in path. When percent_decode_into
        // handles entities, the decoded path won't match, but the raw
        // fallback ALSO entity-decodes (for consistency), so it also won't match.
        // The real fix is to rename the disk files. This test documents current behavior.
        hs.insert(
            "_grab/tudou.com/v/ZmBu2R6WuJk/&amp;resourceId=0_05_05_99/v.swf".to_string(),
        );
        let mut scratch = String::new();
        let mut decode = String::new();
        let exists = link_exists(
            "archives/3517.html",
            true,
            "../../_grab/tudou.com/v/ZmBu2R6WuJk/&resourceId=0_05_05_99/v.swf",
            &mut scratch,
            &mut decode,
            &hs,
        );
        // The URL already has & (no entity to decode), and the disk has &amp; — no match.
        assert!(!exists);
    }
}
