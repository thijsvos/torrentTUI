//! Shell-style Tab completion of paths, for the add bar (`.torrent` files and
//! the folders leading to them) and the add picker's save folder (folders
//! only).
//!
//! [`complete_path`] is pure: the directory listing comes in as a function, so
//! the rules are tested without a filesystem. [`list_subdirs`] and
//! [`list_torrent_sources`] are the real listings, read only on a Tab press.

use std::io;
use std::path::{Path, PathBuf};

use crate::config::expand_tilde;

/// Directory entries read per Tab press. A completion over more than this is
/// not one anybody is reading through.
const MAX_COMPLETION_ENTRIES: usize = 5000;

/// One name a completion can offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub name: String,
    pub is_dir: bool,
}

impl Candidate {
    pub fn dir(name: &str) -> Self {
        Self {
            name: name.to_string(),
            is_dir: true,
        }
    }

    pub fn file(name: &str) -> Self {
        Self {
            name: name.to_string(),
            is_dir: false,
        }
    }

    /// What goes into the field: a folder gets its separator, ready for the
    /// next segment.
    pub fn text(&self) -> String {
        if self.is_dir {
            format!("{}{}", self.name, std::path::MAIN_SEPARATOR)
        } else {
            self.name.clone()
        }
    }
}

/// What a Tab press produced.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Completion {
    /// The new text, when the completion moved it forward.
    pub input: Option<String>,
    /// What was typed before the segment being completed — kept as typed, so
    /// a candidate goes after it.
    pub head: String,
    /// Every match, sorted, when there is more than one.
    pub candidates: Vec<Candidate>,
    /// Nothing in that folder starts with what was typed.
    pub no_match: bool,
}

fn is_separator(c: char) -> bool {
    c == '/' || (cfg!(windows) && c == '\\')
}

/// Complete the last path segment of `input` against what `list` reports for
/// its folder. A unique match completes (a folder with a trailing separator,
/// ready for the next segment); several complete as far as they agree and are
/// returned as candidates. What the user typed before the last segment is kept
/// as typed — `~/Dow` becomes `~/Downloads/`, not `/home/me/Downloads/`. A
/// relative path is taken relative to `base`. Hidden entries only show up once
/// the segment starts with a `.`.
pub fn complete_path(
    input: &str,
    base: &str,
    list: impl Fn(&Path) -> io::Result<Vec<Candidate>>,
) -> Completion {
    let (head, fragment) = match input.rfind(is_separator) {
        Some(i) => input.split_at(i + 1),
        None => ("", input),
    };
    // A bare `~` names a folder, but it is only a prefix once it has its
    // separator.
    if head.is_empty() && fragment == "~" {
        return Completion {
            input: Some(format!("~{}", std::path::MAIN_SEPARATOR)),
            ..Default::default()
        };
    }
    let dir = if head.trim().is_empty() {
        PathBuf::from(base)
    } else {
        let path = PathBuf::from(expand_tilde(head.trim()));
        if path.is_absolute() {
            path
        } else {
            Path::new(base).join(path)
        }
    };
    let none = Completion {
        head: head.to_string(),
        no_match: true,
        ..Default::default()
    };
    let Ok(entries) = list(&dir) else {
        return none;
    };
    let mut matches: Vec<Candidate> = entries
        .into_iter()
        .filter(|c| c.name.starts_with(fragment))
        .filter(|c| !c.name.starts_with('.') || fragment.starts_with('.'))
        .collect();
    matches.sort_by(|a, b| a.name.cmp(&b.name));
    matches.dedup_by(|a, b| a.name == b.name);
    match matches.as_slice() {
        [] => none,
        [only] => Completion {
            input: Some(format!("{head}{}", only.text())),
            head: head.to_string(),
            ..Default::default()
        },
        [first, rest @ ..] => {
            let common = rest.iter().fold(first.name.as_str(), |acc, c| {
                let len = acc
                    .char_indices()
                    .zip(c.name.chars())
                    .take_while(|((_, a), b)| a == b)
                    .last()
                    .map_or(0, |((i, a), _)| i + a.len_utf8());
                &acc[..len]
            });
            Completion {
                input: (common.len() > fragment.len()).then(|| format!("{head}{common}")),
                head: head.to_string(),
                candidates: matches.clone(),
                no_match: false,
            }
        }
    }
}

/// The folders in `dir`, symlinks to folders included: the save folder's
/// listing.
pub fn list_subdirs(dir: &Path) -> io::Result<Vec<Candidate>> {
    list(dir, false)
}

/// The folders in `dir` and its `.torrent` files: the add bar's listing.
/// Matches the suffix `validate_torrent_source` dispatches on, so whatever
/// completes is also what the add bar accepts.
pub fn list_torrent_sources(dir: &Path) -> io::Result<Vec<Candidate>> {
    list(dir, true)
}

