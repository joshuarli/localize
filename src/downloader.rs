use std::hash::Hasher;
use twox_hash::XxHash64;
use rustc_hash::{FxHashMap, FxHashSet};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/125.0.0.0 Safari/537.36";
const MAX_REDIRECTS: u32 = 10;

pub fn asset_path(url: &str, assets_dir: &str) -> String {
    let parsed = match url::Url::parse(url) {
        Ok(p) => p,
        Err(_) => {
            let mut hasher = XxHash64::with_seed(0);
            hasher.write(url.as_bytes());
            let hash = hasher.finish();
            let hash_hex = format!("{hash:016x}");
            return format!("{assets_dir}/unparsable/{hash_hex}-file");
        }
    };
    let host = parsed.host_str().unwrap_or("unknown");
    let basename = parsed
        .path_segments()
        .and_then(|mut s| s.next_back())
        .filter(|b| !b.is_empty())
        .unwrap_or("index");
    let mut hasher = XxHash64::with_seed(0);
    hasher.write(url.as_bytes());
    let hash = hasher.finish();
    let hash_hex = format!("{hash:016x}");
    let shard = &hash_hex[..2];
    let prefix = &hash_hex[..8];
    format!("{assets_dir}/{host}/{shard}/{prefix}-{basename}")
}

fn origin_from_url(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(parsed) => {
            let scheme = parsed.scheme();
            let host = parsed.host_str().unwrap_or("unknown");
            if let Some(port) = parsed.port() {
                format!("{scheme}://{host}:{port}")
            } else {
                format!("{scheme}://{host}")
            }
        }
        Err(_) => String::new(),
    }
}

fn encode_url(raw: &str) -> String {
    let raw = raw.replace(' ', "%20");
    if let Ok(parsed) = url::Url::parse(&raw) {
        return parsed.to_string();
    }
    raw
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum UrlStatus {
    Succeeded,
    Broken,
    Failed,
}

pub struct DownloadConfig<'a> {
    pub root: &'a Path,
    pub assets_dir: &'a str,
    pub timeout: u32,
    pub retries: u32,
    pub user_agent: &'a str,
    pub referer: &'a str,
    pub force: bool,
    pub verbose: bool,
    pub jobs: usize,
}

