use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use arti_client::TorClientConfig;
use crate::ipfs::IpfsConfig;
use crate::lfs::LfsConfig;

/// Configuration error type
#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("Config format error: {0}")]
    Format(String),
    
    #[error("Invalid configuration: {0}")]
    Invalid(String),
}

/// ArtiGit configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ArtiGitConfig {
    /// Path to local repositories
    #[serde(default = "default_repo_dir")]
    pub repo_dir: PathBuf,
    
    /// Tor configuration settings
    #[serde(default)]
    pub tor: TorConfig,
    
    /// Git configuration
    #[serde(default)]
    pub git: GitConfig,
    
    /// IPFS configuration
    #[serde(default)]
    pub ipfs: IpfsConfig,
    
    /// LFS configuration
    #[serde(default)]
    pub lfs: LfsConfig,
}

/// Tor configuration settings
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TorConfig {
    /// Whether to use Tor for all connections
    #[serde(default = "default_use_tor")]
    pub use_tor: bool,
    
    /// Path to Tor data directory (defaults to ~/.arti-git/tor)
    #[serde(default = "default_tor_data_dir")]
    pub data_dir: PathBuf,
    
    /// Onion service configuration for hosting repositories
    #[serde(default)]
    pub onion_service: Option<OnionServiceConfig>,
}

/// Git configuration settings
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GitConfig {
    /// Default remote settings
    #[serde(default)]
    pub default_remote: Option<String>,
    
    /// User information
    #[serde(default)]
    pub user_name: Option<String>,
    
    #[serde(default)]
    pub user_email: Option<String>,
}

/// Onion service configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OnionServiceConfig {
    /// Port for the onion service
    #[serde(default = "default_onion_port")]
    pub port: u16,
    
    /// Directory for onion service keys
    #[serde(default = "default_key_dir")]
    pub key_dir: PathBuf,
}

// Default functions for serde
fn default_repo_dir() -> PathBuf {
    PathBuf::from("./repos")
}

fn default_use_tor() -> bool {
    true
}

fn default_tor_data_dir() -> PathBuf {
    let mut path = dirs::data_dir().unwrap_or_else(|| PathBuf::from("~/.local/share"));
    path.push("arti-git");
    path.push("tor");
    path
}

fn default_onion_port() -> u16 {
    9418 // Default Git port
}

fn default_key_dir() -> PathBuf {
    let mut path = dirs::data_dir().unwrap_or_else(|| PathBuf::from("~/.local/share"));
    path.push("arti-git");
    path.push("onion-keys");
    path
}

impl Default for ArtiGitConfig {
    fn default() -> Self {
        Self {
            repo_dir: default_repo_dir(),
            tor: TorConfig::default(),
            git: GitConfig::default(),
            ipfs: IpfsConfig::default(),
            lfs: LfsConfig::default(),
        }
    }
}

impl Default for TorConfig {
    fn default() -> Self {
        Self {
            use_tor: default_use_tor(),
            data_dir: default_tor_data_dir(),
            onion_service: None,
        }
    }
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            default_remote: None,
            user_name: None,
            user_email: None,
        }
    }
}

impl Default for OnionServiceConfig {
    fn default() -> Self {
        Self {
            port: default_onion_port(),
            key_dir: default_key_dir(),
        }
    }
}

impl ArtiGitConfig {
    /// Load configuration from a file
    pub fn from_file(path: &Path) -> Result<Self, ConfigError> {
        // Create directories if they don't exist
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        
        // Try to read the file
        match std::fs::read_to_string(path) {
            Ok(content) => {
                let config = toml::from_str(&content)
                    .map_err(|e| ConfigError::Format(format!("Failed to parse config: {}", e)))?;
                Ok(config)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // File doesn't exist, create default config
                let config = Self::default();
                let toml = toml::to_string_pretty(&config)
                    .map_err(|e| ConfigError::Format(format!("Failed to serialize config: {}", e)))?;
                    
                std::fs::write(path, toml)?;
                Ok(config)
            }
            Err(e) => Err(ConfigError::Io(e)),
        }
    }
    
    /// Get the default configuration location
    pub fn default_location() -> PathBuf {
        let mut path = dirs::config_dir().unwrap_or_else(|| PathBuf::from("~/.config"));
        path.push("arti-git");
        path.push("config.toml");
        path
    }
    
    /// Save configuration to a file
    pub fn save_to_file(&self, path: &Path) -> Result<(), ConfigError> {
        // Create directories if they don't exist
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        
        // Serialize and save the config
        let toml = toml::to_string_pretty(self)
            .map_err(|e| ConfigError::Format(format!("Failed to serialize config: {}", e)))?;
            
        std::fs::write(path, toml)?;
        Ok(())
    }
    
    /// Convert our TorConfig to Arti's TorClientConfig
    pub fn to_arti_config(&self) -> Result<TorClientConfig, ConfigError> {
        // Start with a default configuration
        let mut arti_config = TorClientConfig::default();
        
        // Set storage location
        arti_config.storage.cache_path = Some(self.tor.data_dir.clone());
        
        // Return the configured Arti config
        Ok(arti_config)
    }
}