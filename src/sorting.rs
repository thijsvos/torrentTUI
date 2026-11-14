//! Sort orders shared across views, and the comparison every name sort uses.
//!
//! Names sort *naturally*: runs of digits compare by value, so `Episode 2`
//! comes before `Episode 10` and `10` before `10.5`. Plain string order puts
//! `10` before `2`, which scrambles exactly the lists people sort by name
//! (episodes, chapters, pages) unless every number happens to be zero-padded.
//! A file extension is compared last, after the rest of the name, so
//! `Chapter 10.cbz` comes before `Chapter 10.5.cbz` — compared straight
//! through, the `5` would beat the `c` and put 10.5 first.
//!
//! The sorts a user picks are remembered in `config.toml`'s `[sort]` section;
//! [`SortPrefs`] is the snapshot `run_app` compares against what it last
//! saved, so every path that changes a sort (a key, the palette, the add
//! picker) is persisted by the same one check.

use std::cmp::Ordering;
use std::iter::Peekable;

use crate::app::ResultSortColumn;
use crate::config::SortConfig;
use crate::types::SortColumn;

/// Compare two names the way a person reads them: case-insensitive, with
/// digit runs compared as numbers, and the extension only once the rest of
/// the name ties. Names that differ only in case or in leading zeros
/// (`a2` / `A02`) fall back to a plain comparison, so this is a total order
/// and sorting with it is deterministic.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (stem_a, ext_a) = split_extension(a);
    let (stem_b, ext_b) = split_extension(b);
    natural_cmp_folded(stem_a, stem_b)
        .then_with(|| match (ext_a, ext_b) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (Some(x), Some(y)) => natural_cmp_folded(x, y),
        })
        .then_with(|| a.cmp(b))
}

/// `name` split before its extension, when the text after the last dot
/// looks like one: up to eight ASCII letters and digits, at least one a
/// letter. That rules out the `5` of a folder called `Chapter 10.5`, the
/// `04 LTS` of `Ubuntu 22.04 LTS`, and anything holding a path separator — a
/// dot in a folder name is not the file's extension.
fn split_extension(name: &str) -> (&str, Option<&str>) {
    match name.rsplit_once('.') {
        Some((stem, ext))
            if (1..=8).contains(&ext.len())
                && ext.bytes().all(|b| b.is_ascii_alphanumeric())
                && ext.bytes().any(|b| b.is_ascii_alphabetic()) =>
        {
            (stem, Some(ext))
        }
        _ => (name, None),
    }
}

/// The case-folded, number-aware part of [`natural_cmp`].
///
/// Equivalent to comparing token lists, where a token is either one character
/// or a maximal run of ASCII digits. Digit runs compare by value; a digit run
/// against a character compares as its first digit would, which is the same
/// answer for every digit (the digits are contiguous in Unicode), so the
/// order stays transitive.
fn natural_cmp_folded(a: &str, b: &str) -> Ordering {
    let mut a = a.chars().flat_map(char::to_lowercase).peekable();
    let mut b = b.chars().flat_map(char::to_lowercase).peekable();
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let ord = cmp_digit_runs(&mut a, &mut b);
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(&y);
                }
                a.next();
                b.next();
            }
        }
    }
}

/// Consume a digit run from each side and compare them by value. Works on
/// the digits themselves, not a parsed integer, so a 40-digit run cannot
/// overflow anything: leading zeros are skipped, a longer run is a bigger
/// number, and equal lengths compare digit by digit.
fn cmp_digit_runs<I, J>(a: &mut Peekable<I>, b: &mut Peekable<J>) -> Ordering
where
    I: Iterator<Item = char>,
    J: Iterator<Item = char>,
{
    while a.peek() == Some(&'0') {
        a.next();
    }
    while b.peek() == Some(&'0') {
        b.next();
    }
    let mut first_difference = Ordering::Equal;
    loop {
        let da = a.next_if(char::is_ascii_digit);
        let db = b.next_if(char::is_ascii_digit);
        match (da, db) {
            (Some(x), Some(y)) => {
                if first_difference == Ordering::Equal {
                    first_difference = x.cmp(&y);
                }
            }
            (Some(_), None) => return Ordering::Greater,
            (None, Some(_)) => return Ordering::Less,
            (None, None) => return first_difference,
        }
    }
}

/// How a torrent's file list is ordered. One setting for both places that
/// list a torrent's files — the Files tab and the add picker — since both
/// show the same thing and a user who wants episodes in order wants it in
/// each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileSort {
    /// The order the torrent lists its files in.
    TorrentOrder,
    /// Largest first: what usually decides what to skip.
    Size,
    /// Natural order by path, so episodes and chapters read in sequence.
    Name,
}

