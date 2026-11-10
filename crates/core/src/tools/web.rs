//! Web search, fetching, image and academic search, and discussion helpers.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use serde::Deserialize;

use super::{ToolBox, cap_tool_result, chats, number_lines};

impl ToolBox {
    /// The web tools: search, fetch, image/academic/discussion search.
    #[allow(clippy::too_many_lines)]
    pub(super) async fn run_web(&self, name: &str, args: &str) -> (String, String) {
        match name {
            "web_search" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let query = v
                    .get("query")
                    .and_then(|q| q.as_str())
                    .unwrap_or_default()
                    .to_string();
                let recency = v
                    .get("recency")
                    .and_then(|r| r.as_str())
                    .map(str::to_string);
                let str_list = |k: &str| {
                    v.get(k)
                        .and_then(|a| a.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(str::to_string))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                };
                let include = str_list("include_domains");
                let mut exclude = str_list("exclude_domains");
                exclude.extend(self.blocked_domains.iter().cloned());
                let status = "Searching the web…".to_string();
                let web = self.search(&query, recency.as_deref(), &include, &exclude);
                let discussions = async {
                    if is_opinion_query(&query) {
                        discussion_search(&self.client, &query, &self.discussions).await
                    } else {
                        Vec::new()
                    }
                };
                let (web, discussions) = tokio::join!(web, discussions);
                let result = match web {
                    Ok(results) => results.format(&query, &discussions),
                    Err(e) => format!("search failed: {e}"),
                };
                (result, status)
            }
            "fetch_url" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let url = v
                    .get("url")
                    .and_then(|u| u.as_str())
                    .unwrap_or_default()
                    .to_string();
                let explicit_offset = v
                    .get("offset")
                    .and_then(serde_json::Value::as_u64)
                    .map(|n| usize::try_from(n.max(1)).unwrap_or(1));
                let limit = v
                    .get("limit")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(200)
                    .clamp(1, 200) as usize;
                let fresh = v
                    .get("fresh")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let status = format!("Fetching {url}…");
                // `chat:<session>#<message>` reads a past conversation of this
                // space, starting near the message unless an offset is given.
                let fetched = if url.starts_with("chat:") {
                    match &self.files {
                        Some(ctx) if !self.research_only => chats::transcript(ctx, &url),
                        _ => Err(anyhow::anyhow!("chat history unavailable")),
                    }
                } else {
                    self.fetch_cached(&url, fresh).await.map(|text| (text, 1))
                };
                let result = match fetched {
                    Ok((text, start_line)) => {
                        let offset = explicit_offset.unwrap_or(start_line);
                        let lines: Vec<&str> = text.lines().collect();
                        let total = lines.len();
                        let start = (offset - 1).min(total);
                        let slice = &lines[start..(start + limit).min(total)];
                        if slice.is_empty() {
                            format!("{url}: offset {offset} is past the end ({total} lines)")
                        } else {
                            format!(
                                "{url} (lines {}-{} of {total}):\n{}",
                                start + 1,
                                start + slice.len(),
                                number_lines(slice, start),
                            )
                        }
                    }
                    Err(e) => format!("fetch failed: {e}"),
                };
                (result, status)
            }
            "image_search" => {
                let value = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let query = value["query"].as_str().unwrap_or_default();
                let hits = image_search(
                    &self.client,
                    query,
                    &self.image_search,
                    self.searxng_url.as_deref(),
                )
                .await;
                (
                    format_image_results(query, &hits),
                    "Searching images…".into(),
                )
            }
            "academic_search" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let query = v
                    .get("query")
                    .and_then(|q| q.as_str())
                    .unwrap_or_default()
                    .to_string();
                let limit = usize::try_from(
                    v.get("limit")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(10),
                )
                .unwrap_or(10);
                let status = "Searching academic literature…".to_string();
                let result = match academic_search(&self.client, &query, limit).await {
                    Ok(papers) if papers.is_empty() => "no results".to_string(),
                    Ok(papers) => format_papers(&papers),
                    Err(e) => format!("academic search failed: {e}"),
                };
                (result, status)
            }
            "discussion_search" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let query = v
                    .get("query")
                    .and_then(|q| q.as_str())
                    .unwrap_or_default()
                    .to_string();
                let status = "Searching HN and Reddit…".to_string();
                let cache_key = format!("discussion://{query}");

                let cached = if let Some(db_path) = &self.db_path {
                    crate::db::open_attached(db_path)
                        .ok()
                        .and_then(|conn| crate::db::cache_get(&conn, &cache_key).ok().flatten())
                        .and_then(|(_, text, fetched_at)| {
                            // Do not preserve the old empty-result sentinel:
                            // it may have been produced by a blocked backend,
                            // and a later fallback can now recover results.
                            if crate::db::is_fresh(&fetched_at, chrono::Utc::now())
                                && text != "no results"
                            {
                                Some(text)
                            } else {
                                None
                            }
                        })
                } else {
                    None
                };

                let result = if let Some(cached_text) = cached {
                    cached_text
                } else {
                    let hits = discussion_search(&self.client, &query, &self.discussions).await;
                    let text = if hits.is_empty() {
                        format!("no discussions found for \"{query}\" · try different words")
                    } else {
                        format_discussion_hits(&hits, &[])
                    };
                    if !hits.is_empty()
                        && let Some(db_path) = &self.db_path
                        && let Ok(conn) = crate::db::open_attached(db_path)
                    {
                        let _ = crate::db::cache_put(&conn, &cache_key, &cache_key, None, &text);
                    }
                    text
                };
                (result, status)
            }
            other => (
                format!("unknown tool: {other}"),
                "Running tool…".to_string(),
            ),
        }
    }
}

