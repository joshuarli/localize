//! HTML text extraction, clustering, translation, and reconstruction.
//!
//! Uses html5gum for tokenization and macos-translate (Apple on-device
//! Translation.framework) for translation.

use apple_translate_rs_sync::{LanguageTranslator, TranslationError, TranslationRequest};
use html5gum::Tokenizer;
use html5gum::emitters::default::DefaultEmitter;
use rustc_hash::FxHashMap;
use std::ops::Range;
use std::path::Path;

static ARTICLE_SEPARATOR: &str = "\n\n===PARAGRAPH_SEPARATOR===\n\n";

#[derive(Debug, Clone, PartialEq, Eq)]
enum SegmentKind {
    ArticleBody(usize),
    Heading,
    Nav,
    Sidebar,
    UIElement,
    AltText,
    Generic,
    Skippable,
}

#[derive(Debug, Clone)]
struct TextSegment {
    span: Range<usize>,
    text: String,
    #[allow(dead_code)]
    tag: String,
    kind: SegmentKind,
    translated: Option<String>,
}

struct Cluster {
    segments: Vec<usize>,
    kind: ClusterKind,
}

enum ClusterKind {
    Article(usize),
    Batch,
}

pub struct ProcessFileResult {
    pub path: String,
    pub total_segments: usize,
    pub translated_segments: usize,
    pub clusters: Vec<ClusterSummary>,
}

pub struct ClusterSummary {
    pub kind: String,
    pub count: usize,
}

// ── Text extraction ────────────────────────────────────────────────────────

struct TagInfo {
    tag_name: String,
    classes: Vec<String>,
    id: Option<String>,
    article_idx: Option<usize>,
}

/// Split text into (prefix_whitespace, core_text, suffix_whitespace).
fn split_ws(text: &str) -> (&str, &str, &str) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return (text, "", "");
    }
    let start = text.find(trimmed).unwrap_or(0);
    let end = start + trimmed.len();
    (&text[..start], trimmed, &text[end..])
}

/// Extract the attribute value and its byte span from a raw attribute string
/// like `alt="A nice photo"`. Returns (value_span, value_text).
fn extract_attr_value(raw: &str, base_offset: usize) -> Option<(Range<usize>, String)> {
    let eq_pos = raw.find('=')?;
    let after_eq = raw[eq_pos + 1..].trim_start();
    if after_eq.is_empty() {
        return None;
    }
    // Offset of after_eq within raw.
    let after_eq_offset =
        eq_pos + 1 + (after_eq.as_ptr() as usize - raw[eq_pos + 1..].as_ptr() as usize);

    if let Some(inner) = after_eq.strip_prefix('\"') {
        let end = inner.find('\"')?;
        let value = &inner[..end];
        let abs_start = base_offset + after_eq_offset + 1;
        if value.is_empty() {
            return None;
        }
        Some((abs_start..abs_start + value.len(), value.to_string()))
    } else if let Some(inner) = after_eq.strip_prefix('\'') {
        let end = inner.find('\'')?;
        let value = &inner[..end];
        let abs_start = base_offset + after_eq_offset + 1;
        if value.is_empty() {
            return None;
        }
        Some((abs_start..abs_start + value.len(), value.to_string()))
    } else {
        // Unquoted value.
        let end = after_eq
            .find(|c: char| c.is_whitespace())
            .unwrap_or(after_eq.len());
        let value = &after_eq[..end];
        if value.is_empty() {
            return None;
        }
        let abs_start = base_offset + after_eq_offset;
        Some((abs_start..abs_start + value.len(), value.to_string()))
    }
}

fn is_article_container(tag_name: &str, classes: &[String], id: &Option<String>) -> bool {
    if tag_name == "article" || tag_name == "main" {
        return true;
    }
    if tag_name != "div" && tag_name != "section" {
        return false;
    }
    let class_str = classes.join(" ");
    for keyword in ["post", "article", "entry-content"] {
        if class_str.contains(keyword) {
            return true;
        }
    }
    if let Some(id_str) = id {
        for keyword in ["post", "article", "entry-content"] {
            if id_str.contains(keyword) {
                return true;
            }
        }
    }
    false
}

fn is_skippable_ancestor(tag_name: &str) -> bool {
    matches!(
        tag_name,
        "script" | "style" | "pre" | "code" | "textarea" | "noscript"
    )
}