impl FileSort {
    pub fn next(self) -> Self {
        match self {
            Self::TorrentOrder => Self::Size,
            Self::Size => Self::Name,
            Self::Name => Self::TorrentOrder,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::TorrentOrder => "torrent order",
            Self::Size => "size",
            Self::Name => "name",
        }
    }

    /// The value `[sort] files` stores.
    pub fn config_name(self) -> &'static str {
        match self {
            Self::TorrentOrder => "torrent",
            Self::Size => "size",
            Self::Name => "name",
        }
    }

    /// Read a `[sort] files` value; anything unknown is `None`.
    pub fn from_config_name(name: &str) -> Option<Self> {
        [Self::TorrentOrder, Self::Size, Self::Name]
            .into_iter()
            .find(|s| s.config_name() == name.trim())
    }

    /// The order to show `count` files in, as positions into the file list.
    /// `name` and `size` read file `i`'s path and byte count.
    pub fn order<'a>(
        self,
        count: usize,
        name: impl Fn(usize) -> &'a str,
        size: impl Fn(usize) -> u64,
    ) -> Vec<usize> {
        let mut order: Vec<usize> = (0..count).collect();
        self.sort(&mut order, name, size);
        order
    }

    /// Sort `positions` (a filtered subset, say) in place. Torrent order is
    /// ascending position, so a subset that arrives shuffled comes out right.
    pub fn sort<'a>(
        self,
        positions: &mut [usize],
        name: impl Fn(usize) -> &'a str,
        size: impl Fn(usize) -> u64,
    ) {
        match self {
            Self::TorrentOrder => positions.sort_unstable(),
            // Stable, so equal sizes keep the torrent's order.
            Self::Size => {
                positions.sort_unstable();
                positions.sort_by_key(|&i| std::cmp::Reverse(size(i)));
            }
            Self::Name => positions.sort_by(|&a, &b| natural_cmp(name(a), name(b))),
        }
    }
}

/// Every sort the app remembers between runs. Compared against the last
/// saved copy after each key, and written to `[sort]` when it differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortPrefs {
    pub torrents: SortColumn,
    pub torrents_reversed: bool,
    pub files: FileSort,
    pub search: ResultSortColumn,
    pub search_reversed: bool,
}

impl Default for SortPrefs {
    fn default() -> Self {
        Self {
            torrents: SortColumn::Index,
            torrents_reversed: false,
            files: FileSort::TorrentOrder,
            search: ResultSortColumn::Seeders,
            search_reversed: false,
        }
    }
}

impl SortPrefs {
    /// Read `[sort]`. A value naming no sort falls back to that sort's
    /// default, the way an unknown theme falls back to `system`: a typo or a
    /// value from a newer version must never stop the app from starting.
    pub fn from_config(config: &SortConfig) -> Self {
        let default = Self::default();
        Self {
            torrents: SortColumn::from_config_name(&config.torrents).unwrap_or(default.torrents),
            torrents_reversed: config.torrents_reversed,
            files: FileSort::from_config_name(&config.files).unwrap_or(default.files),
            search: ResultSortColumn::from_config_name(&config.search).unwrap_or(default.search),
            search_reversed: config.search_reversed,
        }
    }

