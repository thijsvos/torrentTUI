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
    #[serde(default)]
    pub search: SearchConfig,
    #[serde(default)]
    pub privacy: PrivacyConfig,
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
    #[serde(default = "default_true")]
    pub enable_dht: bool,
    /// UPnP is opt-in. Enabling this opens an external port via your router's
    /// IGD/UPnP service, which exposes you to peers outside your LAN.
    #[serde(default)]
    pub enable_upnp: bool,
    /// Download cap in KB/s (KiB/s), where `0` means unlimited. Enforced by
    /// librqbit's token-bucket rate limiter directly in the peer IO path, so
    /// the cap shapes traffic smoothly — no torrent is ever paused to hold a
    /// limit, and changes made with `t` apply live. Values 1-15 are raised to
    /// 16: the limiter grants permits in whole 16 KiB chunks, so a smaller
    /// per-second quota cannot be served.
    #[serde(default)]
    pub max_download_speed_kbps: u64,
    /// Upload cap in KB/s, `0` for unlimited. Same limiter as the download
    /// cap — including the 16 KiB/s floor for nonzero values; the two are
    /// independent buckets and never interact.
    #[serde(default)]
    pub max_upload_speed_kbps: u64,
    /// Bind address for the embedded HTTP API that serves file-stream URLs to
    /// external media players. Default `127.0.0.1:0` (auto-assigned port,
    /// loopback only). The API is mounted read-only (no add/pause/delete
    /// routes) *and* behind HTTP basic auth with a random per-session password
    /// — read-only alone is not enough, because librqbit registers two POST
    /// routes outside that gate. The credentials ride in the stream URL handed
    /// to the media player.
    ///
    /// Binding to a non-loopback host still deserves care: basic auth over
    /// plaintext HTTP is readable by anyone on the path, and the URL (with its
    /// credentials) is visible in the player's argv. The app logs a warning and
    /// shows a status message when it binds off-loopback.
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
pub struct SearchConfig {
    /// Query The Pirate Bay's public JSON API (apibay.org). No account or API
    /// key involved; only the search text is sent, and only when the user
    /// submits a search.
    #[serde(default = "default_true")]
    pub enable_apibay: bool,
    /// Query the torrents-csv.com public API. Same privacy posture as apibay.
    #[serde(default = "default_true")]
    pub enable_torrents_csv: bool,
    /// Per-provider HTTP timeout. Clamped to 1-30 s at use, like
    /// `refresh_rate_ms`, so out-of-range values parse and are quietly bounded.
    #[serde(default = "default_search_timeout_secs")]
    pub timeout_secs: u64,
    /// Cap on merged results shown in the search table. Clamped to 1-500 at
    /// use.
    #[serde(default = "default_search_max_results")]
    pub max_results: usize,
}

/// The `[privacy]` section. Everything here is applied at session creation
/// and needs a restart to change — librqbit builds its connector, blocklist
/// and socket bindings once. Empty strings mean "off" (the same convention as
/// `player.command`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrivacyConfig {
    /// SOCKS5 proxy, `socks5://[user:pass@]host:port` (the only scheme
    /// librqbit accepts). When set, the app locks the session down to what
    /// the proxy can actually carry: outgoing peer connections and HTTP(S)
    /// tracker announces go through the proxy; DHT, incoming connections,
    /// UPnP and local-service discovery are disabled (all of them would
    /// bypass a SOCKS5 proxy); and `udp://` trackers are stripped from
    /// magnet links before they reach the engine. The one gap the app cannot
    /// close: `udp://` trackers embedded in a `.torrent` file are still
    /// announced directly by librqbit — prefer magnets when proxying.
    #[serde(default)]
    pub proxy_url: String,
    /// PeerGuardian `.p2p` blocklist (plain or gzipped): an absolute path, a
    /// `~/` path, a `file://` URL, or an `http(s)://` URL. Applied to both
    /// incoming and outgoing peer connections. Loaded once at startup and
    /// fail-closed: the app refuses to start if the list cannot be fetched, or
    /// if it parses to zero ranges (a wrong-format file would otherwise load
    /// empty and run unprotected behind a green badge). Note an `http(s)`
    /// fetch uses librqbit's own client, bound to neither the proxy nor
    /// `bind_interface` — use a local file when either is set.
    #[serde(default)]
    pub blocklist_url: String,
    /// Bind ALL BitTorrent traffic — DHT, trackers (UDP and HTTP), peer
    /// connections, LSD — to this network interface, e.g. a VPN's `wg0` /
    /// `utun3` / `tun0`. Unlike the proxy this covers every protocol
    /// (including the udp trackers a SOCKS5 proxy cannot), so for VPN users it
    /// is the more complete option; traffic fails instead of escaping if the
    /// interface goes away. Startup fails on an unknown interface name.
    ///
    /// Platform notes: **unsupported on Windows** — the underlying library
    /// errors on any interface name there, so a non-empty value makes the app
    /// refuse to start (macOS/Linux only). And when combined with `proxy_url`,
    /// the binding does NOT cover the proxy's own connections — bind alone is
    /// the fail-safe configuration.
    #[serde(default)]
    pub bind_interface: String,
}

