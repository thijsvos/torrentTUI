//! The engine task: the only code in the crate that touches librqbit's
//! `Session`.
//!
//! [`TorrentEngine`] is a thin, stateless wrapper over the session — every
//! method is a lookup or a passthrough. What little *mutable* engine state
//! remains (the finished-notification set, the Detail-view target) lives in
//! `run_engine`'s locals, deliberately: keeping it on one task's stack is what
//! makes it safe to mutate without locks.
//!
//! Speed limits are librqbit's own token-bucket rate limiter — configured at
//! session creation and swapped live through `session.ratelimits`. The
//! limiter sits directly in the peer IO path, so a cap shapes traffic
//! smoothly; a torrent under a limit keeps transferring instead of being
//! duty-cycle paused the way earlier versions (on librqbit 8, which had no
//! limiter) had to. `Paused` in the table therefore always means the user —
//! or a persisted previous session — paused it.

use crate::config::Config;
use crate::engine::watch;
use crate::types::{FileInfo, PeerInfo, TorrentInfo, TorrentStatus};
use crate::ui::util::sanitize_display;
use anyhow::{Context, Result};
use librqbit::{
    api::TorrentIdOrHash,
    dht::Id20,
    http_api::{HttpApi, HttpApiOptions},
    limits::LimitsConfig,
    AddTorrent, AddTorrentOptions, AddTorrentResponse, Api, ConnectionOptions, DhtSessionConfig,
    ListenerOptions, ManagedTorrent, Session, SessionOptions, SessionPersistenceConfig,
    TorrentStatsState,
};
use librqbit_dualstack_sockets::{BindOpts, TcpListener};
use std::collections::{HashMap, HashSet};
use std::net::Ipv6Addr;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;
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

/// First bindable port in `[start, start + PORT_RANGE_SIZE)`. librqbit 8 took
/// a port *range* and walked it until a bind succeeded; librqbit 9 takes
/// exactly one listen address, so the walk lives here as a best-effort probe.
/// Falls back to `start` when no probe succeeds, so the session bind surfaces
/// the real error. The probe is inherently racy (the port can be taken
/// between probe and bind) and only sees the IPv6 side on platforms where a
/// plain `[::]` bind is not dualstack; either way the worst case is the same
/// bind error the session would have reported anyway.
fn pick_listen_port(start: u16) -> u16 {
    // Saturate against u16::MAX so a high `listen_port` never panics (debug)
    // or wraps to an empty range (release).
    let end = start.saturating_add(PORT_RANGE_SIZE);
    (start..end)
        .find(|&port| std::net::TcpListener::bind((Ipv6Addr::UNSPECIFIED, port)).is_ok())
        .unwrap_or(start)
}

/// Maximum size of a `.torrent` file accepted on disk. Anything larger is
/// rejected before a full read, both as a sanity check and to prevent a
/// symlink-to-huge-file OOM.
pub(crate) const MAX_TORRENT_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// How often the engine rebuilds and pushes a state snapshot to the UI. Also
/// the cadence for completion notifications and bookkeeping pruning.
const STATE_PUSH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

// ---------------------------------------------------------------------------

/// One-shot facts the engine pushes to the UI outside the per-tick state
/// snapshot — the HTTP API base URL and the privacy posture; the channel
/// exists so future engine→UI metadata (listening port, DHT status, etc.)
/// has a place to land without bloating the per-tick `Vec<TorrentInfo>`.
#[derive(Debug, Clone)]
pub enum EngineInfo {
    /// The embedded HTTP API is listening on this base URL (e.g.
    /// `http://127.0.0.1:34567`). Sent once at engine startup when the API
    /// successfully binds. If the bind fails, no message is sent and the UI
    /// keeps `http_api_base = None`, so `s` reports "Streaming API not ready
    /// yet" instead of opening a player.
    HttpApiReady { base_url: String },
    /// Which `[privacy]` features the session actually started with. Sent
    /// once at engine startup, and only when at least one is active — the
    /// header badge and the startup status message both render from it, so
    /// they reflect the running session rather than a parsed config that
    /// might have failed to apply.
    Privacy(PrivacyStatus),
}

/// The active privacy posture, as applied at session creation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivacyStatus {
    /// A SOCKS5 proxy is carrying outgoing peer + HTTP tracker traffic (and
    /// the session is locked down: no DHT, no listener/UPnP, no LSD).
    pub proxy: bool,
    /// All BitTorrent sockets are bound to this interface.
    pub bind_interface: Option<String>,
    /// A blocklist is active, with this many loaded ranges.
    pub blocklist_ranges: Option<usize>,
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

/// Thin wrapper over librqbit's `Session` — lookups, passthroughs and snapshot
/// building, with no state of its own beyond startup facts. Even the speed
/// limits live inside the session (`session.ratelimits`), so a second
/// `TorrentEngine` over the same session would just be another view of the
/// same engine.
pub struct TorrentEngine {
    session: Arc<Session>,
    /// True when a SOCKS5 proxy is configured. The `AddTorrent` path strips
    /// `udp://` trackers from magnets in this mode — a SOCKS5 proxy carries
    /// TCP only, so a udp announce would go around it, straight to the
    /// tracker, carrying the real address.
    proxy_active: bool,
    /// What `run_engine` reports to the UI at startup; `None` when no
    /// privacy feature is configured.
    privacy_status: Option<PrivacyStatus>,
}

