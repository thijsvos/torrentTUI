//! The vocabulary shared between the engine task and the UI.
//!
//! Everything here is a plain owned snapshot with no librqbit types in it. That
//! boundary is what lets the UI render without touching the session, and what
//! keeps librqbit version churn confined to `engine::torrent`.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
/// What the UI shows in the Status column. Derived fresh from librqbit's stats
/// on every tick, so it is a snapshot, not a state machine.
///
/// `Complete` and `Seeding` are the same underlying condition — a finished
/// torrent — split purely on whether it is uploading *right now*, so a seeding
/// torrent with no active peers flips between the two tick to tick. Code that
/// means "finished" must match both; several places in the engine do.
///
/// `Paused` always means the user (or a persisted previous session) paused
/// it. Speed limits are enforced inside librqbit's rate limiter, so the
/// engine never pauses a torrent on its own.
pub enum TorrentStatus {
    FetchingMetadata,
    Downloading,
    Paused,
    Complete,
    Seeding,
    Error(String),
}

impl fmt::Display for TorrentStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TorrentStatus::FetchingMetadata => write!(f, "Fetching Metadata"),
            TorrentStatus::Downloading => write!(f, "Downloading"),
            TorrentStatus::Paused => write!(f, "Paused"),
            TorrentStatus::Complete => write!(f, "Complete"),
            TorrentStatus::Seeding => write!(f, "Seeding"),
            TorrentStatus::Error(e) => write!(f, "Error: {}", e),
        }
    }
}

impl TorrentStatus {
    /// Stable, allocation-free key matching the `Display` text ordering used by
    /// the status-column sort, so comparisons don't `to_string()` each status.
    /// Returned as `(label, detail)`; `Error(msg)` orders by `msg` without
    /// building the full `"Error: {msg}"` string.
    pub fn sort_key(&self) -> (&str, &str) {
        match self {
            TorrentStatus::FetchingMetadata => ("Fetching Metadata", ""),
            TorrentStatus::Downloading => ("Downloading", ""),
            TorrentStatus::Paused => ("Paused", ""),
            TorrentStatus::Complete => ("Complete", ""),
            TorrentStatus::Seeding => ("Seeding", ""),
            TorrentStatus::Error(e) => ("Error: ", e),
        }
    }
}

#[derive(Debug, Clone)]
/// One torrent as the UI sees it — a snapshot rebuilt from scratch by the
/// engine on every tick and shipped over the state channel. Never mutate one
/// expecting it to persist; the next push replaces the whole `Vec`.
///
/// The heavy fields (`files`, `peers`, `trackers`, `info_hash`, `piece_length`)
/// are populated only for the torrent the UI is currently showing in Detail
/// mode — see `EngineCommand::SetDetailTorrent`. For every other torrent they
/// are empty. That is not "no data", it is "not asked for", and rendering them
/// outside the Detail view silently shows nothing. `health` is the exception:
/// it is cheap (librqbit already keeps the counters) and the table's stall
/// marker needs it for every row.
///
/// Speeds are bytes per second.
pub struct TorrentInfo {
    pub id: usize,
    pub name: String,
    pub size_bytes: u64,
    pub downloaded_bytes: u64,
    pub uploaded_bytes: u64,
    pub download_speed: u64,
    pub upload_speed: u64,
    pub peers_connected: u32,
    pub peers_total: u32,
    pub status: TorrentStatus,
    pub eta_seconds: Option<u64>,
    pub files: Vec<FileInfo>,
    pub peers: Vec<PeerInfo>,
    pub info_hash: String,
    /// Detail-only: every tracker librqbit knows for this torrent, with the
    /// announce status the health capture has seen for it.
    pub trackers: Vec<TrackerInfo>,
    pub piece_length: Option<u32>,
    /// Absolute path of the torrent's on-disk root, straight from librqbit's
    /// resolved per-torrent output folder — the multi-file subfolder, or the
    /// file itself for a torrent living directly in the download dir. `None`
    /// until metadata resolves. The `o` reveal keybinding uses this instead of
    /// re-deriving a path from the (display-sanitized) name, which can differ
    /// from what is actually on disk.
    pub content_path: Option<String>,
    /// The raw material for the stall doctor — see [`TorrentHealth`].
    pub health: TorrentHealth,
}

#[derive(Debug, Clone)]
pub struct FileInfo {
    pub name: String,
    pub size_bytes: u64,
    pub progress_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub address: String,
    pub state: String,
    pub downloaded_bytes: u64,
    pub pieces: u32,
    pub errors: u32,
    /// The client name from the extended handshake ("qBittorrent 5.0.1"),
    /// known only once a peer is live and has completed that handshake.
    pub client_name: Option<String>,
}

