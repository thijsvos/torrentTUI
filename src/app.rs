use std::collections::{HashMap, HashSet};

use crate::types::{AppMode, SortColumn, TorrentInfo, TorrentStatus};
use ratatui::widgets::TableState;

pub struct App {
    pub torrents: Vec<TorrentInfo>,
    pub table_state: TableState,
    pub selected_index: usize,
    pub selected_torrent_id: Option<usize>,
    pub mode: AppMode,
    pub detail_tab_index: usize,
    pub sort_column: SortColumn,
    pub sort_reversed: bool,
    pub error_message: Option<String>,
    pub error_timer: Option<std::time::Instant>,
    pub info_message: Option<String>,
    pub info_timer: Option<std::time::Instant>,
    pub spinner_tick: usize,
    pub should_quit: bool,
    // Feature 2: Filter
    pub filter_text: String,
    // Feature 4: Disk space
    pub free_disk_space: Option<u64>,
    pub disk_space_timer: Option<std::time::Instant>,
    // Feature 6: Throttle
    pub throttle_step: u8, // 0 = download, 1 = upload
    pub throttle_input_buf: String,
    pub throttle_download_value: u64,
    pub throttle_upload_value: u64,
    pub speed_limit_download_kbps: u64,
    pub speed_limit_upload_kbps: u64,
    // Mouse support: track the table content area for click mapping
    pub table_area: Option<ratatui::layout::Rect>,
    // Feature 7: File selection
    pub detail_file_index: usize,
    pub deselected_files: HashMap<usize, HashSet<usize>>,
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
            detail_tab_index: 0,
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
        }
    }

    pub fn sorted_torrents(&self) -> Vec<&TorrentInfo> {
        let filter_lower = self.filter_text.to_lowercase();
        let mut torrents: Vec<&TorrentInfo> = self
            .torrents
            .iter()
            .filter(|t| {
                if self.filter_text.is_empty() {
                    true
                } else {
                    t.name.to_lowercase().contains(&filter_lower)
                }
            })
            .collect();

        torrents.sort_by(|a, b| {
            let cmp = match self.sort_column {
                SortColumn::Index => a.id.cmp(&b.id),
                SortColumn::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
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
        let sorted = self.sorted_torrents();
        if let Some(id) = self.selected_torrent_id {
            if let Some(pos) = sorted.iter().position(|t| t.id == id) {
                self.selected_index = pos;
            } else if !sorted.is_empty() {
                self.selected_index = self.selected_index.min(sorted.len() - 1);
            } else {
                self.selected_index = 0;
            }
        } else if !sorted.is_empty() {
            self.selected_index = self.selected_index.min(sorted.len() - 1);
        } else {
            self.selected_index = 0;
        }
        self.table_state.select(Some(self.selected_index));
    }

    pub fn total_download_speed(&self) -> u64 {
        self.torrents.iter().map(|t| t.download_speed).sum()
    }

    pub fn total_upload_speed(&self) -> u64 {
        self.torrents.iter().map(|t| t.upload_speed).sum()
    }

    pub fn active_count(&self) -> usize {
        self.torrents
            .iter()
            .filter(|t| matches!(t.status, TorrentStatus::Downloading))
            .count()
    }

    pub fn set_error(&mut self, msg: String) {
        self.error_message = Some(msg);
        self.error_timer = Some(std::time::Instant::now());
    }

    pub fn set_info(&mut self, msg: String) {
        self.info_message = Some(msg);
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
            self.free_disk_space = get_free_space(download_dir);
            self.disk_space_timer = Some(std::time::Instant::now());
        }
    }
}

fn get_free_space(path: &str) -> Option<u64> {
    fs4::available_space(path).ok()
}