pub fn download_and_rewrite(
    file_urls: &FxHashMap<String, FxHashSet<String>>,
    cfg: &DownloadConfig<'_>,
) -> (FxHashSet<String>, FxHashSet<String>) {
    let unique_urls: FxHashSet<&str> = file_urls.values().flatten().map(|s| s.as_str()).collect();

    let mut to_download: Vec<String> = Vec::new();
    for url in &unique_urls {
        let rel = asset_path(url, cfg.assets_dir);
        if !cfg.force && cfg.root.join(&rel).is_file() {
            if cfg.verbose {
                eprintln!("  (exists) {url}");
            }
            continue;
        }
        to_download.push(url.to_string());
    }

    let status: Arc<Mutex<FxHashMap<String, UrlStatus>>> =
        Arc::new(Mutex::new(FxHashMap::default()));
    for url in &unique_urls {
        if !to_download.iter().any(|u| u == *url) {
            status
                .lock()
                .unwrap()
                .insert(url.to_string(), UrlStatus::Succeeded);
        }
    }

    let download_total = to_download.len();
    let counter = AtomicUsize::new(0);
    let workers = cfg.jobs.max(1);

    // Build a shared ureq agent with connection pooling.
    let agent: Arc<ureq::Agent> = ureq::Agent::new_with_defaults().into();

    // Phase 1: download all unique URLs in parallel.
    if download_total > 0 {
        std::thread::scope(|s| {
            let to_download: &[String] = &to_download;
            let status: &Mutex<FxHashMap<String, UrlStatus>> = &status;
            let counter: &AtomicUsize = &counter;
            let index = Arc::new(AtomicUsize::new(0));
            let ua: Arc<str> = (if cfg.user_agent.is_empty() {
                DEFAULT_USER_AGENT
            } else {
                cfg.user_agent
            })
            .into();
            let timeout = Duration::from_secs(cfg.timeout.into());
            let retries = cfg.retries;

            for _ in 0..workers.min(download_total) {
                let index = Arc::clone(&index);
                let agent = agent.clone();
                let ua = ua.clone();
                let ref_url: Arc<str> = (if cfg.referer.is_empty() {
                    String::new()
                } else {
                    cfg.referer.to_string()
                })
                .into();
                s.spawn(move || {
                    loop {
                        let i = index.fetch_add(1, Ordering::Relaxed);
                        if i >= download_total {
                            break;
                        }
                        let url = &to_download[i];
                        let referer = if ref_url.is_empty() {
                            origin_from_url(url)
                        } else {
                            ref_url.to_string()
                        };
                        let dest = cfg.root.join(asset_path(url, cfg.assets_dir));
                        let result = download_one(
                            &agent, url, &referer, &dest, timeout, retries, &ua,
                        );
                        let done = counter.fetch_add(1, Ordering::Relaxed) + 1;
                        eprint!("\r  Downloading: {done}/{download_total}");
                        let _ = std::io::stderr().flush();
                        match result {
                            Ok(()) => {
                                status
                                    .lock()
                                    .unwrap()
                                    .insert(url.clone(), UrlStatus::Succeeded);
                            }
                            Err((permanent, msg)) => {
                                let st = if permanent {
                                    UrlStatus::Broken
                                } else {
                                    UrlStatus::Failed
                                };
                                status.lock().unwrap().insert(url.clone(), st);
                                if permanent {
                                    eprintln!("\n  [BROKEN] {url}: {msg}");
                                } else {
                                    eprintln!("\n  {url}: {msg}");
                                }
                            }
                        }
                    }
                });
            }
        });
        eprintln!();
    }

    // Phase 2: rewrite HTML files in parallel.
    let rewritten = Mutex::new(FxHashSet::default());
    let broken_urls = Mutex::new(FxHashSet::default());
    let file_list: Vec<(String, FxHashSet<String>)> =
        file_urls.iter().map(|(k, v)| (k.clone(), v.clone())).collect();

    if !file_list.is_empty() {
        std::thread::scope(|s| {
            let status: &Mutex<FxHashMap<String, UrlStatus>> = &status;
            let rewritten: &Mutex<FxHashSet<String>> = &rewritten;
            let broken_urls: &Mutex<FxHashSet<String>> = &broken_urls;
            let file_list: &[(String, FxHashSet<String>)] = &file_list;
            let index = Arc::new(AtomicUsize::new(0));
            let file_count = file_list.len();

            for _ in 0..workers.min(file_count) {
                let index = Arc::clone(&index);
                s.spawn(move || {
                    loop {
                        let i = index.fetch_add(1, Ordering::Relaxed);
                        if i >= file_count {
                            break;
                        }
                        let (file_rel, urls) = &file_list[i];

                        // Check if all URLs for this file are done.
                        let (all_ok, has_broken, has_failed) = {
                            let s = status.lock().unwrap();
                            let mut ok = true;
                            let mut broken = false;
                            let mut failed = false;
                            for u in urls.iter() {
                                match s.get(u) {
                                    Some(UrlStatus::Succeeded) => {}
                                    Some(UrlStatus::Broken) => {
                                        ok = false;
                                        broken = true;
                                    }
                                    Some(UrlStatus::Failed) => {
                                        ok = false;
                                        failed = true;
                                    }
                                    None => {
                                        ok = false;
                                    }
                                }
                            }
                            (ok, broken, failed)
                        };

                        if has_failed {
                            return;
                        }

                        if all_ok || has_broken {
                            let abs = cfg.root.join(file_rel);
                            let url_map: FxHashMap<String, String> = urls
                                .iter()
                                .map(|u| (u.clone(), asset_path(u, cfg.assets_dir)))
                                .collect();
                            let file_broken: FxHashSet<String> = {
                                let s = status.lock().unwrap();
                                urls.iter()
                                    .filter(|u| s.get(*u) == Some(&UrlStatus::Broken))
                                    .cloned()
                                    .collect()
                            };
                            if !file_broken.is_empty() {
                                broken_urls.lock().unwrap().extend(file_broken.clone());
                            }
                            if cfg.verbose {
                                eprintln!("\n  rewriting {file_rel}...");
                            }
                            let content = std::fs::read_to_string(&abs).unwrap_or_default();
                            match crate::rewriter::apply_html(
                                &content,
                                &url_map,
                                &file_broken,
                                file_rel,
                            ) {
                                Ok(new_html) => {
                                    let tmp = abs.with_extension("tmp");
                                    if let Err(e) = std::fs::write(&tmp, &new_html) {
                                        eprintln!("\n  write tmp {file_rel}: {e}");
                                    } else if let Err(e) = std::fs::rename(&tmp, &abs) {
                                        eprintln!("\n  rename {file_rel}: {e}");
                                    } else {
                                        rewritten.lock().unwrap().insert(file_rel.clone());
                                    }
                                }
                                Err(e) => eprintln!("\n  rewrite {file_rel}: {e}"),
                            }
                        }
                    }
                });
            }
        });
    }

    let s = status.lock().unwrap();
    let succeeded_count = s.values().filter(|v| **v == UrlStatus::Succeeded).count();
    let broken_count = s.values().filter(|v| **v == UrlStatus::Broken).count();
    let failed_count = s.values().filter(|v| **v == UrlStatus::Failed).count();
    if broken_count > 0 || failed_count > 0 {
        eprintln!(
            "Download result: {succeeded_count} ok, {broken_count} broken (404), {failed_count} transient failures."
        );
    }
    drop(s);

    (
        rewritten.into_inner().unwrap(),
        broken_urls.into_inner().unwrap(),
    )
}