fn classify_segment(stack: &[TagInfo], text: &str) -> SegmentKind {
    if stack.is_empty() {
        return SegmentKind::Generic;
    }

    for info in stack {
        if is_skippable_ancestor(&info.tag_name) {
            return SegmentKind::Skippable;
        }
    }

    // Check for nav/sidebar/header context first — these override element type.
    for info in stack.iter().rev() {
        let class_str = info.classes.join(" ");
        let id_str = info.id.as_deref().unwrap_or("");

        if info.tag_name == "nav"
            || class_str.contains("nav")
            || class_str.contains("menu")
            || id_str.contains("nav")
            || id_str.contains("menu")
        {
            return SegmentKind::Nav;
        }

        if class_str.contains("sidebar")
            || class_str.contains("widget")
            || class_str.contains("aside")
            || id_str.contains("sidebar")
            || id_str.contains("widget")
        {
            return SegmentKind::Sidebar;
        }

        if info.tag_name == "header" || id_str == "header" {
            return SegmentKind::Nav;
        }
    }

    // Element-specific classification.
    let innermost = &stack[stack.len() - 1];

    if matches!(
        innermost.tag_name.as_str(),
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
    ) {
        return SegmentKind::Heading;
    }

    if matches!(
        innermost.tag_name.as_str(),
        "button" | "label" | "option" | "figcaption"
    ) {
        return SegmentKind::UIElement;
    }

    // Article body — checked after headings so article titles stay as Heading.
    for info in stack.iter().rev() {
        if let Some(article_idx) = info.article_idx {
            return SegmentKind::ArticleBody(article_idx);
        }
    }

    for info in stack.iter().rev() {
        let class_str = info.classes.join(" ");
        let id_str = info.id.as_deref().unwrap_or("");
        if info.tag_name == "footer" || class_str.contains("footer") || id_str.contains("footer") {
            return SegmentKind::Generic;
        }
    }

    let word_count = text.split_whitespace().count();
    if word_count <= 3 {
        SegmentKind::UIElement
    } else {
        SegmentKind::Generic
    }
}

fn extract_segments(html: &str) -> Vec<TextSegment> {
    let mut segments = Vec::new();
    let mut tag_stack: Vec<TagInfo> = Vec::new();
    let mut next_article_idx = 0usize;

    let tokenizer = Tokenizer::new_with_emitter(html, DefaultEmitter::<usize>::new_with_span());

    for token_result in tokenizer {
        let token = match token_result {
            Ok(t) => t,
            Err(_) => continue,
        };

        match &token {
            html5gum::Token::StartTag(tag) => {
                let tag_name = String::from_utf8_lossy(&tag.name[..]).to_ascii_lowercase();

                let mut classes = Vec::new();
                let mut id = None;
                for (attr_name, attr) in &tag.attributes {
                    match &attr_name[..] {
                        b"class" => {
                            let raw = &html[attr.span.start..attr.span.end];
                            classes = raw
                                .split_whitespace()
                                .map(|s| s.to_ascii_lowercase())
                                .collect();
                        }
                        b"id" => {
                            let raw = &html[attr.span.start..attr.span.end];
                            id = Some(raw.to_ascii_lowercase());
                        }
                        _ => {}
                    }
                }

                // Extract translatable attribute values (alt, title).
                for (attr_name, attr) in &tag.attributes {
                    let name = &attr_name[..];
                    if name != b"alt" && name != b"title" {
                        continue;
                    }
                    let raw = &html[attr.span.start..attr.span.end];
                    if let Some((span, value)) = extract_attr_value(raw, attr.span.start) {
                        segments.push(TextSegment {
                            span,
                            text: value,
                            tag: tag_name.clone(),
                            kind: SegmentKind::AltText,
                            translated: None,
                        });
                    }
                }

                let is_article = is_article_container(&tag_name, &classes, &id);
                let article_idx = if is_article {
                    let idx = next_article_idx;
                    next_article_idx += 1;
                    Some(idx)
                } else {
                    tag_stack.last().and_then(|p| p.article_idx)
                };

                tag_stack.push(TagInfo {
                    tag_name,
                    classes,
                    id,
                    article_idx,
                });
            }

            html5gum::Token::EndTag(tag) => {
                let etag_name = String::from_utf8_lossy(&tag.name[..]).to_ascii_lowercase();
                // Pop until we find the matching tag (handle mismatched tags).
                while let Some(top) = tag_stack.last() {
                    if top.tag_name == etag_name {
                        tag_stack.pop();
                        break;
                    }
                    tag_stack.pop();
                }
            }

            html5gum::Token::String(s) => {
                let text = &html[s.span.start..s.span.end];
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    continue;
                }

                let kind = classify_segment(&tag_stack, trimmed);
                if matches!(kind, SegmentKind::Skippable) {
                    continue;
                }

                let innermost_tag = tag_stack
                    .last()
                    .map(|t| t.tag_name.clone())
                    .unwrap_or_default();

                segments.push(TextSegment {
                    span: s.span.start..s.span.end,
                    text: text.to_string(),
                    tag: innermost_tag,
                    kind,
                    translated: None,
                });
            }

            _ => {}
        }
    }

    segments
}

