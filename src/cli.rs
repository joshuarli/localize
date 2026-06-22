use crate::downloader::{DownloadConfig, asset_path, download_and_rewrite};
use crate::scanner::{MediaReference, is_remote_url, scan_file};
use crate::webp_encode::ConvertResult;
use lexopt::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Args {
    command: Option<String>,
    root: Option<String>,
    include: Vec<String>,
    exclude: Vec<String>,
    assets_dir: String,
    json: bool,
    verbose: bool,
    jobs: usize,
    // check
    download: bool,
    clean: bool,
    timeout: u32,
    retries: u32,
    force: bool,
    user_agent: String,
    referer: String,
    // zap
    zap_tag: Option<String>,
    zap_query: Option<String>,
    // translate
    from_lang: Option<String>,
    to_lang: String,
    apply: bool,
    quiet: bool,
    // extract-css
    dir: String,
    // bundle-css
    bundle_dir: String,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            command: None,
            root: None,
            include: Vec::new(),
            exclude: Vec::new(),
            assets_dir: "assets/external".into(),
            json: false,
            verbose: false,
            jobs: 0,
            download: false,
            clean: false,
            timeout: 30,
            retries: 3,
            force: false,
            user_agent: String::new(),
            referer: String::new(),
            zap_tag: None,
            zap_query: None,
            from_lang: None,
            to_lang: "en".into(),
            apply: false,
            quiet: false,
            dir: "localized-css".into(),
            bundle_dir: "bundle".into(),
        }
    }
}

fn print_help() {
    println!(
        "\
localize — maintenance toolkit for static HTML sites.

Usage: localize <command> [ROOT] [flags]

Commands:
  check         Find remote media URLs and broken local links in HTML files.
                With --download, fetch remote assets and rewrite HTML to use
                local paths.  With --clean, fix broken local links by
                unwrapping dead <a> tags and removing dead resource elements.
  bundle-css    Bundle all <link rel=\"stylesheet\"> CSS files across the site
                into a single monolithic content-addressed .css file, then
                rewrite every HTML file to reference the single bundle.
                Dry-run by default, --apply to write.
  extract-css   Extract inline <style> CSS blocks into separate .css files
                and replace them with <link rel=\"stylesheet\"> tags in <head>.
                Dry-run by default, --apply to write.
  minify-html   Minify HTML files: strip whitespace, remove comments,
                collapse redundant attributes, omit optional tags. Dry-run
                by default, --apply to write.
  zap           Remove HTML elements matching a CSS selector whose inner text
                contains a query string. Dry-run by default, --apply to remove.
  towebp        Replace .jpg/.jpeg/.png URL extensions with .webp in href,
                src, and srcset attributes. Dry-run by default, --apply to
                rewrite.
  translate     Translate HTML text content to a target language via Apple's
                on-device Translation framework. Extracts text, clusters by
                element type (article body, headings, nav, sidebar), and
                reconstructs the HTML. Dry-run by default, --apply to write.

Common flags:
  --include <pattern>   Only process files matching glob pattern (repeatable).
  --exclude <pattern>   Skip files matching glob pattern (repeatable).
  --assets-dir <dir>    Asset directory [default: assets/external].
  --json                Output as JSON.
  --verbose             Verbose progress output.
  --quiet, -q           Suppress per-file output (minify-html only).
  --jobs <n>            Max parallel workers [default: CPUs × 4].
  --help, -h            Print this help and exit.

Check flags:
  --download            Download assets and rewrite HTML (default is scan only).
  --clean               Fix broken local links (default is scan only).
  --timeout <s>         Download timeout in seconds [default: 30].
  --retries <n>         Download retry count [default: 3].
  --force               Re-download even if asset already exists.
  --user-agent <str>    Custom User-Agent header.
  --referer <str>       Custom Referer header.

Zap flags:
  --apply               Apply removals (dry-run by default).

Towebp flags:
  --apply               Apply rewrites (dry-run by default).

Minify-html flags:
  --apply               Apply minification (dry-run by default).
  --quiet, -q           Suppress per-file savings output.

Bundle-css flags:
  --apply               Apply bundling (dry-run by default).
  --bundle-dir <dir>    Output directory for the bundle [default: bundle].

Extract-css flags:
  --apply               Apply extraction (dry-run by default).
  -d, --dir <dir>       Output directory for CSS files [default: localized-css].

Translate flags:
  --from <lang>         Source language (BCP-47, e.g. zh-Hans). Auto-detect
                        per file if omitted.
  --to <lang>           Target language (BCP-47, default: en).
  --apply               Apply translations (dry-run by default).

Examples:
  localize check ~/mysite
  localize check ~/mysite --download
  localize check ~/mysite --clean
  localize zap p \"Copyright 2019\" ~/mysite --apply
  localize towebp ~/mysite --apply
  localize minify-html ~/mysite --apply
  localize bundle-css ~/mysite --apply
  localize extract-css ~/mysite --apply
  localize translate ~/mysite --to en --apply"
    );
}

fn parse_args() -> Result<Args, lexopt::Error> {
    // Handle --help/-h anywhere, before lexopt rejects it.
    if std::env::args().any(|a| a == "--help" || a == "-h") {
        print_help();
        std::process::exit(0);
    }

    let mut args = Args::default();
    let mut parser = lexopt::Parser::from_env();

    // First positional arg is the subcommand.
    if let Some(arg) = parser.next()? {
        match arg {
            Value(val) => {
                args.command = Some(val.string()?);
            }
            _ => {
                return Err(
                    "expected subcommand (check, minify-html, zap, towebp, or translate)".into(),
                );
            }
        }
    }

    // Remaining positionals depend on the subcommand.
    match args.command.as_deref() {
        Some("zap") => {
            // Selector (required)
            match parser.next()? {
                Some(Value(val)) => {
                    args.zap_tag = Some(val.string()?);
                }
                _ => {
                    return Err("expected selector (e.g., p, .class, #id, [attr])".into());
                }
            }
            // Query (required)
            match parser.next()? {
                Some(Value(val)) => {
                    args.zap_query = Some(val.string()?);
                }
                _ => {
                    return Err("expected query string".into());
                }
            }
            // Root is optional — caught as a Value in the flag loop, or defaults to ".".
        }
        Some("translate") => {
            // Root is optional — caught as a Value in the flag loop, or defaults to ".".
        }
        _ => {
            // Root is optional — caught as a Value in the flag loop below, or defaults to ".".
        }
    }

    // Remaining flags.
    while let Some(arg) = parser.next()? {
        match arg {
            Long("include") => {
                args.include.push(parser.value()?.string()?);
            }
            Long("exclude") => {
                args.exclude.push(parser.value()?.string()?);
            }
            Long("assets-dir") => {
                args.assets_dir = parser.value()?.string()?;
            }
            Long("json") => {
                args.json = true;
            }
            Long("verbose") => {
                args.verbose = true;
            }
            Long("jobs") => {
                args.jobs = parser.value()?.parse()?;
            }
            Long("timeout") => {
                args.timeout = parser.value()?.parse()?;
            }
            Long("retries") => {
                args.retries = parser.value()?.parse()?;
            }
            Long("force") => {
                args.force = true;
            }
            Long("user-agent") => {
                args.user_agent = parser.value()?.string()?;
            }
            Long("referer") => {
                args.referer = parser.value()?.string()?;
            }
            Long("download") => {
                args.download = true;
            }
            Long("clean") => {
                args.clean = true;
            }
            Long("apply") => {
                args.apply = true;
            }
            Long("quiet") | Short('q') => {
                args.quiet = true;
            }
            Short('d') | Long("dir") => {
                args.dir = parser.value()?.string()?;
            }
            Long("bundle-dir") => {
                args.bundle_dir = parser.value()?.string()?;
            }
            Long("from") => {
                args.from_lang = Some(parser.value()?.string()?);
            }
            Long("to") => {
                args.to_lang = parser.value()?.string()?;
            }
            Long("help") | Short('h') => {
                print_help();
                std::process::exit(0);
            }
            // A bare value after known positionals is the root directory.
            Value(val) if args.root.is_none() => {
                args.root = Some(val.string()?);
            }
            Long(unknown) => {
                return Err(format!("unknown flag --{unknown}").into());
            }
            Short(_) | Value(_) => {
                return Err("unexpected argument".into());
            }
        }
    }

    // Default root for all commands.
    if args.root.is_none() {
        args.root = Some(".".into());
    }

    Ok(args)
}

fn iter_html_files(root: &str, include: &[String], exclude: &[String]) -> Vec<String> {
    let mut matches = Vec::new();
    let include_pats: Vec<glob::Pattern> = include
        .iter()
        .filter_map(|p| glob::Pattern::new(p).ok())
        .collect();
    let exclude_pats: Vec<glob::Pattern> = exclude
        .iter()
        .filter_map(|p| glob::Pattern::new(p).ok())
        .collect();

    for entry in jwalk::WalkDir::new(root).skip_hidden(false).into_iter() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let full = entry.path();
        let rel = full.strip_prefix(root).unwrap_or(&full);
        let rel_str = rel.to_string_lossy();

        if exclude_pats.iter().any(|p| p.matches(&rel_str)) {
            continue;
        }
        if include_pats.iter().any(|p| p.matches(&rel_str)) {
            matches.push(rel_str.to_string());
        }
    }

    matches.sort();
    matches
}

