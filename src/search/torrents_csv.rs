//! The torrents-csv.com public API. No auth, no keys.
//!
//! Unlike apibay, numeric fields are actual JSON numbers, hashes come
//! lowercase, and the results ride under a `torrents` key with a pagination
//! cursor we ignore.

use serde::Deserialize;

use super::{SearchResult, SourceSet};

const SEARCH_URL: &str = "https://torrents-csv.com/service/search";

pub fn search_url(query: &str) -> String {
    format!("{}?q={}", SEARCH_URL, super::encode_query(query))
}

#[derive(Deserialize)]
struct Response {
    #[serde(default)]
    torrents: Vec<serde_json::Value>,
}

/// The subset of torrents-csv's fields we read; `created_unix`, `completed`,
/// `scraped_date` and `id` are ignored. `Option`/`default` so a missing field
/// degrades instead of dropping the entry — except the hash, which is
/// validated hard downstream.
#[derive(Deserialize)]
struct Entry {
    #[serde(default)]
    infohash: String,
    #[serde(default)]
    name: String,
    size_bytes: Option<u64>,
    #[serde(default)]
    seeders: u64,
    #[serde(default)]
    leechers: u64,
}

pub fn parse_response(body: &[u8]) -> Result<Vec<SearchResult>, String> {
    // Same two-stage, per-entry-tolerant parse as apibay: a malformed element
    // (e.g. negative seeders failing the u64) is skipped, not fatal, and the
    // user-visible error string is ours rather than serde's.
    let resp: Response =
        serde_json::from_slice(body).map_err(|_| "invalid response".to_string())?;
    let mut out = Vec::new();
    for value in resp.torrents {
        let Ok(entry) = serde_json::from_value::<Entry>(value) else {
            continue;
        };
        let Some(info_hash) = super::normalize_info_hash(&entry.infohash) else {
            continue;
        };
        out.push(SearchResult {
            title: super::clean_title(&entry.name),
            info_hash,
            size_bytes: entry.size_bytes,
            seeders: entry.seeders,
            leechers: entry.leechers,
            source: SourceSet {
                apibay: false,
                torrents_csv: true,
            },
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Field shapes captured from the live API (2026-08-28): numbers are
    // numbers, hashes lowercase.
    const ONE_RESULT: &str = r#"{
        "torrents": [
            {"infohash":"22b8f63218f1e726ec2f1fb9b38239f95fc6a629","name":"Mastering Ubuntu Server, 3rd Edition","size_bytes":20501290,"created_unix":1609604656,"seeders":9,"leechers":0,"completed":25,"scraped_date":1786868523,"id":139805}
        ],
        "next": 668415
    }"#;

    #[test]
    fn parses_numeric_fields() {
        let results = parse_response(ONE_RESULT.as_bytes()).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Mastering Ubuntu Server, 3rd Edition");
        assert_eq!(
            results[0].info_hash,
            "22b8f63218f1e726ec2f1fb9b38239f95fc6a629"
        );
        assert_eq!(results[0].size_bytes, Some(20501290));
        assert_eq!(results[0].seeders, 9);
        assert!(results[0].source.torrents_csv);
        assert!(!results[0].source.apibay);
    }

    #[test]
    fn empty_torrents_array_is_no_results() {
        let body = r#"{"torrents": [], "next": null}"#;
        assert!(parse_response(body.as_bytes()).unwrap().is_empty());
    }

    #[test]
    fn missing_optional_fields_default() {
        let body = r#"{"torrents": [
            {"infohash":"22b8f63218f1e726ec2f1fb9b38239f95fc6a629","name":"n"}
        ]}"#;
        let results = parse_response(body.as_bytes()).unwrap();
        assert_eq!(results[0].size_bytes, None);
        assert_eq!(results[0].seeders, 0);
        assert_eq!(results[0].leechers, 0);
    }

    #[test]
    fn negative_seeders_entry_is_skipped_others_survive() {
        let body = r#"{"torrents": [
            {"infohash":"22b8f63218f1e726ec2f1fb9b38239f95fc6a629","name":"bad","seeders":-1},
            {"infohash":"88066b90278f2de655ee2dd44e784c340b54e45c","name":"good","seeders":3}
        ]}"#;
        let results = parse_response(body.as_bytes()).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "good");
    }

    #[test]
    fn bad_hash_entry_is_skipped() {
        let body = r#"{"torrents": [
            {"infohash":"zz8f63218f1e726ec2f1fb9b38239f95fc6a629","name":"n","seeders":1}
        ]}"#;
        assert!(parse_response(body.as_bytes()).unwrap().is_empty());
    }

    #[test]
    fn html_body_is_an_error() {
        assert!(parse_response(b"<html>blocked</html>").is_err());
    }

    #[test]
    fn search_url_encodes_the_query() {
        assert_eq!(
            search_url("arch linux"),
            "https://torrents-csv.com/service/search?q=arch%20linux"
        );
    }
}
