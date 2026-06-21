use html5gum::SpanBound;
use html5gum::Tokenizer;
use html5gum::emitters::default::DefaultEmitter;
use rustc_hash::FxHashSet;
use std::ops::Range;
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
pub struct SimpleSelector {
    pub source: String,
    pub tag: Option<String>,
    pub classes: Vec<String>,
    pub id: Option<String>,
    pub attrs: Vec<(String, Option<String>)>,
}

/// Parse a simple CSS selector string.
///
/// Supported syntax:
///   `tag`          — plain tag name
///   `.class`       — any element with class
///   `#id`          — element by id
///   `tag.class`    — tag + class
///   `tag#id.class` — combined
///   `[attr]`       — attribute presence
///   `[attr=value]` — attribute equals value
pub fn parse_selector(input: &str) -> Result<SimpleSelector, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("selector must not be empty".into());
    }

    let s = input;
    let bytes = s.as_bytes();
    let mut pos = 0;

    let mut tag: Option<String> = None;
    let mut classes: Vec<String> = Vec::new();
    let mut id: Option<String> = None;
    let mut attrs: Vec<(String, Option<String>)> = Vec::new();

    // Parse optional tag name (ASCII letters at the start, before any . # [)
    if pos < bytes.len() && bytes[pos].is_ascii_alphabetic() {
        let start = pos;
        while pos < bytes.len() && bytes[pos].is_ascii_alphanumeric() {
            pos += 1;
        }
        tag = Some(s[start..pos].to_ascii_lowercase());
    }

    // Parse remaining components (.class, #id, [attr=val])
    while pos < bytes.len() {
        match bytes[pos] {
            b'.' => {
                pos += 1;
                let start = pos;
                if start >= bytes.len() || !bytes[start].is_ascii_alphanumeric() {
                    return Err(format!("expected class name after '.' at position {start}"));
                }
                while pos < bytes.len()
                    && (bytes[pos].is_ascii_alphanumeric()
                        || bytes[pos] == b'-'
                        || bytes[pos] == b'_')
                {
                    pos += 1;
                }
                classes.push(s[start..pos].to_string());
            }
            b'#' => {
                pos += 1;
                if id.is_some() {
                    return Err("only one #id allowed in selector".into());
                }
                let start = pos;
                if start >= bytes.len() || !bytes[start].is_ascii_alphanumeric() {
                    return Err(format!("expected id name after '#' at position {start}"));
                }
                while pos < bytes.len()
                    && (bytes[pos].is_ascii_alphanumeric()
                        || bytes[pos] == b'-'
                        || bytes[pos] == b'_')
                {
                    pos += 1;
                }
                id = Some(s[start..pos].to_string());
            }
            b'[' => {
                pos += 1;
                let start = pos;
                while pos < bytes.len() && bytes[pos] != b']' && bytes[pos] != b'=' {
                    pos += 1;
                }
                let attr_name = s[start..pos].trim().to_ascii_lowercase();
                if attr_name.is_empty() {
                    return Err("empty attribute name in selector".into());
                }
                if pos >= bytes.len() {
                    return Err("unclosed '[' in selector".into());
                }
                if bytes[pos] == b'=' {
                    pos += 1;
                    let val_start = pos;
                    while pos < bytes.len() && bytes[pos] != b']' {
                        pos += 1;
                    }
                    if pos >= bytes.len() {
                        return Err("unclosed '[' in selector".into());
                    }
                    let attr_value = s[val_start..pos].trim().to_string();
                    attrs.push((attr_name, Some(attr_value)));
                } else {
                    attrs.push((attr_name, None));
                }
                pos += 1; // skip ']'
            }
            _ => {
                return Err(format!(
                    "unexpected character '{}' at position {pos} in selector",
                    bytes[pos] as char
                ));
            }
        }
    }

    if tag.is_none() && classes.is_empty() && id.is_none() && attrs.is_empty() {
        return Err(
            "selector must have at least one component (tag, .class, #id, or [attr])".into(),
        );
    }

    Ok(SimpleSelector {
        source: input.to_string(),
        tag,
        classes,
        id,
        attrs,
    })
}

