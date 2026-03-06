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
    pub magnet_link: String,
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
            magnet_link: String::new(),
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
}
