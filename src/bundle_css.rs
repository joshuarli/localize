use std::ops::Range;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct CssLink {
    pub href: String,
    pub span: Range<usize>,
    pub bundlable: bool,
}

/// Find all `<link rel="stylesheet">` tags in HTML.
/// Returns links with `bundlable: true` for those suitable for bundling
/// (media is "all", "screen", empty, or absent). Links with other media
/// values (e.g. "print", "only screen and (...)") are marked non-bundlable
/// and should be preserved as-is.
pub fn find_stylesheet_links(html: &str) -> Vec<CssLink> {
    let mut links = Vec::new();
    let bytes = html.as_bytes();
    // <link is 5 bytes
    let mut pos = 0;
    while pos + 5 <= bytes.len() {
        // Find next "<link"
        let tag_start = match bytes[pos..]
            .windows(5)
            .position(|w| w.eq_ignore_ascii_case(b"<link"))
        {
            Some(p) => pos + p,
            None => break,
        };
        // Must be at start of tag: preceding char (if any) must not be
        // alphanumeric (catches "xlink", "hreflink", etc.).
        if tag_start > 0 && bytes[tag_start - 1].is_ascii_alphanumeric() {
            pos = tag_start + 5;
            continue;
        }
        // Find closing >
        let tag_end = match bytes[tag_start..].iter().position(|&b| b == b'>') {
            Some(p) => tag_start + p + 1,
            None => break,
        };
        let tag_text = &html[tag_start..tag_end];

        if !has_stylesheet_rel(tag_text) {
            pos = tag_end;
            continue;
        }

        let href = match extract_attr_value(tag_text, "href") {
            Some(h) => h.to_string(),
            None => {
                pos = tag_end;
                continue;
            }
        };

        let media = extract_attr_value(tag_text, "media");
        let bundlable = is_bundlable_media(media);

        links.push(CssLink {
            href,
            span: tag_start..tag_end,
            bundlable,
        });

        pos = tag_end;
    }
    links
}

fn has_stylesheet_rel(tag: &str) -> bool {
    match extract_attr_value(tag, "rel") {
        Some(v) => v
            .split_ascii_whitespace()
            .any(|w| w.eq_ignore_ascii_case("stylesheet")),
        None => false,
    }
}

fn is_bundlable_media(media: Option<&str>) -> bool {
    match media {
        None => true,
        Some(m) => {
            let m = m.trim();
            m.is_empty() || m.eq_ignore_ascii_case("all") || m.eq_ignore_ascii_case("screen")
        }
    }
}

/// Extract an attribute value from an HTML tag string.
/// Handles double-quoted, single-quoted, and unquoted values.
fn extract_attr_value<'a>(tag: &'a str, attr_name: &str) -> Option<&'a str> {
    let bytes = tag.as_bytes();
    let attr_bytes = attr_name.as_bytes();
    let mut pos = 0;

    while pos < bytes.len() {
        while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos >= bytes.len() {
            break;
        }

        let remaining = bytes.len() - pos;
        if remaining >= attr_bytes.len()
            && bytes[pos..pos + attr_bytes.len()].eq_ignore_ascii_case(attr_bytes)
        {
            pos += attr_bytes.len();
            while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }
            if pos < bytes.len() && bytes[pos] == b'=' {
                pos += 1;
                while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
                    pos += 1;
                }
                if pos < bytes.len() {
                    if bytes[pos] == b'"' || bytes[pos] == b'\'' {
                        let quote = bytes[pos];
                        pos += 1;
                        let start = pos;
                        while pos < bytes.len() && bytes[pos] != quote {
                            pos += 1;
                        }
                        return Some(&tag[start..pos]);
                    } else {
                        let start = pos;
                        while pos < bytes.len()
                            && !bytes[pos].is_ascii_whitespace()
                            && bytes[pos] != b'>'
                        {
                            pos += 1;
                        }
                        if pos > start {
                            return Some(&tag[start..pos]);
                        }
                    }
                }
            }
        }

        while pos < bytes.len() && !bytes[pos].is_ascii_whitespace() && bytes[pos] != b'>' {
            pos += 1;
        }
        // If we landed on '>', no more attributes in this tag.
        if pos < bytes.len() && bytes[pos] == b'>' {
            break;
        }
    }

    None
}

