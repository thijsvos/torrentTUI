//! Watch-folder cleanup that librqbit does not do for us.
//!
//! `Session::watch_folder` reads `.torrent` files out of the watch directory
//! and then leaves them there forever — it never moves, renames or deletes
//! them, and it discards the source path immediately after reading. It also
//! rescans the whole directory on every startup. Deleting a torrent in the TUI
//! therefore only removed it until the next launch, when the leftover file was
//! picked up and re-added (issue #39). This module closes that loop: after a
//! torrent is deleted we delete the watch-folder file carrying its info hash.
//!
//! Everything here mirrors librqbit's own matching rules, because we are
//! deleting files the user put there: anything librqbit would not have added,
//! we must not delete.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::engine::torrent::MAX_TORRENT_FILE_SIZE;

/// How deep to walk below the watch root. librqbit's own scan is unbounded,
/// but it only ever reads; we delete, so we keep a lid on the blast radius.
const MAX_WALK_DEPTH: usize = 8;

/// Hard cap on entries visited in one pass. A `watch_dir` that was mistyped
/// into something enormous should degrade to "did nothing and logged it",
/// not "walked the user's entire home directory looking for files to delete".
const MAX_WALK_ENTRIES: usize = 50_000;

/// A `.magnet` file holds a single URI. 64 KiB is far above any real magnet
/// (even one carrying dozens of `tr=` parameters) and far below anything
/// worth reading into memory.
const MAX_MAGNET_FILE_SIZE: u64 = 64 * 1024;

/// What [`remove_sources`] managed to do. Both halves are surfaced to the
/// user, so keep them cheap to summarize.
#[derive(Debug, Default)]
pub struct RemoveOutcome {
    /// Files successfully deleted.
    pub removed: Vec<PathBuf>,
    /// One human-readable message per file we matched but could not delete
    /// (permissions, read-only mount, another process holding a handle).
    /// Swallowing these silently would reintroduce #39 with no diagnostic.
    pub errors: Vec<String>,
}

/// False for filesystem roots and the user's home directory. Both are
/// plausible typos for a watch folder and catastrophic targets for a
/// recursive delete pass, so cleanup is disabled entirely for them.
pub fn is_safe_watch_root(dir: &Path) -> bool {
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    // Filesystem root, or a bare drive prefix like `C:\` on Windows.
    if dir.parent().is_none() {
        return false;
    }
    if let Some(home) = dirs::home_dir() {
        let home = home.canonicalize().unwrap_or(home);
        if dir == home {
            return false;
        }
    }
    true
}

/// The subtree cleanup must not descend into: the download directory, but only
/// when it sits *inside* the watch folder.
///
/// `watch_dir = "~/torrents"` with `download_dir = "~/torrents/downloads"` is
/// an ordinary config that the engine's watch/download equality check lets
/// through. Without this, cleanup would delete `.torrent` files that ship
/// *inside* downloaded content — even on "keep files", where the user asked
/// for the opposite. Reverse nesting (the watch folder inside the download
/// directory) must return `None`, or the exclusion would swallow the entire
/// watch folder and cleanup would never do anything.
pub fn cleanup_exclude(watch_dir: &Path, download_dir: &Path) -> Option<PathBuf> {
    // `Path::starts_with` compares whole components, so `/a/bcd` correctly
    // does not count as living inside `/a/bc`.
    match (watch_dir.canonicalize(), download_dir.canonicalize()) {
        (Ok(watch), Ok(download)) if download != watch && download.starts_with(&watch) => {
            Some(download)
        }
        _ => None,
    }
}

/// Info hash of a `.torrent` file as lowercase hex, or `None` when the file
/// is unreadable or is not a valid v1 torrent.
fn torrent_file_hash(path: &Path) -> Option<String> {
    let len = std::fs::metadata(path).ok()?.len();
    if len > MAX_TORRENT_FILE_SIZE {
        // librqbit applies no size cap, so it *did* add this one and we are
        // about to leave it behind. Say so — it explains an otherwise
        // baffling reappearance on the next launch.
        tracing::warn!(
            "watch folder: {:?} exceeds {} bytes; skipping it during cleanup",
            path,
            MAX_TORRENT_FILE_SIZE
        );
        return None;
    }
    let buf = std::fs::read(path).ok()?;
    let meta = librqbit::torrent_from_bytes::<librqbit::ByteBuf>(&buf).ok()?;
    Some(meta.info_hash.as_string())
}