/// The network shape derived from config — the privacy-critical decisions,
/// pulled out of `Session::new_with_opts` (which needs a real session) so they
/// can be unit-tested. `build_session_opts` turns this plus the rate limits
/// into `SessionOptions`; `TorrentEngine::new` just calls both. Extracting this
/// is what makes the proxy lockdown testable — a regression that re-enabled DHT
/// or the listener under a proxy would otherwise ship silently.
#[derive(Debug)]
struct NetworkPlan {
    dht: bool,
    /// The listen port, or `None` to run with no incoming listener at all.
    listen_port: Option<u16>,
    upnp: bool,
    disable_lsd: bool,
    proxy_url: Option<String>,
    blocklist_url: Option<String>,
    bind_interface: Option<String>,
}

impl NetworkPlan {
    /// Derive the plan from config, applying proxy lockdown. Fails on a proxy
    /// URL whose scheme librqbit would reject, before any socket is touched.
    fn from_config(config: &Config) -> Result<Self> {
        let proxy_url = config
            .privacy
            .checked_proxy_url()
            .map_err(|msg| anyhow::anyhow!(msg))?
            .map(str::to_string);
        let proxied = proxy_url.is_some();

        // Proxy lockdown. A SOCKS5 proxy carries outgoing TCP only, so
        // everything else would bypass it and expose the real address: DHT
        // and UDP trackers are plain UDP, the listener accepts *direct*
        // incoming connections (and its uTP socket and UPnP mapping exist
        // only to invite them), and LSD multicasts on the LAN. Rather than
        // ship a proxy that quietly leaks, proxy mode turns all of those off
        // — even when the config says `enable_dht = true`.
        Ok(Self {
            dht: config.network.enable_dht && !proxied,
            listen_port: (!proxied).then_some(config.network.listen_port),
            upnp: config.network.enable_upnp && !proxied,
            disable_lsd: proxied,
            proxy_url,
            blocklist_url: config.privacy.blocklist_url(),
            bind_interface: config.privacy.bind_interface().map(str::to_string),
        })
    }

    fn proxied(&self) -> bool {
        self.proxy_url.is_some()
    }

    /// The status to report to the UI, or `None` when nothing privacy-related
    /// is active. `blocklist_ranges` is filled in by the caller once the
    /// session has loaded the list.
    fn privacy_status(&self, blocklist_ranges: Option<usize>) -> Option<PrivacyStatus> {
        let active =
            self.proxied() || self.bind_interface.is_some() || self.blocklist_url.is_some();
        active.then(|| PrivacyStatus {
            proxy: self.proxied(),
            bind_interface: self.bind_interface.clone(),
            blocklist_ranges,
        })
    }
}

/// Build `SessionOptions` from a `NetworkPlan` and rate limits. The
/// `pick_listen_port` probe (which binds a socket) is only run when the plan
/// asks for a listener, so proxy mode touches no ports.
fn build_session_opts(plan: &NetworkPlan, ratelimits: LimitsConfig) -> SessionOptions {
    let listen = plan.listen_port.map(|port| ListenerOptions {
        listen_addr: (Ipv6Addr::UNSPECIFIED, pick_listen_port(port)).into(),
        enable_upnp_port_forwarding: plan.upnp,
        ..Default::default()
    });
    SessionOptions {
        dht: plan.dht.then(DhtSessionConfig::default),
        fastresume: true,
        persistence: Some(SessionPersistenceConfig::Json { folder: None }),
        listen,
        disable_local_service_discovery: plan.disable_lsd,
        connect: plan.proxy_url.clone().map(|url| ConnectionOptions {
            proxy_url: Some(url),
            ..Default::default()
        }),
        // Fail-closed by design: librqbit errors out of session creation
        // when the blocklist can't be fetched or the interface doesn't
        // exist, and the app treats that as fatal — starting without a
        // protection the user asked for would be worse than not starting.
        blocklist_url: plan.blocklist_url.clone(),
        bind_device_name: plan.bind_interface.clone(),
        // Configured limits bite from the first byte: the limiter is part
        // of the session, so torrents adopted from fastresume are capped
        // before the first command ever arrives.
        ratelimits,
        ..Default::default()
    }
}

impl TorrentEngine {
    pub async fn new(config: &Config) -> Result<Self> {
        let download_dir = PathBuf::from(&config.general.download_dir);
        std::fs::create_dir_all(&download_dir)?;

        let plan = NetworkPlan::from_config(config)?;
        let proxy_active = plan.proxied();
        let opts = build_session_opts(
            &plan,
            LimitsConfig {
                download_bps: speed_limit_bps(config.network.max_download_speed_kbps),
                upload_bps: speed_limit_bps(config.network.max_upload_speed_kbps),
            },
        );

        let session = Session::new_with_opts(download_dir, opts)
            .await
            .context("starting torrent session (check the [privacy] settings if any are set)")?;

        // Fail-closed on an empty blocklist: librqbit skips unparseable lines
        // and returns Ok, so a wrong-format file (CIDR .txt, an HTML error
        // page served 200) would otherwise load as zero ranges and run
        // effectively unprotected behind a green badge.
        if plan.blocklist_url.is_some() && session.blocklist.len() == 0 {
            anyhow::bail!(
                "blocklist loaded 0 ranges — check the file is PeerGuardian .p2p format \
                 (name:start-end per line); refusing to start unprotected"
            );
        }

        let privacy_status = plan.privacy_status(
            plan.blocklist_url
                .is_some()
                .then(|| session.blocklist.len()),
        );

        Ok(Self {
            session,
            proxy_active,
            privacy_status,
        })
    }