/// Resolve a CSS href relative to the HTML file's directory, returning a
/// normalized path relative to the site root.
pub fn resolve_css_path(html_rel: &str, href: &str) -> String {
    let html_dir = Path::new(html_rel)
        .parent()
        .unwrap_or(Path::new(""))
        .to_string_lossy()
        .replace('\\', "/");

    let combined = if html_dir.is_empty() {
        href.to_string()
    } else {
        format!("{html_dir}/{href}")
    };

    let mut parts: Vec<&str> = Vec::new();
    for part in combined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    parts.join("/")
}

/// Compute the relative path from an HTML file's directory to a target.
pub fn compute_relative_path(html_file: &str, target: &str) -> String {
    let html_dir = Path::new(html_file).parent().unwrap_or(Path::new(""));
    let html_parts: Vec<&str> = html_dir
        .components()
        .map(|c| c.as_os_str().to_str().unwrap_or(""))
        .filter(|p| !p.is_empty())
        .collect();
    let target_parts: Vec<&str> = target
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();

    let common = html_parts
        .iter()
        .zip(target_parts.iter())
        .take_while(|(a, b)| a == b)
        .count();

    let up = html_parts.len() - common;
    let mut result = String::new();
    for _ in 0..up {
        result.push_str("../");
    }
    for part in &target_parts[common..] {
        if !result.is_empty() && !result.ends_with('/') {
            result.push('/');
        }
        result.push_str(part);
    }
    if result.is_empty() {
        target.to_string()
    } else {
        result
    }
}

pub struct BundleResult {
    pub concatenated: String,
    pub bundle_rel: String,
}

