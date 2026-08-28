//! Torrent-indexer search against public no-auth JSON APIs.
//!
//! Runs entirely in a detached tokio task — never inside the engine loop,
//! where a slow HTTP call would stall the 100 ms state ticks, and never on
//! the render path. `spawn_search` fires the enabled providers concurrently
//! and sends one aggregated [`SearchOutcome`] back to `run_app` over its own
//! mpsc channel, like the disk-space probe. Staleness is handled by the
//! generation number the outcome carries: the UI drops anything that doesn't
//! match its current generation, so a superseded query can never clobber a
//! newer one no matter when its response lands.
//!
//! Everything in this module and its providers is panic-free by requirement,
//! not style: the release profile sets `panic = "abort"`, so an unwrap in the
//! spawned task would take the whole app down. Parsing is per-entry tolerant —
//! a malformed element is skipped, a failed provider becomes a short error
//! label, and the other provider's results still render.

pub mod apibay;
pub mod torrents_csv;

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::fmt::Write as _;

use crate::config::SearchConfig;
use tokio::sync::mpsc;

/// Per-provider response body cap. Real responses are well under 100 KB; a
/// misbehaving or hostile endpoint must not be able to balloon memory. Same
/// spirit as the engine's `MAX_TORRENT_FILE_SIZE`.
pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// Cap on the search-input buffer, enforced for typing and paste alike.
pub const MAX_QUERY_CHARS: usize = 200;

/// Titles are attacker-controlled; cap them after sanitizing so one absurd
/// entry can't bloat the results table.
const MAX_TITLE_CHARS: usize = 300;

/// Open trackers appended as `tr=` params to locally built magnet links.
/// Best-effort peer discovery hints — DHT (on by default) does the real work
/// if these rot.
const TRACKERS: &[&str] = &[
    "udp://tracker.opentrackr.org:1337/announce",
    "udp://open.tracker.cl:1337/announce",
    "udp://exodus.desync.com:6969/announce",
];

/// Which indexers produced a result. A set rather than an enum so a row
/// deduped across both providers can say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SourceSet {
    pub apibay: bool,
    pub torrents_csv: bool,
}

impl SourceSet {
    pub fn label(self) -> &'static str {
        match (self.apibay, self.torrents_csv) {
            (true, true) => "both",
            (true, false) => "TPB",
            (false, true) => "tcsv",
            (false, false) => "?",
        }
    }

    fn merge(&mut self, other: SourceSet) {
        self.apibay |= other.apibay;
        self.torrents_csv |= other.torrents_csv;
    }
}

/// One indexer hit. Titles are already `sanitize_display`-ed and info hashes
/// validated to exactly 40 lowercase hex chars at parse time, so downstream
/// code (the results table, `build_magnet`) can trust both.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub title: String,
    pub info_hash: String,
    /// `None` when the provider sent an unparseable size; rendered as "?".
    pub size_bytes: Option<u64>,
    pub seeders: u64,
    pub leechers: u64,
    pub source: SourceSet,
}

/// The one message type on the UI-bound search channel.
#[derive(Debug)]
pub struct SearchOutcome {
    /// Copied from the App's search generation at fire time; the UI drops any
    /// outcome whose generation is no longer current.
    pub generation: u64,
    /// Merged, deduped by info hash, sorted seeders-desc, capped.
    pub results: Vec<SearchResult>,
    /// Short per-provider failure notes ("apibay: timed out"). Empty iff every
    /// enabled provider succeeded. Never contains the query or a URL — reqwest
    /// error strings embed the full request URL, and search terms stay out of
    /// the status bar and the warn-level log by design.
    pub provider_errors: Vec<String>,
}

/// Fire-and-forget: spawns the provider fan-out and sends one outcome. A
/// dropped receiver (the app is quitting) just discards the send.
pub fn spawn_search(
    query: String,
    generation: u64,
    client: reqwest::Client,
    config: SearchConfig,
    tx: mpsc::Sender<SearchOutcome>,
) {
    tokio::spawn(async move {
        let outcome = run_search(&client, &config, &query, generation).await;
        let _ = tx.send(outcome).await;
    });
}

