use std::process::Command;

use anyhow::{Context, Result};

use crate::config::PlayerConfig;

/// Build the `Command` used to open a stream URL in an external player. Split
/// from `spawn_player` so the resolution logic can be tested without actually
/// spawning a process.
///
/// Resolution order:
/// 1. If `config.command` is non-empty, run it with `config.args + [url]`.
/// 2. Otherwise fall back to the OS default opener (`xdg-open` / `open` /
///    `start`).
pub fn build_command(config: &PlayerConfig, url: &str) -> Command {
    if !config.command.is_empty() {
        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args);
        cmd.arg(url);
        return cmd;
    }

    #[cfg(target_os = "macos")]
    {
        let mut cmd = Command::new("open");
        cmd.arg(url);
        cmd
    }
    #[cfg(target_os = "windows")]
    {
        // `cmd /c start "" <url>` — the empty quoted "" is the window title,
        // required so cmd doesn't treat a quoted URL as the title.
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", "start", "", url]);
        cmd
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        let mut cmd = Command::new("xdg-open");
        cmd.arg(url);
        cmd
    }
}

/// Open `url` in the configured (or default) external player. Fire-and-forget:
/// we spawn the process and drop the handle so the player keeps running after
/// TorrentTUI exits. Spawn failure (player binary not found, etc.) is returned
/// to the caller so the status bar can surface a useful message.
pub fn spawn_player(config: &PlayerConfig, url: &str) -> Result<()> {
    let mut cmd = build_command(config, url);
    // Detach: we never read from the player's stdio.
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

    let program = cmd.get_program().to_string_lossy().into_owned();
    let child = cmd
        .spawn()
        .with_context(|| format!("spawn '{}'", program))?;
    // Drop the child handle without waiting — the OS reaps it when it exits.
    drop(child);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn program_of(cmd: &Command) -> String {
        cmd.get_program().to_string_lossy().into_owned()
    }

    fn args_of(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn explicit_command_overrides_os_default() {
        let cfg = PlayerConfig {
            command: "mpv".to_string(),
            args: vec!["--no-terminal".to_string()],
        };
        let cmd = build_command(&cfg, "http://127.0.0.1:8080/torrents/0/stream/1");
        assert_eq!(program_of(&cmd), "mpv");
        assert_eq!(
            args_of(&cmd),
            vec![
                "--no-terminal".to_string(),
                "http://127.0.0.1:8080/torrents/0/stream/1".to_string(),
            ]
        );
    }

    #[test]
    fn empty_command_falls_back_to_os_opener() {
        let cfg = PlayerConfig::default();
        let cmd = build_command(&cfg, "http://example/x");
        let program = program_of(&cmd);
        // Different platforms get different openers, but the URL must end up
        // in argv either way.
        #[cfg(target_os = "macos")]
        {
            assert_eq!(program, "open");
            assert_eq!(args_of(&cmd), vec!["http://example/x".to_string()]);
        }
        #[cfg(target_os = "windows")]
        {
            assert_eq!(program, "cmd");
            assert_eq!(
                args_of(&cmd),
                vec![
                    "/C".to_string(),
                    "start".to_string(),
                    "".to_string(),
                    "http://example/x".to_string(),
                ]
            );
        }
        #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
        {
            assert_eq!(program, "xdg-open");
            assert_eq!(args_of(&cmd), vec!["http://example/x".to_string()]);
        }
    }

    #[test]
    fn explicit_command_with_no_extra_args() {
        let cfg = PlayerConfig {
            command: "vlc".to_string(),
            args: Vec::new(),
        };
        let cmd = build_command(&cfg, "http://x/y");
        assert_eq!(program_of(&cmd), "vlc");
        assert_eq!(args_of(&cmd), vec!["http://x/y".to_string()]);
    }
}
