//! The Pirate Bay's public JSON API (apibay.org). No auth, no keys.
//!
//! Response quirks this parser pins down (and the tests fix in place): every
//! field is a JSON *string*, including the numeric ones, and an empty search
//! returns a one-element array whose entry says "No results returned" rather
//! than an empty array.

use serde::Deserialize;

use super::{SearchResult, SourceSet};

const SEARCH_URL: &str = "https://apibay.org/q.php";

pub fn search_url(query: &str) -> String {
    format!("{}?q={}&cat=0", SEARCH_URL, super::encode_query(query))
}

/// The subset of apibay's fields we read; serde ignores the rest (`num_files`,
/// `username`, `added`, `status`, `category`, `imdb`). All strings per the
/// live API.
#[derive(Deserialize)]
struct Entry {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    info_hash: String,
    #[serde(default)]
    seeders: String,
    #[serde(default)]
    leechers: String,
    #[serde(default)]
    size: String,
}

pub fn parse_response(body: &[u8]) -> Result<Vec<SearchResult>, String> {
    // Two-stage parse: whole-body shape first, then per-element decode where a
    // malformed element is skipped instead of failing the provider. The error
    // string is ours, not serde's — serde errors can embed body fragments.
    let values: Vec<serde_json::Value> =
        serde_json::from_slice(body).map_err(|_| "invalid response".to_string())?;
    let mut out = Vec::new();
    for value in values {
        let Ok(entry) = serde_json::from_value::<Entry>(value) else {
            continue;
        };
        // The no-results sentinel is a real-looking entry; without this filter
        // every empty search shows one bogus row with an all-zero hash.
        if entry.id == "0" && entry.name == "No results returned" {
            continue;
        }
        let Some(info_hash) = super::normalize_info_hash(&entry.info_hash) else {
            continue;
        };
        out.push(SearchResult {
            title: super::clean_title(&entry.name),
            info_hash,
            size_bytes: entry.size.trim().parse::<u64>().ok(),
            seeders: entry.seeders.trim().parse::<u64>().unwrap_or(0),
            leechers: entry.leechers.trim().parse::<u64>().unwrap_or(0),
            source: SourceSet {
                apibay: true,
                torrents_csv: false,
            },
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Field shapes captured from the live API (2026-08-28): all strings.
    const TWO_RESULTS: &str = r#"[
        {"id":"13581493","name":"Arch Linux-2016 02 01-dual iso","info_hash":"88066B90278F2DE655EE2DD44E784C340B54E45C","leechers":"0","seeders":"1","num_files":"1","size":"735051776","username":"john80701","added":"1455746145","status":"member","category":"399","imdb":""},
        {"id":"999","name":"Other Release","info_hash":"22B8F63218F1E726EC2F1FB9B38239F95FC6A629","leechers":"2","seeders":"41","num_files":"3","size":"1073741824","username":"u","added":"1609604656","status":"vip","category":"300","imdb":""}
    ]"#;

    const SENTINEL: &str = r#"[
        {"id":"0","name":"No results returned","info_hash":"0000000000000000000000000000000000000000","leechers":"0","seeders":"0","num_files":"0","size":"0","username":"","added":"0","status":"","category":"0","imdb":""}
    ]"#;

    #[test]
    fn parses_string_numerics_and_lowercases_hashes() {
        let results = parse_response(TWO_RESULTS.as_bytes()).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Arch Linux-2016 02 01-dual iso");
        assert_eq!(
            results[0].info_hash,
            "88066b90278f2de655ee2dd44e784c340b54e45c"
        );
        assert_eq!(results[0].seeders, 1);
        assert_eq!(results[0].size_bytes, Some(735051776));
        assert_eq!(results[1].seeders, 41);
        assert_eq!(results[1].leechers, 2);
        assert!(results[0].source.apibay);
        assert!(!results[0].source.torrents_csv);
    }

    #[test]
    fn no_results_sentinel_is_an_empty_list_not_a_row() {
        assert!(parse_response(SENTINEL.as_bytes()).unwrap().is_empty());
    }

    #[test]
    fn malformed_element_is_skipped_not_fatal() {
        let body = r#"[
            {"id":"1","name":"ok","info_hash":"88066B90278F2DE655EE2DD44E784C340B54E45C","leechers":"0","seeders":"5","size":"10"},
            {"id":42,"name":["not","a","string"]},
            {"id":"2","name":"bad hash","info_hash":"tooshort","leechers":"0","seeders":"5","size":"10"}
        ]"#;
        let results = parse_response(body.as_bytes()).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "ok");
    }

    #[test]
    fn unparseable_numerics_degrade_instead_of_failing() {
        let body = r#"[
            {"id":"1","name":"n","info_hash":"88066B90278F2DE655EE2DD44E784C340B54E45C","leechers":"x","seeders":"-3","size":"not-a-number"}
        ]"#;
        let results = parse_response(body.as_bytes()).unwrap();
        assert_eq!(results[0].seeders, 0);
        assert_eq!(results[0].leechers, 0);
        assert_eq!(results[0].size_bytes, None);
    }

    #[test]
    fn html_body_is_an_error() {
        // A Cloudflare challenge page is a 200 with HTML.
        assert!(parse_response(b"<!DOCTYPE html><html>...</html>").is_err());
    }

    #[test]
    fn hostile_title_is_sanitized() {
        // Built via json! so the fixture carries a real ESC (Cc) and RTL
        // override (Cf) after JSON decoding; both must be stripped before the
        // title reaches the table.
        let body = serde_json::json!([{
            "id": "1",
            "name": format!("a{}[31mb{}c", '\u{1b}', '\u{202E}'),
            "info_hash": "88066B90278F2DE655EE2DD44E784C340B54E45C",
            "leechers": "0",
            "seeders": "0",
            "size": "1"
        }])
        .to_string();
        let results = parse_response(body.as_bytes()).unwrap();
        assert_eq!(results[0].title, "a[31mbc");
    }

    #[test]
    fn search_url_encodes_the_query() {
        assert_eq!(
            search_url("arch linux iso"),
            "https://apibay.org/q.php?q=arch%20linux%20iso&cat=0"
        );
    }
}
