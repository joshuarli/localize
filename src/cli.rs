use crate::downloader::{DownloadConfig, asset_path, download_and_rewrite};
use crate::scanner::{MediaReference, is_remote_url, scan_file};
use lexopt::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Semaphore;

struct Args {
    command: Option<String>,
    root: Option<String>,
    include: Vec<String>,
    exclude: Vec<String>,
    assets_dir: String,
    json: bool,
    verbose: bool,
    jobs: usize,
    // apply-only
    timeout: u32,
    retries: u32,
    force: bool,
    user_agent: String,
    referer: String,
    dry_run: bool,
    // zap
    zap_tag: Option<String>,
    zap_query: Option<String>,
    apply: bool,
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
            timeout: 30,
            retries: 3,
            force: false,
            user_agent: String::new(),
            referer: String::new(),
            dry_run: false,
            zap_tag: None,
            zap_query: None,
            apply: false,
        }
    }
}

fn print_help() {
    println!(
        "\
localize — maintenance toolkit for static HTML sites.

Usage: localize <command> [ROOT] [flags]

Commands:
  scan     Find remote media URLs in HTML files.
  apply    Download remote assets and rewrite HTML to use local relative paths.
  clean    Find and fix broken local links by unwrapping dead <a> tags and
           removing dead resource elements.
  zap      Remove HTML elements matching a CSS selector whose inner text
           contains a query string. Dry-run by default, --apply to remove.
  towebp   Replace .jpg/.jpeg/.png URL extensions with .webp in href, src,
           and srcset attributes. Dry-run by default, --apply to rewrite.

Common flags:
  --include <pattern>   Only process files matching glob pattern (repeatable).
  --exclude <pattern>   Skip files matching glob pattern (repeatable).
  --assets-dir <dir>    Asset directory [default: assets/external].
  --json                Output as JSON (scan, apply).
  --verbose             Verbose progress output.
  --jobs <n>            Max parallel workers [default: CPUs × 4].
  --help, -h            Print this help and exit.

Apply flags:
  --timeout <s>         Download timeout in seconds [default: 30].
  --retries <n>         Download retry count [default: 3].
  --force               Re-download even if asset already exists.
  --user-agent <str>    Custom User-Agent header.
  --referer <str>       Custom Referer header.
  --dry-run             Preview without downloading or rewriting.

Clean flags:
  --force               Apply fixes (dry-run by default).

Zap flags:
  --apply               Apply removals (dry-run by default).

Towebp flags:
  --apply               Apply rewrites (dry-run by default).

Examples:
  localize scan ~/mysite
  localize apply ~/mysite --dry-run
  localize clean ~/mysite --force
  localize zap p \"Copyright 2019\" ~/mysite --apply
  localize towebp ~/mysite --apply"
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
                return Err("expected subcommand (scan, apply, clean, zap, or towebp)".into());
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
            Long("dry-run") => {
                args.dry_run = true;
            }
            Long("apply") => {
                args.apply = true;
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

    for entry in walkdir::WalkDir::new(root) {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let full = entry.path();
        let rel = full.strip_prefix(root).unwrap_or(full);
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

async fn scan_all(
    root: &str,
    files: &[String],
    jobs: usize,
    verbose: bool,
    href_set: &FxHashSet<String>,
) -> Vec<MediaReference> {
    if files.is_empty() {
        return Vec::new();
    }

    let sem = Arc::new(Semaphore::new(jobs));
    let counter = Arc::new(AtomicUsize::new(0));
    let total = files.len();

    let root = Arc::new(root.to_string());
    let href_set = Arc::new(href_set.clone());
    let mut handles = Vec::with_capacity(files.len());
    for rel in files {
        let rel = rel.clone();
        let root = root.clone();
        let sem = sem.clone();
        let counter = counter.clone();
        let href_set = href_set.clone();
        let handle = tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            tokio::task::spawn_blocking(move || {
                let path = Path::new(root.as_str()).join(&rel);
                let content = std::fs::read_to_string(&path).unwrap_or_default();
                let result = scan_file(&rel, &content, &href_set);
                let done = counter.fetch_add(1, Ordering::Relaxed) + 1;
                if !verbose && done.is_multiple_of(16) {
                    eprint!("\rScanning: {done}/{total} files");
                    let _ = std::io::stderr().flush();
                }
                (rel.clone(), result)
            })
            .await
            .unwrap()
        });
        handles.push(handle);
    }

    let mut all_refs = Vec::with_capacity(files.len() * 4);
    for handle in handles {
        match handle.await {
            Ok((rel, result)) => {
                if let Some(err) = &result.error {
                    eprintln!("\nWARNING: {rel}: {err}");
                }
                if verbose && !result.references.is_empty() {
                    eprintln!("  {rel}: {} reference(s)", result.references.len());
                }
                all_refs.extend(result.references);
            }
            Err(e) => {
                eprintln!("\nWARNING: join error: {e}");
            }
        }
    }

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
fn dedup_broken(refs: Vec<MediaReference>) -> Vec<MediaReference> {
    let mut seen = FxHashSet::default();
    let mut out = Vec::with_capacity(refs.len());
    for r in refs {
        if r.broken {
            let resolved = resolve_relative(&r.file_path, &r.url);
            if seen.insert(resolved) {
                out.push(r);
            }
        } else {
            out.push(r);
        }
    }
    out
}

fn cmd_scan(args: Args) -> Result<(), String> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| format!("tokio: {e}"))?;

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
    let refs = rt.block_on(scan_all(root, &files, jobs, args.verbose, &href_set));

    let refs = dedup_broken(refs);

    if args.json {
        print_json(&refs);
    } else {
        print_human(&refs);
    }

    if args.verbose {
        let unique_broken = refs.iter().filter(|r| r.broken).count();
        let remote = refs.len() - unique_broken;
        eprintln!(
            "\nTotal: {} reference(s) in {} file(s) ({} unique local broken, {} remote)",
            refs.len(),
            files.len(),
            unique_broken,
            remote,
        );
    }

    Ok(())
}

fn cmd_apply(args: Args) -> Result<(), String> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| format!("tokio: {e}"))?;

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
    let dl_jobs = if args.jobs == 0 { 8 } else { args.jobs };

    eprintln!("Discovering HTML files in {root}...");
    let files = iter_html_files(root, include, &args.exclude);

    if args.verbose {
        eprintln!("Found {} HTML file(s) to process", files.len());
    }
    if files.is_empty() {
        eprintln!("No HTML files found.");
        return Ok(());
    }

    // 1. Scan.
    eprintln!("Scanning {} file(s) with {jobs} workers...", files.len());
    let empty_set = FxHashSet::default();
    let refs: Arc<[MediaReference]> = rt
        .block_on(scan_all(root, &files, jobs, args.verbose, &empty_set))
        .into();
    if refs.is_empty() {
        if args.verbose {
            eprintln!("No external references found.");
        }
        return Ok(());
    }

    // 2. Deduplicate URLs.
    let unique_urls: Vec<String> = {
        let mut seen = FxHashSet::default();
        let mut urls = Vec::new();
        for r in &*refs {
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

    // 3. Dry run: preview.
    if args.dry_run {
        println!("Would download {} asset(s):", new_urls.len());
        for u in &new_urls {
            println!("  {u} -> {}", asset_path(u, &args.assets_dir));
        }
        let mut by_file: FxHashMap<&str, Vec<&MediaReference>> = FxHashMap::default();
        for r in &*refs {
            by_file.entry(&r.file_path).or_default().push(r);
        }
        println!("\nWould rewrite {} file(s):", by_file.len());
        for f in by_file.keys() {
            println!("  {f} ({} reference(s))", by_file[f].len());
        }
        if args.json {
            print_json(&refs);
        }
        return Ok(());
    }

    // 4. Download + rewrite: files are rewritten as soon as their URLs complete.
    let file_urls: FxHashMap<String, FxHashSet<String>> = {
        let mut map: FxHashMap<String, FxHashSet<String>> = FxHashMap::default();
        for r in &*refs {
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
    let (rewritten, broken_urls) = rt.block_on(download_and_rewrite(&file_urls, &dl_cfg));

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

    // 5. Verify rewritten files.
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

    if args.json {
        print_json(&refs);
    }

    eprintln!(
        "Done. {} unique URL(s), {} file(s) rewritten, {} skipped.",
        unique_urls.len(),
        rewritten.len(),
        skipped.len()
    );

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

fn cmd_clean(args: Args) -> Result<(), String> {
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
    let force = args.force;
    let verbose = args.verbose;

    eprintln!("Discovering HTML files in {root}...");
    let files = iter_html_files(root, include, &args.exclude);

    if verbose {
        eprintln!("Found {} HTML file(s) to scan", files.len());
    }
    if files.is_empty() {
        eprintln!("No HTML files found.");
        return Ok(());
    }

    if force {
        eprintln!("Cleaning {} file(s) with {jobs} workers...", files.len());
    } else {
        eprintln!(
            "Dry-run: scanning {} file(s) with {jobs} workers...",
            files.len()
        );
    }

    let rt = tokio::runtime::Runtime::new().map_err(|e| format!("tokio: {e}"))?;
    let root_path = std::path::Path::new(root);

    eprintln!("Building file index...");
    let href_set = Arc::new(crate::clean::build_href_set(root_path));
    if verbose {
        eprintln!("Indexed {} file(s)", href_set.len());
    }

    let (total_broken, mut file_results, errors) = rt.block_on(async {
        let sem = Arc::new(tokio::sync::Semaphore::new(jobs));
        let counter = Arc::new(AtomicUsize::new(0));
        let file_total = files.len();

        let mut handles = Vec::with_capacity(files.len());
        for rel in &files {
            let rel = rel.clone();
            let sem = sem.clone();
            let counter = counter.clone();
            let root_path = root_path.to_path_buf();
            let href_set = href_set.clone();
            let handle = tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                tokio::task::spawn_blocking(move || {
                    let path = root_path.join(&rel);
                    let content = std::fs::read_to_string(&path).unwrap_or_default();
                    let scan = crate::clean::scan_file(&rel, &content, &href_set);
                    if let Some(err) = &scan.error {
                        return (rel.clone(), Err(format!("{}: {err}", path.display())));
                    }
                    if !scan.broken_links.is_empty() && force {
                        match crate::rewriter::clean_html(&content, &href_set, &rel) {
                            Ok(new_html) => {
                                let tmp = path.with_extension("tmp");
                                if let Err(e) = std::fs::write(&tmp, &new_html)
                                    .map_err(|e| format!("write tmp: {e}"))
                                    .and_then(|_| {
                                        std::fs::rename(&tmp, &path)
                                            .map_err(|e| format!("rename: {e}"))
                                    })
                                {
                                    return (rel.clone(), Err(format!("{}: {e}", path.display())));
                                }
                            }
                            Err(e) => {
                                return (rel.clone(), Err(format!("{}: {e}", path.display())));
                            }
                        }
                    }
                    let done = counter.fetch_add(1, Ordering::Relaxed) + 1;
                    if !verbose {
                        if done.is_multiple_of(16) {
                            eprint!("\rScanning: {done}/{file_total} files");
                        }
                        let _ = std::io::stderr().flush();
                    }
                    (
                        rel.clone(),
                        Ok(crate::clean::CleanResult {
                            broken_links: scan.broken_links,
                            error: scan.error,
                        }),
                    )
                })
                .await
                .unwrap()
            });
            handles.push(handle);
        }

        let mut total_broken = 0usize;
        let mut errors = Vec::new();
        let mut file_results: Vec<(String, Vec<crate::clean::BrokenLink>)> = Vec::new();

        for handle in handles {
            match handle.await {
                Ok((rel, Ok(result))) => {
                    if !result.broken_links.is_empty() {
                        total_broken += result.broken_links.len();
                        file_results.push((rel, result.broken_links));
                    }
                }
                Ok((rel, Err(e))) => {
                    errors.push((rel, e));
                }
                Err(e) => {
                    errors.push((String::new(), format!("join error: {e}")));
                }
            }
        }

        (total_broken, file_results, errors)
    });

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

    // Print per-file broken links in hyperlink's format.
    file_results.sort_by(|a, b| a.0.cmp(&b.0));
    for (rel, links) in &file_results {
        println!("./{rel}");
        for link in links {
            println!("  <{} {}=\"{}\">", link.tag, link.attr, link.url);
        }
        println!();
    }

    if force {
        eprintln!(
            "Cleaned {} broken link(s) in {} file(s).",
            total_broken,
            file_results.len()
        );
    } else {
        eprintln!(
            "Dry-run: found {} broken link(s) in {} file(s). Run with --force to fix.",
            total_broken,
            file_results.len()
        );
    }

    Ok(())
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

    let rt = tokio::runtime::Runtime::new().map_err(|e| format!("tokio: {e}"))?;
    let root_path = std::path::Path::new(root);

    let (total_matches, mut file_results, errors) = rt.block_on(async {
        let sem = Arc::new(Semaphore::new(jobs));
        let counter = Arc::new(AtomicUsize::new(0));
        let file_total = files.len();

        let mut handles = Vec::with_capacity(files.len());
        for rel in &files {
            let rel = rel.clone();
            let sem = sem.clone();
            let counter = counter.clone();
            let root_path = root_path.to_path_buf();
            let selector = selector.clone();
            let query = query.to_string();
            let handle = tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                tokio::task::spawn_blocking(move || {
                    let path = root_path.join(&rel);
                    let content = std::fs::read_to_string(&path).unwrap_or_default();
                    let (result, modified) = if apply {
                        match crate::rewriter::zap_html(&content, &selector, &query) {
                            Ok((html, matches)) => (
                                crate::zap::ZapResult {
                                    matches,
                                    error: None,
                                },
                                Some(html),
                            ),
                            Err(e) => {
                                return (rel.clone(), Err(e));
                            }
                        }
                    } else {
                        let result = crate::zap::scan_html(&content, &selector, &query);
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
                            return (rel.clone(), Err(format!("{}: {e}", path.display())));
                        }
                    }
                    let done = counter.fetch_add(1, Ordering::Relaxed) + 1;
                    if !verbose {
                        if done.is_multiple_of(16) {
                            eprint!("\rScanning: {done}/{file_total} files");
                        }
                        let _ = std::io::stderr().flush();
                    }
                    (rel.clone(), Ok(result))
                })
                .await
                .unwrap()
            });
            handles.push(handle);
        }

        let mut total_matches = 0usize;
        let mut errors = Vec::new();
        let mut file_results: Vec<(String, Vec<crate::zap::ZapMatch>)> = Vec::new();

        for handle in handles {
            match handle.await {
                Ok((rel, Ok(result))) => {
                    if !result.matches.is_empty() {
                        total_matches += result.matches.len();
                        file_results.push((rel, result.matches));
                    }
                }
                Ok((rel, Err(e))) => {
                    errors.push((rel, e));
                }
                Err(e) => {
                    errors.push((String::new(), format!("join error: {e}")));
                }
            }
        }

        (total_matches, file_results, errors)
    });

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
const PER_IMAGE_MEMORY_ESTIMATE: u64 = 20 * 1024 * 1024;

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
                if let Some(rest) = line.strip_prefix("Pages free:")
                    .or_else(|| line.strip_prefix("Pages inactive:"))
                {
                    let val: u64 = rest
                        .trim_end_matches('.')
                        .trim()
                        .parse()
                        .unwrap_or(0);
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
                    let kb: u64 = rest
                        .trim_end_matches(" kB")
                        .trim()
                        .parse()
                        .unwrap_or(0);
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
    let max_jobs = ((available / 2) / PER_IMAGE_MEMORY_ESTIMATE) as usize;
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
        num_cpus() * 4
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

    let rt = tokio::runtime::Runtime::new().map_err(|e| format!("tokio: {e}"))?;
    let root_path = std::path::Path::new(root);

    // Phase 1: collect unique images and convert them (only when --apply).
    let converted: FxHashSet<String> = if apply {
        let mut unique: FxHashSet<String> = FxHashSet::default();
        // Keyed by resolved filesystem path so the same image referenced from
        // multiple HTML files is deduplicated.

        // Scan all HTML files to collect image references.
        {
            let sem = Arc::new(Semaphore::new(jobs));
            let counter = Arc::new(AtomicUsize::new(0));
            let file_total = files.len();
            let unique_mu = std::sync::Mutex::new(&mut unique);

            rt.block_on(async {
                let mut handles = Vec::with_capacity(files.len());
                for rel in &files {
                    let rel = rel.clone();
                    let sem = sem.clone();
                    let root_path = root_path.to_path_buf();
                    let handle = tokio::spawn(async move {
                        let _permit = sem.acquire().await.unwrap();
                        tokio::task::spawn_blocking(move || {
                            let path = root_path.join(&rel);
                            let content = std::fs::read_to_string(&path).unwrap_or_default();
                            let matches = crate::towebp::scan_towebp(&content);
                            (rel, matches)
                        })
                        .await
                        .unwrap()
                    });
                    handles.push(handle);
                }

                for handle in handles {
                    match handle.await {
                        Ok((rel, matches)) => {
                            for m in &matches {
                                let (url, _desc) = split_url_descriptor(&m.url);
                                let resolved =
                                    crate::rewriter::resolve_html_url(&rel, url);
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
                        Err(e) => {
                            eprintln!("warning: scan task panicked: {e}");
                        }
                    }
                }
            });
            drop(unique_mu);
            if !verbose && !files.is_empty() {
                eprintln!();
            }
        }

        let unique_images: Vec<String> = unique.into_iter().collect();
        if verbose {
            eprintln!("Found {} unique image(s) to convert", unique_images.len());
        }

        // Convert images in parallel, bounded by the semaphore to avoid OOM.
        // Each worker holds at most one decoded image in memory at a time.
        let mut converted = FxHashSet::default();
        let trash_root = root_path.join(".trash");
        let converted_mu = std::sync::Mutex::new(&mut converted);
        let counter = Arc::new(AtomicUsize::new(0));
        let convert_total = unique_images.len();

        let (converted_count, failed_count) = rt.block_on(async {
            let sem = Arc::new(Semaphore::new(jobs));
            let mut handles = Vec::with_capacity(unique_images.len());

            for resolved in &unique_images {
                let resolved = resolved.clone();
                let sem = sem.clone();
                let counter = counter.clone();
                let root_path = root_path.to_path_buf();
                let trash_root = trash_root.clone();
                let handle = tokio::spawn(async move {
                    let _permit = sem.acquire().await.unwrap();
                    tokio::task::spawn_blocking(move || {
                        let abs_path = root_path.join(&resolved);

                        // Skip remote URLs, data URIs, and non-existent files.
                        if resolved.starts_with("http://")
                            || resolved.starts_with("https://")
                            || resolved.starts_with("data:")
                            || !abs_path.exists()
                        {
                            if verbose
                                && !abs_path.exists()
                                && !resolved.starts_with("http")
                                && !resolved.starts_with("data:")
                            {
                                let _ = writeln!(
                                    std::io::stderr(),
                                    "  skipping {resolved}: file not found"
                                );
                            }
                            let done = counter.fetch_add(1, Ordering::Relaxed) + 1;
                            if !verbose && done.is_multiple_of(16) {
                                eprint!("\rConverting: {done}/{convert_total} images");
                                let _ = std::io::stderr().flush();
                            }
                            return None;
                        }

                        if verbose {
                            let _ = write!(std::io::stderr(), "  converting {resolved} ... ");
                        }
                        match crate::webp_encode::convert_to_webp(&abs_path) {
                            Ok(webp_bytes) => {
                                let webp_path = abs_path.with_extension("webp");
                                if let Err(e) = std::fs::write(&webp_path, &webp_bytes) {
                                    if verbose {
                                        let _ =
                                            writeln!(std::io::stderr(), "FAILED (write webp: {e})");
                                    }
                                    let done = counter.fetch_add(1, Ordering::Relaxed) + 1;
                                    if !verbose && done.is_multiple_of(16) {
                                        eprint!("\rConverting: {done}/{convert_total} images");
                                        let _ = std::io::stderr().flush();
                                    }
                                    return Some((resolved, false));
                                }
                                // Move original to .trash/.
                                let trash_path = trash_root.join(&resolved);
                                if let Some(parent) = trash_path.parent() {
                                    if let Err(e) = std::fs::create_dir_all(parent) {
                                        if verbose {
                                            let _ = writeln!(
                                                std::io::stderr(),
                                                "FAILED (create trash dir: {e})"
                                            );
                                        }
                                        let _ = std::fs::remove_file(&webp_path);
                                        let done = counter.fetch_add(1, Ordering::Relaxed) + 1;
                                        if !verbose && done.is_multiple_of(16) {
                                            eprint!("\rConverting: {done}/{convert_total} images");
                                            let _ = std::io::stderr().flush();
                                        }
                                        return Some((resolved, false));
                                    }
                                }
                                let trash_path = unique_trash_path(&trash_path);
                                if let Err(e) = std::fs::rename(&abs_path, &trash_path) {
                                    if verbose {
                                        let _ =
                                            writeln!(std::io::stderr(), "FAILED (move to trash: {e})");
                                    }
                                    let _ = std::fs::remove_file(&webp_path);
                                    let done = counter.fetch_add(1, Ordering::Relaxed) + 1;
                                    if !verbose && done.is_multiple_of(16) {
                                        eprint!("\rConverting: {done}/{convert_total} images");
                                        let _ = std::io::stderr().flush();
                                    }
                                    return Some((resolved, false));
                                }
                                if verbose {
                                    let _ = writeln!(std::io::stderr(), "OK");
                                }
                                let done = counter.fetch_add(1, Ordering::Relaxed) + 1;
                                if !verbose && done.is_multiple_of(16) {
                                    eprint!("\rConverting: {done}/{convert_total} images");
                                    let _ = std::io::stderr().flush();
                                }
                                Some((resolved, true))
                            }
                            Err(e) => {
                                if verbose {
                                    let _ = writeln!(std::io::stderr(), "FAILED ({e})");
                                }
                                let done = counter.fetch_add(1, Ordering::Relaxed) + 1;
                                if !verbose && done.is_multiple_of(16) {
                                    eprint!("\rConverting: {done}/{convert_total} images");
                                    let _ = std::io::stderr().flush();
                                }
                                Some((resolved, false))
                            }
                        }
                    })
                    .await
                    .unwrap()
                });
                handles.push(handle);
            }

            let mut converted_count = 0usize;
            let mut failed_count = 0usize;
            for handle in handles {
                if let Ok(Some((resolved, ok))) = handle.await {
                    if ok {
                        if let Ok(mut guard) = converted_mu.lock() {
                            guard.insert(resolved);
                        }
                        converted_count += 1;
                    } else {
                        failed_count += 1;
                    }
                }
            }
            (converted_count, failed_count)
        });
        drop(converted_mu);

        if !verbose && convert_total > 0 {
            eprintln!();
        }

        if converted_count > 0 || failed_count > 0 {
            eprintln!(
                "Converted {converted_count} image(s) to webp{}.",
                if failed_count > 0 {
                    format!(" ({failed_count} failed)")
                } else {
                    String::new()
                }
            );
        }
        if converted.is_empty() {
            eprintln!("No images converted; HTML will not be modified.");
        }

        converted
    } else {
        FxHashSet::default()
    };

    // Phase 2: rewite HTML files (gated on `converted`).
    let (total_matches, mut file_results, errors) = rt.block_on(async {
        let sem = Arc::new(Semaphore::new(jobs));
        let counter = Arc::new(AtomicUsize::new(0));
        let file_total = files.len();
        let converted = Arc::new(converted);
        let apply_flag = apply;

        let mut handles = Vec::with_capacity(files.len());
        for rel in &files {
            let rel = rel.clone();
            let sem = sem.clone();
            let counter = counter.clone();
            let root_path = root_path.to_path_buf();
            let converted = converted.clone();
            let handle = tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                tokio::task::spawn_blocking(move || {
                    let path = root_path.join(&rel);
                    let content = std::fs::read_to_string(&path).unwrap_or_default();
                    let all_matches = crate::towebp::scan_towebp(&content);
                    // Filter matches to those whose resolved path was converted.
                    let matches: Vec<crate::towebp::WebpMatch> = if apply_flag {
                        all_matches
                            .into_iter()
                            .filter(|m| {
                                let (url, _desc) = split_url_descriptor(&m.url);
                                let resolved =
                                    crate::rewriter::resolve_html_url(&rel, url);
                                converted.contains(&resolved)
                            })
                            .collect()
                    } else {
                        all_matches
                    };
                    if !matches.is_empty() && apply_flag {
                        match crate::rewriter::towebp_html(
                            &content,
                            &rel,
                            &converted,
                        ) {
                            Ok(new_html) => {
                                let tmp = path.with_extension("tmp");
                                if let Err(e) = std::fs::write(&tmp, &new_html)
                                    .map_err(|e| format!("write tmp: {e}"))
                                    .and_then(|_| {
                                        std::fs::rename(&tmp, &path)
                                            .map_err(|e| format!("rename: {e}"))
                                    })
                                {
                                    return (rel.clone(), Err(format!("{}: {e}", path.display())));
                                }
                            }
                            Err(e) => {
                                return (rel.clone(), Err(format!("{}: {e}", path.display())));
                            }
                        }
                    }
                    let done = counter.fetch_add(1, Ordering::Relaxed) + 1;
                    if !verbose {
                        if done.is_multiple_of(16) {
                            eprint!("\rProcessing: {done}/{file_total} files");
                        }
                        let _ = std::io::stderr().flush();
                    }
                    (rel.clone(), Ok(matches))
                })
                .await
                .unwrap()
            });
            handles.push(handle);
        }

        let mut total_matches = 0usize;
        let mut errors = Vec::new();
        let mut file_results: Vec<(String, Vec<crate::towebp::WebpMatch>)> = Vec::new();

        for handle in handles {
            match handle.await {
                Ok((rel, Ok(matches))) => {
                    if !matches.is_empty() {
                        total_matches += matches.len();
                        file_results.push((rel, matches));
                    }
                }
                Ok((rel, Err(e))) => {
                    errors.push((rel, e));
                }
                Err(e) => {
                    errors.push((String::new(), format!("join error: {e}")));
                }
            }
        }

        (total_matches, file_results, errors)
    });

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
                println!("  {} {}: {} → {}", m.tag, m.attr, old_resolved, new_resolved);
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
            eprintln!("Usage: localize <scan|apply|clean|zap|towebp> [ROOT] [flags]");
            eprintln!("Try 'localize --help' for more information.");
            return 1;
        }
    };

    let result = match args.command.as_deref() {
        Some("scan") => cmd_scan(args),
        Some("apply") => cmd_apply(args),
        Some("clean") => cmd_clean(args),
        Some("zap") => cmd_zap(args),
        Some("towebp") => cmd_towebp(args),
        Some(cmd) => {
            eprintln!("localize: unknown command '{cmd}'");
            eprintln!("Usage: localize <scan|apply|clean|zap|towebp> [ROOT] [flags]");
            eprintln!("Try 'localize --help' for more information.");
            return 1;
        }
        None => {
            eprintln!("localize: expected subcommand (scan, apply, clean, zap, or towebp)");
            eprintln!("Usage: localize <scan|apply|clean|zap|towebp> [ROOT] [flags]");
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
