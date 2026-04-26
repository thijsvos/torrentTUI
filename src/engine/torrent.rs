use crate::config::Config;
use crate::types::{FileInfo, PeerInfo, TorrentInfo, TorrentStatus};
use crate::ui::util::sanitize_display;
use anyhow::Result;
use librqbit::{
    AddTorrent, AddTorrentResponse, ManagedTorrent, Session, SessionOptions,
    SessionPersistenceConfig, TorrentStatsState,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

pub type ManagedTorrentHandle = Arc<ManagedTorrent>;

// ---------------------------------------------------------------------------
// Tunables
// ---------------------------------------------------------------------------

/// 1 Mbps in bytes/sec (1_000_000 bits/s ÷ 8 = 125_000 B/s).
const MBPS_TO_BPS: f64 = 125_000.0;

/// How many sequential ports the engine binds. The Dockerfile EXPOSE range is
/// derived from this, so changes need to be mirrored there.
const PORT_RANGE_SIZE: u16 = 10;

/// Maximum size of a `.torrent` file accepted on disk. Anything larger is
/// rejected before a full read, both as a sanity check and to prevent a
/// symlink-to-huge-file OOM.
const MAX_TORRENT_FILE_SIZE: u64 = 10 * 1024 * 1024;

// Throttle algorithm tuning -------------------------------------------------

/// How often the throttle loop runs.
const THROTTLE_TICK: std::time::Duration = std::time::Duration::from_millis(100);

/// Window over which an "effective" download speed is computed for throttled
/// torrents (so the displayed speed averages out the duty cycle).
const SPEED_WINDOW_SECS: f64 = 5.0;

/// Fraction of the per-torrent budget that must be reaccumulated before a
/// throttle-paused torrent is unpaused. Hysteresis to prevent oscillation.
const UNPAUSE_HYSTERESIS: f64 = 0.2;

/// Maximum burst, as a multiple of the steady-state per-torrent budget.
const BURST_MULTIPLIER: i64 = 2;

/// Minimum time between pause/unpause transitions for a single torrent.
const STATE_CHANGE_COOLDOWN: std::time::Duration = std::time::Duration::from_millis(1000);

// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum EngineCommand {
    AddTorrent(String),
    Pause(usize),
    Resume(usize),
    Delete {
        id: usize,
        delete_files: bool,
    },
    PauseAll,
    ResumeAll,
    SetSpeedLimits {
        download_kbps: u64,
        upload_kbps: u64,
    },
    SetSelectedFiles {
        id: usize,
        file_indices: Vec<usize>,
    },
    Shutdown,
}

/// Lightweight per-torrent snapshot used by the throttle loop. Avoids the cost
/// of building full peer/file lists on every 100 ms tick.
struct ThrottleSnapshot {
    id: usize,
    status: TorrentStatus,
    downloaded_bytes: u64,
    upload_speed: u64,
}

pub struct TorrentEngine {
    session: Arc<Session>,
}

impl TorrentEngine {
    pub async fn new(config: &Config) -> Result<Self> {
        let download_dir = PathBuf::from(&config.general.download_dir);
        std::fs::create_dir_all(&download_dir)?;

        let port = config.network.listen_port;
        let opts = SessionOptions {
            disable_dht: !config.network.enable_dht,
            fastresume: true,
            persistence: Some(SessionPersistenceConfig::Json { folder: None }),
            listen_port_range: Some(port..port + PORT_RANGE_SIZE),
            enable_upnp_port_forwarding: config.network.enable_upnp,
            ..Default::default()
        };

        let session = Session::new_with_opts(download_dir, opts).await?;
        Ok(Self { session })
    }