/// Strip CSS comments (`/* ... */`), including sourceMappingURL and
/// sourceURL annotations. Preserves everything outside comments byte-for-byte.
fn strip_css_comments(css: &str) -> String {
    let bytes = css.as_bytes();
    let mut out = String::with_capacity(css.len());
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            // Skip comment: find */
            i += 2;
            while i + 1 < bytes.len() {
                if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    i += 2;
                    break;
                }
                i += 1;
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// Concatenate CSS files in the given order, strip comments, and return the
/// concatenated content and fixed bundle path relative to the site root.
/// The caller is responsible for determining the correct cascade order.
pub fn bundle_css_files(
    root: &Path,
    css_files: &[String],
    bundle_dir: &str,
) -> Result<BundleResult, String> {
    let mut concatenated = String::new();

    for css_rel in css_files {
        let css_path = root.join(css_rel);
        match std::fs::read_to_string(&css_path) {
            Ok(content) => {
                let stripped = strip_css_comments(&content);
                let stripped = stripped.trim();
                if stripped.is_empty() {
                    continue;
                }
                if !concatenated.is_empty() {
                    concatenated.push('\n');
                }
                concatenated.push_str(stripped);
            }
            Err(e) => {
                eprintln!("bundle-css: skipping {css_rel}: {e}");
            }
        }
    }

    if concatenated.is_empty() {
        return Err("no CSS content found to bundle".into());
    }

    let bundle_rel = format!("{bundle_dir}/bundle.css");

    Ok(BundleResult {
        concatenated,
        bundle_rel,
    })
}

/// Rewrite HTML: remove the `<link>` tags at the given spans (in reverse
/// order so earlier spans stay valid), then insert the bundle `<link>` tag
/// before `</head>` (with fallbacks for minified/malformed HTML).
///
/// Each span is extended forward to consume trailing whitespace (spaces,
/// tabs, newlines) so removal doesn't leave blank lines behind.
pub fn rewrite_html_for_bundle(
    html: &str,
    spans_to_remove: &[Range<usize>],
    bundle_href: &str,
) -> String {
    let mut modified = html.to_string();

    let mut spans: Vec<Range<usize>> = spans_to_remove.to_vec();
    spans.sort_by_key(|s| std::cmp::Reverse(s.start));
    for span in &spans {
        let mut end = span.end;
        // Consume trailing whitespace so we don't leave blank lines.
        for &b in modified.as_bytes()[end..].iter() {
            if b == b' ' || b == b'\t' || b == b'\r' || b == b'\n' {
                end += 1;
            } else {
                break;
            }
        }
        modified.replace_range(span.start..end, "");
    }

    let bundle_link = format!("<link rel=\"stylesheet\" href=\"{bundle_href}\">");

    let anchor = modified.find("</head>").or_else(|| {
        modified.match_indices("</head").find_map(|(i, _)| {
            let after = &modified[i + 6..];
            if after.is_empty() {
                return Some(i);
            }
            if after.as_bytes()[0].is_ascii_alphabetic() {
                None // skip </header>, </headings, etc.
            } else {
                Some(i)
            }
        })
    });

    if let Some(pos) = anchor {
        modified.insert_str(pos, &format!("\n{bundle_link}\n"));
    } else if let Some(pos) = modified.find("<body") {
        modified.insert_str(pos, &format!("{bundle_link}\n"));
    } else if let Some(pos) = modified.find("<html") {
        let close = modified[pos..]
            .find('>')
            .map(|e| pos + e + 1)
            .unwrap_or(pos);
        modified.insert_str(close, &format!("\n{bundle_link}\n"));
    } else {
        modified.insert_str(0, &format!("{bundle_link}\n"));
    }

    modified
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_stylesheet_link_basic() {
        let html = "<link rel=\"stylesheet\" href=\"foo.css\">";
        let links = find_stylesheet_links(html);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].href, "foo.css");
        assert!(links[0].bundlable);
        assert_eq!(&html[links[0].span.clone()], html);
    }

    #[test]
    fn test_find_stylesheet_link_media_all() {
        let html = "<link href=\"foo.css\" rel=\"stylesheet\" media=\"all\">";
        let links = find_stylesheet_links(html);
        assert_eq!(links.len(), 1);
        assert!(links[0].bundlable);
    }

    #[test]
    fn test_find_stylesheet_link_media_print_not_bundlable() {
        let html = "<link rel=\"stylesheet\" href=\"print.css\" media=\"print\">";
        let links = find_stylesheet_links(html);
        assert_eq!(links.len(), 1);
        assert!(!links[0].bundlable);
    }

    #[test]
    fn test_find_stylesheet_link_media_screen_bundlable() {
        let html = "<link rel=\"stylesheet\" href=\"screen.css\" media=\"screen\">";
        let links = find_stylesheet_links(html);
        assert_eq!(links.len(), 1);
        assert!(links[0].bundlable);
    }

    #[test]
    fn test_find_stylesheet_link_media_responsive_not_bundlable() {
        let html = "<link media=\"only screen and (max-width: 768px)\" href=\"mobile.css\" rel=\"stylesheet\">";
        let links = find_stylesheet_links(html);
        assert_eq!(links.len(), 1);
        assert!(!links[0].bundlable);
    }

    #[test]
    fn test_find_stylesheet_link_no_media_bundlable() {
        let html = "<link rel=stylesheet href=foo.css>";
        let links = find_stylesheet_links(html);
        assert_eq!(links.len(), 1);
        assert!(links[0].bundlable);
        assert_eq!(links[0].href, "foo.css");
    }

    #[test]
    fn test_find_stylesheet_link_unquoted() {
        let html = "<link rel=stylesheet href=foo.css>";
        let links = find_stylesheet_links(html);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].href, "foo.css");
    }

    #[test]
    fn test_mixed_links() {
        let html = concat!(
            "<link rel=\"stylesheet\" href=\"a.css\">",
            "<link rel=\"stylesheet\" href=\"b.css\" media=\"print\">",
            "<link rel=\"stylesheet\" href=\"c.css\">",
        );
        let links = find_stylesheet_links(html);
        assert_eq!(links.len(), 3);
        assert!(links[0].bundlable);
        assert!(!links[1].bundlable);
        assert!(links[2].bundlable);
    }

    #[test]
    fn test_no_stylesheet_links() {
        let html =
            "<link rel=\"icon\" href=\"favicon.ico\"><link rel=\"preload\" href=\"font.woff2\">";
        let links = find_stylesheet_links(html);
        assert!(links.is_empty());
    }

    #[test]
    fn test_not_link_tag() {
        let html = "<xlink href=\"x.css\" rel=\"stylesheet\">";
        let links = find_stylesheet_links(html);
        assert!(links.is_empty());
    }

    #[test]
    fn test_extract_attr_double_quoted() {
        assert_eq!(
            extract_attr_value("<link href=\"foo.css\" rel=\"stylesheet\">", "href"),
            Some("foo.css")
        );
    }

    #[test]
    fn test_extract_attr_single_quoted() {
        assert_eq!(
            extract_attr_value("<link href='bar.css' rel='stylesheet'>", "href"),
            Some("bar.css")
        );
    }

    #[test]
    fn test_extract_attr_unquoted() {
        assert_eq!(
            extract_attr_value("<link href=foo.css rel=stylesheet>", "href"),
            Some("foo.css")
        );
    }

    #[test]
    fn test_extract_attr_spaces_around_equals() {
        assert_eq!(
            extract_attr_value("<link href = \"foo.css\">", "href"),
            Some("foo.css")
        );
    }

    #[test]
    fn test_extract_attr_missing() {
        assert_eq!(
            extract_attr_value("<link rel=\"stylesheet\">", "href"),
            None
        );
    }

    #[test]
    fn test_resolve_css_path_same_dir() {
        let result = resolve_css_path("index.html", "localized-css/foo.css");
        assert_eq!(result, "localized-css/foo.css");
    }

    #[test]
    fn test_resolve_css_path_subdir() {
        let result = resolve_css_path("posts/about.html", "../localized-css/foo.css");
        assert_eq!(result, "localized-css/foo.css");
    }

    #[test]
    fn test_resolve_css_path_relative_down() {
        let result = resolve_css_path("index.html", "_grab/css/style.css");
        assert_eq!(result, "_grab/css/style.css");
    }

    #[test]
    fn test_resolve_css_path_deep_subdir() {
        let result = resolve_css_path(
            "wp-includes/blocks/navigation/index.html",
            "../../../localized-css/foo.css",
        );
        assert_eq!(result, "localized-css/foo.css");
    }

    #[test]
    fn test_compute_relative_same_dir() {
        let rel = compute_relative_path("index.html", "bundle/bundle.css");
        assert_eq!(rel, "bundle/bundle.css");
    }

    #[test]
    fn test_compute_relative_subdir() {
        let rel = compute_relative_path("posts/about.html", "bundle/bundle.css");
        assert_eq!(rel, "../bundle/bundle.css");
    }

    #[test]
    fn test_rewrite_html_removes_links_and_inserts_bundle() {
        let html = concat!(
            "<html><head>",
            "<link rel=\"stylesheet\" href=\"a.css\">",
            "<link rel=\"stylesheet\" href=\"b.css\">",
            "</head><body></body></html>",
        );
        let links = find_stylesheet_links(html);
        let spans: Vec<Range<usize>> = links.iter().map(|l| l.span.clone()).collect();
        let result = rewrite_html_for_bundle(html, &spans, "bundle/bundle.css");
        assert!(result.contains("href=\"bundle/bundle.css\""));
        assert!(!result.contains("a.css"));
        assert!(!result.contains("b.css"));
    }

    #[test]
    fn test_rewrite_html_preserves_non_bundlable() {
        let html = concat!(
            "<html><head>",
            "<link rel=\"stylesheet\" href=\"a.css\">",
            "<link rel=\"stylesheet\" href=\"b.css\" media=\"print\">",
            "</head><body></body></html>",
        );
        let links = find_stylesheet_links(html);
        let bundlable_spans: Vec<Range<usize>> = links
            .iter()
            .filter(|l| l.bundlable)
            .map(|l| l.span.clone())
            .collect();
        let result = rewrite_html_for_bundle(html, &bundlable_spans, "bundle/bundle.css");
        assert!(result.contains("href=\"bundle/bundle.css\""));
        assert!(!result.contains("a.css"));
        assert!(result.contains("b.css")); // preserved
        assert!(result.contains("media=\"print\""));
    }

    #[test]
    fn test_rewrite_html_cleans_trailing_whitespace() {
        // Three <link> tags on separate lines — removal should not leave
        // blank lines behind.
        let html = concat!(
            "<html>\n<head>\n",
            "    <link rel=\"stylesheet\" href=\"a.css\">\n",
            "    <link rel=\"stylesheet\" href=\"b.css\">\n",
            "    <link rel=\"stylesheet\" href=\"c.css\">\n",
            "</head>\n<body></body>\n</html>\n",
        );
        let links = find_stylesheet_links(html);
        let spans: Vec<Range<usize>> = links.iter().map(|l| l.span.clone()).collect();
        let result = rewrite_html_for_bundle(html, &spans, "bundle/bundle.css");

        // Should not contain leftover blank lines where links were removed.
        assert!(
            !result.contains("\n\n\n"),
            "no triple blank lines:\n{result}"
        );
        assert!(!result.contains("\n\n"), "no double blank lines:\n{result}");
        assert!(result.contains("href=\"bundle/bundle.css\""));
        assert!(!result.contains("a.css"));
        assert!(!result.contains("b.css"));
        assert!(!result.contains("c.css"));
    }

    #[test]
    fn test_bundle_css_files_concatenates_in_order() {
        let dir = std::env::temp_dir().join("bundle-test");
        let _ = std::fs::create_dir_all(&dir);
        let a = dir.join("a.css");
        let b = dir.join("b.css");
        std::fs::write(&a, ".a{color:red}").unwrap();
        std::fs::write(&b, ".b{color:blue}").unwrap();

        let files: Vec<String> = vec!["a.css".into(), "b.css".into()];

        let result = bundle_css_files(&dir, &files, "bundle").unwrap();
        assert_eq!(result.concatenated, ".a{color:red}\n.b{color:blue}");
        assert_eq!(result.bundle_rel, "bundle/bundle.css");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_bundle_css_files_respects_provided_order() {
        let dir = std::env::temp_dir().join("bundle-test-2");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("a.css"), ".a{}").unwrap();
        std::fs::write(dir.join("b.css"), ".b{}").unwrap();

        // Pass in reverse order — concatenation should respect it.
        let files: Vec<String> = vec!["b.css".into(), "a.css".into()];
        let result = bundle_css_files(&dir, &files, "bundle").unwrap();
        assert_eq!(result.concatenated, ".b{}\n.a{}");
        assert_eq!(result.bundle_rel, "bundle/bundle.css");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_strip_css_comments_source_url() {
        let css = "body{color:red}/*# sourceURL=theme.css */";
        let result = strip_css_comments(css);
        assert_eq!(result, "body{color:red}");
    }

    #[test]
    fn test_strip_css_comments_source_mapping_url() {
        let css = "/*# sourceMappingURL=style.css.map */\n.foo{display:none}";
        let result = strip_css_comments(css);
        assert_eq!(result, "\n.foo{display:none}");
    }

    #[test]
    fn test_strip_css_comments_multiline() {
        let css = "/* multi\nline\ncomment */\nbody{color:red}";
        let result = strip_css_comments(css);
        assert_eq!(result, "\nbody{color:red}");
    }

    #[test]
    fn test_strip_css_comments_preserves_non_comment_slashes() {
        let css = "url(data:image/svg+xml;base64,PHN2Zy8+DQo=)";
        let result = strip_css_comments(css);
        assert_eq!(result, css);
    }

    #[test]
    fn test_bundle_css_files_strips_comments() {
        let dir = std::env::temp_dir().join("bundle-comment-test");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("a.css"), "/* header */\n.a{color:red}/* inline */").unwrap();
        std::fs::write(dir.join("b.css"), ".b{color:blue}/*# sourceURL=b.css */").unwrap();

        let files: Vec<String> = vec!["a.css".into(), "b.css".into()];
        let result = bundle_css_files(&dir, &files, "bundle").unwrap();
        assert_eq!(result.concatenated, ".a{color:red}\n.b{color:blue}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_cascade_order_preserved() {
        // Two CSS files with equal-specificity rules on the same element.
        // The file that appears *last* in the concatenation wins.
        let dir = std::env::temp_dir().join("bundle-cascade-test");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("base.css"), "body{color:red}").unwrap();
        std::fs::write(dir.join("override.css"), "body{color:blue}").unwrap();

        // index.html references base.css first, then override.css.
        // override.css should appear after base.css in the bundle.
        let files: Vec<String> = vec!["base.css".into(), "override.css".into()];
        let result = bundle_css_files(&dir, &files, "bundle").unwrap();
        assert_eq!(
            result.concatenated, "body{color:red}\nbody{color:blue}",
            "override.css must come after base.css so its rule wins"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
