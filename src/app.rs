use std::collections::{HashMap, HashSet};

use crate::types::{AppMode, DetailTab, SortColumn, TorrentInfo, TorrentStatus};
use ratatui::widgets::TableState;

pub struct App {
    pub torrents: Vec<TorrentInfo>,
    pub table_state: TableState,
    pub selected_index: usize,
    pub selected_torrent_id: Option<usize>,
    pub mode: AppMode,
    pub detail_tab: DetailTab,
    pub sort_column: SortColumn,
    pub sort_reversed: bool,
    pub error_message: Option<String>,
    pub error_timer: Option<std::time::Instant>,
    pub info_message: Option<String>,
    pub info_timer: Option<std::time::Instant>,
    pub spinner_tick: usize,
    pub should_quit: bool,
    pub filter_text: String,
    pub free_disk_space: Option<u64>,
    pub disk_space_timer: Option<std::time::Instant>,
    pub throttle_step: u8, // 0 = download, 1 = upload
    pub throttle_input_buf: String,
    pub throttle_download_value: u64,
    pub throttle_upload_value: u64,
    pub speed_limit_download_kbps: u64,
    pub speed_limit_upload_kbps: u64,
    pub table_area: Option<ratatui::layout::Rect>,
    pub detail_file_index: usize,
    pub deselected_files: HashMap<usize, HashSet<usize>>,
    pub marked_ids: HashSet<usize>,
    pub detail_peer_index: usize,
    /// Top peer-row index currently visible in the Peers tab. The renderer
    /// updates this each frame to keep `detail_peer_index` in view; handlers
    /// only mutate the index.
    pub detail_peer_scroll_offset: usize,
    /// Cached order of indices into `self.torrents` after applying the
    /// current filter and sort. Rebuilt when `sort_dirty` is true. The cache
    /// exists because `sorted_torrents()` is called multiple times per frame
    /// (table, detail, key handlers) and the lowercase-for-filter + sort
    /// work dominated CPU at hundreds of torrents.
    sort_cache: Vec<usize>,
    sort_dirty: bool,
    /// Wired from `config.general.confirm_on_quit`; when false, `q` quits
    /// immediately instead of opening the confirmation dialog.
    pub confirm_on_quit: bool,
}

impl App {
    pub fn new() -> Self {
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        Self {
            torrents: Vec::new(),
            table_state,
            selected_index: 0,
            selected_torrent_id: None,
            mode: AppMode::Normal,
            detail_tab: DetailTab::Stats,
            sort_column: SortColumn::Index,
            sort_reversed: false,
            error_message: None,
            error_timer: None,
            info_message: None,
            info_timer: None,
            spinner_tick: 0,
            should_quit: false,
            filter_text: String::new(),
            free_disk_space: None,
            disk_space_timer: None,
            throttle_step: 0,
            throttle_input_buf: String::new(),
            throttle_download_value: 0,
            throttle_upload_value: 0,
            speed_limit_download_kbps: 0,
            speed_limit_upload_kbps: 0,
            table_area: None,
            detail_file_index: 0,
            deselected_files: HashMap::new(),
            marked_ids: HashSet::new(),
            detail_peer_index: 0,
            detail_peer_scroll_offset: 0,
            sort_cache: Vec::new(),
            sort_dirty: true,
            confirm_on_quit: true,
        }
    }

    pub fn confirm_on_quit_required(&self) -> bool {
        // Trigger on anything that's mid-work: downloading, resolving
        // metadata, or actively seeding. Quitting on a seed-only session
        // still cuts peers abruptly, which warrants the prompt.
        self.confirm_on_quit
            && self.torrents.iter().any(|t| {
                matches!(
                    t.status,
                    TorrentStatus::Downloading
                        | TorrentStatus::FetchingMetadata
                        | TorrentStatus::Seeding
                )
            })
    }

