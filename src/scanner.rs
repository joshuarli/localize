use html5gum::Tokenizer;
use html5gum::emitters::default::DefaultEmitter;
use regex_lite::Regex;
use std::ops::Range;
use std::sync::LazyLock;

static CSS_URL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"url\(\s*["']?\s*(https?://[^"'\s()]+)\s*["']?\s*\)"#).unwrap());

#[derive(Debug, Clone)]
pub struct MediaReference {
    pub file_path: String,
    pub tag: String,
    pub attr: String,
    pub url: String,
    /// Byte range of the URL in the source file.
    pub span: Range<usize>,
    pub descriptor: Option<String>,
}

#[derive(Debug)]
pub struct ScanResult {
    pub references: Vec<MediaReference>,
    pub error: Option<String>,
}

/// Returns the attributes to check for a given tag, or None if uninteresting.
fn tag_attrs(tag: &[u8]) -> Option<&'static [&'static str]> {
    match tag {
        b"a" => Some(&["href"]),
        b"img" => Some(&["src", "srcset"]),
        b"source" => Some(&["src", "srcset"]),
        b"video" => Some(&["src"]),
        b"audio" => Some(&["src"]),
        b"track" => Some(&["src"]),
        b"object" => Some(&["data"]),
        b"script" => Some(&["src"]),
        b"link" => Some(&["href"]),
        _ => None,
    }
}

/// Check whether a URL path ends with a media file extension.
fn is_media_url(url: &str) -> bool {
    // Strip query string and fragment.
    let path = url
        .split('?')
        .next()
        .unwrap_or(url)
        .split('#')
        .next()
        .unwrap_or(url);
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "jpg"
            | "jpeg"
            | "png"
            | "gif"
            | "webp"
            | "svg"
            | "ico"
            | "bmp"
            | "mp4"
            | "webm"
            | "mov"
            | "avi"
            | "mkv"
            | "ogv"
            | "mp3"
            | "wav"
            | "ogg"
            | "flac"
            | "aac"
            | "m4a"
            | "pdf"
            | "woff"
            | "woff2"
            | "ttf"
            | "otf"
            | "eot"
            | "css"
            | "js"
    )
}

fn is_remote_url(url: &str) -> bool {
    if url.is_empty() {
        return false;
    }
    url.starts_with("http://") || url.starts_with("https://")
}

/// Given the raw attribute text (name=value) and its absolute span, find the
/// byte range of just the decoded `value` within the source.
/// Find the byte range of `value` within raw attribute text at `attr_start`.
/// Handles HTML entities: the decoded value from html5gum may differ from the
/// raw source (e.g. `&amp;` → `&`).
fn find_value_in_attr(raw: &str, attr_start: usize, value: &str) -> Option<Range<usize>> {
    // Fast path: literal match.
    if let Some(offset) = raw.find(value) {
        return Some(attr_start + offset..attr_start + offset + value.len());
    }
    // HTML entity in query string: `&amp;` is the decoded `&`.
    if value.contains('&') {
        let substituted = value.replace('&', "&amp;");
        if let Some(offset) = raw.find(&substituted) {
            return Some(attr_start + offset..attr_start + offset + substituted.len());
        }
    }
    None
}