/// Walk the tree once, collecting HTML file paths and building the canonical
/// href set for existence checks. Returns (html_files, href_set).
fn discover_and_index(
    root: &str,
    include: &[String],
    exclude: &[String],
) -> (Vec<String>, FxHashSet<String>) {
    let mut html_files = Vec::with_capacity(4096);
    let mut href_set = FxHashSet::with_capacity_and_hasher(16384, Default::default());

    let include_pats: Vec<glob::Pattern> = include
        .iter()
        .filter_map(|p| glob::Pattern::new(p).ok())
        .collect();
    let exclude_pats: Vec<glob::Pattern> = exclude
        .iter()
        .filter_map(|p| glob::Pattern::new(p).ok())
        .collect();

    for entry in jwalk::WalkDir::new(root).skip_hidden(false).into_iter() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let full = entry.path();
        let rel = full.strip_prefix(root).unwrap_or(&full);
        let rel_lossy = rel.to_string_lossy();
        let rel_str: &str = &rel_lossy;

        if exclude_pats.iter().any(|p| p.matches(rel_str)) {
            continue;
        }

        // Build canonical href for every file.
        let href = if rel_str.ends_with("/index.html") || rel_str.ends_with("/index.htm") {
            match rel_str.rfind('/') {
                Some(pos) => &rel_str[..pos],
                None => "",
            }
        } else if rel_str == "index.html" || rel_str == "index.htm" {
            ""
        } else {
            rel_str
        };
        href_set.insert(href.to_string());

        // Collect HTML files matching include patterns.
        if include_pats.iter().any(|p| p.matches(rel_str)) {
            html_files.push(rel_str.to_string());
        }
    }

    html_files.sort();
    (html_files, href_set)
}

fn scan_all(
    root: &str,
    files: &[String],
    jobs: usize,
    verbose: bool,
    href_set: &FxHashSet<String>,
) -> Vec<MediaReference> {
    if files.is_empty() {
        return Vec::new();
    }

    let total = files.len();
    let workers = jobs.min(files.len());
    let all_refs = std::sync::Mutex::new(Vec::with_capacity(files.len() * 4));
    let errors: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let counter = AtomicUsize::new(0);

    let root_path = Path::new(root);
    let href_set = Arc::new(href_set.clone());

    let _ = crossbeam::thread::scope(|s| {
        let files: &[String] = files;
        let all_refs: &std::sync::Mutex<Vec<MediaReference>> = &all_refs;
        let errors: &std::sync::Mutex<Vec<String>> = &errors;
        let counter: &AtomicUsize = &counter;
        let index = Arc::new(AtomicUsize::new(0));
        for _ in 0..workers {
            let index = Arc::clone(&index);
            let href_set = href_set.clone();
            s.spawn(move |_| {
                loop {
                    let i = index.fetch_add(1, Ordering::Relaxed);
                    if i >= total {
                        break;
                    }
                    let rel = &files[i];
                    let path = root_path.join(rel);
                    let content = std::fs::read_to_string(&path).unwrap_or_default();
                    let result = scan_file(rel, &content, &href_set);
                    let done = counter.fetch_add(1, Ordering::Relaxed) + 1;
                    if !verbose && done.is_multiple_of(16) {
                        eprint!("\rScanning: {done}/{total} files");
                        let _ = std::io::stderr().flush();
                    }
                    if let Some(err) = &result.error
                        && let Ok(mut errs) = errors.lock() {
                            errs.push(format!("{rel}: {err}"));
                        }
                    if verbose && !result.references.is_empty() {
                        eprintln!("  {rel}: {} reference(s)", result.references.len());
                    }
                    if let Ok(mut refs) = all_refs.lock() {
                        refs.extend(result.references);
                    }
                }
            });
        }
    });

    for err in errors.into_inner().unwrap() {
        eprintln!("\nWARNING: {err}");
    }

    let all_refs = all_refs.into_inner().unwrap();

    if !verbose && total > 0 {
        eprintln!();
    }

    all_refs
}

