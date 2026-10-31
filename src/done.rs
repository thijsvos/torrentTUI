//! `--quit-when-done`: deciding, from what the engine pushes, that a session
//! has nothing left to download.
//!
//! "Done" means nothing is still moving: no magnet is waiting for metadata and
//! no torrent is downloading or checking. A paused torrent counts as settled —
//! it will not finish on its own, so waiting for it would wait forever — and so
//! does one in error. Whether the run *succeeded* is a separate question, which
//! [`Outcome::succeeded`] answers for the exit status.
//!
//! Pure on purpose: the TUI and the headless loop feed it the same snapshots,
//! and every rule below is a unit test.

use crate::types::{NetworkHealth, TorrentInfo, TorrentStatus};

/// Health pushes in a row that must find nothing moving before the session
/// counts as done. The engine pushes once a second, so this is about three
/// seconds: room for an add sent at startup to register as pending, and for
/// torrents restored from the last run to start their initial check.
const CALM_PUSHES: u32 = 3;

/// Counts the health pushes that found nothing moving.
#[derive(Debug, Default)]
pub struct DoneWatch {
    calm: u32,
}

impl DoneWatch {
    /// Feed one health push, with the torrents as they stand. Returns the
    /// outcome once the session has been calm for [`CALM_PUSHES`] pushes.
    pub fn observe(&mut self, torrents: &[TorrentInfo], health: &NetworkHealth) -> Option<Outcome> {
        let moving = !health.pending_adds.is_empty()
            || torrents.iter().any(|t| {
                matches!(
                    t.status,
                    TorrentStatus::Downloading | TorrentStatus::FetchingMetadata
                )
            });
        if moving {
            self.calm = 0;
            return None;
        }
        self.calm += 1;
        (self.calm >= CALM_PUSHES).then(|| Outcome::of(torrents, health))
    }

    /// Start counting again — for a moment the user is still deciding
    /// something, like choosing files in the add picker, when an empty
    /// session must not be mistaken for a finished one.
    pub fn hold(&mut self) {
        self.calm = 0;
    }
}

/// How a `--quit-when-done` run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub finished: usize,
    /// Paused before they finished.
    pub paused: usize,
    /// Names of the torrents in error.
    pub errors: Vec<String>,
    /// Adds that ended without a torrent: refused, or a magnet given up on.
    pub failed_adds: u64,
}

impl Outcome {
    fn of(torrents: &[TorrentInfo], health: &NetworkHealth) -> Self {
        let mut out = Outcome {
            finished: 0,
            paused: 0,
            errors: Vec::new(),
            failed_adds: health.failed_adds,
        };
        for t in torrents {
            match t.status {
                TorrentStatus::Error(_) => out.errors.push(t.name.clone()),
                TorrentStatus::Paused if t.downloaded_bytes < t.size_bytes => out.paused += 1,
                _ => out.finished += 1,
            }
        }
        out
    }

    /// Nothing failed. Paused torrents do not count against it: pausing was
    /// the user's choice, not a failure.
    pub fn succeeded(&self) -> bool {
        self.errors.is_empty() && self.failed_adds == 0
    }

