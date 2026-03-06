use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionData {
    pub torrents: Vec<SavedTorrent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedTorrent {
    pub magnet_link: String,
    pub download_path: PathBuf,
    pub is_paused: bool,
}

impl SessionData {
    fn session_path() -> PathBuf {
        crate::config::Config::config_dir().join("session.json")
    }

    pub fn load() -> Result<Self> {
        let path = Self::session_path();
        if !path.exists() {
            return Ok(SessionData::default());
        }
        let content = std::fs::read_to_string(&path)?;
        match serde_json::from_str::<SessionData>(&content) {
            Ok(data) => Ok(data),
            Err(e) => {
                tracing::warn!("Corrupted session file, starting fresh: {}", e);
                Ok(SessionData::default())
            }
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::session_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp_path = path.with_extension("json.tmp");
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(&tmp_path, content)?;
        std::fs::rename(&tmp_path, &path)?;
        Ok(())
    }
}
