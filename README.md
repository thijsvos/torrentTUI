# TorrentTUI

A terminal-based BitTorrent client built with Rust, ratatui, and librqbit.

[![CI](https://github.com/thijsvos/torrentTUI/actions/workflows/ci.yml/badge.svg)](https://github.com/thijsvos/torrentTUI/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/thijsvos/torrentTUI?sort=semver)](https://github.com/thijsvos/torrentTUI/releases/latest)
![Platforms](https://img.shields.io/badge/platforms-Linux%20%7C%20macOS%20%7C%20Windows-blue)
![Rust](https://img.shields.io/badge/language-Rust-orange)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](./LICENSE)
[![@thijsvos](https://img.shields.io/badge/@thijsvos-000000?logo=x)](https://x.com/thijsvos)

<p align="center">
  <img src="assets/demo.gif" width="100%" alt="TorrentTUI demo: a Debian netinst torrent downloading at 11 MB/s from 38 peers, capped to 500 KB/s live with t and settling smoothly at the limit, the command palette opening the built-in indexer search, then Ctrl+D detaching — the confirmation explains that seeding continues and how to stop it — the window closing to a shell where torrenttui --status reports the background session still running, and plain torrenttui taking it back with the progress bar further along than it was left">
</p>
<p align="center"><em>A live download capped to 500 KB/s with <code>t</code> — the status bar shows the rate settling against the limit → command palette → built-in search, no external services → <code>Ctrl+D</code> detaches: the window closes, the session keeps downloading → <code>torrenttui</code> takes it straight back, further along than you left it.</em></p>

## Contents

- [Features](#features) · [Installation](#installation) · [Usage](#usage) · [Keybindings](#keybindings) · [Configuration](#configuration) · [Background sessions](#background-sessions) · [Privacy](#privacy) · [Docker](#docker) · [Contributing](#contributing) · [License](#license)

## Features

- **Magnet link & .torrent file support** — add torrents via magnet links or local `.torrent` files
- **Command palette** — press `:` and fuzzy-search every action with its keybinding shown; no key memorization required
- **Built-in torrent search** — press `s`, type a query, pick a result, and it starts downloading; queries public indexer APIs directly, with no Prowlarr/Jackett, accounts, or API keys
- **Stream while downloading** — press `s` on any media file in the Files tab to open it in your default player; pieces are fetched in playback order
- **Real-time progress** — progress bars, download/upload speeds, ETA, and peer counts
- **Sorting & filtering** — sort by any column, search torrents by name
- **Bandwidth limits** — session-wide download/upload rate limits, applied smoothly by librqbit's token-bucket limiter and adjustable live with `t`
- **Privacy pack** — SOCKS5 proxy with honest lockdown (no DHT/UDP leaking around it), PeerGuardian IP blocklists, and VPN interface binding; see [Privacy](#privacy)
- **Selective file download** — press `A` to pick which files to download and where to save them *before* anything starts; change the selection later in the Files tab
- **Reveal in file manager** — press `o` to open the selected torrent's data in Finder, Explorer, or your Linux file manager
- **Detail view** — inspect torrent info, individual file progress, and peer details
- **Why is it slow?** — press `w` and the Health tab says *why* a torrent is stalled (no way to find peers, peers unreachable, swarm has no seeders, your own cap) and what to change; see [the Health tab](#why-is-it-slow--the-health-tab)
- **Session persistence** — torrents survive restarts via librqbit's built-in fastresume
- **Background sessions** — press `Ctrl+D` to detach: downloads and seeding keep running in a process you started on purpose. Nothing survives a plain `q`. Run `torrenttui` again to take the session back; `torrenttui --stop` ends it
- **Profiles and one-off settings** — `--profile vpn` runs a separate session with its own settings next to your usual one; `-c privacy.proxy_url=…` overrides any setting for a single run; `-q` quits when the downloads are done, for scripts
- **Disk space monitoring** — free space indicator with low-space warnings
- **Completion notifications** — status bar message, plus a desktop notification on Linux/Windows or a system sound on macOS
- **Mouse support** — click to select torrents in the list
- **Configurable** — TOML config file for download directory, network settings, and more

## Installation

### From releases

Download the binary for your platform from [Releases](https://github.com/thijsvos/torrentTUI/releases) — each release is just the six binaries, nothing to unpack.

**Linux:**
```bash
chmod +x torrenttui-linux-x86_64
sudo mv torrenttui-linux-x86_64 /usr/local/bin/torrenttui
```

**macOS:**
```bash
chmod +x torrenttui-macos-universal
sudo mv torrenttui-macos-universal /usr/local/bin/torrenttui
```

**Windows:**
Download `torrenttui-windows-x86_64.exe` and add its directory to your PATH.

Every binary carries a Sigstore build-provenance attestation, so you can check
that what you downloaded was built by this repository's release workflow:

```bash
gh attestation verify torrenttui-linux-x86_64 --repo thijsvos/torrentTUI
```

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
| `-p`, `--profile <NAME>` | Run as a separate profile, with its own settings, torrents and session — see [Profiles](#profiles) |
| `-c`, `--set <KEY=VALUE>` | Override any `config.toml` setting for this run only; repeatable — see [One-off settings](#one-off-settings) |
| `-i`, `--pick` | Open the file picker for the source instead of adding it straight away (what `A` does) |
| `-s`, `--search <QUERY>` | Start with a search for `QUERY` (what `s` does) |
| `-q`, `--quit-when-done` | Quit once nothing is left to download — see [Quitting when done](#quitting-when-done) |
| `--headless` | Run the engine with no terminal (a server, or a detached session) |
| `--status` | Report whether a session is running, and exit |
| `--stop` | Stop a running background session and exit |
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

# Add a magnet to the session that is already running, from any shell
# (-d works here too: the torrent goes to that folder)
torrenttui "magnet:?xt=urn:btih:..."

# Choose the files first, or start with a search
torrenttui -i "magnet:?xt=urn:btih:..."
torrenttui -s "debian netinst"

# This one through a proxy, without editing config.toml
torrenttui -c privacy.proxy_url=socks5://127.0.0.1:9050 "magnet:?xt=urn:btih:..."

# A second session with settings of its own, next to the usual one
torrenttui --profile vpn

# Download, then exit: 0 when everything finished, 1 when something failed
torrenttui --headless -q -p batch "magnet:?xt=urn:btih:..."

# Background session (see Background sessions below)
torrenttui --headless
torrenttui --status
torrenttui --stop
```

## Keybindings

| Key | Action |
|-----|--------|
| `:` / `Ctrl+P` | Open the command palette (fuzzy-search every action) |
| `a` | Add magnet link or .torrent file |
| `A` | Add, choosing which files to download and where to save them first |
| `s` | Search torrent indexers |
| `p` | Pause/unpause selected (or all marked) torrents |
| `P` | Pause/unpause all torrents |
| `d` | Delete selected (or all marked) torrents |
| `o` | Reveal the selected torrent in your file manager (Finder/Explorer/xdg-open); falls back to the download folder while data is still arriving |
| `Enter` | Open detail view |
| `w` | Why is it slow? Open the Health tab for the selected torrent |
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
| `Ctrl+D` | Detach: keep downloading in the background and close the TUI |
| `T` | Choose a colour theme |
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

### Choosing files and a folder before downloading

`a` adds a torrent and starts it at once, with every file, in `download_dir`.
`A` asks first. Paste the magnet or `.torrent` path as usual; TorrentTUI then
fetches the file list *without* starting the download (for a magnet that takes
as long as finding a peer that has it — `Esc` cancels) and opens a picker:

- tick the files you want — in a folder tree, where `Space` on a folder takes
  the whole folder (`t` switches to a flat list of full paths); or filter the
  list to work on part of it — type `season 2`, press `a`, and that season is
  in;
- choose the folder — `~/` and absolute paths work, a plain name like `tv`
  means a `tv` folder inside `download_dir`, `Tab` completes folder names, and
  `↑` brings back folders you saved into before;
- see what the selection needs next to what that folder's disk has free.

`Enter` adds it. A multi-file torrent still gets its own subfolder inside the
folder you chose, just as it would in `download_dir`.

| Key | Action |
|-----|--------|
| `Space` | Toggle the file under the cursor — or every file in the folder under it |
| `a` / `n` / `i` | Select all / none / invert — of the files the filter shows |
| `t` | Switch between the folder tree and a flat list |
| `l` / `h` (or `→` / `←`) | Open / close a folder (on a file, `h` steps out to its folder) |
| `r` | Back to the selection the picker opened with |
| `s` | Sort by torrent order, size, or name |
| `/` | Filter the files (`Enter` back to the list, `Esc` clears) |
| `j` / `k`, `g` / `G`, `PgUp` / `PgDn` | Move, jump to top / bottom, page |
| `Tab` | Edit the save folder (`Tab` there completes, `↑` / `↓` step through recent folders, `Esc` goes back; [the text-field keys](#text-fields) work there too) |
| `Enter` | Add with these files, into this folder |
| `Esc` | Clear the filter, or cancel |

A folder's box reads `[~]` when only some of its files are ticked. The
filter matches file paths containing what you typed. When nothing does,
it falls back to fuzzy matching and the count says `(fuzzy)` — worth a look
before pressing `a`, since the bulk keys act on whatever the list shows.

Good to know:

- **The folder is per torrent and is remembered across restarts**, but it is
  fixed once the torrent is added: librqbit has no way to move a torrent's
  data afterwards. The file selection is not fixed — change it any time in the
  detail view's Files tab.
- **The proxy is not a per-torrent choice.** `[privacy]` applies to the whole
  session, and has to: DHT and the listener belong to the session, so a
  torrent "outside" the proxy would still be announced from your real address.
  See [Privacy](#privacy).
- Unticked files can still show up on disk, empty or partly written: librqbit
  creates every file of a torrent, and a piece that straddles a ticked and an
  unticked file is written whole. That is how BitTorrent works, not a leak of
  the selection.
- `A` works on a search result too: choose its files and folder, and `Enter`
  takes you back to the results.
- The recent folders (the last ten) are kept in `recent_folders.json` next to
  the session state, which already records every torrent's folder. Delete the
  file to clear the list.
- Adds that do not go through the keyboard — the CLI argument, a magnet handed
  to a running session, the watch folder — still add straight away.

### Text fields

Every text box — the add bar, the filter, search, the command palette, the
speed-limit dialog, and the add picker's filter and save folder — edits the
same way, with the keys a shell uses:

| Key | Action |
|-----|--------|
| `←` / `→`, `Home` / `End` (or `Ctrl+A` / `Ctrl+E`) | Move the cursor; typing goes in where it is |
| `Ctrl+←` / `Ctrl+→` (or `Alt+B` / `Alt+F`) | Move a word at a time |
| `Backspace` / `Delete` | Delete the character before / under the cursor |
| `Ctrl+W` (or `Alt+Backspace`, `Ctrl+Backspace`) | Delete the word before the cursor |
| `Alt+D` (or `Ctrl+Delete`) | Delete the word after the cursor |
| `Ctrl+U` / `Ctrl+K` | Delete everything before / after the cursor |
| `Ctrl+Z` / `Ctrl+Y` | Undo / redo (`Ctrl+Shift+Z` redoes too, where the terminal reports it) |

A word is a run of letters and digits, so in a path a word step moves one
folder name: at the end of `/home/me/Downloads/`, `Ctrl+W` leaves `/home/me/`.
Undo takes back a word of typing, a run of deletes, or a paste at a time.

`Tab` completes paths: in the add bar, `.torrent` files and the folders on the
way to them (relative paths start where TorrentTUI was launched, as the add
itself does); in the picker's save folder, folders. When more than one name
fits, `Tab` fills in what they share and opens a menu of them:

| Key | In the completion menu |
|-----|------------------------|
| `Tab` / `↓` | Next choice — the field shows it in place |
| `Shift+Tab` / `↑` | Previous choice |
| `Enter` | Keep the choice (it does not add the torrent yet) |
| `Esc` | Close the menu, back to what you typed |
| Anything else | Keep the choice and carry on — type, `/`, `Backspace` |

Not every terminal sends `Ctrl+←`, `Ctrl+Backspace`, `Ctrl+Delete` or
`Ctrl+Shift+Z` as keys of their own; the `Ctrl`/`Alt` letter chords reach the
app from all of them.

### Search

Press `s`, type what you're looking for (e.g. `arch linux iso`), and hit
`Enter`. TorrentTUI queries two public indexer APIs — [apibay](https://apibay.org)
(The Pirate Bay) and [torrents-csv](https://torrents-csv.com) — merges and
dedups the results, and lists them by seeder count — press `Tab` to sort by
size, title, or leechers instead. Press `Enter` on a result
and it starts downloading immediately — or `A` to choose its files and save
folder first ([the add picker](#choosing-files-and-a-folder-before-downloading));
the magnet link is built locally from
the result's info hash, so nothing is copied or pasted. No Prowlarr, Jackett,
accounts, or API keys involved. Both providers can be toggled off in
`[search]` — see [Configuration](#configuration) and [Privacy](#privacy).

| Key | Action |
|-----|--------|
| `Enter` | Download the selected result (stays in results for multi-grab) |
| `A` | Choose files and save folder, then download the selected result |
| `j` / `k` (or `↓` / `↑`) | Move selection down/up |
| `Tab` | Cycle sort column (Seeders → Size → Title → Leechers) |
| `R` | Reverse sort order |
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
| `Tab` | Cycle tabs (Stats → Info → Files → Peers → Health) |
| `j` / `k` | Navigate files (Files tab) or peers (Peers tab); scroll the Health tab |
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

### Why is it slow? — the Health tab

A torrent sitting at `0 B/s` is the one question every client makes you answer
yourself. Press `w` on any torrent (or `Tab` to the **Health** tab in the
detail view) and TorrentTUI answers it: one verdict, the evidence behind it,
and the single most useful thing to try — naming the config key involved and
whether it applies live (`t`) or needs a restart.

```
⚠ Stalled — 52 peers known, none reachable — nothing received for 2 min
  • 52 known: 49 dead, 3 connecting, 0 queued, 0 live
  • Every outgoing TCP connection failed (214 of 214)
→ Something is blocking outgoing connections — firewall, VPN, or the ISP
```

Verdicts you will see:

- **Stalled — no way to find peers.** DHT is off (proxy lockdown, or
  `network.enable_dht = false`) and every tracker is failing or was a `udp://`
  one the proxy stripped. The fix is spelled out: add an http(s) tracker, or
  change the key named — with a note that `[privacy]` and `[network]` changes
  need a restart.
- **Stalled — peers known, none reachable.** The swarm was discovered but no
  connection succeeds; the session-wide connection counters say whether it is
  every TCP connection (firewall/VPN), every SOCKS5 connection (the proxy is
  down), a bound interface without a route (`privacy.bind_interface`), or a
  blocklist rejecting the whole swarm.
- **Stalled — connected but nobody is sending.** Peers are live but have
  nothing you need: a swarm with no seeders. Also flags data that was fetched
  but never passed a hash check.
- **Capped.** You are at your own `t` limit — the one cause that is fixed
  live, in one keypress.
- **Note.** Working, with something worth knowing: a thin swarm, a failed
  UPnP mapping, seeding without a listener under the proxy.
- **Healthy**, with the freshest tracker announce and the average piece time.

Below the verdict the tab lists every number it looked at: the peer buckets
(live / seen / dead / connecting / queued / not needed), DHT node count,
listener port, UPnP result, each tracker's last announce or error, transfer
quality, and the session-wide connection success rates. In the main list a
stalled torrent's status cell reads `⚠ Stalled` after 30 s without a byte.

A magnet is a special case: librqbit only creates the torrent once a peer has
handed over its metadata, so until then there is nothing to put in the list.
Rather than pretend it was "added", TorrentTUI shows it in a **Resolving**
strip under the list with how long it has been waiting and — after 30 s — why
nobody has answered (DHT state, each of the magnet's trackers). After 15
minutes without metadata the engine gives up and says so. Quitting with a
magnet still resolving asks first, because a quit drops it.

Where does tracker status come from? librqbit's API exposes none — so
TorrentTUI listens to the tracker events librqbit logs, in memory only. The
on-disk log keeps its default `torrenttui=warn` filter; nothing about peers or
trackers is written anywhere. The one thing no client can show is a tracker's
seeder/leecher count: librqbit discards it before it is logged.

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
max_download_speed_kbps = 0   # KiB/s; 0 = unlimited (nonzero values below 16 are raised to 16)
max_upload_speed_kbps = 0     # KiB/s; 0 = unlimited (nonzero values below 16 are raised to 16)
http_api_bind = "127.0.0.1:0" # localhost-only by default (0 = auto-assigned port)

[ui]
refresh_rate_ms = 100
enable_notifications = true
theme = "system"              # colour theme; see Themes below

[player]
command = ""                  # empty = OS default (xdg-open / open / start)
args = []                     # extra args inserted before the URL

[search]
enable_apibay = true          # The Pirate Bay JSON API (apibay.org), no auth
enable_torrents_csv = true    # torrents-csv.com public API, no auth
timeout_secs = 8              # per-provider HTTP timeout (clamped to 1-30)
max_results = 50              # cap on merged results (clamped to 1-500)

[privacy]                     # all applied at startup; restart to change
proxy_url = ""                # SOCKS5 proxy: "socks5://[user:pass@]host:port"
blocklist_url = ""            # PeerGuardian .p2p list: path, ~/path, file:// or http(s)://
bind_interface = ""           # bind all BitTorrent traffic to an interface, e.g. "wg0"
```

Paths (`download_dir`, `watch_dir`, `player.command`) may start with `~/`, which expands to your home directory.

The `[privacy]` keys are all off by default and opt-in. See [Privacy](#privacy) for exactly what each one does and its caveats, or jump to the copy-paste [recipes](#recipes).

### Themes

TorrentTUI ships with a colour-theme picker. Press `T` — or run **Choose a colour theme** from the command palette (`:` / `Ctrl+P`) — to open the selector: the highlight previews each theme live, `Enter` applies it, `Esc` cancels.

The choice is saved as `[ui] theme`, and only that line of `config.toml` changes — comments, key order and `~` paths are left as you wrote them. If the file has a syntax error, the theme applies for the session and nothing is written, so the file stays as it is for you to fix. An unknown `theme` value falls back to `system`.

`system` (the default) is TorrentTUI's original ANSI palette, which follows your terminal's own colour scheme. The other themes are `catppuccin`, `dracula`, `everforest`, `github`, `gruvbox`, `kanagawa`, `monokai`, `nord`, `one-dark`, `rosepine`, `solarized` and `tokyonight`. They set explicit 24-bit colours, so they need a truecolor terminal, and they are dark schemes: they colour text but leave your terminal's background alone, so they are meant for a dark background.

The preset list and the first colour mapping came from [OpenCode](https://github.com/anomalyco/opencode)'s MIT-licensed themes; each was then matched to its scheme's upstream palette.

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

### One-off settings

`-c section.key=value` (long form `--set`) overrides one setting for one run and
is never written back to `config.toml`. Give it as often as you need; names are
the section and key exactly as they appear in the file:

```bash
torrenttui -c privacy.proxy_url=socks5://127.0.0.1:9050 -c network.listen_port=7000
torrenttui -c privacy.proxy_url=      # the proxy off, just this once
```

A text setting takes the rest of the argument as it is, so URLs and paths need
no quoting beyond what your shell wants; other settings take a TOML value
(`true`, `7000`, `["--fs"]`). A name that does not exist or a value of the wrong
type stops the launch with a message — it is never silently ignored, because
the setting that went missing would usually be the one that mattered.

Settings are applied when a session starts, so `-c` cannot change a session
that is already running: `torrenttui -c … "magnet:…"` with a session open
refuses instead of adding the magnet without the setting. Start it as a
[profile](#profiles) to run it next to the other one, or stop the other first.

Mind that anything on a command line is visible to other users of the machine
in `ps` — put proxy credentials in a profile's `config.toml` rather than in `-c`.

### Profiles

`--profile <name>` (`-p`) runs a session of its own: its own `config.toml`,
torrent list, background session, logs and DHT state, in `profiles/<name>/`
beside your config. Profiles run independently, so `torrenttui -p vpn` can sit
next to your usual session — each holds its own lock, and two sessions never
share torrents.

The first launch of a profile starts its `config.toml` as a copy of your main
one, so you only change what differs — say, the proxy:

```bash
torrenttui -p vpn          # first run creates profiles/vpn/config.toml
# edit privacy.proxy_url in that file, then:
torrenttui -p vpn "magnet:?xt=urn:btih:..."
```

Everything that talks to a session takes the profile: `torrenttui -p vpn
--status`, `torrenttui -p vpn --stop`, and a magnet given with `-p vpn` goes to
the `vpn` session if it is running. `Ctrl+D` in a profile detaches that profile.
Names are letters, digits, `-` and `_`.

### Quitting when done

`--quit-when-done` (`-q`) quits once nothing is left to download: no magnet is
still waiting for metadata, and no torrent is downloading or checking. A paused
torrent counts as settled — it will not finish on its own — and so does one in
error. Seeding stops when it quits, like any other quit.

It prints what happened and exits with status `0` when nothing failed, or `1`
when a torrent ended in error or something could not be added (a magnet nobody
could resolve, a broken `.torrent`), so a script can tell. It works in the window and with `--headless`; a separate profile
keeps a scripted run away from your everyday torrents, which would otherwise
count too:

```bash
torrenttui --headless -q -p batch "magnet:?xt=urn:btih:..." && echo downloaded
```

## Background sessions

TorrentTUI normally lives and dies with your terminal: `q` and `Ctrl+C` stop the
engine. That is the default on purpose. But a 4 GB download is 40 minutes of a
window you cannot close, so there is one explicit way out.

**Detach.** Press `Ctrl+D`. It always asks first — regardless of
`confirm_on_quit` — and tells you what will keep running and how to stop it. Say
yes and the TUI closes while a background process keeps downloading *and
seeding*.

```bash
torrenttui --status   # is anything running? (starts nothing, writes nothing)
torrenttui            # take the session back and open the TUI
torrenttui --stop     # end the background session
```

**Take-over is a restart, not an attach.** Running `torrenttui` while a
background session holds the torrents asks it to stop, waits for it, and starts
the TUI with the same torrents. Transfers pause for a few seconds and trackers
see a reconnect; fastresume means nothing is re-hashed.

**Only one instance can own the session.** A second `torrenttui` refuses and
names the pid of the first. This is not tidiness: librqbit's session state is
guarded only inside a single process, so two instances writing the same
`session.json` and the same fastresume files silently lose torrents. (If what
you wanted a second instance for was a different download folder: `A` chooses
the folder per torrent. For different settings, run a
[profile](#profiles) — a session of its own.) Before this existed, a second
launch started anyway.

**Handing work to a running session.** `torrenttui "magnet:?..."` while a
session is running — a window or a background one — adds the magnet to it and
exits, instead of starting a competing instance. This makes TorrentTUI usable as
a `magnet:` handler from a browser. `-d` travels with it: the torrent goes to
that folder, in its own subfolder, just as a session started with that `-d`
would place it. Magnets only: a `.torrent` *path* would be resolved against the
running session's working directory rather than yours, so it is refused with an
explanation. Settings (`-c`) and `--quit-when-done` belong to a session as it
starts, so they are refused rather than dropped.

**On a server.** `--headless` runs the engine with no terminal at all, logging
to `torrenttui-daemon.log` beside your config. It shuts down cleanly on SIGTERM,
so a systemd unit or `docker stop` will not interrupt a flush.

```ini
# ~/.config/systemd/user/torrenttui.service
# Enable with: systemctl --user enable --now torrenttui
[Unit]
Description=TorrentTUI background session
After=network-online.target

[Service]
ExecStart=%h/.local/bin/torrenttui --headless
ExecStop=%h/.local/bin/torrenttui --stop
# Give the session time to flush its state instead of being killed mid-write.
TimeoutStopSec=30
Restart=on-failure

[Install]
WantedBy=default.target
```

Attach from the same machine whenever you want with plain `torrenttui` — that
stops the background session and takes it over, so run `systemctl --user start
torrenttui` again when you are done. Over SSH the same applies: the session
keeps running after you disconnect only if you detached it, never just because
the connection dropped.

```bash
docker run -d --name torrenttui \
  -v ~/Downloads/torrents:/downloads \
  -v ~/.config/torrenttui:/home/torrenttui/.config/torrenttui \
  -p 6881:6881 \
  torrenttui --headless -d /downloads
```

**If a crash takes the session down**, nothing needs cleaning up: ownership is an
OS file lock, which the kernel releases when the process dies.

### Known limits

- Detaching confirms that the background process **started**, not that its
  session came up. It cannot: the parent has to keep the lock until it exits, so
  it can never watch the child take it. If the session then fails to start —
  say a fail-closed blocklist — that is recorded, and `torrenttui --status` and
  the next launch report it.
- `[privacy]` settings are applied when a session starts. A background session
  keeps the posture it started with even if you edit `config.toml` afterwards —
  `--status` shows what it actually applied.
- A speed limit set with `t` belongs to the running session, not to your
  config, so it does not survive a detach and take-over. Put it in
  `[network] max_download_speed_kbps` to have it apply to every session.

## Privacy

A few defaults worth knowing:

- **Logging is filtered.** Only TorrentTUI's own warnings are written to disk; librqbit's INFO-level output (peer IPs, tracker URLs, info hashes) is silenced. Bumping `RUST_LOG` re-enables it — redact before sharing logs.
- **UPnP is off by default.** Enabling it (`network.enable_upnp = true`) opens an external port via your router and exposes you to peers outside your LAN.
- **No background process without an explicit action.** `q` and `Ctrl+C` stop the engine. Only `Ctrl+D` leaves anything running, it always asks for confirmation first (regardless of `confirm_on_quit`), and it tells you the pid and how to stop it. `torrenttui --status` answers "is anything running?" at any time without starting anything. See [Background sessions](#background-sessions).
- **No telemetry.** TorrentTUI makes no outbound connections except to BitTorrent peers, trackers, (if DHT is enabled) the DHT network, and — only when you submit a search — the search providers below.
- **Search queries go to the indexers you enable.** Pressing `Enter` on a search sends the query text (nothing else — no identifiers, accounts, or keys) over HTTPS to `apibay.org` and/or `torrents-csv.com`. No request is made until you submit a search, and either provider can be disabled in `[search]`; disabling both turns the feature off entirely. Queries are not written to the log at the default filter. When `privacy.proxy_url` is set, these queries also go through the proxy.
- **Notifications.** Control characters are stripped from torrent names at the engine boundary, and names are additionally Pango-escaped before reaching the Linux notification daemon. macOS plays a sound instead of sending a notification, so no name leaves the process there. Disable entirely with `ui.enable_notifications = false`.
- **HTTP streaming API is loopback-only and authenticated.** The embedded API used for the `s` stream keybinding binds to `127.0.0.1:0`, is mounted read-only, and requires HTTP basic auth with a random password generated per run. The credentials ride in the stream URL handed to your media player, which means they are visible in that process's argv while it runs. Changing `network.http_api_bind` to a routable interface sends those credentials over plaintext HTTP — do this only on a trusted LAN.

### Recipes

Copy one of these into the `[privacy]` block of your `config.toml` (see [Configuration](#configuration) for where that file lives). Every `[privacy]` key is applied when the session starts, so **restart TorrentTUI after editing**. Each recipe states the essentials; follow the link for the full behavior and caveats.

**Route all BitTorrent traffic through a VPN interface** — the most complete option:

```toml
[privacy]
bind_interface = "wg0"   # WireGuard on Linux; use "utun3"/"tun0" as appropriate — check `ip addr` / `ifconfig`
```

Pins *every* protocol — DHT, UDP **and** HTTP trackers, peer connections, LSD — to that interface, so if the VPN drops, traffic fails instead of escaping via your default route. **macOS/Linux only:** any value makes the app refuse to start on Windows, and startup also fails on an unknown interface name. Indexer search is *not* interface-bound (it follows the OS routing table, which is your VPN only when that is the default route). See the VPN notes under [SOCKS5 proxy](#socks5-proxy).

**Route through a local SOCKS5 proxy (e.g. Tor on `127.0.0.1:9050`)** — lockdown mode:

```toml
[privacy]
proxy_url = "socks5://127.0.0.1:9050"   # socks5:// only; prefix user:pass@ if the proxy needs auth
```

Outgoing peer connections, HTTP(S) tracker announces, and indexer search (auto-upgraded to `socks5h://` so DNS resolves proxy-side too) go through the proxy. DHT, incoming/uTP/UPnP and LSD are disabled, `udp://` trackers are stripped from magnets, and the watch folder is turned off; a green `[proxy]` badge appears once the session is locked down. **Not covered:** `udp://` trackers embedded inside `.torrent` files still announce directly (prefer magnets), and the torrent client still resolves tracker hostnames through your system DNS. See [SOCKS5 proxy](#socks5-proxy).

**Block known-bad peers from a local blocklist file** — combine with either option above:

```toml
[privacy]
blocklist_url = "~/lists/blocklist.p2p"   # PeerGuardian .p2p, plain or gzipped
```

Filters **both incoming and outgoing** peer connections. Loaded once at startup and **fail-closed**: if the file can't be read or parses to zero ranges the app exits rather than run unprotected, and the startup line reports how many ranges loaded. Prefer a **local file** over an `http(s)://` URL whenever `proxy_url` or `bind_interface` is set — a remote list is fetched by a client bound to neither, so it would escape your proxy/VPN. See [IP blocklist](#ip-blocklist).

#### Worked example: Mullvad

Mullvad works with both mechanisms. Connect it in **WireGuard** mode first (the interface and the proxy below only exist while connected), then pick one:

**Bind to the Mullvad interface (recommended, macOS/Linux).** With WireGuard connected, Mullvad creates a tunnel interface — on Linux it is usually `wg0-mullvad` (run `ip addr` to confirm), on macOS a `utunN` device (run `ifconfig` to find the number, which can change between reconnects):

```toml
[privacy]
bind_interface = "wg0-mullvad"   # macOS: the utunN Mullvad created — check `ifconfig`
```

This is the fuller option: DHT and all trackers keep working (more peers) and the binding is fail-closed if the tunnel drops.

**Mullvad's SOCKS5 proxy (any OS, or a different exit).** Mullvad publishes a SOCKS5 proxy reachable only through the WireGuard tunnel, with no auth (per Mullvad's docs, `10.64.0.1:1080` for the current server; they also list per-server proxies to make just this app exit elsewhere):

```toml
[privacy]
proxy_url = "socks5://10.64.0.1:1080"
```

Use this on **Windows** (where `bind_interface` is unsupported) or when you want torrentTUI to exit from a different Mullvad server than the rest of your machine. It runs in lockdown mode, so peer discovery leans on trackers (DHT is off). Either option combines with a local `blocklist_url`.

### SOCKS5 proxy

Set `privacy.proxy_url = "socks5://host:port"` and the session runs in **lockdown mode**. A SOCKS5 proxy only carries TCP, so rather than ship a proxy that quietly leaks, TorrentTUI turns off everything that would bypass it:

| Traffic | With `proxy_url` set |
|---|---|
| Outgoing peer connections | ✅ through the proxy |
| HTTP(S) tracker announces | ✅ through the proxy |
| Indexer search queries (`s`) | ✅ through the proxy |
| DHT (UDP) | **disabled** — would bypass the proxy (even if `enable_dht = true`) |
| Incoming connections / uTP / UPnP | **disabled** — direct by nature and pointless behind a proxy |
| Local service discovery | **disabled** — multicasts on your LAN |
| `udp://` trackers in magnets you add (add dialog, search, CLI) | **stripped** before they reach the engine, using the same URL parser librqbit dispatches on |
| `udp://` trackers inside `.torrent` files | ⚠ **still announced directly** — librqbit reads the announce list from the metainfo, where TorrentTUI cannot filter it. Prefer magnets when proxying |
| Torrents restored from a previous session | ⚠ keep the trackers they were added with. A torrent added *before* proxy mode may carry `udp://` trackers that announce directly; the app **warns at startup** how many, so you can re-add them by magnet (which strips) or switch to `bind_interface` |
| Watch folder (`.torrent` **and** `.magnet`) | **disabled in proxy mode** — librqbit's watcher adds those files itself, bypassing the strip, so the whole feature is turned off rather than leak |
| DNS lookups for tracker hostnames | ⚠ librqbit resolves them through the **system resolver** before dialing the proxy, so your DNS server sees tracker hostnames, timed with activity (peer addresses are raw IPs — no lookup). Indexer search lookups *are* proxied (`socks5h`) |

Two librqbit implementation details worth knowing. Its UDP tracker client binds a UDP socket at session creation regardless of configuration; in proxy mode nothing ever sends on it (magnets are stripped and DHT is off) and its port is never announced, but you will see it in `lsof`. And the search-built magnets include verified `https://` open trackers so peer discovery survives udp-stripping — a magnet carrying *only* `udp://` trackers gets a status-bar warning, because with DHT off it then has no way to find peers.

The header shows a green `[proxy]` badge once the session is actually running locked down, and the app refuses to start on a malformed proxy URL rather than fall back to a direct connection.

**Using a VPN? `bind_interface` is the more complete option.** It pins *every* protocol — DHT, UDP **and** HTTP trackers, peer connections — to the interface you name (`wg0`, `utun3`, …), including the udp trackers the proxy cannot cover, and if the VPN drops, traffic fails instead of escaping through your default route. **Caveats:** on **Windows** interface binding is unsupported by the underlying library — any `bind_interface` value makes the app refuse to start (it is a macOS/Linux feature). And **combining** `proxy_url` with `bind_interface` does *not* bind the proxy's own connections (the SOCKS5 socket and the proxied HTTP tracker client are not interface-bound), so in combined mode a VPN drop can let proxy traffic escape — bind alone is the fail-safe configuration.

### IP blocklist

`privacy.blocklist_url` loads a PeerGuardian `.p2p` list (plain or gzipped) from a local path, `file://` URL, or `http(s)://` URL, and applies it to **both incoming and outgoing** peer connections. It is loaded once at startup and fail-closed: if the list cannot be fetched, and — because a wrong-format file would otherwise load as zero ranges — if it parses to **no ranges at all**, TorrentTUI exits with an error instead of running unprotected. The startup status line reports how many ranges loaded. Note an `http(s)` blocklist is fetched with librqbit's own client, which is bound to **neither** the proxy **nor** `bind_interface` — use a **local file** when either is set, so the fetch can't escape.

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
