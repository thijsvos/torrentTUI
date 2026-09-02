//! The stall doctor: turns the numbers in [`TorrentHealth`] and
//! [`NetworkHealth`] into one plain-language verdict — *why* a torrent is
//! slow or stuck, and what to do about it.
//!
//! Pure, by design. Everything time-based (how long since bytes last moved,
//! how long since the torrent appeared) is measured by `App` and handed in
//! through [`Context`], so every branch below is a table test away from being
//! pinned. No I/O, no clocks, no librqbit types.
//!
//! The order of the checks is the order of *structural* certainty: a torrent
//! in an error state is blocked whatever its peers look like; a torrent with
//! no way to discover peers is stalled whatever its connection stats say; only
//! once those are ruled out do the softer signals (your own speed cap, a thin
//! swarm, corrupt data) get a say. When a verdict names a config key it also
//! says whether the change applies live or needs a restart — librqbit builds
//! its DHT, listener and proxy once, and only the speed caps swap in place.

use crate::engine::torrent::PrivacyStatus;
use crate::types::{NetworkHealth, TorrentInfo, TorrentStatus, TrackerStatus, UpnpState};
use crate::ui::layout::{format_size, format_speed};
use std::time::Duration;

/// How long a downloading torrent may go without a byte of progress — or a
/// magnet without metadata — before the doctor calls it stalled. Tracker
/// announces and DHT lookups routinely take 10–20 s on a fresh add, so this
/// sits above the normal warm-up rather than at it.
pub const STALL_AFTER: Duration = Duration::from_secs(30);

/// A transfer running at this share of its cap or more is "capped": the cap,
/// not the swarm, is what decides the speed.
const CAPPED_AT_PERCENT: u64 = 90;

/// Unverified bytes above this, on a torrent that is not progressing, are
/// worth mentioning as possible corruption. In-flight pieces also count as
/// unverified, so this is deliberately generous and only raised as a cause
/// once the torrent has actually stalled.
const SUSPICIOUS_UNVERIFIED_BYTES: u64 = 32 * 1024 * 1024;

/// A swarm this thin, relative to what has been discovered, gets a note even
/// while data flows.
const THIN_SWARM_LIVE: u32 = 2;
const THIN_SWARM_SEEN: u32 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Healthy,
    /// Working, with something worth knowing.
    Note,
    /// Running against a speed limit the user set.
    Capped,
    /// Not progressing, and the doctor can say why.
    Stalled,
    /// In an error state; nothing will happen until it is fixed.
    Blocked,
}

impl Severity {
    /// A glyph that carries the meaning without colour (#77).
    pub fn glyph(self) -> &'static str {
        match self {
            Severity::Healthy => "\u{2713}",
            Severity::Note | Severity::Capped => "\u{25cf}",
            Severity::Stalled => "\u{26a0}",
            Severity::Blocked => "\u{2716}",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Severity::Healthy => "Healthy",
            Severity::Note => "Note",
            Severity::Capped => "Capped",
            Severity::Stalled => "Stalled",
            Severity::Blocked => "Blocked",
        }
    }
}

/// What the doctor says. `headline` is one line for the Stats tab and the
/// table; `causes` are the evidence, one per line; `next_step` is the single
/// most useful thing to try, naming the key or config knob involved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub severity: Severity,
    pub headline: String,
    pub causes: Vec<String>,
    pub next_step: Option<String>,
}

/// Everything the doctor looks at.
pub struct Context<'a> {
    pub torrent: &'a TorrentInfo,
    /// `None` until the engine's first network push (about a second in).
    pub network: Option<&'a NetworkHealth>,
    /// `None` when no privacy feature is active.
    pub privacy: Option<&'a PrivacyStatus>,
    /// Session-wide caps in KB/s, `0` = unlimited — the values the `t`
    /// dialog holds.
    pub download_limit_kbps: u64,
    pub upload_limit_kbps: u64,
    /// Time since the torrent last gained a byte, `None` while it is gaining
    /// them (or is not downloading at all).
    pub stalled_for: Option<Duration>,
}

/// The one threshold the table's stall marker and the doctor share, so a row
/// never says "Stalled" while the Health tab says otherwise.
///
/// Only a downloading torrent can stall. `FetchingMetadata` is librqbit's
/// *initializing* state — hash-checking whatever is already on disk — which
/// legitimately takes minutes for a large torrent; and a magnet whose
/// metadata has not arrived is not in the session yet at all (librqbit
/// resolves it inside `add_torrent`), so it has no row to mark — see
/// `NetworkHealth::pending_adds`.
pub fn is_stalled(status: &TorrentStatus, stalled_for: Option<Duration>) -> bool {
    matches!(status, TorrentStatus::Downloading) && stalled_for.is_some_and(|d| d >= STALL_AFTER)
}