    pub async fn add_torrent(&self, source: &str) -> Result<(usize, ManagedTorrentHandle, bool)> {
        let source = source.trim();
        let add_torrent = if source.starts_with("magnet:?") {
            AddTorrent::from_url(source)
        } else {
            // Treat as .torrent file path. Cap size to avoid a malicious
            // symlink turning the read into an OOM.
            let meta = tokio::fs::metadata(source).await?;
            anyhow::ensure!(
                meta.len() <= MAX_TORRENT_FILE_SIZE,
                ".torrent file too large (>{} bytes)",
                MAX_TORRENT_FILE_SIZE
            );
            let bytes = tokio::fs::read(source).await?;
            AddTorrent::from_bytes(bytes)
        };

        let response = self.session.add_torrent(add_torrent, None).await?;

        match response {
            AddTorrentResponse::Added(id, handle) => Ok((id, handle, false)),
            AddTorrentResponse::AlreadyManaged(id, handle) => Ok((id, handle, true)),
            AddTorrentResponse::ListOnly(_) => {
                anyhow::bail!("Torrent was list-only")
            }
        }
    }

    pub async fn pause(&self, handle: &ManagedTorrentHandle) -> Result<()> {
        self.session.pause(handle).await?;
        Ok(())
    }

    pub async fn unpause(&self, handle: &ManagedTorrentHandle) -> Result<()> {
        self.session.unpause(handle).await?;
        Ok(())
    }

    pub async fn delete(&self, id: usize, delete_files: bool) -> Result<()> {
        use librqbit::api::TorrentIdOrHash;
        self.session
            .delete(TorrentIdOrHash::Id(id), delete_files)
            .await?;
        Ok(())
    }

    /// Lightweight snapshot of `(id, handle)` pairs. Lets the throttle loop
    /// look up handles in O(1) instead of O(N) `get_handle` per torrent.
    fn handle_snapshot(&self) -> Vec<(usize, ManagedTorrentHandle)> {
        self.session
            .with_torrents(|iter| iter.map(|(id, h)| (id, h.clone())).collect())
    }

    /// Cheap snapshot for the throttle loop. Does not allocate file/peer
    /// lists, which dominate the cost of `get_all_torrents`.
    fn throttle_snapshot(&self) -> Vec<ThrottleSnapshot> {
        self.session.with_torrents(|iter| {
            iter.map(|(id, handle)| {
                let stats = handle.stats();
                let upload_speed = stats
                    .live
                    .as_ref()
                    .map(|l| (l.upload_speed.mbps * MBPS_TO_BPS) as u64)
                    .unwrap_or(0);
                ThrottleSnapshot {
                    id,
                    status: derive_status(&stats),
                    downloaded_bytes: stats.progress_bytes,
                    upload_speed,
                }
            })
            .collect()
        })
    }

