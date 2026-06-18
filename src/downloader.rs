use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::{Request, StatusCode};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use ring::digest::{SHA256, digest};
use rustc_hash::{FxHashMap, FxHashSet};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Notify, Semaphore};

const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/125.0.0.0 Safari/537.36";
const MAX_REDIRECTS: u32 = 10;

pub fn asset_path(url: &str, assets_dir: &str) -> String {
    let parsed = url::Url::parse(url).unwrap();
    let host = parsed.host_str().unwrap_or("unknown");
    let basename = parsed
        .path_segments()
        .and_then(|mut s| s.next_back())
        .filter(|b| !b.is_empty())
        .unwrap_or("index");
    let hash = digest(&SHA256, url.as_bytes());
    let hash_hex: String = hash.as_ref().iter().map(|b| format!("{b:02x}")).collect();
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

/// Percent-encode spaces and other invalid chars in a URL so hyper can parse it.
fn encode_url(raw: &str) -> String {
    let raw = raw.replace(' ', "%20");
    if let Ok(parsed) = url::Url::parse(&raw) {
        return parsed.to_string();
    }
    raw
}

/// Outcome of attempting to download a single URL.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum UrlStatus {
    Succeeded,
    /// Permanent failure (404, 410) — attribute will be renamed to data-broken-*.
    Broken,
    /// Transient failure (timeout, 5xx) — file will be skipped for later retry.
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

/// Download unique URLs and rewrite files in real-time as their URLs complete.
///
/// Returns (rewritten_files, broken_urls).
pub async fn download_and_rewrite(
    file_urls: &FxHashMap<String, FxHashSet<String>>,
    all_refs: Arc<[crate::scanner::MediaReference]>,
    cfg: &DownloadConfig<'_>,
) -> (FxHashSet<String>, FxHashSet<String>) {
    // Build the set of all unique URLs.
    let unique_urls: FxHashSet<&str> = file_urls.values().flatten().map(|s| s.as_str()).collect();

    // Check which assets already exist.
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

    let download_total = to_download.len();

    // Shared state: URL -> status.
    let status: Arc<Mutex<FxHashMap<String, UrlStatus>>> =
        Arc::new(Mutex::new(FxHashMap::default()));
    // Mark already-existing URLs as succeeded.
    for url in &unique_urls {
        if !to_download.iter().any(|u| u == *url) {
            status
                .lock()
                .unwrap()
                .insert(url.to_string(), UrlStatus::Succeeded);
        }
    }
    let notify = Arc::new(Notify::new());
    let counter = Arc::new(AtomicUsize::new(0));

    // Build hyper client.
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();
    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_webpki_roots()
        .https_or_http()
        .enable_http1()
        .build();
    let client: Client<_, http_body_util::Full<Bytes>> = Client::builder(TokioExecutor::new())
        .pool_max_idle_per_host(10)
        .pool_idle_timeout(Duration::from_secs(60))
        .build(https);

    let dl_sem = Arc::new(Semaphore::new(cfg.jobs));
    let ua: Arc<str> = (if cfg.user_agent.is_empty() {
        DEFAULT_USER_AGENT
    } else {
        cfg.user_agent
    })
    .into();
    let timeout_dur = Duration::from_secs(cfg.timeout.into());
    let retries = cfg.retries;

    // Spawn download tasks.
    let mut dl_handles = Vec::with_capacity(to_download.len());
    for url in &to_download {
        let url = url.clone();
        let ref_url: Arc<str> = (if cfg.referer.is_empty() {
            origin_from_url(&url)
        } else {
            cfg.referer.to_string()
        })
        .into();
        let ua = ua.clone();
        let status = status.clone();
        let notify = notify.clone();
        let counter = counter.clone();
        let sem = dl_sem.clone();
        let client = client.clone();
        let dest = cfg.root.join(asset_path(&url, cfg.assets_dir));

        dl_handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let result =
                download_one(&client, &url, &ref_url, &dest, timeout_dur, retries, &ua).await;
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
            notify.notify_waiters();
        }));
    }

    // Spawn per-file rewriter tasks.
    let rewrite_sem = Arc::new(Semaphore::new(cfg.jobs.max(1)));
    let rewritten = Arc::new(Mutex::new(FxHashSet::default()));
    let broken_urls = Arc::new(Mutex::new(FxHashSet::default()));
    let mut rw_handles = Vec::with_capacity(file_urls.len());

    for (file_rel, urls) in file_urls {
        let urls: Arc<FxHashSet<String>> = Arc::new(urls.clone());
        let status = status.clone();
        let notify = notify.clone();
        let rewritten = rewritten.clone();
        let broken_urls = broken_urls.clone();
        let sem = rewrite_sem.clone();
        let all_refs = Arc::clone(&all_refs);
        let root = cfg.root.to_path_buf();
        let file_rel = file_rel.clone();
        let assets_dir = cfg.assets_dir.to_string();
        let verbose = cfg.verbose;

        rw_handles.push(tokio::spawn(async move {
            loop {
                let all_done = {
                    let s = status.lock().unwrap();
                    urls.iter().all(|u| s.contains_key(u))
                };
                if all_done {
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
                        // Transient failures — skip this file entirely.
                        return;
                    }

                    if all_ok || has_broken {
                        let _permit = sem.acquire().await.unwrap();
                        let abs = root.join(&file_rel);
                        let file_refs: Vec<&crate::scanner::MediaReference> = all_refs
                            .iter()
                            .filter(|r| r.file_path == file_rel)
                            .collect();
                        let url_map: FxHashMap<String, String> = urls
                            .iter()
                            .map(|u| (u.clone(), asset_path(u, &assets_dir)))
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
                        if verbose {
                            eprintln!(
                                "\n  rewriting {file_rel} ({} reference(s))",
                                file_refs.len()
                            );
                        }
                        if let Err(e) =
                            crate::rewriter::rewrite_file(&abs, &file_refs, &url_map, &file_broken)
                        {
                            eprintln!("\n  rewrite {file_rel}: {e}");
                        } else {
                            rewritten.lock().unwrap().insert(file_rel.clone());
                        }
                    }
                    return;
                }
                notify.notified().await;
            }
        }));
    }

    // Wait for all downloads.
    for h in dl_handles {
        let _ = h.await;
    }
    if download_total > 0 {
        eprintln!();
    }
    // Wait for all rewriters.
    for h in rw_handles {
        let _ = h.await;
    }

    // Report.
    let s = status.lock().unwrap();
    let succeeded_count = s.values().filter(|v| **v == UrlStatus::Succeeded).count();
    let broken_count = s.values().filter(|v| **v == UrlStatus::Broken).count();
    let failed_count = s.values().filter(|v| **v == UrlStatus::Failed).count();
    if broken_count > 0 || failed_count > 0 {
        eprintln!(
            "Download result: {succeeded_count} ok, {broken_count} broken (404), {failed_count} transient failures."
        );
    }

    (
        Arc::try_unwrap(rewritten).unwrap().into_inner().unwrap(),
        Arc::try_unwrap(broken_urls).unwrap().into_inner().unwrap(),
    )
}

