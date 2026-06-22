use html5gum::emitters::default::DefaultEmitter;
use html5gum::Tokenizer;
use regex_lite::Regex;
use rustc_hash::FxHashMap;
use std::ops::Range;
use std::sync::LazyLock;

static SOURCEURL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/\*#\s*sourceURL=(\S+)\s*\*/").unwrap());

#[derive(Debug, Clone)]
pub struct StyleBlock {
    /// Byte span of the entire `<style ...>...</style>` element.
    pub span: Range<usize>,
    /// The byte-exact CSS content between the opening and closing tags.
    pub content: String,
    /// The id attribute value, if present.
    pub id: Option<String>,
    /// Parsed from `/*# sourceURL=... */` comment, if present.
    pub source_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ExtractResult {
    /// CSS files to write: (filename, content).
    pub writes: Vec<(String, String)>,
    /// `<link>` tags to insert before `</head>`, in document order.
    pub link_tags: Vec<String>,
    /// Spans to delete (the entire `<style>...</style>` blocks).
    pub spans_to_delete: Vec<Range<usize>>,
}

/// Parse all `<style>` elements from HTML, returning their spans and content.
fn find_style_blocks(html: &str) -> Result<Vec<StyleBlock>, String> {
    let mut blocks = Vec::new();
    let mut pending: Option<(usize, usize, Option<String>)> = None; // (whole_start, open_tag_end, id)

    let tokenizer = Tokenizer::new_with_emitter(html, DefaultEmitter::<usize>::new_with_span());

    for token_result in tokenizer {
        let token = match token_result {
            Ok(t) => t,
            Err(e) => {
                // If we were tracking a style block, drop it.
                pending = None;
                // Continue rather than abort — parse errors in known-bad HTML
                // shouldn't prevent extracting the blocks we can find.
                eprintln!("extract-css: parse error: {e}");
                continue;
            }
        };

        match token {
            html5gum::Token::StartTag(tag) if &tag.name[..] == b"style" => {
                let id = tag
                    .attributes
                    .iter()
                    .find(|(name, _)| &name[..] == b"id")
                    .and_then(|(_, val)| std::str::from_utf8(val).ok())
                    .map(|s| s.to_string());
                pending = Some((tag.span.start, tag.span.end, id));
            }
            html5gum::Token::EndTag(tag) if &tag.name[..] == b"style" => {
                if let Some((whole_start, open_tag_end, id)) = pending.take() {
                    let content = &html[open_tag_end..tag.span.start];
                    let source_url = parse_source_url(content);
                    blocks.push(StyleBlock {
                        span: whole_start..tag.span.end,
                        content: content.to_string(),
                        id,
                        source_url,
                    });
                }
            }
            _ => {}
        }
    }

    Ok(blocks)
}

fn parse_source_url(css: &str) -> Option<String> {
    SOURCEURL_RE
        .captures(css)
        .and_then(|m| m.get(1))
        .map(|m| m.as_str().trim().to_string())
}

/// Generate a filename from a style block. Returns just the filename (no directory).
fn compute_filename(
    block: &StyleBlock,
    seen: &mut FxHashMap<String, usize>,
) -> String {
    let base = if let Some(ref url) = block.source_url {
        source_url_to_filename(url)
    } else if let Some(ref id) = block.id {
        id_to_filename(id)
    } else {
        hash_filename(&block.content)
    };

    // Ensure .css extension.
    let base = if base.ends_with(".css") {
        base.to_string()
    } else {
        format!("{base}.css")
    };

    // Handle collisions.
    let count = seen.entry(base.clone()).or_insert(0);
    if *count == 0 {
        *count = 1;
        base
    } else {
        *count += 1;
        // Insert collision suffix before .css.
        let stem = base.strip_suffix(".css").unwrap_or(&base);
        format!("{stem}_{}.css", *count - 1)
    }
}

fn source_url_to_filename(url: &str) -> String {
    // If it's a full URL, extract the path portion.
    let path = if url.starts_with("http://") || url.starts_with("https://") {
        // Try to parse as URL, fall back to the full string.
        if let Ok(parsed) = url::Url::parse(url) {
            parsed.path().trim_start_matches('/').to_string()
        } else {
            url.to_string()
        }
    } else {
        // Strip leading / for root-relative paths.
        url.trim_start_matches('/').to_string()
    };

    // Strip any query string or fragment that snuck through.
    let path = path
        .split('?')
        .next()
        .unwrap_or(&path)
        .split('#')
        .next()
        .unwrap_or(&path);

    // Replace / with __ to flatten directory structure.
    let name = path.replace('/', "__");

    if name.is_empty() {
        "style.css".to_string()
    } else {
        name
    }
}

fn id_to_filename(id: &str) -> String {
    // Strip the common WordPress `-inline-css` suffix.
    if let Some(stripped) = id.strip_suffix("-inline-css") {
        stripped.to_string()
    } else {
        id.to_string()
    }
}

fn hash_filename(content: &str) -> String {
    let hash = xxhash_rust::xxh3::xxh3_64(content.as_bytes());
    format!("style-{hash:016x}")
}

/// Compute the relative path from an HTML file's directory to a target path.
fn compute_relative_path(html_file: &str, target: &str) -> String {
    let html_dir = std::path::Path::new(html_file)
        .parent()
        .unwrap_or(std::path::Path::new(""));
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

pub fn extract_css(
    html: &str,
    file_rel: &str,
    css_dir: &str,
) -> Result<ExtractResult, String> {
    let blocks = find_style_blocks(html)?;

    let mut writes = Vec::new();
    let mut link_tags = Vec::new();
    let mut spans_to_delete = Vec::new();
    let mut seen_names: FxHashMap<String, usize> = FxHashMap::default();

    for block in &blocks {
        if block.content.trim().is_empty() {
            spans_to_delete.push(block.span.clone());
            continue;
        }

        let filename = compute_filename(block, &mut seen_names);
        let css_path = format!("{css_dir}/{filename}");

        let href = compute_relative_path(file_rel, &css_path);
        link_tags.push(format!(
            "<link rel=\"stylesheet\" href=\"{href}\">"
        ));

        writes.push((filename, block.content.clone()));
        spans_to_delete.push(block.span.clone());
    }

    Ok(ExtractResult {
        writes,
        link_tags,
        spans_to_delete,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_simple_style() {
        let html = "<style>body { color: red; }</style>";
        let blocks = find_style_blocks(html).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].content, "body { color: red; }");
        assert_eq!(&html[blocks[0].span.clone()], "<style>body { color: red; }</style>");
    }

    #[test]
    fn test_find_style_with_id() {
        let html = "<style id=\"my-style\">.a{}</style>";
        let blocks = find_style_blocks(html).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].id.as_deref(), Some("my-style"));
    }