    /// The `[sort]` keys whose value differs from `saved`. Only these are
    /// written, so a `-c sort.…` override for one run stays out of the file
    /// even when the user then changes a different sort.
    pub fn changed_keys(self, saved: SortPrefs) -> Vec<&'static str> {
        let mut keys = Vec::new();
        if self.torrents != saved.torrents {
            keys.push("torrents");
        }
        if self.torrents_reversed != saved.torrents_reversed {
            keys.push("torrents_reversed");
        }
        if self.files != saved.files {
            keys.push("files");
        }
        if self.search != saved.search {
            keys.push("search");
        }
        if self.search_reversed != saved.search_reversed {
            keys.push("search_reversed");
        }
        keys
    }

    /// The `[sort]` section that reads back as these prefs.
    pub fn to_config(self) -> SortConfig {
        SortConfig {
            torrents: self.torrents.config_name().to_string(),
            torrents_reversed: self.torrents_reversed,
            files: self.files.config_name().to_string(),
            search: self.search.config_name().to_string(),
            search_reversed: self.search_reversed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(mut names: Vec<&str>) -> Vec<&str> {
        names.sort_by(|a, b| natural_cmp(a, b));
        names
    }

    #[test]
    fn numbers_sort_by_value_not_by_character() {
        assert_eq!(
            sorted(vec!["10", "2", "1", "10.5", "02", "11", "9"]),
            vec!["1", "02", "2", "9", "10", "10.5", "11"]
        );
    }

    #[test]
    fn episodes_without_zero_padding_read_in_order() {
        assert_eq!(
            sorted(vec![
                "Show/Episode 10.mkv",
                "Show/Episode 2.mkv",
                "Show/Episode 1.mkv",
                "Show/Episode 1.5.mkv",
            ]),
            vec![
                "Show/Episode 1.mkv",
                "Show/Episode 1.5.mkv",
                "Show/Episode 2.mkv",
                "Show/Episode 10.mkv",
            ]
        );
    }

    #[test]
    fn a_half_chapter_sits_between_its_neighbours_despite_the_extension() {
        assert_eq!(
            sorted(vec![
                "Chapter 11.cbz",
                "Chapter 10.5.cbz",
                "Chapter 9.cbz",
                "Chapter 10.cbz",
            ]),
            vec![
                "Chapter 9.cbz",
                "Chapter 10.cbz",
                "Chapter 10.5.cbz",
                "Chapter 11.cbz",
            ]
        );
        // Folders (no extension) and pages behave the same way.
        assert_eq!(
            sorted(vec!["Vol 10.5", "Vol 11", "Vol 10"]),
            vec!["Vol 10", "Vol 10.5", "Vol 11"]
        );
        assert_eq!(
            sorted(vec!["10.5.jpg", "02.jpg", "10.jpg", "1.jpg"]),
            vec!["1.jpg", "02.jpg", "10.jpg", "10.5.jpg"]
        );
    }

    #[test]
    fn only_a_plausible_extension_is_split_off() {
        assert_eq!(split_extension("a.mkv"), ("a", Some("mkv")));
        assert_eq!(split_extension("a.MP4"), ("a", Some("MP4")));
        assert_eq!(split_extension("Vol 10.5"), ("Vol 10.5", None));
        assert_eq!(
            split_extension("Ubuntu 22.04 LTS"),
            ("Ubuntu 22.04 LTS", None)
        );
        assert_eq!(
            split_extension("Show.S01/readme"),
            ("Show.S01/readme", None)
        );
        assert_eq!(
            split_extension("Show.S01\\readme"),
            ("Show.S01\\readme", None)
        );
        assert_eq!(
            split_extension("archive.verylongext"),
            ("archive.verylongext", None)
        );
        assert_eq!(split_extension("noext"), ("noext", None));
    }

    #[test]
    fn the_scrambled_season_from_the_issue_comes_out_in_order() {
        // The Files tab from #110, which lists the torrent's own order.
        let names = vec![
            "Season_01\\s01e01_Tis.the.Season.avi",
            "Season_01\\s01e04_Fugazi.avi",
            "Season_01\\s01e07_Lost.Boys.avi",
            "Season_01\\s01e03_Blue.Religion.avi",
            "Season_01\\s01e10_Us.and.Them.avi",
            "Season_02\\s02e06_Heart.Attack.avi",
            "Season_01\\s01e02_Lost.Light.avi",
        ];
        assert_eq!(
            sorted(names),
            vec![
                "Season_01\\s01e01_Tis.the.Season.avi",
                "Season_01\\s01e02_Lost.Light.avi",
                "Season_01\\s01e03_Blue.Religion.avi",
                "Season_01\\s01e04_Fugazi.avi",
                "Season_01\\s01e07_Lost.Boys.avi",
                "Season_01\\s01e10_Us.and.Them.avi",
                "Season_02\\s02e06_Heart.Attack.avi",
            ]
        );
    }

    #[test]
    fn case_is_ignored_but_still_breaks_ties() {
        assert_eq!(natural_cmp("apple", "Banana"), Ordering::Less);
        assert_eq!(natural_cmp("Banana", "apple"), Ordering::Greater);
        // Equal apart from case or zero padding: still a strict, fixed order.
        assert_ne!(natural_cmp("a", "A"), Ordering::Equal);
        assert_eq!(natural_cmp("a", "A"), natural_cmp("a", "A"));
        assert_ne!(natural_cmp("a2", "a02"), Ordering::Equal);
        assert_eq!(natural_cmp("same", "same"), Ordering::Equal);
    }

    #[test]
    fn huge_numbers_do_not_overflow() {
        let small = format!("x{}", "9".repeat(40));
        let big = format!("x1{}", "0".repeat(40));
        assert_eq!(natural_cmp(&small, &big), Ordering::Less);
        assert_eq!(natural_cmp(&big, &small), Ordering::Greater);
    }

    #[test]
    fn a_shorter_name_that_is_a_prefix_comes_first() {
        assert_eq!(natural_cmp("disc", "disc 2"), Ordering::Less);
        assert_eq!(natural_cmp("vol 1", "vol 1 extras"), Ordering::Less);
    }

    #[test]
    fn non_ascii_names_compare_without_panicking() {
        assert_eq!(
            sorted(vec!["Ärger 10", "ärger 9", "日本 2", "日本 1"]),
            vec!["ärger 9", "Ärger 10", "日本 1", "日本 2"]
        );
    }

    /// `sort_by` needs a total order; a comparator that is not transitive can
    /// leave a list unsorted (or, in newer std, panic). Check every triple of
    /// a set built to mix digits, letters, punctuation and case.
    #[test]
    fn the_order_is_transitive_and_antisymmetric() {
        let names = [
            "", "0", "00", "1", "01", "9", "10", "a", "A", "a1", "a01", "a10", "a.5", "a 5", "a-",
            "b", "B2", ".", "1a", "1.5", "z", "Z0", "é", "É1", "a.mkv", "A.MKV", "a1.mkv",
            "a.5.mkv", "a 10.txt", "1.jpg", "1.5.jpg", "10.jpg", ".nfo", "a.b", "a.b.c",
        ];
        for a in names {
            for b in names {
                assert_eq!(
                    natural_cmp(a, b),
                    natural_cmp(b, a).reverse(),
                    "{a:?} {b:?}"
                );
                for c in names {
                    if natural_cmp(a, b) != Ordering::Greater
                        && natural_cmp(b, c) != Ordering::Greater
                    {
                        assert_ne!(
                            natural_cmp(a, c),
                            Ordering::Greater,
                            "{a:?} <= {b:?} <= {c:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn file_sort_cycles_and_round_trips_through_config() {
        let mut s = FileSort::TorrentOrder;
        for expected in [FileSort::Size, FileSort::Name, FileSort::TorrentOrder] {
            s = s.next();
            assert_eq!(s, expected);
            assert_eq!(FileSort::from_config_name(s.config_name()), Some(s));
        }
        assert_eq!(FileSort::from_config_name("  name "), Some(FileSort::Name));
        assert_eq!(FileSort::from_config_name("natural"), None);
    }

    #[test]
    fn file_sort_orders_positions() {
        let names = ["b10", "b9", "a"];
        let sizes = [5, 50, 5];
        let order = |s: FileSort| s.order(3, |i| names[i], |i| sizes[i]);
        assert_eq!(order(FileSort::TorrentOrder), vec![0, 1, 2]);
        assert_eq!(order(FileSort::Name), vec![2, 1, 0]);
        // Largest first; the tie keeps the torrent's order.
        assert_eq!(order(FileSort::Size), vec![1, 0, 2]);
    }

    #[test]
    fn prefs_read_from_config_fall_back_per_value() {
        let config = SortConfig {
            torrents: "index".to_string(),
            torrents_reversed: true,
            files: "no-such-sort".to_string(),
            search: "age".to_string(),
            search_reversed: false,
        };
        let prefs = SortPrefs::from_config(&config);
        assert_eq!(prefs.torrents, SortColumn::Index);
        assert!(prefs.torrents_reversed);
        assert_eq!(
            prefs.files,
            FileSort::TorrentOrder,
            "unknown value -> default"
        );
        assert_eq!(prefs.search, ResultSortColumn::Age);
        assert_eq!(
            SortPrefs::from_config(&SortConfig::default()),
            SortPrefs::default()
        );
        assert_eq!(SortPrefs::default().to_config(), SortConfig::default());
    }

    #[test]
    fn only_the_changed_keys_are_reported() {
        let saved = SortPrefs::default();
        assert!(saved.changed_keys(saved).is_empty());
        let reversed = SortPrefs {
            torrents_reversed: true,
            ..saved
        };
        assert_eq!(reversed.changed_keys(saved), vec!["torrents_reversed"]);
        let everything = SortPrefs {
            torrents: SortColumn::Name,
            torrents_reversed: true,
            files: FileSort::Name,
            search: ResultSortColumn::Age,
            search_reversed: true,
        };
        assert_eq!(
            everything.changed_keys(saved),
            vec![
                "torrents",
                "torrents_reversed",
                "files",
                "search",
                "search_reversed"
            ]
        );
    }

    #[test]
    fn every_pref_round_trips_through_config() {
        for torrents in SortColumn::ALL {
            for files in [FileSort::TorrentOrder, FileSort::Size, FileSort::Name] {
                for search in [
                    ResultSortColumn::Seeders,
                    ResultSortColumn::Size,
                    ResultSortColumn::Title,
                    ResultSortColumn::Leechers,
                    ResultSortColumn::Age,
                ] {
                    let prefs = SortPrefs {
                        torrents,
                        torrents_reversed: true,
                        files,
                        search,
                        search_reversed: true,
                    };
                    assert_eq!(SortPrefs::from_config(&prefs.to_config()), prefs);
                }
            }
        }
    }
}
