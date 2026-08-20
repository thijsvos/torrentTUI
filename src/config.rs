use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default)]
    pub network: NetworkConfig,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub player: PlayerConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralConfig {
    #[serde(default = "default_download_dir")]
    pub download_dir: String,
    #[serde(default = "default_true")]
    pub confirm_on_quit: bool,
    /// Directory watched for `.torrent`/`.magnet` files to add automatically.
    /// Disabled if it resolves to the same directory as `download_dir`, which
    /// would loop. Deleting a torrent also deletes the file that added it from
    /// here — unless the directory is the user's home or a filesystem root, in
    /// which case adding still works but that cleanup is switched off.
    #[serde(default)]
    pub watch_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    /// First port of the range the engine binds; it reserves ten consecutive
    /// ports from here. The Dockerfile's EXPOSE range is derived from the same
    /// span, so widening it means editing both.
    #[serde(default = "default_listen_port")]
    pub listen_port: u16,
    #[serde(default = "default_max_peers")]
    pub max_peers_per_torrent: u32,
    #[serde(default = "default_true")]
    pub enable_dht: bool,
    /// UPnP is opt-in. Enabling this opens an external port via your router's
    /// IGD/UPnP service, which exposes you to peers outside your LAN.
    #[serde(default)]
    pub enable_upnp: bool,
    /// Download cap in KB/s, where `0` means unlimited. Enforced by pausing
    /// and unpausing torrents on a duty cycle rather than by rate limiting,
    /// since librqbit offers no limiter — expect capped torrents to show as
    /// "Throttled" in the table.
    #[serde(default)]
    pub max_download_speed_kbps: u64,
    /// Upload cap in KB/s, `0` for unlimited. Same duty-cycle mechanism as the
    /// download cap, but with one global budget instead of a bucket per
    /// torrent.
    #[serde(default)]
    pub max_upload_speed_kbps: u64,
    /// Bind address for the embedded HTTP API that serves file-stream URLs to
    /// external media players. Default `127.0.0.1:0` (auto-assigned port,
    /// loopback only). The API is mounted READ-ONLY (no add/pause/delete
    /// routes), but it is UNAUTHENTICATED: anyone who can reach it can list
    /// your torrents and stream/read your downloaded files. Binding to a
    /// non-loopback host exposes that to every machine on the network, so only
    /// do it on a fully trusted one; the app logs a warning and shows a status
    /// message when it binds off-loopback.
    #[serde(default = "default_http_api_bind")]
    pub http_api_bind: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlayerConfig {
    /// External program used to open stream URLs from the Files tab. Empty
    /// means "use the OS default opener" (`xdg-open` / `open` / `start`).
    /// Examples: `mpv`, `vlc`, `iina`.
    #[serde(default)]
    pub command: String,
    /// Extra arguments inserted before the URL when invoking `command`.
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    /// Idle repaint interval. Clamped to 16-1000 ms at startup, so values
    /// outside that are accepted by the parser and then quietly ignored.
    /// Rendering is change-driven, so this only paces animations and the ageing
    /// out of status messages — it is not a frame rate floor.
    #[serde(default = "default_refresh_rate")]
    pub refresh_rate_ms: u64,
    #[serde(default = "default_true")]
    pub enable_notifications: bool,
}

fn default_download_dir() -> String {
    dirs::download_dir()
        .unwrap_or_else(|| PathBuf::from("./downloads"))
        .join("torrents")
        .to_string_lossy()
        .to_string()
}

fn default_true() -> bool {
    true
}

fn default_listen_port() -> u16 {
    6881
}

fn default_max_peers() -> u32 {
    50
}

fn default_refresh_rate() -> u64 {
    100
}

fn default_http_api_bind() -> String {
    "127.0.0.1:0".to_string()
}

/// Expand a leading `~` in a user-supplied path against the home directory.
/// TOML strings are always quoted and the TUI input dialog never involves a
/// shell, so nothing else ever performs this expansion — without it a
/// `"~/downloads"` value is treated as a relative path and materializes as a
/// literal `./~/downloads` directory under the process working directory
/// (issue #33). `~user` forms and mid-string tildes are valid path characters
/// and stay untouched, as does everything when no home directory is known.
pub fn expand_tilde(path: &str) -> String {
    let Some(home) = dirs::home_dir() else {
        return path.to_string();
    };
    if path == "~" {
        return home.to_string_lossy().into_owned();
    }
    let rest = path.strip_prefix("~/");
    #[cfg(windows)]
    let rest = rest.or_else(|| path.strip_prefix("~\\"));
    match rest {
        Some(rest) => home.join(rest).to_string_lossy().into_owned(),
        None => path.to_string(),
    }
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            download_dir: default_download_dir(),
            confirm_on_quit: true,
            watch_dir: None,
        }
    }
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            listen_port: default_listen_port(),
            max_peers_per_torrent: default_max_peers(),
            enable_dht: true,
            enable_upnp: false,
            max_download_speed_kbps: 0,
            max_upload_speed_kbps: 0,
            http_api_bind: default_http_api_bind(),
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            refresh_rate_ms: default_refresh_rate(),
            enable_notifications: true,
        }
    }
}

