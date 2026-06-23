/// End-to-end tests for `localize check` with explicit FILES arguments.
///
/// Verifies that:
///  - Explicit files are scanned correctly.
///  - The href_set is built from ALL files on disk, not just the explicit
///    files, so links to existing files are not falsely reported as broken.
///  - A single file positional is auto-detected as a file (not a root dir).
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static SITE_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn unique_temp_dir() -> String {
    let n = SITE_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        "{}/localize-check-e2e-{}-{}",
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

        // index.html — references files that exist and files that don't.
        fs::write(
            dir_path.join("index.html"),
            concat!(
                "<!DOCTYPE html>\n",
                "<html lang=\"en\">\n",
                "<head>\n",
                "<meta charset=\"utf-8\">\n",
                "<title>Home</title>\n",
                "<link rel=\"stylesheet\" href=\"_grab/exists.css\">\n",
                "<link rel=\"stylesheet\" href=\"_grab/missing.css\">\n",
                "</head>\n",
                "<body>\n",
                "<h1>Home</h1>\n",
                "<a href=\"about/index.html\">About</a>\n",
                "<a href=\"missing-page.html\">Dead link</a>\n",
                "<img src=\"_grab/logo.png\" alt=\"logo\">\n",
                "<img src=\"_grab/not-found.png\" alt=\"missing image\">\n",
                "</body>\n",
                "</html>\n",
            ),
        )
        .unwrap();

        // about/index.html — another HTML file, exists on disk.
        let about_dir = dir_path.join("about");
        fs::create_dir_all(&about_dir).unwrap();
        fs::write(
            about_dir.join("index.html"),
            concat!(
                "<!DOCTYPE html>\n",
                "<html lang=\"en\">\n",
                "<head><meta charset=\"utf-8\"><title>About</title></head>\n",
                "<body><h1>About</h1></body>\n",
                "</html>\n",
            ),
        )
        .unwrap();

        // CSS and image assets that exist on disk.
        let grab_dir = dir_path.join("_grab");
        fs::create_dir_all(&grab_dir).unwrap();
        fs::write(grab_dir.join("exists.css"), "body{margin:0}\n").unwrap();
        fs::write(grab_dir.join("logo.png"), "fake-png-content").unwrap();

        MockSite { root: dir }
    }

    fn run_check(&self, args: &[&str]) -> (bool, String, String) {
        let mut full_args: Vec<&str> = vec!["check"];
        full_args.extend(args);
        let output = Command::new(localize_binary())
            .args(&full_args)
            .current_dir(&self.root)
            .output()
            .unwrap();
        (
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        )
    }
}

impl Drop for MockSite {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Collect "broken-local-url:" lines from combined output.
fn broken_urls_from_output(stderr: &str, stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .chain(stderr.lines())
        .filter_map(|line| {
            line.strip_prefix("broken-local-url: ")
                .map(|u| u.to_string())
        })
        .collect()
}

/// Parse the "Dry-run:" line from stderr, returning
/// (broken_count, remote_count, file_count).
fn parse_dry_run(stderr: &str) -> (usize, usize, usize) {
    for line in stderr.lines() {
        if line.starts_with("Dry-run:") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            let broken: usize = parts[1].parse().unwrap();
            let remote: usize = parts[3].parse().unwrap();
            let files: usize = parts[6].parse().unwrap();
            return (broken, remote, files);
        }
    }
    panic!("Dry-run line not found in stderr:\n{stderr}");
}

// ── regression: explicit file has correct href_set ──────────────

/// When an explicit file is passed, the href_set must be built from ALL files on
/// disk. Links to files that exist should NOT be reported as broken.
#[test]
fn explicit_file_does_not_false_positive_on_existing_links() {
    let site = MockSite::new();

    let (ok, stdout, stderr) = site.run_check(&["index.html"]);
    assert!(ok, "check failed: {stderr}");

    let broken_urls = broken_urls_from_output(&stderr, &stdout);

    // These files exist on disk — must NOT appear as broken.
    let must_not_be_broken = &[
        "_grab/exists.css",
        "_grab/logo.png",
    ];
    for url in must_not_be_broken {
        assert!(
            !broken_urls.iter().any(|b| b.ends_with(url)),
            "existing file '{url}' was falsely reported as broken.\nbroken URLs: {broken_urls:?}"
        );
    }

    // These files do NOT exist — MUST appear as broken.
    let must_be_broken = &["_grab/missing.css", "_grab/not-found.png"];
    for url in must_be_broken {
        assert!(
            broken_urls.iter().any(|b| b.ends_with(url)),
            "missing file '{url}' was NOT reported as broken.\nbroken URLs: {broken_urls:?}"
        );
    }

    // Should scan exactly 1 file.
    let (_broken, _remote, file_count) = parse_dry_run(&stderr);
    assert_eq!(file_count, 1, "should scan exactly 1 file");
}

/// When running check with discovery mode (no explicit files), the behavior
/// should be identical: existing files not broken, missing files broken.
#[test]
fn discovery_mode_same_results_as_explicit() {
    let site = MockSite::new();

    let (ok, stdout, stderr) = site.run_check(&["."]);
    assert!(ok, "check failed: {stderr}");

    let broken_urls = broken_urls_from_output(&stderr, &stdout);

    // These exist — must not be broken.
    let must_not_be_broken = &[
        "_grab/exists.css",
        "_grab/logo.png",
    ];
    for url in must_not_be_broken {
        assert!(
            !broken_urls.iter().any(|b| b.ends_with(url)),
            "discovery mode: existing file '{url}' was falsely reported as broken.\nbroken URLs: {broken_urls:?}"
        );
    }

    // These are missing — must be broken.
    let must_be_broken = &["_grab/missing.css", "_grab/not-found.png"];
    for url in must_be_broken {
        assert!(
            broken_urls.iter().any(|b| b.ends_with(url)),
            "discovery mode: missing file '{url}' was NOT reported as broken.\nbroken URLs: {broken_urls:?}"
        );
    }
}

/// Regression: the explicit-file results must match discovery-mode results
/// for the same file.
#[test]
fn explicit_and_discovery_produce_same_broken_urls_for_same_file() {
    let site = MockSite::new();

    // Run with explicit file.
    let (ok1, stdout1, stderr1) = site.run_check(&["index.html"]);
    assert!(ok1, "explicit check failed: {stderr1}");
    let mut broken1 = broken_urls_from_output(&stderr1, &stdout1);
    broken1.sort();

    // Run with discovery.
    let (ok2, stdout2, stderr2) = site.run_check(&["."]);
    assert!(ok2, "discovery check failed: {stderr2}");
    let mut broken2 = broken_urls_from_output(&stderr2, &stdout2);
    broken2.sort();

    assert_eq!(
        broken1, broken2,
        "explicit and discovery mode should report the same broken URLs"
    );
}

/// Multiple explicit files should all be scanned.
#[test]
fn multiple_explicit_files() {
    let site = MockSite::new();

    let (ok, _stdout, stderr) = site.run_check(&["index.html", "about/index.html"]);
    assert!(ok, "check failed: {stderr}");

    let (_broken, _remote, file_count) = parse_dry_run(&stderr);
    assert_eq!(file_count, 2, "should scan exactly 2 files");
}

/// Running check with no arguments (current directory discovery).
#[test]
fn no_args_discovers_all() {
    let site = MockSite::new();

    let (ok, _stdout, stderr) = site.run_check(&[]);
    assert!(ok, "check failed: {stderr}");

    let (_broken, _remote, file_count) = parse_dry_run(&stderr);
    assert_eq!(file_count, 2, "should discover and scan 2 HTML files");
}
