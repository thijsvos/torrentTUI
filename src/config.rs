use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default)]
    pub network: NetworkConfig,
    #[serde(default)]
    pub ui: UiConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralConfig {
    #[serde(default = "default_download_dir")]
    pub download_dir: String,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent_downloads: usize,
    #[serde(default = "default_true")]
    pub confirm_on_quit: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    #[serde(default = "default_listen_port")]
    pub listen_port: u16,
    #[serde(default = "default_max_peers")]
    pub max_peers_per_torrent: u32,
    #[serde(default = "default_true")]
    pub enable_dht: bool,
    #[serde(default)]
    pub max_download_speed_kbps: u64,
    #[serde(default)]
    pub max_upload_speed_kbps: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    #[serde(default = "default_refresh_rate")]
    pub refresh_rate_ms: u64,
}

fn default_download_dir() -> String {
    dirs::download_dir()
        .unwrap_or_else(|| PathBuf::from("./downloads"))
        .join("torrents")
        .to_string_lossy()
        .to_string()
}

fn default_max_concurrent() -> usize {
    5
}

fn default_true() -> bool {
    true
}

fn default_listen_port() -> u16 {
    6881
}

fn default_max_peers() -> u32 {
    50
}

fn default_refresh_rate() -> u64 {
    100
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            download_dir: default_download_dir(),
            max_concurrent_downloads: default_max_concurrent(),
            confirm_on_quit: true,
        }
    }
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            listen_port: default_listen_port(),
            max_peers_per_torrent: default_max_peers(),
            enable_dht: true,
            max_download_speed_kbps: 0,
            max_upload_speed_kbps: 0,
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            refresh_rate_ms: default_refresh_rate(),
        }
    }
}

impl Config {
    pub fn config_dir() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("torrenttui")
    }

    pub fn config_path() -> PathBuf {
        Self::config_dir().join("config.toml")
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_path();
        if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            match toml::from_str::<Config>(&content) {
                Ok(config) => Ok(config),
                Err(e) => {
                    tracing::warn!("Invalid config file, using defaults: {}", e);
                    Ok(Config::default())
                }
            }
        } else {
            let config = Config::default();
            config.save()?;
            Ok(config)
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)?;
        std::fs::write(&path, content)?;
        Ok(())
    }
}