async fn run_search(
    client: &reqwest::Client,
    config: &SearchConfig,
    query: &str,
    generation: u64,
) -> SearchOutcome {
    // One aggregated send rather than per-provider streaming: progressive
    // arrival would re-sort the table under the user's cursor, and total
    // latency is already bounded by the client timeout.
    let apibay_fut = async {
        if !config.enable_apibay {
            return None;
        }
        Some(
            match fetch_capped(client, &apibay::search_url(query)).await {
                Ok(body) => apibay::parse_response(&body).map_err(|e| format!("apibay: {e}")),
                Err(e) => Err(format!("apibay: {e}")),
            },
        )
    };
    let tcsv_fut = async {
        if !config.enable_torrents_csv {
            return None;
        }
        Some(
            match fetch_capped(client, &torrents_csv::search_url(query)).await {
                Ok(body) => {
                    torrents_csv::parse_response(&body).map_err(|e| format!("torrents-csv: {e}"))
                }
                Err(e) => Err(format!("torrents-csv: {e}")),
            },
        )
    };
    let (apibay_res, tcsv_res) = tokio::join!(apibay_fut, tcsv_fut);

    let mut lists = Vec::new();
    let mut provider_errors = Vec::new();
    for res in [apibay_res, tcsv_res].into_iter().flatten() {
        match res {
            Ok(list) => lists.push(list),
            Err(label) => {
                tracing::debug!("search provider failed: {label}");
                provider_errors.push(label);
            }
        }
    }

    SearchOutcome {
        generation,
        results: merge_and_rank(lists, config.max_results.clamp(1, 500)),
        provider_errors,
    }
}

/// GET `url` and read the body through a size cap. The cap has to precede
/// buffering — `content_length` is absent under chunked encoding, so
/// `bytes()`/`json()` would buffer unboundedly first. Errors are short labels
/// safe for the status bar; reqwest's own Display embeds the full URL
/// including the query, which must not reach the UI or the warn-level log.
async fn fetch_capped(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, String> {
    let mut resp = client.get(url).send().await.map_err(|e| {
        if e.is_timeout() {
            "timed out".to_string()
        } else if e.is_connect() {
            "connection failed".to_string()
        } else {
            "request failed".to_string()
        }
    })?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("HTTP {}", status.as_u16()));
    }
    let mut body: Vec<u8> = Vec::new();
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
                    return Err("response too large".to_string());
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(e) => {
                return Err(if e.is_timeout() {
                    "timed out".to_string()
                } else {
                    "read failed".to_string()
                });
            }
        }
    }
    Ok(body)
}

/// Dedup by info hash (already lowercased at parse) keeping the best numbers
/// from each side, sort by seeders descending with title as the tiebreak for a
/// deterministic order, and cap the merged list.
pub fn merge_and_rank(lists: Vec<Vec<SearchResult>>, cap: usize) -> Vec<SearchResult> {
    let mut by_hash: HashMap<String, SearchResult> = HashMap::new();
    for result in lists.into_iter().flatten() {
        match by_hash.entry(result.info_hash.clone()) {
            Entry::Occupied(mut e) => {
                let existing = e.get_mut();
                existing.seeders = existing.seeders.max(result.seeders);
                existing.leechers = existing.leechers.max(result.leechers);
                if existing.size_bytes.is_none() {
                    existing.size_bytes = result.size_bytes;
                }
                existing.source.merge(result.source);
            }
            Entry::Vacant(v) => {
                v.insert(result);
            }
        }
    }
    let mut merged: Vec<SearchResult> = by_hash.into_values().collect();
    merged.sort_by(|a, b| {
        b.seeders
            .cmp(&a.seeders)
            .then_with(|| a.title.cmp(&b.title))
    });
    merged.truncate(cap);
    merged
}

/// `magnet:?xt=urn:btih:<hash>&dn=<title>&tr=<tracker>...`. The display name
/// and trackers are percent-encoded with `NON_ALPHANUMERIC`: over-encoding is
/// harmless, while an unencoded `&` or `#` in a title would corrupt the
/// magnet's parameter structure.
pub fn build_magnet(info_hash: &str, title: &str) -> String {
    use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
    let mut magnet = format!(
        "magnet:?xt=urn:btih:{}&dn={}",
        info_hash,
        utf8_percent_encode(title, NON_ALPHANUMERIC)
    );
    for tracker in TRACKERS {
        let _ = write!(
            magnet,
            "&tr={}",
            utf8_percent_encode(tracker, NON_ALPHANUMERIC)
        );
    }
    magnet
}