pub fn diagnose(ctx: &Context) -> Verdict {
    let t = ctx.torrent;
    match &t.status {
        TorrentStatus::Error(msg) => blocked(msg, t.health.error_chain.as_deref()),
        TorrentStatus::Paused => Verdict {
            severity: Severity::Healthy,
            headline: "Paused by you".to_string(),
            causes: Vec::new(),
            next_step: Some("Press p to resume".to_string()),
        },
        TorrentStatus::Complete | TorrentStatus::Seeding => seeding(ctx),
        TorrentStatus::FetchingMetadata => Verdict {
            severity: Severity::Healthy,
            headline: format!(
                "Verifying existing data on disk — {} checked",
                format_size(t.downloaded_bytes)
            ),
            causes: vec![
                "librqbit hash-checks what is already in the download folder before transferring"
                    .to_string(),
            ],
            next_step: None,
        },
        TorrentStatus::Downloading => downloading(ctx),
    }
}

/// A magnet still being resolved: librqbit only creates the torrent once a
/// peer has handed over the metadata, so until then the only thing to show is
/// how long it has been asking. Session-level, because there is no torrent.
pub fn pending_add_line(label: &str, secs: u64, network: Option<&NetworkHealth>) -> String {
    let mut line = format!("Resolving \"{}\" for {}", label, fmt_secs(secs));
    if secs >= STALL_AFTER.as_secs() {
        line.push_str(" — no peer has sent the metadata yet");
        if let Some(n) = network {
            match &n.dht {
                None => line.push_str(" (DHT is off)"),
                Some(d) if d.routing_table_size + d.routing_table_size_v6 == 0 => {
                    line.push_str(" (DHT has no nodes)")
                }
                Some(_) => {}
            }
        }
    }
    line
}

fn blocked(msg: &str, chain: Option<&str>) -> Verdict {
    let text = chain.unwrap_or(msg);
    let lower = text.to_ascii_lowercase();
    let next_step = if lower.contains("no space left") || lower.contains("disk full") {
        Some("Free up space in general.download_dir, then press p twice to restart it".to_string())
    } else if lower.contains("permission denied") || lower.contains("access is denied") {
        Some("Check write permissions on general.download_dir".to_string())
    } else if lower.contains("bencode")
        || lower.contains("metainfo")
        || lower.contains("invalid torrent")
    {
        Some(
            "The .torrent file looks malformed — re-download it or use the magnet link".to_string(),
        )
    } else {
        None
    };
    Verdict {
        severity: Severity::Blocked,
        headline: format!("Error: {}", msg),
        causes: chain
            .filter(|c| *c != msg)
            .map(|c| c.to_string())
            .into_iter()
            .collect(),
        next_step,
    }
}

fn seeding(ctx: &Context) -> Verdict {
    let t = ctx.torrent;
    let proxied = ctx.privacy.is_some_and(|p| p.proxy);
    let no_listener = ctx.network.is_some_and(|n| n.listen_port.is_none()) || proxied;
    if no_listener {
        return Verdict {
            severity: Severity::Note,
            headline: "Seeding without a listener — only peers you dial can download from you"
                .to_string(),
            causes: vec![if proxied {
                "The SOCKS5 proxy lockdown runs no incoming listener".to_string()
            } else {
                "No incoming listener is bound".to_string()
            }],
            next_step: proxied.then(|| {
                "Seeding works best unproxied; unset privacy.proxy_url (restart required)"
                    .to_string()
            }),
        };
    }
    if let Some(cap) = capped(t.upload_speed, ctx.upload_limit_kbps) {
        return Verdict {
            severity: Severity::Capped,
            headline: format!(
                "Uploading at {} against your {} KB/s upload limit",
                format_speed(t.upload_speed),
                cap
            ),
            causes: Vec::new(),
            next_step: Some("Press t to raise it (applies live)".to_string()),
        };
    }
    let live = t.health.peers.live;
    let mut causes = Vec::new();
    if let Some(n) = ctx.network {
        if let UpnpState::Failed(err) = &n.upnp {
            causes.push(format!(
                "UPnP could not open port {}: {} — peers behind NAT cannot reach you",
                n.listen_port.unwrap_or(0),
                err
            ));
        }
    }
    let headline = if t.upload_speed > 0 {
        format!("Seeding to {} peer{}", live, plural(u64::from(live)))
    } else if live > 0 {
        format!(
            "Complete — {} peer{} connected, nobody needs data right now",
            live,
            plural(u64::from(live))
        )
    } else {
        "Complete — no peers connected".to_string()
    };
    Verdict {
        severity: if causes.is_empty() {
            Severity::Healthy
        } else {
            Severity::Note
        },
        headline,
        causes,
        next_step: None,
    }
}