fn list(dir: &Path, torrents: bool) -> io::Result<Vec<Candidate>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir)?
        .take(MAX_COMPLETION_ENTRIES)
        .flatten()
    {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let is_dir = entry
            .file_type()
            .is_ok_and(|t| t.is_dir() || (t.is_symlink() && entry.path().is_dir()));
        if is_dir {
            found.push(Candidate::dir(&name));
        } else if torrents && name.ends_with(".torrent") {
            found.push(Candidate::file(&name));
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(names: &'static [&'static str]) -> impl Fn(&Path) -> io::Result<Vec<Candidate>> {
        move |_| Ok(names.iter().map(|s| Candidate::dir(s)).collect())
    }

    fn names(c: &Completion) -> Vec<&str> {
        c.candidates.iter().map(|c| c.name.as_str()).collect()
    }

    #[test]
    fn a_unique_folder_completes_with_a_separator() {
        let sep = std::path::MAIN_SEPARATOR;
        let done = complete_path("/srv/me", "/dl", dirs(&["media", "backups"]));
        assert_eq!(done.input, Some(format!("/srv/media{sep}")));
        assert!(done.candidates.is_empty());
    }

    #[test]
    fn a_unique_file_completes_without_one() {
        let list = |_: &Path| Ok(vec![Candidate::file("debian.torrent")]);
        let done = complete_path("/t/deb", ".", list);
        assert_eq!(done.input, Some("/t/debian.torrent".to_string()));
    }

    #[test]
    fn several_extend_to_the_common_prefix_and_are_listed() {
        let done = complete_path("/srv/m", "/dl", dirs(&["movies", "movies-4k", "music"]));
        assert_eq!(done.input, None, "m is already the common prefix");
        assert_eq!(names(&done), vec!["movies", "movies-4k", "music"]);
        assert_eq!(done.head, "/srv/");

        let done = complete_path("/srv/mo", "/dl", dirs(&["movies", "movies-4k"]));
        assert_eq!(done.input, Some("/srv/movies".to_string()));
    }

    #[test]
    fn candidates_are_sorted_and_deduplicated() {
        let list = |_: &Path| {
            Ok(vec![
                Candidate::file("b.torrent"),
                Candidate::dir("a"),
                Candidate::dir("a"),
            ])
        };
        let done = complete_path("", ".", list);
        assert_eq!(names(&done), vec!["a", "b.torrent"]);
    }

    #[test]
    fn what_was_typed_is_kept_and_resolved_where_it_points() {
        let seen = std::cell::RefCell::new(Vec::new());
        let list = |dir: &Path| {
            seen.borrow_mut().push(dir.to_path_buf());
            Ok(vec![Candidate::dir("Downloads")])
        };
        let done = complete_path("~/Dow", "/dl", list);
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(done.input, Some(format!("~/Downloads{sep}")));
        assert_eq!(seen.borrow()[0], PathBuf::from(expand_tilde("~/")));

        // No separator at all: a name inside the base folder.
        let seen = std::cell::RefCell::new(Vec::new());
        let list = |dir: &Path| {
            seen.borrow_mut().push(dir.to_path_buf());
            Ok(vec![Candidate::dir("films")])
        };
        complete_path("fi", "/dl", list);
        assert_eq!(seen.borrow()[0], PathBuf::from("/dl"));

        // A relative head is relative to the base too.
        let seen = std::cell::RefCell::new(Vec::new());
        let list = |dir: &Path| {
            seen.borrow_mut().push(dir.to_path_buf());
            Ok(vec![])
        };
        complete_path("tv/s", "/dl", list);
        assert_eq!(seen.borrow()[0], Path::new("/dl").join("tv/"));
    }

    #[test]
    fn dot_entries_stay_hidden_until_asked() {
        let done = complete_path("/h/", "/dl", dirs(&[".cache", "code"]));
        assert_eq!(
            done.input,
            Some(format!("/h/code{}", std::path::MAIN_SEPARATOR))
        );
        let done = complete_path("/h/.", "/dl", dirs(&[".cache", "code"]));
        assert_eq!(
            done.input,
            Some(format!("/h/.cache{}", std::path::MAIN_SEPARATOR))
        );
    }

    #[test]
    fn no_match_and_unreadable_folders_are_reported() {
        assert!(complete_path("/srv/zz", "/dl", dirs(&["media"])).no_match);
        let unreadable = |_: &Path| -> io::Result<Vec<Candidate>> {
            Err(io::Error::new(io::ErrorKind::NotFound, "gone"))
        };
        assert!(complete_path("/nope/x", "/dl", unreadable).no_match);
    }

    #[test]
    fn a_bare_tilde_completes_to_home() {
        let done = complete_path("~", "/dl", dirs(&[]));
        assert_eq!(done.input, Some(format!("~{}", std::path::MAIN_SEPARATOR)));
    }

    #[test]
    fn the_listings_pick_what_each_field_takes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("file.txt"), b"x").unwrap();
        std::fs::write(dir.path().join("a.torrent"), b"x").unwrap();
        std::fs::write(dir.path().join("b.TORRENT"), b"x").unwrap();

        assert_eq!(
            list_subdirs(dir.path()).unwrap(),
            vec![Candidate::dir("sub")]
        );

        let mut found = list_torrent_sources(dir.path()).unwrap();
        found.sort_by(|a, b| a.name.cmp(&b.name));
        // `.TORRENT` is left out because the add bar would read it as a
        // malformed magnet: its suffix check is case-sensitive.
        assert_eq!(
            found,
            vec![Candidate::file("a.torrent"), Candidate::dir("sub")]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_to_a_folder_counts_as_one() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("real")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("link")).unwrap();
        let mut found = list_subdirs(dir.path()).unwrap();
        found.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(found, vec![Candidate::dir("link"), Candidate::dir("real")]);
    }
}