/// Percent-encode a user query for a `?q=` parameter.
fn encode_query(query: &str) -> String {
    percent_encoding::utf8_percent_encode(query, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// Validate and normalize an indexer-supplied info hash. This string is
/// concatenated into a magnet link, so unlike every other field it has no
/// tolerant fallback: anything that isn't exactly 40 ASCII hex chars rejects
/// the entry.
fn normalize_info_hash(hash: &str) -> Option<String> {
    let hash = hash.trim();
    if hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        Some(hash.to_ascii_lowercase())
    } else {
        None
    }
}

/// Sanitize and cap an indexer-supplied title. Indexer names are the same
/// attacker-controlled-display-string class as torrent metadata names.
fn clean_title(raw: &str) -> String {
    let cleaned = crate::ui::util::sanitize_display(raw);
    let capped: String = cleaned.chars().take(MAX_TITLE_CHARS).collect();
    if capped.trim().is_empty() {
        "(unnamed)".to_string()
    } else {
        capped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(hash: &str, seeders: u64, source: SourceSet) -> SearchResult {
        SearchResult {
            title: format!("t-{hash}"),
            info_hash: hash.to_string(),
            size_bytes: None,
            seeders,
            leechers: 0,
            source,
        }
    }

    const HASH_A: &str = "88066b90278f2de655ee2dd44e784c340b54e45c";
    const HASH_B: &str = "22b8f63218f1e726ec2f1fb9b38239f95fc6a629";

    #[test]
    fn build_magnet_passes_the_manual_flow_gate() {
        // Pins the built magnet to the same validator the `a` add flow uses.
        let magnet = build_magnet(HASH_A, "Arch Linux 2026.08 iso");
        assert!(magnet.starts_with(&format!("magnet:?xt=urn:btih:{HASH_A}&dn=")));
        assert!(crate::ui::input::validate_magnet(&magnet).is_ok());
    }

    #[test]
    fn build_magnet_encodes_hostile_title_chars() {
        let magnet = build_magnet(HASH_A, "a b&c#d?e=f");
        // The dn= value must not introduce raw parameter separators.
        let dn = magnet
            .split("&dn=")
            .nth(1)
            .and_then(|rest| rest.split("&tr=").next())
            .expect("dn param present");
        assert_eq!(dn, "a%20b%26c%23d%3Fe%3Df");
        assert!(crate::ui::input::validate_magnet(&magnet).is_ok());
    }

    #[test]
    fn build_magnet_appends_encoded_trackers() {
        let magnet = build_magnet(HASH_A, "x");
        assert_eq!(magnet.matches("&tr=").count(), TRACKERS.len());
        // `://` must be encoded inside the tr= values.
        assert!(magnet.contains("udp%3A%2F%2F"));
    }

    #[test]
    fn encode_query_handles_unicode_and_separators() {
        assert_eq!(encode_query("arch linux"), "arch%20linux");
        assert_eq!(encode_query("a&b#c"), "a%26b%23c");
        assert_eq!(encode_query("日本"), "%E6%97%A5%E6%9C%AC");
    }

    #[test]
    fn normalize_info_hash_accepts_only_40_hex() {
        assert_eq!(
            normalize_info_hash(&HASH_A.to_uppercase()).as_deref(),
            Some(HASH_A)
        );
        assert!(normalize_info_hash(&HASH_A[..39]).is_none());
        assert!(normalize_info_hash(&format!("{HASH_A}0")).is_none());
        let non_hex = format!("{}g", &HASH_A[..39]);
        assert!(normalize_info_hash(&non_hex).is_none());
        assert!(normalize_info_hash("").is_none());
    }

    #[test]
    fn clean_title_sanitizes_caps_and_defaults() {
        assert_eq!(clean_title("ok name"), "ok name");
        assert_eq!(clean_title("a\u{1b}b\u{202E}c"), "abc");
        assert_eq!(clean_title("  \u{7} "), "(unnamed)");
        let long = "x".repeat(MAX_TITLE_CHARS + 50);
        assert_eq!(clean_title(&long).chars().count(), MAX_TITLE_CHARS);
    }

    #[test]
    fn merge_dedups_across_providers_keeping_best_numbers() {
        let apibay = SourceSet {
            apibay: true,
            ..Default::default()
        };
        let tcsv = SourceSet {
            torrents_csv: true,
            ..Default::default()
        };
        let mut a = result(HASH_A, 5, apibay);
        a.size_bytes = None;
        let mut b = result(HASH_A, 9, tcsv);
        b.size_bytes = Some(700);
        b.leechers = 3;
        let merged = merge_and_rank(vec![vec![a], vec![b]], 50);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].seeders, 9);
        assert_eq!(merged[0].leechers, 3);
        assert_eq!(merged[0].size_bytes, Some(700));
        assert_eq!(merged[0].source.label(), "both");
    }

    #[test]
    fn merge_sorts_by_seeders_desc_and_caps() {
        let src = SourceSet {
            apibay: true,
            ..Default::default()
        };
        let list = vec![result(HASH_A, 1, src), result(HASH_B, 10, src)];
        let merged = merge_and_rank(vec![list.clone()], 50);
        assert_eq!(merged[0].info_hash, HASH_B);
        assert_eq!(merged[1].info_hash, HASH_A);
        assert_eq!(merge_and_rank(vec![list], 1).len(), 1);
    }

    #[test]
    fn merge_of_nothing_is_empty() {
        assert!(merge_and_rank(Vec::new(), 50).is_empty());
    }

    #[test]
    fn source_set_labels() {
        let both = SourceSet {
            apibay: true,
            torrents_csv: true,
        };
        assert_eq!(both.label(), "both");
        assert_eq!(SourceSet::default().label(), "?");
    }
}
