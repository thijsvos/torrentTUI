use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The profile this process runs as, chosen once by [`select_profile`] before
/// anything reads a path. `None` inside means the default profile.
///
/// A process-wide value rather than a parameter because a profile is a
/// property of the whole process: every path the app owns — config, logs, the
/// session and its lock — moves with it, and a caller that missed the
/// parameter would quietly read the default profile's files instead.
static PROFILE: OnceLock<Option<String>> = OnceLock::new();

/// Longest profile name accepted. It ends up in paths and in every hint the
/// app prints, so it is kept short.
const MAX_PROFILE_CHARS: usize = 32;

/// Check a `--profile` name. Letters, digits, `-` and `_` only: the name is a
/// directory name, so anything that could climb out of `profiles/` (`..`, a
/// separator) or mean something to a shell is refused outright.
pub fn check_profile_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("The profile name is empty.".to_string());
    }
    if name.chars().count() > MAX_PROFILE_CHARS {
        return Err(format!(
            "The profile name is too long (limit {MAX_PROFILE_CHARS} characters)."
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "Profile names may only use letters, digits, - and _ (got \"{name}\")."
        ));
    }
    Ok(())
}

/// Run as `profile` for the rest of the process. Called once, from `main`,
/// before any path is read; later calls are ignored.
pub fn select_profile(profile: Option<String>) {
    let _ = PROFILE.set(profile);
}

/// The active profile's name, or `None` for the default one.
pub fn profile() -> Option<&'static str> {
    PROFILE.get().and_then(|p| p.as_deref())
}

/// How to type a `torrenttui` command for this profile, for the hints the app
/// prints: `command("--stop")` is `torrenttui --stop`, or
/// `torrenttui --profile vpn --stop` when running as `vpn`. An empty `args`
/// gives the bare launch command.
pub fn command(args: &str) -> String {
    command_for(profile(), args)
}

fn command_for(profile: Option<&str>, args: &str) -> String {
    let mut out = "torrenttui".to_string();
    if let Some(name) = profile {
        out.push_str(" --profile ");
        out.push_str(name);
    }
    if !args.is_empty() {
        out.push(' ');
        out.push_str(args);
    }
    out
}

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
    /// Colour theme id, chosen from the built-in registry (`theme::THEMES`).
    /// An id matching no theme falls back to `system` at startup, so a stale or
    /// mistyped value never prevents the app from starting. Written back here
    /// whenever the user picks a theme from the in-app selector.
    #[serde(default = "default_theme")]
    pub theme: String,
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