fn download_one(
    agent: &ureq::Agent,
    url: &str,
    referer: &str,
    dest: &Path,
    _timeout: Duration,
    retries: u32,
    user_agent: &str,
) -> Result<(), (bool, String)> {
    let encoded_url = encode_url(url);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| (true, format!("mkdir: {e}")))?;
    }

    let tmp = dest.with_extension(format!("tmp-{}", std::process::id()));

    for attempt in 0..=retries {
        match fetch_with_redirects(agent, &encoded_url, referer, user_agent) {
            Ok((status, body)) => {
                if status >= 200 && status < 300 {
                    if body.is_empty() {
                        let _ = std::fs::remove_file(&tmp);
                        return Err((true, "empty response body".into()));
                    }
                    if looks_like_html(&body) {
                        let _ = std::fs::remove_file(&tmp);
                        return Err((true, "response body is HTML, not a media asset".into()));
                    }
                    std::fs::write(&tmp, &body).map_err(|e| (true, format!("write: {e}")))?;
                    std::fs::rename(&tmp, dest).map_err(|e| (true, format!("rename: {e}")))?;
                    return Ok(());
                }

                if (400..500).contains(&status) && status != 429 {
                    let _ = std::fs::remove_file(&tmp);
                    return Err((true, format!("HTTP {status}")));
                }

                if attempt < retries {
                    let delay = 2u64.pow(attempt);
                    eprintln!(
                        "\n  HTTP {status} (attempt {}/{retries}), retrying in {delay}s...",
                        attempt + 1
                    );
                    std::thread::sleep(Duration::from_secs(delay));
                    continue;
                }
                let _ = std::fs::remove_file(&tmp);
                return Err((false, format!("HTTP {status} after {retries} retries")));
            }
            Err((_permanent, msg)) => {
                if attempt < retries {
                    let delay = 2u64.pow(attempt);
                    eprintln!(
                        "\n  {msg} (attempt {}/{retries}), retrying in {delay}s...",
                        attempt + 1
                    );
                    std::thread::sleep(Duration::from_secs(delay));
                    continue;
                }
                let _ = std::fs::remove_file(&tmp);
                return Err((false, format!("{msg} after {retries} retries")));
            }
        }
    }

    Err((false, "unreachable".into()))
}