#[derive(Debug)]
pub(super) struct SearchHit {
    pub(super) title: String,
    pub(super) url: String,
    pub(super) snippet: String,
    /// Thumbnail the provider itself returned; never discovered by fetching.
    pub(super) image: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct SearxngResponse {
    #[serde(default)]
    pub(super) results: Vec<SearxngResult>,
}

#[derive(Deserialize)]
pub(super) struct SearxngResult {
    pub(super) thumbnail: Option<String>,
    pub(super) img_src: Option<String>,
    pub(super) title: String,
    pub(super) url: String,
    #[serde(default)]
    pub(super) content: String,
}

/// Shared by backends that send a request and expect a JSON body back: send,
/// raise on a non-2xx status, then deserialize. `DuckDuckGo` scrapes HTML
/// instead of parsing JSON, so it doesn't use this helper.
pub(super) async fn send_and_parse<T: serde::de::DeserializeOwned>(
    req: reqwest::RequestBuilder,
) -> anyhow::Result<T> {
    req.send()
        .await?
        .error_for_status()?
        .json::<T>()
        .await
        .map_err(Into::into)
}

/// `SearXNG`'s JSON API needs `search: formats: [html, json]` enabled in the
/// instance's `settings.yml` — off by default. A misconfigured instance
/// surfaces as an HTML/error response here, which `error_for_status`/`json`
/// turns into a readable error for the model rather than a silent empty result.
pub(super) async fn searxng_search(
    client: &reqwest::Client,
    base_url: &str,
    query: &str,
    recency: Option<&str>,
) -> anyhow::Result<Vec<SearchHit>> {
    let mut req = client
        .get(format!("{base_url}/search"))
        .query(&[("q", query), ("format", "json")]);
    if let Some(r) = recency {
        req = req.query(&[("time_range", r)]);
    }
    let resp = send_and_parse::<SearxngResponse>(req).await?;
    Ok(resp
        .results
        .into_iter()
        .take(8)
        .map(|r| SearchHit {
            title: r.title,
            url: r.url,
            snippet: r.content,
            image: thumbnail_url(r.thumbnail.as_deref().or(r.img_src.as_deref())),
        })
        .collect())
}

#[derive(Deserialize)]
pub(super) struct LangsearchResponse {
    pub(super) data: Option<LangsearchData>,
}

#[derive(Deserialize)]
pub(super) struct LangsearchData {
    #[serde(rename = "webPages")]
    pub(super) web_pages: Option<LangsearchWebPages>,
}

#[derive(Deserialize)]
pub(super) struct LangsearchWebPages {
    #[serde(default)]
    pub(super) value: Vec<LangsearchResult>,
}

#[derive(Deserialize)]
pub(super) struct LangsearchResult {
    pub(super) name: String,
    url: String,
    #[serde(default)]
    snippet: String,
}

/// `LangSearch` (<https://langsearch.com)>: free-tier hosted search API, no card
/// required. More reliable than scraping `DuckDuckGo` when the service is
/// reachable; auto mode falls back when its endpoint is unavailable.
pub(super) async fn langsearch_search(
    client: &reqwest::Client,
    endpoint: &str,
    key: &str,
    query: &str,
    recency: Option<&str>,
) -> anyhow::Result<Vec<SearchHit>> {
    let mut body = serde_json::json!({ "query": query, "count": 8 });
    if let Some(r) = recency {
        // LangSearch's freshness values are Bing-style camelCase.
        let freshness = match r {
            "day" => "oneDay",
            "week" => "oneWeek",
            "month" => "oneMonth",
            _ => "oneYear",
        };
        body["freshness"] = serde_json::json!(freshness);
    }
    let req = client.post(endpoint).bearer_auth(key).json(&body);
    let resp = send_and_parse::<LangsearchResponse>(req).await?;
    Ok(resp
        .data
        .and_then(|d| d.web_pages)
        .map(|w| w.value)
        .unwrap_or_default()
        .into_iter()
        .map(|r| SearchHit {
            title: r.name,
            url: r.url,
            snippet: r.snippet,
            image: None,
        })
        .collect())
}

/// Last-resort HTML scraper; bot challenges are backend failures.
/// `DuckDuckGo`'s HTML endpoint; tests point it at a dead local port so the
/// `auto` fallback never reaches the network from a test.
#[cfg(not(test))]
pub(super) const DUCKDUCKGO_HTML: &str = "https://html.duckduckgo.com/html/";
#[cfg(test)]
pub(super) const DUCKDUCKGO_HTML: &str = "http://127.0.0.1:1/html/";

pub(super) async fn duckduckgo_search(
    client: &reqwest::Client,
    endpoint: &str,
    query: &str,
) -> anyhow::Result<Vec<SearchHit>> {
    let html = client
        .get(endpoint)
        .header("User-Agent", "Mozilla/5.0 (compatible; nexus-chat)")
        .query(&[("q", query)])
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    if html.contains("anomaly-modal") || html.contains("challenge-form") {
        anyhow::bail!("DuckDuckGo returned an anti-bot challenge")
    }
    Ok(parse_ddg_html(&html).into_iter().take(8).collect())
}

/// Last-resort HTML scraper, used only after API backends fail.
pub(super) async fn brave_search(
    client: &reqwest::Client,
    endpoint: &str,
    query: &str,
) -> anyhow::Result<Vec<SearchHit>> {
    let response = client
        .get(endpoint)
        .header(
            "User-Agent",
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/131.0 Safari/537.36",
        )
        .query(&[("q", query)])
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await?;
    let status = response.status();
    let html = response.text().await?;
    if !status.is_success() {
        anyhow::bail!("Brave returned HTTP {status}")
    }
    let hits = parse_brave_html(&html);
    if hits.is_empty() && html.to_ascii_lowercase().contains("captcha") {
        anyhow::bail!("Brave returned an anti-bot challenge")
    }
    Ok(hits.into_iter().take(8).collect())
}

/// Extract readable text from a fetched body. The type comes from the bytes
/// (PDF, HTML, office, plain text, …), the `Content-Type` is only a hint, and
/// binary content becomes a one-line note instead of mojibake.
pub(super) fn extract_pdf_or_html(bytes: &[u8], content_type: &str) -> String {
    crate::extract::from_fetched(bytes, content_type)
}

/// GET `url` and return its readable text. Capped at 2MB of raw body and a
/// 30s timeout — a research searcher agent shouldn't be able to wedge on a
/// pathological page.
pub(super) async fn fetch_url_text(client: &reqwest::Client, url: &str) -> anyhow::Result<String> {
    if let Some(feed_url) = reddit_thread_feed_url(url) {
        return fetch_reddit_thread(client, feed_url.as_str()).await;
    }
    let resp = client
        .get(url)
        .header("User-Agent", "Mozilla/5.0 (compatible; nexus-chat)")
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await?
        .error_for_status()?;
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = resp.bytes().await?;
    let capped = &bytes[..bytes.len().min(2_000_000)];
    Ok(extract_pdf_or_html(capped, &content_type))
}

/// Whether `url` points at a `YouTube` watch page (long or short form).
pub(super) fn is_youtube_url(url: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(url) else {
        return false;
    };
    matches!(u.host_str(), Some(h) if h == "youtube.com" || h.ends_with(".youtube.com") || h == "youtu.be")
}

/// Pull the first caption track's `baseUrl` out of a `YouTube` watch page's
/// embedded JSON (`ytInitialData`/`ytInitialPlayerResponse`). The value is
/// JSON-string-escaped (`\/` and `\uXXXX`); unescape just enough to get a
/// usable URL — a full JSON parse isn't needed for one field.
pub(super) fn parse_caption_track_url(watch_page_html: &str) -> Option<String> {
    let marker = "\"baseUrl\":\"";
    let idx = watch_page_html.find(marker)? + marker.len();
    let end = watch_page_html[idx..].find('"')? + idx;
    let raw = &watch_page_html[idx..end];
    Some(raw.replace("\\/", "/").replace("\\u0026", "&"))
}

/// Join a `YouTube` timedtext XML transcript's `<text>` cue contents with
/// spaces into one plain-text string (no timing/markup kept — this is fed
/// to a research searcher, not rendered as captions).
pub(super) fn strip_timedtext_xml(xml: &str) -> String {
    split_tag_blocks(xml, "text")
        .iter()
        .map(|inner| html_unescape_entities(inner))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Minimal HTML entity unescaping for cue text (`&amp;` last, so it doesn't
/// double-unescape compound entities — same ordering as `src/extract.rs`'s
/// `xml_unescape`).
pub(super) fn html_unescape_entities(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// Fetch a `YouTube` video's transcript via the keyless timedtext endpoint:
/// scrape the watch page for a caption track URL, fetch it, and join the
/// cue text. Falls back to the normal page scrape when no caption track is
/// found (private/no-captions videos still return something searchable).
pub(super) async fn fetch_youtube_transcript(
    client: &reqwest::Client,
    url: &str,
) -> anyhow::Result<String> {
    let watch_html = client
        .get(url)
        .header("User-Agent", "Mozilla/5.0 (compatible; nexus-chat)")
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let Some(track_url) = parse_caption_track_url(&watch_html) else {
        return Ok(strip_html_to_text(&watch_html));
    };
    let xml = client
        .get(&track_url)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    Ok(strip_timedtext_xml(&xml))
}

/// Pull `(title, url, snippet)` hits out of a `DuckDuckGo` HTML results page.
/// Each result is `<a class="result__a" href="...uddg=<url>...">title</a>`
/// followed by `<a class="result__snippet" ...>snippet</a>`.
pub(super) fn parse_ddg_html(html: &str) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    let mut pos = 0;
    while let Some(rel) = html[pos..].find("class=\"result__a\"") {
        let marker_at = pos + rel;
        let tag_start = html[..marker_at].rfind('<').unwrap_or(marker_at);
        let Some(gt) = html[marker_at..].find('>') else {
            break;
        };
        let tag = &html[tag_start..marker_at + gt];
        let text_start = marker_at + gt + 1;
        let Some(close_rel) = html[text_start..].find("</a>") else {
            break;
        };
        let title = strip_tags(&html[text_start..text_start + close_rel]);
        pos = text_start + close_rel + 4;

        let Some(href) = extract_attr(tag, "href") else {
            continue;
        };
        let Some(url) = resolve_ddg_href(&href) else {
            continue;
        };
        let snippet = find_snippet(html, pos);
        if !title.is_empty() {
            hits.push(SearchHit {
                title,
                url,
                snippet,
                image: None,
            });
        }
    }
    hits
}

/// Pull result cards from Brave's server-rendered HTML. This intentionally
/// relies only on stable semantic class names and returns ordinary links, so
/// callers can use the same formatter/citation path as API-backed results.
pub(super) fn parse_brave_html(html: &str) -> Vec<SearchHit> {
    const TITLE_MARKER: &str = "<div class=\"title search-snippet-title";
    let mut hits = Vec::new();
    let mut pos = 0;
    while let Some(rel) = html[pos..].find(TITLE_MARKER) {
        let marker_at = pos + rel;
        let Some(gt_rel) = html[marker_at..].find('>') else {
            break;
        };
        let text_start = marker_at + gt_rel + 1;
        let Some(close_rel) = html[text_start..].find("</div>") else {
            break;
        };
        let title = strip_tags(&html[text_start..text_start + close_rel]);
        let title_end = text_start + close_rel + "</div>".len();
        let Some(anchor_start) = html[..marker_at].rfind("<a ") else {
            pos = title_end;
            continue;
        };
        let Some(anchor_gt_rel) = html[anchor_start..].find('>') else {
            pos = title_end;
            continue;
        };
        let anchor_tag = &html[anchor_start..=(anchor_start + anchor_gt_rel)];
        let Some(url) = extract_attr(anchor_tag, "href") else {
            pos = title_end;
            continue;
        };
        let url = html_unescape(&url);
        if !url.starts_with("http") || title.is_empty() {
            pos = title_end;
            continue;
        }

        let segment_end = html[title_end..]
            .find(TITLE_MARKER)
            .map_or(html.len(), |end| title_end + end);
        let snippet = html[title_end..segment_end]
            .find("generic-snippet")
            .and_then(|generic| {
                let start = title_end + generic;
                let content = html[start..segment_end].find("class=\"content")?;
                let content_start = start + content;
                let gt = html[content_start..segment_end].find('>')?;
                let text_start = content_start + gt + 1;
                let close = html[text_start..segment_end].find("</div>")?;
                Some(strip_tags(&html[text_start..text_start + close]))
            })
            .unwrap_or_default();
        hits.push(SearchHit {
            title,
            url,
            snippet,
            image: None,
        });
        pos = title_end;
    }
    hits
}

/// The snippet immediately following a result's title anchor, if any.
pub(super) fn find_snippet(html: &str, from: usize) -> String {
    let marker = "class=\"result__snippet\"";
    let Some(rel) = html[from..].find(marker) else {
        return String::new();
    };
    let idx = from + rel;
    let Some(gt) = html[idx..].find('>') else {
        return String::new();
    };
    let text_start = idx + gt + 1;
    let Some(close) = html[text_start..].find("</a>") else {
        return String::new();
    };
    strip_tags(&html[text_start..text_start + close])
}

/// `DuckDuckGo`'s result links redirect through `/l/?uddg=<percent-encoded-url>`.
pub(super) fn resolve_ddg_href(href: &str) -> Option<String> {
    if href.contains("uddg=") {
        let absolute = if let Some(rest) = href.strip_prefix("//") {
            format!("https://{rest}")
        } else if href.starts_with("http") {
            href.to_string()
        } else {
            format!("https://duckduckgo.com{href}")
        };
        let decoded = reqwest::Url::parse(&absolute)
            .ok()
            .and_then(|url| {
                url.query_pairs()
                    .find(|(k, _)| k == "uddg")
                    .map(|(_, v)| v.into_owned())
            })
            .unwrap_or_default();
        return (!decoded.is_empty()).then_some(decoded);
    }
    if let Some(rest) = href.strip_prefix("//") {
        return Some(format!("https://{rest}"));
    }
    href.starts_with("http").then(|| href.to_string())
}

pub(super) fn extract_attr(tag: &str, name: &str) -> Option<String> {
    let marker = format!("{name}=\"");
    let idx = tag.find(&marker)? + marker.len();
    let end = tag[idx..].find('"')? + idx;
    Some(tag[idx..end].to_string())
}

/// Drop HTML tags and unescape entities, for anchor text pulled out of raw markup.
pub(super) fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    html_unescape(out.trim())
}

pub(super) fn html_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
}

/// Every top-level `<tag>...</tag>` block's inner HTML, in order. Does not
/// recurse into nested same-named tags.
pub(super) fn split_tag_blocks(html: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.find(&open) {
        let Some(tag_end) = rest[start..].find('>') else {
            break;
        };
        let body_start = start + tag_end + 1;
        let Some(close_rel) = rest[body_start..].find(&close) else {
            break;
        };
        let body_end = body_start + close_rel;
        out.push(rest[body_start..body_end].to_string());
        rest = &rest[body_end + close.len()..];
    }
    out
}

/// HTML page body → readable Markdown (headings, lists, tables kept; scripts,
/// styles and inline data dropped). One converter for web fetches and files:
/// see `extract::html`.
pub(super) fn strip_html_to_text(html: &str) -> String {
    crate::extract::html::to_markdown(html)
}

/// One scholarly-paper hit, flattened from the Semantic Scholar response.
pub(super) struct Paper {
    pub(super) title: String,
    pub(super) authors: Vec<String>,
    pub(super) year: Option<i64>,
    pub(super) venue: Option<String>,
    pub(super) abstract_snippet: Option<String>,
    pub(super) citation_count: Option<i64>,
    pub(super) url: String,
}

#[derive(Deserialize)]
pub(super) struct SemanticScholarResponse {
    #[serde(default)]
    pub(super) data: Vec<SemanticScholarPaper>,
}

#[derive(Deserialize)]
pub(super) struct SemanticScholarPaper {
    pub(super) title: String,
    #[serde(default)]
    pub(super) authors: Vec<SemanticScholarAuthor>,
    year: Option<i64>,
    venue: Option<String>,
    #[serde(rename = "abstract")]
    abstract_snippet: Option<String>,
    #[serde(rename = "citationCount")]
    citation_count: Option<i64>,
    url: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct SemanticScholarAuthor {
    pub(super) name: String,
}

/// Semantic Scholar Graph API (api.semanticscholar.org): free, keyless.
/// A 429 (rate limited) surfaces as an error the caller turns into
/// tool-result text — the model falls back to search(mode=web).
pub(super) async fn academic_search(
    client: &reqwest::Client,
    query: &str,
    limit: usize,
) -> anyhow::Result<Vec<Paper>> {
    let req = client
        .get("https://api.semanticscholar.org/graph/v1/paper/search")
        .query(&[
            ("query", query),
            ("limit", &limit.min(20).to_string()),
            (
                "fields",
                "title,authors,year,venue,abstract,citationCount,url",
            ),
        ]);
    let resp = send_and_parse::<SemanticScholarResponse>(req).await?;
    Ok(resp
        .data
        .into_iter()
        .map(|p| Paper {
            title: p.title,
            authors: p.authors.into_iter().map(|a| a.name).collect(),
            year: p.year,
            venue: p.venue,
            abstract_snippet: p.abstract_snippet,
            citation_count: p.citation_count,
            url: p.url.unwrap_or_default(),
        })
        .collect())
}

/// Numbered scholarly-paper results the model cites the same way as
/// `format_results`' web hits: `[n]` inline, matched against this list.
pub(super) fn format_papers(papers: &[Paper]) -> String {
    papers
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let mut meta = Vec::new();
            if !p.authors.is_empty() {
                meta.push(p.authors.join(", "));
            }
            if let Some(y) = p.year {
                meta.push(y.to_string());
            }
            if let Some(v) = &p.venue {
                meta.push(v.clone());
            }
            if let Some(c) = p.citation_count {
                meta.push(format!("{c} citations"));
            }
            let abs = p.abstract_snippet.as_deref().unwrap_or("");
            format!(
                "[{}] {}\n    {}\n    {abs}\n    {}",
                i + 1,
                p.title,
                meta.join(" · "),
                p.url
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub(super) const SEARCH_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const REDDIT_USER_AGENT: &str = concat!(
    "nexus-chat/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/dukunuu/nexus-chat)"
);
pub(super) const REDDIT_RATE_LIMIT: &str =
    "Reddit is rate-limiting right now · try again in a minute";

pub(super) struct RedditToken {
    access_token: String,
    pub(super) expires: Instant,
}

pub(super) struct DiscussionSearch {
    pub(super) reddit_base: String,
    pub(super) oauth_base: String,
    pub(super) hn_url: String,
    pub(super) credentials: Option<(String, String)>,
    pub(super) token: tokio::sync::Mutex<Option<RedditToken>>,
}

impl Default for DiscussionSearch {
    #[cfg(not(test))]
    fn default() -> Self {
        Self {
            reddit_base: "https://www.reddit.com".into(),
            oauth_base: "https://oauth.reddit.com".into(),
            hn_url: "https://hn.algolia.com/api/v1/search".into(),
            credentials: crate::config::load_reddit_credentials(),
            token: tokio::sync::Mutex::new(None),
        }
    }

    /// Tests stay hermetic: a dead local port instead of Reddit and Hacker
    /// News, and never the developer's real Reddit credentials.
    #[cfg(test)]
    fn default() -> Self {
        Self {
            reddit_base: "http://127.0.0.1:1".into(),
            oauth_base: "http://127.0.0.1:1".into(),
            hn_url: "http://127.0.0.1:1/api/v1/search".into(),
            credentials: None,
            token: tokio::sync::Mutex::new(None),
        }
    }
}

/// One discussion-forum hit (Hacker News story or Reddit post), flattened
/// to what the model needs to decide whether to `fetch_url` it.
#[derive(Clone)]
pub(super) struct DiscussionHit {
    pub(super) title: String,
    pub(super) url: String,
    pub(super) meta: String,
}

#[derive(Deserialize)]
pub(super) struct HnSearchResponse {
    #[serde(default)]
    hits: Vec<HnHit>,
}

#[derive(Deserialize)]
pub(super) struct HnHit {
    title: Option<String>,
    #[serde(rename = "objectID")]
    object_id: String,
    #[serde(default)]
    points: i64,
    #[serde(default, rename = "num_comments")]
    num_comments: i64,
}

pub(super) async fn hn_search(
    client: &reqwest::Client,
    query: &str,
    endpoint: &str,
) -> anyhow::Result<Vec<DiscussionHit>> {
    let req = client
        .get(endpoint)
        .query(&[("query", query), ("tags", "story")]);
    let resp = send_and_parse::<HnSearchResponse>(req).await?;
    Ok(resp
        .hits
        .into_iter()
        .take(8)
        .map(|h| DiscussionHit {
            title: h.title.unwrap_or_else(|| "(untitled)".to_string()),
            url: format!("https://news.ycombinator.com/item?id={}", h.object_id),
            meta: format!("HN · {} points · {} comments", h.points, h.num_comments),
        })
        .collect())
}

#[derive(Deserialize)]
pub(super) struct RedditSearchResponse {
    data: RedditListing,
}

#[derive(Deserialize)]
pub(super) struct RedditListing {
    #[serde(default)]
    children: Vec<RedditChild>,
}

#[derive(Deserialize)]
pub(super) struct RedditChild {
    data: RedditPost,
}

#[derive(Deserialize)]
pub(super) struct RedditPost {
    title: String,
    permalink: String,
    subreddit: String,
    #[serde(default)]
    score: i64,
    #[serde(default)]
    num_comments: i64,
    #[serde(default)]
    selftext: String,
}

#[derive(Deserialize)]
pub(super) struct RedditAccessToken {
    access_token: String,
    expires_in: u64,
}

pub(super) async fn reddit_oauth_search(
    client: &reqwest::Client,
    query: &str,
    search: &DiscussionSearch,
    credentials: &(String, String),
) -> anyhow::Result<Vec<DiscussionHit>> {
    let mut cached = search.token.lock().await;
    if cached
        .as_ref()
        .is_none_or(|token| token.expires <= Instant::now())
    {
        let token: RedditAccessToken = send_and_parse(
            client
                .post(format!("{}/api/v1/access_token", search.reddit_base))
                .header("User-Agent", REDDIT_USER_AGENT)
                .basic_auth(&credentials.0, Some(&credentials.1))
                .form(&[("grant_type", "client_credentials")]),
        )
        .await?;
        anyhow::ensure!(
            !token.access_token.is_empty(),
            "missing Reddit access token"
        );
        *cached = Some(RedditToken {
            access_token: token.access_token,
            expires: Instant::now() + Duration::from_secs(token.expires_in),
        });
    }
    let token = cached
        .as_ref()
        .expect("token was acquired")
        .access_token
        .clone();
    drop(cached);
    let resp: RedditSearchResponse = send_and_parse(
        client
            .get(format!("{}/search", search.oauth_base))
            .header("User-Agent", REDDIT_USER_AGENT)
            .bearer_auth(token)
            .query(&[("q", query), ("sort", "relevance"), ("limit", "10")]),
    )
    .await?;
    Ok(resp
        .data
        .children
        .into_iter()
        .take(10)
        .map(|c| DiscussionHit {
            title: c.data.title,
            url: format!("https://www.reddit.com{}", c.data.permalink),
            meta: format!(
                "r/{} · {} points · {} comments\n    {}",
                c.data.subreddit,
                c.data.score,
                c.data.num_comments,
                one_line(&c.data.selftext, 240)
            ),
        })
        .collect())
}

pub(super) async fn reddit_search(
    client: &reqwest::Client,
    query: &str,
    search: &DiscussionSearch,
) -> anyhow::Result<Vec<DiscussionHit>> {
    if let Some(credentials) = &search.credentials {
        if let Ok(hits) = reddit_oauth_search(client, query, search, credentials).await {
            return Ok(hits);
        }
        *search.token.lock().await = None;
    }
    let xml = client
        .get(format!("{}/search.rss", search.reddit_base))
        .header("User-Agent", REDDIT_USER_AGENT)
        .query(&[("q", query), ("sort", "relevance"), ("limit", "10")])
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    Ok(parse_reddit_atom(&xml)
        .into_iter()
        .take(10)
        .map(|entry| DiscussionHit {
            title: entry.title,
            url: entry.url,
            meta: format!(
                "{} · {} · {}\n    {}",
                entry.subreddit,
                entry.updated,
                entry.author,
                thumbnail_text(one_line(&entry.content, 240), entry.image.as_deref())
            ),
        })
        .collect())
}

pub(super) struct RedditEntry {
    pub(super) image: Option<String>,
    pub(super) title: String,
    pub(super) url: String,
    pub(super) subreddit: String,
    pub(super) author: String,
    pub(super) updated: String,
    pub(super) content: String,
    reply: bool,
}

/// The feed uses only a small Atom subset. Skip comments/CDATA while scanning
/// tags, and respect quoted attributes so embedded markup cannot become entries.
pub(super) fn atom_elements<'a>(xml: &'a str, name: &str) -> Vec<(&'a str, &'a str)> {
    let mut elements = Vec::new();
    let mut pos = 0;
    let mut open = None;
    let mut depth = 0;
    while let Some(start) = xml[pos..].find('<').map(|offset| pos + offset) {
        let tail = &xml[start..];
        let special = if tail.starts_with("<![CDATA[") {
            Some("]]>")
        } else if tail.starts_with("<!--") {
            Some("-->")
        } else {
            None
        };
        if let Some(end) = special {
            let Some(offset) = tail.find(end) else { break };
            pos = start + offset + end.len();
            continue;
        }
        let mut quote = None;
        let end = tail.char_indices().skip(1).find_map(|(offset, c)| {
            if let Some(q) = quote {
                if c == q {
                    quote = None;
                }
            } else if matches!(c, '\'' | '"') {
                quote = Some(c);
            } else if c == '>' {
                return Some(start + offset);
            }
            None
        });
        let Some(end) = end else { break };
        let tag = xml[start + 1..end].trim();
        pos = end + 1;
        if tag.strip_prefix('/').map(str::trim) == Some(name) && depth > 0 {
            depth -= 1;
            if depth == 0 {
                let (header, body) = open.take().expect("matching opening tag");
                elements.push((header, &xml[body..start]));
            }
        } else if tag.split([' ', '\t', '\r', '\n', '/']).next() == Some(name) {
            if tag.ends_with('/') {
                if depth == 0 {
                    elements.push((tag, ""));
                }
            } else {
                if depth == 0 {
                    open = Some((tag, pos));
                }
                depth += 1;
            }
        }
    }
    elements
}

pub(super) fn atom_attr(tag: &str, name: &str) -> Option<String> {
    let (_, mut rest) = tag.split_once(char::is_whitespace)?;
    loop {
        rest = rest.trim_start();
        let end = rest.find(|c: char| c.is_whitespace() || c == '=')?;
        let key = &rest[..end];
        rest = rest[end..].trim_start().strip_prefix('=')?.trim_start();
        let quote = rest.chars().next()?;
        if !matches!(quote, '\'' | '"') {
            return None;
        }
        rest = &rest[1..];
        let end = rest.find(quote)?;
        if key == name {
            return Some(atom_unescape(&rest[..end]));
        }
        rest = &rest[end + 1..];
    }
}

pub(super) fn atom_unescape(mut text: &str) -> String {
    let mut out = String::new();
    while let Some(start) = text.find('&') {
        out.push_str(&text[..start]);
        text = &text[start..];
        let Some(end) = text.find(';') else { break };
        let entity = &text[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| entity.strip_prefix('#')?.parse().ok())
                .and_then(char::from_u32),
        };
        if let Some(c) = decoded {
            out.push(c);
        } else {
            out.push_str(&text[..=end]);
        }
        text = &text[end + 1..];
    }
    out.push_str(text);
    out
}

pub(super) fn atom_text(mut text: &str) -> String {
    let mut out = String::new();
    while let Some(start) = text.find("<![CDATA[") {
        out.push_str(&atom_unescape(&text[..start]));
        text = &text[start + 9..];
        let Some(end) = text.find("]]>") else {
            return out;
        };
        out.push_str(&text[..end]);
        text = &text[end + 3..];
    }
    out.push_str(&atom_unescape(text));
    out
}

pub(super) fn first_html_image(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<img ")?;
    let end = html[start..].find('>')? + start;
    atom_attr(&html[start..=end], "src")
}

/// A provider-supplied thumbnail worth showing: an `https` URL with no
/// credentials, templates or whitespace. Anything else is dropped here so the
/// card never shows a "couldn't load" box for an address it would never fetch.
pub(super) fn thumbnail_url(candidate: Option<&str>) -> Option<String> {
    let url = candidate?.trim();
    (url.len() <= 2_000
        && !url.contains(char::is_whitespace)
        && crate::remote_image::validate_url(url).is_ok())
    .then(|| url.to_string())
}

/// Append the `image:` line the result cards read back.
pub(super) fn thumbnail_text(text: String, image: Option<&str>) -> String {
    match thumbnail_url(image) {
        Some(url) => format!("{text}\n    image: {url}"),
        None => text,
    }
}

pub(super) fn parse_reddit_atom(xml: &str) -> Vec<RedditEntry> {
    atom_elements(xml, "entry")
        .into_iter()
        .filter_map(|(_, body)| {
            let field = |name| {
                atom_elements(body, name)
                    .first()
                    .map(|(_, text)| atom_text(text))
            };
            let url = atom_elements(body, "link")
                .into_iter()
                .find_map(|(tag, _)| {
                    let rel = atom_attr(tag, "rel");
                    (rel.is_none() || rel.as_deref() == Some("alternate"))
                        .then(|| atom_attr(tag, "href"))
                        .flatten()
                        .filter(|url| is_reddit_url(url))
                })?;
            let author = field("author")
                .and_then(|author| {
                    atom_elements(&author, "name")
                        .first()
                        .map(|(_, name)| atom_text(name))
                })
                .unwrap_or_else(|| "[deleted]".into());
            let subreddit = atom_elements(body, "category")
                .into_iter()
                .find_map(|(tag, _)| {
                    atom_attr(tag, "label")
                        .filter(|label| label.starts_with("r/"))
                        .or_else(|| atom_attr(tag, "term").map(|sub| format!("r/{sub}")))
                })
                .unwrap_or_else(|| "r/unknown".into());
            let content = field("content").unwrap_or_default();
            let reply = atom_elements(&content, "a").into_iter().any(|(tag, text)| {
                text.contains("[parent]")
                    && atom_attr(tag, "href").is_some_and(|url| reddit_comment_id(&url).is_some())
            });
            Some(RedditEntry {
                image: first_html_image(&content),
                title: one_line(&field("title").unwrap_or_else(|| "(untitled)".into()), 300),
                url,
                subreddit,
                author: format!(
                    "u/{}",
                    author
                        .trim()
                        .trim_start_matches("/u/")
                        .trim_start_matches("u/")
                ),
                updated: field("updated")
                    .unwrap_or_default()
                    .chars()
                    .take(10)
                    .collect(),
                content: atom_unescape(&strip_html_to_text(&content)),
                reply,
            })
        })
        .collect()
}

pub(super) fn one_line(text: &str, limit: usize) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(limit)
        .collect()
}

pub(super) fn reddit_comment_id(url: &str) -> Option<String> {
    let url = reqwest::Url::parse(url).ok()?;
    let segments: Vec<_> = url
        .path_segments()?
        .filter(|part| !part.is_empty())
        .collect();
    let comments = segments.iter().position(|part| *part == "comments")?;
    segments.get(comments + 3).map(|id| (*id).to_string())
}

pub(super) fn reddit_thread_feed_url(url: &str) -> Option<reqwest::Url> {
    if !is_reddit_url(url) {
        return None;
    }
    let mut url = reqwest::Url::parse(url).ok()?;
    let segments: Vec<_> = url
        .path_segments()?
        .filter(|part| !part.is_empty())
        .collect();
    let comments = segments.iter().position(|part| *part == "comments")?;
    segments.get(comments + 1)?;
    let path = format!(
        "/{}/.rss",
        segments[..segments.len().min(comments + 3)].join("/")
    );
    url.set_path(&path);
    url.set_query(Some("limit=20"));
    url.set_fragment(None);
    Some(url)
}

pub(super) async fn fetch_reddit_thread(
    client: &reqwest::Client,
    feed_url: &str,
) -> anyhow::Result<String> {
    for attempt in 0..2 {
        let response = client
            .get(feed_url)
            .header("User-Agent", REDDIT_USER_AGENT)
            .timeout(SEARCH_TIMEOUT)
            .send()
            .await?;
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            if attempt == 0 {
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
            return Ok(REDDIT_RATE_LIMIT.into());
        }
        let xml = response.error_for_status()?.text().await?;
        let entries = parse_reddit_atom(&xml);
        let Some(post) = entries.first() else {
            return Ok("no readable Reddit thread found".into());
        };
        let mut out = format!(
            "{} · {} · {}\n\n{}",
            post.title,
            post.subreddit,
            post.author,
            one_line(&post.content, 3_000)
        );
        for comment in entries.iter().skip(1).filter(|entry| !entry.reply).take(15) {
            let _ = write!(
                out,
                "\n\n{}: {}",
                comment.author,
                one_line(&comment.content, 500)
            );
        }
        return Ok(cap_tool_result(out));
    }
    unreachable!("the second request returns a result")
}

pub(super) fn is_reddit_url(url: &str) -> bool {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|host| host == "reddit.com" || host.ends_with(".reddit.com"))
}