    pub fn get_all_torrents(&self) -> Vec<TorrentInfo> {
        self.session.with_torrents(|iter| {
            iter.map(|(id, handle)| {
                let stats = handle.stats();
                let raw_name = handle
                    .name()
                    .unwrap_or_else(|| "Fetching metadata...".to_string());
                // Sanitize at the engine boundary so every consumer (table,
                // detail header, dialogs, status bar, desktop notification
                // body via libnotify Pango markup) sees safe text.
                let name = sanitize_display(&raw_name);

                let uploaded_bytes = stats.uploaded_bytes;
                let status = derive_status(&stats);

                let (download_speed, upload_speed, eta_seconds, peers_connected) =
                    if let Some(ref live) = stats.live {
                        let dl_bps = (live.download_speed.mbps * MBPS_TO_BPS) as u64;
                        let ul_bps = (live.upload_speed.mbps * MBPS_TO_BPS) as u64;
                        let remaining = stats.total_bytes.saturating_sub(stats.progress_bytes);
                        let eta = compute_eta(remaining, dl_bps);
                        let peers = live.snapshot.peer_stats.live as u32;
                        (dl_bps, ul_bps, eta, peers)
                    } else {
                        (0, 0, None, 0)
                    };

                let peers_total = if let Some(ref live) = stats.live {
                    (live.snapshot.peer_stats.live + live.snapshot.peer_stats.seen) as u32
                } else {
                    0
                };

                let files = handle
                    .with_metadata(|meta| {
                        meta.file_infos
                            .iter()
                            .enumerate()
                            .map(|(i, fi)| {
                                let progress = stats.file_progress.get(i).copied().unwrap_or(0);
                                FileInfo {
                                    name: sanitize_display(&fi.relative_filename.to_string_lossy()),
                                    size_bytes: fi.len,
                                    progress_bytes: progress,
                                }
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();

                let info_hash = handle.info_hash().as_string();
                let trackers: Vec<String> = handle
                    .shared()
                    .trackers
                    .iter()
                    .map(|u| u.to_string())
                    .collect();
                let piece_length = handle.with_metadata(|m| m.info.piece_length).ok();

                // Don't sort here: only the selected torrent's peers are ever
                // displayed, and the detail-view renderer sorts lazily.
                let peers: Vec<PeerInfo> = handle
                    .live()
                    .map(|live| {
                        let snapshot = live.per_peer_stats_snapshot(Default::default());
                        snapshot
                            .peers
                            .into_iter()
                            .map(|(addr, ps)| PeerInfo {
                                address: addr,
                                state: ps.state.to_string(),
                                downloaded_bytes: ps.counters.fetched_bytes,
                                pieces: ps.counters.downloaded_and_checked_pieces,
                                errors: ps.counters.errors,
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                TorrentInfo {
                    id,
                    name,
                    size_bytes: stats.total_bytes,
                    downloaded_bytes: stats.progress_bytes,
                    uploaded_bytes,
                    download_speed,
                    upload_speed,
                    peers_connected,
                    peers_total,
                    status,
                    eta_seconds,
                    files,
                    peers,
                    info_hash,
                    trackers,
                    piece_length,
                    throttle_paused: false, // set by push_state
                }
            })
            .collect()
        })
    }

    pub fn get_handle(&self, id: usize) -> Option<ManagedTorrentHandle> {
        self.session.with_torrents(|iter| {
            for (tid, handle) in iter {
                if tid == id {
                    return Some(handle.clone());
                }
            }
            None
        })
    }

    pub fn session(&self) -> &Arc<Session> {
        &self.session
    }
}

/// Map librqbit's stats into our user-facing `TorrentStatus`. Pure helper so
/// the branching can be unit-tested.
fn derive_status(stats: &librqbit::TorrentStats) -> TorrentStatus {
    match stats.state {
        TorrentStatsState::Initializing => TorrentStatus::FetchingMetadata,
        TorrentStatsState::Live => {
            if stats.finished {
                let ul_speed = stats
                    .live
                    .as_ref()
                    .map(|l| (l.upload_speed.mbps * MBPS_TO_BPS) as u64)
                    .unwrap_or(0);
                if ul_speed > 0 {
                    TorrentStatus::Seeding
                } else {
                    TorrentStatus::Complete
                }
            } else {
                TorrentStatus::Downloading
            }
        }
        TorrentStatsState::Paused => TorrentStatus::Paused,
        TorrentStatsState::Error => TorrentStatus::Error(stats.error.clone().unwrap_or_default()),
    }
}

/// ETA in seconds. Returns `None` for stalled downloads (`dl_bps == 0`) so
/// callers can render "—" instead of a misleading "0s".
pub(crate) fn compute_eta(remaining: u64, dl_bps: u64) -> Option<u64> {
    if dl_bps == 0 {
        None
    } else {
        // Round up so a download with <1s remaining shows "1s" rather than 0
        // (which the formatter renders as "—").
        Some(remaining.div_ceil(dl_bps))
    }
}

/// One step of the per-torrent token bucket. Returns the new token balance
/// after crediting `rate * elapsed_secs` and debiting `bytes_delta`, capped at
/// `BURST_MULTIPLIER * rate`. Pure helper so the math can be tested directly.
pub(crate) fn step_bucket(prev: i64, rate: i64, elapsed_secs: f64, bytes_delta: i64) -> i64 {
    let credit = (rate as f64 * elapsed_secs) as i64;
    let next = prev.saturating_add(credit).saturating_sub(bytes_delta);
    next.min(rate.saturating_mul(BURST_MULTIPLIER))
}

pub async fn run_engine(
    config: Config,
    cmd_rx: mpsc::Receiver<EngineCommand>,
    state_tx: mpsc::Sender<Vec<TorrentInfo>>,
    msg_tx: mpsc::Sender<String>,
) -> Result<()> {
    let engine = TorrentEngine::new(&config).await?;

    // Watch folder for auto-adding torrents. Off by default; only enabled if
    // the user opts in via config.
    if let Some(ref dir) = config.general.watch_dir {
        let path = PathBuf::from(dir);
        std::fs::create_dir_all(&path)?;
        engine.session().watch_folder(&path);
        tracing::info!("Watching folder: {}", dir);
    }

    let enable_notifications = config.ui.enable_notifications;
    let mut cmd_rx = cmd_rx;

    let mut finished_set: HashSet<usize> = HashSet::new();

    // Speed-limit state. Use saturating_mul to avoid u64 overflow when the
    // user types an unreasonably large limit.
    let mut download_limit_bps: u64 = config.network.max_download_speed_kbps.saturating_mul(1024);
    let mut upload_limit_bps: u64 = config.network.max_upload_speed_kbps.saturating_mul(1024);
    let mut throttle_paused: HashSet<usize> = HashSet::new();
    let mut throttle_managed: HashSet<usize> = HashSet::new();
    let mut user_paused: HashSet<usize> = HashSet::new();
    let mut per_torrent_tokens: HashMap<usize, i64> = HashMap::new();
    let mut per_torrent_prev_bytes: HashMap<usize, u64> = HashMap::new();
    let mut ul_tokens: i64 = 0;
    let mut prev_ul_estimated: f64 = 0.0;
    let mut last_throttle_tick = std::time::Instant::now();
    let mut cached_peers: HashMap<usize, (u32, u32)> = HashMap::new();
    let mut cached_upload_speed: HashMap<usize, u64> = HashMap::new();
    let mut speed_tracker: HashMap<usize, (std::time::Instant, u64, u64)> = HashMap::new();
    let mut per_torrent_last_change: HashMap<usize, std::time::Instant> = HashMap::new();

    #[allow(clippy::too_many_arguments)]
    async fn push_state(
        engine: &TorrentEngine,
        state_tx: &mpsc::Sender<Vec<TorrentInfo>>,
        msg_tx: &mpsc::Sender<String>,
        finished_set: &mut HashSet<usize>,
        throttle_managed: &HashSet<usize>,
        download_limit_bps: u64,
        cached_peers: &mut HashMap<usize, (u32, u32)>,
        cached_upload_speed: &mut HashMap<usize, u64>,
        speed_tracker: &mut HashMap<usize, (std::time::Instant, u64, u64)>,
        enable_notifications: bool,
    ) {
        let now = std::time::Instant::now();
        let mut torrents = engine.get_all_torrents();

        let current_ids: HashSet<usize> = torrents.iter().map(|t| t.id).collect();
        finished_set.retain(|id| current_ids.contains(id));
        cached_peers.retain(|id, _| current_ids.contains(id));
        cached_upload_speed.retain(|id, _| current_ids.contains(id));
        speed_tracker.retain(|id, _| current_ids.contains(id));

        let managed_count = throttle_managed.len().max(1) as u64;

        for t in &mut torrents {
            if matches!(t.status, TorrentStatus::Complete | TorrentStatus::Seeding)
                && !finished_set.contains(&t.id)
            {
                finished_set.insert(t.id);
                let _ = msg_tx
                    .send(format!("\u{2713} \"{}\" complete", t.name))
                    .await;

                if enable_notifications {
                    #[cfg(target_os = "macos")]
                    {
                        tokio::task::spawn_blocking(|| {
                            let _ = std::process::Command::new("afplay")
                                .arg("/System/Library/Sounds/Glass.aiff")
                                .output();
                        });
                    }
                    #[cfg(not(target_os = "macos"))]
                    {
                        // Name is already sanitized at the engine boundary, so
                        // it's safe to embed in the libnotify body (which is
                        // parsed as Pango markup on most Linux desktops).
                        let name = t.name.clone();
                        let size = crate::ui::layout::format_size(t.size_bytes);
                        tokio::task::spawn_blocking(move || match notify_rust::Notification::new()
                            .summary("Download Complete")
                            .body(&format!("{} ({})", name, size))
                            .appname("TorrentTUI")
                            .timeout(5000)
                            .show()
                        {
                            Ok(_) => tracing::info!("System notification sent"),
                            Err(e) => tracing::error!("System notification failed: {}", e),
                        });
                    }
                }
            }
            if throttle_managed.contains(&t.id)
                && !matches!(t.status, TorrentStatus::Complete | TorrentStatus::Seeding)
            {
                t.throttle_paused = true;
                let tracker = speed_tracker
                    .entry(t.id)
                    .or_insert((now, t.downloaded_bytes, 0));
                let elapsed = now.duration_since(tracker.0).as_secs_f64();
                if elapsed >= SPEED_WINDOW_SECS {
                    let bytes_delta = t.downloaded_bytes.saturating_sub(tracker.1);
                    tracker.2 = (bytes_delta as f64 / elapsed) as u64;
                    tracker.0 = now;
                    tracker.1 = t.downloaded_bytes;
                }
                t.download_speed = tracker.2;
                if download_limit_bps > 0 {
                    t.download_speed = t.download_speed.min(download_limit_bps / managed_count);
                }
                if t.download_speed > 0 {
                    let remaining = t.size_bytes.saturating_sub(t.downloaded_bytes);
                    t.eta_seconds = compute_eta(remaining, t.download_speed);
                }
                if t.peers_connected > 0 || t.peers_total > 0 {
                    cached_peers.insert(t.id, (t.peers_connected, t.peers_total));
                } else if let Some(&(c_conn, c_total)) = cached_peers.get(&t.id) {
                    t.peers_connected = c_conn;
                    t.peers_total = c_total;
                }
                if t.upload_speed > 0 {
                    cached_upload_speed.insert(t.id, t.upload_speed);
                } else if let Some(&c_ul) = cached_upload_speed.get(&t.id) {
                    t.upload_speed = c_ul;
                }
            }
        }

        let _ = state_tx.send(torrents).await;
    }

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(EngineCommand::AddTorrent(source)) => {
                        match engine.add_torrent(&source).await {
                            Ok((id, handle, already_managed)) => {
                                let stats = handle.stats();
                                let raw = handle.name().unwrap_or_else(|| "Unknown".to_string());
                                let name = sanitize_display(&raw);
                                if already_managed || stats.finished {
                                    let _ = msg_tx
                                        .send(format!("\"{}\" already downloaded", name))
                                        .await;
                                    if stats.finished {
                                        finished_set.insert(id);
                                    }
                                } else {
                                    tracing::info!("Added torrent {}", id);
                                }
                            }
                            Err(e) => {
                                let _ = msg_tx.send(format!("Failed to add torrent: {}", e)).await;
                                tracing::error!("Failed to add torrent: {}", e);
                            }
                        }
                    }
                    Some(EngineCommand::Pause(id)) => {
                        if let Some(handle) = engine.get_handle(id) {
                            if let Err(e) = engine.pause(&handle).await {
                                tracing::error!("Failed to pause torrent {}: {}", id, e);
                            }
                        }
                        user_paused.insert(id);
                        throttle_paused.remove(&id);
                        throttle_managed.remove(&id);
                        per_torrent_tokens.remove(&id);
                        per_torrent_prev_bytes.remove(&id);
                        per_torrent_last_change.remove(&id);
                        cached_upload_speed.remove(&id);
                        speed_tracker.remove(&id);
                    }
                    Some(EngineCommand::Resume(id)) => {
                        if let Some(handle) = engine.get_handle(id) {
                            if let Err(e) = engine.unpause(&handle).await {
                                tracing::error!("Failed to resume torrent {}: {}", id, e);
                            }
                        }
                        user_paused.remove(&id);
                        throttle_paused.remove(&id);
                        let throttling = download_limit_bps > 0 || upload_limit_bps > 0;
                        if throttling {
                            throttle_managed.insert(id);
                        } else {
                            throttle_managed.remove(&id);
                        }
                        per_torrent_tokens.insert(id, 0);
                        per_torrent_prev_bytes.remove(&id);
                        per_torrent_last_change.remove(&id);
                        cached_upload_speed.remove(&id);
                        ul_tokens = 0;
                        last_throttle_tick = std::time::Instant::now();
                    }
                    Some(EngineCommand::Delete { id, delete_files }) => {
                        if let Err(e) = engine.delete(id, delete_files).await {
                            tracing::error!("Failed to delete torrent {}: {}", id, e);
                        }
                        finished_set.remove(&id);
                        user_paused.remove(&id);
                        throttle_paused.remove(&id);
                        throttle_managed.remove(&id);
                        per_torrent_tokens.remove(&id);
                        per_torrent_prev_bytes.remove(&id);
                        per_torrent_last_change.remove(&id);
                        cached_upload_speed.remove(&id);
                        speed_tracker.remove(&id);
                    }
                    Some(EngineCommand::PauseAll) => {
                        for (id, handle) in engine.handle_snapshot() {
                            let _ = engine.pause(&handle).await;
                            user_paused.insert(id);
                        }
                        throttle_paused.clear();
                        throttle_managed.clear();
                        per_torrent_tokens.clear();
                        per_torrent_prev_bytes.clear();
                        per_torrent_last_change.clear();
                        cached_upload_speed.clear();
                        speed_tracker.clear();
                    }
                    Some(EngineCommand::ResumeAll) => {
                        let throttling = download_limit_bps > 0 || upload_limit_bps > 0;
                        for (id, handle) in engine.handle_snapshot() {
                            let _ = engine.unpause(&handle).await;
                            user_paused.remove(&id);
                            if throttling {
                                throttle_managed.insert(id);
                            }
                        }
                        throttle_paused.clear();
                        if !throttling {
                            throttle_managed.clear();
                        }
                        per_torrent_tokens.clear();
                        per_torrent_prev_bytes.clear();
                        per_torrent_last_change.clear();
                        cached_upload_speed.clear();
                        ul_tokens = 0;
                        last_throttle_tick = std::time::Instant::now();
                    }
                    Some(EngineCommand::SetSpeedLimits { download_kbps, upload_kbps }) => {
                        download_limit_bps = download_kbps.saturating_mul(1024);
                        upload_limit_bps = upload_kbps.saturating_mul(1024);
                        per_torrent_tokens.clear();
                        per_torrent_prev_bytes.clear();
                        per_torrent_last_change.clear();
                        cached_upload_speed.clear();
                        ul_tokens = 0;
                        last_throttle_tick = std::time::Instant::now();
                        tracing::info!(
                            "Speed limits set: down={}KB/s up={}KB/s",
                            download_kbps, upload_kbps
                        );
                        let _ = msg_tx.send(format!(
                            "Speed limits updated: \u{2193} {} KB/s / \u{2191} {} KB/s",
                            download_kbps, upload_kbps
                        )).await;
                        if download_kbps == 0 && upload_kbps == 0 {
                            for id in throttle_paused.drain() {
                                if let Some(handle) = engine.get_handle(id) {
                                    let _ = engine.unpause(&handle).await;
                                }
                            }
                            throttle_managed.clear();
                        }
                    }
                    Some(EngineCommand::SetSelectedFiles { id, file_indices }) => {
                        if let Some(handle) = engine.get_handle(id) {
                            let file_set: HashSet<usize> = file_indices.iter().copied().collect();
                            match engine.session().update_only_files(&handle, &file_set).await {
                                Ok(()) => {
                                    let _ = msg_tx.send(format!(
                                        "File selection applied ({} files selected)",
                                        file_indices.len()
                                    )).await;
                                }
                                Err(e) => {
                                    tracing::error!("Failed to update file selection for torrent {}: {}", id, e);
                                    let _ = msg_tx.send(format!("Failed to update file selection: {}", e)).await;
                                }
                            }
                        }
                    }
                    Some(EngineCommand::Shutdown) | None => {
                        tracing::info!("Engine shutting down");
                        break;
                    }
                }
                push_state(&engine, &state_tx, &msg_tx, &mut finished_set, &throttle_managed, download_limit_bps, &mut cached_peers, &mut cached_upload_speed, &mut speed_tracker, enable_notifications).await;
            }
            _ = tokio::time::sleep(THROTTLE_TICK) => {
                let throttling = download_limit_bps > 0 || upload_limit_bps > 0;
                if throttling {
                    let now = std::time::Instant::now();
                    let elapsed_secs = now.duration_since(last_throttle_tick).as_secs_f64();
                    last_throttle_tick = now;

                    // Lightweight snapshot avoids the full peer/file allocation
                    // path that dominates the cost of get_all_torrents.
                    let snapshot = engine.throttle_snapshot();
                    // O(1) handle lookup instead of O(N) get_handle per call.
                    let handle_map: HashMap<usize, ManagedTorrentHandle> =
                        engine.handle_snapshot().into_iter().collect();

                    for t in &snapshot {
                        if matches!(t.status, TorrentStatus::Downloading)
                            && !user_paused.contains(&t.id)
                            && !throttle_managed.contains(&t.id)
                        {
                            throttle_managed.insert(t.id);
                            per_torrent_prev_bytes.entry(t.id).or_insert(t.downloaded_bytes);
                        }
                    }

                    if download_limit_bps > 0 {
                        let active_count = snapshot.iter()
                            .filter(|t| throttle_managed.contains(&t.id)
                                && !user_paused.contains(&t.id)
                                && !matches!(t.status, TorrentStatus::Complete | TorrentStatus::Seeding))
                            .count()
                            .max(1) as u64;
                        let per_torrent_limit = download_limit_bps / active_count;
                        let unpause_threshold =
                            (per_torrent_limit as f64 * UNPAUSE_HYSTERESIS) as i64;

                        for t in &snapshot {
                            if !throttle_managed.contains(&t.id)
                                || user_paused.contains(&t.id)
                                || matches!(t.status, TorrentStatus::Complete | TorrentStatus::Seeding)
                            {
                                continue;
                            }

                            let prev = per_torrent_prev_bytes
                                .entry(t.id)
                                .or_insert(t.downloaded_bytes);
                            let delta = t.downloaded_bytes.saturating_sub(*prev) as i64;
                            *prev = t.downloaded_bytes;

                            let tokens_entry = per_torrent_tokens.entry(t.id).or_insert(0);
                            *tokens_entry = step_bucket(
                                *tokens_entry,
                                per_torrent_limit as i64,
                                elapsed_secs,
                                delta,
                            );
                            let tokens = *tokens_entry;

                            let can_change = per_torrent_last_change
                                .get(&t.id)
                                .is_none_or(|lc| {
                                    now.duration_since(*lc) >= STATE_CHANGE_COOLDOWN
                                });

                            if tokens < 0 {
                                if can_change
                                    && !throttle_paused.contains(&t.id)
                                    && matches!(t.status, TorrentStatus::Downloading)
                                {
                                    if let Some(handle) = handle_map.get(&t.id) {
                                        let _ = engine.pause(handle).await;
                                        throttle_paused.insert(t.id);
                                        per_torrent_last_change.insert(t.id, now);
                                    }
                                }
                            } else if tokens > unpause_threshold
                                && can_change
                                && (throttle_paused.contains(&t.id)
                                    || matches!(t.status, TorrentStatus::Paused))
                                && !user_paused.contains(&t.id)
                            {
                                if let Some(handle) = handle_map.get(&t.id) {
                                    let _ = engine.unpause(handle).await;
                                    throttle_paused.remove(&t.id);
                                    per_torrent_last_change.insert(t.id, now);
                                }
                            }
                        }
                    }

                    if upload_limit_bps > 0 {
                        let current_ul_speed: u64 = snapshot.iter().map(|t| t.upload_speed).sum();
                        let ul_delta = (current_ul_speed as f64 * elapsed_secs) + prev_ul_estimated;
                        let ul_delta_whole = ul_delta as i64;
                        prev_ul_estimated = ul_delta - ul_delta_whole as f64;

                        ul_tokens = ul_tokens
                            .saturating_add((upload_limit_bps as f64 * elapsed_secs) as i64)
                            .saturating_sub(ul_delta_whole)
                            .min(upload_limit_bps as i64);

                        let unpause_threshold =
                            (upload_limit_bps as f64 * UNPAUSE_HYSTERESIS) as i64;

                        if ul_tokens < 0 {
                            for t in &snapshot {
                                if matches!(t.status, TorrentStatus::Downloading)
                                    && !user_paused.contains(&t.id)
                                    && !throttle_paused.contains(&t.id)
                                {
                                    if let Some(handle) = handle_map.get(&t.id) {
                                        let _ = engine.pause(handle).await;
                                        throttle_paused.insert(t.id);
                                        throttle_managed.insert(t.id);
                                    }
                                }
                            }
                        } else if ul_tokens > unpause_threshold {
                            // Symmetric unpause: previously the upload throttle
                            // paused but never unpaused, so torrents got stuck.
                            for t in &snapshot {
                                if throttle_paused.contains(&t.id)
                                    && !user_paused.contains(&t.id)
                                {
                                    if let Some(handle) = handle_map.get(&t.id) {
                                        let _ = engine.unpause(handle).await;
                                        throttle_paused.remove(&t.id);
                                    }
                                }
                            }
                        }
                    }

                    for t in &snapshot {
                        if matches!(t.status, TorrentStatus::Complete | TorrentStatus::Seeding) {
                            throttle_managed.remove(&t.id);
                            throttle_paused.remove(&t.id);
                            per_torrent_tokens.remove(&t.id);
                            per_torrent_prev_bytes.remove(&t.id);
                            per_torrent_last_change.remove(&t.id);
                            cached_upload_speed.remove(&t.id);
                            speed_tracker.remove(&t.id);
                        }
                    }

                    let current_ids: HashSet<usize> = snapshot.iter().map(|t| t.id).collect();
                    throttle_paused.retain(|id| current_ids.contains(id));
                    throttle_managed.retain(|id| current_ids.contains(id));
                    user_paused.retain(|id| current_ids.contains(id));
                    per_torrent_tokens.retain(|id, _| current_ids.contains(id));
                    per_torrent_prev_bytes.retain(|id, _| current_ids.contains(id));
                    per_torrent_last_change.retain(|id, _| current_ids.contains(id));
                }

                push_state(&engine, &state_tx, &msg_tx, &mut finished_set, &throttle_managed, download_limit_bps, &mut cached_peers, &mut cached_upload_speed, &mut speed_tracker, enable_notifications).await;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eta_stalled_returns_none() {
        assert_eq!(compute_eta(1024, 0), None);
    }

    #[test]
    fn eta_normal() {
        assert_eq!(compute_eta(1024, 256), Some(4));
    }

    #[test]
    fn eta_remaining_below_speed_rounds_up() {
        // div_ceil avoids the misleading "0s" when remaining < dl_bps.
        assert_eq!(compute_eta(100, 1000), Some(1));
    }

    #[test]
    fn eta_remaining_zero() {
        assert_eq!(compute_eta(0, 100), Some(0));
    }

    #[test]
    fn step_bucket_credits_then_debits() {
        // 1 MB/s rate, 0.1s elapsed -> credit ~100_000; debit 50_000.
        let next = step_bucket(0, 1_000_000, 0.1, 50_000);
        assert_eq!(next, 50_000);
    }

    #[test]
    fn step_bucket_caps_at_burst() {
        // Even with a huge previous balance, cap at 2 * rate.
        let next = step_bucket(i64::MAX, 1_000, 0.1, 0);
        assert_eq!(next, 2_000);
    }

    #[test]
    fn step_bucket_can_go_negative() {
        // Spent more than credited — expected, drives the pause decision.
        let next = step_bucket(0, 100, 0.1, 1_000);
        assert!(next < 0);
    }

    #[test]
    fn engine_command_is_debug() {
        // Smoke test the derive added for tracing/panic dumps.
        let cmd = EngineCommand::Pause(42);
        assert!(format!("{cmd:?}").contains("Pause"));
    }
}
