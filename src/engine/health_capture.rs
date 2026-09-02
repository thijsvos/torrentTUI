//! Tracker and UPnP health, overheard from librqbit's own tracing output.
//!
//! librqbit 9 exposes no tracker state through its API: `TrackerComms::start`
//! returns a bare stream of peer addresses and drops everything else — the
//! announce interval, the failure reason, the seeder count — before any caller
//! can see it. What it *does* do is log. Every per-tracker task runs inside a
//! span carrying `tracker = <url>` and `info_hash = <hex>`, and the announce
//! outcomes are `debug!`/`trace!` events inside those spans. This module is a
//! `tracing_subscriber::Layer` that listens for exactly those events and keeps
//! the last outcome per `(info hash, tracker)` in memory, so the Health tab can
//! say "announced 3 min ago" or "failing: connection refused".
//!
//! In memory only, by design. The on-disk log keeps its `torrenttui=warn`
//! default precisely so peer IPs, tracker URLs and info hashes never land on
//! disk unasked (#48); this layer never writes anywhere, and it does not
//! subscribe to peer-level events at all — tracker URLs and info hashes are
//! already in `session.json`, peer addresses are not.
//!
//! The message texts matched here are upstream's, pinned in [`MESSAGES`] and
//! by tests that replay them in the exact shape librqbit emits. A librqbit
//! bump that rewords one degrades that tracker to "pending" rather than
//! breaking anything — and fails the test so the table gets updated.

use crate::types::{TrackerCounts, TrackerStatus, UpnpState};
use crate::ui::util::{sanitize_display, truncate};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{field::Field, field::Visit, span, Event, Subscriber};
use tracing_subscriber::{filter::Targets, layer::Context, registry::LookupSpan, Layer};