fn print_human(refs: &[MediaReference]) {
    for r in refs {
        if !is_remote_url(&r.url) && !r.broken {
            continue;
        }
        let kind = if r.broken {
            "broken-local-url"
        } else {
            "remote-url"
        };
        if let Some(ref desc) = r.descriptor {
            println!(
                "{kind}: ./{}:{}:{}  {}  {desc}",
                r.file_path, r.line, r.col, r.url
            );
        } else {
            println!("{kind}: ./{}:{}:{}  {}", r.file_path, r.line, r.col, r.url);
        }
    }
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_ascii_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn print_json(refs: &[MediaReference]) {
    print!("[");
    for (i, r) in refs.iter().enumerate() {
        if i > 0 {
            print!(",");
        }
        let kind = if r.broken {
            "broken-local-url"
        } else if is_remote_url(&r.url) {
            "remote-url"
        } else {
            "local-url"
        };
        print!(
            "\n  {{\"file\":{},\"url\":{},\"kind\":{},\"line\":{},\"col\":{},\"tag\":{},\"attr\":{}",
            json_escape(&r.file_path),
            json_escape(&r.url),
            json_escape(kind),
            r.line,
            r.col,
            json_escape(&r.tag.to_string()),
            json_escape(&r.attr.to_string()),
        );
        if let Some(d) = &r.descriptor {
            print!(",\"descriptor\":{}", json_escape(&d.to_string()));
        } else {
            print!(",\"descriptor\":null");
        }
        print!("}}");
    }
    if !refs.is_empty() {
        println!();
    }
    println!("]");
}

/// Resolve a relative URL against a file's parent directory, normalizing `..` and `.`.
fn resolve_relative(file_path: &str, url: &str) -> String {
    let dir = std::path::Path::new(file_path)
        .parent()
        .unwrap_or(std::path::Path::new(""))
        .to_string_lossy()
        .replace('\\', "/");
    let combined = if dir.is_empty() {
        url.to_string()
    } else {
        format!("{dir}/{url}")
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in combined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    parts.join("/")
}

/// Deduplicate broken-local-url entries by resolved path, keeping the first occurrence.
fn dedup_broken(refs: &[MediaReference]) -> Vec<MediaReference> {
    let mut seen = FxHashSet::default();
    let mut out = Vec::with_capacity(refs.len());
    for r in refs {
        if r.broken {
            let resolved = resolve_relative(&r.file_path, &r.url);
            if seen.insert(resolved) {
                out.push(r.clone());
            }
        } else {
            out.push(r.clone());
        }
    }
    out
}

fn cmd_check(args: Args) -> Result<(), String> {
    let root = args.root.as_ref().ok_or("missing root")?;
    let default_include = vec!["*.html".to_string(), "*.htm".to_string()];
    let include: &[String] = if args.include.is_empty() {
        &default_include
    } else {
        &args.include
    };
    let jobs = if args.jobs == 0 {
        num_cpus() * 4
    } else {
        args.jobs
    };

    eprintln!("Discovering files in {root}...");
    let (files, href_set) = discover_and_index(root, include, &args.exclude);

    if args.verbose {
        eprintln!(
            "Found {} HTML file(s), {} total file(s)",
            files.len(),
            href_set.len()
        );
    }
    if files.is_empty() {
        eprintln!("No HTML files found.");
        return Ok(());
    }

    eprintln!("Scanning {} file(s) with {jobs} workers...", files.len());
    let refs = scan_all(root, &files, jobs, args.verbose, &href_set);
    let deduped = dedup_broken(&refs);

    if args.download {
        // Filter to remote URLs (local broken URLs can't be downloaded).
        let remote_refs: Vec<&MediaReference> = refs.iter().filter(|r| !r.broken).collect();
        if remote_refs.is_empty() {
            if args.verbose {
                eprintln!("No remote references found.");
            }
        } else {
            let unique_urls: Vec<String> = {
                let mut seen = FxHashSet::default();
                let mut urls = Vec::new();
                for r in &remote_refs {
                    if seen.insert(&r.url) {
                        urls.push(r.url.to_string());
                    }
                }
                urls
            };

            let new_urls: Vec<String> = if args.force {
                unique_urls.clone()
            } else {
                unique_urls
                    .iter()
                    .filter(|u| {
                        let rel = asset_path(u, &args.assets_dir);
                        !Path::new(root).join(&rel).is_file()
                    })
                    .cloned()
                    .collect()
            };

            let file_urls: FxHashMap<String, FxHashSet<String>> = {
                let mut map: FxHashMap<String, FxHashSet<String>> = FxHashMap::default();
                for r in &remote_refs {
                    map.entry(r.file_path.to_string())
                        .or_default()
                        .insert(r.url.to_string());
                }
                map
            };
            let total_files = file_urls.len();
            eprintln!(
                "Downloading {} asset(s) across {} file(s) ({} already present)...",
                new_urls.len(),
                total_files,
                unique_urls.len() - new_urls.len()
            );

            let dl_jobs = if args.jobs == 0 { 8 } else { args.jobs };
            let dl_cfg = DownloadConfig {
                root: Path::new(root),
                assets_dir: &args.assets_dir,
                timeout: args.timeout,
                retries: args.retries,
                user_agent: &args.user_agent,
                referer: &args.referer,
                force: args.force,
                verbose: args.verbose,
                jobs: dl_jobs,
            };
            let (rewritten, broken_urls) = download_and_rewrite(&file_urls, &dl_cfg);

            if !broken_urls.is_empty() {
                eprintln!(
                    "{} URL(s) returned 404 — attributes renamed to data-broken-*:",
                    broken_urls.len()
                );
                for u in broken_urls.iter().take(10) {
                    eprintln!("  {u}");
                }
                if broken_urls.len() > 10 {
                    eprintln!("  ... and {} more", broken_urls.len() - 10);
                }
            }

            let skipped: Vec<&String> = file_urls
                .keys()
                .filter(|f| !rewritten.contains(*f))
                .collect();
            if !skipped.is_empty() {
                eprintln!(
                    "Skipped {} file(s) with transient failures (re-run to retry):",
                    skipped.len()
                );
                for f in &skipped {
                    eprintln!("  {f}");
                }
            }

            let rewritten_files: Vec<String> = rewritten.iter().cloned().collect();
            let stray = verify_no_remote(&rewritten_files, root);
            if !stray.is_empty() {
                eprintln!(
                    "WARNING: {} file(s) still contain remote URLs:",
                    stray.len()
                );
                for s in &stray {
                    eprintln!("  {s}");
                }
            }

            eprintln!(
                "Done. {} unique URL(s), {} file(s) rewritten, {} skipped.",
                unique_urls.len(),
                rewritten.len(),
                skipped.len()
            );
        }
    }

    if args.clean {
        let broken_refs: Vec<&MediaReference> = refs.iter().filter(|r| r.broken).collect();
        if broken_refs.is_empty() {
            eprintln!("No broken local links to clean.");
        } else {
            let mut file_broken: FxHashMap<String, Vec<&MediaReference>> = FxHashMap::default();
            for r in &broken_refs {
                file_broken
                    .entry(r.file_path.to_string())
                    .or_default()
                    .push(r);
            }
            eprintln!(
                "Cleaning {} broken link(s) in {} file(s)...",
                broken_refs.len(),
                file_broken.len()
            );

            let cleaned = Arc::new(AtomicUsize::new(0));
            let errors: Arc<std::sync::Mutex<Vec<String>>> =
                Arc::new(std::sync::Mutex::new(Vec::new()));
            let clean_files: Vec<String> = file_broken.keys().cloned().collect();
            let clean_total = clean_files.len();
            let _ = crossbeam::thread::scope(|s| {
                let clean_files: &[String] = &clean_files;
                let href_set: &FxHashSet<String> = &href_set;
                let index = Arc::new(AtomicUsize::new(0));
                for _ in 0..jobs.min(clean_total) {
                    let index = Arc::clone(&index);
                    let cleaned = cleaned.clone();
                    let errors = errors.clone();
                    s.spawn(move |_| {
                        loop {
                            let i = index.fetch_add(1, Ordering::Relaxed);
                            if i >= clean_total {
                                break;
                            }
                            let rel = &clean_files[i];
                            let path = Path::new(root).join(rel);
                            let content = std::fs::read_to_string(&path).unwrap_or_default();
                            match crate::rewriter::clean_html(&content, href_set, rel) {
                                Ok(new_html) => {
                                    let tmp = path.with_extension("tmp");
                                    if let Err(e) = std::fs::write(&tmp, &new_html)
                                        .map_err(|e| format!("write tmp: {e}"))
                                        .and_then(|_| {
                                            std::fs::rename(&tmp, &path)
                                                .map_err(|e| format!("rename: {e}"))
                                        })
                                    {
                                        errors.lock().unwrap().push(format!("{rel}: {e}"));
                                    } else {
                                        cleaned.fetch_add(1, Ordering::Relaxed);
                                    }
                                }
                                Err(e) => {
                                    errors.lock().unwrap().push(format!("{rel}: {e}"));
                                }
                            }
                        }
                    });
                }
            });

            let errors = errors.lock().unwrap();
            if !errors.is_empty() {
                eprintln!("Errors:");
                for e in errors.iter() {
                    eprintln!("  {e}");
                }
            }

            eprintln!(
                "Cleaned {} broken link(s) in {} file(s).",
                broken_refs.len(),
                cleaned.load(Ordering::Relaxed)
            );
        }
    }

    if !args.download && !args.clean {
        if args.json {
            print_json(&deduped);
        } else {
            print_human(&deduped);
        }
        let broken = deduped.iter().filter(|r| r.broken).count();
        let remote = deduped.len() - broken;
        eprintln!(
            "Dry-run: {} broken-local-url, {} remote-url in {} file(s).",
            broken,
            remote,
            files.len()
        );
    }

    if args.verbose {
        let total_refs: &[MediaReference] = if args.download || args.clean {
            &refs
        } else {
            &deduped
        };
        let unique_broken = total_refs.iter().filter(|r| r.broken).count();
        let remote = total_refs.len() - unique_broken;
        eprintln!(
            "\nTotal: {} reference(s) in {} file(s) ({} unique local broken, {} remote)",
            total_refs.len(),
            files.len(),
            unique_broken,
            remote,
        );
    }

    Ok(())
}

fn verify_no_remote(files: &[String], root: &str) -> Vec<String> {
    let mut stray = Vec::new();
    for rel in files {
        let abs = Path::new(root).join(rel);
        let content = match std::fs::read_to_string(&abs) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let empty_set = FxHashSet::default();
        let result = scan_file(rel, &content, &empty_set);
        if !result.references.is_empty() {
            stray.push(rel.clone());
        }
    }
    stray
}

fn cmd_minify_html(args: Args) -> Result<(), String> {
    let root = args.root.as_deref().unwrap_or(".");
    let apply = args.apply;
    let quiet = args.quiet;
    let verbose = args.verbose;

    let default_include = vec!["*.html".to_string(), "*.htm".to_string()];
    let include: &[String] = if args.include.is_empty() {
        &default_include
    } else {
        &args.include
    };
    let jobs = if args.jobs == 0 {
        num_cpus() * 4
    } else {
        args.jobs
    };

    eprintln!("Discovering HTML files in {root}...");
    let files = iter_html_files(root, include, &args.exclude);

    if verbose {
        eprintln!("Found {} HTML file(s) to minify", files.len());
    }
    if files.is_empty() {
        eprintln!("No HTML files found.");
        return Ok(());
    }

    if apply {
        eprintln!("Minifying {} file(s) with {jobs} workers...", files.len());
    } else {
        eprintln!(
            "Dry-run: scanning {} file(s) with {jobs} workers...",
            files.len()
        );
    }

    let root_path = std::path::Path::new(root);
    let total_saved = AtomicUsize::new(0);
    let total_original = AtomicUsize::new(0);
    let file_count = files.len();
    let workers = jobs.min(files.len());

    let index = Arc::new(AtomicUsize::new(0));

    let _ = crossbeam::thread::scope(|s| {
        let files: &[String] = &files;
        let total_saved: &AtomicUsize = &total_saved;
        let total_original: &AtomicUsize = &total_original;
        for _ in 0..workers {
            let index = Arc::clone(&index);
            s.spawn(move |_| {
                loop {
                    let i = index.fetch_add(1, Ordering::Relaxed);
                    if i >= file_count {
                        break;
                    }
                    let rel = &files[i];
                    let path = root_path.join(rel);
                    let original = match std::fs::read(&path) {
                        Ok(c) => c,
                        Err(e) => {
                            eprintln!("{rel}: read error: {e}");
                            continue;
                        }
                    };
                    if original.is_empty() {
                        continue;
                    }
                    let original_len = original.len();
                    let cfg = minify_html::Cfg::new();
                    let minified = minify_html::minify(&original, &cfg);
                    let saved = original_len.saturating_sub(minified.len());
                    total_original.fetch_add(original_len, Ordering::Relaxed);
                    total_saved.fetch_add(saved, Ordering::Relaxed);
                    if apply && minified != original {
                        let tmp = path.with_extension("tmp");
                        if let Err(e) = std::fs::write(&tmp, &minified)
                            .map_err(|e| format!("write tmp: {e}"))
                            .and_then(|_| {
                                std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))
                            })
                        {
                            eprintln!("{rel}: {e}");
                        }
                    }
                    if !quiet && saved > 0 {
                        let pct = (saved as f64 / original_len as f64) * 100.0;
                        eprintln!("{rel}: -{pct:.1}% ({saved} bytes)");
                    }
                }
            });
        }
    });

    let total_saved = total_saved.load(Ordering::Relaxed);
    let total_original = total_original.load(Ordering::Relaxed);
    let total_pct = if total_original > 0 {
        (total_saved as f64 / total_original as f64) * 100.0
    } else {
        0.0
    };

    if apply {
        eprintln!(
            "Minified {} file(s), saved {} bytes ({total_pct:.1}%).",
            file_count, total_saved
        );
    } else {
        eprintln!(
            "Dry-run: {} file(s) would save {} bytes ({total_pct:.1}%). Run with --apply to minify.",
            file_count, total_saved
        );
    }

    Ok(())
}

