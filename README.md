# TorrentTUI

A terminal-based BitTorrent client built with Rust, ratatui, and librqbit.

[![CI](https://github.com/thijsvos/torrentTUI/actions/workflows/ci.yml/badge.svg)](https://github.com/thijsvos/torrentTUI/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/thijsvos/torrentTUI?sort=semver)](https://github.com/thijsvos/torrentTUI/releases/latest)
![Platforms](https://img.shields.io/badge/platforms-Linux%20%7C%20macOS%20%7C%20Windows-blue)
![Rust](https://img.shields.io/badge/language-Rust-orange)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](./LICENSE)
[![@thijsvos](https://img.shields.io/badge/@thijsvos-000000?logo=x)](https://x.com/thijsvos)

<p align="center">
  <img src="assets/demo.gif" width="100%" alt="TorrentTUI demo: opening the command palette with a colon, running the built-in torrent search, downloading an Arch Linux ISO from the results with Enter, and revealing the data in the file manager with o">
</p>
<p align="center"><em>Command palette → built-in search → pick a result → downloading → reveal in your file manager. No external services.</em></p>

## Contents

- [Features](#features) · [Installation](#installation) · [Usage](#usage) · [Keybindings](#keybindings) · [Configuration](#configuration) · [Privacy](#privacy) · [Docker](#docker) · [Contributing](#contributing) · [License](#license)

## Features

- **Magnet link & .torrent file support** — add torrents via magnet links or local `.torrent` files
- **Command palette** — press `:` and fuzzy-search every action with its keybinding shown; no key memorization required
- **Built-in torrent search** — press `s`, type a query, pick a result, and it starts downloading; queries public indexer APIs directly, with no Prowlarr/Jackett, accounts, or API keys
- **Stream while downloading** — press `s` on any media file in the Files tab to open it in your default player; pieces are fetched in playback order
- **Real-time progress** — progress bars, download/upload speeds, ETA, and peer counts
- **Sorting & filtering** — sort by any column, search torrents by name
- **Bandwidth limits** — session-wide download/upload rate limits, applied smoothly by librqbit's token-bucket limiter and adjustable live with `t`
- **Selective file download** — choose which files to download from multi-file torrents
- **Reveal in file manager** — press `o` to open the selected torrent's data in Finder, Explorer, or your Linux file manager
- **Detail view** — inspect torrent info, individual file progress, and peer details
- **Session persistence** — torrents survive restarts via librqbit's built-in fastresume
- **Disk space monitoring** — free space indicator with low-space warnings
- **Completion notifications** — status bar message, plus a desktop notification on Linux/Windows or a system sound on macOS
- **Mouse support** — click to select torrents in the list
- **Configurable** — TOML config file for download directory, network settings, and more

## Installation

### From releases

Download the latest binary for your platform from [Releases](https://github.com/thijsvos/torrentTUI/releases).

**Linux:**
```bash
tar xzf torrenttui-linux-x86_64.tar.gz
sudo mv torrenttui-linux-x86_64 /usr/local/bin/torrenttui
```

**macOS:**
```bash
tar xzf torrenttui-macos-universal.tar.gz
sudo mv torrenttui-macos-universal /usr/local/bin/torrenttui
```

**Windows:**
Extract `torrenttui-windows-x86_64.zip` and add the directory to your PATH.

### From source

Requires Rust 1.95 or newer (any recent stable toolchain works).

```bash
git clone https://github.com/thijsvos/torrentTUI.git
cd torrentTUI
cargo build --release
```

The binary will be at `target/release/torrenttui`.

## Usage

```bash
torrenttui [OPTIONS] [TORRENT_SOURCE]
```

| Option | Description |
|--------|-------------|
| `<TORRENT_SOURCE>` | Magnet link or `.torrent` file path to add on startup (positional) |
| `-d`, `--download-dir <PATH>` | Override download directory (otherwise read from config) |
| `-h`, `--help` | Print help |
| `-V`, `--version` | Print version |

Examples:

```bash
# Launch the TUI
torrenttui

# Add a magnet link on startup
torrenttui "magnet:?xt=urn:btih:..."

# Add a .torrent file on startup
torrenttui path/to/file.torrent

# Override download directory
torrenttui -d /path/to/downloads
```

## Keybindings

| Key | Action |
|-----|--------|
| `:` / `Ctrl+P` | Open the command palette (fuzzy-search every action) |
| `a` | Add magnet link or .torrent file |
| `s` | Search torrent indexers |
| `p` | Pause/unpause selected (or all marked) torrents |
| `P` | Pause/unpause all torrents |
| `d` | Delete selected (or all marked) torrents |
| `o` | Reveal the selected torrent in your file manager (Finder/Explorer/xdg-open); falls back to the download folder while data is still arriving |
| `Enter` | Open detail view |
| `j` / `k` (or `↓` / `↑`) | Move selection down/up |
| `Tab` | Cycle sort column |
| `r` | Reverse sort order |
| `/` | Filter torrent list |
| `t` | Set speed limits |
| `Space` | Mark/unmark current torrent (then advances selection) |
| `v` | Mark all visible torrents |
| `V` | Clear all marks |
| `Esc` | Clear marks (or close current dialog) |
| `?` | Toggle help |
| `q` | Quit |
| `Ctrl+C` | Quit (double press to force) |

Deleting prompts for `[K]eep files` or `[D]elete files` — that choice is about the
*downloaded data*. Either way, if a watch folder is configured, the matching
`.torrent`/`.magnet` is removed from it so the torrent does not come back on the next
launch. See [Watch folder](#watch-folder).

### Command palette

Can't remember a key? Press `:` (or `Ctrl+P`) in the torrent list, the detail
view, or the search results, and type a few letters of what you want — the
palette fuzzy-searches every action available right there, shows each one's
keybinding, and runs the highlighted one on `Enter`. Arrows (or `Ctrl+j`/`k`)
navigate, `Esc` closes. Actions that don't currently apply (nothing selected,
nothing to retry) are hidden until they do.

The palette, the `?` help overlay, the status-bar hints, and the keybinding
tables in this README all come from one action registry in the source
(`src/actions.rs`) — a test regenerates these tables from it, so the
documentation cannot drift from the real bindings.

### Search

Press `s`, type what you're looking for (e.g. `arch linux iso`), and hit
`Enter`. TorrentTUI queries two public indexer APIs — [apibay](https://apibay.org)
(The Pirate Bay) and [torrents-csv](https://torrents-csv.com) — merges and
dedups the results, and lists them by seeder count. Press `Enter` on a result
and it starts downloading immediately; the magnet link is built locally from
the result's info hash, so nothing is copied or pasted. No Prowlarr, Jackett,
accounts, or API keys involved. Both providers can be toggled off in
`[search]` — see [Configuration](#configuration) and [Privacy](#privacy).

| Key | Action |
|-----|--------|
| `Enter` | Download the selected result (stays in results for multi-grab) |
| `j` / `k` (or `↓` / `↑`) | Move selection down/up |
| `s` | Edit the query (pre-filled) |
| `r` | Retry the same query |
| `Esc` / `q` | Back to the torrent list (results are kept) |

#### Where do the magnet links come from?

Nowhere — TorrentTUI makes them itself. The indexers don't return magnet
links; each result carries the torrent's **info hash**, a 40-character
fingerprint that uniquely identifies it. A magnet link is just that
fingerprint wrapped in text, so when you press `Enter` the app assembles one
locally (`magnet:?xt=urn:btih:<hash>&dn=<name>&tr=…`) with zero extra network
requests. The engine then asks the BitTorrent network — DHT plus a few open
trackers — "who has the file with this fingerprint?"; peers answer with the
full torrent metadata and the download starts. This is the same mechanism The
Pirate Bay's own website uses: its magnet buttons are built in your browser
from the same API, and there is no warehouse of magnet links anywhere.

### Detail view

| Key | Action |
|-----|--------|
| `Tab` | Cycle tabs (Stats → Info → Files → Peers) |
| `j` / `k` | Navigate files (Files tab) or peers (Peers tab) |
| `Space` | Toggle file selection (Files tab) |
| `S` | Apply current file selection to engine (Files tab) |
| `s` | Stream selected file in default media player (Files tab) |
| `o` | Reveal this torrent in your file manager |
| `Esc` / `q` | Back to list |

### Streaming while downloading

In the Files tab, files with a known media extension are marked with `▶`. Press
`s` on any file (not just media — anything streamable over HTTP) to open it in
your system's default player. Librqbit prioritizes the pieces in playback
order, so videos generally start within seconds of pressing `s` — well before
the download is complete.

The engine binds a small HTTP API on `127.0.0.1` (auto-assigned port) at
startup; the player connects to that URL. The API is loopback-only by default;
see [Configuration](#configuration) and [Privacy](#privacy) before changing the
bind address.

Pick a specific player by setting:

```toml
[player]
command = "mpv"          # or "vlc", "iina", etc.
args = ["--no-terminal"] # optional extra args inserted before the URL
```

Leave `command` empty to use the OS default opener (`xdg-open` on Linux,
`open` on macOS, `start` on Windows).

## Configuration

Config file is created automatically at:
- **Linux:** `~/.config/torrenttui/config.toml`
- **macOS:** `~/Library/Application Support/torrenttui/config.toml`
- **Windows:** `%APPDATA%\torrenttui\config.toml`

### Default config

```toml
[general]
download_dir = "~/Downloads/torrents"
confirm_on_quit = true
# watch_dir = "~/torrents/watch"  # optional; auto-add .torrent files dropped here

[network]
listen_port = 6881
enable_dht = true
enable_upnp = false           # opt in to open an external port via UPnP
max_download_speed_kbps = 0   # KiB/s; 0 = unlimited (values 1-15 are raised to 16)
max_upload_speed_kbps = 0     # KiB/s; 0 = unlimited
http_api_bind = "127.0.0.1:0" # localhost-only by default (0 = auto-assigned port)

[ui]
refresh_rate_ms = 100
enable_notifications = true

[player]
command = ""                  # empty = OS default (xdg-open / open / start)
args = []                     # extra args inserted before the URL

[search]
enable_apibay = true          # The Pirate Bay JSON API (apibay.org), no auth
enable_torrents_csv = true    # torrents-csv.com public API, no auth
timeout_secs = 8              # per-provider HTTP timeout (clamped to 1-30)
max_results = 50              # cap on merged results (clamped to 1-500)
```

Paths (`download_dir`, `watch_dir`, `player.command`) may start with `~/`, which expands to your home directory.

### Watch folder

Set `watch_dir` and any `.torrent` file dropped there is added automatically,
both while TorrentTUI is running and on the next startup. `.magnet` files are
picked up too, but only while the app is running — librqbit's startup rescan
reads `.torrent` only, so a `.magnet` dropped while it is closed is ignored.

**Deleting a torrent also deletes its source file from the watch folder.** This applies
to both `[K]eep files` and `[D]elete files` — the choice there is about the downloaded
data, not the metadata. Without this the folder is rescanned at every launch and the
torrent you deleted comes straight back.

Worth knowing if the folder is shared with something else (a Syncthing folder, an \*arr
blackhole directory): TorrentTUI removes only files whose info hash matches a torrent you
deleted, but it does remove them for good. Cleanup is skipped entirely when `watch_dir` is
your home directory or a filesystem root, and never descends into `download_dir` when that
lives inside the watch folder.

### Logging

By default only `torrenttui=warn` is logged to `~/.config/torrenttui/torrenttui.log`. Set `RUST_LOG` to bump verbosity (e.g. `RUST_LOG=torrenttui=debug,librqbit=info`).

## Privacy

A few defaults worth knowing:

- **Logging is filtered.** Only TorrentTUI's own warnings are written to disk; librqbit's INFO-level output (peer IPs, tracker URLs, info hashes) is silenced. Bumping `RUST_LOG` re-enables it — redact before sharing logs.
- **UPnP is off by default.** Enabling it (`network.enable_upnp = true`) opens an external port via your router and exposes you to peers outside your LAN.
- **No telemetry.** TorrentTUI makes no outbound connections except to BitTorrent peers, trackers, (if DHT is enabled) the DHT network, and — only when you submit a search — the search providers below.
- **Search queries go to the indexers you enable.** Pressing `Enter` on a search sends the query text (nothing else — no identifiers, accounts, or keys) over HTTPS to `apibay.org` and/or `torrents-csv.com`. No request is made until you submit a search, and either provider can be disabled in `[search]`; disabling both turns the feature off entirely. Queries are not written to the log at the default filter.
- **Notifications.** Control characters are stripped from torrent names at the engine boundary, and names are additionally Pango-escaped before reaching the Linux notification daemon. macOS plays a sound instead of sending a notification, so no name leaves the process there. Disable entirely with `ui.enable_notifications = false`.
- **HTTP streaming API is loopback-only and authenticated.** The embedded API used for the `s` stream keybinding binds to `127.0.0.1:0`, is mounted read-only, and requires HTTP basic auth with a random password generated per run. The credentials ride in the stream URL handed to your media player, which means they are visible in that process's argv while it runs. Changing `network.http_api_bind` to a routable interface sends those credentials over plaintext HTTP — do this only on a trusted LAN.

## Docker

### Build

```bash
cd torrentTUI
docker build -t torrenttui .
```

### Run

```bash
docker run -it \
  -v ~/Downloads/torrents:/downloads \
  -v ~/.config/torrenttui:/home/torrenttui/.config/torrenttui \
  -p 6881-6890:6881-6890 \
  torrenttui
```

Add a magnet link on startup:

```bash
docker run -it \
  -v ~/Downloads/torrents:/downloads \
  -p 6881:6881 \
  torrenttui -d /downloads "magnet:?xt=urn:btih:..."
```

The `-it` flags are required since TorrentTUI is an interactive terminal application. The config volume is optional but enables session persistence across container restarts.

## Built with

- [librqbit](https://github.com/ikatson/librqbit) — BitTorrent engine (also handles fastresume / session persistence)
- [ratatui](https://github.com/ratatui/ratatui) — Terminal UI framework
- [crossterm](https://github.com/crossterm-rs/crossterm) — Terminal manipulation
- [tokio](https://github.com/tokio-rs/tokio) — Async runtime

## Contributing

PRs are welcome. See [CONTRIBUTING.md](./CONTRIBUTING.md) for the dev setup, lint commands, and release process. Found a bug? [Open an issue](https://github.com/thijsvos/torrentTUI/issues/new/choose). Found a security problem? See [SECURITY.md](./SECURITY.md) — please don't open a public issue.

## License

[MIT](./LICENSE) © Thijs Vos
