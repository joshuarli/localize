use html5gum::Tokenizer;
use html5gum::emitters::default::DefaultEmitter;
use std::ops::Range;

#[derive(Debug, Clone)]
pub struct WebpMatch {
    pub tag: String,
    pub attr: String,
    pub url: String,
    pub new_url: String,
}

fn image_attrs(tag: &[u8]) -> Option<&'static [&'static str]> {
    match tag {
        b"img" | b"source" => Some(&["src", "srcset"]),
        b"video" | b"audio" | b"track" | b"script" | b"embed" | b"iframe" => Some(&["src"]),
        b"a" | b"link" => Some(&["href"]),
        b"object" => Some(&["data"]),
        _ => None,
    }
}

/// Find the byte range of a jpg/jpeg/png extension within a URL in source.
/// Returns None if the URL doesn't have a convertible image extension.
fn find_ext_span(raw_attr: &str, attr_start: usize, url: &str) -> Option<Range<usize>> {
    let path_end = url
        .find('?')
        .unwrap_or_else(|| url.find('#').unwrap_or(url.len()));
    let path = &url[..path_end];
    let lower = path.to_ascii_lowercase();

    let ext_len = if lower.ends_with(".jpg") {
        4
    } else if lower.ends_with(".jpeg") {
        5
    } else if lower.ends_with(".png") {
        4
    } else {
        return None;
    };

    let ext_start_in_url = path.len() - ext_len;
    let url_offset = raw_attr.find(url)?;
    let abs_start = attr_start + url_offset + ext_start_in_url;
    Some(abs_start..abs_start + ext_len)
}

/// Replace the extension in a URL with `.webp`.
fn to_webp_url(url: &str) -> String {
    let path_end = url
        .find('?')
        .unwrap_or_else(|| url.find('#').unwrap_or(url.len()));
    let path = &url[..path_end];
    let rest = &url[path_end..];

    let lower = path.to_ascii_lowercase();
    let new_path = if lower.ends_with(".jpg") {
        format!("{}.webp", &path[..path.len() - 4])
    } else if lower.ends_with(".jpeg") {
        format!("{}.webp", &path[..path.len() - 5])
    } else if lower.ends_with(".png") {
        format!("{}.webp", &path[..path.len() - 4])
    } else {
        return url.to_string();
    };

    format!("{new_path}{rest}")
}

/// Parse srcset entries. Returns Vec of (url, descriptor).
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
        let descriptor = if tokens.len() > 1 {
            Some(tokens[1..].join(" "))
        } else {
            None
        };
        entries.push((tokens[0].to_string(), descriptor));
    }
    entries
}