fn cmd_bundle_css(args: Args) -> Result<(), String> {
    let root = args.root.as_deref().unwrap_or(".");
    let apply = args.apply;
    let verbose = args.verbose;
    let bundle_dir = &args.bundle_dir;

    let default_include = vec!["*.html".to_string(), "*.htm".to_string()];
    let include: &[String] = if args.include.is_empty() {
        &default_include
    } else {
        &args.include
    };
    let jobs = if args.jobs == 0 {
        num_cpus() * 4
    } else {
        args.jobs
    };

    eprintln!("Discovering HTML files in {root}...");
    let files = collect_html_depth_first(root, include, &args.exclude);

    if verbose {
        eprintln!("Found {} HTML file(s) to process", files.len());
    }
    if files.is_empty() {
        eprintln!("No HTML files found.");
        return Ok(());
    }

    // Phase 1: scan all HTML files for <link rel="stylesheet"> tags, collect
    // unique CSS paths and per-file link spans.
    eprintln!(
        "Scanning {} file(s) for stylesheet links with {jobs} workers...",
        files.len()
    );

    let root_path = std::path::Path::new(root);
    let file_count = files.len();
    let workers = jobs.min(files.len());

    let unique_css: std::sync::Mutex<BTreeSet<String>> = std::sync::Mutex::new(BTreeSet::new());
    // Ordered resolved paths from index.html (canonical cascade order).
    let index_order: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let per_file: std::sync::Mutex<Vec<(String, Vec<crate::bundle_css::CssLink>)>> =
        std::sync::Mutex::new(Vec::new());
    let done_counter = AtomicUsize::new(0);

    let _ = crossbeam::thread::scope(|s| {
        let files: &[String] = &files;
        let unique_css: &std::sync::Mutex<BTreeSet<String>> = &unique_css;
        let index_order: &std::sync::Mutex<Vec<String>> = &index_order;
        let per_file: &std::sync::Mutex<Vec<(String, Vec<crate::bundle_css::CssLink>)>> = &per_file;
        let done_counter: &AtomicUsize = &done_counter;
        let index = Arc::new(AtomicUsize::new(0));
        for _ in 0..workers {
            let index = Arc::clone(&index);
            s.spawn(move |_| {
                loop {
                    let i = index.fetch_add(1, Ordering::Relaxed);
                    if i >= file_count {
                        break;
                    }
                    let rel = &files[i];
                    let path = root_path.join(rel);
                    let content = std::fs::read_to_string(&path).unwrap_or_default();

                    let links = crate::bundle_css::find_stylesheet_links(&content);

                    // Collect resolved CSS paths in document order.
                    let mut resolved_paths: Vec<String> = Vec::new();
                    for link in &links {
                        if link.bundlable {
                            let resolved = crate::bundle_css::resolve_css_path(rel, &link.href);
                            resolved_paths.push(resolved);
                        }
                    }

                    if !resolved_paths.is_empty()
                        && let Ok(mut css_set) = unique_css.lock() {
                            for p in &resolved_paths {
                                css_set.insert(p.clone());
                            }
                        }

                    // Capture index.html's cascade order once.
                    if rel == "index.html"
                        && let Ok(mut order) = index_order.lock()
                            && order.is_empty() {
                                *order = resolved_paths;
                            }

                    if !links.is_empty()
                        && let Ok(mut pf) = per_file.lock() {
                            pf.push((rel.clone(), links));
                        }

                    let done = done_counter.fetch_add(1, Ordering::Relaxed) + 1;
                    if !verbose && done.is_multiple_of(16) {
                        eprint!("\rScanning: {done}/{file_count} files");
                        let _ = std::io::stderr().flush();
                    }
                }
            });
        }
    });

    if !verbose && !files.is_empty() {
        eprintln!();
    }

    let unique_css = unique_css.into_inner().unwrap();
    let index_order = index_order.into_inner().unwrap();
    let per_file = per_file.into_inner().unwrap();

    // Build canonical concatenation order: index.html's cascade order first,
    // then remaining files (not in index.html) sorted alphabetically.
    let mut ordered_files: Vec<String> = Vec::with_capacity(unique_css.len());
    let mut seen: FxHashSet<String> = FxHashSet::default();
    for p in &index_order {
        if unique_css.contains(p) && seen.insert(p.clone()) {
            ordered_files.push(p.clone());
        }
    }
    // Append any CSS files not referenced by index.html, sorted.
    let mut remaining: Vec<&String> = unique_css.iter().filter(|p| !seen.contains(*p)).collect();
    remaining.sort();
    for p in remaining {
        ordered_files.push(p.clone());
    }

    eprintln!(
        "Found {} unique CSS file(s) to bundle across {} HTML file(s).",
        unique_css.len(),
        per_file
            .iter()
            .filter(|(_, links)| links.iter().any(|l| l.bundlable))
            .count(),
    );

    if ordered_files.is_empty() {
        eprintln!("No local CSS files to bundle.");
        return Ok(());
    }

    // Phase 2: concatenate in canonical order and write the bundle.
    let bundle_result = crate::bundle_css::bundle_css_files(root_path, &ordered_files, bundle_dir)
        .map_err(|e| format!("failed to create bundle: {e}"))?;

    let bundle_rel = &bundle_result.bundle_rel;
    let bundle_disk_path = root_path.join(bundle_rel);
    let bundle_size = bundle_result.concatenated.len();

    if apply {
        if let Some(parent) = bundle_disk_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&bundle_disk_path, &bundle_result.concatenated)
            .map_err(|e| format!("failed to write bundle: {e}"))?;
        eprintln!(
            "Wrote bundle ({:.1} KB) to ./{}",
            bundle_size as f64 / 1024.0,
            bundle_rel,
        );

        // Phase 2b: trash source CSS files that were bundled.
        let trash_root = root_path.join(".trash");
        let mut trashed = 0usize;
        for css_rel in &ordered_files {
            let src = root_path.join(css_rel);
            if !src.is_file() {
                continue;
            }
            let trash_path = trash_root.join(css_rel);
            if let Some(parent) = trash_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if std::fs::rename(&src, &trash_path).is_ok() {
                trashed += 1;
            }
        }
        if trashed > 0 {
            eprintln!("Trashed {} source CSS file(s) to .trash/.", trashed,);
        }
    } else {
        eprintln!(
            "Dry-run: would write bundle ({:.1} KB) to ./{}",
            bundle_size as f64 / 1024.0,
            bundle_rel,
        );
    }

    // Phase 3: rewrite each HTML file that has bundlable links.
    if apply {
        eprintln!(
            "Rewriting {} file(s) with {jobs} workers...",
            per_file.len()
        );

        let rewrite_count = AtomicUsize::new(0);
        let rewrite_errors: std::sync::Mutex<Vec<(String, String)>> =
            std::sync::Mutex::new(Vec::new());
        let rewrite_done = AtomicUsize::new(0);

        let _ = crossbeam::thread::scope(|s| {
            let per_file: &[(String, Vec<crate::bundle_css::CssLink>)] = &per_file;
            let bundle_rel: &str = bundle_rel;
            let rewrite_count: &AtomicUsize = &rewrite_count;
            let rewrite_errors: &std::sync::Mutex<Vec<(String, String)>> = &rewrite_errors;
            let rewrite_done: &AtomicUsize = &rewrite_done;
            let index = Arc::new(AtomicUsize::new(0));
            for _ in 0..workers {
                let index = Arc::clone(&index);
                s.spawn(move |_| {
                    loop {
                        let i = index.fetch_add(1, Ordering::Relaxed);
                        if i >= per_file.len() {
                            break;
                        }
                        let (rel, links) = &per_file[i];
                        let path = root_path.join(rel);

                        let bundlable_spans: Vec<std::ops::Range<usize>> = links
                            .iter()
                            .filter(|l| l.bundlable)
                            .map(|l| l.span.clone())
                            .collect();

                        if bundlable_spans.is_empty() {
                            let done = rewrite_done.fetch_add(1, Ordering::Relaxed) + 1;
                            if !verbose && done.is_multiple_of(16) {
                                eprint!("\rRewriting: {done}/{} files", per_file.len());
                                let _ = std::io::stderr().flush();
                            }
                            continue;
                        }

                        let content = std::fs::read_to_string(&path).unwrap_or_default();
                        let bundle_href = crate::bundle_css::compute_relative_path(rel, bundle_rel);
                        let modified = crate::bundle_css::rewrite_html_for_bundle(
                            &content,
                            &bundlable_spans,
                            &bundle_href,
                        );

                        let tmp = path.with_extension("tmp");
                        if let Err(e) = std::fs::write(&tmp, &modified)
                            .map_err(|e| format!("write tmp: {e}"))
                            .and_then(|_| {
                                std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))
                            })
                        {
                            if let Ok(mut errs) = rewrite_errors.lock() {
                                errs.push((rel.clone(), format!("{}: {e}", path.display())));
                            }
                        } else {
                            rewrite_count.fetch_add(1, Ordering::Relaxed);
                        }

                        let done = rewrite_done.fetch_add(1, Ordering::Relaxed) + 1;
                        if !verbose && done.is_multiple_of(16) {
                            eprint!("\rRewriting: {done}/{} files", per_file.len());
                            let _ = std::io::stderr().flush();
                        }
                    }
                });
            }
        });

        if !verbose && !per_file.is_empty() {
            eprintln!();
        }

        let rewrite_errors = rewrite_errors.into_inner().unwrap();
        if !rewrite_errors.is_empty() {
            eprintln!("Errors:");
            for (rel, err) in &rewrite_errors {
                eprintln!("  {rel}: {err}");
            }
        }

        let rewrote = rewrite_count.load(Ordering::Relaxed);
        eprintln!("Rewrote {} file(s), bundle at ./{}", rewrote, bundle_rel,);
    } else {
        // Dry-run: show what would change per file.
        for (rel, links) in &per_file {
            let bundlable: Vec<_> = links.iter().filter(|l| l.bundlable).collect();
            if bundlable.is_empty() {
                continue;
            }
            let bundle_href = crate::bundle_css::compute_relative_path(rel, bundle_rel);
            println!("./{rel}");
            for link in &bundlable {
                let resolved = crate::bundle_css::resolve_css_path(rel, &link.href);
                println!("  - {resolved}");
            }
            println!("  + <link rel=\"stylesheet\" href=\"{bundle_href}\">");
            println!();
        }
        eprintln!(
            "Dry-run: {} unique CSS file(s) would bundle into ./{}. Run with --apply to write.",
            unique_css.len(),
            bundle_rel,
        );
    }

    Ok(())
}