// ── Clustering ──────────────────────────────────────────────────────────────

fn cluster_segments(segments: &[TextSegment]) -> Vec<Cluster> {
    let mut clusters = Vec::new();

    // Contiguous article-body segments of the same article index → one Article cluster.
    let mut i = 0;
    while i < segments.len() {
        if let SegmentKind::ArticleBody(article_idx) = segments[i].kind {
            let idx = article_idx;
            let mut seg_indices = vec![i];
            i += 1;
            while i < segments.len() {
                if let SegmentKind::ArticleBody(aid) = segments[i].kind {
                    if aid == idx {
                        seg_indices.push(i);
                        i += 1;
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
            clusters.push(Cluster {
                segments: seg_indices,
                kind: ClusterKind::Article(idx),
            });
        } else {
            i += 1;
        }
    }

    // Collect remaining segments by kind for batch translation.
    macro_rules! collect_batch {
        ($kind_pat:pat, $vec:ident) => {
            for (i, seg) in segments.iter().enumerate() {
                if matches!(seg.kind, $kind_pat) {
                    $vec.push(i);
                }
            }
        };
    }

    let mut heading_segs = Vec::new();
    let mut nav_segs = Vec::new();
    let mut sidebar_segs = Vec::new();
    let mut ui_segs = Vec::new();
    let mut alt_segs = Vec::new();
    let mut generic_segs = Vec::new();

    collect_batch!(SegmentKind::Heading, heading_segs);
    collect_batch!(SegmentKind::Nav, nav_segs);
    collect_batch!(SegmentKind::Sidebar, sidebar_segs);
    collect_batch!(SegmentKind::UIElement, ui_segs);
    collect_batch!(SegmentKind::AltText, alt_segs);
    collect_batch!(SegmentKind::Generic, generic_segs);

    if !heading_segs.is_empty() {
        clusters.push(Cluster {
            segments: heading_segs,
            kind: ClusterKind::Batch,
        });
    }
    if !nav_segs.is_empty() {
        clusters.push(Cluster {
            segments: nav_segs,
            kind: ClusterKind::Batch,
        });
    }
    if !sidebar_segs.is_empty() {
        clusters.push(Cluster {
            segments: sidebar_segs,
            kind: ClusterKind::Batch,
        });
    }
    if !ui_segs.is_empty() {
        clusters.push(Cluster {
            segments: ui_segs,
            kind: ClusterKind::Batch,
        });
    }
    if !alt_segs.is_empty() {
        clusters.push(Cluster {
            segments: alt_segs,
            kind: ClusterKind::Batch,
        });
    }
    if !generic_segs.is_empty() {
        clusters.push(Cluster {
            segments: generic_segs,
            kind: ClusterKind::Batch,
        });
    }

    clusters
}

// ── Translation ─────────────────────────────────────────────────────────────

/// Batch-translate cores, checking `cache` first. Returns one `Option` per core;
/// `None` means the translation failed (or was not attempted).
fn translate_cores_cached(
    translator: &LanguageTranslator,
    cache: &mut FxHashMap<String, String>,
    cores: &[&str],
) -> Vec<Option<String>> {
    let mut results: Vec<Option<String>> = vec![None; cores.len()];
    let mut uncached: Vec<(usize, &str)> = Vec::new();

    for (i, core) in cores.iter().enumerate() {
        if let Some(cached) = cache.get(*core) {
            results[i] = Some(cached.clone());
        } else {
            uncached.push((i, core));
        }
    }

    if uncached.is_empty() {
        return results;
    }

    let requests: Vec<TranslationRequest> = uncached
        .iter()
        .map(|(_, t)| TranslationRequest::new(*t))
        .collect();
    let batch_results = translator.translate_batch(&requests);

    for (j, result) in batch_results.into_iter().enumerate() {
        let (i, core) = uncached[j];
        if let Ok(resp) = result {
            cache.insert(core.to_string(), resp.target_text.clone());
            results[i] = Some(resp.target_text);
        }
    }

    results
}

fn translate_article_cluster(
    translator: &LanguageTranslator,
    cluster: &Cluster,
    segments: &mut [TextSegment],
    cache: &mut FxHashMap<String, String>,
    verbose: bool,
) -> usize {
    let mut count = 0;

    // Collect (prefix, core, suffix) as owned strings to avoid borrowing segments.
    let indices: Vec<usize> = cluster.segments.clone();
    let part_strs: Vec<(String, String, String)> = indices
        .iter()
        .map(|&idx| {
            let (p, c, s) = split_ws(&segments[idx].text);
            (p.to_string(), c.to_string(), s.to_string())
        })
        .collect();

    let cores: Vec<&str> = part_strs.iter().map(|(_, c, _)| c.as_str()).collect();
    let joined = cores.join(ARTICLE_SEPARATOR);

    let article_translation = match translator.translate(&joined) {
        Ok(response) => Some(response),
        Err(TranslationError::TimedOut { seconds, .. }) => {
            // Timeout on first attempt — retry once. Article translations are
            // long and the model may need a second warm-up pass.
            if verbose {
                eprintln!("  note: article translation timed out after {seconds}s, retrying...");
            }
            match translator.translate(&joined) {
                Ok(response) => Some(response),
                Err(e2) => {
                    if verbose {
                        eprintln!(
                            "  warning: article translation retry also failed ({e2}), falling back to batch"
                        );
                    }
                    None
                }
            }
        }
        Err(e) => {
            if verbose {
                eprintln!("  warning: article translation failed ({e}), falling back to batch");
            }
            None
        }
    };

    if let Some(response) = article_translation {
        let translated_parts: Vec<&str> = response.target_text.split(ARTICLE_SEPARATOR).collect();
        if translated_parts.len() == cores.len() {
            for (i, &idx) in indices.iter().enumerate() {
                let (prefix, _, suffix) = &part_strs[i];
                let mut result =
                    String::with_capacity(prefix.len() + translated_parts[i].len() + suffix.len());
                result.push_str(prefix);
                result.push_str(translated_parts[i]);
                result.push_str(suffix);
                segments[idx].translated = Some(result);
                count += 1;
            }
            return count;
        }
        if verbose {
            eprintln!(
                "  note: separator split mismatch (expected {}, got {}), falling back to batch",
                cores.len(),
                translated_parts.len()
            );
        }
    }

    // Fallback: batch translate individually, with caching.
    let translated = translate_cores_cached(translator, cache, &cores);
    for (i, &idx) in indices.iter().enumerate() {
        if let Some(ref t) = translated[i] {
            let (prefix, _, suffix) = &part_strs[i];
            let mut result = String::with_capacity(prefix.len() + t.len() + suffix.len());
            result.push_str(prefix);
            result.push_str(t);
            result.push_str(suffix);
            segments[idx].translated = Some(result);
            count += 1;
        } else if verbose {
            eprintln!("  warning: segment translation failed, keeping original");
        }
    }

    count
}

fn translate_batch_cluster(
    translator: &LanguageTranslator,
    cluster: &Cluster,
    segments: &mut [TextSegment],
    cache: &mut FxHashMap<String, String>,
    verbose: bool,
) -> usize {
    let mut count = 0;

    let indices: Vec<usize> = cluster.segments.clone();
    let part_strs: Vec<(String, String, String)> = indices
        .iter()
        .map(|&idx| {
            let (p, c, s) = split_ws(&segments[idx].text);
            (p.to_string(), c.to_string(), s.to_string())
        })
        .collect();

    let cores: Vec<&str> = part_strs.iter().map(|(_, c, _)| c.as_str()).collect();

    let translated = translate_cores_cached(translator, cache, &cores);

    for (i, &idx) in indices.iter().enumerate() {
        match &translated[i] {
            Some(translated_text) => {
                let (prefix, _, suffix) = &part_strs[i];
                let mut result = String::with_capacity(
                    prefix.len() + translated_text.len() + suffix.len(),
                );
                result.push_str(prefix);
                result.push_str(translated_text);
                result.push_str(suffix);
                segments[idx].translated = Some(result);
                count += 1;
            }
            None => {
                if verbose {
                    eprintln!("  warning: translation failed for segment");
                }
            }
        }
    }

    count
}

fn translate_clusters(
    translator: &LanguageTranslator,
    clusters: &mut [Cluster],
    segments: &mut [TextSegment],
    cache: &mut FxHashMap<String, String>,
    verbose: bool,
) -> usize {
    let mut total = 0;
    for cluster in clusters {
        let count = match &cluster.kind {
            ClusterKind::Article(_) => {
                translate_article_cluster(translator, cluster, segments, cache, verbose)
            }
            ClusterKind::Batch => {
                translate_batch_cluster(translator, cluster, segments, cache, verbose)
            }
        };
        total += count;
    }
    total
}

// ── Reconstruction ──────────────────────────────────────────────────────────

/// Escape `&`, `<`, and `>` for safe insertion into HTML text content.
fn escape_html_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// Escape for insertion into a double-quoted HTML attribute value.
fn escape_html_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// CSS snippet injected into each translated page. Uses a hidden checkbox at
/// the top of `<body>` and the `:has()` selector so clicking any translated
/// element toggles all of them between translation and original globally.
static TRANSLATE_SNIPPET: &str = concat!(
    "<input type=\"checkbox\" id=\"localized-toggle\" style=\"display:none\">",
    "<style>",
    ".localized-translated-text{cursor:pointer;transition:opacity .15s ease}",
    ".localized-translated-text:hover{opacity:.82}",
    ".localized-translated-text .localized-original{display:none}",
    "html:has(#localized-toggle:checked) .localized-translated-text .localized-translation{display:none}",
    "html:has(#localized-toggle:checked) .localized-translated-text .localized-original{display:inline}",
    "</style>",
);

fn inject_translate_snippet(html: &mut String) {
    // Place the hidden checkbox + style right after the opening <body> tag.
    if let Some(pos) = html.find("<body") {
        if let Some(end) = html[pos..].find('>') {
            html.insert_str(pos + end + 1, TRANSLATE_SNIPPET);
            return;
        }
    }
    if let Some(pos) = html.find("<head") {
        if let Some(end) = html[pos..].find('>') {
            html.insert_str(pos + end + 1, TRANSLATE_SNIPPET);
            return;
        }
    }
    html.insert_str(0, TRANSLATE_SNIPPET);
}

fn apply_translations(html: &str, segments: &[TextSegment]) -> String {
    let mut result = html.to_string();

    // Sort descending by span start so earlier spans remain valid after replacement.
    let mut replacements: Vec<(Range<usize>, String)> = segments
        .iter()
        .filter_map(|seg| {
            let translated = seg.translated.as_ref()?;
            let (prefix, core, suffix) = split_ws(&seg.text);

            // `translated` is prefix + translated_core + suffix; extract the core.
            let translated_core = &translated[prefix.len()..translated.len() - suffix.len()];

            // AltText segments live inside HTML attributes (alt="…", title="…").
            // Wrapping them in HTML tags would produce broken markup, so just
            // replace the value text directly.
            let is_attr = matches!(seg.kind, SegmentKind::AltText);
            // <title> content must be plain text — tags inside are not rendered.
            let is_title = seg.tag.as_str() == "title";

            if is_attr || is_title {
                let escaped = escape_html_attr(translated_core);
                return Some((seg.span.clone(), format!("{prefix}{escaped}{suffix}")));
            }

            // Use <label> so clicking toggles the global checkbox via :has().
            // Fall back to <span> inside links/buttons to avoid double-interaction.
            let is_interactive = matches!(seg.tag.as_str(), "a" | "button");
            let el = if is_interactive { "span" } else { "label" };
            let for_attr = if is_interactive { "" } else { " for=\"localized-toggle\"" };

            let wrapped = format!(
                "{prefix}<{el} class=\"localized-translated-text\"{for_attr}><span class=\"localized-translation\">{translated}</span><span class=\"localized-original\" aria-hidden=\"true\">{original}</span></{el}>{suffix}",
                prefix = prefix,
                el = el,
                for_attr = for_attr,
                translated = escape_html_text(translated_core),
                original = core,
                suffix = suffix,
            );
            Some((seg.span.clone(), wrapped))
        })
        .collect();
    replacements.sort_by_key(|b| std::cmp::Reverse(b.0.start));

    for (span, replacement) in &replacements {
        result.replace_range(span.clone(), replacement);
    }

    result
}

// ── Cluster summaries ───────────────────────────────────────────────────────

fn cluster_summaries(clusters: &[Cluster], segments: &[TextSegment]) -> Vec<ClusterSummary> {
    clusters
        .iter()
        .map(|c| {
            let kind = match &c.kind {
                ClusterKind::Article(n) => format!("Article #{}", n),
                ClusterKind::Batch => {
                    // Infer batch kind from the first segment.
                    match segments.get(c.segments.first().copied().unwrap_or(0)) {
                        Some(seg) => match seg.kind {
                            SegmentKind::Heading => "Headings".into(),
                            SegmentKind::Nav => "Nav".into(),
                            SegmentKind::Sidebar => "Sidebar".into(),
                            SegmentKind::UIElement => "UI elements".into(),
                            SegmentKind::AltText => "Alt/title text".into(),
                            SegmentKind::Generic => "Generic".into(),
                            _ => "Batch".into(),
                        },
                        None => "Batch".into(),
                    }
                }
            };
            ClusterSummary {
                kind,
                count: c.segments.len(),
            }
        })
        .collect()
}

// ── File processing ─────────────────────────────────────────────────────────

pub fn process_file(
    path: &Path,
    from_lang: Option<&str>,
    to_lang: &str,
    apply: bool,
    cache: &mut FxHashMap<String, String>,
    verbose: bool,
) -> Result<ProcessFileResult, String> {
    let rel = path
        .strip_prefix(std::env::current_dir().unwrap_or_default())
        .unwrap_or(path)
        .display()
        .to_string();

    let html =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;

    let mut segments = extract_segments(&html);

    if segments.is_empty() {
        return Ok(ProcessFileResult {
            path: rel,
            total_segments: 0,
            translated_segments: 0,
            clusters: Vec::new(),
        });
    }

    // Auto-detect source language if not provided.
    let source_lang = if let Some(lang) = from_lang {
        lang.to_string()
    } else {
        let combined: String = segments
            .iter()
            .map(|s| split_ws(&s.text).1)
            .collect::<Vec<&str>>()
            .join(" ");
        match apple_translate_rs_sync::detect_language(&combined) {
            Some(lang) => lang,
            None => {
                return Err(format!(
                    "{}: could not auto-detect source language",
                    path.display()
                ));
            }
        }
    };

    let translator = match LanguageTranslator::new(&source_lang, to_lang) {
        Ok(t) => t,
        Err(TranslationError::LanguageNotInstalled { source, target }) => {
            return Err(format!(
                "{}: translation model not installed for {source} → {target}\n\
                 Hint: open System Settings → General → Language & Region → \
                 Translation to download the model",
                path.display()
            ));
        }
        Err(TranslationError::LanguageUnsupported { source, target }) => {
            return Err(format!(
                "{}: language pair {source} → {target} is not supported by \
                 Apple's on-device Translation framework",
                path.display()
            ));
        }
        Err(TranslationError::TimedOut { operation, seconds }) => {
            return Err(format!(
                "{}: {operation} timed out after {seconds}s — the model may \
                 still be downloading; try again in a moment",
                path.display()
            ));
        }
        Err(e) => {
            return Err(format!(
                "{}: language pair {source_lang}→{to_lang} unavailable: {e}",
                path.display()
            ));
        }
    };

    // Pre-warm the translation engine (downloads model if needed).
    if let Err(e) = translator.prepare() {
        match &e {
            TranslationError::TimedOut { .. } => {
                // Timeout during warm-up is common on first use while the model
                // loads; the actual translation call will also warm the engine.
                if verbose {
                    eprintln!("  note: prepare timed out (model may still be warming up)");
                }
            }
            _ => {
                if verbose {
                    eprintln!("  warning: prepare failed (non-fatal): {e}");
                }
            }
        }
    }

    let mut clusters = cluster_segments(&segments);
    let translated_count = translate_clusters(&translator, &mut clusters, &mut segments, cache, verbose);

    let summaries = cluster_summaries(&clusters, &segments);

    if apply && translated_count > 0 {
        let mut new_html = apply_translations(&html, &segments);
        inject_translate_snippet(&mut new_html);
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &new_html).map_err(|e| format!("write tmp {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .map_err(|e| format!("rename {} → {}: {e}", tmp.display(), path.display()))?;
    }

    Ok(ProcessFileResult {
        path: rel,
        total_segments: segments.len(),
        translated_segments: translated_count,
        clusters: summaries,
    })
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_ws() {
        assert_eq!(split_ws("hello"), ("", "hello", ""));
        assert_eq!(split_ws("  hello  "), ("  ", "hello", "  "));
        assert_eq!(split_ws("\n  hello\n  "), ("\n  ", "hello", "\n  "));
        assert_eq!(split_ws("  "), ("  ", "", ""));
    }

    #[test]
    fn test_extract_paragraph() {
        let html = "<html><body><p>Hello world</p></body></html>";
        let segs = extract_segments(html);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].text, "Hello world");
        assert_eq!(segs[0].tag, "p");
        assert_eq!(&html[segs[0].span.start..segs[0].span.end], "Hello world");
    }

    #[test]
    fn test_extract_heading() {
        let html = "<h1>Title</h1><p>Body text here.</p>";
        let segs = extract_segments(html);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].text, "Title");
        assert!(matches!(segs[0].kind, SegmentKind::Heading));
        assert_eq!(segs[1].text, "Body text here.");
    }

    #[test]
    fn test_skip_script_style() {
        let html =
            "<script>console.log('hi')</script><style>body{}</style><p>Visible</p><pre>code</pre>";
        let segs = extract_segments(html);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].text, "Visible");
    }

    #[test]
    fn test_extract_alt_attribute() {
        let html = r#"<img src="x.jpg" alt="A nice photo">"#;
        let segs = extract_segments(html);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].text, "A nice photo");
        assert!(matches!(segs[0].kind, SegmentKind::AltText));
    }

    #[test]
    fn test_extract_title_attribute() {
        let html = r#"<a href="/" title="Go home">Home</a>"#;
        let segs = extract_segments(html);
        assert_eq!(segs.len(), 2); // title attr + "Home" text
        assert!(
            segs.iter()
                .any(|s| s.text == "Go home" && matches!(s.kind, SegmentKind::AltText))
        );
        assert!(segs.iter().any(|s| s.text == "Home"));
    }

    #[test]
    fn test_nav_classification() {
        let html = "<nav><ul><li><a>Home</a></li></ul></nav>";
        let segs = extract_segments(html);
        assert_eq!(segs.len(), 1);
        assert!(matches!(segs[0].kind, SegmentKind::Nav));
    }

    #[test]
    fn test_article_classification() {
        let html = "<div class=\"post\"><h2>Post Title</h2><p>Paragraph one.</p><p>Paragraph two.</p></div>";
        let segs = extract_segments(html);
        // Title + 2 paragraphs, all should be in article context
        assert!(matches!(segs[0].kind, SegmentKind::Heading));
        assert!(matches!(segs[1].kind, SegmentKind::ArticleBody(_)));
        assert!(matches!(segs[2].kind, SegmentKind::ArticleBody(_)));
        // Both paragraphs should have the same article index
        if let SegmentKind::ArticleBody(a) = segs[1].kind {
            assert!(matches!(segs[2].kind, SegmentKind::ArticleBody(b) if b == a));
        }
    }

    #[test]
    fn test_sidebar_classification() {
        let html = "<div class=\"sidebar\"><h3>About</h3><p>Some info.</p></div>";
        let segs = extract_segments(html);
        assert!(segs.iter().all(|s| matches!(s.kind, SegmentKind::Sidebar)));
    }

    #[test]
    fn test_cluster_article_grouping() {
        let html = r#"<article>
            <h2>Title</h2>
            <p>Paragraph 1.</p>
            <p>Paragraph 2.</p>
            <p>Paragraph 3.</p>
        </article>"#;
        let segs = extract_segments(html);
        let clusters = cluster_segments(&segs);

        let article_clusters: Vec<_> = clusters
            .iter()
            .filter(|c| matches!(c.kind, ClusterKind::Article(_)))
            .collect();
        assert_eq!(article_clusters.len(), 1);
        // 3 paragraphs grouped as one article cluster
        assert_eq!(article_clusters[0].segments.len(), 3);
    }

    #[test]
    fn test_cluster_batch_grouping() {
        let html = "<nav><a>Home</a><a>About</a></nav><h1>Title</h1>";
        let segs = extract_segments(html);
        let clusters = cluster_segments(&segs);

        // Should have Nav cluster and Heading cluster
        assert!(
            clusters
                .iter()
                .any(|c| matches!(c.kind, ClusterKind::Batch) && c.segments.len() == 2)
        );
        assert!(
            clusters
                .iter()
                .any(|c| matches!(c.kind, ClusterKind::Batch) && c.segments.len() == 1)
        );
    }

    #[test]
    fn test_apply_translations_descending_order() {
        let html = "<p>First</p><p>Second</p>";
        let mut segs = extract_segments(html);
        assert_eq!(segs.len(), 2);

        segs[0].translated = Some("Premier".into());
        segs[1].translated = Some("Deuxième".into());

        let result = apply_translations(html, &segs);
        assert!(result.contains("Premier"));
        assert!(result.contains("Deuxième"));
        assert!(result.contains(r#"<label class="localized-translated-text" for="localized-toggle">"#));
        assert!(result.contains(r#"<span class="localized-original" aria-hidden="true">First</span>"#));
        assert!(result.contains(r#"<span class="localized-original" aria-hidden="true">Second</span>"#));
    }

    #[test]
    fn test_apply_translations_preserves_whitespace() {
        let html = "<div>\n  <p>Hello</p>\n</div>";
        let mut segs = extract_segments(html);
        assert_eq!(segs.len(), 1);

        let (prefix, core, suffix) = split_ws(&segs[0].text);
        assert_eq!(core, "Hello");

        segs[0].translated = Some(format!("{prefix}Hola{suffix}"));
        let result = apply_translations(html, &segs);
        // Whitespace preserved outside the wrapper
        assert!(result.contains("Hola</span>"));
        assert!(result.starts_with("<div>\n  <p>"));
        assert!(result.contains("\n</div>"));
        // Original text preserved
        assert!(result.contains(r#"<span class="localized-original" aria-hidden="true">Hello</span>"#));
    }

    #[test]
    fn test_empty_html() {
        let segs = extract_segments("");
        assert!(segs.is_empty());
    }

    #[test]
    fn test_no_translatable_text() {
        let html = "<script>code</script><style>css</style>";
        let segs = extract_segments(html);
        assert!(segs.is_empty());
    }

    #[test]
    fn test_escape_html_text() {
        assert_eq!(escape_html_text("hello"), "hello");
        assert_eq!(escape_html_text("a < b"), "a &lt; b");
        assert_eq!(escape_html_text("a & b"), "a &amp; b");
        assert_eq!(escape_html_text("<script>"), "&lt;script&gt;");
    }

    #[test]
    fn test_inject_translate_snippet() {
        let mut html = "<html><head></head><body><p>hi</p></body></html>".to_string();
        inject_translate_snippet(&mut html);
        assert!(html.contains("localized-translated-text"));
        assert!(html.contains("localized-toggle"));
        assert!(html.contains(":has("));
        // Snippet inserted right after <body>
        let body_pos = html.find("<body").unwrap();
        let toggle_pos = html.find("localized-toggle").unwrap();
        assert!(toggle_pos > body_pos);
    }

    #[test]
    fn test_inject_translate_snippet_no_body() {
        let mut html = "<html><head></head><p>hi</p></html>".to_string();
        inject_translate_snippet(&mut html);
        assert!(html.contains("localized-translated-text"));
    }

    #[test]
    fn test_apply_translations_uses_span_for_links() {
        let html = r#"<a href="/">Home</a>"#;
        let mut segs = extract_segments(html);
        // "Home" is inside <a>; should get <span> wrapper not <label>
        segs[0].translated = Some("Accueil".into());
        let result = apply_translations(html, &segs);
        assert!(result.contains(r#"<span class="localized-translated-text">"#));
        assert!(!result.contains("<label"));
    }

    #[test]
    fn test_apply_translations_escapes_html() {
        let html = "<p>x</p>";
        let mut segs = extract_segments(html);
        segs[0].translated = Some("<script>alert('hi')</script>".into());
        let result = apply_translations(html, &segs);
        // The translated text should be escaped
        assert!(result.contains("&lt;script&gt;"));
    }

    #[test]
    fn test_apply_translations_preserves_original() {
        let html = "<p>Hello &amp; welcome</p>";
        let mut segs = extract_segments(html);
        // The extracted text includes the raw HTML entity
        let core = split_ws(&segs[0].text).1;
        assert_eq!(core, "Hello &amp; welcome");
        segs[0].translated = Some("Bonjour &amp; bienvenue".into());
        let result = apply_translations(html, &segs);
        // Original preserved as-is (raw HTML source)
        assert!(result.contains(r#"<span class="localized-original" aria-hidden="true">Hello &amp; welcome</span>"#));
        // Translated text is HTML-escaped (the & in the translation gets double-escaped)
        assert!(result.contains("Bonjour &amp;amp; bienvenue"));
    }

    #[test]
    fn test_escape_html_attr() {
        assert_eq!(escape_html_attr("hello"), "hello");
        assert_eq!(escape_html_attr(r#"a "quoted" value"#), "a &quot;quoted&quot; value");
        assert_eq!(escape_html_attr("a & b"), "a &amp; b");
        assert_eq!(escape_html_attr("<x>"), "&lt;x&gt;");
    }

    #[test]
    fn test_apply_translations_alt_text_no_wrapper() {
        let html = r#"<img src="x.jpg" alt="A nice photo">"#;
        let mut segs = extract_segments(html);
        assert!(matches!(segs[0].kind, SegmentKind::AltText));
        segs[0].translated = Some("Une belle photo".into());
        let result = apply_translations(html, &segs);
        // Alt attribute value replaced directly — no wrapper spans
        assert!(!result.contains("localized-translated-text"));
        assert!(!result.contains("<label"));
        assert!(result.contains(r#"alt="Une belle photo""#));
    }

    #[test]
    fn test_apply_translations_title_no_wrapper() {
        let html = "<title>My Page</title>";
        let mut segs = extract_segments(html);
        assert_eq!(segs[0].tag, "title");
        segs[0].translated = Some("Ma Page".into());
        let result = apply_translations(html, &segs);
        // <title> content replaced directly — no wrapper spans
        assert!(!result.contains("localized-translated-text"));
        assert_eq!(result, "<title>Ma Page</title>");
    }
}