fn default_theme() -> String {
    "system".to_string()
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
            theme: default_theme(),
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
    /// The app's per-user directory for the active profile. Holds
    /// `config.toml`, the rotating `torrenttui.log` and the session, so main.rs
    /// calls this before any config is loaded. A named profile lives in
    /// `profiles/<name>/` below the default one's, so it shares nothing with
    /// it. Falls back to the process working directory when the platform
    /// reports no config dir, which keeps the app runnable in a container with
    /// no HOME rather than failing at startup.
    pub fn config_dir() -> PathBuf {
        let base = Self::default_config_dir();
        match profile() {
            Some(name) => profile_dir(&base, name),
            None => base,
        }
    }

    /// The default profile's directory, whichever profile is active.
    fn default_config_dir() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("torrenttui")
    }

    pub fn config_path() -> PathBuf {
        Self::config_dir().join("config.toml")
    }

    /// Give a named profile its first `config.toml`: a copy of the default
    /// profile's, so a new profile starts as the setup the user already has
    /// and only needs the one setting that differs. Returns a note naming the
    /// file when it made one. Does nothing for the default profile, for a
    /// profile that already has a config, or when there is nothing to copy —
    /// [`Config::load`] then writes the defaults as usual.
    pub fn seed_profile() -> Result<Option<String>> {
        let Some(name) = profile() else {
            return Ok(None);
        };
        let made = seed_profile_at(
            &Self::default_config_dir().join("config.toml"),
            &Self::config_path(),
        )?;
        Ok(made.then(|| {
            format!(
                "New profile \"{name}\": its settings start as a copy of yours, in {}",
                Self::config_path().display()
            )
        }))
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
    ///
    /// `overrides` (from `--set`) are laid over whatever was read — the file,
    /// or the defaults when it did not parse — and are never written anywhere:
    /// the defaults written on a first run are the plain defaults.
    pub fn load(overrides: &[Override]) -> Result<(Self, Option<String>)> {
        load_at(&Self::config_path(), overrides)
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

    /// Persist a theme chosen in the app by editing only `[ui] theme` in the
    /// file on disk.
    ///
    /// Not a re-serialization of the in-memory config, which at runtime is
    /// not what the file says. That would drop comments and unknown keys,
    /// write `~` paths back expanded, bake a one-off `--download-dir` or
    /// `--set` into the file, and — when the file failed to parse and the
    /// session fell back to defaults — replace the user's settings, proxy
    /// included, with those defaults.
    pub fn save_theme(theme: &str) -> Result<()> {
        save_theme_at(&Self::config_path(), theme)
    }
}

/// [`Config::save_theme`] against an explicit path, for tests.
fn save_theme_at(path: &Path, theme: &str) -> Result<()> {
    // Write through a symlink rather than replacing it: dotfile managers link
    // config.toml into a repo, and a rename onto the link would cut it loose.
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        // Deleted while the app ran. A file holding just the theme is valid:
        // every other key falls back to its default.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let mut doc: toml_edit::DocumentMut = content.parse().map_err(|e: toml_edit::TomlError| {
        let line = e
            .span()
            .map(|span| content[..span.start].matches('\n').count() + 1);
        match line {
            Some(line) => anyhow::anyhow!("config.toml line {line}: {}", e.message()),
            None => anyhow::anyhow!("config.toml: {}", e.message()),
        }
    })?;
    let ui = doc.entry("ui").or_insert(toml_edit::table());
    let Some(ui) = ui.as_table_like_mut() else {
        anyhow::bail!("config.toml: `ui` is not a table");
    };
    match ui.get_mut("theme").and_then(toml_edit::Item::as_value_mut) {
        // Replace just the value, keeping its spacing and trailing comment.
        Some(value) => {
            let decor = value.decor().clone();
            *value = toml_edit::Value::from(theme);
            *value.decor_mut() = decor;
        }
        None => {
            ui.insert("theme", toml_edit::value(theme));
        }
    }
    write_atomic(&path, &doc.to_string())
}

/// [`Config::load`] against an explicit path, for tests.
fn load_at(path: &Path, overrides: &[Override]) -> Result<(Config, Option<String>)> {
    let (table, warning) = if path.exists() {
        let content = std::fs::read_to_string(path)?;
        match toml::from_str::<Config>(&content) {
            // Parsed again as a plain table so overrides land on the file's
            // own values, keys this version does not know included.
            Ok(_) => (toml::from_str::<toml::Table>(&content)?, None),
            Err(e) => {
                let msg = format!("Invalid config file, using defaults: {}", e);
                tracing::warn!("{msg}");
                (toml::Table::new(), Some(msg))
            }
        }
    } else {
        write_atomic(path, &toml::to_string_pretty(&Config::default())?)?;
        (toml::Table::new(), None)
    };
    let mut config = with_overrides(table, overrides)?;
    // Tilde expansion runs here, after the overrides, so a `--set` path gets
    // the same `~` handling as one in the file.
    config.expand_paths();
    Ok((config, warning))
}

/// Where a named profile lives below the default profile's directory.
fn profile_dir(base: &Path, name: &str) -> PathBuf {
    base.join("profiles").join(name)
}

/// [`Config::seed_profile`] against explicit paths, for tests. Returns whether
/// it wrote `target`.
fn seed_profile_at(source: &Path, target: &Path) -> Result<bool> {
    if target.exists() {
        return Ok(false);
    }
    let content = match std::fs::read_to_string(source) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    write_atomic(target, &content)?;
    // The copy can hold the same proxy credentials, so it gets the same
    // permissions as the file it came from.
    if let Ok(meta) = std::fs::metadata(source) {
        std::fs::set_permissions(target, meta.permissions())?;
    }
    Ok(true)
}

/// One `--set section.key=value`, checked against the settings this version
/// has. Built only by [`parse_override`].
#[derive(Debug, Clone, PartialEq)]
pub struct Override {
    section: String,
    key: String,
    value: toml::Value,
    /// The argument as given, for passing on to a detached background copy.
    arg: String,
}

impl Override {
    pub fn arg(&self) -> &str {
        &self.arg
    }
}

