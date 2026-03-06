use std::fmt;

#[derive(Debug, Clone)]
pub enum TorrentStatus {
    FetchingMetadata,
    Downloading,
    Paused,
    Complete,
    Error(String),
}

impl fmt::Display for TorrentStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TorrentStatus::FetchingMetadata => write!(f, "Fetching Metadata"),
            TorrentStatus::Downloading => write!(f, "Downloading"),
            TorrentStatus::Paused => write!(f, "Paused"),
            TorrentStatus::Complete => write!(f, "Complete"),
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
    pub download_speed: u64,
    pub upload_speed: u64,
    pub peers_connected: u32,
    pub peers_total: u32,
    pub status: TorrentStatus,
    pub eta_seconds: Option<u64>,
    pub magnet_link: String,
    pub files: Vec<FileInfo>,
    /// True when paused by the throttle system, not by the user
    pub throttle_paused: bool,
}

#[derive(Debug, Clone)]
pub struct FileInfo {
    pub name: String,
    pub size_bytes: u64,
    pub progress_bytes: u64,
}

impl TorrentInfo {
    pub fn progress_percent(&self) -> f64 {
        if self.size_bytes == 0 {
            return 0.0;
        }
        (self.downloaded_bytes as f64 / self.size_bytes as f64) * 100.0
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

#[derive(Debug, Clone, PartialEq)]
pub enum SortColumn {
    Index,
    Name,
    Size,
    Progress,
    Speed,
    Peers,
    Eta,
    Status,
}

impl SortColumn {
    pub fn column_index(&self) -> usize {
        match self {
            SortColumn::Index => 0,
            SortColumn::Name => 1,
            SortColumn::Size => 2,
            SortColumn::Progress => 3,
            SortColumn::Speed => 4,
            SortColumn::Peers => 5,
            SortColumn::Eta => 6,
            SortColumn::Status => 7,
        }
    }
}