/// Check whether an html5gum StartTag matches a simple selector.
fn matches_selector<O: SpanBound>(tag: &html5gum::Token<O>, sel: &SimpleSelector) -> bool {
    let start_tag = match tag {
        html5gum::Token::StartTag(t) => t,
        _ => return false,
    };

    if let Some(ref t) = sel.tag
        && !start_tag.name.eq_ignore_ascii_case(t.as_bytes())
    {
        return false;
    }

    if let Some(ref target_id) = sel.id {
        let mut found = false;
        for (name, value) in &start_tag.attributes {
            if name.eq_ignore_ascii_case(b"id")
                && std::str::from_utf8(&value[..]).is_ok_and(|v| v == *target_id)
            {
                found = true;
                break;
            }
        }
        if !found {
            return false;
        }
    }

    if !sel.classes.is_empty() {
        let class_attr = start_tag
            .attributes
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(b"class"));
        match class_attr {
            None => return false,
            Some((_, value)) => {
                let class_str = std::str::from_utf8(&value[..]).unwrap_or("");
                let element_classes: Vec<&str> = class_str.split_whitespace().collect();
                for cls in &sel.classes {
                    if !element_classes.contains(&cls.as_str()) {
                        return false;
                    }
                }
            }
        }
    }

    for (attr_name, attr_value) in &sel.attrs {
        let mut found = false;
        for (name, value) in &start_tag.attributes {
            if name.eq_ignore_ascii_case(attr_name.as_bytes()) {
                if let Some(expected) = attr_value {
                    if std::str::from_utf8(&value[..]).is_ok_and(|v| v == *expected) {
                        found = true;
                        break;
                    }
                } else {
                    found = true;
                    break;
                }
            }
        }
        if !found {
            return false;
        }
    }

    true
}

#[derive(Debug, Clone)]
pub struct ZapMatch {
    pub span: Range<usize>,
    pub tag: String,
    pub text_preview: String,
}

#[derive(Debug)]
pub struct ZapResult {
    pub matches: Vec<ZapMatch>,
    pub error: Option<String>,
}