async fn download_one(
    client: &Client<
        HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
        http_body_util::Full<Bytes>,
    >,
    url: &str,
    referer: &str,
    dest: &Path,
    timeout: Duration,
    retries: u32,
    user_agent: &str,
) -> Result<(), (bool, String)> {
    let encoded_url = encode_url(url); // fix spaces and other unencoded chars
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| (true, format!("mkdir: {e}")))?;
    }

    let tmp = dest.with_extension(format!("tmp-{}", std::process::id()));

    for attempt in 0..=retries {
        match fetch_with_redirects(client, &encoded_url, referer, user_agent, timeout).await {
            Ok((status, body)) => {
                if status.is_success() {
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

                if status.is_client_error() && status != StatusCode::TOO_MANY_REQUESTS {
                    let _ = std::fs::remove_file(&tmp);
                    return Err((true, format!("HTTP {status}")));
                }

                if attempt < retries {
                    let delay = 2u64.pow(attempt);
                    eprintln!(
                        "\n  HTTP {status} (attempt {}/{retries}), retrying in {delay}s...",
                        attempt + 1
                    );
                    tokio::time::sleep(Duration::from_secs(delay)).await;
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
                    tokio::time::sleep(Duration::from_secs(delay)).await;
                    continue;
                }
                let _ = std::fs::remove_file(&tmp);
                return Err((false, format!("{msg} after {retries} retries")));
            }
        }
    }

    Err((false, "unreachable".into()))
}

async fn fetch_with_redirects(
    client: &Client<
        HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
        http_body_util::Full<Bytes>,
    >,
    url: &str,
    referer: &str,
    user_agent: &str,
    timeout: Duration,
) -> Result<(StatusCode, Bytes), (bool, String)> {
    let mut current_url = url.to_string();

    for _ in 0..MAX_REDIRECTS {
        let req = Request::builder()
            .uri(&current_url)
            .header("User-Agent", user_agent)
            .header("Referer", referer)
            .header("Accept", "*/*")
            .body(http_body_util::Full::new(Bytes::new()))
            .map_err(|e| (true, format!("bad request: {e}")))?;

        let fut = client.request(req);
        let resp = tokio::time::timeout(timeout, fut)
            .await
            .map_err(|_| (false, "timeout".to_string()))?
            .map_err(|e| (false, format!("request: {e}")))?;

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
            .collect()
            .await
            .map_err(|e| (false, format!("read body: {e}")))?
            .to_bytes();

        return Ok((status, body));
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
        && haystack
            .iter()
            .zip(prefix)
            .all(|(h, p)| h.eq_ignore_ascii_case(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_url_preserves_valid() {
        let url = "https://example.com/path/file.jpg";
        assert_eq!(encode_url(url), url);
    }

    #[test]
    fn test_encode_url_fixes_spaces() {
        let url = "https://example.com/path/12.34 pm.png";
        let encoded = encode_url(url);
        assert_eq!(encoded, "https://example.com/path/12.34%20pm.png");
    }

    #[test]
    fn test_asset_path_deterministic() {
        let p1 = asset_path("https://cdn.example.com/img/logo.png", "assets/external");
        let p2 = asset_path("https://cdn.example.com/img/logo.png", "assets/external");
        assert_eq!(p1, p2);
    }

    #[test]
    fn test_asset_path_structure() {
        let path = asset_path("https://cdn.example.com/img/logo.png", "assets/external");
        assert!(path.starts_with("assets/external/cdn.example.com/"));
        assert!(path.ends_with("-logo.png"));
    }

    #[test]
    fn test_asset_path_no_path() {
        let path = asset_path("https://example.com", "assets");
        assert!(path.contains("index"));
    }

    #[test]
    fn test_origin_from_url() {
        assert_eq!(
            origin_from_url("https://s3-us-west-2.amazonaws.com/bucket/file.jpg"),
            "https://s3-us-west-2.amazonaws.com"
        );
    }

    #[test]
    fn test_looks_like_html() {
        assert!(looks_like_html(b"<!DOCTYPE html><html>..."));
        assert!(!looks_like_html(b"\x89PNG\r\n\x1a\nfake png"));
    }
}
