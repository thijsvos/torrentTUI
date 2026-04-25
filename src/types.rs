use std::fmt;

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
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
    pub trackers: Vec<String>,
    pub piece_length: Option<u32>,
    /// True when paused by the throttle system, not by the user
    pub throttle_paused: bool,
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
}

impl TorrentInfo {
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
    Filter,
    ThrottleInput,
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
}

impl DetailTab {
    pub fn next(self) -> Self {
        match self {
            DetailTab::Stats => DetailTab::Info,
            DetailTab::Info => DetailTab::Files,
            DetailTab::Files => DetailTab::Peers,
            DetailTab::Peers => DetailTab::Stats,
        }
    }

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
            throttle_paused: false,
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
        for _ in 0..4 {
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
    }
}