    #[test]
    fn test_source_url_full_url() {
        let css = "/*# sourceURL=https://example.com/wp-includes/style.min.css */\n.foo{}";
        let url = parse_source_url(css);
        assert_eq!(url.as_deref(), Some("https://example.com/wp-includes/style.min.css"));
    }

    #[test]
    fn test_source_url_root_relative() {
        let css = "/*# sourceURL=/wp-includes/style.css */\n.bar{}";
        let url = parse_source_url(css);
        assert_eq!(url.as_deref(), Some("/wp-includes/style.css"));
    }

    #[test]
    fn test_source_url_bare_name() {
        let css = "img:is([sizes=auto i]){contain-intrinsic-size:3000px 1500px}\n/*# sourceURL=wp-img-auto-sizes-contain-inline-css */";
        let url = parse_source_url(css);
        assert_eq!(url.as_deref(), Some("wp-img-auto-sizes-contain-inline-css"));
    }

    #[test]
    fn test_source_url_not_present() {
        let css = ".x { color: red; }";
        let url = parse_source_url(css);
        assert!(url.is_none());
    }

    #[test]
    fn test_naming_from_source_url_full() {
        let block = StyleBlock {
            span: 0..10,
            content: "x{}".into(),
            id: Some("some-id".into()),
            source_url: Some("https://example.com/wp-includes/blocks/site-logo/style.min.css".into()),
        };
        let mut seen = FxHashMap::default();
        let name = compute_filename(&block, &mut seen);
        // Should use sourceURL (priority 1), not id.
        assert_eq!(name, "wp-includes__blocks__site-logo__style.min.css");
    }

    #[test]
    fn test_naming_from_id() {
        let block = StyleBlock {
            span: 0..10,
            content: "x{}".into(),
            id: Some("wp-block-site-logo-inline-css".into()),
            source_url: None,
        };
        let mut seen = FxHashMap::default();
        let name = compute_filename(&block, &mut seen);
        assert_eq!(name, "wp-block-site-logo.css");
    }