/// The upstream message shapes this layer keys on, in one place. Prefixes are
/// matched with `starts_with`; the error text is whatever follows.
///
/// From `librqbit-tracker-comms-9.0.1/src/tracker_comms.rs` unless noted.
pub(crate) struct Messages {
    /// HTTP announce succeeded; the `{:?}` is the sleep `Duration`.
    pub http_ok_prefix: &'static str,
    pub http_ok_suffix: &'static str,
    /// HTTP announce failed (backoff `notify`).
    pub http_err_prefix: &'static str,
    /// UDP tracker hostname could not be resolved.
    pub udp_resolve_err_prefix: &'static str,
    /// UDP announce failed.
    pub udp_err_prefix: &'static str,
    /// UDP announce succeeded (`trace!`, inside the `udp request` span).
    pub udp_ok: &'static str,
    /// UDP monitor about to sleep; the `interval` field carries the
    /// `Option<Duration>` the tracker asked for.
    pub udp_sleep: &'static str,
    /// `librqbit-upnp-9.0.1/src/lib.rs`.
    pub upnp_ok: &'static str,
    pub upnp_err_prefixes: [&'static str; 3],
}

pub(crate) const MESSAGES: Messages = Messages {
    http_ok_prefix: "sleeping for ",
    http_ok_suffix: " after calling tracker",
    http_err_prefix: "error calling tracker: ",
    udp_resolve_err_prefix: "error resolving tracker: ",
    udp_err_prefix: "error reading announce response: ",
    udp_ok: "received announce response",
    udp_sleep: "sleeping",
    upnp_ok: "successfully port forwarded",
    upnp_err_prefixes: [
        "failed to forward port: ",
        "failed to run SSDP/UPNP discovery: ",
        "failed to determine local IP for endpoint at ",
    ],
};

/// Span names librqbit wraps each per-tracker monitor task in.
const TRACKER_SPANS: [&str; 2] = ["udp_tracker", "http_tracker"];

/// Bound on trackers remembered per torrent; a magnet can carry hundreds of
/// `tr=` parameters and nothing the UI shows needs more than this.
const MAX_TRACKERS_PER_HASH: usize = 64;
/// Bound on torrents remembered. Entries for torrents no longer in the session
/// are swept by [`HealthCapture::retain_hashes`]; this is the backstop for a
/// sweep that never comes.
const MAX_HASHES: usize = 10_000;
/// Longest error text kept per tracker. The Health tab shows one line.
const MAX_ERROR_CHARS: usize = 120;

/// Last known announce outcome for one tracker of one torrent.
#[derive(Debug, Clone, Default)]
pub struct TrackerRecord {
    last_ok: Option<Instant>,
    next_interval: Option<Duration>,
    last_error: Option<(Instant, String)>,
}

impl TrackerRecord {
    /// Classify against `now`. The last event wins: a failure after a success
    /// is `Failing`, a success after failures is `Ok`.
    fn status(&self, now: Instant) -> TrackerStatus {
        match (self.last_ok, &self.last_error) {
            (Some(ok), Some((err_at, err))) if *err_at > ok => TrackerStatus::Failing {
                last_error: err.clone(),
                secs_ago: now.saturating_duration_since(*err_at).as_secs(),
            },
            (Some(ok), _) => TrackerStatus::Ok {
                last_announce_secs_ago: now.saturating_duration_since(ok).as_secs(),
                next_in_secs: self.next_interval.map(|i| {
                    i.saturating_sub(now.saturating_duration_since(ok))
                        .as_secs()
                }),
            },
            (None, Some((err_at, err))) => TrackerStatus::Failing {
                last_error: err.clone(),
                secs_ago: now.saturating_duration_since(*err_at).as_secs(),
            },
            (None, None) => TrackerStatus::Pending,
        }
    }
}

#[derive(Default)]
struct State {
    /// info hash (40-char lowercase hex) → tracker URL → record.
    trackers: HashMap<String, HashMap<String, TrackerRecord>>,
    /// info hash → `udp://` trackers the proxy lockdown removed at add time.
    stripped_udp: HashMap<String, usize>,
    upnp: UpnpState,
}

/// The shared store: written by [`HealthLayer`] from librqbit's tasks, read by
/// the engine when it builds snapshots. Locks are held for a few map
/// operations; nothing inside ever logs (a `tracing` call from inside a layer
/// re-enters the subscriber).
#[derive(Default)]
pub struct HealthCapture {
    state: Mutex<State>,
}

impl HealthCapture {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // A panic while holding the lock is the only way to poison it, and
        // `panic = "abort"` means we never observe that in release; in debug
        // builds prefer the stale data over a second panic.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Record one tracker event. `message` is the event's `message` field,
    /// `interval` the `interval` field when present (UDP sleep line).
    pub(crate) fn record_tracker_event(
        &self,
        hash: &str,
        url: &str,
        message: &str,
        interval: Option<&str>,
        now: Instant,
    ) {
        let m = &MESSAGES;
        enum Outcome {
            Ok(Option<Duration>),
            Sleep(Option<Duration>),
            Err(String),
        }
        let outcome = if let Some(rest) = message.strip_prefix(m.http_ok_prefix) {
            let dur = rest
                .strip_suffix(m.http_ok_suffix)
                .and_then(parse_debug_duration);
            Outcome::Ok(dur)
        } else if message == m.udp_ok {
            Outcome::Ok(None)
        } else if message == m.udp_sleep {
            Outcome::Sleep(interval.and_then(parse_debug_duration))
        } else if let Some(err) = message
            .strip_prefix(m.http_err_prefix)
            .or_else(|| message.strip_prefix(m.udp_resolve_err_prefix))
            .or_else(|| message.strip_prefix(m.udp_err_prefix))
        {
            Outcome::Err(truncate(&sanitize_display(err), MAX_ERROR_CHARS))
        } else {
            return;
        };

        let mut state = self.lock();
        if !state.trackers.contains_key(hash) && state.trackers.len() >= MAX_HASHES {
            return;
        }
        let per_torrent = state.trackers.entry(hash.to_string()).or_default();
        if !per_torrent.contains_key(url) && per_torrent.len() >= MAX_TRACKERS_PER_HASH {
            return;
        }
        let record = per_torrent.entry(url.to_string()).or_default();
        match outcome {
            Outcome::Ok(dur) => {
                record.last_ok = Some(now);
                if dur.is_some() {
                    record.next_interval = dur;
                }
            }
            // The UDP monitor logs the sleep right after a successful
            // announce, so this only refines an `Ok` that already landed.
            Outcome::Sleep(dur) => {
                if dur.is_some() {
                    record.next_interval = dur;
                }
            }
            Outcome::Err(text) => record.last_error = Some((now, text)),
        }
    }