/// Scan an HTML string for image URLs that can be converted to webp.
pub fn scan_towebp(html: &str) -> Vec<WebpMatch> {
    let mut matches: Vec<WebpMatch> = Vec::new();

    let tokenizer = Tokenizer::new_with_emitter(html, DefaultEmitter::<usize>::new_with_span());

    for token_result in tokenizer {
        let token = match token_result {
            Ok(t) => t,
            Err(_) => continue,
        };

        if let html5gum::Token::StartTag(tag) = token {
            let tag_name = std::str::from_utf8(&tag.name[..]).unwrap_or("");
            let attrs_to_check = match image_attrs(&tag.name[..]) {
                Some(a) => a,
                None => continue,
            };

            for (name, attr) in &tag.attributes {
                let attr_name = std::str::from_utf8(&name[..]).unwrap_or("");
                if !attrs_to_check.contains(&attr_name) {
                    continue;
                }
                let attr_value = std::str::from_utf8(&attr[..]).unwrap_or("");
                let raw = &html[attr.span.start..attr.span.end];

                if attr_name == "srcset" {
                    for (url, descriptor) in parse_srcset_entries(attr_value) {
                        if find_ext_span(raw, attr.span.start, &url).is_some() {
                            let new_url = to_webp_url(&url);
                            let new_entry = if let Some(ref d) = descriptor {
                                format!("{new_url} {d}")
                            } else {
                                new_url.clone()
                            };
                            matches.push(WebpMatch {
                                tag: tag_name.to_string(),
                                attr: "srcset".into(),
                                url: format!(
                                    "{url} {descriptor}",
                                    descriptor = descriptor.as_deref().unwrap_or("")
                                ),
                                new_url: new_entry,
                            });
                        }
                    }
                } else if find_ext_span(raw, attr.span.start, attr_value).is_some() {
                    let new_url = to_webp_url(attr_value);
                    matches.push(WebpMatch {
                        tag: tag_name.to_string(),
                        attr: attr_name.to_string(),
                        url: attr_value.to_string(),
                        new_url,
                    });
                }
            }
        }
    }

    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_webp_url_jpg() {
        assert_eq!(to_webp_url("photo.jpg"), "photo.webp");
        assert_eq!(
            to_webp_url("https://cdn.example.com/photo.JPG"),
            "https://cdn.example.com/photo.webp"
        );
    }

    #[test]
    fn test_to_webp_url_jpeg() {
        assert_eq!(to_webp_url("photo.jpeg"), "photo.webp");
        assert_eq!(to_webp_url("photo.JPEG"), "photo.webp");
    }

    #[test]
    fn test_to_webp_url_png() {
        assert_eq!(to_webp_url("logo.png"), "logo.webp");
        assert_eq!(to_webp_url("logo.PNG"), "logo.webp");
    }

    #[test]
    fn test_to_webp_url_with_query() {
        assert_eq!(to_webp_url("photo.jpg?w=800"), "photo.webp?w=800");
        assert_eq!(to_webp_url("photo.png?v=2"), "photo.webp?v=2");
    }

    #[test]
    fn test_to_webp_url_with_fragment() {
        assert_eq!(to_webp_url("photo.jpg#hash"), "photo.webp#hash");
    }

    #[test]
    fn test_to_webp_url_no_match() {
        assert_eq!(to_webp_url("photo.webp"), "photo.webp");
        assert_eq!(to_webp_url("photo.gif"), "photo.gif");
        assert_eq!(to_webp_url("styles.css"), "styles.css");
    }

    #[test]
    fn test_scan_img_src() {
        let html = r#"<img src="photo.jpg" alt="x">"#;
        let matches = scan_towebp(html);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].tag, "img");
        assert_eq!(matches[0].attr, "src");
        assert_eq!(matches[0].url, "photo.jpg");
        assert_eq!(matches[0].new_url, "photo.webp");
    }

    #[test]
    fn test_scan_img_src_remote() {
        let html = r#"<img src="https://cdn.example.com/photo.png">"#;
        let matches = scan_towebp(html);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].url, "https://cdn.example.com/photo.png");
        assert_eq!(matches[0].new_url, "https://cdn.example.com/photo.webp");
    }

    #[test]
    fn test_scan_srcset() {
        let html = r#"<img srcset="small.jpg 400w, large.png 800w">"#;
        let matches = scan_towebp(html);
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].url, "small.jpg 400w");
        assert_eq!(matches[0].new_url, "small.webp 400w");
        assert_eq!(matches[1].url, "large.png 800w");
        assert_eq!(matches[1].new_url, "large.webp 800w");
    }

    #[test]
    fn test_scan_a_href() {
        let html = r#"<a href="image.jpeg">link</a>"#;
        let matches = scan_towebp(html);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].tag, "a");
        assert_eq!(matches[0].attr, "href");
    }

    #[test]
    fn test_scan_link_href() {
        let html = r#"<link rel="icon" href="favicon.png">"#;
        let matches = scan_towebp(html);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].tag, "link");
    }

    #[test]
    fn test_scan_ignores_non_image() {
        let html = r#"<img src="photo.webp"><img src="video.mp4"><a href="page.html">"#;
        let matches = scan_towebp(html);
        assert_eq!(matches.len(), 0);
    }

    #[test]
    fn test_scan_ignores_data_uri() {
        let html = r#"<img src="data:image/png;base64,abc">"#;
        let matches = scan_towebp(html);
        assert_eq!(matches.len(), 0);
    }

    #[test]
    fn test_scan_video_poster_not_checked() {
        let html = r#"<video poster="thumb.jpg" src="video.mp4">"#;
        let matches = scan_towebp(html);
        assert_eq!(matches.len(), 0); // poster not in src/href/srcset
    }
}