fn fetch_with_redirects(
    agent: &ureq::Agent,
    url: &str,
    referer: &str,
    user_agent: &str,
) -> Result<(u16, Vec<u8>), (bool, String)> {
    let mut current_url = url.to_string();

    for _ in 0..MAX_REDIRECTS {
        let resp = agent
            .get(&current_url)
            .header("User-Agent", user_agent)
            .header("Referer", referer)
            .header("Accept", "*/*")
            .call()
            .map_err(|e| {
                let msg = e.to_string();
                let permanent = msg.contains("certificate")
                    || msg.contains("tls")
                    || msg.contains("ssl")
                    || msg.contains("bad uri")
                    || msg.contains("invalid url");
                (permanent, format!("request: {msg}"))
            })?;

        let status = resp.status();

        if status.is_redirection() {
            if let Some(location) = resp.headers().get("location") {
                current_url = location
                    .to_str()
                    .map_err(|_| (true, "invalid redirect location".to_string()))?
                    .to_string();
                continue;
            }
            return Err((true, "redirect without Location header".into()));
        }

        let body = resp
            .into_body()
            .read_to_vec()
            .map_err(|e| (false, format!("read body: {e}")))?;

        return Ok((status.as_u16(), body));
    }

    Err((true, "too many redirects".into()))
}

fn looks_like_html(data: &[u8]) -> bool {
    if data.len() < 10 {
        return false;
    }
    let head = &data[..512.min(data.len())];
    let start = match head
        .iter()
        .position(|&b| b != b' ' && b != b'\t' && b != b'\n' && b != b'\r')
    {
        Some(i) => i,
        None => return false,
    };
    let head = &head[start..];
    has_prefix_ignore_ascii_case(head, b"<!doctype html")
        || has_prefix_ignore_ascii_case(head, b"<html")
        || has_prefix_ignore_ascii_case(head, b"<title>")
        || has_prefix_ignore_ascii_case(head, b"<head>")
        || has_prefix_ignore_ascii_case(head, b"<body>")
}

fn has_prefix_ignore_ascii_case(haystack: &[u8], prefix: &[u8]) -> bool {
    haystack.len() >= prefix.len()
        && haystack[..prefix.len()].eq_ignore_ascii_case(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_asset_path_structure() {
        let url = "https://example.com/images/photo.jpg";
        let path = asset_path(url, "assets");
        assert!(path.starts_with("assets/example.com/"));
        assert!(path.ends_with("-photo.jpg"));
        assert!(!path.contains(".."));
    }

    #[test]
    fn test_asset_path_no_path() {
        let url = "https://example.com";
        let path = asset_path(url, "assets");
        assert!(path.ends_with("-index"));
    }

    #[test]
    fn test_asset_path_deterministic() {
        let a = asset_path("https://a.com/x.jpg", "assets");
        let b = asset_path("https://a.com/x.jpg", "assets");
        assert_eq!(a, b);
    }

    #[test]
    fn test_origin_from_url() {
        assert_eq!(
            origin_from_url("https://example.com/path"),
            "https://example.com"
        );
        assert_eq!(
            origin_from_url("http://example.com:8080/path"),
            "http://example.com:8080"
        );
    }

    #[test]
    fn test_encode_url_fixes_spaces() {
        let result = encode_url("https://example.com/my image.jpg");
        assert_eq!(result, "https://example.com/my%20image.jpg");
    }

    #[test]
    fn test_encode_url_preserves_valid() {
        let url = "https://example.com/path?q=1";
        assert_eq!(encode_url(url), url);
    }

    #[test]
    fn test_looks_like_html() {
        assert!(looks_like_html(b"<!doctype html><html>"));
        assert!(looks_like_html(b"  \n <html lang=en>"));
        assert!(!looks_like_html(b"\x89PNG\r\n\x1a\n"));
        assert!(!looks_like_html(b"short"));
    }
}