    /// True when the session runs behind a SOCKS5 proxy (lockdown mode).
    pub fn proxy_active(&self) -> bool {
        self.proxy_active
    }

    /// Count torrents whose tracker list still contains a `udp://` entry. Used
    /// once at startup in proxy mode to warn about torrents restored from a
    /// previous session, whose udp trackers announce directly around the proxy
    /// and which the app has no public API to sanitize in place.
    pub fn torrents_with_udp_trackers(&self) -> usize {
        // `with_torrents` takes an `Fn`, so accumulate through a Cell.
        let count = std::cell::Cell::new(0usize);
        self.session.with_torrents(|torrents| {
            for (_, handle) in torrents {
                if handle.shared().trackers.iter().any(|u| u.scheme() == "udp") {
                    count.set(count.get() + 1);
                }
            }
        });
        count.get()
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

    /// Pause a torrent in the session. A plain passthrough — since speed
    /// limits moved into librqbit's rate limiter, nothing in the engine pauses
    /// torrents on its own, so a pause always came from the user (or from a
    /// previous session via fastresume).
    pub async fn pause(&self, handle: &ManagedTorrentHandle) -> Result<()> {
        self.session.pause(handle).await?;
        Ok(())
    }

    pub async fn unpause(&self, handle: &ManagedTorrentHandle) -> Result<()> {
        self.session.unpause(handle).await?;
        Ok(())
    }

    /// Apply session-wide speed limits, live. librqbit swaps the governor
    /// bucket behind an `ArcSwap`, so the new caps take effect on the next
    /// chunk without restarting anything. `0` means unlimited.
    pub fn set_speed_limits(&self, download_kbps: u64, upload_kbps: u64) {
        self.session
            .ratelimits
            .set_download_bps(speed_limit_bps(download_kbps));
        self.session
            .ratelimits
            .set_upload_bps(speed_limit_bps(upload_kbps));
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

    /// Lightweight snapshot of `(id, handle)` pairs. Lets the bulk command
    /// handlers look up handles in O(1) instead of O(N) `get_handle` per id.
    fn handle_snapshot(&self) -> Vec<(usize, ManagedTorrentHandle)> {
        self.session
            .with_torrents(|iter| iter.map(|(id, h)| (id, h.clone())).collect())
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
                        let peers = live.snapshot.peer_stats.live;
                        (dl_bps, ul_bps, eta, peers)
                    } else {
                        (0, 0, None, 0)
                    };

                // `seen` is incremented once per distinct address when the
                // peer is first added, so every live peer is already counted
                // in it. Adding `live` on top double-counted the connected
                // ones (20 of 50 rendered as 20/70).
                let peers_total = if let Some(ref live) = stats.live {
                    live.snapshot.peer_stats.seen
                } else {
                    0
                };

                let is_detail = detail_id == Some(id);

                // The on-disk root for the `o` reveal keybinding. librqbit's
                // per-torrent output folder already has the multi-file
                // subfolder joined in at add time; a torrent whose files sit
                // directly in the shared download dir (fewer than two files
                // gets no subfolder) points at its single file instead so the
                // file manager can select it. Computed from librqbit's own
                // paths, never from the display name — the name is sanitized
                // for rendering and can differ from what is on disk.
                let content_path = handle
                    .with_metadata(|meta| {
                        let out = handle.output_folder();
                        match meta.file_infos.as_slice() {
                            [only] => out.join(&only.relative_filename),
                            _ => out.to_path_buf(),
                        }
                    })
                    .ok()
                    .map(|p| p.to_string_lossy().into_owned());

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
                    handle.with_metadata(|m| m.info.info().piece_length).ok()
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
                    content_path,
                }
            })
            .collect()
        })
    }

    /// Look up a live torrent by id. Linear scan under the session lock, which
    /// is why the bulk command handlers use `handle_snapshot()` once per batch
    /// instead of calling this per id. `None` means the torrent is no longer in
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
        // librqbit 9 can hold a torrent paused while it is still checking
        // files; surface that as Paused, matching what the engine will do
        // with it (nothing) rather than what it is doing right now.
        TorrentStatsState::Initializing { paused: true } => TorrentStatus::Paused,
        TorrentStatsState::Initializing { paused: false } => TorrentStatus::FetchingMetadata,
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