/// librqbit's per-torrent peer bookkeeping, every bucket. The table shows only
/// `live`/`seen`; the doctor needs the rest to tell "nobody found" from
/// "everybody found, nobody reachable" from "connected but starved".
///
/// `seen` is monotonic — every distinct address ever handed to the torrent —
/// so it is not the sum of the others.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PeerBreakdown {
    pub queued: u32,
    pub connecting: u32,
    pub live: u32,
    pub seen: u32,
    pub dead: u32,
    pub not_needed: u32,
    pub live_tcp: u32,
    pub live_utp: u32,
    pub live_socks: u32,
}

/// Tracker roll-up available for every torrent every tick (the per-tracker
/// list in `TorrentInfo::trackers` is detail-only). `total` counts trackers
/// librqbit is actually announcing to; `stripped_udp` counts the `udp://`
/// entries the proxy lockdown removed from the magnet before librqbit ever saw
/// it, which is why they are not in `total`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TrackerCounts {
    pub total: usize,
    /// Answered at least once and not failing now.
    pub ok: usize,
    /// Last announce errored.
    pub failing: usize,
    /// No announce result seen yet.
    pub pending: usize,
    /// A scheme librqbit refuses (`wss://` and the like).
    pub unsupported: usize,
    /// `udp://` trackers still present under a proxy — a restored torrent's
    /// announces go straight around the proxy.
    pub bypassing_proxy: usize,
    pub stripped_udp: usize,
}

/// Per-torrent health numbers, populated for EVERY torrent every tick — they
/// come straight out of counters librqbit maintains anyway, so filling them
/// costs nothing beyond the copy.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TorrentHealth {
    pub peers: PeerBreakdown,
    /// Bytes received from peers, including pieces that later failed their
    /// hash check and pieces still in flight.
    pub fetched_bytes: u64,
    /// Bytes that passed the hash check. `fetched - checked` is in-flight
    /// data plus everything discarded as corrupt.
    pub checked_bytes: u64,
    /// Mean wall time per completed piece; `None` until one has landed.
    pub avg_piece_ms: Option<u64>,
    pub trackers: TrackerCounts,
    /// Detail-only: the full error chain behind `TorrentStatus::Error`, which
    /// carries more than the one-line message the status column shows.
    pub error_chain: Option<String>,
}

/// One tracker as the Health and Info tabs show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackerInfo {
    pub url: String,
    pub status: TrackerStatus,
}

/// What the last announce to a tracker did. Reconstructed from librqbit's
/// own tracing events (it exposes no tracker state through its API), so this
/// is "what we overheard", never seeders/leechers — those are dropped
/// upstream before anything is logged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackerStatus {
    /// No announce has completed yet.
    Pending,
    /// The last announce succeeded this many seconds ago; librqbit will
    /// announce again after `next_in_secs` if the tracker said so.
    Ok {
        last_announce_secs_ago: u64,
        next_in_secs: Option<u64>,
    },
    /// The last announce failed with this (sanitized, truncated) error.
    Failing { last_error: String, secs_ago: u64 },
    /// librqbit refused the URL's scheme and never announces to it.
    Unsupported,
    /// A `udp://` tracker on a torrent restored under a SOCKS5 proxy: librqbit
    /// announces to it directly, around the proxy.
    BypassesProxy,
}

impl TrackerStatus {
    /// Short label for tables: one word, no colour needed to tell them apart.
    pub fn label(&self) -> &'static str {
        match self {
            TrackerStatus::Pending => "pending",
            TrackerStatus::Ok { .. } => "ok",
            TrackerStatus::Failing { .. } => "failing",
            TrackerStatus::Unsupported => "unsupported",
            TrackerStatus::BypassesProxy => "bypasses proxy",
        }
    }
}

/// Session-wide network facts, pushed by the engine about once a second as
/// `EngineInfo::Network`. Everything the doctor needs that is not about one
/// torrent: can we find peers at all (DHT), can anyone find us (listener,
/// UPnP), and do our outgoing connections actually succeed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NetworkHealth {
    /// The port librqbit accepts incoming connections on; `None` when the
    /// session runs without a listener (proxy lockdown).
    pub listen_port: Option<u16>,
    /// `None` when DHT is disabled — by config or by the proxy lockdown.
    pub dht: Option<DhtHealth>,
    /// Connections the blocklist refused, since session start.
    pub blocked_incoming: u64,
    pub blocked_outgoing: u64,
    pub connect: ConnectHealth,
    /// Session-wide peer buckets (every torrent summed).
    pub peers: PeerBreakdown,
    pub uptime_secs: u64,
    pub upnp: UpnpState,
    /// librqbit's listener defaults to TCP only; the doctor mentions this
    /// rather than showing a confusing all-zero uTP column.
    pub utp_enabled: bool,
    /// Adds still inside librqbit's `add_torrent`. A magnet sits here until a
    /// peer hands over its metadata — there is no torrent row to show until
    /// then, so a magnet nobody is seeding would otherwise just vanish.
    pub pending_adds: Vec<PendingAdd>,
}

