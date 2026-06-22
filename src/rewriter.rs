//! Unified HTML modification via lol_html.
//!
//! apply, clean, and towebp use lol_html element handlers for correct, single-pass
//! HTML rewriting. zap uses html5gum for text-aware detection + span-based removal
//! (lol_html's streaming model can't retroactively remove elements based on text
//! content discovered after the element handler fires).

use lol_html::{RewriteStrSettings, element, rewrite_str};
use regex_lite::Regex;
use rustc_hash::{FxHashMap, FxHashSet};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::LazyLock;

use crate::clean::{is_local_link, link_exists};
use crate::zap::{ZapMatch, scan_html};

static CSS_URL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"url\(\s*["']?\s*(https?://[^"'\s()]+)\s*["']?\s*\)"#).unwrap());

/// Compute the relative path from an HTML file's directory to an asset.
///
/// Both paths are relative to the scan root.
pub fn compute_relative_path(html_file: &str, asset_rel: &str) -> String {
    let html_dir = Path::new(html_file).parent().unwrap_or(Path::new(""));
    let rel = if html_dir.as_os_str().is_empty() {
        asset_rel.to_string()
    } else {
        html_dir.join(asset_rel).to_string_lossy().to_string()
    };
    let rel = rel.replace('\\', "/");
    if !html_dir.as_os_str().is_empty() {
        relative_path_simple(html_file, asset_rel)
    } else {
        rel
    }
}

fn relative_path_simple(html_file: &str, asset_rel: &str) -> String {
    let html_dir = Path::new(html_file).parent().unwrap_or(Path::new(""));
    let html_parts: Vec<&str> = html_dir
        .components()
        .map(|c| c.as_os_str().to_str().unwrap_or(""))
        .filter(|p| !p.is_empty())
        .collect();
    let asset_parts: Vec<&str> = Path::new(asset_rel)
        .components()
        .map(|c| c.as_os_str().to_str().unwrap_or(""))
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();

    let common = html_parts
        .iter()
        .zip(asset_parts.iter())
        .take_while(|(a, b)| a == b)
        .count();

    let up = html_parts.len() - common;
    let mut result = String::new();
    for _ in 0..up {
        result.push_str("../");
    }
    for part in &asset_parts[common..] {
        if !result.is_empty() && !result.ends_with('/') {
            result.push('/');
        }
        result.push_str(part);
    }
    if result.is_empty() {
        asset_rel.to_string()
    } else {
        result
    }
}

/// Rewrite srcset attribute value: replace each URL with its local relative path.
fn rewrite_srcset_value(val: &str, url_map: &FxHashMap<String, String>, file_path: &str) -> String {
    val.split(',')
        .map(|p| {
            let fields: Vec<&str> = p.split_whitespace().collect();
            if fields.is_empty() {
                return p.trim().to_string();
            }
            if let Some(local_rel) = url_map.get(fields[0]) {
                let rel_path = compute_relative_path(file_path, local_rel);
                let mut result = rel_path;
                for f in &fields[1..] {
                    result.push(' ');
                    result.push_str(f);
                }
                result
            } else {
                p.trim().to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Replace the image extension in a URL with `.webp`.
fn towebp_url(url: &str) -> String {
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

/// Returns true if the URL path ends with a convertible image extension.
fn has_image_ext(url: &str) -> bool {
    let path_end = url
        .find('?')
        .unwrap_or_else(|| url.find('#').unwrap_or(url.len()));
    let path = &url[..path_end];
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".jpg") || lower.ends_with(".jpeg") || lower.ends_with(".png")
}

// ---------------------------------------------------------------------------
// apply: URL rewriting
// ---------------------------------------------------------------------------

/// Rewrite HTML for the `apply` command: replace remote URLs with local paths,
/// and rename attributes for broken URLs.
pub fn apply_html(
    html: &str,
    url_map: &FxHashMap<String, String>,
    broken_urls: &FxHashSet<String>,
    file_path: &str,
) -> Result<String, String> {
    let url_map: Rc<FxHashMap<String, String>> = Rc::new(url_map.clone());
    let broken_urls: Rc<FxHashSet<String>> = Rc::new(broken_urls.clone());
    let fp: Rc<str> = Rc::from(file_path);

    let mut handlers = Vec::new();

    // Helper: rewrite a single-valued attribute (src, href, data, content).
    let build_single = |sel: &'static str, attr: &'static str| {
        let um = url_map.clone();
        let bu = broken_urls.clone();
        let fp = fp.clone();
        element!(sel, move |el| {
            if let Some(val) = el.get_attribute(attr) {
                if let Some(local_rel) = um.get(&val) {
                    let rel_path = compute_relative_path(&fp, local_rel);
                    el.set_attribute(attr, &rel_path).ok();
                } else if bu.contains(&val) {
                    let broken_name = format!("data-broken-{attr}");
                    el.set_attribute(&broken_name, &val).ok();
                    el.remove_attribute(attr);
                }
            }
            Ok(())
        })
    };

    handlers.push(build_single("img[src]", "src"));
    handlers.push(build_single("source[src]", "src"));
    handlers.push(build_single("video[src]", "src"));
    handlers.push(build_single("audio[src]", "src"));
    handlers.push(build_single("track[src]", "src"));
    handlers.push(build_single("object[data]", "data"));
    handlers.push(build_single("script[src]", "src"));
    handlers.push(build_single("a[href]", "href"));
    handlers.push(build_single("link[href]", "href"));

    // img[srcset]
    {
        let um = url_map.clone();
        let fp = fp.clone();
        handlers.push(element!("img[srcset]", move |el| {
            if let Some(val) = el.get_attribute("srcset") {
                let rewritten = rewrite_srcset_value(&val, &um, &fp);
                el.set_attribute("srcset", &rewritten).ok();
            }
            Ok(())
        }));
    }

    // source[srcset]
    {
        let um = url_map.clone();
        let fp = fp.clone();
        handlers.push(element!("source[srcset]", move |el| {
            if let Some(val) = el.get_attribute("srcset") {
                let rewritten = rewrite_srcset_value(&val, &um, &fp);
                el.set_attribute("srcset", &rewritten).ok();
            }
            Ok(())
        }));
    }

    // meta[property="og:image"][content], meta[name="twitter:image"][content]
    {
        let um = url_map.clone();
        let bu = broken_urls.clone();
        let fp = fp.clone();
        handlers.push(element!(
            "meta[property=\"og:image\"][content], meta[name=\"twitter:image\"][content]",
            move |el| {
                if let Some(val) = el.get_attribute("content") {
                    if let Some(local_rel) = um.get(&val) {
                        let rel_path = compute_relative_path(&fp, local_rel);
                        el.set_attribute("content", &rel_path).ok();
                    } else if bu.contains(&val) {
                        el.set_attribute("data-broken-content", &val).ok();
                        el.remove_attribute("content");
                    }
                }
                Ok(())
            }
        ));
    }

    // *[style] — rewrite CSS url() references
    {
        let um = url_map.clone();
        let fp = fp.clone();
        handlers.push(element!("*[style]", move |el| {
            if let Some(style_val) = el.get_attribute("style") {
                let rewritten = rewrite_style_value(&style_val, &um, &fp);
                if rewritten != style_val {
                    el.set_attribute("style", &rewritten).ok();
                }
            }
            Ok(())
        }));
    }

    let settings = RewriteStrSettings {
        element_content_handlers: handlers,
        ..RewriteStrSettings::default()
    };

    rewrite_str(html, settings).map_err(|e| format!("lol-html: {e}"))
}

/// Rewrite CSS url() references in a style attribute value.
fn rewrite_style_value(
    style: &str,
    url_map: &FxHashMap<String, String>,
    file_path: &str,
) -> String {
    let mut result = style.to_string();
    let caps: Vec<_> = CSS_URL_RE
        .captures_iter(style)
        .map(|m| {
            let full = m.get(0).unwrap();
            let url_match = m.get(1).unwrap();
            (full.range(), url_match.as_str().to_string())
        })
        .collect();
    for (range, url) in caps.into_iter().rev() {
        if let Some(local_rel) = url_map.get(&url) {
            let rel_path = compute_relative_path(file_path, local_rel);
            let replacement = format!("url({rel_path})");
            result.replace_range(range, &replacement);
        }
    }
    result
}

// ---------------------------------------------------------------------------
// clean: broken link removal
// ---------------------------------------------------------------------------

/// Rewrite HTML for the `clean` command: remove elements with broken local links.
/// Unwraps non-void elements (a, area, iframe, object), removes void elements
/// and script entirely.
pub fn clean_html(
    html: &str,
    href_set: &FxHashSet<String>,
    file_path: &str,
) -> Result<String, String> {
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
    let href_set: Rc<FxHashSet<String>> = Rc::new(href_set.clone());
    let dh: Rc<str> = Rc::from(doc_href);
    let scratch: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    let decode_buf: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));

    let mut handlers = Vec::new();

    // Single-valued attributes on elements that should be unwrapped (remove tags, keep content).
    let build_unwrap = |sel: &'static str, attr: &'static str| {
        let hs = href_set.clone();
        let dh = dh.clone();
        let scr = scratch.clone();
        let dec = decode_buf.clone();
        element!(sel, move |el| {
            if let Some(val) = el.get_attribute(attr)
                && is_local_link(&val)
            {
                let mut s = scr.borrow_mut();
                let mut d = dec.borrow_mut();
                if !link_exists(&dh, doc_is_index, &val, &mut s, &mut d, &hs) {
                    el.remove_and_keep_content();
                }
            }
            Ok(())
        })
    };

    handlers.push(build_unwrap("a[href]", "href"));
    handlers.push(build_unwrap("area[href]", "href"));
    handlers.push(build_unwrap("iframe[src]", "src"));
    handlers.push(build_unwrap("object[data]", "data"));

    // Void elements — remove entirely.
    let build_remove = |sel: &'static str, attr: &'static str| {
        let hs = href_set.clone();
        let dh = dh.clone();
        let scr = scratch.clone();
        let dec = decode_buf.clone();
        element!(sel, move |el| {
            if let Some(val) = el.get_attribute(attr)
                && is_local_link(&val)
            {
                let mut s = scr.borrow_mut();
                let mut d = dec.borrow_mut();
                if !link_exists(&dh, doc_is_index, &val, &mut s, &mut d, &hs) {
                    el.remove();
                }
            }
            Ok(())
        })
    };

    handlers.push(build_remove("link[href]", "href"));
    handlers.push(build_remove("img[src]", "src"));

    // img[srcset] — remove if any URL in srcset is broken
    {
        let hs = href_set.clone();
        let dh = dh.clone();
        let scr = scratch.clone();
        let dec = decode_buf.clone();
        handlers.push(element!("img[srcset]", move |el| {
            if let Some(val) = el.get_attribute("srcset") {
                for entry in val.split(',') {
                    let fields: Vec<&str> = entry.split_whitespace().collect();
                    if fields.is_empty() {
                        continue;
                    }
                    let url = fields[0].trim();
                    if is_local_link(url) {
                        let mut s = scr.borrow_mut();
                        let mut d = dec.borrow_mut();
                        if !link_exists(&dh, doc_is_index, url, &mut s, &mut d, &hs) {
                            el.remove();
                            return Ok(());
                        }
                    }
                }
            }
            Ok(())
        }));
    }

    // script[src] — remove entire element (including body) when src is broken.
    {
        let hs = href_set.clone();
        let dh = dh.clone();
        let scr = scratch.clone();
        let dec = decode_buf.clone();
        handlers.push(element!("script[src]", move |el| {
            if let Some(val) = el.get_attribute("src")
                && is_local_link(&val)
            {
                let mut s = scr.borrow_mut();
                let mut d = dec.borrow_mut();
                if !link_exists(&dh, doc_is_index, &val, &mut s, &mut d, &hs) {
                    el.remove();
                }
            }
            Ok(())
        }));
    }

    let settings = RewriteStrSettings {
        element_content_handlers: handlers,
        ..RewriteStrSettings::default()
    };

    rewrite_str(html, settings).map_err(|e| format!("lol-html: {e}"))
}

// ---------------------------------------------------------------------------
// towebp: image extension rewriting
// ---------------------------------------------------------------------------

/// Resolve a URL from an HTML file to a normalized relative path.
/// Query strings and fragments are stripped before resolution so that
/// `photo.jpg?w=800` resolves to `photo.jpg`.
pub(crate) fn resolve_html_url(html_rel: &str, url: &str) -> String {
    if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("data:") {
        return url.to_string();
    }
    // Strip query string and fragment before resolving the filesystem path.
    let path_only = url
        .find('?')
        .unwrap_or_else(|| url.find('#').unwrap_or(url.len()));
    let path = &url[..path_only];
    let html_dir = Path::new(html_rel).parent().unwrap_or(Path::new(""));
    let combined = html_dir.join(path);
    let mut parts: Vec<&str> = Vec::new();
    for c in combined.components() {
        match c {
            std::path::Component::ParentDir => {
                parts.pop();
            }
            std::path::Component::CurDir => {}
            std::path::Component::Normal(p) => {
                if let Some(s) = p.to_str() {
                    parts.push(s);
                }
            }
            _ => {}
        }
    }
    if parts.is_empty() {
        return ".".to_string();
    }
    parts.join("/")
}

/// Rewrite HTML for the `towebp` command: replace .jpg/.jpeg/.png extensions
/// with .webp in src, href, and srcset attributes — but only for images that
/// were successfully converted (present in `converted`).
pub fn towebp_html(
    html: &str,
    file_rel: &str,
    converted: &FxHashSet<String>,
) -> Result<String, String> {
    let file_rel: Rc<str> = Rc::from(file_rel);
    let converted: Rc<FxHashSet<String>> = Rc::new(converted.clone());

    let mut handlers = Vec::new();

    // Single-valued attributes.
    let build_single = |sel: &'static str, attrs: &'static [&'static str]| {
        let attr_list: Vec<&'static str> = attrs.to_vec();
        let fr = file_rel.clone();
        let cv = converted.clone();
        element!(sel, move |el| {
            for attr in &attr_list {
                if let Some(val) = el.get_attribute(attr)
                    && has_image_ext(&val)
                {
                    let resolved = resolve_html_url(&fr, &val);
                    if cv.contains(&resolved) {
                        let new_url = towebp_url(&val);
                        el.set_attribute(attr, &new_url).ok();
                    }
                }
            }
            Ok(())
        })
    };

    handlers.push(build_single(
        "img[src], source[src], video[src], audio[src], track[src], embed[src], iframe[src], script[src]",
        &["src"],
    ));
    handlers.push(build_single("a[href], link[href]", &["href"]));
    handlers.push(build_single("object[data]", &["data"]));

    // srcset attributes — rewrite each URL in the list (only if the URL's
    // resolved file was converted).
    {
        let fr = file_rel.clone();
        let cv = converted.clone();
        handlers.push(element!("img[srcset], source[srcset]", move |el| {
            if let Some(val) = el.get_attribute("srcset") {
                let rewritten = towebp_srcset_value_gated(&val, &fr, &cv);
                if rewritten != val {
                    el.set_attribute("srcset", &rewritten).ok();
                }
            }
            Ok(())
        }));
    }

    let settings = RewriteStrSettings {
        element_content_handlers: handlers,
        ..RewriteStrSettings::default()
    };

    rewrite_str(html, settings).map_err(|e| format!("lol-html: {e}"))
}

/// Like towebp_srcset_value, but only rewrites URLs whose resolved file path
/// is in the `converted` set.
fn towebp_srcset_value_gated(
    val: &str,
    file_rel: &str,
    converted: &FxHashSet<String>,
) -> String {
    val.split(',')
        .map(|p| {
            let fields: Vec<&str> = p.split_whitespace().collect();
            if fields.is_empty() {
                return p.trim().to_string();
            }
            let resolved = resolve_html_url(file_rel, fields[0]);
            if converted.contains(&resolved) {
                let new_url = towebp_url(fields[0]);
                if fields.len() > 1 {
                    format!("{} {}", new_url, fields[1..].join(" "))
                } else {
                    new_url
                }
            } else {
                p.trim().to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------------------------------------------------------------------
// zap: element removal by CSS selector + text content
// ---------------------------------------------------------------------------

/// Rewrite HTML for the `zap` command: remove elements matching a CSS selector
/// whose inner text contains the query string.
///
/// Uses html5gum for text-aware detection (lol_html can't retroactively remove
/// elements based on text content), then applies span-based removal.
pub fn zap_html(
    html: &str,
    selector: &crate::zap::SimpleSelector,
    query: &str,
) -> Result<(String, Vec<ZapMatch>), String> {
    let result = scan_html(html, selector, query);
    if let Some(err) = &result.error {
        return Err(err.clone());
    }

    if result.matches.is_empty() {
        return Ok((html.to_string(), Vec::new()));
    }

    let mut modified = html.to_string();
    let mut sorted: Vec<&ZapMatch> = result.matches.iter().collect();
    sorted.sort_by_key(|m| std::cmp::Reverse(m.span.start));
    for m in &sorted {
        modified.replace_range(m.span.clone(), "");
    }

    Ok((modified, result.matches))
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_relative_same_dir() {
        let result = compute_relative_path(
            "index.html",
            "assets/external/cdn.example.com/ab/12345678-logo.png",
        );
        assert!(result.contains("assets/external/"));
        assert!(!result.contains(".."));
    }

    #[test]
    fn test_compute_relative_subdir() {
        let result = compute_relative_path(
            "pages/about.html",
            "assets/external/cdn/ab/12345678-img.png",
        );
        assert!(result.starts_with("../"));
        assert!(result.contains("assets/external/"));
    }

    #[test]
    fn test_compute_relative_deeply_nested() {
        let result = compute_relative_path("a/b/c/d.html", "assets/x.jpg");
        assert_eq!(result, "../../../assets/x.jpg");
    }

    #[test]
    fn test_towebp_url() {
        assert_eq!(towebp_url("photo.jpg"), "photo.webp");
        assert_eq!(towebp_url("photo.jpeg"), "photo.webp");
        assert_eq!(towebp_url("photo.png"), "photo.webp");
        assert_eq!(
            towebp_url("https://cdn.example.com/photo.JPG"),
            "https://cdn.example.com/photo.webp"
        );
        assert_eq!(towebp_url("photo.jpg?w=800"), "photo.webp?w=800");
        assert_eq!(towebp_url("photo.png#hash"), "photo.webp#hash");
        assert_eq!(towebp_url("photo.webp"), "photo.webp");
        assert_eq!(towebp_url("photo.gif"), "photo.gif");
    }

    #[test]
    fn test_has_image_ext() {
        assert!(has_image_ext("photo.jpg"));
        assert!(has_image_ext("photo.JPEG"));
        assert!(has_image_ext("photo.png"));
        assert!(has_image_ext("https://cdn.example.com/img.jpg?w=400"));
        assert!(!has_image_ext("photo.webp"));
        assert!(!has_image_ext("photo.gif"));
        assert!(!has_image_ext("styles.css"));
    }

    #[test]
    fn test_towebp_srcset() {
        let input = "small.jpg 400w, large.png 800w";
        let mut converted = FxHashSet::default();
        converted.insert("small.jpg".into());
        converted.insert("large.png".into());
        let output = towebp_srcset_value_gated(input, "index.html", &converted);
        assert_eq!(output, "small.webp 400w, large.webp 800w");
    }

    #[test]
    fn test_towebp_html_basic() {
        let html = r#"<img src="photo.jpg" alt="x"><img src="logo.png">"#;
        let mut converted = FxHashSet::default();
        converted.insert("photo.jpg".into());
        converted.insert("logo.png".into());
        let result = towebp_html(html, "index.html", &converted).unwrap();
        assert_eq!(
            result,
            r#"<img src="photo.webp" alt="x"><img src="logo.webp">"#
        );
    }

    #[test]
    fn test_towebp_html_srcset() {
        let html = r#"<img srcset="small.jpg 400w, large.png 800w">"#;
        let mut converted = FxHashSet::default();
        converted.insert("small.jpg".into());
        converted.insert("large.png".into());
        let result = towebp_html(html, "index.html", &converted).unwrap();
        assert_eq!(result, r#"<img srcset="small.webp 400w, large.webp 800w">"#);
    }

    #[test]
    fn test_towebp_html_query_preserved() {
        let html = r#"<img src="photo.jpg?w=800&amp;h=600">"#;
        let mut converted = FxHashSet::default();
        converted.insert("photo.jpg".into());
        let result = towebp_html(html, "index.html", &converted).unwrap();
        assert!(result.contains("photo.webp?w=800"), "got: {result}");
    }

    #[test]
    fn test_towebp_html_ignores_non_image() {
        let html = r#"<img src="photo.webp"><img src="video.mp4"><a href="page.html">"#;
        let converted = FxHashSet::default();
        let result = towebp_html(html, "index.html", &converted).unwrap();
        assert_eq!(result, html);
    }

    #[test]
    fn test_towebp_html_gated() {
        // Only photo.jpg was converted; logo.png should NOT be rewritten.
        let html = r#"<img src="photo.jpg" alt="x"><img src="logo.png">"#;
        let mut converted = FxHashSet::default();
        converted.insert("photo.jpg".into());
        let result = towebp_html(html, "index.html", &converted).unwrap();
        assert_eq!(
            result,
            r#"<img src="photo.webp" alt="x"><img src="logo.png">"#
        );
    }

    #[test]
    fn test_resolve_html_url_same_file_different_html_paths() {
        // Regression: the unique-image dedup relies on resolve_html_url returning
        // the same key for the same image file regardless of which HTML file
        // references it.
        let from_root = resolve_html_url("index.html", "images/photo.jpg");
        let from_subdir = resolve_html_url("blog/post.html", "../images/photo.jpg");
        assert_eq!(from_root, "images/photo.jpg");
        assert_eq!(from_subdir, "images/photo.jpg");
    }

    #[test]
    fn test_towebp_html_dedup_across_files() {
        // Same image referenced from two HTML files at different paths.  When
        // the image is in the converted set, both files' references get rewritten.
        let mut converted = FxHashSet::default();
        converted.insert("images/photo.jpg".into());

        let html_root = r#"<img src="images/photo.jpg">"#;
        let result = towebp_html(html_root, "index.html", &converted).unwrap();
        assert_eq!(result, r#"<img src="images/photo.webp">"#);

        let html_sub = r#"<img src="../images/photo.jpg">"#;
        let result = towebp_html(html_sub, "blog/post.html", &converted).unwrap();
        assert_eq!(result, r#"<img src="../images/photo.webp">"#);
    }

    #[test]
    fn test_towebp_html_nested_parent_refs() {
        // Regression: nested HTML files with `../` paths must resolve to the
        // same key the CLI phase 1 used when populating `converted`.  This is
        // the pattern from the real-world bug — product pages in subdirectories
        // referencing images via `../../_grab/.../file.jpg`.
        let mut converted = FxHashSet::default();
        converted.insert("_grab/example.com/uploads/2021/photo.jpg".into());

        // Three levels deep, referencing up two levels then into _grab.
        let html = r#"<img src="../../_grab/example.com/uploads/2021/photo.jpg">"#;
        let result =
            towebp_html(html, "product-category/blades-diy/index.html", &converted).unwrap();
        assert_eq!(
            result,
            r#"<img src="../../_grab/example.com/uploads/2021/photo.webp">"#
        );
    }

    #[test]
    fn test_towebp_html_no_conversions_leaves_html_untouched() {
        // Regression: with an empty converted set (dry-run, or nothing converted),
        // towebp_html must leave all references intact so the scan output is
        // accurate.
        let converted = FxHashSet::default();
        let html = r#"<img src="photo.jpg"><img src="logo.png"><img srcset="a.jpg 1x, b.png 2x">"#;
        let result = towebp_html(html, "index.html", &converted).unwrap();
        assert_eq!(result, html);
    }

    #[test]
    fn test_apply_html_single_src() {
        let mut url_map = FxHashMap::default();
        url_map.insert(
            "https://cdn.example.com/logo.png".to_string(),
            "assets/external/cdn/ab/12345678-logo.png".to_string(),
        );
        let html = r#"<img src="https://cdn.example.com/logo.png">"#;
        let result = apply_html(html, &url_map, &FxHashSet::default(), "index.html").unwrap();
        assert!(!result.contains("https://cdn.example.com/logo.png"));
        assert!(result.contains("assets/external/cdn/ab/12345678-logo.png"));
    }

    #[test]
    fn test_apply_html_broken_url() {
        let mut broken = FxHashSet::default();
        broken.insert("https://cdn.example.com/gone.png".to_string());
        let html = r#"<img src="https://cdn.example.com/gone.png" alt="x">"#;
        let result = apply_html(html, &FxHashMap::default(), &broken, "index.html").unwrap();
        assert!(
            result.contains("data-broken-src"),
            "expected data-broken-src in: {result}"
        );
        assert!(
            result.contains("https://cdn.example.com/gone.png"),
            "original URL preserved"
        );
        // lol_html removes src and adds data-broken-src with the original URL.
        assert!(
            result.contains("data-broken-src=\"https://cdn.example.com/gone.png\""),
            "expected data-broken-src with URL preserved: {result}"
        );
        assert!(
            !result.contains(" src=\""),
            "src attribute should be removed: {result}"
        );
    }

    #[test]
    fn test_apply_html_srcset() {
        let mut url_map = FxHashMap::default();
        url_map.insert("https://a.com/s.jpg".to_string(), "local/s.jpg".to_string());
        url_map.insert("https://a.com/l.jpg".to_string(), "local/l.jpg".to_string());
        let html = r#"<img srcset="https://a.com/s.jpg 400w, https://a.com/l.jpg 800w">"#;
        let result = apply_html(html, &url_map, &FxHashSet::default(), "index.html").unwrap();
        assert!(result.contains("local/s.jpg 400w"));
        assert!(result.contains("local/l.jpg 800w"));
        assert!(!result.contains("https://a.com/"));
    }

    #[test]
    fn test_clean_html_unwrap_a() {
        let mut hs = FxHashSet::default();
        // Only the document itself exists; picture/926.html does not.
        hs.insert("material/test".to_string());
        let html = r#"<p><a href="../picture/926.html">click <b>here</b></a></p>"#;
        let result = clean_html(html, &hs, "material/test.html").unwrap();
        assert_eq!(result, "<p>click <b>here</b></p>");
    }

    #[test]
    fn test_clean_html_remove_img() {
        let hs = FxHashSet::default(); // nothing exists
        let html = r#"<div><img src="broken.jpg" alt="x"></div>"#;
        let result = clean_html(html, &hs, "test.html").unwrap();
        assert_eq!(result, "<div></div>");
    }

    #[test]
    fn test_clean_html_remove_script() {
        let hs = FxHashSet::default();
        let html = r#"<script src="broken.js"></script>"#;
        let result = clean_html(html, &hs, "test.html").unwrap();
        assert_eq!(result.trim(), "");
    }

    #[test]
    fn test_clean_html_ignores_valid_link() {
        let mut hs = FxHashSet::default();
        hs.insert("about.html".to_string());
        let html = r#"<a href="about.html">About</a>"#;
        let result = clean_html(html, &hs, "test.html").unwrap();
        assert_eq!(result, html);
    }

    #[test]
    fn test_clean_html_ignores_remote() {
        let hs = FxHashSet::default();
        let html = r#"<a href="https://example.com/page.html">link</a>"#;
        let result = clean_html(html, &hs, "test.html").unwrap();
        assert_eq!(result, html);
    }
}