fn cmd_extract_css(args: Args) -> Result<(), String> {
    let root = args.root.as_deref().unwrap_or(".");
    let apply = args.apply;
    let verbose = args.verbose;
    let dir = &args.dir;

    let default_include = vec!["*.html".to_string(), "*.htm".to_string()];
    let include: &[String] = if args.include.is_empty() {
        &default_include
    } else {
        &args.include
    };
    let jobs = if args.jobs == 0 {
        num_cpus() * 4
    } else {
        args.jobs
    };

    eprintln!("Discovering HTML files in {root}...");
    let files = collect_html_depth_first(root, include, &args.exclude);

    if verbose {
        eprintln!("Found {} HTML file(s) to process", files.len());
    }
    if files.is_empty() {
        eprintln!("No HTML files found.");
        return Ok(());
    }

    if apply {
        eprintln!(
            "Extracting inline CSS from {} file(s) into {dir}/ with {jobs} workers...",
            files.len()
        );
    } else {
        eprintln!(
            "Dry-run: scanning {} file(s) for inline CSS with {jobs} workers...",
            files.len()
        );
    }

    let root_path = std::path::Path::new(root);
    let file_count = files.len();
    let workers = jobs.min(files.len());

    let total_styles = AtomicUsize::new(0);
    let total_files = AtomicUsize::new(0);
    let errors_mu = std::sync::Mutex::new(Vec::new());
    let file_results = std::sync::Mutex::new(Vec::new());
    let done_counter = AtomicUsize::new(0);

    let _ = crossbeam::thread::scope(|s| {
        let files: &[String] = &files;
        let errors_mu: &std::sync::Mutex<Vec<(String, String)>> = &errors_mu;
        let file_results: &std::sync::Mutex<Vec<(String, Vec<String>)>> = &file_results;
        let total_styles: &AtomicUsize = &total_styles;
        let total_files: &AtomicUsize = &total_files;
        let done_counter: &AtomicUsize = &done_counter;
        let index = Arc::new(AtomicUsize::new(0));
        for _ in 0..workers {
            let index = Arc::clone(&index);
            s.spawn(move |_| {
                loop {
                    let i = index.fetch_add(1, Ordering::Relaxed);
                    if i >= file_count {
                        break;
                    }
                    let rel = &files[i];
                    let path = root_path.join(rel);
                    let content = std::fs::read_to_string(&path).unwrap_or_default();

                    let result = match crate::extract_css::extract_css(&content, rel, dir) {
                        Ok(r) => r,
                        Err(e) => {
                            if let Ok(mut errs) = errors_mu.lock() {
                                errs.push((rel.clone(), e));
                            }
                            let done = done_counter.fetch_add(1, Ordering::Relaxed) + 1;
                            if !verbose && done.is_multiple_of(16) {
                                eprint!("\rProcessing: {done}/{file_count} files");
                                let _ = std::io::stderr().flush();
                            }
                            continue;
                        }
                    };

                    if result.writes.is_empty() {
                        let done = done_counter.fetch_add(1, Ordering::Relaxed) + 1;
                        if !verbose && done.is_multiple_of(16) {
                            eprint!("\rProcessing: {done}/{file_count} files");
                            let _ = std::io::stderr().flush();
                        }
                        continue;
                    }

                    if apply {
                        // Delete <style> blocks (reverse span order, like zap).
                        // Extend each span past trailing whitespace so removal
                        // doesn't leave blank lines behind.
                        let mut modified = content;
                        let mut spans: Vec<std::ops::Range<usize>> =
                            result.spans_to_delete.to_vec();
                        spans.sort_by_key(|s| std::cmp::Reverse(s.start));
                        for span in &spans {
                            let mut end = span.end;
                            for &b in modified.as_bytes()[end..].iter() {
                                if b == b' ' || b == b'\t' || b == b'\r' || b == b'\n' {
                                    end += 1;
                                } else {
                                    break;
                                }
                            }
                            modified.replace_range(span.start..end, "");
                        }

                        // Insert <link> tags before </head>.
                        // Minified HTML may omit </head> (HTML5-optional).
                        // Fall back to before <body, then after <html>, then
                        // position 0.
                        let links = result.link_tags.join("\n");
                        let anchor = modified.find("</head>").or_else(|| {
                            modified.match_indices("</head").find_map(|(i, _)| {
                                let after = &modified[i + 6..];
                                if after.is_empty() {
                                    return Some(i);
                                }
                                if after.as_bytes()[0].is_ascii_alphabetic() {
                                    None // skip </header>, </headings, etc.
                                } else {
                                    Some(i)
                                }
                            })
                        });
                        if let Some(pos) = anchor {
                            modified.insert_str(pos, &format!("\n{links}\n"));
                        } else if let Some(pos) = modified.find("<body") {
                            modified.insert_str(pos, &format!("{links}\n"));
                        } else if let Some(pos) = modified.find("<html") {
                            let close = modified[pos..]
                                .find('>')
                                .map(|e| pos + e + 1)
                                .unwrap_or(pos);
                            modified.insert_str(close, &format!("\n{links}\n"));
                        } else {
                            modified.insert_str(0, &format!("{links}\n"));
                        }

                        // Write CSS files (content-addressed, concurrency-safe).
                        let css_dir = root_path.join(dir);
                        for (hash, css_content) in &result.writes {
                            let prefix = &hash[..2];
                            let css_path = css_dir.join(prefix).join(format!("{hash}.css"));
                            if let Some(parent) = css_path.parent() {
                                let _ = std::fs::create_dir_all(parent);
                            }
                            // create_new is atomic (O_CREAT | O_EXCL) — if
                            // another worker already wrote this block, or the
                            // file exists from a prior run, skip.
                            if let Ok(mut f) = std::fs::OpenOptions::new()
                                .write(true)
                                .create_new(true)
                                .open(&css_path)
                            {
                                let _ = std::io::Write::write_all(&mut f, css_content.as_bytes());
                            }
                        }

                        // Write modified HTML via tmp+rename.
                        let tmp = path.with_extension("tmp");
                        if let Err(e) = std::fs::write(&tmp, &modified)
                            .map_err(|e| format!("write tmp: {e}"))
                            .and_then(|_| {
                                std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))
                            })
                            && let Ok(mut errs) = errors_mu.lock() {
                                errs.push((rel.clone(), format!("{}: {e}", path.display())));
                            }
                    }

                    total_styles.fetch_add(result.writes.len(), Ordering::Relaxed);
                    total_files.fetch_add(1, Ordering::Relaxed);
                    if let Ok(mut results) = file_results.lock() {
                        let paths: Vec<String> = result
                            .writes
                            .iter()
                            .map(|(hash, _)| format!("{dir}/{}/{hash}.css", &hash[..2]))
                            .collect();
                        results.push((rel.clone(), paths));
                    }

                    let done = done_counter.fetch_add(1, Ordering::Relaxed) + 1;
                    if !verbose && done.is_multiple_of(16) {
                        eprint!("\rProcessing: {done}/{file_count} files");
                        let _ = std::io::stderr().flush();
                    }
                }
            });
        }
    });

    if !verbose && !files.is_empty() {
        eprintln!();
    }

    let errors = errors_mu.into_inner().unwrap();
    if !errors.is_empty() {
        eprintln!("Errors:");
        for (rel, err) in &errors {
            eprintln!("  {rel}: {err}");
        }
    }

    let mut file_results = file_results.into_inner().unwrap();
    file_results.sort_by(|a, b| a.0.cmp(&b.0));
    let total_styles = total_styles.load(Ordering::Relaxed);
    let total_files = total_files.load(Ordering::Relaxed);
    for (rel, paths) in &file_results {
        if paths.is_empty() {
            continue;
        }
        println!("./{rel}");
        for p in paths {
            println!("  → {p}");
        }
        println!();
    }

    if apply {
        eprintln!(
            "Extracted {} inline style(s) from {} file(s) into {dir}/.",
            total_styles, total_files
        );
    } else {
        eprintln!(
            "Dry-run: found {} inline style(s) in {} file(s). Run with --apply to extract.",
            total_styles, total_files
        );
    }

    Ok(())
}

/// Depth-first HTML file collector. Groups sibling files together so workers
/// processing adjacent entries encounter the same CSS blocks close in time,
/// reducing the window for duplicate writes.
fn collect_html_depth_first(root: &str, include: &[String], exclude: &[String]) -> Vec<String> {
    let include_pats: Vec<glob::Pattern> = include
        .iter()
        .filter_map(|p| glob::Pattern::new(p).ok())
        .collect();
    let exclude_pats: Vec<glob::Pattern> = exclude
        .iter()
        .filter_map(|p| glob::Pattern::new(p).ok())
        .collect();
    let mut files = Vec::new();
    walk_depth_first(
        std::path::Path::new(root),
        root,
        &include_pats,
        &exclude_pats,
        &mut files,
    );
    files
}

fn walk_depth_first(
    dir: &std::path::Path,
    root: &str,
    include: &[glob::Pattern],
    exclude: &[glob::Pattern],
    files: &mut Vec<String>,
) {
    let mut entries: Vec<_> = match std::fs::read_dir(dir) {
        Ok(iter) => iter.filter_map(|e| e.ok()).collect(),
        Err(_) => return,
    };
    entries.sort_by_key(|a| a.file_name());

    for entry in &entries {
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();

        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            walk_depth_first(&path, root, include, exclude, files);
        } else if entry.file_type().map(|t| t.is_file()).unwrap_or(false)
            && include.iter().any(|p| p.matches(&rel))
            && !exclude.iter().any(|p| p.matches(&rel))
        {
            files.push(rel);
        }
    }
}

