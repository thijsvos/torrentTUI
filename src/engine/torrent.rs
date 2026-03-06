use crate::config::Config;
use crate::types::{FileInfo, TorrentInfo, TorrentStatus};
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
            listen_port_range: Some(port..port + 10),
            enable_upnp_port_forwarding: true,
            ..Default::default()
        };

        let session = Session::new_with_opts(download_dir, opts).await?;
        Ok(Self { session })
    }

    pub async fn add_torrent(&self, source: &str) -> Result<(usize, ManagedTorrentHandle, bool)> {
        let source = source.trim();
        let add_torrent = if source.starts_with("magnet:") {
            AddTorrent::from_url(source)
        } else {
            // Treat as .torrent file path
            let bytes = std::fs::read(source)?;
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

    pub fn get_all_torrents(&self) -> Vec<TorrentInfo> {
        self.session.with_torrents(|iter| {
            iter.map(|(id, handle)| {
                let stats = handle.stats();
                let name = handle
                    .name()
                    .unwrap_or_else(|| "Fetching metadata...".to_string());

                let status = match stats.state {
                    TorrentStatsState::Initializing => TorrentStatus::FetchingMetadata,
                    TorrentStatsState::Live => {
                        if stats.finished {
                            TorrentStatus::Complete
                        } else {
                            TorrentStatus::Downloading
                        }
                    }
                    TorrentStatsState::Paused => TorrentStatus::Paused,
                    TorrentStatsState::Error => {
                        TorrentStatus::Error(stats.error.clone().unwrap_or_default())
                    }
                };

                let (download_speed, upload_speed, eta_seconds, peers_connected) =
                    if let Some(ref live) = stats.live {
                        let dl_bps = (live.download_speed.mbps * 125_000.0) as u64;
                        let ul_bps = (live.upload_speed.mbps * 125_000.0) as u64;
                        let remaining = stats.total_bytes.saturating_sub(stats.progress_bytes);
                        let eta = if dl_bps > 0 {
                            Some(remaining / dl_bps)
                        } else {
                            None
                        };
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
                                    name: fi.relative_filename.to_string_lossy().to_string(),
                                    size_bytes: fi.len,
                                    progress_bytes: progress,
                                }
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();

                TorrentInfo {
                    id,
                    name,
                    size_bytes: stats.total_bytes,
                    downloaded_bytes: stats.progress_bytes,
                    download_speed,
                    upload_speed,
                    peers_connected,
                    peers_total,
                    status,
                    eta_seconds,
                    magnet_link: String::new(),
                    files,
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

pub async fn run_engine(
    config: Config,
    cmd_rx: mpsc::Receiver<EngineCommand>,
    state_tx: mpsc::Sender<Vec<TorrentInfo>>,
    msg_tx: mpsc::Sender<String>,
) -> Result<()> {
    let engine = TorrentEngine::new(&config).await?;
    let mut cmd_rx = cmd_rx;

    // Track finished torrents for completion notification
    let mut finished_set: HashSet<usize> = HashSet::new();

    // Speed limiting state
    let mut download_limit_bps: u64 = config.network.max_download_speed_kbps * 1024;
    let mut upload_limit_bps: u64 = config.network.max_upload_speed_kbps * 1024;
    // Currently paused by throttle (actual engine state)
    let mut throttle_paused: HashSet<usize> = HashSet::new();
    // All torrents under throttle management (for stable UI display)
    let mut throttle_managed: HashSet<usize> = HashSet::new();
    let mut user_paused: HashSet<usize> = HashSet::new();
    // Per-torrent token buckets for fair bandwidth distribution
    let mut per_torrent_tokens: HashMap<usize, i64> = HashMap::new();
    let mut per_torrent_prev_bytes: HashMap<usize, u64> = HashMap::new();
    // Global upload tracking
    let mut ul_tokens: i64 = 0;
    let mut prev_ul_estimated: f64 = 0.0;
    let mut last_throttle_tick = std::time::Instant::now();
    // Cached display values for stable UI during throttle duty cycle
    let mut cached_peers: HashMap<usize, (u32, u32)> = HashMap::new();
    let mut cached_upload_speed: HashMap<usize, u64> = HashMap::new();
    // Rolling speed: (window_start, start_bytes, last_computed_speed)
    let mut speed_tracker: HashMap<usize, (std::time::Instant, u64, u64)> = HashMap::new();
    // Per-torrent last state change time to prevent rapid oscillation
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
            if matches!(t.status, TorrentStatus::Complete) && !finished_set.contains(&t.id) {
                finished_set.insert(t.id);
                let _ = msg_tx
                    .send(format!("\u{2713} \"{}\" complete", t.name))
                    .await;
                eprint!("\x07");
            }
            // Show stable "Throttled" for all managed torrents (even during active bursts)
            if throttle_managed.contains(&t.id) && !matches!(t.status, TorrentStatus::Complete) {
                t.throttle_paused = true;
                // Compute actual effective speed from real byte progress over 5s window
                let tracker = speed_tracker
                    .entry(t.id)
                    .or_insert((now, t.downloaded_bytes, 0));
                let elapsed = now.duration_since(tracker.0).as_secs_f64();
                if elapsed >= 5.0 {
                    let bytes_delta = t.downloaded_bytes.saturating_sub(tracker.1);
                    tracker.2 = (bytes_delta as f64 / elapsed) as u64;
                    tracker.0 = now;
                    tracker.1 = t.downloaded_bytes;
                }
                t.download_speed = tracker.2;
                // Cap at per-torrent throttle limit (bursts can inflate the rolling average)
                if download_limit_bps > 0 {
                    t.download_speed = t.download_speed.min(download_limit_bps / managed_count);
                }
                if t.download_speed > 0 {
                    let remaining = t.size_bytes.saturating_sub(t.downloaded_bytes);
                    t.eta_seconds = Some(remaining / t.download_speed);
                }
                // Cache peer/upload values when non-zero, use cached when paused reports 0
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
                                let name = handle.name().unwrap_or_else(|| "Unknown".to_string());
                                if already_managed || stats.finished {
                                    let _ = msg_tx.send(format!("\"{}\" already downloaded", name)).await;
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
                        // If throttle is active, keep under management so it stays throttled
                        let throttling = download_limit_bps > 0 || upload_limit_bps > 0;
                        if throttling {
                            throttle_managed.insert(id);
                        } else {
                            throttle_managed.remove(&id);
                        }
                        // Reset this torrent's per-torrent budget (no burst)
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
                        let ids_and_handles: Vec<_> = engine.session().with_torrents(|iter| {
                            iter.map(|(id, h)| (id, h.clone())).collect()
                        });
                        for (id, handle) in ids_and_handles {
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
                        let ids_and_handles: Vec<_> = engine.session().with_torrents(|iter| {
                            iter.map(|(id, h)| (id, h.clone())).collect()
                        });
                        let throttling = download_limit_bps > 0 || upload_limit_bps > 0;
                        for (id, handle) in ids_and_handles {
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
                        download_limit_bps = download_kbps * 1024;
                        upload_limit_bps = upload_kbps * 1024;
                        // Reset all per-torrent buckets so new limits take effect immediately
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
                            "Speed limits updated: {} {} KB/s / {} {} KB/s",
                            "\u{2193}", download_kbps, "\u{2191}", upload_kbps
                        )).await;
                        // If limits removed, unpause any throttle-paused torrents
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
                push_state(&engine, &state_tx, &msg_tx, &mut finished_set, &throttle_managed, download_limit_bps, &mut cached_peers, &mut cached_upload_speed, &mut speed_tracker).await;
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                // Token-bucket speed limiting (per-torrent for fair bandwidth sharing)
                let throttling = download_limit_bps > 0 || upload_limit_bps > 0;
                if throttling {
                    let now = std::time::Instant::now();
                    let elapsed_secs = now.duration_since(last_throttle_tick).as_secs_f64();
                    last_throttle_tick = now;

                    let torrents = engine.get_all_torrents();

                    // Auto-enroll any downloading torrent into throttle management
                    for t in &torrents {
                        if matches!(t.status, TorrentStatus::Downloading)
                            && !user_paused.contains(&t.id)
                            && !throttle_managed.contains(&t.id)
                        {
                            throttle_managed.insert(t.id);
                            per_torrent_prev_bytes.entry(t.id).or_insert(t.downloaded_bytes);
                        }
                    }

                    // Per-torrent download throttling
                    if download_limit_bps > 0 {
                        let active_count = torrents.iter()
                            .filter(|t| throttle_managed.contains(&t.id)
                                && !user_paused.contains(&t.id)
                                && !matches!(t.status, TorrentStatus::Complete))
                            .count()
                            .max(1) as u64;
                        let per_torrent_limit = download_limit_bps / active_count;
                        // Hysteresis: require 20% of budget before unpausing
                        let unpause_threshold = (per_torrent_limit as f64 * 0.2) as i64;

                        for t in &torrents {
                            if !throttle_managed.contains(&t.id)
                                || user_paused.contains(&t.id)
                                || matches!(t.status, TorrentStatus::Complete)
                            {
                                continue;
                            }

                            let tokens = per_torrent_tokens.entry(t.id).or_insert(0);
                            let prev = per_torrent_prev_bytes.entry(t.id).or_insert(t.downloaded_bytes);
                            let delta = t.downloaded_bytes.saturating_sub(*prev) as i64;
                            *prev = t.downloaded_bytes;

                            *tokens += (per_torrent_limit as f64 * elapsed_secs) as i64;
                            *tokens -= delta;
                            // Allow up to 2 seconds of burst accumulation
                            *tokens = (*tokens).min(per_torrent_limit as i64 * 2);

                            // Minimum 1 second between state changes per torrent
                            let can_change = per_torrent_last_change
                                .get(&t.id)
                                .is_none_or(|lc| now.duration_since(*lc).as_millis() >= 1000);

                            if *tokens < 0 {
                                // This torrent exceeded its share, pause it
                                if can_change
                                    && !throttle_paused.contains(&t.id)
                                    && matches!(t.status, TorrentStatus::Downloading)
                                {
                                    if let Some(handle) = engine.get_handle(t.id) {
                                        let _ = engine.pause(&handle).await;
                                        throttle_paused.insert(t.id);
                                        per_torrent_last_change.insert(t.id, now);
                                    }
                                }
                            } else if *tokens > unpause_threshold {
                                // This torrent has recovered enough budget, unpause it
                                if can_change
                                    && (throttle_paused.contains(&t.id)
                                        || matches!(t.status, TorrentStatus::Paused))
                                    && !user_paused.contains(&t.id)
                                {
                                    if let Some(handle) = engine.get_handle(t.id) {
                                        let _ = engine.unpause(&handle).await;
                                        throttle_paused.remove(&t.id);
                                        per_torrent_last_change.insert(t.id, now);
                                    }
                                }
                            }
                        }
                    }

                    // Global upload throttling
                    if upload_limit_bps > 0 {
                        let current_ul_speed: u64 = torrents.iter().map(|t| t.upload_speed).sum();
                        let ul_delta = (current_ul_speed as f64 * elapsed_secs) + prev_ul_estimated;
                        let ul_delta_whole = ul_delta as i64;
                        prev_ul_estimated = ul_delta - ul_delta_whole as f64;

                        ul_tokens += (upload_limit_bps as f64 * elapsed_secs) as i64;
                        ul_tokens -= ul_delta_whole;
                        ul_tokens = ul_tokens.min(upload_limit_bps as i64);

                        if ul_tokens < 0 {
                            for t in &torrents {
                                if matches!(t.status, TorrentStatus::Downloading)
                                    && !user_paused.contains(&t.id)
                                    && !throttle_paused.contains(&t.id)
                                {
                                    if let Some(handle) = engine.get_handle(t.id) {
                                        let _ = engine.pause(&handle).await;
                                        throttle_paused.insert(t.id);
                                        throttle_managed.insert(t.id);
                                    }
                                }
                            }
                        }
                    }

                    // Remove completed torrents from throttle tracking
                    for t in &torrents {
                        if matches!(t.status, TorrentStatus::Complete) {
                            throttle_managed.remove(&t.id);
                            throttle_paused.remove(&t.id);
                            per_torrent_tokens.remove(&t.id);
                            per_torrent_prev_bytes.remove(&t.id);
                            per_torrent_last_change.remove(&t.id);
                            cached_upload_speed.remove(&t.id);
                            speed_tracker.remove(&t.id);
                        }
                    }

                    // Clean up stale IDs
                    let current_ids: HashSet<usize> = torrents.iter().map(|t| t.id).collect();
                    throttle_paused.retain(|id| current_ids.contains(id));
                    throttle_managed.retain(|id| current_ids.contains(id));
                    user_paused.retain(|id| current_ids.contains(id));
                    per_torrent_tokens.retain(|id, _| current_ids.contains(id));
                    per_torrent_prev_bytes.retain(|id, _| current_ids.contains(id));
                    per_torrent_last_change.retain(|id, _| current_ids.contains(id));
                }

                push_state(&engine, &state_tx, &msg_tx, &mut finished_set, &throttle_managed, download_limit_bps, &mut cached_peers, &mut cached_upload_speed, &mut speed_tracker).await;
            }
        }
    }

    Ok(())
}