fn downloading(ctx: &Context) -> Verdict {
    let t = ctx.torrent;
    let peers = &t.health.peers;

    if let Some(stalled_for) = ctx.stalled_for.filter(|d| *d >= STALL_AFTER) {
        let since = fmt_secs(stalled_for.as_secs());
        if peers.seen == 0 {
            let disc = discovery(ctx);
            return Verdict {
                severity: Severity::Stalled,
                headline: if disc.no_sources {
                    "No way to find peers".to_string()
                } else {
                    format!("No peers found — nothing received for {}", since)
                },
                causes: disc.causes,
                next_step: disc.next_step,
            };
        }
        if peers.live == 0 {
            let (causes, next_step) = connectivity(ctx);
            return Verdict {
                severity: Severity::Stalled,
                headline: format!(
                    "{} peers known, none reachable — nothing received for {}",
                    peers.seen, since
                ),
                causes,
                next_step,
            };
        }
        // Connected, but starved.
        let mut causes = vec![format!(
            "{} of {} known peers connected; {} dead, {} connecting",
            peers.live, peers.seen, peers.dead, peers.connecting
        )];
        if peers.not_needed > 0 {
            causes.push(format!(
                "{} peer{} ha{} nothing we still need",
                peers.not_needed,
                plural(u64::from(peers.not_needed)),
                if peers.not_needed == 1 { "s" } else { "ve" }
            ));
        }
        let unverified = t
            .health
            .fetched_bytes
            .saturating_sub(t.health.checked_bytes);
        if unverified >= SUSPICIOUS_UNVERIFIED_BYTES {
            causes.push(format!(
                "{} fetched but never verified — a peer may be sending corrupt data",
                format_size(unverified)
            ));
        }
        return Verdict {
            severity: Severity::Stalled,
            headline: format!(
                "{} peer{} connected but nobody is sending — nothing received for {}",
                peers.live,
                plural(u64::from(peers.live)),
                since
            ),
            causes,
            next_step: Some(
                "The swarm may have no seeders; check the seeder count where you found the torrent"
                    .to_string(),
            ),
        };
    }

    if let Some(cap) = capped(t.download_speed, ctx.download_limit_kbps) {
        return Verdict {
            severity: Severity::Capped,
            headline: format!(
                "Running at {} against your {} KB/s download limit",
                format_speed(t.download_speed),
                cap
            ),
            causes: Vec::new(),
            next_step: Some("Press t to raise it (applies live)".to_string()),
        };
    }

    if peers.live <= THIN_SWARM_LIVE && peers.seen >= THIN_SWARM_SEEN {
        let (causes, next_step) = connectivity(ctx);
        return Verdict {
            severity: Severity::Note,
            headline: format!(
                "Only {} of {} known peers connected",
                peers.live, peers.seen
            ),
            causes,
            next_step,
        };
    }

    // Nothing connected yet but still inside the warm-up: not a stall, not
    // healthy either — say what is being tried.
    if peers.live == 0 {
        let disc = discovery(ctx);
        return Verdict {
            severity: Severity::Note,
            headline: if peers.seen == 0 {
                "Looking for peers".to_string()
            } else {
                format!("{} peers found, connecting", peers.seen)
            },
            causes: disc.causes,
            next_step: None,
        };
    }

    let mut headline = format!(
        "{} peer{} connected",
        peers.live,
        plural(u64::from(peers.live))
    );
    if let Some(ms) = t.health.avg_piece_ms {
        headline.push_str(&format!(", avg piece {:.1} s", ms as f64 / 1000.0));
    }
    let causes = best_tracker_line(t).into_iter().collect();
    Verdict {
        severity: Severity::Healthy,
        headline,
        causes,
        next_step: None,
    }
}

/// The share of the cap the transfer is using, when there is a cap and the
/// transfer is at or above `CAPPED_AT_PERCENT` of it. Returns the cap.
fn capped(speed_bps: u64, limit_kbps: u64) -> Option<u64> {
    if limit_kbps == 0 {
        return None;
    }
    let limit_bps = limit_kbps.saturating_mul(1024);
    (speed_bps.saturating_mul(100) >= limit_bps.saturating_mul(CAPPED_AT_PERCENT))
        .then_some(limit_kbps)
}

struct Discovery {
    causes: Vec<String>,
    next_step: Option<String>,
    /// Neither DHT nor any tracker can produce a peer.
    no_sources: bool,
}