fn cmd_zap(args: Args) -> Result<(), String> {
    let root = args.root.as_deref().unwrap_or(".");
    let selector_raw = args.zap_tag.as_deref().ok_or("missing selector")?;
    let query = args.zap_query.as_deref().ok_or("missing query")?;
    let apply = args.apply;
    let verbose = args.verbose;

    if selector_raw.is_empty() {
        return Err("selector must not be empty".into());
    }
    if query.is_empty() {
        return Err("query must not be empty".into());
    }

    let selector =
        crate::zap::parse_selector(selector_raw).map_err(|e| format!("invalid selector: {e}"))?;

    let default_include = vec!["*.html".to_string(), "*.htm".to_string()];
    let include: &[String] = if args.include.is_empty() {
        &default_include
    } else {
        &args.include
    };
    let jobs = if args.jobs == 0 {
        num_cpus() * 4
    } else {
        args.jobs
    };

    eprintln!("Discovering HTML files in {root}...");
    let files = iter_html_files(root, include, &args.exclude);

    if verbose {
        eprintln!("Found {} HTML file(s) to scan", files.len());
    }
    if files.is_empty() {
        eprintln!("No HTML files found.");
        return Ok(());
    }

    let sel_display = selector.source.clone();
    if apply {
        eprintln!(
            "Zapping {sel_display} elements containing \"{query}\" in {} file(s) with {jobs} workers...",
            files.len()
        );
    } else {
        eprintln!(
            "Dry-run: scanning {} file(s) for {sel_display} elements containing \"{query}\" with {jobs} workers...",
            files.len()
        );
    }

    let root_path = std::path::Path::new(root);
    let file_count = files.len();
    let workers = jobs.min(files.len());

    let total_matches = AtomicUsize::new(0);
    let file_results = std::sync::Mutex::new(Vec::new());
    let errors: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());
    let done_counter = AtomicUsize::new(0);

    let _ = crossbeam::thread::scope(|s| {
        let files: &[String] = &files;
        let selector: &crate::zap::SimpleSelector = &selector;
        let errors: &std::sync::Mutex<Vec<(String, String)>> = &errors;
        let file_results: &std::sync::Mutex<Vec<(String, Vec<crate::zap::ZapMatch>)>> =
            &file_results;
        let total_matches: &AtomicUsize = &total_matches;
        let done_counter: &AtomicUsize = &done_counter;
        let index = Arc::new(AtomicUsize::new(0));
        for _ in 0..workers {
            let index = Arc::clone(&index);
            s.spawn(move |_| {
                loop {
                    let i = index.fetch_add(1, Ordering::Relaxed);
                    if i >= file_count {
                        break;
                    }
                    let rel = &files[i];
                    let path = root_path.join(rel);
                    let content = std::fs::read_to_string(&path).unwrap_or_default();
                    let (result, modified) = if apply {
                        match crate::rewriter::zap_html(&content, selector, query) {
                            Ok((html, matches)) => (
                                crate::zap::ZapResult {
                                    matches,
                                    error: None,
                                },
                                Some(html),
                            ),
                            Err(e) => {
                                if let Ok(mut errs) = errors.lock() {
                                    errs.push((rel.clone(), e));
                                }
                                let done = done_counter.fetch_add(1, Ordering::Relaxed) + 1;
                                if !verbose && done.is_multiple_of(16) {
                                    eprint!("\rScanning: {done}/{file_count} files");
                                    let _ = std::io::stderr().flush();
                                }
                                continue;
                            }
                        }
                    } else {
                        let result = crate::zap::scan_html(&content, selector, query);
                        (result, None)
                    };
                    if let Some(new_html) = modified
                        && (!new_html.is_empty() || content.is_empty())
                    {
                        let tmp = path.with_extension("tmp");
                        if let Err(e) = std::fs::write(&tmp, &new_html)
                            .map_err(|e| format!("write tmp: {e}"))
                            .and_then(|_| {
                                std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))
                            })
                        {
                            if let Ok(mut errs) = errors.lock() {
                                errs.push((rel.clone(), format!("{}: {e}", path.display())));
                            }
                            let done = done_counter.fetch_add(1, Ordering::Relaxed) + 1;
                            if !verbose && done.is_multiple_of(16) {
                                eprint!("\rScanning: {done}/{file_count} files");
                                let _ = std::io::stderr().flush();
                            }
                            continue;
                        }
                    }
                    if !result.matches.is_empty() {
                        total_matches.fetch_add(result.matches.len(), Ordering::Relaxed);
                        if let Ok(mut results) = file_results.lock() {
                            results.push((rel.clone(), result.matches));
                        }
                    }
                    let done = done_counter.fetch_add(1, Ordering::Relaxed) + 1;
                    if !verbose && done.is_multiple_of(16) {
                        eprint!("\rScanning: {done}/{file_count} files");
                        let _ = std::io::stderr().flush();
                    }
                }
            });
        }
    });

    let total_matches = total_matches.load(Ordering::Relaxed);
    let mut file_results = file_results.into_inner().unwrap();
    let errors = errors.into_inner().unwrap();

    if !verbose && !files.is_empty() {
        eprintln!();
    }

    if !errors.is_empty() {
        eprintln!("Errors:");
        for (rel, err) in &errors {
            if rel.is_empty() {
                eprintln!("  {err}");
            } else {
                eprintln!("  {rel}: {err}");
            }
        }
    }

    file_results.sort_by(|a, b| a.0.cmp(&b.0));
    for (rel, matches) in &file_results {
        println!("./{rel}");
        for m in matches {
            println!("  {} containing: {}", m.tag, m.text_preview);
        }
        println!();
    }

    if apply {
        eprintln!(
            "Zapped {} element(s) in {} file(s).",
            total_matches,
            file_results.len()
        );
    } else {
        eprintln!(
            "Dry-run: found {} matching {sel_display} element(s) in {} file(s). Run with --apply to remove.",
            total_matches,
            file_results.len()
        );
    }

    Ok(())
}

/// Split a URL from an optional descriptor (e.g. "image.jpg 400w" → ("image.jpg", "400w")).
fn split_url_descriptor(entry: &str) -> (&str, &str) {
    entry.split_once(' ').unwrap_or((entry, ""))
}

/// Upper bound on decoded image size in bytes for concurrency calculations.
/// A 5 MB PNG decompresses to ~20 MB of RGBA; 20 MB is a reasonable web-image estimate.
const PER_IMAGE_MEMORY_ESTIMATE: u64 = 8 * 1024 * 1024;

