use html5gum::Tokenizer;
use html5gum::emitters::default::DefaultEmitter;
use regex_lite::Regex;
use rustc_hash::FxHashSet;
use std::ops::Range;
use std::sync::{Arc, LazyLock};

static CSS_URL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"url\(\s*["']?\s*(https?://[^"'\s()]+)\s*["']?\s*\)"#).unwrap());

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaTag {
    A,
    Img,
    Link,
    Script,
    Style,
    Meta,
    Video,
    Audio,
    Source,
    Track,
    Object,
    Other(Box<str>),
}

impl MediaTag {
    fn from_bytes(bytes: &[u8]) -> Self {
        match bytes {
            b"a" => Self::A,
            b"img" => Self::Img,
            b"link" => Self::Link,
            b"script" => Self::Script,
            b"style" => Self::Style,
            b"meta" => Self::Meta,
            b"video" => Self::Video,
            b"audio" => Self::Audio,
            b"source" => Self::Source,
            b"track" => Self::Track,
            b"object" => Self::Object,
            other => Self::Other(
                String::from_utf8_lossy(other).into_owned().into_boxed_str(),
            ),
        }
    }
}

impl std::fmt::Display for MediaTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::A => f.write_str("a"),
            Self::Img => f.write_str("img"),
            Self::Link => f.write_str("link"),
            Self::Script => f.write_str("script"),
            Self::Style => f.write_str("style"),
            Self::Meta => f.write_str("meta"),
            Self::Video => f.write_str("video"),
            Self::Audio => f.write_str("audio"),
            Self::Source => f.write_str("source"),
            Self::Track => f.write_str("track"),
            Self::Object => f.write_str("object"),
            Self::Other(s) => f.write_str(s),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MediaAttr {
    Src,
    Href,
    Data,
    Content,
    Style,
    Css,
    Srcset,
}

impl MediaAttr {
    fn from_str(s: &str) -> Self {
        match s {
            "src" => Self::Src,
            "href" => Self::Href,
            "data" => Self::Data,
            "content" => Self::Content,
            "style" => Self::Style,
            "css" => Self::Css,
            "srcset" => Self::Srcset,
            _ => unreachable!("unknown media attr: {s}"),
        }
    }
}

impl std::fmt::Display for MediaAttr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Src => "src",
            Self::Href => "href",
            Self::Data => "data",
            Self::Content => "content",
            Self::Style => "style",
            Self::Css => "css",
            Self::Srcset => "srcset",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SrcsetDescriptor {
    /// Width descriptor, e.g. `400w`. Stored as the numeric value.
    Width(u16),
    /// Pixel density descriptor, e.g. `2x`. Stored as numerator × 100 (2x = 200).
    Density(u16),
}

impl SrcsetDescriptor {
    fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if let Some(n) = raw.strip_suffix('w').and_then(|s| s.parse::<u16>().ok()) {
            Some(Self::Width(n))
        } else {
            raw.strip_suffix('x')
                .and_then(|s| s.parse::<f64>().ok())
                .map(|n| Self::Density((n * 100.0) as u16))
        }
    }
}

