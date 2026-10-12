//! Save folders the add picker was confirmed with, newest first, so the next
//! `A` can offer them with `↑`.
//!
//! Kept as `recent_folders.json` in the session directory. Only the session
//! owner runs the picker, and the single-instance lock (`crate::instance`)
//! makes that one process, so there is never a second writer. Written with
//! the same tmp-then-rename idiom as the daemon record, so a reader never
//! sees half a file. The list adds nothing private that the session did not
//! already hold: librqbit's `session.json` next to it records every
//! torrent's folder in full.
//!
//! Best effort throughout. A missing, unreadable, corrupt or newer-format
//! file is an empty list, and a failed save loses only the convenience.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::expand_tilde;

const FILE: &str = "recent_folders.json";

/// Bumped if the format changes, so an older build reads a newer file as
/// empty instead of misreading it.
const SCHEMA: u32 = 1;

const MAX_RECENT: usize = 10;

#[derive(Serialize, Deserialize)]
struct OnDisk {
    schema: u32,
    dirs: Vec<String>,
}

#[derive(Debug, Default)]
pub struct RecentDirs {
    dirs: Vec<String>,
    /// Where to save; `None` keeps the list in memory only (tests).
    path: Option<PathBuf>,
}

/// The form two folder strings are compared in: `~` expanded, trailing
/// separators dropped, so `~/tv/` and `/home/me/tv` are one entry.
fn normalized(dir: &str) -> String {
    let expanded = expand_tilde(dir.trim());
    let trimmed = expanded.trim_end_matches(['/', std::path::MAIN_SEPARATOR]);
    if trimmed.is_empty() {
        // A filesystem root is all separators.
        expanded
    } else {
        trimmed.to_string()
    }
}

impl RecentDirs {
    pub fn load(session_dir: &Path) -> Self {
        let path = session_dir.join(FILE);
        let dirs = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<OnDisk>(&bytes).ok())
            .filter(|on_disk| on_disk.schema == SCHEMA)
            .map(|on_disk| on_disk.dirs)
            .unwrap_or_default();
        Self {
            dirs: dirs.into_iter().take(MAX_RECENT).collect(),
            path: Some(path),
        }
    }

    /// Newest first.
    pub fn list(&self) -> &[String] {
        &self.dirs
    }

    /// Put `dir` at the front, dropping an older copy of it. The download
    /// dir is never stored: the picker always starts on it anyway.
    pub fn push(&mut self, dir: &str, download_dir: &str) {
        let dir = dir.trim();
        let key = normalized(dir);
        if dir.is_empty() || key == normalized(download_dir) {
            return;
        }
        self.dirs.retain(|d| normalized(d) != key);
        self.dirs.insert(0, dir.to_string());
        self.dirs.truncate(MAX_RECENT);
    }

    pub fn save(&self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let body = serde_json::to_vec_pretty(&OnDisk {
            schema: SCHEMA,
            dirs: self.dirs.clone(),
        })?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, body)?;
        fs::rename(&tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_first_without_duplicates_capped() {
        let mut r = RecentDirs::default();
        r.push("/a", "/dl");
        r.push("/b", "/dl");
        r.push("/a/", "/dl"); // the same folder as /a
        assert_eq!(r.list(), ["/a/", "/b"]);
        for i in 0..20 {
            r.push(&format!("/d{i}"), "/dl");
        }
        assert_eq!(r.list().len(), MAX_RECENT);
        assert_eq!(r.list()[0], "/d19");
    }

    #[test]
    fn the_download_dir_and_blanks_are_not_stored() {
        let mut r = RecentDirs::default();
        r.push("/dl/", "/dl");
        r.push("   ", "/dl");
        assert!(r.list().is_empty());
    }

    #[test]
    fn tilde_and_absolute_forms_are_one_folder() {
        let home = expand_tilde("~");
        let mut r = RecentDirs::default();
        r.push(&format!("{home}/tv"), "/dl");
        r.push("~/tv", "/dl");
        assert_eq!(r.list(), ["~/tv"]);
    }

    #[test]
    fn round_trips_through_the_session_dir() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = RecentDirs::load(dir.path());
        assert!(r.list().is_empty(), "no file yet");
        r.push("/srv/tv", "/dl");
        r.push("~/films", "/dl");
        r.save().unwrap();
        let again = RecentDirs::load(dir.path());
        assert_eq!(again.list(), ["~/films", "/srv/tv"]);
        assert!(!dir.path().join("recent_folders.json.tmp").exists());
    }

    #[test]
    fn a_corrupt_or_newer_file_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(FILE), b"{ not json").unwrap();
        assert!(RecentDirs::load(dir.path()).list().is_empty());
        fs::write(dir.path().join(FILE), br#"{"schema": 99, "dirs": ["/x"]}"#).unwrap();
        assert!(RecentDirs::load(dir.path()).list().is_empty());
    }
}
