/// End-to-end test for extract-css → bundle-css pipeline.
///
/// Builds a mock site with inline <style> blocks and external <link>
/// references, then runs each command and verifies the output.
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static SITE_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn unique_temp_dir() -> String {
    let n = SITE_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        "{}/localize-e2e-{}-{}",
        std::env::temp_dir().display(),
        std::process::id(),
        n
    )
}

fn localize_binary() -> String {
    env!("CARGO_BIN_EXE_localize").to_string()
}

struct MockSite {
    root: String,
}

impl MockSite {
    fn new() -> Self {
        let dir = unique_temp_dir();
        let dir_path = Path::new(&dir);
        let _ = fs::remove_dir_all(dir_path);
        fs::create_dir_all(dir_path).unwrap();

        // index.html — has inline <style> blocks and <link> references
        fs::write(
            dir_path.join("index.html"),
            concat!(
                "<!DOCTYPE html>\n",
                "<html lang=\"en\">\n",
                "<head>\n",
                "<meta charset=\"utf-8\">\n",
                "<title>Home</title>\n",
                "<link rel=\"stylesheet\" href=\"_grab/theme.css\">\n",
                "<link rel=\"stylesheet\" href=\"_grab/components.css\">\n",
                "<style>.hero{color:red}</style>\n",
                "<style>.footer{padding:10px}</style>\n",
                "</head>\n",
                "<body>\n",
                "<h1>Home</h1>\n",
                "<link rel=\"stylesheet\" href=\"_grab/in-body.css\">\n",
                "</body>\n",
                "</html>\n",
            ),
        )
        .unwrap();

        // about/index.html — shares some <link> refs, has unique <style>
        let about_dir = dir_path.join("about");
        fs::create_dir_all(&about_dir).unwrap();
        fs::write(
            about_dir.join("index.html"),
            concat!(
                "<!DOCTYPE html>\n",
                "<html lang=\"en\">\n",
                "<head>\n",
                "<meta charset=\"utf-8\">\n",
                "<title>About</title>\n",
                "<link rel=\"stylesheet\" href=\"../_grab/theme.css\">\n",
                "<style>.about-bio{font-style:italic}</style>\n",
                "</head>\n",
                "<body>\n",
                "<h1>About</h1>\n",
                "</body>\n",
                "</html>\n",
            ),
        )
        .unwrap();

        // CSS files referenced by <link> tags
        let grab_dir = dir_path.join("_grab");
        fs::create_dir_all(&grab_dir).unwrap();
        fs::write(grab_dir.join("theme.css"), "body{font-family:sans-serif}\n").unwrap();
        fs::write(
            grab_dir.join("components.css"),
            ".btn{padding:8px}\n.card{border:1px solid #ccc}\n",
        )
        .unwrap();
        fs::write(grab_dir.join("in-body.css"), "/* in-body style */\n").unwrap();

        MockSite { root: dir }
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(Path::new(&self.root).join(rel)).unwrap()
    }

    fn exists(&self, rel: &str) -> bool {
        Path::new(&self.root).join(rel).exists()
    }