/// Returns *available* system memory in bytes (free + inactive pages), or a
/// fallback.  Available memory is what the OS can hand out without swapping.
fn system_available_memory_bytes() -> u64 {
    #[cfg(target_os = "macos")]
    {
        let page_size = std::process::Command::new("sysctl")
            .args(["-n", "vm.pagesize"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(16384);
        let vm_stat = std::process::Command::new("vm_stat")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok());
        if let Some(out) = vm_stat {
            let mut free = 0u64;
            let mut inactive = 0u64;
            for line in out.lines() {
                if let Some(rest) = line
                    .strip_prefix("Pages free:")
                    .or_else(|| line.strip_prefix("Pages inactive:"))
                {
                    let val: u64 = rest.trim_end_matches('.').trim().parse().unwrap_or(0);
                    if line.contains("free") {
                        free = val;
                    } else {
                        inactive = val;
                    }
                }
            }
            let available = (free + inactive) * page_size;
            if available > 0 {
                return available;
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(s) = std::fs::read_to_string("/proc/meminfo") {
            let mut available: u64 = 0;
            for line in s.lines() {
                if let Some(rest) = line.strip_prefix("MemAvailable:") {
                    let kb: u64 = rest.trim_end_matches(" kB").trim().parse().unwrap_or(0);
                    if kb > 0 {
                        available = kb * 1024;
                        break;
                    }
                }
            }
            if available > 0 {
                return available;
            }
        }
    }
    // Fallback: assume 2 GB available.
    2 * 1024 * 1024 * 1024
}

/// Cap the job count so that peak memory (jobs × per-image decode buffer)
/// stays under half of available system memory.
fn memory_capped_jobs(raw_jobs: usize) -> usize {
    let available = system_available_memory_bytes();
    let max_jobs = (available / PER_IMAGE_MEMORY_ESTIMATE) as usize;
    raw_jobs.min(max_jobs).max(1)
}

fn cmd_towebp(args: Args) -> Result<(), String> {
    let root = args.root.as_deref().unwrap_or(".");
    let apply = args.apply;
    let verbose = args.verbose;

    let default_include = vec!["*.html".to_string(), "*.htm".to_string()];
    let include: &[String] = if args.include.is_empty() {
        &default_include
    } else {
        &args.include
    };
    let raw_jobs = if args.jobs == 0 {
        num_cpus() * 8
    } else {
        args.jobs
    };
    let jobs = memory_capped_jobs(raw_jobs);

    eprintln!("Discovering HTML files in {root}...");
    let files = iter_html_files(root, include, &args.exclude);

    if verbose {
        eprintln!("Found {} HTML file(s) to scan", files.len());
    }
    if files.is_empty() {
        eprintln!("No HTML files found.");
        return Ok(());
    }

    if verbose {
        eprintln!(
            "Memory cap: {} workers (raw: {raw_jobs}, available: {} GB)",
            jobs,
            system_available_memory_bytes() / (1024 * 1024 * 1024),
        );
    }
    if apply {
        eprintln!(
            "Converting jpg/jpeg/png → webp in {} file(s) with {jobs} workers...",
            files.len()
        );
    } else {
        eprintln!(
            "Dry-run: scanning {} file(s) for jpg/jpeg/png URLs with {jobs} workers...",
            files.len()
        );
    }

    let root_path = std::path::Path::new(root);

    // Phase 1: collect unique images and convert them (only when --apply).
    let converted: FxHashSet<String> = if apply {
        let mut unique: FxHashSet<String> =
            FxHashSet::with_capacity_and_hasher(files.len(), Default::default());
        // Keyed by resolved filesystem path so the same image referenced from
        // multiple HTML files is deduplicated.

        // Scan all HTML files to collect image references.
        {
            let counter = AtomicUsize::new(0);
            let file_total = files.len();
            let unique_mu = std::sync::Mutex::new(&mut unique);
            let phase1_workers = jobs.min(files.len());

            let _ = crossbeam::thread::scope(|s| {
                let files: &[String] = &files;
                let counter: &AtomicUsize = &counter;
                let unique_mu: &std::sync::Mutex<&mut FxHashSet<String>> = &unique_mu;
                let index = Arc::new(AtomicUsize::new(0));
                for _ in 0..phase1_workers {
                    let index = Arc::clone(&index);
                    s.spawn(move |_| {
                        loop {
                            let i = index.fetch_add(1, Ordering::Relaxed);
                            if i >= file_total {
                                break;
                            }
                            let rel = &files[i];
                            let path = root_path.join(rel);
                            let content = std::fs::read_to_string(&path).unwrap_or_default();
                            let matches = crate::towebp::scan_towebp(&content);
                            for m in &matches {
                                let (url, _desc) = split_url_descriptor(&m.url);
                                let resolved = crate::rewriter::resolve_html_url(rel, url);
                                if let Ok(mut guard) = unique_mu.lock() {
                                    guard.insert(resolved);
                                }
                            }
                            let done = counter.fetch_add(1, Ordering::Relaxed) + 1;
                            if !verbose && done.is_multiple_of(16) {
                                eprint!("\rPhase 1 — scanning: {done}/{file_total} files");
                                let _ = std::io::stderr().flush();
                            }
                        }
                    });
                }
            });
            drop(unique_mu);
            if !verbose && !files.is_empty() {
                eprint!("\rPhase 1 — scanning: {file_total}/{file_total} files\n");
                let _ = std::io::stderr().flush();
            }
        }

        let unique_images: Vec<String> = unique.into_iter().collect();
        eprintln!("Found {} unique image(s) to convert", unique_images.len());

        // Convert images in parallel, bounded by the semaphore to avoid OOM.
        // Each worker holds at most one decoded image in memory at a time.
        let mut converted = FxHashSet::default();
        let trash_root = root_path.join(".trash");
        let converted_mu = std::sync::Mutex::new(&mut converted);
        let counter = Arc::new(AtomicUsize::new(0));
        let convert_total = unique_images.len();

        let converted_count = AtomicUsize::new(0);
        let failed_count = AtomicUsize::new(0);
        let skipped_count = AtomicUsize::new(0);
        let failure_details: std::sync::Mutex<Vec<(String, String)>> =
            std::sync::Mutex::new(Vec::new());
        let phase1b_workers = jobs.min(unique_images.len());

        let _ = crossbeam::thread::scope(|s| {
            let unique_images: &[String] = &unique_images;
            let counter: &AtomicUsize = &counter;
            let converted_count: &AtomicUsize = &converted_count;
            let failed_count: &AtomicUsize = &failed_count;
            let skipped_count: &AtomicUsize = &skipped_count;
            let converted_mu: &std::sync::Mutex<&mut FxHashSet<String>> = &converted_mu;
            let failure_details: &std::sync::Mutex<Vec<(String, String)>> = &failure_details;
            let trash_root: &std::path::Path = &trash_root;
            let index = Arc::new(AtomicUsize::new(0));
            for _ in 0..phase1b_workers {
                let index = Arc::clone(&index);
                s.spawn(move |_| {
                    loop {
                        let i = index.fetch_add(1, Ordering::Relaxed);
                        if i >= convert_total {
                            break;
                        }
                        let resolved = &unique_images[i];
                        let abs_path = root_path.join(resolved);

                        // Skip remote URLs and data URIs.
                        if resolved.starts_with("http://")
                            || resolved.starts_with("https://")
                            || resolved.starts_with("data:")
                        {
                            skipped_count.fetch_add(1, Ordering::Relaxed);
                            let done = counter.fetch_add(1, Ordering::Relaxed) + 1;
                            if !verbose && done.is_multiple_of(16) {
                                eprint!("\rConverting: {done}/{convert_total} images");
                                let _ = std::io::stderr().flush();
                            }
                            continue;
                        }
                        // If the original doesn't exist but the .webp already does
                        // (e.g. from a previous run where HTML rewriting was
                        // interrupted), treat it as already converted.
                        if !abs_path.exists() {
                            let webp_path = abs_path.with_extension("webp");
                            if webp_path.exists() {
                                if let Ok(mut guard) = converted_mu.lock() {
                                    guard.insert(resolved.clone());
                                }
                                converted_count.fetch_add(1, Ordering::Relaxed);
                                let done = counter.fetch_add(1, Ordering::Relaxed) + 1;
                                if !verbose && done.is_multiple_of(16) {
                                    eprint!("\rConverting: {done}/{convert_total} images");
                                    let _ = std::io::stderr().flush();
                                }
                                continue;
                            }
                            skipped_count.fetch_add(1, Ordering::Relaxed);
                            let done = counter.fetch_add(1, Ordering::Relaxed) + 1;
                            if !verbose && done.is_multiple_of(16) {
                                eprint!("\rConverting: {done}/{convert_total} images");
                                let _ = std::io::stderr().flush();
                            }
                            continue;
                        }

                        if verbose {
                            let _ = write!(std::io::stderr(), "  converting {resolved} ... ");
                        }
                        let outcome = match crate::webp_encode::convert_to_webp(&abs_path) {
                            Ok(ConvertResult::Converted(webp_bytes)) => {
                                let webp_path = abs_path.with_extension("webp");
                                if let Err(e) = std::fs::write(&webp_path, &webp_bytes) {
                                    Err(format!("write webp: {e}"))
                                } else {
                                    let trash_path = trash_root.join(resolved);
                                    if let Some(parent) = trash_path.parent() {
                                        if let Err(e) = std::fs::create_dir_all(parent) {
                                            let _ = std::fs::remove_file(&webp_path);
                                            Err(format!("create trash dir: {e}"))
                                        } else {
                                            let trash_path = unique_trash_path(&trash_path);
                                            if let Err(e) = std::fs::rename(&abs_path, &trash_path)
                                            {
                                                let _ = std::fs::remove_file(&webp_path);
                                                Err(format!("move to trash: {e}"))
                                            } else {
                                                Ok(())
                                            }
                                        }
                                    } else {
                                        let trash_path = unique_trash_path(&trash_path);
                                        if let Err(e) = std::fs::rename(&abs_path, &trash_path) {
                                            let _ = std::fs::remove_file(&webp_path);
                                            Err(format!("move to trash: {e}"))
                                        } else {
                                            Ok(())
                                        }
                                    }
                                }
                            }
                            Ok(ConvertResult::AlreadyWebp) => {
                                let already_webp_ext = abs_path
                                    .extension()
                                    .and_then(|e| e.to_str())
                                    .map(|e| e.eq_ignore_ascii_case("webp"))
                                    .unwrap_or(false);
                                if already_webp_ext {
                                    Ok(())
                                } else {
                                    let webp_path = abs_path.with_extension("webp");
                                    if webp_path.exists() {
                                        std::fs::remove_file(&abs_path)
                                            .map_err(|e| format!("remove duplicate: {e}"))
                                    } else if let Err(e) = std::fs::rename(&abs_path, &webp_path) {
                                        Err(format!("rename to webp: {e}"))
                                    } else {
                                        Ok(())
                                    }
                                }
                            }
                            Err(e) => Err(e),
                        };
                        let done = counter.fetch_add(1, Ordering::Relaxed) + 1;
                        if !verbose && done.is_multiple_of(16) {
                            eprint!("\rConverting: {done}/{convert_total} images");
                            let _ = std::io::stderr().flush();
                        }
                        match outcome {
                            Ok(()) => {
                                if verbose {
                                    let _ = writeln!(std::io::stderr(), "OK");
                                }
                                if let Ok(mut guard) = converted_mu.lock() {
                                    guard.insert(resolved.clone());
                                }
                                converted_count.fetch_add(1, Ordering::Relaxed);
                            }
                            Err(e) => {
                                if verbose {
                                    let _ = writeln!(std::io::stderr(), "FAILED ({e})");
                                }
                                if let Ok(mut fd) = failure_details.lock() {
                                    fd.push((resolved.clone(), e));
                                }
                                failed_count.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                });
            }
        });

        let converted_count = converted_count.load(Ordering::Relaxed);
        let failed_count = failed_count.load(Ordering::Relaxed);
        let skipped_count = skipped_count.load(Ordering::Relaxed);
        let failure_details = failure_details.into_inner().unwrap();
        if !failure_details.is_empty() {
            eprintln!();
            for (path, err) in &failure_details {
                eprintln!("  FAILED {path}: {err}");
            }
        }
        drop(converted_mu);

        if !verbose && convert_total > 0 {
            eprintln!();
        }

        eprintln!("Converted {converted_count}, failed {failed_count}, skipped {skipped_count}.",);
        if converted.is_empty() {
            eprintln!("No images converted; HTML will not be modified.");
        }

        converted
    } else {
        FxHashSet::default()
    };

    // Phase 2: rewite HTML files (gated on `converted`).
    let converted = Arc::new(converted);
    let total_matches = AtomicUsize::new(0);
    let file_results: std::sync::Mutex<Vec<(String, Vec<crate::towebp::WebpMatch>)>> =
        std::sync::Mutex::new(Vec::new());
    let errors: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());
    let phase2_done = AtomicUsize::new(0);
    let phase2_file_count = files.len();
    let phase2_workers = jobs.min(files.len());

    let _ = crossbeam::thread::scope(|s| {
        let files: &[String] = &files;
        let converted: &FxHashSet<String> = &converted;
        let total_matches: &AtomicUsize = &total_matches;
        let file_results: &std::sync::Mutex<Vec<(String, Vec<crate::towebp::WebpMatch>)>> =
            &file_results;
        let errors: &std::sync::Mutex<Vec<(String, String)>> = &errors;
        let phase2_done: &AtomicUsize = &phase2_done;
        let index = Arc::new(AtomicUsize::new(0));
        for _ in 0..phase2_workers {
            let index = Arc::clone(&index);
            s.spawn(move |_| {
                loop {
                    let i = index.fetch_add(1, Ordering::Relaxed);
                    if i >= phase2_file_count {
                        break;
                    }
                    let rel = &files[i];
                    let path = root_path.join(rel);
                    let content = std::fs::read_to_string(&path).unwrap_or_default();
                    let all_matches = crate::towebp::scan_towebp(&content);
                    if apply {
                        match crate::rewriter::towebp_html(&content, rel, converted) {
                            Ok(new_html) => {
                                let tmp = path.with_extension("tmp");
                                if let Err(e) = std::fs::write(&tmp, &new_html)
                                    .map_err(|e| format!("write tmp: {e}"))
                                    .and_then(|_| {
                                        std::fs::rename(&tmp, &path)
                                            .map_err(|e| format!("rename: {e}"))
                                    })
                                {
                                    if let Ok(mut errs) = errors.lock() {
                                        errs.push((
                                            rel.clone(),
                                            format!("{}: {e}", path.display()),
                                        ));
                                    }
                                    let done = phase2_done.fetch_add(1, Ordering::Relaxed) + 1;
                                    if !verbose && done.is_multiple_of(16) {
                                        eprint!("\rProcessing: {done}/{phase2_file_count} files");
                                        let _ = std::io::stderr().flush();
                                    }
                                    continue;
                                }
                            }
                            Err(e) => {
                                if let Ok(mut errs) = errors.lock() {
                                    errs.push((rel.clone(), format!("{}: {e}", path.display())));
                                }
                                let done = phase2_done.fetch_add(1, Ordering::Relaxed) + 1;
                                if !verbose && done.is_multiple_of(16) {
                                    eprint!("\rProcessing: {done}/{phase2_file_count} files");
                                    let _ = std::io::stderr().flush();
                                }
                                continue;
                            }
                        }
                    }
                    let matches: Vec<crate::towebp::WebpMatch> = if apply {
                        all_matches
                            .into_iter()
                            .filter(|m| {
                                let (url, _desc) = split_url_descriptor(&m.url);
                                let resolved = crate::rewriter::resolve_html_url(rel, url);
                                converted.contains(&resolved)
                            })
                            .collect()
                    } else {
                        all_matches
                    };
                    if !matches.is_empty() {
                        total_matches.fetch_add(matches.len(), Ordering::Relaxed);
                        if let Ok(mut results) = file_results.lock() {
                            results.push((rel.clone(), matches));
                        }
                    }
                    let done = phase2_done.fetch_add(1, Ordering::Relaxed) + 1;
                    if !verbose && done.is_multiple_of(16) {
                        eprint!("\rProcessing: {done}/{phase2_file_count} files");
                        let _ = std::io::stderr().flush();
                    }
                }
            });
        }
    });

    let total_matches = total_matches.load(Ordering::Relaxed);
    let mut file_results = file_results.into_inner().unwrap();
    let errors = errors.into_inner().unwrap();

    if !verbose && !files.is_empty() {
        eprintln!();
    }

    if !errors.is_empty() {
        eprintln!("Errors:");
        for (rel, err) in &errors {
            if rel.is_empty() {
                eprintln!("  {err}");
            } else {
                eprintln!("  {rel}: {err}");
            }
        }
    }

    file_results.sort_by(|a, b| a.0.cmp(&b.0));
    for (rel, matches) in &file_results {
        println!("./{rel}");
        for m in matches {
            let (old_url, old_desc) = split_url_descriptor(&m.url);
            let (new_url, new_desc) = split_url_descriptor(&m.new_url);
            let old_resolved = crate::rewriter::resolve_html_url(rel, old_url);
            let new_resolved = crate::rewriter::resolve_html_url(rel, new_url);
            if old_desc.is_empty() {
                println!(
                    "  {} {}: {} → {}",
                    m.tag, m.attr, old_resolved, new_resolved
                );
            } else {
                println!(
                    "  {} {}: {} {} → {} {}",
                    m.tag, m.attr, old_resolved, old_desc, new_resolved, new_desc
                );
            }
        }
        println!();
    }

    if apply {
        eprintln!(
            "Rewrote {} URL(s) in {} file(s).",
            total_matches,
            file_results.len()
        );
    } else {
        eprintln!(
            "Dry-run: found {} URL(s) to convert in {} file(s). Run with --apply to rewrite.",
            total_matches,
            file_results.len()
        );
    }

    Ok(())
}

/// Find an available path in the trash by appending a numeric suffix if needed.
fn unique_trash_path(path: &Path) -> std::path::PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let parent = path.parent().unwrap_or(Path::new(""));
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    for n in 1..1000 {
        let candidate = if ext.is_empty() {
            parent.join(format!("{stem}.{n}"))
        } else {
            parent.join(format!("{stem}.{n}.{ext}"))
        };
        if !candidate.exists() {
            return candidate;
        }
    }
    // Fallback: use a random suffix.
    parent.join(format!(
        "{stem}.{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ))
}

fn cmd_translate(args: Args) -> Result<(), String> {
    let root = args.root.as_deref().unwrap_or(".");
    let apply = args.apply;
    let verbose = args.verbose;
    let to_lang = &args.to_lang;
    let from_lang = args.from_lang.as_deref();

    let default_include = vec!["*.html".to_string(), "*.htm".to_string()];
    let include: &[String] = if args.include.is_empty() {
        &default_include
    } else {
        &args.include
    };

    eprintln!("Discovering HTML files in {root}...");
    let files = iter_html_files(root, include, &args.exclude);

    if verbose {
        eprintln!("Found {} HTML file(s) to process", files.len());
    }
    if files.is_empty() {
        eprintln!("No HTML files found.");
        return Ok(());
    }

    if apply {
        eprintln!(
            "Translating {} file(s) to {to_lang} sequentially...",
            files.len()
        );
    } else {
        eprintln!(
            "Dry-run: scanning {} file(s) for translatable text to {to_lang}...",
            files.len()
        );
    }

    let root_path = std::path::Path::new(root);
    let mut total_segments = 0usize;
    let mut total_translated = 0usize;
    let mut file_count = 0usize;
    let mut errors = Vec::new();

    for rel in &files {
        let path = root_path.join(rel);
        match crate::translate::process_file(&path, from_lang, to_lang, apply, verbose) {
            Ok(result) => {
                total_segments += result.total_segments;
                total_translated += result.translated_segments;
                file_count += 1;

                if result.total_segments > 0 {
                    if verbose {
                        println!("{}", result.path);
                        for cluster in &result.clusters {
                            println!("  {}: {} segment(s)", cluster.kind, cluster.count);
                        }
                        println!();
                    } else {
                        eprint!("\rProcessing: {file_count}/{} files", files.len());
                        let _ = std::io::stderr().flush();
                    }
                }
            }
            Err(e) => {
                errors.push((rel.clone(), e));
            }
        }
    }

    if !verbose && !files.is_empty() {
        eprintln!();
    }

    if !errors.is_empty() {
        eprintln!("Errors:");
        for (rel, err) in &errors {
            eprintln!("  {rel}: {err}");
        }
    }

    if apply {
        eprintln!(
            "Translated {} segment(s) in {} file(s).",
            total_translated, file_count
        );
    } else {
        eprintln!(
            "Dry-run: {} translatable segment(s) in {} file(s). Run with --apply to translate.",
            total_segments, file_count
        );
    }

    Ok(())
}

fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

pub fn run() -> i32 {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("localize: {e}");
            eprintln!(
                "Usage: localize <bundle-css|check|extract-css|minify-html|towebp|translate|zap> [ROOT] [flags]"
            );
            eprintln!("Try 'localize --help' for more information.");
            return 1;
        }
    };

    let result = match args.command.as_deref() {
        Some("check") => cmd_check(args),
        Some("bundle-css") => cmd_bundle_css(args),
        Some("extract-css") => cmd_extract_css(args),
        Some("minify-html") => cmd_minify_html(args),
        Some("zap") => cmd_zap(args),
        Some("towebp") => cmd_towebp(args),
        Some("translate") => cmd_translate(args),
        Some(cmd) => {
            eprintln!("localize: unknown command '{cmd}'");
            eprintln!(
                "Usage: localize <bundle-css|check|extract-css|minify-html|towebp|translate|zap> [ROOT] [flags]"
            );
            eprintln!("Try 'localize --help' for more information.");
            return 1;
        }
        None => {
            eprintln!(
                "localize: expected subcommand (check, extract-css, minify-html, towebp, translate, or zap)"
            );
            eprintln!(
                "Usage: localize <bundle-css|check|extract-css|minify-html|towebp|translate|zap> [ROOT] [flags]"
            );
            eprintln!("Try 'localize --help' for more information.");
            return 1;
        }
    };

    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("localize: {e}");
            1
        }
    }
}
