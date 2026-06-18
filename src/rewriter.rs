use crate::scanner::MediaReference;
use rustc_hash::{FxHashMap, FxHashSet};
use std::ops::Range;
use std::path::Path;

/// Compute the relative path from an HTML file's directory to an asset.
///
/// Both paths are relative to the scan root.
pub fn compute_relative_path(html_file: &str, asset_rel: &str) -> String {
    let html_dir = std::path::Path::new(html_file)
        .parent()
        .unwrap_or(Path::new(""));
    let rel = if html_dir.as_os_str().is_empty() {
        asset_rel.to_string()
    } else {
        html_dir.join(asset_rel).to_string_lossy().to_string()
    };
    // Normalize to forward slashes.
    let rel = rel.replace('\\', "/");
    // If html_dir is non-empty, compute proper relative path.
    if !html_dir.as_os_str().is_empty() {
        // Manual relative path computation.
        relative_path_simple(html_file, asset_rel)
    } else {
        rel
    }
}

/// Compute relative path from dir_of(html_file) to asset_rel.
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

    // Find common prefix length.
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

/// Rewrite a single HTML file, replacing remote URLs with local relative paths.
///
/// URLs in `broken_urls` have their attribute renamed to `data-broken-*` so
/// the browser won't request them, but the original URL is preserved in source.
pub fn rewrite_file(
    path: &Path,
    refs: &[&MediaReference],
    url_map: &FxHashMap<String, String>,
    broken_urls: &FxHashSet<String>,
) -> Result<(), String> {
    let mut content = std::fs::read_to_string(path).map_err(|e| format!("read: {e}"))?;

    // Sort by span.start descending so earlier replacements don't shift later spans.
    let mut sorted: Vec<&&MediaReference> = refs.iter().collect();
    sorted.sort_by_key(|r| std::cmp::Reverse(r.span.start));

    for r in &sorted {
        if broken_urls.contains(&r.url) {
            if let Some(name_span) = find_attr_name_span(&content, r.span.start) {
                let new_name = format!("data-broken-{}", &content[name_span.clone()]);
                content.replace_range(name_span, &new_name);
            }
        } else if let Some(local_rel) = url_map.get(&r.url) {
            let rel_path = compute_relative_path(&r.file_path, local_rel);
            content.replace_range(r.span.start..r.span.end, &rel_path);
        }
    }

    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &content).map_err(|e| format!("write tmp: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename: {e}"))?;

    Ok(())
}

/// Find the byte range of the attribute name that owns the URL starting at `url_start`.
fn find_attr_name_span(content: &str, url_start: usize) -> Option<Range<usize>> {
    let bytes = content.as_bytes();
    let mut i = url_start;
    // Skip back past opening quote (if any).
    if i > 0 && (bytes[i - 1] == b'"' || bytes[i - 1] == b'\'') {
        i -= 1;
    }
    // Skip whitespace between quote and `=`.
    while i > 0 && bytes[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    // Skip `=`.
    if i > 0 && bytes[i - 1] == b'=' {
        i -= 1;
    } else {
        return None;
    }
    // Skip whitespace before `=`.
    while i > 0 && bytes[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    // i is at end of attribute name. Walk back to find its start.
    let name_end = i;
    while i > 0 && !bytes[i - 1].is_ascii_whitespace() && bytes[i - 1] != b'<' {
        i -= 1;
    }
    Some(i..name_end)
}

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
    fn test_rewrite_single() {
        let tmpdir = tempfile::tempdir().unwrap();
        let file_path = tmpdir.path().join("test.html");
        let html = r#"<img src="https://cdn.example.com/logo.png">"#;
        std::fs::write(&file_path, html).unwrap();

        let refs = crate::scanner::scan_file("test.html", html).references;
        assert_eq!(refs.len(), 1);
        let refs: Vec<&MediaReference> = refs.iter().collect();

        let mut url_map = FxHashMap::default();
        url_map.insert(
            "https://cdn.example.com/logo.png".to_string(),
            "assets/external/cdn/ab/12345678-logo.png".to_string(),
        );

        rewrite_file(&file_path, &refs, &url_map, &FxHashSet::default()).unwrap();

        let rewritten = std::fs::read_to_string(&file_path).unwrap();
        assert!(!rewritten.contains("https://cdn.example.com/logo.png"));
        assert!(rewritten.contains("assets/external/cdn/ab/12345678-logo.png"));
    }

    #[test]
    fn test_rewrite_srcset() {
        let tmpdir = tempfile::tempdir().unwrap();
        let file_path = tmpdir.path().join("test.html");
        let html = r#"<img srcset="https://a.com/s.jpg 400w, https://a.com/l.jpg 800w">"#;
        std::fs::write(&file_path, html).unwrap();

        let refs = crate::scanner::scan_file("test.html", html).references;
        assert_eq!(refs.len(), 2);
        let refs: Vec<&MediaReference> = refs.iter().collect();

        let mut url_map = FxHashMap::default();
        url_map.insert("https://a.com/s.jpg".to_string(), "local/s.jpg".to_string());
        url_map.insert("https://a.com/l.jpg".to_string(), "local/l.jpg".to_string());

        rewrite_file(&file_path, &refs, &url_map, &FxHashSet::default()).unwrap();

        let rewritten = std::fs::read_to_string(&file_path).unwrap();
        assert!(rewritten.contains("local/s.jpg 400w"));
        assert!(rewritten.contains("local/l.jpg 800w"));
        assert!(!rewritten.contains("https://a.com/"));
    }

    #[test]
    fn test_broken_url_renames_attribute() {
        let tmpdir = tempfile::tempdir().unwrap();
        let file_path = tmpdir.path().join("test.html");
        let html = r#"<a href="https://s3.example.com/missing.jpg"><img src="local.jpg"></a>"#;
        std::fs::write(&file_path, html).unwrap();

        let refs = crate::scanner::scan_file("test.html", html).references;
        assert_eq!(refs.len(), 1);
        let refs: Vec<&MediaReference> = refs.iter().collect();

        let url_map = FxHashMap::default(); // no downloads succeeded
        let mut broken = FxHashSet::default();
        broken.insert("https://s3.example.com/missing.jpg".to_string());

        rewrite_file(&file_path, &refs, &url_map, &broken).unwrap();

        let rewritten = std::fs::read_to_string(&file_path).unwrap();
        assert!(
            rewritten.contains("data-broken-href"),
            "expected data-broken-href in: {rewritten}"
        );
        assert!(
            rewritten.contains("https://s3.example.com/missing.jpg"),
            "original URL preserved"
        );
    }

    #[test]
    fn test_broken_url_on_img_src() {
        let tmpdir = tempfile::tempdir().unwrap();
        let file_path = tmpdir.path().join("test.html");
        let html = r#"<img src="https://cdn.example.com/gone.png" alt="x">"#;
        std::fs::write(&file_path, html).unwrap();

        let refs = crate::scanner::scan_file("test.html", html).references;
        assert_eq!(refs.len(), 1);
        let refs: Vec<&MediaReference> = refs.iter().collect();

        let url_map = FxHashMap::default();
        let mut broken = FxHashSet::default();
        broken.insert("https://cdn.example.com/gone.png".to_string());

        rewrite_file(&file_path, &refs, &url_map, &broken).unwrap();

        let rewritten = std::fs::read_to_string(&file_path).unwrap();
        assert!(
            rewritten.contains("data-broken-src"),
            "expected data-broken-src"
        );
        assert!(
            rewritten.contains("https://cdn.example.com/gone.png"),
            "original URL preserved"
        );
    }
}