/// Info hash of a `.magnet` file. The contents are handed to `Magnet::parse`
/// verbatim, exactly as librqbit does — no trimming, and no lossy UTF-8
/// conversion. librqbit's bare-hex fast path rejects a trailing newline, and
/// a file it rejected was never the source of a torrent, so accepting more
/// than librqbit does here would mean deleting an unrelated file.
fn magnet_file_hash(path: &Path) -> Option<String> {
    let len = std::fs::metadata(path).ok()?.len();
    if len > MAX_MAGNET_FILE_SIZE {
        return None;
    }
    let url = std::fs::read_to_string(path).ok()?;
    // `as_id20` is `None` for a v2-only magnet, which librqbit rejects too.
    librqbit::Magnet::parse(&url)
        .ok()?
        .as_id20()
        .map(|h| h.as_string())
}

/// Delete every `.torrent`/`.magnet` file under `watch_dir` whose info hash is
/// in `hashes`. `exclude`, when given, prunes that subtree from the walk — the
/// engine passes the download directory when it sits inside the watch folder,
/// so `.torrent` files shipped *inside* downloaded content are never touched.
///
/// Blocking: call it from `spawn_blocking`.
pub fn remove_sources(
    watch_dir: &Path,
    exclude: Option<&Path>,
    hashes: &HashSet<String>,
) -> RemoveOutcome {
    let mut outcome = RemoveOutcome::default();
    if hashes.is_empty() {
        return outcome;
    }

    // Canonicalize both sides so the `exclude` comparison below is a plain
    // path equality rather than a guess about how the user spelled the paths.
    let root = watch_dir
        .canonicalize()
        .unwrap_or_else(|_| watch_dir.to_path_buf());
    let exclude = exclude.and_then(|p| p.canonicalize().ok());

    let walker = WalkDir::new(&root)
        .max_depth(MAX_WALK_DEPTH)
        .into_iter()
        .filter_entry(|e| match &exclude {
            Some(ex) => e.path() != ex,
            None => true,
        });

    let mut visited = 0usize;
    // Unreadable entries (permissions, a directory deleted mid-walk) are
    // skipped rather than reported: they are not files we matched.
    for entry in walker.filter_map(|e| e.ok()) {
        visited += 1;
        if visited > MAX_WALK_ENTRIES {
            tracing::warn!(
                "watch folder: stopped scanning {:?} after {} entries",
                root,
                MAX_WALK_ENTRIES
            );
            break;
        }
        // With walkdir's default `follow_links(false)` a symlink is reported
        // as a symlink entry, so this skips symlinks as well — matching
        // librqbit's startup scan, which never adds a symlinked `.torrent`.
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        // Case-sensitive, like librqbit's literal `match` on the extension:
        // it never adds `Foo.TORRENT`, so that file is never ours to delete.
        let hash = match path.extension().and_then(|e| e.to_str()) {
            Some("torrent") => torrent_file_hash(path),
            Some("magnet") => magnet_file_hash(path),
            _ => continue,
        };
        let Some(hash) = hash else { continue };
        if !hashes.contains(&hash) {
            continue;
        }
        match std::fs::remove_file(path) {
            Ok(()) => {
                tracing::info!("watch folder: removed {:?}", path);
                outcome.removed.push(path.to_path_buf());
            }
            Err(e) => {
                outcome.errors.push(format!("{}: {}", path.display(), e));
            }
        }
    }

    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Smallest bencoded v1 torrent librqbit will parse. `pieces` and
    /// `piece length` are non-`Option` in `TorrentMetaV1Info`, so both must be
    /// present; info-dict keys are in the lexicographic order bencode wants.
    fn minimal_torrent(name: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"d4:infod6:lengthi1e");
        buf.extend_from_slice(format!("4:name{}:{}", name.len(), name).as_bytes());
        buf.extend_from_slice(b"12:piece lengthi16384e6:pieces20:");
        buf.extend_from_slice(&[0u8; 20]);
        buf.extend_from_slice(b"ee");
        buf
    }

    /// Read the hash back out of the bytes rather than hardcoding one — the
    /// digest is taken over the raw `info` slice, so it has to come from the
    /// same parser the production code uses.
    fn hash_of(bytes: &[u8]) -> String {
        librqbit::torrent_from_bytes::<librqbit::ByteBuf>(bytes)
            .expect("test torrent should parse")
            .info_hash
            .as_string()
    }

    fn set(hashes: &[&str]) -> HashSet<String> {
        hashes.iter().map(|h| h.to_string()).collect()
    }

    #[test]
    fn removes_matching_torrent_and_leaves_others() {
        let dir = tempfile::tempdir().unwrap();
        let wanted = minimal_torrent("wanted");
        let other = minimal_torrent("other");
        fs::write(dir.path().join("wanted.torrent"), &wanted).unwrap();
        fs::write(dir.path().join("other.torrent"), &other).unwrap();

        let outcome = remove_sources(dir.path(), None, &set(&[&hash_of(&wanted)]));

        assert_eq!(outcome.removed.len(), 1);
        assert!(outcome.errors.is_empty());
        assert!(!dir.path().join("wanted.torrent").exists());
        assert!(dir.path().join("other.torrent").exists());
    }

    #[test]
    fn finds_torrent_in_subdirectory() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        let bytes = minimal_torrent("nested");
        fs::write(nested.join("nested.torrent"), &bytes).unwrap();

        let outcome = remove_sources(dir.path(), None, &set(&[&hash_of(&bytes)]));

        assert_eq!(outcome.removed.len(), 1);
        assert!(!nested.join("nested.torrent").exists());
    }

    #[test]
    fn removes_duplicate_copies_of_the_same_torrent() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = minimal_torrent("dup");
        fs::write(dir.path().join("one.torrent"), &bytes).unwrap();
        fs::write(dir.path().join("two.torrent"), &bytes).unwrap();

        let outcome = remove_sources(dir.path(), None, &set(&[&hash_of(&bytes)]));

        assert_eq!(outcome.removed.len(), 2);
    }

    #[test]
    fn removes_matching_magnet_file() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = minimal_torrent("magnetic");
        let hash = hash_of(&bytes);
        fs::write(
            dir.path().join("link.magnet"),
            format!("magnet:?xt=urn:btih:{}", hash),
        )
        .unwrap();

        let outcome = remove_sources(dir.path(), None, &set(&[&hash]));

        assert_eq!(outcome.removed.len(), 1);
        assert!(!dir.path().join("link.magnet").exists());
    }

    #[test]
    fn leaves_magnet_file_librqbit_would_reject() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = minimal_torrent("bare-hex");
        let hash = hash_of(&bytes);
        // A bare 40-char hash is valid input to librqbit, but only at exactly
        // 40 bytes — the trailing newline pushes it onto the URL path, where
        // it fails for want of a scheme. We must reject it too, or we would
        // delete a file that never produced a torrent.
        fs::write(dir.path().join("bare.magnet"), format!("{}\n", hash)).unwrap();

        let outcome = remove_sources(dir.path(), None, &set(&[&hash]));

        assert!(outcome.removed.is_empty());
        assert!(dir.path().join("bare.magnet").exists());
    }

    #[test]
    fn skips_corrupt_torrent_without_reporting_an_error() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("truncated.torrent"), b"d4:infod6:length").unwrap();

        let outcome = remove_sources(dir.path(), None, &set(&["0".repeat(40).as_str()]));

        assert!(outcome.removed.is_empty());
        assert!(outcome.errors.is_empty());
        assert!(dir.path().join("truncated.torrent").exists());
    }

    #[test]
    fn ignores_uppercase_extension() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = minimal_torrent("shouty");
        fs::write(dir.path().join("SHOUTY.TORRENT"), &bytes).unwrap();

        let outcome = remove_sources(dir.path(), None, &set(&[&hash_of(&bytes)]));

        assert!(outcome.removed.is_empty());
        assert!(dir.path().join("SHOUTY.TORRENT").exists());
    }

    #[test]
    fn prunes_excluded_subtree() {
        let dir = tempfile::tempdir().unwrap();
        let downloads = dir.path().join("downloads");
        fs::create_dir_all(downloads.join("release")).unwrap();
        let bytes = minimal_torrent("shipped");
        fs::write(dir.path().join("shipped.torrent"), &bytes).unwrap();
        fs::write(downloads.join("release").join("shipped.torrent"), &bytes).unwrap();

        let outcome = remove_sources(dir.path(), Some(&downloads), &set(&[&hash_of(&bytes)]));

        assert_eq!(outcome.removed.len(), 1);
        assert!(!dir.path().join("shipped.torrent").exists());
        assert!(downloads.join("release").join("shipped.torrent").exists());
    }

    #[test]
    fn empty_hash_set_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = minimal_torrent("untouched");
        fs::write(dir.path().join("untouched.torrent"), &bytes).unwrap();

        let outcome = remove_sources(dir.path(), None, &HashSet::new());

        assert!(outcome.removed.is_empty());
        assert!(dir.path().join("untouched.torrent").exists());
    }

    #[test]
    fn missing_watch_dir_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("does-not-exist");

        let outcome = remove_sources(&gone, None, &set(&["0".repeat(40).as_str()]));

        assert!(outcome.removed.is_empty());
        assert!(outcome.errors.is_empty());
    }

    #[test]
    fn rejects_dangerous_watch_roots() {
        let dir = tempfile::tempdir().unwrap();
        assert!(is_safe_watch_root(dir.path()));
        assert!(!is_safe_watch_root(Path::new(
            std::path::Component::RootDir.as_os_str()
        )));
        if let Some(home) = dirs::home_dir() {
            assert!(!is_safe_watch_root(&home));
        }
    }

    #[test]
    fn excludes_download_dir_nested_in_watch_dir() {
        let dir = tempfile::tempdir().unwrap();
        let watch = dir.path().join("torrents");
        let downloads = watch.join("downloads");
        fs::create_dir_all(&downloads).unwrap();

        assert_eq!(
            cleanup_exclude(&watch, &downloads),
            Some(downloads.canonicalize().unwrap())
        );
    }

    #[test]
    fn does_not_exclude_sibling_or_enclosing_download_dir() {
        let dir = tempfile::tempdir().unwrap();
        let watch = dir.path().join("watch");
        let sibling = dir.path().join("downloads");
        fs::create_dir_all(&watch).unwrap();
        fs::create_dir_all(&sibling).unwrap();

        // Siblings: nothing to prune.
        assert_eq!(cleanup_exclude(&watch, &sibling), None);
        // A name that merely shares a prefix is not nested.
        let lookalike = dir.path().join("watch-archive");
        fs::create_dir_all(&lookalike).unwrap();
        assert_eq!(cleanup_exclude(&watch, &lookalike), None);
        // The watch folder living inside the download dir must not prune, or
        // the exclusion would swallow the whole watch folder.
        let inner = sibling.join("watch");
        fs::create_dir_all(&inner).unwrap();
        assert_eq!(cleanup_exclude(&inner, &sibling), None);
        // Identical paths are the engine's separate "would loop" case.
        assert_eq!(cleanup_exclude(&watch, &watch), None);
    }

    #[cfg(unix)]
    #[test]
    fn leaves_symlinked_torrent_alone() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        fs::create_dir_all(&real).unwrap();
        let bytes = minimal_torrent("linked");
        let target = real.join("linked.torrent");
        fs::write(&target, &bytes).unwrap();
        let watch = dir.path().join("watch");
        fs::create_dir_all(&watch).unwrap();
        std::os::unix::fs::symlink(&target, watch.join("linked.torrent")).unwrap();

        let outcome = remove_sources(&watch, None, &set(&[&hash_of(&bytes)]));

        assert!(outcome.removed.is_empty());
        assert!(target.exists());
    }
}