    /// Mark the cached sort order as dirty. Call from anywhere that mutates
    /// `torrents`, `sort_column`, `sort_reversed`, or `filter_text`.
    pub fn invalidate_sort(&mut self) {
        self.sort_dirty = true;
    }

    /// Replace the torrent list and refresh derived state (sort cache,
    /// selection bookkeeping, pruned marks). One canonical entry point so
    /// callers can't forget to invalidate.
    pub fn handle_state_push(&mut self, torrents: Vec<TorrentInfo>) {
        self.torrents = torrents;
        self.invalidate_sort();
        self.prune_stale_state();
        self.ensure_sort_cache();
        self.restore_selection();
    }

    /// Set a new sort column and refresh the cache.
    pub fn change_sort_column(&mut self, next: SortColumn) {
        self.sort_column = next;
        self.invalidate_sort();
        self.ensure_sort_cache();
        self.restore_selection();
    }

    /// Toggle the reversed flag and refresh.
    pub fn toggle_sort_reversed(&mut self) {
        self.sort_reversed = !self.sort_reversed;
        self.invalidate_sort();
        self.ensure_sort_cache();
        self.restore_selection();
    }

    /// Append a character to the filter and refresh.
    pub fn push_filter_char(&mut self, c: char) {
        self.filter_text.push(c);
        self.invalidate_sort();
        self.ensure_sort_cache();
        self.restore_selection();
    }

    /// Drop the trailing filter character and refresh.
    pub fn pop_filter_char(&mut self) {
        self.filter_text.pop();
        self.invalidate_sort();
        self.ensure_sort_cache();
        self.restore_selection();
    }

    /// Empty the filter and refresh.
    pub fn clear_filter(&mut self) {
        if !self.filter_text.is_empty() {
            self.filter_text.clear();
            self.invalidate_sort();
            self.ensure_sort_cache();
        }
        self.restore_selection();
    }