/// Can this torrent find peers at all? DHT and trackers, each judged from
/// what the engine reported rather than from config.
fn discovery(ctx: &Context) -> Discovery {
    let t = ctx.torrent;
    let proxied = ctx.privacy.is_some_and(|p| p.proxy);
    let mut causes = Vec::new();
    let mut next_step = None;

    // DHT.
    let dht_usable = match ctx.network {
        None => true, // not reported yet; assume the best
        Some(n) => match &n.dht {
            None if proxied => {
                causes.push("DHT is off (proxy lockdown)".to_string());
                false
            }
            None => {
                causes.push("DHT is disabled (network.enable_dht = false)".to_string());
                false
            }
            Some(d) if d.routing_table_size + d.routing_table_size_v6 == 0 => {
                causes.push(
                    "DHT has no nodes yet (still bootstrapping — or UDP is blocked)".to_string(),
                );
                false
            }
            Some(d) => {
                causes.push(format!(
                    "DHT: {} nodes",
                    d.routing_table_size + d.routing_table_size_v6
                ));
                true
            }
        },
    };

    // Trackers.
    let tc = &t.health.trackers;
    let trackers_usable = tc.ok > 0 || tc.pending > 0;
    if tc.total == 0 && tc.stripped_udp > 0 {
        causes.push(format!(
            "All {} tracker{} were udp:// and stripped (a SOCKS5 proxy carries TCP only)",
            tc.stripped_udp,
            plural(tc.stripped_udp as u64)
        ));
    } else if tc.total == 0 {
        causes.push("No trackers on this torrent".to_string());
    } else {
        let mut parts = Vec::new();
        if tc.ok > 0 {
            parts.push(format!("{} ok", tc.ok));
        }
        if tc.failing > 0 {
            parts.push(format!("{} failing", tc.failing));
        }
        if tc.pending > 0 {
            parts.push(format!("{} not answered yet", tc.pending));
        }
        if tc.unsupported > 0 {
            parts.push(format!("{} unsupported", tc.unsupported));
        }
        if tc.bypassing_proxy > 0 {
            parts.push(format!("{} udp:// bypassing the proxy", tc.bypassing_proxy));
        }
        if tc.stripped_udp > 0 {
            parts.push(format!("{} udp:// stripped", tc.stripped_udp));
        }
        causes.push(format!("Trackers: {} — {}", tc.total, parts.join(", ")));
        if let Some(line) = worst_tracker_line(t) {
            causes.push(line);
        }
    }

    let no_sources = !dht_usable && !trackers_usable;
    if no_sources {
        let all_trackers_failing = tc.failing > 0 && tc.failing == tc.total;
        let dht_off_by_config = ctx.network.is_some_and(|n| n.dht.is_none()) && !proxied;
        next_step = Some(if proxied && (tc.stripped_udp > 0 || tc.total == 0) {
            "Add an http(s) tracker to the magnet (&tr=https://…), or unset privacy.proxy_url (restart required)".to_string()
        } else if dht_off_by_config && all_trackers_failing {
            "Every tracker is failing and DHT is disabled — set network.enable_dht = true (restart required)".to_string()
        } else if dht_off_by_config {
            "Set network.enable_dht = true (restart required), or add a working tracker".to_string()
        } else if all_trackers_failing {
            "Every tracker is failing (see the error above) and DHT has no nodes — is UDP blocked, or the network down?".to_string()
        } else {
            "DHT has no nodes — if it stays at 0, UDP is blocked; add an http(s) tracker as a fallback".to_string()
        });
    }

    Discovery {
        causes,
        next_step,
        no_sources,
    }
}

/// Peers are known but not connecting: what the session-wide connection
/// counters say about why.
fn connectivity(ctx: &Context) -> (Vec<String>, Option<String>) {
    let t = ctx.torrent;
    let p = &t.health.peers;
    let proxied = ctx.privacy.is_some_and(|p| p.proxy);
    let mut causes = vec![format!(
        "{} known: {} dead, {} connecting, {} queued, {} live",
        p.seen, p.dead, p.connecting, p.queued, p.live
    )];
    let mut next_step = None;

    if let Some(n) = ctx.network {
        let (name, stats) = if proxied {
            ("SOCKS5", n.connect.socks)
        } else {
            ("TCP", n.connect.tcp)
        };
        if stats.attempts >= 10 && stats.successes == 0 {
            causes.push(format!(
                "Every outgoing {} connection failed ({} of {})",
                name, stats.errors, stats.attempts
            ));
            next_step = Some(if proxied {
                "Is the proxy up? Check privacy.proxy_url (restart required to change)".to_string()
            } else if let Some(iface) = ctx.privacy.and_then(|p| p.bind_interface.as_deref()) {
                format!("Is {iface} up and routed? All sockets are bound to it (privacy.bind_interface)")
            } else {
                "Something is blocking outgoing connections — firewall, VPN, or the ISP".to_string()
            });
        } else if stats.attempts >= 10 && stats.failure_ratio() >= 0.9 {
            causes.push(format!(
                "{:.0}% of outgoing {} connections fail ({} of {})",
                stats.failure_ratio() * 100.0,
                name,
                stats.errors,
                stats.attempts
            ));
        }
        if n.blocked_outgoing > 0 {
            causes.push(format!(
                "Blocklist rejected {} outgoing connection{}",
                n.blocked_outgoing,
                plural(n.blocked_outgoing)
            ));
            if next_step.is_none() && n.blocked_outgoing >= u64::from(p.seen.max(1)) {
                next_step = Some(
                    "The blocklist is rejecting most of this swarm — check privacy.blocklist_url (restart required)"
                        .to_string(),
                );
            }
        }
    }
    if next_step.is_none() && p.dead > 0 && p.dead >= p.seen / 2 {
        next_step = Some(
            "Most known peers refuse or time out — the swarm may be mostly dead; give it a few minutes"
                .to_string(),
        );
    }
    (causes, next_step)
}

/// The most recently answering tracker, for the healthy verdict.
fn best_tracker_line(t: &TorrentInfo) -> Option<String> {
    t.trackers
        .iter()
        .filter_map(|tr| match tr.status {
            TrackerStatus::Ok {
                last_announce_secs_ago,
                next_in_secs,
            } => Some((last_announce_secs_ago, next_in_secs, &tr.url)),
            _ => None,
        })
        .min_by_key(|(ago, _, _)| *ago)
        .map(|(ago, next, url)| {
            let mut line = format!("{} announced {} ago", tracker_host(url), fmt_secs(ago));
            if let Some(n) = next {
                line.push_str(&format!(" (next in {})", fmt_secs(n)));
            }
            line
        })
}