    pub(crate) fn record_upnp_event(&self, message: &str) {
        let m = &MESSAGES;
        let next = if message == m.upnp_ok {
            UpnpState::Forwarded
        } else if let Some(prefix) = m.upnp_err_prefixes.iter().find(|p| message.starts_with(*p)) {
            let err = &message[prefix.len()..];
            UpnpState::Failed(truncate(&sanitize_display(err), MAX_ERROR_CHARS))
        } else {
            return;
        };
        self.lock().upnp = next;
    }

    /// The engine calls this when UPnP is enabled, so "no news" reads as
    /// pending rather than off.
    pub fn mark_upnp_pending(&self) {
        let mut state = self.lock();
        if state.upnp == UpnpState::Off {
            state.upnp = UpnpState::Pending;
        }
    }

    pub fn upnp(&self) -> UpnpState {
        self.lock().upnp.clone()
    }

    /// Remember how many `udp://` trackers were stripped from a magnet before
    /// librqbit saw it, so the doctor can explain a torrent that has "no
    /// trackers" under a proxy.
    pub fn note_stripped_udp(&self, hash: &str, removed: usize) {
        if removed == 0 {
            return;
        }
        let mut state = self.lock();
        if state.stripped_udp.len() < MAX_HASHES {
            state.stripped_udp.insert(hash.to_string(), removed);
        }
    }

    /// Status of one tracker of one torrent. `scheme` decides the two states
    /// no event can tell us about: a scheme librqbit refuses, and a `udp://`
    /// tracker announcing around a proxy.
    pub fn tracker_status(
        &self,
        hash: &str,
        url: &str,
        scheme: &str,
        proxied: bool,
        now: Instant,
    ) -> TrackerStatus {
        match scheme {
            "http" | "https" => {}
            "udp" if proxied => return TrackerStatus::BypassesProxy,
            "udp" => {}
            _ => return TrackerStatus::Unsupported,
        }
        let state = self.lock();
        state
            .trackers
            .get(hash)
            .and_then(|t| t.get(url))
            .map(|r| r.status(now))
            .unwrap_or(TrackerStatus::Pending)
    }

    /// Roll-up for one torrent over its tracker list — the cheap form the
    /// engine fills for every torrent every tick.
    pub fn tracker_counts<'a>(
        &self,
        hash: &str,
        trackers: impl Iterator<Item = (&'a str, &'a str)>,
        proxied: bool,
        now: Instant,
    ) -> TrackerCounts {
        let state = self.lock();
        let records = state.trackers.get(hash);
        let mut counts = TrackerCounts {
            stripped_udp: state.stripped_udp.get(hash).copied().unwrap_or(0),
            ..Default::default()
        };
        for (url, scheme) in trackers {
            counts.total += 1;
            match scheme {
                "http" | "https" | "udp" if !(scheme == "udp" && proxied) => {
                    match records.and_then(|t| t.get(url)).map(|r| r.status(now)) {
                        Some(TrackerStatus::Ok { .. }) => counts.ok += 1,
                        Some(TrackerStatus::Failing { .. }) => counts.failing += 1,
                        _ => counts.pending += 1,
                    }
                }
                "udp" => counts.bypassing_proxy += 1,
                _ => counts.unsupported += 1,
            }
        }
        counts
    }

    /// Drop records for torrents no longer in the session.
    pub fn retain_hashes(&self, keep: &HashSet<String>) {
        let mut state = self.lock();
        state.trackers.retain(|h, _| keep.contains(h));
        state.stripped_udp.retain(|h, _| keep.contains(h));
    }
}

/// Parse `Duration`'s `Debug` output (`1800s`, `1.5s`, `500ms`, `250µs`,
/// `10ns`), optionally wrapped in `Some(…)` — the two shapes librqbit's
/// tracker events carry it in.
pub(crate) fn parse_debug_duration(text: &str) -> Option<Duration> {
    let text = text.trim();
    let text = text
        .strip_prefix("Some(")
        .and_then(|t| t.strip_suffix(')'))
        .unwrap_or(text);
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let value: f64 = number.parse().ok()?;
    let scale = match unit {
        "s" => 1.0,
        "ms" => 1e-3,
        "µs" | "us" => 1e-6,
        "ns" => 1e-9,
        _ => return None,
    };
    Duration::try_from_secs_f64(value * scale).ok()
}