/// One add in flight — see `NetworkHealth::pending_adds`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingAdd {
    /// The magnet's `dn=` name, a `.torrent` file name, or the hash prefix.
    pub label: String,
    pub secs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DhtHealth {
    /// Nodes in the IPv4 routing table. Zero for the first seconds after
    /// start (bootstrapping) — and forever when UDP is blocked.
    pub routing_table_size: usize,
    pub routing_table_size_v6: usize,
    pub outstanding_requests: usize,
}

/// Outgoing connection attempts per transport since session start, IPv4 and
/// IPv6 summed. `socks` counts only when a proxy is configured; `utp` only
/// when uTP is enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConnectHealth {
    pub tcp: TransportStats,
    pub socks: TransportStats,
    pub utp: TransportStats,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TransportStats {
    pub attempts: u64,
    pub successes: u64,
    pub errors: u64,
}

impl TransportStats {
    /// Failed attempts as a share of all attempts, 0.0 when nothing has been
    /// tried yet.
    pub fn failure_ratio(&self) -> f64 {
        if self.attempts == 0 {
            0.0
        } else {
            self.errors as f64 / self.attempts as f64
        }
    }
}

/// What the UPnP port forwarder reported, reconstructed from its tracing
/// output (librqbit keeps no handle to it).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum UpnpState {
    /// Not enabled (`network.enable_upnp = false`, or proxy lockdown).
    #[default]
    Off,
    /// Enabled, no result reported yet.
    Pending,
    Forwarded,
    Failed(String),
}

