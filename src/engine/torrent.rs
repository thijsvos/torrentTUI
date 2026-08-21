//! The engine task: the only code in the crate that touches librqbit's
//! `Session`.
//!
//! [`TorrentEngine`] is a thin, stateless wrapper over the session — every
//! method is a lookup or a passthrough. All the *mutable* engine state lives in
//! `run_engine`'s locals, deliberately: which torrents the user paused, which
//! the throttle paused, the per-torrent token buckets, and the caches that stop
//! the display flickering during a duty cycle. Keeping it on one task's stack
//! is what makes it safe to mutate without locks.
//!
//! Throttling is a token bucket per torrent plus one global upload bucket,
//! stepped every 100 ms. librqbit exposes no rate limiter, so a torrent that
//! overdraws its bucket is genuinely paused and unpaused again once it has
//! reaccumulated `UNPAUSE_HYSTERESIS` of its budget. That is why `Paused` can
//! appear in the table without the user having asked for it, and why the
//! hysteresis, the cooldown and the cached peer/speed values exist.

use crate::config::Config;
use crate::engine::watch;
use crate::types::{FileInfo, PeerInfo, TorrentInfo, TorrentStatus};
use crate::ui::util::sanitize_display;
use anyhow::{Context, Result};
use librqbit::{
    api::TorrentIdOrHash,
    dht::Id20,
    http_api::{HttpApi, HttpApiOptions},
    AddTorrent, AddTorrentOptions, AddTorrentResponse, Api, ManagedTorrent, Session,
    SessionOptions, SessionPersistenceConfig, TorrentStatsState,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::mpsc;

/// A reference to a torrent inside the session. Cloning is a refcount bump, so
/// snapshotting handles for a tick is cheap — but holding one does not keep the
/// torrent *registered*: after a delete the session no longer knows about it
/// while your handle stays alive. Check `is_present` when liveness matters.
pub type ManagedTorrentHandle = Arc<ManagedTorrent>;

// ---------------------------------------------------------------------------
// Tunables
// ---------------------------------------------------------------------------

/// Converts librqbit's `Speed.mbps` to bytes/sec. Despite the field name it is
/// mebibytes per second, not megabits: `speed_estimator.rs` computes it as
/// `bytes_per_second / 1024 / 1024` and librqbit renders the same value as
/// "MiB/s". Treating it as megabits understated every speed by 8.39x and
/// inflated every ETA by the same factor (#46).
const MIB_TO_BYTES: f64 = 1_048_576.0;

/// Username half of the embedded HTTP API's per-session basic auth. The
/// password is random per run; see `start_http_api`.
const HTTP_API_USER: &str = "torrenttui";

/// How many sequential ports the engine binds. The Dockerfile EXPOSE range is
/// derived from this, so changes need to be mirrored there.
const PORT_RANGE_SIZE: u16 = 10;

/// Maximum size of a `.torrent` file accepted on disk. Anything larger is
/// rejected before a full read, both as a sanity check and to prevent a
/// symlink-to-huge-file OOM.
pub(crate) const MAX_TORRENT_FILE_SIZE: u64 = 10 * 1024 * 1024;

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

/// One-shot facts the engine pushes to the UI outside the per-tick state
/// snapshot — currently just the HTTP API base URL, but the channel exists so
/// future engine→UI metadata (listening port, DHT status, etc.) has a place
/// to land without bloating the per-tick `Vec<TorrentInfo>`.
#[derive(Debug, Clone)]
pub enum EngineInfo {
    /// The embedded HTTP API is listening on this base URL (e.g.
    /// `http://127.0.0.1:34567`). Sent once at engine startup when the API
    /// successfully binds. If the bind fails, no message is sent and the UI
    /// keeps `http_api_base = None`, so `s` reports "Streaming API not ready
    /// yet" instead of opening a player.
    HttpApiReady { base_url: String },
}

#[derive(Debug)]
/// Everything the UI can ask the engine to do. The only channel for mutating
/// torrent state — nothing outside `engine::torrent` holds a session handle.
///
/// The channel carrying these is 32 deep and the engine's own state channel is
/// 4 deep, so a UI that fans out one command per marked torrent can fill the
/// queue and deadlock against an engine blocked pushing state. That is what the
/// `*Many` variants are for; prefer them over loops.
pub enum EngineCommand {
    AddTorrent(String),
    Pause(usize),
    Resume(usize),
    /// Bulk pause. Replaces a fan-out of N `Pause(id)` sends from the UI,
    /// which could deadlock on the 32-slot command channel for >32 marks.
    PauseMany(Vec<usize>),
    /// Bulk resume — see `PauseMany`.
    ResumeMany(Vec<usize>),
    Delete {
        id: usize,
        delete_files: bool,
    },
    /// Bulk delete — see `PauseMany`.
    DeleteMany {
        ids: Vec<usize>,
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
    /// Which torrent the UI is showing in Detail mode, or `None` when leaving
    /// Detail. The engine populates the heavy
    /// `files` / `peers` fields only for that torrent, dropping per-tick
    /// allocations from O(N × peers × files) to O(peers + files).
    SetDetailTorrent(Option<usize>),
    Shutdown,
}

/// Where to look for the `.torrent`/`.magnet` files the watch folder fed to the
/// engine, and which subtree to leave alone while looking. Present only when
/// the watcher actually started on a directory it is safe to delete from.
#[derive(Clone)]
struct WatchCleanup {
    root: PathBuf,
    /// The download directory, when it sits inside `root`.
    exclude: Option<PathBuf>,
}

/// Lightweight per-torrent snapshot used by the throttle loop. Avoids the cost
/// of building full peer/file lists on every 100 ms tick.
struct ThrottleSnapshot {
    id: usize,
    status: TorrentStatus,
    downloaded_bytes: u64,
    upload_speed: u64,
}

/// Thin wrapper over librqbit's `Session` — lookups, passthroughs and snapshot
/// building, with no state of its own. Everything mutable (user-paused set,
/// throttle buckets, display caches) lives in `run_engine`'s locals, so a
/// second `TorrentEngine` would be a session view with no throttling attached
/// rather than a second engine.
pub struct TorrentEngine {
    session: Arc<Session>,
}

impl TorrentEngine {
    pub async fn new(config: &Config) -> Result<Self> {
        let download_dir = PathBuf::from(&config.general.download_dir);
        std::fs::create_dir_all(&download_dir)?;

        let port = config.network.listen_port;
        // Saturate against u16::MAX so a high `listen_port` never panics
        // (debug) or wraps to an empty range (release).
        let port_end = port.saturating_add(PORT_RANGE_SIZE);
        let opts = SessionOptions {
            disable_dht: !config.network.enable_dht,
            fastresume: true,
            persistence: Some(SessionPersistenceConfig::Json { folder: None }),
            listen_port_range: Some(port..port_end),
            enable_upnp_port_forwarding: config.network.enable_upnp,
            ..Default::default()
        };

        let session = Session::new_with_opts(download_dir, opts).await?;
        Ok(Self { session })
    }

    /// Add a magnet URI or a `.torrent` file path. The trailing `bool` is
    /// "already managed": the session recognised this info hash and returned
    /// the existing torrent instead of adding one, which is what lets the
    /// caller say "already downloaded" rather than reporting a fresh add.
    ///
    /// Anything not starting with `magnet:?` is treated as a filesystem path
    /// and size-capped before reading, so a symlink pointing at something
    /// enormous cannot be read into memory.
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

        // `overwrite` lets librqbit open files that already exist. Without it
        // adding a torrent whose data is already on disk fails outright with
        // "error creating a new file (because allow_overwrite = false)" (#41),
        // which breaks the two flows people actually hit: re-adding a finished
        // torrent so it seeds, and recovering after the session state is lost
        // while the downloads survive. librqbit hash-checks before writing, so
        // intact data is recognised as complete and partial data resumes
        // rather than being clobbered. Its own watch folder already sets this,
        // so this is also what makes `a` and the CLI argument behave the same
        // way as dropping a file into `watch_dir`.
        let response = self
            .session
            .add_torrent(
                add_torrent,
                Some(AddTorrentOptions {
                    overwrite: true,
                    ..Default::default()
                }),
            )
            .await?;

        match response {
            AddTorrentResponse::Added(id, handle) => Ok((id, handle, false)),
            AddTorrentResponse::AlreadyManaged(id, handle) => Ok((id, handle, true)),
            AddTorrentResponse::ListOnly(_) => {
                anyhow::bail!("Torrent was list-only")
            }
        }
    }

    /// Pause a torrent in the session and nothing else. The caller owns the
    /// bookkeeping: `run_engine` distinguishes a user pause (`user_paused`,
    /// which the throttle loop must never undo) from a throttle pause
    /// (`throttle_paused`, which it unpauses once tokens recover). Calling this
    /// without updating the right set means the throttle loop fights the user.
    pub async fn pause(&self, handle: &ManagedTorrentHandle) -> Result<()> {
        self.session.pause(handle).await?;
        Ok(())
    }

    pub async fn unpause(&self, handle: &ManagedTorrentHandle) -> Result<()> {
        self.session.unpause(handle).await?;
        Ok(())
    }

    /// Delete a torrent, addressed by info hash rather than id. librqbit's
    /// persistence hands out `max(existing ids) + 1`, so removing the highest
    /// id frees it for immediate reuse by the watch-folder adder running
    /// concurrently — addressing by hash takes that whole race off the table.
    pub async fn delete(&self, target: TorrentIdOrHash, delete_files: bool) -> Result<()> {
        // librqbit's `Session::delete` does `metadata.load_full().expect("TODO")`
        // *after* it has already removed the torrent from its db, and our
        // release profile sets `panic = "abort"` — that would take the whole
        // process down. Unreachable in librqbit 8.1.1 (a magnet is only
        // inserted into the db once its metadata resolves), so this is
        // defence in depth against lazy metadata landing upstream.
        if let Some(handle) = self.session.get(target) {
            if handle.with_metadata(|_| ()).is_err() {
                tracing::error!(
                    "refusing to delete {:?}: metadata unresolved, deleting would abort the process",
                    target
                );
                anyhow::bail!("torrent metadata is still resolving; try again in a moment");
            }
        }
        self.session.delete(target, delete_files).await?;
        Ok(())
    }

    /// Info hash of a live torrent. Must be read *before* deleting, since
    /// deletion is what removes the handle from the session.
    pub fn info_hash(&self, id: usize) -> Option<Id20> {
        self.get_handle(id).map(|h| h.info_hash())
    }

    /// Info hashes for a batch of ids, resolved under a single lock so no
    /// concurrent add can slip in between two lookups. Ids that are no longer
    /// live are simply absent from the result.
    pub fn info_hashes(&self, ids: &[usize]) -> HashMap<usize, Id20> {
        let wanted: HashSet<usize> = ids.iter().copied().collect();
        self.session.with_torrents(|iter| {
            iter.filter(|(id, _)| wanted.contains(id))
                .map(|(id, handle)| (id, handle.info_hash()))
                .collect()
        })
    }

    /// Whether a torrent with this hash is still in the session. Used to
    /// decide whether watch-folder cleanup should run: `Session::delete` can
    /// return `Err` *after* removing the torrent from both the db and the
    /// persistence store ("torrent deleted, but could not delete files"), and
    /// skipping cleanup in that case would reproduce issue #39 exactly.
    pub fn is_present(&self, hash: Id20) -> bool {
        self.session.get(TorrentIdOrHash::Hash(hash)).is_some()
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
                    .map(|l| (l.upload_speed.mbps * MIB_TO_BYTES) as u64)
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

    /// Build per-torrent snapshots for the UI. When `detail_id` is `Some`,
    /// only that torrent gets its `files` and `peers` populated (and the
    /// `trackers` / `info_hash` / `piece_length` info fields). All other
    /// torrents get empty placeholders. The full path was hot: ~5000
    /// FileInfo + PeerInfo allocations per tick at 50 torrents × 100 files
    /// × 50 peers, of which the UI only ever displayed one torrent's worth.
    pub fn get_all_torrents(&self, detail_id: Option<usize>) -> Vec<TorrentInfo> {
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
                        let dl_bps = (live.download_speed.mbps * MIB_TO_BYTES) as u64;
                        let ul_bps = (live.upload_speed.mbps * MIB_TO_BYTES) as u64;
                        let remaining = stats.total_bytes.saturating_sub(stats.progress_bytes);
                        let eta = compute_eta(remaining, dl_bps);
                        let peers = live.snapshot.peer_stats.live as u32;
                        (dl_bps, ul_bps, eta, peers)
                    } else {
                        (0, 0, None, 0)
                    };

                // `seen` is incremented once per distinct address when the
                // peer is first added, so every live peer is already counted
                // in it. Adding `live` on top double-counted the connected
                // ones (20 of 50 rendered as 20/70).
                let peers_total = if let Some(ref live) = stats.live {
                    live.snapshot.peer_stats.seen as u32
                } else {
                    0
                };

                let is_detail = detail_id == Some(id);

                let files = if is_detail {
                    handle
                        .with_metadata(|meta| {
                            meta.file_infos
                                .iter()
                                .enumerate()
                                .map(|(i, fi)| {
                                    let progress = stats.file_progress.get(i).copied().unwrap_or(0);
                                    FileInfo {
                                        name: sanitize_display(
                                            &fi.relative_filename.to_string_lossy(),
                                        ),
                                        size_bytes: fi.len,
                                        progress_bytes: progress,
                                    }
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };

                let info_hash = if is_detail {
                    handle.info_hash().as_string()
                } else {
                    String::new()
                };
                let trackers: Vec<String> = if is_detail {
                    handle
                        .shared()
                        .trackers
                        .iter()
                        .map(|u| u.to_string())
                        .collect()
                } else {
                    Vec::new()
                };
                let piece_length = if is_detail {
                    handle.with_metadata(|m| m.info.piece_length).ok()
                } else {
                    None
                };

                // Don't sort here: only the selected torrent's peers are ever
                // displayed, and the detail-view renderer sorts lazily.
                let peers: Vec<PeerInfo> = if is_detail {
                    handle
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
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };

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
                    throttle_managed: false, // set by push_state
                }
            })
            .collect()
        })
    }

    /// Look up a live torrent by id. Linear scan under the session lock, which
    /// is why the throttle loop uses `handle_snapshot()` once per tick instead
    /// of calling this per torrent. `None` means the torrent is no longer in
    /// the session — a double-tapped delete, or a stale mark — and callers
    /// generally stay quiet about it rather than surfacing an error.
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

    /// Escape hatch to the raw session, for the few operations this wrapper
    /// deliberately does not mirror: `update_only_files`, `watch_folder`, and
    /// handing the session to librqbit's `Api` for the read-only HTTP server.
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
                    .map(|l| (l.upload_speed.mbps * MIB_TO_BYTES) as u64)
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

/// Build the HTTP stream URL the UI hands to external media players. Pure so
/// the path composition is unit-testable without spinning up an HTTP server.
pub(crate) fn stream_url(base: &str, torrent_id: usize, file_idx: usize) -> String {
    // librqbit's route is `/torrents/{id_or_infohash}/stream/{file_idx}`; we
    // pass the numeric ID since it's stable for the engine's lifetime.
    format!(
        "{}/torrents/{}/stream/{}",
        base.trim_end_matches('/'),
        torrent_id,
        file_idx
    )
}

/// Bind and spawn librqbit's HTTP API server. Returns the base URL to publish
/// to the UI, the bound address so the caller can warn on a non-loopback bind,
/// and the server task's handle so it can be stopped on shutdown. `Err` means
/// the bind failed (port in use, address unparseable); the spawned server's own
/// errors are logged, not returned. We never panic here.
async fn start_http_api(
    engine: &TorrentEngine,
    bind: &str,
) -> Result<(String, std::net::SocketAddr, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind {}", bind))?;
    let bound = listener.local_addr()?;

    let api = Api::new(engine.session().clone(), None, None);

    // Random per-session credentials. librqbit applies basic auth as a
    // `route_layer` over the whole router, so unlike `read_only` it also covers
    // the two POST routes that survive it: `POST /torrents/resolve_magnet` and
    // `POST /rust_log`. The latter takes a text/plain body, which makes it a
    // CORS "simple request" that any web page could send cross-origin to raise
    // this process's log filter — writing peer IPs, tracker URLs and info
    // hashes to disk, exactly what the default filter exists to prevent (#48).
    // A browser cannot attach an Authorization header without a preflight, and
    // librqbit's CORS layer rejects preflights from unknown origins.
    //
    // `generate_peer_id` fills the trailing 12 bytes from the OS RNG; the last
    // 24 hex characters are those bytes, i.e. 96 bits.
    let generated = librqbit::generate_peer_id(b"-tt0000-").as_string();
    let token = generated[generated.len() - 24..].to_string();
    // Mount the API read-only. librqbit gates the torrent-control routes
    // (add / pause / start / forget / delete / limits / update_only_files)
    // behind `!read_only`, and the TUI drives all of those through the Session
    // API directly, so read-only loses nothing there. It is not a GET-only
    // mount though: librqbit 8.1.1 registers `POST /torrents/resolve_magnet`
    // and `POST /rust_log` unconditionally. The latter takes a plain-text body,
    // so a browser page can reach it cross-origin without a preflight and raise
    // this process's log filter — which is what keeps peer IPs, tracker URLs
    // and info hashes out of the on-disk log by default.
    let http_api = HttpApi::new(
        api,
        Some(HttpApiOptions {
            read_only: true,
            basic_auth: Some((HTTP_API_USER.to_string(), token.clone())),
        }),
    );

    let task = tokio::spawn(async move {
        if let Err(e) = http_api.make_http_api_and_run(listener, None).await {
            tracing::error!("HTTP API server exited with error: {}", e);
        }
    });

    // SocketAddr's Display already wraps IPv6 hosts in brackets, so the
    // resulting URL is correct for both 127.0.0.1:N and [::1]:N. Return the
    // bound address (so the caller can warn on a non-loopback bind) and the
    // task handle (so it can be stopped on shutdown).
    Ok((
        format!("http://{}:{}@{}", HTTP_API_USER, token, bound),
        bound,
        task,
    ))
}

/// Spawn a background task that deletes the watch-folder files which fed the
/// torrents identified by `hashes`, and reap any previously finished ones off
/// `tasks`. Returns immediately — the deletion has not happened yet. A silent
/// no-op when `cleanup` is `None` (no watcher, or a watch root cleanup
/// refused) or when `hashes` is empty — in both cases nothing was matched.
///
/// Deliberately runs *after* `Session::delete`: removing the file first would
/// destroy a `.torrent` for a torrent that might still be live, and it would
/// close no race — librqbit's watcher ignores every event that isn't
/// Create/Modify, and an add already in flight is holding the file's bytes in
/// memory anyway. The trade-off is that this is best-effort: a crash between
/// the delete and the walk leaves the file behind.
fn spawn_watch_cleanup(
    tasks: &mut tokio::task::JoinSet<()>,
    cleanup: &Option<WatchCleanup>,
    hashes: HashSet<String>,
    msg_tx: &mpsc::Sender<String>,
) {
    // Reap already-finished cleanups; the set is only drained in full at
    // shutdown, so without this a long session accumulates dead entries.
    while tasks.try_join_next().is_some() {}

    let Some(cleanup) = cleanup.clone() else {
        return;
    };
    if hashes.is_empty() {
        return;
    }
    let msg_tx = msg_tx.clone();
    tasks.spawn(async move {
        let outcome = match tokio::task::spawn_blocking(move || {
            watch::remove_sources(&cleanup.root, cleanup.exclude.as_deref(), &hashes)
        })
        .await
        {
            Ok(outcome) => outcome,
            Err(e) => {
                tracing::error!("watch folder cleanup task failed: {}", e);
                return;
            }
        };

        // At most two messages, however many files were involved. The message
        // channel holds 16 and a full one blocks the engine loop; worse, the
        // UI concatenates unread messages into a single unwrapped line, so a
        // burst would push everything past the right edge of the terminal.
        if !outcome.removed.is_empty() {
            let detail = match outcome.removed.as_slice() {
                [one] => one
                    .file_name()
                    .map(|n| sanitize_display(&n.to_string_lossy()))
                    .unwrap_or_else(|| "1 file".to_string()),
                many => format!("{} files", many.len()),
            };
            let _ = msg_tx
                .send(format!("Removed {} from watch folder", detail))
                .await;
        }
        if !outcome.errors.is_empty() {
            for err in &outcome.errors {
                tracing::warn!("watch folder cleanup: {}", err);
            }
            let _ = msg_tx
                .send(format!(
                    "\u{26a0} {} watch file(s) could not be removed",
                    outcome.errors.len()
                ))
                .await;
        }
    });
}

/// The engine task. Owns the librqbit session, all throttle bookkeeping, and
/// the watch folder, and runs until it receives `Shutdown` or the command
/// channel closes — both are normal exits that return `Ok`.
///
/// Two arms drive everything: an incoming command, or the throttle tick. Both
/// end by calling `push_state`, so the UI gets a fresh snapshot after every
/// action without each command handler having to remember to send one.
///
/// Startup failures are degraded, never fatal: a failed HTTP API bind just
/// means the UI never receives `HttpApiReady` and streaming stays disabled, and
/// an unusable `watch_dir` disables the watcher with a status message. Only
/// session construction can abort the task, and main.rs treats that as "engine
/// died, quit".
///
/// Shutdown drains in-flight watch-folder cleanups before returning, because
/// dropping the `JoinSet` would abort them and leave a `.torrent` on disk to
/// resurrect a torrent the user just deleted.
pub async fn run_engine(
    config: Config,
    cmd_rx: mpsc::Receiver<EngineCommand>,
    state_tx: mpsc::Sender<Vec<TorrentInfo>>,
    msg_tx: mpsc::Sender<String>,
    info_tx: mpsc::Sender<EngineInfo>,
) -> Result<()> {
    let engine = TorrentEngine::new(&config).await?;

    // Bring up the embedded HTTP API for media streaming. A bind failure
    // (port in use, address invalid) is degraded gracefully: the UI just
    // won't get an HttpApiReady and the `s` keybind shows an error if
    // pressed. The task handle is kept so we can stop it on shutdown.
    let mut http_task: Option<tokio::task::JoinHandle<()>> = None;
    match start_http_api(&engine, &config.network.http_api_bind).await {
        Ok((base_url, bound, task)) => {
            http_task = Some(task);
            // `base_url` carries the session credentials — log the bare address.
            tracing::info!("HTTP streaming API listening on http://{}", bound);
            // The API is read-only (no add/pause/delete), but it is still
            // unauthenticated, so a non-loopback bind lets any host on that
            // network list your torrents and stream/read your downloaded files.
            // Warn loudly in both the log and the UI status bar.
            if !bound.ip().is_loopback() {
                let warn = format!(
                    "\u{26a0} Streaming API bound to {} (non-loopback). Access needs the per-session password, but it travels in plaintext HTTP — use only on a trusted LAN",
                    bound
                );
                tracing::warn!("{}", warn);
                if let Err(e) = msg_tx.send(warn).await {
                    tracing::warn!("non-loopback warning send dropped (UI gone?): {}", e);
                }
            }
            if let Err(e) = info_tx
                .send(EngineInfo::HttpApiReady {
                    base_url: base_url.clone(),
                })
                .await
            {
                tracing::warn!("HttpApiReady send dropped (UI gone?): {}", e);
            }
        }
        Err(e) => {
            tracing::warn!("HTTP streaming API failed to start: {}", e);
            if let Err(se) = msg_tx
                .send(format!("Streaming disabled (API bind failed: {})", e))
                .await
            {
                tracing::warn!("streaming-disabled message send dropped (UI gone?): {}", se);
            }
        }
    }

    // Watch folder for auto-adding torrents. Off by default; only enabled if
    // the user opts in via config. Failures here must not abort the engine
    // task — the UI would silently stop receiving state updates and a single
    // bad config knob would brick the whole app.
    //
    // When the watcher does start, `watch_cleanup` carries what the delete
    // path needs: the folder to scan, and a subtree to leave alone. `None`
    // means "no cleanup", so a watcher we refused to start never causes files
    // to be deleted.
    let mut watch_cleanup: Option<WatchCleanup> = None;
    if let Some(ref dir) = config.general.watch_dir {
        let path = PathBuf::from(dir);
        let download_dir = PathBuf::from(&config.general.download_dir);
        // Compare canonicalized paths when possible (handles trailing slash,
        // case-insensitive volumes, etc.); fall back to literal path equality.
        let watch_eq_download = match (path.canonicalize(), download_dir.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => path == download_dir,
        };
        if watch_eq_download {
            let _ = msg_tx
                .send("Watch folder disabled: equal to download_dir would loop".to_string())
                .await;
            tracing::warn!("watch_dir equals download_dir; refusing to watch");
        } else {
            match std::fs::create_dir_all(&path) {
                Ok(()) => {
                    engine.session().watch_folder(&path);
                    tracing::info!("Watching folder: {}", dir);
                    // Deleting a torrent removes its source file from here
                    // (issue #39), so the safety bar is higher than for
                    // watching alone: refuse roots where a recursive delete
                    // pass would be unrecoverable.
                    if watch::is_safe_watch_root(&path) {
                        let exclude = watch::cleanup_exclude(&path, &download_dir);
                        watch_cleanup = Some(WatchCleanup {
                            root: path,
                            exclude,
                        });
                    } else {
                        tracing::warn!(
                            "watch_dir {:?} is a home or root directory; \
                             auto-adding still works but .torrent cleanup on delete is disabled",
                            dir
                        );
                    }
                }
                Err(e) => {
                    let _ = msg_tx.send(format!("Watch folder disabled: {}", e)).await;
                    tracing::warn!("watch_dir {:?} unavailable: {}", dir, e);
                }
            }
        }
    }
    let watch_cleanup = watch_cleanup;

    // Watch-folder cleanup runs off the command loop so a slow walk (deep
    // tree, network mount) can't hold up the state push that makes the
    // deleted row disappear. Drained on shutdown so a cleanup in flight
    // still lands.
    let mut cleanup_tasks: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();

    // A fixed cadence, not a fresh `sleep` per loop iteration: the latter
    // restarts on every command, so a burst of them could starve the throttle
    // indefinitely and skew the token buckets' elapsed time.
    let mut throttle_tick = tokio::time::interval(THROTTLE_TICK);
    throttle_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let enable_notifications = config.ui.enable_notifications;
    let mut cmd_rx = cmd_rx;

    let mut finished_set: HashSet<usize> = HashSet::new();

    // Speed-limit state. Use saturating_mul to avoid u64 overflow when the
    // user types an unreasonably large limit.
    // Clamp with the same cap the UI applies, so the enforced limit and the
    // one shown in the status bar cannot disagree. Unclamped, `as i64` below
    // is a *truncating* cast: a large enough value goes negative, pins
    // `ul_tokens` at -1 and pauses every torrent forever.
    let mut download_limit_bps: u64 =
        crate::clamped_speed_limit(config.network.max_download_speed_kbps).saturating_mul(1024);
    let mut upload_limit_bps: u64 =
        crate::clamped_speed_limit(config.network.max_upload_speed_kbps).saturating_mul(1024);
    // Which torrents the *download* bucket paused, and which the *upload*
    // bucket paused. They must stay separate: a shared set let the upload
    // block's symmetric unpause release torrents the download bucket had just
    // paused, defeating the download limit whenever both were set.
    let mut throttle_paused: HashSet<usize> = HashSet::new();
    let mut ul_throttle_paused: HashSet<usize> = HashSet::new();
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
    // Tracks which torrent the UI is showing in Detail mode. When None, the
    // per-tick snapshot skips files/peers/trackers entirely.
    let mut detail_torrent_id: Option<usize> = None;

    /// Build the latest per-torrent snapshot and broadcast it to the UI. Also
    /// fires completion notifications, applies throttle-managed display
    /// overrides (effective speed + paused flag), and prunes the bookkeeping
    /// (finished-set / peers / upload-speed / speed-tracker) of torrents that
    /// no longer exist. Runs at the end of every command except `Shutdown`,
    /// which breaks out of the loop first, and on every timer tick.
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
        detail_id: Option<usize>,
    ) {
        let now = std::time::Instant::now();
        let mut torrents = engine.get_all_torrents(detail_id);

        let current_ids: HashSet<usize> = torrents.iter().map(|t| t.id).collect();
        finished_set.retain(|id| current_ids.contains(id));
        cached_peers.retain(|id, _| current_ids.contains(id));
        cached_upload_speed.retain(|id, _| current_ids.contains(id));
        speed_tracker.retain(|id, _| current_ids.contains(id));

        // Match the throttle loop's `active_count` divisor: count only
        // throttle-managed torrents that are still downloading. Otherwise a
        // completed torrent lingering in `throttle_managed` between a command
        // and the next cleanup tick inflates the divisor, so the displayed
        // per-torrent cap undershoots what's actually enforced.
        let managed_count = torrents
            .iter()
            .filter(|t| {
                throttle_managed.contains(&t.id)
                    && !matches!(t.status, TorrentStatus::Complete | TorrentStatus::Seeding)
            })
            .count()
            .max(1) as u64;

        for t in &mut torrents {
            if matches!(t.status, TorrentStatus::Complete | TorrentStatus::Seeding)
                && !finished_set.contains(&t.id)
            {
                finished_set.insert(t.id);
                // Advisory; not worth blocking the engine for.
                let _ = msg_tx.try_send(format!("\u{2713} \"{}\" complete", t.name));

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
                        // Control characters were stripped at the engine
                        // boundary; the markup escaping has to happen here,
                        // because libnotify parses the body as Pango markup on
                        // most Linux desktops and nothing else wants it.
                        let name = crate::ui::util::escape_markup(&t.name);
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
            // Only smooth over the two states the duty cycle flips between.
            // Letting this cover Error would hide the error text behind a
            // permanent "Throttled" label, since nothing removes an errored
            // torrent from `throttle_managed`.
            if throttle_managed.contains(&t.id)
                && matches!(t.status, TorrentStatus::Downloading | TorrentStatus::Paused)
            {
                t.throttle_managed = true;
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

        // try_send, not send: these are snapshots, so a full channel means the
        // UI is behind and the next tick supersedes this one anyway. Awaiting
        // here would stall command handling and the throttle loop behind UI
        // drain latency.
        let _ = state_tx.try_send(torrents);
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
                        ul_throttle_paused.remove(&id);
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
                        ul_throttle_paused.remove(&id);
                        let throttling = download_limit_bps > 0 || upload_limit_bps > 0;
                        if throttling {
                            throttle_managed.insert(id);
                        } else {
                            throttle_managed.remove(&id);
                        }
                        per_torrent_tokens.insert(id, 0);
                        // Reset the effective-speed window too, or the average
                        // spans the pause and under-reports after a resume.
                        speed_tracker.remove(&id);
                        per_torrent_prev_bytes.remove(&id);
                        per_torrent_last_change.remove(&id);
                        cached_upload_speed.remove(&id);
                        ul_tokens = 0;
                        last_throttle_tick = std::time::Instant::now();
                    }
                    Some(EngineCommand::Delete { id, delete_files }) => {
                        // Read the hash before deleting — deletion is what
                        // removes the handle. `None` means the torrent is
                        // already gone (a double-tapped `d`, a stale mark);
                        // stay quiet rather than surfacing librqbit's
                        // "no such torrent in db".
                        if let Some(hash) = engine.info_hash(id) {
                            if let Err(e) = engine
                                .delete(TorrentIdOrHash::Hash(hash), delete_files)
                                .await
                            {
                                tracing::error!("Failed to delete torrent {}: {}", id, e);
                                let _ = msg_tx
                                    .send(format!("\u{26a0} Delete failed: {}", e))
                                    .await;
                            }
                            // Gated on the torrent being gone rather than on
                            // Ok: librqbit reports "torrent deleted, but could
                            // not delete files" *after* dropping it from both
                            // the db and persistence, and skipping cleanup
                            // there would leave the source file to resurrect
                            // it on the next launch.
                            if !engine.is_present(hash) {
                                spawn_watch_cleanup(
                                    &mut cleanup_tasks,
                                    &watch_cleanup,
                                    HashSet::from([hash.as_string()]),
                                    &msg_tx,
                                );
                            }
                        }
                        finished_set.remove(&id);
                        user_paused.remove(&id);
                        throttle_paused.remove(&id);
                        ul_throttle_paused.remove(&id);
                        throttle_managed.remove(&id);
                        per_torrent_tokens.remove(&id);
                        per_torrent_prev_bytes.remove(&id);
                        per_torrent_last_change.remove(&id);
                        cached_upload_speed.remove(&id);
                        speed_tracker.remove(&id);
                    }
                    Some(EngineCommand::PauseMany(ids)) => {
                        // One snapshot, then O(1) lookups. `get_handle` is a
                        // linear scan under the session lock, so calling it per
                        // id made a bulk action O(M*N) with M lock acquisitions.
                        let handles: HashMap<usize, ManagedTorrentHandle> =
                            engine.handle_snapshot().into_iter().collect();
                        for id in ids {
                            if let Some(handle) = handles.get(&id) {
                                if let Err(e) = engine.pause(handle).await {
                                    tracing::error!("Failed to pause torrent {}: {}", id, e);
                                }
                            }
                            user_paused.insert(id);
                            throttle_paused.remove(&id);
                        ul_throttle_paused.remove(&id);
                            throttle_managed.remove(&id);
                            per_torrent_tokens.remove(&id);
                            per_torrent_prev_bytes.remove(&id);
                            per_torrent_last_change.remove(&id);
                            cached_upload_speed.remove(&id);
                            speed_tracker.remove(&id);
                        }
                    }
                    Some(EngineCommand::ResumeMany(ids)) => {
                        // One snapshot, then O(1) lookups. `get_handle` is a
                        // linear scan under the session lock, so calling it per
                        // id made a bulk action O(M*N) with M lock acquisitions.
                        let handles: HashMap<usize, ManagedTorrentHandle> =
                            engine.handle_snapshot().into_iter().collect();
                        let throttling = download_limit_bps > 0 || upload_limit_bps > 0;
                        for id in ids {
                            if let Some(handle) = handles.get(&id) {
                                if let Err(e) = engine.unpause(handle).await {
                                    tracing::error!("Failed to resume torrent {}: {}", id, e);
                                }
                            }
                            user_paused.remove(&id);
                            throttle_paused.remove(&id);
                        ul_throttle_paused.remove(&id);
                            if throttling {
                                throttle_managed.insert(id);
                            } else {
                                throttle_managed.remove(&id);
                            }
                            per_torrent_tokens.insert(id, 0);
                            speed_tracker.remove(&id);
                        // Reset the effective-speed window too, or the average
                        // spans the pause and under-reports after a resume.
                        speed_tracker.remove(&id);
                            per_torrent_prev_bytes.remove(&id);
                            per_torrent_last_change.remove(&id);
                            cached_upload_speed.remove(&id);
                        }
                        ul_tokens = 0;
                        last_throttle_tick = std::time::Instant::now();
                    }
                    Some(EngineCommand::DeleteMany { ids, delete_files }) => {
                        // One snapshot for the whole batch, before any
                        // deleting: librqbit hands out `max(ids) + 1`, so
                        // freeing the highest id makes it immediately
                        // reusable by the watch-folder adder running
                        // concurrently. Resolving hashes lazily inside the
                        // loop could pick up a recycled id's new torrent.
                        let batch: HashMap<usize, Id20> = engine.info_hashes(&ids);
                        let mut deleted_hashes: HashSet<String> = HashSet::new();
                        let mut failures = 0usize;
                        for id in ids {
                            if let Some(&hash) = batch.get(&id) {
                                if let Err(e) = engine
                                    .delete(TorrentIdOrHash::Hash(hash), delete_files)
                                    .await
                                {
                                    tracing::error!("Failed to delete torrent {}: {}", id, e);
                                    failures += 1;
                                }
                                if !engine.is_present(hash) {
                                    deleted_hashes.insert(hash.as_string());
                                }
                            }
                            finished_set.remove(&id);
                            user_paused.remove(&id);
                            throttle_paused.remove(&id);
                        ul_throttle_paused.remove(&id);
                            throttle_managed.remove(&id);
                            per_torrent_tokens.remove(&id);
                            per_torrent_prev_bytes.remove(&id);
                            per_torrent_last_change.remove(&id);
                            cached_upload_speed.remove(&id);
                            speed_tracker.remove(&id);
                        }
                        // One cleanup pass and one failure message for the
                        // whole batch — see `spawn_watch_cleanup` on why the
                        // message channel must not be fed per-item.
                        spawn_watch_cleanup(
                            &mut cleanup_tasks,
                            &watch_cleanup,
                            deleted_hashes,
                            &msg_tx,
                        );
                        if failures > 0 {
                            let _ = msg_tx
                                .send(format!("\u{26a0} {} torrent(s) failed to delete", failures))
                                .await;
                        }
                    }
                    Some(EngineCommand::PauseAll) => {
                        for (id, handle) in engine.handle_snapshot() {
                            let _ = engine.pause(&handle).await;
                            user_paused.insert(id);
                        }
                        throttle_paused.clear();
                        ul_throttle_paused.clear();
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
                        ul_throttle_paused.clear();
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
                            for id in throttle_paused.drain().chain(ul_throttle_paused.drain()) {
                                if let Some(handle) = engine.get_handle(id) {
                                    let _ = engine.unpause(&handle).await;
                                }
                            }
                            throttle_managed.clear();
                        }
                    }
                    Some(EngineCommand::SetDetailTorrent(id)) => {
                        detail_torrent_id = id;
                        // push_state runs at the bottom of this arm and will
                        // populate (or strip) files/peers for the new target.
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
                push_state(&engine, &state_tx, &msg_tx, &mut finished_set, &throttle_managed, download_limit_bps, &mut cached_peers, &mut cached_upload_speed, &mut speed_tracker, enable_notifications, detail_torrent_id).await;
            }
            _ = throttle_tick.tick() => {
                let throttling = download_limit_bps > 0 || upload_limit_bps > 0;
                if throttling {
                    let now = std::time::Instant::now();
                    // Clamp at 1s so a system suspend-resume can't credit
                    // hours of fictitious bytes into the token buckets (the
                    // upload-bucket math at `ul_delta as i64` would otherwise
                    // saturate to i64::MAX and corrupt `prev_ul_estimated`).
                    let elapsed_secs = now
                        .duration_since(last_throttle_tick)
                        .as_secs_f64()
                        .min(1.0);
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
                            // While throttle-paused, librqbit can still settle
                            // in-flight pieces — debiting that against the
                            // active bucket would keep tokens below the unpause
                            // threshold forever. Reset the cursor so the next
                            // active tick measures only real flow.
                            let delta = if throttle_paused.contains(&t.id) {
                                0
                            } else {
                                t.downloaded_bytes.saturating_sub(*prev) as i64
                            };
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
                                // A finished torrent is the only kind that is
                                // purely uploading, so the upload cap has to
                                // cover it or it does nothing for seeding.
                                if matches!(
                                    t.status,
                                    TorrentStatus::Downloading
                                        | TorrentStatus::Complete
                                        | TorrentStatus::Seeding
                                ) && !user_paused.contains(&t.id)
                                    && !ul_throttle_paused.contains(&t.id)
                                    && !throttle_paused.contains(&t.id)
                                {
                                    if let Some(handle) = handle_map.get(&t.id) {
                                        let _ = engine.pause(handle).await;
                                        ul_throttle_paused.insert(t.id);
                                        throttle_managed.insert(t.id);
                                        per_torrent_last_change.insert(t.id, now);
                                    }
                                }
                            }
                        } else if ul_tokens > unpause_threshold {
                            // Symmetric unpause, but only of what *this* bucket
                            // paused, and only when the download bucket is not
                            // still overdrawn. Releasing from the shared set
                            // used to hand back torrents the download limit had
                            // just paused, ten times a second.
                            for t in &snapshot {
                                if ul_throttle_paused.contains(&t.id)
                                    && !user_paused.contains(&t.id)
                                    && per_torrent_tokens.get(&t.id).copied().unwrap_or(0) >= 0
                                {
                                    if let Some(handle) = handle_map.get(&t.id) {
                                        let _ = engine.unpause(handle).await;
                                        ul_throttle_paused.remove(&t.id);
                                        per_torrent_last_change.insert(t.id, now);
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
                    ul_throttle_paused.retain(|id| current_ids.contains(id));
                    throttle_managed.retain(|id| current_ids.contains(id));
                    user_paused.retain(|id| current_ids.contains(id));
                    per_torrent_tokens.retain(|id, _| current_ids.contains(id));
                    per_torrent_prev_bytes.retain(|id, _| current_ids.contains(id));
                    per_torrent_last_change.retain(|id, _| current_ids.contains(id));
                }

                push_state(&engine, &state_tx, &msg_tx, &mut finished_set, &throttle_managed, download_limit_bps, &mut cached_peers, &mut cached_upload_speed, &mut speed_tracker, enable_notifications, detail_torrent_id).await;
            }
        }
    }

    // Release our own pauses before exiting. librqbit persists `is_paused`,
    // so a torrent caught mid duty cycle would come back Paused on the next
    // launch — and the adopt loop only picks up `Downloading`, so nothing
    // would ever resume it.
    for id in throttle_paused.drain().chain(ul_throttle_paused.drain()) {
        if let Some(handle) = engine.get_handle(id) {
            let _ = engine.unpause(&handle).await;
        }
    }

    // Let any watch-folder cleanup in flight finish. Dropping the JoinSet here
    // would abort it, leaving the source `.torrent` on disk to resurrect a
    // torrent the user just deleted. main.rs caps the whole shutdown at 5 s.
    while cleanup_tasks.join_next().await.is_some() {}

    // Stop the HTTP API server task spawned in start_http_api. main.rs awaits
    // this function on shutdown, so aborting here makes teardown deterministic
    // rather than leaving the task to be reaped when the runtime drops.
    if let Some(task) = http_task {
        task.abort();
        tracing::info!("HTTP API task stopped on shutdown");
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
    fn mib_to_bytes_matches_librqbits_units() {
        // librqbit's `Speed.mbps` is `bytes_per_second / 1024 / 1024` and is
        // rendered as "MiB/s" — mebibytes, despite the field name. If someone
        // "corrects" this back to a megabit conversion (125_000), every speed
        // in the table drops by 8.39x and every ETA grows by the same (#46).
        assert_eq!(MIB_TO_BYTES, 1_048_576.0);
        assert_eq!((1.0_f64 * MIB_TO_BYTES) as u64, 1024 * 1024);
        // A half-MiB/s reading is 512 KiB/s, not 62.5 KB/s.
        assert_eq!((0.5_f64 * MIB_TO_BYTES) as u64, 512 * 1024);
    }

    #[test]
    fn eta_uses_the_same_byte_scale_as_the_speed_column() {
        // 10 MiB left at a reported 1.0 mbps (= 1 MiB/s) is 10 seconds. Under
        // the old megabit constant this came out as 84.
        let remaining = 10 * 1024 * 1024;
        let dl_bps = (1.0_f64 * MIB_TO_BYTES) as u64;
        assert_eq!(compute_eta(remaining, dl_bps), Some(10));
    }

    #[test]
    fn engine_command_is_debug() {
        // Smoke test the derive added for tracing/panic dumps.
        let cmd = EngineCommand::Pause(42);
        assert!(format!("{cmd:?}").contains("Pause"));
    }

    #[test]
    fn stream_url_basic() {
        assert_eq!(
            stream_url("http://127.0.0.1:34567", 3, 0),
            "http://127.0.0.1:34567/torrents/3/stream/0"
        );
    }

    #[test]
    fn stream_url_strips_trailing_slash() {
        // Defensive — a caller that hand-builds the base might include "/".
        assert_eq!(
            stream_url("http://127.0.0.1:34567/", 1, 7),
            "http://127.0.0.1:34567/torrents/1/stream/7"
        );
    }

    #[test]
    fn stream_url_ipv6_host_is_unchanged() {
        // SocketAddr::Display brackets IPv6 hosts; the URL must keep them.
        let url = stream_url("http://[::1]:9000", 12, 4);
        assert_eq!(url, "http://[::1]:9000/torrents/12/stream/4");
    }
}