/// One failing tracker with its error, for the stalled verdicts.
fn worst_tracker_line(t: &TorrentInfo) -> Option<String> {
    t.trackers.iter().find_map(|tr| match &tr.status {
        TrackerStatus::Failing { last_error, .. } => {
            Some(format!("{}: {}", tracker_host(&tr.url), last_error))
        }
        _ => None,
    })
}

/// `https://tracker.opentrackr.org:443/announce` → `tracker.opentrackr.org`.
pub fn tracker_host(url: &str) -> &str {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let host_port = rest.split(['/', '?']).next().unwrap_or(rest);
    host_port
        .rsplit_once(':')
        .map(|(h, _)| h)
        .unwrap_or(host_port)
}

pub fn fmt_secs(s: u64) -> String {
    if s < 60 {
        format!("{s} s")
    } else if s < 3600 {
        format!("{} min", s / 60)
    } else {
        format!("{} h {} min", s / 3600, (s % 3600) / 60)
    }
}

fn plural(n: u64) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        ConnectHealth, DhtHealth, PeerBreakdown, TorrentHealth, TrackerCounts, TrackerInfo,
        TransportStats,
    };

    fn torrent(status: TorrentStatus) -> TorrentInfo {
        TorrentInfo {
            id: 0,
            name: "t".to_string(),
            size_bytes: 1_000_000,
            downloaded_bytes: 500_000,
            uploaded_bytes: 0,
            download_speed: 0,
            upload_speed: 0,
            peers_connected: 0,
            peers_total: 0,
            status,
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

    fn network() -> NetworkHealth {
        NetworkHealth {
            listen_port: Some(6881),
            dht: Some(DhtHealth {
                routing_table_size: 300,
                routing_table_size_v6: 12,
                outstanding_requests: 0,
            }),
            connect: ConnectHealth {
                tcp: TransportStats {
                    attempts: 100,
                    successes: 80,
                    errors: 20,
                },
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn proxied() -> PrivacyStatus {
        PrivacyStatus {
            proxy: true,
            bind_interface: None,
            blocklist_ranges: None,
        }
    }

    fn ctx<'a>(t: &'a TorrentInfo, n: Option<&'a NetworkHealth>) -> Context<'a> {
        Context {
            torrent: t,
            network: n,
            privacy: None,
            download_limit_kbps: 0,
            upload_limit_kbps: 0,
            stalled_for: None,
        }
    }

    fn joined(v: &Verdict) -> String {
        let mut s = v.headline.clone();
        for c in &v.causes {
            s.push('\n');
            s.push_str(c);
        }
        if let Some(n) = &v.next_step {
            s.push('\n');
            s.push_str(n);
        }
        s
    }

    // -- structural first -----------------------------------------------------

    #[test]
    fn error_is_blocked_and_classifies_disk_full() {
        let mut t = torrent(TorrentStatus::Error("write failed".to_string()));
        t.health.error_chain =
            Some("write failed: No space left on device (os error 28)".to_string());
        let v = diagnose(&ctx(&t, Some(&network())));
        assert_eq!(v.severity, Severity::Blocked);
        assert!(v.headline.starts_with("Error: write failed"));
        assert!(v.causes[0].contains("No space left"));
        assert!(v.next_step.unwrap().contains("general.download_dir"));
    }

    #[test]
    fn paused_is_healthy_with_the_resume_hint() {
        let t = torrent(TorrentStatus::Paused);
        let v = diagnose(&ctx(&t, None));
        assert_eq!(v.severity, Severity::Healthy);
        assert_eq!(v.next_step.as_deref(), Some("Press p to resume"));
    }

    // -- discovery ------------------------------------------------------------

    #[test]
    fn proxied_torrent_with_only_stripped_udp_trackers_has_no_way_to_find_peers() {
        let mut t = torrent(TorrentStatus::Downloading);
        t.health.trackers = TrackerCounts {
            stripped_udp: 4,
            ..Default::default()
        };
        let n = NetworkHealth {
            listen_port: None,
            dht: None,
            ..Default::default()
        };
        let privacy = proxied();
        let mut c = ctx(&t, Some(&n));
        c.privacy = Some(&privacy);
        c.stalled_for = Some(Duration::from_secs(40));
        let v = diagnose(&c);
        assert_eq!(v.severity, Severity::Stalled);
        assert_eq!(v.headline, "No way to find peers");
        let text = joined(&v);
        assert!(text.contains("DHT is off (proxy lockdown)"), "{text}");
        assert!(
            text.contains("All 4 trackers were udp:// and stripped"),
            "{text}"
        );
        assert!(
            text.contains("privacy.proxy_url (restart required)"),
            "{text}"
        );
    }

    #[test]
    fn a_resolving_magnet_is_reported_at_session_level() {
        let n = network();
        assert_eq!(
            pending_add_line("debian.iso", 5, Some(&n)),
            "Resolving \"debian.iso\" for 5 s"
        );
        let line = pending_add_line("debian.iso", 45, Some(&n));
        assert!(line.contains("no peer has sent the metadata yet"), "{line}");
        assert!(!line.contains("DHT"), "{line}");
        let off = NetworkHealth {
            dht: None,
            ..network()
        };
        assert!(pending_add_line("x", 45, Some(&off)).ends_with("(DHT is off)"));
    }

    #[test]
    fn dht_disabled_by_config_names_the_key() {
        let mut t = torrent(TorrentStatus::Downloading);
        t.health.trackers = TrackerCounts {
            total: 2,
            failing: 2,
            ..Default::default()
        };
        let n = NetworkHealth {
            dht: None,
            ..network()
        };
        let mut c = ctx(&t, Some(&n));
        c.stalled_for = Some(Duration::from_secs(45));
        let v = diagnose(&c);
        assert_eq!(v.severity, Severity::Stalled);
        assert_eq!(v.headline, "No way to find peers");
        let text = joined(&v);
        assert!(text.contains("network.enable_dht = false"), "{text}");
        assert!(text.contains("Trackers: 2 — 2 failing"), "{text}");
        assert!(
            text.contains(
                "Every tracker is failing and DHT is disabled — set network.enable_dht = true (restart required)"
            ),
            "{text}"
        );
    }

    #[test]
    fn a_failing_http_tracker_with_no_dht_nodes_is_not_told_to_add_an_http_tracker() {
        // What a corporate network looks like: the only tracker answers 403
        // and DHT never bootstraps. The old hint suggested adding an http(s)
        // tracker — the one thing the torrent already had.
        let mut t = torrent(TorrentStatus::Downloading);
        t.health.trackers = TrackerCounts {
            total: 1,
            failing: 1,
            ..Default::default()
        };
        t.trackers = vec![TrackerInfo {
            url: "http://bttracker.debian.org:6969/announce".to_string(),
            status: TrackerStatus::Failing {
                last_error: "tracker responded with 403".to_string(),
                secs_ago: 16,
            },
        }];
        let n = NetworkHealth {
            dht: Some(DhtHealth::default()),
            ..network()
        };
        let mut c = ctx(&t, Some(&n));
        c.stalled_for = Some(Duration::from_secs(41));
        let v = diagnose(&c);
        assert_eq!(v.headline, "No way to find peers");
        let text = joined(&v);
        assert!(
            text.contains("bttracker.debian.org: tracker responded with 403"),
            "{text}"
        );
        let step = v.next_step.unwrap();
        assert!(step.starts_with("Every tracker is failing"), "{step}");
        assert!(!step.contains("add an http(s) tracker"), "{step}");
    }

    #[test]
    fn no_peers_inside_the_warmup_is_a_note_not_healthy() {
        let t = torrent(TorrentStatus::Downloading);
        let n = network();
        let mut c = ctx(&t, Some(&n));
        c.stalled_for = Some(Duration::from_secs(5));
        let v = diagnose(&c);
        assert_eq!(v.severity, Severity::Note);
        assert_eq!(v.headline, "Looking for peers");
        assert!(joined(&v).contains("DHT: 312 nodes"));
    }

    #[test]
    fn initializing_is_verifying_data_never_a_stall() {
        let mut t = torrent(TorrentStatus::FetchingMetadata);
        t.downloaded_bytes = 300 * 1024 * 1024;
        let n = network();
        let mut c = ctx(&t, Some(&n));
        // Even with a "stalled" clock, hash-checking is not a transfer.
        c.stalled_for = Some(Duration::from_secs(600));
        let v = diagnose(&c);
        assert_eq!(v.severity, Severity::Healthy);
        assert_eq!(
            v.headline,
            "Verifying existing data on disk — 300.0 MB checked"
        );
        assert!(!is_stalled(&t.status, c.stalled_for));
    }

    #[test]
    fn dht_bootstrapping_is_called_out_not_blamed_on_config() {
        let t = torrent(TorrentStatus::Downloading);
        let n = NetworkHealth {
            dht: Some(DhtHealth::default()),
            ..network()
        };
        let mut c = ctx(&t, Some(&n));
        c.stalled_for = Some(Duration::from_secs(31));
        let v = diagnose(&c);
        let text = joined(&v);
        assert!(text.contains("DHT has no nodes yet"), "{text}");
        assert!(!text.contains("enable_dht"), "{text}");
        assert!(text.contains("UDP is blocked"), "{text}");
    }

    // -- connectivity ---------------------------------------------------------

    #[test]
    fn peers_known_but_every_tcp_connect_failing_blames_the_network() {
        let mut t = torrent(TorrentStatus::Downloading);
        t.health.peers = PeerBreakdown {
            seen: 52,
            dead: 49,
            connecting: 3,
            ..Default::default()
        };
        let n = NetworkHealth {
            connect: ConnectHealth {
                tcp: TransportStats {
                    attempts: 214,
                    successes: 0,
                    errors: 214,
                },
                ..Default::default()
            },
            ..network()
        };
        let mut c = ctx(&t, Some(&n));
        c.stalled_for = Some(Duration::from_secs(90));
        let v = diagnose(&c);
        assert_eq!(v.severity, Severity::Stalled);
        assert_eq!(
            v.headline,
            "52 peers known, none reachable — nothing received for 1 min"
        );
        let text = joined(&v);
        assert!(
            text.contains("Every outgoing TCP connection failed (214 of 214)"),
            "{text}"
        );
        assert!(text.contains("firewall, VPN, or the ISP"), "{text}");
    }

    #[test]
    fn under_a_proxy_the_socks_counters_are_the_ones_judged() {
        let mut t = torrent(TorrentStatus::Downloading);
        t.health.peers = PeerBreakdown {
            seen: 10,
            dead: 10,
            ..Default::default()
        };
        let n = NetworkHealth {
            listen_port: None,
            dht: None,
            connect: ConnectHealth {
                socks: TransportStats {
                    attempts: 40,
                    successes: 0,
                    errors: 40,
                },
                // Healthy TCP numbers must not mask a dead proxy.
                tcp: TransportStats {
                    attempts: 40,
                    successes: 40,
                    errors: 0,
                },
                ..Default::default()
            },
            ..Default::default()
        };
        let privacy = proxied();
        let mut c = ctx(&t, Some(&n));
        c.privacy = Some(&privacy);
        c.stalled_for = Some(Duration::from_secs(60));
        let v = diagnose(&c);
        let text = joined(&v);
        assert!(
            text.contains("Every outgoing SOCKS5 connection failed"),
            "{text}"
        );
        assert!(text.contains("Is the proxy up?"), "{text}");
    }

    #[test]
    fn bound_interface_is_named_when_connects_fail() {
        let mut t = torrent(TorrentStatus::Downloading);
        t.health.peers = PeerBreakdown {
            seen: 30,
            dead: 30,
            ..Default::default()
        };
        let n = NetworkHealth {
            connect: ConnectHealth {
                tcp: TransportStats {
                    attempts: 30,
                    successes: 0,
                    errors: 30,
                },
                ..Default::default()
            },
            ..network()
        };
        let privacy = PrivacyStatus {
            proxy: false,
            bind_interface: Some("wg0".to_string()),
            blocklist_ranges: None,
        };
        let mut c = ctx(&t, Some(&n));
        c.privacy = Some(&privacy);
        c.stalled_for = Some(Duration::from_secs(31));
        let v = diagnose(&c);
        assert!(v.next_step.unwrap().contains("Is wg0 up and routed?"));
    }

    #[test]
    fn blocklist_rejecting_the_whole_swarm_names_the_blocklist() {
        let mut t = torrent(TorrentStatus::Downloading);
        t.health.peers = PeerBreakdown {
            seen: 20,
            dead: 5,
            ..Default::default()
        };
        let n = NetworkHealth {
            blocked_outgoing: 40,
            ..network()
        };
        let mut c = ctx(&t, Some(&n));
        c.stalled_for = Some(Duration::from_secs(31));
        let v = diagnose(&c);
        let text = joined(&v);
        assert!(
            text.contains("Blocklist rejected 40 outgoing connections"),
            "{text}"
        );
        assert!(text.contains("privacy.blocklist_url"), "{text}");
    }

    // -- starved --------------------------------------------------------------

    #[test]
    fn connected_but_starved_suspects_the_swarm_and_flags_unverified_data() {
        let mut t = torrent(TorrentStatus::Downloading);
        t.health.peers = PeerBreakdown {
            seen: 12,
            live: 8,
            not_needed: 6,
            dead: 1,
            connecting: 1,
            ..Default::default()
        };
        t.health.fetched_bytes = 300 * 1024 * 1024;
        t.health.checked_bytes = 200 * 1024 * 1024;
        let n = network();
        let mut c = ctx(&t, Some(&n));
        c.stalled_for = Some(Duration::from_secs(120));
        let v = diagnose(&c);
        assert_eq!(v.severity, Severity::Stalled);
        assert!(
            v.headline
                .starts_with("8 peers connected but nobody is sending"),
            "{}",
            v.headline
        );
        let text = joined(&v);
        assert!(
            text.contains("6 peers have nothing we still need"),
            "{text}"
        );
        assert!(
            text.contains("100.0 MB fetched but never verified"),
            "{text}"
        );
        assert!(text.contains("no seeders"), "{text}");
    }

    // -- flowing --------------------------------------------------------------

    #[test]
    fn capped_at_ninety_percent_of_the_limit_points_at_t() {
        let mut t = torrent(TorrentStatus::Downloading);
        t.download_speed = 450 * 1024; // exactly 90% of 500 KB/s
        t.health.peers.live = 30;
        let n = network();
        let mut c = ctx(&t, Some(&n));
        c.download_limit_kbps = 500;
        let v = diagnose(&c);
        assert_eq!(v.severity, Severity::Capped);
        assert!(
            v.headline.contains("500 KB/s download limit"),
            "{}",
            v.headline
        );
        assert_eq!(
            v.next_step.as_deref(),
            Some("Press t to raise it (applies live)")
        );

        // One byte under the line: not capped, just healthy.
        t.download_speed = 450 * 1024 - 1;
        let mut c = ctx(&t, Some(&n));
        c.download_limit_kbps = 500;
        assert_eq!(diagnose(&c).severity, Severity::Healthy);
    }

    #[test]
    fn thin_swarm_is_a_note_with_connectivity_evidence() {
        let mut t = torrent(TorrentStatus::Downloading);
        t.download_speed = 10_000;
        t.health.peers = PeerBreakdown {
            seen: 40,
            live: 2,
            dead: 30,
            ..Default::default()
        };
        let v = diagnose(&ctx(&t, Some(&network())));
        assert_eq!(v.severity, Severity::Note);
        assert_eq!(v.headline, "Only 2 of 40 known peers connected");
        assert!(v.causes[0].starts_with("40 known: 30 dead"));
    }

    #[test]
    fn healthy_verdict_quotes_the_freshest_tracker() {
        let mut t = torrent(TorrentStatus::Downloading);
        t.download_speed = 2_000_000;
        t.health.peers = PeerBreakdown {
            seen: 60,
            live: 42,
            ..Default::default()
        };
        t.health.avg_piece_ms = Some(800);
        t.trackers = vec![
            TrackerInfo {
                url: "https://old.example/announce".to_string(),
                status: TrackerStatus::Ok {
                    last_announce_secs_ago: 900,
                    next_in_secs: Some(100),
                },
            },
            TrackerInfo {
                url: "https://tracker.opentrackr.org:443/announce".to_string(),
                status: TrackerStatus::Ok {
                    last_announce_secs_ago: 180,
                    next_in_secs: Some(1620),
                },
            },
        ];
        let v = diagnose(&ctx(&t, Some(&network())));
        assert_eq!(v.severity, Severity::Healthy);
        assert_eq!(v.headline, "42 peers connected, avg piece 0.8 s");
        assert_eq!(
            v.causes,
            vec!["tracker.opentrackr.org announced 3 min ago (next in 27 min)".to_string()]
        );
    }

    #[test]
    fn utp_being_off_is_never_a_cause() {
        let mut t = torrent(TorrentStatus::Downloading);
        t.download_speed = 5_000;
        t.health.peers.live = 5;
        t.health.peers.seen = 5;
        let n = NetworkHealth {
            utp_enabled: false,
            ..network()
        };
        let text = joined(&diagnose(&ctx(&t, Some(&n))));
        assert!(!text.to_lowercase().contains("utp"), "{text}");
    }

    // -- seeding --------------------------------------------------------------

    #[test]
    fn seeding_under_a_proxy_explains_the_missing_listener() {
        let mut t = torrent(TorrentStatus::Seeding);
        t.upload_speed = 50_000;
        let n = NetworkHealth {
            listen_port: None,
            ..Default::default()
        };
        let privacy = proxied();
        let mut c = ctx(&t, Some(&n));
        c.privacy = Some(&privacy);
        let v = diagnose(&c);
        assert_eq!(v.severity, Severity::Note);
        assert!(v.headline.contains("without a listener"));
        assert!(v.next_step.unwrap().contains("privacy.proxy_url"));
    }

    #[test]
    fn seeding_notes_a_failed_upnp_mapping() {
        let mut t = torrent(TorrentStatus::Complete);
        t.health.peers.live = 0;
        let n = NetworkHealth {
            upnp: UpnpState::Failed("no gateway".to_string()),
            ..network()
        };
        let v = diagnose(&ctx(&t, Some(&n)));
        assert_eq!(v.severity, Severity::Note);
        assert_eq!(v.headline, "Complete — no peers connected");
        assert!(v.causes[0].contains("UPnP could not open port 6881: no gateway"));
    }

    #[test]
    fn upload_cap_is_reported_while_seeding() {
        let mut t = torrent(TorrentStatus::Seeding);
        t.upload_speed = 100 * 1024;
        let n = network();
        let mut c = ctx(&t, Some(&n));
        c.upload_limit_kbps = 100;
        let v = diagnose(&c);
        assert_eq!(v.severity, Severity::Capped);
        assert!(v.headline.contains("upload limit"));
    }

    // -- helpers --------------------------------------------------------------

    #[test]
    fn tracker_host_strips_scheme_port_and_path() {
        assert_eq!(
            tracker_host("https://tracker.opentrackr.org:443/announce"),
            "tracker.opentrackr.org"
        );
        assert_eq!(tracker_host("udp://x.example:1337"), "x.example");
        assert_eq!(tracker_host("http://y.example/a?b=c"), "y.example");
        assert_eq!(tracker_host("garbage"), "garbage");
    }

    #[test]
    fn fmt_secs_picks_the_largest_unit() {
        assert_eq!(fmt_secs(5), "5 s");
        assert_eq!(fmt_secs(90), "1 min");
        assert_eq!(fmt_secs(3_720), "1 h 2 min");
    }

    #[test]
    fn stall_marker_only_applies_to_transfers_in_progress() {
        let long = Some(Duration::from_secs(600));
        assert!(!is_stalled(&TorrentStatus::Paused, long));
        assert!(!is_stalled(&TorrentStatus::Complete, long));
        assert!(!is_stalled(&TorrentStatus::FetchingMetadata, long));
        assert!(!is_stalled(&TorrentStatus::Error("x".into()), long));
        assert!(is_stalled(&TorrentStatus::Downloading, long));
        assert!(is_stalled(&TorrentStatus::Downloading, Some(STALL_AFTER)));
        assert!(!is_stalled(
            &TorrentStatus::Downloading,
            Some(STALL_AFTER - Duration::from_millis(1))
        ));
        assert!(!is_stalled(&TorrentStatus::Downloading, None));
    }
}