impl PrivacyConfig {
    /// The proxy URL when one is configured; whitespace-only counts as off.
    pub fn proxy_url(&self) -> Option<&str> {
        non_empty(&self.proxy_url)
    }

    /// The proxy URL, scheme-checked. `Err` carries a message ready for the
    /// startup error path — librqbit would reject the scheme anyway, but at
    /// session-creation depth the wording names no config key.
    pub fn checked_proxy_url(&self) -> Result<Option<&str>, String> {
        match self.proxy_url() {
            None => Ok(None),
            Some(url) if url.starts_with("socks5://") => Ok(Some(url)),
            Some(url) => Err(format!(
                "privacy.proxy_url must start with socks5:// (got \"{}\")",
                url
            )),
        }
    }

    /// The interface to bind to, when configured.
    pub fn bind_interface(&self) -> Option<&str> {
        non_empty(&self.bind_interface)
    }

    /// A one-line summary of the privacy posture, for the daemon record.
    /// `None` when nothing is configured.
    ///
    /// This deliberately records the config *as it was when the session
    /// started*, because that is what the session actually applied —
    /// `[privacy]` is read once at session creation. A background session
    /// therefore keeps this posture even after `config.toml` is edited, and a
    /// later launch can say so instead of implying the edit took effect.
    /// The proxy URL is summarized, never echoed: it can carry credentials.
    pub fn summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        if self.proxy_url().is_some() {
            parts.push("proxy".to_string());
        }
        if let Some(iface) = self.bind_interface() {
            parts.push(iface.to_string());
        }
        if non_empty(&self.blocklist_url).is_some() {
            parts.push("blocklist".to_string());
        }
        (!parts.is_empty()).then(|| parts.join("+"))
    }

    /// The blocklist source normalized to a URL: `http(s)://` and `file://`
    /// pass through, anything else is treated as a filesystem path —
    /// `~`-expanded, made absolute, and turned into a `file://` URL via
    /// `Url::from_file_path` so reserved characters (`#`, `?`, `%`, spaces) in
    /// the path are percent-encoded and the host is empty. A hand-built
    /// `format!("file://{path}")` mis-parses on those characters and treats a
    /// relative path's first segment as the host — both fatal, since librqbit
    /// loads the blocklist fail-closed. Returns the raw string if it cannot be
    /// made into a file URL, letting librqbit surface the error.
    pub fn blocklist_url(&self) -> Option<String> {
        let raw = non_empty(&self.blocklist_url)?;
        if raw.starts_with("http://") || raw.starts_with("https://") || raw.starts_with("file://") {
            return Some(raw.to_string());
        }
        let expanded = expand_tilde(raw);
        let abs =
            std::path::absolute(&expanded).unwrap_or_else(|_| PathBuf::from(expanded.clone()));
        match url::Url::from_file_path(&abs) {
            Ok(u) => Some(u.to_string()),
            Err(()) => Some(expanded), // non-absolute; let librqbit report it
        }
    }

    /// True when any privacy feature is on — used by the config tests as a
    /// quick "is anything active" check (the engine derives the same thing
    /// through `NetworkPlan`).
    #[cfg(test)]
    pub fn any_active(&self) -> bool {
        self.proxy_url().is_some()
            || self.bind_interface().is_some()
            || self.blocklist_url().is_some()
    }
}

/// `Some(trimmed)` when the value has non-whitespace content.
fn non_empty(s: &str) -> Option<&str> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
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

fn default_refresh_rate() -> u64 {
    100
}

fn default_http_api_bind() -> String {
    "127.0.0.1:0".to_string()
}

fn default_search_timeout_secs() -> u64 {
    8
}