/// Fields of a tracker span, stored in the span's extensions on creation so
/// events inside it can be attributed without re-parsing anything.
#[derive(Debug)]
struct TrackerSpan {
    hash: String,
    url: String,
}

#[derive(Default)]
struct SpanFields {
    tracker: Option<String>,
    info_hash: Option<String>,
}

impl Visit for SpanFields {
    // `tracker = %url` and `info_hash = ?id` both arrive here: `%` wraps the
    // value in a Debug impl that forwards to Display, and `Id20`'s Debug is
    // its bare 40-char hex — the same text `Id20::as_string` gives the engine.
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "tracker" => self.tracker = Some(format!("{value:?}")),
            "info_hash" => self.info_hash = Some(format!("{value:?}")),
            _ => {}
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "tracker" => self.tracker = Some(value.to_string()),
            "info_hash" => self.info_hash = Some(value.to_string()),
            _ => {}
        }
    }
}

#[derive(Default)]
struct EventFields {
    message: Option<String>,
    interval: Option<String>,
}

impl Visit for EventFields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "message" => self.message = Some(format!("{value:?}")),
            "interval" => self.interval = Some(format!("{value:?}")),
            _ => {}
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "message" => self.message = Some(value.to_string()),
            "interval" => self.interval = Some(value.to_string()),
            _ => {}
        }
    }
}

/// The layer. Attach with [`HealthLayer::targets`] as its per-layer filter so
/// it sees tracker and UPnP events regardless of the file log's filter — and
/// nothing else, in particular no peer-level events.
pub struct HealthLayer {
    capture: Arc<HealthCapture>,
}

impl HealthLayer {
    pub fn new(capture: Arc<HealthCapture>) -> Self {
        Self { capture }
    }

    /// Per-layer filter: tracker events down to TRACE (the UDP success line
    /// is a `trace!`), UPnP at DEBUG. Targets are the sub-crates' *lib* names,
    /// which is why `librqbit=debug` in a plain `RUST_LOG` never shows these.
    pub fn targets() -> Targets {
        Targets::new()
            .with_target("librqbit_tracker_comms", tracing::Level::TRACE)
            .with_target("librqbit_upnp", tracing::Level::DEBUG)
    }
}

impl<S> Layer<S> for HealthLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &span::Attributes<'_>, id: &span::Id, ctx: Context<'_, S>) {
        if !TRACKER_SPANS.contains(&attrs.metadata().name()) {
            return;
        }
        let mut fields = SpanFields::default();
        attrs.record(&mut fields);
        let (Some(hash), Some(url)) = (fields.info_hash, fields.tracker) else {
            return;
        };
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(TrackerSpan { hash, url });
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let mut fields = EventFields::default();
        event.record(&mut fields);
        let Some(message) = fields.message else {
            return;
        };

        if event.metadata().target().starts_with("librqbit_upnp") {
            self.capture.record_upnp_event(&message);
            return;
        }