/// Scan HTML for elements matching `selector` whose inner text contains `query`.
/// Tag matching is case-insensitive (HTML semantics); query is case-sensitive.
pub fn scan_html(html: &str, selector: &SimpleSelector, query: &str) -> ZapResult {
    let mut matches: Vec<ZapMatch> = Vec::new();
    let display_tag = selector.source.clone();

    struct OpenEl {
        start: usize,
        end: usize,
    }
    let mut stack: Vec<OpenEl> = Vec::new();

    let tokenizer = Tokenizer::new_with_emitter(html, DefaultEmitter::<usize>::new_with_span());

    for token_result in tokenizer {
        let token = match token_result {
            Ok(t) => t,
            Err(e) => {
                return ZapResult {
                    matches,
                    error: Some(format!("Parse error: {e}")),
                };
            }
        };

        match &token {
            html5gum::Token::StartTag(tag) => {
                if !matches_selector(&token, selector) {
                    continue;
                }
                if tag.self_closing || VOID_ELEMENTS.contains(&tag.name[..]) {
                    continue;
                }
                stack.push(OpenEl {
                    start: tag.span.start,
                    end: tag.span.end,
                });
            }
            html5gum::Token::EndTag(tag) => {
                // Only consider end tags that match the selector's tag (if specified),
                // or any end tag if the selector has no tag requirement.
                if let Some(ref t) = selector.tag
                    && !tag.name.eq_ignore_ascii_case(t.as_bytes())
                {
                    continue;
                }
                if let Some(open) = stack.pop() {
                    let inner = &html[open.end..tag.span.start];
                    if inner.contains(query) {
                        let preview = text_preview(inner.trim(), 80);
                        matches.push(ZapMatch {
                            span: open.start..tag.span.end,
                            tag: display_tag.clone(),
                            text_preview: preview,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    ZapResult {
        matches,
        error: None,
    }
}

fn text_preview(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }
    let mut end = 0;
    for (i, (byte_pos, _)) in s.char_indices().enumerate() {
        if i == max_len {
            end = byte_pos;
            break;
        }
    }
    if end == 0 {
        end = s.len();
    }
    format!("{}...", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(s: &str) -> SimpleSelector {
        parse_selector(s).unwrap()
    }

    // --- Parser tests ---

    #[test]
    fn test_parse_plain_tag() {
        let s = sel("p");
        assert_eq!(s.tag.as_deref(), Some("p"));
        assert!(s.classes.is_empty());
        assert!(s.id.is_none());
        assert!(s.attrs.is_empty());
    }

    #[test]
    fn test_parse_class_only() {
        let s = sel(".warning");
        assert!(s.tag.is_none());
        assert_eq!(s.classes, vec!["warning"]);
    }

    #[test]
    fn test_parse_id_only() {
        let s = sel("#banner");
        assert!(s.tag.is_none());
        assert_eq!(s.id.as_deref(), Some("banner"));
    }

    #[test]
    fn test_parse_tag_class() {
        let s = sel("p.warning");
        assert_eq!(s.tag.as_deref(), Some("p"));
        assert_eq!(s.classes, vec!["warning"]);
    }

    #[test]
    fn test_parse_tag_multi_class() {
        let s = sel("div.a.b.c");
        assert_eq!(s.tag.as_deref(), Some("div"));
        assert_eq!(s.classes, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_parse_tag_id_class() {
        let s = sel("div#main.warning");
        assert_eq!(s.tag.as_deref(), Some("div"));
        assert_eq!(s.id.as_deref(), Some("main"));
        assert_eq!(s.classes, vec!["warning"]);
    }

    #[test]
    fn test_parse_attr_presence() {
        let s = sel("[hidden]");
        assert_eq!(s.attrs, vec![("hidden".into(), None)]);
    }

    #[test]
    fn test_parse_attr_equals() {
        let s = sel("[data-foo=bar]");
        assert_eq!(s.attrs, vec![("data-foo".into(), Some("bar".into()))]);
    }

    #[test]
    fn test_parse_multi_attr() {
        let s = sel("[a][b=c]");
        assert_eq!(
            s.attrs,
            vec![("a".into(), None), ("b".into(), Some("c".into()))]
        );
    }

    #[test]
    fn test_parse_combo() {
        let s = sel("p.warning#banner[hidden][data-x=y]");
        assert_eq!(s.tag.as_deref(), Some("p"));
        assert_eq!(s.id.as_deref(), Some("banner"));
        assert_eq!(s.classes, vec!["warning"]);
        assert_eq!(
            s.attrs,
            vec![("hidden".into(), None), ("data-x".into(), Some("y".into()))]
        );
    }

    #[test]
    fn test_parse_empty_errors() {
        assert!(parse_selector("").is_err());
    }

    #[test]
    fn test_parse_unclosed_attr_errors() {
        assert!(parse_selector("[foo").is_err());
    }

    // --- Matching tests ---

    #[test]
    fn test_match_by_class() {
        let html = r#"<p class="warn">x</p>"#;
        let result = scan_html(html, &sel(".warn"), "x");
        assert_eq!(result.matches.len(), 1);
    }

    #[test]
    fn test_match_by_id() {
        let html = r#"<div id="b">x</div>"#;
        let result = scan_html(html, &sel("#b"), "x");
        assert_eq!(result.matches.len(), 1);
    }

    #[test]
    fn test_match_tag_class_combo() {
        let html = r#"<p class="warn other">x</p>"#;
        let result = scan_html(html, &sel("p.warn"), "x");
        assert_eq!(result.matches.len(), 1);
    }

    #[test]
    fn test_no_match_wrong_class() {
        let html = r#"<p class="other">x</p>"#;
        let result = scan_html(html, &sel("p.warn"), "x");
        assert_eq!(result.matches.len(), 0);
    }

    #[test]
    fn test_match_attr_presence() {
        let html = r#"<div hidden>x</div>"#;
        let result = scan_html(html, &sel("[hidden]"), "x");
        assert_eq!(result.matches.len(), 1);
    }

    #[test]
    fn test_match_attr_value() {
        let html = r#"<span data-x="y">x</span>"#;
        let result = scan_html(html, &sel("[data-x=y]"), "x");
        assert_eq!(result.matches.len(), 1);
    }

    #[test]
    fn test_no_match_attr_value() {
        let html = r#"<span data-x="z">x</span>"#;
        let result = scan_html(html, &sel("[data-x=y]"), "x");
        assert_eq!(result.matches.len(), 0);
    }

    #[test]
    fn test_match_class_any_tag() {
        let html = r#"<div class="warn">x</div>"#;
        let result = scan_html(html, &sel(".warn"), "x");
        assert_eq!(result.matches.len(), 1);
    }

    #[test]
    fn test_class_whitespace_split() {
        let html = r#"<p class="a b c">x</p>"#;
        let result = scan_html(html, &sel(".b"), "x");
        assert_eq!(result.matches.len(), 1);
    }

    #[test]
    fn test_tag_case_insensitive_selector() {
        let html = "<P>Digitalfire</P>";
        let result = scan_html(html, &sel("p"), "Digitalfire");
        assert_eq!(result.matches.len(), 1);
    }

    // --- Existing tests adapted for SimpleSelector ---

    #[test]
    fn test_basic_match() {
        let html = "<p>Digitalfire will shut down on Jan</p>";
        let result = scan_html(html, &sel("p"), "Digitalfire");
        assert!(result.error.is_none());
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].span, 0..html.len());
        assert_eq!(result.matches[0].tag, "p");
    }

    #[test]
    fn test_no_match() {
        let html = "<p>Some other text</p>";
        let result = scan_html(html, &sel("p"), "Digitalfire");
        assert!(result.error.is_none());
        assert_eq!(result.matches.len(), 0);
    }

    #[test]
    fn test_different_tag() {
        let html = "<div>Digitalfire</div>";
        let result = scan_html(html, &sel("p"), "Digitalfire");
        assert!(result.error.is_none());
        assert_eq!(result.matches.len(), 0);
    }

    #[test]
    fn test_nested_elements() {
        let html = "<p>Hello <b>world</b></p>";
        let result = scan_html(html, &sel("p"), "Hello");
        assert!(result.error.is_none());
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].span, 0..html.len());
    }

    #[test]
    fn test_query_in_child_element_via_raw_bytes() {
        let html = "<p><span>Digitalfire</span></p>";
        let result = scan_html(html, &sel("p"), "Digitalfire");
        assert!(result.error.is_none());
        assert_eq!(result.matches.len(), 1);
    }

    #[test]
    fn test_multiple_matches() {
        let html = "<p>foo</p><p>bar</p>";
        let result = scan_html(html, &sel("p"), "foo");
        assert!(result.error.is_none());
        assert_eq!(result.matches.len(), 1);
    }

    #[test]
    fn test_self_closing() {
        let html = "<p/>text";
        let result = scan_html(html, &sel("p"), "text");
        assert!(result.error.is_none());
        assert_eq!(result.matches.len(), 0);
    }

    #[test]
    fn test_void_element() {
        let html = r#"<img src="x.jpg">"#;
        let result = scan_html(html, &sel("img"), "x");
        assert!(result.error.is_none());
        assert_eq!(result.matches.len(), 0);
    }

    #[test]
    fn test_case_insensitive_tag() {
        let html = "<P>Digitalfire</P>";
        let result = scan_html(html, &sel("p"), "Digitalfire");
        assert!(result.error.is_none());
        assert_eq!(result.matches.len(), 1);
    }

    #[test]
    fn test_case_sensitive_query() {
        let html = "<p>Digitalfire</p>";
        let result = scan_html(html, &sel("p"), "digitalfire");
        assert!(result.error.is_none());
        assert_eq!(result.matches.len(), 0);
    }

    #[test]
    fn test_nested_same_tag() {
        let html = "<div><div>target</div></div>";
        let result = scan_html(html, &sel("div"), "target");
        assert!(result.error.is_none());
        // Both outer and inner div contain "target" — outer's raw inner bytes
        // include "<div>target</div>" which contains the substring.
        assert_eq!(result.matches.len(), 2);
        let inner = &result.matches[0];
        assert_eq!(&html[inner.span.start..inner.span.end], "<div>target</div>");
        let outer = &result.matches[1];
        assert_eq!(&html[outer.span.start..outer.span.end], html);
    }

    #[test]
    fn test_unclosed_element() {
        let html = "<p>text";
        let result = scan_html(html, &sel("p"), "text");
        assert!(result.error.is_none());
        assert_eq!(result.matches.len(), 0);
    }

    #[test]
    fn test_empty_content() {
        let html = "<p></p>";
        let result = scan_html(html, &sel("p"), "x");
        assert!(result.error.is_none());
        assert_eq!(result.matches.len(), 0);
    }

    #[test]
    fn test_query_in_attribute_not_inner() {
        let html = r#"<p class="Digitalfire">text</p>"#;
        let result = scan_html(html, &sel("p"), "Digitalfire");
        assert!(result.error.is_none());
        assert_eq!(result.matches.len(), 0);
    }

    #[test]
    fn test_text_preview_truncates() {
        let long = "x".repeat(120);
        let html = format!("<p>{long}</p>");
        let result = scan_html(&html, &sel("p"), "x");
        assert_eq!(result.matches.len(), 1);
        assert!(result.matches[0].text_preview.ends_with("..."));
        assert!(result.matches[0].text_preview.len() < long.len());
    }
}
