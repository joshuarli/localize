use html5gum::emitters::default::DefaultEmitter;
use html5gum::Tokenizer;
use std::ops::Range;

#[derive(Debug, Clone)]
pub struct StyleBlock {
    /// Byte span of the entire `<style ...>...</style>` element.
    pub span: Range<usize>,
    /// The byte-exact CSS content between the opening and closing tags.
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct ExtractResult {
    /// CSS files to write: (hash, content).
    pub writes: Vec<(String, String)>,
    /// `<link>` tags to insert before `</head>`, in document order.
    pub link_tags: Vec<String>,
    /// Spans to delete (the entire `<style>...</style>` blocks).
    pub spans_to_delete: Vec<Range<usize>>,
}

/// Parse all `<style>` elements from HTML, returning their spans and content.
fn find_style_blocks(html: &str) -> Result<Vec<StyleBlock>, String> {
    let mut blocks = Vec::new();
    let mut pending: Option<(usize, usize)> = None; // (whole_start, open_tag_end)

    let tokenizer = Tokenizer::new_with_emitter(html, DefaultEmitter::<usize>::new_with_span());

    for token_result in tokenizer {
        let token = match token_result {
            Ok(t) => t,
            Err(e) => {
                pending = None;
                eprintln!("extract-css: parse error: {e}");
                continue;
            }
        };

        match token {
            html5gum::Token::StartTag(tag) if &tag.name[..] == b"style" => {
                pending = Some((tag.span.start, tag.span.end));
            }
            html5gum::Token::EndTag(tag) if &tag.name[..] == b"style" => {
                if let Some((whole_start, open_tag_end)) = pending.take() {
                    let content = &html[open_tag_end..tag.span.start];
                    blocks.push(StyleBlock {
                        span: whole_start..tag.span.end,
                        content: content.to_string(),
                    });
                }
            }
            _ => {}
        }
    }

    Ok(blocks)
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

    for block in &blocks {
        if block.content.trim().is_empty() {
            spans_to_delete.push(block.span.clone());
            continue;
        }

        let hash = format!("{:016x}", xxhash_rust::xxh3::xxh3_64(block.content.as_bytes()));
        let prefix = &hash[..2];
        let css_path = format!("{css_dir}/{prefix}/{hash}.css");
        let href = compute_relative_path(file_rel, &css_path);

        link_tags.push(format!(
            "<link rel=\"stylesheet\" href=\"{href}\">"
        ));

        writes.push((hash, block.content.clone()));
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
    fn test_empty_style_skipped_in_extract() {
        let html = "<style class=\"wp-fonts-local\"></style>";
        let result = extract_css(html, "index.html", "localized-css").unwrap();
        assert!(result.writes.is_empty());
        assert!(result.link_tags.is_empty());
        assert_eq!(result.spans_to_delete.len(), 1);
    }

    #[test]
    fn test_multiple_styles() {
        let html = concat!(
            "<style>.a{}</style>",
            "<style>.b{}</style>",
        );
        let result = extract_css(html, "index.html", "localized-css").unwrap();
        assert_eq!(result.writes.len(), 2);
        assert_eq!(result.link_tags.len(), 2);
        assert_eq!(result.spans_to_delete.len(), 2);
    }

    #[test]
    fn test_content_addressed_path() {
        let html = "<style>.x{}</style>";
        let result = extract_css(html, "index.html", "localized-css").unwrap();
        // Content ".x{}" hashes to a known value, path is localized-css/{prefix}/{hash}.css
        let hash = &result.writes[0].0;
        let prefix = &hash[..2];
        assert_eq!(hash.len(), 16);
        assert!(result.link_tags[0].contains(&format!("localized-css/{prefix}/{hash}.css")));
    }

    #[test]
    fn test_identical_content_same_hash() {
        let css = ".a{color:red}";
        let html1 = format!("<style>{css}</style>");
        let html2 = format!("<style>{css}</style>");
        let r1 = extract_css(&html1, "index.html", "css").unwrap();
        let r2 = extract_css(&html2, "index.html", "css").unwrap();
        assert_eq!(r1.writes[0].0, r2.writes[0].0);
    }

    #[test]
    fn test_different_content_different_hash() {
        let r1 = extract_css("<style>.a{}</style>", "index.html", "css").unwrap();
        let r2 = extract_css("<style>.b{}</style>", "index.html", "css").unwrap();
        assert_ne!(r1.writes[0].0, r2.writes[0].0);
    }

    #[test]
    fn test_full_extract_generates_correct_link() {
        let html = "<html><head></head><body><style>.x{}</style></body></html>";
        let result = extract_css(html, "index.html", "localized-css").unwrap();
        assert_eq!(result.writes.len(), 1);
        assert_eq!(result.link_tags.len(), 1);
        assert!(result.link_tags[0].contains("href=\"localized-css/"));
        assert!(result.link_tags[0].ends_with(".css\">"));
        assert_eq!(result.spans_to_delete.len(), 1);
    }

    #[test]
    fn test_no_style_elements() {
        let html = "<html><head></head><body><p>Hello</p></body></html>";
        let result = extract_css(html, "index.html", "localized-css").unwrap();
        assert!(result.writes.is_empty());
        assert!(result.link_tags.is_empty());
        assert!(result.spans_to_delete.is_empty());
    }

    #[test]
    fn test_compute_relative_same_dir() {
        let result = compute_relative_path("index.html", "localized-css/a1/a1b2c3d4e5f67890.css");
        assert_eq!(result, "localized-css/a1/a1b2c3d4e5f67890.css");
    }

    #[test]
    fn test_compute_relative_subdir() {
        let result = compute_relative_path("posts/about.html", "localized-css/a1/a1b2c3d4e5f67890.css");
        assert_eq!(result, "../localized-css/a1/a1b2c3d4e5f67890.css");
    }

    #[test]
    fn test_extract_preserves_css_content() {
        let css = ".wp-block-site-logo{box-sizing:border-box;line-height:0}";
        let html = format!("<style>{css}</style>");
        let result = extract_css(&html, "index.html", "localized-css").unwrap();
        assert_eq!(result.writes[0].1, css);
    }
}
