# TorrentTUI

A terminal-based BitTorrent client built with Rust.

![Rust](https://img.shields.io/badge/language-Rust-orange)
![License](https://img.shields.io/badge/license-MIT-blue)
[![@thijsvos](https://img.shields.io/badge/@thijsvos-000000?logo=x)](https://x.com/thijsvos)

![demo](assets/demo.gif)

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

```bash
git clone https://github.com/thijsvos/torrentTUI.git
cd torrentTUI
cargo build --release
```

The binary will be at `target/release/torrenttui`.

## Usage

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
| `p` | Pause/unpause selected torrent |
| `P` | Pause/unpause all torrents |
| `d` | Delete selected torrent |
| `Enter` | Open detail view |
| `j` / `k` | Move selection down/up |
| `Tab` | Cycle sort column / detail tab |
| `r` | Reverse sort order |
| `/` | Search/filter torrents |
| `t` | Set speed limits |
| `?` | Toggle help |
| `q` | Quit |
| `Ctrl+C` | Quit (double press to force) |

### Detail view (Files tab)

| Key | Action |
|-----|--------|
| `j` / `k` | Navigate files |
| `Space` | Toggle file selection |
| `Esc` | Back to list |

## Configuration

Config file is created automatically at:
- **Linux:** `~/.config/torrenttui/config.toml`
- **macOS:** `~/Library/Application Support/torrenttui/config.toml`
- **Windows:** `%APPDATA%\torrenttui\config.toml`

### Default config

```toml
[general]
download_dir = "~/Downloads/torrents"
max_concurrent_downloads = 5
confirm_on_quit = true

[network]
listen_port = 6881
max_peers_per_torrent = 50
enable_dht = true
max_download_speed_kbps = 0  # 0 = unlimited
max_upload_speed_kbps = 0    # 0 = unlimited
```

## Docker

### Build

```bash
cd torrenttui
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

- [librqbit](https://github.com/ikatson/librqbit) — BitTorrent engine
- [ratatui](https://github.com/ratatui/ratatui) — Terminal UI framework
- [crossterm](https://github.com/crossterm-rs/crossterm) — Terminal manipulation
- [tokio](https://github.com/tokio-rs/tokio) — Async runtime

## License

MIT