pub(super) fn interleave_discussions<'a>(
    hn: &'a [DiscussionHit],
    reddit: &'a [DiscussionHit],
) -> Vec<&'a DiscussionHit> {
    let mut seen = std::collections::HashSet::new();
    (0..hn.len().max(reddit.len()))
        .flat_map(|rank| [hn.get(rank), reddit.get(rank)])
        .flatten()
        .filter(|hit| seen.insert(normalize_url(&hit.url)))
        .collect()
}

/// Interleaved, deduplicated discussions using the source card's URL-first format.
pub(super) fn format_discussion_hits(hn: &[DiscussionHit], reddit: &[DiscussionHit]) -> String {
    format_discussion_list(&interleave_discussions(hn, reddit), 1)
}

pub(super) fn format_discussion_list(hits: &[&DiscussionHit], start: usize) -> String {
    hits.iter()
        .enumerate()
        .map(|(i, h)| format!("[{}] {}\n    {}\n    {}", start + i, h.title, h.url, h.meta))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Each backend has the same deadline as web search; one failing never hides the other.
pub(super) async fn discussion_search(
    client: &reqwest::Client,
    query: &str,
    search: &DiscussionSearch,
) -> Vec<DiscussionHit> {
    let (hn, reddit) = tokio::join!(
        tokio::time::timeout(SEARCH_TIMEOUT, hn_search(client, query, &search.hn_url)),
        tokio::time::timeout(SEARCH_TIMEOUT, reddit_search(client, query, search)),
    );
    let hn = hn.ok().and_then(Result::ok).unwrap_or_default();
    let reddit = reddit.ok().and_then(Result::ok).unwrap_or_default();
    interleave_discussions(&hn, &reddit)
        .into_iter()
        .cloned()
        .collect()
}

#[derive(Clone)]
pub(super) struct ImageSearch {
    pub(super) wikipedia: String,
    pub(super) commons: String,
    pub(super) openverse: String,
}

impl Default for ImageSearch {
    fn default() -> Self {
        #[cfg(not(test))]
        let endpoints = [
            "https://en.wikipedia.org/w/api.php",
            "https://commons.wikimedia.org/w/api.php",
            "https://api.openverse.org/v1/images/",
        ];
        #[cfg(test)]
        let endpoints = ["http://127.0.0.1:1"; 3];
        Self {
            wikipedia: endpoints[0].into(),
            commons: endpoints[1].into(),
            openverse: endpoints[2].into(),
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct ImageHit {
    pub(super) title: String,
    pub(super) url: String,
    pub(super) page: String,
    pub(super) license: String,
    pub(super) source: String,
}

pub(super) fn parse_image_hits(value: &serde_json::Value, provider: usize) -> Vec<ImageHit> {
    use serde_json::Value;
    let mut pages: Vec<_> = value["query"]["pages"]
        .as_object()
        .into_iter()
        .flat_map(|pages| pages.values())
        .collect();
    pages.sort_by_key(|page| page["index"].as_i64().unwrap_or(0));
    let results: Vec<_> = if provider < 2 {
        pages
    } else {
        value["results"].as_array().into_iter().flatten().collect()
    };
    results
        .into_iter()
        .filter_map(|item| {
            let text = |key: &str| item[key].as_str().unwrap_or_default();
            let info = &item["imageinfo"][0];
            if provider == 1
                && !matches!(
                    info["mime"].as_str(),
                    Some("image/png" | "image/jpeg" | "image/webp" | "image/gif")
                )
            {
                return None;
            }
            let urls = match provider {
                0 => [&item["thumbnail"]["source"], &Value::Null],
                1 => [&info["thumburl"], &info["url"]],
                2 => [&item["url"], &item["thumbnail"]],
                _ => [&item["img_src"], &item["thumbnail_src"]],
            };
            let url = urls
                .into_iter()
                .filter_map(Value::as_str)
                .find(|url| crate::remote_image::validate_url(url).is_ok())?;
            let (page, license) = match provider {
                0 => (text("fullurl"), ""),
                1 => (
                    info["descriptionurl"].as_str().unwrap_or_default(),
                    info["extmetadata"]["LicenseShortName"]["value"]
                        .as_str()
                        .unwrap_or_default(),
                ),
                2 => (text("foreign_landing_url"), text("license")),
                _ => (text("url"), ""),
            };
            Some(ImageHit {
                title: text("title").into(),
                url: url.into(),
                page: page.into(),
                license: license.into(),
                source: if provider == 2 {
                    text("source").into()
                } else {
                    String::new()
                },
            })
        })
        .collect()
}

pub(super) async fn image_provider(
    client: &reqwest::Client,
    endpoint: &str,
    query: &str,
    provider: usize,
) -> anyhow::Result<Vec<ImageHit>> {
    let request = client.get(endpoint).header(
        reqwest::header::USER_AGENT,
        concat!(
            "nexus-chat/",
            env!("CARGO_PKG_VERSION"),
            " (+",
            env!("CARGO_PKG_REPOSITORY"),
            ")"
        ),
    );
    let request = match provider {
        0 | 1 => {
            let request = request.query(&[
                ("action", "query"),
                ("format", "json"),
                ("generator", "search"),
                ("gsrsearch", query),
                ("gsrlimit", if provider == 0 { "4" } else { "6" }),
            ]);
            if provider == 0 {
                request.query(&[
                    ("prop", "pageimages|info"),
                    ("inprop", "url"),
                    ("piprop", "thumbnail"),
                    ("pithumbsize", "800"),
                ])
            } else {
                request.query(&[
                    ("gsrnamespace", "6"),
                    ("prop", "imageinfo"),
                    ("iiprop", "url|size|mime|extmetadata"),
                    ("iiurlwidth", "800"),
                ])
            }
        }
        2 => request.query(&[("q", query), ("page_size", "6")]),
        _ => request.query(&[("q", query), ("format", "json"), ("categories", "images")]),
    };
    let value = send_and_parse::<serde_json::Value>(request).await?;
    Ok(parse_image_hits(&value, provider))
}

pub(super) fn merge_image_hits(providers: &[Vec<ImageHit>]) -> Vec<ImageHit> {
    let mut seen = std::collections::HashSet::new();
    let mut hits = Vec::new();
    for rank in 0..providers.iter().map(Vec::len).max().unwrap_or(0) {
        for provider in providers {
            if let Some(hit) = provider.get(rank)
                && seen.insert(hit.url.clone())
            {
                hits.push(hit.clone());
                if hits.len() == 8 {
                    return hits;
                }
            }
        }
    }
    hits
}

pub(super) async fn image_search(
    client: &reqwest::Client,
    query: &str,
    search: &ImageSearch,
    searxng: Option<&str>,
) -> Vec<ImageHit> {
    let searxng = searxng.map(|base| format!("{}/search", base.trim_end_matches('/')));
    let endpoints = [
        Some(search.wikipedia.as_str()),
        Some(search.commons.as_str()),
        Some(search.openverse.as_str()),
        searxng.as_deref(),
    ];
    let providers = futures_util::future::join_all(endpoints.into_iter().enumerate().map(
        |(provider, endpoint)| async move {
            let Some(endpoint) = endpoint else {
                return Vec::new();
            };
            tokio::time::timeout(
                SEARCH_TIMEOUT,
                image_provider(client, endpoint, query, provider),
            )
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default()
        },
    ))
    .await;
    merge_image_hits(&providers)
}

pub(super) fn format_image_results(query: &str, hits: &[ImageHit]) -> String {
    if hits.is_empty() {
        return format!("no images found for \"{query}\" · try a more specific name");
    }
    hits.iter()
        .enumerate()
        .map(|(index, hit)| {
            let domain = reqwest::Url::parse(&hit.page)
                .ok()
                .and_then(|url| url.host_str().map(str::to_string))
                .unwrap_or_default();
            let mut meta = format!("image · {domain}");
            for field in [&hit.source, &hit.license] {
                if !field.is_empty() {
                    let _ = write!(meta, " · {field}");
                }
            }
            format!(
                "[{}] {}\n    {}\n    {meta} · page {}",
                index + 1,
                hit.title,
                hit.url,
                hit.page
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Normalize a source URL for dedup: lowercase the host only (path/query
/// case is preserved — some servers are case-sensitive there), strip
/// `utm_*`/`fbclid` query params, and drop a trailing `/` and any fragment.
/// Unparseable input (not actually a URL) is returned unchanged so it still
/// participates in a plain string-equality dedup.
pub fn normalize_url(url: &str) -> String {
    let Ok(mut u) = reqwest::Url::parse(url) else {
        return url.to_string();
    };
    u.set_fragment(None);
    let kept: Vec<(String, String)> = u
        .query_pairs()
        .filter(|(k, _)| k != "fbclid" && !k.starts_with("utm_"))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if kept.is_empty() {
        u.set_query(None);
    } else {
        let q = kept
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        u.set_query(Some(&q));
    }
    if let Some(h) = u.host_str().map(str::to_lowercase) {
        let _ = u.set_host(Some(&h));
    }
    if u.path().ends_with('/') && u.path() != "/" {
        let trimmed = u.path().trim_end_matches('/').to_string();
        u.set_path(&trimmed);
    }
    let mut s = u.to_string();
    if let Some(stripped) = s.strip_suffix('/') {
        s = stripped.to_string();
    }
    s
}

/// Collapse duplicate cited sources across a set of Searcher findings.
/// Each finding may end with a `Sources:` block of `N. url` lines (see
/// `SEARCHER_PROMPT` in research.rs); a source line whose normalized URL
/// already appeared in an earlier finding is dropped from later ones so
/// the Synthesizer doesn't see the same source cited from every angle.
/// Non-source lines are untouched.
pub fn dedup_source_lines(findings: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    findings
        .iter()
        .map(|f| {
            let mut out_lines = Vec::new();
            let mut in_sources = false;
            for line in f.lines() {
                if line.trim().eq_ignore_ascii_case("Sources:") {
                    in_sources = true;
                    out_lines.push(line.to_string());
                    continue;
                }
                if in_sources
                    && let Some((_, url)) = line.trim().split_once(['.', ')'])
                    && !seen.insert(normalize_url(url.trim()))
                {
                    continue; // dup — drop this line
                }
                out_lines.push(line.to_string());
            }
            out_lines.join("\n")
        })
        .collect()
}

/// Every normalized URL cited in a set of findings' `Sources:` blocks —
/// what gets linked into a research session's source bundle.
pub fn cited_url_norms(findings: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for f in findings {
        let mut in_sources = false;
        for line in f.lines() {
            if line.trim().eq_ignore_ascii_case("Sources:") {
                in_sources = true;
                continue;
            }
            if in_sources && let Some((_, url)) = line.trim().split_once(['.', ')']) {
                let url = url.trim();
                if !url.is_empty() {
                    out.push(normalize_url(url));
                }
            }
        }
    }
    out
}

/// Rewrite `query` with `site:`/`-site:` terms — backend-agnostic (every
/// engine this app talks to honors Google-style site filters), so
/// `include_domains`/`exclude_domains`/`blocked_domains` need no per-backend
/// plumbing beyond this string rewrite.
pub fn rewrite_query_with_domains(query: &str, include: &[String], exclude: &[String]) -> String {
    let mut q = query.to_string();
    for d in include {
        let _ = write!(q, " site:{d}");
    }
    for d in exclude {
        let _ = write!(q, " -site:{d}");
    }
    q
}

pub(super) fn is_opinion_query(query: &str) -> bool {
    let lower = query.to_lowercase();
    let words: Vec<_> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect();
    words.iter().any(|word| {
        matches!(
            *word,
            "best"
                | "vs"
                | "versus"
                | "worth"
                | "recommend"
                | "recommendation"
                | "review"
                | "reviews"
                | "experience"
                | "opinion"
                | "alternative"
                | "alternatives"
                | "reddit"
                | "hn"
                | "which"
        )
    }) || words
        .windows(2)
        .any(|pair| pair == ["should", "i"] || pair == ["is", "it"])
}

pub(super) fn relevance_tokens(text: &str) -> Vec<String> {
    let folded: String = text
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => 'a',
            'ç' | 'ć' | 'č' => 'c',
            'ď' | 'đ' => 'd',
            'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ė' | 'ę' | 'ě' => 'e',
            'ì' | 'í' | 'î' | 'ï' | 'ī' | 'ı' => 'i',
            'ł' => 'l',
            'ñ' | 'ń' | 'ň' => 'n',
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' => 'o',
            'ř' => 'r',
            'ś' | 'š' | 'ş' => 's',
            'ť' => 't',
            'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' => 'u',
            'ý' | 'ÿ' => 'y',
            'ź' | 'ż' | 'ž' => 'z',
            _ => c,
        })
        .collect();
    folded
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| {
            token.chars().count() >= 3
                && !matches!(
                    *token,
                    "the"
                        | "and"
                        | "for"
                        | "with"
                        | "from"
                        | "that"
                        | "this"
                        | "what"
                        | "how"
                        | "are"
                        | "was"
                        | "were"
                        | "has"
                        | "have"
                        | "does"
                        | "can"
                        | "about"
                        | "into"
                        | "not"
                        | "but"
                        | "you"
                        | "your"
                )
        })
        .map(str::to_string)
        .collect()
}

pub(super) fn filter_web_hits(query: &str, hits: &mut Vec<SearchHit>) -> usize {
    let tokens = relevance_tokens(query);
    if tokens.is_empty() {
        return 0;
    }
    let mut ranked: Vec<_> = hits
        .iter()
        .enumerate()
        .map(|(index, hit)| {
            let text =
                relevance_tokens(&format!("{} {} {}", hit.title, hit.url, hit.snippet)).join(" ");
            (
                index,
                tokens
                    .iter()
                    .filter(|token| text.contains(token.as_str()))
                    .count(),
            )
        })
        .collect();
    ranked.sort_by_key(|(_, score)| std::cmp::Reverse(*score));
    let mut keep = vec![false; hits.len()];
    for (rank, (index, score)) in ranked.into_iter().enumerate() {
        keep[index] = score > 0 || rank < 3;
    }
    let before = hits.len();
    let mut index = 0;
    hits.retain(|_| {
        let retain = keep[index];
        index += 1;
        retain
    });
    before - hits.len()
}

pub(super) fn format_web_results(
    query: &str,
    mut hits: Vec<SearchHit>,
    discussions: &[DiscussionHit],
) -> String {
    let dropped = filter_web_hits(query, &mut hits);
    let mut out = if hits.is_empty() {
        "no results".into()
    } else {
        format_results(&hits)
    };
    if dropped > 0 {
        let _ = write!(out, "\n\n(dropped {dropped} off-topic results)");
    }
    let discussions: Vec<_> = discussions
        .iter()
        .filter(|hit| {
            !hits
                .iter()
                .any(|web| normalize_url(&web.url) == normalize_url(&hit.url))
        })
        .take(3)
        .collect();
    if !discussions.is_empty() {
        let _ = write!(
            out,
            "\n\ndiscussions:\n{}",
            format_discussion_list(&discussions, hits.len() + 1)
        );
    }
    out
}

/// Perplexity-style numbered results the model cites inline as `[n]`.
pub(super) fn format_results(hits: &[SearchHit]) -> String {
    hits.iter()
        .enumerate()
        .map(|(i, h)| {
            let image = h
                .image
                .as_ref()
                .map_or_else(String::new, |url| format!("\n    image: {url}"));
            format!(
                "[{}] {}\n    {}\n    {}{image}",
                i + 1,
                h.title,
                h.url,
                h.snippet
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}
