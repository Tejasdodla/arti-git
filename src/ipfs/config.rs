use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::core::{GitError, Result};

/// Configuration for IPFS integration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpfsConfig {
    /// Whether to enable IPFS integration
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    
    /// Path to the IPFS repository
    #[serde(default = "default_repo_path")]
    pub repo_path: PathBuf,
    
    /// API endpoint for IPFS daemon
    #[serde(default = "default_api_endpoint")]
    pub api_endpoint: String,
    
    /// API port for IPFS daemon
    #[serde(default = "default_api_port")]
    pub api_port: u16,
    
    /// Whether to use the local IPFS daemon
    #[serde(default = "default_use_local_daemon")]
    pub use_local_daemon: bool,
    
    /// Whether to start a daemon if one is not found
    #[serde(default = "default_start_daemon_if_needed")]
    pub start_daemon_if_needed: bool,
    
    /// Whether to pin objects to the local IPFS node
    #[serde(default = "default_pin_objects")]
    pub pin_objects: bool,
}

fn default_enabled() -> bool {
    false // Disabled by default for now
}

fn default_repo_path() -> PathBuf {
    match home::home_dir() {
        Some(home) => home.join(".ipfs"),
        None => PathBuf::from("/tmp/.ipfs"), // Fallback
    }
}

fn default_api_endpoint() -> String {
    "http://127.0.0.1".to_string()
}

fn default_api_port() -> u16 {
    5001 // Default IPFS API port
}

fn default_use_local_daemon() -> bool {
    true
}

fn default_start_daemon_if_needed() -> bool {
    false
}

fn default_pin_objects() -> bool {
    true
}

impl Default for IpfsConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            repo_path: default_repo_path(),
            api_endpoint: default_api_endpoint(),
            api_port: default_api_port(),
            use_local_daemon: default_use_local_daemon(),
            start_daemon_if_needed: default_start_daemon_if_needed(),
            pin_objects: default_pin_objects(),
        }
    }
}

impl IpfsConfig {
    /// Create a new IpfsConfig with default values
    pub fn new() -> Self {
        Self::default()
    }
    
    /// Get the full API URL
    pub fn api_url(&self) -> String {
        format!("{}:{}", self.api_endpoint, self.api_port)
    }
    
    /// Load configuration from a file
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path)
            .map_err(|e| GitError::Config(format!("Failed to read config file {}: {}", path.display(), e)))?;
            
        toml::from_str(&content)
            .map_err(|e| GitError::Config(format!("Failed to parse config file {}: {}", path.display(), e)))
    }
    
    /// Save configuration to a file
    pub fn save_to_file(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let content = toml::to_string_pretty(self)
            .map_err(|e| GitError::Config(format!("Failed to serialize config: {}", e)))?;
            
        std::fs::write(path, content)
            .map_err(|e| GitError::Config(format!("Failed to write config file {}: {}", path.display(), e)))?;
            
        Ok(())
    }
}