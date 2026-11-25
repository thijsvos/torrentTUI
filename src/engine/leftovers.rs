//! What deleting a torrent with "Keep files" cleans up: what is left of the
//! files the user did not select (#109).
//!
//! librqbit creates every file of a torrent when it starts it, selected or
//! not, and a piece that straddles a selected and an unselected file is
//! written whole, so an unselected file ends up on disk empty or holding a
//! piece's worth of bytes. While the torrent is in the session those bytes
//! have to stay: they belong to pieces it seeds, and librqbit's resume check
//! samples a piece from every file, so deleting them makes it throw away the
//! fastresume data, re-hash the whole torrent and download the boundary
//! pieces again, recreating the files. Once the torrent is gone nothing needs
//! them, which is also when qBittorrent drops its `.parts` file.
//!
//! "Keep files" means keep what you downloaded. An unselected file that is
//! complete was downloaded (selected earlier, or already on disk when the
//! torrent was added), so it stays; everything else unselected goes.

use std::cmp::Reverse;
use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::{Component, PathBuf};

/// The files of one torrent that a "Keep files" delete removes, captured
/// before the delete: the handle that knows them is gone afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leftovers {
    /// librqbit's per-torrent output folder: the torrent's own subfolder for
    /// a multi-file torrent. Never removed itself.
    pub root: PathBuf,
    /// Paths relative to `root`, as librqbit lays them out.
    pub files: Vec<PathBuf>,
}

/// One file of a torrent as far as the leftover rule cares.
#[derive(Debug, Clone, Copy)]
pub struct FileFacts {
    pub len: u64,
    /// Bytes of verified pieces that fall in this file (librqbit's
    /// `file_progress`). A boundary piece counts here even when the file is
    /// not selected, which is the `1.0%` an unselected file can show.
    pub progress: u64,
    /// BEP 47 padding files never exist on disk.
    pub padding: bool,
}

/// Indices of the files a "Keep files" delete removes. `only_files` is
/// librqbit's selection: `None` means every file is selected, so nothing is
/// left over. A selected file is never a leftover, nor is a complete one; an
/// empty file is complete only in name, so an unselected one goes.
pub fn leftover_indices(files: &[FileFacts], only_files: Option<&[usize]>) -> Vec<usize> {
    let Some(only) = only_files else {
        return Vec::new();
    };
    let mut selected = vec![false; files.len()];
    for &i in only {
        if let Some(s) = selected.get_mut(i) {
            *s = true;
        }
    }
    files
        .iter()
        .enumerate()
        .filter(|&(i, f)| !selected[i] && !f.padding && (f.len == 0 || f.progress < f.len))
        .map(|(i, _)| i)
        .collect()
}

/// What [`remove`] did, summarized for the status bar.
#[derive(Debug, Default)]
pub struct Outcome {
    pub removed: usize,
    pub errors: Vec<String>,
}