        // Walk outward from the event's span: the UDP announce lines sit in a
        // `udp request` span nested inside the tracker span.
        let Some(scope) = ctx.event_scope(event) else {
            return;
        };
        for span in scope {
            let ext = span.extensions();
            if let Some(t) = ext.get::<TrackerSpan>() {
                self.capture.record_tracker_event(
                    &t.hash,
                    &t.url,
                    &message,
                    fields.interval.as_deref(),
                    Instant::now(),
                );
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::prelude::*;

    const HASH: &str = "8337c196d4536e9af5d2c7e599f0f1b7d71eee54";
    const URL: &str = "https://tracker.example/announce";
    const TARGET: &str = "librqbit_tracker_comms::tracker_comms";

    /// Stands in for `Id20`: Debug prints bare hex, no quotes.
    struct Hex(&'static str);
    impl std::fmt::Debug for Hex {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.0)
        }
    }

    fn with_layer(f: impl FnOnce()) -> Arc<HealthCapture> {
        let capture = HealthCapture::new();
        let subscriber = tracing_subscriber::registry()
            .with(HealthLayer::new(capture.clone()).with_filter(HealthLayer::targets()));
        tracing::subscriber::with_default(subscriber, f);
        capture
    }

    // -- the parser -----------------------------------------------------------

    #[test]
    fn parses_every_duration_debug_shape() {
        assert_eq!(
            parse_debug_duration("1800s"),
            Some(Duration::from_secs(1800))
        );
        assert_eq!(
            parse_debug_duration("1.5s"),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(
            parse_debug_duration("500ms"),
            Some(Duration::from_millis(500))
        );
        assert_eq!(
            parse_debug_duration("250µs"),
            Some(Duration::from_micros(250))
        );
        assert_eq!(parse_debug_duration("10ns"), Some(Duration::from_nanos(10)));
        assert_eq!(
            parse_debug_duration("Some(1800s)"),
            Some(Duration::from_secs(1800))
        );
        assert_eq!(parse_debug_duration("None"), None);
        assert_eq!(parse_debug_duration("1800"), None);
        assert_eq!(parse_debug_duration("-5s"), None);
        assert_eq!(parse_debug_duration(""), None);
    }

    // -- the store ------------------------------------------------------------

    #[test]
    fn http_success_records_ok_with_the_interval() {
        let cap = HealthCapture::default();
        let t0 = Instant::now();
        cap.record_tracker_event(
            HASH,
            URL,
            "sleeping for 1800s after calling tracker",
            None,
            t0,
        );
        let status = cap.tracker_status(HASH, URL, "https", false, t0 + Duration::from_secs(60));
        assert_eq!(
            status,
            TrackerStatus::Ok {
                last_announce_secs_ago: 60,
                next_in_secs: Some(1740),
            }
        );
    }

    #[test]
    fn failure_after_success_is_failing_and_success_after_failure_is_ok() {
        let cap = HealthCapture::default();
        let t0 = Instant::now();
        cap.record_tracker_event(
            HASH,
            URL,
            "sleeping for 60s after calling tracker",
            None,
            t0,
        );
        cap.record_tracker_event(
            HASH,
            URL,
            "error calling tracker: connection refused",
            None,
            t0 + Duration::from_secs(70),
        );
        let t1 = t0 + Duration::from_secs(75);
        assert_eq!(
            cap.tracker_status(HASH, URL, "https", false, t1),
            TrackerStatus::Failing {
                last_error: "connection refused".to_string(),
                secs_ago: 5,
            }
        );
        cap.record_tracker_event(
            HASH,
            URL,
            "sleeping for 60s after calling tracker",
            None,
            t0 + Duration::from_secs(80),
        );
        assert!(matches!(
            cap.tracker_status(HASH, URL, "https", false, t0 + Duration::from_secs(81)),
            TrackerStatus::Ok { .. }
        ));
    }

    #[test]
    fn udp_success_then_sleep_line_fills_in_the_interval() {
        let cap = HealthCapture::default();
        let t0 = Instant::now();
        let udp = "udp://tracker.example:1337/announce";
        cap.record_tracker_event(HASH, udp, "received announce response", None, t0);
        cap.record_tracker_event(HASH, udp, "sleeping", Some("Some(900s)"), t0);
        assert_eq!(
            cap.tracker_status(HASH, udp, "udp", false, t0 + Duration::from_secs(100)),
            TrackerStatus::Ok {
                last_announce_secs_ago: 100,
                next_in_secs: Some(800),
            }
        );
    }

    #[test]
    fn udp_errors_and_resolve_errors_are_failing() {
        let cap = HealthCapture::default();
        let t0 = Instant::now();
        let udp = "udp://tracker.example:1337/announce";
        cap.record_tracker_event(
            HASH,
            udp,
            "error resolving tracker: failed to lookup address information",
            None,
            t0,
        );
        assert!(matches!(
            cap.tracker_status(HASH, udp, "udp", false, t0),
            TrackerStatus::Failing { ref last_error, .. } if last_error.starts_with("failed to lookup")
        ));
        cap.record_tracker_event(
            HASH,
            udp,
            "error reading announce response: timed out",
            None,
            t0,
        );
        assert!(matches!(
            cap.tracker_status(HASH, udp, "udp", false, t0),
            TrackerStatus::Failing { ref last_error, .. } if last_error == "timed out"
        ));
    }

    #[test]
    fn error_text_is_sanitized_and_truncated() {
        let cap = HealthCapture::default();
        let t0 = Instant::now();
        let long = format!("error calling tracker: \u{1b}[31m{}", "x".repeat(500));
        cap.record_tracker_event(HASH, URL, &long, None, t0);
        match cap.tracker_status(HASH, URL, "https", false, t0) {
            TrackerStatus::Failing { last_error, .. } => {
                assert!(!last_error.contains('\u{1b}'));
                assert!(last_error.chars().count() <= MAX_ERROR_CHARS);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn unrelated_messages_leave_no_record() {
        let cap = HealthCapture::default();
        cap.record_tracker_event(HASH, URL, "starting monitor", None, Instant::now());
        assert_eq!(
            cap.tracker_status(HASH, URL, "https", false, Instant::now()),
            TrackerStatus::Pending
        );
    }

    #[test]
    fn scheme_decides_unsupported_and_proxy_bypass() {
        let cap = HealthCapture::default();
        let now = Instant::now();
        assert_eq!(
            cap.tracker_status(HASH, "wss://x.example", "wss", false, now),
            TrackerStatus::Unsupported
        );
        assert_eq!(
            cap.tracker_status(HASH, "udp://x.example:1", "udp", true, now),
            TrackerStatus::BypassesProxy
        );
        assert_eq!(
            cap.tracker_status(HASH, "udp://x.example:1", "udp", false, now),
            TrackerStatus::Pending
        );
    }

    #[test]
    fn counts_roll_up_every_bucket() {
        let cap = HealthCapture::default();
        let t0 = Instant::now();
        cap.record_tracker_event(
            HASH,
            "https://ok.example/a",
            "sleeping for 1s after calling tracker",
            None,
            t0,
        );
        cap.record_tracker_event(
            HASH,
            "https://bad.example/a",
            "error calling tracker: nope",
            None,
            t0,
        );
        cap.note_stripped_udp(HASH, 3);
        let trackers = [
            ("https://ok.example/a", "https"),
            ("https://bad.example/a", "https"),
            ("http://new.example/a", "http"),
            ("wss://ws.example/a", "wss"),
            ("udp://leak.example:1/a", "udp"),
        ];
        let counts = cap.tracker_counts(HASH, trackers.iter().copied(), true, t0);
        assert_eq!(
            counts,
            TrackerCounts {
                total: 5,
                ok: 1,
                failing: 1,
                pending: 1,
                unsupported: 1,
                bypassing_proxy: 1,
                stripped_udp: 3,
            }
        );
        // Same list, no proxy: the udp tracker is just pending.
        let counts = cap.tracker_counts(HASH, trackers.iter().copied(), false, t0);
        assert_eq!(counts.bypassing_proxy, 0);
        assert_eq!(counts.pending, 2);
    }

    #[test]
    fn retain_drops_forgotten_torrents_and_bounds_hold() {
        let cap = HealthCapture::default();
        let t0 = Instant::now();
        cap.record_tracker_event(HASH, URL, "received announce response", None, t0);
        cap.note_stripped_udp(HASH, 1);
        cap.retain_hashes(&HashSet::new());
        assert_eq!(
            cap.tracker_status(HASH, URL, "https", false, t0),
            TrackerStatus::Pending
        );
        assert_eq!(
            cap.tracker_counts(HASH, std::iter::empty(), false, t0)
                .stripped_udp,
            0
        );

        for i in 0..(MAX_TRACKERS_PER_HASH + 5) {
            cap.record_tracker_event(
                HASH,
                &format!("https://t{i}.example/a"),
                "received announce response",
                None,
                t0,
            );
        }
        assert_eq!(cap.lock().trackers[HASH].len(), MAX_TRACKERS_PER_HASH);
    }

    #[test]
    fn upnp_events_move_the_state() {
        let cap = HealthCapture::default();
        assert_eq!(cap.upnp(), UpnpState::Off);
        cap.mark_upnp_pending();
        assert_eq!(cap.upnp(), UpnpState::Pending);
        cap.record_upnp_event("failed to forward port: no gateway found");
        assert_eq!(
            cap.upnp(),
            UpnpState::Failed("no gateway found".to_string())
        );
        cap.record_upnp_event("successfully port forwarded");
        assert_eq!(cap.upnp(), UpnpState::Forwarded);
        // Pending never overrides a real result.
        cap.mark_upnp_pending();
        assert_eq!(cap.upnp(), UpnpState::Forwarded);
    }

    // -- the layer, fed events in librqbit's exact shape ----------------------

    #[test]
    fn layer_attributes_http_events_to_the_tracker_span() {
        let cap = with_layer(|| {
            let span = tracing::debug_span!(
                target: "librqbit_tracker_comms::tracker_comms",
                parent: None,
                "http_tracker",
                tracker = %URL,
                info_hash = ?Hex(HASH)
            );
            let _g = span.enter();
            tracing::debug!(
                target: TARGET,
                "sleeping for {:?} after calling tracker",
                Duration::from_secs(1800)
            );
        });
        assert!(matches!(
            cap.tracker_status(HASH, URL, "https", false, Instant::now()),
            TrackerStatus::Ok {
                next_in_secs: Some(n),
                ..
            } if n >= 1795
        ));
    }

    #[test]
    fn layer_finds_the_tracker_span_through_a_nested_udp_request_span() {
        let udp = "udp://tracker.example:1337/announce";
        let cap = with_layer(|| {
            let span = tracing::debug_span!(
                target: TARGET,
                parent: None,
                "udp_tracker",
                tracker = %udp,
                info_hash = ?Hex(HASH)
            );
            let _g = span.enter();
            {
                let inner =
                    tracing::trace_span!(target: TARGET, "udp request", addr = ?"1.2.3.4:1337");
                let _i = inner.enter();
                tracing::trace!(target: TARGET, len = 12usize, "received announce response");
            }
            tracing::trace!(target: TARGET, interval = ?Some(Duration::from_secs(900)), "sleeping");
        });
        assert!(matches!(
            cap.tracker_status(HASH, udp, "udp", false, Instant::now()),
            TrackerStatus::Ok {
                next_in_secs: Some(n),
                ..
            } if n >= 895
        ));
    }

    #[test]
    fn layer_records_failures_with_the_retry_field_present() {
        let cap = with_layer(|| {
            let span = tracing::debug_span!(
                target: TARGET,
                parent: None,
                "http_tracker",
                tracker = %URL,
                info_hash = ?Hex(HASH)
            );
            let _g = span.enter();
            let err = anyhow::anyhow!("connection refused").context("announcing");
            tracing::debug!(target: TARGET, retry_in = ?Duration::from_secs(10), "error calling tracker: {err:#}");
        });
        assert_eq!(
            cap.tracker_status(HASH, URL, "https", false, Instant::now()),
            TrackerStatus::Failing {
                last_error: "announcing: connection refused".to_string(),
                secs_ago: 0,
            }
        );
    }

    #[test]
    fn layer_ignores_events_outside_tracker_spans_and_other_targets() {
        let cap = with_layer(|| {
            tracing::debug!(target: TARGET, "sleeping for 5s after calling tracker");
            let span = tracing::debug_span!(target: "torrenttui", parent: None, "not_a_tracker", tracker = %URL, info_hash = ?Hex(HASH));
            let _g = span.enter();
            tracing::debug!(target: "torrenttui", "sleeping for 5s after calling tracker");
        });
        assert_eq!(
            cap.tracker_status(HASH, URL, "https", false, Instant::now()),
            TrackerStatus::Pending
        );
    }

    #[test]
    fn layer_picks_up_upnp_outcomes() {
        let cap = with_layer(|| {
            tracing::warn!(target: "librqbit_upnp", "failed to run SSDP/UPNP discovery: timed out");
        });
        assert_eq!(cap.upnp(), UpnpState::Failed("timed out".to_string()));
        let cap = with_layer(|| {
            tracing::debug!(target: "librqbit_upnp", local_ip = %"10.0.0.2", port = 6881u16, "successfully port forwarded");
        });
        assert_eq!(cap.upnp(), UpnpState::Forwarded);
    }
}