impl Config {
    /// The app's per-user directory. Holds `config.toml` and the rotating
    /// `torrenttui.log`, so main.rs calls this before any config is loaded.
    /// Falls back to the process working directory when the platform reports
    /// no config dir, which keeps the app runnable in a container with no HOME
    /// rather than failing at startup.
    pub fn config_dir() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("torrenttui")
    }

    pub fn config_path() -> PathBuf {
        Self::config_dir().join("config.toml")
    }

    /// Load config from disk. Returns `(config, optional_warning)`. The warning
    /// is set when the config file existed but couldn't be parsed; callers
    /// should surface it to the user (the file is still treated as defaults).
    ///
    /// Not a pure read: when no config file exists this writes one containing
    /// the defaults, so a first launch materializes `config.toml` and its
    /// parent directory — and `Err` covers that write as well as the read. A
    /// file that exists but is unparseable is deliberately left alone for the
    /// user to fix.
    /// Tilde expansion runs only on the parsed-file path; the default and
    /// parse-failure paths return values that never contain `~`.
    pub fn load() -> Result<(Self, Option<String>)> {
        let path = Self::config_path();
        if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            match toml::from_str::<Config>(&content) {
                Ok(mut config) => {
                    config.expand_paths();
                    Ok((config, None))
                }
                Err(e) => {
                    let msg = format!("Invalid config file, using defaults: {}", e);
                    tracing::warn!("{msg}");
                    Ok((Config::default(), Some(msg)))
                }
            }
        } else {
            let config = Config::default();
            config.save()?;
            Ok((config, None))
        }
    }

    /// Expand `~` in every user-supplied path the config carries. Called once
    /// at load time — the single choke point before any of these strings reach
    /// `PathBuf::from` (session root, watch folder, disk-space probe) or
    /// `Command::new` (player spawn). Never runs against an existing config
    /// file's on-disk contents, so users' `~` spellings are preserved there.
    pub fn expand_paths(&mut self) {
        self.general.download_dir = expand_tilde(&self.general.download_dir);
        self.general.watch_dir = self.general.watch_dir.take().map(|d| expand_tilde(&d));
        self.player.command = expand_tilde(&self.player.command);
    }

    /// Write the config out, creating the directory if needed. Serializes the
    /// in-memory values, so any `~` the user wrote has already been expanded by
    /// `load` and gets persisted in expanded form.
    pub fn save(&self) -> Result<()> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)?;
        // Atomic write: tmp file + fsync + rename. A power loss or SIGKILL
        // mid-write would otherwise leave the user with a zero-byte config
        // and silently restored defaults on next launch.
        let tmp = path.with_extension("toml.tmp");
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(content.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_defaults() {
        let config = Config::default();
        assert!(config.general.confirm_on_quit);
        assert!(config.general.watch_dir.is_none());
        assert_eq!(config.network.listen_port, 6881);
        assert_eq!(config.network.max_peers_per_torrent, 50);
        assert!(config.network.enable_dht);
        assert!(!config.network.enable_upnp);
        assert_eq!(config.network.max_download_speed_kbps, 0);
        assert_eq!(config.network.max_upload_speed_kbps, 0);
        // Localhost-only by default — never bind the HTTP API to a routable
        // interface without an explicit user decision.
        assert_eq!(config.network.http_api_bind, "127.0.0.1:0");
        assert_eq!(config.ui.refresh_rate_ms, 100);
        assert!(config.ui.enable_notifications);
        assert!(config.player.command.is_empty());
        assert!(config.player.args.is_empty());
    }

    #[test]
    fn test_partial_toml() {
        let toml_str = r#"
[general]
confirm_on_quit = false
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(!config.general.confirm_on_quit);
        assert_eq!(config.network.listen_port, 6881);
        assert_eq!(config.ui.refresh_rate_ms, 100);
        assert!(config.ui.enable_notifications);
    }

    #[test]
    fn test_full_toml() {
        let toml_str = r#"
[general]
download_dir = "/tmp/downloads"
confirm_on_quit = false
watch_dir = "/var/torrents/watch"

[network]
listen_port = 7000
max_peers_per_torrent = 100
enable_dht = false
enable_upnp = true
max_download_speed_kbps = 500
max_upload_speed_kbps = 100
http_api_bind = "127.0.0.1:8731"

[ui]
refresh_rate_ms = 200
enable_notifications = false

[player]
command = "mpv"
args = ["--no-terminal"]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.general.download_dir, "/tmp/downloads");
        assert!(!config.general.confirm_on_quit);
        assert_eq!(
            config.general.watch_dir.as_deref(),
            Some("/var/torrents/watch")
        );
        assert_eq!(config.network.listen_port, 7000);
        assert_eq!(config.network.max_peers_per_torrent, 100);
        assert!(!config.network.enable_dht);
        assert!(config.network.enable_upnp);
        assert_eq!(config.network.max_download_speed_kbps, 500);
        assert_eq!(config.network.max_upload_speed_kbps, 100);
        assert_eq!(config.network.http_api_bind, "127.0.0.1:8731");
        assert_eq!(config.ui.refresh_rate_ms, 200);
        assert!(!config.ui.enable_notifications);
        assert_eq!(config.player.command, "mpv");
        assert_eq!(config.player.args, vec!["--no-terminal".to_string()]);
    }

    #[test]
    fn test_expand_tilde() {
        let home = dirs::home_dir().expect("home dir required for this test");
        assert_eq!(expand_tilde("~"), home.to_string_lossy());
        assert_eq!(
            expand_tilde("~/rtorrent/download"),
            home.join("rtorrent/download").to_string_lossy()
        );
        // Only a leading `~/` is special — `~user` and mid-string tildes are
        // ordinary path characters.
        assert_eq!(expand_tilde("~user/download"), "~user/download");
        assert_eq!(expand_tilde("/tmp/~backup"), "/tmp/~backup");
        assert_eq!(expand_tilde("/absolute/path"), "/absolute/path");
        assert_eq!(expand_tilde("relative/path"), "relative/path");
        assert_eq!(expand_tilde(""), "");
    }

    #[cfg(windows)]
    #[test]
    fn test_expand_tilde_backslash() {
        let home = dirs::home_dir().expect("home dir required for this test");
        assert_eq!(expand_tilde(r"~\dl"), home.join("dl").to_string_lossy());
    }

    #[test]
    fn test_expand_paths_tilde_config() {
        let toml_str = r#"
[general]
download_dir = "~/rtorrent/download"
watch_dir = "~/rtorrent/watch"

[player]
command = "~/bin/mpv"
"#;
        let mut config: Config = toml::from_str(toml_str).unwrap();
        config.expand_paths();
        let home = dirs::home_dir().expect("home dir required for this test");
        assert_eq!(
            config.general.download_dir,
            home.join("rtorrent/download").to_string_lossy()
        );
        assert_eq!(
            config.general.watch_dir.as_deref().unwrap(),
            home.join("rtorrent/watch").to_string_lossy()
        );
        assert_eq!(
            config.player.command,
            home.join("bin/mpv").to_string_lossy()
        );
    }

    #[test]
    fn test_expand_paths_leaves_plain_config_untouched() {
        let mut config = Config::default();
        let download_dir = config.general.download_dir.clone();
        config.expand_paths();
        assert_eq!(config.general.download_dir, download_dir);
        assert!(config.general.watch_dir.is_none());
        assert!(config.player.command.is_empty());
    }

    #[test]
    fn test_player_section_omitted_defaults_apply() {
        // A config file written before the [player] section existed should
        // still parse cleanly and produce the empty defaults.
        let toml_str = r#"
[general]
download_dir = "/tmp/downloads"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.player.command, "");
        assert!(config.player.args.is_empty());
        assert_eq!(config.network.http_api_bind, "127.0.0.1:0");
    }
}