/// Every setting `--set` may name, with its default value — which also gives
/// the type a value has to have. From the serialized defaults, so a setting
/// added to the config is settable without touching this. `watch_dir` is the
/// one optional setting, and serializing skips it while it is unset, so it is
/// filled in here.
fn known_settings() -> toml::Table {
    let mut config = Config::default();
    config.general.watch_dir = Some(String::new());
    to_table(&config)
}

/// `config` as a TOML table.
fn to_table(config: &Config) -> toml::Table {
    match toml::Value::try_from(config) {
        Ok(toml::Value::Table(table)) => table,
        _ => toml::Table::new(),
    }
}

/// Parse and check a `--set` argument, so a mistake stops the launch with a
/// message instead of being ignored. That matters more here than in the file:
/// a mistyped key would be dropped silently, and an override that made the
/// config fail to parse would fall back to the defaults — the very setting the
/// user meant to add (say, a proxy) gone without a word.
///
/// The value is read the way the setting's type needs: a text setting takes
/// the rest of the argument as it is, so `privacy.proxy_url=socks5://…` needs
/// no TOML quoting (quotes are accepted and removed); anything else is parsed
/// as a TOML value — `true`, `6882`, `["--fs"]`.
pub fn parse_override(arg: &str) -> std::result::Result<Override, String> {
    let Some((name, raw)) = arg.split_once('=') else {
        return Err(
            "expected section.key=value, like privacy.proxy_url=socks5://127.0.0.1:9050"
                .to_string(),
        );
    };
    let name = name.trim();
    let known = known_settings();
    let (section, key) = name.split_once('.').unwrap_or((name, ""));
    let expected = known
        .get(section)
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get(key));
    let Some(expected) = expected else {
        return Err(format!(
            "no setting named \"{name}\". Settings are named section.key, as in config.toml: {}",
            setting_names(&known).join(", ")
        ));
    };
    let value = if expected.is_str() {
        match toml_value(raw) {
            Some(toml::Value::String(s)) => toml::Value::String(s),
            _ => toml::Value::String(raw.to_string()),
        }
    } else {
        toml_value(raw).ok_or_else(|| {
            format!(
                "{name} takes a {} value (got \"{raw}\")",
                expected.type_str()
            )
        })?
    };
    let parsed = Override {
        section: section.to_string(),
        key: key.to_string(),
        value,
        arg: arg.to_string(),
    };
    // The real check: apply it to the defaults and deserialize. That catches
    // what the type alone does not, like a port that does not fit in 16 bits,
    // and since the file is applied the same way, an override that passes
    // here cannot make the real config fail to parse later.
    with_overrides(to_table(&Config::default()), std::slice::from_ref(&parsed))
        .map_err(|e| format!("{name}: {}", e.root_cause()))?;
    Ok(parsed)
}

/// `raw` as a TOML value, or `None` when it is not one.
fn toml_value(raw: &str) -> Option<toml::Value> {
    let doc: toml::Table = toml::from_str(&format!("v = {raw}")).ok()?;
    doc.get("v").cloned()
}

/// `section.key` for every setting, sorted.
fn setting_names(known: &toml::Table) -> Vec<String> {
    let mut names = Vec::new();
    for (section, table) in known {
        if let Some(table) = table.as_table() {
            for key in table.keys() {
                names.push(format!("{section}.{key}"));
            }
        }
    }
    names
}

/// Lay `overrides` over `table` and deserialize the result.
fn with_overrides(mut table: toml::Table, overrides: &[Override]) -> Result<Config> {
    for o in overrides {
        let section = table
            .entry(o.section.clone())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        // A section that is not a table cannot have come from a file that
        // parsed as a `Config`, but replacing it keeps this total.
        if !section.is_table() {
            *section = toml::Value::Table(toml::Table::new());
        }
        if let Some(section) = section.as_table_mut() {
            section.insert(o.key.clone(), o.value.clone());
        }
    }
    Ok(toml::Value::Table(table).try_into::<Config>()?)
}

