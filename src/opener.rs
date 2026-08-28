//! Revealing a torrent's data in the system file manager (Finder, Explorer,
//! or the Linux default via xdg-open). Same shape as `player`: a pure
//! command builder that tests can inspect, and a thin detached spawn.
//!
//! The path to reveal comes from the engine (`TorrentInfo::content_path`,
//! librqbit's own resolved output location) — never derived from the torrent
//! name, which is display-sanitized and so can differ from what is on disk,
//! and which on Windows could smuggle a drive-relative prefix through a
//! naive join.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

/// What `o` should show for a torrent.
#[derive(Debug, Clone, PartialEq)]
pub enum RevealTarget {
    /// The torrent's on-disk root — select it in its containing folder where
    /// the platform supports selection.
    Item(PathBuf),
    /// Fallback: just open this directory. Reached when the engine has no
    /// path yet (metadata still resolving) or the path is gone (data moved
    /// or deleted outside the app).
    Dir(PathBuf),
}

/// Resolve the reveal target: the engine-reported content path when it
/// exists on disk, else the download directory.
pub fn resolve_reveal_target(download_dir: &Path, content_path: Option<&Path>) -> RevealTarget {
    if let Some(path) = content_path {
        if path.exists() {
            return RevealTarget::Item(path.to_path_buf());
        }
    }
    RevealTarget::Dir(download_dir.to_path_buf())
}

/// The single argument Explorer wants for select-in-folder. Explorer parses
/// its own command line and treats an unquoted comma as an argument
/// separator, so the path is embedded in literal quotes (`"` cannot occur in
/// a Windows path). Passed via `raw_arg` on Windows so the quotes survive.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn explorer_select_arg(path: &Path) -> String {
    format!("/select,\"{}\"", path.display())
}

/// Build the platform file-manager invocation for a target. Split from
/// [`reveal`] so resolution can be tested without spawning Finder.
pub fn build_command(target: &RevealTarget) -> Command {
    match target {
        RevealTarget::Item(path) => {
            #[cfg(target_os = "macos")]
            {
                // `open -R` reveals the item selected in a Finder window.
                let mut cmd = Command::new("open");
                cmd.arg("-R").arg(path);
                cmd
            }
            #[cfg(target_os = "windows")]
            {
                use std::os::windows::process::CommandExt;
                let mut cmd = Command::new("explorer");
                cmd.raw_arg(explorer_select_arg(path));
                cmd
            }
            #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
            {
                // xdg-open has no selection support; open the containing
                // folder so the item is in view.
                let mut cmd = Command::new("xdg-open");
                cmd.arg(path.parent().unwrap_or(path));
                cmd
            }
        }
        RevealTarget::Dir(path) => {
            #[cfg(target_os = "macos")]
            {
                let mut cmd = Command::new("open");
                cmd.arg(path);
                cmd
            }
            #[cfg(target_os = "windows")]
            {
                let mut cmd = Command::new("explorer");
                cmd.arg(path);
                cmd
            }
            #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
            {
                let mut cmd = Command::new("xdg-open");
                cmd.arg(path);
                cmd
            }
        }
    }
}

/// Open the file manager at `target`. Fire-and-forget like `spawn_player`:
/// spawn detached, drop the handle, surface only the spawn failure.
pub fn reveal(target: &RevealTarget) -> Result<()> {
    let mut cmd = build_command(target);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

    let program = cmd.get_program().to_string_lossy().into_owned();
    let child = cmd
        .spawn()
        .with_context(|| format!("spawn '{}'", program))?;
    drop(child);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args_of(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn existing_content_path_resolves_to_a_selectable_item() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("Some Torrent");
        std::fs::create_dir(&folder).unwrap();
        let target = resolve_reveal_target(dir.path(), Some(&folder));
        assert_eq!(target, RevealTarget::Item(folder));
        // A single file resolves the same way as a folder.
        let file = dir.path().join("solo.iso");
        std::fs::write(&file, b"x").unwrap();
        let target = resolve_reveal_target(dir.path(), Some(&file));
        assert_eq!(target, RevealTarget::Item(file));
    }

    #[test]
    fn missing_or_unknown_content_falls_back_to_the_download_dir() {
        let dir = tempfile::tempdir().unwrap();
        // Metadata not resolved yet: no path at all.
        let target = resolve_reveal_target(dir.path(), None);
        assert_eq!(target, RevealTarget::Dir(dir.path().to_path_buf()));
        // Path known but no longer on disk (moved/deleted outside the app).
        let gone = dir.path().join("moved-away");
        let target = resolve_reveal_target(dir.path(), Some(&gone));
        assert_eq!(target, RevealTarget::Dir(dir.path().to_path_buf()));
    }

    #[test]
    fn explorer_select_arg_quotes_the_path() {
        // Explorer splits its command line on unquoted commas; the embedded
        // quotes keep a comma-containing path as one /select payload.
        let arg = explorer_select_arg(Path::new("/dl/a, b"));
        assert_eq!(arg, "/select,\"/dl/a, b\"");
    }

    #[test]
    fn item_command_selects_where_the_platform_can() {
        let target = RevealTarget::Item(PathBuf::from("/dl/Some Torrent"));
        let cmd = build_command(&target);
        let program = cmd.get_program().to_string_lossy().into_owned();
        #[cfg(target_os = "macos")]
        {
            assert_eq!(program, "open");
            assert_eq!(
                args_of(&cmd),
                vec!["-R".to_string(), "/dl/Some Torrent".to_string()]
            );
        }
        #[cfg(target_os = "windows")]
        {
            // raw_arg contents aren't introspectable through get_args, so
            // only the program is asserted here; explorer_select_arg has its
            // own test above.
            assert_eq!(program, "explorer");
        }
        #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
        {
            assert_eq!(program, "xdg-open");
            assert_eq!(args_of(&cmd), vec!["/dl".to_string()]);
        }
    }

    #[test]
    fn dir_command_opens_the_directory_plainly() {
        let target = RevealTarget::Dir(PathBuf::from("/dl"));
        let cmd = build_command(&target);
        let program = cmd.get_program().to_string_lossy().into_owned();
        #[cfg(target_os = "macos")]
        {
            assert_eq!(program, "open");
        }
        #[cfg(target_os = "windows")]
        {
            assert_eq!(program, "explorer");
        }
        #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
        {
            assert_eq!(program, "xdg-open");
        }
        assert_eq!(args_of(&cmd).last().map(String::as_str), Some("/dl"));
    }
}