/// Delete the leftover files, then any folder that held one and is now
/// empty, deepest first, up to but not including `root`.
///
/// Runs after librqbit has dropped the torrent, which closes its files.
/// Every path is checked to stay inside `root`: a relative path must be plain
/// names only, and the folder it resolves to must still be under `root` once
/// symlinks are followed. Only regular files are deleted; a symlink, or a
/// folder where the file should be, is somebody else's and is left alone. A
/// file that is already gone is not an error.
pub fn remove(leftovers: &Leftovers) -> Outcome {
    let mut outcome = Outcome::default();
    if leftovers.files.is_empty() {
        return outcome;
    }
    let root = match std::fs::canonicalize(&leftovers.root) {
        Ok(root) => root,
        Err(e) if e.kind() == ErrorKind::NotFound => return outcome,
        Err(e) => {
            outcome
                .errors
                .push(format!("{}: {e}", leftovers.root.display()));
            return outcome;
        }
    };

    let mut dirs: HashSet<PathBuf> = HashSet::new();
    for rel in &leftovers.files {
        if rel.as_os_str().is_empty()
            || !rel.components().all(|c| matches!(c, Component::Normal(_)))
        {
            outcome
                .errors
                .push(format!("{}: not a plain relative path", rel.display()));
            continue;
        }
        let path = root.join(rel);
        let Some(folder) = path.parent() else {
            continue;
        };
        match std::fs::canonicalize(folder) {
            Ok(real) if real.starts_with(&root) => {}
            Ok(_) => {
                outcome.errors.push(format!(
                    "{}: resolves outside the torrent folder",
                    path.display()
                ));
                continue;
            }
            Err(e) if e.kind() == ErrorKind::NotFound => continue,
            Err(e) => {
                outcome.errors.push(format!("{}: {e}", path.display()));
                continue;
            }
        }
        let mut parent = rel.parent();
        while let Some(dir) = parent.filter(|d| !d.as_os_str().is_empty()) {
            dirs.insert(dir.to_path_buf());
            parent = dir.parent();
        }
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_file() => match std::fs::remove_file(&path) {
                Ok(()) => outcome.removed += 1,
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => outcome.errors.push(format!("{}: {e}", path.display())),
            },
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => outcome.errors.push(format!("{}: {e}", path.display())),
        }
    }

    let mut dirs: Vec<PathBuf> = dirs.into_iter().collect();
    dirs.sort_by_key(|d| Reverse(d.components().count()));
    for dir in dirs {
        let path = root.join(&dir);
        // `remove_dir` only removes an empty folder, which is the point. A
        // symlink or junction is skipped: removing one would remove the link
        // whatever its target holds.
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_dir()) {
            let _ = std::fs::remove_dir(&path);
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn file(len: u64, progress: u64) -> FileFacts {
        FileFacts {
            len,
            progress,
            padding: false,
        }
    }

    #[test]
    fn nothing_is_left_over_when_every_file_is_selected() {
        let files = [file(10, 0), file(10, 5)];
        assert!(leftover_indices(&files, None).is_empty());
        assert!(leftover_indices(&files, Some(&[0, 1])).is_empty());
    }

    #[test]
    fn unselected_files_that_are_not_complete_are_left_over() {
        // 0: selected; 1: untouched; 2: a boundary piece's worth; 3: complete.
        let files = [file(10, 3), file(10, 0), file(10, 1), file(10, 10)];
        assert_eq!(leftover_indices(&files, Some(&[0])), vec![1, 2]);
    }

    #[test]
    fn an_empty_unselected_file_is_left_over_and_padding_never_is() {
        let files = [
            file(10, 0),
            FileFacts {
                len: 4,
                progress: 0,
                padding: true,
            },
            file(0, 0),
        ];
        assert_eq!(leftover_indices(&files, Some(&[0])), vec![2]);
    }

    #[test]
    fn an_out_of_range_selection_index_is_ignored() {
        let files = [file(10, 0), file(10, 0)];
        assert_eq!(leftover_indices(&files, Some(&[0, 7])), vec![1]);
    }

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn remove_deletes_the_files_and_the_folders_they_empty() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Show");
        write(&root.join("S01/e01.mkv"), b"kept");
        write(&root.join("S01/e02.mkv"), b"x");
        write(&root.join("S02/e01.mkv"), b"");
        write(&root.join("S02/extras/a.nfo"), b"");
        let outcome = remove(&Leftovers {
            root: root.clone(),
            files: vec![
                "S01/e02.mkv".into(),
                "S02/e01.mkv".into(),
                "S02/extras/a.nfo".into(),
            ],
        });
        assert_eq!(outcome.removed, 3);
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        assert!(root.join("S01/e01.mkv").exists());
        assert!(!root.join("S01/e02.mkv").exists());
        // S01 still holds a file; S02 and its subfolder are now empty.
        assert!(root.join("S01").is_dir());
        assert!(!root.join("S02").exists());
        assert!(root.is_dir(), "the torrent's own folder stays");
    }

    #[test]
    fn remove_keeps_a_folder_something_else_was_put_in() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        write(&root.join("extras/a.nfo"), b"");
        write(&root.join("extras/mine.txt"), b"not the torrent's");
        let outcome = remove(&Leftovers {
            root: root.clone(),
            files: vec!["extras/a.nfo".into()],
        });
        assert_eq!(outcome.removed, 1);
        assert!(root.join("extras/mine.txt").exists());
    }

    #[test]
    fn remove_refuses_paths_that_leave_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("t");
        std::fs::create_dir_all(&root).unwrap();
        write(&dir.path().join("outside.txt"), b"keep me");
        for rel in ["../outside.txt", "/etc/passwd", ""] {
            let outcome = remove(&Leftovers {
                root: root.clone(),
                files: vec![rel.into()],
            });
            assert_eq!(outcome.removed, 0, "{rel}");
            assert_eq!(outcome.errors.len(), 1, "{rel}: {:?}", outcome.errors);
        }
        assert!(dir.path().join("outside.txt").exists());
    }

    #[test]
    fn a_missing_file_or_root_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = remove(&Leftovers {
            root: dir.path().to_path_buf(),
            files: vec!["gone.bin".into(), "nested/gone.bin".into()],
        });
        assert_eq!(outcome.removed, 0);
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);

        let outcome = remove(&Leftovers {
            root: dir.path().join("never-created"),
            files: vec!["a.bin".into()],
        });
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
    }

    #[cfg(unix)]
    #[test]
    fn remove_does_not_follow_a_symlinked_folder_out_of_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("t");
        let elsewhere = dir.path().join("elsewhere");
        write(&elsewhere.join("a.bin"), b"keep me");
        std::fs::create_dir_all(&root).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join("sub")).unwrap();
        let outcome = remove(&Leftovers {
            root: root.clone(),
            files: vec!["sub/a.bin".into()],
        });
        assert_eq!(outcome.removed, 0);
        assert_eq!(outcome.errors.len(), 1, "{:?}", outcome.errors);
        assert!(elsewhere.join("a.bin").exists());
        assert!(root.join("sub").exists(), "the link itself is left alone");
    }

    #[cfg(unix)]
    #[test]
    fn remove_leaves_a_symlink_where_a_file_was_expected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("t");
        write(&dir.path().join("target.bin"), b"keep me");
        std::fs::create_dir_all(&root).unwrap();
        std::os::unix::fs::symlink(dir.path().join("target.bin"), root.join("a.bin")).unwrap();
        let outcome = remove(&Leftovers {
            root: root.clone(),
            files: vec!["a.bin".into()],
        });
        assert_eq!(outcome.removed, 0);
        assert!(root.join("a.bin").symlink_metadata().is_ok());
        assert!(dir.path().join("target.bin").exists());
    }
}