/// Config/dialog speed limit ("KB/s" meaning KiB/s, `0` = unlimited) → the
/// bytes/sec quota librqbit's rate limiter takes. Applies the same clamp as
/// every other entry point, which also enforces the 16 KiB/s floor: librqbit
/// acquires limiter permits in whole 16 KiB chunks, and governor reports an
/// acquire larger than the per-second quota as impossible — an error that
/// kills the peer task it happens on instead of pacing it. Saturating math
/// throughout; `clamped_speed_limit`'s cap keeps `kbps * 1024` within `u32`.
pub(crate) fn speed_limit_bps(kbps: u64) -> Option<NonZeroU32> {
    if kbps == 0 {
        return None;
    }
    let clamped = crate::clamped_speed_limit(kbps);
    let bps = clamped.saturating_mul(1024).min(u64::from(u32::MAX)) as u32;
    NonZeroU32::new(bps)
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
    // Resolve the string ourselves: librqbit 9's listener takes a bare
    // `SocketAddr` where tokio's bind accepted anything `ToSocketAddrs`, and
    // the config value may be a hostname like `localhost:0`.
    let addr = tokio::net::lookup_host(bind)
        .await
        .with_context(|| format!("resolve {}", bind))?
        .next()
        .with_context(|| format!("no addresses for {}", bind))?;
    let listener = TcpListener::bind_tcp(addr, BindOpts::default())
        .with_context(|| format!("bind {}", bind))?;
    let bound = listener.bind_addr();

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
            // `allow_create: false` and no upload body cap — both moot behind
            // `read_only`, which drops the state-modifying routes entirely.
            ..Default::default()
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

/// What `strip_udp_trackers` did to a magnet link.
pub(crate) struct StrippedMagnet {
    pub magnet: String,
    /// How many `tr=` parameters were dropped.
    pub removed: usize,
    /// How many trackers the magnet still carries.
    pub trackers_left: usize,
}

/// Form-decode one `application/x-www-form-urlencoded` component the way a URL
/// query parser does: `+` becomes a space, then percent-decode. This is how
/// librqbit reads magnet `tr=` values (`Url::query_pairs`), so matching it
/// exactly is what keeps [`strip_udp_trackers`] from disagreeing with librqbit
/// about a crafted value's scheme.
fn form_decode(component: &str) -> String {
    let spaced: String = component
        .chars()
        .map(|c| if c == '+' { ' ' } else { c })
        .collect();
    percent_encoding::percent_decode_str(&spaced)
        .decode_utf8_lossy()
        .into_owned()
}

/// True if `tr_value` (already form-decoded) names a udp tracker, decided by
/// the SAME parser librqbit dispatches on (`url::Url::parse` then `.scheme()`).
/// A hand-rolled `starts_with("udp://")` on a partly-decoded string is
/// bypassable: `url` strips interior tabs/newlines and trims leading control
/// characters, so `ud\tp://...` or a space-prefixed value parses as scheme
/// `udp` for librqbit while a substring check keeps it — and it would then
/// announce directly, around the proxy, leaking the real address.
fn is_udp_tracker(tr_value: &str) -> bool {
    url::Url::parse(tr_value).is_ok_and(|u| u.scheme() == "udp")
}

/// Drop `tr=` parameters naming `udp://` trackers from a magnet link. Used in
/// proxy mode only: a SOCKS5 proxy is TCP-only, so librqbit would announce to
/// udp trackers *directly*, leaking the real address to them — while the
/// http(s) trackers that remain are announced through the proxy. Everything
/// except the dropped parameters passes through byte-for-byte (no re-encoding,
/// no reordering), so hashes, names and exotic parameters survive untouched.
/// `.torrent` files are the documented gap: their announce lists live inside
/// the metainfo where the app never looks.
pub(crate) fn strip_udp_trackers(magnet: &str) -> StrippedMagnet {
    let Some((base, query)) = magnet.split_once('?') else {
        return StrippedMagnet {
            magnet: magnet.to_string(),
            removed: 0,
            trackers_left: 0,
        };
    };

    let mut kept: Vec<&str> = Vec::new();
    let mut removed = 0usize;
    let mut trackers_left = 0usize;
    for param in query.split('&') {
        let (name, value) = param.split_once('=').unwrap_or((param, ""));
        if form_decode(name).eq_ignore_ascii_case("tr") {
            if is_udp_tracker(&form_decode(value)) {
                removed += 1;
                continue;
            }
            trackers_left += 1;
        }
        kept.push(param);
    }

    StrippedMagnet {
        magnet: format!("{}?{}", base, kept.join("&")),
        removed,
        trackers_left,
    }
}

/// True for librqbit's "redundant transition" errors — `pause` on a paused
/// torrent ("torrent is already paused") and `unpause` on a live one
/// ("torrent is already live"). These are the *normal* outcome of bulk
/// toggles, not failures: `p` on a mixed marked set and `P`/`R` on a mixed
/// session deliberately send the whole set and let the engine sort it out.
/// Matched on the message text because librqbit raises them via `bail!` with
/// no dedicated variant; the worst a wording change upstream can cause is a
/// spurious warning, never a hidden failure.
fn is_benign_state_error(msg: &str) -> bool {
    msg.contains("already live") || msg.contains("already paused")
}

/// One status-bar line for a failed bulk operation. The status bar renders a
/// single unwrapped line and the message channel is bounded, so a batch gets
/// exactly one summary — count plus the first error — never one send per id.
fn bulk_op_error_message(op: &str, failed: usize, first_err: &str) -> String {
    format!(
        "\u{26a0} {} failed for {} torrent(s): {}",
        op, failed, first_err
    )
}

/// Failure accumulator for one bulk pause/resume batch — see
/// [`bulk_op_error_message`] for the reporting contract and
/// [`is_benign_state_error`] for why redundant transitions don't count.
#[derive(Default)]
struct BulkOpFailures {
    failed: usize,
    first_err: String,
}

impl BulkOpFailures {
    fn record(&mut self, op: &str, id: usize, res: Result<()>) {
        let Err(e) = res else { return };
        let msg = e.to_string();
        if is_benign_state_error(&msg) {
            tracing::debug!("{} skipped for torrent {}: {}", op, id, msg);
            return;
        }
        tracing::error!("{} failed for torrent {}: {}", op, id, msg);
        if self.failed == 0 {
            self.first_err = msg;
        }
        self.failed += 1;
    }

    async fn report(&self, op: &str, msg_tx: &mpsc::Sender<String>) {
        if self.failed > 0 {
            let _ = msg_tx
                .send(bulk_op_error_message(op, self.failed, &self.first_err))
                .await;
        }
    }
}

/// Surface a failed engine operation on the status bar as well as in the log.
/// Pause/resume failures used to be log-only, which made a failed `p` look
/// like a dead key: the default `torrenttui=warn` filter writes to a file
/// nobody is watching while the UI shows nothing. Mirrors the message shape
/// the Delete handler already uses. Benign redundant-transition errors (a
/// double-tapped `p` racing the next state push) are logged at debug and not
/// shown — see [`is_benign_state_error`].
async fn report_op_error(
    op: &str,
    id: usize,
    e: &impl std::fmt::Display,
    msg_tx: &mpsc::Sender<String>,
) {
    let msg = e.to_string();
    if is_benign_state_error(&msg) {
        tracing::debug!("{} skipped for torrent {}: {}", op, id, msg);
        return;
    }
    tracing::error!("{} failed for torrent {}: {}", op, id, msg);
    let _ = msg_tx
        .send(format!("\u{26a0} {} failed: {}", op, msg))
        .await;
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

/// The engine task. Owns the librqbit session and the watch folder, and runs
/// until it receives `Shutdown` or the command channel closes — both are
/// normal exits that return `Ok`.
///
/// Two arms drive everything: an incoming command, or the state-push tick.
/// Both end by calling `push_state`, so the UI gets a fresh snapshot after
/// every action without each command handler having to remember to send one.
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

    // Report the applied privacy posture once, right after the session that
    // embodies it exists — the header badge renders from this, never from
    // the raw config.
    if let Some(status) = engine.privacy_status.clone() {
        if let Err(e) = info_tx.send(EngineInfo::Privacy(status)).await {
            tracing::warn!("privacy status send dropped (UI gone?): {}", e);
        }
    }

    // Warn about restored torrents that carry udp:// trackers under a proxy.
    // librqbit restores persisted torrents inside `Session::new_with_opts`,
    // before the app can strip anything, and its UDP tracker client announces
    // them directly — around the proxy. The app cannot rewrite a live
    // torrent's tracker list through librqbit's public API, so the honest move
    // is to flag it so the user can re-add by magnet (which strips) or switch
    // to `bind_interface`.
    if engine.proxy_active() {
        let leaky = engine.torrents_with_udp_trackers();
        if leaky > 0 {
            let _ = msg_tx
                .send(format!(
                    "\u{26a0} Proxy mode: {} restored torrent(s) still list udp:// trackers, \
                     which announce directly. Re-add by magnet, or use privacy.bind_interface",
                    leaky
                ))
                .await;
        }
    }

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
    if engine.proxy_active() && config.general.watch_dir.is_some() {
        // librqbit's watcher adds `.torrent` AND `.magnet` files directly, on
        // its own thread, never through EngineCommand::AddTorrent — so the
        // udp-tracker strip cannot reach them and a dropped magnet would
        // announce its udp trackers around the proxy. Disable the watcher
        // entirely in proxy mode rather than ship that leak.
        let _ = msg_tx
            .send(
                "Watch folder disabled in proxy mode (watched files bypass udp-tracker \
                 stripping)"
                    .to_string(),
            )
            .await;
        tracing::info!("watch folder disabled: proxy mode active");
    } else if let Some(ref dir) = config.general.watch_dir {
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
    // restarts on every command, so a burst of them could starve the UI of
    // snapshots indefinitely.
    let mut state_tick = tokio::time::interval(STATE_PUSH_INTERVAL);
    state_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let enable_notifications = config.ui.enable_notifications;
    let mut cmd_rx = cmd_rx;

    let mut finished_set: HashSet<usize> = HashSet::new();

    // Tracks which torrent the UI is showing in Detail mode. When None, the
    // per-tick snapshot skips files/peers/trackers entirely.
    let mut detail_torrent_id: Option<usize> = None;

    /// Build the latest per-torrent snapshot and broadcast it to the UI. Also
    /// fires completion notifications and prunes the finished-set of torrents
    /// that no longer exist. Runs at the end of every command except
    /// `Shutdown`, which breaks out of the loop first, and on every timer
    /// tick.
    async fn push_state(
        engine: &TorrentEngine,
        state_tx: &mpsc::Sender<Vec<TorrentInfo>>,
        msg_tx: &mpsc::Sender<String>,
        finished_set: &mut HashSet<usize>,
        enable_notifications: bool,
        detail_id: Option<usize>,
    ) {
        let torrents = engine.get_all_torrents(detail_id);

        let current_ids: HashSet<usize> = torrents.iter().map(|t| t.id).collect();
        finished_set.retain(|id| current_ids.contains(id));

        for t in &torrents {
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
        }

        // try_send, not send: these are snapshots, so a full channel means the
        // UI is behind and the next tick supersedes this one anyway. Awaiting
        // here would stall command handling and the state tick behind UI
        // drain latency.
        let _ = state_tx.try_send(torrents);
    }

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(EngineCommand::AddTorrent(source)) => {
                        // Proxy mode: drop udp:// trackers before the magnet
                        // reaches librqbit (see strip_udp_trackers). Warn when
                        // that leaves the magnet trackerless — with DHT also
                        // off in this mode there is no discovery path left.
                        let source = if engine.proxy_active() && source.starts_with("magnet:?") {
                            let stripped = strip_udp_trackers(&source);
                            if stripped.removed > 0 && stripped.trackers_left == 0 {
                                let _ = msg_tx
                                    .send(
                                        "\u{26a0} Proxy mode: magnet had only udp:// trackers \
                                         (unusable through a SOCKS5 proxy) — it may not find peers"
                                            .to_string(),
                                    )
                                    .await;
                            }
                            stripped.magnet
                        } else {
                            source
                        };
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
                                report_op_error("Pause", id, &e, &msg_tx).await;
                            }
                        }
                    }
                    Some(EngineCommand::Resume(id)) => {
                        if let Some(handle) = engine.get_handle(id) {
                            if let Err(e) = engine.unpause(&handle).await {
                                report_op_error("Resume", id, &e, &msg_tx).await;
                            }
                        }
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
                    }
                    Some(EngineCommand::PauseMany(ids)) => {
                        // One snapshot, then O(1) lookups. `get_handle` is a
                        // linear scan under the session lock, so calling it per
                        // id made a bulk action O(M*N) with M lock acquisitions.
                        let handles: HashMap<usize, ManagedTorrentHandle> =
                            engine.handle_snapshot().into_iter().collect();
                        let mut failures = BulkOpFailures::default();
                        for id in ids {
                            if let Some(handle) = handles.get(&id) {
                                failures.record("Pause", id, engine.pause(handle).await);
                            }
                        }
                        failures.report("Pause", &msg_tx).await;
                    }
                    Some(EngineCommand::ResumeMany(ids)) => {
                        // One snapshot, then O(1) lookups. `get_handle` is a
                        // linear scan under the session lock, so calling it per
                        // id made a bulk action O(M*N) with M lock acquisitions.
                        let handles: HashMap<usize, ManagedTorrentHandle> =
                            engine.handle_snapshot().into_iter().collect();
                        let mut failures = BulkOpFailures::default();
                        for id in ids {
                            if let Some(handle) = handles.get(&id) {
                                failures.record("Resume", id, engine.unpause(handle).await);
                            }
                        }
                        failures.report("Resume", &msg_tx).await;
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
                        // Pauses every handle and lets the benign-error filter
                        // absorb the already-paused ones — real failures get
                        // the same single-summary treatment as PauseMany.
                        let mut failures = BulkOpFailures::default();
                        for (id, handle) in engine.handle_snapshot() {
                            failures.record("Pause", id, engine.pause(&handle).await);
                        }
                        failures.report("Pause", &msg_tx).await;
                    }
                    Some(EngineCommand::ResumeAll) => {
                        // See PauseAll — already-live errors are the normal
                        // case for a blanket resume.
                        let mut failures = BulkOpFailures::default();
                        for (id, handle) in engine.handle_snapshot() {
                            failures.record("Resume", id, engine.unpause(&handle).await);
                        }
                        failures.report("Resume", &msg_tx).await;
                    }
                    Some(EngineCommand::SetSpeedLimits { download_kbps, upload_kbps }) => {
                        engine.set_speed_limits(download_kbps, upload_kbps);
                        // Echo the clamped values, so the message agrees with
                        // both the enforced quota and the status-bar badge.
                        let down = crate::clamped_speed_limit(download_kbps);
                        let up = crate::clamped_speed_limit(upload_kbps);
                        tracing::info!("Speed limits set: down={}KB/s up={}KB/s", down, up);
                        let _ = msg_tx.send(format!(
                            "Speed limits updated: \u{2193} {} KB/s / \u{2191} {} KB/s",
                            down, up
                        )).await;
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
                push_state(&engine, &state_tx, &msg_tx, &mut finished_set, enable_notifications, detail_torrent_id).await;
            }
            _ = state_tick.tick() => {
                push_state(&engine, &state_tx, &msg_tx, &mut finished_set, enable_notifications, detail_torrent_id).await;
            }
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
    fn strip_udp_trackers_drops_udp_and_keeps_the_rest_verbatim() {
        let magnet = "magnet:?xt=urn:btih:1234567890abcdef1234567890abcdef12345678\
                      &dn=Some%20Name\
                      &tr=udp%3A%2F%2Ftracker.example%3A1337%2Fannounce\
                      &tr=https%3A%2F%2Ftr.example%3A443%2Fannounce\
                      &tr=UDP%3A%2F%2Fupper.example%3A80\
                      &tr=http%3A%2F%2Fplain.example%2Fannounce\
                      &x.pe=10.0.0.1%3A6881";
        let stripped = strip_udp_trackers(magnet);
        assert_eq!(stripped.removed, 2);
        assert_eq!(stripped.trackers_left, 2);
        assert_eq!(
            stripped.magnet,
            "magnet:?xt=urn:btih:1234567890abcdef1234567890abcdef12345678\
             &dn=Some%20Name\
             &tr=https%3A%2F%2Ftr.example%3A443%2Fannounce\
             &tr=http%3A%2F%2Fplain.example%2Fannounce\
             &x.pe=10.0.0.1%3A6881",
            "non-udp params must survive byte-for-byte"
        );
    }

    #[test]
    fn strip_udp_trackers_flags_a_magnet_left_trackerless() {
        let stripped = strip_udp_trackers(
            "magnet:?xt=urn:btih:1234567890abcdef1234567890abcdef12345678\
             &tr=udp%3A%2F%2Fonly.example%3A1337",
        );
        assert_eq!(stripped.removed, 1);
        assert_eq!(stripped.trackers_left, 0);
    }

    #[test]
    fn strip_udp_trackers_handles_unencoded_and_queryless_magnets() {
        // Unencoded tr= values occur in the wild; the sniff must decode-agnostic.
        let stripped = strip_udp_trackers(
            "magnet:?xt=urn:btih:abc&tr=udp://x.example:1337&tr=wss://y.example",
        );
        assert_eq!(stripped.removed, 1);
        assert_eq!(stripped.trackers_left, 1);
        assert_eq!(
            stripped.magnet,
            "magnet:?xt=urn:btih:abc&tr=wss://y.example"
        );

        let untouched = strip_udp_trackers("magnet:no-question-mark");
        assert_eq!(untouched.magnet, "magnet:no-question-mark");
        assert_eq!(untouched.removed, 0);
    }

    #[test]
    fn strip_udp_trackers_matches_librqbits_url_parser_on_crafted_values() {
        // These are the bypasses a substring sniff misses: librqbit form-
        // decodes then url::Url::parse, which strips an interior tab and maps
        // a leading '+' to a space it then trims — both yield scheme "udp".
        // The strip must agree, or the udp announce leaks around the proxy.
        for (label, tr) in [
            ("interior tab", "tr=ud%09p%3A%2F%2Fevil.example%3A1337"),
            ("leading plus", "tr=+udp%3A%2F%2Fevil.example"),
            ("leading space", "tr=%20udp%3A%2F%2Fevil.example"),
            ("uppercase TR name", "TR=udp%3A%2F%2Fevil.example"),
        ] {
            let stripped = strip_udp_trackers(&format!("magnet:?xt=urn:btih:abc&{}", tr));
            assert_eq!(stripped.removed, 1, "{label}: should be stripped");
            assert_eq!(stripped.trackers_left, 0, "{label}");
        }
    }

    #[test]
    fn is_udp_tracker_agrees_with_scheme() {
        assert!(is_udp_tracker("udp://x:1337"));
        assert!(is_udp_tracker("ud\tp://x:1337")); // url strips interior tab
        assert!(is_udp_tracker(" udp://x")); // leading space trimmed
        assert!(!is_udp_tracker("https://x/announce"));
        assert!(!is_udp_tracker("wss://x"));
        assert!(!is_udp_tracker("not a url"));
    }

    fn cfg_with_privacy(proxy: &str, dht: bool) -> Config {
        let mut cfg = Config::default();
        cfg.network.enable_dht = dht;
        cfg.privacy.proxy_url = proxy.to_string();
        cfg
    }

    #[test]
    fn proxy_mode_locks_down_the_network_plan() {
        // The core safety property: a proxy forces DHT/listener/LSD off even
        // with enable_dht = true. Pinned here because the real SessionOptions
        // path needs a live session and can't be unit-tested.
        let plan =
            NetworkPlan::from_config(&cfg_with_privacy("socks5://127.0.0.1:1080", true)).unwrap();
        assert!(!plan.dht, "DHT must be off under proxy");
        assert!(plan.listen_port.is_none(), "no listener under proxy");
        assert!(!plan.upnp, "no UPnP under proxy");
        assert!(plan.disable_lsd, "LSD off under proxy");
        assert_eq!(plan.proxy_url.as_deref(), Some("socks5://127.0.0.1:1080"));

        // And the SessionOptions built from it agree.
        let opts = build_session_opts(&plan, LimitsConfig::default());
        assert!(opts.dht.is_none());
        assert!(opts.listen.is_none());
        assert!(opts.disable_local_service_discovery);
        assert_eq!(
            opts.connect.and_then(|c| c.proxy_url).as_deref(),
            Some("socks5://127.0.0.1:1080")
        );
    }

    #[test]
    fn without_a_proxy_dht_and_listener_stay_on() {
        let plan = NetworkPlan::from_config(&cfg_with_privacy("", true)).unwrap();
        assert!(plan.dht);
        assert!(plan.listen_port.is_some());
        assert!(!plan.disable_lsd);
        assert!(plan.proxy_url.is_none());
        let opts = build_session_opts(&plan, LimitsConfig::default());
        assert!(opts.dht.is_some());
        assert!(opts.listen.is_some());
        assert!(opts.connect.is_none());
    }

    #[test]
    fn a_bad_proxy_scheme_fails_the_plan_before_any_socket() {
        let err = NetworkPlan::from_config(&cfg_with_privacy("http://127.0.0.1:8080", true))
            .unwrap_err()
            .to_string();
        assert!(err.contains("privacy.proxy_url"), "got: {err}");
    }

    #[test]
    fn blocklist_and_bind_pass_through_the_plan() {
        let mut cfg = Config::default();
        cfg.privacy.bind_interface = "wg0".to_string();
        cfg.privacy.blocklist_url = "/tmp/x.p2p".to_string();
        let plan = NetworkPlan::from_config(&cfg).unwrap();
        assert_eq!(plan.bind_interface.as_deref(), Some("wg0"));
        assert!(plan.blocklist_url.is_some());
        let opts = build_session_opts(&plan, LimitsConfig::default());
        assert_eq!(opts.bind_device_name.as_deref(), Some("wg0"));
        assert!(opts.blocklist_url.is_some());
    }

    #[test]
    fn privacy_status_is_none_when_nothing_is_active() {
        let plan = NetworkPlan::from_config(&Config::default()).unwrap();
        assert!(plan.privacy_status(None).is_none());
    }

    #[tokio::test]
    async fn report_op_error_reaches_the_status_channel() {
        let (tx, mut rx) = mpsc::channel(16);
        report_op_error("Pause", 3, &"boom", &tx).await;
        assert_eq!(rx.recv().await.unwrap(), "\u{26a0} Pause failed: boom");
    }

    #[tokio::test]
    async fn report_op_error_stays_quiet_on_redundant_transitions() {
        // A double-tapped `p` racing the next state push makes librqbit bail
        // with "torrent is already paused" — success from the user's view.
        let (tx, mut rx) = mpsc::channel(16);
        report_op_error("Pause", 3, &"torrent is already paused", &tx).await;
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn benign_state_errors_are_recognized() {
        assert!(is_benign_state_error("torrent is already paused"));
        assert!(is_benign_state_error("torrent is already live"));
        assert!(!is_benign_state_error("disk full"));
    }

    #[test]
    fn bulk_op_error_message_shape_is_pinned() {
        assert_eq!(
            bulk_op_error_message("Resume", 2, "boom"),
            "\u{26a0} Resume failed for 2 torrent(s): boom"
        );
    }

    #[tokio::test]
    async fn bulk_failures_send_one_summary_with_the_first_real_error() {
        let (tx, mut rx) = mpsc::channel(16);
        let mut failures = BulkOpFailures::default();
        failures.record("Resume", 0, Ok(()));
        // Benign redundant transition: the normal case for a mixed `p` batch,
        // must not count as a failure.
        failures.record("Resume", 1, Err(anyhow::anyhow!("torrent is already live")));
        failures.record("Resume", 2, Err(anyhow::anyhow!("boom")));
        failures.record("Resume", 3, Err(anyhow::anyhow!("later")));
        failures.report("Resume", &tx).await;
        assert_eq!(
            rx.recv().await.unwrap(),
            "\u{26a0} Resume failed for 2 torrent(s): boom"
        );
        assert!(rx.try_recv().is_err(), "exactly one summary per batch");
    }

    #[tokio::test]
    async fn bulk_report_sends_nothing_without_real_failures() {
        let (tx, mut rx) = mpsc::channel(16);
        let mut failures = BulkOpFailures::default();
        failures.record("Pause", 0, Ok(()));
        failures.record(
            "Pause",
            1,
            Err(anyhow::anyhow!("torrent is already paused")),
        );
        failures.report("Pause", &tx).await;
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn pick_listen_port_skips_taken_port() {
        // Occupy an ephemeral port the same way the probe binds, then ask for
        // it: the probe must move past it but stay inside the range.
        let taken = std::net::TcpListener::bind((Ipv6Addr::UNSPECIFIED, 0)).unwrap();
        let port = taken.local_addr().unwrap().port();
        if port > u16::MAX - PORT_RANGE_SIZE {
            // The range would saturate empty and the fallback kicks in; the
            // empty-range test below covers that path.
            return;
        }
        let picked = pick_listen_port(port);
        assert_ne!(picked, port);
        assert!((port..port.saturating_add(PORT_RANGE_SIZE)).contains(&picked));
    }

    #[test]
    fn pick_listen_port_empty_range_falls_back_to_start() {
        // start == u16::MAX saturates into an empty range; the fallback keeps
        // the configured port so the session reports the real bind error.
        assert_eq!(pick_listen_port(u16::MAX), u16::MAX);
    }

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
    fn speed_limit_zero_means_unlimited() {
        assert_eq!(speed_limit_bps(0), None);
    }

    #[test]
    fn speed_limit_converts_kib_not_kilo_or_megabit() {
        // Config "KB/s" means KiB/s: ×1024 to bytes/sec — not ×1000, and not
        // a megabit conversion. Getting this wrong is the #46 bug class.
        assert_eq!(speed_limit_bps(1024).map(|v| v.get()), Some(1024 * 1024));
        assert_eq!(speed_limit_bps(16).map(|v| v.get()), Some(16 * 1024));
    }

    #[test]
    fn speed_limit_floors_at_one_chunk_per_second() {
        // librqbit acquires limiter permits in whole 16 KiB chunks; a quota
        // below one chunk/sec makes governor kill the peer task instead of
        // pacing it. 1 KB/s must clamp up to 16 KiB/s, not pass through.
        assert_eq!(speed_limit_bps(1).map(|v| v.get()), Some(16 * 1024));
        assert_eq!(speed_limit_bps(15).map(|v| v.get()), Some(16 * 1024));
    }

    #[test]
    fn speed_limit_never_overflows_the_limiter_quota() {
        // The limiter quota is a NonZeroU32 of bytes/sec; the shared cap must
        // keep kbps × 1024 inside it (and huge inputs must not panic).
        let max = speed_limit_bps(u64::MAX).map(|v| v.get()).unwrap_or(0);
        assert!(max > 0);
        assert_eq!(u64::from(max), crate::clamped_speed_limit(u64::MAX) * 1024);
        // And inside governor's pacing ceiling: replenishment is one
        // byte-permit per whole nanosecond at most, so any quota above 1e9
        // bytes/sec would be displayed but not enforced.
        assert!(u64::from(max) <= 1_000_000_000);
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