    fn run_extract_css(&self) -> String {
        let output = Command::new(localize_binary())
            .args(["extract-css", &self.root, "--apply"])
            .output()
            .unwrap();
        if !output.status.success() {
            panic!(
                "extract-css failed:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        String::from_utf8_lossy(&output.stderr).to_string()
    }

    fn run_bundle_css(&self) -> String {
        let output = Command::new(localize_binary())
            .args(["bundle-css", &self.root, "--apply"])
            .output()
            .unwrap();
        if !output.status.success() {
            panic!(
                "bundle-css failed:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        String::from_utf8_lossy(&output.stderr).to_string()
    }

    fn run_bundle_css_dry(&self) -> String {
        let output = Command::new(localize_binary())
            .args(["bundle-css", &self.root])
            .output()
            .unwrap();
        if !output.status.success() {
            panic!(
                "bundle-css dry-run failed:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        String::from_utf8_lossy(&output.stderr).to_string()
    }
}

impl Drop for MockSite {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn e2e_extract_css_removes_style_tags() {
    let site = MockSite::new();
    site.run_extract_css();

    let html = site.read("index.html");
    // <style> tags should be gone
    assert!(!html.contains("<style>"), "style tags should be removed, got:\n{html}");
    assert!(!html.contains("<style "), "style tags should be removed");
    // <link> tags should be present (for extracted CSS)
    assert!(html.contains("<link rel=\"stylesheet\""), "missing link tags:\n{html}");
    // Content-addressed CSS files should exist
    assert!(
        site.exists("localized-css"),
        "localized-css dir should exist"
    );
    // Original <link> tags should still be present (not touched by extract-css)
    assert!(html.contains("_grab/theme.css"), "original link refs preserved:\n{html}");
    assert!(html.contains("_grab/components.css"), "original link refs preserved:\n{html}");
}

#[test]
fn e2e_extract_css_empty_style_removed() {
    let site = MockSite::new();
    // Add an empty style tag
    let about_html = site.read("about/index.html");
    let modified = about_html.replace(
        "</head>",
        "<style class=\"wp-fonts-local\"></style>\n</head>",
    );
    fs::write(
        Path::new(&site.root).join("about/index.html"),
        modified,
    )
    .unwrap();

    site.run_extract_css();

    let html = site.read("about/index.html");
    assert!(!html.contains("wp-fonts-local"), "empty style removed:\n{html}");
}

#[test]
fn e2e_extract_css_content_addressed() {
    // Same CSS in two files should produce the same hash
    let site = MockSite::new();
    // Add identical style to about page
    let about_html = site.read("about/index.html");
    let modified = about_html.replace(
        "</head>",
        "<style>.hero{color:red}</style>\n</head>",
    );
    fs::write(
        Path::new(&site.root).join("about/index.html"),
        modified,
    )
    .unwrap();

    site.run_extract_css();

    let index_html = site.read("index.html");
    let about_html = site.read("about/index.html");

    // Both files should reference the same CSS file for .hero{color:red}
    let extract_href = |_html: &str, css_content: &str| -> String {
        let hash = format!("{:016x}", xxhash_rust::xxh3::xxh3_64(css_content.as_bytes()));
        let prefix = &hash[..2];
        format!("localized-css/{prefix}/{hash}.css")
    };
    let hero_href = extract_href(&index_html, ".hero{color:red}");
    assert!(index_html.contains(&hero_href), "index missing hero ref:\n{index_html}");
    assert!(about_html.contains(&hero_href), "about missing hero ref:\n{about_html}");
}

#[test]
fn e2e_bundle_css_creates_single_bundle() {
    let site = MockSite::new();
    site.run_extract_css();
    let stderr = site.run_bundle_css();

    let index_html = site.read("index.html");
    let about_html = site.read("about/index.html");

    // Both files should reference a single bundle
    let links_in: Vec<&str> = index_html
        .lines()
        .filter(|l| l.contains("<link rel=\"stylesheet\""))
        .collect();
    assert_eq!(
        links_in.len(),
        1,
        "index.html should have exactly 1 stylesheet link, got {}:\n{index_html}",
        links_in.len()
    );
    assert!(
        links_in[0].contains("bundle/"),
        "link should point to bundle, got: {}",
        links_in[0]
    );

    let links_about: Vec<&str> = about_html
        .lines()
        .filter(|l| l.contains("<link rel=\"stylesheet\""))
        .collect();
    assert_eq!(
        links_about.len(),
        1,
        "about/index.html should have exactly 1 stylesheet link, got {}:\n{about_html}",
        links_about.len()
    );
    assert!(
        links_about[0].contains("../bundle/"),
        "about link should point to bundle with relative path, got: {}",
        links_about[0]
    );

    // Bundle file should exist at the fixed path.
    assert!(site.exists("bundle/bundle.css"), "bundle CSS file should exist at bundle/bundle.css");

    let bundle_content = fs::read_to_string(
        Path::new(&site.root).join("bundle/bundle.css"),
    )
    .unwrap();
    assert!(bundle_content.contains(".hero{color:red}"), "missing hero style");
    assert!(bundle_content.contains(".footer{padding:10px}"), "missing footer style");
    assert!(bundle_content.contains(".about-bio{font-style:italic}"), "missing about style");
    assert!(
        bundle_content.contains("body{font-family:sans-serif}"),
        "missing theme style"
    );
    assert!(bundle_content.contains(".btn{padding:8px}"), "missing components style");

    // Verify stderr reports correct counts
    assert!(stderr.contains("bundle"), "stderr should mention bundle");
}

#[test]
fn e2e_bundle_css_preserves_media_specific_links() {
    let site = MockSite::new();
    // Add a media-specific link that should NOT be bundled
    let index_html = site.read("index.html");
    let modified = index_html.replace(
        "</head>",
        "<link media=\"only screen and (max-width: 768px)\" href=\"_grab/mobile.css\" rel=\"stylesheet\">\n</head>",
    );
    fs::write(Path::new(&site.root).join("index.html"), modified).unwrap();
    fs::write(
        Path::new(&site.root).join("_grab/mobile.css"),
        ".mobile-hide{display:none}\n",
    )
    .unwrap();

    site.run_extract_css();
    site.run_bundle_css();

    let html = site.read("index.html");
    // The media-specific link should still be there
    assert!(html.contains("max-width: 768px"), "media link preserved:\n{html}");
    assert!(html.contains("mobile.css"), "mobile.css preserved:\n{html}");
    // But there should also be a bundle link
    assert!(html.contains("bundle/"), "bundle link should exist:\n{html}");
    // Count stylesheet links: 1 bundle + 1 media
    let links: Vec<&str> = html
        .lines()
        .filter(|l| l.contains("rel=\"stylesheet\"") || l.contains("rel=stylesheet"))
        .collect();
    assert_eq!(links.len(), 2, "should have 2 links (bundle + media):\n{html}");
}

#[test]
fn e2e_bundle_css_dry_run_reports_without_writing() {
    let site = MockSite::new();
    site.run_extract_css();

    let stderr = site.run_bundle_css_dry();
    assert!(stderr.contains("Dry-run"), "should indicate dry-run:\n{stderr}");

    // HTML files should NOT be modified
    let index_html = site.read("index.html");
    assert!(
        index_html.contains("_grab/theme.css"),
        "original links preserved in dry-run:\n{index_html}"
    );
    // Bundle dir should NOT exist
    assert!(!site.exists("bundle"), "bundle dir should not exist after dry-run");
}

#[test]
fn e2e_full_pipeline_extract_then_bundle() {
    // Simulates the full workflow on a ZIM-like static site:
    // 1. extract-css to pull out inline styles
    // 2. bundle-css to merge everything into one file
    let site = MockSite::new();

    // Phase 1: extract
    site.run_extract_css();
    let index_html = site.read("index.html");
    assert!(!index_html.contains("<style>"), "no style tags after extract");
    assert!(!index_html.contains("<style "), "no style tags after extract");

    // Phase 2: bundle
    site.run_bundle_css();
    let index_html = site.read("index.html");
    let links: Vec<&str> = index_html
        .lines()
        .filter(|l| l.contains("<link rel=\"stylesheet\""))
        .collect();
    assert_eq!(links.len(), 1, "single bundle link after full pipeline");

    let about_html = site.read("about/index.html");
    let about_links: Vec<&str> = about_html
        .lines()
        .filter(|l| l.contains("<link rel=\"stylesheet\""))
        .collect();
    assert_eq!(about_links.len(), 1, "single bundle link in about page");

    // Verify the about page's relative path to bundle goes up one level
    assert!(
        about_html.contains("../bundle/"),
        "about page should reference ../bundle/:\n{about_html}"
    );
}

#[test]
fn e2e_bundle_css_fixed_path() {
    // The bundle is always at the fixed path bundle/bundle.css.
    let site = MockSite::new();
    site.run_extract_css();
    site.run_bundle_css();

    let index_html = site.read("index.html");
    assert!(
        index_html.contains("bundle/bundle.css"),
        "index should reference bundle/bundle.css:\n{index_html}"
    );

    let about_html = site.read("about/index.html");
    assert!(
        about_html.contains("../bundle/bundle.css"),
        "about should reference ../bundle/bundle.css:\n{about_html}"
    );

    assert!(site.exists("bundle/bundle.css"), "bundle file should exist");
}