impl std::fmt::Display for SrcsetDescriptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Width(n) => write!(f, "{n}w"),
            Self::Density(n) => {
                let d = *n as f64 / 100.0;
                if d.fract() == 0.0 {
                    write!(f, "{}x", d as u32)
                } else {
                    write!(f, "{d}x")
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct MediaReference {
    pub file_path: Arc<str>,
    pub tag: MediaTag,
    pub attr: MediaAttr,
    pub url: Box<str>,
    /// Byte range of the URL in the source file.
    pub span: Range<usize>,
    pub descriptor: Option<SrcsetDescriptor>,
    /// true if this is a local URL whose file does not exist on disk.
    pub broken: bool,
    /// 1-based line number of the URL in the source file.
    pub line: usize,
    /// 1-based column number of the URL in the source file.
    pub col: usize,
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

pub(crate) fn is_remote_url(url: &str) -> bool {
    if url.is_empty() {
        return false;
    }
    url.starts_with("http://") || url.starts_with("https://") || url.starts_with("//")
}

fn is_local_media_ref(url: &str) -> bool {
    if url.is_empty() {
        return false;
    }
    if url.starts_with('#') || url.starts_with('?') {
        return false;
    }
    if url.starts_with("data:")
        || url.starts_with("javascript:")
        || url.starts_with("mailto:")
    {
        return false;
    }
    true
}

/// Check whether a URL is an analytics/tracking beacon masquerading as a media URL.
/// These are typically 1×1 GIFs embedded in <img> tags for data collection,
/// not real media assets worth localizing.
/// Matches any host with a "pixel" subdomain (e.g. pixel.wp.com, pixel.facebook.com),
/// matching the same logic used by grab's `is_tracking_subdomain`.
fn is_tracking_url(url: &str) -> bool {
    let parsed = match url::Url::parse(url) {
        Ok(p) => p,
        Err(_) => return false,
    };
    let host = parsed.host_str().unwrap_or("");
    host.to_lowercase()
        .split('.')
        .any(|part| part == "pixel")
}

/// Check whether a local URL path represents a tracking pixel that was
/// rewritten by the grab tool (e.g. `_grab/pixel.wp.com/g__q-252f96.gif`).
/// When the HTML rewriter misses a tracking pixel, its src is rewritten to a
/// local path but the asset is never downloaded — producing a false broken
/// URL report. This check suppresses those false positives.
fn is_tracking_local_url(url: &str) -> bool {
    let path = url.trim_start_matches('/');
    let segments: Vec<&str> = path.split('/').collect();
    if let Some(pos) = segments.iter().position(|&s| s == "_grab") {
        if let Some(host) = segments.get(pos + 1) {
            return host.to_lowercase().split('.').any(|p| p == "pixel");
        }
    }
    false
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

pub fn scan_file(file_path: &str, html: &str, href_set: &FxHashSet<String>) -> ScanResult {
    // Fast path: skip files that can't possibly contain media references.
    if !html.contains("src=")
        && !html.contains("href=")
        && !html.contains("url(")
        && !html.contains("srcset=")
        && !html.contains("data=")
        && !html.contains("content=")
    {
        return ScanResult {
            references: Vec::new(),
            error: None,
        };
    }

    let mut refs: Vec<MediaReference> = Vec::new();

    // Compute canonical href for this document so we can resolve relative URLs.
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
    let mut scratch = String::new();
    let mut decode_buf = String::new();

    // Precompute line start offsets so we can map byte offset → line:col in O(log n).
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(html.match_indices('\n').map(|(i, _)| i + 1))
        .collect();

    let file_path: Arc<str> = Arc::from(file_path);
    let mut push_ref = |tag: MediaTag, attr: MediaAttr, url: &str, span: Range<usize>, descriptor: Option<SrcsetDescriptor>, broken: bool| {
        let line = match line_starts.binary_search(&span.start) {
            Ok(i) => i + 1,
            Err(i) => i,
        };
        let col = span.start - line_starts[line - 1] + 1;
        refs.push(MediaReference {
            file_path: Arc::clone(&file_path),
            tag,
            attr,
            url: url.to_string().into_boxed_str(),
            span,
            descriptor,
            broken,
            line,
            col,
        });
    };

    // Returns true if a local URL is missing from disk.
    let mut is_broken = |url: &str| -> bool {
        let resolved = crate::clean::resolve_href(
            &doc_href,
            doc_is_index,
            url,
            &mut scratch,
            &mut decode_buf,
        );
        !href_set.contains(resolved)
    };

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
                    for (name, attr) in &tag.attributes {
                        if &name[..] == b"style" {
                            let val = std::str::from_utf8(&attr[..]).unwrap_or("");
                            let raw = &html[attr.span.start..attr.span.end];
                            for m in CSS_URL_RE.captures_iter(val) {
                                if let Some(url_match) = m.get(1) {
                                    let url = url_match.as_str();
                                    if let Some(url_span) = find_value_in_attr(raw, attr.span.start, url) {
                                        push_ref(MediaTag::Style, MediaAttr::Style, url, url_span, None, false);
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
                                    if is_tracking_url(&url) {
                                        continue;
                                    }
                                    if let Some(url_span) = find_value_in_attr(raw, attr.span.start, &url) {
                                        push_ref(
                                            MediaTag::from_bytes(tag_name),
                                            MediaAttr::Srcset,
                                            &url,
                                            url_span,
                                            descriptor,
                                            false,
                                        );
                                    }
                                } else if is_local_media_ref(&url)
                                    && is_broken(&url)
                                    && !is_tracking_local_url(&url)
                                    && let Some(url_span) = find_value_in_attr(raw, attr.span.start, &url)
                                {
                                    push_ref(
                                        MediaTag::from_bytes(tag_name),
                                        MediaAttr::Srcset,
                                        &url,
                                        url_span,
                                        descriptor,
                                        true,
                                    );
                                }
                            }
                        } else if is_remote_url(attr_value) {
                            if is_tracking_url(attr_value) {
                                continue;
                            }
                            if (tag_name == b"a" || tag_name == b"link") && !is_media_url(attr_value) {
                                continue;
                            }
                            if let Some(url_span) = find_value_in_attr(raw, attr.span.start, attr_value) {
                                push_ref(
                                    MediaTag::from_bytes(tag_name),
                                    MediaAttr::from_str(attr_name),
                                    attr_value,
                                    url_span,
                                    None,
                                    false,
                                );
                            }
                        } else if is_local_media_ref(attr_value) && is_broken(attr_value) && !is_tracking_local_url(attr_value) {
                            if (tag_name == b"a" || tag_name == b"link") && !is_media_url(attr_value) {
                                continue;
                            }
                            if let Some(url_span) = find_value_in_attr(raw, attr.span.start, attr_value) {
                                push_ref(
                                    MediaTag::from_bytes(tag_name),
                                    MediaAttr::from_str(attr_name),
                                    attr_value,
                                    url_span,
                                    None,
                                    true,
                                );
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
                                    let craw = &html[cattr.span.start..cattr.span.end];
                                    if is_remote_url(content_val) {
                                        if let Some(url_span) =
                                            find_value_in_attr(craw, cattr.span.start, content_val)
                                        {
                                            push_ref(MediaTag::Meta, MediaAttr::Content, content_val, url_span, None, false);
                                        }
                                    } else if is_local_media_ref(content_val)
                                        && is_broken(content_val)
                                        && !is_tracking_local_url(content_val)
                                        && let Some(url_span) =
                                            find_value_in_attr(craw, cattr.span.start, content_val)
                                    {
                                        push_ref(MediaTag::Meta, MediaAttr::Content, content_val, url_span, None, true);
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
                                if is_tracking_url(url) {
                                    continue;
                                }
                                if let Some(url_span) = find_value_in_attr(raw, attr.span.start, url) {
                                    push_ref(
                                        MediaTag::Style,
                                        MediaAttr::Style,
                                        url,
                                        url_span,
                                        None,
                                        false,
                                    );
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
                        if is_tracking_url(url) {
                            continue;
                        }
                        let abs_start = style_start + url_match.start();
                        let abs_end = style_start + url_match.end();
                        push_ref(MediaTag::Style, MediaAttr::Css, url, abs_start..abs_end, None, false);
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
fn parse_srcset_entries(raw: &str) -> Vec<(String, Option<SrcsetDescriptor>)> {
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
            SrcsetDescriptor::parse(tokens[1])
        } else {
            None
        };
        entries.push((url.to_string(), descriptor));
    }
    entries
}


/// Test helper: deref Box<str> or Arc<str> to &str for assert_eq! comparisons.
#[cfg(test)]
fn s<T: std::ops::Deref<Target = str>>(v: &T) -> &str { v }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_remote_url() {
        assert!(is_remote_url("http://example.com/img.png"));
        assert!(is_remote_url("https://example.com/img.png"));
        assert!(is_remote_url("//cdn.example.com/logo.png"));
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
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert!(result.error.is_none());
        assert_eq!(result.references.len(), 1);
        let r = &result.references[0];
        assert_eq!(r.tag, MediaTag::Img);
        assert_eq!(r.attr, MediaAttr::Src);
        assert_eq!(s(&r.url), "https://cdn.example.com/logo.png");
        // Verify the span extracts the correct URL.
        assert_eq!(
            &html[r.span.start..r.span.end],
            "https://cdn.example.com/logo.png"
        );
    }

    #[test]
    fn test_img_srcset() {
        let html = r#"<img srcset="https://a.com/s.jpg 400w, https://a.com/l.jpg 800w">"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 2);
        assert_eq!(s(&result.references[0].url), "https://a.com/s.jpg");
        assert_eq!(result.references[0].descriptor, Some(SrcsetDescriptor::Width(400)));
        assert_eq!(s(&result.references[1].url), "https://a.com/l.jpg");
        assert_eq!(result.references[1].descriptor, Some(SrcsetDescriptor::Width(800)));
    }

    #[test]
    fn test_captures_broken_local_img_src() {
        let html = r#"<img src="images/photo.jpg">"#;
        // Empty set → file is treated as missing.
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 1);
        assert_eq!(s(&result.references[0].url), "images/photo.jpg");
        assert!(result.references[0].broken);
    }

    #[test]
    fn test_skips_local_img_src_when_exists() {
        let mut set = FxHashSet::default();
        set.insert("images/photo.jpg".to_string());
        let html = r#"<img src="images/photo.jpg">"#;
        let result = scan_file("test.html", html, &set);
        assert_eq!(result.references.len(), 0);
    }

    #[test]
    fn test_ignores_data_uri() {
        let html = r#"<img src="data:image/png;base64,abc">"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 0);
    }

    #[test]
    fn test_meta_og_image() {
        let html = r#"<meta property="og:image" content="https://cdn.example.com/hero.png">"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 1);
        assert_eq!(result.references[0].tag, MediaTag::Meta);
        assert_eq!(result.references[0].attr, MediaAttr::Content);
        assert_eq!(s(&result.references[0].url), "https://cdn.example.com/hero.png");
    }

    #[test]
    fn test_meta_twitter_image() {
        let html = r#"<meta name="twitter:image" content="https://cdn.example.com/hero.png">"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 1);
        assert_eq!(s(&result.references[0].url), "https://cdn.example.com/hero.png");
    }

    #[test]
    fn test_inline_style_url() {
        let html = r#"<div style="background: url(https://cdn.example.com/bg.png)"></div>"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 1);
        assert_eq!(result.references[0].tag, MediaTag::Style);
        assert_eq!(result.references[0].attr, MediaAttr::Style);
        assert_eq!(s(&result.references[0].url), "https://cdn.example.com/bg.png");
        assert_eq!(
            &html[result.references[0].span.start..result.references[0].span.end],
            "https://cdn.example.com/bg.png"
        );
    }

    #[test]
    fn test_style_tag_content() {
        let html = "<style>.bg { background: url(https://cdn.example.com/bg.jpg); }</style>";
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 1);
        assert_eq!(result.references[0].tag, MediaTag::Style);
        assert_eq!(result.references[0].attr, MediaAttr::Css);
        assert_eq!(s(&result.references[0].url), "https://cdn.example.com/bg.jpg");
    }

    #[test]
    fn test_video_src() {
        let html = r#"<video src="https://media.example.com/video.mp4"></video>"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 1);
        assert_eq!(result.references[0].tag, MediaTag::Video);
    }

    #[test]
    fn test_link_href() {
        let html = r#"<link rel="stylesheet" href="https://cdn.example.com/vendor.css">"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 1);
        assert_eq!(result.references[0].tag, MediaTag::Link);
        assert_eq!(result.references[0].attr, MediaAttr::Href);
    }

    #[test]
    fn test_script_src() {
        let html = r#"<script src="https://cdn.example.com/vendor.js"></script>"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 1);
        assert_eq!(result.references[0].tag, MediaTag::Script);
        assert_eq!(result.references[0].attr, MediaAttr::Src);
    }

    #[test]
    fn test_object_data() {
        let html = r#"<object data="https://docs.example.com/doc.pdf"></object>"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 1);
        assert_eq!(result.references[0].tag, MediaTag::Object);
        assert_eq!(result.references[0].attr, MediaAttr::Data);
    }

    #[test]
    fn test_duplicate_urls() {
        let html = r#"<img src="https://cdn.example.com/logo.png"><img src="https://cdn.example.com/logo.png">"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 2);
        assert_eq!(s(&result.references[0].url), s(&result.references[1].url));
    }

    #[test]
    fn test_captures_root_relative_src() {
        let html = r#"<img src="/assets/logo.png">"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 1);
        assert_eq!(s(&result.references[0].url), "/assets/logo.png");
    }

    #[test]
    fn test_link_href_captures_local_media() {
        // Local .css href in <link> — media extension, so captured.
        let html = r#"<link rel="stylesheet" href="local/style.css">"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 1);
        assert_eq!(s(&result.references[0].url), "local/style.css");
        assert_eq!(result.references[0].tag, MediaTag::Link);
    }

    #[test]
    fn test_link_href_ignores_local_non_media() {
        // Local href in <link> without media extension — skipped.
        let html = r#"<link rel="alternate" href="feed.xml">"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        // .xml is not a media extension, so it's skipped.
        assert_eq!(result.references.len(), 0);
    }

    #[test]
    fn test_meta_other_property_ignored() {
        let html = r#"<meta property="og:title" content="https://cdn.example.com/title.png">"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 0);
    }

    #[test]
    fn test_audio_src() {
        let html = r#"<audio src="https://media.example.com/audio.mp3"></audio>"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 1);
        assert_eq!(result.references[0].tag, MediaTag::Audio);
    }

    #[test]
    fn test_source_src() {
        let html = r#"<source src="https://media.example.com/video.mp4">"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 1);
        assert_eq!(result.references[0].tag, MediaTag::Source);
    }

    #[test]
    fn test_source_srcset() {
        let html = r#"<source srcset="https://a.com/b.webp 1x, https://a.com/b2x.webp 2x">"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 2);
        assert_eq!(result.references[0].attr, MediaAttr::Srcset);
    }

    #[test]
    fn test_track_src() {
        let html = r#"<track src="https://media.example.com/subtitles.vtt">"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 1);
        assert_eq!(result.references[0].tag, MediaTag::Track);
    }

    #[test]
    fn test_style_url_only_within_block() {
        // The same URL appears in a data attribute OUTSIDE the style block.
        let html = concat!(
            "<style>.bg { background: url(https://cdn.example.com/a.jpg); }</style>\n",
            r#"<img data-fallback="https://cdn.example.com/a.jpg">"#,
        );
        let result = scan_file("test.html", html, &FxHashSet::default());
        let style_refs: Vec<_> = result
            .references
            .iter()
            .filter(|r| r.attr == MediaAttr::Css)
            .collect();
        assert_eq!(style_refs.len(), 1);
        // Verify the span falls inside the <style> element.
        let style_end = html.find("</style>").unwrap();
        assert!(style_refs[0].span.start < style_end);
    }

    #[test]
    fn test_malformed_html_no_crash() {
        let html = r#"<img src="https://example.com/img.png""#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert!(result.references.len() <= 1); // may or may not parse, but shouldn't crash
    }

    #[test]
    fn test_a_href() {
        let html =
            r#"<a href="https://s3-us-west-2.amazonaws.com/reference/images/pic.jpg">link</a>"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 1);
        assert_eq!(result.references[0].tag, MediaTag::A);
        assert_eq!(result.references[0].attr, MediaAttr::Href);
        assert_eq!(
            s(&result.references[0].url),
            "https://s3-us-west-2.amazonaws.com/reference/images/pic.jpg"
        );
    }

    #[test]
    fn test_a_href_ignores_relative() {
        let html = r#"<a href="about.html">link</a>"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 0);
    }

    #[test]
    fn test_a_href_ignores_fragment() {
        let html = "<a href=\"#section\">link</a>";
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 0);
    }

    #[test]
    fn test_a_href_ignores_non_media() {
        // Social links, web pages — not media assets.
        let html = r#"<a href="https://www.facebook.com/insightlive">FB</a>"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 0);
    }

    #[test]
    fn test_a_href_media_extension() {
        // <a> wrapping an image URL with a media extension should match.
        let html = r#"<a href="https://s3.amazonaws.com/bucket/photo.jpg">img</a>"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 1);
        assert_eq!(result.references[0].tag, MediaTag::A);
    }

    #[test]
    fn test_span_extraction() {
        let html = r#"<img src="https://example.com/img.png">"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 1);
        let r = &result.references[0];
        let extracted = &html[r.span.start..r.span.end];
        assert_eq!(extracted, "https://example.com/img.png");
    }

    #[test]
    fn test_entity_in_url() {
        // &amp; in query string should not cause a panic.
        let html = r#"<a href="https://example.com/page?a=1&amp;b=2">link</a>"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        // Should find the reference (URL with query params is treated as a media-like URL
        // because it ends with "?a=1&amp;b=2" — actually the decoded value "?a=1&b=2"
        // has no extension, so is_media_url returns false for <a> tags. But this test
        // verifies the entity doesn't cause a panic regardless.
        assert!(result.error.is_none());
    }

    #[test]
    fn test_link_href_ignores_non_media() {
        // RSS feed, canonical URL, REST API — not media assets.
        let html = r#"<link rel="alternate" type="application/rss+xml" href="https://islandblacksmith.ca/feed/">"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 0);
    }

    #[test]
    fn test_tracking_pixel_ignored() {
        // WordPress Stats tracking beacon — not a real media asset.
        let html = r#"<img src="https://pixel.wp.com/g.gif?v=ext&blog=123&post=1&rand=0.123" alt="" width="6" height="5" id="wpstats">"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 0);
    }

    #[test]
    fn test_tracking_local_url_wpstats_ignored() {
        // Local path rewritten by grab from pixel.wp.com — should be
        // recognized as a tracking artifact, not reported as broken.
        let html = r#"<img src="_grab/pixel.wp.com/g__q-252f96.gif" alt="" width="6" height="5" id="wpstats">"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 0);
    }

    #[test]
    fn test_tracking_local_url_pixel_facebook_ignored() {
        // Local path from pixel.facebook.com — tracking host.
        let html = r#"<img src="_grab/pixel.facebook.com/tr__q-a1b2c3.gif" width="6" height="5">"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 0);
    }

    #[test]
    fn test_tracking_local_url_non_tracking_reported() {
        // Local path from a non-tracking host — still reported as broken.
        let html = r#"<img src="_grab/cdn.example.com/animation.gif">"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 1);
        assert!(result.references[0].broken);
        assert_eq!(s(&result.references[0].url), "_grab/cdn.example.com/animation.gif");
    }

    #[test]
    fn test_tracking_local_url_pixelperfect_not_ignored() {
        // pixelperfect.com is not a tracking host (no standalone "pixel" subdomain).
        let html = r#"<img src="_grab/pixelperfect.com/images/logo.png">"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 1);
        assert!(result.references[0].broken);
    }

    #[test]
    fn test_tracking_url_pixel_subdomain_ignored() {
        // Any host with a "pixel" subdomain is filtered.
        let html = r#"<img src="https://pixel.facebook.com/tr?id=123">"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 0);
    }

    #[test]
    fn test_tracking_url_non_pixel_host_matched() {
        // Non-pixel hosts with g.gif are still matched (not filtering based on path).
        let html = r#"<img src="https://example.com/g.gif">"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 1);
    }

    #[test]
    fn test_srcset_tracking_url_ignored() {
        // Tracking URL in srcset is filtered out.
        let html = r#"<img srcset="https://pixel.wp.com/g.gif 1x, https://cdn.example.com/real.jpg 2x">"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 1);
        assert_eq!(s(&result.references[0].url), "https://cdn.example.com/real.jpg");
    }

    #[test]
    fn test_inline_style_tracking_url_ignored() {
        // Tracking URL in inline style url() is filtered.
        let html = r#"<div style="background: url(https://pixel.wp.com/g.gif)"></div>"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 0);
    }

    #[test]
    fn test_style_block_tracking_url_ignored() {
        // Tracking URL in <style> block url() is filtered.
        let html = "<style>.bg { background: url(https://pixel.wp.com/g.gif); }</style>";
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 0);
    }

    #[test]
    fn test_real_gif_still_matched() {
        // A real .gif file on a regular host is still matched.
        let html = r#"<img src="https://cdn.example.com/animation.gif">"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 1);
    }

    #[test]
    fn test_gif_on_non_tracking_host() {
        // g.gif on a non-tracking domain is still matched.
        let html = r#"<img src="https://example.com/g.gif">"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 1);
    }

    #[test]
    fn test_link_href_media() {
        // <link> to a CSS or font file is a media asset.
        let html = r#"<link rel="stylesheet" href="https://cdn.example.com/vendor.css">"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 1);
        assert_eq!(result.references[0].tag, MediaTag::Link);
        assert_eq!(result.references[0].attr, MediaAttr::Href);
    }

    #[test]
    fn test_amp_entity_in_srcset() {
        let html =
            r#"<img srcset="https://a.com/img.jpg?w=400&amp;h=300 400w, https://a.com/img.jpg 800w">"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert!(result.error.is_none());
        assert_eq!(result.references.len(), 2);
        assert_eq!(
            s(&result.references[0].url),
            "https://a.com/img.jpg?w=400&h=300"
        );
        let extracted = &html[result.references[0].span.start..result.references[0].span.end];
        assert_eq!(extracted, "https://a.com/img.jpg?w=400&amp;h=300");
    }

    #[test]
    fn test_local_a_href_media() {
        let html = r#"<a href="docs/manual.pdf">PDF</a>"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 1);
        assert_eq!(s(&result.references[0].url), "docs/manual.pdf");
        assert_eq!(result.references[0].tag, MediaTag::A);
    }

    #[test]
    fn test_local_a_href_non_media_ignored() {
        let html = r#"<a href="about.html">About</a>"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 0);
    }

    #[test]
    fn test_local_video_src() {
        let html = r#"<video src="media/intro.mp4"></video>"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 1);
        assert_eq!(s(&result.references[0].url), "media/intro.mp4");
        assert_eq!(result.references[0].tag, MediaTag::Video);
    }

    #[test]
    fn test_local_script_src() {
        let html = r#"<script src="js/app.js"></script>"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 1);
        assert_eq!(s(&result.references[0].url), "js/app.js");
        assert_eq!(result.references[0].tag, MediaTag::Script);
    }

    #[test]
    fn test_local_srcset() {
        let html = r#"<img srcset="img/small.jpg 400w, img/large.jpg 800w">"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 2);
        assert_eq!(s(&result.references[0].url), "img/small.jpg");
        assert_eq!(result.references[0].descriptor, Some(SrcsetDescriptor::Width(400)));
        assert_eq!(s(&result.references[1].url), "img/large.jpg");
        assert_eq!(result.references[1].descriptor, Some(SrcsetDescriptor::Width(800)));
    }

    #[test]
    fn test_local_mixed_srcset() {
        let html = r#"<img srcset="local/a.jpg 1x, https://cdn.example.com/b.jpg 2x">"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 2);
        assert_eq!(s(&result.references[0].url), "local/a.jpg");
        assert_eq!(s(&result.references[1].url), "https://cdn.example.com/b.jpg");
    }

    #[test]
    fn test_local_inline_style_url_ignored() {
        // Local CSS url() references are not captured — too noisy.
        let html = r#"<div style="background: url(../img/bg.png)"></div>"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 0);
    }

    #[test]
    fn test_local_style_block_url_ignored() {
        // Local CSS url() references are not captured — too noisy.
        let html = "<style>.hero { background: url(images/hero.jpg); }</style>";
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 0);
    }

    #[test]
    fn test_local_meta_og_image() {
        let html = r#"<meta property="og:image" content="/img/hero.png">"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 1);
        assert_eq!(s(&result.references[0].url), "/img/hero.png");
        assert_eq!(result.references[0].tag, MediaTag::Meta);
    }

    #[test]
    fn test_ignores_fragment_only() {
        let html = "<img src=\"#section\">";
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 0);
    }

    #[test]
    fn test_ignores_query_only() {
        let html = r#"<img src="?v=2">"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 0);
    }

    #[test]
    fn test_ignores_javascript_url() {
        let html = r#"<img src="javascript:void(0)">"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 0);
    }

    #[test]
    fn test_ignores_mailto() {
        let html = r#"<a href="mailto:user@example.com">email</a>"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        // mailto is not a media URL extension, so skipped for <a>.
        assert_eq!(result.references.len(), 0);
    }

    #[test]
    fn test_protocol_relative_treated_as_remote() {
        // Protocol-relative URLs (//example.com/foo) are treated as remote.
        let html = r#"<img src="//cdn.example.com/logo.png">"#;
        let result = scan_file("test.html", html, &FxHashSet::default());
        assert_eq!(result.references.len(), 1);
        assert_eq!(s(&result.references[0].url), "//cdn.example.com/logo.png");
        assert!(!result.references[0].broken);
    }
}