/// Atomic write: tmp file + fsync + rename. A power loss or SIGKILL mid-write
/// would otherwise leave the user with a zero-byte config and silently
/// restored defaults on next launch. An existing file's permissions carry
/// over, so a config kept private (it can hold proxy credentials) stays so.
fn write_atomic(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("toml.tmp");
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
    }
    if let Ok(meta) = std::fs::metadata(path) {
        std::fs::set_permissions(&tmp, meta.permissions())?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
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
        assert_eq!(config.ui.theme, "system");
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
        // A config written before theming existed still parses and defaults.
        assert_eq!(config.ui.theme, "system");
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
theme = "tokyonight"

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
        assert_eq!(config.ui.theme, "tokyonight");
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

    /// Write `content` as a config file in a fresh temp dir, save `theme`
    /// into it, and return the dir (kept alive), the path and the result.
    fn save_theme_into(
        content: &str,
        theme: &str,
    ) -> (tempfile::TempDir, std::path::PathBuf, Result<()>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, content).unwrap();
        let result = save_theme_at(&path, theme);
        (dir, path, result)
    }

    #[test]
    fn saving_a_theme_changes_that_one_value_and_nothing_else() {
        // Everything a whole-file rewrite used to destroy: comments, an
        // unexpanded `~`, a key this version does not know, key order, and the
        // trailing comment on the theme line itself (the README's sample
        // config carries one).
        let before = r#"# my notes: keep downloads on the NAS
[general]
download_dir = "~/dl"   # tilde on purpose, dotfiles are shared
max_concurrent_downloads = 5

[privacy]
proxy_url = "socks5://127.0.0.1:9050"

[ui]
enable_notifications = false
theme = "system"              # colour theme; see Themes below
"#;
        let (_dir, path, result) = save_theme_into(before, "nord");
        result.unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            after,
            before.replace(r#"theme = "system""#, r#"theme = "nord""#)
        );
        let config: Config = toml::from_str(&after).unwrap();
        assert_eq!(config.ui.theme, "nord");
        assert_eq!(config.general.download_dir, "~/dl");
        assert_eq!(config.privacy.proxy_url, "socks5://127.0.0.1:9050");
    }

    #[test]
    fn saving_a_theme_adds_the_key_or_the_table_when_missing() {
        let (_dir, path, result) = save_theme_into("[ui]\nrefresh_rate_ms = 200\n", "dracula");
        result.unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[ui]\nrefresh_rate_ms = 200\ntheme = \"dracula\"\n"
        );

        let (_dir, path, result) = save_theme_into("[general]\ndownload_dir = \"~/dl\"\n", "nord");
        result.unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[general]\ndownload_dir = \"~/dl\"\n\n[ui]\ntheme = \"nord\"\n"
        );

        // An inline table is a table too.
        let (_dir, path, result) = save_theme_into("ui = { refresh_rate_ms = 200 }\n", "nord");
        result.unwrap();
        let config: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.ui.theme, "nord");
        assert_eq!(config.ui.refresh_rate_ms, 200);
    }

    #[test]
    fn saving_a_theme_leaves_a_file_that_does_not_parse_untouched() {
        // The session is running on defaults because of this typo; writing
        // defaults back would wipe the proxy the user is about to fix.
        let before =
            "[privacy]\nproxy_url = \"socks5://127.0.0.1:9050\"\n[ui\nrefresh_rate_ms = 100\n";
        let (_dir, path, result) = save_theme_into(before, "nord");
        let err = result.unwrap_err().to_string();
        assert!(err.starts_with("config.toml line 3"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn a_parse_error_names_its_own_line_even_at_the_start_of_one() {
        // The error span starts on line 3's first byte; the text before it
        // ends in a newline, which a line count must not stop short at.
        let (_dir, _path, result) = save_theme_into("[ui]\nrefresh_rate_ms = 100\n= 5\n", "nord");
        let err = result.unwrap_err().to_string();
        assert!(err.starts_with("config.toml line 3:"), "{err}");
    }

    #[test]
    fn saving_a_theme_refuses_a_ui_key_that_is_not_a_table() {
        let (_dir, path, result) = save_theme_into("ui = 5\n", "nord");
        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "ui = 5\n");
    }

    #[test]
    fn saving_a_theme_recreates_a_deleted_config_with_just_the_theme() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        save_theme_at(&path, "gruvbox").unwrap();
        let config: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.ui.theme, "gruvbox");
        assert_eq!(config.network.listen_port, 6881, "the rest defaults");
    }

    #[cfg(unix)]
    #[test]
    fn saving_a_theme_keeps_a_private_config_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[privacy]\nproxy_url = \"socks5://u:p@host:1080\"\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        save_theme_at(&path, "nord").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn saving_a_theme_writes_through_a_symlinked_config() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("dotfiles-config.toml");
        let link = dir.path().join("config.toml");
        std::fs::write(&real, "[ui]\n").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        save_theme_at(&link, "nord").unwrap();
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::read_to_string(&real).unwrap(),
            "[ui]\ntheme = \"nord\"\n"
        );
    }

    // -- profiles -------------------------------------------------------------

    #[test]
    fn profile_names_are_plain_directory_names() {
        for good in ["vpn", "work-2", "a_b", "X"] {
            assert_eq!(check_profile_name(good), Ok(()), "{good}");
        }
        for bad in ["", "..", "a/b", "a\\b", "with space", "dot.name", "ü"] {
            assert!(check_profile_name(bad).is_err(), "{bad:?}");
        }
        assert!(check_profile_name(&"a".repeat(MAX_PROFILE_CHARS)).is_ok());
        assert!(check_profile_name(&"a".repeat(MAX_PROFILE_CHARS + 1)).is_err());
    }

    #[test]
    fn hints_carry_the_profile() {
        assert_eq!(command_for(None, ""), "torrenttui");
        assert_eq!(command_for(None, "--stop"), "torrenttui --stop");
        assert_eq!(command_for(Some("vpn"), ""), "torrenttui --profile vpn");
        assert_eq!(
            command_for(Some("vpn"), "--stop"),
            "torrenttui --profile vpn --stop"
        );
    }

    #[test]
    fn a_profile_lives_below_the_default_one() {
        let base = Path::new("/cfg/torrenttui");
        assert_eq!(
            profile_dir(base, "vpn"),
            PathBuf::from("/cfg/torrenttui/profiles/vpn")
        );
    }

    #[test]
    fn a_new_profile_starts_as_a_copy_of_the_default_config() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("config.toml");
        let target = dir.path().join("profiles/vpn/config.toml");
        let content = "# mine\n[general]\ndownload_dir = \"~/dl\"\n";
        std::fs::write(&source, content).unwrap();

        assert!(seed_profile_at(&source, &target).unwrap());
        // Byte for byte: comments and `~` spellings survive.
        assert_eq!(std::fs::read_to_string(&target).unwrap(), content);

        // Once only: the profile's own edits are never overwritten.
        std::fs::write(&target, "[ui]\ntheme = \"nord\"\n").unwrap();
        assert!(!seed_profile_at(&source, &target).unwrap());
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "[ui]\ntheme = \"nord\"\n"
        );
    }

    #[test]
    fn with_no_default_config_a_new_profile_gets_plain_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("profiles/vpn/config.toml");
        assert!(!seed_profile_at(&dir.path().join("config.toml"), &target).unwrap());
        assert!(!target.exists());
        // `load` then writes the defaults, as on any first run.
        let (config, warning) = load_at(&target, &[]).unwrap();
        assert!(warning.is_none());
        assert!(target.exists());
        assert_eq!(config.network.listen_port, 6881);
    }

    #[cfg(unix)]
    #[test]
    fn a_profile_copy_of_a_private_config_stays_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("config.toml");
        std::fs::write(&source, "[privacy]\nproxy_url = \"socks5://u:p@h:1080\"\n").unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600)).unwrap();
        let target = dir.path().join("profiles/vpn/config.toml");
        seed_profile_at(&source, &target).unwrap();
        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    // -- --set ----------------------------------------------------------------

    fn set(args: &[&str]) -> Vec<Override> {
        args.iter().map(|a| parse_override(a).unwrap()).collect()
    }

    #[test]
    fn a_text_setting_takes_the_rest_of_the_argument_as_is() {
        let o = parse_override("privacy.proxy_url=socks5://u:p=w@127.0.0.1:9050").unwrap();
        assert_eq!(
            o.value,
            toml::Value::String("socks5://u:p=w@127.0.0.1:9050".to_string())
        );
        // TOML quoting is accepted, not required.
        let o = parse_override(r#"general.download_dir="~/my dl""#).unwrap();
        assert_eq!(o.value, toml::Value::String("~/my dl".to_string()));
        // Something that happens to read as a number stays text.
        let o = parse_override("network.http_api_bind=8080").unwrap();
        assert_eq!(o.value, toml::Value::String("8080".to_string()));
        // Empty turns a text setting off.
        let o = parse_override("privacy.proxy_url=").unwrap();
        assert_eq!(o.value, toml::Value::String(String::new()));
        assert_eq!(o.arg(), "privacy.proxy_url=");
    }

    #[test]
    fn other_settings_take_a_toml_value_of_their_type() {
        assert_eq!(
            parse_override("network.enable_dht=false").unwrap().value,
            toml::Value::Boolean(false)
        );
        assert_eq!(
            parse_override("network.listen_port=7000").unwrap().value,
            toml::Value::Integer(7000)
        );
        let o = parse_override(r#"player.args=["--fs", "--no-osc"]"#).unwrap();
        assert!(o.value.is_array());
        // The optional setting is settable too.
        assert!(parse_override("general.watch_dir=~/watch").is_ok());
    }

    #[test]
    fn a_mistake_in_set_is_an_error_not_a_silent_default() {
        let err = parse_override("privacy.proxy=socks5://x").unwrap_err();
        assert!(err.contains("no setting named \"privacy.proxy\""), "{err}");
        assert!(
            err.contains("privacy.proxy_url"),
            "lists the real names: {err}"
        );
        assert!(parse_override("proxy_url=x").is_err(), "needs a section");
        assert!(parse_override("privacy=x").is_err());
        assert!(parse_override("privacy.proxy_url").is_err(), "needs =");

        let err = parse_override("network.enable_dht=yes").unwrap_err();
        assert!(err.contains("network.enable_dht takes a boolean"), "{err}");
        // Right type, out of range: caught by deserializing for real.
        let err = parse_override("network.listen_port=70000").unwrap_err();
        assert!(err.starts_with("network.listen_port:"), "{err}");
        assert!(parse_override("network.listen_port=\"6881\"").is_err());
    }

    #[test]
    fn overrides_land_on_the_file_and_are_never_written_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let content = "[general]\ndownload_dir = \"/srv/dl\"\nfuture_key = 1\n\n[network]\nlisten_port = 7000\n";
        std::fs::write(&path, content).unwrap();

        let (config, warning) = load_at(
            &path,
            &set(&[
                "privacy.proxy_url=socks5://127.0.0.1:9050",
                "network.enable_dht=false",
            ]),
        )
        .unwrap();
        assert!(warning.is_none());
        assert_eq!(config.privacy.proxy_url, "socks5://127.0.0.1:9050");
        assert!(!config.network.enable_dht);
        // The file's own values stay where nothing overrides them.
        assert_eq!(config.general.download_dir, "/srv/dl");
        assert_eq!(config.network.listen_port, 7000);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
    }

    #[test]
    fn the_last_override_of_a_setting_wins() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let (config, _) = load_at(
            &path,
            &set(&["network.listen_port=7000", "network.listen_port=7100"]),
        )
        .unwrap();
        assert_eq!(config.network.listen_port, 7100);
    }

    #[test]
    fn overrides_apply_over_defaults_when_the_file_does_not_parse() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[network\n").unwrap();
        let (config, warning) =
            load_at(&path, &set(&["privacy.proxy_url=socks5://127.0.0.1:9050"])).unwrap();
        assert!(warning.is_some());
        assert_eq!(config.privacy.proxy_url, "socks5://127.0.0.1:9050");
        // The broken file is left for the user to fix.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[network\n");
    }

    #[test]
    fn a_first_run_writes_plain_defaults_not_the_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let (config, _) = load_at(&path, &set(&["privacy.proxy_url=socks5://h:1"])).unwrap();
        assert_eq!(config.privacy.proxy_url, "socks5://h:1");
        let on_disk: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(on_disk.privacy.proxy_url.is_empty());
    }

    #[test]
    fn an_override_path_gets_tilde_expansion() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let (config, _) = load_at(
            &dir.path().join("config.toml"),
            &set(&["general.download_dir=~/dl"]),
        )
        .unwrap();
        assert_eq!(
            config.general.download_dir,
            home.join("dl").to_string_lossy()
        );
    }

    #[test]
    fn every_setting_in_the_defaults_can_be_overridden() {
        // Guards `known_settings` against a section or key it cannot see.
        let names = setting_names(&known_settings());
        for name in [
            "general.download_dir",
            "general.watch_dir",
            "network.listen_port",
            "privacy.proxy_url",
            "privacy.bind_interface",
            "search.max_results",
            "ui.theme",
            "player.args",
        ] {
            assert!(names.iter().any(|n| n == name), "{name} missing: {names:?}");
        }
    }
}