    fn rebuild_sort_cache(&mut self) {
        let filter_lower = self.filter_text.to_lowercase();
        // Collect indices into `self.torrents` after applying the filter.
        let mut indices: Vec<usize> = self
            .torrents
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                if filter_lower.is_empty() {
                    true
                } else {
                    t.name.to_lowercase().contains(&filter_lower)
                }
            })
            .map(|(i, _)| i)
            .collect();

        if self.sort_column == SortColumn::Name {
            // Lowercase keys precomputed so each name is lowered once, not
            // O(N log N) times during sort comparisons.
            let lc: Vec<String> = indices
                .iter()
                .map(|&i| self.torrents[i].name.to_lowercase())
                .collect();
            let mut pos: Vec<usize> = (0..indices.len()).collect();
            pos.sort_by(|&a, &b| {
                let cmp = lc[a].cmp(&lc[b]);
                if self.sort_reversed {
                    cmp.reverse()
                } else {
                    cmp
                }
            });
            indices = pos.into_iter().map(|i| indices[i]).collect();
        } else {
            let sort_column = self.sort_column;
            let sort_reversed = self.sort_reversed;
            let torrents = &self.torrents;
            indices.sort_by(|&a, &b| {
                let ta = &torrents[a];
                let tb = &torrents[b];
                let cmp = match sort_column {
                    SortColumn::Index => ta.id.cmp(&tb.id),
                    SortColumn::Name => {
                        // Handled by the early-return above; degrade gracefully.
                        debug_assert!(false, "SortColumn::Name should hit the early return");
                        std::cmp::Ordering::Equal
                    }
                    SortColumn::Size => ta.size_bytes.cmp(&tb.size_bytes),
                    SortColumn::Progress => ta
                        .progress_percent()
                        .partial_cmp(&tb.progress_percent())
                        .unwrap_or(std::cmp::Ordering::Equal),
                    SortColumn::Speed => ta.download_speed.cmp(&tb.download_speed),
                    SortColumn::Peers => ta.peers_connected.cmp(&tb.peers_connected),
                    SortColumn::Eta => match (ta.eta_seconds, tb.eta_seconds) {
                        (Some(a_eta), Some(b_eta)) => a_eta.cmp(&b_eta),
                        (Some(_), None) => std::cmp::Ordering::Less,
                        (None, Some(_)) => std::cmp::Ordering::Greater,
                        (None, None) => std::cmp::Ordering::Equal,
                    },
                    SortColumn::Status => ta.status.to_string().cmp(&tb.status.to_string()),
                };
                if sort_reversed {
                    cmp.reverse()
                } else {
                    cmp
                }
            });
        }

        self.sort_cache = indices;
        self.sort_dirty = false;
    }

    pub fn sorted_torrents(&self) -> Vec<&TorrentInfo> {
        // Read path: never rebuild. Callers that need a fresh cache must
        // ensure invalidate_sort + ensure_sort_cache ran first; for the
        // hot path this is wrapped by the convenience methods below.
        if self.sort_dirty {
            // Fall back to the live sort when called before the cache has
            // been (re)built. Avoids needing `&mut self` in render paths.
            // This branch is also what tests rely on — they construct an
            // App and immediately call sorted_torrents without a state push.
            return self.sorted_torrents_live();
        }
        self.sort_cache
            .iter()
            .filter_map(|&i| self.torrents.get(i))
            .collect()
    }

    /// Refresh the sort cache if dirty. Call before render paths that need
    /// fresh data. The hot path (run_app) keeps this cheap by invalidating
    /// only when the underlying inputs change.
    pub fn ensure_sort_cache(&mut self) {
        if self.sort_dirty {
            self.rebuild_sort_cache();
        }
    }

    /// Live (uncached) sort. Used as a fallback in `sorted_torrents()` when
    /// the cache is dirty and the caller only has `&self`. Keeps the original
    /// O(N log N) cost but skips the lowercase keys allocation when not Name.
    fn sorted_torrents_live(&self) -> Vec<&TorrentInfo> {
        let filter_lower = self.filter_text.to_lowercase();
        let mut torrents: Vec<&TorrentInfo> = self
            .torrents
            .iter()
            .filter(|t| {
                if filter_lower.is_empty() {
                    true
                } else {
                    t.name.to_lowercase().contains(&filter_lower)
                }
            })
            .collect();

        if self.sort_column == SortColumn::Name {
            let lc: Vec<String> = torrents.iter().map(|t| t.name.to_lowercase()).collect();
            let mut indices: Vec<usize> = (0..torrents.len()).collect();
            indices.sort_by(|&a, &b| {
                let cmp = lc[a].cmp(&lc[b]);
                if self.sort_reversed {
                    cmp.reverse()
                } else {
                    cmp
                }
            });
            return indices.into_iter().map(|i| torrents[i]).collect();
        }

        torrents.sort_by(|a, b| {
            let cmp = match self.sort_column {
                SortColumn::Index => a.id.cmp(&b.id),
                SortColumn::Name => {
                    debug_assert!(false, "SortColumn::Name should hit the early return");
                    std::cmp::Ordering::Equal
                }
                SortColumn::Size => a.size_bytes.cmp(&b.size_bytes),
                SortColumn::Progress => a
                    .progress_percent()
                    .partial_cmp(&b.progress_percent())
                    .unwrap_or(std::cmp::Ordering::Equal),
                SortColumn::Speed => a.download_speed.cmp(&b.download_speed),
                SortColumn::Peers => a.peers_connected.cmp(&b.peers_connected),
                SortColumn::Eta => match (a.eta_seconds, b.eta_seconds) {
                    (Some(a_eta), Some(b_eta)) => a_eta.cmp(&b_eta),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                },
                SortColumn::Status => a.status.to_string().cmp(&b.status.to_string()),
            };
            if self.sort_reversed {
                cmp.reverse()
            } else {
                cmp
            }
        });

        torrents
    }

    pub fn next(&mut self) {
        let count = self.sorted_torrents().len();
        if count == 0 {
            return;
        }
        self.selected_index = (self.selected_index + 1).min(count - 1);
        self.update_selected_id();
        self.table_state.select(Some(self.selected_index));
    }

    pub fn previous(&mut self) {
        if self.sorted_torrents().is_empty() {
            return;
        }
        self.selected_index = self.selected_index.saturating_sub(1);
        self.update_selected_id();
        self.table_state.select(Some(self.selected_index));
    }

    pub fn update_selected_id(&mut self) {
        let sorted = self.sorted_torrents();
        self.selected_torrent_id = sorted.get(self.selected_index).map(|t| t.id);
    }

    pub fn selected_torrent(&self) -> Option<&TorrentInfo> {
        let sorted = self.sorted_torrents();
        sorted.get(self.selected_index).copied()
    }

    pub fn restore_selection(&mut self) {
        let (new_index, new_id) = {
            let sorted = self.sorted_torrents();
            let len = sorted.len();
            let new_index = if let Some(id) = self.selected_torrent_id {
                if let Some(pos) = sorted.iter().position(|t| t.id == id) {
                    pos
                } else if len > 0 {
                    self.selected_index.min(len - 1)
                } else {
                    0
                }
            } else if len > 0 {
                self.selected_index.min(len - 1)
            } else {
                0
            };
            // Refresh the cached id from the resolved index so callers don't
            // see a stale value between user actions.
            let new_id = sorted.get(new_index).map(|t| t.id);
            (new_index, new_id)
        };
        self.selected_index = new_index;
        self.selected_torrent_id = new_id;
        self.table_state.select(Some(self.selected_index));
        self.clamp_detail_indices();
    }

    /// Clamp detail-view indices against the currently selected torrent's
    /// file/peer counts. Called after every state push so a torrent that
    /// briefly drops to FetchingMetadata (files emptied) or that gains/loses
    /// peers can't leave the cursor pointing past the end of the list.
    pub fn clamp_detail_indices(&mut self) {
        // Extract bounds in a scope so the immutable borrow of `self` is
        // released before we mutate the index fields.
        let bounds = self
            .selected_torrent()
            .map(|t| (t.files.len(), t.peers.len()));
        match bounds {
            Some((file_count, peer_count)) => {
                let file_max = file_count.saturating_sub(1);
                if self.detail_file_index > file_max {
                    self.detail_file_index = file_max;
                }
                let peer_max = peer_count.saturating_sub(1);
                if self.detail_peer_index > peer_max {
                    self.detail_peer_index = peer_max;
                }
            }
            None => {
                self.detail_file_index = 0;
                self.detail_peer_index = 0;
            }
        }
    }

    /// Drop UI-side bookkeeping for torrents that are no longer in the list.
    /// The engine prunes its own maps; this mirrors the same pattern on the UI
    /// side (called from the state-update arm of the main loop).
    pub fn prune_stale_state(&mut self) {
        let current_ids: HashSet<usize> = self.torrents.iter().map(|t| t.id).collect();
        self.marked_ids.retain(|id| current_ids.contains(id));
        self.deselected_files
            .retain(|id, _| current_ids.contains(id));
    }

    pub fn total_download_speed(&self) -> u64 {
        self.torrents.iter().map(|t| t.download_speed).sum()
    }

    pub fn total_upload_speed(&self) -> u64 {
        self.torrents.iter().map(|t| t.upload_speed).sum()
    }

    pub fn total_uploaded_bytes(&self) -> u64 {
        self.torrents.iter().map(|t| t.uploaded_bytes).sum()
    }

    pub fn total_downloaded_bytes(&self) -> u64 {
        self.torrents.iter().map(|t| t.downloaded_bytes).sum()
    }

    pub fn active_count(&self) -> usize {
        self.torrents
            .iter()
            .filter(|t| matches!(t.status, TorrentStatus::Downloading))
            .count()
    }

    pub fn has_fetching_metadata(&self) -> bool {
        self.torrents
            .iter()
            .any(|t| matches!(t.status, TorrentStatus::FetchingMetadata))
    }

    pub fn set_error(&mut self, msg: String) {
        self.error_message = Some(msg);
        self.error_timer = Some(std::time::Instant::now());
    }

    pub fn set_info(&mut self, msg: String) {
        // If a previous info message is still on screen, append the new one so
        // burst notifications (e.g. multiple completions in one tick) don't
        // overwrite each other silently.
        if let Some(existing) = self.info_message.take() {
            self.info_message = Some(format!("{} | {}", existing, msg));
        } else {
            self.info_message = Some(msg);
        }
        self.info_timer = Some(std::time::Instant::now());
    }

    pub fn clear_expired_messages(&mut self) {
        if let Some(timer) = self.error_timer {
            if timer.elapsed() > std::time::Duration::from_secs(3) {
                self.error_message = None;
                self.error_timer = None;
            }
        }
        if let Some(timer) = self.info_timer {
            if timer.elapsed() > std::time::Duration::from_secs(5) {
                self.info_message = None;
                self.info_timer = None;
            }
        }
    }

    pub fn tick_spinner(&mut self) {
        self.spinner_tick = (self.spinner_tick + 1) % 10;
    }

    pub fn is_file_selected(&self, torrent_id: usize, file_index: usize) -> bool {
        !self
            .deselected_files
            .get(&torrent_id)
            .is_some_and(|s| s.contains(&file_index))
    }

    pub fn toggle_file_selection(&mut self, torrent_id: usize, file_index: usize) {
        let set = self.deselected_files.entry(torrent_id).or_default();
        if set.contains(&file_index) {
            set.remove(&file_index);
        } else {
            set.insert(file_index);
        }
    }

    pub fn selected_file_indices(&self, torrent_id: usize, total_files: usize) -> Vec<usize> {
        (0..total_files)
            .filter(|i| self.is_file_selected(torrent_id, *i))
            .collect()
    }

    pub fn update_disk_space(&mut self, download_dir: &str) {
        let should_update = match self.disk_space_timer {
            None => true,
            Some(t) => t.elapsed() > std::time::Duration::from_secs(5),
        };
        if should_update {
            self.free_disk_space = fs4::available_space(download_dir).ok();
            self.disk_space_timer = Some(std::time::Instant::now());
        }
    }

    pub fn toggle_mark(&mut self) {
        if let Some(torrent) = self.selected_torrent() {
            let id = torrent.id;
            if self.marked_ids.contains(&id) {
                self.marked_ids.remove(&id);
            } else {
                self.marked_ids.insert(id);
            }
        }
    }

    pub fn clear_marks(&mut self) {
        self.marked_ids.clear();
    }

    pub fn mark_all(&mut self) {
        let ids: Vec<usize> = self.sorted_torrents().iter().map(|t| t.id).collect();
        self.marked_ids.extend(ids);
    }

    pub fn has_marks(&self) -> bool {
        !self.marked_ids.is_empty()
    }

    pub fn marked_count(&self) -> usize {
        self.marked_ids.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TorrentStatus;

    fn make_torrent(id: usize, name: &str, size: u64, status: TorrentStatus) -> TorrentInfo {
        TorrentInfo {
            id,
            name: name.to_string(),
            size_bytes: size,
            downloaded_bytes: 0,
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
            throttle_paused: false,
        }
    }

    fn app_with_torrents(torrents: Vec<TorrentInfo>) -> App {
        let mut app = App::new();
        app.torrents = torrents;
        app
    }

    #[test]
    fn sorted_torrents_no_filter() {
        let app = app_with_torrents(vec![
            make_torrent(0, "Alpha", 100, TorrentStatus::Downloading),
            make_torrent(1, "Beta", 200, TorrentStatus::Paused),
        ]);
        assert_eq!(app.sorted_torrents().len(), 2);
    }

    #[test]
    fn sorted_torrents_filter_case_insensitive() {
        let mut app = app_with_torrents(vec![
            make_torrent(0, "Alpha", 100, TorrentStatus::Downloading),
            make_torrent(1, "Beta", 200, TorrentStatus::Paused),
        ]);
        app.filter_text = "alpha".to_string();
        let sorted = app.sorted_torrents();
        assert_eq!(sorted.len(), 1);
        assert_eq!(sorted[0].name, "Alpha");
    }

    #[test]
    fn sorted_torrents_filter_no_matches() {
        let mut app = app_with_torrents(vec![make_torrent(
            0,
            "Alpha",
            100,
            TorrentStatus::Downloading,
        )]);
        app.filter_text = "zzz".to_string();
        assert!(app.sorted_torrents().is_empty());
    }

    #[test]
    fn sorted_by_name() {
        let mut app = app_with_torrents(vec![
            make_torrent(0, "Zeta", 100, TorrentStatus::Downloading),
            make_torrent(1, "Alpha", 200, TorrentStatus::Downloading),
        ]);
        app.sort_column = SortColumn::Name;
        let sorted = app.sorted_torrents();
        assert_eq!(sorted[0].name, "Alpha");
        assert_eq!(sorted[1].name, "Zeta");
    }

    #[test]
    fn sorted_by_name_reversed() {
        let mut app = app_with_torrents(vec![
            make_torrent(0, "Alpha", 100, TorrentStatus::Downloading),
            make_torrent(1, "Zeta", 200, TorrentStatus::Downloading),
        ]);
        app.sort_column = SortColumn::Name;
        app.sort_reversed = true;
        let sorted = app.sorted_torrents();
        assert_eq!(sorted[0].name, "Zeta");
    }

    #[test]
    fn sorted_by_size() {
        let mut app = app_with_torrents(vec![
            make_torrent(0, "Big", 1000, TorrentStatus::Downloading),
            make_torrent(1, "Small", 100, TorrentStatus::Downloading),
        ]);
        app.sort_column = SortColumn::Size;
        let sorted = app.sorted_torrents();
        assert_eq!(sorted[0].name, "Small");
        assert_eq!(sorted[1].name, "Big");
    }

    #[test]
    fn sorted_eta_none_last() {
        let mut t1 = make_torrent(0, "A", 100, TorrentStatus::Downloading);
        t1.eta_seconds = Some(60);
        let mut t2 = make_torrent(1, "B", 100, TorrentStatus::Downloading);
        t2.eta_seconds = None;
        let mut app = app_with_torrents(vec![t2, t1]);
        app.sort_column = SortColumn::Eta;
        let sorted = app.sorted_torrents();
        assert_eq!(sorted[0].name, "A"); // Some(60) first
        assert_eq!(sorted[1].name, "B"); // None last
    }

    #[test]
    fn next_empty_list() {
        let mut app = App::new();
        app.next(); // should not panic
        assert_eq!(app.selected_index, 0);
    }

    #[test]
    fn next_single_item() {
        let mut app =
            app_with_torrents(vec![make_torrent(0, "A", 100, TorrentStatus::Downloading)]);
        app.next();
        assert_eq!(app.selected_index, 0); // stays at 0
    }

    #[test]
    fn next_advances() {
        let mut app = app_with_torrents(vec![
            make_torrent(0, "A", 100, TorrentStatus::Downloading),
            make_torrent(1, "B", 100, TorrentStatus::Downloading),
            make_torrent(2, "C", 100, TorrentStatus::Downloading),
        ]);
        app.next();
        assert_eq!(app.selected_index, 1);
        app.next();
        assert_eq!(app.selected_index, 2);
        app.next();
        assert_eq!(app.selected_index, 2); // clamped at end
    }

    #[test]
    fn previous_at_zero() {
        let mut app =
            app_with_torrents(vec![make_torrent(0, "A", 100, TorrentStatus::Downloading)]);
        app.previous();
        assert_eq!(app.selected_index, 0);
    }

    #[test]
    fn previous_moves_up() {
        let mut app = app_with_torrents(vec![
            make_torrent(0, "A", 100, TorrentStatus::Downloading),
            make_torrent(1, "B", 100, TorrentStatus::Downloading),
        ]);
        app.selected_index = 1;
        app.previous();
        assert_eq!(app.selected_index, 0);
    }

    #[test]
    fn selected_torrent_returns_correct() {
        let app = app_with_torrents(vec![
            make_torrent(0, "A", 100, TorrentStatus::Downloading),
            make_torrent(1, "B", 200, TorrentStatus::Downloading),
        ]);
        let t = app.selected_torrent().unwrap();
        assert_eq!(t.name, "A");
    }

    #[test]
    fn toggle_mark() {
        let mut app =
            app_with_torrents(vec![make_torrent(0, "A", 100, TorrentStatus::Downloading)]);
        assert!(!app.has_marks());
        app.toggle_mark();
        assert!(app.has_marks());
        assert_eq!(app.marked_count(), 1);
        app.toggle_mark();
        assert!(!app.has_marks());
    }

    #[test]
    fn mark_all_and_clear() {
        let mut app = app_with_torrents(vec![
            make_torrent(0, "A", 100, TorrentStatus::Downloading),
            make_torrent(1, "B", 100, TorrentStatus::Downloading),
            make_torrent(2, "C", 100, TorrentStatus::Downloading),
        ]);
        app.mark_all();
        assert_eq!(app.marked_count(), 3);
        app.clear_marks();
        assert!(!app.has_marks());
    }

    #[test]
    fn marks_tracked_by_id() {
        let mut app = app_with_torrents(vec![
            make_torrent(5, "A", 100, TorrentStatus::Downloading),
            make_torrent(10, "B", 100, TorrentStatus::Downloading),
        ]);
        app.toggle_mark(); // marks id=5 (first in sorted)
        assert!(app.marked_ids.contains(&5));
        assert!(!app.marked_ids.contains(&10));
    }

    #[test]
    fn restore_selection_refreshes_cached_id() {
        let mut app = app_with_torrents(vec![
            make_torrent(5, "A", 100, TorrentStatus::Downloading),
            make_torrent(10, "B", 100, TorrentStatus::Downloading),
        ]);
        app.selected_index = 1;
        app.update_selected_id();
        assert_eq!(app.selected_torrent_id, Some(10));

        // Remove the torrent currently selected and call restore_selection.
        app.torrents.retain(|t| t.id != 10);
        app.restore_selection();
        assert_eq!(app.selected_index, 0);
        // The cached id must follow the new selection, not stay stale on 10.
        assert_eq!(app.selected_torrent_id, Some(5));
    }

    #[test]
    fn prune_stale_state_drops_marks_and_deselections() {
        let mut app = app_with_torrents(vec![
            make_torrent(1, "A", 100, TorrentStatus::Downloading),
            make_torrent(2, "B", 100, TorrentStatus::Downloading),
        ]);
        app.marked_ids.insert(1);
        app.marked_ids.insert(2);
        app.marked_ids.insert(99); // stale, no torrent with id 99
        app.deselected_files.insert(2, HashSet::new());
        app.deselected_files.insert(42, HashSet::new()); // stale

        app.prune_stale_state();

        assert!(app.marked_ids.contains(&1));
        assert!(app.marked_ids.contains(&2));
        assert!(!app.marked_ids.contains(&99));
        assert!(app.deselected_files.contains_key(&2));
        assert!(!app.deselected_files.contains_key(&42));
    }

    #[test]
    fn set_info_concatenates_when_unread() {
        let mut app = App::new();
        app.set_info("first".to_string());
        app.set_info("second".to_string());
        assert_eq!(app.info_message.as_deref(), Some("first | second"));
    }
}