    #[test]
    fn test_naming_hash_fallback() {
        let block = StyleBlock {
            span: 0..10,
            content: "body{color:red}".into(),
            id: None,
            source_url: None,
        };
        let mut seen = FxHashMap::default();
        let name = compute_filename(&block, &mut seen);
        assert!(name.starts_with("style-"));
        assert!(name.ends_with(".css"));
    }

    #[test]
    fn test_collision_suffix() {
        let block = StyleBlock {
            span: 0..10,
            content: "a{}".into(),
            id: Some("my-style".into()),
            source_url: None,
        };
        let mut seen = FxHashMap::default();
        let name1 = compute_filename(&block, &mut seen);
        assert_eq!(name1, "my-style.css");
        let name2 = compute_filename(&block, &mut seen);
        assert_eq!(name2, "my-style_1.css");
        let name3 = compute_filename(&block, &mut seen);
        assert_eq!(name3, "my-style_2.css");
    }

    #[test]
    fn test_empty_style_skipped_in_extract() {
        let html = "<style class=\"wp-fonts-local\"></style>";
        let result = extract_css(html, "index.html", "assets/css").unwrap();
        assert!(result.writes.is_empty());
        assert!(result.link_tags.is_empty());
        assert_eq!(result.spans_to_delete.len(), 1);
    }

    #[test]
    fn test_multiple_styles() {
        let html = concat!(
            "<style id=\"a\">.a{}</style>",
            "<style id=\"b\">.b{}</style>",
        );
        let result = extract_css(html, "index.html", "assets/css").unwrap();
        assert_eq!(result.writes.len(), 2);
        assert_eq!(result.link_tags.len(), 2);
        assert_eq!(result.spans_to_delete.len(), 2);
    }

    #[test]
    fn test_full_extract_generates_correct_link() {
        let html = "<html><head></head><body><style id=\"s1\">.x{}</style></body></html>";
        let result = extract_css(html, "index.html", "assets/css").unwrap();
        assert_eq!(result.writes.len(), 1);
        assert_eq!(result.link_tags.len(), 1);
        assert!(result.link_tags[0].contains("href=\"assets/css/s1.css\""));
        assert_eq!(result.spans_to_delete.len(), 1);
    }

    #[test]
    fn test_no_style_elements() {
        let html = "<html><head></head><body><p>Hello</p></body></html>";
        let result = extract_css(html, "index.html", "assets/css").unwrap();
        assert!(result.writes.is_empty());
        assert!(result.link_tags.is_empty());
        assert!(result.spans_to_delete.is_empty());
    }

    #[test]
    fn test_compute_relative_same_dir() {
        let result = compute_relative_path("index.html", "assets/css/style.css");
        assert_eq!(result, "assets/css/style.css");
    }

    #[test]
    fn test_compute_relative_subdir() {
        let result = compute_relative_path("posts/about.html", "assets/css/style.css");
        assert_eq!(result, "../assets/css/style.css");
    }

    #[test]
    fn test_extract_preserves_css_content() {
        let css = ".wp-block-site-logo{box-sizing:border-box;line-height:0}\n/*# sourceURL=style.css */";
        let html = format!("<style id=\"x\">{css}</style>");
        let result = extract_css(&html, "index.html", "assets/css").unwrap();
        assert_eq!(result.writes[0].1, css);
    }

    #[test]
    fn test_source_url_without_css_extension() {
        let block = StyleBlock {
            span: 0..10,
            content: "x{}".into(),
            id: None,
            source_url: Some("my-css-chunk".into()),
        };
        let mut seen = FxHashMap::default();
        let name = compute_filename(&block, &mut seen);
        assert_eq!(name, "my-css-chunk.css");
    }

    #[test]
    fn test_id_without_inline_css_suffix() {
        let block = StyleBlock {
            span: 0..10,
            content: "x{}".into(),
            id: Some("custom-id".into()),
            source_url: None,
        };
        let mut seen = FxHashMap::default();
        let name = compute_filename(&block, &mut seen);
        assert_eq!(name, "custom-id.css");
    }

    #[test]
    fn test_source_url_with_query_string_stripped() {
        let block = StyleBlock {
            span: 0..10,
            content: "x{}".into(),
            id: None,
            source_url: Some("https://example.com/style.css?ver=1.0".into()),
        };
        let mut seen = FxHashMap::default();
        let name = compute_filename(&block, &mut seen);
        assert_eq!(name, "style.css");
    }
}