    /// One line for the terminal, or the log of a headless run.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.finished > 0 {
            parts.push(format!("{} finished", self.finished));
        }
        if self.paused > 0 {
            parts.push(format!("{} paused before finishing", self.paused));
        }
        if !self.errors.is_empty() {
            parts.push(format!(
                "{} failed ({})",
                self.errors.len(),
                self.errors.join(", ")
            ));
        }
        if self.failed_adds > 0 {
            parts.push(format!(
                "{} could not be added (see the log)",
                self.failed_adds
            ));
        }
        if parts.is_empty() {
            return "Nothing to download.".to_string();
        }
        format!("Nothing left to download: {}.", parts.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{PendingAdd, TorrentHealth};

    fn torrent(name: &str, status: TorrentStatus, done: u64) -> TorrentInfo {
        TorrentInfo {
            id: 0,
            name: name.to_string(),
            size_bytes: 100,
            downloaded_bytes: done,
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

    fn calm_for(
        watch: &mut DoneWatch,
        torrents: &[TorrentInfo],
        health: &NetworkHealth,
    ) -> Option<Outcome> {
        let mut last = None;
        for _ in 0..CALM_PUSHES {
            last = watch.observe(torrents, health);
        }
        last
    }

    #[test]
    fn done_needs_several_calm_pushes_in_a_row() {
        let mut watch = DoneWatch::default();
        let seeding = [torrent("a", TorrentStatus::Seeding, 100)];
        let health = NetworkHealth::default();
        for _ in 1..CALM_PUSHES {
            assert!(watch.observe(&seeding, &health).is_none());
        }
        let outcome = watch.observe(&seeding, &health).expect("done");
        assert_eq!(outcome.finished, 1);
        assert!(outcome.succeeded());
    }

    #[test]
    fn a_download_or_a_check_in_progress_is_not_done() {
        let health = NetworkHealth::default();
        for status in [TorrentStatus::Downloading, TorrentStatus::FetchingMetadata] {
            let mut watch = DoneWatch::default();
            let torrents = [
                torrent("a", TorrentStatus::Seeding, 100),
                torrent("b", status, 10),
            ];
            assert!(calm_for(&mut watch, &torrents, &health).is_none());
        }
    }

    #[test]
    fn a_magnet_still_resolving_is_not_done() {
        let mut watch = DoneWatch::default();
        let health = NetworkHealth {
            pending_adds: vec![PendingAdd::default()],
            ..Default::default()
        };
        assert!(calm_for(&mut watch, &[], &health).is_none());
    }

    #[test]
    fn movement_restarts_the_count() {
        let mut watch = DoneWatch::default();
        let health = NetworkHealth::default();
        let seeding = [torrent("a", TorrentStatus::Seeding, 100)];
        let downloading = [torrent("a", TorrentStatus::Downloading, 50)];
        for _ in 1..CALM_PUSHES {
            watch.observe(&seeding, &health);
        }
        watch.observe(&downloading, &health);
        for _ in 1..CALM_PUSHES {
            assert!(watch.observe(&seeding, &health).is_none());
        }
        assert!(watch.observe(&seeding, &health).is_some());
    }

    #[test]
    fn holding_restarts_the_count() {
        let mut watch = DoneWatch::default();
        let health = NetworkHealth::default();
        for _ in 1..CALM_PUSHES {
            watch.observe(&[], &health);
        }
        watch.hold();
        assert!(watch.observe(&[], &health).is_none());
    }

    #[test]
    fn paused_torrents_settle_but_do_not_fail_the_run() {
        let mut watch = DoneWatch::default();
        let torrents = [
            torrent("a", TorrentStatus::Seeding, 100),
            torrent("b", TorrentStatus::Paused, 40),
            // Finished, then paused: that is a finished torrent.
            torrent("c", TorrentStatus::Paused, 100),
        ];
        let outcome = calm_for(&mut watch, &torrents, &NetworkHealth::default()).expect("done");
        assert_eq!(outcome.finished, 2);
        assert_eq!(outcome.paused, 1);
        assert!(outcome.succeeded());
        assert_eq!(
            outcome.summary(),
            "Nothing left to download: 2 finished, 1 paused before finishing."
        );
    }

    #[test]
    fn an_error_or_a_failed_add_fails_the_run() {
        let mut watch = DoneWatch::default();
        let torrents = [torrent("bad", TorrentStatus::Error("disk full".into()), 10)];
        let outcome = calm_for(&mut watch, &torrents, &NetworkHealth::default()).expect("done");
        assert!(!outcome.succeeded());
        assert_eq!(
            outcome.summary(),
            "Nothing left to download: 1 failed (bad)."
        );

        // A magnet nobody seeded never becomes a torrent; only the count
        // remembers it.
        let mut watch = DoneWatch::default();
        let health = NetworkHealth {
            failed_adds: 1,
            ..Default::default()
        };
        let outcome = calm_for(&mut watch, &[], &health).expect("done");
        assert!(!outcome.succeeded());
        assert_eq!(
            outcome.summary(),
            "Nothing left to download: 1 could not be added (see the log)."
        );
    }

    #[test]
    fn an_empty_session_is_done_with_nothing_to_report() {
        let mut watch = DoneWatch::default();
        let outcome = calm_for(&mut watch, &[], &NetworkHealth::default()).expect("done");
        assert!(outcome.succeeded());
        assert_eq!(outcome.summary(), "Nothing to download.");
    }
}
