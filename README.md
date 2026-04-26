# TorrentTUI

A terminal-based BitTorrent client built with Rust, ratatui, and librqbit.

[![CI](https://github.com/thijsvos/torrentTUI/actions/workflows/ci.yml/badge.svg)](https://github.com/thijsvos/torrentTUI/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/thijsvos/torrentTUI?sort=semver)](https://github.com/thijsvos/torrentTUI/releases/latest)
![Platforms](https://img.shields.io/badge/platforms-Linux%20%7C%20macOS%20%7C%20Windows-blue)
![Rust](https://img.shields.io/badge/language-Rust-orange)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](./LICENSE)
[![@thijsvos](https://img.shields.io/badge/@thijsvos-000000?logo=x)](https://x.com/thijsvos)

![demo](assets/demo.gif)

## Contents

- [Features](#features) · [Installation](#installation) · [Usage](#usage) · [Keybindings](#keybindings) · [Configuration](#configuration) · [Privacy](#privacy) · [Docker](#docker) · [Contributing](#contributing) · [License](#license)

## Features

- **Magnet link & .torrent file support** — add torrents via magnet links or local `.torrent` files
- **Real-time progress** — progress bars, download/upload speeds, ETA, and peer counts
- **Sorting & filtering** — sort by any column, search torrents by name
- **Bandwidth throttling** — per-torrent fair throttling with configurable download/upload limits
- **Selective file download** — choose which files to download from multi-file torrents
- **Detail view** — inspect torrent info, individual file progress, and peer details
- **Session persistence** — torrents survive restarts via librqbit's built-in fastresume
- **Disk space monitoring** — free space indicator with low-space warnings
- **Completion notifications** — terminal bell + status bar notification when downloads finish
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
| `a` | Add magnet link or .torrent file |
| `p` | Pause/unpause selected (or all marked) torrents |
| `P` | Pause/unpause all torrents |
| `d` | Delete selected (or all marked) torrents |
| `Enter` | Open detail view |
| `j` / `k` (or `↓` / `↑`) | Move selection down/up |
| `Tab` | Cycle sort column / detail tab |
| `r` | Reverse sort order |
| `/` | Search/filter torrents |
| `t` | Set speed limits |
| `Space` | Mark/unmark current torrent (then advances selection) |
| `v` | Mark all visible torrents |
| `V` | Clear all marks |
| `Esc` | Clear marks (or close current dialog) |
| `?` | Toggle help |
| `q` | Quit |
| `Ctrl+C` | Quit (double press to force) |

### Detail view

| Key | Action |
|-----|--------|
| `Tab` | Cycle tabs (Stats → Info → Files → Peers) |
| `j` / `k` | Navigate files (Files tab) or peers (Peers tab) |
| `Space` | Toggle file selection (Files tab) |
| `S` | Apply current file selection to engine (Files tab) |
| `Esc` / `q` | Back to list |

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
# watch_dir = "/path/to/watch"  # optional; auto-add .torrent files dropped here

[network]
listen_port = 6881
max_peers_per_torrent = 50
enable_dht = true
enable_upnp = false           # opt in to open an external port via UPnP
max_download_speed_kbps = 0   # 0 = unlimited
max_upload_speed_kbps = 0     # 0 = unlimited

[ui]
refresh_rate_ms = 100
enable_notifications = true
```

### Logging

By default only `torrenttui=warn` is logged to `~/.config/torrenttui/torrenttui.log`. Set `RUST_LOG` to bump verbosity (e.g. `RUST_LOG=torrenttui=debug,librqbit=info`).

## Privacy

A few defaults worth knowing:

- **Logging is filtered.** Only TorrentTUI's own warnings are written to disk; librqbit's INFO-level output (peer IPs, tracker URLs, info hashes) is silenced. Bumping `RUST_LOG` re-enables it — redact before sharing logs.
- **UPnP is off by default.** Enabling it (`network.enable_upnp = true`) opens an external port via your router and exposes you to peers outside your LAN.
- **No telemetry.** TorrentTUI makes no outbound connections except to BitTorrent peers, trackers, and (if DHT is enabled) the DHT network.
- **Notifications.** Torrent names are sanitized before being sent to the OS notification daemon (Pango/HTML escaped on Linux). Disable entirely with `ui.enable_notifications = false`.

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