impl TorrentInfo {
    /// Completion as 0.0-100.0. Clamped at both ends because librqbit can
    /// briefly report more downloaded than total right after a recheck, which
    /// would otherwise overrun progress bars; a zero-size torrent (metadata
    /// still resolving) reads as 0 rather than dividing by zero.
    pub fn progress_percent(&self) -> f64 {
        if self.size_bytes == 0 {
            return 0.0;
        }
        ((self.downloaded_bytes as f64 / self.size_bytes as f64) * 100.0).clamp(0.0, 100.0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AppMode {
    Normal,
    Input,
    Detail,
    Help,
    ConfirmDelete,
    ConfirmQuit,
    /// Confirming a detach to the background. Unlike `ConfirmQuit` this is not
    /// gated on `general.confirm_on_quit`: starting a process that outlives the
    /// app is never allowed to be a single keystroke.
    ConfirmDetach,
    Filter,
    ThrottleInput,
    /// Typing an indexer-search query in the bottom bar. The main area keeps
    /// showing whatever was there (torrent table, or previous results).
    Search,
    /// Browsing indexer-search results (or the loading spinner) in the main
    /// area.
    SearchResults,
    /// The command palette overlay: fuzzy-search every action, Enter runs it.
    /// The view it opened over keeps rendering underneath; the mode to return
    /// to lives in `App::palette`.
    Palette,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SortColumn {
    Index = 0,
    Name = 1,
    Size = 2,
    Progress = 3,
    Speed = 4,
    Peers = 5,
    Eta = 6,
    Status = 7,
}

impl SortColumn {
    /// Position of this column in the rendered table, taken straight from the
    /// `#[repr(u8)]` discriminant. It indexes `table.rs`'s header labels and
    /// the cell order in `render_table`, so all three must be reordered
    /// together — a mismatch puts the sort arrow above the wrong heading with
    /// no error.
    pub fn column_index(&self) -> usize {
        *self as u8 as usize
    }

    pub fn next(self) -> Self {
        match self {
            SortColumn::Index => SortColumn::Name,
            SortColumn::Name => SortColumn::Size,
            SortColumn::Size => SortColumn::Progress,
            SortColumn::Progress => SortColumn::Speed,
            SortColumn::Speed => SortColumn::Peers,
            SortColumn::Peers => SortColumn::Eta,
            SortColumn::Eta => SortColumn::Status,
            SortColumn::Status => SortColumn::Index,
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DetailTab {
    Stats = 0,
    Info = 1,
    Files = 2,
    Peers = 3,
    /// The stall doctor's verdict plus the discovery, connectivity and
    /// transfer numbers it was built from.
    Health = 4,
}

impl DetailTab {
    pub fn next(self) -> Self {
        match self {
            DetailTab::Stats => DetailTab::Info,
            DetailTab::Info => DetailTab::Files,
            DetailTab::Files => DetailTab::Peers,
            DetailTab::Peers => DetailTab::Health,
            DetailTab::Health => DetailTab::Stats,
        }
    }

    /// Position of this tab in the rendered tab bar. Indexes the titles built
    /// in `detail.rs`, so the two must stay in the same order.
    pub fn index(self) -> usize {
        self as u8 as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_torrent(id: usize, size: u64, downloaded: u64) -> TorrentInfo {
        TorrentInfo {
            id,
            name: format!("torrent_{}", id),
            size_bytes: size,
            downloaded_bytes: downloaded,
            uploaded_bytes: 0,
            download_speed: 0,
            upload_speed: 0,
            peers_connected: 0,
            peers_total: 0,
            status: TorrentStatus::Downloading,
            eta_seconds: None,
            files: Vec::new(),
            peers: Vec::new(),
            info_hash: String::new(),
            trackers: Vec::new(),
            piece_length: None,
            content_path: None,
            health: TorrentHealth::default(),
        }
    }

    #[test]
    fn progress_percent_normal() {
        let t = make_torrent(0, 100, 50);
        assert!((t.progress_percent() - 50.0).abs() < f64::EPSILON);
    }

    #[test]
    fn progress_percent_complete() {
        let t = make_torrent(0, 100, 100);
        assert!((t.progress_percent() - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn progress_percent_zero_size() {
        let t = make_torrent(0, 0, 0);
        assert!((t.progress_percent() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn progress_percent_clamped_to_100() {
        // librqbit can briefly report downloaded > size after rechecks.
        let t = make_torrent(0, 100, 200);
        assert!((t.progress_percent() - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn status_display() {
        assert_eq!(
            TorrentStatus::FetchingMetadata.to_string(),
            "Fetching Metadata"
        );
        assert_eq!(TorrentStatus::Downloading.to_string(), "Downloading");
        assert_eq!(TorrentStatus::Paused.to_string(), "Paused");
        assert_eq!(TorrentStatus::Complete.to_string(), "Complete");
        assert_eq!(TorrentStatus::Seeding.to_string(), "Seeding");
        assert_eq!(
            TorrentStatus::Error("disk full".to_string()).to_string(),
            "Error: disk full"
        );
    }

    #[test]
    fn sort_column_indices() {
        assert_eq!(SortColumn::Index.column_index(), 0);
        assert_eq!(SortColumn::Name.column_index(), 1);
        assert_eq!(SortColumn::Status.column_index(), 7);
    }

    #[test]
    fn sort_column_next_cycles() {
        let mut col = SortColumn::Index;
        for _ in 0..8 {
            col = col.next();
        }
        assert_eq!(col, SortColumn::Index);
    }

    #[test]
    fn detail_tab_next_cycles() {
        let mut tab = DetailTab::Stats;
        for _ in 0..5 {
            tab = tab.next();
        }
        assert_eq!(tab, DetailTab::Stats);
    }

    #[test]
    fn detail_tab_index_matches_repr() {
        assert_eq!(DetailTab::Stats.index(), 0);
        assert_eq!(DetailTab::Info.index(), 1);
        assert_eq!(DetailTab::Files.index(), 2);
        assert_eq!(DetailTab::Peers.index(), 3);
        assert_eq!(DetailTab::Health.index(), 4);
    }

    #[test]
    fn tracker_status_labels_are_distinct_words() {
        // The Info and Health tabs rely on the word alone, never colour, to
        // tell tracker states apart.
        let labels = [
            TrackerStatus::Pending.label(),
            TrackerStatus::Ok {
                last_announce_secs_ago: 1,
                next_in_secs: None,
            }
            .label(),
            TrackerStatus::Failing {
                last_error: "x".into(),
                secs_ago: 1,
            }
            .label(),
            TrackerStatus::Unsupported.label(),
            TrackerStatus::BypassesProxy.label(),
        ];
        for (i, a) in labels.iter().enumerate() {
            for b in &labels[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn sort_key_matches_display_ordering() {
        // sort_key is an allocation-free stand-in for comparing Display strings
        // in the status-column sort; it must order pairwise identically.
        let statuses = [
            TorrentStatus::FetchingMetadata,
            TorrentStatus::Downloading,
            TorrentStatus::Paused,
            TorrentStatus::Complete,
            TorrentStatus::Seeding,
            TorrentStatus::Error("alpha".to_string()),
            TorrentStatus::Error("beta".to_string()),
        ];
        for a in &statuses {
            for b in &statuses {
                assert_eq!(
                    a.sort_key().cmp(&b.sort_key()),
                    a.to_string().cmp(&b.to_string()),
                    "sort_key disagreed with Display for {a} vs {b}"
                );
            }
        }
    }
}