pub fn scan_file(file_path: &str, html: &str) -> ScanResult {
    let mut refs: Vec<MediaReference> = Vec::new();

    let tokenizer = Tokenizer::new_with_emitter(html, DefaultEmitter::<usize>::new_with_span());

    // Track <style> element content.
    let mut in_style = false;
    let mut style_start = 0usize; // byte offset where <style> content begins

    for token_result in tokenizer {
        let token = match token_result {
            Ok(t) => t,
            Err(e) => {
                return ScanResult {
                    references: refs,
                    error: Some(format!("Parse error: {e}")),
                };
            }
        };

        match token {
            html5gum::Token::StartTag(tag) => {
                let tag_name = &tag.name[..]; // &[u8]

                // <style> element — track content range.
                if tag_name == b"style" {
                    in_style = true;
                    // Style content starts after the opening tag's '>'.
                    style_start = tag.span.end;
                    // Also check for remote URLs in inline style attrs on <style> itself (unlikely but correct).
                    for (name, attr) in &tag.attributes {
                        if &name[..] == b"style" {
                            let val = std::str::from_utf8(&attr[..]).unwrap_or("");
                            let raw = &html[attr.span.start..attr.span.end];
                            for m in CSS_URL_RE.captures_iter(val) {
                                if let Some(url_match) = m.get(1) {
                                    let url = url_match.as_str();
                                    if let Some(url_span) = find_value_in_attr(raw, attr.span.start, url) {
                                        refs.push(MediaReference {
                                            file_path: file_path.to_string(),
                                            tag: "style".into(),
                                            attr: "style".into(),
                                            url: url.to_string(),
                                            span: url_span,
                                            descriptor: None,
                                        });
                                    }
                                }
                            }
                        }
                    }
                    continue;
                }

                let attrs_to_check = tag_attrs(tag_name);
                let is_meta = tag_name == b"meta";

                // Quick skip if this tag can't possibly carry media references.
                let mut has_style_attr = false;
                if attrs_to_check.is_none() && !is_meta {
                    #[allow(clippy::for_kv_map)]
                    for (name, _) in &tag.attributes {
                        if &name[..] == b"style" {
                            has_style_attr = true;
                            break;
                        }
                    }
                    if !has_style_attr {
                        continue;
                    }
                }

                // Build a map of decoded attribute values for pattern matching.
                for (name, attr) in &tag.attributes {
                    let attr_name = std::str::from_utf8(&name[..]).unwrap_or("");
                    let attr_value = std::str::from_utf8(&attr[..]).unwrap_or("");
                    let raw = &html[attr.span.start..attr.span.end];

                    // 1. Standard media attributes (src, srcset, href, data, etc.)
                    if let Some(attrs) = attrs_to_check
                        && attrs.contains(&attr_name)
                    {
                        if attr_name == "srcset" {
                            for (url, descriptor) in parse_srcset_entries(attr_value) {
                                if is_remote_url(&url) {
                                    if let Some(url_span) = find_value_in_attr(raw, attr.span.start, &url) {
                                        refs.push(MediaReference {
                                            file_path: file_path.to_string(),
                                            tag: std::str::from_utf8(tag_name)
                                                .unwrap_or("")
                                                .to_string(),
                                            attr: "srcset".into(),
                                            url: url.to_string(),
                                            span: url_span,
                                            descriptor,
                                        });
                                    }
                                }
                            }
                        } else if is_remote_url(attr_value) {
                            // <a href> is for navigation, not media — only
                            // match if the URL points to a media file.
                            if tag_name == b"a" && !is_media_url(attr_value) {
                                continue;
                            }
                            if let Some(url_span) = find_value_in_attr(raw, attr.span.start, attr_value) {
                                refs.push(MediaReference {
                                    file_path: file_path.to_string(),
                                    tag: std::str::from_utf8(tag_name).unwrap_or("").to_string(),
                                    attr: attr_name.to_string(),
                                    url: attr_value.to_string(),
                                    span: url_span,
                                    descriptor: None,
                                });
                            }
                        }
                    }

                    // 2. Meta tag patterns (og:image, twitter:image)
                    if is_meta {
                        let is_og_image = attr_name == "property" && attr_value == "og:image";
                        let is_twitter_image = attr_name == "name" && attr_value == "twitter:image";
                        if is_og_image || is_twitter_image {
                            // Look for the content attribute.
                            for (cname, cattr) in &tag.attributes {
                                if &cname[..] == b"content" {
                                    let content_val = std::str::from_utf8(&cattr[..]).unwrap_or("");
                                    if is_remote_url(content_val) {
                                        let craw = &html[cattr.span.start..cattr.span.end];
                                        if let Some(url_span) =
                                            find_value_in_attr(craw, cattr.span.start, content_val)
                                        {
                                            refs.push(MediaReference {
                                                file_path: file_path.to_string(),
                                                tag: "meta".into(),
                                                attr: "content".into(),
                                                url: content_val.to_string(),
                                                span: url_span,
                                                descriptor: None,
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // 3. Inline style attributes on any tag.
                    if attr_name == "style" {
                        for m in CSS_URL_RE.captures_iter(attr_value) {
                            if let Some(url_match) = m.get(1) {
                                let url = url_match.as_str();
                                if let Some(url_span) = find_value_in_attr(raw, attr.span.start, url) {
                                    refs.push(MediaReference {
                                        file_path: file_path.to_string(),
                                        tag: std::str::from_utf8(tag_name).unwrap_or("").to_string(),
                                        attr: "style".into(),
                                        url: url.to_string(),
                                        span: url_span,
                                        descriptor: None,
                                    });
                                }
                            }
                        }
                    }
                }
            }

            html5gum::Token::EndTag(tag) if in_style && &tag.name[..] == b"style" => {
                in_style = false;
                // Scan collected style content for CSS url() references.
                if style_start > tag.span.start || style_start > html.len() || tag.span.start > html.len() {
                    continue;
                }
                let style_content = &html[style_start..tag.span.start];
                for m in CSS_URL_RE.captures_iter(style_content) {
                    if let Some(url_match) = m.get(1) {
                        let url = url_match.as_str();
                        let abs_start = style_start + url_match.start();
                        let abs_end = style_start + url_match.end();
                        refs.push(MediaReference {
                            file_path: file_path.to_string(),
                            tag: "style".into(),
                            attr: "css".into(),
                            url: url.to_string(),
                            span: abs_start..abs_end,
                            descriptor: None,
                        });
                    }
                }
            }

            _ => {}
        }
    }

    ScanResult {
        references: refs,
        error: None,
    }
}

/// Parse srcset entries. Returns Vec of (url, optional_descriptor).
/// Only returns entries with remote URLs.
fn parse_srcset_entries(raw: &str) -> Vec<(String, Option<String>)> {
    let mut entries = Vec::new();
    for part in raw.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        // Split into tokens: URL followed by optional descriptor.
        let tokens: Vec<&str> = part.split_whitespace().collect();
        if tokens.is_empty() {
            continue;
        }
        let url = tokens[0];
        if !is_remote_url(url) {
            continue;
        }
        let descriptor = if tokens.len() > 1 {
            Some(tokens[1..].join(" "))
        } else {
            None
        };
        entries.push((url.to_string(), descriptor));
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_remote_url() {
        assert!(is_remote_url("http://example.com/img.png"));
        assert!(is_remote_url("https://example.com/img.png"));
        assert!(!is_remote_url("images/photo.jpg"));
        assert!(!is_remote_url("/assets/banner.png"));
        assert!(!is_remote_url("data:image/png;base64,abc"));
        assert!(!is_remote_url("blob:http://example.com/123"));
        assert!(!is_remote_url("javascript:void(0)"));
        assert!(!is_remote_url("mailto:user@example.com"));
        assert!(!is_remote_url(""));
    }

    #[test]
    fn test_img_src() {
        let html = r#"<img src="https://cdn.example.com/logo.png" alt="logo">"#;
        let result = scan_file("test.html", html);
        assert!(result.error.is_none());
        assert_eq!(result.references.len(), 1);
        let r = &result.references[0];
        assert_eq!(r.tag, "img");
        assert_eq!(r.attr, "src");
        assert_eq!(r.url, "https://cdn.example.com/logo.png");
        // Verify the span extracts the correct URL.
        assert_eq!(
            &html[r.span.start..r.span.end],
            "https://cdn.example.com/logo.png"
        );
    }

    #[test]
    fn test_img_srcset() {
        let html = r#"<img srcset="https://a.com/s.jpg 400w, https://a.com/l.jpg 800w">"#;
        let result = scan_file("test.html", html);
        assert_eq!(result.references.len(), 2);
        assert_eq!(result.references[0].url, "https://a.com/s.jpg");
        assert_eq!(result.references[0].descriptor.as_deref(), Some("400w"));
        assert_eq!(result.references[1].url, "https://a.com/l.jpg");
        assert_eq!(result.references[1].descriptor.as_deref(), Some("800w"));
    }

    #[test]
    fn test_ignores_relative_src() {
        let html = r#"<img src="images/photo.jpg">"#;
        let result = scan_file("test.html", html);
        assert_eq!(result.references.len(), 0);
    }

    #[test]
    fn test_ignores_data_uri() {
        let html = r#"<img src="data:image/png;base64,abc">"#;
        let result = scan_file("test.html", html);
        assert_eq!(result.references.len(), 0);
    }

    #[test]
    fn test_meta_og_image() {
        let html = r#"<meta property="og:image" content="https://cdn.example.com/hero.png">"#;
        let result = scan_file("test.html", html);
        assert_eq!(result.references.len(), 1);
        assert_eq!(result.references[0].tag, "meta");
        assert_eq!(result.references[0].attr, "content");
        assert_eq!(result.references[0].url, "https://cdn.example.com/hero.png");
    }

    #[test]
    fn test_meta_twitter_image() {
        let html = r#"<meta name="twitter:image" content="https://cdn.example.com/hero.png">"#;
        let result = scan_file("test.html", html);
        assert_eq!(result.references.len(), 1);
        assert_eq!(result.references[0].url, "https://cdn.example.com/hero.png");
    }

    #[test]
    fn test_inline_style_url() {
        let html = r#"<div style="background: url(https://cdn.example.com/bg.png)"></div>"#;
        let result = scan_file("test.html", html);
        assert_eq!(result.references.len(), 1);
        assert_eq!(result.references[0].tag, "div");
        assert_eq!(result.references[0].attr, "style");
        assert_eq!(result.references[0].url, "https://cdn.example.com/bg.png");
        assert_eq!(
            &html[result.references[0].span.start..result.references[0].span.end],
            "https://cdn.example.com/bg.png"
        );
    }

    #[test]
    fn test_style_tag_content() {
        let html = "<style>.bg { background: url(https://cdn.example.com/bg.jpg); }</style>";
        let result = scan_file("test.html", html);
        assert_eq!(result.references.len(), 1);
        assert_eq!(result.references[0].tag, "style");
        assert_eq!(result.references[0].attr, "css");
        assert_eq!(result.references[0].url, "https://cdn.example.com/bg.jpg");
    }

    #[test]
    fn test_video_src() {
        let html = r#"<video src="https://media.example.com/video.mp4"></video>"#;
        let result = scan_file("test.html", html);
        assert_eq!(result.references.len(), 1);
        assert_eq!(result.references[0].tag, "video");
    }

    #[test]
    fn test_link_href() {
        let html = r#"<link rel="stylesheet" href="https://cdn.example.com/vendor.css">"#;
        let result = scan_file("test.html", html);
        assert_eq!(result.references.len(), 1);
        assert_eq!(result.references[0].tag, "link");
        assert_eq!(result.references[0].attr, "href");
    }

    #[test]
    fn test_script_src() {
        let html = r#"<script src="https://cdn.example.com/vendor.js"></script>"#;
        let result = scan_file("test.html", html);
        assert_eq!(result.references.len(), 1);
        assert_eq!(result.references[0].tag, "script");
        assert_eq!(result.references[0].attr, "src");
    }

    #[test]
    fn test_object_data() {
        let html = r#"<object data="https://docs.example.com/doc.pdf"></object>"#;
        let result = scan_file("test.html", html);
        assert_eq!(result.references.len(), 1);
        assert_eq!(result.references[0].tag, "object");
        assert_eq!(result.references[0].attr, "data");
    }

    #[test]
    fn test_duplicate_urls() {
        let html = r#"<img src="https://cdn.example.com/logo.png"><img src="https://cdn.example.com/logo.png">"#;
        let result = scan_file("test.html", html);
        assert_eq!(result.references.len(), 2);
        assert_eq!(result.references[0].url, result.references[1].url);
    }

    #[test]
    fn test_ignores_root_relative_src() {
        let html = r#"<img src="/assets/logo.png">"#;
        let result = scan_file("test.html", html);
        assert_eq!(result.references.len(), 0);
    }

    #[test]
    fn test_link_href_ignores_relative() {
        let html = r#"<link rel="stylesheet" href="local/style.css">"#;
        let result = scan_file("test.html", html);
        assert_eq!(result.references.len(), 0);
    }

    #[test]
    fn test_meta_other_property_ignored() {
        let html = r#"<meta property="og:title" content="https://cdn.example.com/title.png">"#;
        let result = scan_file("test.html", html);
        assert_eq!(result.references.len(), 0);
    }

    #[test]
    fn test_audio_src() {
        let html = r#"<audio src="https://media.example.com/audio.mp3"></audio>"#;
        let result = scan_file("test.html", html);
        assert_eq!(result.references.len(), 1);
        assert_eq!(result.references[0].tag, "audio");
    }

    #[test]
    fn test_source_src() {
        let html = r#"<source src="https://media.example.com/video.mp4">"#;
        let result = scan_file("test.html", html);
        assert_eq!(result.references.len(), 1);
        assert_eq!(result.references[0].tag, "source");
    }

    #[test]
    fn test_source_srcset() {
        let html = r#"<source srcset="https://a.com/b.webp 1x, https://a.com/b2x.webp 2x">"#;
        let result = scan_file("test.html", html);
        assert_eq!(result.references.len(), 2);
        assert_eq!(result.references[0].attr, "srcset");
    }

    #[test]
    fn test_track_src() {
        let html = r#"<track src="https://media.example.com/subtitles.vtt">"#;
        let result = scan_file("test.html", html);
        assert_eq!(result.references.len(), 1);
        assert_eq!(result.references[0].tag, "track");
    }

    #[test]
    fn test_style_url_only_within_block() {
        // The same URL appears in a data attribute OUTSIDE the style block.
        let html = concat!(
            "<style>.bg { background: url(https://cdn.example.com/a.jpg); }</style>\n",
            r#"<img data-fallback="https://cdn.example.com/a.jpg">"#,
        );
        let result = scan_file("test.html", html);
        let style_refs: Vec<_> = result
            .references
            .iter()
            .filter(|r| r.attr == "css")
            .collect();
        assert_eq!(style_refs.len(), 1);
        // Verify the span falls inside the <style> element.
        let style_end = html.find("</style>").unwrap();
        assert!(style_refs[0].span.start < style_end);
    }

    #[test]
    fn test_malformed_html_no_crash() {
        let html = r#"<img src="https://example.com/img.png""#;
        let result = scan_file("test.html", html);
        assert!(result.references.len() <= 1); // may or may not parse, but shouldn't crash
    }

    #[test]
    fn test_a_href() {
        let html =
            r#"<a href="https://s3-us-west-2.amazonaws.com/reference/images/pic.jpg">link</a>"#;
        let result = scan_file("test.html", html);
        assert_eq!(result.references.len(), 1);
        assert_eq!(result.references[0].tag, "a");
        assert_eq!(result.references[0].attr, "href");
        assert_eq!(
            result.references[0].url,
            "https://s3-us-west-2.amazonaws.com/reference/images/pic.jpg"
        );
    }

    #[test]
    fn test_a_href_ignores_relative() {
        let html = r#"<a href="about.html">link</a>"#;
        let result = scan_file("test.html", html);
        assert_eq!(result.references.len(), 0);
    }

    #[test]
    fn test_a_href_ignores_fragment() {
        let html = "<a href=\"#section\">link</a>";
        let result = scan_file("test.html", html);
        assert_eq!(result.references.len(), 0);
    }

    #[test]
    fn test_a_href_ignores_non_media() {
        // Social links, web pages — not media assets.
        let html = r#"<a href="https://www.facebook.com/insightlive">FB</a>"#;
        let result = scan_file("test.html", html);
        assert_eq!(result.references.len(), 0);
    }

    #[test]
    fn test_a_href_media_extension() {
        // <a> wrapping an image URL with a media extension should match.
        let html = r#"<a href="https://s3.amazonaws.com/bucket/photo.jpg">img</a>"#;
        let result = scan_file("test.html", html);
        assert_eq!(result.references.len(), 1);
        assert_eq!(result.references[0].tag, "a");
    }

    #[test]
    fn test_span_extraction() {
        let html = r#"<img src="https://example.com/img.png">"#;
        let result = scan_file("test.html", html);
        assert_eq!(result.references.len(), 1);
        let r = &result.references[0];
        let extracted = &html[r.span.start..r.span.end];
        assert_eq!(extracted, "https://example.com/img.png");
    }

    #[test]
    fn test_entity_in_url() {
        // &amp; in query string should not cause a panic.
        let html = r#"<a href="https://example.com/page?a=1&amp;b=2">link</a>"#;
        let result = scan_file("test.html", html);
        // Should find the reference (URL with query params is treated as a media-like URL
        // because it ends with "?a=1&amp;b=2" — actually the decoded value "?a=1&b=2"
        // has no extension, so is_media_url returns false for <a> tags. But this test
        // verifies the entity doesn't cause a panic regardless.
        assert!(result.error.is_none());
    }

    #[test]
    fn test_amp_entity_in_srcset() {
        let html =
            r#"<img srcset="https://a.com/img.jpg?w=400&amp;h=300 400w, https://a.com/img.jpg 800w">"#;
        let result = scan_file("test.html", html);
        assert!(result.error.is_none());
        assert_eq!(result.references.len(), 2);
        assert_eq!(
            result.references[0].url,
            "https://a.com/img.jpg?w=400&h=300"
        );
        let extracted = &html[result.references[0].span.start..result.references[0].span.end];
        assert_eq!(extracted, "https://a.com/img.jpg?w=400&amp;h=300");
    }
}
