use crate::downloader::{DownloadConfig, asset_path, download_and_rewrite};
use crate::rewriter::compute_relative_path;
use crate::scanner::{MediaReference, scan_file};
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
        }
    }
}

fn parse_args() -> Result<Args, lexopt::Error> {
    let mut args = Args::default();
    let mut parser = lexopt::Parser::from_env();

    // First positional arg is the subcommand.
    if let Some(arg) = parser.next()? {
        match arg {
            Value(val) => {
                args.command = Some(val.string()?);
            }
            _ => {
                return Err("expected subcommand (scan or apply)".into());
            }
        }
    }

    // Second positional arg is the root.
    if let Some(arg) = parser.next()? {
        match arg {
            Value(val) => {
                args.root = Some(val.string()?);
            }
            _ => {
                return Err("expected root directory".into());
            }
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
            Long(unknown) => {
                return Err(format!("unknown flag --{unknown}").into());
            }
            Short(_) | Value(_) => {
                return Err("unexpected argument".into());
            }
        }
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

        // Check exclude patterns first.
        if exclude_pats.iter().any(|p| p.matches(&rel_str)) {
            continue;
        }
        // Check include patterns.
        if include_pats.iter().any(|p| p.matches(&rel_str)) {
            matches.push(rel_str.to_string());
        }
    }

    matches.sort();
    matches
}

async fn scan_all(root: &str, files: &[String], jobs: usize, verbose: bool) -> Vec<MediaReference> {
    if files.is_empty() {
        return Vec::new();
    }

    let sem = Arc::new(Semaphore::new(jobs));
    let counter = Arc::new(AtomicUsize::new(0));
    let total = files.len();

    let root = Arc::new(root.to_string());
    let mut handles = Vec::with_capacity(files.len());
    for rel in files {
        let rel = rel.clone();
        let root = root.clone();
        let sem = sem.clone();
        let counter = counter.clone();
        let handle = tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            tokio::task::spawn_blocking(move || {
                let path = Path::new(root.as_str()).join(&rel);
                let content = std::fs::read_to_string(&path).unwrap_or_default();
                let result = scan_file(&rel, &content);
                let done = counter.fetch_add(1, Ordering::Relaxed) + 1;
                if !verbose {
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

    let mut all_refs = Vec::new();
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

fn print_human(refs: &[MediaReference], assets_dir: &str) {
    let mut current_file: Option<&str> = None;
    for r in refs {
        if current_file != Some(&r.file_path) {
            if current_file.is_some() {
                println!();
            }
            current_file = Some(&r.file_path);
        }

        let local = asset_path(&r.url, assets_dir);
        println!("{}", r.file_path);
        println!("  type: {}", r.tag);
        if r.attr != r.tag {
            println!("  attr: {}", r.attr);
        }
        println!("  url: {}", r.url);
        println!("  local: {local}");
        if let Some(desc) = &r.descriptor {
            println!("  descriptor: {desc}");
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

fn print_json(refs: &[MediaReference], assets_dir: &str) {
    print!("[");
    for (i, r) in refs.iter().enumerate() {
        if i > 0 {
            print!(",");
        }
        let local_rel = asset_path(&r.url, assets_dir);
        let replacement = compute_relative_path(&r.file_path, &local_rel);
        print!(
            "\n  {{\"file\":{},\"url\":{},\"replacement\":{},\"tag\":{},\"attr\":{}",
            json_escape(&r.file_path),
            json_escape(&r.url),
            json_escape(&replacement),
            json_escape(&r.tag),
            json_escape(&r.attr),
        );
        if let Some(d) = &r.descriptor {
            print!(",\"descriptor\":{}", json_escape(d));
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

    eprintln!("Discovering HTML files in {root}...");
    let files = iter_html_files(root, include, &args.exclude);

    if args.verbose {
        eprintln!("Found {} HTML file(s) to scan", files.len());
    }
    if files.is_empty() {
        eprintln!("No HTML files found.");
        return Ok(());
    }

    eprintln!("Scanning {} file(s) with {jobs} workers...", files.len());
    let refs = rt.block_on(scan_all(root, &files, jobs, args.verbose));

    if args.json {
        print_json(&refs, &args.assets_dir);
    } else {
        print_human(&refs, &args.assets_dir);
    }

    if args.verbose {
        eprintln!(
            "\nTotal: {} external reference(s) in {} file(s)",
            refs.len(),
            files.len()
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
    let refs: Arc<[MediaReference]> = rt
        .block_on(scan_all(root, &files, jobs, args.verbose))
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
                urls.push(r.url.clone());
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
            print_json(&refs, &args.assets_dir);
        }
        return Ok(());
    }

    // 4. Download + rewrite: files are rewritten as soon as their URLs complete.
    let file_urls: FxHashMap<String, FxHashSet<String>> = {
        let mut map: FxHashMap<String, FxHashSet<String>> = FxHashMap::default();
        for r in &*refs {
            map.entry(r.file_path.clone())
                .or_default()
                .insert(r.url.clone());
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
    let (rewritten, broken_urls) =
        rt.block_on(download_and_rewrite(&file_urls, Arc::clone(&refs), &dl_cfg));

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
        print_json(&refs, &args.assets_dir);
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
        let result = scan_file(rel, &content);
        if !result.references.is_empty() {
            stray.push(rel.clone());
        }
    }
    stray
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
            eprintln!("Usage: localize <scan|apply> <ROOT> [flags]");
            return 1;
        }
    };

    let result = match args.command.as_deref() {
        Some("scan") => cmd_scan(args),
        Some("apply") => cmd_apply(args),
        Some(cmd) => {
            eprintln!("localize: unknown command '{cmd}'");
            eprintln!("Usage: localize <scan|apply> <ROOT> [flags]");
            return 1;
        }
        None => {
            eprintln!("localize: expected subcommand (scan or apply)");
            eprintln!("Usage: localize <scan|apply> <ROOT> [flags]");
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