fn default_search_max_results() -> usize {
    50
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

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            enable_apibay: true,
            enable_torrents_csv: true,
            timeout_secs: default_search_timeout_secs(),
            max_results: default_search_max_results(),
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
        assert!(config.search.enable_apibay);
        assert!(config.search.enable_torrents_csv);
        assert_eq!(config.search.timeout_secs, 8);
        assert_eq!(config.search.max_results, 50);
        // Privacy features are strictly opt-in.
        assert!(!config.privacy.any_active());
        assert!(config.privacy.proxy_url().is_none());
        assert!(config.privacy.bind_interface().is_none());
        assert!(config.privacy.blocklist_url().is_none());
    }

    #[test]
    fn privacy_section_absent_from_toml_means_off() {
        let config: Config = toml::from_str("[network]\nenable_dht = true\n").unwrap();
        assert!(!config.privacy.any_active());
    }

    #[test]
    fn privacy_whitespace_values_count_as_off() {
        let config: Config =
            toml::from_str("[privacy]\nproxy_url = \"  \"\nblocklist_url = \"\"\n").unwrap();
        assert!(!config.privacy.any_active());
        assert_eq!(config.privacy.checked_proxy_url(), Ok(None));
    }

    #[test]
    fn privacy_proxy_url_scheme_is_checked() {
        let good = PrivacyConfig {
            proxy_url: "socks5://user:pass@127.0.0.1:1080".to_string(),
            ..Default::default()
        };
        assert_eq!(
            good.checked_proxy_url(),
            Ok(Some("socks5://user:pass@127.0.0.1:1080"))
        );

        let bad = PrivacyConfig {
            proxy_url: "http://127.0.0.1:8080".to_string(),
            ..Default::default()
        };
        let err = bad.checked_proxy_url().unwrap_err();
        // The message must name the config key and the accepted scheme —
        // it is what the startup error path prints.
        assert!(err.contains("privacy.proxy_url"), "got: {err}");
        assert!(err.contains("socks5://"), "got: {err}");
    }

    #[test]
    fn privacy_blocklist_http_and_file_urls_pass_through() {
        for passthrough in [
            "https://example.com/list.p2p.gz",
            "http://example.com/list.p2p",
            "file:///var/lists/block.p2p",
        ] {
            let cfg = PrivacyConfig {
                blocklist_url: passthrough.to_string(),
                ..Default::default()
            };
            assert_eq!(cfg.blocklist_url().as_deref(), Some(passthrough));
        }
    }

    #[test]
    fn privacy_blocklist_path_becomes_a_file_url_that_round_trips() {
        // The contract that matters cross-platform: whatever blocklist_url()
        // emits must survive librqbit's `Url::parse(...).to_file_path()` and
        // come back as the absolute path. Asserting the exact string would be
        // Unix-shaped and fail on the Windows CI leg (drive letters,
        // backslashes), so round-trip through the same parser librqbit uses.
        let src = if cfg!(windows) {
            r"C:\lists\block.p2p"
        } else {
            "/var/lists/block.p2p"
        };
        let cfg = PrivacyConfig {
            blocklist_url: src.to_string(),
            ..Default::default()
        };
        let url = cfg.blocklist_url().expect("absolute path yields a url");
        let back = url::Url::parse(&url)
            .expect("valid url")
            .to_file_path()
            .expect("file url");
        assert_eq!(back, std::path::absolute(src).unwrap());
    }

    #[test]
    fn privacy_blocklist_tilde_path_expands_against_home() {
        let Some(home) = dirs::home_dir() else {
            return; // No home in this environment — nothing to assert.
        };
        let cfg = PrivacyConfig {
            blocklist_url: "~/lists/block.p2p".to_string(),
            ..Default::default()
        };
        let url = cfg.blocklist_url().expect("tilde path yields a url");
        let back = url::Url::parse(&url)
            .expect("valid url")
            .to_file_path()
            .expect("file url");
        assert_eq!(back, home.join("lists").join("block.p2p"));
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
        assert!(config.search.enable_apibay);
        assert!(config.search.enable_torrents_csv);
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

[search]
enable_apibay = false
enable_torrents_csv = true
timeout_secs = 3
max_results = 10
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.general.download_dir, "/tmp/downloads");
        assert!(!config.general.confirm_on_quit);
        assert_eq!(
            config.general.watch_dir.as_deref(),
            Some("/var/torrents/watch")
        );
        assert_eq!(config.network.listen_port, 7000);
        assert!(!config.network.enable_dht);
        assert!(config.network.enable_upnp);
        assert_eq!(config.network.max_download_speed_kbps, 500);
        assert_eq!(config.network.max_upload_speed_kbps, 100);
        assert_eq!(config.network.http_api_bind, "127.0.0.1:8731");
        assert_eq!(config.ui.refresh_rate_ms, 200);
        assert!(!config.ui.enable_notifications);
        assert_eq!(config.player.command, "mpv");
        assert_eq!(config.player.args, vec!["--no-terminal".to_string()]);
        assert!(!config.search.enable_apibay);
        assert!(config.search.enable_torrents_csv);
        assert_eq!(config.search.timeout_secs, 3);
        assert_eq!(config.search.max_results, 10);
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

    #[test]
    fn test_search_section_omitted_defaults_apply() {
        // A config file written before the [search] section existed should
        // still parse cleanly and produce the enabled defaults.
        let toml_str = r#"
[general]
download_dir = "/tmp/downloads"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.search.enable_apibay);
        assert!(config.search.enable_torrents_csv);
        assert_eq!(config.search.timeout_secs, 8);
        assert_eq!(config.search.max_results, 50);
    }

    #[test]
    fn test_search_both_providers_disabled_parses() {
        let toml_str = r#"
[search]
enable_apibay = false
enable_torrents_csv = false
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(!config.search.enable_apibay);
        assert!(!config.search.enable_torrents_csv);
    }
}
